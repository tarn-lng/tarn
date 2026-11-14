use super::*;

/// Native adapters own no environment. Their signatures reuse the exact C ABI
/// classifier used by external calls; Tarn bodies keep their private ABI.
pub(super) fn emit_callbacks(module: &mut ObjectModule, p: &post::Program,
    ids: &HashMap<FunctionId, FuncId>) -> Result<HashMap<FunctionId, FuncId>> {
    let mut targets = Vec::new();
    for f in &p.functions { for b in &f.blocks {
        if let Terminator::Call { callee: Callee::Intrinsic(name), args, .. } = &b.term
            && name == "ffi.callback"
        {
            let [Operand::Const(Const::Fn(id, ts))] = args.as_slice() else { return Err(Error::bug("C callback target missing")); };
            if !ts.is_empty() { return Err(Error::bug("unspecialized C callback")); }
            targets.push(*id);
        }
    }}
    targets.sort_by_key(|id| id.0);
    targets.dedup();
    let mut adapters = HashMap::new();
    for target in targets {
        let f = &p.functions.get(target.0 as usize).ok_or_else(|| Error::bug("C callback target missing"))?.decl;
        if f.kind != FnKind::Body || f.asynchronous.is_some() { return Err(Error::bug("invalid C callback target")); }
        let sig = c_signature(module, f)?;
        let id = module.declare_function(&format!("tarn_c_callback_{}", target.0), Linkage::Local, &sig)
            .map_err(|e| Error::bug(e.to_string()))?;
        let mut ctx = module.make_context();
        ctx.func.signature = sig;
        let mut fb = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let params = b.block_params(entry).to_vec();
            let callee = module.declare_func_in_func(ids[&target], b.func);
            let call = b.ins().call(callee, &params);
            let results = b.inst_results(call).to_vec();
            b.ins().return_(&results);
            b.seal_all_blocks();
            b.finalize();
        }
        cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
        module.define_function(id, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
        adapters.insert(target, id);
    }
    Ok(adapters)
}

/// `#[export("name")]` (ADR 0101, P6): a global C-ABI symbol that forwards to
/// the ordinary Tarn body. Returns each adapter's section, if it has one.
/// `contract` holds the signatures of the freestanding contract: a program may
/// define those symbols itself, but only with exactly the signature the
/// generated code calls them with.
pub(super) fn emit_exports(module: &mut ObjectModule, p: &post::Program, t: &tarn_types::Typed, ids: &HashMap<FunctionId, FuncId>,
    target: crate::Target, contract: &HashMap<String, cl::Signature>) -> Result<Vec<(FuncId, String)>> {
    let mut sections = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    for f in &p.functions {
        let Some(attrs) = f.decl.symbol.and_then(|s| t.decls.fn_attrs.get(&s)) else { continue };
        let Some(name) = &attrs.export else { continue };
        if f.decl.kind != FnKind::Body || f.decl.asynchronous.is_some() || !f.decl.generics.is_empty() { return Err(Error::bug("invalid exported function")); }
        if let Some(first) = seen.insert(name.clone(), f.decl.name.clone()) {
            return Err(Error::unsupported(format!("symbol `{name}` is exported by both `{first}` and `{}`", f.decl.name)));
        }
        crate::statics::check_export_name(name, target, true)?;
        let sig = c_signature(module, &f.decl).map_err(|e| Error::unsupported(format!("exported function `{}`: {}", f.decl.name, e.message)))?;
        if let Some(required) = contract.get(name.as_str()).filter(|_| target.os == crate::Os::None) {
            if required.params != sig.params || required.returns != sig.returns {
                let show = |s: &cl::Signature| format!("({}) -> {}", s.params.iter().map(|a| a.value_type.to_string()).collect::<Vec<_>>().join(", "), s.returns.first().map_or("void".to_string(), |a| a.value_type.to_string()));
                return Err(Error::unsupported(format!(
                    "`{name}` is part of the freestanding contract and must have the signature {} (usize and raw pointers are i64); `{}` has {}",
                    show(required), f.decl.name, show(&sig)
                )));
            }
        }
        let id = module.declare_function(name, Linkage::Export, &sig).map_err(|e| Error::unsupported(format!("exported symbol `{name}` conflicts with another declaration: {e}")))?;
        define_adapter(module, id, sig, ids[&f.decl.id])?;
        if let Some(section) = &attrs.section { sections.push((id, section.clone())); }
    }
    Ok(sections)
}

