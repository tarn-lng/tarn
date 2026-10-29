//! Items: imports, functions, structs, enums, interfaces, impls.

use crate::Parser;
use tarn_ast::*;
use tarn_diagnostics::Diagnostic;
use tarn_lexer::TokenKind;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnContext {
    /// Top-level `fn` (receiver only allowed with an owner: `fn T.m(&self)`).
    Free,
    /// Inside `interface { }`: no body.
    Interface,
    /// Inside `impl I for T { }`.
    Impl,
}

pub(crate) fn starts_item(k: &TokenKind) -> bool {
    use TokenKind::*;
    matches!(k, Fn | Async | Struct | Enum | Interface | Impl | Import | Pub | Copy | Extern)
}

impl Parser {
    pub(crate) fn parse_module(&mut self) -> Module {
        let start = self.span();
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            let before = self.pos;
            let item = self.parse_item();
            let failed = matches!(item.kind, ItemKind::Error);
            items.push(item);
            match self.peek() {
                TokenKind::Newline | TokenKind::Eof if !failed => {}
                _ => {
                    if !failed {
                        let found = crate::describe(self.peek());
                        let span = self.span();
                        self.error(
                            Diagnostic::error("E1001", "unexpected_token", format!("expected end of line after item, found {found}")).primary(span, "expected a line break"),
                        );
                    }
                    self.recover_item();
                }
            }
            if self.pos == before {
                self.bump();
            }
        }
        let file = start.file;
        let end = self.tokens.last().map(|t| t.span.end).unwrap_or(0);
        Module { file, items, span: tarn_diagnostics::Span::new(file, 0, end) }
    }

    fn parse_item(&mut self) -> Item {
        let start = self.span();
        let id = self.id();
        let pub_span = self.span();
        let is_pub = self.eat(&TokenKind::Pub);
        let kind = match self.peek().clone() {
            TokenKind::Import => {
                if is_pub {
                    self.error(
                        Diagnostic::error("E1015", "invalid_pub", "`pub` is not allowed on imports")
                            .primary(pub_span, "remove this")
                            .note("re-exports are not supported in draft 0"),
                    );
                }
                self.bump();
                let span = self.span();
                if let TokenKind::Str(path) = self.peek().clone() {
                    self.bump();
                    ItemKind::Import(Import { path, path_span: span })
                } else {
                    let found = crate::describe(self.peek());
                    self.error(
                        Diagnostic::error("E1001", "unexpected_token", format!("expected module path string, found {found}"))
                            .primary(span, "expected a string such as \"fs\"")
                            .help("write `import \"fs\"`"),
                    );
                    ItemKind::Error
                }
            }
            TokenKind::Fn => ItemKind::Fn(self.parse_fn(FnContext::Free, None)),
            TokenKind::Async => ItemKind::Fn(self.parse_fn(FnContext::Free, None)),
            TokenKind::Unsafe if self.nth(1) == &TokenKind::Fn => ItemKind::Fn(self.parse_fn(FnContext::Free, None)),
            TokenKind::Extern => {
                self.bump();
                let abi = match self.peek().clone() {
                    TokenKind::Str(s) => {
                        self.bump();
                        s
                    }
                    _ => {
                        self.expect(&TokenKind::Str(String::new()));
                        String::new()
                    }
                };
                if !self.at(&TokenKind::Fn) {
                    self.expect(&TokenKind::Fn);
                    return Item { id, span: self.since(start), is_pub, kind: ItemKind::Error };
                }
                ItemKind::Fn(self.parse_fn(FnContext::Free, Some(abi)))
            }
            TokenKind::Copy => {
                self.bump();
                if self.at(&TokenKind::Enum) {
                    self.parse_enum(true)
                } else if self.expect(&TokenKind::Struct) {
                    self.parse_struct(true)
                } else {
                    return Item { id, span: self.since(start), is_pub, kind: ItemKind::Error };
                }
            }
            TokenKind::Struct => {
                self.bump();
                self.parse_struct(false)
            }
            TokenKind::Enum => self.parse_enum(false),
            TokenKind::Const => {
                self.bump();
                let name = self.expect_ident("constant name");
                let ty = self.parse_type();
                self.expect(&TokenKind::Eq);
                ItemKind::Const(ConstDecl { name, ty, value: self.parse_expr() })
            }
            TokenKind::Interface => self.parse_interface(),
            TokenKind::Impl => self.parse_impl(is_pub, pub_span),
            _ => {
                let found = crate::describe(self.peek());
                let span = self.span();
                self.error(
                    Diagnostic::error("E1013", "expected_item", format!("expected item, found {found}"))
                        .primary(span, "expected `fn`, `const`, `struct`, `enum`, `interface`, `impl` or `import`")
                        .note("statements must be inside a function body"),
                );
                ItemKind::Error
            }
        };
        Item { id, span: self.since(start), is_pub, kind }
    }

    pub(crate) fn parse_fn(&mut self, ctx: FnContext, abi: Option<String>) -> FnDecl {
        let start = if abi.is_some() { self.prev_span } else { self.span() };
        let id = self.id();
        let is_unsafe = self.eat(&TokenKind::Unsafe);
        let is_async = self.eat(&TokenKind::Async);
        self.expect(&TokenKind::Fn);
        let mut name = self.expect_ident("function name");
        // `fn f<T>(` vs `fn Pair<A, B>.m(`: the token after the list decides
        // whose list it is (ADR 0015); no backtracking.
        let mut generics = self.parse_generic_params();
        let mut owner = None;
        if ctx == FnContext::Free && self.at(&TokenKind::Dot) {
            self.bump();
            owner = Some(Owner { name, params: std::mem::take(&mut generics) });
            name = self.expect_ident("method name");
            generics = self.parse_generic_params();
        }
        let allow_receiver = owner.is_some() || ctx != FnContext::Free;
        let (receiver, params) = self.parse_params(allow_receiver);
        let ret = if crate::types::starts_type(self.peek()) { Some(self.parse_type()) } else { None };

        let borrows = if matches!(self.peek(), TokenKind::Ident(n) if n == "borrows") {
            self.bump();
            self.expect(&TokenKind::LParen);
            let mut names = Vec::new();
            while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
                names.push(self.expect_ident("borrow source parameter"));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RParen);
            Some(names)
        } else {
            None
        };
        let body = if self.at(&TokenKind::LBrace) {
            let b = self.parse_block();
            if ctx == FnContext::Interface {
                self.error(
                    Diagnostic::error("E1012", "unexpected_function_body", "interface methods cannot have a body")
                        .primary(b.span, "remove this body")
                        .help("implement the method in an `impl` block"),
                );
            }
            Some(b)
        } else {
            if ctx != FnContext::Interface && abi.is_none() {
                let span = self.span();
                let found = crate::describe(self.peek());
                self.error(
                    Diagnostic::error("E1011", "missing_function_body", format!("function `{}` has no body", name.name))
                        .primary(span, format!("expected `{{`, found {found}"))
                        .help("the opening `{` must be on the same line as the signature"),
                );
                // Recover the body written on the next line, without a second error.
                if *self.peek() == TokenKind::Newline && *self.peek_past_newlines() == TokenKind::LBrace {
                    self.skip_newlines();
                    let b = self.parse_block();
                    return FnDecl { id, span: self.since(start), is_async, is_unsafe, abi, owner, name, generics, receiver, params, ret, borrows, body: Some(b) };
                }
            }
            None
        };
        FnDecl { id, span: self.since(start), is_async, is_unsafe, abi, owner, name, generics, receiver, params, ret, borrows, body }
    }

    /// `(` [receiver] {`,` param} [`,`] `)`
    fn parse_params(&mut self, allow_receiver: bool) -> (Option<Receiver>, Vec<Param>) {
        let mut receiver = None;
        let mut params = Vec::new();
        if !self.expect(&TokenKind::LParen) {
            return (None, params);
        }
        self.with_mode(true, |p| {
            let mut index = 0;
            while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) && !p.at(&TokenKind::LBrace) {
                let start = p.span();
                let before = p.pos;
                if let Some(kind) = p.receiver_ahead() {
                    for _ in 0..receiver_len(kind) {
                        p.bump();
                    }
                    let span = p.since(start);
                    if !allow_receiver || index != 0 {
                        let msg = if allow_receiver { "`self` must be the first parameter" } else { "`self` is only allowed in methods" };
                        p.error(Diagnostic::error("E1014", "misplaced_receiver", msg).primary(span, "").help("declare methods as `fn Type.name(&self, ...)`"));
                    } else {
                        receiver = Some(Receiver { id: p.id(), kind, span });
                    }
                } else {
                    let id = p.id();
                    let name = p.expect_ident("parameter name");
                    let ty = p.parse_type();
                    params.push(Param { id, span: p.since(start), name, ty });
                }
                index += 1;
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
                if p.pos == before {
                    p.bump();
                }
            }
            if !p.expect(&TokenKind::RParen) {
                // Skip to `)` or the body.
                while !matches!(p.peek(), TokenKind::RParen | TokenKind::LBrace | TokenKind::Eof) {
                    p.bump();
                }
                p.eat(&TokenKind::RParen);
            }
        });
        (receiver, params)
    }

    fn receiver_ahead(&mut self) -> Option<ReceiverKind> {
        let is_self = |k: &TokenKind| matches!(k, TokenKind::Ident(n) if n == "self");
        if is_self(self.peek()) {
            return Some(ReceiverKind::Value);
        }
        if *self.peek() == TokenKind::Amp {
            if is_self(&self.nth(1).clone()) {
                return Some(ReceiverKind::Ref);
            }
            if *self.nth(1) == TokenKind::Mut && is_self(&self.nth(2).clone()) {
                return Some(ReceiverKind::RefMut);
            }
        }
        None
    }

    /// `<T, U: Bound + Other>` (optional).
    pub(crate) fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        let mut out = Vec::new();
        if !self.eat(&TokenKind::Lt) {
            return out;
        }
        self.with_mode(true, |p| {
            loop {
                if p.at(&TokenKind::Gt) {
                    break;
                }
                let id = p.id();
                let name = p.expect_ident("type parameter name");
                let mut bounds = Vec::new();
                if p.eat(&TokenKind::Colon) {
                    bounds.push(p.parse_path(true));
                    while p.eat(&TokenKind::Plus) {
                        bounds.push(p.parse_path(true));
                    }
                }
                out.push(GenericParam { id, name, bounds });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            p.expect_gt();
        });
        out
    }

    fn parse_struct(&mut self, is_copy: bool) -> ItemKind {
        let name = self.expect_ident("struct name");
        let generics = self.parse_generic_params();
        let mut fields = Vec::new();
        self.parse_body("field", |p| {
            let start = p.span();
            let id = p.id();
            let is_pub = p.eat(&TokenKind::Pub);
            let name = p.expect_ident("field name");
            let ty = p.parse_type();
            fields.push(FieldDecl { id, span: p.since(start), is_pub, name, ty });
        });
        ItemKind::Struct(StructDecl { is_copy, name, generics, fields })
    }

    fn parse_enum(&mut self, is_copy: bool) -> ItemKind {
        self.bump();
        let name = self.expect_ident("enum name");
        let generics = self.parse_generic_params();
        let mut variants = Vec::new();
        self.parse_body("variant", |p| {
            let start = p.span();
            let id = p.id();
            let name = p.expect_ident("variant name");
            let mut fields = Vec::new();
            if p.eat(&TokenKind::LParen) {
                p.with_mode(true, |p| {
                    while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                        fields.push(p.parse_type());
                        if !p.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    p.expect(&TokenKind::RParen);
                });
            }
            variants.push(Variant { id, span: p.since(start), name, fields });
        });
        ItemKind::Enum(EnumDecl { is_copy, name, generics, variants })
    }

    fn parse_interface(&mut self) -> ItemKind {
        self.bump();
        let name = self.expect_ident("interface name");
        let generics_start = self.span();
        let generics = self.parse_generic_params();
        if !generics.is_empty() {
            let span = self.since(generics_start);
            self.error(
                Diagnostic::error("E1021", "generic_interface", "interfaces cannot have type parameters")
                    .primary(span, "remove these type parameters")
                    .note("generic interfaces are not supported in v0")
                    .help("make the methods generic, or declare one interface per concrete type"),
            );
        }
        let mut methods = Vec::new();
        self.parse_body("method", |p| {
            if p.at(&TokenKind::Fn) {
                methods.push(p.parse_fn(FnContext::Interface, None));
            } else {
                p.expected_member("method declaration `fn name(...)`");
            }
        });
        ItemKind::Interface(InterfaceDecl { name, generics, methods })
    }

    fn parse_impl(&mut self, is_pub: bool, pub_span: tarn_diagnostics::Span) -> ItemKind {
        if is_pub {
            self.error(
                Diagnostic::error("E1015", "invalid_pub", "`pub` is not allowed on `impl`")
                    .primary(pub_span, "remove this")
                    .note("an implementation is visible wherever both the interface and the type are"),
            );
        }
        self.bump();
        let interface = self.parse_path(true);
        self.expect(&TokenKind::For);
        let (target, binder_bounds) = self.parse_impl_target();
        let mut methods = Vec::new();
        self.parse_body("method", |p| {
            if p.at(&TokenKind::Fn) {
                methods.push(p.parse_fn(FnContext::Impl, None));
            } else {
                p.expected_member("method `fn name(...) { ... }`");
            }
        });
        ItemKind::Impl(ImplDecl { interface, target, binder_bounds, methods })
    }

    /// `Pair<A: Eq, B>`: a path type whose arguments may carry bounds
    /// (ADR 0078). Other targets parse as ordinary types; resolution rejects them.
    fn parse_impl_target(&mut self) -> (Type, Vec<Vec<Path>>) {
        if !matches!(self.peek(), TokenKind::Ident(_)) {
            return (self.parse_type(), Vec::new());
        }
        let start = self.span();
        let mut segments = vec![self.expect_ident("type name")];
        while self.at(&TokenKind::Dot) && matches!(self.nth(1), TokenKind::Ident(_)) {
            self.bump();
            segments.push(self.expect_ident("name"));
        }
        let mut args = Vec::new();
        let mut binder_bounds = Vec::new();
        if self.at(&TokenKind::Lt) {
            self.bump();
            self.with_mode(true, |p| {
                while !p.at(&TokenKind::Gt) && !p.at(&TokenKind::Shr) && !p.at(&TokenKind::Eof) {
                    args.push(p.parse_type());
                    let mut bounds = Vec::new();
                    if p.eat(&TokenKind::Colon) {
                        bounds.push(p.parse_path(true));
                        while p.eat(&TokenKind::Plus) {
                            bounds.push(p.parse_path(true));
                        }
                    }
                    binder_bounds.push(bounds);
                    if !p.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                p.expect_gt();
            });
        }
        let span = self.since(start);
        let path = Path { id: self.id(), span, segments, args };
        (Type { id: self.id(), span, kind: TypeKind::Path(path) }, binder_bounds)
    }

    fn expected_member(&mut self, what: &str) {
        let found = crate::describe(self.peek());
        let span = self.span();
        self.error(Diagnostic::error("E1001", "unexpected_token", format!("expected {what}, found {found}")).primary(span, ""));
        self.recover_stmt();
    }

    /// `{` members separated by line breaks `}` — shared by struct, enum,
    /// interface and impl bodies.
    fn parse_body(&mut self, member: &str, mut each: impl FnMut(&mut Parser)) {
        let open = self.span();
        if !self.expect(&TokenKind::LBrace) {
            return;
        }
        self.with_mode(false, |p| {
            loop {
                p.skip_newlines();
                match p.peek() {
                    TokenKind::RBrace => {
                        p.bump();
                        return;
                    }
                    TokenKind::Eof => {
                        p.unclosed(open);
                        return;
                    }
                    _ => {}
                }
                let before = p.pos;
                each(p);
                match p.peek().clone() {
                    TokenKind::Newline => {
                        p.bump();
                    }
                    TokenKind::RBrace | TokenKind::Eof => {}
                    TokenKind::Comma => {
                        let span = p.span();
                        p.error(
                            Diagnostic::error("E1001", "unexpected_token", format!("expected end of line after {member}, found `,`"))
                                .primary(span, "")
                                .help(format!("each {member} goes on its own line, without commas")),
                        );
                        p.bump();
                    }
                    k => {
                        let span = p.span();
                        p.error(Diagnostic::error("E1001", "unexpected_token", format!("expected end of line after {member}, found {}", crate::describe(&k))).primary(span, ""));
                        p.recover_stmt();
                    }
                }
                if p.pos == before {
                    p.bump();
                }
            }
        });
    }

    pub(crate) fn unclosed(&mut self, open: tarn_diagnostics::Span) {
        let span = self.span();
        self.error(Diagnostic::error("E1010", "unclosed_delimiter", "unclosed delimiter `{`").primary(span, "expected `}` before end of file").secondary(open, "opened here"));
    }
}

fn receiver_len(kind: ReceiverKind) -> usize {
    match kind {
        ReceiverKind::Value => 1,
        ReceiverKind::Ref => 2,
        ReceiverKind::RefMut => 3,
    }
}
