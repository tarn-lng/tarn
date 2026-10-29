//! Hand-written recursive-descent parser for Tarn, with Pratt-style
//! precedence climbing for binary operators. Deterministic: bounded
//! lookahead, no backtracking (ADR 0011).
//!
//! Grammar: `docs/grammar.md`. Newline policy: ADR 0008.
//!
//! The parser never stops at the first error. Every `parse_*` function
//! returns a node (possibly an `Error` node) and the callers resynchronize at
//! statement or item boundaries, so one mistake produces one diagnostic.

mod depth;
mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;

use tarn_ast::{Expr, ExprKind, Module, NodeId, Path};
use tarn_diagnostics::{Diagnostic, FileId, SourceFile, Span};
use tarn_lexer::{Comment, Token, TokenKind};

pub struct ParseResult {
    pub module: Module,
    /// Lexer diagnostics first, then parser diagnostics, in source order per phase.
    pub diagnostics: Vec<Diagnostic>,
    /// Comments, kept for the formatter.
    pub comments: Vec<Comment>,
}

/// Lex and parse one file.
pub fn parse_file(file_id: FileId, file: &SourceFile) -> ParseResult {
    let lexed = tarn_lexer::lex(file_id, file);
    let mut p = Parser::new(file_id, lexed.tokens);
    let mut module = p.parse_module();
    let mut diagnostics = lexed.diagnostics;
    // Trees past MAX_NESTING were already cut short and reported (E1020).
    let too_deep = p.too_deep;
    diagnostics.extend(p.diags);
    if !too_deep && let Some(d) = depth::check(&module) {
        diagnostics.push(d);
        depth::dismantle(&mut module);
    }
    ParseResult { module, diagnostics, comments: lexed.comments }
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diags: Vec<Diagnostic>,
    next_id: u32,
    /// Newline significance stack: `true` = newlines are ignored (inside
    /// `()`, `[]`, `<>`, literal braces); `false` = significant (blocks).
    /// Empty = top level, significant.
    modes: Vec<bool>,
    /// Struct literals are not allowed in `if`/`for`/`match` heads.
    no_struct: bool,
    /// Span of the last consumed token; node spans end here.
    prev_span: Span,
    /// Token index of the last reported error, to avoid cascades.
    last_error_pos: Option<usize>,
    /// Current expression/block/type/pattern nesting, bounded by MAX_NESTING.
    depth: usize,
    /// Set once nesting overflowed: the rest of the file is skipped silently.
    too_deep: bool,
}

/// Deepest accepted nesting of expressions, blocks, types and patterns. Every
/// later phase walks the AST recursively, so this bound keeps them all within
/// the stack instead of aborting on generated or malformed input. Real code
/// nests a few dozen levels at most; 128 levels still fit a 2 MiB thread
/// stack in an unoptimized build through every phase (256 did not).
pub(crate) const MAX_NESTING: usize = 128;

pub(crate) fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Ident(n) => format!("identifier `{n}`"),
        TokenKind::Int(_) | TokenKind::Float(_) => "number".to_string(),
        TokenKind::Str(_) => "string".to_string(),
        TokenKind::Newline => "end of line".to_string(),
        TokenKind::Eof => "end of file".to_string(),
        k => format!("`{}`", k.name()),
    }
}

fn same(a: &TokenKind, b: &TokenKind) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

impl Parser {
    fn new(file: FileId, tokens: Vec<Token>) -> Parser {
        let first = tokens.first().map(|t| t.span).unwrap_or(Span::new(file, 0, 0));
        Parser {
            tokens,
            pos: 0,
            diags: Vec::new(),
            next_id: 0,
            modes: Vec::new(),
            no_struct: false,
            prev_span: Span::new(file, first.start, first.start),
            last_error_pos: None,
            depth: 0,
            too_deep: false,
        }
    }

    // ------------------------------------------------------------ token access

    fn newlines_ignored(&self) -> bool {
        self.modes.last().copied().unwrap_or(false)
    }

    fn skip_ignored(&mut self) {
        if self.newlines_ignored() {
            self.skip_newlines();
        }
    }

    /// Skip newline tokens explicitly (incomplete-expression contexts).
    fn skip_newlines(&mut self) {
        while self.tokens[self.pos].kind == TokenKind::Newline {
            self.pos += 1;
        }
    }

    fn tok(&mut self) -> &Token {
        self.skip_ignored();
        &self.tokens[self.pos]
    }

    fn peek(&mut self) -> &TokenKind {
        &self.tok().kind
    }

    /// Raw lookahead `n` tokens after the current one (no newline skipping).
    fn nth(&mut self, n: usize) -> &TokenKind {
        self.skip_ignored();
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    /// First non-newline token at or after the current position.
    fn peek_past_newlines(&self) -> &TokenKind {
        let mut i = self.pos;
        while self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }
        &self.tokens[i].kind
    }

    fn at(&mut self, kind: &TokenKind) -> bool {
        same(self.peek(), kind)
    }

    fn span(&mut self) -> Span {
        self.tok().span
    }

    fn bump(&mut self) -> Token {
        self.skip_ignored();
        let t = self.tokens[self.pos].clone();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        self.prev_span = t.span;
        t
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        let found = describe(self.peek());
        let span = self.span();
        self.error(
            Diagnostic::error("E1001", "unexpected_token", format!("expected `{}`, found {found}", kind.name()))
                .primary(span, format!("expected `{}`", kind.name())),
        );
        false
    }

