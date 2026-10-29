//! Pass 2: resolve signatures and bodies with lexical scopes.

use crate::collect::{Cx, is_upper, kind_name};
use crate::*;
use tarn_ast::*;

/// A function or closure body being resolved.
struct BodyCx {
    id: NodeId,
    span: Span,
    is_closure: bool,
}

/// An inner binding that hides a local of an enclosing scope. If the outer
/// one is used after `scope_end`, W2001 is reported once.
struct Shadow {
    outer: SymbolId,
    inner: SymbolId,
    scope_end: u32,
    reported: bool,
}

pub(crate) struct Walker<'c, 'a> {
    cx: &'c mut Cx<'a>,
    m: ModuleId,
    scope: ScopeId,
    bodies: Vec<BodyCx>,
    /// Per open block: names declared by `let` later in that block, for
    /// "used before declaration" (E2004).
    pending: Vec<(ScopeId, HashMap<String, Span>)>,
    shadows: Vec<Shadow>,
}

impl<'a> Cx<'a> {
    pub fn walk_all(&mut self) {
        for i in 0..self.inputs.len() {
            let m = ModuleId(i as u32);
            let scope = self.r.modules[i].scope;
            let mut w = Walker { cx: self, m, scope, bodies: Vec::new(), pending: Vec::new(), shadows: Vec::new() };
            w.module(self_inputs(w.cx, m));
            self.unused_imports(m);
        }
    }

    fn unused_imports(&mut self, m: ModuleId) {
        let scope = self.r.modules[m.0 as usize].scope;
        let syms = self.r.scope(scope).symbols.clone();
        for id in syms {
            let s = self.sym(id);
            if matches!(s.kind, SymbolKind::Module(ModuleTarget::Local(_) | ModuleTarget::Std(_))) && !self.used.contains(&id) {
                let d = Diagnostic {
                    severity: tarn_diagnostics::Severity::Warning,
                    code: "W2003",
                    kind: "unused_import",
                    message: format!("unused import `{}`", s.name),
                    labels: Vec::new(),
                    notes: Vec::new(),
                    help: Some("remove the import".to_string()),
                    obligations: Vec::new(),
                }
                .primary(s.span.unwrap(), "");
                self.diags.push(d);
            }
        }
    }
}

fn self_inputs<'a>(cx: &Cx<'a>, m: ModuleId) -> &'a Module {
    cx.inputs[m.0 as usize].ast
}

fn warning(code: &'static str, kind: &'static str, message: String) -> Diagnostic {
    let mut d = Diagnostic::error(code, kind, message);
    d.severity = tarn_diagnostics::Severity::Warning;
    d
}

impl<'c, 'a> Walker<'c, 'a> {
    // ------------------------------------------------------------ helpers

    fn tables(&mut self) -> &mut ModuleTables {
        &mut self.cx.r.tables[self.m.0 as usize]
    }

    fn use_(&mut self, node: NodeId, span: Span, res: Res) {
        if let Res::Symbol(id) = res {
            self.cx.mark_used(id);
        }
        self.tables().uses.insert(node, Use { res, span });
    }

    fn push_scope(&mut self, kind: ScopeKind, span: Span) -> ScopeId {
        let s = self.cx.r.add_scope(kind, Some(self.scope), Some(span));
        self.scope = s;
        s
    }

    fn in_scope(&mut self, kind: ScopeKind, span: Span, f: impl FnOnce(&mut Self)) {
        let saved = self.scope;
        self.push_scope(kind, span);
        f(self);
        self.scope = saved;
    }

    fn current_body(&self) -> Option<NodeId> {
        self.bodies.last().map(|b| b.id)
    }

    /// Declare a local-like symbol in the current scope, applying the
    /// shadowing rules (ADR 0009).
    fn declare_local(&mut self, name: &Ident, kind: SymbolKind, def: NodeId) -> SymbolId {
        let outer = self.cx.r.scope(self.scope).parent.and_then(|p| self.cx.r.lookup(p, &name.name));
        let same_scope = self.cx.r.scope(self.scope).get(&name.name).is_some();
        let sym =
            Symbol { name: name.name.clone(), kind, module: Some(self.m), def: Some(def), span: Some(name.span), scope: self.scope, is_pub: false, body: self.current_body() };
        let id = self.cx.declare(self.scope, sym, self.m, "E2003");
        // Remove from "declared later" once declared.
        if let Some((s, names)) = self.pending.last_mut()
            && *s == self.scope
        {
            names.remove(&name.name);
        }
        if same_scope || name.name == "_" {
            return id;
        }
        let Some(outer) = outer else { return id };
        let outer_sym = self.cx.sym(outer).clone();
        if outer_sym.kind.is_local() {
            if matches!(outer_sym.kind, SymbolKind::Local { mutable: true }) {
                self.cx.diags.push(
                    warning("W2001", "confusing_shadow", format!("`{}` shadows a mutable variable of an enclosing scope", name.name))
                        .primary(name.span, "new binding here")
                        .secondary(outer_sym.span.unwrap(), "`var` declared here")
                        .note("assignments to the inner binding do not change the outer one")
                        .help("use a different name"),
                );
            } else {
                let end = self.cx.r.scope(self.scope).span.map(|s| s.end).unwrap_or(u32::MAX);
                self.shadows.push(Shadow { outer, inner: id, scope_end: end, reported: false });
            }
        } else if outer_sym.module.is_some() || matches!(outer_sym.kind, SymbolKind::Builtin | SymbolKind::PreludeType) {
            let what = kind_name(&outer_sym.kind);
            let mut d = warning("W2002", "shadows_item", format!("`{}` shadows the {what} `{}`", name.name, name.name)).primary(name.span, "").help("use a different name");
            if let Some(s) = outer_sym.span {
                d = d.secondary(s, "declared here");
            }
            self.cx.diags.push(d);
        }
        id
    }