/// A function with `sig` that forwards its arguments to `target`.
fn define_adapter(module: &mut ObjectModule, id: FuncId, sig: cl::Signature, target: FuncId) -> Result<()> {
    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let mut fb = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry).to_vec();
        let callee = module.declare_func_in_func(target, b.func);
        let call = b.ins().call(callee, &params);
        let results = b.inst_results(call).to_vec();
        b.ins().return_(&results);
        b.seal_all_blocks();
        b.finalize();
    }
    cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
    module.define_function(id, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
    Ok(())
}

impl Cx<'_, '_> {
    /// Trusted `ffi` intrinsics (ADRs 0047/0080). Pointer conversions do not
    /// access pointee storage; scalar reads/writes are explicitly unsafe APIs.
    pub(super) fn ffi(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        let Some(operation) = name.strip_prefix("ffi.") else { return Ok(None) };
        let word = |v: &Val| v.value.ok_or_else(|| Error::bug("ffi operand"));
        // `of(&data)` with an inferred unsized T would yield the address of a
        // temporary (pointer, length) pair, not of the data.
        if let ("of" | "of_mut", [a]) = (operation, args)
            && matches!(&a.ty, Ty::Ref(_, inner) if matches!(inner.as_ref(), Ty::Slice(_) | Ty::Any(_)))
        {
            return Err(Error::unsupported("ffi.of on a slice or dynamic reference; use ffi.slice"));
        }
        let value = match (operation, args, dest) {
            ("_read", [pointer], ty) => {
                if c_type(ty).is_none() { return Err(Error::unsupported("ffi.read supports only scalars and raw pointers")); }
                if !matches!(&pointer.ty, Ty::Ptr(false, inner) if inner.as_ref() == ty) { return Err(Error::bug("ffi.read pointer type")); }
                self.b.ins().load(scalar(ty).unwrap(), cl::MemFlags::new(), word(pointer)?, 0)
            }
            ("_write", [pointer, value], Ty::Void) => {
                if c_type(&value.ty).is_none() { return Err(Error::unsupported("ffi.write supports only scalars and raw pointers")); }
                if !matches!(&pointer.ty, Ty::Ptr(true, inner) if inner.as_ref() == &value.ty) { return Err(Error::bug("ffi.write pointer type")); }
                self.b.ins().store(cl::MemFlags::new(), word(value)?, word(pointer)?, 0);
                return Ok(Some(Val { value: None, ty: Ty::Void }));
            }
            // Volatile twins (ADR 0101, P4). Cranelift's alias analysis forwards
            // stores to loads and merges repeated loads of one address for
            // plain memory operations, so volatile accesses are emitted as
            // atomic ones: those are memory fences for that analysis and are
            // never forwarded, merged, reordered or dropped (SeqCst, which is
            // stronger than volatile requires). Only 8..64-bit integers and
            // pointers have atomic forms; floats go through an integer bitcast.
            ("_read_volatile", [pointer], ty) => {
                if c_type(ty).is_none() { return Err(Error::unsupported("ffi.read_volatile supports only scalars and raw pointers")); }
                if !matches!(&pointer.ty, Ty::Ptr(false, inner) if inner.as_ref() == ty) { return Err(Error::bug("ffi.read_volatile pointer type")); }
                let (clt, bits) = Self::volatile_type(ty)?;
                let v = self.b.ins().atomic_load(clt, cl::MemFlags::new(), word(pointer)?);
                self.volatile_finish(v, bits, ty)
            }
            ("_write_volatile", [pointer, value], Ty::Void) => {
                if c_type(&value.ty).is_none() { return Err(Error::unsupported("ffi.write_volatile supports only scalars and raw pointers")); }
                if !matches!(&pointer.ty, Ty::Ptr(true, inner) if inner.as_ref() == &value.ty) { return Err(Error::bug("ffi.write_volatile pointer type")); }
                let (clt, _) = Self::volatile_type(&value.ty)?;
                let raw = word(value)?;
                let raw = if self.b.func.dfg.value_type(raw) == clt { raw } else { self.b.ins().bitcast(clt, cl::MemFlags::new(), raw) };
                self.b.ins().atomic_store(cl::MemFlags::new(), raw, word(pointer)?);
                return Ok(Some(Val { value: None, ty: Ty::Void }));
            }
            ("fence", [], Ty::Void) => {
                self.b.ins().fence();
                return Ok(Some(Val { value: None, ty: Ty::Void }));
            }
            ("null", [], Ty::Ptr(true, _)) => self.b.ins().iconst(types::I64, 0),
            ("of", [a], Ty::Ptr(false, _)) if matches!(a.ty, Ty::Ref(false, _)) => word(a)?,
            ("of_mut", [a], Ty::Ptr(true, _)) if matches!(a.ty, Ty::Ref(true, _)) => word(a)?,
            ("slice", [a], Ty::Ptr(false, _)) | ("slice_mut", [a], Ty::Ptr(true, _))
                if matches!(&a.ty, Ty::Ref(_, s) if matches!(s.as_ref(), Ty::Slice(_))) =>
            {
                let fat = word(a)?;
                self.b.ins().load(types::I64, cl::MemFlags::new(), fat, 0)
            }
            ("to_const", [a], Ty::Ptr(false, _)) if matches!(a.ty, Ty::Ptr(true, _)) => word(a)?,
            ("address", [a], Ty::Int(IntTy::Usize)) if matches!(a.ty, Ty::Ptr(..)) => word(a)?,
            ("from_address", [a], Ty::Ptr(true, _)) if a.ty == Ty::Int(IntTy::Usize) => word(a)?,
            // Address arithmetic only (wrapping, like `address` + `from_address`);
            // the stride is the pointee's size.
            ("offset", [p, count], Ty::Ptr(false, inner)) | ("offset_mut", [p, count], Ty::Ptr(true, inner))
                if p.ty == *dest && count.ty == Ty::Int(IntTy::Usize) =>
            {
                let stride = i64::from(layout::layout(self.t, inner)?.size);
                let bytes = self.b.ins().imul_imm(word(count)?, stride);
                self.b.ins().iadd(word(p)?, bytes)
            }
            _ => return Err(Error::bug(format!("ffi intrinsic {name} shape"))),
        };
        Ok(Some(Val { value: Some(value), ty: dest.clone() }))
    }

