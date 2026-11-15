//! Instantiate ownership obligations over generic IR without monomorphizing
//! code or layouts. An instance is a finite vector of linearity bits.
use std::collections::{HashSet, VecDeque};
use tarn_diagnostics::Diagnostic;
use tarn_ir::{Callee, Const, FunctionId, Operand, Program, Rvalue, StatementKind, Terminator};
use tarn_resolve::Resolved;
use tarn_types::{ParamId, Typed};

pub(crate) fn check_instances(p: &Program, r: &Resolved, t: &Typed) -> Vec<Diagnostic> {
    if t.decls.must_consume.is_empty() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut work: VecDeque<_> = p
        .functions
        .iter()
        .map(|f| (f.id, vec![false; f.generics.len()]))
        .collect();
    let mut diagnostics = Vec::new();
    while let Some((id, bits)) = work.pop_front() {
        if !seen.insert((id, bits.clone())) {
            continue;
        }
        let f = p.function(id);
        let linear: HashSet<ParamId> = f
            .generics
            .iter()
            .zip(&bits)
            .filter(|(_, b)| **b)
            .map(|(p, _)| *p)
            .collect();
        if bits.iter().any(|b| *b) {
            let (_, ds) = crate::moves::check_function_linear(f, r, t, linear.clone());
            diagnostics.extend(
                ds.into_iter()
                    .filter(|d| d.code == "E4007" || d.code == "E4008"),
            );
        }
        let mut enqueue = |id: FunctionId, args: &[tarn_types::Ty]| {
            let callee = p.function(id);
            if matches!(callee.kind, tarn_ir::FnKind::Intrinsic)
                && args
                    .iter()
                    .any(|a| t.decls.is_must_consume_with(a, &linear))
            {
                diagnostics.push(Diagnostic::error("E4008", "unsupported_linear_operation", "native generic containers and intrinsics cannot own must-consume values")
                    .primary(f.span, "this instance would let native code erase a resource obligation")
                    .help("use a checked struct or enum container; Vec and other native element-dropping containers do not yet support must-consume elements"));
            }
            let bits = callee
                .generics
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    args.get(i)
                        .is_some_and(|a| t.decls.is_must_consume_with(a, &linear))
                })
                .collect();
            work.push_back((id, bits));
        };
        fn op(o: &Operand, enqueue: &mut impl FnMut(FunctionId, &[tarn_types::Ty])) {
            if let Operand::Const(Const::Fn(id, args)) = o {
                enqueue(*id, args);
            }
        }
        for b in &f.blocks {
            for s in &b.stmts {
                if let StatementKind::Assign(_, rv) = &s.kind {
                    match rv {
                        Rvalue::Aggregate(kind, os) => {
                            if let tarn_ir::Aggregate::Closure(id, _) = kind {
                                let closure = p.function(*id);
                                let inherited: Vec<_> = closure
                                    .generics
                                    .iter()
                                    .map(|g| tarn_types::Ty::Param(*g))
                                    .collect();
                                enqueue(*id, &inherited);
                            }
                            for o in os {
                                op(o, &mut enqueue);
                            }
                        }
                        Rvalue::Use(o)
                        | Rvalue::Unary(_, o)
                        | Rvalue::Cast(o, _)
                        | Rvalue::Coerce(_, o, _) => op(o, &mut enqueue),
                        Rvalue::Binary(_, a, b) => {
                            op(a, &mut enqueue);
                            op(b, &mut enqueue);
                        }
                        _ => {}
                    }
                }
            }
            if let Terminator::Call { callee, args, .. } = &b.term {
                if let Callee::Fn(id, type_args) = callee {
                    enqueue(*id, type_args);
                }
                if let Callee::TaskSpawn {
                    worker,
                    drop_result,
                    type_args,
                    ..
                } = callee
                {
                    enqueue(*worker, type_args);
                    enqueue(*drop_result, type_args);
                }
                if let Callee::Value(o) = callee {
                    op(o, &mut enqueue);
                }
                for o in args {
                    op(o, &mut enqueue);
                }
            }
        }
    }
    // The same source operation can be reached through several bit instances.
    diagnostics.sort_by_key(|d| {
        (
            d.primary_span().map(|s| (s.file, s.start, s.end)),
            d.code,
            d.message.clone(),
        )
    });
    diagnostics.dedup();
    diagnostics
}
