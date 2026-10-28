//! Executable destruction IR (ADR 0026). No implicit initialization tests.
//! Guards and variant dispatch have explicit runtime semantics, so ordinary
//! CFG edges remain intact; only successful call edges need flag bridges.
use crate::*;
use tarn_resolve::Resolved;
use tarn_types::{Ty, Typed};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlagId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagKind {
    /// One initialization bit for a structural move path.
    Value(Place),
    /// One bit per array element, for consuming iteration only.
    Elements(Place, u64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Drop {
    /// Unconditional destruction of this complete, initialized value.
    Value(Place),
    /// Test this explicit bit; execute the nested destruction only if true.
    Guard(FlagId, Box<Drop>),
    /// Partial aggregate: destroy only listed children in declaration order.
    Fields { place: Place, fields: Vec<(u32, Drop)> },
    /// Read the live enum's tag and execute only that variant's field plan.
    Variants { place: Place, variants: Vec<Vec<(u32, Drop)>> },
    /// Drop live bitmap elements in increasing index order; never read holes.
    Remaining { place: Place, flag: FlagId },
}

#[derive(Clone, Debug)]
pub enum Op {
    /// Assign/StorageLive/StorageDead only; abstract Drop is forbidden.
    Plain(StatementKind),
    /// Set a boolean bit, or fill every element of an Elements bitmap.
    Set(FlagId, bool),
    ClearElement(FlagId, LocalId),
    Destroy(Drop),
}

#[derive(Clone, Debug)]
pub struct Statement {
    pub op: Op,
    pub span: Span,
}
#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Statement>,
    pub term: Terminator,
    pub term_span: Span,
}
#[derive(Clone, Debug)]
pub struct Function {
    /// Signature and locals; `decl.blocks` is empty, not a second CFG.
    pub decl: crate::Function,
    pub flags: Vec<FlagKind>,
    pub blocks: Vec<Block>,
}
#[derive(Debug, Default)]
pub struct Program {
    pub functions: Vec<Function>,
    pub by_symbol: HashMap<SymbolId, FunctionId>,
}

/// Type of a structural place, checked rather than indexed blindly. Used by
/// the post-drop verifier and elaborator, including variant payload fields.
pub fn place_ty(f: &crate::Function, t: &Typed, p: &Place) -> Option<Ty> {
    let mut ty = f.locals.get(p.local.0 as usize)?.ty.clone();
    let mut payload = None;
    for pr in &p.proj {
        match pr {
            Proj::Deref => {
                let Ty::Ref(_, x) = ty else { return None };
                ty = *x;
            }
            Proj::Index(i) => {
                if f.locals.get(i.0 as usize)?.ty != Ty::Int(tarn_types::IntTy::Usize) {
                    return None;
                }
                ty = match ty {
                    Ty::Array(x, _) | Ty::Slice(x) => *x,
                    _ => return None,
                };
            }
            Proj::Downcast(v) => {
                let Ty::Adt(s, args) = &ty else { return None };
                let def = t.decls.enums.get(s)?;
                let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                payload = Some(def.variants.get(*v as usize)?.fields.iter().map(|x| tarn_types::subst(x, &map)).collect::<Vec<_>>());
            }
            Proj::Field(i) => {
                if let Some(xs) = payload.take() {
                    ty = xs.get(*i as usize)?.clone();
                } else {
                    let Ty::Adt(s, args) = &ty else { return None };
                    let def = t.decls.structs.get(s)?;
                    let map = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                    ty = tarn_types::subst(&def.fields.get(*i as usize)?.ty, &map);
                }
            }
        }
    }
    Some(ty)
}

/// Compiler invariant verifier, including must-initialized flag dataflow.
pub fn verify(p: &Program, t: &Typed) -> Vec<String> {
    let mut mirror = crate::Program { functions: Vec::new(), by_symbol: p.by_symbol.clone() };
    for f in &p.functions {
        let mut decl = f.decl.clone();
        decl.blocks = f
            .blocks
            .iter()
            .map(|b| BasicBlock {
                stmts: b
                    .stmts
                    .iter()
                    .filter_map(|s| match &s.op {
                        Op::Plain(kind) => Some(crate::Statement { kind: kind.clone(), span: s.span }),
                        _ => None,
                    })
                    .collect(),
                term: b.term.clone(),
                term_span: b.term_span,
            })
            .collect();
        mirror.functions.push(decl);
    }
    let mut errors = crate::verify(&mirror);
    errors.extend(crate::network_abi::verify(t));
    errors.extend(crate::filesystem_abi::verify(t));
    errors.extend(crate::process_abi::verify(t));
    errors.extend(crate::async_frame::verify(p, t));
    // Only this trusted, structurally verified primitive may read an owned
    // scalar from a raw pointer without inventing reference provenance.
    for (id, write) in [(t.decls.ffi_read, false), (t.decls.ffi_write, true)] {
        if let Some(id) = id {
            let valid = t.decls.fns.get(&id).is_some_and(|sig| {
                let [parameter] = sig.generics.as_slice() else { return false };
                let value = Ty::Param(*parameter);
                let params = if write { vec![Ty::Ptr(true, Box::new(value.clone())), value.clone()] }
                    else { vec![Ty::Ptr(false, Box::new(value.clone()))] };
                sig.abi.as_deref() == Some("intrinsic") && sig.receiver.is_none() && !sig.is_async
                    && sig.params == params && sig.ret == if write { Ty::Void } else { value }
                    && (write || sig.contract.result == tarn_types::ResultContract::Copy)
                    && t.decls.copy.is_some_and(|copy| t.decls.bounds.get(parameter).is_some_and(|bounds| bounds.contains(&copy)))
            });
            if !valid { errors.push("invalid scalar FFI memory declaration".into()); }
        }
    }
    for f in &p.functions {
        let mut err = |msg: String| errors.push(format!("{}: {msg}", f.decl.name));
        if !f.decl.blocks.is_empty() {
            err("post-drop metadata contains a second CFG".into());
        }
        if let FnKind::Closure { captures, environment, owned, consumes, destructor, destructor_body, .. } = &f.decl.kind {
            if captures.len() != environment.len() || captures.len() > f.decl.param_count as usize || captures.len() >= f.decl.locals.len() {
                err("invalid closure environment arity".into());
            } else {
                for ((mode, ty), param) in captures.iter().zip(environment).zip(f.decl.params()) {
                    let actual = &f.decl.local(param).ty;
                    let valid = if *owned {
                        *mode == CaptureMode::Move && if *consumes { actual == ty } else { matches!(actual, Ty::Ref(_, inner) if inner.as_ref() == ty) }
                    } else {
                        actual == ty && matches!((mode, ty), (CaptureMode::SharedBorrow, Ty::Ref(false, _)) | (CaptureMode::MutableBorrow, Ty::Ref(true, _)))
                    };
                    if !valid {
                        err("invalid closure capture representation".into());
                    }
                }
            }
            if *destructor_body && (!*owned || !*consumes || destructor.is_some() || f.decl.ret != Ty::Void || f.decl.param_count as usize != captures.len()) {
                err("invalid environment destruction body".into());
            }
            if let Some(id) = destructor {
                let valid = *owned
                    && p.functions.get(id.0 as usize).is_some_and(|d| {
                        d.decl.ret == Ty::Void
                            && d.decl.param_count as usize == environment.len()
                            && matches!(&d.decl.kind, FnKind::Closure { owned: true, consumes: true, destructor: None, destructor_body: true, environment: fields, .. } if fields == environment)
                    });
                if !valid {
                    err("invalid closure environment destruction plan".into());
                }
            } else if *owned && !*destructor_body {
                err("owned environment lacks destruction plan".into());
            }
        }
        for flag in &f.flags {
            let place = match flag {
                FlagKind::Value(p) | FlagKind::Elements(p, _) => p,
            };
            if place_ty(&f.decl, t, place).is_none() {
                err("flag for nonexistent place".into());
            }
            if let FlagKind::Elements(p, n) = flag
                && (!matches!(place_ty(&f.decl, t, p), Some(Ty::Array(_, len)) if len == *n)
                    || !p.proj.is_empty()
                    || f.decl.locals.get(p.local.0 as usize).is_none_or(|l| l.kind != LocalKind::IterArray))
            {
                err("invalid iteration bitmap".into());
            }
        }
        for b in &f.blocks {
            if let Terminator::Call { callee: Callee::Intrinsic(name), args, dest, .. } = &b.term {
                if name == "ffi.callback" {
                    let valid = matches!(args.as_slice(), [Operand::Const(Const::Fn(id, ts))]
                        if ts.is_empty() && p.functions.get(id.0 as usize).is_some_and(|target|
                            target.decl.kind == FnKind::Body && target.decl.generics.is_empty() && target.decl.asynchronous.is_none()))
                        && place_ty(&f.decl, t, dest) == Some(Ty::Ptr(false, Box::new(Ty::Int(tarn_types::IntTy::U8))));
                    if !valid { err("invalid static C callback metadata".into()); }
                }
                // String bridges and operators have a fixed typed ABI. Check
                // it before native execution; no ownership is inferred here.
                let string_ref = Ty::Ref(false, Box::new(Ty::Str));
                let bytes_ref = Ty::Ref(false, Box::new(Ty::Slice(Box::new(Ty::Int(tarn_types::IntTy::U8)))));
                let string_abi = match name.as_str() {
                    "string.bytes" => Some((vec![string_ref.clone()], bytes_ref.clone())),
                    "core.string_from_utf8" => Some((vec![bytes_ref], Ty::Adt(t.prelude.option, vec![Ty::Str]))),
                    "string.add" => Some((vec![string_ref.clone(), string_ref], Ty::Str)),
                    "string.eq" | "string.ne" | "string.lt" | "string.le" | "string.gt" | "string.ge" =>
                        Some((vec![string_ref.clone(), string_ref], Ty::Bool)),
                    _ => None,
                };
                if let Some((params, ret)) = string_abi {
                    let valid = place_ty(&f.decl, t, dest) == Some(ret) && args.len() == params.len()
                        && args.iter().zip(params).all(|(arg, ty)| matches!(arg, Operand::Copy(place) if place_ty(&f.decl, t, place) == Some(ty)));
                    if !valid { err("invalid string intrinsic ABI".into()); }
                }
                let task = name.strip_prefix("runtime.").is_some_and(|n| tarn_types::TASK_INTRINSICS.contains(&n));
                if task && t.decls.task_intrinsics.len() != tarn_types::TASK_INTRINSICS.len() {
                    err("missing task intrinsic declarations".into());
                }
                if tarn_types::stdlib_intrinsic_operation(name).is_some() && !task {
                    let valid = t.decls.net_intrinsics.get(name).or_else(|| t.decls.fs_intrinsics.get(name)).or_else(|| t.decls.process_intrinsics.get(name)).and_then(|id| t.decls.fns.get(id)).is_some_and(|sig| {
                        sig.abi.as_deref() == Some("intrinsic") && sig.generics.is_empty() && sig.receiver.is_none()
                        && sig.params.len() == args.len() && place_ty(&f.decl, t, dest) == Some(sig.ret.clone())
                        && args.iter().zip(&sig.params).all(|(arg, ty)| match arg {
                            Operand::Copy(place) => t.decls.is_copy(ty) && place_ty(&f.decl, t, place).as_ref() == Some(ty),
                            Operand::Move(place) => place_ty(&f.decl, t, place).as_ref() == Some(ty),
                            Operand::Const(crate::Const::Int(_, i)) => *ty == Ty::Int(*i),
                            Operand::Const(crate::Const::Bool(_)) => *ty == Ty::Bool,
                            Operand::Const(_) => false,
                        })
                    });
                    if !valid { err("invalid network intrinsic metadata".into()); }
                }
            }
            if let Terminator::Call { callee: Callee::TaskSpawn { worker, drop_result, scoped, .. }, args, dest, spawn, .. } = &b.term {
                let valid = p.functions.get(worker.0 as usize).zip(p.functions.get(drop_result.0 as usize)).is_some_and(|(wf, df)| {
                    wf.decl.param_count == 1 && df.decl.param_count == 1 && df.decl.ret == Ty::Void
                    && wf.decl.locals.get(1).is_some_and(|l| matches!(&l.ty, Ty::Fn(_, ps, ret) if ps.is_empty() && **ret == wf.decl.ret))
                    && df.decl.locals.get(1).is_some_and(|l| l.ty == wf.decl.ret)
                    && args.len() == if *scoped { 2 } else { 1 }
                    && (!*scoped || matches!(args.get(1), Some(Operand::Copy(p)) if place_ty(&f.decl, t, p) == Some(Ty::Ref(false, Box::new(Ty::Bool)))
                        && f.blocks.iter().flat_map(|b| &b.stmts).any(|statement| matches!(&statement.op,
                            Op::Plain(crate::StatementKind::Assign(dest, Rvalue::Ref(false, scope)))
                            if dest == p && scope.proj.is_empty() && f.decl.locals.get(scope.local.0 as usize).is_some_and(|l| l.kind == LocalKind::TaskScopeWitness && l.ty == Ty::Bool)))))
                    && match &args[0] { Operand::Move(p) => place_ty(&f.decl, t, p), _ => None } == wf.decl.locals.get(1).map(|l| l.ty.clone())
                    && matches!(place_ty(&f.decl, t, dest), Some(Ty::Adt(id, ts)) if Some(id) == t.decls.task && ts == vec![wf.decl.ret.clone()])
                });
                if !valid || !spawn { err("invalid task worker/result metadata".into()); }
            }
            for s in &b.stmts {
                match &s.op {
                    Op::Plain(StatementKind::Drop(_)) => err("abstract Drop remains".into()),
                    Op::Plain(StatementKind::Assign(_, Rvalue::Aggregate(Aggregate::Closure(id, storage), ops))) => {
                        if let Some(target) = p.functions.get(id.0 as usize) && let FnKind::Closure { owned, environment, .. } = &target.decl.kind {
                            if *owned == storage.is_some() || ops.len() != environment.len() || storage.as_ref().is_some_and(|p| place_ty(&f.decl,t,p) != Some(Ty::Bool)) { err("invalid closure storage or capture arity".into()); }
                        }
                    },
                    Op::Destroy(d) => check_drop(d, f, t, &mut err),
                    Op::Set(id, _) | Op::ClearElement(id, _) if id.0 as usize >= f.flags.len() => err("nonexistent drop flag".into()),
                    Op::ClearElement(id, idx) => {
                        if !matches!(f.flags[id.0 as usize], FlagKind::Elements(..)) || f.decl.locals.get(idx.0 as usize).is_none_or(|l| l.ty != Ty::Int(tarn_types::IntTy::Usize))
                        {
                            err("invalid bitmap index".into());
                        }
                    }
                    _ => {}
                }
            }
        }
        // Physical frames keep flags across polls; their initialization was
        // verified on the source form before frame lowering.
        if f.blocks.is_empty() || f.decl.asynchronous.as_ref().is_some_and(|a| a.frame.is_some()) {
            continue;
        }
        // Intersection at joins, initialized to top except entry. Back-edges
        // cannot supply an initialization missing on the first iteration.
        let n = f.blocks.len();
        let mut ins = vec![vec![true; f.flags.len()]; n];
        ins[0].fill(false);
        loop {
            let mut changed = false;
            for (bi, b) in f.blocks.iter().enumerate() {
                let mut out = ins[bi].clone();
                for s in &b.stmts {
                    if let Op::Set(id, _) = s.op
                        && let Some(x) = out.get_mut(id.0 as usize)
                    {
                        *x = true;
                    }
                }
                for succ in b.term.successors() {
                    let si = succ.0 as usize;
                    if si >= n {
                        continue;
                    }
                    for (x, y) in ins[si].iter_mut().zip(&out) {
                        if *x && !y {
                            *x = false;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut state = ins[bi].clone();
            for s in &b.stmts {
                let mut reads = Vec::new();
                match &s.op {
                    Op::Set(id, _) => {
                        if let Some(x) = state.get_mut(id.0 as usize) {
                            *x = true;
                        }
                    }
                    Op::ClearElement(id, _) => reads.push(*id),
                    Op::Destroy(d) => drop_flags(d, &mut reads),
                    _ => {}
                }
                for id in reads {
                    if !state.get(id.0 as usize).copied().unwrap_or(false) {
                        err(format!("bb{bi}: flag df{} read before initialization", id.0));
                    }
                }
            }
        }
    }
    errors
}

fn drop_flags(d: &Drop, out: &mut Vec<FlagId>) {
    match d {
        Drop::Guard(id, d) => {
            out.push(*id);
            drop_flags(d, out);
        }
        Drop::Fields { fields, .. } => {
            for (_, d) in fields {
                drop_flags(d, out);
            }
        }
        Drop::Variants { variants, .. } => {
            for fs in variants {
                for (_, d) in fs {
                    drop_flags(d, out);
                }
            }
        }
        Drop::Remaining { flag, .. } => out.push(*flag),
        Drop::Value(_) => {}
    }
}

fn root(d: &Drop) -> &Place {
    match d {
        Drop::Guard(_, d) => root(d),
        Drop::Value(p) | Drop::Fields { place: p, .. } | Drop::Variants { place: p, .. } | Drop::Remaining { place: p, .. } => p,
    }
}
fn check_drop(d: &Drop, f: &Function, t: &Typed, err: &mut impl FnMut(String)) {
    let p = root(d);
    let Some(ty) = place_ty(&f.decl, t, p) else {
        err("drop of nonexistent place".into());
        return;
    };
    match d {
        Drop::Value(_) => {
            if t.decls.is_copy(&ty) {
                err("resource drop of Copy place".into());
            }
        }
        Drop::Guard(id, inner) => {
            if !matches!(f.flags.get(id.0 as usize), Some(FlagKind::Value(q)) if q == root(inner)) {
                err("nonexistent or inconsistent guard flag".into());
            }
            check_drop(inner, f, t, err);
        }
        Drop::Remaining { flag, place } => {
            if !matches!(f.flags.get(flag.0 as usize), Some(FlagKind::Elements(q, _)) if q == place) {
                err("nonexistent or inconsistent bitmap flag".into());
            }
        }
        Drop::Fields { place, fields } => {
            if !matches!(&ty, Ty::Adt(s, _) if t.decls.structs.contains_key(s)) {
                err("partial drop of non-struct".into());
            }
            check_fields(place, fields, f, t, err);
        }
        Drop::Variants { place, variants } => {
            if !matches!(&ty, Ty::Adt(s, _) if t.decls.enums.get(s).is_some_and(|e| e.variants.len() == variants.len())) {
                err("inconsistent variant drop".into());
            }
            for (v, fs) in variants.iter().enumerate() {
                check_fields(&place.project(Proj::Downcast(v as u32)), fs, f, t, err);
            }
        }
    }
}
fn check_fields(p: &Place, fs: &[(u32, Drop)], f: &Function, t: &Typed, err: &mut impl FnMut(String)) {
    let mut prev = None;
    for (i, d) in fs {
        if prev.is_some_and(|x| x >= *i) || *root(d) != p.project(Proj::Field(*i)) {
            err("inconsistent partial drop (order, overlap or child)".into());
        }
        prev = Some(*i);
        check_drop(d, f, t, err);
    }
}

pub fn print_program(p: &Program, r: &Resolved, t: &Typed) -> String {
    use std::fmt::Write;
    let context = crate::Program { functions: p.functions.iter().map(|f| f.decl.clone()).collect(), by_symbol: p.by_symbol.clone() };
    let pp = crate::pretty::P { p: &context, r, t };
    let mut out = String::new();
    for f in &p.functions {
        if f.blocks.is_empty() {
            continue;
        }
        let _ = writeln!(out, "fn {} [post-drop] {{", f.decl.name);
        for (i, l) in f.decl.locals.iter().enumerate() {
            let _ = writeln!(out, "    _{i}: {}", pp.ty(&l.ty));
        }
        for (i, flag) in f.flags.iter().enumerate() {
            let text = match flag {
                FlagKind::Value(p) => format!("bool for {}", pp.place(p)),
                FlagKind::Elements(p, n) => format!("bits[{n}] for {}", pp.place(p)),
            };
            let _ = writeln!(out, "    df{i}: {text}");
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            let _ = writeln!(out, "  bb{bi}:");
            for s in &b.stmts {
                let text = match &s.op {
                    Op::Plain(StatementKind::Assign(p, rv)) => {
                        format!("{} = {}", pp.place(p), pp.rvalue(rv))
                    }
                    Op::Plain(StatementKind::StorageLive(l)) => format!("live _{}", l.0),
                    Op::Plain(StatementKind::StorageDead(l)) => format!("dead _{}", l.0),
                    Op::Plain(StatementKind::Drop(_)) => "INVALID abstract drop".into(),
                    Op::Set(id, v) => match f.flags.get(id.0 as usize) {
                        Some(FlagKind::Elements(..)) => format!("df{}.fill({v})", id.0),
                        _ => format!("df{} = {v}", id.0),
                    },
                    Op::ClearElement(id, idx) => format!("df{}[_{}] = false", id.0, idx.0),
                    Op::Destroy(d) => print_drop(d, &pp),
                };
                let _ = writeln!(out, "    {text}");
            }
            let text = match &b.term {
                Terminator::Goto(b) => format!("goto bb{}", b.0),
                Terminator::Switch { discr, cases, otherwise } => {
                    format!("switch {} [{}, _ => bb{}]", pp.operand(discr), cases.iter().map(|(v, b)| format!("{v} => bb{}", b.0)).collect::<Vec<_>>().join(", "), otherwise.0)
                }
                Terminator::Call { callee, args, dest, next, spawn, .. } => format!(
                    "{}{} = call {}({}){}",
                    if *spawn { "spawn " } else { "" },
                    pp.place(dest),
                    pp.callee(callee),
                    pp.ops(args),
                    next.map(|b| format!(" -> bb{}", b.0)).unwrap_or(" -> !".into())
                ),
                Terminator::Return => "return".into(),
                Terminator::Suspend { resume, abandon } => format!("suspend -> [resume: bb{}, abandon: bb{}]", resume.0, abandon.0),
                Terminator::Abandon => "abandon".into(),
                Terminator::Unreachable => "unreachable".into(),
            };
            let _ = writeln!(out, "    {text}");
        }
        out.push_str("}\n\n");
    }
    out
}
fn print_drop(d: &Drop, pp: &crate::pretty::P<'_>) -> String {
    match d {
        Drop::Value(p) => format!("destroy {}", pp.place(p)),
        Drop::Guard(id, d) => format!("if df{} {{ {} }}", id.0, print_drop(d, pp)),
        Drop::Fields { place, fields } => format!("partial {} {{ {} }}", pp.place(place), fields.iter().map(|(_, d)| print_drop(d, pp)).collect::<Vec<_>>().join("; ")),
        Drop::Variants { place, variants } => format!(
            "variants {} {{ {} }}",
            pp.place(place),
            variants.iter().enumerate().map(|(v, fs)| format!("#{v}: {}", fs.iter().map(|(_, d)| print_drop(d, pp)).collect::<Vec<_>>().join("; "))).collect::<Vec<_>>().join(", ")
        ),
        Drop::Remaining { place, flag } => {
            format!("destroy_remaining {} using df{}", pp.place(place), flag.0)
        }
    }
}