    /// Lexical lookup with use tracking: shadow warnings and captures.
    fn lookup(&mut self, name: &str, span: Span) -> Option<SymbolId> {
        let id = self.cx.r.lookup(self.scope, name)?;
        // W2001: outer binding used again after a shadowing scope ended.
        for sh in &mut self.shadows {
            if sh.outer == id && !sh.reported && span.start >= sh.scope_end {
                sh.reported = true;
                let inner = self.cx.r.symbol(sh.inner).span.unwrap();
                self.cx.diags.push(
                    warning("W2001", "confusing_shadow", format!("`{name}` is shadowed in an inner scope and used again after it"))
                        .primary(inner, "inner binding hides the outer one here")
                        .secondary(span, "this refers to the outer binding")
                        .note("changes made through the inner binding are not visible here")
                        .help("give the inner binding a different name"),
                );
            }
        }
        // Captures: every closure between the use and the symbol's body.
        let sym_body = self.cx.sym(id).body;
        if sym_body.is_some() {
            for b in self.bodies.iter().rev() {
                if Some(b.id) == sym_body {
                    break;
                }
                if b.is_closure {
                    let caps = self.cx.r.tables[self.m.0 as usize].captures.entry(b.id).or_insert_with(|| Captures { span: b.span, symbols: Vec::new() });
                    if !caps.symbols.contains(&id) {
                        caps.symbols.push(id);
                    }
                }
            }
        }
        Some(id)
    }

    fn undefined(&mut self, name: &str, span: Span) {
        // `_ = f()` from Go or Rust: Tarn discards a result by writing the
        // expression as a statement, so `_` is never a discard target.
        if name == "_" && !self.pending.iter().any(|(_, names)| names.contains_key(name)) {
            self.cx.diags.push(
                Diagnostic::error("E2001", "undefined_name", "`_` is not a discard target")
                    .primary(span, "not a declared name")
                    .note("an expression statement already discards its value: `f()` evaluates and drops the result")
                    .help("remove `_ =` and keep the expression as a statement"),
            );
            return;
        }
        // Declared later in an enclosing block?
        for (_, names) in self.pending.iter().rev() {
            if let Some(&decl) = names.get(name) {
                self.cx.diags.push(
                    Diagnostic::error("E2004", "used_before_declaration", format!("`{name}` is used before its declaration"))
                        .primary(span, "used here")
                        .secondary(decl, "declared here")
                        .help("move the declaration above its first use"),
                );
                return;
            }
        }
        if name == "self" {
            self.cx.diags.push(
                Diagnostic::error("E2015", "self_outside_method", "`self` is only available in methods with a receiver")
                    .primary(span, "")
                    .help("declare a receiver: `fn Type.name(&self, ...)`"),
            );
            return;
        }
        let mut d = Diagnostic::error("E2001", "undefined_name", format!("cannot find `{name}` in this scope")).primary(span, "not found");
        if let Some(e) = self.variant_owner(name) {
            d = d.help(format!("variants are qualified outside patterns: `{e}.{name}`"));
        } else if let Some(s) = self.similar(name) {
            d = d.help(format!("a name with a similar spelling exists: `{s}`"));
        }
        self.cx.diags.push(d);
    }

    /// An enum visible from this module that has a variant `name`.
    fn variant_owner(&self, name: &str) -> Option<String> {
        let ms = self.cx.r.modules[self.m.0 as usize].scope;
        self.cx.r.scope(ms).symbols.iter().find_map(|&id| {
            let s = self.cx.sym(id);
            (matches!(s.kind, SymbolKind::Enum) && self.cx.r.member(id, name).is_some()).then(|| s.name.clone())
        })
    }

    fn similar(&self, name: &str) -> Option<String> {
        let mut names = Vec::new();
        let mut s = Some(self.scope);
        while let Some(id) = s {
            let sc = self.cx.r.scope(id);
            names.extend(sc.symbols.iter().map(|&i| self.cx.sym(i).name.clone()));
            s = sc.parent;
        }
        suggest::best(name, names.iter().map(String::as_str)).map(str::to_string)
    }

    // ------------------------------------------------------------ paths

