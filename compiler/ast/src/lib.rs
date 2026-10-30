//! Syntax tree for Tarn.
//!
//! The AST is purely syntactic: it records what was written, with a span on
//! every node, and nothing the parser cannot know (no resolved names, no
//! types). Later phases attach information in side tables keyed by
//! [`NodeId`]. `Error` variants mark places where the parser recovered from a
//! syntax error; later phases skip them silently.

mod dump;

pub use dump::{dump_module, type_to_string};
use tarn_diagnostics::{FileId, Span};

/// Unique (per module) id of a node, for side tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub file: FileId,
    pub items: Vec<Item>,
    pub span: Span,
}

// ---------------------------------------------------------------- items

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub kind: ItemKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Import(Import),
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Error,
}

/// `const NAME T = value` (ADR 0090): a named compile-time scalar or string.
#[derive(Clone, Debug, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// The module path as written, e.g. `fs` or `app/config`.
    pub path: String,
    pub path_span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub id: NodeId,
    pub span: Span,
    /// Source modifier only; execution and frame metadata live in later tables.
    pub is_async: bool,
    /// `unsafe fn`: every call requires an `unsafe` block (ADR 0047).
    pub is_unsafe: bool,
    /// `extern "C"` ABI string, if any.
    pub abi: Option<String>,
    /// `User` in `fn User.new()`, `Pair<A, B>` in `fn Pair<A, B>.first()`.
    pub owner: Option<Owner>,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub receiver: Option<Receiver>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// Explicit result provenance for bodyless declarations: `borrows(a, self)`.
    pub borrows: Option<Vec<Ident>>,
    /// `None` for interface methods and extern declarations.
    pub body: Option<Block>,
}

