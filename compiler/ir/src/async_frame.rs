//! Physical async frames (ADR 0037).
//!
//! Ownership phases check a source async body as an ordinary CFG in which
//! `Suspend { resume, abandon }` is an explicit edge, and drop elaboration
//! makes every destruction explicit on both edges. This pass runs only after
//! that verified elaboration and makes no ownership decision: it relocates
//! the values that must survive a suspension into a stable heap frame and
//! turns the suspension edges into an explicit state machine:
//!
//! ```text
//! bb0 (dispatch): switch state
//!     0       -> old entry (flag setup), then abandon ? A0 : R0
//!     k       -> abandon ? Ak : Rk
//!     done    -> abandon ? return : panic "polled after completion"
//! Suspend k   -> state = k; _0 = Pending; return
//! Return      -> _0 = Ready(result); state = done; return
//! Abandon     -> state = done; _0 = Pending; return
//! ```
//!
//! The backend only places the listed locals and every drop flag at fixed
//! frame offsets; it never decides which value is initialized or destroyed.
use crate::post_drop::{self as post, Drop, Op};
use crate::*;
use tarn_types::{IntTy, Ty, Typed};

const POLLED_AFTER_COMPLETION: &str = "async computation polled after completion";

pub fn lower(p: &mut post::Program, t: &Typed) -> Vec<String> {
    let mut errors = Vec::new();
    for f in &mut p.functions {
        if f.decl.asynchronous.is_some() && !f.blocks.is_empty() {
            if let Err(e) = lower_function(f, t) {
                errors.push(format!("{}: {e}", f.decl.name));
            }
        }
    }
    errors.extend(verify(p, t));
    errors
}

