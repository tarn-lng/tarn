use std::collections::{HashMap, HashSet};
use tarn_ast::*;
use tarn_resolve::{Res, SymbolId};
/// Symbols a closure body writes or borrows mutably (assignment target root,
/// `&mut` operand root, `&mut self` receiver root): captured by `&mut`.
pub(crate) fn mutated_symbols(body: &Block, uses: &HashMap<NodeId, tarn_resolve::Use>, t: &crate::TypeTables) -> HashSet<SymbolId> {
    fn root(e: &Expr, uses: &HashMap<NodeId, tarn_resolve::Use>) -> Option<SymbolId> {
        match &e.kind {
            ExprKind::Ident(_) => match uses.get(&e.id).map(|u| &u.res) {
                Some(Res::Symbol(s)) => Some(*s),
                _ => None,
            },
            ExprKind::Field { base, .. } | ExprKind::Index { base, .. } | ExprKind::Paren(base) | ExprKind::Unary { op: UnaryOp::Deref, operand: base } => root(base, uses),
            _ => None,
        }
    }
    struct V<'x> {
        uses: &'x HashMap<NodeId, tarn_resolve::Use>,
        t: &'x crate::TypeTables,
        out: HashSet<SymbolId>,
    }
    impl V<'_> {
        fn stmts(&mut self, s: &[Stmt]) {
            for x in s {
                self.stmt(x);
            }
        }
        fn stmt(&mut self, s: &Stmt) {
            match &s.kind {
                StmtKind::Let { value: Some(value), .. } => self.expr(value),
                StmtKind::Assign { target, value } => {
                    if let Some(r) = root(target, self.uses) {
                        self.out.insert(r);
                    }
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Expr(e) | StmtKind::Spawn(e) | StmtKind::Return(Some(e)) => self.expr(e),
                StmtKind::If(i) => self.if_(i),
                StmtKind::For(f) => {
                    match &f.kind {
                        ForKind::While(c) => self.expr(c),
                        ForKind::In { iter, .. } => self.expr(iter),
                        ForKind::Infinite => {}
                    }
                    self.stmts(&f.body.stmts);
                }
                StmtKind::Match(m) => {
                    self.expr(&m.scrutinee);
                    for a in &m.arms {
                        if let Some(g) = &a.guard {
                            self.expr(g);
                        }
                        self.stmt(&a.body);
                    }
                }
                StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => self.stmts(&b.stmts),
                _ => {}
            }
        }
        fn if_(&mut self, i: &IfStmt) {
            self.expr(&i.cond);
            self.stmts(&i.then_block.stmts);
            match i.else_branch.as_deref() {
                Some(ElseBranch::If(e)) => self.if_(e),
                Some(ElseBranch::Block(b)) => self.stmts(&b.stmts),
                None => {}
            }
        }
        fn expr(&mut self, e: &Expr) {
            match &e.kind {
                ExprKind::Unary { op: UnaryOp::RefMut, operand } => {
                    if let Some(r) = root(operand, self.uses) {
                        self.out.insert(r);
                    }
                    self.expr(operand);
                }
                ExprKind::Call { callee, args } => {
                    if let (Some(rcv), ExprKind::Field { base, .. }) = (self.t.receivers.get(&e.id), &callee.kind)
                        && rcv.kind == ReceiverKind::RefMut
                                                && let Some(r) = root(base, self.uses)
                    {
                        self.out.insert(r);
                    }
                    if matches!(self.t.expr_types.get(&callee.id), Some(crate::Ty::Fn(CallMode::Mutable, ..)))
                        && let Some(r) = root(callee, self.uses)
                    {
                        self.out.insert(r);
                    }
                    self.expr(callee);
                    for a in args {
                        if matches!(self.t.expr_types.get(&a.id), Some(crate::Ty::Ref(true, _))) && let Some(r) = root(a, self.uses) { self.out.insert(r); }
                        self.expr(a);
                    }
                }
                ExprKind::Field { base, .. } | ExprKind::Paren(base) | ExprKind::Try(base) | ExprKind::Await(base) | ExprKind::Unary { operand: base, .. } => self.expr(base),
                ExprKind::Interpolated(parts) => {
                    for part in parts {
                        if let InterpPart::Value(v) = part {
                            self.expr(v);
                        }
                    }
                }
                ExprKind::Index { base, index } => {
                    self.expr(base);
                    self.expr(index);
                }
                ExprKind::Binary { lhs, rhs, .. } => {
                    self.expr(lhs);
                    self.expr(rhs);
                }
                ExprKind::StructLit { fields, .. } => {
                    for f in fields {
                        if let Some(v) = &f.value {
                            self.expr(v);
                        }
                    }
                }
                ExprKind::ArrayLit { elems, .. } => {
                    for x in elems {
                        self.expr(x);
                    }
                }
                ExprKind::Closure { owned: false, body, .. } => self.stmts(&body.stmts),
                ExprKind::Match(m) => {
                    self.expr(&m.scrutinee);
                    for a in &m.arms {
                        if let Some(g) = &a.guard {
                            self.expr(g);
                        }
                        self.stmt(&a.body);
                    }
                }
                ExprKind::Range { start, end, .. } => {
                    for x in [start, end].into_iter().flatten() {
                        self.expr(x);
                    }
                }
                _ => {}
            }
        }
    }
    let mut v = V { uses, t, out: HashSet::new() };
    v.stmts(&body.stmts);
    v.out
}

