//! Blocks and statements.

use crate::Parser;
use tarn_ast::*;
use tarn_diagnostics::Diagnostic;
use tarn_lexer::TokenKind;

impl Parser {
    /// `{` statements separated by line breaks `}`
    pub(crate) fn parse_block(&mut self) -> Block {
        let start = self.span();
        if !self.descend() {
            return Block { id: self.id(), span: self.since(start), stmts: Vec::new() };
        }
        let result = self.parse_block_nested();
        self.ascend();
        result
    }

    fn parse_block_nested(&mut self) -> Block {
        let open = self.span();
        let id = self.id();
        let mut stmts = Vec::new();
        if !self.expect(&TokenKind::LBrace) {
            return Block { id, span: self.since(open), stmts };
        }
        self.with_mode(false, |p| {
            loop {
                p.skip_newlines();
                match p.peek() {
                    TokenKind::RBrace => {
                        p.bump();
                        break;
                    }
                    TokenKind::Eof => {
                        p.unclosed(open);
                        break;
                    }
                    _ => {}
                }
                let before = p.pos;
                let stmt = p.parse_stmt();
                let failed = matches!(stmt.kind, StmtKind::Error);
                stmts.push(stmt);
                p.end_stmt(failed);
                if p.pos == before {
                    p.bump();
                }
            }
        });
        Block { id, span: self.since(open), stmts }
    }

    /// After a statement: a line break, or the `}`/end of file that follows.
    fn end_stmt(&mut self, failed: bool) {
        match self.peek() {
            TokenKind::Newline => {
                self.bump();
            }
            TokenKind::RBrace | TokenKind::Eof => {}
            _ => {
                if !failed {
                    let found = crate::describe(self.peek());
                    let span = self.span();
                    let mut d = Diagnostic::error(
                        "E1005",
                        "expected_statement_end",
                        format!("expected end of statement, found {found}"),
                    )
                    .primary(span, "expected a line break here");
                    let after_let = matches!(&self.tokens[self.pos.saturating_sub(1)].kind, TokenKind::Ident(w) if w == "let");
                    if after_let && matches!(self.peek(), TokenKind::Ident(_)) {
                        d = d.help("Tarn has no `let`: write `x := value`, `x: T := value`, or `var x = value` for a mutable binding");
                    } else if matches!(self.peek(), TokenKind::ColonEq) {
                        d = d.help("`:=` declares a new name and needs a plain identifier on its left");
                    } else {
                        d = d.help("put each statement on its own line");
                    }
                    self.error(d);
                }
                self.recover_stmt();
            }
        }
    }

    pub(crate) fn parse_stmt(&mut self) -> Stmt {
        let start = self.span();
        let kind = self.stmt_kind();
        Stmt { id: self.id(), span: self.since(start), kind }
    }

