//! Post-drop constant branch simplification and unreachable block pruning (ADR 0114).
//! Runs after type, move, borrow, and drop elaboration (and monomorphization/inlining)
//! so dead branches never bypass frontend/ownership checks, while unreachable call
//! edges are removed before backend codegen emits ELF relocations.

use crate::*;
use post_drop::{self as post, Op};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstVal {
    Bool(bool),
    Int(i128, IntTy),
}

pub fn simplify_constant_branches(program: &mut post::Program) {
    for function in &mut program.functions {
        simplify_function(function);
    }
}

fn collect_addressed(function: &post::Function) -> HashSet<LocalId> {
    let mut addressed = HashSet::new();
    for block in &function.blocks {
        for stmt in &block.stmts {
            if let Op::Plain(StatementKind::Assign(_, rvalue)) = &stmt.op {
                match rvalue {
                    Rvalue::Ref(_, place) | Rvalue::SliceRef { base: place, .. } => {
                        addressed.insert(place.local);
                    }
                    Rvalue::Aggregate(Aggregate::Closure(_, Some(place)), _) => {
                        addressed.insert(place.local);
                    }
                    _ => {}
                }
            }
        }
    }
    addressed
}

fn simplify_function(function: &mut post::Function) {
    if function.blocks.is_empty() || !matches!(function.decl.kind, FnKind::Body) {
        return;
    }
    let param_count = function.decl.param_count as u32;
    for _pass in 0..4 {
        let addressed = collect_addressed(function);
        let local_count = function.decl.locals.len();
        let mut assign_count = vec![0u32; local_count];
        let mut live_count = vec![0u32; local_count];

        for block in &function.blocks {
            for stmt in &block.stmts {
                match &stmt.op {
                    Op::Plain(StatementKind::Assign(place, _)) => {
                        if let Some(c) = assign_count.get_mut(place.local.0 as usize) {
                            *c = c.saturating_add(1);
                        }
                    }
                    Op::Plain(StatementKind::StorageLive(l)) => {
                        if let Some(c) = live_count.get_mut(l.0 as usize) {
                            *c = c.saturating_add(1);
                        }
                    }
                    _ => {}
                }
            }
            if let Terminator::Call { dest, .. } = &block.term {
                if let Some(c) = assign_count.get_mut(dest.local.0 as usize) {
                    *c = c.saturating_add(1);
                }
            }
        }

        let is_single_assign = |l: LocalId| -> bool {
            let idx = l.0 as usize;
            l.0 > param_count
                && !addressed.contains(&l)
                && assign_count.get(idx).copied().unwrap_or(0) == 1
                && live_count.get(idx).copied().unwrap_or(0) <= 1
        };

        // Pre-pass: find global constants for single-assignment scalar locals.
        let mut global_consts: HashMap<LocalId, ConstVal> = HashMap::new();
        for _ in 0..2 {
            for block in &function.blocks {
                let mut local_vals = global_consts.clone();
                for stmt in &block.stmts {
                    step_stmt(&stmt.op, &mut local_vals, &mut global_consts, &is_single_assign);
                }
            }
        }

        // Simplify constant Switches across reachable blocks.
        let mut changed = false;
        for block in &mut function.blocks {
            let mut local_vals = global_consts.clone();
            for stmt in &block.stmts {
                step_stmt(&stmt.op, &mut local_vals, &mut global_consts, &is_single_assign);
            }
            if let Terminator::Switch { discr, cases, otherwise } = &block.term {
                if let Some(val) = eval_operand(discr, &local_vals) {
                    let key = match val {
                        ConstVal::Bool(b) => i128::from(b),
                        ConstVal::Int(v, _) => v,
                    };
                    let target = cases
                        .iter()
                        .find(|(c, _)| *c == key)
                        .map(|(_, b)| *b)
                        .unwrap_or(*otherwise);
                    block.term = Terminator::Goto(target);
                    changed = true;
                }
            }
        }

        let pruned = prune_unreachable_blocks(function);
        if !changed && !pruned {
            break;
        }
    }
}

