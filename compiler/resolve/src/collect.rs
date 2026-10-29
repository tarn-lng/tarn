//! Pass 1: create module scopes and declare every module-level name, enum
//! variant, interface method and inherent method. Items are order-independent,
//! so all of this happens before any body or signature is resolved (pass 2,
//! `walk.rs`).

use crate::*;
use std::collections::HashSet;
use tarn_ast::{FnDecl, ItemKind};

pub(crate) struct Cx<'a> {
    pub import_targets: HashMap<(String, String), String>,
    pub inputs: &'a [ModuleInput<'a>],
    pub r: Resolved,
    pub diags: Vec<Diagnostic>,
    pub prelude: ScopeId,
    /// Symbols referenced at least once (for unused-import warnings).
    pub used: HashSet<SymbolId>,
}

pub(crate) fn is_upper(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

impl<'a> Cx<'a> {
    pub fn new(inputs: &'a [ModuleInput<'a>]) -> Cx<'a> {
        let mut r = Resolved::default();
        let prelude = prelude::build(&mut r);
        Cx { import_targets: HashMap::new(), inputs, r, diags: Vec::new(), prelude, used: HashSet::new() }
    }

    pub fn finish(self) -> (Resolved, Vec<Diagnostic>) {
        (self.r, self.diags)
    }

    pub fn sym(&self, id: SymbolId) -> &Symbol {
        self.r.symbol(id)
    }

    /// Declare and report same-scope duplicates (`code` E2002 for items and
    /// members, E2003 for locals).
    pub fn declare(&mut self, scope: ScopeId, sym: Symbol, def_module: ModuleId, code: &'static str) -> SymbolId {
        let name = sym.name.clone();
        let span = sym.span;
        let def = sym.def;
        let (id, prev) = self.r.declare(scope, sym);
        if let Some(def) = def {
            self.r.tables[def_module.0 as usize].defs.insert(def, id);
        }
        if let Some(prev) = prev {
            self.duplicate(code, &name, span, prev);
        }
        id
    }

    pub fn duplicate(&mut self, code: &'static str, name: &str, span: Option<Span>, prev: SymbolId) {
        let (kind, msg) = if code == "E2003" {
            ("duplicate_in_scope", format!("`{name}` is already declared in this scope"))
        } else {
            ("duplicate_definition", format!("`{name}` is defined more than once"))
        };
        let Some(span) = span else { return };
        let mut d = Diagnostic::error(code, kind, msg).primary(span, "redeclared here");
        if let Some(p) = self.sym(prev).span {
            d = d.secondary(p, "first declared here");
        }
        if code == "E2003" {
            d = d.help("use a different name, or assign with `=` if the binding is a `var`");
        }
        self.diags.push(d);
    }

    fn item_symbol(&self, name: &tarn_ast::Ident, kind: SymbolKind, m: ModuleId, def: NodeId, scope: ScopeId, is_pub: bool) -> Symbol {
        Symbol { name: name.name.clone(), kind, module: Some(m), def: Some(def), span: Some(name.span), scope, is_pub, body: None }
    }

    pub fn collect(&mut self) {
        // Module scopes first, so imports can see every local module.
        for (i, input) in self.inputs.iter().enumerate() {
            let scope = self.r.add_scope(ScopeKind::Module(ModuleId(i as u32)), Some(self.prelude), Some(input.ast.span));
            self.r.modules.push(ModuleInfo { name: input.name.clone(), scope });
            self.r.tables.push(ModuleTables::default());
        }
        for i in 0..self.inputs.len() {
            self.collect_module(ModuleId(i as u32));
        }
        self.export_core();
        // Methods need every type declared first.
        for i in 0..self.inputs.len() {
            self.collect_methods(ModuleId(i as u32));
        }
    }

    fn core_module(&self) -> Option<ModuleId> {
        self.inputs.iter().position(|m| m.name == prelude::CORE).map(|i| ModuleId(i as u32))
    }

    /// `pub` items of `core` (and the variants of `Option`/`Result`) become
    /// visible in every module through the prelude scope.
    fn export_core(&mut self) {
        let Some(core) = self.core_module() else { return };
        let scope = self.r.modules[core.0 as usize].scope;
        for id in self.r.scope(scope).symbols.clone() {
            let s = self.sym(id);
            if !s.is_pub || matches!(s.kind, SymbolKind::Module(_)) {
                continue;
            }
            let is_prelude_enum = matches!(s.kind, SymbolKind::Enum) && prelude::PRELUDE_ENUMS.contains(&s.name.as_str());
            self.r.alias(self.prelude, id);
            if is_prelude_enum {
                for v in self.r.members.get(&id).cloned().unwrap_or_default() {
                    self.r.alias(self.prelude, v);
                }
            }
        }
    }

    fn collect_module(&mut self, m: ModuleId) {
        let ast = self.inputs[m.0 as usize].ast;
        let scope = self.r.modules[m.0 as usize].scope;
        for item in &ast.items {
            match &item.kind {
                ItemKind::Import(imp) => {
                    let target_path = self.import_targets.get(&(self.inputs[m.0 as usize].name.clone(), imp.path.clone())).cloned().unwrap_or_else(|| imp.path.clone());
                    let target = self.import_target(&target_path, imp.path_span, m);
                    let name = imp.path.rsplit('/').next().unwrap_or(&imp.path).to_string();
                    let sym = Symbol {
                        name,
                        kind: SymbolKind::Module(target),
                        module: Some(m),
                        def: Some(item.id),
                        span: Some(imp.path_span),
                        scope,
                        is_pub: false,
                        body: None,
                    };
                    self.declare(scope, sym, m, "E2002");
                }
                ItemKind::Fn(f) if f.abi.as_deref() == Some("intrinsic") && !self.inputs[m.0 as usize].trusted_stdlib => {
                    self.diags.push(
                        Diagnostic::error("E2027", "intrinsic_outside_core", "`extern \"intrinsic\"` functions require trusted embedded stdlib declarations")
                            .primary(f.name.span, "")
                            .help("use `extern \"C\"` to call a C function"),
                    );
                }
                ItemKind::Fn(f) if f.owner.is_none() => {
                    let sym = self.item_symbol(&f.name, SymbolKind::Function, m, f.id, scope, item.is_pub);
                    self.declare(scope, sym, m, "E2002");
                }
                ItemKind::Const(c) => {
                    let sym = self.item_symbol(&c.name, SymbolKind::Const, m, item.id, scope, item.is_pub);
                    self.declare(scope, sym, m, "E2002");
                }
                ItemKind::Struct(s) => {
                    let sym = self.item_symbol(&s.name, SymbolKind::Struct, m, item.id, scope, item.is_pub);
                    let id = self.declare(scope, sym, m, "E2002");
                    self.r.type_arity.insert(id, s.generics.len());
                    let mut seen: HashMap<&str, Span> = HashMap::new();
                    for f in &s.fields {
                        if let Some(prev) = seen.insert(&f.name.name, f.name.span) {
                            self.diags.push(
                                Diagnostic::error("E2002", "duplicate_definition", format!("field `{}` is defined more than once", f.name.name))
                                    .primary(f.name.span, "redeclared here")
                                    .secondary(prev, "first declared here"),
                            );
                        }
                    }
                }
                ItemKind::Enum(e) => {
                    let sym = self.item_symbol(&e.name, SymbolKind::Enum, m, item.id, scope, item.is_pub);
                    let enum_id = self.declare(scope, sym, m, "E2002");
                    self.r.type_arity.insert(enum_id, e.generics.len());
                    for v in &e.variants {
                        if !is_upper(&v.name.name) {
                            self.diags.push(
                                Diagnostic::error("E2017", "variant_name_case", format!("variant `{}` must start with an uppercase letter", v.name.name))
                                    .primary(v.name.span, "")
                                    .note("in patterns, capitalized names are variants and lowercase names are new bindings")
                                    .help(format!("rename it to `{}`", capitalize(&v.name.name))),
                            );
                        }
                        let sym = self.item_symbol(&v.name, SymbolKind::Variant { parent: enum_id }, m, v.id, scope, item.is_pub);
                        self.add_member(enum_id, sym, m);
                    }
                }
                ItemKind::Interface(i) => {
                    let sym = self.item_symbol(&i.name, SymbolKind::Interface, m, item.id, scope, item.is_pub);
                    let iface = self.declare(scope, sym, m, "E2002");
                    self.r.type_arity.insert(iface, i.generics.len());
                    for f in &i.methods {
                        let sym = self.item_symbol(&f.name, SymbolKind::InterfaceMethod { interface: iface }, m, f.id, scope, item.is_pub);
                        self.add_member(iface, sym, m);
                    }
                }
                ItemKind::Fn(_) | ItemKind::Impl(_) | ItemKind::Error => {}
            }
        }
    }

    /// Members live in `Resolved::members`, not in any lexical scope.
    pub fn add_member(&mut self, owner: SymbolId, sym: Symbol, m: ModuleId) -> SymbolId {
        let name = sym.name.clone();
        let span = sym.span;
        let def = sym.def;
        let prev = self.r.member(owner, &name);
        let id = SymbolId(self.r.symbols.len() as u32);
        self.r.symbols.push(sym);
        self.r.members.entry(owner).or_default().push(id);
        if let Some(def) = def {
            self.r.tables[m.0 as usize].defs.insert(def, id);
        }
        if let Some(prev) = prev {
            self.duplicate("E2002", &name, span, prev);
        }
        id
    }

    fn import_target(&mut self, path: &str, span: Span, from: ModuleId) -> ModuleTarget {
        if let Some(i) = self.inputs.iter().position(|m| m.name == path) {
            return ModuleTarget::Local(ModuleId(i as u32));
        }
        if prelude::STD_MODULES.contains(&path) {
            return ModuleTarget::Std(path.to_string());
        }
        let mut d = Diagnostic::error("E2007", "module_not_found", format!("cannot find module `{path}`"))
            .primary(span, "no such module")
            .note(if path.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
                "import paths name files below the entry file's directory; `.`, `..` and empty segments are not allowed".to_string()
            } else {
                match &self.inputs[from.0 as usize].import_root {
                    // The entry file's directory is the root of local modules.
                    Some(root) if root != "." => format!("local modules are files relative to the entry file's directory: expected `{root}/{path}.tarn`"),
                    _ => format!("local modules are files relative to the entry file's directory: expected `{path}.tarn`"),
                }
            });
        if let Some(s) = suggest::best(path, prelude::STD_MODULES.iter().copied()) {
            d = d.help(format!("did you mean the standard module `{s}`?"));
        }
        self.diags.push(d);
        ModuleTarget::Missing
    }

    fn collect_methods(&mut self, m: ModuleId) {
        let ast = self.inputs[m.0 as usize].ast;
        let scope = self.r.modules[m.0 as usize].scope;
        for item in &ast.items {
            let ItemKind::Fn(f) = &item.kind else { continue };
            let Some(owner) = &f.owner else { continue };
            let Some(owner_id) = self.method_owner(&owner.name, scope, f) else { continue };
            self.mark_used(owner_id);
            self.r.tables[m.0 as usize].uses.insert(item.id, Use { res: Res::Symbol(owner_id), span: owner.name.span });
            self.check_owner_binders(owner, owner_id, f);
            let sym = self.item_symbol(&f.name, SymbolKind::Method { owner: owner_id }, m, f.id, scope, item.is_pub);
            self.add_member(owner_id, sym, m);
        }
    }

    /// `T` in `fn T.name`: a struct or enum declared in this same module.
    fn method_owner(&mut self, owner: &tarn_ast::Ident, scope: ScopeId, f: &FnDecl) -> Option<SymbolId> {
        // `core` declares the API of primitive types: `fn string.len(&self)`.
        if let Some(p) = self.r.lookup(scope, &owner.name)
            && matches!(self.sym(p).kind, SymbolKind::Primitive)
        {
            if self.sym_module_of_scope(scope) == self.core_module() {
                return Some(p);
            }
            self.diags.push(
                Diagnostic::error("E2011", "invalid_method_owner", format!("cannot declare method `{}` on primitive type `{}`", f.name.name, owner.name))
                    .primary(owner.span, "")
                    .note("the methods of primitive types are declared in `core`"),
            );
            return None;
        }
        let found = self.r.scope(scope).get(&owner.name);
        match found.map(|id| (id, self.sym(id).kind.clone())) {
            Some((id, SymbolKind::Struct | SymbolKind::Enum)) => Some(id),
            Some((_, kind)) => {
                self.diags.push(
                    Diagnostic::error(
                        "E2011",
                        "invalid_method_owner",
                        format!("cannot declare method `{}` on {} `{}`", f.name.name, kind_name(&kind), owner.name),
                    )
                    .primary(owner.span, "")
                    .note("methods are declared on structs and enums defined in the same module"),
                );
                None
            }
            None => {
                let mut d = Diagnostic::error("E2001", "undefined_name", format!("cannot find type `{}` in this module", owner.name))
                    .primary(owner.span, "not found");
                if self.r.lookup(scope, &owner.name).is_some() {
                    d = d.note("methods can only be declared on types defined in the same module");
                }
                self.diags.push(d);
                None
            }
        }
    }

    /// ADR 0015: `fn Pair<A, B>.m` — one plain binder per type parameter.
    fn check_owner_binders(&mut self, owner: &tarn_ast::Owner, owner_id: SymbolId, f: &FnDecl) {
        let arity = self.r.type_arity.get(&owner_id).copied().unwrap_or(0);
        let given = owner.params.len();
        if given != arity {
            let tname = &owner.name.name;
            let decl_params = self.type_param_names(owner_id);
            let fix = if arity == 0 {
                format!("fn {tname}.{}", f.name.name)
            } else {
                format!("fn {tname}<{}>.{}", decl_params.join(", "), f.name.name)
            };
            let span = owner.params.last().map(|p| owner.name.span.to(p.name.span)).unwrap_or(owner.name.span);
            self.diags.push(
                Diagnostic::error(
                    "E2019",
                    "owner_arity",
                    format!("`{tname}` has {arity} type parameter{}, but the method declares {given}", if arity == 1 { "" } else { "s" }),
                )
                .primary(span, "")
                .help(format!("write `{fix}`"))
                .note("the names after the type are binders for its parameters, in order (ADR 0015)"),
            );
        }
        for p in &owner.params {
            if let Some(b) = p.bounds.first() {
                self.diags.push(
                    Diagnostic::error("E2021", "owner_binder_bound", "bounds are not allowed on method owner parameters")
                        .primary(b.span, "")
                        .help("declare the bound on the type itself: `struct Map<K: Hash, V>`")
                        .note("conditional methods are not supported in v0 (ADR 0015)"),
                );
            }
        }
    }

    /// Generic parameter names of a struct/enum/interface, from its declaration.
    pub fn type_param_names(&self, id: SymbolId) -> Vec<String> {
        let s = self.sym(id);
        let (Some(m), Some(def)) = (s.module, s.def) else { return Vec::new() };
        for item in &self.inputs[m.0 as usize].ast.items {
            if item.id != def {
                continue;
            }
            let gs = match &item.kind {
                ItemKind::Struct(x) => &x.generics,
                ItemKind::Enum(x) => &x.generics,
                ItemKind::Interface(x) => &x.generics,
                _ => return Vec::new(),
            };
            return gs.iter().map(|g| g.name.name.clone()).collect();
        }
        Vec::new()
    }

    fn sym_module_of_scope(&self, scope: ScopeId) -> Option<ModuleId> {
        match self.r.scope(scope).kind {
            ScopeKind::Module(m) => Some(m),
            _ => None,
        }
    }

    pub fn mark_used(&mut self, id: SymbolId) {
        self.used.insert(id);
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

pub(crate) fn kind_name(k: &SymbolKind) -> &'static str {
    match k {
        SymbolKind::Primitive => "primitive type",
        SymbolKind::Builtin => "builtin function",
        SymbolKind::PreludeType => "type",
        SymbolKind::Module(_) => "module",
        SymbolKind::Function => "function",
        SymbolKind::Const => "constant",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Variant { .. } => "variant",
        SymbolKind::Method { .. } => "method",
        SymbolKind::InterfaceMethod { .. } => "interface method",
        SymbolKind::ImplMethod { .. } => "method",
        SymbolKind::GenericParam => "type parameter",
        SymbolKind::Param => "parameter",
        SymbolKind::SelfParam => "`self`",
        SymbolKind::Local { .. } => "local variable",
        SymbolKind::PatternBinding => "pattern binding",
        SymbolKind::LoopBinding => "loop variable",
        SymbolKind::ClosureParam => "closure parameter",
    }
}
