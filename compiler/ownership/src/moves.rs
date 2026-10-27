//! Move/init checking (phase 6A) as a forward dataflow analysis on the IR.
//!
//! State per move path: `maybe_init`, `maybe_uninit` and the move sites that
//! may reach this point. Join is pointwise union (ADR 0024), so a value moved
//! on *any* incoming path is "possibly moved" unless every path reinitialized
//! it. After the fixpoint, a second pass checks every use against the state
//! at that point and classifies every `Drop` for drop elaboration.

use crate::paths::{Lookup, MovePathId, MovePaths, PathElem};
use crate::{DropDecision, FnMoves, InitState};
use crate::util::BitSet;
use std::collections::{BTreeMap, HashMap, HashSet};
use tarn_diagnostics::{Diagnostic, Span};
use tarn_ir::{BlockId, Callee, Function, LocalKind, Operand, Place, Rvalue, StatementKind, Terminator};
use tarn_resolve::Resolved;
use tarn_types::{Ty, Typed};

/// Initialization bits per move path, plus the move sites of the paths that
/// may have been moved. Sparse in moves and bit-packed in flags, so states of
/// long functions stay small to clone and join.
#[derive(Clone, PartialEq, Debug)]
struct State {
    init: BitSet,
    uninit: BitSet,
    /// Move sites (indices into `Cx::sites`) that may have moved each path.
    moves: BTreeMap<usize, Vec<u32>>,
}

impl State {
    fn moves(&self, i: usize) -> &[u32] {
        self.moves.get(&i).map_or(&[], Vec::as_slice)
    }

    fn join(&mut self, o: &State) -> bool {
        let mut changed = self.init.union(&o.init);
        changed |= self.uninit.union(&o.uninit);
        for (i, sites) in &o.moves {
            let mine = self.moves.entry(*i).or_default();
            for m in sites {
                if let Err(at) = mine.binary_search(m) {
                    mine.insert(at, *m);
                    changed = true;
                }
            }
        }
        changed
    }
}

/// Where a use happens, for diagnostics.
#[derive(Clone, Copy)]
struct At {
    span: Span,
    /// Key of a potential move site: (block, statement or terminator, operand).
    key: (u32, u32, u32),
}

struct Cx<'a> {
    f: &'a Function,
    r: &'a Resolved,
    t: &'a Typed,
    mp: MovePaths,
    sites: HashMap<(u32, u32, u32), u32>,
    site_info: Vec<(Span, MovePathId)>,
    report: bool,
    reported: HashSet<MovePathId>,
    diags: Vec<Diagnostic>,
    drops: Vec<(BlockId, usize, DropDecision)>,
    drop_states: HashMap<(BlockId, usize), Vec<(Place, InitState)>>,
}

pub fn check_function(f: &Function, r: &Resolved, t: &Typed) -> (FnMoves, Vec<Diagnostic>) {
    if f.blocks.is_empty() {
        return (FnMoves::default(), Vec::new());
    }
    let mp = MovePaths::build(f);
    let mut cx = Cx { f, r, t, mp, sites: HashMap::new(), site_info: Vec::new(), report: false, reported: HashSet::new(), diags: Vec::new(), drops: Vec::new(), drop_states: HashMap::new() };
    let entry = cx.entry_state();
    let n = f.blocks.len();
    let mut ins: Vec<Option<State>> = vec![None; n];
    ins[0] = Some(entry);
    // Reverse postorder: joins are revisited once per change, not per path.
    let rank = crate::util::reverse_postorder(f);
    let mut work = std::collections::BTreeSet::from([(rank[0], 0usize)]);
    while let Some((_, b)) = work.pop_first() {
        let mut st = ins[b].clone().unwrap();
        cx.block(b, &mut st);
        for s in f.blocks[b].term.successors() {
            let s = s.0 as usize;
            let changed = match &mut ins[s] {
                Some(cur) => cur.join(&st),
                slot @ None => {
                    *slot = Some(st.clone());
                    true
                }
            };
            if changed {
                work.insert((rank[s], s));
            }
        }
    }
    // Reporting pass over the fixpoint.
    cx.report = true;
    for (b, st) in ins.into_iter().enumerate() {
        if let Some(mut st) = st {
            cx.block(b, &mut st);
        }
    }
    (FnMoves { drops: cx.drops, drop_states: cx.drop_states, has_errors: false }, cx.diags)
}