fn step_stmt<F: Fn(LocalId) -> bool>(
    op: &Op,
    local_vals: &mut HashMap<LocalId, ConstVal>,
    global_consts: &mut HashMap<LocalId, ConstVal>,
    is_single_assign: &F,
) {
    match op {
        Op::Plain(StatementKind::StorageLive(l)) | Op::Plain(StatementKind::StorageDead(l)) => {
            local_vals.remove(l);
        }
        Op::Plain(StatementKind::Assign(place, rvalue)) => {
            if place.proj.is_empty() {
                if let Some(val) = eval_rvalue(rvalue, local_vals) {
                    local_vals.insert(place.local, val);
                    if is_single_assign(place.local) {
                        global_consts.insert(place.local, val);
                    }
                } else {
                    local_vals.remove(&place.local);
                }
            } else {
                local_vals.remove(&place.local);
            }
        }
        _ => {}
    }
}

fn prune_unreachable_blocks(function: &mut post::Function) -> bool {
    let n = function.blocks.len();
    if n <= 1 {
        return false;
    }
    let mut reachable = vec![false; n];
    let mut q = VecDeque::new();
    reachable[0] = true;
    q.push_back(0usize);
    while let Some(idx) = q.pop_front() {
        for succ in function.blocks[idx].term.successors() {
            let s = succ.0 as usize;
            if s < n && !reachable[s] {
                reachable[s] = true;
                q.push_back(s);
            }
        }
    }
    if reachable.iter().all(|&r| r) {
        return false;
    }
    let mut remap = vec![BlockId(0); n];
    let mut next_id: u32 = 0;
    for (old, &keep) in reachable.iter().enumerate() {
        if keep {
            remap[old] = BlockId(next_id);
            next_id += 1;
        }
    }
    let old_blocks = std::mem::take(&mut function.blocks);
    for (old_idx, mut block) in old_blocks.into_iter().enumerate() {
        if reachable[old_idx] {
            for target in block.term.targets_mut() {
                *target = remap[target.0 as usize];
            }
            function.blocks.push(block);
        }
    }
    true
}

fn eval_operand(op: &Operand, locals: &HashMap<LocalId, ConstVal>) -> Option<ConstVal> {
    match op {
        Operand::Const(Const::Bool(b)) => Some(ConstVal::Bool(*b)),
        Operand::Const(Const::Int(v, ty)) => Some(ConstVal::Int(*v, *ty)),
        Operand::Copy(place) | Operand::Move(place) if place.proj.is_empty() => {
            locals.get(&place.local).copied()
        }
        _ => None,
    }
}

fn eval_rvalue(rv: &Rvalue, locals: &HashMap<LocalId, ConstVal>) -> Option<ConstVal> {
    match rv {
        Rvalue::Use(op) => eval_operand(op, locals),
        Rvalue::Unary(UnOp::Not, op) => match eval_operand(op, locals)? {
            ConstVal::Bool(b) => Some(ConstVal::Bool(!b)),
            ConstVal::Int(v, ty) => {
                let (min, max) = ty.range();
                let res = if ty.signed() {
                    !v
                } else {
                    (!v) & max
                };
                if res >= min && res <= max { Some(ConstVal::Int(res, ty)) } else { None }
            }
        },
        Rvalue::Unary(UnOp::Neg, op) => match eval_operand(op, locals)? {
            ConstVal::Int(v, ty) if ty.signed() => {
                let (min, max) = ty.range();
                let res = v.checked_neg()?;
                if res >= min && res <= max { Some(ConstVal::Int(res, ty)) } else { None }
            }
            _ => None,
        },
        Rvalue::Cast(op, Ty::Int(target_ty)) => match eval_operand(op, locals)? {
            ConstVal::Int(v, _) => {
                let (min, max) = target_ty.range();
                if v >= min && v <= max { Some(ConstVal::Int(v, *target_ty)) } else { None }
            }
            ConstVal::Bool(b) => Some(ConstVal::Int(i128::from(b), *target_ty)),
        },
        Rvalue::Binary(op, lhs, rhs) => eval_binary(*op, eval_operand(lhs, locals)?, eval_operand(rhs, locals)?),
        _ => None,
    }
}

fn int_bits(ty: IntTy) -> u32 {
    match ty {
        IntTy::I8 | IntTy::U8 => 8,
        IntTy::I16 | IntTy::U16 => 16,
        IntTy::I32 | IntTy::U32 => 32,
        IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => 64,
    }
}

