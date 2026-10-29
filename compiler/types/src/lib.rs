//! Type checking for Tarn (phase 4).
//!
//! Input: parsed modules and the resolver's output. Output: side tables keyed
//! by `NodeId` / `SymbolId` (expression types, local types, method targets,
//! pattern variants) — the AST stays untouched. Rules: `docs/types.md`.

mod captures;
mod capabilities;
pub use capabilities::{Capability, NativeCapabilities};
mod check;
mod env;
mod exhaust;
mod ty;

pub use check::subst;
pub use env::{Decls, EnumDef, Env, FieldDef, FnSig, PassingMode, Prelude, ResultContract, SemanticContract, StructDef, TASK_INTRINSICS, VariantDef};
pub use tarn_ast::{CallMode, ReceiverKind};

/// The native operation named by a private trusted-stdlib intrinsic call,
/// `<layer>._<operation>` (ADR 0041), e.g. `net._read` -> `read`.
pub fn stdlib_intrinsic_operation(name: &str) -> Option<&str> {
    let (module, operation) = name.split_once("._")?;
    // FFI memory intrinsics have a generic scalar ABI, verified separately
    // from the fixed native I/O bridges (ADR 0080).
    (module != "ffi" && tarn_resolve::STDLIB_LAYERS.contains(&module)).then_some(operation)
}
pub use ty::{FloatTy, IntTy, ParamId, Ty};

use std::collections::HashMap;
use tarn_ast::{ItemKind, NodeId};
use tarn_diagnostics::{Diagnostic, SourceMap};
use tarn_resolve::{ModuleId, ModuleInput, Resolved, SymbolId, SymbolKind};

#[derive(Clone, Debug, PartialEq)]
pub enum MethodTarget {
    Symbol(SymbolId),
    /// Provisional built-in method (`len`, `sqrt`, ...).
    Intrinsic(String),
    /// Method of an opaque std value; unchecked.
    Opaque,
}

/// An implicit conversion applied by the checker at an expected-type site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coercion {
    /// `&mut T` used as `&T`.
    pub mut_to_shared: bool,
    pub kind: CoercionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoercionKind {
    None,
    /// `&[N]T → &[]T`.
    Unsize,
    /// `&T → &any I`.
    ToDyn(SymbolId),
    /// `async computation<T>` → trusted manual poller `mut fn(&Waker) Progress<T>`
    /// (ADR 0037). Same owned representation; never the reverse direction.
    Poller,
    /// A string literal used where `&string` is expected: borrow a temporary,
    /// as for the written form `&"..."` (Phase 26B).
    BorrowLiteral,
}

/// How a binding introduced by a pattern or `for` gets its value (ADR 0018).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingMode {
    Move,
    Copy,
    /// Borrow (`true` = mutable) of the matched place, reached through a reference.
    Ref(bool),
}

/// Receiver adjustment of a method call: dereference `derefs` times, then
/// borrow (`&self`, `&mut self`) or copy/move (`self`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receiver {
    pub derefs: u8,
    pub kind: tarn_ast::ReceiverKind,
}

/// Every semantic decision the checker makes, keyed by node, so that later
/// phases never re-derive them (ADR 0023).
#[derive(Default, Debug)]
pub struct TypeTables {
    /// Spawn expressions authorized by a lexical completion boundary.
    pub scoped_spawns: std::collections::HashSet<NodeId>,
    pub expr_types: HashMap<NodeId, Ty>,
    /// Calls whose argument is observed rather than consumed.
    pub borrowed_builtin_calls: std::collections::HashSet<NodeId>,
    /// Declaration-ordered inferred capture ownership.
    pub closure_captures: HashMap<NodeId, Vec<(SymbolId, CaptureMode)>>,
    /// Calls through values, including callable struct fields.
    pub callable_calls: std::collections::HashSet<NodeId>,
    /// Owned environment fields for nested capture-use inference.
    pub owned_captures: HashMap<NodeId, Vec<(SymbolId, Ty)>>,
    /// Captures requiring exclusive body access, even in owned environments.
    pub mutable_captures: HashMap<NodeId, std::collections::HashSet<SymbolId>>,
    /// Method-call expression → method.
    pub method_calls: HashMap<NodeId, MethodTarget>,
    /// `==`/`!=` expression → the `Eq.eq` method it calls (Phase 26C).
    pub eq_ops: HashMap<NodeId, SymbolId>,
    /// Interpolated value in `f"..."` → the `Display.display` method it calls (ADR 0077).
    pub interpolations: HashMap<NodeId, SymbolId>,
    /// Method-call expression → receiver adjustment.
    pub receivers: HashMap<NodeId, Receiver>,
    /// Pattern node → enum variant it matches (resolves `ScrutineeVariant`).
    pub pattern_variants: HashMap<NodeId, SymbolId>,
    /// Expression → coercion applied to its value.
    pub coercions: HashMap<NodeId, Coercion>,
    /// Call (or generic function used as a value) → type arguments, in the
    /// order of the callee's generic parameters.
    pub type_args: HashMap<NodeId, Vec<Ty>>,
    /// Binding node (pattern, field shorthand, `for` statement) → mode.
    pub binding_modes: HashMap<NodeId, BindingMode>,
}