fn lower_function(f: &mut post::Function, t: &Typed) -> Result<(), String> {
    let info = f.decl.asynchronous.clone().ok_or("missing async metadata")?;
    let progress = t.decls.exec_progress.ok_or("missing trusted Progress")?;
    let def = t.decls.enums.get(&progress).ok_or("missing Progress layout")?;
    let pending = def.variants.iter().position(|v| v.name == "Pending").ok_or("Progress.Pending")? as u32;
    let ready = def.variants.iter().position(|v| v.name == "Ready").ok_or("Progress.Ready")? as u32;
    if !matches!(f.blocks[0].term, Terminator::Suspend { .. }) {
        return Err("async body does not start suspended".into());
    }
    let params: Vec<LocalId> = f.decl.params().filter(|l| *l != info.waker).collect();
    let stored_values = stored_locals(f, info.waker, &params);

    // Locals added by the physical form.
    let output = f.decl.ret.clone();
    let span = f.decl.span;
    let mut add = |ty: Ty, name: &str| {
        f.decl.locals.push(LocalDecl { ty, kind: LocalKind::Temp, name: Some(name.into()), symbol: None, mutable: true, span });
        LocalId(f.decl.locals.len() as u32 - 1)
    };
    let result = add(output.clone(), "async result");
    let state = add(Ty::Int(IntTy::U32), "async state");
    let abandon = add(Ty::Bool, "async abandon");
    rename_return(f, result);
    let progress_ty = Ty::Adt(progress, vec![output.clone()]);
    f.decl.locals[RETURN.0 as usize].ty = progress_ty.clone();
    f.decl.ret = progress_ty;

    // Number suspension points in block order: deterministic state IDs.
    let suspends: Vec<usize> = (0..f.blocks.len()).filter(|&b| matches!(f.blocks[b].term, Terminator::Suspend { .. })).collect();
    let done = suspends.len() as u32;
    let st = |v: u32| Op::Plain(StatementKind::Assign(Place::local(state), Rvalue::Use(Operand::Const(Const::Int(v as i128, IntTy::U32)))));
    let pending_value = || Op::Plain(StatementKind::Assign(Place::local(RETURN), Rvalue::Aggregate(Aggregate::Variant(progress, pending, vec![output.clone()]), Vec::new())));
    let s = |op: Op| post::Statement { op, span };

    // The old entry moves to a fresh block; block 0 becomes the dispatcher.
    let entry = BlockId(f.blocks.len() as u32);
    let old_entry = std::mem::replace(&mut f.blocks[0], post::Block { stmts: Vec::new(), term: Terminator::Unreachable, term_span: span });
    f.blocks.push(old_entry);
    let suspends: Vec<usize> = suspends.into_iter().map(|b| if b == 0 { entry.0 as usize } else { b }).collect();
    let mut cases = Vec::new();
    for (k, &b) in suspends.iter().enumerate() {
        let Terminator::Suspend { resume, abandon: on_abandon } = f.blocks[b].term.clone() else { unreachable!() };
        let choose = Terminator::Switch { discr: Operand::Copy(Place::local(abandon)), cases: vec![(1, on_abandon)], otherwise: resume };
        if k == 0 {
            // Unstarted: run entry flag setup, then begin or abandon.
            f.blocks[b].term = choose;
            cases.push((0, entry));
        } else {
            let blk = &mut f.blocks[b];
            blk.stmts.push(s(st(k as u32)));
            blk.stmts.push(s(pending_value()));
            blk.term = Terminator::Return;
            f.blocks.push(post::Block { stmts: Vec::new(), term: choose, term_span: span });
            cases.push((k as i128, BlockId(f.blocks.len() as u32 - 1)));
        }
    }
    for b in 0..f.blocks.len() {
        match f.blocks[b].term {
            Terminator::Return if b != 0 && !suspends.contains(&b) => {
                let ready_value = Rvalue::Aggregate(Aggregate::Variant(progress, ready, vec![output.clone()]), vec![Operand::Move(Place::local(result))]);
                f.blocks[b].stmts.push(s(Op::Plain(StatementKind::Assign(Place::local(RETURN), ready_value))));
                f.blocks[b].stmts.push(s(st(done)));
            }
            Terminator::Abandon => {
                f.blocks[b].stmts.push(s(st(done)));
                f.blocks[b].stmts.push(s(pending_value()));
                f.blocks[b].term = Terminator::Return;
            }
            _ => {}
        }
    }
    // Completed frames hold nothing: destruction returns, polling aborts.
    let finished = BlockId(f.blocks.len() as u32);
    let quit = BlockId(finished.0 + 1);
    let fault = BlockId(finished.0 + 2);
    let message = add_local(f, Ty::Void, "async fault");
    f.blocks.push(post::Block { stmts: Vec::new(), term: Terminator::Switch { discr: Operand::Copy(Place::local(abandon)), cases: vec![(1, quit)], otherwise: fault }, term_span: span });
    f.blocks.push(post::Block { stmts: vec![s(pending_value())], term: Terminator::Return, term_span: span });
    f.blocks.push(post::Block {
        stmts: Vec::new(),
        term: Terminator::Call {
            callee: Callee::Builtin(Builtin::Panic),
            args: vec![Operand::Const(Const::Str(POLLED_AFTER_COMPLETION.into()))],
            arg_spans: vec![span],
            dest: Place::local(message),
            next: None,
            spawn: false,
        },
        term_span: span,
    });
    f.blocks[0].term = Terminator::Switch { discr: Operand::Copy(Place::local(state)), cases, otherwise: finished };

    let mut stored = params.clone();
    stored.push(state);
    stored.extend(stored_values.into_iter().filter(|l| !params.contains(l)));
    f.decl.param_count = 0;
    f.decl.asynchronous = Some(AsyncInfo { waker: info.waker, frame: Some(AsyncFrame { stored, params, state, abandon, output, result, done }) });
    Ok(())
}

fn add_local(f: &mut post::Function, ty: Ty, name: &str) -> LocalId {
    let span = f.decl.span;
    f.decl.locals.push(LocalDecl { ty, kind: LocalKind::Temp, name: Some(name.into()), symbol: None, mutable: false, span });
    LocalId(f.decl.locals.len() as u32 - 1)
}

