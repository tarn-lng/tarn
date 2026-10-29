use super::*;

// Vec<T> storage execution (phase 14A). Element ownership, loans and which
// values are initialized are frontend/post-drop decisions; this module only
// moves bytes in and out of owned heap storage and destroys what remains.
impl Cx<'_, '_> {
    fn vec_fields(&self, vec: &Ty) -> Result<(i32, i32, i32, Ty)> {
        let Ty::Adt(id, ts) = vec else { return Err(Error::bug("vector type")) };
        if Some(*id) != self.t.decls.vec || ts.len() != 1 {
            return Err(Error::bug("vector identity"));
        }
        let l = layout::layout(self.t, vec)?;
        let offset = |i: usize| l.fields.get(i).map(|(o, _)| *o as i32).ok_or_else(|| Error::bug("vector layout"));
        Ok((offset(0)?, offset(1)?, offset(2)?, ts[0].clone()))
    }

    fn vec_element(&mut self, data: cl::Value, index: cl::Value, elem: &Ty) -> Result<cl::Value> {
        let size = layout::layout(self.t, elem)?.size;
        let offset = self.b.ins().imul_imm(index, i64::from(size));
        Ok(self.b.ins().iadd(data, offset))
    }

    fn vec_bounds(&mut self, index: cl::Value, len: cl::Value) {
        let bad = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
        self.fault_if(bad);
    }

    /// Snapshot an element (owned bytes) into a fresh slot.
    fn vec_take(&mut self, addr: cl::Value, ty: &Ty) -> Result<Val> {
        let l = layout::layout(self.t, ty)?;
        let value = if l.size == 0 {
            None
        } else if let Some(st) = scalar(ty) {
            Some(self.b.ins().load(st, cl::MemFlags::new(), addr, 0))
        } else {
            let slot = self.stack(l.size, l.align);
            let copy = self.b.ins().stack_addr(types::I64, slot, 0);
            self.copy(copy, addr, l.size);
            Some(copy)
        };
        Ok(Val { value, ty: ty.clone() })
    }

    fn vec_put(&mut self, addr: cl::Value, value: &Val) -> Result<()> {
        let l = layout::layout(self.t, &value.ty)?;
        if l.size != 0 {
            let v = value.value.ok_or_else(|| Error::bug("missing vector element"))?;
            if scalar(&value.ty).is_some() {
                self.b.ins().store(cl::MemFlags::new(), v, addr, 0);
            } else {
                self.copy(addr, v, l.size);
            }
        }
        Ok(())
    }

