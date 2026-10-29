//! Checking function bodies: expressions, statements, patterns, mutability,
//! exhaustiveness and returns. One `FnCx` per top-level function (closures are
//! checked inside their enclosing function, sharing its inference table).

use crate::env::{Env, FnSig};
use crate::ty::*;
use crate::{BindingMode, Coercion, CoercionKind, MethodTarget, Receiver, TypeTables};
use std::collections::HashMap;
use tarn_ast::*;
use tarn_diagnostics::{Diagnostic, Span};
use tarn_resolve::{ModuleId, Res, SymbolId, SymbolKind};

pub struct FnCx<'e, 'a> {
    pub env: &'e Env<'a>,
    pub m: ModuleId,
    pub infer: Infer,
    pub diags: Vec<Diagnostic>,
    pub locals: HashMap<SymbolId, Ty>,
    pub tables: TypeTables,
    rets: Vec<Ty>,
    async_context: bool,
    unsafe_depth: u32,
    task_scope_depth: u32,
    /// String literal expressions, which may be borrowed where `&string` is
    /// expected (Phase 26B).
    string_literals: std::collections::HashSet<NodeId>,
    /// Integer literals to range-check once types are known: (span, value, type).
    literals: Vec<(Span, i128, Ty)>,
    /// Generic arguments that must implement an interface: (type, interface, span).
    obligations: Vec<(Ty, SymbolId, Span, Option<Vec<Ty>>)>,
    /// Arguments of `print` to check once types are known.
    printables: Vec<(Ty, Span)>,
    task_capabilities: Vec<(Ty, crate::Capability, Span, String)>,
    scoped_results: Vec<(Ty, Span)>,
    callable_transfer: HashMap<SymbolId, Vec<Ty>>,
    callable_share: HashMap<SymbolId, Vec<Ty>>,
    /// Bindings whose type must be fully inferred: (symbol, name span).
    inferred_lets: Vec<(SymbolId, Span)>,
    /// Operand of the `&`/`&mut` being checked (slices are legal there).
    borrowed: Option<NodeId>,
    /// Assignment target being checked: `*r = v` writes, it does not read.
    assigned: Option<NodeId>,
}

/// One step of an implementation proof: `ty` must implement `interface`,
/// required by the impl bound at `bound` (ADR 0078).
#[derive(Clone)]
struct Obligation {
    ty: Ty,
    interface: SymbolId,
    bound: Option<Span>,
}

fn peel(t: &Ty) -> (Ty, Option<bool>) {
    let mut t = t.clone();
    let mut through = None;
    while let Ty::Ref(m, inner) = t {
        through = Some(through.map_or(m, |x: bool| x && m));
        t = *inner;
    }
    (t, through)
}

pub fn subst(t: &Ty, map: &HashMap<ParamId, Ty>) -> Ty {
    match t {
        Ty::Param(p) => map.get(p).cloned().unwrap_or(Ty::Param(*p)),
        Ty::Adt(s, a) => Ty::Adt(*s, a.iter().map(|x| subst(x, map)).collect()),
        Ty::Ref(m, x) => Ty::Ref(*m, Box::new(subst(x, map))),
        Ty::Ptr(m, x) => Ty::Ptr(*m, Box::new(subst(x, map))),
        Ty::Array(x, n) => Ty::Array(Box::new(subst(x, map)), *n),
        Ty::Slice(x) => Ty::Slice(Box::new(subst(x, map))),
        Ty::Fn(mode, ps, r) => Ty::Fn(*mode, ps.iter().map(|x| subst(x, map)).collect(), Box::new(subst(r, map))),
        Ty::Async(r) => Ty::Async(Box::new(subst(r, map))),
        t => t.clone(),
    }
}

impl<'e, 'a> FnCx<'e, 'a> {
    fn internal_visible(&self, owner: ModuleId) -> bool {
        tarn_resolve::internal_visible(&self.env.inputs[self.m.0 as usize], &self.env.inputs[owner.0 as usize])
    }

    pub fn new(env: &'e Env<'a>, m: ModuleId) -> Self {
        FnCx {
            env,
            m,
            infer: Infer::default(),
            diags: Vec::new(),
            locals: HashMap::new(),
            tables: TypeTables::default(),
            rets: Vec::new(),
            async_context: false,
            unsafe_depth: 0,
            task_scope_depth: 0,
            string_literals: std::collections::HashSet::new(),
            literals: Vec::new(),
            obligations: Vec::new(),
            printables: Vec::new(),
            task_capabilities: Vec::new(),
            scoped_results: Vec::new(),
            callable_transfer: HashMap::new(),
            callable_share: HashMap::new(),
            inferred_lets: Vec::new(),
            borrowed: None,
            assigned: None,
        }
    }

    // ------------------------------------------------------------ helpers

    fn show(&self, t: &Ty) -> String {
        crate::display_vars(&self.infer.zonk(t), self.env, &self.infer)
    }

    fn res(&self, node: NodeId) -> Option<Res> {
        self.env.r.tables[self.m.0 as usize].uses.get(&node).map(|u| u.res.clone())
    }

    fn def(&self, node: NodeId) -> Option<SymbolId> {
        self.env.r.tables[self.m.0 as usize].defs.get(&node).copied()
    }

    fn kind(&self, s: SymbolId) -> &SymbolKind {
        &self.env.r.symbol(s).kind
    }

    fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    fn mismatch(&mut self, span: Span, expected: &Ty, found: &Ty) {
        let (e, f) = (self.show(expected), self.show(found));
        let mut d = Diagnostic::error("E3001", "type_mismatch", format!("expected `{e}`, found `{f}`")).primary(span, format!("this is `{f}`"));
        let (ze, zf) = (self.infer.zonk(expected), self.infer.zonk(found));
        if let (Ty::Int(_) | Ty::Float(_), Ty::Int(_) | Ty::Float(_)) = (&ze, &zf) {
            d = d.help(format!("numeric types never convert implicitly; write `{e}(value)`"));
        } else if let Ty::Ref(_, inner) = &ze
            && self.infer.unify(&inner.clone(), &zf)
        {
            d = d.help("borrow it with `&`");
        }
        self.err(d);
    }

    /// `actual` is used where `expected` is required. Applies the implicit
    /// coercions: `&mut T → &T`, `*mut T → *T`, `&[N]T → &[]T`, `&T → &any I`.
    /// Records the coercion applied to `node` (for IR lowering).
    fn coerce(&mut self, actual: &Ty, expected: &Ty, span: Span, node: Option<NodeId>) -> bool {
        let (a, x) = (self.infer.shallow(actual), self.infer.shallow(expected));
        // `f("GET")` for `f(method &string)`: the literal is borrowed exactly
        // as if written `&"GET"` (a temporary), so no new semantics.
        if a == Ty::Str
            && matches!(&x, Ty::Ref(false, inner) if **inner == Ty::Str)
            && let Some(n) = node
            && self.string_literals.contains(&n)
        {
            self.tables.coercions.insert(n, Coercion { mut_to_shared: false, kind: CoercionKind::BorrowLiteral });
            return true;
        }
        if let (Ty::Ref(am, ai), Ty::Ref(xm, xi)) = (&a, &x)
            && (*am || !*xm)
        {
            let mut_to_shared = *am && !*xm;
            let (ai, xi) = (self.infer.shallow(ai), self.infer.shallow(xi));
            let applied = match (&ai, &xi) {
                (Ty::Array(ae, _), Ty::Slice(xe)) => self.infer.unify(ae, xe).then_some(CoercionKind::Unsize),
                (Ty::Any(i), Ty::Any(j)) if i == j => Some(CoercionKind::None),
                (_, Ty::Any(iface)) => {
                    let iface = *iface;
                    // A bounded impl that does not apply reports its cause and
                    // stands in for the coercion, avoiding a second mismatch.
                    (self.implements(&ai, iface) || self.report_unmet(span, "coercion to `any`", &ai, iface)).then_some(CoercionKind::ToDyn(iface))
                }
                _ => self.infer.unify(&ai, &xi).then_some(CoercionKind::None),
            };
            if let Some(kind) = applied {
                if let Some(n) = node
                    && (mut_to_shared || kind != CoercionKind::None)
                {
                    self.tables.coercions.insert(n, Coercion { mut_to_shared, kind });
                }
                return true;
            }
        }
        // `*mut T → *T`: the same address with fewer permissions, as
        // `ffi.to_const` does; never the reverse direction.
        if let (Ty::Ptr(true, ai), Ty::Ptr(false, xi)) = (&a, &x)
            && self.infer.unify(ai, xi)
        {
            if let Some(n) = node {
                self.tables.coercions.insert(n, Coercion { mut_to_shared: true, kind: CoercionKind::None });
            }
            return true;
        }
        if let (Ty::Async(output), Ty::Fn(tarn_ast::CallMode::Mutable, params, ret)) = (&a, &x)
            && let [Ty::Ref(false, waker)] = params.as_slice()
            && matches!(waker.as_ref(), Ty::Adt(w, ws) if Some(*w) == self.env.decls.exec_waker && ws.is_empty())
            && let Ty::Adt(p, ps) = self.infer.shallow(ret)
            && Some(p) == self.env.decls.exec_progress
            && ps.len() == 1
            && self.infer.unify(output, &ps[0])
        {
            if let Some(n) = node {
                self.tables.coercions.insert(n, Coercion { mut_to_shared: false, kind: CoercionKind::Poller });
            }
            return true;
        }
        if self.infer.unify(&a, &x) {
            return true;
        }
        self.mismatch(span, expected, actual);
        false
    }

    fn implements(&self, t: &Ty, iface: SymbolId) -> bool {
        self.unmet(t, iface, 0).is_empty()
    }

    /// Why `t` does not implement `iface` (ADR 0078): one chain per distinct
    /// failure, outermost requirement first; empty when it does. Bounded
    /// impls require their conditions of the type arguments, recursively.
    fn unmet(&self, t: &Ty, iface: SymbolId, depth: usize) -> Vec<Vec<Obligation>> {
        let z = self.infer.zonk(t);
        let step = |bound| Obligation { ty: z.clone(), interface: iface, bound };
        // A literal still being inferred stands for its default type.
        let probe = match z {
            Ty::Var(v) if self.infer.kind(v) == VarKind::Int => Ty::Int(IntTy::I64),
            Ty::Var(v) if self.infer.kind(v) == VarKind::Float => Ty::Float(FloatTy::F64),
            _ => z.clone(),
        };
        if !self.implements_head(&probe, iface) {
            return vec![vec![step(None)]];
        }
        let Ty::Adt(target, args) = &z else { return Vec::new() };
        let Some(conditions) = self.env.decls.impl_conditions.get(&(iface, *target)) else { return Vec::new() };
        // Types are finite, so this only guards against a bug; exhausting it
        // stays conservative (ADR 0033).
        if depth > 64 {
            return vec![vec![step(None)]];
        }
        let mut out = Vec::new();
        for (index, required, bound) in conditions {
            let Some(arg) = args.get(*index) else { continue };
            for mut chain in self.unmet(arg, *required, depth + 1) {
                chain[0].bound = Some(*bound);
                chain.insert(0, step(None));
                out.push(chain);
            }
        }
        out
    }

    /// `t` has an implementation of `iface`, ignoring impl conditions.
    fn implements_head(&self, t: &Ty, iface: SymbolId) -> bool {
        if Some(iface) == self.env.decls.transfer {
            return self.env.decls.capability(&self.infer.zonk(t), crate::Capability::Transfer);
        }
        if Some(iface) == self.env.decls.share {
            return self.env.decls.capability(&self.infer.zonk(t), crate::Capability::Share);
        }
        match self.infer.shallow(t) {
            _ if Some(iface) == self.env.prelude.copy => self.is_copy(t),
            Ty::Adt(s, _) => self.env.has_impl(iface, s),
            ty @ (Ty::Bool | Ty::Int(_) | Ty::Float(_) | Ty::Str) => self.env.impl_target(&ty).is_some_and(|s| self.env.has_impl(iface, s)),
            Ty::Param(p) => self.env.decls.bounds.get(&p).is_some_and(|b| b.contains(&iface)),
            Ty::Any(i) => i == iface,
            Ty::Opaque | Ty::Error => true,
            _ => false,
        }
    }

    /// `{v}` in `f"..."`: a value whose type implements `Display`, reached
    /// through any number of references (ADR 0077).
    fn interpolated_value(&mut self, v: &Expr) {
        let t = self.expr(v, None);
        let z = self.infer.zonk(&t);
        let (inner, _) = peel(&z);
        if matches!(inner, Ty::Opaque | Ty::Error) {
            return;
        }
        // Every integer type implements `Display`, so a literal still being
        // inferred is accepted; specialization resolves its final type.
        let int_literal = matches!(inner, Ty::Var(var) if self.infer.kind(var) == VarKind::Int);
        let display = self.env.decls.display;
        if let Some(display) = display
            && (int_literal || self.implements(&inner, display))
            && let Some(method) = self.env.r.member(display, "display")
        {
            self.tables.interpolations.insert(v.id, method);
            return;
        }
        if let Some(display) = display
            && self.report_unmet(v.span, "interpolation", &inner, display)
        {
            return;
        }
        let shown = self.show(&inner);
        let mut d = Diagnostic::error("E3076", "not_displayable", format!("`{shown}` cannot be interpolated into a string"))
            .primary(v.span, format!("this is `{shown}`, which does not implement `Display`"));
        d = match inner {
            Ty::Adt(..) => d.help(format!("implement it: `impl Display for {shown} {{ fn display(&self, out &mut Builder) {{ ... }} }}`")),
            Ty::Float(_) => d.help("convert it first: `text := string.from_f64(value)`, then interpolate `{text}`"),
            Ty::Var(var) if self.infer.kind(var) == VarKind::Float => {
                d.help("convert it first: `text := string.from_f64(value)`, then interpolate `{text}`")
            }
            _ => d.note("integers, `bool`, `string` and types implementing `Display` can be interpolated"),
        };
        self.err(d);
    }