// ---------------------------------------------------------------- stored locals

/// Values that must survive a suspension: everything live on entry to a
/// resume or abandonment edge (abandonment destruction is a use), plus every
/// address-taken local, since a reference held across the suspension may
/// point into it. Parameters are always stored: construction writes them.
/// The waker is supplied again by every poll and is never stored.
fn stored_locals(f: &post::Function, waker: LocalId, params: &[LocalId]) -> Vec<LocalId> {
    let n = f.decl.locals.len();
    let nb = f.blocks.len();
    let mut live_in = vec![vec![false; n]; nb];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut live = vec![false; n];
            for s in f.blocks[b].term.successors() {
                for (x, y) in live.iter_mut().zip(&live_in[s.0 as usize]) {
                    *x |= *y;
                }
            }
            term_uses(&f.blocks[b].term, &mut live);
            for st in f.blocks[b].stmts.iter().rev() {
                op_back(&st.op, f, &mut live);
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
    }
    let mut keep = vec![false; n];
    for blk in &f.blocks {
        if let Terminator::Suspend { resume, abandon } = blk.term {
            for b in [resume, abandon] {
                for (k, x) in keep.iter_mut().zip(&live_in[b.0 as usize]) {
                    *k |= *x;
                }
            }
        }
        for st in &blk.stmts {
            if let Op::Plain(StatementKind::Assign(_, Rvalue::Ref(_, p) | Rvalue::SliceRef { base: p, .. })) = &st.op
                && p.proj.first() != Some(&Proj::Deref)
            {
                keep[p.local.0 as usize] = true;
            }
            // A borrowed closure's environment lives with its storage witness:
            // a closure held across a suspension must not point into the
            // per-poll native stack.
            if let Op::Plain(StatementKind::Assign(_, Rvalue::Aggregate(Aggregate::Closure(_, Some(p)), _))) = &st.op {
                keep[p.local.0 as usize] = true;
            }
        }
    }
    for p in params {
        keep[p.0 as usize] = true;
    }
    keep[waker.0 as usize] = false;
    keep[RETURN.0 as usize] = false;
    (0..n).filter(|&l| keep[l]).map(|l| LocalId(l as u32)).collect()
}

fn place_reads(p: &Place, live: &mut [bool]) {
    live[p.local.0 as usize] = true;
    for pr in &p.proj {
        if let Proj::Index(i) = pr {
            live[i.0 as usize] = true;
        }
    }
}

fn operand_reads(o: &Operand, live: &mut [bool]) {
    if let Operand::Copy(p) | Operand::Move(p) = o {
        place_reads(p, live);
    }
}

