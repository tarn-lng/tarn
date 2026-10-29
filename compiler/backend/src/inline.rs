//! Mechanical inlining of small direct calls in specialized post-drop IR.
//!
//! Runs after monomorphization, on fully verified executable IR. It copies the
//! callee's locals, drop flags and blocks into the caller with renumbered IDs:
//! parameters become locals assigned from the call arguments, and `return`
//! becomes an assignment of the callee result to the call destination. No
//! ownership, initialization or drop decision changes: every destruction plan
//! is copied verbatim. The result is re-verified by `post_drop::verify`.
use tarn_ir::post_drop::{self as post, Drop, FlagId, Op};
use tarn_ir::*;

/// Callee size limit (statements + terminators) and caller growth budget.
const SMALL: usize = 40;
const CALLER_LIMIT: usize = 4000;
const ROUNDS: usize = 3;

pub fn inline(mut p: post::Program, t: &tarn_types::Typed) -> post::Program {
    for _ in 0..ROUNDS {
        let candidates: Vec<bool> = p.functions.iter().map(|f| inlinable(f)).collect();
        let mut changed = false;
        for i in 0..p.functions.len() {
            if p.functions[i].decl.asynchronous.is_some() {
                continue;
            }
            // Only the caller's own blocks: spliced callee blocks are appended
            // and must wait for the next round, or one round would follow a
            // call chain to the caller limit in every function (ROUNDS bounds
            // the inlining depth only if each round expands one level).
            let original = p.functions[i].blocks.len();
            let mut b = 0;
            while b < original {
                let target = match &p.functions[i].blocks[b].term {
                    Terminator::Call { callee: Callee::Fn(id, _), next: Some(_), spawn: false, .. } if (id.0 as usize) != i && candidates.get(id.0 as usize).copied().unwrap_or(false) => *id,
                    _ => {
                        b += 1;
                        continue;
                    }
                };
                if size(&p.functions[i]) + size(&p.functions[target.0 as usize]) > CALLER_LIMIT {
                    b += 1;
                    continue;
                }
                let callee = p.functions[target.0 as usize].clone();
                // Malformed calls stay calls, so code generation reports them.
                if !abi_matches(&p.functions[i], b, &callee, t) {
                    b += 1;
                    continue;
                }
                splice(&mut p.functions[i], b, &callee);
                changed = true;
                b += 1;
            }
        }
        if !changed {
            break;
        }
    }
    p
}

fn abi_matches(caller: &post::Function, at: usize, callee: &post::Function, t: &tarn_types::Typed) -> bool {
    let Terminator::Call { args, dest, .. } = &caller.blocks[at].term else { return false };
    let operand = |o: &Operand| match o {
        Operand::Copy(p) | Operand::Move(p) => post::place_ty(&caller.decl, t, p),
        Operand::Const(_) => None,
    };
    args.len() == callee.decl.param_count as usize
        && post::place_ty(&caller.decl, t, dest).as_ref() == Some(&callee.decl.ret)
        && callee.decl.params().zip(args).all(|(param, arg)| match arg {
            Operand::Const(Const::Int(_, i)) => callee.decl.local(param).ty == tarn_types::Ty::Int(*i),
            Operand::Const(Const::Bool(_)) => callee.decl.local(param).ty == tarn_types::Ty::Bool,
            Operand::Const(Const::Unit) => callee.decl.local(param).ty == tarn_types::Ty::Void,
            Operand::Const(_) => false,
            o => operand(o).as_ref() == Some(&callee.decl.local(param).ty),
        })
}

fn size(f: &post::Function) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}