/// Inferred ownership of a captured binding; its invocation access is encoded
/// separately in the callable type and mutable-capture table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode { SharedBorrow, MutableBorrow, Move }

pub struct Typed {
    pub tables: Vec<TypeTables>,
    /// Types of locals, parameters and bindings.
    pub locals: HashMap<SymbolId, Ty>,
    /// Struct/enum layouts, signatures, bounds — for later phases.
    pub decls: Decls,
    pub prelude: env::Prelude,
}

impl Typed {
    pub fn display(&self, t: &Ty, r: &Resolved) -> String {
        show(t, r, &self.decls.param_names, &|_| "_")
    }
}

/// Type-check a resolved program.
pub fn check(inputs: &[ModuleInput], r: &Resolved) -> (Typed, Vec<Diagnostic>) {
    let mut env = Env::new(inputs, r);
    let mut diags = std::mem::take(&mut env.diags);
    validate_copy(&env, &mut diags);
    validate_no_ref_fields(&env, &mut diags);
    validate_impls(&env, &mut diags);
    let mut tables: Vec<TypeTables> = (0..inputs.len()).map(|_| TypeTables::default()).collect();
    let mut locals = HashMap::new();
    for (mi, input) in inputs.iter().enumerate() {
        let m = ModuleId(mi as u32);
        let mut fns = Vec::new();
        for item in &input.ast.items {
            match &item.kind {
                ItemKind::Fn(f) => fns.push(f),
                ItemKind::Impl(i) => fns.extend(i.methods.iter()),
                _ => {}
            }
        }
        for f in fns {
            let Some(sym) = r.tables[mi].defs.get(&f.id) else { continue };
            let Some(sig) = env.decls.fns.get(sym).cloned() else { continue };
            let mut cx = check::FnCx::new(&env, m);
            cx.function(f, &sig);
            cx.finish();
            diags.append(&mut cx.diags);
            locals.extend(cx.locals);
            let t = &mut tables[mi];
            t.expr_types.extend(cx.tables.expr_types);
            t.scoped_spawns.extend(cx.tables.scoped_spawns);
            t.mutable_captures.extend(cx.tables.mutable_captures);
            t.owned_captures.extend(cx.tables.owned_captures);
            t.callable_calls.extend(cx.tables.callable_calls);
            t.borrowed_builtin_calls.extend(cx.tables.borrowed_builtin_calls);
            t.closure_captures.extend(cx.tables.closure_captures);
            t.method_calls.extend(cx.tables.method_calls);
            t.eq_ops.extend(cx.tables.eq_ops);
            t.interpolations.extend(cx.tables.interpolations);
            t.pattern_variants.extend(cx.tables.pattern_variants);
            t.receivers.extend(cx.tables.receivers);
            t.coercions.extend(cx.tables.coercions);
            t.type_args.extend(cx.tables.type_args);
            t.binding_modes.extend(cx.tables.binding_modes);
        }
    }
    let Env { decls, prelude, .. } = env;
    (Typed { tables, locals, decls, prelude }, diags)
}

/// `copy struct` / `copy enum` may only contain copy types (E3024).
fn validate_copy(env: &Env, diags: &mut Vec<Diagnostic>) {
    let cx = check::FnCx::new(env, ModuleId(0));
    let mut check = |sym: SymbolId, tys: Vec<&Ty>| {
        for t in tys {
            if !cx.is_copy(t) {
                let s = env.r.symbol(sym);
                let shown = display(t, env);
                let mut d = Diagnostic::error("E3024", "copy_with_non_copy", format!("`copy` type `{}` contains `{shown}`, which is not copy", s.name))
                    .primary(s.span.unwrap(), "")
                    .help("remove `copy`, or use only copy types (numbers, bool, `&T`, copy structs and enums)");
                if matches!(t, Ty::Param(_)) {
                    d = d.note("type parameters are never copy in v0");
                }
                diags.push(d);
                return;
            }
        }
    };
    let mut syms: Vec<_> = env.decls.structs.keys().chain(env.decls.enums.keys()).copied().collect();
    syms.sort();
    for s in syms {
        if let Some(d) = env.decls.structs.get(&s).filter(|d| d.is_copy) {
            check(s, d.fields.iter().map(|f| &f.ty).collect());
        }
        if let Some(d) = env.decls.enums.get(&s).filter(|d| d.is_copy) {
            check(s, d.variants.iter().flat_map(|v| v.fields.iter()).collect());
        }
    }
}