    fn volatile_type(ty: &Ty) -> Result<(cl::Type, u32)> {
        let t = scalar(ty).ok_or_else(|| Error::unsupported("volatile access to this type"))?;
        let int = match t { types::F32 => types::I32, types::F64 => types::I64, other => other };
        Ok((int, int.bits()))
    }

    fn volatile_finish(&mut self, v: cl::Value, _bits: u32, ty: &Ty) -> cl::Value {
        let want = scalar(ty).unwrap();
        if want == self.b.func.dfg.value_type(v) { v } else { self.b.ins().bitcast(want, cl::MemFlags::new(), v) }
    }

    /// Calls to the `arch_*` CPU intrinsics (ADR 0101, P5): typed calls to
    /// hand-assembled stubs defined in this object (see `crate::arch`).
    pub(super) fn arch(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        let Some((module_name, _)) = name.split_once('.') else { return Ok(None) };
        if crate::arch::module_arch(module_name).is_none() { return Ok(None); }
        let target = self.runtime.target.ok_or_else(|| Error::bug("no target"))?;
        let (id, stub) = self.runtime.stubs.id(self.module, target, name)?;
        if args.len() != stub.params.len() { return Err(Error::bug(format!("{name} arity"))); }
        let values: Vec<cl::Value> = args.iter().map(|a| a.value.ok_or_else(|| Error::bug("CPU intrinsic operand"))).collect::<Result<_>>()?;
        for (value, ty) in values.iter().zip(&stub.params) {
            if self.b.func.dfg.value_type(*value) != *ty { return Err(Error::bug(format!("{name} operand type"))); }
        }
        let callee = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(callee, &values);
        let value = self.b.inst_results(call).first().copied();
        Ok(Some(Val { value, ty: dest.clone() }))
    }
}