    fn expect_ident(&mut self, what: &str) -> tarn_ast::Ident {
        if let TokenKind::Ident(name) = self.peek().clone() {
            let t = self.bump();
            return tarn_ast::Ident { name, span: t.span };
        }
        let found = describe(self.peek());
        let span = self.span();
        self.error(
            Diagnostic::error("E1001", "unexpected_token", format!("expected {what}, found {found}"))
                .primary(span, format!("expected {what}")),
        );
        tarn_ast::Ident { name: String::from("<error>"), span: Span::new(span.file, span.start, span.start) }
    }

    /// Accept `>`, splitting `>>` when closing nested generic arguments.
    fn expect_gt(&mut self) -> bool {
        if self.at(&TokenKind::Shr) {
            self.split_token(TokenKind::Gt);
            return true;
        }
        self.expect(&TokenKind::Gt)
    }

    /// Consume the first character of a two-character token (`>>`, `&&`),
    /// leaving the second one as `rest`.
    fn split_token(&mut self, rest: TokenKind) {
        self.skip_ignored();
        let i = self.pos;
        let orig = self.tokens[i].clone();
        let first = Span::new(orig.span.file, orig.span.start, orig.span.start + 1);
        let t = &mut self.tokens[i];
        t.kind = rest;
        t.span.start += 1;
        t.column += 1;
        self.prev_span = first;
    }

    // ------------------------------------------------------------ modes

    fn with_mode<T>(&mut self, ignore_newlines: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        self.modes.push(ignore_newlines);
        let saved = self.no_struct;
        self.no_struct = false;
        let r = f(self);
        self.no_struct = saved;
        self.modes.pop();
        r
    }

    fn with_no_struct<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.no_struct;
        self.no_struct = true;
        let r = f(self);
        self.no_struct = saved;
        r
    }

    // ------------------------------------------------------------ ids & spans

    fn id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Span from `start` to the end of the last consumed token.
    fn since(&self, start: Span) -> Span {
        if self.prev_span.end < start.start {
            return Span::new(start.file, start.start, start.start);
        }
        Span::new(start.file, start.start, self.prev_span.end)
    }

    // ------------------------------------------------------------ errors

    /// Enter one nesting level. Past MAX_NESTING, report E1020 once and skip
    /// to the end of the file: recovering at that depth would only cascade.
    fn descend(&mut self) -> bool {
        if self.too_deep {
            return false;
        }
        if self.depth >= MAX_NESTING {
            let span = self.span();
            self.error(
                Diagnostic::error("E1020", "nesting_too_deep", format!("code is nested more than {MAX_NESTING} levels deep"))
                    .primary(span, "nesting limit reached here")
                    .help("split the code into smaller functions or flatten the expression"),
            );
            self.too_deep = true;
            while *self.peek() != TokenKind::Eof {
                self.bump();
            }
            return false;
        }
        self.depth += 1;
        true
    }

    fn ascend(&mut self) {
        self.depth -= 1;
    }

    fn error(&mut self, d: Diagnostic) {
        if self.too_deep {
            return;
        }
        if self.last_error_pos == Some(self.pos) {
            return;
        }
        self.last_error_pos = Some(self.pos);
        self.diags.push(d);
    }

    /// Skip to the end of the current statement: a newline or `}` at nesting
    /// depth 0 (not consumed), balancing delimiters on the way.
    fn recover_stmt(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.tokens[self.pos].kind {
                TokenKind::Eof => return,
                TokenKind::Newline if depth == 0 => return,
                TokenKind::RBrace if depth == 0 => return,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            self.prev_span = self.tokens[self.pos].span;
            self.pos += 1;
        }
    }

    /// Skip to the next line that starts with an item keyword at depth 0.
    fn recover_item(&mut self) {
        let mut depth = 0usize;
        loop {
            let k = &self.tokens[self.pos].kind;
            match k {
                TokenKind::Eof => return,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth = depth.saturating_sub(1),
                TokenKind::Newline if depth == 0 => {
                    let next = &self.tokens[self.pos + 1].kind;
                    if items::starts_item(next) || *next == TokenKind::Eof {
                        self.pos += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.prev_span = self.tokens[self.pos].span;
            self.pos += 1;
        }
    }

    fn error_expr(&mut self, span: Span) -> Expr {
        Expr { id: self.id(), span, kind: ExprKind::Error }
    }
}

/// `a`, `a.b.C` as a path, if the expression is only identifiers and dots.
pub(crate) fn expr_to_path(e: &Expr, id: NodeId) -> Option<Path> {
    fn go(e: &Expr, out: &mut Vec<tarn_ast::Ident>) -> bool {
        match &e.kind {
            ExprKind::Ident(n) => {
                out.push(tarn_ast::Ident { name: n.clone(), span: e.span });
                true
            }
            ExprKind::Field { base, name } => {
                if !go(base, out) {
                    return false;
                }
                out.push(name.clone());
                true
            }
            _ => false,
        }
    }
    let mut segments = Vec::new();
    go(e, &mut segments).then(|| Path { id, span: e.span, segments, args: Vec::new() })
}