/// Struct fields and variant payloads cannot be declared as references in v0
/// (ADR 0010, E4203). This is what lets the borrow checker treat locals as the
/// only holders of references (ADR 0025). Type arguments may still be
/// references (`Option<&T>`, `Pair<&T, U>`): those values are tracked as
/// holders like any local.
fn validate_no_ref_fields(env: &Env, diags: &mut Vec<Diagnostic>) {
    fn has_ref(t: &Ty) -> bool {
        match t {
            Ty::Ref(..) => true,
            Ty::Adt(_, args) => args.iter().any(has_ref),
            Ty::Array(e, _) | Ty::Slice(e) => has_ref(e),
            _ => false,
        }
    }
    let mut syms: Vec<_> = env.decls.structs.keys().chain(env.decls.enums.keys()).copied().collect();
    syms.sort();
    for s in syms {
        let tys: Vec<&Ty> = match (env.decls.structs.get(&s), env.decls.enums.get(&s)) {
            (Some(d), _) => d.fields.iter().map(|f| &f.ty).collect(),
            (_, Some(d)) => d.variants.iter().flat_map(|v| v.fields.iter()).collect(),
            _ => continue,
        };
        if let Some(t) = tys.into_iter().find(|t| has_ref(t)) {
            let sym = env.r.symbol(s);
            diags.push(
                Diagnostic::error("E4203", "reference_in_field", format!("`{}` cannot hold a reference (`{}`) in its fields", sym.name, display(t, env)))
                    .primary(sym.span.unwrap(), "")
                    .note("v0 types own their data; references live only in variables and parameters (ADR 0010)")
                    .help("store an owned value, or make the type generic and instantiate it with a reference"),
            );
        }
    }
}

/// Impl methods must match their interface declaration (E3023).
fn validate_impls(env: &Env, diags: &mut Vec<Diagnostic>) {
    for imp in &env.r.impls {
        let Some(iface) = imp.interface else { continue };
        if Some(iface) == env.decls.transfer || Some(iface) == env.decls.share {
            if let Some(item) = env.inputs[imp.module.0 as usize].ast.items.iter().find(|item| item.id == imp.node) {
                diags.push(Diagnostic::error("E3048", "semantic_capability_impl", "cross-thread capabilities cannot be granted with an impl")
                    .primary(item.span, "capabilities are structural or explicitly trusted declaration metadata")
                    .help("require the capability on generic parameters and ensure every component qualifies"));
            }
            continue;
        }
        for &m in &imp.methods {
            let name = &env.r.symbol(m).name;
            let Some(decl) = env.r.member(iface, name) else { continue };
            let (Some(have), Some(want)) = (env.decls.fns.get(&m), env.decls.fns.get(&decl)) else { continue };
            // The interface's `Self` is this impl's target type (Phase 26C).
            let mut want = want.clone();
            if let (Some(this), Some(target)) = (env.r.interface_self.get(&iface), &have.self_ty) {
                let map = std::collections::HashMap::from([(ParamId(this.0), target.clone())]);
                want.params = want.params.iter().map(|t| crate::check::subst(t, &map)).collect();
                want.ret = crate::check::subst(&want.ret, &map);
            }
            let want = &want;
            let mut problems = Vec::new();
            if have.receiver != want.receiver {
                problems.push(format!("receiver is `{}`, the interface declares `{}`", recv_name(have.receiver), recv_name(want.receiver)));
            }
            if have.params.len() != want.params.len() {
                problems.push(format!("{} parameters, the interface declares {}", have.params.len(), want.params.len()));
            } else {
                for (i, (a, b)) in have.params.iter().zip(&want.params).enumerate() {
                    if a != b {
                        problems.push(format!("parameter {} is `{}`, the interface declares `{}`", i + 1, display(a, env), display(b, env)));
                    }
                }
            }
            if have.ret != want.ret {
                problems.push(format!("returns `{}`, the interface declares `{}`", display(&have.ret, env), display(&want.ret, env)));
            }
            if !problems.is_empty() {
                let iname = &env.r.symbol(iface).name;
                let mut d = Diagnostic::error("E3023", "impl_signature_mismatch", format!("method `{name}` does not match its declaration in `{iname}`"))
                    .primary(have.span, "")
                    .secondary(want.span, "declared here");
                for p in problems {
                    d = d.note(p);
                }
                diags.push(d);
            }
        }
    }
}