/// Small, ordinary, non-recursive bodies without tasks or closure environments.
fn inlinable(f: &post::Function) -> bool {
    if f.blocks.is_empty() || f.decl.asynchronous.is_some() || !matches!(f.decl.kind, FnKind::Body) || size(f) > SMALL || f.decl.name == "main" {
        return false;
    }
    // Diverging bodies would leave the continuation unreachable.
    if !f.blocks.iter().any(|b| matches!(b.term, Terminator::Return)) || f.decl.locals.iter().any(|l| l.kind == LocalKind::TaskScopeWitness) {
        return false;
    }
    f.blocks.iter().all(|b| match &b.term {
        Terminator::Call { callee: Callee::Fn(id, _), .. } => *id != f.decl.id,
        Terminator::Call { callee: Callee::TaskSpawn { .. } | Callee::Builtin(Builtin::JoinScope), .. } => false,
        Terminator::Call { spawn: true, .. } => false,
        Terminator::Suspend { .. } | Terminator::Abandon => false,
        _ => true,
    })
}

struct Map {
    locals: u32,
    blocks: u32,
    flags: u32,
}

impl Map {
    fn local(&self, l: LocalId) -> LocalId {
        LocalId(l.0 + self.locals)
    }
    fn block(&self, b: BlockId) -> BlockId {
        BlockId(b.0 + self.blocks)
    }
    fn flag(&self, f: FlagId) -> FlagId {
        FlagId(f.0 + self.flags)
    }
    fn place(&self, p: &Place) -> Place {
        Place {
            local: self.local(p.local),
            proj: p.proj.iter().map(|pr| match pr {
                Proj::Index(l) => Proj::Index(self.local(*l)),
                other => other.clone(),
            }).collect(),
        }
    }
    fn operand(&self, o: &Operand) -> Operand {
        match o {
            Operand::Copy(p) => Operand::Copy(self.place(p)),
            Operand::Move(p) => Operand::Move(self.place(p)),
            Operand::Const(c) => Operand::Const(c.clone()),
        }
    }
    fn rvalue(&self, rv: &Rvalue) -> Rvalue {
        match rv {
            Rvalue::Use(o) => Rvalue::Use(self.operand(o)),
            Rvalue::Ref(m, p) => Rvalue::Ref(*m, self.place(p)),
            Rvalue::SliceRef { mutable, base, start, end } => Rvalue::SliceRef {
                mutable: *mutable,
                base: self.place(base),
                start: start.as_ref().map(|o| self.operand(o)),
                end: end.as_ref().map(|o| self.operand(o)),
            },
            Rvalue::Binary(op, a, b) => Rvalue::Binary(*op, self.operand(a), self.operand(b)),
            Rvalue::Unary(op, o) => Rvalue::Unary(*op, self.operand(o)),
            Rvalue::Aggregate(kind, os) => {
                let kind = match kind {
                    Aggregate::Closure(id, storage) => Aggregate::Closure(*id, storage.as_ref().map(|p| self.place(p))),
                    other => other.clone(),
                };
                Rvalue::Aggregate(kind, os.iter().map(|o| self.operand(o)).collect())
            }
            Rvalue::Cast(o, t) => Rvalue::Cast(self.operand(o), t.clone()),
            Rvalue::Coerce(k, o, t) => Rvalue::Coerce(k.clone(), self.operand(o), t.clone()),
            Rvalue::Discriminant(p) => Rvalue::Discriminant(self.place(p)),
            Rvalue::Len(p) => Rvalue::Len(self.place(p)),
        }
    }
    fn drop(&self, d: &Drop) -> Drop {
        let fields = |fs: &[(u32, Drop)]| fs.iter().map(|(i, d)| (*i, self.drop(d))).collect();
        match d {
            Drop::Value(p) => Drop::Value(self.place(p)),
            Drop::Guard(f, d) => Drop::Guard(self.flag(*f), Box::new(self.drop(d))),
            Drop::Fields { place, fields: fs } => Drop::Fields { place: self.place(place), fields: fields(fs) },
            Drop::Variants { place, variants } => Drop::Variants { place: self.place(place), variants: variants.iter().map(|v| fields(v)).collect() },
            Drop::Remaining { place, flag } => Drop::Remaining { place: self.place(place), flag: self.flag(*flag) },
            Drop::RemainingVec { place, cursor } => Drop::RemainingVec { place: self.place(place), cursor: self.local(*cursor) },
        }
    }
    fn op(&self, op: &Op) -> Op {
        match op {
            Op::Plain(StatementKind::Assign(p, rv)) => Op::Plain(StatementKind::Assign(self.place(p), self.rvalue(rv))),
            Op::Plain(StatementKind::StorageLive(l)) => Op::Plain(StatementKind::StorageLive(self.local(*l))),
            Op::Plain(StatementKind::StorageDead(l)) => Op::Plain(StatementKind::StorageDead(self.local(*l))),
            Op::Plain(StatementKind::Drop(p)) => Op::Plain(StatementKind::Drop(self.place(p))),
            Op::Set(f, v) => Op::Set(self.flag(*f), *v),
            Op::ClearElement(f, l) => Op::ClearElement(self.flag(*f), self.local(*l)),
            Op::Destroy(d) => Op::Destroy(self.drop(d)),
        }
    }
    fn term(&self, t: &Terminator) -> Terminator {
        let mut t = match t {
            Terminator::Switch { discr, cases, otherwise } => Terminator::Switch { discr: self.operand(discr), cases: cases.clone(), otherwise: *otherwise },
            Terminator::Call { callee, args, arg_spans, dest, next, spawn } => Terminator::Call {
                callee: match callee {
                    Callee::Value(o) => Callee::Value(self.operand(o)),
                    other => other.clone(),
                },
                args: args.iter().map(|o| self.operand(o)).collect(),
                arg_spans: arg_spans.clone(),
                dest: self.place(dest),
                next: *next,
                spawn: *spawn,
            },
            other => other.clone(),
        };
        for b in t.targets_mut() {
            *b = self.block(*b);
        }
        t
    }
}