/// Capture moves use typed value contexts. Borrow operands and borrowed method
/// receivers do not transfer ownership. The move checker remains authoritative
/// about initialization, including partial moves and reinitialization.
/// `expr_ty` gives an expression's resolved type on demand; resolving only the
/// closure body's expressions keeps this proportional to the closure.
pub(crate) fn consumed_symbols(body: &Block, uses: &HashMap<NodeId, tarn_resolve::Use>, t: &crate::TypeTables, expr_ty: &dyn Fn(NodeId) -> Option<crate::Ty>, decls: &crate::Decls, captures: &HashMap<SymbolId, crate::Ty>) -> HashSet<SymbolId> {
    struct V<'a> {
        uses: &'a HashMap<NodeId, tarn_resolve::Use>,
        t: &'a crate::TypeTables,
        expr_ty: &'a dyn Fn(NodeId) -> Option<crate::Ty>,
        decls: &'a crate::Decls,
        captures: &'a HashMap<SymbolId, crate::Ty>,
        out: HashSet<SymbolId>,
    }
    impl V<'_> {
        fn root(&self, e: &Expr) -> Option<SymbolId> {
            match &e.kind {
                ExprKind::Ident(_) => match self.uses.get(&e.id).map(|u| &u.res) {
                    Some(Res::Symbol(s)) => Some(*s),
                    _ => None,
                },
                ExprKind::Field { base, .. } | ExprKind::Index { base, .. } | ExprKind::Paren(base) | ExprKind::Unary { op: UnaryOp::Deref, operand: base } => self.root(base),
                _ => None,
            }
        }
        fn expr(&mut self, e: &Expr, moving: bool) {
            if moving
                && (self.expr_ty)(e.id).is_some_and(|ty| !self.decls.is_copy(&ty))
                && let Some(s) = self.root(e)
                && self.captures.contains_key(&s)
            {
                self.out.insert(s);
            }
            match &e.kind {
                ExprKind::Unary { op: UnaryOp::Ref | UnaryOp::RefMut, operand } => self.expr(operand, false),
                ExprKind::Call { callee, args } => {
                    let print = self.t.borrowed_builtin_calls.contains(&e.id);
                    if let ExprKind::Field { base, .. } = &callee.kind && !self.t.callable_calls.contains(&e.id) {
                        let owned = self.t.receivers.get(&e.id).is_some_and(|r| r.kind == ReceiverKind::Value);
                        self.expr(base, owned);
                    } else {
                        let consuming = matches!((self.expr_ty)(callee.id), Some(crate::Ty::Fn(CallMode::Once, ..)));
                        self.expr(callee, consuming);
                    }
                    for a in args {
                        self.expr(a, !print && !matches!((self.expr_ty)(a.id), Some(crate::Ty::Ref(..))));
                    }
                }
                ExprKind::Field { base, .. } => self.expr(base, false),
                ExprKind::Paren(base) | ExprKind::Try(base) | ExprKind::Await(base) => self.expr(base, moving),
                // Interpolation only borrows its values (ADR 0077).
                ExprKind::Interpolated(parts) => {
                    for part in parts {
                        if let InterpPart::Value(v) = part {
                            self.expr(v, false);
                        }
                    }
                }
                ExprKind::Index { base, index } => {
                    self.expr(base, false);
                    self.expr(index, true);
                }
                ExprKind::Unary { operand, .. } => self.expr(operand, true),
                ExprKind::Binary { lhs, rhs, .. } => {
                    self.expr(lhs, true);
                    self.expr(rhs, true);
                }
                ExprKind::StructLit { fields, .. } => {
                    for f in fields {
                        if let Some(v) = &f.value {
                            self.expr(v, true);
                        } else if let Some(Res::Symbol(s)) = self.uses.get(&f.id).map(|u| &u.res)
                            && self.captures.get(s).is_some_and(|ty| !self.decls.is_copy(ty))
                        {
                            self.out.insert(*s);
                        }
                    }
                }
                ExprKind::ArrayLit { elems, .. } => {
                    for e in elems {
                        self.expr(e, true);
                    }
                }
                ExprKind::Closure { owned, .. } => {
                    if *owned && let Some(fields) = self.t.owned_captures.get(&e.id) {
                        for (s, _) in fields {
                            if self.captures.get(s).is_some_and(|ty| !self.decls.is_copy(ty)) {
                                self.out.insert(*s);
                            }
                        }
                    }
                }
                ExprKind::Range { start, end, .. } => {
                    for e in [start, end].into_iter().flatten() {
                        self.expr(e, true);
                    }
                }
                ExprKind::Match(m) => {
                    self.expr(&m.scrutinee, m.arms.iter().any(|a| self.pattern_moves(&a.pattern)));
                    for a in &m.arms {
                        if let Some(g) = &a.guard {
                            self.expr(g, true);
                        }
                        match &a.body.kind {
                            StmtKind::Expr(v) => self.expr(v, moving),
                            _ => self.stmt(&a.body),
                        }
                    }
                }
                _ => {}
            }
        }
        fn pattern_moves(&self, p: &Pattern) -> bool {
            if self.t.binding_modes.get(&p.id) == Some(&crate::BindingMode::Move) { return true; }
            match &p.kind {
                PatternKind::Variant { args, .. } => args.iter().any(|p| self.pattern_moves(p)),
                PatternKind::Struct { fields, .. } => fields.iter().any(|f| self.t.binding_modes.get(&f.id) == Some(&crate::BindingMode::Move) || f.pattern.as_ref().is_some_and(|p| self.pattern_moves(p))),
                _ => false,
            }
        }
        fn block(&mut self, b: &Block) {
            for s in &b.stmts {
                self.stmt(s);
            }
        }
        fn if_(&mut self, i: &IfStmt) {
            self.expr(&i.cond, true);
            self.block(&i.then_block);
            match i.else_branch.as_deref() {
                Some(ElseBranch::If(i)) => self.if_(i),
                Some(ElseBranch::Block(b)) => self.block(b),
                None => {}
            }
        }
        fn stmt(&mut self, s: &Stmt) {
            match &s.kind {
                StmtKind::Let { value: Some(e), .. } | StmtKind::Expr(e) | StmtKind::Return(Some(e)) | StmtKind::Spawn(e) => self.expr(e, true),
                StmtKind::Assign { target, value } => {
                    self.expr(target, false);
                    self.expr(value, true);
                }
                StmtKind::If(i) => self.if_(i),
                StmtKind::For(f) => {
                    match &f.kind {
                        ForKind::While(e) | ForKind::In { iter: e, .. } => self.expr(e, true),
                        _ => {}
                    }
                    self.block(&f.body);
                }
                StmtKind::Match(m) => {
                    self.expr(&m.scrutinee, m.arms.iter().any(|a| self.pattern_moves(&a.pattern)));
                    for a in &m.arms {
                        if let Some(g) = &a.guard {
                            self.expr(g, true);
                        }
                        self.stmt(&a.body);
                    }
                }
                StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => self.block(b),
                _ => {}
            }
        }
    }
    let mut v = V { uses, t, expr_ty, decls, captures, out: HashSet::new() };
    v.block(body);
    v.out
}
