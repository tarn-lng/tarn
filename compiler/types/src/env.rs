//! Declarations as types: struct/enum layouts and function signatures,
//! lowered from the AST using the resolver's tables.

use crate::ty::*;
use std::collections::HashMap;
use tarn_ast::{FnDecl, GenericParam, ItemKind, ReceiverKind, Type, TypeKind};
use tarn_diagnostics::{Diagnostic, Span};
use tarn_resolve::{ModuleId, ModuleInput, Res, Resolved, SymbolId, SymbolKind};

pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
    pub is_pub: bool,
}

pub struct StructDef {
    pub module: ModuleId,
    pub generics: Vec<ParamId>,
    pub fields: Vec<FieldDef>,
    pub is_copy: bool,
}

pub struct VariantDef {
    pub sym: SymbolId,
    pub name: String,
    pub fields: Vec<Ty>,
}

pub struct EnumDef {
    pub generics: Vec<ParamId>,
    pub variants: Vec<VariantDef>,
    pub is_copy: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PassingMode {
    Copy,
    Move,
    SharedBorrow,
    MutableBorrow,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ResultContract {
    Copy,
    Owned,
    Borrowed(Vec<usize>),
    InferredBorrow,
    Ambiguous(usize),
    NoSource,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticContract {
    /// Receiver (if present) is parameter zero.
    pub parameters: Vec<PassingMode>,
    pub result: ResultContract,
}
#[derive(Clone)]
pub struct FnSig {
    pub is_async: bool,
    /// `unsafe fn`: callers need an `unsafe` block (ADR 0047).
    pub is_unsafe: bool,
    /// Owner binders, then the function's own generics.
    pub generics: Vec<ParamId>,
    pub receiver: Option<ReceiverKind>,
    /// Type of `self` for methods of structs/enums and impl methods.
    pub self_ty: Option<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// `extern "C"` / `extern "intrinsic"`.
    pub abi: Option<String>,
    pub module: ModuleId,
    pub span: Span,
    pub contract: SemanticContract,
}

/// Prelude symbols the checker needs by identity.
/// The "lang items": the only names the type checker knows by identity.
/// `Option`, `Result` and `Copy` are declared in `core` (ADR 0020).
pub struct Prelude {
    pub option: SymbolId,
    pub result: SymbolId,
    /// `Copy` capability; `None` only when compiling without `core`.
    pub copy: Option<SymbolId>,
    pub channel: SymbolId,
    pub print: SymbolId,
    pub panic: SymbolId,
    pub channel_fn: SymbolId,
}

/// Private trusted `net` task-record intrinsics (ADR 0038). They are storage
/// primitives for cooperative tasks, not part of the network syscall ABI.
pub const TASK_INTRINSICS: [&str; 7] = ["_task_new", "_task_complete", "_task_take", "_task_wait", "_task_abandoned", "_inbox_push", "_inbox_pop"];

/// Declarations as types. Produced by the type checker and handed to later
/// phases (IR lowering) so they never re-derive layouts or signatures.
#[derive(Default)]
pub struct Decls {
    /// Trusted static C adapter constructor (ADR 0080), found by identity.
    pub ffi_callback: Option<SymbolId>,
    /// Scalar-only unsafe memory bridge; never returns a Tarn reference.
    pub ffi_read: Option<SymbolId>,
    pub ffi_write: Option<SymbolId>,
    pub task: Option<SymbolId>,
    pub net_sockets: Vec<SymbolId>,
    pub fs_file: Option<SymbolId>,
    pub fs_directory: Option<SymbolId>,
    pub fs_intrinsics: HashMap<String, SymbolId>,
    pub process_owner: Option<SymbolId>,
    pub process_pipe: Option<SymbolId>,
    pub process_intrinsics: HashMap<String, SymbolId>,
    pub net_error: Option<SymbolId>,
    pub net_poll: Option<SymbolId>,
    /// `net.TlsStream` (ADR 0066): a private native curl handle owner.
    pub net_tls: Option<SymbolId>,
    pub exec_waker: Option<SymbolId>,
    pub exec_state: Option<SymbolId>,
    pub exec_owner: Option<SymbolId>,
    pub exec_operation: Option<SymbolId>,
    /// Trusted `Progress<R>` and `Operation<R>.poll_with` used by await lowering.
    pub exec_progress: Option<SymbolId>,
    pub exec_poll_with: Option<SymbolId>,
    /// Private trusted async-body primitives: per-poll waker, bare suspension.
    pub exec_async_waker: Option<SymbolId>,
    pub exec_async_park: Option<SymbolId>,
    /// Phase 14A cooperative tasks: handle, runner reference, spawn, and the
    /// private task-record intrinsics (kept out of the network ABI set).
    pub async_task: Option<SymbolId>,
    pub task_ref: Option<SymbolId>,
    pub exec_spawn: Option<SymbolId>,
    pub task_intrinsics: Vec<SymbolId>,
    pub result: Option<SymbolId>,
    pub net_intrinsics: HashMap<String, SymbolId>,
    pub mutex: Option<SymbolId>,
    pub vec: Option<SymbolId>,
    pub mutex_guard: Option<SymbolId>,
    pub atomics: HashMap<SymbolId, Ty>,
    pub transfer: Option<SymbolId>,
    /// `core.Eq`, behind `==`/`!=` for structs, enums and type parameters.
    pub eq: Option<SymbolId>,
    /// `core.Display` and `core.Builder`, behind `f"..."` (ADR 0077).
    pub display: Option<SymbolId>,
    pub builder: Option<SymbolId>,
    pub share: Option<SymbolId>,
    pub native_capabilities: HashMap<SymbolId, crate::NativeCapabilities>,
    /// Interface declaration order, plus resolved implementation IDs.
    pub interfaces: HashMap<SymbolId, Vec<SymbolId>>,
    pub interface_methods: HashMap<SymbolId, (SymbolId, usize)>,
    pub implementations: HashMap<(SymbolId, SymbolId), Vec<SymbolId>>,
    /// `(interface, target)` → bounds the impl requires of the target's type
    /// arguments: (argument index, required interface, bound span) (ADR 0078).
    pub impl_conditions: HashMap<(SymbolId, SymbolId), Vec<(usize, SymbolId, tarn_diagnostics::Span)>>,
    /// Primitive types that have `impl`s in `core` and the symbol naming them.
    pub primitive_targets: Vec<(Ty, SymbolId)>,
    pub structs: HashMap<SymbolId, StructDef>,
    pub enums: HashMap<SymbolId, EnumDef>,
    pub fns: HashMap<SymbolId, FnSig>,
    /// `core.Location`: a trailing parameter of this type is filled with the
    /// call site when omitted (ADR 0094).
    pub location: Option<SymbolId>,
    /// Evaluated `const` items (ADR 0090).
    pub consts: HashMap<SymbolId, crate::consts::ConstDef>,
    pub bounds: HashMap<ParamId, Vec<SymbolId>>,
    pub param_names: HashMap<ParamId, String>,
    /// The `Copy` capability from `core`, if loaded.
    pub copy: Option<SymbolId>,
}

impl Decls {
    /// Is `t` a copy type? The single definition used by the type checker and
    /// by IR lowering (ADR 0021). Inference variables are not handled here.
    pub fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Bool | Ty::Int(_) | Ty::Float(_) | Ty::Never | Ty::Void | Ty::Opaque | Ty::Error => true,
            Ty::Ref(m, _) => !m,
            Ty::Ptr(..) => true,
            Ty::Array(e, _) => self.is_copy(e),
            Ty::Adt(s, _) => self.structs.get(s).is_some_and(|d| d.is_copy) || self.enums.get(s).is_some_and(|d| d.is_copy),
            Ty::Param(p) => self.copy.is_some_and(|c| self.bounds.get(p).is_some_and(|b| b.contains(&c))),
            _ => false,
        }
    }
}

pub struct Env<'a> {
    pub inputs: &'a [ModuleInput<'a>],
    pub r: &'a Resolved,
    pub decls: Decls,
    pub prelude: Prelude,
    pub diags: Vec<Diagnostic>,
}