/// Replace the call terminating `caller.blocks[at]` with the callee's body.
fn splice(caller: &mut post::Function, at: usize, callee: &post::Function) {
    let Terminator::Call { args, dest, next: Some(next), .. } = caller.blocks[at].term.clone() else { return };
    let map = Map { locals: caller.decl.locals.len() as u32, blocks: caller.blocks.len() as u32, flags: caller.flags.len() as u32 };
    for l in &callee.decl.locals {
        let mut l = l.clone();
        if l.kind == LocalKind::Return || l.kind == LocalKind::Param {
            l.kind = LocalKind::Temp;
        }
        if let LocalKind::IterVec(cursor) = l.kind {
            l.kind = LocalKind::IterVec(map.local(cursor));
        }
        caller.decl.locals.push(l);
    }
    for f in &callee.flags {
        caller.flags.push(match f {
            post::FlagKind::Value(p) => post::FlagKind::Value(map.place(p)),
            post::FlagKind::Elements(p, n) => post::FlagKind::Elements(map.place(p), *n),
        });
    }
    let span = caller.blocks[at].term_span;
    // Arguments become the callee's parameters, then control enters its body.
    for (param, arg) in callee.decl.params().zip(&args) {
        caller.blocks[at].stmts.push(post::Statement { op: Op::Plain(StatementKind::Assign(Place::local(map.local(param)), Rvalue::Use(arg.clone()))), span });
    }
    caller.blocks[at].term = Terminator::Goto(map.block(BlockId(0)));
    let result = map.local(RETURN);
    for b in &callee.blocks {
        let stmts = b.stmts.iter().map(|s| post::Statement { op: map.op(&s.op), span: s.span }).collect::<Vec<_>>();
        let (stmts, term) = match &b.term {
            Terminator::Return => {
                let mut stmts = stmts;
                stmts.push(post::Statement { op: Op::Plain(StatementKind::Assign(dest.clone(), Rvalue::Use(Operand::Move(Place::local(result))))), span: b.term_span });
                (stmts, Terminator::Goto(next))
            }
            t => (stmts, map.term(t)),
        };
        caller.blocks.push(post::Block { stmts, term, term_span: b.term_span });
    }
}