impl Cx<'_> {
    fn entry_state(&self) -> State {
        let n = self.mp.len();
        let mut st = State { init: BitSet::new(n), uninit: BitSet::new(n), moves: BTreeMap::new() };
        for i in 0..n {
            st.uninit.insert(i);
        }
        for l in self.f.params() {
            for &p in self.mp.subtree(self.mp.root(l)) {
                st.init.insert(p.0 as usize);
                st.uninit.remove(p.0 as usize);
            }
        }
        st
    }

    fn set_init(&self, st: &mut State, p: MovePathId) {
        for &q in self.mp.subtree(p) {
            let i = q.0 as usize;
            st.init.insert(i);
            st.uninit.remove(i);
            st.moves.remove(&i);
        }
    }

    fn set_uninit(&self, st: &mut State, p: MovePathId, site: Option<u32>) {
        for &q in self.mp.subtree(p) {
            let i = q.0 as usize;
            st.init.remove(i);
            st.uninit.insert(i);
            match site {
                Some(site) => st.moves.insert(i, vec![site]),
                None => st.moves.remove(&i),
            };
        }
    }

    fn site(&mut self, at: At, p: MovePathId) -> u32 {
        if let Some(&s) = self.sites.get(&at.key) {
            return s;
        }
        let id = self.site_info.len() as u32;
        self.site_info.push((at.span, p));
        self.sites.insert(at.key, id);
        id
    }

    // ------------------------------------------------------------ transfer

    fn block(&mut self, b: usize, st: &mut State) {
        let blk = &self.f.blocks[b];
        for (si, s) in blk.stmts.iter().enumerate() {
            let at = |k: u32| At { span: s.span, key: (b as u32, si as u32, k) };
            match &s.kind {
                StatementKind::Assign(place, rv) => {
                    self.rvalue(rv, st, at(0), s.span);
                    self.write(place, st, s.span);
                }
                StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => {
                    let root = self.mp.root(*l);
                    self.set_uninit(st, root, None);
                }
                StatementKind::Drop(place) => {
                    if let Lookup::Exact(p) = self.mp.lookup(place) {
                        if self.report {
                            let d = self.drop_decision(st, p);
                            self.drops.push((BlockId(b as u32), si, d));
                            // Drop elaboration reads only the dropped path's own
                            // state (via its ancestors) and its subtree; recording
                            // every path of the function made each drop O(paths).
                            let mut relevant = self.mp.ancestors(p);
                            relevant.extend_from_slice(self.mp.subtree(p));
                            let states = relevant.into_iter().map(|id| {
                                let i = id.0 as usize;
                                let path = self.mp.path(id);
                                let mut proj = Vec::new();
                                let mut cur = MovePathId(i as u32);
                                while let Some(parent) = self.mp.path(cur).parent {
                                    proj.push(match self.mp.path(cur).elem.unwrap() {
                                        PathElem::Field(f) => tarn_ir::Proj::Field(f),
                                        PathElem::Downcast(v) => tarn_ir::Proj::Downcast(v),
                                    });
                                    cur = parent;
                                }
                                proj.reverse();
                                let state = match (st.init.contains(i), st.uninit.contains(i)) {
                                    (true, false) => InitState::Live,
                                    (true, true) => InitState::Maybe,
                                    _ => InitState::Dead,
                                };
                                (Place { local: path.local, proj }, state)
                            }).collect();
                            self.drop_states.insert((BlockId(b as u32), si), states);
                        }
                        self.set_uninit(st, p, None);
                    } else if self.report {
                        self.drops.push((BlockId(b as u32), si, DropDecision::Static));
                        self.drop_states.insert((BlockId(b as u32), si), vec![(place.clone(), InitState::Live)]);
                    }
                }
            }
        }
        let tspan = blk.term_span;
        match &blk.term {
            Terminator::Call { callee, args, arg_spans, dest, .. } => {
                if let Callee::Value(o) = callee {
                    self.operand(o, st, At { span: tspan, key: (b as u32, u32::MAX, u32::MAX) });
                }
                for (i, a) in args.iter().enumerate() {
                    let span = arg_spans.get(i).copied().unwrap_or(tspan);
                    self.operand(a, st, At { span, key: (b as u32, u32::MAX, i as u32) });
                }
                self.write(dest, st, tspan);
            }
            Terminator::Switch { discr, .. } => self.operand(discr, st, At { span: tspan, key: (b as u32, u32::MAX, 0) }),
            _ => {}
        }
    }

    fn rvalue(&mut self, rv: &Rvalue, st: &mut State, at: At, span: Span) {
        let at_k = |k: u32| At { span: at.span, key: (at.key.0, at.key.1, k) };
        match rv {
            Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => self.operand(o, st, at_k(0)),
            Rvalue::Binary(_, a, b) => {
                self.operand(a, st, at_k(0));
                self.operand(b, st, at_k(1));
            }
            Rvalue::Aggregate(kind, os) => {
                if let tarn_ir::Aggregate::Closure(_, Some(storage)) = kind { self.read(storage, st, span, true); }
                for (i, o) in os.iter().enumerate() {
                    self.operand(o, st, at_k(i as u32));
                }
            }
            Rvalue::Ref(_, p) => self.read(p, st, span, true),
            Rvalue::SliceRef { base, start, end, .. } => {
                self.read(base, st, span, true);
                for (i, o) in start.iter().chain(end.iter()).enumerate() {
                    self.operand(o, st, at_k(i as u32));
                }
            }
            // Reading a discriminant or a length only needs the place itself.
            Rvalue::Discriminant(p) | Rvalue::Len(p) => self.read(p, st, span, false),
        }
    }

    fn operand(&mut self, o: &Operand, st: &mut State, at: At) {
        match o {
            Operand::Const(_) => {}
            Operand::Copy(p) => self.read(p, st, at.span, true),
            Operand::Move(p) => {
                self.read(p, st, at.span, true);
                match self.mp.lookup(p) {
                    Lookup::Exact(path) => {
                        let site = self.site(at, path);
                        self.set_uninit(st, path, Some(site));
                    }
                    Lookup::Deref(_) => self.err_move_out_of_ref(p, at.span),
                    Lookup::Index(base) => {
                        // Consuming iteration moves elements out of its own
                        // array (ADR 0024); any other move by index is an error.
                        if self.f.local(self.mp.path(base).local).kind != LocalKind::IterArray {
                            self.err_move_out_of_index(at.span);
                        }
                    }
                }
            }
        }
    }

    /// A read of `place`. `deep`: the whole value must be initialized
    /// (reads, borrows); otherwise only the place itself (discriminant, len).
    fn read(&mut self, place: &Place, st: &State, span: Span, deep: bool) {
        let (path, deep) = match self.mp.lookup(place) {
            Lookup::Exact(p) => (p, deep),
            // Reading through a reference or index needs the reference /
            // array itself to be initialized.
            Lookup::Deref(p) | Lookup::Index(p) => (p, false),
        };
        if !self.report || self.reported.contains(&path) {
            return;
        }
        let own = path.0 as usize;
        if st.uninit.contains(own) {
            self.reported.insert(path);
            self.err_use(path, st, span);
            return;
        }
        if deep && let Some(&d) = self.mp.subtree(path)[1..].iter().find(|d| st.uninit.contains(d.0 as usize)) {
            self.reported.insert(path);
            self.err_partial(path, d, st, span);
        }
    }

    fn write(&mut self, place: &Place, st: &mut State, span: Span) {
        match self.mp.lookup(place) {
            Lookup::Exact(p) => {
                // Assigning a field needs its parents to exist.
                if self.report
                    && let Some(&a) = self.mp.ancestors(p).iter().find(|a| st.uninit.contains(a.0 as usize))
                    && !self.reported.contains(&a)
                {
                    self.reported.insert(a);
                    self.err_assign_into_moved(p, a, st, span);
                }
                self.set_init(st, p);
            }
            Lookup::Deref(p) | Lookup::Index(p) => {
                // `(*r).f = v` / `a[i] = v`: the reference / array must exist.
                if self.report && st.uninit.contains(p.0 as usize) && !self.reported.contains(&p) {
                    self.reported.insert(p);
                    self.err_use(p, st, span);
                }
            }
        }
    }

    fn drop_decision(&self, st: &State, p: MovePathId) -> DropDecision {
        let sub = self.mp.subtree(p);
        let maybe = |q: &MovePathId| st.init.contains(q.0 as usize) && st.uninit.contains(q.0 as usize);
        let def_init = |q: &MovePathId| st.init.contains(q.0 as usize) && !st.uninit.contains(q.0 as usize);
        let def_uninit = |q: &MovePathId| !st.init.contains(q.0 as usize) && st.uninit.contains(q.0 as usize);
        if sub.iter().all(def_init) {
            DropDecision::Static
        } else if def_uninit(&p) {
            if maybe(&p) { DropDecision::Conditional } else { DropDecision::Dead }
        } else if sub.iter().any(maybe) {
            DropDecision::Conditional
        } else {
            // The root is initialized; some fields were moved on every path:
            // drop only what remains. Report the moved fields.
            let moved: Vec<String> = sub[1..]
                .iter()
                .filter(|q| def_uninit(q) && self.mp.path(**q).parent.is_some_and(|par| !def_uninit(&par)))
                .map(|q| self.name(*q))
                .collect();
            DropDecision::Partial(moved)
        }
    }

    // ------------------------------------------------------------ names

    /// Source-like name of a path: `p.first`, `s.Named.0`, `value` for temps.
    fn name(&self, p: MovePathId) -> String {
        self.name_ty(p).0
    }

    fn name_ty(&self, p: MovePathId) -> (String, Ty) {
        let path = self.mp.path(p);
        match (path.parent, path.elem) {
            (None, _) => {
                let l = self.f.local(path.local);
                let n = match l.kind {
                    LocalKind::User | LocalKind::Param => l.name.clone().unwrap_or_else(|| "value".into()),
                    _ => "value".into(),
                };
                (n, l.ty.clone())
            }
            (Some(parent), Some(elem)) => {
                let (pn, pty) = self.name_ty(parent);
                let pty = peel(&pty);
                match (elem, &pty) {
                    (PathElem::Field(i), Ty::Adt(s, args)) if self.t.decls.structs.contains_key(s) => {
                        let def = &self.t.decls.structs[s];
                        let map: HashMap<_, _> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                        let f = &def.fields[i as usize];
                        (format!("{pn}.{}", f.name), tarn_types::subst(&f.ty, &map))
                    }
                    (PathElem::Downcast(v), Ty::Adt(e, _)) => {
                        let vn = self.t.decls.enums.get(e).and_then(|d| d.variants.get(v as usize)).map(|x| x.name.clone()).unwrap_or_default();
                        (format!("{pn}.{vn}"), pty.clone())
                    }
                    (PathElem::Field(i), _) => {
                        // Field `i` of a variant: the parent is a `Downcast`.
                        let ty = self.variant_field_ty(parent, i).unwrap_or(Ty::Error);
                        (format!("{pn}.{i}"), ty)
                    }
                    _ => (pn, Ty::Error),
                }
            }
            _ => ("value".into(), Ty::Error),
        }
    }

    fn variant_field_ty(&self, downcast: MovePathId, i: u32) -> Option<Ty> {
        let dp = self.mp.path(downcast);
        let Some(PathElem::Downcast(v)) = dp.elem else { return None };
        let (_, ety) = self.name_ty(dp.parent?);
        let Ty::Adt(e, args) = peel(&ety) else { return None };
        let def = self.t.decls.enums.get(&e)?;
        let map: HashMap<_, _> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
        def.variants.get(v as usize).and_then(|x| x.fields.get(i as usize)).map(|f| tarn_types::subst(f, &map))
    }

    fn ty_name(&self, t: &Ty) -> String {
        self.t.display(t, self.r)
    }

    // ------------------------------------------------------------ diagnostics

    fn move_labels(&self, mut d: Diagnostic, path: MovePathId, st: &State, use_span: Span, verb: &str) -> Diagnostic {
        let mut in_loop = false;
        for &m in st.moves(path.0 as usize) {
            let (span, _) = self.site_info[m as usize];
            if span.start >= use_span.start {
                in_loop = true;
                d = d.secondary(span, format!("{verb} here, in a previous iteration of the loop"));
            } else {
                d = d.secondary(span, format!("{verb} here"));
            }
        }
        if in_loop {
            d = d.note("a value moved inside a loop is gone in the next iteration");
        }
        d
    }

    /// The outermost path whose recorded moves cover every move of `path`:
    /// the value the move sites actually moved.
    fn moved_ancestor(&self, path: MovePathId, st: &State) -> MovePathId {
        let sites = st.moves(path.0 as usize);
        let mut moved = path;
        let mut cur = self.mp.path(path).parent;
        while let Some(p) = cur {
            let theirs = st.moves(p.0 as usize);
            if sites.is_empty() || !sites.iter().all(|m| theirs.contains(m)) { break }
            moved = p;
            cur = self.mp.path(p).parent;
        }
        moved
    }

    fn err_use(&mut self, path: MovePathId, st: &State, span: Span) {
        let (name, ty) = self.name_ty(path);
        let i = path.0 as usize;
        let possibly = st.init.contains(i);
        let moved = st.moves.contains_key(&i);
        let d = if moved {
            let msg = if possibly { format!("use of possibly moved value `{name}`") } else { format!("use of moved value `{name}`") };
            let label = if possibly { "used here, but it may have been moved" } else { "value used here after move" };
            let mut d = Diagnostic::error("E4001", "use_after_move", msg).primary(span, label);
            d = self.move_labels(d, path, st, span, "value moved");
            // Explain and fix the value that was actually moved: using `a.data`
            // after `consume(a)` is fixed by borrowing `a`, not `a.data`.
            let moved_path = self.moved_ancestor(path, st);
            let (mname, mty) = if moved_path == path { (name.clone(), ty) } else { self.name_ty(moved_path) };
            if mname != name {
                d = d.note(format!("`{name}` is part of `{mname}`, which was moved"));
            }
            let tn = self.ty_name(&mty);
            d = d.note(format!("`{tn}` is not a copy type, so assigning or passing it moves it"));
            if possibly {
                d.help("move it on every path or on none, or reassign it before this use")
            } else {
                d.help(format!("to keep using `{mname}`, borrow it where it is moved (`&{mname}`) instead of moving it"))
            }
        } else {
            let msg = if possibly { format!("use of possibly uninitialized `{name}`") } else { format!("use of uninitialized `{name}`") };
            let mut d = Diagnostic::error("E4005", "uninitialized", msg).primary(span, if possibly { "may not be initialized here" } else { "not initialized here" });
            let decl = self.f.local(self.mp.path(path).local).span;
            d = d.secondary(decl, "declared here without a value");
            d.help("assign it on every path before this use")
        };
        self.diags.push(d);
    }

    fn err_partial(&mut self, path: MovePathId, moved: MovePathId, st: &State, span: Span) {
        let name = self.name(path);
        let mname = self.name(moved);
        let possibly = st.init.contains(moved.0 as usize);
        let msg = if possibly { format!("use of possibly partially moved value `{name}`") } else { format!("use of partially moved value `{name}`") };
        let mut d = Diagnostic::error("E4002", "use_of_partially_moved", msg).primary(span, "the whole value is used here");
        d = self.move_labels(d, moved, st, span, &format!("`{mname}` moved"));
        d = d.note(format!("the fields of `{name}` that were not moved can still be used one by one"));
        self.diags.push(d);
    }

    fn err_assign_into_moved(&mut self, field: MovePathId, parent: MovePathId, st: &State, span: Span) {
        let (fname, pname) = (self.name(field), self.name(parent));
        let moved = st.moves.contains_key(&(parent.0 as usize));
        let what = if moved { "has been moved" } else { "is not initialized" };
        let mut d = Diagnostic::error("E4006", "assign_into_moved", format!("cannot assign to `{fname}`: `{pname}` {what}")).primary(span, "");
        d = self.move_labels(d, parent, st, span, &format!("`{pname}` moved"));
        self.diags.push(d.help(format!("assign the whole value instead: `{pname} = ...`")));
    }

    fn err_move_out_of_ref(&mut self, _p: &Place, span: Span) {
        if !self.report {
            return;
        }
        self.diags.push(
            Diagnostic::error("E4003", "move_out_of_reference", "cannot move a value out from behind a reference")
                .primary(span, "this moves a non-copy value out of a borrowed place")
                .help("borrow it instead (`&...`, or return `&T`), or make an owned copy with a method such as `.clone()` when the type provides one")
                .note("a reference only lends the value; the owner keeps it"),
        );
    }

    fn err_move_out_of_index(&mut self, span: Span) {
        if !self.report {
            return;
        }
        self.diags.push(
            Diagnostic::error("E4004", "move_out_of_index", "cannot move an element out of an array by index")
                .primary(span, "moving this element would leave a hole in the array")
                .help("borrow it (`&arr[i]`), or consume the whole array with `for x in arr`"),
        );
    }
}

fn peel(t: &Ty) -> Ty {
    match t {
        Ty::Ref(_, inner) => peel(inner),
        t => t.clone(),
    }
}