fn eval_binary(op: BinOp, lhs: ConstVal, rhs: ConstVal) -> Option<ConstVal> {
    match (lhs, rhs) {
        (ConstVal::Bool(a), ConstVal::Bool(b)) => match op {
            BinOp::Eq => Some(ConstVal::Bool(a == b)),
            BinOp::Ne => Some(ConstVal::Bool(a != b)),
            BinOp::BitAnd => Some(ConstVal::Bool(a & b)),
            BinOp::BitOr => Some(ConstVal::Bool(a | b)),
            BinOp::BitXor => Some(ConstVal::Bool(a ^ b)),
            _ => None,
        },
        (ConstVal::Int(a, ty_a), ConstVal::Int(b, ty_b)) if ty_a == ty_b => {
            let (min, max) = ty_a.range();
            let bits = int_bits(ty_a);
            let checked_int = |res: i128| -> Option<ConstVal> {
                if res >= min && res <= max { Some(ConstVal::Int(res, ty_a)) } else { None }
            };
            match op {
                BinOp::Eq => Some(ConstVal::Bool(a == b)),
                BinOp::Ne => Some(ConstVal::Bool(a != b)),
                BinOp::Lt => Some(ConstVal::Bool(a < b)),
                BinOp::Le => Some(ConstVal::Bool(a <= b)),
                BinOp::Gt => Some(ConstVal::Bool(a > b)),
                BinOp::Ge => Some(ConstVal::Bool(a >= b)),
                BinOp::Add | BinOp::AddProven => checked_int(a.checked_add(b)?),
                BinOp::Sub | BinOp::SubProven => checked_int(a.checked_sub(b)?),
                BinOp::Mul | BinOp::MulProven => checked_int(a.checked_mul(b)?),
                BinOp::Div => {
                    if b == 0 { return None; }
                    checked_int(a.checked_div(b)?)
                }
                BinOp::Rem => {
                    if b == 0 { return None; }
                    checked_int(a.checked_rem(b)?)
                }
                BinOp::BitAnd => checked_int(a & b),
                BinOp::BitOr => checked_int(a | b),
                BinOp::BitXor => checked_int(a ^ b),
                BinOp::Shl => {
                    if b < 0 || b >= i128::from(bits) { return None; }
                    if ty_a.signed() {
                        let mask = (1i128 << bits) - 1;
                        let raw = ((a as u128) << (b as u32)) & (mask as u128);
                        let sign_bit = 1u128 << (bits - 1);
                        let signed = if (raw & sign_bit) != 0 { (raw as i128) - (1i128 << bits) } else { raw as i128 };
                        checked_int(signed)
                    } else {
                        checked_int(((a as u128) << (b as u32) & (max as u128)) as i128)
                    }
                }
                BinOp::Shr => {
                    if b < 0 || b >= i128::from(bits) { return None; }
                    checked_int(a >> (b as u32))
                }
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tarn_diagnostics::Span;
    use tarn_resolve::SymbolId;

    fn dummy_span() -> Span {
        Span::new(tarn_diagnostics::FileId(0), 0, 0)
    }

    fn make_fn(locals: Vec<Ty>, blocks: Vec<post::Block>) -> post::Function {
        let local_decls = locals
            .into_iter()
            .enumerate()
            .map(|(i, ty)| LocalDecl {
                ty,
                kind: if i == 0 { LocalKind::Return } else { LocalKind::Temp },
                name: None,
                symbol: None,
                mutable: false,
                span: dummy_span(),
            })
            .collect();
        post::Function {
            decl: Function {
                id: FunctionId(0),
                symbol: Some(SymbolId(0)),
                name: "test_fn".into(),
                kind: FnKind::Body,
                generics: Vec::new(),
                param_count: 0,
                ret: Ty::Void,
                locals: local_decls,
                blocks: Vec::new(),
                asynchronous: None,
                span: dummy_span(),
            },
            flags: Vec::new(),
            blocks,
        }
    }

    fn plain_assign(local: u32, rv: Rvalue) -> post::Statement {
        post::Statement {
            op: Op::Plain(StatementKind::Assign(Place::local(LocalId(local)), rv)),
            span: dummy_span(),
        }
    }

    #[test]
    fn folds_constant_comparison_and_prunes_dead_call() {
        let sp = dummy_span();
        let blocks = vec![
            post::Block {
                stmts: vec![plain_assign(
                    1,
                    Rvalue::Binary(
                        BinOp::Eq,
                        Operand::Const(Const::Int(0, IntTy::U64)),
                        Operand::Const(Const::Int(1, IntTy::U64)),
                    ),
                )],
                term: Terminator::Switch {
                    discr: Operand::Copy(Place::local(LocalId(1))),
                    cases: vec![(0, BlockId(2))],
                    otherwise: BlockId(1),
                },
                term_span: sp,
            },
            post::Block {
                stmts: Vec::new(),
                term: Terminator::Call {
                    callee: Callee::Fn(FunctionId(99), Vec::new()),
                    args: Vec::new(),
                    arg_spans: Vec::new(),
                    dest: Place::local(LocalId(2)),
                    next: Some(BlockId(2)),
                    spawn: false,
                },
                term_span: sp,
            },
            post::Block {
                stmts: Vec::new(),
                term: Terminator::Return,
                term_span: sp,
            },
        ];
        let mut prog = post::Program {
            functions: vec![make_fn(vec![Ty::Void, Ty::Bool, Ty::Void], blocks)],
            by_symbol: HashMap::new(),
        };
        simplify_constant_branches(&mut prog);
        assert_eq!(prog.functions[0].blocks.len(), 2);
        assert!(matches!(prog.functions[0].blocks[0].term, Terminator::Goto(BlockId(1))));
        assert!(matches!(prog.functions[0].blocks[1].term, Terminator::Return));
    }

    #[test]
    fn propagates_immutable_local_and_preserves_runtime_branch() {
        let sp = dummy_span();
        // _1 = 0_u64; _2 = (_1 == 0_u64); switch _2 [0 => bb2, _ => bb1]
        let blocks = vec![
            post::Block {
                stmts: vec![
                    plain_assign(1, Rvalue::Use(Operand::Const(Const::Int(0, IntTy::U64)))),
                    plain_assign(
                        2,
                        Rvalue::Binary(
                            BinOp::Eq,
                            Operand::Copy(Place::local(LocalId(1))),
                            Operand::Const(Const::Int(0, IntTy::U64)),
                        ),
                    ),
                ],
                term: Terminator::Switch {
                    discr: Operand::Copy(Place::local(LocalId(2))),
                    cases: vec![(0, BlockId(2))],
                    otherwise: BlockId(1),
                },
                term_span: sp,
            },
            post::Block {
                stmts: Vec::new(),
                term: Terminator::Return,
                term_span: sp,
            },
            post::Block {
                stmts: Vec::new(),
                term: Terminator::Call {
                    callee: Callee::Fn(FunctionId(99), Vec::new()),
                    args: Vec::new(),
                    arg_spans: Vec::new(),
                    dest: Place::local(LocalId(3)),
                    next: Some(BlockId(1)),
                    spawn: false,
                },
                term_span: sp,
            },
        ];
        let mut prog = post::Program {
            functions: vec![make_fn(vec![Ty::Void, Ty::Int(IntTy::U64), Ty::Bool, Ty::Void], blocks)],
            by_symbol: HashMap::new(),
        };
        simplify_constant_branches(&mut prog);
        assert_eq!(prog.functions[0].blocks.len(), 2);
        assert!(matches!(prog.functions[0].blocks[0].term, Terminator::Goto(BlockId(1))));
    }

    #[test]
    fn preserves_overflow_trap_without_folding() {
        let sp = dummy_span();
        let blocks = vec![
            post::Block {
                stmts: vec![
                    plain_assign(
                        1,
                        Rvalue::Binary(
                            BinOp::Add,
                            Operand::Const(Const::Int(250, IntTy::U8)),
                            Operand::Const(Const::Int(10, IntTy::U8)),
                        ),
                    ),
                    plain_assign(
                        2,
                        Rvalue::Binary(
                            BinOp::Eq,
                            Operand::Copy(Place::local(LocalId(1))),
                            Operand::Const(Const::Int(4, IntTy::U8)),
                        ),
                    ),
                ],
                term: Terminator::Switch {
                    discr: Operand::Copy(Place::local(LocalId(2))),
                    cases: vec![(0, BlockId(2))],
                    otherwise: BlockId(1),
                },
                term_span: sp,
            },
            post::Block { stmts: Vec::new(), term: Terminator::Return, term_span: sp },
            post::Block { stmts: Vec::new(), term: Terminator::Return, term_span: sp },
        ];
        let mut prog = post::Program {
            functions: vec![make_fn(vec![Ty::Void, Ty::Int(IntTy::U8), Ty::Bool], blocks)],
            by_symbol: HashMap::new(),
        };
        simplify_constant_branches(&mut prog);
        assert_eq!(prog.functions[0].blocks.len(), 3);
        assert!(matches!(prog.functions[0].blocks[0].term, Terminator::Switch { .. }));
    }
}
