//! Execute verified initialized-place exchanges and explicit empty-storage checks.
//! No allocation or drop occurs between loading and replacing an owned slot.
use super::*;

impl Cx<'_, '_> {
    pub(super) fn owned_slot(&mut self, ty: &Ty) -> Result<cl::Value> {
        let layout = layout::layout(self.t, ty)?;
        let slot = self.stack(layout.size, layout.align);
        Ok(self.b.ins().stack_addr(types::I64, slot, 0))
    }

    pub(super) fn owned_read(&mut self, addr: cl::Value, ty: &Ty) -> Result<Val> {
        let layout = layout::layout(self.t, ty)?;
        let value = if layout.size == 0 { None }
            else if let Some(st) = scalar(ty) { Some(self.b.ins().load(st, cl::MemFlags::new(), addr, 0)) }
            else {
                // Snapshot before replace writes the payload. In particular an
                // aggregate result must not alias the newly installed value.
                let copy = self.owned_slot(ty)?;
                self.copy(copy, addr, layout.size);
                Some(copy)
            };
        Ok(Val { value, ty: ty.clone() })
    }

    pub(super) fn owned_write(&mut self, addr: cl::Value, value: &Val) -> Result<()> {
        let l = layout::layout(self.t, &value.ty)?;
        if l.size != 0 {
            let v = value.value.ok_or_else(|| Error::bug("missing synchronization payload"))?;
            if scalar(&value.ty).is_some() { self.b.ins().store(cl::MemFlags::new(), v, addr, 0); }
            else { self.copy(addr, v, l.size); }
        }
        Ok(())
    }

    /// Runtime assertion used by the explicit empty-storage consumption API.
    /// A nominal resource can never be empty; Option/Vec supply empty states.
    fn assert_empty_owned(&mut self, addr: cl::Value, ty: &Ty) -> Result<()> {
        if !self.t.decls.is_must_consume(ty) { return Ok(()); }
        match ty {
            Ty::Array(elem, n) => {
                let size = layout::layout(self.t, elem)?.size;
                for i in 0..*n {
                    let at = self.b.ins().iadd_imm(addr, (i * u64::from(size)) as i64);
                    self.assert_empty_owned(at, elem)?;
                }
            }
            Ty::Adt(id, _) if Some(*id) == self.t.decls.vec => {
                let l = layout::layout(self.t, ty)?;
                let len = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, l.fields[1].0 as i32);
                let live = self.b.ins().icmp_imm(IntCC::NotEqual, len, 0);
                self.fault_if(live);
            }
            Ty::Adt(id, _) if self.t.decls.must_consume.contains(id) => {
                let live = self.b.ins().iconst(types::I8, 1);
                self.fault_if(live);
            }
            Ty::Adt(..) => {
                let l = layout::layout(self.t, ty)?;
                if l.variants.is_empty() {
                    // An owning generic argument may describe opaque storage
                    // behind a raw handle. Only Vec has a verified empty-header
                    // protocol; scalar fields cannot prove such an owner empty.
                    if !l.fields.iter().any(|(_, field)| self.t.decls.is_must_consume(field)) {
                        let live = self.b.ins().iconst(types::I8, 1);
                        self.fault_if(live);
                    }
                    for (offset, field) in l.fields {
                        let at = self.b.ins().iadd_imm(addr, i64::from(offset));
                        self.assert_empty_owned(at, &field)?;
                    }
                } else {
                    let tag = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
                    let done = self.b.create_block();
                    for (index, fields) in l.variants.iter().enumerate() {
                        let (arm, next) = (self.b.create_block(), self.b.create_block());
                        let active = self.b.ins().icmp_imm(IntCC::Equal, tag, index as i64);
                        self.b.ins().brif(active, arm, &[], next, &[]);
                        self.b.switch_to_block(arm);
                        for (offset, field) in fields {
                            let at = self.b.ins().iadd_imm(addr, i64::from(*offset));
                            self.assert_empty_owned(at, field)?;
                        }
                        self.b.ins().jump(done, &[]);
                        self.b.switch_to_block(next);
                    }
                    // Invalid tags are outside the initialized-value contract.
                    self.b.ins().jump(done, &[]);
                    self.b.switch_to_block(done);
                }
            }
            _ => return Err(Error::bug("unsupported empty owning storage")),
        }
        Ok(())
    }

    pub(super) fn owning_slot(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        if name == "core.finish_empty" {
            let [value] = args else { return Err(Error::bug("finish_empty arity")); };
            if *dest != Ty::Void { return Err(Error::bug("finish_empty result")); }
            if layout::layout(self.t, &value.ty)?.size == 0 && !self.t.decls.is_must_consume(&value.ty) {
                return Ok(Some(Val { value: None, ty: Ty::Void }));
            }
            // Aggregates are passed by address; scalars carry no obligations.
            if matches!(value.ty, Ty::Adt(..) | Ty::Array(..)) {
                let addr = value.value.unwrap_or_else(|| self.b.ins().iconst(types::I64, 0));
                self.assert_empty_owned(addr, &value.ty)?;
                self.drop_at(addr, &value.ty)?;
            } else if self.needs_drop(&value.ty) {
                let addr = self.owned_slot(&value.ty)?;
                self.owned_write(addr, value)?;
                self.drop_at(addr, &value.ty)?;
            }
            return Ok(Some(Val { value: None, ty: Ty::Void }));
        }
        if !matches!(name, "core.replace_owned" | "ffi._replace_owned" | "ffi._write_owned" | "ffi._take_owned") { return Ok(None); }
        let arity = if name == "ffi._take_owned" { 1 } else { 2 };
        if args.len() != arity || (name == "ffi._write_owned" && *dest != Ty::Void) {
            return Err(Error::bug("owned slot ABI"));
        }
        let pointer = &args[0];
        let ty = match &pointer.ty {
            Ty::Ref(true, ty) if name == "core.replace_owned" => ty.as_ref(),
            Ty::Ptr(true, ty) if name.starts_with("ffi.") => ty.as_ref(),
            _ => return Err(Error::bug("owned slot pointer")),
        };
        let addr = pointer.value.ok_or_else(|| Error::bug("owned slot address"))?;
        let old = if name == "ffi._write_owned" { Val { value: None, ty: Ty::Void } }
            else { if dest != ty { return Err(Error::bug("owned slot result")); } self.owned_read(addr, ty)? };
        if name != "ffi._take_owned" {
            let value = args.get(1).ok_or_else(|| Error::bug("owned replacement"))?;
            if value.ty != *ty { return Err(Error::bug("owned replacement type")); }
            self.owned_write(addr, value)?;
        }
        Ok(Some(old))
    }

}
