//! Expression tree depth bound (ADR 0081).
//!
//! MAX_NESTING bounds the parser's own recursion, but method chains and
//! binary operator sequences are parsed in loops and still build trees one
//! level deeper per link. Every later phase recurses over those trees, so a
//! generated 1000-link chain overflowed the stack. This pass measures the
//! depth of each item's statements and expressions once, after parsing, and
//! reports E1022 when it exceeds MAX_TREE_DEPTH, using an explicit stack.

use tarn_ast::*;
use tarn_diagnostics::{Diagnostic, Span};

/// Deepest accepted statement/expression tree, counted in AST levels: a
/// method call `.f(x)` is two (call and field), an operator one. The semantic
/// phases run on a 256 MiB thread (`tarn_driver::COMPILER_STACK`); an
/// unoptimized build checked and compiled 14000-call chains (28000 levels)
/// there, so 8192 levels (4096 chained calls) leave over 3x room.
pub(crate) const MAX_TREE_DEPTH: usize = 8192;

/// First node found deeper than MAX_TREE_DEPTH in one module, if any. The
/// walk keeps its own stack, so it never recurses whatever the tree depth.
pub(crate) fn check(module: &Module) -> Option<Diagnostic> {
    let mut stack: Vec<(Node, usize)> = Vec::new();
    for item in &module.items {
        let bodies: Vec<&FnDecl> = match &item.kind {
            ItemKind::Fn(f) => vec![f],
            ItemKind::Impl(i) => i.methods.iter().collect(),
            ItemKind::Interface(i) => i.methods.iter().collect(),
            _ => Vec::new(),
        };
        stack.extend(bodies.into_iter().filter_map(|f| f.body.as_ref()).map(|b| (Node::Block(b), 0)));
        while let Some((node, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH {
                return Some(
                    Diagnostic::error("E1022", "expression_too_deep", format!("expression is more than {MAX_TREE_DEPTH} levels deep"))
                        .primary(node.span(), "depth limit reached here")
                        .note("each call, field access, index and operator adds a level; about 4096 chained method calls fit")
                        .help("split the chain or the operator sequence across several bindings"),
                );
            }
            node.children(depth + 1, &mut stack);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum Node<'a> {
    Block(&'a Block),
    Stmt(&'a Stmt),
    If(&'a IfStmt),
    Expr(&'a Expr),
}

impl<'a> Node<'a> {
    fn span(&self) -> Span {
        match self {
            Node::Block(b) => b.span,
            Node::Stmt(s) => s.span,
            Node::If(i) => i.span,
            Node::Expr(e) => e.span,
        }
    }

    fn children(self, d: usize, out: &mut Vec<(Node<'a>, usize)>) {
        let mut expr = |e: &'a Expr| out.push((Node::Expr(e), d));
        match self {
            Node::Block(b) => {
                for s in &b.stmts {
                    out.push((Node::Stmt(s), d));
                }
            }
            Node::Stmt(s) => match &s.kind {
                StmtKind::Let { value, .. } => value.iter().for_each(expr),
                StmtKind::Assign { target, value } => {
                    expr(target);
                    expr(value);
                }
                StmtKind::Expr(e) | StmtKind::Spawn(e) => expr(e),
                StmtKind::Return(e) => e.iter().for_each(expr),
                StmtKind::If(i) => out.push((Node::If(i), d)),
                StmtKind::For(f) => {
                    match &f.kind {
                        ForKind::Infinite => {}
                        ForKind::While(e) | ForKind::In { iter: e, .. } => expr(e),
                    }
                    out.push((Node::Block(&f.body), d));
                }
                StmtKind::Match(m) => {
                    expr(&m.scrutinee);
                    for arm in &m.arms {
                        arm.guard.iter().for_each(|g| out.push((Node::Expr(g), d + 1)));
                        out.push((Node::Stmt(&arm.body), d + 1));
                    }
                }
                StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => out.push((Node::Block(b), d)),
                StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
            },
            Node::If(i) => {
                expr(&i.cond);
                out.push((Node::Block(&i.then_block), d));
                match i.else_branch.as_deref() {
                    Some(ElseBranch::If(next)) => out.push((Node::If(next), d)),
                    Some(ElseBranch::Block(b)) => out.push((Node::Block(b), d)),
                    None => {}
                }
            }
            Node::Expr(e) => match &e.kind {
                ExprKind::Paren(x) | ExprKind::Try(x) | ExprKind::Await(x) => expr(x),
                ExprKind::Field { base, .. } => expr(base),
                ExprKind::Unary { operand, .. } => expr(operand),
                ExprKind::Call { callee, args } => {
                    expr(callee);
                    args.iter().for_each(expr);
                }
                ExprKind::Index { base, index } => {
                    expr(base);
                    expr(index);
                }
                ExprKind::Binary { lhs, rhs, .. } => {
                    expr(lhs);
                    expr(rhs);
                }
                ExprKind::Range { start, end, .. } => {
                    start.iter().for_each(|x| expr(x));
                    end.iter().for_each(|x| expr(x));
                }
                ExprKind::StructLit { fields, .. } => fields.iter().filter_map(|f| f.value.as_ref()).for_each(expr),
                ExprKind::ArrayLit { elems, .. } => elems.iter().for_each(expr),
                ExprKind::Interpolated(parts) => parts.iter().for_each(|p| {
                    if let InterpPart::Value(v) = p {
                        expr(v)
                    }
                }),
                ExprKind::Closure { body, .. } => out.push((Node::Block(body), d)),
                ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Unit | ExprKind::Ident(_) | ExprKind::Error => {}
            },
        }
    }
}

/// Removes every body from a module rejected with E1022 and destroys them
/// without recursion: dropping a tree that deep would itself overflow the
/// stack. Statement nesting stays bounded by MAX_NESTING, so only expression
/// links need the explicit stack.
pub(crate) fn dismantle(module: &mut Module) {
    let mut pending: Vec<Expr> = Vec::new();
    for item in &mut module.items {
        match &mut item.kind {
            ItemKind::Fn(f) => take_body(f, &mut pending),
            ItemKind::Impl(i) => i.methods.iter_mut().for_each(|m| take_body(m, &mut pending)),
            ItemKind::Interface(i) => i.methods.iter_mut().for_each(|m| take_body(m, &mut pending)),
            _ => {}
        }
        while let Some(e) = pending.pop() {
            split(e, &mut pending);
        }
    }
}

fn take_body(f: &mut FnDecl, pending: &mut Vec<Expr>) {
    if let Some(body) = f.body.take() {
        block_exprs(body, pending);
        f.body = Some(Block { id: f.id, span: f.span, stmts: Vec::new() });
    }
}

// Moves the direct children of `e` onto `pending`; `e` itself is then
// dropped with no deep subtree left in it.
fn split(e: Expr, pending: &mut Vec<Expr>) {
    match e.kind {
        ExprKind::Paren(x) | ExprKind::Try(x) | ExprKind::Await(x) => pending.push(*x),
        ExprKind::Field { base, .. } => pending.push(*base),
        ExprKind::Unary { operand, .. } => pending.push(*operand),
        ExprKind::Call { callee, args } => {
            pending.push(*callee);
            pending.extend(args);
        }
        ExprKind::Index { base, index } => {
            pending.push(*base);
            pending.push(*index);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            pending.push(*lhs);
            pending.push(*rhs);
        }
        ExprKind::Range { start, end, .. } => {
            pending.extend(start.map(|x| *x));
            pending.extend(end.map(|x| *x));
        }
        ExprKind::StructLit { fields, .. } => pending.extend(fields.into_iter().filter_map(|f| f.value)),
        ExprKind::ArrayLit { elems, .. } => pending.extend(elems),
        ExprKind::Interpolated(parts) => pending.extend(parts.into_iter().filter_map(|p| match p {
            InterpPart::Value(v) => Some(v),
            InterpPart::Text(_) => None,
        })),
        ExprKind::Closure { body, .. } => block_exprs(body, pending),
        _ => {}
    }
}

// Statement nesting is bounded by MAX_NESTING, so this recursion is shallow.
fn block_exprs(b: Block, pending: &mut Vec<Expr>) {
    for s in b.stmts {
        stmt_exprs(s, pending);
    }
}

fn stmt_exprs(s: Stmt, pending: &mut Vec<Expr>) {
    match s.kind {
        StmtKind::Let { value, .. } => pending.extend(value),
        StmtKind::Assign { target, value } => {
            pending.push(target);
            pending.push(value);
        }
        StmtKind::Expr(e) | StmtKind::Spawn(e) => pending.push(e),
        StmtKind::Return(e) => pending.extend(e),
        StmtKind::If(i) => if_exprs(i, pending),
        StmtKind::For(f) => {
            match f.kind {
                ForKind::Infinite => {}
                ForKind::While(e) | ForKind::In { iter: e, .. } => pending.push(e),
            }
            block_exprs(f.body, pending);
        }
        StmtKind::Match(m) => {
            pending.push(m.scrutinee);
            for arm in m.arms {
                pending.extend(arm.guard);
                stmt_exprs(*arm.body, pending);
            }
        }
        StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => block_exprs(b, pending),
        StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
    }
}

fn if_exprs(i: IfStmt, pending: &mut Vec<Expr>) {
    pending.push(i.cond);
    block_exprs(i.then_block, pending);
    match i.else_branch.map(|b| *b) {
        Some(ElseBranch::If(next)) => if_exprs(next, pending),
        Some(ElseBranch::Block(b)) => block_exprs(b, pending),
        None => {}
    }
}