impl<'a> Env<'a> {
    pub fn new(inputs: &'a [ModuleInput<'a>], r: &'a Resolved) -> Env<'a> {
        let ps = r.scope(tarn_resolve::ScopeId(0));
        let get = |n: &str| ps.get(n).expect("core must declare the lang items");
        let prelude = Prelude {
            option: get("Option"),
            result: get("Result"),
            copy: ps.get("Copy"),
            channel: get("Channel"),
            print: get("print"),
            panic: get("panic"),
            channel_fn: get("channel"),
        };
        let decls = Decls { location: ps.get("Location"), task: ps.get("Task"), eq: ps.get("Eq"), transfer: ps.get("Transfer"), share: ps.get("Share"), copy: prelude.copy, ..Decls::default() };
        let mut decls = decls;
        decls.result = ps.get("Result");
        decls.mutex = ps.get("Mutex");
        decls.vec = ps.get("Vec");
        decls.display = ps.get("Display");
        decls.builder = ps.get("Builder");
        decls.mutex_guard = ps.get("MutexGuard");
        for (name, ty) in [("AtomicBool", "bool"), ("AtomicI32", "i32"), ("AtomicI64", "i64"), ("AtomicU32", "u32"), ("AtomicU64", "u64"), ("AtomicUsize", "usize")] {
            if let Some(id) = ps.get(name) { decls.atomics.insert(id, primitive(ty)); }
        }
        let mut env = Env { inputs, r, decls, prelude, diags: Vec::new() };
        // Trusted stdlib declarations are found in their own modules (ADR 0041);
        // a user module with the same name is never trusted.
        let stdlib = |name: &str| inputs.iter().position(|input| input.name == name && input.trusted_stdlib).map(|index| (index, r.scope(r.modules[index].scope)));
        let method = |module: usize, owner: Option<SymbolId>, name: &str| r.symbols.iter().enumerate().find(|(_, s)| {
            s.name == name && s.module == Some(ModuleId(module as u32)) && matches!(s.kind, SymbolKind::Method { owner: o } if Some(o) == owner)
        }).map(|(i, _)| SymbolId(i as u32));
        if let Some((_, ffi)) = stdlib("ffi") {
            env.decls.ffi_callback = ffi.get("callback");
            env.decls.ffi_read = ffi.get("_read");
            env.decls.ffi_write = ffi.get("_write");
        }
        if let Some((_, io)) = stdlib("io") {
            env.decls.exec_waker = io.get("Waker");
            env.decls.exec_progress = io.get("Progress");
            env.decls.exec_async_waker = io.get("_with_waker");
            env.decls.exec_async_park = io.get("_async_park");
            if let Some(id) = env.decls.exec_waker {
                env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: false, share: false });
            }
        }
        if let Some((index, runtime)) = stdlib("runtime") {
            env.decls.exec_owner = runtime.get("Execution");
            env.decls.exec_operation = runtime.get("Operation");
            env.decls.exec_state = runtime.get("_ExecutionState");
            env.decls.async_task = runtime.get("AsyncTask");
            env.decls.task_ref = runtime.get("_TaskRef");
            env.decls.task_intrinsics = TASK_INTRINSICS.iter().filter_map(|n| runtime.get(n)).collect();
            env.decls.exec_spawn = method(index, env.decls.exec_owner, "spawn_async");
            env.decls.exec_poll_with = method(index, env.decls.exec_operation, "poll_with");
            for name in ["_ExecutionState", "Execution"] {
                if let Some(id) = runtime.get(name) {
                    env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: false, share: false });
                }
            }
        }
        env.collect();
        crate::consts::evaluate(&mut env);
        if let Some((_, io)) = stdlib("io") {
            env.decls.net_error = io.get("Error");
        }
        if let Some((_, net)) = stdlib("net") {
            env.decls.net_poll = net.get("Poll");
            env.decls.net_tls = net.get("TlsStream");
            for id in [env.decls.net_poll, env.decls.net_tls].into_iter().flatten() {
                env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: true, share: false });
            }
        }
        // Descriptor owners; the order is part of the verified native ABI.
        for (module, name) in [("net", "TcpListener"), ("net", "TcpStream"), ("net", "UdpSocket"), ("time", "Timer")] {
            if let Some(id) = stdlib(module).and_then(|(_, scope)| scope.get(name)) {
                env.decls.net_sockets.push(id);
                env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: true, share: false });
            }
        }
        if let Some((_, fs)) = stdlib("fs") {
            env.decls.fs_file = fs.get("File");
            env.decls.fs_directory = fs.get("_Directory");
            for id in [env.decls.fs_file, env.decls.fs_directory].into_iter().flatten() {
                env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: true, share: false });
            }
        }
        if let Some((_, process)) = stdlib("process") {
            env.decls.process_owner = process.get("Process");
            env.decls.process_pipe = process.get("_Pipe");
            for id in [env.decls.process_owner, env.decls.process_pipe].into_iter().flatten() {
                env.decls.native_capabilities.insert(id, crate::NativeCapabilities { transfer: true, share: false });
            }
        }
        for module in tarn_resolve::STDLIB_LAYERS {
            let Some((_, scope)) = stdlib(module) else { continue };
            for id in &scope.symbols {
                let symbol = r.symbol(*id);
                let separate_abi = Some(*id) == env.decls.exec_async_waker || Some(*id) == env.decls.exec_async_park || env.decls.task_intrinsics.contains(id)
                    || Some(*id) == env.decls.ffi_read || Some(*id) == env.decls.ffi_write;
                if !separate_abi && symbol.name.starts_with("_") && env.decls.fns.get(id).is_some_and(|sig| sig.abi.as_deref() == Some("intrinsic")) {
                    let catalog = if *module == "fs" { &mut env.decls.fs_intrinsics } else if *module == "process" { &mut env.decls.process_intrinsics } else { &mut env.decls.net_intrinsics };
                    catalog.insert(format!("{module}.{}", symbol.name), *id);
                }
            }
        }
        for imp in &r.impls {
            if let (Some(interface), Some(target)) = (imp.interface, imp.target) {
                if let Some(order) = env.decls.interfaces.get(&interface) {
                    let methods = order.iter().filter_map(|decl| imp.methods.iter().find(|m| r.symbol(**m).name == r.symbol(*decl).name).copied()).collect();
                    env.decls.implementations.insert((interface, target), methods);
                    if matches!(r.symbol(target).kind, SymbolKind::Primitive) && !env.decls.primitive_targets.iter().any(|(_, t)| *t == target) {
                        env.decls.primitive_targets.push((primitive(&r.symbol(target).name), target));
                    }
                }
            }
        }
        env
    }

    fn def(&self, m: ModuleId, node: tarn_ast::NodeId) -> Option<SymbolId> {
        self.r.tables[m.0 as usize].defs.get(&node).copied()
    }

    fn params_of(&mut self, m: ModuleId, gs: &[GenericParam]) -> Vec<ParamId> {
        let mut out = Vec::new();
        for g in gs {
            let Some(s) = self.def(m, g.id) else { continue };
            let p = ParamId(s.0);
            self.decls.param_names.insert(p, g.name.name.clone());
            let bounds: Vec<SymbolId> = g
                .bounds
                .iter()
                .filter_map(|b| match self.r.tables[m.0 as usize].uses.get(&b.id).map(|u| &u.res) {
                    Some(Res::Symbol(i)) => Some(*i),
                    _ => None,
                })
                .collect();
            self.decls.bounds.insert(p, bounds);
            out.push(p);
        }
        out
    }

    fn collect(&mut self) {
        self.collect_pass(true);
        self.collect_pass(false);
    }

    /// Pass 1 (`types`): structs and enums. Pass 2: functions, interfaces and
    /// impls, which may need the types' declared bounds.
    fn collect_pass(&mut self, types: bool) {
        for (mi, input) in self.inputs.iter().enumerate() {
            let m = ModuleId(mi as u32);
            for item in &input.ast.items {
                let is_type = matches!(item.kind, ItemKind::Struct(_) | ItemKind::Enum(_));
                if is_type != types {
                    continue;
                }
                match &item.kind {
                    ItemKind::Struct(s) => {
                        let Some(sym) = self.def(m, item.id) else { continue };
                        let generics = self.params_of(m, &s.generics);
                        let fields = s.fields.iter().map(|f| FieldDef { name: f.name.name.clone(), ty: self.lower(m, &f.ty), is_pub: f.is_pub }).collect();
                        self.decls.structs.insert(sym, StructDef { module: m, generics, fields, is_copy: s.is_copy });
                    }
                    ItemKind::Enum(e) => {
                        let Some(sym) = self.def(m, item.id) else { continue };
                        let generics = self.params_of(m, &e.generics);
                        let variants = e
                            .variants
                            .iter()
                            .filter_map(|v| {
                                let vs = self.def(m, v.id)?;
                                let fields = v.fields.iter().map(|t| self.lower(m, t)).collect();
                                Some(VariantDef { sym: vs, name: v.name.name.clone(), fields })
                            })
                            .collect();
                        self.decls.enums.insert(sym, EnumDef { generics, variants, is_copy: e.is_copy });
                    }
                    ItemKind::Fn(f) => {
                        // The resolver records `fn T.m`'s owner on the item node.
                        let owner = match self.r.tables[mi].uses.get(&item.id).map(|u| &u.res) {
                            Some(Res::Symbol(o)) => Some(*o),
                            _ => None,
                        };
                        self.fn_sig(m, f, &[], None, owner)
                    }
                    ItemKind::Interface(i) => {
                        if let Some(sym) = self.def(m, item.id) {
                            let methods: Vec<_> = i.methods.iter().filter_map(|f| self.def(m, f.id)).collect();
                            for (index, method) in methods.iter().enumerate() {
                                self.decls.interface_methods.insert(*method, (sym, index));
                            }
                            self.decls.interfaces.insert(sym, methods);
                        }
                        self.params_of(m, &i.generics);
                        for f in &i.methods {
                            self.fn_sig(m, f, &[], None, None);
                        }
                    }
                    ItemKind::Impl(i) => {
                        // Target binders were declared with the argument types' ids.
                        let (target, binders) = match (&i.target.kind, self.r.tables[mi].uses.get(&i.target.id).map(|u| &u.res)) {
                            (TypeKind::Path(p), Some(Res::Symbol(t))) => {
                                let bs: Vec<ParamId> = p.args.iter().filter_map(|a| self.def(m, a.id)).map(|s| ParamId(s.0)).collect();
                                for (a, b) in p.args.iter().zip(&bs) {
                                    if let TypeKind::Path(ap) = &a.kind {
                                        self.decls.param_names.insert(*b, ap.segments[0].name.clone());
                                    }
                                }
                                (Some(*t), bs)
                            }
                            _ => (None, Vec::new()),
                        };
                        if let Some(t) = target {
                            self.inherit_bounds(t, &binders);
                        }
                        // ADR 0078: binder bounds hold inside the impl and are
                        // conditions on every use of it.
                        let uses = &self.r.tables[mi].uses;
                        let interface = match uses.get(&i.interface.id).map(|u| &u.res) {
                            Some(Res::Symbol(s)) => Some(*s),
                            _ => None,
                        };
                        let mut conditions = Vec::new();
                        for (index, (b, bounds)) in binders.iter().zip(&i.binder_bounds).enumerate() {
                            for bound in bounds {
                                if let Some(Res::Symbol(required)) = uses.get(&bound.id).map(|u| &u.res) {
                                    self.decls.bounds.entry(*b).or_default().push(*required);
                                    conditions.push((index, *required, bound.span));
                                }
                            }
                        }
                        if let (Some(interface), Some(t)) = (interface, target)
                            && !conditions.is_empty()
                        {
                            self.decls.impl_conditions.insert((interface, t), conditions);
                        }
                        let self_ty = target.map(|t| {
                            if matches!(self.r.symbol(t).kind, SymbolKind::Primitive) { primitive(&self.r.symbol(t).name) } else { Ty::Adt(t, binders.iter().map(|b| Ty::Param(*b)).collect()) }
                        });
                        for f in &i.methods {
                            self.fn_sig(m, f, &binders, self_ty.clone(), None);
                        }
                    }
                    ItemKind::Import(_) | ItemKind::Const(_) | ItemKind::Error => {}
                }
            }
        }
    }

    fn fn_sig(&mut self, m: ModuleId, f: &FnDecl, outer: &[ParamId], impl_self: Option<Ty>, owner: Option<SymbolId>) {
        let Some(sym) = self.def(m, f.id) else { return };
        let mut generics = outer.to_vec();
        let mut self_ty = impl_self;
        if let Some(o) = &f.owner {
            let binders = self.params_of(m, &o.params);
            if let Some(owner) = owner {
                self_ty = Some(if matches!(self.r.symbol(owner).kind, SymbolKind::Primitive) {
                    primitive(&self.r.symbol(owner).name)
                } else {
                    Ty::Adt(owner, binders.iter().map(|b| Ty::Param(*b)).collect())
                });
                self.inherit_bounds(owner, &binders);
            }
            generics.extend(binders);
        }
        generics.extend(self.params_of(m, &f.generics));
        // Trusted bootstrap read copies its payload. Express its restriction
        // through the existing generic obligation table, not a new conditional
        // method syntax (ordinary owner bounds remain forbidden by ADR 0015).
        if f.abi.as_deref() == Some("intrinsic")
            && ((f.name.name == "read" && matches!(&self_ty, Some(Ty::Adt(id, _)) if Some(*id) == self.decls.mutex_guard))
                || ((f.name.name == "at" || f.name.name == "extend_from_slice") && matches!(&self_ty, Some(Ty::Adt(id, _)) if Some(*id) == self.decls.vec)))
            && let (Some(parameter), Some(copy)) = (generics.first(), self.decls.copy) {
            self.decls.bounds.entry(*parameter).or_default().push(copy);
        }
        let mut params: Vec<Ty> = f.params.iter().map(|p| self.lower(m, &p.ty)).collect();
        let mut ret = f.ret.as_ref().map(|t| self.lower(m, t)).unwrap_or(Ty::Void);
        // Impl methods: `Self` is the target type with its binders (Phase 26C).
        if let Some(target) = &self_ty
            && let Some(this) = self.r.scope(self.r.symbol(sym).scope).get("Self")
            && matches!(self.r.symbol(this).kind, SymbolKind::GenericParam)
        {
            let map = HashMap::from([(ParamId(this.0), target.clone())]);
            params = params.iter().map(|t| crate::check::subst(t, &map)).collect();
            ret = crate::check::subst(&ret, &map);
        }
        let receiver = f.receiver.as_ref().map(|r| r.kind);
        let passing = |ty: &Ty| match ty {
            Ty::Ref(false, _) => PassingMode::SharedBorrow,
            Ty::Ref(true, _) => PassingMode::MutableBorrow,
            _ if self.decls.is_copy(ty) => PassingMode::Copy,
            _ => PassingMode::Move,
        };
        let mut parameter_modes = Vec::new();
        if let Some(receiver) = receiver {
            parameter_modes.push(match receiver {
                ReceiverKind::Ref => PassingMode::SharedBorrow,
                ReceiverKind::RefMut => PassingMode::MutableBorrow,
                ReceiverKind::Value => self_ty.as_ref().map(&passing).unwrap_or(PassingMode::Move),
            });
        }
        parameter_modes.extend(params.iter().map(&passing));
        let offset = usize::from(receiver.is_some());
        let mut sources = Vec::new();
        let holds = |ty: &Ty| self.decls.may_contain_references(ty);
        // The private Task lang item transfers an owned stored result; it does
        // not manufacture a reference from a bodyless declaration.
        let task_join = f.abi.as_deref() == Some("intrinsic") && f.name.name == "join"
            && matches!(&self_ty, Some(Ty::Adt(id, _)) if Some(*id) == self.decls.task);
        let sync_constructor = f.abi.as_deref() == Some("intrinsic") && (f.name.name == "new" || f.name.name == "with_capacity")
            && matches!(&self_ty, Some(Ty::Adt(id, _)) if Some(*id) == self.decls.mutex || Some(*id) == self.decls.vec || self.decls.atomics.contains_key(id));
        // This trusted primitive only executes for scalars/raw pointers. Native
        // ABI validation rejects every loan-bearing instantiation (ADR 0080).
        let scalar_read = Some(sym) == self.decls.ffi_read;
        let result = if scalar_read { ResultContract::Copy } else if task_join || sync_constructor { ResultContract::Owned } else if !holds(&ret) {
            if self.decls.is_copy(&ret) { ResultContract::Copy } else { ResultContract::Owned }
        } else if f.body.is_some() {
            ResultContract::InferredBorrow
        } else if matches!(receiver, Some(ReceiverKind::Ref | ReceiverKind::RefMut)) {
            ResultContract::Borrowed(vec![0])
        } else {
            sources.extend(params.iter().enumerate().filter(|(_, t)| holds(t)).map(|(i, _)| i + offset));
            match sources.len() {
                0 => ResultContract::NoSource,
                1 => ResultContract::Borrowed(sources.clone()),
                n => ResultContract::Ambiguous(n),
            }
        };
        let mut contract = SemanticContract { parameters: parameter_modes, result };
        if let Some(names) = &f.borrows {
            let mut explicit = Vec::new();
            let mut invalid = f.body.is_some() || !holds(&ret) || names.is_empty();
            for name in names {
                let pos = if name.name == "self" && matches!(receiver, Some(ReceiverKind::Ref | ReceiverKind::RefMut)) {
                    Some(0)
                } else {
                    f.params.iter().enumerate().find(|(i, p)| p.name.name == name.name && matches!(params[*i], Ty::Ref(..))).map(|(i, _)| i + offset)
                };
                if let Some(pos) = pos {
                    if explicit.contains(&pos) {
                        invalid = true;
                    } else {
                        explicit.push(pos);
                    }
                } else {
                    invalid = true;
                }
            }
            if invalid {
                self.diags.push(
                    Diagnostic::error("E3042", "invalid_semantic_contract", "invalid borrowed-result contract")
                        .primary(f.span, "use distinct reference inputs on a bodyless borrowed-result declaration"),
                );
            } else {
                explicit.sort();
                contract.result = ResultContract::Borrowed(explicit);
            }
        }
        let sig = FnSig { is_async: f.is_async, is_unsafe: f.is_unsafe, generics, receiver, self_ty, params, ret, abi: f.abi.clone(), module: m, span: f.name.span, contract };
        self.decls.fns.insert(sym, sig);
    }

    /// AST type → `Ty` for declarations; diagnostics go to `self.diags`.
    pub fn lower(&mut self, m: ModuleId, t: &Type) -> Ty {
        let mut diags = std::mem::take(&mut self.diags);
        let ty = self.lower_with(m, t, &mut diags);
        self.diags = diags;
        ty
    }

    /// AST type → `Ty`, using the resolver's `uses` table.
    pub fn lower_with(&self, m: ModuleId, t: &Type, diags: &mut Vec<Diagnostic>) -> Ty {
        let uses = &self.r.tables[m.0 as usize].uses;
        match &t.kind {
            TypeKind::Path(p) => match uses.get(&p.id).map(|u| u.res.clone()) {
                Some(Res::Symbol(s)) => {
                    let sym = self.r.symbol(s);
                    match &sym.kind {
                        SymbolKind::Primitive => primitive(&sym.name),
                        SymbolKind::PreludeType | SymbolKind::Struct | SymbolKind::Enum => {
                            let arity = self.r.type_arity.get(&s).copied().unwrap_or(0);
                            let mut args: Vec<Ty> = p.args.iter().map(|a| self.lower_with(m, a, diags)).collect();
                            args.resize(arity, Ty::Error);
                            // The prelude `Error` is a placeholder until the stdlib defines it.
                            if matches!(sym.kind, SymbolKind::PreludeType) && sym.name == "Error" {
                                diags.push(
                                    Diagnostic::error("E3040", "unmodeled_std_api", "prelude `Error` has no ownership/provenance contract")
                                        .primary(p.span, "define a concrete error type instead"),
                                );
                                return Ty::Error;
                            }
                            Ty::Adt(s, args)
                        }
                        SymbolKind::GenericParam => Ty::Param(ParamId(s.0)),
                        SymbolKind::Interface => {
                            diags.push(
                                Diagnostic::error("E3032", "interface_as_type", format!("interface `{}` cannot be used as a type directly", sym.name))
                                    .primary(p.span, "")
                                    .help(format!("use `any {}` for dynamic dispatch, or a generic parameter `<T: {}>`", sym.name, sym.name)),
                            );
                            Ty::Error
                        }
                        _ => Ty::Error,
                    }
                }
                Some(Res::External { .. }) => {
                    diags.push(
                        Diagnostic::error("E3040", "unmodeled_std_api", "standard-library type has no ownership/provenance contract")
                            .primary(p.span, "cannot safely type-check this type")
                            .help("use a module with explicit Tarn declarations"),
                    );
                    Ty::Error
                }
                _ => Ty::Error,
            },
            TypeKind::Ref { mutable, inner } => {
                let inner_ty = if matches!(inner.kind, TypeKind::Any(_)) {
                    match uses.get(&inner.id).map(|u| &u.res) {
                        Some(Res::Symbol(i)) if Some(*i) == self.decls.transfer || Some(*i) == self.decls.share => {
                            diags.push(Diagnostic::error("E3050", "semantic_capability_dynamic", "cross-thread capabilities are bounds, not dynamic interfaces")
                                .primary(inner.span, "no runtime dispatch object exists for this capability")
                                .help("use `T: Transfer` or `T: Share` on a generic parameter"));
                            Ty::Error
                        }
                        Some(Res::Symbol(i)) => Ty::Any(*i),
                        _ => Ty::Error,
                    }
                } else {
                    self.lower_with(m, inner, diags)
                };
                Ty::Ref(*mutable, Box::new(inner_ty))
            }
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.lower_with(m, inner, diags))),
            TypeKind::Array { len, elem } => {
                let elem = self.lower_with(m, elem, diags);
                match len.kind {
                    tarn_ast::ExprKind::Int(n) => Ty::Array(Box::new(elem), n),
                    _ => {
                        diags.push(
                            Diagnostic::error("E3033", "array_length", "array length must be an integer literal")
                                .primary(len.span, "")
                                .note("constant expressions are not supported in v0"),
                        );
                        Ty::Error
                    }
                }
            }
            TypeKind::Fn { mode, params, ret } => {
                Ty::Fn(*mode, params.iter().map(|p| self.lower_with(m, p, diags)).collect(), Box::new(ret.as_ref().map(|r| self.lower_with(m, r, diags)).unwrap_or(Ty::Void)))
            }
            TypeKind::Ptr { mutable, inner } => {
                if matches!(inner.kind, TypeKind::Slice(_) | TypeKind::Any(_)) {
                    diags.push(
                        Diagnostic::error("E3072", "unsized_pointee", "raw pointers point to sized values")
                            .primary(t.span, "a slice or dynamic interface has no single address")
                            .help("point to the first element instead: `*T` from `ffi.slice(data)`"),
                    );
                    return Ty::Error;
                }
                Ty::Ptr(*mutable, Box::new(self.lower_with(m, inner, diags)))
            }
            TypeKind::Any(_) => {
                diags.push(
                    Diagnostic::error("E3041", "owned_dynamic_interface", "dynamic interfaces are only supported through references")
                        .primary(t.span, "use `&any I` or `&mut any I`"),
                );
                Ty::Error
            }
            TypeKind::Error => Ty::Error,
        }
    }

    /// Owner and impl binders carry the bounds declared on the type
    /// (ADR 0021): inside `fn Point<T>.m`, `T: Copy` holds if `Point<T: Copy>`.
    fn inherit_bounds(&mut self, ty: SymbolId, binders: &[ParamId]) {
        let declared: Vec<ParamId> = match (self.decls.structs.get(&ty), self.decls.enums.get(&ty)) {
            (Some(s), _) => s.generics.clone(),
            (_, Some(e)) => e.generics.clone(),
            _ => return,
        };
        for (b, d) in binders.iter().zip(declared) {
            let inherited = self.decls.bounds.get(&d).cloned().unwrap_or_default();
            self.decls.bounds.entry(*b).or_default().extend(inherited);
        }
    }

    /// The symbol an `impl` names for this type: a struct/enum, or a
    /// primitive (impls for primitives exist only in `core`, ADR 0016).
    pub fn impl_target(&self, ty: &Ty) -> Option<SymbolId> {
        self.decls.impl_target(ty)
    }

    pub fn has_impl(&self, iface: SymbolId, target: SymbolId) -> bool {
        self.r.impls.iter().any(|i| i.interface == Some(iface) && i.target == Some(target))
    }
}