fn recv_name(r: Option<tarn_ast::ReceiverKind>) -> &'static str {
    match r {
        None => "none",
        Some(tarn_ast::ReceiverKind::Value) => "self",
        Some(tarn_ast::ReceiverKind::Ref) => "&self",
        Some(tarn_ast::ReceiverKind::RefMut) => "&mut self",
    }
}

/// `a.b[i]` as written, for diagnostics; `None` for other expressions.
pub(crate) fn place_text(e: &tarn_ast::Expr) -> Option<String> {
    use tarn_ast::ExprKind;
    match &e.kind {
        ExprKind::Ident(n) => Some(n.clone()),
        ExprKind::Field { base, name } => Some(format!("{}.{}", place_text(base)?, name.name)),
        ExprKind::Index { base, .. } => Some(format!("{}[…]", place_text(base)?)),
        ExprKind::Paren(inner) => place_text(inner),
        _ => None,
    }
}

pub(crate) fn binding_kind(k: &SymbolKind) -> &'static str {
    match k {
        SymbolKind::Param => "parameter",
        SymbolKind::SelfParam => "`self` parameter",
        SymbolKind::PatternBinding => "pattern binding",
        SymbolKind::LoopBinding => "loop variable",
        SymbolKind::ClosureParam => "closure parameter",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Interface => "interface",
        SymbolKind::Primitive => "type",
        SymbolKind::PreludeType => "type",
        SymbolKind::GenericParam => "type parameter",
        SymbolKind::Builtin => "builtin",
        _ => "item",
    }
}

/// Source-like rendering of a type.
pub fn display(t: &Ty, env: &Env) -> String {
    show(t, env.r, &env.decls.param_names, &|_| "_")
}

/// Like `display`, naming unresolved literal variables `{integer}` / `{float}`.
pub(crate) fn display_vars(t: &Ty, env: &Env, infer: &ty::Infer) -> String {
    show(t, env.r, &env.decls.param_names, &|v| match infer.kind(v) {
        ty::VarKind::Int => "{integer}",
        ty::VarKind::Float => "{float}",
        ty::VarKind::General => "_",
    })
}

fn show(t: &Ty, r: &Resolved, names: &HashMap<ParamId, String>, var: &dyn Fn(u32) -> &'static str) -> String {
    let show = |t: &Ty| show(t, r, names, var);
    let list = |ts: &[Ty]| ts.iter().map(show).collect::<Vec<_>>().join(", ");
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(i) => i.name().into(),
        Ty::Float(FloatTy::F32) => "f32".into(),
        Ty::Float(FloatTy::F64) => "f64".into(),
        Ty::Str => "string".into(),
        Ty::Void => "void".into(),
        Ty::Never => "never".into(),
        Ty::Adt(s, args) if args.is_empty() => r.symbol(*s).name.clone(),
        Ty::Adt(s, args) => format!("{}<{}>", r.symbol(*s).name, list(args)),
        Ty::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, show(x)),
        Ty::Ptr(m, x) => format!("*{}{}", if *m { "mut " } else { "" }, show(x)),
        Ty::Array(x, n) => format!("[{n}]{}", show(x)),
        Ty::Slice(x) => format!("[]{}", show(x)),
        Ty::Async(output) => format!("async computation<{}>", show(output)),
        Ty::Fn(mode, ps, ret) => {
            let ret = match &**ret {
                Ty::Void => String::new(),
                x => format!(" {}", show(x)),
            };
            format!("{}fn({}){ret}", mode.prefix(), list(ps))
        }
        Ty::Param(p) => names.get(p).cloned().unwrap_or_else(|| "?".into()),
        Ty::Any(i) => format!("any {}", r.symbol(*i).name),
        Ty::Var(v) => var(*v).into(),
        Ty::Opaque => "<std>".into(),
        Ty::Error => "<error>".into(),
    }
}

/// `L:C name: type` for every local, parameter and binding (tests, tools).
pub fn dump_types(typed: &Typed, r: &Resolved, sources: &SourceMap) -> String {
    let mut out = String::new();
    for (mi, info) in r.modules.iter().enumerate() {
        if info.name == "core" {
            continue;
        }
        out.push_str(&format!("== {}\n", info.name));
        let mut rows: Vec<(u32, String)> = typed
            .locals
            .iter()
            .filter(|(s, _)| r.symbol(**s).module == Some(ModuleId(mi as u32)))
            .filter_map(|(s, t)| {
                let sym = r.symbol(*s);
                let span = sym.span?;
                let lc = sources.line_col(span);
                Some((span.start, format!("{}:{} {}: {}", lc.line, lc.column, sym.name, show(t, r, &typed.decls.param_names, &|_| "_"))))
            })
            .collect();
        rows.sort();
        for (_, row) in rows {
            out.push_str(&row);
            out.push('\n');
        }
    }
    out
}