    fn stmt_kind(&mut self) -> StmtKind {
        match self.peek().clone() {
            TokenKind::Return => {
                self.bump();
                if matches!(self.peek(), TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.parse_expr()))
                }
            }
            TokenKind::Break => {
                self.bump();
                StmtKind::Break
            }
            TokenKind::Continue => {
                self.bump();
                StmtKind::Continue
            }
            TokenKind::Var => {
                self.bump();
                let name = self.expect_ident("variable name");
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type())
                } else if crate::types::starts_type(self.peek()) {
                    // Draft-0 form `var x T = v`.
                    let ty = self.parse_type();
                    self.missing_colon(&name, &ty, "var ", " = ...");
                    Some(ty)
                } else {
                    None
                };
                // `var x: T` without `=`: declared, initialized later.
                if ty.is_some() && matches!(self.peek(), TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof) {
                    return StmtKind::Let { mutable: true, name, ty, value: None };
                }
                if ty.is_none() && matches!(self.peek(), TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof) {
                    let span = name.span;
                    self.error(
                        Diagnostic::error("E1019", "uninit_var_needs_type", format!("`var {}` without a value needs a type", name.name))
                            .primary(span, "")
                            .help(format!("write `var {}: <type>` and assign it before use, or give it a value: `var {} = ...`", name.name, name.name)),
                    );
                    return StmtKind::Let { mutable: true, name, ty, value: None };
                }
                if !self.expect(&TokenKind::Eq) {
                    return StmtKind::Error;
                }
                self.skip_newlines();
                StmtKind::Let { mutable: true, name, ty, value: Some(self.parse_expr()) }
            }
            TokenKind::If => StmtKind::If(self.parse_if()),
            TokenKind::For => self.parse_for(),
            TokenKind::Match => StmtKind::Match(self.parse_match()),
            TokenKind::LBrace => StmtKind::Block(self.parse_block()),
            TokenKind::Unsafe => {
                self.bump();
                StmtKind::Unsafe(self.parse_block())
            }
            TokenKind::Spawn if (*self.nth(1) == TokenKind::Fn || matches!(self.nth(1), TokenKind::Ident(n) if n == "move")) => {
                StmtKind::Expr(self.parse_expr())
            }
            TokenKind::Spawn => {
                self.bump();
                StmtKind::Spawn(self.parse_expr())
            }
            TokenKind::Ident(n) if n == "scope" && *self.nth(1) == TokenKind::LBrace => {
                self.bump();
                StmtKind::Scope(self.parse_block())
            }
            TokenKind::Ident(_) if *self.nth(1) == TokenKind::ColonEq => {
                let name = self.expect_ident("name");
                self.bump();
                self.skip_newlines();
                StmtKind::Let { mutable: false, name, ty: None, value: Some(self.parse_expr()) }
            }
            TokenKind::Ident(_) if *self.nth(1) == TokenKind::Colon => {
                let name = self.expect_ident("name");
                self.bump();
                let ty = self.parse_type();
                self.typed_let(name, ty)
            }
            TokenKind::Ident(_) if crate::types::starts_type(&self.nth(1).clone()) && self.colon_eq_on_line() => {
                // Draft-0 form `x T := v`: diagnose with a fix-it, then parse it
                // as the binding it clearly is.
                let name = self.expect_ident("name");
                let ty = self.parse_type();
                self.missing_colon(&name, &ty, "", " := ...");
                self.typed_let(name, ty)
            }
            _ => self.expr_or_assign(),
        }
    }

    /// Rest of `name: Type := value` after the type.
    fn typed_let(&mut self, name: Ident, ty: Type) -> StmtKind {
        if self.at(&TokenKind::Eq) {
            let span = self.span();
            self.error(
                Diagnostic::error("E1001", "unexpected_token", "expected `:=`, found `=`")
                    .primary(span, "expected `:=`")
                    .help(format!("`=` assigns to an existing variable; to declare a mutable one write `var {}: ... = ...`", name.name)),
            );
            self.bump();
        } else if !self.expect(&TokenKind::ColonEq) {
            return StmtKind::Error;
        }
        self.skip_newlines();
        StmtKind::Let { mutable: false, name, ty: Some(ty), value: Some(self.parse_expr()) }
    }

    /// Is there a `:=` later on this line, outside delimiters? Only used to
    /// diagnose the draft-0 binding form; never to decide a valid parse.
    fn colon_eq_on_line(&self) -> bool {
        let mut depth = 0usize;
        for t in &self.tokens[self.pos..] {
            match t.kind {
                TokenKind::Newline | TokenKind::Eof => return false,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth = depth.saturating_sub(1),
                TokenKind::ColonEq if depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    fn missing_colon(&mut self, name: &Ident, ty: &Type, prefix: &str, suffix: &str) {
        self.error(
            Diagnostic::error("E1018", "missing_type_colon", "a type annotation on a binding needs `:`")
                .primary(name.span.to(ty.span), "")
                .help(format!("write `{prefix}{}: {}{suffix}`", name.name, tarn_ast::type_to_string(ty)))
                .note("see ADR 0011: `x: T := v` and `var x: T = v`"),
        );
    }

    fn expr_or_assign(&mut self) -> StmtKind {
        let e = self.parse_expr();
        if matches!(e.kind, ExprKind::Error) {
            return StmtKind::Error;
        }
        if self.at(&TokenKind::Eq) {
            if !is_place(&e) {
                self.error(
                    Diagnostic::error("E1008", "invalid_assignment_target", "cannot assign to this expression")
                        .primary(e.span, "not a variable, field, index or `*r`")
                        .help("only `name`, `a.field`, `a[i]` and `*r` can be assigned"),
                );
            }
            self.bump();
            self.skip_newlines();
            let value = self.parse_expr();
            return StmtKind::Assign { target: e, value };
        }
        StmtKind::Expr(e)
    }

    /// Condition of `if`/`for`/`match`: no bare struct literals.
    fn parse_cond(&mut self) -> Expr {
        let e = self.with_no_struct(|p| p.parse_expr());
        // `if p == Point{x: 1} {` — diagnose the likely struct literal.
        let looks_like_lit = *self.peek() == TokenKind::LBrace
            && matches!(self.nth(1), TokenKind::Ident(_))
            && matches!(self.nth(2), TokenKind::Colon | TokenKind::Comma);
        // The type name is the rightmost operand: `p == Point` + `{x: 1}`.
        let mut last = &e;
        while let ExprKind::Binary { rhs, .. } = &last.kind {
            last = rhs;
        }
        let id = self.id();
        if looks_like_lit && let Some(path) = crate::expr_to_path(last, id) {
            let lit = self.parse_struct_lit(path, last.span);
            self.error(
                Diagnostic::error("E1009", "struct_literal_in_condition", "struct literal not allowed here")
                    .primary(lit.span, "")
                    .help("wrap the struct literal in parentheses: `(Point{x: 1})`"),
            );
            return self.error_expr(e.span.to(lit.span));
        }
        e
    }

    fn parse_if(&mut self) -> IfStmt {
        let start = self.span();
        self.expect(&TokenKind::If);
        let cond = self.parse_cond();
        let then_block = self.parse_block();
        let mut else_branch = None;
        if *self.peek() == TokenKind::Newline && *self.peek_past_newlines() == TokenKind::Else {
            self.skip_newlines();
            let span = self.span();
            self.error(
                Diagnostic::error("E1016", "else_on_new_line", "`else` must be on the same line as `}`")
                    .primary(span, "")
                    .help("write `} else {`"),
            );
        }
        if self.eat(&TokenKind::Else) {
            else_branch = Some(Box::new(if self.at(&TokenKind::If) {
                ElseBranch::If(self.parse_if())
            } else {
                ElseBranch::Block(self.parse_block())
            }));
        }
        IfStmt { span: self.since(start), cond, then_block, else_branch }
    }

    fn parse_for(&mut self) -> StmtKind {
        self.bump();
        let kind = if self.at(&TokenKind::LBrace) {
            ForKind::Infinite
        } else if matches!(self.peek(), TokenKind::Ident(_)) && *self.nth(1) == TokenKind::In {
            let binding = self.expect_ident("loop variable");
            self.bump();
            ForKind::In { binding, iter: self.parse_cond() }
        } else {
            ForKind::While(self.parse_cond())
        };
        StmtKind::For(ForStmt { kind, body: self.parse_block() })
    }

    pub(crate) fn parse_match(&mut self) -> MatchStmt {
        self.bump();
        let scrutinee = self.parse_cond();
        let mut arms = Vec::new();
        let open = self.span();
        if !self.expect(&TokenKind::LBrace) {
            return MatchStmt { scrutinee, arms };
        }
        self.with_mode(false, |p| {
            loop {
                p.skip_newlines();
                match p.peek() {
                    TokenKind::RBrace => {
                        p.bump();
                        break;
                    }
                    TokenKind::Eof => {
                        p.unclosed(open);
                        break;
                    }
                    _ => {}
                }
                let before = p.pos;
                let start = p.span();
                let id = p.id();
                let pattern = p.parse_pattern();
                let guard = if p.eat(&TokenKind::If) { Some(p.parse_expr()) } else { None };
                let body = if p.expect(&TokenKind::FatArrow) {
                    p.skip_newlines();
                    p.parse_stmt()
                } else {
                    let span = p.span();
                    Stmt { id: p.id(), span, kind: StmtKind::Error }
                };
                let failed = matches!(body.kind, StmtKind::Error);
                arms.push(MatchArm { id, span: p.since(start), pattern, guard, body: Box::new(body) });
                p.end_stmt(failed);
                if p.pos == before {
                    p.bump();
                }
            }
        });
        MatchStmt { scrutinee, arms }
    }
}

fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. } => true,
        ExprKind::Paren(inner) => is_place(inner),
        ExprKind::Unary { op: tarn_ast::UnaryOp::Deref, .. } => true,
        _ => false,
    }
}