impl Decls {
    /// The symbol an `impl` names for this type: a struct/enum, or a
    /// primitive (impls for primitives exist only in `core`, ADR 0016).
    pub fn impl_target(&self, ty: &Ty) -> Option<SymbolId> {
        match ty {
            Ty::Adt(s, _) => Some(*s),
            _ => self.primitive_targets.iter().find(|(t, _)| t == ty).map(|(_, s)| *s),
        }
    }
}

pub fn primitive(name: &str) -> Ty {
    match name {
        "bool" => Ty::Bool,
        "i8" => Ty::Int(IntTy::I8),
        "i16" => Ty::Int(IntTy::I16),
        "i32" => Ty::Int(IntTy::I32),
        "i64" => Ty::Int(IntTy::I64),
        "isize" => Ty::Int(IntTy::Isize),
        "u8" => Ty::Int(IntTy::U8),
        "u16" => Ty::Int(IntTy::U16),
        "u32" => Ty::Int(IntTy::U32),
        "u64" => Ty::Int(IntTy::U64),
        "usize" => Ty::Int(IntTy::Usize),
        "f32" => Ty::Float(FloatTy::F32),
        "f64" => Ty::Float(FloatTy::F64),
        "string" => Ty::Str,
        "void" => Ty::Void,
        "never" => Ty::Never,
        _ => Ty::Error,
    }
}
