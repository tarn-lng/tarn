//! `static` items (ADR 0101, P6): fixed-layout global data. Bytes come from
//! the checker's evaluated initializer and the shared C-compatible layout, so
//! a `static` is exactly what a linker script or boot loader expects to find.
use crate::{Error, Result, Target};
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use cranelift_object::ObjectModule;
use std::collections::HashMap;
use tarn_resolve::SymbolId;
use tarn_types::statics::StaticInit;
use tarn_types::{FloatTy, Ty, Typed};

/// Largest object a static may be (a page-table tree or boot stack is far
/// smaller); keeps a typo like `[4000000000]u8` from allocating gigabytes.
const MAX_STATIC_SIZE: u32 = 1 << 30;

/// Every static of the program, sorted by symbol id.
fn sorted(t: &Typed) -> Vec<SymbolId> {
    let mut ids: Vec<SymbolId> = t.decls.statics.keys().copied().collect();
    ids.sort_by_key(|s| s.0);
    ids
}

/// Does `name` collide with Tarn's own symbols? Only the freestanding contract
/// may be defined by the program, and only on `*-none` targets.
pub(crate) fn check_export_name(name: &str, target: Target, is_function: bool) -> Result<()> {
    if name == "main" && target.os != crate::Os::None {
        return Err(Error::unsupported("`#[export(\"main\")]` would replace the program's C `main` shim"));
    }
    if name.starts_with("tarn_") {
        let contract = is_function && target.os == crate::Os::None && crate::freestanding::CONTRACT.contains(&name);
        if !contract {
            return Err(Error::unsupported(format!(
                "`#[export(\"{name}\")]`: names starting with `tarn_` belong to the compiler{}",
                if is_function { "; on a `*-none` target only the freestanding contract may be defined" } else { "" }
            )));
        }
    }
    Ok(())
}

/// Declare every static. Definitions come later: initializers may hold
/// addresses of other statics.
pub(crate) fn declare(module: &mut ObjectModule, t: &Typed, target: Target) -> Result<HashMap<SymbolId, DataId>> {
    let mut ids = HashMap::new();
    for (n, sym) in sorted(t).into_iter().enumerate() {
        let def = &t.decls.statics[&sym];
        if def.section.is_some() && target.is_macos() {
            return Err(Error::unsupported(format!("static `{}`: `#[section]` is supported on ELF targets only, not {target}", def.name)));
        }
        let linkage = match &def.export {
            Some(name) => {
                check_export_name(name, target, false)?;
                Linkage::Export
            }
            None => Linkage::Local,
        };
        let name = def.export.clone().unwrap_or_else(|| format!("tarn_static_{n}__{}", def.name));
        let id = module.declare_data(&name, linkage, def.mutable, false).map_err(|e| Error::unsupported(format!("static `{}`: {e}", def.name)))?;
        ids.insert(sym, id);
    }
    Ok(ids)
}

pub(crate) fn define(module: &mut ObjectModule, t: &Typed, ids: &HashMap<SymbolId, DataId>) -> Result<()> {
    for sym in sorted(t) {
        let def = &t.decls.statics[&sym];
        let layout = tarn_ir::layout::static_layout(t, &def.ty).map_err(|e| Error::unsupported(format!("static `{}`: {}", def.name, e.message)))?;
        if layout.size > MAX_STATIC_SIZE {
            return Err(Error::unsupported(format!("static `{}` is {} bytes; the limit is 1 GiB", def.name, layout.size)));
        }
        let mut description = DataDescription::new();
        let mut relocs = Vec::new();
        let mut bytes = vec![0u8; layout.size as usize];
        if def.init != StaticInit::Zero || def.section.is_some() {
            fill(t, &def.ty, &def.init, 0, &mut bytes, &mut relocs)?;
            description.define(bytes.into_boxed_slice());
        } else {
            description.define_zeroinit(layout.size as usize);
        }
        for (offset, target) in relocs {
            let id = *ids.get(&target).ok_or_else(|| Error::bug("address of an undeclared static"))?;
            let gv = module.declare_data_in_data(id, &mut description);
            description.write_data_addr(offset, gv, 0);
        }
        let align = def.align.unwrap_or(1).max(layout.align);
        description.set_align(u64::from(align));
        if let Some(section) = &def.section {
            description.set_segment_section("", section.as_str());
            // Keep the object alive under `--gc-sections`: nothing in the
            // program refers to a boot-loader request, but the loader does.
            description.set_used(true);
        }
        module.define_data(ids[&sym], &description).map_err(|e| Error::bug(format!("static `{}`: {e}", def.name)))?;
    }
    Ok(())
}

fn fill(t: &Typed, ty: &Ty, init: &StaticInit, at: usize, buf: &mut [u8], relocs: &mut Vec<(u32, SymbolId)>) -> Result<()> {
    let size = |ty: &Ty| tarn_ir::layout::static_layout(t, ty).map(|l| l.size as usize).map_err(|e| Error::unsupported(e.message));
    match (init, ty) {
        (StaticInit::Zero, _) => {}
        (StaticInit::Int(v), Ty::Int(_)) => {
            let n = size(ty)?;
            buf[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
        }
        (StaticInit::Bool(b), Ty::Bool) => buf[at] = u8::from(*b),
        (StaticInit::Float(text), Ty::Float(f)) => {
            let v: f64 = text.replace('_', "").parse().map_err(|_| Error::bug("invalid float constant"))?;
            match f {
                FloatTy::F32 => buf[at..at + 4].copy_from_slice(&(v as f32).to_le_bytes()),
                FloatTy::F64 => buf[at..at + 8].copy_from_slice(&v.to_le_bytes()),
            }
        }
        (StaticInit::Address(a), Ty::Ptr(..)) => buf[at..at + 8].copy_from_slice(&a.to_le_bytes()),
        (StaticInit::Static(s), Ty::Ptr(..)) => relocs.push((at as u32, *s)),
        (StaticInit::Aggregate(items), Ty::Array(elem, _)) => {
            let step = size(elem)?;
            for (i, item) in items.iter().enumerate() {
                fill(t, elem, item, at + i * step, buf, relocs)?;
            }
        }
        (StaticInit::Aggregate(items), Ty::Adt(..)) => {
            let layout = tarn_ir::layout::static_layout(t, ty).map_err(|e| Error::unsupported(e.message))?;
            if layout.fields.len() != items.len() {
                return Err(Error::bug("static initializer does not match its struct"));
            }
            for ((offset, field), item) in layout.fields.iter().zip(items) {
                fill(t, field, item, at + *offset as usize, buf, relocs)?;
            }
        }
        _ => return Err(Error::bug("static initializer does not match its type")),
    }
    Ok(())
}
