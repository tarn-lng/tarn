//! Borrow checking (phase 6B, ADR 0025) on the typed IR.
//!
//! Three dataflow analyses per function:
//!
//! 1. **Liveness** (backward): at each point, which locals may be used later.
//! 2. **Loan flow** (forward, union at joins): which loans each local may
//!    hold. A loan is created by `Ref(kind, place)`; it flows through copies,
//!    moves, coercions, aggregates (closures, `Option<&T>`) and, across calls,
//!    through the callee's *provenance* (which arguments the result may
//!    borrow from).
//! 3. **Conflicts**: a loan is *active* at a point if a local that may hold
//!    it is live there — this is what makes lifetimes non-lexical: a loan ends
//!    with the last use of the references that carry it. Every access to a
//!    place is checked against the active loans of overlapping places.
//!
//! Returned references are checked against the function's own frame, and
//! provenance summaries are inferred bottom-up (fixpoint for recursion).

use crate::util::{BitSet, may_hold_refs, place_name};
use std::sync::RwLock;
use std::collections::{HashMap, HashSet, VecDeque};
use tarn_diagnostics::{Diagnostic, Span};
use tarn_ir::{BlockId, Callee, Function, FunctionId, LocalId, Operand, Place, Program, Proj, RETURN, Rvalue, StatementKind, Terminator};
use tarn_resolve::{Resolved, SymbolKind};
use tarn_types::{FnSig, Ty, Typed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoanKind {
    Shared,
    Mutable,
}

/// A borrow: *what* is borrowed (`place`), *how* (`kind`), *where* it was
/// created, and *who* initially holds the reference. Where it is still active
/// is derived: wherever a live local may hold it.
#[derive(Clone, Debug)]
pub struct Loan {
    pub kind: LoanKind,
    pub place: Place,
    pub block: BlockId,
    pub stmt: usize,
    pub span: Span,
    pub holder: LocalId,
    /// Placeholder for "whatever the caller lent through parameter `i`".
    /// Never conflicts; used for provenance and returns.
    pub param: Option<usize>,
}

/// Argument positions (receiver first) whose loans a function's result may carry.
pub type Provenance = Vec<usize>;

/// Stored-loan effects (ADR 0068): `(src, dst)` argument positions such that
/// loans of argument `src` may be kept behind mutable argument `dst` after the
/// call returns. Sorted and deduplicated.
pub type Stores = Vec<(usize, usize)>;

#[derive(Debug, Default)]
pub struct FnBorrows {
    pub loans: Vec<Loan>,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Read,
    BorrowShared,
    BorrowMut,
    Write,
    Move,
    Drop,
}

/// Place overlap (ADR 0025): same root local, and the projections never
/// diverge at two *different* fields or variants. `Deref` and `Index` never
/// prove disjointness (conservative: no reasoning about runtime indices).
pub fn overlap(a: &Place, b: &Place) -> bool {
    if a.local != b.local {
        return false;
    }
    for (x, y) in a.proj.iter().zip(&b.proj) {
        match (x, y) {
            (Proj::Field(i), Proj::Field(j)) | (Proj::Downcast(i), Proj::Downcast(j)) if i != j => {
                return false;
            }
            _ => {}
        }
    }
    true
}

// ---------------------------------------------------------------- provenance of signatures

/// ADR 0010 elision for declarations without a body: receiver → {self};
/// exactly one reference parameter → it; otherwise none can be inferred.
pub(crate) enum Elided {
    None,
    Prov(Provenance),
    Ambiguous(usize),
    NoSource,
}

pub(crate) fn elide(sig: &FnSig) -> Elided {
    match &sig.contract.result {
        tarn_types::ResultContract::Copy | tarn_types::ResultContract::Owned | tarn_types::ResultContract::InferredBorrow => Elided::None,
        tarn_types::ResultContract::Borrowed(sources) => Elided::Prov(sources.clone()),
        tarn_types::ResultContract::Ambiguous(n) => Elided::Ambiguous(*n),
        tarn_types::ResultContract::NoSource => Elided::NoSource,
    }
}

// ---------------------------------------------------------------- per-function analysis

struct Fx<'a> {
    f: &'a Function,
    t: &'a Typed,
    /// Shared provenance table; the fixpoint updates it between analyses.
    prov: &'a RwLock<HashMap<FunctionId, Provenance>>,
    /// Shared store-summary table (ADR 0068), updated by its own fixpoint.
    stores: &'a RwLock<HashMap<FunctionId, Stores>>,
    /// Store pairs found by the current pass, with the first statement causing each.
    found: std::collections::BTreeMap<(usize, usize), Span>,
    /// `Execution.spawn`, whose computation may borrow only its Execution.
    spawn_fn: Option<FunctionId>,
    loans: Vec<Loan>,
    /// Loan created at (block, statement).
    loan_at: HashMap<(usize, usize), usize>,
    nl: usize,
    /// `live[b][i]`: locals live before statement `i` of block `b`
    /// (`i == stmts.len()`: before the terminator); `live_out[b]` after it.
    live: Vec<Vec<BitSet>>,
    live_out: Vec<BitSet>,
    /// Per local, computed once: its type may hold loans (references or tasks).
    holds_loans: Vec<bool>,
    /// Per local, computed once: its type owns a task or loan-bearing resource.
    resource: Vec<bool>,
    diags: Vec<Diagnostic>,
    reported: HashSet<usize>,
}

/// Loans each local may hold. Sparse: only locals currently holding a loan
/// are stored, so states of long functions stay small to clone and join.
#[derive(Clone, Debug, Default, PartialEq)]
struct Holds(std::collections::BTreeMap<usize, BitSet>);

static NO_LOANS: BitSet = BitSet::EMPTY;

impl Holds {
    fn get(&self, l: usize) -> &BitSet {
        self.0.get(&l).unwrap_or(&NO_LOANS)
    }
    fn set(&mut self, l: usize, loans: BitSet) {
        if loans.is_empty() {
            self.0.remove(&l);
        } else {
            self.0.insert(l, loans);
        }
    }
    fn clear(&mut self, l: usize) {
        self.0.remove(&l);
    }
    fn add(&mut self, l: usize, loans: &BitSet) -> bool {
        !loans.is_empty() && self.0.entry(l).or_default().union(loans)
    }
    fn join(&mut self, other: &Holds) -> bool {
        let mut changed = false;
        for (l, loans) in &other.0 {
            changed |= self.add(*l, loans);
        }
        changed
    }
}

fn operand_place(o: &Operand) -> Option<&Place> {
    match o {
        Operand::Copy(p) | Operand::Move(p) => Some(p),
        Operand::Const(_) => None,
    }
}

