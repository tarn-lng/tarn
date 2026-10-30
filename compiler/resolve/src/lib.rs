//! Name resolution for Tarn.
//!
//! Input: the parsed modules of a program. Output: a [`Resolved`] value with
//! a symbol table, a scope tree and, per module, side tables keyed by
//! [`NodeId`] — the AST itself is never modified and holds no semantic data.
//!
//! ```text
//! AST:        NodeId 42 -> Expr::Ident("foo")
//! uses:       NodeId 42 -> Res::Symbol(SymbolId 17)
//! symbols:    SymbolId 17 -> { name: foo, kind: Local, def: NodeId 11, scope: ScopeId 4 }
//! ```
//!
//! Rules: `docs/resolution.md`.

mod collect;
mod dump;
mod prelude;
pub mod suggest;
mod walk;

use std::collections::HashMap;
use tarn_ast::{Module, NodeId};
use tarn_diagnostics::{Diagnostic, Span};

pub use dump::dump_resolution;
pub use prelude::STD_MODULES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u32);

/// One parsed module handed to the resolver.
pub struct ModuleInput<'a> {
    /// Module path as used in imports: `main`, `geometry`, `app/config`.
    pub name: String,
    pub ast: &'a Module,
    /// Bundled source provenance, never inferred from a user module filename.
    pub trusted_stdlib: bool,
    /// Directory its local imports resolve against, for diagnostics only.
    pub import_root: Option<String>,
}

/// Trusted stdlib layers and the layers each one builds on (ADR 0041), lowest
/// first. A trusted module may use private items and fields only of the layers
/// it builds on, never of layers above or beside it; user modules never see
/// private stdlib items.
pub const STDLIB_LAYERS: &[&str] = &["io", "time", "net", "runtime", "fs", "process", "ffi"];
const STDLIB_BELOW: &[(&str, &[&str])] = &[("io", &[]), ("ffi", &[]), ("fs", &["io"]), ("process", &["io", "net", "fs"]), ("time", &["io"]), ("net", &["io"]), ("runtime", &["io", "time", "net"])];

/// Whether `from` may use a private item or field declared in `to`.
pub fn internal_visible(from: &ModuleInput, to: &ModuleInput) -> bool {
    from.trusted_stdlib && to.trusted_stdlib
        && STDLIB_BELOW.iter().any(|(layer, below)| *layer == from.name && below.contains(&to.name.as_str()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleTarget {
    Local(ModuleId),
    /// A standard-library module. Its members are not checked until the
    /// stdlib exists (phase 16); uses resolve to [`Res::External`].
    Std(String),
    /// Import that could not be found (already reported).
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    // Prelude.
    Primitive,
    /// `print`, `panic`, `channel`.
    Builtin,
    /// `Option`, `Result`, `Error`, `Channel`, `Sender`.
    PreludeType,

    // Module level.
    Module(ModuleTarget),
    Function,
    /// `const NAME T = value` (ADR 0090).
    Const,
    Struct,
    Enum,
    Interface,
    /// Member of an enum.
    Variant { parent: SymbolId },
    /// `fn T.name(...)` — member of a struct or enum.
    Method { owner: SymbolId },
    /// Declaration inside `interface I { }`.
    InterfaceMethod { interface: SymbolId },
    /// Method inside `impl I for T { }`.
    ImplMethod { interface: Option<SymbolId>, target: Option<SymbolId> },
    GenericParam,

    // Local to a function or closure body.
    Param,
    SelfParam,
    Local { mutable: bool },
    PatternBinding,
    LoopBinding,
    ClosureParam,
}

impl SymbolKind {
    /// Can this symbol name a type?
    pub fn is_type(&self) -> bool {
        matches!(
            self,
            SymbolKind::Primitive
                | SymbolKind::PreludeType
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::GenericParam
        )
    }

    /// Declared inside a function or closure (subject to shadowing rules).
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            SymbolKind::Param
                | SymbolKind::SelfParam
                | SymbolKind::Local { .. }
                | SymbolKind::PatternBinding
                | SymbolKind::LoopBinding
                | SymbolKind::ClosureParam
        )
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Module of the definition; `None` for the prelude.
    pub module: Option<ModuleId>,
    /// Defining node (item, statement, parameter, pattern...); `None` for the prelude.
    pub def: Option<NodeId>,
    /// Span of the defining name.
    pub span: Option<Span>,
    pub scope: ScopeId,
    pub is_pub: bool,
    /// For locals: the function or closure body that owns them (`FnDecl.id`
    /// or closure `Expr.id`). Used to compute closure captures.
    pub body: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Prelude,
    Module(ModuleId),
    /// Generic parameters of a struct, enum, interface or impl.
    Item,
    /// Generics, receiver, parameters and the top level of the body.
    Function,
    /// Closure parameters and the top level of its body.
    Closure,
    /// `for x in` binding and the top level of the loop body.
    Loop,
    /// Pattern bindings, guard and the top level of the arm body.
    MatchArm,
    /// Any other `{ }`.
    Block,
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    pub span: Option<Span>,
    /// Symbols in declaration order.
    pub symbols: Vec<SymbolId>,
    names: HashMap<String, SymbolId>,
}

impl Scope {
    pub fn get(&self, name: &str) -> Option<SymbolId> {
        self.names.get(name).copied()
    }
}

/// What a name-bearing node refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Res {
    Symbol(SymbolId),
    /// Member of a standard-library module, unchecked until the stdlib exists:
    /// `fs.read_text` → `External { module: "fs", path: ["read_text"] }`.
    External { module: String, path: Vec<String> },
    /// Unqualified variant in a pattern (`Circle(r)`): resolved by the type
    /// checker against the scrutinee's enum.
    ScrutineeVariant(String),
}

/// A resolved reference and where it occurs (for tools: refs, hover, rename).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
    pub res: Res,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    /// Span of the closure expression.
    pub span: Span,
    pub symbols: Vec<SymbolId>,
}