    /// Member `name` of what `base` resolved to, for `a.b` chains in types,
    /// expressions and patterns. `Ok(None)` = not a static path (value field).
    fn member(&mut self, base: &Res, name: &Ident) -> Result<Option<Res>, ()> {
        match base {
            Res::External { module, path } => {
                let mut path = path.clone();
                path.push(name.name.clone());
                Ok(Some(Res::External { module: module.clone(), path }))
            }
            Res::ScrutineeVariant(_) => Ok(None),
            Res::Symbol(id) => {
                let id = *id;
                let sym = self.cx.sym(id).clone();
                match &sym.kind {
                    SymbolKind::Module(ModuleTarget::Std(m)) => Ok(Some(Res::External { module: m.clone(), path: vec![name.name.clone()] })),
                    SymbolKind::Module(ModuleTarget::Missing) => Err(()),
                    SymbolKind::Module(ModuleTarget::Local(target)) => {
                        let scope = self.cx.r.modules[target.0 as usize].scope;
                        match self.cx.r.scope(scope).get(&name.name) {
                            Some(mid) if (self.cx.sym(mid).is_pub || crate::internal_visible(&self.cx.inputs[self.m.0 as usize], &self.cx.inputs[target.0 as usize]))
                                && !matches!(self.cx.sym(mid).kind, SymbolKind::Module(_)) => Ok(Some(Res::Symbol(mid))),
                            Some(mid) => {
                                let def = self.cx.sym(mid).span;
                                let mut d =
                                    Diagnostic::error("E2006", "private_item", format!("`{}` is private to module `{}`", name.name, sym.name)).primary(name.span, "not `pub`");
                                if let Some(def) = def {
                                    d = d.secondary(def, "declared here");
                                }
                                if matches!(self.cx.sym(mid).kind, SymbolKind::Module(_)) {
                                    d = d.note("imports are not re-exported");
                                } else {
                                    d = d.help(format!("mark it `pub` in `{}`", sym.name));
                                }
                                self.cx.diags.push(d);
                                Err(())
                            }
                            None => {
                                self.no_member(&format!("module `{}`", sym.name), name, scope_names(&self.cx.r, scope));
                                Err(())
                            }
                        }
                    }
                    SymbolKind::Struct | SymbolKind::Enum | SymbolKind::PreludeType => match self.assoc(id, name) {
                        Some(r) => Ok(Some(r)),
                        None => {
                            let names = self.cx.r.members.get(&id).map(|v| v.iter().map(|&m| self.cx.sym(m).name.clone()).collect()).unwrap_or_default();
                            self.no_member(&format!("{} `{}`", kind_name(&sym.kind), sym.name), name, names);
                            Err(())
                        }
                    },
                    // Interface members are reached through values, not paths.
                    _ => Ok(None),
                }
            }
        }
    }

    /// Variant, inherent method or impl method of a type.
    fn assoc(&mut self, ty: SymbolId, name: &Ident) -> Option<Res> {
        if let Some(m) = self.cx.r.member(ty, &name.name) {
            return Some(Res::Symbol(m));
        }
        let found: Vec<SymbolId> =
            self.cx.r.impls.iter().filter(|i| i.target == Some(ty)).flat_map(|i| i.methods.iter().copied()).filter(|&mid| self.cx.sym(mid).name == name.name).collect();
        match found.as_slice() {
            [] => None,
            [one] => Some(Res::Symbol(*one)),
            many => {
                let mut d = Diagnostic::error("E2010", "ambiguous_method", format!("`{}` is implemented by more than one interface for this type", name.name))
                    .primary(name.span, "ambiguous");
                for &m in many {
                    if let Some(s) = self.cx.sym(m).span {
                        d = d.secondary(s, "candidate");
                    }
                }
                self.cx.diags.push(d.help("call it through a value constrained to one interface"));
                Some(Res::Symbol(many[0]))
            }
        }
    }

    fn no_member(&mut self, owner: &str, name: &Ident, candidates: Vec<String>) {
        let mut d = Diagnostic::error("E2005", "no_such_member", format!("{owner} has no member `{}`", name.name)).primary(name.span, "not found");
        if let Some(s) = suggest::best(&name.name, candidates.iter().map(String::as_str)) {
            d = d.help(format!("did you mean `{s}`?"));
        }
        self.cx.diags.push(d);
    }

    /// Resolve the segments of a path (types, struct literals, patterns).
    fn path_res(&mut self, path: &Path) -> Option<Res> {
        let first = &path.segments[0];
        let Some(id) = self.lookup(&first.name, first.span) else {
            self.undefined(&first.name, first.span);
            return None;
        };
        self.cx.mark_used(id);
        let mut res = Res::Symbol(id);
        for seg in &path.segments[1..] {
            match self.member(&res, seg) {
                Ok(Some(r)) => res = r,
                Ok(None) => {
                    let what = match &res {
                        Res::Symbol(id) => format!("{} `{}`", kind_name(&self.cx.sym(*id).kind), self.cx.sym(*id).name),
                        _ => "this name".to_string(),
                    };
                    self.cx.diags.push(
                        Diagnostic::error("E2005", "no_such_member", format!("{what} has no member `{}`", seg.name))
                            .primary(seg.span, "")
                            .note("only modules, structs and enums have members that can be named in a path"),
                    );
                    return None;
                }
                Err(()) => return None,
            }
        }
        Some(res)
    }