fn rvalue_reads(rv: &Rvalue, live: &mut [bool]) {
    match rv {
        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => operand_reads(o, live),
        Rvalue::Binary(_, a, b) => {
            operand_reads(a, live);
            operand_reads(b, live);
        }
        Rvalue::Aggregate(kind, os) => {
            os.iter().for_each(|o| operand_reads(o, live));
            if let Aggregate::Closure(_, Some(p)) = kind {
                place_reads(p, live);
            }
        }
        Rvalue::Ref(_, p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => place_reads(p, live),
        Rvalue::SliceRef { base, start, end, .. } => {
            place_reads(base, live);
            start.iter().chain(end.iter()).for_each(|o| operand_reads(o, live));
        }
    }
}

fn drop_reads(d: &Drop, live: &mut [bool]) {
    match d {
        Drop::Value(p) | Drop::Fields { place: p, .. } | Drop::Variants { place: p, .. } | Drop::Remaining { place: p, .. } => place_reads(p, live),
        Drop::RemainingVec { place, cursor } => {
            place_reads(place, live);
            live[cursor.0 as usize] = true;
        }
        Drop::Guard(_, d) => drop_reads(d, live),
    }
}

/// Flag-guarded drops and partial plans read their root as a whole: keep it.
fn op_back(op: &Op, f: &post::Function, live: &mut [bool]) {
    match op {
        Op::Plain(StatementKind::Assign(dest, rv)) => {
            if dest.proj.is_empty() {
                live[dest.local.0 as usize] = false;
            } else {
                place_reads(dest, live);
            }
            rvalue_reads(rv, live);
        }
        Op::Plain(StatementKind::StorageLive(_)) => {}
        Op::Plain(StatementKind::StorageDead(l)) => live[l.0 as usize] = false,
        Op::Plain(StatementKind::Drop(p)) => place_reads(p, live),
        Op::Set(..) => {}
        Op::ClearElement(id, i) => {
            live[i.0 as usize] = true;
            if let Some(post::FlagKind::Elements(p, _)) = f.flags.get(id.0 as usize) {
                place_reads(p, live);
            }
        }
        Op::Destroy(d) => drop_reads(d, live),
    }
}

fn term_uses(term: &Terminator, live: &mut [bool]) {
    match term {
        Terminator::Call { callee, args, dest, .. } => {
            if dest.proj.is_empty() {
                live[dest.local.0 as usize] = false;
            } else {
                place_reads(dest, live);
            }
            args.iter().for_each(|a| operand_reads(a, live));
            if let Callee::Value(o) = callee {
                operand_reads(o, live);
            }
        }
        Terminator::Switch { discr, .. } => operand_reads(discr, live),
        Terminator::Return => live[RETURN.0 as usize] = true,
        _ => {}
    }
}

// ---------------------------------------------------------------- renaming

/// The source result `_0: T` becomes an ordinary local; `_0` is `Progress<T>`.
fn rename_return(f: &mut post::Function, to: LocalId) {
    let fix = |p: &mut Place| {
        if p.local == RETURN {
            p.local = to;
        }
    };
    fn op(o: &mut Operand, fix: &impl Fn(&mut Place)) {
        if let Operand::Copy(p) | Operand::Move(p) = o {
            fix(p);
        }
    }
    fn drop(d: &mut Drop, fix: &impl Fn(&mut Place)) {
        match d {
            Drop::Value(p) | Drop::Remaining { place: p, .. } | Drop::RemainingVec { place: p, .. } => fix(p),
            Drop::Guard(_, d) => drop(d, fix),
            Drop::Fields { place, fields } => {
                fix(place);
                fields.iter_mut().for_each(|(_, d)| drop(d, fix));
            }
            Drop::Variants { place, variants } => {
                fix(place);
                variants.iter_mut().flatten().for_each(|(_, d)| drop(d, fix));
            }
        }
    }
    for flag in &mut f.flags {
        match flag {
            post::FlagKind::Value(p) | post::FlagKind::Elements(p, _) => fix(p),
        }
    }
    for b in &mut f.blocks {
        for s in &mut b.stmts {
            match &mut s.op {
                Op::Plain(StatementKind::Assign(dest, rv)) => {
                    fix(dest);
                    match rv {
                        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) | Rvalue::Coerce(_, o, _) => op(o, &fix),
                        Rvalue::Binary(_, a, b) => {
                            op(a, &fix);
                            op(b, &fix);
                        }
                        Rvalue::Aggregate(kind, os) => {
                            os.iter_mut().for_each(|o| op(o, &fix));
                            if let Aggregate::Closure(_, Some(p)) = kind {
                                fix(p);
                            }
                        }
                        Rvalue::Ref(_, p) | Rvalue::Discriminant(p) | Rvalue::Len(p) => fix(p),
                        Rvalue::SliceRef { base, start, end, .. } => {
                            fix(base);
                            start.iter_mut().chain(end.iter_mut()).for_each(|o| op(o, &fix));
                        }
                    }
                }
                Op::Plain(StatementKind::StorageLive(l) | StatementKind::StorageDead(l)) => {
                    if *l == RETURN {
                        *l = to;
                    }
                }
                Op::Plain(StatementKind::Drop(p)) => fix(p),
                Op::Destroy(d) => drop(d, &fix),
                Op::Set(..) | Op::ClearElement(..) => {}
            }
        }
        match &mut b.term {
            Terminator::Call { callee, args, dest, .. } => {
                fix(dest);
                args.iter_mut().for_each(|a| op(a, &fix));
                if let Callee::Value(o) = callee {
                    op(o, &fix);
                }
            }
            Terminator::Switch { discr, .. } => op(discr, &fix),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- verification

/// Structural invariants of the physical form. Initialization and drop
/// correctness were verified on the source form before this pass.
pub fn verify(p: &post::Program, t: &Typed) -> Vec<String> {
    let mut errors = Vec::new();
    for f in &p.functions {
        // Before lowering, the source form legitimately contains suspensions.
        let source = f.decl.asynchronous.as_ref().is_some_and(|a| a.frame.is_none());
        let mut err = |m: &str| errors.push(format!("{}: {m}", f.decl.name));
        for b in &f.blocks {
            if !source && matches!(b.term, Terminator::Suspend { .. } | Terminator::Abandon) {
                err("suspension edge outside an async body");
            }
            for s in &b.stmts {
                if let Op::Plain(StatementKind::Assign(_, Rvalue::Aggregate(Aggregate::AsyncFrame(id, _), ops))) = &s.op {
                    let target = p.functions.get(id.0 as usize).and_then(|g| g.decl.asynchronous.as_ref());
                    let arity = |g: &post::Function| g.decl.params().filter(|l| Some(*l) != g.decl.asynchronous.as_ref().map(|a| a.waker)).count();
                    match target.map(|a| &a.frame) {
                        Some(Some(frame)) if frame.params.len() == ops.len() => {}
                        Some(None) if arity(&p.functions[id.0 as usize]) == ops.len() => {}
                        _ => err("async construction does not match its async body"),
                    }
                }
            }
        }
        let Some(info) = &f.decl.asynchronous else { continue };
        let Some(frame) = &info.frame else { continue };
        if f.blocks.is_empty() {
            continue;
        }
        let n = f.decl.locals.len() as u32;
        let valid_local = |l: LocalId| l.0 < n;
        if !frame.stored.iter().all(|l| valid_local(*l))
            || !frame.params.iter().all(|l| frame.stored.contains(l))
            || !frame.stored.contains(&frame.state)
            || frame.stored.contains(&info.waker)
            || frame.stored.contains(&frame.abandon)
            || frame.stored.contains(&RETURN)
        {
            err("invalid frame placement");
        }
        let mut seen = std::collections::HashSet::new();
        if !frame.stored.iter().all(|l| seen.insert(*l)) {
            err("duplicate frame slot");
        }
        if f.decl.local(frame.state).ty != Ty::Int(IntTy::U32) || f.decl.local(frame.abandon).ty != Ty::Bool || f.decl.local(frame.result).ty != frame.output {
            err("invalid frame control locals");
        }
        if !matches!(&f.decl.ret, Ty::Adt(id, args) if Some(*id) == t.decls.exec_progress && *args == vec![frame.output.clone()]) {
            err("async poll result is not Progress<T>");
        }
        // Dispatch covers every state exactly once, in order.
        match &f.blocks[0].term {
            Terminator::Switch { discr: Operand::Copy(d), cases, .. } if d.local == frame.state && d.proj.is_empty() => {
                if cases.len() as u32 != frame.done || cases.iter().enumerate().any(|(k, (v, _))| *v != k as i128) {
                    err("state dispatch does not cover every suspension");
                }
            }
            _ => err("frame entry is not a state dispatch"),
        }
        // Every state write is a known state.
        for b in &f.blocks {
            for s in &b.stmts {
                if let Op::Plain(StatementKind::Assign(p, Rvalue::Use(Operand::Const(Const::Int(v, _))))) = &s.op
                    && p.local == frame.state
                    && (*v < 0 || *v > frame.done as i128)
                {
                    err("state write outside the state machine");
                }
            }
        }
    }
    errors
}
