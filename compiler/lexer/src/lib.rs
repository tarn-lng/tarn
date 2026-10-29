//! Lexer for Tarn source files.
//!
//! Produces tokens with spans, preserves line breaks as `Newline` tokens
//! (consecutive breaks collapse into one; the parser decides which ones end a
//! statement, ADR 0008) and collects comments as trivia for the formatter. Errors are
//! reported as diagnostics; the lexer always recovers and reaches `Eof`.

mod token;

pub use token::{FPiece, KEYWORDS, Token, TokenKind, keyword};

use tarn_diagnostics::{Diagnostic, FileId, LineCol, SourceFile, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub span: Span,
    /// `///` doc comment.
    pub doc: bool,
    /// Text after the `//` or `///` marker.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct LexResult {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(file_id: FileId, file: &SourceFile) -> LexResult {
    let mut lx = Lexer {
        file_id,
        file,
        src: file.text.as_bytes(),
        pos: 0,
        out: LexResult::default(),
        last: None,
    };
    lx.run();
    lx.out
}

struct Lexer<'a> {
    file_id: FileId,
    file: &'a SourceFile,
    src: &'a [u8],
    pos: usize,
    out: LexResult,
    /// Offset and position of the previous token. Tokens arrive in order, so
    /// a token on the same line counts characters only from there: counting
    /// from the line start made long generated lines quadratic.
    last: Option<(u32, LineCol)>,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        self.peek_at(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn span(&self, start: usize) -> Span {
        Span::new(self.file_id, start as u32, self.pos as u32)
    }

    fn text(&self, start: usize) -> &'a str {
        &self.file.text[start..self.pos]
    }

    fn error(&mut self, d: Diagnostic) {
        self.out.diagnostics.push(d);
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start);
        self.push_raw(kind, span);
    }

    fn push_raw(&mut self, kind: TokenKind, span: Span) {
        let lc = match self.last {
            Some((offset, prev)) if offset <= span.start && !self.src[offset as usize..span.start as usize].contains(&b'\n') => LineCol {
                line: prev.line,
                column: prev.column + self.file.text[offset as usize..span.start as usize].chars().count() as u32,
            },
            _ => self.file.line_col(span.start),
        };
        self.last = Some((span.start, lc));
        self.out.tokens.push(Token { kind, span, line: lc.line, column: lc.column });
    }

    /// A line break becomes a `Newline` token unless it would be the first
    /// token or would follow another `Newline`.
    fn line_break(&mut self, start: usize) {
        let collapse = self.out.tokens.last().is_none_or(|t| t.kind == TokenKind::Newline);
        if !collapse {
            let span = self.span(start);
            self.push_raw(TokenKind::Newline, span);
        }
    }

    fn run(&mut self) {
        loop {
            let start = self.pos;
            let c = self.peek();
            if self.pos >= self.src.len() {
                self.push_raw(TokenKind::Eof, self.span(start));
                return;
            }
            match c {
                b'\n' => {
                    self.pos += 1;
                    self.line_break(start);
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'/' if self.peek_at(1) == b'/' => self.comment(),
                b'"' => self.string(),
                b'0'..=b'9' => self.number(),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.ident(),
                _ => self.punct(),
            }
        }
    }

    fn comment(&mut self) {
        let start = self.pos;
        let doc = self.src[self.pos..].starts_with(b"///") && self.peek_at(3) != b'/';
        let marker = if doc { 3 } else { 2 };
        while self.pos < self.src.len() && self.peek() != b'\n' {
            self.pos += 1;
        }
        let text = self.file.text[start + marker..self.pos].trim_end_matches('\r').to_string();
        let span = self.span(start);
        self.out.comments.push(Comment { span, doc, text });
    }

    fn ident(&mut self) {
        let start = self.pos;
        while matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') {
            self.pos += 1;
        }
        let text = self.text(start);
        if text == "f" && self.peek() == b'"' {
            self.fstring(start);
            return;
        }
        let kind = keyword(text).unwrap_or_else(|| TokenKind::Ident(text.to_string()));
        self.push(kind, start);
    }

    fn number(&mut self) {
        let start = self.pos;
        let radix = match (self.peek(), self.peek_at(1)) {
            (b'0', b'x') => 16,
            (b'0', b'o') => 8,
            (b'0', b'b') => 2,
            _ => 10,
        };
        let mut is_float = false;
        if radix != 10 {
            self.pos += 2;
            self.eat_alnum();
        } else {
            self.eat_digits();
            // `1.5` is a float; `1..2` and `1.foo` are not.
            if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
                is_float = true;
                self.pos += 1;
                self.eat_digits();
            }
            if matches!(self.peek(), b'e' | b'E') {
                let sign = matches!(self.peek_at(1), b'+' | b'-') as usize;
                if self.peek_at(1 + sign).is_ascii_digit() {
                    is_float = true;
                    self.pos += 1 + sign;
                    self.eat_digits();
                }
            }
            // Trailing letters (`12abc`) make the literal invalid as a whole.
            self.eat_alnum();
        }

        let text = self.text(start);
        let span = self.span(start);
        let digits_part = if radix == 10 { text } else { &text[2..] };
        let valid_chars = |s: &str| {
            !s.is_empty()
                && !s.starts_with('_')
                && !s.ends_with('_')
                && s.chars().all(|c| c == '_' || c.is_digit(radix))
        };

        if is_float {
            let clean: String = text.chars().filter(|&c| c != '_').collect();
            let ok = !text.contains("_.")
                && !text.contains("._")
                && !text.ends_with('_')
                && clean.parse::<f64>().is_ok();
            if !ok {
                self.invalid_number(span, text);
            }
            self.push(TokenKind::Float(clean), start);
            return;
        }

        if !valid_chars(digits_part) {
            self.invalid_number(span, text);
            self.push(TokenKind::Int(0), start);
            return;
        }
        let clean: String = digits_part.chars().filter(|&c| c != '_').collect();
        match u64::from_str_radix(&clean, radix) {
            Ok(v) => self.push(TokenKind::Int(v), start),
            Err(_) => {
                self.error(
                    Diagnostic::error("E0005", "number_out_of_range", "integer literal is too large")
                        .primary(span, "does not fit in 64 bits")
                        .note(format!("the largest integer literal is {}", u64::MAX)),
                );
                self.push(TokenKind::Int(0), start);
            }
        }
    }

    fn invalid_number(&mut self, span: Span, text: &str) {
        self.error(
            Diagnostic::error("E0004", "invalid_number", format!("invalid numeric literal `{text}`"))
                .primary(span, "")
                .help("digits may be separated by `_`, but not start or end with it; prefixes are 0x, 0o, 0b"),
        );
    }

    fn eat_digits(&mut self) {
        while matches!(self.peek(), b'0'..=b'9' | b'_') {
            self.pos += 1;
        }
    }

    fn eat_alnum(&mut self) {
        while matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') {
            self.pos += 1;
        }
    }

    fn string(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let mut value = String::new();
        loop {
            let c = self.peek();
            if self.pos >= self.src.len() || c == b'\n' {
                let span = self.span(start);
                self.error(
                    Diagnostic::error("E0002", "unterminated_string", "unterminated string literal")
                        .primary(span, "string starts here")
                        .help("close it with `\"`; strings cannot span lines"),
                );
                break;
            }
            match c {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => self.escape(&mut value),
                _ => {
                    // Copy one UTF-8 scalar.
                    let ch = self.file.text[self.pos..].chars().next().unwrap();
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        self.push(TokenKind::Str(value), start);
    }

    /// `f"text {name.field} {{literal}}"`; `start` is at the `f`.
    fn fstring(&mut self, start: usize) {
        self.pos += 1;
        let mut pieces = Vec::new();
        let mut text = String::new();
        loop {
            let c = self.peek();
            if self.pos >= self.src.len() || c == b'\n' {
                let span = self.span(start);
                self.error(
                    Diagnostic::error("E0002", "unterminated_string", "unterminated string literal")
                        .primary(span, "string starts here")
                        .help("close it with `\"`; strings cannot span lines"),
                );
                break;
            }
            match c {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => self.escape(&mut text),
                b'{' if self.peek_at(1) == b'{' => {
                    text.push('{');
                    self.pos += 2;
                }
                b'}' if self.peek_at(1) == b'}' => {
                    text.push('}');
                    self.pos += 2;
                }
                b'{' => {
                    if !text.is_empty() {
                        pieces.push(FPiece::Text(std::mem::take(&mut text)));
                    }
                    if let Some(path) = self.hole(start) {
                        pieces.push(FPiece::Hole(path));
                    }
                }
                b'}' => {
                    let at = self.pos;
                    self.pos += 1;
                    self.error(
                        Diagnostic::error("E0006", "invalid_interpolation", "unmatched `}` in an interpolated string")
                            .primary(self.span(at), "")
                            .help("write `}}` for a literal brace"),
                    );
                }
                _ => {
                    let ch = self.file.text[self.pos..].chars().next().unwrap();
                    text.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if !text.is_empty() {
            pieces.push(FPiece::Text(text));
        }
        self.push(TokenKind::FStr(pieces), start);
    }

    /// `{a.b}` inside `f"..."`, at the `{`: dotted names up to `}`.
    fn hole(&mut self, token_start: usize) -> Option<Vec<(String, u32)>> {
        let open = self.pos;
        self.pos += 1;
        let mut path = Vec::new();
        loop {
            let name_start = self.pos;
            if matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'_') {
                while matches!(self.peek(), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') {
                    self.pos += 1;
                }
                path.push((self.text(name_start).to_string(), (name_start - token_start) as u32));
                match self.peek() {
                    b'.' => {
                        self.pos += 1;
                        continue;
                    }
                    b'}' => {
                        self.pos += 1;
                        return Some(path);
                    }
                    _ => {}
                }
            }
            // Skip the rest of the hole, balancing nested braces, staying on
            // this line and inside the literal.
            let mut depth = 0usize;
            while self.pos < self.src.len() && !matches!(self.peek(), b'"' | b'\n') {
                match self.peek() {
                    b'{' => depth += 1,
                    b'}' if depth == 0 => break,
                    b'}' => depth -= 1,
                    _ => {}
                }
                self.pos += 1;
            }
            let closed = self.peek() == b'}';
            if closed {
                self.pos += 1;
            }
            let span = self.span(open);
            let mut d = Diagnostic::error("E0006", "invalid_interpolation", "expected a name or field path inside `{...}`")
                .primary(span, "")
                .note("an interpolated string accepts only `{name}` and `{name.field}`");
            d = if closed {
                d.help("compute other values into a binding first; write `{{` for a literal brace")
            } else {
                d.help("close the interpolation with `}`, or write `{{` for a literal brace")
            };
            self.error(d);
            return None;
        }
    }

    fn escape(&mut self, value: &mut String) {
        let start = self.pos;
        self.pos += 1;
        let c = self.peek();
        let simple = match c {
            b'n' => Some('\n'),
            b't' => Some('\t'),
            b'r' => Some('\r'),
            b'0' => Some('\0'),
            b'\\' => Some('\\'),
            b'"' => Some('"'),
            _ => None,
        };
        if let Some(ch) = simple {
            self.pos += 1;
            value.push(ch);
            return;
        }
        if c == b'u' && self.peek_at(1) == b'{' {
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek().is_ascii_hexdigit() {
                self.pos += 1;
            }
            let digits = &self.file.text[digits_start..self.pos];
            if self.peek() == b'}' {
                self.pos += 1;
                if let Some(ch) = u32::from_str_radix(digits, 16).ok().filter(|_| digits.len() <= 6).and_then(char::from_u32) {
                    value.push(ch);
                    return;
                }
            }
            let span = self.span(start);
            self.error(
                Diagnostic::error("E0003", "invalid_escape", "invalid unicode escape")
                    .primary(span, "")
                    .help("write `\\u{XXXX}` with 1 to 6 hex digits naming a valid code point"),
            );
            return;
        }
        if c != b'\n' && self.pos < self.src.len() {
            let ch = self.file.text[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
        }
        let span = self.span(start);
        let text = self.text(start).to_string();
        self.error(
            Diagnostic::error("E0003", "invalid_escape", format!("invalid escape sequence `{text}`"))
                .primary(span, "")
                .help("valid escapes are \\n \\t \\r \\0 \\\\ \\\" \\u{...}"),
        );
    }

    fn punct(&mut self) {
        use TokenKind::*;
        let start = self.pos;
        let (a, b, c) = (self.peek(), self.peek_at(1), self.peek_at(2));
        let (kind, len) = match (a, b, c) {
            (b'.', b'.', b'=') => (DotDotEq, 3),
            (b'.', b'.', _) => (DotDot, 2),
            (b':', b'=', _) => (ColonEq, 2),
            (b'-', b'>', _) => (Arrow, 2),
            (b'=', b'>', _) => (FatArrow, 2),
            (b'=', b'=', _) => (EqEq, 2),
            (b'!', b'=', _) => (BangEq, 2),
            (b'<', b'=', _) => (LtEq, 2),
            (b'>', b'=', _) => (GtEq, 2),
            (b'<', b'<', _) => (Shl, 2),
            (b'>', b'>', _) => (Shr, 2),
            (b'&', b'&', _) => (AmpAmp, 2),
            (b'|', b'|', _) => (PipePipe, 2),
            (b'(', _, _) => (LParen, 1),
            (b')', _, _) => (RParen, 1),
            (b'{', _, _) => (LBrace, 1),
            (b'}', _, _) => (RBrace, 1),
            (b'[', _, _) => (LBracket, 1),
            (b']', _, _) => (RBracket, 1),
            (b',', _, _) => (Comma, 1),
            (b':', _, _) => (Colon, 1),
            (b'.', _, _) => (Dot, 1),
            (b'=', _, _) => (Eq, 1),
            (b'+', _, _) => (Plus, 1),
            (b'-', _, _) => (Minus, 1),
            (b'*', _, _) => (Star, 1),
            (b'/', _, _) => (Slash, 1),
            (b'%', _, _) => (Percent, 1),
            (b'&', _, _) => (Amp, 1),
            (b'|', _, _) => (Pipe, 1),
            (b'^', _, _) => (Caret, 1),
            (b'!', _, _) => (Bang, 1),
            (b'<', _, _) => (Lt, 1),
            (b'>', _, _) => (Gt, 1),
            _ => {
                let ch = self.file.text[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                let span = self.span(start);
                let mut d = Diagnostic::error(
                    "E0001",
                    "unexpected_character",
                    format!("unexpected character `{}`", ch.escape_debug()),
                )
                .primary(span, "");
                if ch == ';' {
                    d = d.help("Tarn has no semicolons; end the statement with a line break");
                } else if ch == '\'' {
                    d = d.help("strings use double quotes: \"...\"");
                }
                self.error(d);
                return;
            }
        };
        self.pos += len;
        self.push(kind, start);
    }
}