    // ------------------------------------------------------------ items

    fn module(&mut self, ast: &Module) {
        for item in &ast.items {
            match &item.kind {
                ItemKind::Fn(f) => self.function(f),
                ItemKind::Struct(s) => self.in_scope(ScopeKind::Item, item.span, |w| {
                    w.generics(&s.generics);
                    for f in &s.fields {
                        w.ty(&f.ty);
                    }
                }),
                ItemKind::Enum(e) => self.in_scope(ScopeKind::Item, item.span, |w| {
                    w.generics(&e.generics);
                    for v in &e.variants {
                        for t in &v.fields {
                            w.ty(t);
                        }
                    }
                }),
                ItemKind::Interface(i) => self.in_scope(ScopeKind::Item, item.span, |w| {
                    // `Self`: the implementing type, an implicit type parameter
                    // of the interface's method signatures (Phase 26C).
                    let this = Symbol {
                        name: "Self".into(),
                        kind: SymbolKind::GenericParam,
                        module: Some(w.m),
                        def: None,
                        span: Some(i.name.span),
                        scope: w.scope,
                        is_pub: false,
                        body: None,
                    };
                    let self_param = w.cx.declare(w.scope, this, w.m, "E2003");
                    if let Some(iface) = w.cx.r.tables[w.m.0 as usize].defs.get(&item.id).copied() {
                        w.cx.r.interface_self.insert(iface, self_param);
                    }
                    w.generics(&i.generics);
                    for f in &i.methods {
                        w.function(f);
                    }
                }),
                ItemKind::Impl(i) => self.impl_block(item, i),
                ItemKind::Import(_) | ItemKind::Error => {}
            }
        }
    }

    fn generics(&mut self, gs: &[GenericParam]) {
        for g in gs {
            let sym = Symbol {
                name: g.name.name.clone(),
                kind: SymbolKind::GenericParam,
                module: Some(self.m),
                def: Some(g.id),
                span: Some(g.name.span),
                scope: self.scope,
                is_pub: false,
                body: None,
            };
            self.cx.declare(self.scope, sym, self.m, "E2003");
        }
        for g in gs {
            for b in &g.bounds {
                self.interface_path(b, "a bound");
            }
        }
    }

    /// Owner/impl binders. Owner bounds were rejected (E2021); impl binder
    /// bounds are interfaces the impl requires of its arguments (ADR 0078).
    fn binders(&mut self, ps: &[GenericParam]) {
        for g in ps {
            for b in &g.bounds {
                self.interface_path(b, "a bound");
            }
            // A binder named like an existing type is almost certainly an
            // attempted specialization (`Pair<i32, B>`), not a fresh name.
            if let Some(existing) = self.cx.r.lookup(self.scope, &g.name.name)
                && self.cx.sym(existing).kind.is_type()
                && !matches!(self.cx.sym(existing).kind, SymbolKind::GenericParam)
            {
                let k = kind_name(&self.cx.sym(existing).kind);
                self.cx.diags.push(
                    Diagnostic::error("E2022", "specialized_impl", format!("`{}` names an existing {k}; type parameter binders must be fresh names", g.name.name))
                        .primary(g.name.span, "")
                        .note("`Pair<A, B>` covers every `Pair`; specialized impls and methods are not supported in v0 (ADR 0015, 0016)"),
                );
                continue;
            }
            let sym = Symbol {
                name: g.name.name.clone(),
                kind: SymbolKind::GenericParam,
                module: Some(self.m),
                def: Some(g.id),
                span: Some(g.name.span),
                scope: self.scope,
                is_pub: false,
                body: None,
            };
            self.cx.declare(self.scope, sym, self.m, "E2003");
        }
    }

    /// A path that must name an interface (bounds, `impl I for`, `any I`).
    fn interface_path(&mut self, p: &Path, what: &str) -> Option<SymbolId> {
        for a in &p.args {
            self.ty(a);
        }
        let res = self.path_res(p)?;
        self.use_(p.id, p.span, res.clone());
        match res {
            Res::Symbol(id) if matches!(self.cx.sym(id).kind, SymbolKind::Interface) => Some(id),
            Res::Symbol(id) => {
                let k = kind_name(&self.cx.sym(id).kind);
                self.cx.diags.push(
                    Diagnostic::error("E2012", "not_an_interface", format!("expected an interface in {what}, found {k} `{}`", path_text(p))).primary(p.span, "not an interface"),
                );
                None
            }
            _ => None,
        }
    }

    fn function(&mut self, f: &FnDecl) {
        self.bodies.push(BodyCx { id: f.id, span: f.span, is_closure: false });
        self.in_scope(ScopeKind::Function, f.span, |w| {
            if let Some(o) = &f.owner {
                w.binders(&o.params);
            }
            w.generics(&f.generics);
            if let Some(r) = &f.receiver {
                let name = Ident { name: "self".to_string(), span: r.span };
                w.declare_local(&name, SymbolKind::SelfParam, r.id);
            }
            for p in &f.params {
                w.ty(&p.ty);
                w.declare_local(&p.name, SymbolKind::Param, p.id);
            }
            if let Some(r) = &f.ret {
                w.ty(r);
            }
            if let Some(b) = &f.body {
                w.stmts(&b.stmts);
            }
        });
        self.bodies.pop();
    }

