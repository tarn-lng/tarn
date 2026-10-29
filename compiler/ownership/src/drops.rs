//! Drop elaboration consumes 6A facts; never reruns borrow or move checking.
use crate::{InitState, MoveResults};
use std::collections::HashMap;
use tarn_ir::{self as ir, post_drop as post, *};
use tarn_types::{Ty, Typed};

/// Only call after successful 6A/6B. Missing facts are compiler failures.
pub fn elaborate_drops(p: &ir::Program, t: &Typed, moves: &MoveResults) -> Result<post::Program, Vec<String>> {
    let mut out = post::Program { functions: Vec::new(), by_symbol: p.by_symbol.clone() };
    let mut errors = Vec::new();
    // Functions are elaborated independently; results keep program order.
    let elaborated = crate::util::par_map(p.functions.iter().collect(), |f: &ir::Function| {
        let mut errors = Vec::new();
        let Some(facts) = moves.functions.get(&f.id) else {
            errors.push(format!("{}: missing move results", f.name));
            return (None, errors);
        };
        if facts.has_errors {
            errors.push(format!("{}: elaboration after failed move checking", f.name));
            return (None, errors);
        }
        let mut cx = Cx { f, t, flags: Vec::new() };
        let mut plans = HashMap::new();
        // Build all plans first: flags used at any site must be maintained at
        // every mutation, including ones textually preceding that site.
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, s) in b.stmts.iter().enumerate() {
                if let StatementKind::Drop(p) = &s.kind {
                    let key = (BlockId(bi as u32), si);
                    let Some(states) = facts.drop_states.get(&key) else {
                        errors.push(format!("{}: missing drop state at bb{bi}:{si}", f.name));
                        continue;
                    };
                    let Some(ty) = post::place_ty(f, t, p) else {
                        errors.push(format!("{}: invalid drop place", f.name));
                        continue;
                    };
                    plans.insert(key, cx.plan(p, &ty, states));
                }
            }
        }
        let mut blocks: Vec<(post::Block, Option<post::Block>)> = Vec::new();
        let mut bridge_count = 0;
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut stmts = Vec::new();
            if bi == 0 {
                for (i, flag) in cx.flags.iter().enumerate() {
                    let p = flag_place(flag);
                    // Parameters begin initialized; all other slots do not.
                    emit(&mut stmts, post::Op::Set(post::FlagId(i as u32), p.local.0 > 0 && p.local.0 <= f.param_count), f.span);
                }
            }
            for (si, s) in b.stmts.iter().enumerate() {
                match &s.kind {
                    StatementKind::Drop(p) => {
                        if let Some(Some(d)) = plans.get(&(BlockId(bi as u32), si)) {
                            emit(&mut stmts, post::Op::Destroy(d.clone()), s.span);
                        }
                        cx.update(p, false, &mut stmts, s.span);
                    }
                    StatementKind::Assign(p, rv) => {
                        // Moves happen while evaluating the RHS, before the
                        // destination is initialized. There is no unwind.
                        let mut operands = Vec::new();
                        rv_ops(rv, &mut operands);
                        emit(&mut stmts, post::Op::Plain(s.kind.clone()), s.span);
                        for o in operands {
                            cx.move_op(o, &mut stmts, s.span);
                        }
                        cx.update(p, true, &mut stmts, s.span);
                    }
                    StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => {
                        emit(&mut stmts, post::Op::Plain(s.kind.clone()), s.span);
                        cx.update(&Place::local(*l), false, &mut stmts, s.span);
                    }
                }
            }
            let mut term = b.term.clone();
            match &b.term {
                Terminator::Switch { discr, .. } => cx.move_op(discr, &mut stmts, b.term_span),
                Terminator::Call { callee, args, .. } => {
                    if let Callee::Value(o) = callee {
                        cx.move_op(o, &mut stmts, b.term_span);
                    }
                    for o in args {
                        cx.move_op(o, &mut stmts, b.term_span);
                    }
                }
                _ => {}
            }
            // Call destination becomes initialized only on the normal return
            // edge. A bridge avoids setting flags on other predecessors.
            if let Terminator::Call { dest, next: Some(next), .. } = &mut term {
                let mut updates = Vec::new();
                cx.update(dest, true, &mut updates, b.term_span);
                if !updates.is_empty() {
                    let bridge = BlockId((f.blocks.len() + bridge_count) as u32);
                    bridge_count += 1;
                    let edge = post::Block { stmts: updates, term: Terminator::Goto(*next), term_span: b.term_span };
                    *next = bridge;
                    blocks.push((post::Block { stmts, term, term_span: b.term_span }, Some(edge)));
                    continue;
                }
            }
            blocks.push((post::Block { stmts, term, term_span: b.term_span }, None));
        }
        let mut final_blocks: Vec<_> = blocks.iter().map(|(b, _)| b.clone()).collect();
        final_blocks.extend(blocks.into_iter().filter_map(|(_, bridge)| bridge));
        let mut decl = f.clone();
        decl.blocks.clear();
        (Some(post::Function { decl, flags: cx.flags, blocks: final_blocks }), errors)
    });
    for (function, mut function_errors) in elaborated {
        errors.append(&mut function_errors);
        out.functions.extend(function);
    }
    if errors.is_empty() {
        errors.extend(post::verify(&out, t));
    }
    // Verified source async bodies become physical frames (ADR 0037). Without
    // async bodies lowering changes nothing, and its checks are part of the
    // verification above, so re-verifying the same program is skipped.
    let has_async = out.functions.iter().any(|f| f.decl.asynchronous.is_some() && !f.blocks.is_empty());
    if errors.is_empty() && has_async {
        errors.extend(tarn_ir::async_frame::lower(&mut out, t));
        if errors.is_empty() {
            errors.extend(post::verify(&out, t));
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}
struct Cx<'a> {
    f: &'a ir::Function,
    t: &'a Typed,
    flags: Vec<post::FlagKind>,
}
fn flag_place(f: &post::FlagKind) -> &Place {
    match f {
        post::FlagKind::Value(p) | post::FlagKind::Elements(p, _) => p,
    }
}
fn prefix(a: &Place, b: &Place) -> bool {
    a.local == b.local && b.proj.starts_with(&a.proj)
}
fn emit(out: &mut Vec<post::Statement>, op: post::Op, span: tarn_diagnostics::Span) {
    out.push(post::Statement { op, span });
}
fn state(p: &Place, states: &[(Place, InitState)]) -> InitState {
    states.iter().filter(|(q, _)| prefix(q, p)).max_by_key(|(q, _)| q.proj.len()).map(|(_, s)| *s).unwrap_or(InitState::Dead)
}
impl Cx<'_> {
    fn flag(&mut self, kind: post::FlagKind) -> post::FlagId {
        if let Some(i) = self.flags.iter().position(|x| *x == kind) {
            return post::FlagId(i as u32);
        }
        let id = post::FlagId(self.flags.len() as u32);
        self.flags.push(kind);
        id
    }
    fn plan(&mut self, p: &Place, ty: &Ty, states: &[(Place, InitState)]) -> Option<post::Drop> {
        if self.t.decls.is_copy(ty) || state(p, states) == InitState::Dead {
            return None;
        }
        if p.proj.is_empty()
            && self.f.local(p.local).kind == LocalKind::IterArray
            && let Ty::Array(_, n) = ty
        {
            let flag = self.flag(post::FlagKind::Elements(p.clone(), *n));
            return Some(post::Drop::Remaining { place: p.clone(), flag });
        }
        if p.proj.is_empty()
            && let LocalKind::IterVec(cursor) = self.f.local(p.local).kind
        {
            // The cursor is assigned before the vector, so it is live wherever
            // the vector is (ADR 0086).
            let d = post::Drop::RemainingVec { place: p.clone(), cursor };
            return Some(if state(p, states) == InitState::Maybe {
                post::Drop::Guard(self.flag(post::FlagKind::Value(p.clone())), Box::new(d))
            } else {
                d
            });
        }
        let own = state(p, states);
        // Two Maybe bits do not imply correlation: the parent can be
        // wholly moved on one edge and partially moved on another. Never
        // merge them into a single whole-value guard.
        let split = states.iter().any(|(q, s)| prefix(p, q) && q.proj.len() > p.proj.len() && *s != InitState::Live);
        let d = if split {
            match ty {
                Ty::Adt(s, args) if self.t.decls.structs.contains_key(s) => {
                    let def = &self.t.decls.structs[s];
                    let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                    let fields: Vec<_> = def.fields.iter().map(|f| tarn_types::subst(&f.ty, &map)).collect();
                    let fields =
                        fields.iter().enumerate().filter_map(|(i, ty)| self.plan(&p.project(Proj::Field(i as u32)), ty, states).map(|d| (i as u32, d))).collect::<Vec<_>>();
                    if fields.is_empty() {
                        return None;
                    }
                    // Each child has its own init test; a partial struct never
                    // executes a complete parent drop.
                    return Some(post::Drop::Fields { place: p.clone(), fields });
                }
                Ty::Adt(s, args) if self.t.decls.enums.contains_key(s) => {
                    let def = &self.t.decls.enums[s];
                    let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                    let variants: Vec<Vec<_>> = def.variants.iter().map(|v| v.fields.iter().map(|ty| tarn_types::subst(ty, &map)).collect()).collect();
                    let variants = variants
                        .iter()
                        .enumerate()
                        .map(|(v, fields)| {
                            fields
                                .iter()
                                .enumerate()
                                .filter_map(|(i, ty)| self.plan(&p.project(Proj::Downcast(v as u32)).project(Proj::Field(i as u32)), ty, states).map(|d| (i as u32, d)))
                                .collect()
                        })
                        .collect();
                    post::Drop::Variants { place: p.clone(), variants }
                }
                _ => post::Drop::Value(p.clone()),
            }
        } else {
            post::Drop::Value(p.clone())
        };
        if own == InitState::Maybe {
            let flag = self.flag(post::FlagKind::Value(p.clone()));
            Some(post::Drop::Guard(flag, Box::new(d)))
        } else {
            Some(d)
        }
    }
    fn update(&self, p: &Place, value: bool, out: &mut Vec<post::Statement>, span: tarn_diagnostics::Span) {
        for (i, flag) in self.flags.iter().enumerate() {
            if prefix(p, flag_place(flag)) {
                emit(out, post::Op::Set(post::FlagId(i as u32), value), span);
            }
        }
    }
    fn move_op(&self, o: &Operand, out: &mut Vec<post::Statement>, span: tarn_diagnostics::Span) {
        if let Operand::Move(p) = o {
            self.update(p, false, out, span);
            if let Some(Proj::Index(idx)) = p.proj.first() {
                for (i, f) in self.flags.iter().enumerate() {
                    if matches!(f, post::FlagKind::Elements(q, _) if q.local == p.local) {
                        emit(out, post::Op::ClearElement(post::FlagId(i as u32), *idx), span);
                    }
                }
            }
        }
    }
}
fn rv_ops<'a>(rv: &'a Rvalue, out: &mut Vec<&'a Operand>) {
    match rv {
        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => out.push(o),
        Rvalue::Binary(_, a, b) => out.extend([a, b]),
        Rvalue::Aggregate(_, os) => out.extend(os),
        Rvalue::SliceRef { start, end, .. } => out.extend(start.iter().chain(end.iter())),
        _ => {}
    }
}