impl<'a> Fx<'a> {
    fn new(f: &'a Function, t: &'a Typed, prov: &'a RwLock<HashMap<FunctionId, Provenance>>, stores: &'a RwLock<HashMap<FunctionId, Stores>>) -> Self {
        let mut fx =
            Fx { f, t, prov, stores, found: std::collections::BTreeMap::new(), spawn_fn: None, loans: Vec::new(), loan_at: HashMap::new(), nl: f.locals.len(), live: Vec::new(), live_out: Vec::new(), holds_loans: Vec::new(), resource: Vec::new(), diags: Vec::new(), reported: HashSet::new() };
        // Deep types make these structural queries costly; ask once per local.
        fx.holds_loans = f.locals.iter().map(|l| t.decls.may_contain_references(&l.ty) || t.decls.contains_task(&l.ty)).collect();
        fx.resource = f.locals.iter().map(|l| t.decls.contains_task(&l.ty) || t.decls.contains_loan_resource(&l.ty)).collect();
        // Placeholder loans: one per reference-holding parameter.
        for (i, l) in f.params().enumerate() {
            if t.decls.may_contain_references(&f.local(l).ty) || t.decls.contains_task(&f.local(l).ty) {
                let kind = if matches!(f.local(l).ty, Ty::Ref(true, _)) { LoanKind::Mutable } else { LoanKind::Shared };
                fx.loans.push(Loan { kind, place: Place::local(l).project(Proj::Deref), block: BlockId(0), stmt: 0, span: f.local(l).span, holder: l, param: Some(i) });
            }
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, s) in b.stmts.iter().enumerate() {
                if let StatementKind::Assign(dest, rv) = &s.kind {
                    let (kind, place) = match rv {
                        Rvalue::Ref(m, p) => (*m, p),
                        Rvalue::SliceRef { mutable, base, .. } => (*mutable, base),
                        Rvalue::Aggregate(tarn_ir::Aggregate::Closure(_, Some(storage)), _) => (false, storage),
                        _ => continue,
                    };
                    let kind = if kind { LoanKind::Mutable } else { LoanKind::Shared };
                    fx.loan_at.insert((bi, si), fx.loans.len());
                    fx.loans.push(Loan { kind, place: place.clone(), block: BlockId(bi as u32), stmt: si, span: s.span, holder: dest.local, param: None });
                }
            }
        }
        fx.liveness();
        fx
    }

    // ------------------------------------------------------------ liveness

    fn place_uses(p: &Place, live: &mut BitSet) {
        if p.proj.contains(&Proj::Deref) {
            live.insert(p.local.0 as usize);
        }
        for pr in &p.proj {
            if let Proj::Index(l) = pr {
                live.insert(l.0 as usize);
            }
        }
    }

    fn read_uses(p: &Place, live: &mut BitSet) {
        live.insert(p.local.0 as usize);
        Self::place_uses(p, live);
    }

    fn rvalue_uses(rv: &Rvalue, live: &mut BitSet) {
        let op = |o: &Operand, live: &mut BitSet| {
            if let Some(p) = operand_place(o) {
                Self::read_uses(p, live);
            }
        };
        match rv {
            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => op(o, live),
            Rvalue::Binary(_, a, b) => {
                op(a, live);
                op(b, live);
            }
            Rvalue::Aggregate(kind, os) => {
                os.iter().for_each(|o| op(o, live));
                if let tarn_ir::Aggregate::Closure(_, Some(storage)) = kind { Self::read_uses(storage, live); }
            },
            Rvalue::Ref(_, p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => Self::read_uses(p, live),
            Rvalue::SliceRef { base, start, end, .. } => {
                Self::read_uses(base, live);
                start.iter().chain(end.iter()).for_each(|o| op(o, live));
            }
        }
    }

    /// Backward transfer of one statement: `live` goes from after to before.
    fn stmt_back(&self, kind: &StatementKind, live: &mut BitSet) {
        match kind {
            StatementKind::Assign(dest, rv) => {
                if dest.proj.is_empty() {
                    live.remove(dest.local.0 as usize);
                } else {
                    Self::place_uses(dest, live);
                }
                Self::rvalue_uses(rv, live);
            }
            StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => live.remove(l.0 as usize),
            // Resource destruction can use retained loans: joining a task or
            // unlocking a guard or retiring a wake keeps borrowed storage live until this use.
            StatementKind::Drop(p) => {
                Self::place_uses(p, live);
                if self.resource[p.local.0 as usize] { Self::read_uses(p, live); }
            },
        }
    }

    fn term_back(term: &Terminator, live: &mut BitSet) {
        match term {
            Terminator::Call { callee, args, dest, .. } => {
                if dest.proj.is_empty() {
                    live.remove(dest.local.0 as usize);
                } else {
                    Self::place_uses(dest, live);
                }
                for a in args {
                    if let Some(p) = operand_place(a) {
                        Self::read_uses(p, live);
                    }
                }
                if let Callee::Value(o) = callee
                    && let Some(p) = operand_place(o)
                {
                    Self::read_uses(p, live);
                }
            }
            Terminator::Switch { discr, .. } => {
                if let Some(p) = operand_place(discr) {
                    Self::read_uses(p, live);
                }
            }
            Terminator::Return => {
                live.insert(RETURN.0 as usize);
            }
            _ => {}
        }
    }

    fn liveness(&mut self) {
        let n = self.f.blocks.len();
        let mut live_in = vec![BitSet::new(self.nl); n];
        // Backward worklist in postorder (successors first); a change only
        // re-queues predecessors. Full rounds in index order needed one round
        // per nesting level, quadratic for deeply nested control flow.
        let rank = crate::util::reverse_postorder(self.f);
        let mut predecessors = vec![Vec::new(); n];
        for (b, block) in self.f.blocks.iter().enumerate() {
            for s in block.term.successors() {
                predecessors[s.0 as usize].push(b);
            }
        }
        let mut work: std::collections::BTreeSet<(std::cmp::Reverse<usize>, usize)> = (0..n).map(|b| (std::cmp::Reverse(rank[b]), b)).collect();
        while let Some((_, b)) = work.pop_first() {
            let mut live = BitSet::new(self.nl);
            for s in self.f.blocks[b].term.successors() {
                live.union(&live_in[s.0 as usize]);
            }
            Self::term_back(&self.f.blocks[b].term, &mut live);
            for s in self.f.blocks[b].stmts.iter().rev() {
                self.stmt_back(&s.kind, &mut live);
            }
            if live_in[b] != live {
                live_in[b] = live;
                for &p in &predecessors[b] {
                    work.insert((std::cmp::Reverse(rank[p]), p));
                }
            }
        }
        self.live = Vec::with_capacity(n);
        self.live_out = Vec::with_capacity(n);
        for b in 0..n {
            let blk = &self.f.blocks[b];
            let mut out = BitSet::new(self.nl);
            for s in blk.term.successors() {
                out.union(&live_in[s.0 as usize]);
            }
            let mut points = vec![BitSet::new(self.nl); blk.stmts.len() + 1];
            let mut live = out.clone();
            Self::term_back(&blk.term, &mut live);
            points[blk.stmts.len()] = live.clone();
            for (i, s) in blk.stmts.iter().enumerate().rev() {
                self.stmt_back(&s.kind, &mut live);
                points[i] = live.clone();
            }
            self.live.push(points);
            self.live_out.push(out);
        }
    }

    fn live_after(&self, b: usize, i: usize) -> &BitSet {
        if i + 1 < self.live[b].len() { &self.live[b][i + 1] } else { &self.live_out[b] }
    }

    // ------------------------------------------------------------ loan flow

    fn entry_holds(&self) -> Holds {
        let mut h = Holds::default();
        for (i, l) in self.loans.iter().enumerate() {
            if l.param.is_some() {
                h.0.entry(l.holder.0 as usize).or_default().insert(i);
            }
        }
        h
    }

    fn holds_of(&self, h: &Holds, o: &Operand, into: &mut BitSet) {
        if let Some(p) = operand_place(o) {
            into.union(h.get(p.local.0 as usize));
        }
    }

    fn rvalue_inflow(&self, h: &Holds, b: usize, si: usize, rv: &Rvalue) -> BitSet {
        let mut inflow = BitSet::new(self.loans.len());
        match rv {
            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => self.holds_of(h, o, &mut inflow),
            Rvalue::Binary(..) | Rvalue::Discriminant(_) | Rvalue::Len(_) => {}
            Rvalue::Aggregate(_, os) => {
                os.iter().for_each(|o| self.holds_of(h, o, &mut inflow));
                if let Some(&l) = self.loan_at.get(&(b, si)) { inflow.insert(l); }
            },
            Rvalue::Ref(_, p) | Rvalue::SliceRef { base: p, .. } => {
                if let Some(&l) = self.loan_at.get(&(b, si)) {
                    inflow.insert(l);
                }
                // A reference into `p` also depends on what `p` borrows
                // (reborrows through `*r` keep `r`'s loans alive).
                inflow.union(h.get(p.local.0 as usize));
            }
        }
        inflow
    }

    fn write_holds(&self, h: &mut Holds, dest: &Place, inflow: BitSet) {
        let d = dest.local.0 as usize;
        if dest.proj.is_empty() {
            h.set(d, if self.holds_loans[d] { inflow } else { BitSet::EMPTY });
        } else if !dest.proj.contains(&Proj::Deref) {
            h.add(d, &inflow);
        }
    }

    /// Loans a call result may hold: those of the arguments in the callee's
    /// provenance (all arguments when the callee is not statically known).
    fn call_inflow(&self, h: &Holds, callee: &Callee, args: &[Operand], dest: &Place) -> BitSet {
        let mut inflow = BitSet::new(self.loans.len());
        if dest.proj.is_empty() && !self.holds_loans[dest.local.0 as usize] {
            return inflow;
        }
        let positions: Option<Provenance> = match callee {
            Callee::Fn(id, _) => self.prov.read().unwrap().get(id).cloned(),
            Callee::Virtual { method, .. } => self.t.decls.fns.get(method).map(|s| match elide(s) {
                Elided::Prov(p) => p,
                _ => Vec::new(),
            }),
            Callee::TaskSpawn { scoped: true, .. } => None,
            Callee::Builtin(_) | Callee::TaskSpawn { .. } => Some(Vec::new()),
            // Source calls without contracts are rejected with E3040. Keep
            // manually constructed/recovery IR conservative as well.
            Callee::Opaque(_) => None,
            Callee::Intrinsic(_) | Callee::Value(_) => None,
        };
        // Polling a source async computation: its result cannot borrow the
        // generated frame (the async body's own provenance rejects returning
        // frame storage) nor the per-poll waker, which source code cannot name.
        // It carries the loans the computation captured (ADR 0037).
        // Elements moved out of a vector carry the loans the vector stores,
        // never the call's own borrow of the vector.
        if let Callee::Intrinsic(name) = callee
            && matches!(name.as_str(), "Vec.pop" | "Vec.replace" | "Vec.swap_remove" | "runtime._task_take")
            && let Some(receiver) = args.first().and_then(operand_place)
        {
            let mut held = BitSet::new(self.loans.len());
            for a in args {
                self.holds_of(h, a, &mut held);
            }
            for l in held.iter() {
                if self.loans[l].holder != receiver.local {
                    inflow.insert(l);
                }
            }
            return inflow;
        }
        let polled_async = matches!(callee, Callee::Value(o) if operand_place(o).is_some_and(|p| matches!(tarn_ir::post_drop::place_ty(self.f, self.t, p), Some(Ty::Ref(_, inner)) if matches!(*inner, Ty::Async(_)))));
        match positions {
            _ if polled_async => {}
            Some(ps) => ps.iter().filter_map(|&i| args.get(i)).for_each(|a| self.holds_of(h, a, &mut inflow)),
            None => args.iter().for_each(|a| self.holds_of(h, a, &mut inflow)),
        }
        if let Callee::Value(o) = callee {
            let mut callee_holds = BitSet::new(self.loans.len());
            self.holds_of(h, o, &mut callee_holds);
            for l in callee_holds.iter() {
                let place = &self.loans[l].place;
                let frame_borrow = polled_async && self.loans[l].param.is_none() && !place.proj.contains(&Proj::Deref) && matches!(self.f.local(place.local).ty, Ty::Async(_));
                if !frame_borrow {
                    inflow.insert(l);
                }
            }
        }
        inflow
    }

    fn consume_resource_operand(&self, h: &mut Holds, operand: &Operand) {
        if let Operand::Move(place) = operand
            && place.proj.is_empty() && self.resource[place.local.0 as usize] {
            h.clear(place.local.0 as usize);
        }
    }

    fn block_forward(&mut self, b: usize, h: &mut Holds, check: bool) {
        let blk = &self.f.blocks[b];
        for (si, s) in blk.stmts.iter().enumerate() {
            if check {
                self.check_stmt(b, si, &s.kind, s.span, h);
            }
            match &s.kind {
                StatementKind::Assign(dest, rv) => {
                    let inflow = self.rvalue_inflow(h, b, si, rv);
                    if dest.proj.contains(&Proj::Deref) {
                        // Direct stores of a local's loan are E4207 in check_stmt.
                        self.record_store(h, dest.local, &inflow, s.span, None);
                    }
                    match rv {
                        Rvalue::Use(o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => self.consume_resource_operand(h, o),
                        Rvalue::Aggregate(_, operands) => for o in operands { self.consume_resource_operand(h, o); },
                        _ => {},
                    }
                    self.write_holds(h, dest, inflow);
                }
                StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => h.clear(l.0 as usize),
                StatementKind::Drop(place) => {
                    if place.proj.is_empty() && self.resource[place.local.0 as usize] {
                        h.clear(place.local.0 as usize);
                    }
                }
            }
        }
        if check {
            self.check_term(b, h);
        }
        if let Terminator::Call { callee, args, dest, arg_spans, .. } = &blk.term {
            let inflow = self.call_inflow(h, callee, args, dest);
            self.call_stores(h, callee, args, arg_spans, blk.term_span, check);
            self.store_effects(h, callee, args);
            for operand in args { self.consume_resource_operand(h, operand); }
            self.write_holds(h, dest, inflow);
        }
    }

    /// Parameters whose caller-owned storage `local` reaches: placeholders of
    /// mutable reference parameters it holds, directly or through a reborrow,
    /// whose referent can hold references.
    fn caller_targets(&self, h: &Holds, local: LocalId) -> Vec<usize> {
        let mut out = Vec::new();
        for l in h.get(local.0 as usize).iter() {
            let loan = &self.loans[l];
            if let Some(k) = loan.param
                && loan.kind == LoanKind::Mutable
                && matches!(&self.f.local(loan.holder).ty, Ty::Ref(true, inner) if self.t.decls.may_contain_references(inner))
                && !out.contains(&k)
            {
                out.push(k);
            }
        }
        out
    }

    /// `inflow` may be kept in memory behind `local` (ADR 0068). Parameter
    /// placeholders become store pairs of this function; when `call` names
    /// the escaping store (a call site), a loan of this frame's own storage
    /// kept behind a caller parameter is E4207.
    fn record_store(&mut self, h: &Holds, local: LocalId, inflow: &BitSet, span: Span, call: Option<bool>) {
        let targets = self.caller_targets(h, local);
        if targets.is_empty() {
            return;
        }
        for l in inflow.iter() {
            let loan = &self.loans[l];
            match loan.param {
                Some(m) => {
                    for &k in &targets {
                        if m != k {
                            self.found.entry((m, k)).or_insert(span);
                        }
                    }
                }
                None if call == Some(true) && !loan.place.proj.contains(&Proj::Deref) => {
                    self.report_escape_call(l, targets[0], span);
                }
                None => {}
            }
        }
    }

    /// Store pairs a callee may perform: its summary for bodies, otherwise
    /// any argument behind any other (intrinsics, callables, dispatch).
    fn callee_stores(&self, callee: &Callee, arity: usize) -> Stores {
        match callee {
            Callee::Builtin(_) | Callee::TaskSpawn { .. } => Vec::new(),
            Callee::Fn(id, _) if self.stores.read().unwrap().contains_key(id) => self.stores.read().unwrap()[id].clone(),
            _ => (0..arity).flat_map(|s| (0..arity).filter(move |&d| d != s).map(move |d| (s, d))).collect(),
        }
    }

    fn call_stores(&mut self, h: &Holds, callee: &Callee, args: &[Operand], arg_spans: &[Span], span: Span, check: bool) {
        for (src, dst) in self.callee_stores(callee, args.len()) {
            let (Some(a), Some(d)) = (args.get(src), args.get(dst).and_then(operand_place)) else { continue };
            // Only a mutable reference argument can be written through.
            if !matches!(tarn_ir::post_drop::place_ty(self.f, self.t, d), Some(Ty::Ref(true, inner)) if self.t.decls.may_contain_references(&inner)) {
                continue;
            }
            // Only a mutable reference argument can be written through.
            if !matches!(tarn_ir::post_drop::place_ty(self.f, self.t, d), Some(Ty::Ref(true, inner)) if self.t.decls.may_contain_references(&inner)) {
                continue;
            }
            let mut inflow = BitSet::new(self.loans.len());
            self.holds_of(h, a, &mut inflow);
            if inflow.is_empty() {
                continue;
            }
            let at = arg_spans.get(src).copied().unwrap_or(span);
            self.record_store(h, d.local, &inflow, at, Some(check));
        }
    }

    /// Store pairs of this function for the current tables: every block is
    /// replayed from its fixpoint entry state.
    fn store_summary(&mut self, ins: &[Option<Holds>]) -> Stores {
        self.found.clear();
        for (b, h) in ins.iter().enumerate() {
            if let Some(mut h) = h.clone() {
                self.block_forward(b, &mut h, false);
            }
        }
        self.found.keys().copied().collect()
    }

    /// A callee may store any argument's loans behind a mutable reference it
    /// receives. The borrowed storage then holds those loans: union them into
    /// every local mutably borrowed by an argument whose type can hold
    /// references. Conservative; builtins and resources store nothing.
    fn store_effects(&self, h: &mut Holds, callee: &Callee, args: &[Operand]) {
        if matches!(callee, Callee::Builtin(_) | Callee::TaskSpawn { .. }) {
            return;
        }
        let mut targets = Vec::new();
        for a in args {
            let Some(arg) = operand_place(a) else { continue };
            let mut held = BitSet::new(self.loans.len());
            self.holds_of(h, a, &mut held);
            for l in held.iter() {
                let loan = &self.loans[l];
                // Only the argument's own borrow names storage the callee can
                // write; loans it merely carries were created elsewhere.
                if loan.holder == arg.local && loan.kind == LoanKind::Mutable && loan.param.is_none() && tarn_ir::post_drop::place_ty(self.f, self.t, &loan.place).is_some_and(|ty| self.t.decls.may_contain_references(&ty)) && !targets.contains(&(loan.place.local, l)) {
                    targets.push((loan.place.local, l));
                }
            }
        }
        if targets.is_empty() {
            return;
        }
        let mut stored = BitSet::new(self.loans.len());
        for a in args {
            self.holds_of(h, a, &mut stored);
        }
        for (target, own) in targets {
            let mut incoming = stored.clone();
            incoming.remove(own);
            // Storage never holds a borrow of itself.
            for l in stored.iter() {
                if self.loans[l].place.local == target {
                    incoming.remove(l);
                }
            }
            h.add(target.0 as usize, &incoming);
        }
    }

    /// Forward fixpoint; returns the holds at every block entry.
    fn flow(&mut self) -> Vec<Option<Holds>> {
        let n = self.f.blocks.len();
        let mut ins: Vec<Option<Holds>> = vec![None; n];
        ins[0] = Some(self.entry_holds());
        // Reverse postorder: joins are revisited once per change, not per path.
        let rank = crate::util::reverse_postorder(self.f);
        let mut work = std::collections::BTreeSet::from([(rank[0], 0usize)]);
        while let Some((_, b)) = work.pop_first() {
            let mut h = ins[b].clone().unwrap();
            self.block_forward(b, &mut h, false);
            // Loans of locals dead on exit never conflict again (`active` only
            // considers live holders) and every later read of a local implies
            // it is live here, so they leave the state instead of piling up.
            h.0.retain(|l, _| self.live_out[b].contains(*l));
            for s in self.f.blocks[b].term.successors() {
                let s = s.0 as usize;
                let changed = match &mut ins[s] {
                    Some(cur) => cur.join(&h),
                    slot @ None => {
                        *slot = Some(h.clone());
                        true
                    }
                };
                if changed {
                    work.insert((rank[s], s));
                }
            }
        }
        ins
    }

    /// Provenance of this function's result: the parameters whose placeholder
    /// loans reach `_0` at a `return`.
    fn summary(&mut self, ins: &[Option<Holds>]) -> Provenance {
        let mut out = Vec::new();
        for (b, blk) in self.f.blocks.iter().enumerate() {
            if !matches!(blk.term, Terminator::Return) {
                continue;
            }
            let Some(mut h) = ins[b].clone() else { continue };
            self.block_forward(b, &mut h, false);
            for l in h.get(RETURN.0 as usize).iter() {
                if let Some(i) = self.loans[l].param
                    && !out.contains(&i)
                {
                    out.push(i);
                }
            }
        }
        out.sort_unstable();
        out
    }

    // ------------------------------------------------------------ conflicts

    /// Real loans held by locals in `live`, with the locals holding each.
    fn active(&self, h: &Holds, live: &BitSet) -> Vec<(usize, Vec<LocalId>)> {
        let mut out: HashMap<usize, Vec<LocalId>> = HashMap::new();
        for (&l, loans) in h.0.iter().filter(|(l, _)| live.contains(**l)) {
            for loan in loans.iter() {
                if self.loans[loan].param.is_none() {
                    out.entry(loan).or_default().push(LocalId(l as u32));
                }
            }
        }
        let mut v: Vec<_> = out.into_iter().collect();
        v.sort_by_key(|(l, _)| *l);
        v
    }

    fn conflicts(kind: LoanKind, access: Access) -> bool {
        match access {
            Access::Read | Access::BorrowShared => kind == LoanKind::Mutable,
            _ => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn access(&mut self, b: usize, i: usize, place: &Place, access: Access, span: Span, h: &Holds, after: bool) {
        let live = if after { self.live_after(b, i).clone() } else { self.live[b][i].clone() };
        for (loan, holders) in self.active(h, &live) {
            let ln = &self.loans[loan];
            // Holds *before* this statement never contain the loan it creates,
            // except from a previous loop iteration — a real conflict.
            if !overlap(&ln.place, place) || !Self::conflicts(ln.kind, access) {
                continue;
            }
            // Dropping a local whose borrow is only kept alive by the returned
            // value: reported as a returned reference instead (clearer).
            if access == Access::Drop && holders.iter().all(|x| *x == RETURN) {
                continue;
            }
            // Reading or writing *through* the reference that holds a loan is
            // its intended use, not a conflict (e.g. `(*r).f` while `r` holds
            // a loan of `x`): such places are rooted at the reference.
            if self.reported.insert(loan) {
                self.report(b, i, loan, &holders, place, access, span);
            }
        }
    }

    fn check_stmt(&mut self, b: usize, i: usize, kind: &StatementKind, span: Span, h: &Holds) {
        match kind {
            StatementKind::Assign(dest, rv) => {
                if matches!(rv, Rvalue::Aggregate(tarn_ir::Aggregate::Closure(..), _)) {
                    let inflow = self.rvalue_inflow(h, b, i, rv);
                    if inflow.iter().any(|l| self.f.local(self.loans[l].place.local).kind == tarn_ir::LocalKind::TaskScopeWitness) {
                        self.diags.push(Diagnostic::error("E4208", "scoped_task_escape", "a scoped task handle cannot be captured by another callable")
                            .primary(span, "join the task inside its scope before capturing its result"));
                    }
                }
                self.rvalue_accesses(b, i, rv, span, h);
                self.access(b, i, dest, Access::Write, span, h, true);
                // Storing a borrow of a local into memory behind a reference
                // would let it escape the frame.
                if dest.proj.contains(&Proj::Deref) {
                    let inflow = self.rvalue_inflow(h, b, i, rv);
                    if let Some(l) = inflow.iter().find(|&l| self.loans[l].param.is_none() && !self.loans[l].place.proj.contains(&Proj::Deref)) {
                        self.report_escape_store(l, dest, span);
                    }
                }
            }
            StatementKind::Drop(p) => {
                // `Drop(p)` immediately followed by `p = v` is one overwrite;
                // `Drop(x)` followed by `StorageDead(x)` is the end of `x`'s scope.
                let rest = &self.f.blocks[b].stmts[i + 1..];
                let overwrite = matches!(rest.first().map(|s| &s.kind), Some(StatementKind::Assign(q, _)) if q == p);
                let scope_end = p.proj.is_empty() && rest.iter().any(|s| s.kind == StatementKind::StorageDead(p.local));
                if scope_end {
                    self.storage_end(b, i, p.local, span, h);
                } else {
                    self.access(b, i, p, if overwrite { Access::Write } else { Access::Drop }, span, h, true);
                }
            }
            StatementKind::StorageDead(l) => self.storage_end(b, i, *l, span, h),
            StatementKind::StorageLive(_) => {}
        }
    }

    /// The value and storage of `l` end (scope exit): no live loan may point
    /// into it. A reference only kept alive by `return` is reported as a
    /// returned reference instead (clearer).
    fn storage_end(&mut self, b: usize, i: usize, l: LocalId, span: Span, h: &Holds) {
        let live = self.live_after(b, i).clone();
        for (loan, holders) in self.active(h, &live) {
            let ln = &self.loans[loan];
            let into_l = ln.place.local == l && !ln.place.proj.contains(&Proj::Deref);
            let only_return = holders.iter().all(|x| *x == RETURN);
            if into_l && !only_return && self.reported.insert(loan) {
                self.report_dead(b, i, loan, &holders, span);
            }
        }
    }

    fn rvalue_accesses(&mut self, b: usize, i: usize, rv: &Rvalue, span: Span, h: &Holds) {
        let op = |o: &Operand| match o {
            Operand::Copy(p) => Some((p.clone(), Access::Read)),
            Operand::Move(p) => Some((p.clone(), Access::Move)),
            Operand::Const(_) => None,
        };
        let mut accs: Vec<(Place, Access)> = Vec::new();
        match rv {
            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => accs.extend(op(o)),
            Rvalue::Binary(_, a, c) => {
                accs.extend(op(a));
                accs.extend(op(c));
            }
            Rvalue::Aggregate(kind, os) => {
                accs.extend(os.iter().filter_map(op));
                if let tarn_ir::Aggregate::Closure(_, Some(storage)) = kind { accs.push((storage.clone(), Access::BorrowShared)); }
            },
            Rvalue::Ref(m, p) | Rvalue::SliceRef { mutable: m, base: p, .. } => {
                accs.push((p.clone(), if *m { Access::BorrowMut } else { Access::BorrowShared }));
                if let Rvalue::SliceRef { start, end, .. } = rv {
                    accs.extend(start.iter().chain(end.iter()).filter_map(op));
                }
            }
            Rvalue::Discriminant(p) | Rvalue::Len(p) => accs.push((p.clone(), Access::Read)),
        }
        for (p, a) in accs {
            self.access(b, i, &p, a, span, h, false);
        }
    }

    fn check_term(&mut self, b: usize, h: &Holds) {
        let blk = &self.f.blocks[b];
        let i = blk.stmts.len();
        let tspan = blk.term_span;
        match &blk.term {
            Terminator::Call { callee, args, arg_spans, dest, spawn, .. } => {
                // Native replacement transfers bytes into an existing owner.
                // Result provenance alone cannot retain newly stored loans in
                // that owner. Keep this boundary conservative until stored-loan
                // effects are modeled, rather than accepting a dangling payload.
                if matches!(callee, Callee::Intrinsic(name) if name == "MutexGuard.replace")
                    && args.len() == 2
                    && let Some(receiver) = operand_place(&args[0])
                {
                    let mut receiver_ty = &self.f.local(receiver.local).ty;
                    while let Ty::Ref(_, inner) = receiver_ty { receiver_ty = inner; }
                    if matches!(receiver_ty, Ty::Adt(id, _) if Some(*id) == self.t.decls.mutex_guard) {
                        let mut stored = BitSet::new(self.loans.len());
                        self.holds_of(h, &args[1], &mut stored);
                        if let Some(loan) = stored.iter().next() {
                            self.diags.push(Diagnostic::error("E3051", "unmodeled_replacement_loans", "replacement values with retained loans are not supported")
                                .primary(arg_spans.get(1).copied().unwrap_or(tspan), "use an owned replacement without retained borrows")
                                .secondary(self.loans[loan].span, "the replacement carries this loan")
                                .note("v0 does not propagate newly stored loans from a guard into its mutex owner"));
                        }
                    }
                }
                if let Callee::Value(o) = callee
                    && let Some(p) = operand_place(o)
                {
                    self.access(b, i, p, Access::Read, tspan, h, false);
                }
                for (k, a) in args.iter().enumerate() {
                    let span = arg_spans.get(k).copied().unwrap_or(tspan);
                    match a {
                        Operand::Copy(p) => self.access(b, i, p, Access::Read, span, h, false),
                        Operand::Move(p) => self.access(b, i, p, Access::Move, span, h, false),
                        Operand::Const(_) => {}
                    }
                    if *spawn && !matches!(callee, Callee::TaskSpawn { scoped: true, .. }) {
                        let mut held = BitSet::new(self.loans.len());
                        self.holds_of(h, a, &mut held);
                        if held.iter().next().is_some() {
                            self.diags.push(
                                Diagnostic::error("E4206", "reference_to_spawned_task", "a spawned task cannot receive a reference")
                                    .primary(span, "this argument holds a reference")
                                    .note("v0 does not track borrows across tasks; pass owned values to `spawn`"),
                            );
                        }
                    }
                }
                // v0 cooperative tasks may borrow only the Execution they run on:
                // the spawn mailbox and executor never outlive it (ADR 0038).
                if let Callee::Fn(id, _) = callee
                    && Some(*id) == self.spawn_fn
                    && let [owner, computation] = args.as_slice()
                {
                    let (mut allowed, mut held) = (BitSet::new(self.loans.len()), BitSet::new(self.loans.len()));
                    self.holds_of(h, owner, &mut allowed);
                    self.holds_of(h, computation, &mut held);
                    let owner_roots: Vec<LocalId> = allowed.iter().map(|l| self.loans[l].place.local).collect();
                    if held.iter().any(|l| !allowed.contains(l) && !owner_roots.contains(&self.loans[l].place.local)) {
                        self.diags.push(
                            Diagnostic::error("E4209", "borrow_in_async_task", "a spawned async task cannot hold borrowed data")
                                .primary(arg_spans.get(1).copied().unwrap_or(tspan), "this computation holds a reference")
                                .note("v0 async tasks may borrow only the Execution they run on; pass owned values"),
                        );
                    }
                }
                if !matches!(callee, Callee::Intrinsic(name) if name == "Task.join") && !matches!(callee, Callee::TaskSpawn { .. }) {
                    if args.iter().filter_map(operand_place).any(|p| self.t.decls.contains_task(&self.f.local(p.local).ty)
                        && h.get(p.local.0 as usize).iter().any(|l| self.f.local(self.loans[l].place.local).kind == tarn_ir::LocalKind::TaskScopeWitness)) {
                        self.diags.push(Diagnostic::error("E4208", "scoped_task_escape", "a scoped task handle must complete inside its creating scope")
                            .primary(tspan, "join the handle locally before passing its result"));
                    }
                }
                self.access(b, i, dest, Access::Write, tspan, h, true);
            }
            Terminator::Switch { discr, .. } => {
                if let Some(p) = operand_place(discr) {
                    self.access(b, i, p, Access::Read, tspan, h, false);
                }
            }
            Terminator::Return => self.check_return(h, tspan),
            _ => {}
        }
    }

    /// A returned value may only carry loans of the caller's memory: loans
    /// through a reference parameter (`*p…`) or the parameters' own placeholders.
    fn check_return(&mut self, h: &Holds, span: Span) {
        let storage_loan = |ln: &Loan| self.f.local(ln.place.local).name.as_deref() == Some("closure environment");
        let capture_error = h.get(RETURN.0 as usize).iter().any(|l| {
            let ln = &self.loans[l];
            ln.param.is_none() && !ln.place.proj.contains(&Proj::Deref) && !storage_loan(ln)
        });
        for l in h.get(RETURN.0 as usize).iter() {
            let ln = &self.loans[l];
            if ln.param.is_some() || ln.place.proj.contains(&Proj::Deref) || !self.reported.insert(l) {
                continue;
            }
            if storage_loan(ln) && capture_error { continue; }
            let name = place_name(self.f, self.t, &ln.place);
            let d = if self.f.local(ln.place.local).kind == tarn_ir::LocalKind::TaskScopeWitness {
                Diagnostic::error("E4208", "scoped_task_escape", "a scoped task handle cannot escape its creating scope")
                    .primary(span, "join the task inside its scope and return its owned result")
                    .secondary(ln.span, "the task starts in this scope")
            } else if storage_loan(ln) {
                Diagnostic::error("E4205", "closure_escapes_borrow", "borrowed closure cannot escape its stack environment")
                    .primary(span, "the closure escapes here")
                    .secondary(ln.span, "this environment ends when its creating scope exits")
                    .help("use `move fn` to create an owned environment; moved references must still remain valid")
            } else if matches!(self.f.ret, Ty::Fn(..)) {
                Diagnostic::error("E4205", "closure_escapes_borrow", format!("returned closure captures `{name}` by reference, but `{name}` is dropped when the function returns"))
                    .primary(span, "the closure escapes here")
                    .secondary(ln.span, format!("`{name}` is captured by reference here"))
                    .note("captured references must remain valid for the entire returned closure lifetime")
                    .help("capture an owned value with `move fn`, or borrow from a parameter")
            } else if ln.place.proj.is_empty() && matches!(self.f.local(ln.place.local).ty, Ty::Fn(tarn_types::CallMode::Shared | tarn_types::CallMode::Mutable, ..)) {
                // Calling an owned callable borrows it; a `move fn` may return a
                // reference into its own captured values, which die with it.
                Diagnostic::error("E4201", "reference_escapes", format!("the result of calling `{name}` may borrow from `{name}`, which is dropped when this function returns"))
                    .primary(span, "returned here")
                    .secondary(ln.span, format!("`{name}` is called here"))
                    .note("the result type may hold a reference, and a `move fn` can return a reference into its own captured values")
                    .help(format!("take `{name}` as `once fn(...)` (the call consumes it), or as `&fn(...)` so the caller keeps it alive"))
            } else {
                Diagnostic::error("E4201", "reference_escapes", format!("cannot return a reference to `{name}`"))
                    .primary(span, "returned here")
                    .secondary(ln.span, format!("`{name}` is borrowed here"))
                    .note(format!("`{name}` belongs to this function and is dropped when it returns"))
                    .help("return an owned value instead, or borrow from a parameter")
            };
            self.diags.push(d);
        }
    }

    // ------------------------------------------------------------ diagnostics

    /// The first later use (in CFG order) of any of `holders` after (b, i).
    fn later_use(&self, b: usize, i: usize, holders: &[LocalId]) -> Option<Span> {
        let uses = |live: &mut BitSet| holders.iter().any(|l| live.contains(l.0 as usize));
        let mut seen = HashSet::new();
        let mut queue: VecDeque<(usize, usize)> = VecDeque::from([(b, i + 1)]);
        while let Some((blk, start)) = queue.pop_front() {
            let block = &self.f.blocks[blk];
            for si in start..block.stmts.len() {
                let mut u = BitSet::new(self.nl);
                if let StatementKind::Assign(_, rv) = &block.stmts[si].kind {
                    Self::rvalue_uses(rv, &mut u);
                    if uses(&mut u) {
                        return Some(block.stmts[si].span);
                    }
                }
            }
            if start <= block.stmts.len() {
                let mut u = BitSet::new(self.nl);
                if let Terminator::Call { args, arg_spans, .. } = &block.term {
                    for (k, a) in args.iter().enumerate() {
                        let mut ua = BitSet::new(self.nl);
                        if let Some(p) = operand_place(a) {
                            Self::read_uses(p, &mut ua);
                        }
                        if uses(&mut ua) {
                            return Some(arg_spans.get(k).copied().unwrap_or(block.term_span));
                        }
                    }
                }
                Self::term_back(&block.term, &mut u);
                if !matches!(block.term, Terminator::Call { .. } | Terminator::Return) && uses(&mut u) {
                    return Some(block.term_span);
                }
            }
            for s in block.term.successors() {
                if seen.insert(s.0) {
                    queue.push_back((s.0 as usize, 0));
                }
            }
        }
        None
    }

    fn kind_word(k: LoanKind) -> &'static str {
        match k {
            LoanKind::Shared => "shared",
            LoanKind::Mutable => "mutable",
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn report(&mut self, b: usize, i: usize, loan: usize, holders: &[LocalId], place: &Place, access: Access, span: Span) {
        let ln = self.loans[loan].clone();
        let acc = place_name(self.f, self.t, place);
        let bor = place_name(self.f, self.t, &ln.place);
        let (code, kind, msg, label) = match (access, ln.kind) {
            (Access::BorrowMut, LoanKind::Shared) => {
                ("E4101", "conflicting_borrow", format!("cannot borrow `{acc}` as mutable because it is also borrowed as shared"), "mutable borrow conflicts here")
            }
            (Access::BorrowMut, LoanKind::Mutable) => ("E4101", "conflicting_borrow", format!("cannot borrow `{acc}` as mutable more than once at a time"), "second mutable borrow here"),
            (Access::BorrowShared, _) => ("E4101", "conflicting_borrow", format!("cannot borrow `{acc}` as shared because it is mutably borrowed"), "shared borrow conflicts here"),
            (Access::Read, _) => ("E4104", "use_while_mutably_borrowed", format!("cannot use `{acc}` while it is mutably borrowed"), "used here"),
            (Access::Write, _) => ("E4102", "assign_while_borrowed", format!("cannot assign to `{acc}` because it is borrowed"), "assigned here while borrowed"),
            (Access::Move, _) => ("E4103", "move_while_borrowed", format!("cannot move out of `{acc}` because it is borrowed"), "moved here while borrowed"),
            (Access::Drop, _) => ("E4105", "dropped_while_borrowed", format!("`{acc}` is dropped while it is still borrowed"), "dropped here"),
        };
        let mut d = Diagnostic::error(code, kind, msg).primary(span, label).secondary(ln.span, format!("{} borrow of `{bor}` starts here", Self::kind_word(ln.kind)));
        if let Some(u) = self.later_use(b, i, holders) {
            d = d.secondary(u, "the borrow is still used here");
        }
        d = d.note("a borrow lasts until the last use of the reference that holds it");
        if holders.iter().any(|l| self.t.decls.contains_task(&self.f.local(*l).ty)) {
            d = d.note("a scoped worker keeps its borrow until join or scope completion");
        }
        self.diags.push(d);
    }

    fn report_dead(&mut self, b: usize, i: usize, loan: usize, holders: &[LocalId], span: Span) {
        let ln = self.loans[loan].clone();
        let root = Place::local(ln.place.local);
        let name = place_name(self.f, self.t, &root);
        let mut d = Diagnostic::error("E4105", "does_not_live_long_enough", format!("`{name}` does not live long enough"))
            .primary(span, format!("`{name}` goes out of scope here while still borrowed"))
            .secondary(ln.span, format!("`{}` is borrowed here", place_name(self.f, self.t, &ln.place)));
        if let Some(u) = self.later_use(b, i, holders) {
            d = d.secondary(u, "the borrow is still used here");
        }
        self.diags.push(d.help(format!("declare `{name}` in an outer scope so it outlives the reference")));
    }

    fn report_escape_call(&mut self, loan: usize, param: usize, span: Span) {
        if !self.reported.insert(loan) {
            return;
        }
        let ln = self.loans[loan].clone();
        let name = place_name(self.f, self.t, &ln.place);
        let target = self.f.local(LocalId(param as u32 + 1)).name.clone().unwrap_or_else(|| format!("#{param}"));
        let mut d = Diagnostic::error("E4207", "borrow_escapes_through_reference", format!("cannot store a reference to `{name}` into `{target}`"))
            .primary(span, format!("this call may keep the reference behind `{target}`"));
        if ln.span != span {
            d = d.secondary(ln.span, format!("`{name}` is borrowed here"));
        }
        self.diags.push(
            d.note(format!("`{target}` belongs to the caller; `{name}` is dropped when this function returns"))
                .help("pass an owned value, or keep the data in storage that outlives the caller"),
        );
    }

    fn report_escape_store(&mut self, loan: usize, dest: &Place, span: Span) {
        if !self.reported.insert(loan) {
            return;
        }
        let ln = self.loans[loan].clone();
        let (name, target) = (place_name(self.f, self.t, &ln.place), place_name(self.f, self.t, dest));
        self.diags.push(
            Diagnostic::error("E4207", "borrow_escapes_through_reference", format!("cannot store a reference to `{name}` into `{target}`"))
                .primary(span, "the reference would outlive this function")
                .secondary(ln.span, format!("`{name}` is borrowed here"))
                .note(format!("`{target}` belongs to the caller; `{name}` is dropped when this function returns")),
        );
    }
}

// ---------------------------------------------------------------- program

#[derive(Debug, Default)]
pub struct BorrowResults {
    pub functions: HashMap<FunctionId, FnBorrows>,
}

impl BorrowResults {
    /// `name(args) -> borrows from: …` for functions returning references.
    pub fn provenance_lines(&self, p: &Program) -> Vec<String> {
        let mut out: Vec<String> = p
            .functions
            .iter()
            .filter(|f| !f.blocks.is_empty() && may_hold_refs(&f.ret))
            .map(|f| {
                let prov = self.functions.get(&f.id).map(|b| b.provenance.clone()).unwrap_or_default();
                let names: Vec<String> = prov.iter().map(|&i| f.local(LocalId(i as u32 + 1)).name.clone().unwrap_or_else(|| format!("#{i}"))).collect();
                format!("{} -> borrows from {{{}}}", f.name, names.join(", "))
            })
            .collect();
        out.sort();
        out
    }
}

/// Run the borrow checker. Functions in `skip` (move errors) are only used
/// for provenance, not checked, to avoid cascading diagnostics.
pub fn check_borrows(p: &Program, r: &Resolved, t: &Typed, skip: &HashSet<FunctionId>) -> (BorrowResults, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    // 1. Provenance of declarations without a body (ADR 0010 elision).
    let mut prov: HashMap<FunctionId, Provenance> = HashMap::new();
    for f in &p.functions {
        if !f.blocks.is_empty() {
            prov.insert(f.id, Vec::new());
            continue;
        }
        if let Some(sig) = f.symbol.and_then(|s| t.decls.fns.get(&s))
            && let Elided::Prov(ps) = elide(sig)
        {
            prov.insert(f.id, ps);
        }
    }
    check_declarations(r, t, &mut diags);
    // 2. Bodies: provenance summaries to a fixpoint (recursion), then checking.
    // Each function's loans and liveness are built once. A function is
    // re-analyzed only when a direct callee's summary changes; summaries only
    // grow, so this reaches the same least fixpoint as repeated full rounds.
    let prov = RwLock::new(prov);
    // Store summaries (ADR 0068): bodies start empty; declarations without a
    // body have no entry and are treated conservatively at call sites.
    let stores: RwLock<HashMap<FunctionId, Stores>> = RwLock::new(p.functions.iter().filter(|f| !f.blocks.is_empty()).map(|f| (f.id, Vec::new())).collect());
    let spawn_fn = t.decls.exec_spawn.and_then(|s| p.by_symbol.get(&s)).copied();
    let bodies: Vec<&Function> = p.functions.iter().filter(|f| !f.blocks.is_empty()).collect();
    // First round on all cores against the initial table: equivalent to the
    // first worklist pass, since summaries only grow and changes re-queue.
    let first = crate::util::par_map(bodies.clone(), |f| {
        let mut fx = Fx::new(f, t, &prov, &stores);
        fx.spawn_fn = spawn_fn;
        let ins = fx.flow();
        let summary = fx.summary(&ins);
        (fx, ins, summary)
    });
    let mut fxs: Vec<Fx> = Vec::with_capacity(bodies.len());
    let mut flows: Vec<Vec<Option<Holds>>> = Vec::with_capacity(bodies.len());
    let mut summaries = Vec::with_capacity(bodies.len());
    for (fx, ins, summary) in first {
        fxs.push(fx);
        flows.push(ins);
        summaries.push(summary);
    }
    let index: HashMap<FunctionId, usize> = bodies.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    let mut callers: Vec<Vec<usize>> = vec![Vec::new(); bodies.len()];
    for (i, f) in bodies.iter().enumerate() {
        for b in &f.blocks {
            if let Terminator::Call { callee: Callee::Fn(id, _), .. } = &b.term
                && let Some(&callee) = index.get(id)
                && !callers[callee].contains(&i)
            {
                callers[callee].push(i);
            }
        }
    }
    let mut queued = vec![false; bodies.len()];
    let mut work = std::collections::BTreeSet::new();
    for (i, summary) in summaries.into_iter().enumerate() {
        if prov.read().unwrap().get(&bodies[i].id) != Some(&summary) {
            prov.write().unwrap().insert(bodies[i].id, summary);
            for &c in &callers[i] {
                if !queued[c] {
                    queued[c] = true;
                    work.insert(c);
                }
            }
        }
    }
    // Same bound as the former 32 full rounds.
    let mut budget = 32 * bodies.len().max(1);
    while let Some(i) = work.pop_first() {
        queued[i] = false;
        let ins = fxs[i].flow();
        let s = fxs[i].summary(&ins);
        flows[i] = ins;
        if prov.read().unwrap().get(&bodies[i].id) != Some(&s) {
            prov.write().unwrap().insert(bodies[i].id, s);
            for &c in &callers[i] {
                if !queued[c] {
                    queued[c] = true;
                    work.insert(c);
                }
            }
        }
        budget -= 1;
        if budget == 0 {
            break;
        }
    }
    // Store summaries never change loan flow, so they reach their own least
    // fixpoint on the final flows. Pairs only grow; the set is finite.
    let mut work: std::collections::BTreeSet<usize> = (0..bodies.len()).collect();
    let mut budget = 32 * bodies.len().max(1);
    while let Some(i) = work.pop_first() {
        let s = fxs[i].store_summary(&flows[i]);
        if stores.read().unwrap().get(&bodies[i].id) != Some(&s) {
            stores.write().unwrap().insert(bodies[i].id, s);
            work.extend(callers[i].iter().copied());
        }
        budget -= 1;
        if budget == 0 {
            break;
        }
    }
    // Checking reads the final table only; functions are independent.
    let checked = crate::util::par_map(bodies.iter().zip(fxs).zip(flows).collect(), |((f, mut fx), ins)| {
        if !skip.contains(&f.id) {
            for (b, h) in ins.iter().enumerate() {
                if let Some(mut h) = h.clone() {
                    fx.block_forward(b, &mut h, true);
                }
            }
            // A closure's call parameters are borrowed for one invocation only;
            // keeping one in captured state lets the invoker's local escape.
            if let tarn_ir::FnKind::Closure { captures, .. } = &f.kind {
                let n = captures.len();
                let escaping: Vec<((usize, usize), Span)> = fx.found.iter().filter(|((src, dst), _)| *src >= n && *dst < n).map(|(k, v)| (*k, *v)).collect();
                if let Some(&((src, dst), span)) = escaping.first() {
                    let name = |i: usize| f.local(LocalId(i as u32 + 1)).name.clone().unwrap_or_else(|| format!("#{i}"));
                    fx.diags.push(
                        Diagnostic::error("E4210", "callable_parameter_escapes", format!("cannot keep parameter `{}` in captured `{}`", name(src), name(dst)))
                            .primary(span, format!("`{}` would outlive this call", name(src)))
                            .note("a callable's reference parameters are only borrowed for the duration of each call")
                            .help("copy or clone the data you need, or keep owned values in the captured state"),
                    );
                }
            }
        }
        (f, fx)
    });
    let mut results = BorrowResults::default();
    for (f, mut fx) in checked {
        diags.append(&mut fx.diags);
        let provenance = prov.read().unwrap().get(&f.id).cloned().unwrap_or_default();
        results.functions.insert(f.id, FnBorrows { loans: fx.loans, provenance });
    }
    check_impl_provenance(p, r, t, &prov.read().unwrap(), &mut diags);
    (results, diags)
}

/// E4202 for interface methods and extern functions whose returned reference
/// has no unambiguous source (ADR 0010).
fn check_declarations(r: &Resolved, t: &Typed, diags: &mut Vec<Diagnostic>) {
    let mut syms: Vec<_> = t.decls.fns.iter().collect();
    syms.sort_by_key(|(s, _)| **s);
    for (s, sig) in syms {
        let sym = r.symbol(*s);
        let bodyless = matches!(sym.kind, SymbolKind::InterfaceMethod { .. }) || sig.abi.is_some();
        if !bodyless {
            continue;
        }
        match elide(sig) {
            Elided::Ambiguous(n) => diags.push(
                Diagnostic::error("E4202", "ambiguous_provenance", format!("cannot infer where the reference returned by `{}` comes from", sym.name))
                    .primary(sig.span, format!("{n} reference parameters could be the source"))
                    .note("without an explicit borrows(...) clause, only a borrowed receiver or a single reference parameter determines it")
                    .help("declare borrows(input, ...), return an owned value, or keep only one reference input"),
            ),
            Elided::NoSource => diags.push(
                Diagnostic::error("E4201", "reference_escapes", format!("`{}` returns a reference but has no reference parameter to borrow from", sym.name))
                    .primary(sig.span, "")
                    .help("return an owned value"),
            ),
            _ => {}
        }
    }
}

/// E4204: an `impl` method may not return borrows its interface does not allow.
fn check_impl_provenance(p: &Program, r: &Resolved, t: &Typed, prov: &HashMap<FunctionId, Provenance>, diags: &mut Vec<Diagnostic>) {
    for f in &p.functions {
        let Some(sym) = f.symbol else { continue };
        let SymbolKind::ImplMethod { interface: Some(iface), .. } = &r.symbol(sym).kind else { continue };
        let Some(decl) = r.member(*iface, &r.symbol(sym).name) else { continue };
        let Some(dsig) = t.decls.fns.get(&decl) else { continue };
        let Elided::Prov(allowed) = elide(dsig) else { continue };
        let have = prov.get(&f.id).cloned().unwrap_or_default();
        if have.iter().any(|i| !allowed.contains(i)) {
            diags.push(
                Diagnostic::error("E4204", "provenance_mismatch", format!("`{}` returns a borrow its interface declaration does not allow", r.symbol(sym).name))
                    .primary(t.decls.fns[&sym].span, "")
                    .secondary(dsig.span, "the interface restricts which inputs may supply the result")
                    .help("return a reference derived only from the declared allowed inputs"),
            );
        }
    }
}

