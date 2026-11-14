//! `static` items and `#[export]`/`#[section]`/`#[align]` attributes
//! (ADR 0101, P6).
//!
//! A static is fixed-layout global data: integers, floats, `bool`, raw
//! pointers, fixed arrays and structs of those, laid out in declaration order
//! with natural alignment (the C layout). The initializer is evaluated at
//! compile time into a `StaticInit` tree; omitting it zero-fills the object.
//! Using a static's name as an expression yields its address (`*T`, or
//! `*mut T` for `static mut`); nothing ever borrows static storage, so the
//! ownership model has nothing to track and every access goes through
//! `unsafe` pointer reads and writes.
use crate::env::Env;
use crate::ty::Ty;
use crate::IntTy;
use std::collections::HashMap;
use tarn_ast::{Attr, AttrArg, ExprKind, FnDecl, ItemKind, StaticDecl};
use tarn_diagnostics::{Diagnostic, Span};
use tarn_resolve::{ModuleId, Res, SymbolId, SymbolKind};

#[derive(Clone, Debug, PartialEq)]
pub enum StaticInit {
    /// Every byte zero.
    Zero,
    Int(i128),
    /// Literal text, as written.
    Float(String),
    Bool(bool),
    /// Absolute address (`ffi.from_address(n)`); `0` is the null pointer.
    Address(u64),
    /// Address of another static.
    Static(SymbolId),
    /// C callback address (ADR 0102), resolved before code generation.
    Function(SymbolId),
    /// Struct fields or array elements, in order.
    Aggregate(Vec<StaticInit>),
}

#[derive(Clone, Debug)]
pub struct StaticDef {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub init: StaticInit,
    /// `#[align(n)]`, a power of two.
    pub align: Option<u32>,
    pub section: Option<String>,
    pub export: Option<String>,
    pub module: ModuleId,
    pub span: Span,
}

/// Attributes of a function that reach the backend.
#[derive(Clone, Debug, Default)]
pub struct FnAttrs {
    pub export: Option<String>,
    pub section: Option<String>,
    /// `#[interrupt]` (ADR 0101, P7): the function is an interrupt handler body.
    pub interrupt: bool,
    /// `#[interrupt(n)]`: aarch64 vector-table slot (0..=15) that branches to the handler.
    pub vector: Option<u32>,
    /// `#[vector_table]`: the function stands for the generated aarch64 vector table.
    pub vector_table: bool,
    /// `#[pvh_entry]` (ADR 0101, P8): the function is the 64-bit kernel entry
    /// that the generated PVH boot code calls (x86_64 only).
    pub pvh_entry: bool,
}

impl FnAttrs {
    /// Functions that must be compiled although nothing calls them.
    pub fn is_root(&self) -> bool {
        self.export.is_some() || self.vector.is_some() || self.vector_table || self.pvh_entry
    }
}

fn invalid_static(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error("E3085", "invalid_static", message.into()).primary(span, "")
}

fn invalid_attr(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error("E3086", "invalid_attribute", message.into()).primary(span, "")
}