    /// E3077 when `t` has an implementation of `iface` whose bounds fail
    /// (ADR 0078): root cause first, the bound that required it, the chain
    /// and a fix that coherence allows. Returns false, reporting nothing,
    /// when `t` has no implementation at all; callers keep their own error.
    fn report_unmet(&mut self, span: Span, site: &str, t: &Ty, iface: SymbolId) -> bool {
        let chains = self.unmet(t, iface, 0);
        if chains.is_empty() || chains.iter().all(|c| c.len() < 2) {
            return false;
        }
        let mut roots: Vec<Vec<Obligation>> = Vec::new();
        for chain in chains {
            let root = chain.last().expect("chains are non-empty");
            if !roots.iter().any(|r| r.last().is_some_and(|x| x.ty == root.ty && x.interface == root.interface)) {
                roots.push(chain);
            }
        }
        let iname = |s: &Self, i: SymbolId| s.env.r.symbol(i).name.clone();
        let first = &roots[0];
        let root = first.last().unwrap();
        let (root_ty, root_iface) = (self.show(&root.ty), iname(self, root.interface));
        let outer = self.show(&first[0].ty);
        let mut d = Diagnostic::error("E3077", "impl_bound_not_satisfied", format!("`{root_ty}` does not implement `{root_iface}`"))
            .primary(span, format!("{site} on `{outer}` requires `{}`", iname(self, iface)));
        if let Some(bound) = root.bound {
            d = d.secondary(bound, "required by this bound");
        }
        let shown: Vec<String> = first.iter().map(|o| format!("`{}: {}`", self.show(&o.ty), iname(self, o.interface))).collect();
        let path = if shown.len() > 3 {
            format!("{} needs ... {} more ... needs {} needs {}", shown[0], shown.len() - 3, shown[shown.len() - 2], shown[shown.len() - 1])
        } else {
            shown.join(" needs ")
        };
        d = d.note(path);
        for other in roots.iter().skip(1).take(2) {
            let r = other.last().unwrap();
            d = d.note(format!("also: `{}` does not implement `{}`", self.show(&r.ty), iname(self, r.interface)));
        }
        d = d.help(match &root.ty {
            Ty::Param(p) => format!(
                "add the bound to the generic parameter: `{}: {root_iface}`",
                self.env.decls.param_names.get(p).cloned().unwrap_or_else(|| root_ty.clone())
            ),
            Ty::Adt(..) => format!("implement it in the module of `{root_iface}` or of `{root_ty}`: `impl {root_iface} for {root_ty} {{ ... }}`"),
            _ => format!("`{root_ty}` cannot implement `{root_iface}` outside core; wrap it in a struct you define (ADR 0016)"),
        });
        let steps = first
            .iter()
            .map(|o| tarn_diagnostics::Obligation { ty: self.show(&o.ty), interface: iname(self, o.interface), bound: o.bound })
            .collect();
        self.err(d.obligations(steps));
        true
    }

    pub fn is_copy(&self, t: &Ty) -> bool {
        match self.infer.zonk(t) {
            // Unresolved literal variables are numbers, hence copy.
            Ty::Var(v) => self.infer.kind(v) != VarKind::General,
            z => self.env.decls.is_copy(&z),
        }
    }

    fn instantiate(&mut self, generics: &[ParamId]) -> HashMap<ParamId, Ty> {
        generics.iter().map(|p| (*p, self.infer.fresh(VarKind::General))).collect()
    }

    fn obligations_for(&mut self, generics: &[ParamId], map: &HashMap<ParamId, Ty>, span: Span) {
        for p in generics {
            for iface in self.env.decls.bounds.get(p).cloned().unwrap_or_default() {
                self.obligations.push((map[p].clone(), iface, span, None));
            }
        }
    }

    fn record(&mut self, e: &Expr, t: Ty) -> Ty {
        self.tables.expr_types.insert(e.id, t.clone());
        t
    }

    // ------------------------------------------------------------ functions

    pub fn function(&mut self, f: &FnDecl, sig: &FnSig) {
        self.async_context = f.is_async;
        if let (Some(r), Some(st)) = (&f.receiver, &sig.self_ty) {
            let t = match r.kind {
                ReceiverKind::Value => st.clone(),
                ReceiverKind::Ref => Ty::Ref(false, Box::new(st.clone())),
                ReceiverKind::RefMut => Ty::Ref(true, Box::new(st.clone())),
            };
            if let Some(s) = self.def(r.id) {
                self.locals.insert(s, t);
            }
        }
        for (p, t) in f.params.iter().zip(&sig.params) {
            if let Some(s) = self.def(p.id) {
                self.locals.insert(s, t.clone());
            }
        }
        let Some(body) = &f.body else { return };
        self.rets.push(sig.ret.clone());
        self.stmts(&body.stmts);
        self.rets.pop();
        if !matches!(sig.ret, Ty::Void | Ty::Never) && !self.diverges(&body.stmts) {
            let ret = self.show(&sig.ret);
            self.err(
                Diagnostic::error("E3009", "missing_return", format!("function `{}` may end without returning `{ret}`", f.name.name))
                    .primary(Span::new(body.span.file, body.span.end - 1, body.span.end), "can reach the end here")
                    .secondary(f.ret.as_ref().map(|t| t.span).unwrap_or(f.name.span), format!("returns `{ret}`"))
                    .help("add a `return` on every path, or end with `panic(...)`"),
            );
        }
    }

    /// Defaults, deferred checks and final types. Called once per function.
    pub fn finish(&mut self) {
        self.infer.default_literals();
        for (s, span) in std::mem::take(&mut self.inferred_lets) {
            let t = self.locals[&s].clone();
            if self.infer.has_unresolved(&t) {
                let name = self.env.r.symbol(s).name.clone();
                let shown = self.show(&t);
                self.err(
                    Diagnostic::error("E3010", "cannot_infer", format!("cannot infer the type of `{name}`"))
                        .primary(span, format!("inferred so far: `{shown}`"))
                        .help(format!("annotate it: `{name}: <type> := ...`")),
                );
            }
        }
        for (span, value, t) in std::mem::take(&mut self.literals) {
            if let Ty::Int(it) = self.infer.zonk(&t) {
                let (lo, hi) = it.range();
                if value < lo || value > hi {
                    self.err(
                        Diagnostic::error("E3025", "literal_out_of_range", format!("literal `{value}` does not fit in `{}`", it.name()))
                            .primary(span, "")
                            .note(format!("`{}` holds {lo} to {hi}", it.name())),
                    );
                }
            }
        }
        for (ty, span) in std::mem::take(&mut self.scoped_results) {
            if self.env.decls.may_contain_references(&self.infer.zonk(&ty)) {
                self.err(Diagnostic::error("E3049", "scoped_task_borrowed_result", "scoped task results containing references are not supported yet")
                    .primary(span, "return owned data from the worker")
                    .note("join completes the worker; it does not by itself establish result provenance"));
            }
        }
        for (ty, cap, span, context) in std::mem::take(&mut self.task_capabilities) {
            let ty = self.infer.zonk(&ty);
            if !self.env.decls.capability(&ty, cap) {
                let shown = self.show(&ty);
                self.err(Diagnostic::error("E3047", "task_capability_required", format!("{context} of type `{shown}` requires `{cap:?}`"))
                    .primary(span, "cannot cross this native task boundary")
                    .help("use structurally capable owned data or declare the required generic bound; unknown native and erased callable values need explicit evidence"));
            }
        }
        for (t, iface, span, evidence) in std::mem::take(&mut self.obligations) {
            let z = self.infer.zonk(&t);
            let cap = if Some(iface) == self.env.decls.transfer { Some(crate::Capability::Transfer) }
                else if Some(iface) == self.env.decls.share { Some(crate::Capability::Share) } else { None };
            let witnessed = matches!(z, Ty::Fn(..)) && cap.zip(evidence.as_ref()).is_some_and(|(cap, fields)|
                fields.iter().all(|t| self.env.decls.capability(&self.infer.zonk(t), cap)));
            if !witnessed && !self.implements(&z, iface) && !matches!(z, Ty::Var(_)) && !self.report_unmet(span, "a bound", &z, iface) {
                let (tn, iname) = (self.show(&z), self.env.r.symbol(iface).name.clone());
                let help = if Some(iface) == self.env.decls.transfer || Some(iface) == self.env.decls.share {
                    format!("`{tn}` must satisfy `{iname}` structurally; generic parameters require an explicit bound")
                } else if Some(iface) == self.env.prelude.copy {
                    format!("`{tn}` is not a copy type; copy types are numbers, `bool`, `&T` and types declared with `copy`")
                } else if matches!(z, Ty::Adt(..)) {
                    format!("add `impl {iname} for {tn}` in the module of `{iname}` or of `{tn}`")
                } else {
                    format!("only structs and enums can implement interfaces; wrap the `{tn}` in a struct (ADR 0016)")
                };
                self.err(
                    Diagnostic::error("E3022", "bound_not_satisfied", format!("`{tn}` does not implement `{iname}`"))
                        .primary(span, format!("required by a bound `{iname}` here"))
                        .help(help),
                );
            }
        }
        for (t, span) in std::mem::take(&mut self.printables) {
            let z = self.infer.zonk(&t);
            let (inner, _) = peel(&z);
            if !matches!(inner, Ty::Bool | Ty::Int(_) | Ty::Float(_) | Ty::Str | Ty::Opaque | Ty::Error | Ty::Var(_)) {
                let shown = self.show(&z);
                self.err(
                    Diagnostic::error("E3026", "not_printable", format!("`print` cannot print a value of type `{shown}`"))
                        .primary(span, "")
                        .note("in v0, `print` accepts booleans, numbers and strings"),
                );
            }
        }
        for fields in self.tables.owned_captures.values_mut() {
            for (_, ty) in fields { *ty = self.infer.zonk(ty); }
        }
        for t in self.tables.expr_types.values_mut() {
            *t = self.infer.zonk(t);
        }
        for ts in self.tables.type_args.values_mut() {
            for t in ts.iter_mut() {
                *t = self.infer.zonk(t);
            }
        }
        for t in self.locals.values_mut() {
            *t = self.infer.zonk(t);
        }
    }

