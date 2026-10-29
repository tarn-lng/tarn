//! Bounded first-iteration peeling of verified, scalar-only two-block loops.
//! The copied iteration keeps its original checks. The original loop then has
//! only post-iteration entry states, so ordinary range analysis can prove
//! invariants without assuming anything about the initial caller input.
use crate::*;
use post_drop::{self as post, Op};
use std::collections::HashSet;

const MAX_LOOP_SIZE: usize = 48;
const MAX_GROWTH: usize = 256;
const MAX_LOOPS: usize = 16;

pub fn optimize_scalar_loops(program: &mut post::Program) {
    if std::env::var_os("TARN_NO_RANGE_PROOFS").is_some() {
        return;
    }
    for function in &mut program.functions {
        if peel(function) {
            crate::ranges::prove_post_function(function);
        }
    }
}

fn scalar(function: &post::Function, local: LocalId) -> bool {
    matches!(function.decl.local(local).ty, Ty::Int(_) | Ty::Bool)
}
fn operand(function: &post::Function, operand: &Operand, addressed: &HashSet<LocalId>) -> bool {
    match operand {
        Operand::Copy(place) | Operand::Move(place) => {
            place.proj.is_empty()
                && scalar(function, place.local)
                && !addressed.contains(&place.local)
        }
        Operand::Const(Const::Int(_, _) | Const::Bool(_)) => true,
        _ => false,
    }
}
fn plain(
    function: &post::Function,
    statement: &StatementKind,
    addressed: &HashSet<LocalId>,
) -> bool {
    match statement {
        StatementKind::StorageLive(local) | StatementKind::StorageDead(local) => {
            scalar(function, *local) && !addressed.contains(local)
        }
        StatementKind::Assign(place, value)
            if place.proj.is_empty()
                && scalar(function, place.local)
                && !addressed.contains(&place.local) =>
        {
            match value {
                Rvalue::Use(value) | Rvalue::Unary(_, value) => operand(function, value, addressed),
                Rvalue::Binary(_, x, y) => {
                    operand(function, x, addressed) && operand(function, y, addressed)
                }
                Rvalue::Cast(value, Ty::Int(_) | Ty::Bool) => operand(function, value, addressed),
                _ => false,
            }
        }
        _ => false,
    }
}
fn pure(function: &post::Function, block: usize, addressed: &HashSet<LocalId>) -> bool {
    function.blocks[block].stmts.iter().all(
        |statement| matches!(&statement.op, Op::Plain(kind) if plain(function, kind, addressed)),
    )
}

fn peel(function: &mut post::Function) -> bool {
    if function.decl.asynchronous.is_some() || !matches!(function.decl.kind, FnKind::Body) {
        return false;
    }
    let count = function.blocks.len();
    let mut predecessors = vec![Vec::new(); count];
    let mut addressed = HashSet::new();
    for (index, block) in function.blocks.iter().enumerate() {
        for target in block.term.successors() {
            predecessors[target.0 as usize].push(index);
        }
        for statement in &block.stmts {
            if let Op::Plain(StatementKind::Assign(_, Rvalue::Ref(_, place))) = &statement.op {
                addressed.insert(place.local);
            }
        }
    }
    let mut candidates = Vec::new();
    let mut used = HashSet::new();
    let mut growth = 0;
    // Only original blocks are candidates: copied iterations are never peeled
    // again, and the total duplicated code has an explicit per-function budget.
    for header in 1..count {
        let Terminator::Switch {
            discr,
            cases,
            otherwise,
        } = &function.blocks[header].term
        else {
            continue;
        };
        if cases.len() != 1 || cases[0].0 != 0 || !operand(function, discr, &addressed) {
            continue;
        }
        let body = otherwise.0 as usize;
        let exit = cases[0].1 .0 as usize;
        if body == header
            || exit == header
            || exit == body
            || predecessors[body] != [header]
            || !matches!(function.blocks[body].term, Terminator::Goto(target) if target.0 as usize == header)
            || !predecessors[header].iter().any(|p| *p != body)
            || used.contains(&header)
            || used.contains(&body)
        {
            continue;
        }
        let size = function.blocks[header].stmts.len() + function.blocks[body].stmts.len() + 2;
        if size > MAX_LOOP_SIZE
            || growth + size > MAX_GROWTH
            || !pure(function, header, &addressed)
            || !pure(function, body, &addressed)
        {
            continue;
        }
        // A constant unsigned remainder can establish a stable carried range.
        // Loops without such a source of new information are left alone.
        let bounded = function.blocks[body].stmts.iter().any(|s| matches!(&s.op,
            Op::Plain(StatementKind::Assign(_, Rvalue::Binary(BinOp::Rem, _, Operand::Const(Const::Int(d, ty))))) if *d > 0 && !ty.signed()));
        let checked = function.blocks[body].stmts.iter().any(|s| {
            matches!(
                &s.op,
                Op::Plain(StatementKind::Assign(
                    _,
                    Rvalue::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul, _, _)
                ))
            )
        });
        if !bounded || !checked {
            continue;
        }
        growth += size;
        used.extend([header, body]);
        candidates.push((header, body));
        if candidates.len() == MAX_LOOPS {
            break;
        }
    }
    for &(header, body) in &candidates {
        let first_header = BlockId(function.blocks.len() as u32);
        let first_body = BlockId(first_header.0 + 1);
        let mut prefix_header = function.blocks[header].clone();
        let prefix_body = function.blocks[body].clone();
        for target in prefix_header.term.targets_mut() {
            if target.0 as usize == body {
                *target = first_body;
            }
        }
        // All entries first execute the guarded copied condition/iteration.
        // The original body and the copied body both return to the original
        // header, preserving zero trips and every subsequent iteration.
        for (index, block) in function.blocks.iter_mut().enumerate() {
            if index != body {
                for target in block.term.targets_mut() {
                    if target.0 as usize == header {
                        *target = first_header;
                    }
                }
            }
        }
        function.blocks.extend([prefix_header, prefix_body]);
    }
    !candidates.is_empty()
}