    fn impl_block(&mut self, item: &Item, i: &ImplDecl) {
        let saved = self.scope;
        self.push_scope(ScopeKind::Item, item.span);
        let iface = self.interface_path(&i.interface, "`impl`");
        let target = self.impl_target(&i.target, &i.binder_bounds);
        // `Self` in an impl is its target type; the type checker substitutes
        // this implicit parameter with the target and its binders.
        let this = Symbol {
            name: "Self".into(),
            kind: SymbolKind::GenericParam,
            module: Some(self.m),
            def: None,
            span: Some(item.span),
            scope: self.scope,
            is_pub: false,
            body: None,
        };
        self.cx.declare(self.scope, this, self.m, "E2003");
        self.check_coherence(item, i, iface, target);
        self.impl_members(item, i, iface, target);
        self.scope = saved;
    }

    /// `impl I for T` / `impl I for T<A, B>`: `T` a struct or enum, its
    /// arguments fresh binders (ADR 0016).
    fn impl_target(&mut self, t: &Type, bounds: &[Vec<Path>]) -> Option<SymbolId> {
        let TypeKind::Path(p) = &t.kind else {
            self.cx.diags.push(
                Diagnostic::error("E2024", "invalid_impl_target", "interfaces can only be implemented for structs and enums")
                    .primary(t.span, "")
                    .note("v0 has no impls for references, slices, arrays or function types (ADR 0016)"),
            );
            return None;
        };
        let bare = Path { id: p.id, span: p.span, segments: p.segments.clone(), args: Vec::new() };
        let res = self.path_res(&bare)?;
        let Res::Symbol(id) = res else { return None };
        let kind = self.cx.sym(id).kind.clone();
        // ADR 0016: primitives get interface impls only inside `core`, which
        // declares them; coherence then holds because no other module can.
        let core_primitive = matches!(kind, SymbolKind::Primitive)
            && self.cx.r.modules[self.m.0 as usize].name == prelude::CORE
            && self.cx.inputs[self.m.0 as usize].trusted_stdlib;
        if !matches!(kind, SymbolKind::Struct | SymbolKind::Enum) && !core_primitive {
            let mut d = Diagnostic::error("E2024", "invalid_impl_target", format!("cannot implement an interface for {} `{}`", kind_name(&kind), path_text(p)))
                .primary(p.span, "")
                .note("only structs and enums can implement interfaces in v0 (ADR 0016)");
            if matches!(kind, SymbolKind::Primitive) {
                d = d.help("wrap the value in a struct you define: `struct Meters { value f64 }`");
            }
            self.cx.diags.push(d);
            return None;
        }
        self.use_(p.id, p.span, Res::Symbol(id));
        self.use_(t.id, t.span, Res::Symbol(id));
        let arity = self.cx.r.type_arity.get(&id).copied().unwrap_or(0);
        if arity != p.args.len() {
            let names = self.cx.type_param_names(id);
            self.cx.diags.push(
                Diagnostic::error("E2025", "type_arity", format!("`{}` takes {arity} type arguments, but {} were given", path_text(p), p.args.len()))
                    .primary(p.span, "")
                    .help(format!("write `{}<{}>`", path_text(p), names.join(", "))),
            );
        }
        let mut binders = Vec::new();
        for (index, a) in p.args.iter().enumerate() {
            match &a.kind {
                TypeKind::Path(ap) if ap.segments.len() == 1 && ap.args.is_empty() => {
                    let bounds = bounds.get(index).cloned().unwrap_or_default();
                    binders.push(GenericParam { id: a.id, name: ap.segments[0].clone(), bounds });
                }
                _ => self.cx.diags.push(
                    Diagnostic::error("E2022", "specialized_impl", "impl type arguments must be type parameter names")
                        .primary(a.span, "")
                        .note("`impl I for Pair<A, B>` implements `I` for every `Pair`; specialized impls are not supported in v0"),
                ),
            }
        }
        self.binders(&binders);
        Some(id)
    }