/// A C identifier: the only spelling accepted for exported symbols.
pub fn valid_symbol(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 255 && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An ELF section name: `.` then letters, digits, `.`, `_` or `-`.
pub fn valid_section(name: &str) -> bool {
    name.len() <= 64 && name.len() > 1 && name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What the backend needs of one attribute list. Reports every problem.
struct Parsed {
    export: Option<String>,
    section: Option<String>,
    align: Option<u32>,
    interrupt: bool,
    vector: Option<u32>,
    vector_table: bool,
    pvh_entry: bool,
}

fn parse_attrs(attrs: &[Attr], is_fn: bool, diags: &mut Vec<Diagnostic>) -> Parsed {
    let mut out = Parsed { export: None, section: None, align: None, interrupt: false, vector: None, vector_table: false, pvh_entry: false };
    let what = if is_fn { "function" } else { "static" };
    for a in attrs {
        let known: &[&str] = if is_fn { &["export", "section", "interrupt", "vector_table", "pvh_entry"] } else { &["export", "section", "align"] };
        if !known.contains(&a.name.as_str()) {
            diags.push(invalid_attr(a.span, format!("unknown attribute `{}` on a {what}", a.name)).help(format!("a {what} accepts {}", known.iter().map(|k| format!("`#[{k}(..)]`")).collect::<Vec<_>>().join(", "))));
            continue;
        }
        let duplicate = match a.name.as_str() {
            "export" => out.export.is_some(),
            "section" => out.section.is_some(),
            "interrupt" => out.interrupt,
            "vector_table" => out.vector_table,
            "pvh_entry" => out.pvh_entry,
            _ => out.align.is_some(),
        };
        if duplicate {
            diags.push(invalid_attr(a.span, format!("`#[{}]` given more than once", a.name)));
            continue;
        }
        match (a.name.as_str(), &a.arg) {
            ("export", Some(AttrArg::Str(s))) if valid_symbol(s) => out.export = Some(s.clone()),
            ("export", Some(AttrArg::Str(s))) => diags.push(invalid_attr(a.span, format!("`{s}` is not a valid symbol name")).help("use a C identifier: letters, digits and `_`, not starting with a digit")),
            ("export", _) => diags.push(invalid_attr(a.span, "`#[export]` needs a symbol name").help("write `#[export(\"name\")]`")),
            ("section", Some(AttrArg::Str(s))) if valid_section(s) => out.section = Some(s.clone()),
            ("section", Some(AttrArg::Str(s))) => diags.push(invalid_attr(a.span, format!("`{s}` is not a valid section name")).help("use `.` followed by letters, digits, `.`, `_` or `-`, such as `.text.boot`")),
            ("section", _) => diags.push(invalid_attr(a.span, "`#[section]` needs a section name").help("write `#[section(\".text.boot\")]`")),
            ("interrupt", None) => out.interrupt = true,
            ("interrupt", Some(AttrArg::Int(n))) if *n < 16 => {
                out.interrupt = true;
                out.vector = Some(*n as u32);
            }
            ("interrupt", Some(AttrArg::Int(n))) => diags.push(invalid_attr(a.span, format!("vector table slot {n} is out of range")).help("an aarch64 vector table has 16 slots, 0 to 15")),
            ("interrupt", _) => diags.push(invalid_attr(a.span, "`#[interrupt]` takes no argument or a vector table slot number").help("write `#[interrupt]` or `#[interrupt(5)]`")),
            ("vector_table", None) => out.vector_table = true,
            ("vector_table", _) => diags.push(invalid_attr(a.span, "`#[vector_table]` takes no argument")),
            ("pvh_entry", None) => out.pvh_entry = true,
            ("pvh_entry", _) => diags.push(invalid_attr(a.span, "`#[pvh_entry]` takes no argument")),
            ("align", Some(AttrArg::Int(n))) if n.is_power_of_two() && *n <= 65536 => out.align = Some(*n as u32),
            ("align", Some(AttrArg::Int(n))) => diags.push(invalid_attr(a.span, format!("alignment {n} is not a power of two up to 65536"))),
            ("align", _) => diags.push(invalid_attr(a.span, "`#[align]` needs a number").help("write `#[align(4096)]`")),
            _ => {}
        }
    }
    out
}

/// Validate attributes on functions and fill `decls.fn_attrs`; evaluate every
/// `static` into `decls.statics`.
pub fn evaluate(env: &mut Env<'_>) {
    let inputs = env.inputs;
    let mut exported: HashMap<String, Span> = HashMap::new();
    // ---- functions
    for (mi, input) in inputs.iter().enumerate() {
        for item in &input.ast.items {
            let ItemKind::Fn(f) = &item.kind else { continue };
            if f.attrs.is_empty() {
                continue;
            }
            let Some(&sym) = env.r.tables[mi].defs.get(&f.id) else { continue };
            let mut diags = Vec::new();
            let parsed = parse_attrs(&f.attrs, true, &mut diags);
            diags.extend(function_problems(env, f, sym, &parsed));
            if let Some(name) = &parsed.export {
                if let Some(first) = exported.insert(name.clone(), f.name.span) {
                    diags.push(invalid_attr(f.name.span, format!("symbol `{name}` is exported twice")).secondary(first, "first exported here"));
                }
            }
            env.diags.extend(diags);
            env.decls.fn_attrs.insert(sym, FnAttrs { export: parsed.export, section: parsed.section, interrupt: parsed.interrupt, vector: parsed.vector, vector_table: parsed.vector_table, pvh_entry: parsed.pvh_entry });
        }
    }
    // ---- statics, pass 1: types and attributes
    let mut list: Vec<(SymbolId, ModuleId, &StaticDecl, Span)> = Vec::new();
    for (mi, input) in inputs.iter().enumerate() {
        for item in &input.ast.items {
            if let ItemKind::Static(d) = &item.kind
                && let Some(&sym) = env.r.tables[mi].defs.get(&item.id)
            {
                list.push((sym, ModuleId(mi as u32), d, item.span));
            }
        }
    }
    list.sort_by_key(|(s, ..)| s.0);
    for (sym, m, d, span) in &list {
        let ty = env.lower(*m, &d.ty);
        if ty == Ty::Error {
            continue;
        }
        if let Err(why) = storable(env, &ty, 0) {
            env.diags.push(invalid_static(d.ty.span, format!("a static cannot have type `{}`: {why}", crate::show(&ty, env.r, &env.decls.param_names, &|_| "_"))).note("statics hold integers, floats, `bool`, raw pointers, fixed arrays and structs of those"));
            continue;
        }
        let mut diags = Vec::new();
        let parsed = parse_attrs(&d.attrs, false, &mut diags);
        if let Some(name) = &parsed.export {
            if let Some(first) = exported.insert(name.clone(), d.name.span) {
                diags.push(invalid_attr(d.name.span, format!("symbol `{name}` is exported twice")).secondary(first, "first exported here"));
            }
        }
        env.diags.extend(diags);
        env.decls.statics.insert(
            *sym,
            StaticDef { name: d.name.name.clone(), ty, mutable: d.is_mut, init: StaticInit::Zero, align: parsed.align, section: parsed.section, export: parsed.export, module: *m, span: *span },
        );
    }
    // ---- pass 2: initializers
    for (sym, m, d, _) in &list {
        let Some(def) = env.decls.statics.get(sym) else { continue };
        let ty = def.ty.clone();
        let Some(value) = &d.value else { continue };
        match init(env, *m, value, &ty) {
            Ok(i) => {
                if let Some(def) = env.decls.statics.get_mut(sym) {
                    def.init = i;
                }
            }
            Err(e) => {
                if !e.code.is_empty() {
                    env.diags.push(*e);
                }
            }
        }
    }
}

fn function_problems(env: &Env<'_>, f: &FnDecl, sym: SymbolId, parsed: &Parsed) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let span = f.attrs.first().map(|a| a.span).unwrap_or(f.name.span);
    if f.abi.is_some() {
        out.push(invalid_attr(span, "attributes apply to functions with a Tarn body, not to `extern` declarations").help("an `extern \"C\"` declaration names a symbol defined elsewhere"));
        return out;
    }
    if f.owner.is_some() || !f.generics.is_empty() {
        out.push(invalid_attr(span, "attributes are not allowed on generic functions or methods").help("a symbol names exactly one machine function; write a plain function that calls the generic one with concrete types"));
        return out;
    }
    if f.is_async {
        out.push(invalid_attr(span, "attributes are not allowed on `async` functions"));
        return out;
    }
    if parsed.interrupt || parsed.vector_table {
        let show = |t: &Ty| crate::show(t, env.r, &env.decls.param_names, &|_| "_");
        let which = if parsed.interrupt { "interrupt" } else { "vector_table" };
        if parsed.interrupt && parsed.vector_table {
            out.push(invalid_attr(span, "`#[interrupt]` and `#[vector_table]` cannot be combined"));
        }
        if f.is_unsafe {
            out.push(invalid_attr(span, format!("`#[{which}]` functions cannot be `unsafe fn`; use `unsafe` blocks inside")));
        }
        if let Some(sig) = env.decls.fns.get(&sym) {
            if sig.ret != Ty::Void && sig.ret != Ty::Error {
                out.push(invalid_attr(f.ret.as_ref().map(|t| t.span).unwrap_or(f.name.span), format!("a `#[{which}]` function returns nothing, found `{}`", show(&sig.ret))));
            }
            if parsed.vector_table {
                if !sig.params.is_empty() {
                    out.push(invalid_attr(f.name.span, "a `#[vector_table]` function takes no parameters").help("write `#[vector_table] fn vectors() {}`; the compiler generates the table"));
                }
            } else {
                let frame_ok = |t: &Ty| matches!(t, Ty::Ptr(true, inner) if !matches!(**inner, Ty::Slice(_) | Ty::Any(_) | Ty::Str)) || *t == Ty::Error;
                match sig.params.as_slice() {
                    [frame] if frame_ok(frame) => {}
                    [frame, code] if frame_ok(frame) && (*code == Ty::Int(IntTy::U64) || *code == Ty::Error) => {}
                    other => {
                        out.push(invalid_attr(f.name.span, format!("an `#[interrupt]` function takes `(frame *mut Frame)` or, on x86_64 for exceptions that push one, `(frame *mut Frame, code u64)`; found ({})",
                            other.iter().map(|t| show(t)).collect::<Vec<_>>().join(", ")))
                            .note("the frame is a raw `*mut` pointer to the CPU-saved state; the error code, when present, is a `u64`"));
                    }
                }
            }
        }
        if parsed.vector_table && parsed.vector.is_some() {
            out.push(invalid_attr(span, "a slot number belongs to `#[interrupt(n)]`"));
        }
    }
    if parsed.pvh_entry {
        let show = |t: &Ty| crate::show(t, env.r, &env.decls.param_names, &|_| "_");
        if f.is_unsafe {
            out.push(invalid_attr(span, "`#[pvh_entry]` functions cannot be `unsafe fn`; use `unsafe` blocks inside"));
        }
        if let Some(sig) = env.decls.fns.get(&sym) {
            if sig.ret != Ty::Void && sig.ret != Ty::Error {
                out.push(invalid_attr(f.ret.as_ref().map(|t| t.span).unwrap_or(f.name.span), format!("a `#[pvh_entry]` function returns nothing, found `{}`", show(&sig.ret))));
            }
            let info_ok = |t: &Ty| matches!(t, Ty::Ptr(_, inner) if !matches!(**inner, Ty::Slice(_) | Ty::Any(_) | Ty::Str)) || *t == Ty::Int(IntTy::U64) || *t == Ty::Error;
            match sig.params.as_slice() {
                [] => {}
                [info] if info_ok(info) => {}
                other => out.push(invalid_attr(f.name.span, format!("a `#[pvh_entry]` function takes no parameters or one `u64` or raw pointer (the address of the `hvm_start_info` the boot loader passes); found ({})",
                    other.iter().map(|t| show(t)).collect::<Vec<_>>().join(", ")))),
            }
        }
    }
    if parsed.export.is_some()
        && let Some(sig) = env.decls.fns.get(&sym)
    {
        let c_type = |t: &Ty| matches!(t, Ty::Bool | Ty::Int(_) | Ty::Float(_) | Ty::Ptr(..));
        let show = |t: &Ty| crate::show(t, env.r, &env.decls.param_names, &|_| "_");
        for (p, ty) in f.params.iter().zip(&sig.params) {
            if !c_type(ty) && *ty != Ty::Error {
                out.push(invalid_attr(p.ty.span, format!("exported function parameter `{}` has type `{}`, which has no C ABI", p.name.name, show(ty))).note("exported functions take integers, floats, `bool` and raw pointers"));
            }
        }
        if !c_type(&sig.ret) && sig.ret != Ty::Void && sig.ret != Ty::Error {
            let at = f.ret.as_ref().map(|t| t.span).unwrap_or(f.name.span);
            out.push(invalid_attr(at, format!("exported function returns `{}`, which has no C ABI", show(&sig.ret))).note("exported functions return nothing, an integer, a float, `bool` or a raw pointer"));
        }
    }
    out
}

/// Why `ty` cannot be the type of a static, if it cannot.
fn storable(env: &Env<'_>, ty: &Ty, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("nesting is too deep".into());
    }
    match ty {
        Ty::Bool | Ty::Int(_) | Ty::Float(_) => Ok(()),
        Ty::Ptr(_, inner) if matches!(**inner, Ty::Slice(_) | Ty::Any(_)) => Err("raw pointers to unsized values are not allowed".into()),
        Ty::Ptr(..) => Ok(()),
        Ty::Array(_, 0) => Err("a zero-length array has no storage".into()),
        Ty::Array(e, _) => storable(env, e, depth + 1),
        Ty::Adt(s, args) => {
            let Some(def) = env.decls.structs.get(s) else { return Err("only structs have a fixed layout; enums do not".into()) };
            if !args.is_empty() || !def.generics.is_empty() {
                return Err("generic structs are not allowed".into());
            }
            def.fields.iter().try_for_each(|f| storable(env, &f.ty, depth + 1).map_err(|e| format!("field `{}`: {e}", f.name)))
        }
        Ty::Str => Err("`string` owns heap memory; use a `[N]u8` array and a length".into()),
        _ => Err("this type has no fixed C layout".into()),
    }
}

type Fail = Box<Diagnostic>;

fn init(env: &mut Env<'_>, m: ModuleId, e: &tarn_ast::Expr, ty: &Ty) -> Result<StaticInit, Fail> {
    let bad = |what: &str| -> Fail {
        Box::new(invalid_static(e.span, format!("{what} is not a valid initializer here")).note("static initializers are literals, constants, struct and array literals of those, `ffi.null()`, `ffi.from_address(n)`, `ffi.callback(function)` and the names of other statics"))
    };
    if let ExprKind::Paren(inner) = &e.kind {
        return init(env, m, inner, ty);
    }
    match ty {
        Ty::Int(_) | Ty::Float(_) | Ty::Bool => {
            let v = crate::consts::scalar(env, m, e, ty)?;
            Ok(match v {
                crate::consts::ConstValue::Int(v) => StaticInit::Int(v),
                crate::consts::ConstValue::Float(s) => StaticInit::Float(s),
                crate::consts::ConstValue::Bool(b) => StaticInit::Bool(b),
                crate::consts::ConstValue::Str(_) => return Err(bad("a string")),
            })
        }
        Ty::Ptr(pointer_mut, pointee) => {
            // `OTHER`: the address of another static.
            if matches!(e.kind, ExprKind::Ident(_) | ExprKind::Field { .. })
                && let Some(Res::Symbol(s)) = env.r.tables[m.0 as usize].uses.get(&e.id).map(|u| u.res.clone())
                && matches!(env.r.symbol(s).kind, SymbolKind::Static)
            {
                let Some(target) = env.decls.statics.get(&s) else { return Err(Box::new(Diagnostic::error("", "", ""))) };
                if &target.ty != pointee.as_ref() || (*pointer_mut && !target.mutable) {
                    let shown = |t: &Ty, mutable: bool| format!("*{}{}", if mutable { "mut " } else { "" }, crate::show(t, env.r, &env.decls.param_names, &|_| "_"));
                    return Err(Box::new(invalid_static(e.span, format!("`{}` has type `{}`, expected `{}`", target.name, shown(&target.ty, target.mutable), shown(pointee, *pointer_mut)))));
                }
                return Ok(StaticInit::Static(s));
            }
            if let ExprKind::Call { callee, args } = &e.kind
                && let Some(Res::Symbol(f)) = env.r.tables[m.0 as usize].uses.get(&callee.id).map(|u| u.res.clone())
            {
                if Some(f) == env.decls.ffi_callback {
                    let fail = || Box::new(invalid_static(e.span, "a static callback must directly name a safe, synchronous, non-generic Tarn function with scalar/pointer C ABI parameters and result"));
                    if *pointer_mut || **pointee != Ty::Int(IntTy::U8) { return Err(Box::new(invalid_static(e.span, "`ffi.callback(function)` has type `*u8`"))); }
                    let [argument] = args.as_slice() else { return Err(fail()); };
                    let mut argument = argument;
                    while let ExprKind::Paren(inner) = &argument.kind { argument = inner; }
                    let Some(Res::Symbol(s)) = env.r.tables[m.0 as usize].uses.get(&argument.id).map(|u| &u.res) else { return Err(fail()); };
                    let Some(sig) = env.decls.fns.get(s) else { return Err(fail()); };
                    let scalar = |ty: &Ty| matches!(ty, Ty::Bool | Ty::Int(_) | Ty::Float(_) | Ty::Ptr(..));
                    if !callback_function(sig) || !sig.params.iter().all(scalar) || !(sig.ret == Ty::Void || scalar(&sig.ret)) {
                        return Err(fail());
                    }
                    return Ok(StaticInit::Function(*s));
                }
                if Some(f) == env.decls.ffi_null && args.is_empty() {
                    return Ok(StaticInit::Address(0));
                }
                if Some(f) == env.decls.ffi_from_address && args.len() == 1 {
                    return match crate::consts::scalar(env, m, &args[0], &Ty::Int(crate::ty::IntTy::Usize))? {
                        crate::consts::ConstValue::Int(v) => Ok(StaticInit::Address(v as u64)),
                        _ => Err(bad("this address")),
                    };
                }
                if Some(f) == env.decls.ffi_to_const && args.len() == 1 && !*pointer_mut {
                    // `ffi.to_const(OTHER)`: a `*T` from a `*mut T` static.
                    return init(env, m, &args[0], &Ty::Ptr(true, pointee.clone()));
                }
            }
            Err(bad("this expression"))
        }
        Ty::Array(elem, n) => {
            let ExprKind::ArrayLit { elems, .. } = &e.kind else { return Err(bad("this expression").into()) };
            if elems.len() as u64 != *n {
                return Err(Box::new(invalid_static(e.span, format!("array of {n} elements initialized with {}", elems.len()))));
            }
            let items = elems.iter().map(|x| init(env, m, x, elem)).collect::<Result<Vec<_>, _>>()?;
            Ok(StaticInit::Aggregate(items))
        }
        Ty::Adt(s, _) => {
            let ExprKind::StructLit { fields, .. } = &e.kind else { return Err(bad("this expression")) };
            let Some(def) = env.decls.structs.get(s) else { return Err(bad("this expression")) };
            let declared: Vec<(String, Ty)> = def.fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect();
            let mut values: Vec<Option<StaticInit>> = vec![None; declared.len()];
            for f in fields {
                let Some(at) = declared.iter().position(|(n, _)| *n == f.name.name) else {
                    return Err(Box::new(invalid_static(f.name.span, format!("struct has no field `{}`", f.name.name))));
                };
                if values[at].is_some() {
                    return Err(Box::new(invalid_static(f.name.span, format!("field `{}` given twice", f.name.name))));
                }
                let Some(value) = &f.value else { return Err(bad("a field shorthand")) };
                values[at] = Some(init(env, m, value, &declared[at].1)?);
            }
            let mut out = Vec::new();
            for (value, (name, _)) in values.into_iter().zip(&declared) {
                out.push(value.ok_or_else(|| Box::new(invalid_static(e.span, format!("missing field `{name}`")).help("a static initializer must give every field")))?);
            }
            Ok(StaticInit::Aggregate(out))
        }
        _ => Err(bad("this expression")),
    }
}

/// The named-function contract shared by runtime and static callbacks.
pub(crate) fn callback_function(f: &crate::env::FnSig) -> bool {
    f.abi.is_none() && !f.is_async && !f.is_unsafe && f.generics.is_empty() && f.receiver.is_none()
}

/// All static callback targets, deterministic and deduplicated, including nested aggregates.
pub fn function_targets(decls: &crate::env::Decls) -> Vec<SymbolId> {
    fn visit(init: &StaticInit, out: &mut Vec<SymbolId>) {
        match init {
            StaticInit::Function(s) => out.push(*s),
            StaticInit::Aggregate(items) => { for item in items { visit(item, out); } }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for def in decls.statics.values() { visit(&def.init, &mut out); }
    out.sort_by_key(|s| s.0);
    out.dedup();
    out
}