    pub(super) fn vector(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        let Some(operation) = name.strip_prefix("Vec.") else { return Ok(None) };
        if operation == "new" {
            let (data, len, cap, _) = self.vec_fields(dest)?;
            let l = layout::layout(self.t, dest)?;
            let slot = self.stack(l.size, l.align);
            let addr = self.b.ins().stack_addr(types::I64, slot, 0);
            let zero = self.b.ins().iconst(types::I64, 0);
            for offset in [data, len, cap] {
                self.b.ins().store(cl::MemFlags::new(), zero, addr, offset);
            }
            return Ok(Some(Val { value: Some(addr), ty: dest.clone() }));
        }
        if operation == "with_capacity" && args.len() == 1 && args[0].ty == Ty::Int(IntTy::Usize) {
            let (data, len, cap, elem) = self.vec_fields(dest)?;
            let l = layout::layout(self.t, dest)?;
            let slot = self.stack(l.size, l.align);
            let addr = self.b.ins().stack_addr(types::I64, slot, 0);
            let capacity = args[0].value.ok_or_else(|| Error::bug("vector capacity"))?;
            let zero = self.b.ins().iconst(types::I64, 0);
            // Zero capacity keeps the null storage of Vec.new.
            let size = self.b.ins().iconst(types::I64, i64::from(layout::layout(self.t, &elem)?.size));
            let (alloc, done) = (self.b.create_block(), self.b.create_block());
            self.b.append_block_param(done, types::I64);
            let empty = self.b.ins().icmp_imm(IntCC::Equal, capacity, 0);
            self.b.ins().brif(empty, done, &[zero.into()], alloc, &[]);
            self.b.switch_to_block(alloc);
            let storage = self.runtime("tarn_rt_vec_grow", &[zero, capacity, size])[0];
            self.b.ins().jump(done, &[storage.into()]);
            self.b.switch_to_block(done);
            let storage = self.b.block_params(done)[0];
            self.b.ins().store(cl::MemFlags::new(), storage, addr, data);
            self.b.ins().store(cl::MemFlags::new(), zero, addr, len);
            self.b.ins().store(cl::MemFlags::new(), capacity, addr, cap);
            return Ok(Some(Val { value: Some(addr), ty: dest.clone() }));
        }
        let receiver = args.first().ok_or_else(|| Error::bug("vector receiver"))?;
        let Ty::Ref(mutable, vec) = &receiver.ty else { return Err(Error::bug("vector receiver type")) };
        let (data_at, len_at, cap_at, elem) = self.vec_fields(vec)?;
        let addr = receiver.value.ok_or_else(|| Error::bug("vector storage"))?;
        let flags = cl::MemFlags::new();
        let data = self.b.ins().load(types::I64, flags, addr, data_at);
        let len = self.b.ins().load(types::I64, flags, addr, len_at);
        let value = match (operation, args.len()) {
            ("len", 1) if *dest == Ty::Int(IntTy::Usize) => Val { value: Some(len), ty: dest.clone() },
            ("is_empty", 1) if *dest == Ty::Bool => {
                let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                Val { value: Some(empty), ty: Ty::Bool }
            }
            ("get", 2) | ("get_mut", 2) if *dest == Ty::Ref(operation == "get_mut", Box::new(elem.clone())) && (operation == "get" || *mutable) => {
                let index = args[1].value.ok_or_else(|| Error::bug("vector index"))?;
                self.vec_bounds(index, len);
                let at = self.vec_element(data, index, &elem)?;
                Val { value: Some(at), ty: dest.clone() }
            }
            ("as_slice", 1) | ("as_mut_slice", 1)
                if *dest == Ty::Ref(operation == "as_mut_slice", Box::new(Ty::Slice(Box::new(elem.clone())))) && (operation == "as_slice" || *mutable) =>
            {
                Val { value: Some(self.pair(data, len)), ty: dest.clone() }
            }
            // Consuming iteration (ADR 0086): move the element at the cursor
            // out without changing the header; the IterVec drop skips it.
            ("iter_take", 2) if *mutable && *dest == elem && args[1].ty == Ty::Int(IntTy::Usize) => {
                let index = args[1].value.ok_or_else(|| Error::bug("vector cursor"))?;
                self.vec_bounds(index, len);
                let at = self.vec_element(data, index, &elem)?;
                self.vec_take(at, &elem)?
            }
            ("at", 2) if *dest == elem && self.t.decls.is_copy(&elem) => {
                let index = args[1].value.ok_or_else(|| Error::bug("vector index"))?;
                self.vec_bounds(index, len);
                let at = self.vec_element(data, index, &elem)?;
                self.vec_take(at, &elem)?
            }
            ("extend_from_slice", 2)
                if *mutable && *dest == Ty::Void && self.t.decls.is_copy(&elem)
                    && args[1].ty == Ty::Ref(false, Box::new(Ty::Slice(Box::new(elem.clone())))) =>
            {
                if (data_at, len_at, cap_at) != (0, 8, 16) {
                    return Err(Error::bug("vector header layout for bulk append"));
                }
                let slice = args[1].value.ok_or_else(|| Error::bug("bulk append slice"))?;
                let src = self.b.ins().load(types::I64, flags, slice, 0);
                let count = self.b.ins().load(types::I64, flags, slice, 8);
                let size = self.b.ins().iconst(types::I64, i64::from(layout::layout(self.t, &elem)?.size));
                self.runtime("tarn_rt_vec_extend", &[addr, src, count, size]);
                Val { value: None, ty: Ty::Void }
            }
            ("push", 2) if *mutable && args[1].ty == elem && *dest == Ty::Void => {
                let cap = self.b.ins().load(types::I64, flags, addr, cap_at);
                let full = self.b.ins().icmp(IntCC::Equal, len, cap);
                let grow = self.b.create_block();
                let store = self.b.create_block();
                self.b.ins().brif(full, grow, &[], store, &[]);
                self.b.switch_to_block(grow);
                let doubled = self.b.ins().imul_imm(cap, 2);
                let four = self.b.ins().iconst(types::I64, 4);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, cap, 0);
                let next = self.b.ins().select(empty, four, doubled);
                let size = self.b.ins().iconst(types::I64, i64::from(layout::layout(self.t, &elem)?.size));
                let grown = self.runtime("tarn_rt_vec_grow", &[data, next, size])[0];
                self.b.ins().store(flags, grown, addr, data_at);
                self.b.ins().store(flags, next, addr, cap_at);
                self.b.ins().jump(store, &[]);
                self.b.switch_to_block(store);
                let data = self.b.ins().load(types::I64, flags, addr, data_at);
                let at = self.vec_element(data, len, &elem)?;
                self.vec_put(at, &args[1])?;
                let len = self.b.ins().iadd_imm(len, 1);
                self.b.ins().store(flags, len, addr, len_at);
                Val { value: None, ty: Ty::Void }
            }
            ("pop", 1) if *mutable => {
                let Ty::Adt(option, _) = dest else { return Err(Error::bug("vector pop result")) };
                let def = self.t.decls.enums.get(option).ok_or_else(|| Error::bug("option layout"))?;
                let none = def.variants.iter().position(|v| v.name == "None").ok_or_else(|| Error::bug("Option.None"))?;
                let some = def.variants.iter().position(|v| v.name == "Some").ok_or_else(|| Error::bug("Option.Some"))?;
                let l = layout::layout(self.t, dest)?;
                let payload = l.variants.get(some).and_then(|v| v.first()).cloned().ok_or_else(|| Error::bug("Option payload"))?;
                if payload.1 != elem {
                    return Err(Error::bug("vector pop ABI"));
                }
                let slot = self.stack(l.size, l.align);
                let out = self.b.ins().stack_addr(types::I64, slot, 0);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, len, 0);
                let (on_empty, on_value, done) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(empty, on_empty, &[], on_value, &[]);
                self.b.switch_to_block(on_empty);
                let tag = self.b.ins().iconst(types::I32, none as i64);
                self.b.ins().store(flags, tag, out, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(on_value);
                let last = self.b.ins().iadd_imm(len, -1);
                self.b.ins().store(flags, last, addr, len_at);
                let at = self.vec_element(data, last, &elem)?;
                let size = layout::layout(self.t, &elem)?.size;
                let target = self.b.ins().iadd_imm(out, i64::from(payload.0));
                self.copy(target, at, size);
                let tag = self.b.ins().iconst(types::I32, some as i64);
                self.b.ins().store(flags, tag, out, 0);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                Val { value: Some(out), ty: dest.clone() }
            }
            ("replace", 3) if *mutable && args[2].ty == elem && *dest == elem => {
                let index = args[1].value.ok_or_else(|| Error::bug("vector index"))?;
                self.vec_bounds(index, len);
                let at = self.vec_element(data, index, &elem)?;
                let old = self.vec_take(at, &elem)?;
                self.vec_put(at, &args[2])?;
                old
            }
            ("swap", 3) if *mutable && *dest == Ty::Void => {
                let (i, j) = (args[1].value.ok_or_else(|| Error::bug("vector index"))?, args[2].value.ok_or_else(|| Error::bug("vector index"))?);
                self.vec_bounds(i, len);
                self.vec_bounds(j, len);
                let (x, y) = (self.vec_element(data, i, &elem)?, self.vec_element(data, j, &elem)?);
                let first = self.vec_take(x, &elem)?;
                let size = layout::layout(self.t, &elem)?.size;
                // Equal indices: copying y onto x is a self-copy, then x is restored.
                self.copy(x, y, size);
                self.vec_put(y, &first)?;
                Val { value: None, ty: Ty::Void }
            }
            ("swap_remove", 2) if *mutable && *dest == elem => {
                let index = args[1].value.ok_or_else(|| Error::bug("vector index"))?;
                self.vec_bounds(index, len);
                let at = self.vec_element(data, index, &elem)?;
                let old = self.vec_take(at, &elem)?;
                let last = self.b.ins().iadd_imm(len, -1);
                let from = self.vec_element(data, last, &elem)?;
                let size = layout::layout(self.t, &elem)?.size;
                self.copy(at, from, size);
                self.b.ins().store(flags, last, addr, len_at);
                old
            }
            _ => return Err(Error::bug(format!("vector operation ABI: {operation}"))),
        };
        Ok(Some(value))
    }

    /// Destroy every remaining element in index order, then free storage.
    pub(super) fn drop_vector(&mut self, addr: cl::Value, vec: &Ty) -> Result<()> {
        let zero = self.b.ins().iconst(types::I64, 0);
        self.drop_vector_from(addr, vec, zero)
    }

    /// Destroy the elements from `start` to the length in index order, then
    /// free storage. Elements before `start` were moved out by a consuming
    /// `for` loop (ADR 0086) and are not read.
    pub(super) fn drop_vector_from(&mut self, addr: cl::Value, vec: &Ty, start: cl::Value) -> Result<()> {
        let (data_at, len_at, _, elem) = self.vec_fields(vec)?;
        let flags = cl::MemFlags::new();
        let data = self.b.ins().load(types::I64, flags, addr, data_at);
        let len = self.b.ins().load(types::I64, flags, addr, len_at);
        if !self.t.decls.is_copy(&elem) && !matches!(elem, Ty::Ref(..)) {
            let header = self.b.create_block();
            let body = self.b.create_block();
            let exit = self.b.create_block();
            self.b.append_block_param(header, types::I64);
            self.b.ins().jump(header, &[start.into()]);
            self.b.switch_to_block(header);
            let i = self.b.block_params(header)[0];
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
            self.b.ins().brif(more, body, &[], exit, &[]);
            self.b.switch_to_block(body);
            let at = self.vec_element(data, i, &elem)?;
            let glue = self.element_glue(&elem)?;
            let target = self.module.declare_func_in_func(glue, self.b.func);
            self.b.ins().call(target, &[at]);
            let next = self.b.ins().iadd_imm(i, 1);
            self.b.ins().jump(header, &[next.into()]);
            self.b.switch_to_block(exit);
        }
        self.runtime("tarn_rt_env_free", &[data]);
        Ok(())
    }
}