    // ------------------------------------------------------------ statements

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { value: None, ty, .. } => {
                // `var x: T`: the type is the annotation; initialization is
                // checked on the IR (E4005).
                let t = ty.as_ref().map(|t| self.env_lower(t)).unwrap_or(Ty::Error);
                if let Some(sym) = self.def(s.id) {
                    self.locals.insert(sym, t);
                }
            }
            StmtKind::Let { name, ty, value: Some(value), .. } => {
                let ann = ty.as_ref().map(|t| self.env_lower(t));
                let vt = self.expr(value, ann.as_ref());
                let t = match ann {
                    Some(a) => {
                        self.coerce(&vt, &a, value.span, Some(value.id));
                        a
                    }
                    None => vt,
                };
                if let Some(sym) = self.def(s.id) {
                    if matches!(self.infer.shallow(&t), Ty::Void) && ty.is_none() {
                        self.err(
                            Diagnostic::error("E3001", "type_mismatch", format!("`{}` would have type `void`", name.name)).primary(value.span, "this expression produces no value"),
                        );
                    }
                    if let Some(evidence) = self.callable_transfer_evidence(value) {
                        self.callable_transfer.insert(sym, evidence);
                    }
                    if matches!(self.infer.shallow(&t), Ty::Fn(CallMode::Shared, ..))
                        && let Some(evidence) = self.callable_capability_evidence(value, crate::Capability::Share) {
                        self.callable_share.insert(sym, evidence);
                    }
                    self.locals.insert(sym, t);
                    self.inferred_lets.push((sym, name.span));
                }
            }
            StmtKind::Assign { target, value } => {
                // Assignment can be conditional; discard creation-site evidence
                // rather than guessing the reaching callable environment.
                if let Some(Res::Symbol(symbol)) = self.res(target.id) {
                    self.callable_transfer.remove(&symbol);
                    self.callable_share.remove(&symbol);
                }
                let saved = self.assigned.replace(target.id);
                let tt = self.expr(target, None);
                self.assigned = saved;
                self.require_mut(target, false);
                let vt = self.expr(value, Some(&tt));
                self.coerce(&vt, &tt, value.span, Some(value.id));
            }
            StmtKind::Expr(e) => {
                self.expr(e, None);
            }
            StmtKind::Spawn(e) => {
                if !matches!(e.kind, ExprKind::Call { .. }) {
                    self.err(Diagnostic::error("E3036", "spawn_needs_call", "`spawn` takes a function call").primary(e.span, ""));
                }
                self.expr(e, None);
            }
            StmtKind::Return(value) => {
                let ret = self.rets.last().cloned().unwrap_or(Ty::Void);
                match value {
                    Some(v) => {
                        if matches!(ret, Ty::Void) {
                            self.expr(v, None);
                            self.err(
                                Diagnostic::error("E3028", "return_value_mismatch", "this function returns `void`, but a value is returned")
                                    .primary(v.span, "")
                                    .help("remove the value, or declare a return type"),
                            );
                        } else {
                            let t = self.expr(v, Some(&ret));
                            self.coerce(&t, &ret, v.span, Some(v.id));
                        }
                    }
                    None if !matches!(ret, Ty::Void | Ty::Never) => {
                        let shown = self.show(&ret);
                        self.err(Diagnostic::error("E3028", "return_value_mismatch", format!("`return` without a value in a function returning `{shown}`")).primary(s.span, ""));
                    }
                    None => {}
                }
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Error => {}
            StmtKind::If(i) => self.if_stmt(i),
            StmtKind::For(f) => self.for_stmt(s, f),
            StmtKind::Match(m) => self.match_stmt(m),
            StmtKind::Block(b) => self.stmts(&b.stmts),
            StmtKind::Scope(b) => {
                self.task_scope_depth += 1;
                self.stmts(&b.stmts);
                self.task_scope_depth -= 1;
            },
            StmtKind::Unsafe(b) => {
                self.unsafe_depth += 1;
                self.stmts(&b.stmts);
                self.unsafe_depth -= 1;
            }
        }
    }

    fn env_lower(&mut self, t: &Type) -> Ty {
        self.env.lower_with(self.m, t, &mut self.diags)
    }

    fn cond(&mut self, e: &Expr) {
        let t = self.expr(e, Some(&Ty::Bool));
        self.coerce(&t, &Ty::Bool, e.span, Some(e.id));
    }

    fn if_stmt(&mut self, i: &IfStmt) {
        self.cond(&i.cond);
        self.stmts(&i.then_block.stmts);
        match i.else_branch.as_deref() {
            Some(ElseBranch::If(e)) => self.if_stmt(e),
            Some(ElseBranch::Block(b)) => self.stmts(&b.stmts),
            None => {}
        }
    }

    fn for_stmt(&mut self, s: &Stmt, f: &ForStmt) {
        match &f.kind {
            ForKind::Infinite => {}
            ForKind::While(c) => self.cond(c),
            ForKind::In { iter, .. } => {
                let elem = match &iter.kind {
                    ExprKind::Range { start, end, .. } => {
                        let t = self.infer.fresh(VarKind::Int);
                        for b in [start, end].into_iter().flatten() {
                            let bt = self.expr(b, Some(&t));
                            self.coerce(&bt, &t, b.span, Some(b.id));
                        }
                        if start.is_none() || end.is_none() {
                            self.err(Diagnostic::error("E3027", "invalid_for_iter", "a `for` range needs a start and an end").primary(iter.span, ""));
                        }
                        self.record(iter, Ty::Opaque);
                        (t, BindingMode::Copy)
                    }
                    _ => {
                        let it = self.expr(iter, None);
                        self.iter_elem(&it, iter.span)
                    }
                };
                let (elem, mode) = elem;
                self.tables.binding_modes.insert(s.id, mode);
                if let Some(sym) = self.def(s.id) {
                    self.locals.insert(sym, elem);
                }
            }
        }
        self.stmts(&f.body.stmts);
    }

    /// ADR 0018: iterating through a reference yields copies of copy elements
    /// and references to non-copy elements.
    fn iter_elem(&mut self, t: &Ty, span: Span) -> (Ty, BindingMode) {
        let z = self.infer.zonk(t);
        let (base, through) = peel(&z);
        match &base {
            Ty::Array(e, _) | Ty::Slice(e) if through.is_some() => (self.project(e, through), self.mode(e, through)),
            Ty::Array(e, _) => ((**e).clone(), self.mode(e, None)),
            // ADR 0086: `for x in vec` consumes the vector; through `&`/`&mut`
            // it iterates its slice.
            Ty::Adt(id, args) if Some(*id) == self.env.decls.vec && args.len() == 1 => {
                let e = args[0].clone();
                (self.project(&e, through), self.mode(&e, through))
            }
            Ty::Opaque | Ty::Error => (Ty::Opaque, BindingMode::Copy),
            _ => (self.bad_iter(&z, span), BindingMode::Copy),
        }
    }

    fn bad_iter(&mut self, t: &Ty, span: Span) -> Ty {
        let shown = self.show(t);
        self.err(
            Diagnostic::error("E3027", "invalid_for_iter", format!("cannot iterate over `{shown}`"))
                .primary(span, "")
                .note("`for` iterates over ranges `a..b`, arrays, vectors and `&` / `&mut` arrays, slices and vectors"),
        );
        Ty::Error
    }

    /// ADR 0018: how a binding receives a value reached through `through`.
    fn mode(&self, t: &Ty, through: Option<bool>) -> BindingMode {
        match through {
            _ if self.is_copy(t) => BindingMode::Copy,
            None => BindingMode::Move,
            Some(m) => BindingMode::Ref(m),
        }
    }

    /// Element/field reached through a reference (ADR 0018).
    fn project(&self, t: &Ty, through: Option<bool>) -> Ty {
        match through {
            None => t.clone(),
            Some(_) if self.is_copy(t) => t.clone(),
            Some(m) => Ty::Ref(m, Box::new(t.clone())),
        }
    }

    // ------------------------------------------------------------ match

    fn match_stmt(&mut self, m: &MatchStmt) {
        let st = self.expr(&m.scrutinee, None);
        for arm in &m.arms {
            self.pattern(&arm.pattern, &st);
            if let Some(g) = &arm.guard {
                self.cond(g);
            }
            match &arm.body.kind {
                StmtKind::Block(b) => self.stmts(&b.stmts),
                _ => self.stmt(&arm.body),
            }
        }
        self.exhaustive(m, &st);
    }

    /// `match` in expression position (ADR 0089): every arm is a value of one
    /// type, or a block that never finishes (`return`, `panic`, ...).
    fn match_expr(&mut self, m: &MatchStmt, expected: Option<&Ty>) -> Ty {
        let st = self.expr(&m.scrutinee, None);
        let mut result: Option<Ty> = expected.cloned();
        let mut failed = false;
        for arm in &m.arms {
            self.pattern(&arm.pattern, &st);
            if let Some(g) = &arm.guard {
                self.cond(g);
            }
            match &arm.body.kind {
                StmtKind::Expr(value) => {
                    let t = self.expr(value, result.as_ref());
                    match &result {
                        Some(r) => {
                            let r = r.clone();
                            self.coerce(&t, &r, value.span, Some(value.id));
                        }
                        None if !matches!(self.infer.shallow(&t), Ty::Never) => result = Some(t),
                        None => {}
                    }
                }
                StmtKind::Block(b) => {
                    self.stmts(&b.stmts);
                    if !self.leaves(&b.stmts) {
                        failed = true;
                        self.err(
                            Diagnostic::error("E3083", "match_arm_without_value", "this arm of a `match` expression produces no value")
                                .primary(arm.body.span, "control reaches the end of this block")
                                .help("write the value after `=>`, or end the block with `return`, `break`, `continue` or `panic`"),
                        );
                    }
                }
                _ => {
                    self.stmt(&arm.body);
                    if self.leaves(std::slice::from_ref(arm.body.as_ref())) {
                        continue;
                    }
                    failed = true;
                    self.err(
                        Diagnostic::error("E3083", "match_arm_without_value", "this arm of a `match` expression produces no value")
                            .primary(arm.body.span, "a statement, not a value")
                            .help("write the value after `=>`, or use a `match` statement"),
                    );
                }
            }
        }
        self.exhaustive(m, &st);
        // An arm already reported as valueless must not make the whole match
        // `Never`: that would turn every later use into a false error.
        result.unwrap_or(if failed { Ty::Error } else { Ty::Never })
    }

    /// Whether control never reaches the end of `stmts`, counting a direct
    /// `break` or `continue` of the enclosing loop.
    fn leaves(&self, stmts: &[Stmt]) -> bool {
        self.diverges(stmts) || stmts.iter().any(|s| matches!(s.kind, StmtKind::Break | StmtKind::Continue))
    }

    fn pattern(&mut self, p: &Pattern, t: &Ty) {
        let z = self.infer.zonk(t);
        let (inner, through) = peel(&z);
        match &p.kind {
            PatternKind::Wildcard | PatternKind::Error => {}
            PatternKind::Ident(_) => match self.res(p.id) {
                None => {
                    if let Some(sym) = self.def(p.id) {
                        let bt = self.project(&inner, through);
                        self.locals.insert(sym, bt);
                        self.tables.binding_modes.insert(p.id, self.mode(&inner, through));
                    }
                }
                Some(r) => {
                    self.variant_pattern(p, &r, &inner, through, &[]);
                }
            },
            PatternKind::Literal(e) => {
                let lt = self.expr(e, Some(&inner));
                if !self.infer.unify(&lt, &inner) {
                    self.mismatch(e.span, &inner, &lt);
                }
            }
            PatternKind::Range { start, end, .. } => {
                for b in [start, end] {
                    let bt = self.expr(b, Some(&inner));
                    if !self.infer.unify(&bt, &inner) {
                        self.mismatch(b.span, &inner, &bt);
                    }
                }
            }
            PatternKind::Variant { args, .. } => {
                if let Some(r) = self.res(p.id) {
                    self.variant_pattern(p, &r, &inner, through, args);
                } else {
                    for a in args {
                        self.pattern(a, &Ty::Error);
                    }
                }
            }
            PatternKind::Struct { fields, .. } => {
                let Some(Res::Symbol(s)) = self.res(p.id) else { return };
                let targs = match &inner {
                    Ty::Adt(x, a) if *x == s => a.clone(),
                    Ty::Opaque | Ty::Error => Vec::new(),
                    other => {
                        let expected = self.show(other);
                        let name = self.env.r.symbol(s).name.clone();
                        self.err(Diagnostic::error("E3020", "pattern_mismatch", format!("pattern of struct `{name}` cannot match `{expected}`")).primary(p.span, ""));
                        return;
                    }
                };
                let Some(def) = self.env.decls.structs.get(&s) else { return };
                let map: HashMap<ParamId, Ty> = def.generics.iter().copied().zip(targs).collect();
                for fp in fields {
                    let Some(fd) = def.fields.iter().find(|f| f.name == fp.name.name) else {
                        let name = self.env.r.symbol(s).name.clone();
                        self.err(Diagnostic::error("E3012", "unknown_field", format!("struct `{name}` has no field `{}`", fp.name.name)).primary(fp.name.span, ""));
                        continue;
                    };
                    let ft = subst(&fd.ty, &map);
                    match &fp.pattern {
                        Some(sub) => {
                            let wrapped = match through {
                                Some(m) => Ty::Ref(m, Box::new(ft)),
                                None => ft,
                            };
                            self.pattern(sub, &wrapped);
                        }
                        None => {
                            if let Some(sym) = self.def(fp.id) {
                                let bt = self.project(&ft, through);
                                self.locals.insert(sym, bt);
                                self.tables.binding_modes.insert(fp.id, self.mode(&ft, through));
                            }
                        }
                    }
                }
            }
        }
    }

    fn variant_pattern(&mut self, p: &Pattern, r: &Res, scrut: &Ty, through: Option<bool>, args: &[Pattern]) {
        let (enum_sym, targs) = match scrut {
            Ty::Adt(s, a) if self.env.decls.enums.contains_key(s) => (*s, a.clone()),
            Ty::Opaque | Ty::Error | Ty::Var(_) => {
                for a in args {
                    self.pattern(a, &Ty::Opaque);
                }
                return;
            }
            other => {
                let shown = self.show(other);
                let mut d = Diagnostic::error("E3020", "pattern_mismatch", format!("a variant pattern cannot match a value of type `{shown}`"))
                    .primary(p.span, "")
                    .note("only enum values have variants");
                if let (Res::ScrutineeVariant(name), PatternKind::Ident(_)) = (r, &p.kind)
                    && matches!(other, Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Str)
                {
                    d = d.help(format!("constants are not patterns; compare with a guard: `value if value == {name} =>`"));
                }
                self.err(d);
                return;
            }
        };
        let def = &self.env.decls.enums[&enum_sym];
        let enum_name = self.env.r.symbol(enum_sym).name.clone();
        let variant = match r {
            Res::Symbol(v) => def.variants.iter().find(|x| x.sym == *v),
            Res::ScrutineeVariant(n) => def.variants.iter().find(|x| &x.name == n),
            Res::External { .. } => None,
        };
        let Some(variant) = variant else {
            let name = match r {
                Res::Symbol(v) => self.env.r.symbol(*v).name.clone(),
                Res::ScrutineeVariant(n) => n.clone(),
                _ => String::new(),
            };
            let names: Vec<_> = def.variants.iter().map(|v| v.name.clone()).collect();
            let mut d = Diagnostic::error("E3019", "unknown_variant", format!("enum `{enum_name}` has no variant `{name}`")).primary(p.span, "");
            d = d.note(format!("its variants are: {}", names.join(", ")));
            self.err(d);
            return;
        };
        let (vsym, vname, fields) = (variant.sym, variant.name.clone(), variant.fields.clone());
        self.tables.pattern_variants.insert(p.id, vsym);
        let unit_form = matches!(p.kind, PatternKind::Ident(_));
        if unit_form && !fields.is_empty() || !unit_form && fields.len() != args.len() {
            self.err(
                Diagnostic::error(
                    "E3020",
                    "pattern_mismatch",
                    format!("variant `{enum_name}.{vname}` has {} field{}, but the pattern has {}", fields.len(), if fields.len() == 1 { "" } else { "s" }, args.len()),
                )
                .primary(p.span, ""),
            );
        }
        let map: HashMap<ParamId, Ty> = def.generics.iter().copied().zip(targs).collect();
        for (a, ft) in args.iter().zip(fields.iter()) {
            let ft = subst(ft, &map);
            let wrapped = match through {
                Some(m) => Ty::Ref(m, Box::new(ft)),
                None => ft,
            };
            self.pattern(a, &wrapped);
        }
    }

    /// Exhaustiveness and reachability with the usefulness algorithm
    /// (ADR 0019, `exhaust.rs`). Guarded arms never count as covering.
    fn exhaustive(&mut self, m: &MatchStmt, st: &Ty) {
        if matches!(self.infer.shallow(st), Ty::Opaque | Ty::Error) {
            return;
        }
        let tys = vec![st.clone()];
        let mut rows: Vec<Vec<crate::exhaust::Pat>> = Vec::new();
        for arm in &m.arms {
            let row = vec![self.lower_pat(&arm.pattern, st)];
            if !self.useful(&rows, &row, &tys) {
                let mut d = Diagnostic::error("W3001", "unreachable_arm", "this arm can never match")
                    .primary(arm.pattern.span, "")
                    .note("every value it matches is handled by an earlier arm")
                    .help("remove it, or move it before the arm that covers it");
                d.severity = tarn_diagnostics::Severity::Warning;
                self.err(d);
            }
            if arm.guard.is_none() {
                rows.push(row);
            }
        }
        if let Some(w) = self.witness(&rows, &tys) {
            let missing = self.show_pat(&w[0]);
            self.err(
                Diagnostic::error("E3018", "non_exhaustive_match", format!("`match` does not cover `{missing}`"))
                    .primary(m.scrutinee.span, format!("`{missing}` is not handled"))
                    .help(format!("add an arm for `{missing}`"))
                    .note("arms with an `if` guard do not count as covering"),
            );
        }
    }

    /// Whether control can never reach the end of these statements.
    pub fn diverges(&self, stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| self.stmt_diverges(s))
    }

    fn stmt_diverges(&self, s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Return(_) => true,
            StmtKind::Expr(e) => matches!(self.tables.expr_types.get(&e.id).map(|t| self.infer.zonk(t)), Some(Ty::Never)),
            StmtKind::If(i) => self.if_diverges(i),
            StmtKind::Match(m) => {
                !m.arms.is_empty()
                    && !self.diags.iter().any(|d| d.code == "E3018" && d.primary_span() == Some(m.scrutinee.span))
                    && m.arms.iter().all(|a| match &a.body.kind {
                        StmtKind::Block(b) => self.diverges(&b.stmts),
                        _ => self.stmt_diverges(&a.body),
                    })
            }
            StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => self.diverges(&b.stmts),
            StmtKind::For(f) => matches!(f.kind, ForKind::Infinite) && !has_break(&f.body.stmts),
            _ => false,
        }
    }

    fn if_diverges(&self, i: &IfStmt) -> bool {
        self.diverges(&i.then_block.stmts)
            && match i.else_branch.as_deref() {
                Some(ElseBranch::If(e)) => self.if_diverges(e),
                Some(ElseBranch::Block(b)) => self.diverges(&b.stmts),
                None => false,
            }
    }

    // ------------------------------------------------------------ mutability (ADR 0017)

    /// Report if `e` is not a mutable place (ADR 0017). `borrow` selects the
    /// E3016 wording (`&mut`, `&mut self` calls) over E3015 (assignment).
    fn require_mut(&mut self, e: &Expr, borrow: bool) -> bool {
        self.require_mut_at(e, e, borrow)
    }

    fn require_mut_at(&mut self, whole: &Expr, e: &Expr, borrow: bool) -> bool {
        let (code, kind) = if borrow { ("E3016", "borrow_mut_immutable") } else { ("E3015", "assign_immutable") };
        let target = |n: &str| {
            if borrow { format!("cannot borrow `{n}` as mutable") } else { format!("cannot assign to `{n}`") }
        };
        let projected = !std::ptr::eq(whole, e);
        match &e.kind {
            ExprKind::Paren(inner) => self.require_mut_at(whole, inner, borrow),
            ExprKind::Ident(name) => {
                let Some(Res::Symbol(s)) = self.res(e.id) else { return true };
                let sym = self.env.r.symbol(s).clone();
                let place = if projected { format!("{name}.…") } else { name.clone() };
                let place = if projected { crate::place_text(whole).unwrap_or(place) } else { place };
                match sym.kind {
                    SymbolKind::Local { mutable: true } => true,
                    SymbolKind::Local { mutable: false } => {
                        let mut d = Diagnostic::error(code, kind, format!("{}, because `{name}` is not declared with `var`", target(&place))).primary(whole.span, "");
                        if let Some(sp) = sym.span {
                            d = d.secondary(sp, "declared immutable here");
                        }
                        self.err(d.help(format!("declare it with `var {name} = ...`")));
                        false
                    }
                    SymbolKind::Param | SymbolKind::SelfParam | SymbolKind::PatternBinding | SymbolKind::LoopBinding | SymbolKind::ClosureParam => {
                        let what = crate::binding_kind(&sym.kind);
                        self.err(
                            Diagnostic::error(code, kind, format!("{}: `{name}` is a {what}, and {what}s are immutable", target(&place)))
                                .primary(whole.span, "")
                                .help(format!("copy it into a mutable binding first: `var {name}2 = {name}`")),
                        );
                        false
                    }
                    _ => {
                        self.err(Diagnostic::error(code, kind, target(&place)).primary(whole.span, ""));
                        false
                    }
                }
            }
            ExprKind::Unary { op: UnaryOp::Deref, operand: base } | ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => {
                let bt = self.tables.expr_types.get(&base.id).map(|t| self.infer.zonk(t)).unwrap_or(Ty::Error);
                match bt {
                    Ty::Ref(true, _) | Ty::Opaque | Ty::Error => true,
                    Ty::Ref(false, _) => {
                        let shown = self.show(&bt);
                        let verb = if borrow { "borrow as mutable" } else { "assign" };
                        self.err(
                            Diagnostic::error(code, kind, format!("cannot {verb} through a shared reference `{shown}`"))
                                .primary(whole.span, "")
                                .secondary(base.span, "this is a `&` reference")
                                .help("take a mutable reference (`&mut`) instead"),
                        );
                        false
                    }
                    _ => self.require_mut_at(whole, base, borrow),
                }
            }
            _ => {
                let what = if borrow { "borrow a temporary value as mutable" } else { "assign to a temporary value" };
                self.err(Diagnostic::error(code, kind, format!("cannot {what}")).primary(whole.span, "").help("store it in a `var` first"));
                false
            }
        }
    }

    // ------------------------------------------------------------ expressions

    pub fn expr(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        let t = self.expr_inner(e, expected);
        self.record(e, t)
    }

    fn int_literal(&mut self, e: &Expr, value: i128) -> Ty {
        let t = self.infer.fresh(VarKind::Int);
        self.literals.push((e.span, value, t.clone()));
        t
    }

    fn expr_inner(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        match &e.kind {
            ExprKind::Int(v) => self.int_literal(e, *v as i128),
            ExprKind::Float(_) => self.infer.fresh(VarKind::Float),
            ExprKind::Str(_) => {
                self.string_literals.insert(e.id);
                Ty::Str
            }
            ExprKind::Interpolated(parts) => {
                for part in parts {
                    if let InterpPart::Value(v) = part {
                        self.interpolated_value(v);
                    }
                }
                Ty::Str
            }
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Void,
            ExprKind::Error => Ty::Error,
            ExprKind::Paren(inner) => {
                let saved = self.assigned;
                if saved == Some(e.id) { self.assigned = Some(inner.id); }
                let ty = self.expr(inner, expected);
                self.assigned = saved;
                ty
            },
            ExprKind::Ident(_) => self.name_value(e, expected),
            ExprKind::Field { base, name } => match self.res(e.id) {
                Some(_) => self.name_value(e, expected),
                None => self.field(e, base, name),
            },
            ExprKind::Call { callee, args } => self.call(e, callee, args, expected),
            ExprKind::Index { base, index } => self.index(e, base, index),
            ExprKind::Unary { op, operand } => self.unary(e, *op, operand, expected),
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs),
            ExprKind::Range { .. } => {
                self.err(
                    Diagnostic::error("E3034", "range_outside_for", "ranges can only be used in `for` loops and slicing").primary(e.span, "").note("ranges are not values in v0"),
                );
                Ty::Error
            }
            ExprKind::Try(inner) => self.try_expr(e, inner),
            ExprKind::Await(inner) => {
                let expected_operation = expected.map(|output| Ty::Async(Box::new(output.clone())));
                let operand = self.expr(inner, expected_operation.as_ref());
                if !self.async_context {
                    self.err(Diagnostic::error("E3060", "await_outside_async", "`await` is only allowed inside an async function")
                        .primary(e.span, "this function does not suspend"));
                    return Ty::Error;
                }
                match self.infer.shallow(&operand) {
                    Ty::Async(output) => return *output,
                    Ty::Adt(id, args) if Some(id) == self.env.decls.exec_operation && args.len() == 1 => return args[0].clone(),
                    Ty::Error => return Ty::Error,
                    _ => {}
                }
                {
                    self.err(Diagnostic::error("E3061", "not_awaitable", "`await` requires a suspended computation")
                        .primary(inner.span, "this value is not awaitable"));
                }
                Ty::Error
            }
            ExprKind::StructLit { fields, .. } => self.struct_lit(e, fields),
            ExprKind::ArrayLit { ty, elems } => {
                let at = self.env_lower(ty);
                let (elem, len) = match &at {
                    Ty::Array(el, n) => ((**el).clone(), Some(*n)),
                    Ty::Slice(el) => ((**el).clone(), None),
                    _ => (Ty::Error, None),
                };
                for x in elems {
                    let xt = self.expr(x, Some(&elem));
                    self.coerce(&xt, &elem, x.span, Some(x.id));
                }
                if let Some(n) = len
                    && n != elems.len() as u64
                {
                    self.err(
                        Diagnostic::error("E3037", "array_length_mismatch", format!("array of length {n} has {} elements", elems.len()))
                            .primary(e.span, "")
                            .help("write `[]T{...}` to take the length from the elements"),
                    );
                }
                Ty::Array(Box::new(elem), elems.len() as u64)
            }
            ExprKind::Closure { params, ret, body, .. } => self.closure(e, params, ret.as_ref(), body, expected),
            ExprKind::Match(m) => self.match_expr(m, expected),
        }
    }

    /// Snapshot evidence for a known callable value. Erased signatures alone
    /// never grant transfer authority, and reassignment invalidates evidence.
    fn callable_transfer_evidence(&self, expression: &Expr) -> Option<Vec<Ty>> {
        self.callable_capability_evidence(expression, crate::Capability::Transfer)
    }

    fn callable_capability_evidence(&self, expression: &Expr, cap: crate::Capability) -> Option<Vec<Ty>> {
        match &expression.kind {
            ExprKind::Paren(inner) => self.callable_capability_evidence(inner, cap),
            ExprKind::Closure { .. } => {
                if cap == crate::Capability::Share && !matches!(self.tables.expr_types.get(&expression.id).map(|t| self.infer.shallow(t)), Some(Ty::Fn(CallMode::Shared, ..))) { return None; }
                let mut evidence = Vec::new();
                for (symbol, mode) in self.tables.closure_captures.get(&expression.id)? {
                    let ty = self.locals.get(symbol)?.clone();
                    let required = if *mode == crate::CaptureMode::SharedBorrow { crate::Capability::Share } else { cap };
                    if matches!(self.infer.shallow(&ty), Ty::Fn(..)) {
                        let nested = if required == crate::Capability::Share { self.callable_share.get(symbol)? } else { self.callable_transfer.get(symbol)? };
                        evidence.extend(nested.iter().map(|t| if *mode == crate::CaptureMode::SharedBorrow { Ty::Ref(false, Box::new(t.clone())) } else { t.clone() }));
                    } else {
                        evidence.push(match mode {
                            crate::CaptureMode::Move => ty,
                            crate::CaptureMode::SharedBorrow => Ty::Ref(false, Box::new(ty)),
                            crate::CaptureMode::MutableBorrow => Ty::Ref(true, Box::new(ty)),
                        });
                    }
                    if evidence.len() > 4096 { return None; }
                }
                Some(evidence)
            }
            _ => match self.res(expression.id) {
                Some(Res::Symbol(symbol)) if matches!(self.kind(symbol), SymbolKind::Function) => Some(Vec::new()),
                Some(Res::Symbol(symbol)) => if cap == crate::Capability::Share { self.callable_share.get(&symbol).cloned() } else { self.callable_transfer.get(&symbol).cloned() },
                _ => None,
            },
        }
    }

    /// An identifier or a static path used as a value.
    fn name_value(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        match self.res(e.id) {
            Some(Res::External { .. }) => {
                self.err(
                    Diagnostic::error("E3040", "unmodeled_std_api", "standard-library API has no ownership/provenance contract")
                        .primary(e.span, "cannot safely type-check this API")
                        .help("use a module with explicit Tarn declarations"),
                );
                Ty::Error
            }
            Some(Res::ScrutineeVariant(_)) | None => Ty::Error,
            Some(Res::Symbol(s)) => match self.kind(s).clone() {
                SymbolKind::Const => self.env.decls.consts.get(&s).map_or(Ty::Error, |c| c.ty.clone()),
                SymbolKind::Local { .. } | SymbolKind::Param | SymbolKind::SelfParam | SymbolKind::PatternBinding | SymbolKind::LoopBinding | SymbolKind::ClosureParam => {
                    self.locals.get(&s).cloned().unwrap_or(Ty::Error)
                }
                SymbolKind::Variant { parent } => {
                    let (fields, generics) = self.variant_info(parent, s);
                    let map = self.instantiate(&generics);
                    let adt = Ty::Adt(parent, generics.iter().map(|p| map[p].clone()).collect());
                    // Where a function is expected, a payload variant is its
                    // constructor: `map_err(http.Error.Io)` (Phase 23B).
                    // It captures nothing, so it adopts the expected invocation mode.
                    if !fields.is_empty() && let Some(Ty::Fn(mode, ..)) = expected.map(|t| self.infer.shallow(t)) {
                        return Ty::Fn(mode, fields.iter().map(|f| subst(f, &map)).collect(), Box::new(adt));
                    }
                    if !fields.is_empty() {
                        // Written as the user would: `Some(...)`, `Shape.Circle(...)`.
                        let vname = self.env.r.symbol(s).name.clone();
                        let in_prelude = self.env.r.scope(tarn_resolve::ScopeId(0)).get(&vname) == Some(s);
                        let name = if in_prelude { vname } else { format!("{}.{vname}", self.env.r.symbol(parent).name) };
                        self.err(
                            Diagnostic::error("E3038", "variant_needs_args", format!("variant `{name}` takes {} value{}", fields.len(), if fields.len() == 1 { "" } else { "s" }))
                                .primary(e.span, "")
                                .help(format!("write `{name}(...)`")),
                        );
                        return Ty::Error;
                    }
                    Ty::Adt(parent, generics.iter().map(|p| map[p].clone()).collect())
                }
                SymbolKind::Function | SymbolKind::Method { .. } | SymbolKind::ImplMethod { .. } => {
                    if Some(s) == self.env.decls.ffi_callback {
                        self.err(Diagnostic::error("E3081", "invalid_c_callback", "the C callback constructor must be called directly")
                            .primary(e.span, "use `ffi.callback(handler)` instead of storing the constructor"));
                        return Ty::Error;
                    }
                    let Some(sig) = self.env.decls.fns.get(&s).cloned() else { return Ty::Error };
                    if sig.is_unsafe || sig.abi.as_deref().is_some_and(|abi| abi != "intrinsic") {
                        self.err(Diagnostic::error("E3082", "unsafe_function_value", "an unsafe or foreign function cannot become an ordinary callable value")
                            .primary(e.span, "the `fn` type does not preserve its unsafe call requirement")
                            .help("call the function directly inside `unsafe`, or write a Tarn wrapper that checks its safety requirements"));
                        return Ty::Error;
                    }
                    let map = self.instantiate(&sig.generics);
                    if !sig.generics.is_empty() {
                        self.tables.type_args.insert(e.id, sig.generics.iter().map(|p| map[p].clone()).collect());
                    }
                    let mut ps: Vec<Ty> = Vec::new();
                    if let (Some(r), Some(st)) = (sig.receiver, &sig.self_ty) {
                        ps.push(receiver_ty(r, st));
                    }
                    ps.extend(sig.params.iter().cloned());
                    let output = subst(&sig.ret, &map);
                    let result = if sig.is_async { Ty::Async(Box::new(output)) } else { output };
                    // A named function has no environment (null, ADR 0032), so
                    // it serves any invocation mode: `map_err(wrap)` where a
                    // `once fn` is expected, `mut fn` callbacks (ADR 0087).
                    let mode = match expected.map(|x| self.infer.shallow(x)) {
                        Some(Ty::Fn(mode, ..)) => mode,
                        _ => CallMode::Shared,
                    };
                    Ty::Fn(mode, ps.iter().map(|p| subst(p, &map)).collect(), Box::new(result))
                }
                SymbolKind::Module(_) => Ty::Error,
                k => {
                    let name = self.env.r.symbol(s).name.clone();
                    let what = crate::binding_kind(&k);
                    self.err(Diagnostic::error("E3035", "not_a_value", format!("{what} `{name}` is not a value")).primary(e.span, "").help(match k {
                        SymbolKind::Struct => format!("build one with `{name}{{...}}`"),
                        SymbolKind::Builtin => format!("call it: `{name}(...)`"),
                        _ => "use a value of this type instead".to_string(),
                    }));
                    Ty::Error
                }
            },
        }
    }

    fn variant_info(&self, parent: SymbolId, v: SymbolId) -> (Vec<Ty>, Vec<ParamId>) {
        let Some(def) = self.env.decls.enums.get(&parent) else { return (Vec::new(), Vec::new()) };
        let fields = def.variants.iter().find(|x| x.sym == v).map(|x| x.fields.clone()).unwrap_or_default();
        (fields, def.generics.clone())
    }

    fn field(&mut self, e: &Expr, base: &Expr, name: &Ident) -> Ty {
        let bt = self.expr(base, None);
        self.field_of(e, bt, name)
    }

    /// Field type given the already-checked type `bt` of its base.
    fn field_of(&mut self, e: &Expr, bt: Ty, name: &Ident) -> Ty {
        let (inner, through) = peel(&self.infer.zonk(&bt));
        let _ = through;
        match &inner {
            Ty::Opaque | Ty::Error => Ty::Opaque,
            Ty::Adt(s, args) if self.env.decls.structs.contains_key(s) => {
                let def = &self.env.decls.structs[s];
                let Some(f) = def.fields.iter().find(|f| f.name == name.name) else {
                    let tname = self.show(&inner);
                    let names: Vec<&str> = def.fields.iter().map(|f| f.name.as_str()).collect();
                    let mut d = Diagnostic::error("E3004", "no_field", format!("`{tname}` has no field `{}`", name.name)).primary(name.span, "");
                    if let Some(sug) = tarn_resolve::suggest::best(&name.name, names.iter().copied()) {
                        d = d.help(format!("did you mean `{sug}`?"));
                    } else if !names.is_empty() {
                        d = d.note(format!("its fields are: {}", names.join(", ")));
                    }
                    self.err(d);
                    return Ty::Error;
                };
                if def.module != self.m && !f.is_pub && !self.internal_visible(def.module) {
                    let tname = self.env.r.symbol(*s).name.clone();
                    self.err(
                        Diagnostic::error("E3014", "private_field", format!("field `{}` of `{tname}` is private", name.name))
                            .primary(name.span, "")
                            .help("mark the field `pub`, or add a method that exposes it"),
                    );
                }
                let map: HashMap<ParamId, Ty> = def.generics.iter().copied().zip(args.iter().cloned()).collect();
                let _ = e;
                subst(&f.ty, &map)
            }
            other => {
                let shown = self.show(other);
                let mut d = Diagnostic::error("E3004", "no_field", format!("`{shown}` has no field `{}`", name.name)).primary(name.span, "");
                if matches!(other, Ty::Adt(..)) {
                    d = d.note("only structs have fields; did you mean to call a method?");
                }
                self.err(d);
                Ty::Error
            }
        }
    }

    fn index(&mut self, e: &Expr, base: &Expr, index: &Expr) -> Ty {
        let bt = self.expr(base, None);
        let (inner, _) = peel(&self.infer.zonk(&bt));
        let elem = match &inner {
            Ty::Array(e, _) | Ty::Slice(e) => (**e).clone(),
            Ty::Opaque | Ty::Error => Ty::Opaque,
            other => {
                let shown = self.show(other);
                self.err(Diagnostic::error("E3008", "not_indexable", format!("cannot index into a value of type `{shown}`")).primary(base.span, ""));
                Ty::Error
            }
        };
        let usize_ = Ty::Int(IntTy::Usize);
        if let ExprKind::Range { start, end, .. } = &index.kind {
            for b in [start, end].into_iter().flatten() {
                let t = self.expr(b, Some(&usize_));
                self.coerce(&t, &usize_, b.span, Some(b.id));
            }
            self.record(index, Ty::Opaque);
            // v0 has no owned slices: `arr[a..b]` only exists behind `&`.
            if self.borrowed != Some(e.id) {
                self.err(
                    Diagnostic::error("E3039", "slice_by_value", "a slice can only be used through a reference")
                        .primary(e.span, "this is an unsized slice")
                        .help("borrow it: `&arr[a..b]` (or `&mut arr[a..b]`)")
                        .note("Tarn v0 has no owned slices"),
                );
                return Ty::Error;
            }
            return Ty::Slice(Box::new(elem));
        }
        let it = self.expr(index, Some(&usize_));
        self.coerce(&it, &usize_, index.span, Some(index.id));
        elem
    }

    fn unary(&mut self, e: &Expr, op: UnaryOp, operand: &Expr, expected: Option<&Ty>) -> Ty {
        match op {
            UnaryOp::Spawn => {
                let ty = self.expr(operand, None);
                match self.infer.shallow(&ty) {
                    Ty::Fn(_, params, result) if params.is_empty() => {
                        if self.task_scope_depth == 0 && !matches!(operand.kind, ExprKind::Closure { owned: true, .. }) {
                            self.err(Diagnostic::error("E4206", "reference_to_spawned_task", "unscoped spawn requires an owned closure")
                                .primary(operand.span, "use `move fn` to transfer captures"));
                        }
                        self.task_capabilities.push((*result.clone(), crate::Capability::Transfer, operand.span, "task result".into()));
                        if self.task_scope_depth > 0 {
                            self.tables.scoped_spawns.insert(e.id);
                            self.scoped_results.push((*result.clone(), operand.span));
                            if let Some(captures) = self.tables.closure_captures.get(&operand.id) {
                                for (symbol, mode) in captures {
                                    let ty = self.locals.get(symbol).cloned().unwrap_or(Ty::Error);
                                    let cap = if *mode == crate::CaptureMode::SharedBorrow { crate::Capability::Share } else { crate::Capability::Transfer };
                                    let name = self.env.r.symbol(*symbol).name.clone();
                                    let evidence = if cap == crate::Capability::Share { self.callable_share.get(symbol) } else { self.callable_transfer.get(symbol) };
                                    if matches!(self.infer.shallow(&ty), Ty::Fn(..)) && let Some(evidence) = evidence {
                                        for component in evidence {
                                            self.task_capabilities.push((component.clone(), cap, operand.span, format!("scoped callable capture `{name}`")));
                                        }
                                    } else {
                                        self.task_capabilities.push((ty, cap, operand.span, format!("scoped capture `{name}`")));
                                    }
                                }
                            }
                        }
                        if self.task_scope_depth == 0 && let Some(captures) = self.tables.owned_captures.get(&operand.id) {
                            for (symbol, ty) in captures {
                                let name = self.env.r.symbol(*symbol).name.clone();
                                if matches!(self.infer.shallow(ty), Ty::Fn(..)) {
                                    if let Some(evidence) = self.callable_transfer.get(symbol) {
                                        for component in evidence {
                                            self.task_capabilities.push((component.clone(), crate::Capability::Transfer, operand.span, format!("callable capture `{name}`")));
                                        }
                                    } else {
                                        self.task_capabilities.push((ty.clone(), crate::Capability::Transfer, operand.span, format!("capture `{name}`")));
                                    }
                                } else {
                                    self.task_capabilities.push((ty.clone(), crate::Capability::Transfer, operand.span, format!("capture `{name}`")));
                                }
                            }
                        }
                        self.env.decls.task.map(|id| Ty::Adt(id, vec![*result])).unwrap_or(Ty::Error)
                    }
                    _ => {
                        self.err(Diagnostic::error("E3036", "spawn_needs_call", "spawn requires a zero-parameter owned closure")
                            .primary(operand.span, ""));
                        Ty::Error
                    }
                }
            }
            UnaryOp::Neg => {
                let t = match operand.kind {
                    ExprKind::Int(v) => {
                        let t = self.int_literal(e, -(v as i128));
                        self.record(operand, t.clone())
                    }
                    _ => self.expr(operand, expected),
                };
                match self.infer.shallow(&t) {
                    Ty::Int(i) if !i.signed() => {
                        self.err(Diagnostic::error("E3007", "invalid_unary", format!("cannot negate an unsigned `{}`", i.name())).primary(e.span, ""));
                        Ty::Error
                    }
                    Ty::Int(_) | Ty::Float(_) | Ty::Opaque | Ty::Error => t,
                    Ty::Var(v) if self.infer.kind(v) != VarKind::General => t,
                    other => {
                        let shown = self.show(&other);
                        self.err(Diagnostic::error("E3007", "invalid_unary", format!("cannot negate a value of type `{shown}`")).primary(e.span, ""));
                        Ty::Error
                    }
                }
            }
            UnaryOp::Not => {
                let t = self.expr(operand, Some(&Ty::Bool));
                self.coerce(&t, &Ty::Bool, operand.span, Some(operand.id));
                Ty::Bool
            }
            UnaryOp::Deref => {
                let place = self.borrowed == Some(e.id) || self.assigned == Some(e.id);
                let t = self.expr(operand, None);
                let z = self.infer.zonk(&t);
                match z {
                    Ty::Ref(_, inner) => {
                        if !place && !self.is_copy(&inner) {
                            let shown = self.show(&inner);
                            self.err(
                                Diagnostic::error("E3079", "deref_move", format!("cannot move `{shown}` out of a reference"))
                                    .primary(e.span, "")
                                    .note("`*r` reads a copy; only copy types can be read this way")
                                    .help("borrow it (`&*r`, or use `r` directly), clone it, or assign a new value with `*r = value`"),
                            );
                        }
                        *inner
                    }
                    Ty::Opaque | Ty::Error => z,
                    other => {
                        let shown = self.show(&other);
                        let help = if matches!(other, Ty::Ptr(..)) { "raw pointers are read and written through `ffi` helpers inside `unsafe`" } else { "`*` applies to references (`&T` or `&mut T`)" };
                        self.err(Diagnostic::error("E3080", "deref_non_reference", format!("cannot dereference `{shown}`")).primary(e.span, "").help(help));
                        Ty::Error
                    }
                }
            }
            UnaryOp::Ref | UnaryOp::RefMut => {
                let inner_expected = match expected.map(|x| self.infer.shallow(x)) {
                    Some(Ty::Ref(_, x)) => Some(*x),
                    _ => None,
                };
                // `&arr[a..b]`: the slice is legal only as the borrowed operand.
                let mut target = operand;
                while let ExprKind::Paren(inner) = &target.kind {
                    target = inner;
                }
                let saved = self.borrowed.replace(target.id);
                let t = self.expr(operand, inner_expected.as_ref());
                self.borrowed = saved;
                let mutable = op == UnaryOp::RefMut;
                if mutable {
                    self.require_mut(operand, true);
                }
                Ty::Ref(mutable, Box::new(t))
            }
        }
    }

    fn binary(&mut self, e: &Expr, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Ty {
        use BinaryOp::*;
        let lt = self.expr(lhs, None);
        // Comparisons see through references (`&u32 == 1`): the right side is
        // checked against the referenced type, not the reference.
        let comparison = matches!(op, Eq | Ne | Lt | Le | Gt | Ge);
        let lz = self.infer.zonk(&lt);
        let rhs_expected = if comparison { peel(&lz).0 } else { lt.clone() };
        let rt = self.expr(rhs, Some(&rhs_expected));
        let (l, r) = (self.infer.shallow(&lt), self.infer.shallow(&rt));
        let numeric = |t: &Ty, cx: &Self| match t {
            Ty::Int(_) | Ty::Float(_) | Ty::Opaque | Ty::Error => true,
            Ty::Var(v) => cx.infer.kind(*v) != VarKind::General,
            _ => false,
        };
        let integer = |t: &Ty, cx: &Self| match t {
            Ty::Int(_) | Ty::Opaque | Ty::Error => true,
            Ty::Var(v) => cx.infer.kind(*v) == VarKind::Int,
            _ => false,
        };
        let fail = |cx: &mut Self, why: &str| {
            let (ls, rs) = (cx.show(&lt), cx.show(&rt));
            cx.err(Diagnostic::error("E3006", "invalid_operands", format!("cannot apply `{}` to `{ls}` and `{rs}`", op.symbol())).primary(e.span, "").note(why.to_string()));
            Ty::Error
        };
        match op {
            Add if matches!(peel(&l).0, Ty::Str) => {
                if matches!(peel(&r).0, Ty::Str | Ty::Opaque | Ty::Error) {
                    Ty::Str
                } else {
                    fail(self, "`+` joins two strings")
                }
            }
            Add | Sub | Mul | Div | Rem => {
                if !numeric(&l, self) || !numeric(&r, self) {
                    return fail(self, "arithmetic needs two numbers of the same type");
                }
                if !self.infer.unify(&l, &r) {
                    return fail(self, "both operands must have the same type; numbers never convert implicitly");
                }
                l
            }
            BitAnd | BitOr | BitXor => {
                if !integer(&l, self) || !self.infer.unify(&l, &r) {
                    return fail(self, "bitwise operators need two integers of the same type");
                }
                l
            }
            Shl | Shr => {
                if !integer(&l, self) || !integer(&r, self) {
                    return fail(self, "shifts need integers");
                }
                l
            }
            And | Or => {
                self.coerce(&lt, &Ty::Bool, lhs.span, Some(lhs.id));
                self.coerce(&rt, &Ty::Bool, rhs.span, Some(rhs.id));
                Ty::Bool
            }
            Eq | Ne | Lt | Le | Gt | Ge => {
                // Either side may be a shared reference: `&T` compares with `T`.
                let (lz, rz) = (self.infer.zonk(&l), self.infer.zonk(&r));
                let (lp, rp) = (peel(&lz).0, peel(&rz).0);
                if !self.infer.unify(&lp, &rp) {
                    return fail(self, "comparison needs two values of the same type");
                }
                let z = self.infer.zonk(&l);
                let (inner, _) = peel(&z);
                let ok = match &inner {
                    Ty::Int(_) | Ty::Float(_) | Ty::Str | Ty::Opaque | Ty::Error => true,
                    // Raw pointers compare by address; nothing is accessed.
                    Ty::Bool | Ty::Ptr(..) => matches!(op, Eq | Ne),
                    Ty::Var(v) => self.infer.kind(*v) != VarKind::General,
                    _ => false,
                };
                if !ok
                    && matches!(op, Eq | Ne)
                    && matches!(inner, Ty::Adt(..) | Ty::Param(_))
                    && let Some(eq) = self.env.decls.eq
                    && self.implements(&inner, eq)
                    && let Some(method) = self.env.r.member(eq, "eq")
                {
                    self.tables.eq_ops.insert(e.id, method);
                    return Ty::Bool;
                }
                if !ok
                    && matches!(op, Eq | Ne)
                    && let Some(eq) = self.env.decls.eq
                    && self.report_unmet(e.span, "`==`", &inner, eq)
                {
                    return Ty::Bool;
                }
                if !ok {
                    return fail(self, "`==`/`!=` need numbers, strings, booleans or a type implementing `Eq`; implement `Eq` or use `match`");
                }
                Ty::Bool
            }
        }
    }

    fn try_expr(&mut self, e: &Expr, inner: &Expr) -> Ty {
        let t = self.expr(inner, None);
        let z = self.infer.zonk(&t);
        let ret = self.infer.zonk(&self.rets.last().cloned().unwrap_or(Ty::Void));
        let (opt, res) = (self.env.prelude.option, self.env.prelude.result);
        match (&z, &ret) {
            (Ty::Opaque | Ty::Error, _) => Ty::Opaque,
            (Ty::Adt(s, a), Ty::Adt(rs, ra)) if *s == res && *rs == res => {
                if !self.infer.unify(&a[1], &ra[1]) {
                    let (got, want) = (self.show(&a[1]), self.show(&ra[1]));
                    self.err(
                        Diagnostic::error("E3017", "try_mismatch", format!("`try` would return an error of type `{got}` from a function whose error type is `{want}`"))
                            .primary(e.span, "")
                            .note("v0 does not convert error types automatically")
                            .help("convert it explicitly with `match`, or change the function's error type"),
                    );
                }
                a[0].clone()
            }
            (Ty::Adt(s, a), Ty::Adt(rs, _)) if *s == opt && *rs == opt => a[0].clone(),
            (Ty::Adt(s, a), _) if *s == res || *s == opt => {
                let (got, want) = (self.show(&z), self.show(&ret));
                self.err(
                    Diagnostic::error("E3017", "try_mismatch", format!("`try` on `{got}` needs the function to return a `{}`", if *s == res { "Result" } else { "Option" }))
                        .primary(e.span, "")
                        .note(format!("this function returns `{want}`")),
                );
                a.first().cloned().unwrap_or(Ty::Error)
            }
            _ => {
                let got = self.show(&z);
                self.err(Diagnostic::error("E3017", "try_mismatch", format!("`try` needs a `Result` or `Option`, found `{got}`")).primary(inner.span, ""));
                Ty::Error
            }
        }
    }

    fn struct_lit(&mut self, e: &Expr, fields: &[FieldInit]) -> Ty {
        let Some(Res::Symbol(s)) = self.res(e.id) else {
            if matches!(self.res(e.id), Some(Res::External { .. })) {
                self.err(
                    Diagnostic::error("E3040", "unmodeled_std_api", "standard-library constructor has no ownership/provenance contract")
                        .primary(e.span, "use a concrete Tarn declaration"),
                );
            }
            for f in fields {
                if let Some(v) = &f.value {
                    self.expr(v, None);
                }
            }
            return Ty::Error;
        };
        let Some(def) = self.env.decls.structs.get(&s) else { return Ty::Error };
        let name = self.env.r.symbol(s).name.clone();
        let map = self.instantiate(&def.generics.clone());
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for f in fields {
            let fd = def.fields.iter().find(|x| x.name == f.name.name);
            let expected = fd.map(|x| subst(&x.ty, &map));
            let vt = match &f.value {
                Some(v) => self.expr(v, expected.as_ref()),
                None => match self.res(f.id) {
                    Some(Res::Symbol(l)) => self.locals.get(&l).cloned().unwrap_or(Ty::Error),
                    _ => Ty::Error,
                },
            };
            let Some(fd) = fd else {
                self.err(Diagnostic::error("E3012", "unknown_field", format!("struct `{name}` has no field `{}`", f.name.name)).primary(f.name.span, ""));
                continue;
            };
            if let Some(prev) = seen.insert(&fd.name, f.name.span) {
                self.err(
                    Diagnostic::error("E3013", "duplicate_field_init", format!("field `{}` is given twice", f.name.name))
                        .primary(f.name.span, "")
                        .secondary(prev, "first given here"),
                );
            }
            if def.module != self.m && !fd.is_pub && !self.internal_visible(def.module) {
                self.err(Diagnostic::error("E3014", "private_field", format!("field `{}` of `{name}` is private", f.name.name)).primary(f.name.span, ""));
            }
            self.coerce(&vt, &expected.unwrap(), f.span, f.value.as_ref().map(|v| v.id));
        }
        let missing: Vec<String> = def.fields.iter().filter(|f| !seen.contains_key(f.name.as_str())).map(|f| format!("`{}`", f.name)).collect();
        if !missing.is_empty() {
            self.err(
                Diagnostic::error("E3011", "missing_fields", format!("missing field{} {} in `{name}`", if missing.len() == 1 { "" } else { "s" }, missing.join(", ")))
                    .primary(e.span, "")
                    .note("every field must be given; there are no default values"),
            );
        }
        let generics = def.generics.clone();
        self.obligations_for(&generics, &map, e.span);
        Ty::Adt(s, generics.iter().map(|p| map[p].clone()).collect())
    }

    fn closure(&mut self, e: &Expr, params: &[ClosureParam], ret: Option<&Type>, body: &Block, expected: Option<&Ty>) -> Ty {
        let exp = expected.map(|t| self.infer.shallow(t));
        let (exp_ps, exp_ret) = match &exp {
            Some(Ty::Fn(_, ps, r)) if ps.len() == params.len() => (Some(ps.clone()), Some((**r).clone())),
            Some(Ty::Opaque) => (Some(vec![Ty::Opaque; params.len()]), Some(Ty::Opaque)),
            _ => (None, None),
        };
        let mut pts = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let t = match (&p.ty, &exp_ps) {
                (Some(t), _) => self.env_lower(t),
                (None, Some(ps)) => ps[i].clone(),
                (None, None) => {
                    self.err(
                        Diagnostic::error("E3010", "cannot_infer", format!("cannot infer the type of closure parameter `{}`", p.name.name))
                            .primary(p.name.span, "")
                            .help(format!("annotate it: `fn({} T) ...`", p.name.name)),
                    );
                    Ty::Error
                }
            };
            if let Some(s) = self.def(p.id) {
                self.locals.insert(s, t.clone());
            }
            pts.push(t);
        }
        let rt = match (ret, exp_ret) {
            (Some(t), _) => self.env_lower(t),
            (None, Some(r)) => r,
            (None, None) => Ty::Void,
        };
        self.rets.push(rt.clone());
        let parent_async_context = self.async_context;
        self.async_context = false;
        let parent_task_depth = self.task_scope_depth;
        self.task_scope_depth = 0;
        self.stmts(&body.stmts);
        self.task_scope_depth = parent_task_depth;
        self.async_context = parent_async_context;
        self.rets.pop();
        if !matches!(self.infer.shallow(&rt), Ty::Void | Ty::Never | Ty::Opaque) && !self.diverges(&body.stmts) {
            let shown = self.show(&rt);
            self.err(
                Diagnostic::error("E3009", "missing_return", format!("closure may end without returning `{shown}`"))
                    .primary(Span::new(body.span.file, body.span.end - 1, body.span.end), ""),
            );
        }
        let mutated = crate::captures::mutated_symbols(body, &self.env.r.tables[self.m.0 as usize].uses, &self.tables);
        let captured = self.env.r.tables[self.m.0 as usize].captures.get(&e.id);
        let captures: HashMap<SymbolId, Ty> = captured.map(|c| c.symbols.iter().map(|s| {
            let ty = self.infer.zonk(&self.locals.get(s).cloned().unwrap_or(Ty::Error));
            let ty = match ty { Ty::Var(v) if self.infer.kind(v) == VarKind::Int => Ty::Int(IntTy::I64), Ty::Var(v) if self.infer.kind(v) == VarKind::Float => Ty::Float(FloatTy::F64), ty => ty };
            (*s, ty)
        }).collect()).unwrap_or_default();
        // Resolve only the expressions the body analysis asks about; copying
        // and resolving the whole function's tables per closure was quadratic.
        let (infer, tables) = (&self.infer, &self.tables);
        let expr_ty = |id: NodeId| {
            tables.expr_types.get(&id).map(|ty| match infer.zonk(ty) {
                Ty::Var(v) if infer.kind(v) == VarKind::Int => Ty::Int(IntTy::I64),
                Ty::Var(v) if infer.kind(v) == VarKind::Float => Ty::Float(FloatTy::F64),
                ty => ty,
            })
        };
        let consuming = !crate::captures::consumed_symbols(body, &self.env.r.tables[self.m.0 as usize].uses, &self.tables, &expr_ty, &self.env.decls, &captures).is_empty();
        let mutable = captured.is_some_and(|c| c.symbols.iter().any(|s| mutated.contains(s)));
        let owned = matches!(e.kind, ExprKind::Closure { owned: true, .. });
        let modes = captured.map(|c| c.symbols.iter().map(|s| (*s, if owned { crate::CaptureMode::Move } else if mutated.contains(s) { crate::CaptureMode::MutableBorrow } else { crate::CaptureMode::SharedBorrow })).collect()).unwrap_or_default();
        self.tables.closure_captures.insert(e.id, modes);
        self.tables.mutable_captures.insert(e.id, mutated);
        if matches!(e.kind, ExprKind::Closure { owned: true, .. }) {
            let fields = captured.map(|c| c.symbols.iter().map(|s| (*s, self.infer.zonk(&self.locals.get(s).cloned().unwrap_or(Ty::Error)))).collect()).unwrap_or_default();
            self.tables.owned_captures.insert(e.id, fields);
        }
        // A closure written where `mut fn` is expected may be invoked
        // exclusively even if it only reads its captures: exclusive access is
        // strictly stronger, and capture modes stay shared. Once is not adopted
        // because consuming invocation also changes environment release.
        let wants_mutable = matches!(exp, Some(Ty::Fn(CallMode::Mutable, ..)));
        // A borrowed (non-`move`) environment lives on the stack or is null, so
        // consuming invocation releases nothing and may also be adopted. An
        // owned non-consuming environment would stay allocated: still rejected.
        let wants_once = matches!(exp, Some(Ty::Fn(CallMode::Once, ..))) && !owned;
        Ty::Fn(
            if consuming || wants_once {
                CallMode::Once
            } else if mutable || wants_mutable {
                CallMode::Mutable
            } else {
                CallMode::Shared
            },
            pts,
            Box::new(rt),
        )
    }

    // ------------------------------------------------------------ calls

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Expr], expected: Option<&Ty>) -> Ty {
        let target = self.res(callee.id);
        match target {
            Some(Res::External { .. }) => {
                self.err(
                    Diagnostic::error("E3040", "unmodeled_std_api", "standard-library API has no ownership/provenance contract")
                        .primary(callee.span, "cannot safely type-check this API")
                        .help("use a module with explicit Tarn declarations"),
                );
                self.record(callee, Ty::Error);
                for a in args {
                    self.expr(a, None);
                }
                Ty::Error
            }
            Some(Res::Symbol(s)) => match self.kind(s).clone() {
                SymbolKind::Builtin => self.builtin(e, s, args),
                SymbolKind::Primitive => self.conversion(e, s, args),
                SymbolKind::Variant { parent } => {
                    let (fields, generics) = self.variant_info(parent, s);
                    let mut map = self.instantiate(&generics);
                    let result = Ty::Adt(parent, generics.iter().map(|p| map[p].clone()).collect());
                    if let Some(x) = expected {
                        self.infer.unify(&result, x);
                    }
                    for (k, v) in map.iter_mut() {
                        let _ = k;
                        *v = self.infer.shallow(v);
                    }
                    let ps: Vec<Ty> = fields.iter().map(|f| subst(f, &map)).collect();
                    let name = self.env.r.symbol(s).name.clone();
                    self.args(e, &format!("variant `{name}`"), &ps, args);
                    self.obligations_for(&generics, &map, e.span);
                    result
                }
                SymbolKind::Function | SymbolKind::Method { .. } | SymbolKind::ImplMethod { .. } => {
                    let Some(sig) = self.env.decls.fns.get(&s).cloned() else { return Ty::Error };
                    self.sig_call(e, s, &sig, None, args, expected)
                }
                SymbolKind::Local { .. } | SymbolKind::Param | SymbolKind::PatternBinding | SymbolKind::LoopBinding | SymbolKind::ClosureParam | SymbolKind::SelfParam => {
                    self.value_call(e, callee, args)
                }
                k => {
                    let name = self.env.r.symbol(s).name.clone();
                    let what = crate::binding_kind(&k);
                    let mut d = Diagnostic::error("E3003", "not_callable", format!("{what} `{name}` cannot be called")).primary(callee.span, "");
                    if matches!(k, SymbolKind::Struct) {
                        d = d.help(format!("build a value with `{name}{{field: value}}`, or call an associated function like `{name}.new(...)`"));
                    }
                    self.err(d);
                    for a in args {
                        self.expr(a, None);
                    }
                    Ty::Error
                }
            },
            Some(Res::ScrutineeVariant(_)) => Ty::Error,
            None => match &callee.kind {
                ExprKind::Field { base, name } => {
                    let bt = self.expr(base, None);
                    let (inner, _) = peel(&self.infer.shallow(&bt));
                    let field = if let Ty::Adt(s, _) = inner { self.env.decls.structs.get(&s).is_some_and(|d| d.fields.iter().any(|f| f.name == name.name)) } else { false };
                    // Reuse the base type: checking `base` again would double the
                    // work at every link of a call chain (exponential time).
                    if field {
                        let ct = self.field_of(callee, bt, name);
                        let ct = self.record(callee, ct);
                        self.value_call_typed(e, callee, ct, args)
                    } else {
                        self.method_call(e, base, bt, name, args)
                    }
                },
                _ => self.value_call(e, callee, args),
            },
        }
    }

    fn callable_path_access(&self, e: &Expr) -> Option<bool> {
        let base = match &e.kind { ExprKind::Field { base, .. } | ExprKind::Index { base, .. } | ExprKind::Paren(base) => base, _ => return None };
        let direct = self.tables.expr_types.get(&base.id).and_then(|t| peel(&self.infer.shallow(t)).1);
        match (direct, self.callable_path_access(base)) {
            (Some(a), Some(b)) => Some(a && b),
            (a, b) => a.or(b),
        }
    }

    fn value_call(&mut self, e: &Expr, callee: &Expr, args: &[Expr]) -> Ty {
        let ct = self.expr(callee, None);
        self.value_call_typed(e, callee, ct, args)
    }

    /// Call through a value whose type `ct` was already checked and recorded.
    fn value_call_typed(&mut self, e: &Expr, callee: &Expr, ct: Ty, args: &[Expr]) -> Ty {
        self.tables.callable_calls.insert(e.id);
        let (inner, through) = peel(&self.infer.shallow(&ct));
        match inner {
            Ty::Fn(mode, ps, r) => {
                let through = through.or(self.callable_path_access(callee));
                if (mode == CallMode::Mutable && through == Some(false)) || (mode == CallMode::Once && through.is_some()) {
                    self.err(
                        Diagnostic::error("E3044", "callable_access", "callable cannot be invoked through this reference").primary(callee.span, "invocation requires exclusive access or ownership"),
                    );
                }
                self.args(e, "this function", &ps, args);
                *r
            }
            Ty::Opaque | Ty::Error => {
                for a in args {
                    self.expr(a, Some(&Ty::Opaque));
                }
                Ty::Opaque
            }
            other => {
                let shown = self.show(&other);
                self.err(Diagnostic::error("E3003", "not_callable", format!("a value of type `{shown}` cannot be called")).primary(callee.span, ""));
                Ty::Error
            }
        }
    }

    fn args(&mut self, e: &Expr, what: &str, params: &[Ty], args: &[Expr]) {
        if params.len() != args.len() {
            self.err(
                Diagnostic::error(
                    "E3002",
                    "wrong_arg_count",
                    format!(
                        "{what} takes {} argument{}, but {} {} given",
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .primary(e.span, ""),
            );
        }
        for (i, a) in args.iter().enumerate() {
            match params.get(i) {
                Some(p) => {
                    let at = self.expr(a, Some(p));
                    self.coerce(&at, p, a.span, Some(a.id));
                }
                None => {
                    self.expr(a, None);
                }
            }
        }
    }

    /// Call through a signature. `recv` = already-typed receiver for method
    /// call syntax; for static calls (`T.m(x)`) the receiver is the first arg.
    fn sig_call(&mut self, e: &Expr, s: SymbolId, sig: &FnSig, recv: Option<(&Expr, Ty, HashMap<ParamId, Ty>)>, args: &[Expr], expected: Option<&Ty>) -> Ty {
        let name = self.env.r.symbol(s).name.clone();
        // Intrinsics are safe; foreign functions are not.
        if sig.abi.as_deref().is_some_and(|a| a != "intrinsic") && self.unsafe_depth == 0 {
            self.err(
                Diagnostic::error("E3031", "extern_call_outside_unsafe", format!("calling the extern function `{name}` requires `unsafe`"))
                    .primary(e.span, "")
                    .help("wrap the call in `unsafe { ... }` and document why it is sound"),
            );
        } else if sig.is_unsafe && self.unsafe_depth == 0 {
            self.err(
                Diagnostic::error("E3071", "unsafe_call_outside_unsafe", format!("calling the unsafe function `{name}` requires `unsafe`"))
                    .primary(e.span, "")
                    .help("wrap the call in `unsafe { ... }` after checking the function's documented requirements"),
            );
        }
        let mut map = self.instantiate(&sig.generics);
        let has_recv = recv.is_some();
        if let Some((_, _, fixed)) = &recv {
            map.extend(fixed.clone());
        }
        // Propagate the expected result type into the generics first, so
        // arguments like `None` or closures see concrete types.
        if let Some(x) = expected {
            let output = subst(&sig.ret, &map);
            let r = if sig.is_async { Ty::Async(Box::new(output)) } else { output };
            self.infer.unify(&r, x);
        }
        let mut ps: Vec<Ty> = Vec::new();
        if !has_recv && let (Some(r), Some(st)) = (sig.receiver, &sig.self_ty) {
            ps.push(receiver_ty(r, st));
        }
        ps.extend(sig.params.iter().cloned());
        let source_parameters = ps.clone();
        let ps: Vec<Ty> = ps.iter().map(|p| subst(p, &map)).collect();
        if !sig.generics.is_empty() {
            self.tables.type_args.insert(e.id, sig.generics.iter().map(|p| map[p].clone()).collect());
        }
        let what = if has_recv { format!("method `{name}`") } else { format!("`{name}`") };
        self.args(e, &what, &ps, args);
        if Some(s) == self.env.decls.ffi_callback && let [argument] = args {
            let mut argument = argument;
            while let ExprKind::Paren(inner) = &argument.kind { argument = inner; }
            let valid = match self.res(argument.id) {
                Some(Res::Symbol(id)) => self.env.decls.fns.get(&id).is_some_and(|f| {
                    f.abi.is_none() && !f.is_async && !f.is_unsafe && f.generics.is_empty() && f.receiver.is_none()
                }),
                _ => false,
            };
            if !valid {
                self.err(Diagnostic::error("E3081", "invalid_c_callback", "C callback must directly name an ordinary non-generic Tarn function")
                    .primary(argument.span, "closures, callable locals, async, unsafe and foreign functions are not supported")
                    .help("use a named function with scalar/pointer parameters and pass context explicitly through a raw pointer"));
            }
        }
        let obligation_start = self.obligations.len();
        self.obligations_for(&sig.generics, &map, e.span);
        let mut evidence_updates = Vec::new();
        for index in obligation_start..self.obligations.len() {
            let (ty, iface, _, _) = &self.obligations[index];
            let cap = if Some(*iface) == self.env.decls.transfer { crate::Capability::Transfer }
                else if Some(*iface) == self.env.decls.share { crate::Capability::Share } else { continue };
            if !matches!(self.infer.zonk(ty), Ty::Fn(..)) { continue; }
            let Some(parameter) = sig.generics.iter().find(|p| map[*p] == *ty) else { continue };
            let mut components = Vec::new();
            let mut found = false;
            let mut complete = true;
            for (declared, argument) in source_parameters.iter().zip(args) {
                let source = match declared {
                    Ty::Param(p) if p == parameter => Some(argument),
                    Ty::Ref(_, t) if **t == Ty::Param(*parameter) => match &argument.kind {
                        ExprKind::Unary { op: UnaryOp::Ref | UnaryOp::RefMut, operand } => Some(operand.as_ref()),
                        _ => None,
                    },
                    _ => continue,
                };
                found = true;
                let evidence = source.and_then(|a| self.callable_capability_evidence(a, cap));
                if let Some(fields) = evidence { components.extend(fields); } else { complete = false; }
                if components.len() > 4096 { complete = false; break; }
            }
            if found && complete { evidence_updates.push((index, components)); }
        }
        for (index, evidence) in evidence_updates { self.obligations[index].3 = Some(evidence); }
        let output = subst(&sig.ret, &map);
        if sig.is_async { Ty::Async(Box::new(output)) } else { output }
    }

    /// `bt` is the already-checked type of `base`; `base` is not checked again.
    fn method_call(&mut self, e: &Expr, base: &Expr, bt: Ty, name: &Ident, args: &[Expr]) -> Ty {
        let z = self.infer.zonk(&bt);
        let (inner, through) = peel(&z);
        // Find the method.
        let found: Option<(SymbolId, FnSig, HashMap<ParamId, Ty>)> = match &inner {
            Ty::Opaque | Ty::Error => {
                self.tables.method_calls.insert(e.id, MethodTarget::Opaque);
                for a in args {
                    self.expr(a, Some(&Ty::Opaque));
                }
                return Ty::Opaque;
            }
            Ty::Adt(s, targs) => {
                if *s == self.env.prelude.channel || self.env.r.symbol(*s).name == "Sender" && matches!(self.kind(*s), SymbolKind::PreludeType) {
                    // No runtime contract exists yet; never erase result provenance.
                    self.err(
                        Diagnostic::error("E3040", "unmodeled_std_api", "channel API has no ownership/provenance contract")
                            .primary(e.span, "concurrency semantics are not implemented"),
                    );
                    self.tables.method_calls.insert(e.id, MethodTarget::Opaque);
                    for a in args {
                        self.expr(a, Some(&Ty::Opaque));
                    }
                    return Ty::Opaque;
                }
                self.lookup_method(*s, targs, &name.name)
            }
            Ty::Param(p) => self.interface_method(self.env.decls.bounds.get(p).cloned().unwrap_or_default(), &name.name, &inner),
            Ty::Any(i) => {
                let found = self.interface_method(vec![*i], &name.name, &inner);
                // `Self` beyond the receiver needs the concrete type, which a
                // dynamic object does not have (Phase 26C).
                if let Some((_, sig, _)) = &found
                    && let Some(this) = self.env.r.interface_self.get(i)
                    && sig.params.iter().chain(std::iter::once(&sig.ret)).any(|t| mentions(t, ParamId(this.0)))
                {
                    let iname = self.env.r.symbol(*i).name.clone();
                    self.err(
                        Diagnostic::error("E3073", "self_through_dynamic_interface", format!("method `{}` of `{iname}` uses `Self`, which `any {iname}` cannot provide", name.name))
                            .primary(name.span, "")
                            .help(format!("use a generic parameter instead: `<T: {iname}>`")),
                    );
                    return Ty::Error;
                }
                found
            }
            // Methods of primitive types are declared in `core` (ADR 0020).
            Ty::Str | Ty::Int(_) | Ty::Float(_) | Ty::Bool => {
                let pname = crate::display_vars(&inner, self.env, &self.infer);
                match self.env.r.scope(tarn_resolve::ScopeId(0)).get(&pname) {
                    Some(prim) => self.lookup_method(prim, &[], &name.name),
                    None => None,
                }
            }
            _ => None,
        };
        let Some((msym, sig, fixed)) = found else {
            if let Some(t) = self.intrinsic(e, &inner, name, args) {
                return t;
            }
            let shown = self.show(&inner);
            let mut d = Diagnostic::error("E3005", "no_method", format!("`{shown}` has no method `{}`", name.name)).primary(name.span, "");
            if matches!(inner, Ty::Str | Ty::Int(_) | Ty::Float(_)) {
                d = d.note("the standard library is not available yet; v0 only has a few built-in methods (see docs/types.md)");
            } else if let Ty::Param(_) = inner {
                d = d.help("add an interface bound that declares this method: `<T: Interface>`");
            }
            self.err(d);
            for a in args {
                self.expr(a, None);
            }
            return Ty::Error;
        };
        self.tables.method_calls.insert(e.id, MethodTarget::Symbol(msym));
        if let Some(kind) = sig.receiver {
            let mut derefs = 0u8;
            let mut t = z.clone();
            while let Ty::Ref(_, inner) = t {
                derefs += 1;
                t = *inner;
            }
            self.tables.receivers.insert(e.id, Receiver { derefs, kind });
        }
        let Some(rk) = sig.receiver else {
            let tn = self.env.r.symbol(msym).name.clone();
            self.err(
                Diagnostic::error("E3005", "no_method", format!("`{tn}` is an associated function, not a method"))
                    .primary(name.span, "")
                    .help(format!("call it on the type: `{}.{tn}(...)`", self.show(&inner))),
            );
            return Ty::Error;
        };
        // Receiver adjustment: auto-borrow places, auto-deref references.
        match rk {
            ReceiverKind::Ref => {}
            ReceiverKind::RefMut => match through {
                Some(true) => {}
                Some(false) => {
                    let shown = self.show(&z);
                    self.err(
                        Diagnostic::error("E3016", "borrow_mut_immutable", format!("`{}` needs `&mut self`, but the receiver is `{shown}`", name.name))
                            .primary(base.span, "this is a `&` reference")
                            .help("take a mutable reference (`&mut`) instead"),
                    );
                }
                None => {
                    self.require_mut(base, true);
                }
            },
            ReceiverKind::Value => {
                if through.is_some() && !self.is_copy(&inner) {
                    let shown = self.show(&inner);
                    self.err(
                        Diagnostic::error("E3030", "move_out_of_reference", format!("`{}` takes `self` by value, but only a reference to `{shown}` is available", name.name))
                            .primary(base.span, "")
                            .note(format!("`{shown}` is not a copy type, so it cannot be moved out of a reference")),
                    );
                }
            }
        }
        self.sig_call(e, msym, &sig, Some((base, bt, fixed)), args, None)
    }

    /// Inherent methods, then impl methods; binds the owner's binders to the
    /// receiver's type arguments.
    fn lookup_method(&self, s: SymbolId, targs: &[Ty], name: &str) -> Option<(SymbolId, FnSig, HashMap<ParamId, Ty>)> {
        let mut cands: Vec<SymbolId> = Vec::new();
        if let Some(m) = self.env.r.member(s, name)
            && matches!(self.kind(m), SymbolKind::Method { .. })
        {
            cands.push(m);
        }
        if cands.is_empty() {
            for i in &self.env.r.impls {
                if i.target == Some(s) {
                    cands.extend(i.methods.iter().copied().filter(|&m| self.env.r.symbol(m).name == name));
                }
            }
        }
        let m = *cands.first()?;
        let sig = self.env.decls.fns.get(&m)?.clone();
        let fixed: HashMap<ParamId, Ty> = sig.generics.iter().copied().zip(targs.iter().cloned()).collect();
        Some((m, sig, fixed))
    }

    /// A method declared by one of `ifaces`; its `Self` is the receiver type.
    fn interface_method(&self, ifaces: Vec<SymbolId>, name: &str, receiver: &Ty) -> Option<(SymbolId, FnSig, HashMap<ParamId, Ty>)> {
        for i in ifaces {
            if let Some(m) = self.env.r.member(i, name)
                && let Some(sig) = self.env.decls.fns.get(&m)
            {
                let fixed = self.env.r.interface_self.get(&i).map(|this| HashMap::from([(ParamId(this.0), receiver.clone())])).unwrap_or_default();
                return Some((m, sig.clone(), fixed));
            }
        }
        None
    }

    /// The last methods still known only to the compiler: arrays and slices
    /// cannot be named as method owners in `core` yet (ADR 0020, open item).
    /// Do not add entries here; declare them in `core` instead.
    fn intrinsic(&mut self, e: &Expr, recv: &Ty, name: &Ident, args: &[Expr]) -> Option<Ty> {
        let ret = match (recv, name.name.as_str()) {
            (Ty::Array(..) | Ty::Slice(_), "len") => Ty::Int(IntTy::Usize),
            (Ty::Array(..) | Ty::Slice(_), "is_empty") => Ty::Bool,
            _ => return None,
        };
        self.tables.method_calls.insert(e.id, MethodTarget::Intrinsic(name.name.clone()));
        let mut derefs = 0u8;
        let mut t = self.infer.zonk(
            &self
                .tables
                .expr_types
                .get(&match &e.kind {
                    ExprKind::Call { callee, .. } => match &callee.kind {
                        ExprKind::Field { base, .. } => base.id,
                        _ => callee.id,
                    },
                    _ => e.id,
                })
                .cloned()
                .unwrap_or(Ty::Error),
        );
        while let Ty::Ref(_, inner) = t {
            derefs += 1;
            t = *inner;
        }
        self.tables.receivers.insert(e.id, Receiver { derefs, kind: ReceiverKind::Ref });
        self.args(e, &format!("method `{}`", name.name), &[], args);
        Some(ret)
    }

    fn builtin(&mut self, e: &Expr, s: SymbolId, args: &[Expr]) -> Ty {
        let p = &self.env.prelude;
        if s == p.print {
            self.tables.borrowed_builtin_calls.insert(e.id);
            if args.len() != 1 {
                self.args(e, "`print`", &[Ty::Opaque], args);
            } else {
                let t = self.expr(&args[0], None);
                self.printables.push((t, args[0].span));
            }
            Ty::Void
        } else if s == p.panic {
            self.args(e, "`panic`", &[Ty::Str], args);
            Ty::Never
        } else if s == p.channel_fn {
            self.err(
                Diagnostic::error("E3040", "unmodeled_std_api", "channel API has no ownership/provenance contract").primary(e.span, "concurrency semantics are not implemented"),
            );
            let cap = self.infer.fresh(VarKind::Int);
            self.args(e, "`channel`", &[cap], args);
            let elem = self.infer.fresh(VarKind::General);
            Ty::Adt(p.channel, vec![elem])
        } else {
            Ty::Error
        }
    }

    /// `u64(x)`: checked numeric conversion.
    fn conversion(&mut self, e: &Expr, s: SymbolId, args: &[Expr]) -> Ty {
        let target = crate::env::primitive(&self.env.r.symbol(s).name);
        if !matches!(target, Ty::Int(_) | Ty::Float(_)) {
            let name = self.env.r.symbol(s).name.clone();
            self.err(Diagnostic::error("E3029", "invalid_conversion", format!("`{name}` is not a conversion function")).primary(e.span, ""));
            return Ty::Error;
        }
        if args.len() != 1 {
            self.args(e, "a conversion", &[Ty::Opaque], args);
            return target;
        }
        let t = self.expr(&args[0], None);
        let ok = match self.infer.shallow(&t) {
            Ty::Int(_) | Ty::Float(_) | Ty::Opaque | Ty::Error => true,
            Ty::Var(v) => self.infer.kind(v) != VarKind::General,
            _ => false,
        };
        if !ok {
            let shown = self.show(&t);
            self.err(
                Diagnostic::error("E3029", "invalid_conversion", format!("cannot convert `{shown}` to `{}`", self.show(&target)))
                    .primary(args[0].span, "")
                    .note("conversions are between numeric types"),
            );
        }
        target
    }
}

pub fn receiver_ty(r: ReceiverKind, st: &Ty) -> Ty {
    match r {
        ReceiverKind::Value => st.clone(),
        ReceiverKind::Ref => Ty::Ref(false, Box::new(st.clone())),
        ReceiverKind::RefMut => Ty::Ref(true, Box::new(st.clone())),
    }
}

fn has_break(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Break => true,
        StmtKind::If(i) => if_has_break(i),
        StmtKind::Match(m) => m.arms.iter().any(|a| has_break(std::slice::from_ref(&a.body))),
        StmtKind::Block(b) | StmtKind::Unsafe(b) | StmtKind::Scope(b) => has_break(&b.stmts),
        StmtKind::Expr(e) | StmtKind::Spawn(e) | StmtKind::Return(Some(e)) | StmtKind::Let { value: Some(e), .. } => expr_has_break(e),
        StmtKind::Assign { target, value } => expr_has_break(target) || expr_has_break(value),
        // `break` inside a nested loop belongs to that loop.
        _ => false,
    })
}