#[derive(Clone, Debug, Default)]
pub struct ModuleTables {
    /// Nodes that introduce a symbol (items, params, `let`, patterns, `for`...).
    pub defs: HashMap<NodeId, SymbolId>,
    /// Nodes that refer to a name: `Ident`/`Field` exprs, type paths, struct
    /// literal paths, patterns, shorthand fields.
    pub uses: HashMap<NodeId, Use>,
    /// Closure expression → outer locals it captures (directly or through a
    /// nested closure), in first-use order.
    pub captures: HashMap<NodeId, Captures>,
}

#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub name: String,
    pub scope: ScopeId,
}

#[derive(Clone, Debug)]
pub struct ImplInfo {
    pub module: ModuleId,
    pub node: NodeId,
    pub interface: Option<SymbolId>,
    pub target: Option<SymbolId>,
    pub methods: Vec<SymbolId>,
}

#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    pub modules: Vec<ModuleInfo>,
    /// Per-module side tables, indexed by `ModuleId`.
    pub tables: Vec<ModuleTables>,
    /// Enum → variants and inherent methods; struct → inherent methods;
    /// interface → declared methods.
    pub members: HashMap<SymbolId, Vec<SymbolId>>,
    pub impls: Vec<ImplInfo>,
    /// Number of generic parameters of every struct, enum, interface and
    /// prelude type (`Option` → 1, `Result` → 2).
    pub type_arity: HashMap<SymbolId, usize>,
    /// Interface → its implicit `Self` type parameter (Phase 26C).
    pub interface_self: HashMap<SymbolId, SymbolId>,
}

impl Resolved {
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    pub fn member(&self, owner: SymbolId, name: &str) -> Option<SymbolId> {
        self.members.get(&owner)?.iter().copied().find(|&m| self.symbol(m).name == name)
    }

    fn add_scope(&mut self, kind: ScopeKind, parent: Option<ScopeId>, span: Option<Span>) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope { kind, parent, span, symbols: Vec::new(), names: HashMap::new() });
        id
    }

    /// Add a symbol to `scope`. Returns the previous symbol with that name in
    /// the same scope, if any (the caller reports the duplicate).
    fn declare(&mut self, scope: ScopeId, sym: Symbol) -> (SymbolId, Option<SymbolId>) {
        let id = SymbolId(self.symbols.len() as u32);
        let name = sym.name.clone();
        self.symbols.push(sym);
        let s = &mut self.scopes[scope.0 as usize];
        let prev = s.names.get(&name).copied();
        if prev.is_none() {
            s.names.insert(name, id);
        }
        s.symbols.push(id);
        (id, prev)
    }

    /// Make an existing symbol visible under its name in another scope
    /// (used to export `core` into the prelude). Existing names win.
    fn alias(&mut self, scope: ScopeId, id: SymbolId) {
        let name = self.symbol(id).name.clone();
        self.scopes[scope.0 as usize].names.entry(name).or_insert(id);
    }

    /// Lexical lookup from `scope` outwards.
    pub fn lookup(&self, mut scope: ScopeId, name: &str) -> Option<SymbolId> {
        loop {
            let s = self.scope(scope);
            if let Some(id) = s.get(name) {
                return Some(id);
            }
            scope = s.parent?;
        }
    }
}