    /// ADR 0016: the impl lives in the module of the interface or of the type,
    /// and there is at most one per (interface, type).
    fn check_coherence(&mut self, item: &Item, i: &ImplDecl, iface: Option<SymbolId>, target: Option<SymbolId>) {
        if let Some(iface) = iface
            && self.cx.sym(iface).name == "Copy"
            && self.cx.sym(iface).module.is_some_and(|m| self.cx.r.modules[m.0 as usize].name == prelude::CORE)
        {
            self.cx.diags.push(
                Diagnostic::error("E2026", "impl_copy", "`Copy` is not implemented with `impl`")
                    .primary(i.interface.span, "")
                    .help("declare the type with the `copy` keyword: `copy struct T { ... }` (ADR 0021)"),
            );
            return;
        }
        let (Some(iface), Some(target)) = (iface, target) else { return };
        let (im, tm) = (self.cx.sym(iface).module, self.cx.sym(target).module);
        if im != Some(self.m) && tm != Some(self.m) {
            let iname = self.cx.sym(iface).name.clone();
            let tname = self.cx.sym(target).name.clone();
            let modname = |m: Option<ModuleId>| m.map(|m| self.cx.r.modules[m.0 as usize].name.clone()).unwrap_or_default();
            let (imn, tmn) = (modname(im), modname(tm));
            self.cx.diags.push(
                Diagnostic::error("E2023", "impl_coherence", format!("`impl {iname} for {tname}` must be in module `{imn}` or `{tmn}`"))
                    .primary(item.span.to(i.interface.span), "")
                    .note("an impl lives with its interface or its type, so a program can never contain two different impls for the same pair (ADR 0016)")
                    .help(format!("move this impl to `{imn}` or `{tmn}`, or wrap `{tname}` in a struct defined here")),
            );
        }
        if let Some(prev) = self.cx.r.impls.iter().find(|x| x.interface == Some(iface) && x.target == Some(target)) {
            let iname = self.cx.sym(iface).name.clone();
            let tname = self.cx.sym(target).name.clone();
            let _ = prev;
            self.cx.diags.push(
                Diagnostic::error("E2020", "duplicate_impl", format!("`{iname}` is implemented more than once for `{tname}`")).primary(i.interface.span, "second implementation"),
            );
        }
    }

    fn impl_members(&mut self, item: &Item, i: &ImplDecl, iface: Option<SymbolId>, target: Option<SymbolId>) {
        let scope = self.scope;
        let mut methods = Vec::new();
        let mut seen: HashMap<String, SymbolId> = HashMap::new();
        for f in &i.methods {
            let sym = Symbol {
                name: f.name.name.clone(),
                kind: SymbolKind::ImplMethod { interface: iface, target },
                module: Some(self.m),
                def: Some(f.id),
                span: Some(f.name.span),
                scope,
                is_pub: true,
                body: None,
            };
            let id = SymbolId(self.cx.r.symbols.len() as u32);
            self.cx.r.symbols.push(sym);
            self.tables().defs.insert(f.id, id);
            if let Some(&prev) = seen.get(&f.name.name) {
                self.cx.duplicate("E2002", &f.name.name, Some(f.name.span), prev);
            }
            seen.insert(f.name.name.clone(), id);
            methods.push(id);
            if let Some(iface) = iface
                && self.cx.r.member(iface, &f.name.name).is_none()
            {
                let iname = self.cx.sym(iface).name.clone();
                self.cx.diags.push(
                    Diagnostic::error("E2013", "not_an_interface_member", format!("`{}` is not a method of interface `{iname}`", f.name.name))
                        .primary(f.name.span, "")
                        .help("inherent methods are declared outside `impl`: `fn Type.name(...)`"),
                );
            }
        }
        if let Some(iface) = iface {
            let required: Vec<SymbolId> = self.cx.r.members.get(&iface).cloned().unwrap_or_default();
            let missing: Vec<String> = required.iter().map(|&m| self.cx.sym(m).name.clone()).filter(|n| !seen.contains_key(n)).collect();
            if !missing.is_empty() {
                let iname = self.cx.sym(iface).name.clone();
                let list = missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
                self.cx.diags.push(
                    Diagnostic::error("E2014", "missing_interface_method", format!("`impl {iname}` is missing {list}")).primary(i.interface.span, "incomplete implementation"),
                );
            }
        }
        self.cx.r.impls.push(ImplInfo { module: self.m, node: item.id, interface: iface, target, methods });
        for f in &i.methods {
            self.function(f);
        }
    }

    // ------------------------------------------------------------ types