/// A `break` of the enclosing loop inside an arm of a `match` expression.
fn expr_has_break(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Match(m) => expr_has_break(&m.scrutinee) || m.arms.iter().any(|a| a.guard.as_ref().is_some_and(expr_has_break) || has_break(std::slice::from_ref(&a.body))),
        ExprKind::Paren(x) | ExprKind::Try(x) | ExprKind::Await(x) | ExprKind::Field { base: x, .. } | ExprKind::Unary { operand: x, .. } => expr_has_break(x),
        ExprKind::Call { callee, args } => expr_has_break(callee) || args.iter().any(expr_has_break),
        ExprKind::Index { base, index } => expr_has_break(base) || expr_has_break(index),
        ExprKind::Binary { lhs, rhs, .. } => expr_has_break(lhs) || expr_has_break(rhs),
        ExprKind::Range { start, end, .. } => start.as_deref().is_some_and(expr_has_break) || end.as_deref().is_some_and(expr_has_break),
        ExprKind::StructLit { fields, .. } => fields.iter().filter_map(|f| f.value.as_ref()).any(expr_has_break),
        ExprKind::ArrayLit { elems, .. } => elems.iter().any(expr_has_break),
        // A closure body cannot leave the loop around it.
        _ => false,
    }
}

fn if_has_break(i: &IfStmt) -> bool {
    has_break(&i.then_block.stmts)
        || match i.else_branch.as_deref() {
            Some(ElseBranch::If(e)) => if_has_break(e),
            Some(ElseBranch::Block(b)) => has_break(&b.stmts),
            None => false,
        }
}

/// Whether `t` mentions the type parameter `p` anywhere.
fn mentions(t: &Ty, p: ParamId) -> bool {
    match t {
        Ty::Param(q) => *q == p,
        Ty::Ref(_, x) | Ty::Ptr(_, x) | Ty::Array(x, _) | Ty::Slice(x) | Ty::Async(x) => mentions(x, p),
        Ty::Adt(_, args) => args.iter().any(|a| mentions(a, p)),
        Ty::Fn(_, ps, r) => ps.iter().any(|a| mentions(a, p)) || mentions(r, p),
        _ => false,
    }
}
