use super::*;

impl Cx<'_, '_> {
    pub(super) fn filesystem(
        &mut self,
        name: &str,
        args: &[Val],
        dest: &Ty,
    ) -> Result<Option<Val>> {
        let Some(id) = self.t.decls.fs_intrinsics.get(name) else {
            return Ok(None);
        };
        let sig = &self.t.decls.fns[id];
        if sig.ret != *dest
            || sig.params.len() != args.len()
            || sig.params.iter().zip(args).any(|(ty, a)| *ty != a.ty)
        {
            return Err(Error::bug("filesystem intrinsic signature mismatch"));
        }
        let result = layout::layout(self.t, dest)?;
        if result.size != 40
            || result.align != 8
            || result.fields.iter().map(|(o, _)| *o).collect::<Vec<_>>() != [0, 4, 8, 16]
        {
            return Err(Error::bug("filesystem result layout mismatch"));
        }
        let slot = self.stack(result.size, result.align);
        let out = self.b.ins().stack_addr(types::I64, slot, 0);
        let mut values = vec![out];
        for arg in args {
            let value = arg.value.ok_or_else(|| Error::bug("filesystem argument"))?;
            match &arg.ty {
                Ty::Ref(_, inner) if matches!(inner.as_ref(), Ty::Slice(_)) => {
                    values.push(self.b.ins().load(types::I64, cl::MemFlags::new(), value, 0));
                    values.push(self.b.ins().load(types::I64, cl::MemFlags::new(), value, 8));
                }
                Ty::Ref(_, inner) if **inner == Ty::Str => {
                    values.push(self.b.ins().load(types::I64, cl::MemFlags::new(), value, 0))
                }
                Ty::Adt(id, _) if Some(*id) == self.t.decls.fs_file => {
                    values.push(self.b.ins().load(types::I32, cl::MemFlags::new(), value, 0))
                }
                _ => values.push(value),
            }
        }
        let operation = name
            .strip_prefix("fs._")
            .ok_or_else(|| Error::bug("filesystem operation"))?;
        self.runtime_raw(&format!("tarn_rt_fs_{operation}"), &values, out);
        Ok(Some(Val {
            value: Some(out),
            ty: dest.clone(),
        }))
    }
}
