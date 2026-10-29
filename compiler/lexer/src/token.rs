use tarn_diagnostics::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// 1-based line of `span.start`.
    pub line: u32,
    /// 1-based column (in Unicode scalar values) of `span.start`.
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Ident(String),
    /// Value of a decimal, hex, octal or binary literal.
    Int(u64),
    /// Source text of the literal with `_` removed (parsed later, keeps `Eq`-free value out of the lexer).
    Float(String),
    /// Cooked string value (escapes resolved).
    Str(String),
    /// Interpolated literal `f"..."` (ADR 0077).
    FStr(Vec<FPiece>),

    // Keywords.
    Fn,
    Async,
    Await,
    Return,
    If,
    Else,
    For,
    In,
    Break,
    Continue,
    Match,
    Const,
    Struct,
    Enum,
    Interface,
    Impl,
    Copy,
    Var,
    Pub,
    Import,
    True,
    False,
    Try,
    Spawn,
    Unsafe,
    Extern,
    Mut,

    // Delimiters.
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // Punctuation and operators.
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    Eq,
    ColonEq,
    Arrow,
    FatArrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Bang,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Shl,
    Shr,

    /// Statement terminator inserted at a line break (see ADR 0003).
    Newline,
    Eof,
}

/// Every reserved word, in one table: the lexer and `tarn syntax` both read
/// it, so tools never describe a different language than the compiler parses.
pub const KEYWORDS: &[(&str, TokenKind)] = &[
    ("fn", TokenKind::Fn),
    ("async", TokenKind::Async),
    ("await", TokenKind::Await),
    ("return", TokenKind::Return),
    ("if", TokenKind::If),
    ("else", TokenKind::Else),
    ("for", TokenKind::For),
    ("in", TokenKind::In),
    ("break", TokenKind::Break),
    ("continue", TokenKind::Continue),
    ("match", TokenKind::Match),
    ("const", TokenKind::Const),
    ("struct", TokenKind::Struct),
    ("enum", TokenKind::Enum),
    ("interface", TokenKind::Interface),
    ("impl", TokenKind::Impl),
    ("copy", TokenKind::Copy),
    ("var", TokenKind::Var),
    ("pub", TokenKind::Pub),
    ("import", TokenKind::Import),
    ("true", TokenKind::True),
    ("false", TokenKind::False),
    ("try", TokenKind::Try),
    ("spawn", TokenKind::Spawn),
    ("unsafe", TokenKind::Unsafe),
    ("extern", TokenKind::Extern),
    ("mut", TokenKind::Mut),
];

pub fn keyword(ident: &str) -> Option<TokenKind> {
    KEYWORDS.iter().find(|(word, _)| *word == ident).map(|(_, kind)| kind.clone())
}

impl TokenKind {
    /// Short stable name, used by `tarn lex` and in tests.
    pub fn name(&self) -> &'static str {
        use TokenKind::*;
        match self {
            Ident(_) => "ident",
            Int(_) => "int",
            Float(_) => "float",
            Str(_) => "string",
            FStr(_) => "fstring",
            Fn => "fn",
            Async => "async",
            Await => "await",
            Return => "return",
            If => "if",
            Else => "else",
            For => "for",
            In => "in",
            Break => "break",
            Continue => "continue",
            Match => "match",
            Const => "const",
            Struct => "struct",
            Enum => "enum",
            Interface => "interface",
            Impl => "impl",
            Copy => "copy",
            Var => "var",
            Pub => "pub",
            Import => "import",
            True => "true",
            False => "false",
            Try => "try",
            Spawn => "spawn",
            Unsafe => "unsafe",
            Extern => "extern",
            Mut => "mut",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Comma => ",",
            Colon => ":",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            Eq => "=",
            ColonEq => ":=",
            Arrow => "->",
            FatArrow => "=>",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            Amp => "&",
            AmpAmp => "&&",
            Pipe => "|",
            PipePipe => "||",
            Caret => "^",
            Bang => "!",
            EqEq => "==",
            BangEq => "!=",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
            Shl => "<<",
            Shr => ">>",
            Newline => "newline",
            Eof => "eof",
        }
    }
}

/// One piece of an interpolated literal `f"..."` (ADR 0077).
#[derive(Clone, Debug, PartialEq)]
pub enum FPiece {
    /// Cooked text: escapes, `{{` and `}}` resolved.
    Text(String),
    /// `{a.b.c}`: the dotted names, each with its byte offset from the start
    /// of the token (kinds stay position-independent).
    Hole(Vec<(String, u32)>),
}