/// Resolve a whole program. Diagnostics are returned in emission order; the
/// driver sorts them by position.
pub fn resolve(modules: &[ModuleInput]) -> (Resolved, Vec<Diagnostic>) {
    resolve_with_imports(modules, &HashMap::new())
}

/// Driver-selected import identities; source paths and aliases remain neutral AST data.
pub fn resolve_with_imports(modules: &[ModuleInput], targets: &HashMap<(String, String), String>) -> (Resolved, Vec<Diagnostic>) {
    let mut cx = collect::Cx::new(modules);
    cx.import_targets = targets.clone();
    cx.collect();
    cx.walk_all();
    cx.finish()
}

#[cfg(test)]
mod tests {
    /// Regression: `self` once reused the function's `NodeId` and overwrote
    /// the function's entry in `defs`.
    #[test]
    fn receiver_has_its_own_def() {
        use tarn_diagnostics::SourceMap;
        let mut map = SourceMap::new();
        let id = map.add("t.tarn", "struct S {\n}\n\nfn S.get(&self) {\n}\n");
        let parsed = tarn_parser::parse_file(id, map.file(id)).module;
        let inputs = [crate::ModuleInput { name: "t".into(), ast: &parsed, trusted_stdlib: false, import_root: None }];
        let (r, d) = crate::resolve(&inputs);
        assert!(d.is_empty());
        let tarn_ast::ItemKind::Fn(f) = &parsed.items[1].kind else { panic!() };
        let fsym = r.tables[0].defs[&f.id];
        assert!(matches!(r.symbol(fsym).kind, crate::SymbolKind::Method { .. }));
        let rsym = r.tables[0].defs[&f.receiver.as_ref().unwrap().id];
        assert!(matches!(r.symbol(rsym).kind, crate::SymbolKind::SelfParam));
    }

    #[test]
    fn primitive_impl_requires_core_source_provenance() {
        let mut map = tarn_diagnostics::SourceMap::new();
        let id = map.add("core.tarn", "interface Extra { fn hash(&self) u64 }\nimpl Extra for i32 { fn hash(&self) u64 { return 0 } }\n");
        let ast = tarn_parser::parse_file(id, map.file(id)).module;
        for trusted in [false, true] {
            let inputs = [crate::ModuleInput { name: "core".into(), ast: &ast, trusted_stdlib: trusted, import_root: None }];
            let (_, diagnostics) = crate::resolve(&inputs);
            assert_eq!(diagnostics.iter().any(|d| d.code == "E2024"), !trusted, "{diagnostics:?}");
        }
    }

    #[test]
    fn private_stdlib_access_flows_only_down_the_layers() {
        use tarn_diagnostics::SourceMap;
        let mut map = SourceMap::new();
        let id = map.add("t.tarn", "");
        let ast = tarn_parser::parse_file(id, map.file(id)).module;
        let module = |name: &str, trusted_stdlib| crate::ModuleInput { name: name.into(), ast: &ast, trusted_stdlib, import_root: None };
        let (io, time, net, runtime) = (module("io", true), module("time", true), module("net", true), module("runtime", true));
        assert!(crate::internal_visible(&time, &io) && crate::internal_visible(&net, &io) && crate::internal_visible(&runtime, &net));
        let fs = module("fs", true);
        assert!(crate::internal_visible(&fs, &io));
        assert!(!crate::internal_visible(&fs, &net) && !crate::internal_visible(&net, &fs));
        assert!(!crate::internal_visible(&runtime, &fs) && !crate::internal_visible(&fs, &runtime));
        assert!(!crate::internal_visible(&module("fs", false), &io));
        let process = module("process", true);
        // ADR 0070/0071: process hands Unix streams and files to children.
        assert!(crate::internal_visible(&process, &io) && crate::internal_visible(&process, &net) && crate::internal_visible(&process, &fs));
        assert!(!crate::internal_visible(&net, &process) && !crate::internal_visible(&fs, &process));
        for sibling in [&time, &runtime] {
            assert!(!crate::internal_visible(&process, sibling) && !crate::internal_visible(sibling, &process));
        }
        assert!(!crate::internal_visible(&module("process", false), &io));
        // Never upward, sideways, to itself, from user code or into a user module named like a layer.
        assert!(!crate::internal_visible(&io, &net) && !crate::internal_visible(&net, &runtime));
        assert!(!crate::internal_visible(&net, &time) && !crate::internal_visible(&time, &net));
        assert!(!crate::internal_visible(&io, &io));
        assert!(!crate::internal_visible(&module("main", false), &io));
        assert!(!crate::internal_visible(&runtime, &module("io", false)));
        assert!(!crate::internal_visible(&module("runtime", false), &io));
    }
}
