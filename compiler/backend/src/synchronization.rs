use super::*;

// Private synchronization execution. Types, loans, and initialized destruction
// are frontend/post-drop decisions; this module only executes their contracts.
impl Cx<'_, '_> {
    pub(super) fn synchronization(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        let Some((owner, operation)) = name.split_once('.') else { return Ok(None) };
        if owner == "Mutex" && operation == "new" {
            let Ty::Adt(id, ts) = dest else { return Err(Error::bug("mutex construction type")) };
            if Some(*id) != self.t.decls.mutex || ts.len() != 1 || args.len() != 1 || args[0].ty != ts[0] {
                return Err(Error::bug("mutex construction ABI"));
            }
            let addr = self.owned_slot(dest)?;
            let native = self.runtime("tarn_rt_mutex_create", &[])[0];
            self.b.ins().store(cl::MemFlags::new(), native, addr, 0);
            let payload = self.b.ins().iadd_imm(addr, i64::from(layout::layout(self.t, dest)?.fields[1].0));
            self.owned_write(payload, &args[0])?;
            return Ok(Some(Val { value: Some(addr), ty: dest.clone() }));
        }
        if owner == "Mutex" && operation == "lock" {
            let [receiver] = args else { return Err(Error::bug("mutex lock arity")) };
            let Ty::Ref(false, mutex) = &receiver.ty else { return Err(Error::bug("mutex lock receiver")) };
            let Ty::Adt(id, ts) = mutex.as_ref() else { return Err(Error::bug("mutex lock type")) };
            if Some(*id) != self.t.decls.mutex || ts.len() != 1 || !matches!(dest, Ty::Adt(g, gs) if Some(*g) == self.t.decls.mutex_guard && gs == ts) {
                return Err(Error::bug("mutex lock result"));
            }
            let addr = receiver.value.ok_or_else(|| Error::bug("mutex receiver storage"))?;
            let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_mutex_lock", &[native]);
            let payload = self.b.ins().iadd_imm(addr, i64::from(layout::layout(self.t, mutex)?.fields[1].0));
            let guard = self.pair(native, payload);
            return Ok(Some(Val { value: Some(guard), ty: dest.clone() }));
        }
        if owner == "MutexGuard" {
            let receiver = args.first().ok_or_else(|| Error::bug("guard receiver arity"))?;
            let Ty::Ref(mutable, guard) = &receiver.ty else { return Err(Error::bug("guard receiver")) };
            let Ty::Adt(id, ts) = guard.as_ref() else { return Err(Error::bug("guard type")) };
            if Some(*id) != self.t.decls.mutex_guard || ts.len() != 1 { return Err(Error::bug("guard identity")); }
            let ty = &ts[0];
            let addr = receiver.value.ok_or_else(|| Error::bug("guard storage"))?;
            let payload = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 8);
            let value = match operation {
                "value" if *mutable && args.len() == 1 && *dest == Ty::Ref(true, Box::new(ty.clone())) => Val { value: Some(payload), ty: dest.clone() },
                "read" if args.len() == 1 && dest == ty && self.t.decls.is_copy(ty) => self.owned_read(payload, ty)?,
                "replace" if *mutable && args.len() == 2 && dest == ty && args[1].ty == *ty => {
                    let old = self.owned_read(payload, ty)?;
                    self.owned_write(payload, &args[1])?;
                    old
                }
                _ => return Err(Error::bug("guard operation ABI")),
            };
            return Ok(Some(value));
        }
        let atomic = match owner {
            "AtomicBool" => "bool", "AtomicI32" => "i32", "AtomicI64" => "i64",
            "AtomicU32" => "u32", "AtomicU64" => "u64", "AtomicUsize" => "usize",
            _ => return Ok(None),
        };
        let (id, scalar_ty) = if operation == "new" {
            let Ty::Adt(id, _) = dest else { return Err(Error::bug("atomic constructor result")) };
            (*id, self.t.decls.atomics.get(id).ok_or_else(|| Error::bug("atomic identity"))?.clone())
        } else {
            let Some(Val { ty: Ty::Ref(false, inner), .. }) = args.first() else { return Err(Error::bug("atomic receiver")) };
            let Ty::Adt(id, _) = inner.as_ref() else { return Err(Error::bug("atomic receiver type")) };
            (*id, self.t.decls.atomics.get(id).ok_or_else(|| Error::bug("atomic identity"))?.clone())
        };
        if (match atomic { "bool" => Ty::Bool, "i32" => Ty::Int(IntTy::I32), "i64" => Ty::Int(IntTy::I64), "u32" => Ty::Int(IntTy::U32), "u64" => Ty::Int(IntTy::U64), _ => Ty::Int(IntTy::Usize) }) != scalar_ty { return Err(Error::bug("atomic scalar identity")); }
        let arity = match operation { "new" | "load" => 1, "compare_exchange" => 3, "store" | "swap" | "fetch_add" | "fetch_sub" => 2, _ => return Err(Error::bug("atomic operation")) };
        let result_ty = match operation { "new" => Ty::Adt(id, vec![]), "store" => Ty::Void, "compare_exchange" => Ty::Bool, _ => scalar_ty.clone() };
        if args.len() != arity || *dest != result_ty || (atomic == "bool" && operation.starts_with("fetch_")) || args.iter().skip(usize::from(operation != "new")).any(|a| a.ty != scalar_ty) {
            return Err(Error::bug("atomic operation ABI"));
        }
        let mut values = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let mut v = arg.value.ok_or_else(|| Error::bug("atomic argument"))?;
            if i == 0 && operation != "new" { v = self.b.ins().load(types::I64, cl::MemFlags::new(), v, 0); }
            values.push(v);
        }
        let result = self.runtime(&format!("tarn_rt_atomic_{atomic}_{operation}"), &values);
        let value = if operation == "new" {
            let addr = self.owned_slot(dest)?;
            self.b.ins().store(cl::MemFlags::new(), result[0], addr, 0);
            Some(addr)
        } else { result.first().copied() };
        Ok(Some(Val { value, ty: dest.clone() }))
    }
}
