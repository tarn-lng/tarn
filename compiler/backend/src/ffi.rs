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
}