/// The type a method is declared on. `params` are fresh binders for the
/// type's generic parameters, in declaration order (ADR 0015).
#[derive(Clone, Debug, PartialEq)]
pub struct Owner {
    pub name: Ident,
    pub params: Vec<GenericParam>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverKind {
    /// `self` — consumes the receiver.
    Value,
    /// `&self`
    Ref,
    /// `&mut self`
    RefMut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Receiver {
    pub id: NodeId,
    pub kind: ReceiverKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    pub id: NodeId,
    pub name: Ident,
    /// Interface bounds: `T: Writer + Ordered`.
    pub bounds: Vec<Path>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDecl {
    pub is_copy: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub id: NodeId,
    pub span: Span,
    pub is_pub: bool,
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    pub is_copy: bool,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    /// Positional payload types; empty for unit variants.
    pub fields: Vec<Type>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<FnDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplDecl {
    pub interface: Path,
    pub target: Type,
    /// Bounds on each target type argument, in order (`Pair<A: Eq, B>` →
    /// `[[Eq], []]`); empty when the target has no arguments (ADR 0078).
    pub binder_bounds: Vec<Vec<Path>>,
    pub methods: Vec<FnDecl>,
}

// ---------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub id: NodeId,
    pub span: Span,
    pub kind: TypeKind,
}

/// The access required to invoke a callable, independent of value ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallMode {
    Shared,
    Mutable,
    Once,
}

impl CallMode {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Shared => "",
            Self::Mutable => "mut ",
            Self::Once => "once ",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `i32`, `User`, `fs.Error`, `Option<T>`.
    Path(Path),
    /// `&T` / `&mut T`
    Ref {
        mutable: bool,
        inner: Box<Type>,
    },
    /// `*T` / `*mut T`: raw pointer (ADR 0047). Carries no loan.
    Ptr {
        mutable: bool,
        inner: Box<Type>,
    },
    /// `[]T`
    Slice(Box<Type>),
    /// `[N]T`
    Array {
        len: Box<Expr>,
        elem: Box<Type>,
    },
    /// `fn(A, B) R`
    Fn {
        mode: CallMode,
        params: Vec<Type>,
        ret: Option<Box<Type>>,
    },
    /// `any Writer` — dynamic dispatch.
    Any(Path),
    Error,
}

/// A possibly qualified name with optional generic arguments on the last
/// segment: `a.b.C<T, U>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub id: NodeId,
    pub span: Span,
    pub segments: Vec<Ident>,
    pub args: Vec<Type>,
}

// ---------------------------------------------------------------- statements

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: NodeId,
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub id: NodeId,
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `x := e`, `x: T := e`, `var x = e`, `var x: T = e`, and the
    /// uninitialized `var x: T` (`value` is `None`; ADR 0024).
    Let {
        mutable: bool,
        name: Ident,
        ty: Option<Type>,
        value: Option<Expr>,
    },
    /// `place = e`
    Assign {
        target: Expr,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    Break,
    Continue,
    If(IfStmt),
    For(ForStmt),
    Match(MatchStmt),
    Block(Block),
    Unsafe(Block),
    /// `scope { ... }` — structured concurrency.
    Scope(Block),
    /// `spawn call(...)`
    Spawn(Expr),
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IfStmt {
    pub span: Span,
    pub cond: Expr,
    pub then_block: Block,
    pub else_branch: Option<Box<ElseBranch>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElseBranch {
    If(IfStmt),
    Block(Block),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForStmt {
    pub kind: ForKind,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ForKind {
    /// `for { }`
    Infinite,
    /// `for cond { }`
    While(Expr),
    /// `for x in iter { }`
    In { binding: Ident, iter: Expr },
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchStmt {
    pub scrutinee: Expr,
    pub arms: Vec<MatchArm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub id: NodeId,
    pub span: Span,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    /// A single statement; `{ ... }` arms are a `StmtKind::Block`.
    pub body: Box<Stmt>,
}

// ---------------------------------------------------------------- expressions

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub id: NodeId,
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    Text(String),
    Value(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(u64),
    /// Literal text without `_`.
    Float(String),
    Str(String),
    /// `f"a{x}b"` (ADR 0077); each value is an `Ident` or `Field` chain.
    Interpolated(Vec<InterpPart>),
    Bool(bool),
    /// `()`
    Unit,
    Ident(String),
    /// `(e)` — kept so tools can round-trip the source.
    Paren(Box<Expr>),
    /// `base.name` — field access, method selection or module member;
    /// name resolution decides which.
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `a..b`, `a..=b`, `..b`, `a..`
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    /// `try e`
    Try(Box<Expr>),
    /// Source suspension expression, retained until typed state lowering.
    Await(Box<Expr>),
    /// `Name{field: e, other}`
    StructLit {
        path: Path,
        fields: Vec<FieldInit>,
    },
    /// `[N]T{a, b}`
    ArrayLit {
        ty: Type,
        elems: Vec<Expr>,
    },
    /// `fn(x, y i32) R { ... }`
    Closure {
        owned: bool,
        params: Vec<ClosureParam>,
        ret: Option<Type>,
        body: Block,
    },
    /// `match e { P => value }` in expression position (ADR 0089). Each arm
    /// body is `StmtKind::Expr` (the arm's value) or a diverging block.
    Match(Box<MatchStmt>),
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Spawn,
    Neg,
    Not,
    Ref,
    RefMut,
    /// `*r`: the referent of `&T`/`&mut T` (ADR 0060).
    Deref,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitOr,
    BitXor,
    BitAnd,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Or => "||",
            And => "&&",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            BitOr => "|",
            BitXor => "^",
            BitAnd => "&",
            Shl => "<<",
            Shr => ">>",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
        }
    }
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Spawn => "spawn ",
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
            UnaryOp::Ref => "&",
            UnaryOp::RefMut => "&mut",
            UnaryOp::Deref => "*",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    /// Id of the shorthand reference in `User{name}` (resolved as a use of `name`).
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    /// `None` for shorthand `User{name}`.
    pub value: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClosureParam {
    pub id: NodeId,
    pub name: Ident,
    pub ty: Option<Type>,
}

// ---------------------------------------------------------------- patterns

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub id: NodeId,
    pub span: Span,
    pub kind: PatternKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// `_`
    Wildcard,
    /// `x` or `Empty`: a new binding or a unit variant; resolved later.
    Ident(String),
    /// `0`, `-1`, `"s"`, `true`
    Literal(Expr),
    /// `0..=9`, `'a'..'z'`
    Range {
        start: Expr,
        end: Expr,
        inclusive: bool,
    },
    /// `Circle(r)`, `Shape.Rect(w, h)`
    Variant {
        path: Path,
        args: Vec<Pattern>,
    },
    /// `User{name, age: 0}`
    Struct {
        path: Path,
        fields: Vec<FieldPattern>,
    },
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldPattern {
    /// Id of the binding introduced by shorthand `{name}`.
    pub id: NodeId,
    pub span: Span,
    pub name: Ident,
    /// `None` for shorthand `{name}` (binds `name`).
    pub pattern: Option<Pattern>,
}