    fn ty(&mut self, t: &Type) {
        match &t.kind {
            TypeKind::Path(p) => {
                for a in &p.args {
                    self.ty(a);
                }
                let Some(mut res) = self.path_res(p) else { return };
                // An imported module may share a primitive's spelling (`string`).
                // In a single-component type path, retain the primitive type;
                // expression/member paths continue to name the module.
                if p.segments.len() == 1 && matches!(&res,Res::Symbol(id) if matches!(self.cx.sym(*id).kind,SymbolKind::Module(_))) {
                    if let Some((index, _)) = self.cx.r.symbols.iter().enumerate().find(|(_, s)| s.name == p.segments[0].name && matches!(s.kind, SymbolKind::Primitive)) {
                        res = Res::Symbol(SymbolId(index as u32));
                    }
                }
                if let Res::Symbol(id) = res
                    && let Some(&arity) = self.cx.r.type_arity.get(&id)
                    && arity != p.args.len()
                {
                    let name = path_text(p);
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E2025",
                            "type_arity",
                            format!(
                                "`{name}` takes {arity} type argument{}, but {} {} given",
                                if arity == 1 { "" } else { "s" },
                                p.args.len(),
                                if p.args.len() == 1 { "was" } else { "were" }
                            ),
                        )
                        .primary(p.span, ""),
                    );
                }
                if let Res::Symbol(id) = res
                    && !self.cx.sym(id).kind.is_type()
                {
                    let k = kind_name(&self.cx.sym(id).kind);
                    self.cx.diags.push(Diagnostic::error("E2008", "not_a_type", format!("expected a type, found {k} `{}`", path_text(p))).primary(p.span, "not a type"));
                    return;
                }
                self.use_(p.id, p.span, res.clone());
                self.use_(t.id, t.span, res);
            }
            TypeKind::Ref { inner, .. } | TypeKind::Ptr { inner, .. } | TypeKind::Slice(inner) => self.ty(inner),
            TypeKind::Array { len, elem } => {
                self.expr(len);
                self.ty(elem);
            }
            TypeKind::Fn { params, ret, .. } => {
                for p in params {
                    self.ty(p);
                }
                if let Some(r) = ret {
                    self.ty(r);
                }
            }
            TypeKind::Any(p) => {
                if let Some(id) = self.interface_path(p, "`any`") {
                    self.use_(t.id, t.span, Res::Symbol(id));
                }
            }
            TypeKind::Error => {}
        }
    }

    // ------------------------------------------------------------ statements

    /// Statements of a block whose scope is already the current one.
    fn stmts(&mut self, stmts: &[Stmt]) {
        let mut later = HashMap::new();
        for s in stmts {
            if let StmtKind::Let { name, .. } = &s.kind {
                later.entry(name.name.clone()).or_insert(name.span);
            }
        }
        self.pending.push((self.scope, later));
        for s in stmts {
            self.stmt(s);
        }
        self.pending.pop();
    }

    fn block(&mut self, b: &Block) {
        self.in_scope(ScopeKind::Block, b.span, |w| w.stmts(&b.stmts));
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { mutable, name, ty, value } => {
                if let Some(t) = ty {
                    self.ty(t);
                }
                // The initializer cannot see the new name.
                if let Some(v) = value {
                    self.expr(v);
                }
                self.declare_local(name, SymbolKind::Local { mutable: *mutable }, s.id);
            }
            StmtKind::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::Expr(e) | StmtKind::Spawn(e) | StmtKind::Return(Some(e)) => {
                self.expr(e);
            }
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
            StmtKind::If(i) => self.if_stmt(i),
            StmtKind::For(f) => match &f.kind {
                ForKind::Infinite => self.block(&f.body),
                ForKind::While(c) => {
                    self.expr(c);
                    self.block(&f.body);
                }
                ForKind::In { binding, iter } => {
                    self.expr(iter);
                    self.in_scope(ScopeKind::Loop, f.body.span, |w| {
                        w.declare_local(binding, SymbolKind::LoopBinding, s.id);
                        w.stmts(&f.body.stmts);
                    });
                }
            },
            StmtKind::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    self.in_scope(ScopeKind::MatchArm, arm.span, |w| {
                        w.pattern(&arm.pattern);
                        if let Some(g) = &arm.guard {
                            w.expr(g);
                        }
                        match &arm.body.kind {
                            StmtKind::Block(b) => w.stmts(&b.stmts),
                            _ => w.stmt(&arm.body),
                        }
                    });
                }
            }
            StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => self.block(b),
        }
    }

    fn if_stmt(&mut self, i: &IfStmt) {
        self.expr(&i.cond);
        self.block(&i.then_block);
        match i.else_branch.as_deref() {
            Some(ElseBranch::If(e)) => self.if_stmt(e),
            Some(ElseBranch::Block(b)) => self.block(b),
            None => {}
        }
    }

    // ------------------------------------------------------------ expressions

    /// Resolve an expression; returns what it names when it is a static path
    /// (`x`, `geometry.Point`, `Shape.Circle`, `fs.read_text`).
    fn expr(&mut self, e: &Expr) -> Option<Res> {
        match &e.kind {
            ExprKind::Ident(name) => match self.lookup(name, e.span) {
                Some(id) => {
                    self.use_(e.id, e.span, Res::Symbol(id));
                    Some(Res::Symbol(id))
                }
                None => {
                    self.undefined(name, e.span);
                    None
                }
            },
            ExprKind::Field { base, name } => {
                let base_res = self.expr(base)?;
                match self.member(&base_res, name) {
                    Ok(Some(r)) => {
                        self.use_(e.id, e.span, r.clone());
                        Some(r)
                    }
                    Ok(None) | Err(()) => None,
                }
            }
            ExprKind::Paren(inner) => {
                self.expr(inner);
                None
            }
            ExprKind::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Value(v) = part {
                        self.expr(v);
                    }
                }
                None
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
                None
            }
            ExprKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
                None
            }
            ExprKind::Unary { operand, .. } | ExprKind::Try(operand) | ExprKind::Await(operand) => {
                self.expr(operand);
                None
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
                None
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.expr(s);
                }
                if let Some(x) = end {
                    self.expr(x);
                }
                None
            }
            ExprKind::StructLit { path, fields } => {
                if let Some(res) = self.path_res(path) {
                    match res {
                        Res::Symbol(id) if !matches!(self.cx.sym(id).kind, SymbolKind::Struct) => {
                            let k = kind_name(&self.cx.sym(id).kind);
                            let mut d =
                                Diagnostic::error("E2016", "not_a_struct", format!("expected a struct, found {k} `{}`", path_text(path))).primary(path.span, "not a struct");
                            if matches!(self.cx.sym(id).kind, SymbolKind::Variant { .. }) {
                                d = d.help("variants take positional values: `Shape.Rect(1.0, 2.0)`");
                            }
                            self.cx.diags.push(d);
                        }
                        r => {
                            self.use_(path.id, path.span, r.clone());
                            self.use_(e.id, e.span, r);
                        }
                    }
                }
                for f in fields {
                    match &f.value {
                        Some(v) => {
                            self.expr(v);
                        }
                        None => match self.lookup(&f.name.name, f.name.span) {
                            Some(id) => self.use_(f.id, f.name.span, Res::Symbol(id)),
                            None => self.undefined(&f.name.name, f.name.span),
                        },
                    }
                }
                None
            }
            ExprKind::ArrayLit { ty, elems } => {
                self.ty(ty);
                for x in elems {
                    self.expr(x);
                }
                None
            }
            ExprKind::Closure { params, ret, body, .. } => {
                self.bodies.push(BodyCx { id: e.id, span: e.span, is_closure: true });
                self.in_scope(ScopeKind::Closure, e.span, |w| {
                    for p in params {
                        if let Some(t) = &p.ty {
                            w.ty(t);
                        }
                        w.declare_local(&p.name, SymbolKind::ClosureParam, p.id);
                    }
                    if let Some(r) = ret {
                        w.ty(r);
                    }
                    w.stmts(&body.stmts);
                });
                self.bodies.pop();
                None
            }
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Unit | ExprKind::Error => None,
        }
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Wildcard | PatternKind::Error => {}
            PatternKind::Literal(e) => {
                self.expr(e);
            }
            PatternKind::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            PatternKind::Ident(name) => {
                let ident = Ident { name: name.clone(), span: p.span };
                if is_upper(name) {
                    self.variant_name(p.id, &ident);
                } else {
                    self.declare_local(&ident, SymbolKind::PatternBinding, p.id);
                }
            }
            PatternKind::Variant { path, args } => {
                if path.segments.len() == 1 {
                    let seg = &path.segments[0];
                    if is_upper(&seg.name) {
                        self.variant_name(p.id, seg);
                        if let Some(u) = self.tables().uses.get(&p.id).cloned() {
                            self.use_(path.id, path.span, u.res);
                        }
                    } else {
                        self.cx.diags.push(
                            Diagnostic::error("E2018", "expected_variant", format!("expected a variant, found `{}`", seg.name))
                                .primary(seg.span, "")
                                .note("variant names start with an uppercase letter"),
                        );
                    }
                } else if let Some(res) = self.path_res(path) {
                    match res {
                        Res::Symbol(id) if !matches!(self.cx.sym(id).kind, SymbolKind::Variant { .. }) => {
                            let k = kind_name(&self.cx.sym(id).kind);
                            self.cx
                                .diags
                                .push(Diagnostic::error("E2018", "expected_variant", format!("expected a variant, found {k} `{}`", path_text(path))).primary(path.span, ""));
                        }
                        r => {
                            self.use_(path.id, path.span, r.clone());
                            self.use_(p.id, p.span, r);
                        }
                    }
                }
                for a in args {
                    self.pattern(a);
                }
            }
            PatternKind::Struct { path, fields } => {
                if let Some(res) = self.path_res(path) {
                    if let Res::Symbol(id) = res
                        && !matches!(self.cx.sym(id).kind, SymbolKind::Struct)
                    {
                        let k = kind_name(&self.cx.sym(id).kind);
                        self.cx.diags.push(Diagnostic::error("E2016", "not_a_struct", format!("expected a struct, found {k} `{}`", path_text(path))).primary(path.span, ""));
                    } else {
                        self.use_(path.id, path.span, res.clone());
                        self.use_(p.id, p.span, res);
                    }
                }
                for f in fields {
                    match &f.pattern {
                        Some(sub) => self.pattern(sub),
                        None => {
                            self.declare_local(&f.name, SymbolKind::PatternBinding, f.id);
                        }
                    }
                }
            }
        }
    }

    /// Unqualified capitalized name in a pattern: a prelude/lexically visible
    /// variant, or a variant of the scrutinee's enum (deferred).
    fn variant_name(&mut self, node: NodeId, name: &Ident) {
        if let Some(id) = self.cx.r.lookup(self.scope, &name.name)
            && matches!(self.cx.sym(id).kind, SymbolKind::Variant { .. })
        {
            self.use_(node, name.span, Res::Symbol(id));
            return;
        }
        self.use_(node, name.span, Res::ScrutineeVariant(name.name.clone()));
    }
}

pub(crate) fn path_text(p: &Path) -> String {
    p.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(".")
}

fn scope_names(r: &Resolved, scope: ScopeId) -> Vec<String> {
    r.scope(scope).symbols.iter().map(|&i| r.symbol(i).name.clone()).collect()
}
