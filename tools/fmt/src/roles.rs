//! Syntactic operator roles, derived from AST spans rather than reparsing tokens.
use std::collections::HashSet;
use tarn_ast::*;
use tarn_lexer::Token;

pub fn binary_operators(module: &Module, tokens: &[&Token], out: &mut HashSet<u32>) {
    for item in &module.items {
        match &item.kind {
            ItemKind::Fn(f) => function(f, tokens, out),
            ItemKind::Struct(s) => {
                for field in &s.fields {
                    ty(&field.ty, tokens, out);
                }
            }
            ItemKind::Enum(e) => {
                for variant in &e.variants {
                    for t in &variant.fields {
                        ty(t, tokens, out);
                    }
                }
            }
            ItemKind::Interface(i) => {
                for f in &i.methods {
                    function(f, tokens, out);
                }
            }
            ItemKind::Impl(i) => {
                ty(&i.target, tokens, out);
                for f in &i.methods {
                    function(f, tokens, out);
                }
            }
            ItemKind::Import(_) | ItemKind::Error => {}
        }
    }
}
fn function(f: &FnDecl, tokens: &[&Token], out: &mut HashSet<u32>) {
    for p in &f.params {
        ty(&p.ty, tokens, out);
    }
    if let Some(t) = &f.ret {
        ty(t, tokens, out);
    }
    if let Some(b) = &f.body {
        block(b, tokens, out);
    }
}
fn path(p: &Path, tokens: &[&Token], out: &mut HashSet<u32>) {
    for t in &p.args {
        ty(t, tokens, out);
    }
}
fn ty(t: &Type, tokens: &[&Token], out: &mut HashSet<u32>) {
    match &t.kind {
        TypeKind::Path(p) | TypeKind::Any(p) => path(p, tokens, out),
        TypeKind::Ref { inner, .. } | TypeKind::Ptr { inner, .. } | TypeKind::Slice(inner) => {
            ty(inner, tokens, out)
        }
        TypeKind::Array { len, elem } => {
            expr(len, tokens, out);
            ty(elem, tokens, out);
        }
        TypeKind::Fn { params, ret, .. } => {
            for t in params {
                ty(t, tokens, out);
            }
            if let Some(t) = ret {
                ty(t, tokens, out);
            }
        }
        TypeKind::Error => {}
    }
}
fn block(b: &Block, tokens: &[&Token], out: &mut HashSet<u32>) {
    for s in &b.stmts {
        stmt(s, tokens, out);
    }
}
fn condition(i: &IfStmt, tokens: &[&Token], out: &mut HashSet<u32>) {
    expr(&i.cond, tokens, out);
    block(&i.then_block, tokens, out);
    if let Some(other) = &i.else_branch {
        match other.as_ref() {
            ElseBranch::If(i) => condition(i, tokens, out),
            ElseBranch::Block(b) => block(b, tokens, out),
        }
    }
}
fn stmt(s: &Stmt, tokens: &[&Token], out: &mut HashSet<u32>) {
    match &s.kind {
        StmtKind::Let { ty: t, value, .. } => {
            if let Some(t) = t {
                ty(t, tokens, out);
            }
            if let Some(e) = value {
                expr(e, tokens, out);
            }
        }
        StmtKind::Assign { target, value } => {
            expr(target, tokens, out);
            expr(value, tokens, out);
        }
        StmtKind::Expr(e) | StmtKind::Spawn(e) => expr(e, tokens, out),
        StmtKind::Return(e) => {
            if let Some(e) = e {
                expr(e, tokens, out);
            }
        }
        StmtKind::If(i) => condition(i, tokens, out),
        StmtKind::For(f) => {
            match &f.kind {
                ForKind::While(e) | ForKind::In { iter: e, .. } => expr(e, tokens, out),
                ForKind::Infinite => {}
            }
            block(&f.body, tokens, out);
        }
        StmtKind::Match(m) => {
            expr(&m.scrutinee, tokens, out);
            for arm in &m.arms {
                pattern(&arm.pattern, tokens, out);
                if let Some(e) = &arm.guard {
                    expr(e, tokens, out);
                }
                stmt(&arm.body, tokens, out);
            }
        }
        StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => block(b, tokens, out),
        StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
    }
}
fn pattern(p: &Pattern, tokens: &[&Token], out: &mut HashSet<u32>) {
    match &p.kind {
        PatternKind::Literal(e) => expr(e, tokens, out),
        PatternKind::Range { start, end, .. } => {
            expr(start, tokens, out);
            expr(end, tokens, out);
        }
        PatternKind::Variant { path: p, args } => {
            path(p, tokens, out);
            for p in args {
                pattern(p, tokens, out);
            }
        }
        PatternKind::Struct { path: p, fields } => {
            path(p, tokens, out);
            for f in fields {
                if let Some(p) = &f.pattern {
                    pattern(p, tokens, out);
                }
            }
        }
        PatternKind::Wildcard | PatternKind::Ident(_) | PatternKind::Error => {}
    }
}
fn expr(e: &Expr, tokens: &[&Token], out: &mut HashSet<u32>) {
    match &e.kind {
        ExprKind::Binary { lhs, rhs, .. } => {
            if let Some(token) = tokens
                .get(tokens.partition_point(|t| t.span.start < lhs.span.end))
                .filter(|t| t.span.end <= rhs.span.start)
            {
                out.insert(token.span.start);
            }
            expr(lhs, tokens, out);
            expr(rhs, tokens, out);
        }
        ExprKind::Paren(e)
        | ExprKind::Try(e)
        | ExprKind::Await(e)
        | ExprKind::Unary { operand: e, .. } => expr(e, tokens, out),
        ExprKind::Field { base, .. } => expr(base, tokens, out),
        ExprKind::Index { base, index } => {
            expr(base, tokens, out);
            expr(index, tokens, out);
        }
        ExprKind::Call { callee, args } => {
            expr(callee, tokens, out);
            for e in args {
                expr(e, tokens, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                expr(e, tokens, out);
            }
            if let Some(e) = end {
                expr(e, tokens, out);
            }
        }
        ExprKind::StructLit { path: p, fields } => {
            path(p, tokens, out);
            for f in fields {
                if let Some(e) = &f.value {
                    expr(e, tokens, out);
                }
            }
        }
        ExprKind::ArrayLit { ty: t, elems } => {
            ty(t, tokens, out);
            for e in elems {
                expr(e, tokens, out);
            }
        }
        ExprKind::Match(m) => {
            expr(&m.scrutinee, tokens, out);
            for arm in &m.arms {
                pattern(&arm.pattern, tokens, out);
                if let Some(e) = &arm.guard {
                    expr(e, tokens, out);
                }
                stmt(&arm.body, tokens, out);
            }
        }
        ExprKind::Closure {
            params, ret, body, ..
        } => {
            for p in params {
                if let Some(t) = &p.ty {
                    ty(t, tokens, out);
                }
            }
            if let Some(t) = ret {
                ty(t, tokens, out);
            }
            block(body, tokens, out);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Interpolated(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Ident(_)
        | ExprKind::Error => {}
    }
}
