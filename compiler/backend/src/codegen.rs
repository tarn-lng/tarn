//! Cranelift-specific lowering. Scalars use frontend SSA variables unless
//! address-taken; aggregate values use canonical-layout stack memory.
use crate::{Error, Result, layout};
use cranelift_codegen::{
    ir::{
        self as cl, InstBuilder,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tarn_ir::{self as ir, post_drop as post, *};
use tarn_types::{FloatTy, IntTy, Ty, Typed};

pub fn int_bits(t: IntTy) -> u16 {
    match t {
        IntTy::I8 | IntTy::U8 => 8,
        IntTy::I16 | IntTy::U16 => 16,
        IntTy::I32 | IntTy::U32 => 32,
        _ => 64,
    }
}
fn int_fits(n: i128, t: IntTy) -> bool {
    let bits = u32::from(int_bits(t));
    let signed = matches!(t, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize);
    if signed { n >= -(1i128 << (bits - 1)) && n < (1i128 << (bits - 1)) } else { n >= 0 && n < (1i128 << bits) }
}
fn scalar(ty: &Ty) -> Option<cl::Type> {
    Some(match ty {
        Ty::Bool => types::I8,
        Ty::Int(i) => match int_bits(*i) {
            8 => types::I8,
            16 => types::I16,
            32 => types::I32,
            _ => types::I64,
        },
        Ty::Float(FloatTy::F32) => types::F32,
        Ty::Float(_) => types::F64,
        Ty::Ref(_, inner) if !matches!(inner.as_ref(), Ty::Slice(_) | Ty::Any(_)) => types::I64,
        Ty::Str | Ty::Ptr(..) => types::I64,
        _ => return None,
    })
}
fn abi_type(t: &Typed, ty: &Ty) -> Result<Option<cl::Type>> {
    let l = layout::layout(t, ty)?;
    Ok(if l.size == 0 { None } else { Some(scalar(ty).unwrap_or(types::I64)) })
}
fn frame(f: &ir::Function) -> Option<&AsyncFrame> {
    f.asynchronous.as_ref().and_then(|a| a.frame.as_ref())
}
/// Mechanical placement of a verified async frame (ADR 0037): an 8-byte
/// destruction header, the listed locals, then every drop flag.
struct FrameLayout {
    size: u32,
    locals: HashMap<LocalId, u32>,
    flags: Vec<u32>,
    /// Borrowed closure environments whose storage witness is frame-resident.
    environments: HashMap<LocalId, u32>,
}
fn frame_layout(t: &Typed, p: &post::Program, f: &post::Function) -> Result<FrameLayout> {
    let frame = frame(&f.decl).ok_or_else(|| Error::bug("frame layout of non-async function"))?;
    let mut offset = 8u32;
    let mut locals = HashMap::new();
    let grow = |offset: u32, size: u32| offset.checked_add(size).filter(|n| *n <= 65536).ok_or_else(|| Error::unsupported("async frame exceeds 64 KiB"));
    for l in &frame.stored {
        let lyt = layout::layout(t, &f.decl.local(*l).ty)?;
        offset = (offset + lyt.align.max(1) - 1) & !(lyt.align.max(1) - 1);
        locals.insert(*l, offset);
        offset = grow(offset, lyt.size)?;
    }
    let mut flags = Vec::new();
    for flag in &f.flags {
        flags.push(offset);
        let n = match flag {
            post::FlagKind::Value(_) => 1,
            post::FlagKind::Elements(_, n) => u32::try_from(*n).map_err(|_| Error::unsupported("bitmap too large"))?.max(1),
        };
        offset = grow(offset, n)?;
    }
    let mut environments = HashMap::new();
    for b in &f.blocks {
        for s in &b.stmts {
            if let post::Op::Plain(StatementKind::Assign(_, Rvalue::Aggregate(Aggregate::Closure(_, Some(storage)), _))) = &s.op
                && locals.contains_key(&storage.local)
                && !environments.contains_key(&storage.local)
            {
                // Sized by the backend from the closure's own environment; see `closure_env_size`.
                environments.insert(storage.local, 0);
            }
        }
    }
    frame_environments(t, p, f, FrameLayout { size: offset.max(8), locals, flags, environments })
}
/// Place frame-resident closure environments after the frame's other slots.
fn frame_environments(t: &Typed, p: &post::Program, f: &post::Function, mut placement: FrameLayout) -> Result<FrameLayout> {
    let mut offset = placement.size;
    let mut placed = HashMap::new();
    for b in &f.blocks {
        for s in &b.stmts {
            if let post::Op::Plain(StatementKind::Assign(_, Rvalue::Aggregate(Aggregate::Closure(id, Some(storage)), _))) = &s.op
                && placement.environments.contains_key(&storage.local)
            {
                let (size, _) = environment(t, &p.functions[id.0 as usize].decl)?;
                let at = (offset + 7) & !7;
                let previous = placed.get(&storage.local).copied();
                if previous.is_none_or(|(_, s)| s < size) {
                    placed.insert(storage.local, (at, size));
                    offset = at.checked_add(size).filter(|n| *n <= 65536).ok_or_else(|| Error::unsupported("async frame exceeds 64 KiB"))?;
                }
            }
        }
    }
    placement.environments = placed.into_iter().map(|(l, (at, _))| (l, at)).collect();
    placement.size = offset.max(8);
    Ok(placement)
}
fn signature(module: &ObjectModule, t: &Typed, f: &ir::Function) -> Result<cl::Signature> {
    let mut sig = module.make_signature();
    if layout::layout(t, &f.ret)?.size > 0 && scalar(&f.ret).is_none() {
        sig.params.push(cl::AbiParam::new(types::I64));
    }
    if frame(f).is_some() {
        // (frame, waker, abandon); the result is Progress<T>.
        sig.params.extend([cl::AbiParam::new(types::I64), cl::AbiParam::new(types::I64), cl::AbiParam::new(types::I8)]);
        if let Some(ty) = scalar(&f.ret) {
            sig.returns.push(cl::AbiParam::new(ty));
        }
        return Ok(sig);
    }
    for p in f.params() {
        if let Some(ty) = abi_type(t, &f.local(p).ty)? {
            sig.params.push(cl::AbiParam::new(ty));
        }
    }
    if let Some(ty) = abi_type(t, &f.ret)?
        && scalar(&f.ret).is_some()
    {
        sig.returns.push(cl::AbiParam::new(ty));
    }
    Ok(sig)
}

fn capture_count(f: &ir::Function) -> usize {
    match &f.kind {
        FnKind::Closure { captures, .. } => captures.len(),
        _ => 0,
    }
}
fn environment(t: &Typed, f: &ir::Function) -> Result<(u32, Vec<(u32, Ty)>)> {
    let mut offset = 8;
    let mut fields = Vec::new();
    let env = match &f.kind {
        FnKind::Closure { environment, .. } => environment.clone(),
        _ => Vec::new(),
    };
    for ty in env {
        let layout = layout::layout(t, &ty)?;
        offset = (offset + layout.align - 1) & !(layout.align - 1);
        fields.push((offset, ty));
        offset = offset.checked_add(layout.size).filter(|n| *n <= 65536).ok_or_else(|| Error::unsupported("closure environment exceeds 64 KiB"))?;
    }
    Ok((offset.max(1), fields))
}
/// `extern "C"` (ADR 0047): native Linux x86_64/AArch64 scalars only. Narrow integers and
/// bool are extended by the caller as C compilers expect.
fn c_type(ty: &Ty) -> Option<cl::AbiParam> {
    let ext = |t, signed: bool| if signed { cl::AbiParam::new(t).sext() } else { cl::AbiParam::new(t).uext() };
    Some(match ty {
        Ty::Bool => ext(types::I8, false),
        Ty::Int(i) => {
            let signed = matches!(i, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize);
            match int_bits(*i) {
                8 => ext(types::I8, signed),
                16 => ext(types::I16, signed),
                32 => ext(types::I32, signed),
                _ => cl::AbiParam::new(types::I64),
            }
        }
        Ty::Float(FloatTy::F32) => cl::AbiParam::new(types::F32),
        Ty::Float(_) => cl::AbiParam::new(types::F64),
        Ty::Ptr(..) => cl::AbiParam::new(types::I64),
        _ => return None,
    })
}
fn c_signature_types(f: &ir::Function) -> Result<(Vec<cl::AbiParam>, Option<cl::AbiParam>)> {
    let unsupported = |ty: &Ty| Error::unsupported(format!("extern \"C\" fn {}: type {ty:?} has no C ABI in v0 (scalars only)", c_symbol(f)));
    let params = f.params().map(|l| c_type(&f.local(l).ty).ok_or_else(|| unsupported(&f.local(l).ty))).collect::<Result<_>>()?;
    let ret = if f.ret == Ty::Void { None } else { Some(c_type(&f.ret).ok_or_else(|| unsupported(&f.ret))?) };
    Ok((params, ret))
}
fn c_signature(module: &ObjectModule, f: &ir::Function) -> Result<cl::Signature> {
    let (params, ret) = c_signature_types(f)?;
    let mut sig = module.make_signature();
    sig.params = params;
    sig.returns.extend(ret);
    Ok(sig)
}
/// Monomorphization suffixes instance names; C symbols are the source name.
fn c_symbol(f: &ir::Function) -> &str {
    f.name.split("::").next().unwrap_or(&f.name)
}
fn closure_signature(module: &ObjectModule, t: &Typed, f: &ir::Function) -> Result<cl::Signature> {
    let mut sig = signature(module, t, f)?;
    if frame(f).is_some() {
        // Polled as `mut fn(&Waker) Progress<T>`: drop the abandon lane.
        sig.params.pop();
        return Ok(sig);
    }
    let sret = usize::from(layout::layout(t, &f.ret)?.size > 0 && scalar(&f.ret).is_none());
    let capture_lanes = f.params().take(capture_count(f)).filter(|l| layout::layout(t, &f.local(*l).ty).is_ok_and(|l| l.size > 0)).count();
    sig.params.splice(sret..sret + capture_lanes, [cl::AbiParam::new(types::I64)]);
    Ok(sig)
}
/// Poll adapter (`mut fn(&Waker)` ABI) and frame destruction for one async
/// body. Destruction runs the body's verified abandonment path, then frees.
fn emit_frame_thunks(module: &mut ObjectModule, t: &Typed, f: &ir::Function, body: FuncId, poll: FuncId, drop: FuncId, free: FuncId) -> Result<()> {
    let sret = layout::layout(t, &f.ret)?.size > 0 && scalar(&f.ret).is_none();
    let size = layout::layout(t, &f.ret)?;
    for (thunk, abandon) in [(poll, false), (drop, true)] {
        let mut ctx = module.make_context();
        ctx.func.signature = if abandon {
            let mut sig = module.make_signature();
            sig.params.push(cl::AbiParam::new(types::I64));
            sig
        } else {
            closure_signature(module, t, f)?
        };
        let mut fb = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let params = b.block_params(entry).to_vec();
            let target = module.declare_func_in_func(body, b.func);
            let mut values = Vec::new();
            if abandon {
                if sret {
                    let slot = b.create_sized_stack_slot(cl::StackSlotData::new(cl::StackSlotKind::ExplicitSlot, size.size.max(1), size.align.trailing_zeros() as u8));
                    values.push(b.ins().stack_addr(types::I64, slot, 0));
                }
                let zero = b.ins().iconst(types::I64, 0);
                let yes = b.ins().iconst(types::I8, 1);
                values.extend([params[0], zero, yes]);
            } else {
                values.extend_from_slice(&params);
                let no = b.ins().iconst(types::I8, 0);
                values.push(no);
            }
            let call = b.ins().call(target, &values);
            let results = b.inst_results(call).to_vec();
            if abandon {
                let free = module.declare_func_in_func(free, b.func);
                b.ins().call(free, &[params[0]]);
                b.ins().return_(&[]);
            } else {
                b.ins().return_(&results);
            }
            b.seal_all_blocks();
            b.finalize();
        }
        cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
        module.define_function(thunk, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
    }
    Ok(())
}
pub(crate) fn verify_dynamic(p: &post::Program, t: &Typed) -> Result<()> {
    for f in &p.functions {
        if f.decl.locals.iter().any(|local| matches!(local.ty, Ty::Any(_))) {
            return Err(Error::unsupported("owned dynamic interface"));
        }
        for b in &f.blocks {
            for stmt in &b.stmts {
                if let post::Op::Plain(StatementKind::Assign(_, Rvalue::Coerce(CoerceKind::DynTable { interface, concrete, methods }, source, ty))) = &stmt.op {
                    let source_place = match source {
                        Operand::Copy(place) | Operand::Move(place) => place,
                        _ => return Err(Error::bug("invalid dynamic source")),
                    };
                    let source_ty = post::place_ty(&f.decl, t, source_place).ok_or_else(|| Error::bug("invalid dynamic source place"))?;
                    if !matches!((&source_ty,ty),(Ty::Ref(sm,inner),Ty::Ref(dm,_)) if inner.as_ref()==concrete && (*sm || !*dm)) {
                        return Err(Error::bug("invalid dynamic source/mutability"));
                    }
                    validate_table(p, t, *interface, concrete, methods, ty)?;
                }
            }
            if let Terminator::Call { callee: Callee::Virtual { method, .. }, args, dest, .. } = &b.term {
                let receiver = args.first().ok_or_else(|| Error::bug("missing virtual receiver"))?;
                let place = match receiver {
                    Operand::Copy(p) | Operand::Move(p) => p,
                    _ => return Err(Error::bug("invalid dynamic representation")),
                };
                let ty = post::place_ty(&f.decl, t, place).ok_or_else(|| Error::bug("invalid dynamic place"))?;
                let Ty::Ref(mutable, inner) = ty else {
                    return Err(Error::bug("invalid dynamic representation"));
                };
                let Ty::Any(interface) = *inner else {
                    return Err(Error::bug("virtual receiver is not dynamic"));
                };
                if !p
                    .functions
                    .iter()
                    .flat_map(|f| &f.blocks)
                    .flat_map(|b| &b.stmts)
                    .any(|s| matches!(&s.op,post::Op::Plain(StatementKind::Assign(_,Rvalue::Coerce(CoerceKind::DynTable{interface:i,..},_,_))) if *i==interface))
                {
                    return Err(Error::bug("missing reachable vtable"));
                }
                if !t.decls.interfaces.get(&interface).is_some_and(|order| order.contains(method)) {
                    return Err(Error::bug("method outside vtable"));
                }
                let sig = t.decls.fns.get(method).ok_or_else(|| Error::bug("missing virtual signature"))?;
                if sig.receiver == Some(tarn_types::ReceiverKind::RefMut) && !mutable {
                    return Err(Error::bug("invalid mutable dynamic receiver"));
                }
                if post::place_ty(&f.decl, t, dest).as_ref() != Some(&sig.ret) {
                    return Err(Error::bug("dynamic result mismatch"));
                }
            }
        }
    }
    Ok(())
}
pub fn emit(p: &post::Program, t: &Typed) -> Result<Vec<u8>> {
    emit_entry(p, t, false)
}

pub fn emit_entry(p: &post::Program, t: &Typed, test: bool) -> Result<Vec<u8>> {
    let main = if test { p.functions.first() } else { p.functions.iter().find(|f| f.decl.name == "main" && f.decl.symbol.and_then(|s| t.decls.fns.get(&s)).is_some_and(|sig| sig.module.0 == 0)) }.ok_or_else(|| Error::unsupported("program requires fn main()"))?;
    let result_main = matches!(&main.decl.ret, Ty::Adt(id, args) if Some(*id) == t.decls.result && args.len() == 2 && args[0] == Ty::Void && matches!(&args[1], Ty::Adt(e, ts) if Some(*e) == t.decls.net_error && ts.is_empty()));
    if test {
        if main.decl.param_count != 1 || main.decl.ret != Ty::Void || main.decl.locals.get(1).map(|l| &l.ty) != Some(&Ty::Int(tarn_types::IntTy::I32)) {
            return Err(Error::bug("test entry must accept one i32 and return void"));
        }
    } else if main.decl.param_count != 0 || (main.decl.ret != Ty::Void && !result_main) {
        return Err(Error::unsupported("entry must be fn main() or fn main() Result<void, net.Error>"));
    }
    // Only reachable functions are code-generated. Unsupported unused stdlib
    // declarations and generic helpers do not prevent scalar executables.
    // Monomorphization already retained exactly reachable instances, including
    // closure bodies and referenced function values.
    let reachable: HashSet<_> = p.functions.iter().map(|f| f.decl.id).collect();
    for f in &p.functions {
        if f.decl.kind == FnKind::Extern {
            c_signature_types(&f.decl)?;
        } else if f.blocks.is_empty() {
            return Err(Error::unsupported(format!("function {} has no native body", f.decl.name)));
        }
    }
    verify_dynamic(p, t)?;
    let mut settings = settings::builder();
    settings.set("opt_level", "speed").map_err(|e| Error::bug(e.to_string()))?;
    // AArch64 non-PIC symbol addresses use relocations that cranelift-object
    // 0.125.3 cannot emit. Its PIC/GOT relocations are supported by ELF.
    settings.set("is_pic", if cfg!(target_arch = "aarch64") { "true" } else { "false" }).map_err(|e| Error::bug(e.to_string()))?;
    // Cranelift's IR verifier checks this backend, not the user's program.
    // Debug builds of the compiler (and so every test run) keep it; release
    // builds skip its cost unless TARN_VERIFY_CODEGEN asks for it.
    let verify = cfg!(debug_assertions) || std::env::var_os("TARN_VERIFY_CODEGEN").is_some();
    settings.set("enable_verifier", if verify { "true" } else { "false" }).map_err(|e| Error::bug(e.to_string()))?;
    let isa = cranelift_native::builder().map_err(|e| Error::unsupported(e.to_string()))?.finish(settings::Flags::new(settings)).map_err(|e| Error::bug(e.to_string()))?;
    let builder = ObjectBuilder::new(isa, "tarn", cranelift_module::default_libcall_names()).map_err(|e| Error::bug(e.to_string()))?;
    let mut module = ObjectModule::new(builder);
    let mut ids = HashMap::new();
    let mut ordered: Vec<_> = reachable.into_iter().collect();
    ordered.sort();
    for id in &ordered {
        let f = &p.functions[id.0 as usize].decl;
        let fid = if f.kind == FnKind::Extern {
            // The C symbol itself, resolved by the system linker.
            let sig = c_signature(&module, f)?;
            let name = c_symbol(f);
            module.declare_function(name, Linkage::Import, &sig).map_err(|e| Error::unsupported(format!("extern \"C\" fn {name}: {e}")))?
        } else {
            let sig = signature(&module, t, f)?;
            // Readable local symbols for profilers and debuggers: the id keeps
            // them unique, the source name says what they are.
            let readable: String = f.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            module.declare_function(&format!("tarn_fn_{}__{readable}", id.0), Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?
        };
        ids.insert(*id, fid);
    }
    let mut tables = Vec::<(tarn_resolve::SymbolId, Ty, Vec<FunctionId>, DataId)>::new();
    for f in &p.functions {
        for b in &f.blocks {
            for stmt in &b.stmts {
                if let post::Op::Plain(StatementKind::Assign(_, Rvalue::Coerce(CoerceKind::DynTable { interface, concrete, methods }, _, ty))) = &stmt.op {
                    validate_table(p, t, *interface, concrete, methods, ty)?;
                    if !tables.iter().any(|(i, c, _, _)| i == interface && c == concrete) {
                        let data = module.declare_data(&format!("tarn_vtable_{}", tables.len()), Linkage::Local, false, false).map_err(|e| Error::bug(e.to_string()))?;
                        let mut description = DataDescription::new();
                        description.define(vec![0u8; (methods.len() * 8).max(1)].into_boxed_slice());
                        description.set_align(8);
                        for (n, method) in methods.iter().enumerate() {
                            let func = module.declare_func_in_data(*ids.get(method).ok_or_else(|| Error::bug("missing vtable method"))?, &mut description);
                            description.write_function_addr((n * 8) as u32, func);
                        }
                        module.define_data(data, &description).map_err(|e| Error::bug(e.to_string()))?;
                        tables.push((*interface, concrete.clone(), methods.clone(), data));
                    } else if tables.iter().any(|(i, c, ms, _)| i == interface && c == concrete && ms != methods) {
                        return Err(Error::bug("conflicting vtable"));
                    }
                }
            }
        }
    }
    let c_callbacks = ffi::emit_callbacks(&mut module, p, &ids)?;
    let mut thunks = HashMap::new();
    for id in &ordered {
        let f = &p.functions[id.0 as usize].decl;
        let sig = closure_signature(&module, t, f)?;
        let thunk = module.declare_function(&format!("tarn_thunk_{}", id.0), Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?;
        thunks.insert(*id, thunk);
    }
    let mut frame_drops = HashMap::new();
    for id in &ordered {
        if frame(&p.functions[id.0 as usize].decl).is_some() {
            let mut sig = module.make_signature();
            sig.params.push(cl::AbiParam::new(types::I64));
            let drop = module.declare_function(&format!("tarn_async_drop_{}", id.0), Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?;
            frame_drops.insert(*id, drop);
        }
    }
    let task_adapters = emit_task_adapters(&mut module, p, t, &ids)?;
    let mut runtime = HashMap::new();
    for (name, params, returns) in [
        ("tarn_rt_net_main_error", vec![types::I32, types::I32], vec![]),
        ("tarn_rt_net_drop", vec![types::I32], vec![]),
        ("tarn_rt_process_drop", vec![types::I32], vec![]),
        ("tarn_rt_process_pipe_drop", vec![types::I32], vec![]),
        ("tarn_rt_process_close", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_process_wait", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_process_kill", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_process_terminate", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_process_read", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_process_write", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_process_pidfd", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_process_spawn", vec![types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I64, types::I8, types::I8, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_process_poll_exit", vec![types::I64, types::I32, types::I32], vec![]),
        ("tarn_rt_fs_drop", vec![types::I32], vec![]),
        ("tarn_rt_fs_dir_drop", vec![types::I64], vec![]),
        ("tarn_rt_fs_open", vec![types::I64, types::I64, types::I32, types::I32], vec![]),
        ("tarn_rt_fs_read", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_fs_write", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_fs_seek", vec![types::I64, types::I32, types::I64, types::I32], vec![]),
        ("tarn_rt_fs_file_metadata", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_fs_sync", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_fs_close", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_fs_metadata", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_fs_mkdir", vec![types::I64, types::I64, types::I32], vec![]),
        ("tarn_rt_fs_remove_file", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_fs_remove_dir", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_fs_rename", vec![types::I64, types::I64, types::I64], vec![]),
        ("tarn_rt_fs_dir_open", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_fs_dir_next", vec![types::I64, types::I64, types::I64, types::I64], vec![]),
        ("tarn_rt_net_exec_new", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_waker_new", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_wake", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_wake_link", vec![types::I64; 3], vec![]),
        ("tarn_rt_net_wake_owner", vec![types::I64; 3], vec![]),
        ("tarn_rt_net_wake_take", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_wake_arm", vec![types::I64, types::I64, types::I32, types::I32], vec![]),
        ("tarn_rt_net_wake_clear", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_exec_wait", vec![types::I64, types::I64, types::I32], vec![]),
        ("tarn_rt_net_waker_drop", vec![types::I64], vec![]),
        ("tarn_rt_net_exec_drop", vec![types::I64], vec![]),
        ("tarn_rt_net_poll_drop", vec![types::I64], vec![]),
        ("tarn_rt_net_tls_drop", vec![types::I64], vec![]),
        ("tarn_rt_net_tls_connect", vec![types::I64; 4], vec![]),
        ("tarn_rt_net_tls_read", vec![types::I64, types::I64, types::I64, types::I64, types::I8], vec![]),
        ("tarn_rt_net_tls_write", vec![types::I64, types::I64, types::I64, types::I64, types::I8], vec![]),
        ("tarn_rt_net_tls_fd", vec![types::I64; 2], vec![]),
        ("tarn_rt_net_close_tls", vec![types::I64; 2], vec![]),
        ("tarn_rt_net_nonblocking", vec![types::I64, types::I32, types::I8], vec![]),
        ("tarn_rt_net_mode", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_connected", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_now", vec![types::I64], vec![]),
        ("tarn_rt_net_poll_new", vec![types::I64], vec![]),
        ("tarn_rt_net_poll_ctl", vec![types::I64, types::I64, types::I32, types::I32, types::I32, types::I64], vec![]),
        ("tarn_rt_net_poll_wait", vec![types::I64, types::I64, types::I64, types::I64, types::I32], vec![]),
        ("tarn_rt_net_close_poll", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_resolve", vec![types::I64; 2], vec![]),
        ("tarn_rt_net_socket", vec![types::I64, types::I64, types::I8], vec![]),
        ("tarn_rt_net_bind", vec![types::I64, types::I32, types::I64], vec![]),
        ("tarn_rt_net_connect", vec![types::I64, types::I32, types::I64], vec![]),
        ("tarn_rt_net_connect_unix", vec![types::I64; 2], vec![]),
        ("tarn_rt_net_pair_unix", vec![types::I64], vec![]),
        ("tarn_rt_net_listen", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_accept", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_close", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_addr", vec![types::I64, types::I32, types::I8], vec![]),
        ("tarn_rt_net_shutdown", vec![types::I64, types::I32, types::I32], vec![]),
        ("tarn_rt_net_read", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_net_write", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_net_recv", vec![types::I64, types::I32, types::I64, types::I64], vec![]),
        ("tarn_rt_net_timer_new", vec![types::I64, types::I64], vec![]),
        ("tarn_rt_net_timer_read", vec![types::I64, types::I32], vec![]),
        ("tarn_rt_net_send", vec![types::I64, types::I32, types::I64, types::I64, types::I64], vec![]),
        ("tarn_rt_mutex_create", vec![], vec![types::I64]),
        ("tarn_rt_mutex_lock", vec![types::I64], vec![]),
        ("tarn_rt_mutex_unlock", vec![types::I64], vec![]),
        ("tarn_rt_mutex_destroy", vec![types::I64], vec![]),
        ("tarn_rt_atomic_destroy", vec![types::I64], vec![]),
        ("tarn_rt_task_spawn", vec![types::I64; 5], vec![types::I64]),
        ("tarn_rt_task_wait", vec![types::I64], vec![types::I64]),
        ("tarn_rt_task_release", vec![types::I64], vec![]),
        ("tarn_rt_task_drop", vec![types::I64], vec![]),
        ("tarn_rt_rem_f32", vec![types::F32, types::F32], vec![types::F32]),
        ("tarn_rt_rem_f64", vec![types::F64, types::F64], vec![types::F64]),
        ("tarn_rt_string", vec![types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_drop_string", vec![types::I64], vec![]),
        ("tarn_rt_string_utf8", vec![types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_string_range", vec![types::I64, types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_vec_extend", vec![types::I64; 4], vec![]),
        ("tarn_rt_string_add", vec![types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_string_compare", vec![types::I64, types::I64], vec![types::I32]),
        ("tarn_rt_env_alloc", vec![types::I64], vec![types::I64]),
        ("tarn_rt_vec_grow", vec![types::I64, types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_async_task_new", vec![types::I64, types::I64], vec![types::I64]),
        ("tarn_rt_async_task_result", vec![types::I64], vec![types::I64]),
        ("tarn_rt_async_task_complete", vec![types::I64], vec![]),
        ("tarn_rt_async_task_take", vec![types::I64], vec![types::I8]),
        ("tarn_rt_async_task_wait", vec![types::I64; 3], vec![]),
        ("tarn_rt_async_task_abandoned", vec![types::I64], vec![types::I8]),
        ("tarn_rt_async_task_release", vec![types::I64], vec![]),
        ("tarn_rt_async_task_handle_drop", vec![types::I64], vec![types::I8]),
        ("tarn_rt_async_inbox_push", vec![types::I64; 3], vec![]),
        ("tarn_rt_async_inbox_pop", vec![types::I64; 3], vec![types::I8]),
        ("tarn_rt_env_free", vec![types::I64], vec![]),
        ("tarn_rt_env_drop", vec![types::I64], vec![]),
        ("tarn_rt_print_i64", vec![types::I64], vec![]),
        ("tarn_rt_print_u64", vec![types::I64], vec![]),
        ("tarn_rt_print_f64", vec![types::F64], vec![]),
        ("tarn_rt_print_bool", vec![types::I8], vec![]),
        ("tarn_rt_print_string", vec![types::I64], vec![]),
        ("tarn_rt_panic", vec![types::I64], vec![]),
        ("tarn_rt_fault", vec![], vec![]),
    ] {
        let mut sig = module.make_signature();
        sig.params = params.into_iter().map(cl::AbiParam::new).collect();
        sig.returns = returns.into_iter().map(cl::AbiParam::new).collect();
        let id = module.declare_function(name, Linkage::Import, &sig).map_err(|e| Error::bug(e.to_string()))?;
        runtime.insert(name.to_owned(), id);
    }
    for (name, width) in [("bool", types::I8), ("i32", types::I32), ("i64", types::I64), ("u32", types::I32), ("u64", types::I64), ("usize", types::I64)] {
        for (op, params, returns) in [
            ("new", vec![width], vec![types::I64]),
            ("load", vec![types::I64], vec![width]),
            ("store", vec![types::I64, width], vec![]),
            ("swap", vec![types::I64, width], vec![width]),
            ("compare_exchange", vec![types::I64, width, width], vec![types::I8]),
            ("fetch_add", vec![types::I64, width], vec![width]),
            ("fetch_sub", vec![types::I64, width], vec![width]),
        ] {
            if name == "bool" && op.starts_with("fetch_") { continue; }
            let name = format!("tarn_rt_atomic_{name}_{op}");
            let mut sig = module.make_signature();
            sig.params.extend(params.into_iter().map(cl::AbiParam::new));
            sig.returns.extend(returns.into_iter().map(cl::AbiParam::new));
            let id = module.declare_function(&name, Linkage::Import, &sig).map_err(|e| Error::bug(e.to_string()))?;
            runtime.insert(name, id);
        }
    }
    // Vector element destruction functions, declared on demand by drop_vector
    // and defined after ordinary functions (see Cx::element_glue).
    let glue = RefCell::new(Vec::new());
    // String literals, emitted once per program as static TarnString images.
    let literals = RefCell::new(HashMap::new());
    // Translation needs the module; machine-code compilation does not, so it
    // runs in parallel afterwards (see compile_in_parallel).
    let mut translated = Vec::new();
    for id in ordered {
        let f = &p.functions[id.0 as usize];
        if f.decl.kind == FnKind::Extern {
            continue;
        }
        let mut ctx = module.make_context();
        ctx.func.signature = signature(&module, t, &f.decl)?;
        let mut fb = FunctionBuilderContext::new();
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let mut cx = Cx {
                b,
                module: &mut module,
                p,
                t,
                f,
                ids: &ids,
                thunks: &thunks,
                c_callbacks: &c_callbacks,
                frame_drops: &frame_drops,
                env: None,
                environments: HashMap::new(),
                runtime: &runtime,
                task_adapters: &task_adapters,
                tables: &tables,
                glue: &glue,
                literals: &literals,
                locals: Vec::new(),
                flags: Vec::new(),
                blocks: Vec::new(),
                sret: None,
            };
            cx.function()?;
            cx.b.finalize();
        }
        translated.push((ids[&id], f.decl.name.as_str(), ctx));
    }
    compile_in_parallel(&mut module, translated, verify)?;
    let mut thunk_order: Vec<_> = thunks.iter().collect();
    thunk_order.sort_by_key(|(id, _)| **id);
    for (id, thunk) in thunk_order {
        let f = &p.functions[id.0 as usize].decl;
        if frame(f).is_some() {
            emit_frame_thunks(&mut module, t, f, ids[id], *thunk, frame_drops[id], runtime["tarn_rt_env_free"])?;
            continue;
        }
        let n = capture_count(f);
        let mut ctx = module.make_context();
        ctx.func.signature = closure_signature(&module, t, f)?;
        let mut fb = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let params = b.block_params(entry).to_vec();
            let aggregate = layout::layout(t, &f.ret)?.size > 0 && scalar(&f.ret).is_none();
            let env = params[usize::from(aggregate)];
            let mut values = Vec::new();
            if aggregate {
                values.push(params[0]);
            }
            let (_, fields) = environment(t, f)?;
            for ((offset, _ty), param) in fields.into_iter().zip(f.params().take(n)) {
                if layout::layout(t, &f.local(param).ty)?.size == 0 {
                    continue;
                }
                let ptr = b.ins().iadd_imm(env, i64::from(offset));
                let borrowed_owned = matches!(f.kind, FnKind::Closure { owned: true, consumes: false, .. });
                let value = if borrowed_owned {
                    ptr
                } else if let Some(ty) = scalar(&f.local(param).ty) {
                    b.ins().load(ty, cl::MemFlags::new(), ptr, 0)
                } else {
                    ptr
                };
                values.push(value);
            }
            values.extend_from_slice(&params[usize::from(aggregate) + 1..]);
            let target = module.declare_func_in_func(ids[id], b.func);
            let call = b.ins().call(target, &values);
            let results = b.inst_results(call).to_vec();
            if matches!(f.kind, FnKind::Closure { owned: true, consumes: true, .. }) {
                let free = module.declare_func_in_func(runtime["tarn_rt_env_free"], b.func);
                b.ins().call(free, &[env]);
            }
            b.ins().return_(&results);
            b.seal_all_blocks();
            b.finalize();
            let _ = n;
        }
        cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
        module.define_function(*thunk, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
    }
    // Each glue function may declare further glue for nested vectors; a type
    // reached again through itself reuses its declared function instead of
    // expanding forever (`enum Value { List(Vec<Value>) }`).
    let mut defined = 0;
    while defined < glue.borrow().len() {
        let (ty, id) = glue.borrow()[defined].clone();
        defined += 1;
        let mut ctx = module.make_context();
        ctx.func.signature.params.push(cl::AbiParam::new(types::I64));
        let mut fb = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let addr = b.block_params(entry)[0];
            // Destruction reads no per-function state; `main` only fills Cx.
            let mut cx = Cx {
                b,
                module: &mut module,
                p,
                t,
                f: main,
                ids: &ids,
                thunks: &thunks,
                c_callbacks: &c_callbacks,
                frame_drops: &frame_drops,
                env: None,
                environments: HashMap::new(),
                runtime: &runtime,
                task_adapters: &task_adapters,
                tables: &tables,
                glue: &glue,
                literals: &literals,
                locals: Vec::new(),
                flags: Vec::new(),
                blocks: Vec::new(),
                sret: None,
            };
            cx.drop_at(addr, &ty)?;
            cx.b.ins().return_(&[]);
            cx.b.seal_all_blocks();
            cx.b.finalize();
        }
        cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
        module.define_function(id, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
    }
    // libc startup calls the C main shim; internal Tarn main returns void or Result<void, net.Error>.
    let mut sig = module.make_signature();
    sig.returns.push(cl::AbiParam::new(types::I32));
    if test {
        sig.params.push(cl::AbiParam::new(types::I32));
        sig.params.push(cl::AbiParam::new(types::I64));
    }
    let selector = if test {
        let mut sig = module.make_signature();
        sig.params.push(cl::AbiParam::new(types::I32));
        sig.params.push(cl::AbiParam::new(types::I64));
        sig.returns.push(cl::AbiParam::new(types::I32));
        Some(module.declare_function("tarn_rt_test_select", Linkage::Import, &sig).map_err(|e| Error::bug(e.to_string()))?)
    } else { None };
    let entry = module.declare_function("main", Linkage::Export, &sig).map_err(|e| Error::bug(e.to_string()))?;
    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let mut fb = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
        let block = b.create_block();
        if test { b.append_block_params_for_function_params(block); }
        b.switch_to_block(block);
        let target = module.declare_func_in_func(ids[&main.decl.id], b.func);
        if result_main {
            let l = layout::layout(t, &main.decl.ret)?;
            if l.variants.len() != 2 || !l.variants[0].iter().all(|(_, ty)| *ty == Ty::Void) || l.variants[1].len() != 1 { return Err(Error::bug("main result layout")); }
            let slot = b.create_sized_stack_slot(cl::StackSlotData::new(cl::StackSlotKind::ExplicitSlot, l.size, l.align.trailing_zeros() as u8));
            let addr = b.ins().stack_addr(types::I64, slot, 0);
            b.ins().call(target, &[addr]);
            let tag = b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
            let success = b.create_block();
            let failure = b.create_block();
            let ok = b.ins().icmp_imm(IntCC::Equal, tag, 0);
            b.ins().brif(ok, success, &[], failure, &[]);
            b.switch_to_block(failure);
            let (offset, error_ty) = &l.variants[1][0];
            let error_layout = layout::layout(t, error_ty)?;
            if error_layout.fields.len() != 3 { return Err(Error::bug("main error layout")); }
            let kind = b.ins().load(types::I32, cl::MemFlags::new(), addr, (*offset + error_layout.fields[0].0) as i32);
            let code = b.ins().load(types::I32, cl::MemFlags::new(), addr, (*offset + error_layout.fields[1].0) as i32);
            let report = module.declare_func_in_func(runtime["tarn_rt_net_main_error"], b.func);
            b.ins().call(report, &[kind, code]);
            let one = b.ins().iconst(types::I32, 1);
            b.ins().return_(&[one]);
            b.switch_to_block(success);
        } else if let Some(selector) = selector {
            let params = b.block_params(block).to_vec();
            let selector = module.declare_func_in_func(selector, b.func);
            let call = b.ins().call(selector, &params);
            let selected = b.inst_results(call)[0];
            b.ins().call(target, &[selected]);
        } else { b.ins().call(target, &[]); }
        let zero = b.ins().iconst(types::I32, 0);
        b.ins().return_(&[zero]);
        b.seal_all_blocks();
        b.finalize();
    }
    module.define_function(entry, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
    module.finish().emit().map_err(|e| Error::bug(e.to_string()))
}
fn emit_task_adapters(module: &mut ObjectModule, p: &post::Program, t: &Typed,
    ids: &HashMap<FunctionId, FuncId>) -> Result<HashMap<FunctionId, (FuncId, FuncId)>> {
    let mut shapes = std::collections::BTreeMap::new();
    for f in &p.functions { for b in &f.blocks {
        if let Terminator::Call { callee: Callee::TaskSpawn { worker, drop_result, .. }, .. } = &b.term {
            if shapes.insert(*worker, *drop_result).is_some_and(|old| old != *drop_result) {
                return Err(Error::bug("conflicting task destruction metadata"));
            }
        }
    }}
    let mut adapters = HashMap::new();
    for (worker, drop_result) in shapes {
        let f = &p.functions.get(worker.0 as usize).ok_or_else(|| Error::bug("missing task worker"))?.decl;
        let drop = &p.functions.get(drop_result.0 as usize).ok_or_else(|| Error::bug("missing result destruction"))?.decl;
        if f.param_count != 1 || !matches!(f.local(LocalId(1)).ty, Ty::Fn(_, ref args, ref ret) if args.is_empty() && **ret == f.ret) || drop.param_count != 1 || drop.local(LocalId(1)).ty != f.ret || drop.ret != Ty::Void {
            return Err(Error::bug("task adapter metadata mismatch"));
        }
        let mut declared = Vec::new();
        for (is_worker, target) in [(true, worker), (false, drop_result)] {
            let mut sig = module.make_signature();
            sig.params = vec![cl::AbiParam::new(types::I64); if is_worker { 3 } else { 1 }];
            let id = module.declare_function(&format!("tarn_task_{}_{}", if is_worker { "worker" } else { "drop" }, worker.0), Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?;
            let mut ctx = module.make_context();
            ctx.func.signature = sig;
            let mut fb = FunctionBuilderContext::new();
            {
                let mut b = FunctionBuilder::new(&mut ctx.func, &mut fb);
                let entry = b.create_block();
                b.append_block_params_for_function_params(entry);
                b.switch_to_block(entry);
                let params = b.block_params(entry).to_vec();
                let mut values = Vec::new();
                let size = layout::layout(t, &f.ret)?.size;
                if is_worker {
                    if size > 0 && scalar(&f.ret).is_none() { values.push(params[0]); }
                    let slot = b.create_sized_stack_slot(cl::StackSlotData::new(cl::StackSlotKind::ExplicitSlot, 16, 3));
                    let pair = b.ins().stack_addr(types::I64, slot, 0);
                    b.ins().store(cl::MemFlags::new(), params[1], pair, 0);
                    b.ins().store(cl::MemFlags::new(), params[2], pair, 8);
                    values.push(pair);
                } else if let Some(ty) = scalar(&f.ret) {
                    values.push(b.ins().load(ty, cl::MemFlags::new(), params[0], 0));
                } else if size > 0 { values.push(params[0]); }
                let target = module.declare_func_in_func(ids[&target], b.func);
                let call = b.ins().call(target, &values);
                if is_worker && scalar(&f.ret).is_some() {
                    let value = b.inst_results(call)[0];
                    b.ins().store(cl::MemFlags::new(), value, params[0], 0);
                }
                b.ins().return_(&[]);
                b.seal_all_blocks();
                b.finalize();
            }
            cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(e.to_string()))?;
            module.define_function(id, &mut ctx).map_err(|e| Error::bug(e.to_string()))?;
            declared.push(id);
        }
        adapters.insert(worker, (declared[0], declared[1]));
    }
    Ok(adapters)
}

#[derive(Clone, Copy)]
enum Slot {
    Ssa(Variable),
    Stack(cl::StackSlot),
    /// Offset in the stable heap frame of an async body.
    Frame(u32),
    Empty,
}
#[derive(Clone, Copy)]
enum Flag {
    Bit(Variable),
    Bits(cl::StackSlot, u64),
    /// Async frame offsets: flags persist across polls.
    FrameBit(u32),
    FrameBits(u32, u64),
}
#[derive(Clone)]
struct Val {
    value: Option<cl::Value>,
    ty: Ty,
}
struct Cx<'a, 'b> {
    b: FunctionBuilder<'a>,
    module: &'b mut ObjectModule,
    p: &'b post::Program,
    t: &'b Typed,
    f: &'b post::Function,
    ids: &'b HashMap<FunctionId, FuncId>,
    thunks: &'b HashMap<FunctionId, FuncId>,
    c_callbacks: &'b HashMap<FunctionId, FuncId>,
    frame_drops: &'b HashMap<FunctionId, FuncId>,
    /// Frame pointer of an async body.
    env: Option<cl::Value>,
    /// Frame offsets of borrowed closure environments (async bodies).
    environments: HashMap<LocalId, u32>,
    runtime: &'b HashMap<String, FuncId>,
    task_adapters: &'b HashMap<FunctionId, (FuncId, FuncId)>,
    tables: &'b [(tarn_resolve::SymbolId, Ty, Vec<FunctionId>, DataId)],
    /// Declared vector element destruction functions, by element type.
    glue: &'b RefCell<Vec<(Ty, FuncId)>>,
    /// Static string literal data by content (Phase 27).
    literals: &'b RefCell<HashMap<String, DataId>>,
    locals: Vec<Slot>,
    flags: Vec<Flag>,
    blocks: Vec<cl::Block>,
    sret: Option<cl::Value>,
}
impl Cx<'_, '_> {
    fn function(&mut self) -> Result<()> {
        let mut addressed = HashSet::new();
        for block in &self.f.blocks {
            for s in &block.stmts {
                if let post::Op::Plain(StatementKind::Assign(_, Rvalue::Ref(_, p))) = &s.op {
                    if !matches!(p.proj.first(), Some(Proj::Deref)) {
                        addressed.insert(p.local);
                    }
                }
            }
        }
        let placement = if frame(&self.f.decl).is_some() { Some(frame_layout(self.t, self.p, self.f)?) } else { None };
        self.environments = placement.as_ref().map(|p| p.environments.clone()).unwrap_or_default();
        for (i, l) in self.f.decl.locals.iter().enumerate() {
            let lyt = layout::layout(self.t, &l.ty)?;
            let slot = if let Some(offset) = placement.as_ref().and_then(|p| p.locals.get(&LocalId(i as u32))) {
                if lyt.size == 0 { Slot::Empty } else { Slot::Frame(*offset) }
            } else if lyt.size == 0 {
                Slot::Empty
            } else if let Some(ty) = scalar(&l.ty).filter(|_| !addressed.contains(&LocalId(i as u32))) {
                Slot::Ssa(self.b.declare_var(ty))
            } else {
                Slot::Stack(self.stack(lyt.size, lyt.align))
            };
            self.locals.push(slot);
        }
        for (i, flag) in self.f.flags.iter().enumerate() {
            let slot = match (flag, &placement) {
                (post::FlagKind::Value(_), Some(p)) => Flag::FrameBit(p.flags[i]),
                (post::FlagKind::Elements(_, n), Some(p)) => Flag::FrameBits(p.flags[i], *n),
                (post::FlagKind::Value(_), None) => Flag::Bit(self.b.declare_var(types::I8)),
                (post::FlagKind::Elements(_, n), None) => Flag::Bits(self.stack(u32::try_from(*n).map_err(|_| Error::unsupported("bitmap too large"))?.max(1), 1), *n),
            };
            self.flags.push(slot);
        }
        self.blocks = self.f.blocks.iter().map(|_| self.b.create_block()).collect();
        // Seal each IR block once every predecessor's terminator is emitted.
        // Unsealed blocks make the SSA builder add speculative block
        // parameters for every variable read, then remove them all at the end:
        // quadratic in blocks x variables for long functions or large matches.
        let mut remaining = vec![0usize; self.f.blocks.len()];
        for block in &self.f.blocks {
            let mut successors = block.term.successors();
            successors.sort();
            successors.dedup();
            for s in successors {
                remaining[s.0 as usize] += 1;
            }
        }
        for (b, n) in remaining.iter().enumerate() {
            if *n == 0 {
                self.b.seal_block(self.blocks[b]);
            }
        }
        let entry = self.blocks[0];
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        let params = self.b.block_params(entry).to_vec();
        let mut pi = 0;
        if layout::layout(self.t, &self.f.decl.ret)?.size > 0 && scalar(&self.f.decl.ret).is_none() {
            self.sret = Some(params[pi]);
            pi += 1;
        }
        for local in self.f.decl.params() {
            let ty = self.f.decl.local(local).ty.clone();
            if abi_type(self.t, &ty)?.is_some() {
                self.write(&Place::local(local), Val { value: Some(params[pi]), ty })?;
                pi += 1;
            }
        }
        let mut entry_values = Vec::new();
        if let (Some(info), Some(frame)) = (&self.f.decl.asynchronous, frame(&self.f.decl)) {
            self.env = Some(params[pi]);
            for (local, value) in [(info.waker, params[pi + 1]), (frame.abandon, params[pi + 2])] {
                let ty = self.f.decl.local(local).ty.clone();
                self.write(&Place::local(local), Val { value: Some(value), ty })?;
                entry_values.push(local.0 as usize);
            }
        }
        // Non-SSA uninitialized locals have no reads on valid source paths.
        // Give SSA slots an arbitrary zero entry definition so a dead/uninit
        // edge through a guarded drop does not require an undefined SSA use.
        for i in 0..self.locals.len() {
            if (i > 0 && i <= self.f.decl.param_count as usize) || entry_values.contains(&i) {
                continue;
            }
            if let Slot::Ssa(var) = self.locals[i] {
                let ty = scalar(&self.f.decl.locals[i].ty).unwrap();
                let v = self.zero(ty);
                self.b.def_var(var, v);
            }
        }
        for (bi, block) in self.f.blocks.iter().enumerate() {
            if bi != 0 {
                self.b.switch_to_block(self.blocks[bi]);
            }
            for stmt in &block.stmts {
                self.statement(&stmt.op)?;
            }
            self.term(&block.term)?;
            let mut successors = block.term.successors();
            successors.sort();
            successors.dedup();
            for s in successors {
                remaining[s.0 as usize] -= 1;
                if remaining[s.0 as usize] == 0 {
                    self.b.seal_block(self.blocks[s.0 as usize]);
                }
            }
        }
        // Safety net: anything still unsealed (sealing twice is not allowed,
        // seal_all_blocks skips sealed blocks).
        self.b.seal_all_blocks();
        // The outer caller consumes/finalizes the builder after lowering.
        Ok(())
    }
    /// Address and width of a memory-resident flag (stack bitmap or frame).
    fn flag_memory(&mut self, id: post::FlagId) -> Result<(cl::Value, u64)> {
        Ok(match self.flags[id.0 as usize] {
            Flag::Bits(slot, n) => (self.b.ins().stack_addr(types::I64, slot, 0), n),
            Flag::FrameBit(offset) | Flag::FrameBits(offset, _) => {
                let env = self.env.ok_or_else(|| Error::bug("frame flag outside async body"))?;
                let n = if let Flag::FrameBits(_, n) = self.flags[id.0 as usize] { n } else { 1 };
                (self.b.ins().iadd_imm(env, i64::from(offset)), n)
            }
            Flag::Bit(_) => return Err(Error::bug("register flag has no address")),
        })
    }
    fn stack(&mut self, size: u32, alignment: u32) -> cl::StackSlot {
        self.b.create_sized_stack_slot(cl::StackSlotData::new(cl::StackSlotKind::ExplicitSlot, size.max(1), alignment.trailing_zeros() as u8))
    }
    fn zero(&mut self, ty: cl::Type) -> cl::Value {
        if ty == types::F32 {
            self.b.ins().f32const(0.0)
        } else if ty == types::F64 {
            self.b.ins().f64const(0.0)
        } else {
            self.b.ins().iconst(ty, 0)
        }
    }
    /// Out-of-line destruction of one `ty` at an address. Vector elements use
    /// it so recursive element types become runtime recursion, not unbounded
    /// inline expansion.
    fn element_glue(&mut self, ty: &Ty) -> Result<FuncId> {
        if let Some((_, id)) = self.glue.borrow().iter().find(|(t, _)| t == ty) {
            return Ok(*id);
        }
        let mut sig = self.module.make_signature();
        sig.params.push(cl::AbiParam::new(types::I64));
        let n = self.glue.borrow().len();
        let id = self.module.declare_function(&format!("tarn_drop_glue_{n}"), Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?;
        self.glue.borrow_mut().push((ty.clone(), id));
        Ok(id)
    }
    fn runtime(&mut self, name: &str, args: &[cl::Value]) -> Vec<cl::Value> {
        let target = self.module.declare_func_in_func(self.runtime[name], self.b.func);
        let call = self.b.ins().call(target, args);
        self.b.inst_results(call).to_vec()
    }
    /// The value of `v` when it is an integer constant in this function.
    fn constant(&self, v: cl::Value) -> Option<i64> {
        let cl::ValueDef::Result(inst, _) = self.b.func.dfg.value_def(v) else { return None };
        match self.b.func.dfg.insts[inst] {
            cl::InstructionData::UnaryImm { opcode: cl::Opcode::Iconst, imm } => Some(imm.bits()),
            _ => None,
        }
    }
    /// Enter a helper block whose predecessors are all emitted, sealing it so
    /// variable reads inside it resolve without speculative block parameters.
    fn enter(&mut self, block: cl::Block) {
        self.b.switch_to_block(block);
        self.b.seal_block(block);
    }
    fn fault_if(&mut self, condition: cl::Value) {
        let fail = self.b.create_block();
        let next = self.b.create_block();
        // Faults abort: keep them out of the hot path's layout.
        self.b.set_cold_block(fail);
        self.b.ins().brif(condition, fail, &[], next, &[]);
        self.enter(fail);
        self.runtime("tarn_rt_fault", &[]);
        self.b.ins().trap(cl::TrapCode::user(1).unwrap());
        self.enter(next);
    }
    fn place_ty(&self, p: &Place) -> Result<Ty> {
        post::place_ty(&self.f.decl, self.t, p).ok_or_else(|| Error::bug(format!("invalid place {p:?}")))
    }
    fn addr(&mut self, p: &Place) -> Result<cl::Value> {
        let mut ty = self.f.decl.locals.get(p.local.0 as usize).ok_or_else(|| Error::bug("missing local"))?.ty.clone();
        let mut addr = if matches!(p.proj.first(), Some(Proj::Deref)) {
            self.read(&Place::local(p.local))?.value.unwrap()
        } else {
            match self.locals[p.local.0 as usize] {
                Slot::Stack(s) => self.b.ins().stack_addr(types::I64, s, 0),
                Slot::Empty => {
                    let s = self.stack(1, 1);
                    self.b.ins().stack_addr(types::I64, s, 0)
                }
                Slot::Frame(offset) => {
                    let env = self.env.ok_or_else(|| Error::bug("frame slot outside async body"))?;
                    self.b.ins().iadd_imm(env, i64::from(offset))
                }
                Slot::Ssa(_) => return Err(Error::bug("address-taken SSA local")),
            }
        };
        let mut payload = None;
        let mut slice_len = None;
        for (i, pr) in p.proj.iter().enumerate() {
            match pr {
                Proj::Deref => {
                    let Ty::Ref(_, inner) = ty else {
                        return Err(Error::bug("deref non-reference"));
                    };
                    if matches!(inner.as_ref(), Ty::Slice(_)) {
                        slice_len = Some(self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 8));
                        addr = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                    } else if i != 0 {
                        addr = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                    }
                    ty = *inner;
                }
                Proj::Downcast(v) => {
                    payload = Some(layout::layout(self.t, &ty)?.variants.get(*v as usize).ok_or_else(|| Error::bug("invalid variant"))?.clone());
                }
                Proj::Field(i) => {
                    let fields = match payload.take() {
                        Some(xs) => xs,
                        None => layout::layout(self.t, &ty)?.fields,
                    };
                    let (offset, field) = fields.get(*i as usize).ok_or_else(|| Error::bug("invalid field"))?;
                    addr = self.b.ins().iadd_imm(addr, i64::from(*offset));
                    ty = field.clone();
                }
                Proj::Index(idx) => {
                    let (elem, len) = match ty {
                        Ty::Array(elem, n) => (elem, self.b.ins().iconst(types::I64, n as i64)),
                        Ty::Slice(elem) => (elem, slice_len.ok_or_else(|| Error::bug("missing slice length"))?),
                        _ => return Err(Error::bug("index of noncollection")),
                    };
                    let index = self.read(&Place::local(*idx))?.value.unwrap();
                    let invalid = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
                    self.fault_if(invalid);
                    let offset = self.b.ins().imul_imm(index, i64::from(layout::layout(self.t, &elem)?.size));
                    addr = self.b.ins().iadd(addr, offset);
                    ty = *elem;
                }
            }
        }
        Ok(addr)
    }
    fn through_refs(&mut self, mut v: Val) -> Result<Val> {
        while let Ty::Ref(_, inner) = &v.ty {
            let inner = (**inner).clone();
            let st = scalar(&inner).ok_or_else(|| Error::unsupported("comparison through a reference to an aggregate"))?;
            let addr = v.value.ok_or_else(|| Error::bug("missing reference value"))?;
            v = Val { value: Some(self.b.ins().load(st, cl::MemFlags::new(), addr, 0)), ty: inner };
        }
        Ok(v)
    }
    fn read(&mut self, p: &Place) -> Result<Val> {
        let ty = self.place_ty(p)?;
        let l = layout::layout(self.t, &ty)?;
        if l.size == 0 {
            return Ok(Val { value: None, ty });
        }
        if p.proj.is_empty()
            && let Slot::Ssa(var) = self.locals[p.local.0 as usize]
        {
            return Ok(Val { value: Some(self.b.use_var(var)), ty });
        }
        let addr = self.addr(p)?;
        let value = if let Some(st) = scalar(&ty) { self.b.ins().load(st, cl::MemFlags::new(), addr, 0) } else { addr };
        Ok(Val { value: Some(value), ty })
    }
    fn write(&mut self, p: &Place, v: Val) -> Result<()> {
        let ty = self.place_ty(p)?;
        // A capture-free closure literal typed `mut fn` or `once fn` (ADR 0048,
        // Phase 23A) is emitted
        // as a shared function item; the code/null-environment pair is
        // identical, so only the invocation mode differs.
        let adopted = matches!((&ty, &v.ty), (Ty::Fn(tarn_types::CallMode::Mutable | tarn_types::CallMode::Once, ps, r), Ty::Fn(tarn_types::CallMode::Shared, qs, s)) if ps == qs && r == s);
        if ty != v.ty && !adopted {
            return Err(Error::bug(format!("assignment ABI mismatch {ty:?} <- {:?}", v.ty)));
        }
        let l = layout::layout(self.t, &ty)?;
        if l.size == 0 {
            return Ok(());
        }
        let value = v.value.ok_or_else(|| Error::bug("missing nonvoid value"))?;
        if p.proj.is_empty()
            && let Slot::Ssa(var) = self.locals[p.local.0 as usize]
        {
            self.b.def_var(var, value);
            return Ok(());
        }
        let addr = self.addr(p)?;
        if scalar(&ty).is_some() {
            self.b.ins().store(cl::MemFlags::new(), value, addr, 0);
        } else {
            self.copy(addr, value, l.size);
        }
        Ok(())
    }
    /// Store an aggregate's fields at `addr`. All operands are evaluated
    /// before any destination byte is written.
    fn build_aggregate(&mut self, kind: &Aggregate, ops: &[Operand], dest: &Ty, addr: cl::Value) -> Result<()> {
                let l = layout::layout(self.t, dest)?;
                let fields = match kind {
                    Aggregate::Struct(..) => l.fields,
                    Aggregate::Variant(_, v, _) => {
                        let tag = self.b.ins().iconst(types::I32, i64::from(*v));
                        self.b.ins().store(cl::MemFlags::new(), tag, addr, 0);
                        l.variants.get(*v as usize).ok_or_else(|| Error::bug("aggregate variant"))?.clone()
                    }
                    Aggregate::Array(elem) => {
                        let size = layout::layout(self.t, elem)?.size;
                        ops.iter().enumerate().map(|(i, _)| (i as u32 * size, elem.clone())).collect()
                    }
                    _ => return Err(Error::unsupported("closure aggregate")),
                };
                if fields.len() != ops.len() {
                    return Err(Error::bug("aggregate arity"));
                }
                // Evaluate all operands before storing destination bytes.
                let values = ops.iter().map(|o| self.operand(o)).collect::<Result<Vec<_>>>()?;
                for ((offset, ty), v) in fields.into_iter().zip(values) {
                    if ty != v.ty {
                        return Err(Error::bug("aggregate field type"));
                    }
                    let size = layout::layout(self.t, &ty)?.size;
                    if size == 0 {
                        continue;
                    }
                    let ptr = self.b.ins().iadd_imm(addr, i64::from(offset));
                    if scalar(&ty).is_some() {
                        self.b.ins().store(cl::MemFlags::new(), v.value.unwrap(), ptr, 0);
                    } else {
                        self.copy(ptr, v.value.unwrap(), size);
                    }
                }
                Ok(())
    }
    /// Building directly into the destination is safe when no aggregate
    /// operand can share its storage: scalar operands are loaded first, and
    /// aggregate operands must be other locals, not reached through references.
    fn aggregate_in_place(&self, dest: &Place, ops: &[Operand]) -> Result<bool> {
        if dest.proj.contains(&Proj::Deref) {
            return Ok(false);
        }
        for o in ops {
            if let Operand::Copy(p) | Operand::Move(p) = o {
                let scalar_value = scalar(&self.place_ty(p)?).is_some();
                if !scalar_value && (p.local == dest.local || p.proj.contains(&Proj::Deref)) {
                    return Ok(false);
                }
            }
        }
        Ok(!matches!(self.locals[dest.local.0 as usize], Slot::Ssa(_)))
    }
    fn copy(&mut self, dest: cl::Value, source: cl::Value, size: u32) {
        // Small internal copy, no dependency on libc memcpy signature. Load all
        // bytes before writing so overlapping aggregate assignments are safe.
        // Word-sized chunks with unaligned MemFlags on both supported targets; the tail uses
        // narrower widths. Byte-wise copies dominated aggregate-heavy code.
        let mut chunks = Vec::new();
        let mut offset = 0u32;
        while offset < size {
            let width = [8u32, 4, 2, 1].into_iter().find(|w| offset + w <= size).unwrap();
            let ty = match width { 8 => types::I64, 4 => types::I32, 2 => types::I16, _ => types::I8 };
            chunks.push((offset as i32, self.b.ins().load(ty, cl::MemFlags::new(), source, offset as i32)));
            offset += width;
        }
        for (offset, v) in chunks {
            self.b.ins().store(cl::MemFlags::new(), v, dest, offset);
        }
    }
    fn operand(&mut self, o: &Operand) -> Result<Val> {
        match o {
            Operand::Copy(p) | Operand::Move(p) => self.read(p),
            Operand::Const(c) => Ok(match c {
                Const::Int(n, i) => Val { value: Some(self.b.ins().iconst(scalar(&Ty::Int(*i)).unwrap(), *n as i64)), ty: Ty::Int(*i) },
                Const::Bool(v) => Val { value: Some(self.b.ins().iconst(types::I8, i64::from(*v))), ty: Ty::Bool },
                Const::Float(s, f) => {
                    let n: f64 = s.replace('_', "").parse().map_err(|_| Error::bug("invalid float constant"))?;
                    Val { value: Some(if *f == FloatTy::F32 { self.b.ins().f32const(n as f32) } else { self.b.ins().f64const(n) }), ty: Ty::Float(*f) }
                }
                Const::Unit => Val { value: None, ty: Ty::Void },
                Const::Str(s) => {
                    // A literal is a read-only TarnString image (len, bytes) in
                    // the `tarn_strings` section: no allocation per evaluation.
                    // It is still an owned string; tarn_rt_drop_string skips
                    // `free` for addresses in that section (Phase 27).
                    let existing = self.literals.borrow().get(s).copied();
                    let id = match existing {
                        Some(id) => id,
                        None => {
                            let id = self.module.declare_anonymous_data(false, false).map_err(|e| Error::bug(e.to_string()))?;
                            let mut image = (s.len() as u64).to_le_bytes().to_vec();
                            image.extend_from_slice(s.as_bytes());
                            let mut data = DataDescription::new();
                            data.define(image.into_boxed_slice());
                            data.set_align(8);
                            data.set_segment_section("", "tarn_strings");
                            self.module.define_data(id, &data).map_err(|e| Error::bug(e.to_string()))?;
                            self.literals.borrow_mut().insert(s.clone(), id);
                            id
                        }
                    };
                    let gv = self.module.declare_data_in_func(id, self.b.func);
                    Val { value: Some(self.b.ins().global_value(types::I64, gv)), ty: Ty::Str }
                }
                Const::Fn(id, _) => {
                    let f = &self.p.functions[id.0 as usize].decl;
                    let ty = Ty::Fn(tarn_types::CallMode::Shared, f.params().map(|l| f.local(l).ty.clone()).collect(), Box::new(f.ret.clone()));
                    let target = self.module.declare_func_in_func(self.thunks[id], self.b.func);
                    let code = self.b.ins().func_addr(types::I64, target);
                    let env = self.b.ins().iconst(types::I64, 0);
                    Val { value: Some(self.pair(code, env)), ty }
                }
                _ => return Err(Error::unsupported(format!("constant {c:?}"))),
            }),
        }
    }
    fn statement(&mut self, op: &post::Op) -> Result<()> {
        match op {
            post::Op::Plain(StatementKind::Assign(p, rv)) => {
                let ty = self.place_ty(p)?;
                if let Rvalue::Aggregate(kind @ (Aggregate::Struct(..) | Aggregate::Variant(..) | Aggregate::Array(_)), ops) = rv
                    && layout::layout(self.t, &ty)?.size > 0
                    && self.aggregate_in_place(p, ops)?
                {
                    let addr = self.addr(p)?;
                    return self.build_aggregate(kind, ops, &ty, addr);
                }
                let value = self.rvalue(rv, &ty)?;
                self.write(p, value)?;
            }
            post::Op::Plain(StatementKind::StorageLive(_) | StatementKind::StorageDead(_)) => {}
            post::Op::Plain(StatementKind::Drop(_)) => return Err(Error::bug("abstract drop")),
            post::Op::Set(id, value) => {
                let v = self.b.ins().iconst(types::I8, i64::from(*value));
                match self.flags[id.0 as usize] {
                    Flag::Bit(var) => self.b.def_var(var, v),
                    Flag::FrameBit(_) | Flag::Bits(..) | Flag::FrameBits(..) => {
                        let (base, n) = self.flag_memory(*id)?;
                        for i in 0..n {
                            self.b.ins().store(cl::MemFlags::new(), v, base, i as i32);
                        }
                    }
                }
            }
            post::Op::ClearElement(id, idx) => {
                if !matches!(self.flags[id.0 as usize], Flag::Bits(..) | Flag::FrameBits(..)) {
                    return Err(Error::bug("element flag not bitmap"));
                }
                let (ptr, n) = self.flag_memory(*id)?;
                let i = self.read(&Place::local(*idx))?.value.unwrap();
                let bad = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, i, n as i64);
                self.fault_if(bad);
                let ptr = self.b.ins().iadd(ptr, i);
                let zero = self.b.ins().iconst(types::I8, 0);
                self.b.ins().store(cl::MemFlags::new(), zero, ptr, 0);
            }
            post::Op::Destroy(d) => self.drop_plan(d)?,
        }
        Ok(())
    }
    fn rvalue(&mut self, rv: &Rvalue, dest: &Ty) -> Result<Val> {
        match rv {
            Rvalue::Use(o) => self.operand(o),
            Rvalue::Ref(m, p) => {
                let inner = self.place_ty(p)?;
                let value = if matches!(inner, Ty::Any(_)) {
                    if !matches!(p.proj.last(), Some(Proj::Deref)) {
                        return Err(Error::bug("dynamic reborrow projection"));
                    }
                    let mut owner = p.clone();
                    owner.proj.pop();
                    self.read(&owner)?.value.ok_or_else(|| Error::bug("empty dynamic reborrow"))?
                } else if matches!(inner, Ty::Slice(_)) {
                    let (ptr, len, _) = self.collection(p)?;
                    self.pair(ptr, len)
                } else {
                    self.addr(p)?
                };
                Ok(Val { value: Some(value), ty: Ty::Ref(*m, Box::new(inner)) })
            }
            Rvalue::Binary(op, a, b) => {
                let a = self.operand(a)?;
                let b = self.operand(b)?;
                self.binary(*op, a, b)
            }
            Rvalue::Unary(op, o) => {
                let v = self.operand(o)?;
                let a = v.value.unwrap();
                let value = match (op, &v.ty) {
                    (UnOp::Not, Ty::Bool) => self.b.ins().icmp_imm(IntCC::Equal, a, 0),
                    (UnOp::Not, Ty::Int(_)) => self.b.ins().bnot(a),
                    (UnOp::Neg, Ty::Float(_)) => self.b.ins().fneg(a),
                    (UnOp::Neg, Ty::Int(i)) if i.signed() => {
                        let min = -(1i128 << (int_bits(*i) - 1));
                        let bad = self.b.ins().icmp_imm(IntCC::Equal, a, min as i64);
                        self.fault_if(bad);
                        self.b.ins().ineg(a)
                    }
                    _ => return Err(Error::unsupported("unary operation")),
                };
                Ok(Val { value: Some(value), ty: v.ty })
            }
            Rvalue::Aggregate(Aggregate::AsyncFrame(id, _), ops) => {
                // Lazy construction: allocate the stable frame, move arguments
                // into their parameter slots, start in state 0. No body code runs.
                let g = &self.p.functions[id.0 as usize];
                let frame = frame(&g.decl).ok_or_else(|| Error::bug("async construction without frame"))?;
                let placement = frame_layout(self.t, self.p, g)?;
                if frame.params.len() != ops.len() {
                    return Err(Error::bug("async construction arity"));
                }
                let size = self.b.ins().iconst(types::I64, i64::from(placement.size));
                let env = self.runtime("tarn_rt_env_alloc", &[size])[0];
                let drop = self.module.declare_func_in_func(self.frame_drops[id], self.b.func);
                let drop_code = self.b.ins().func_addr(types::I64, drop);
                self.b.ins().store(cl::MemFlags::new(), drop_code, env, 0);
                for (param, o) in frame.params.iter().zip(ops) {
                    let v = self.operand(o)?;
                    let ty = &g.decl.local(*param).ty;
                    if v.ty != *ty {
                        return Err(Error::bug("async parameter type"));
                    }
                    let ptr = self.b.ins().iadd_imm(env, i64::from(placement.locals[param]));
                    if let Some(value) = v.value {
                        if scalar(ty).is_some() {
                            self.b.ins().store(cl::MemFlags::new(), value, ptr, 0);
                        } else {
                            self.copy(ptr, value, layout::layout(self.t, ty)?.size);
                        }
                    }
                }
                let zero = self.b.ins().iconst(types::I32, 0);
                self.b.ins().store(cl::MemFlags::new(), zero, env, placement.locals[&frame.state] as i32);
                let target = self.module.declare_func_in_func(self.thunks[id], self.b.func);
                let code = self.b.ins().func_addr(types::I64, target);
                let value = self.pair(code, env);
                Ok(Val { value: Some(value), ty: dest.clone() })
            }
            Rvalue::Aggregate(Aggregate::Closure(id, storage), ops) => {
                let f = &self.p.functions[id.0 as usize].decl;
                let in_frame = storage.as_ref().and_then(|s| self.environments.get(&s.local).copied());
                let (size, fields) = environment(self.t, f)?;
                if fields.len() != ops.len() {
                    return Err(Error::bug("closure capture arity"));
                }
                let (owned, destructor) = match f.kind {
                    FnKind::Closure { owned, destructor, .. } => (owned, destructor),
                    _ => return Err(Error::bug("closure kind")),
                };
                let env = if owned {
                    let size = self.b.ins().iconst(types::I64, i64::from(size));
                    self.runtime("tarn_rt_env_alloc", &[size])[0]
                } else if let (Some(offset), Some(frame)) = (in_frame, self.env) {
                    // Borrowed environment of an async body: stable frame storage.
                    self.b.ins().iadd_imm(frame, i64::from(offset))
                } else {
                    let slot = self.stack(size, 8);
                    self.b.ins().stack_addr(types::I64, slot, 0)
                };
                let drop_code = if let Some(id) = destructor {
                    let target = self.module.declare_func_in_func(self.thunks[&id], self.b.func);
                    self.b.ins().func_addr(types::I64, target)
                } else {
                    self.b.ins().iconst(types::I64, 0)
                };
                self.b.ins().store(cl::MemFlags::new(), drop_code, env, 0);
                for ((offset, ty), o) in fields.into_iter().zip(ops) {
                    let v = self.operand(o)?;
                    if v.ty != ty {
                        return Err(Error::bug("closure capture type"));
                    }
                    let ptr = self.b.ins().iadd_imm(env, i64::from(offset));
                    if let Some(value) = v.value {
                        if scalar(&ty).is_some() {
                            self.b.ins().store(cl::MemFlags::new(), value, ptr, 0);
                        } else {
                            self.copy(ptr, value, layout::layout(self.t, &ty)?.size);
                        }
                    }
                }
                let target = self.module.declare_func_in_func(self.thunks[id], self.b.func);
                let code = self.b.ins().func_addr(types::I64, target);
                let value = self.pair(code, env);
                Ok(Val { value: Some(value), ty: dest.clone() })
            }
            Rvalue::Aggregate(kind, ops) => {
                let l = layout::layout(self.t, dest)?;
                let s = self.stack(l.size, l.align);
                let addr = self.b.ins().stack_addr(types::I64, s, 0);
                self.build_aggregate(kind, ops, dest, addr)?;
                Ok(Val { value: if l.size == 0 { None } else { Some(addr) }, ty: dest.clone() })
            }
            Rvalue::Discriminant(p) => {
                let ptr = self.addr(p)?;
                let v = self.b.ins().load(types::I32, cl::MemFlags::new(), ptr, 0);
                let st = scalar(dest).ok_or_else(|| Error::bug("tag destination"))?;
                let v = if st == types::I32 { v } else { self.b.ins().uextend(st, v) };
                Ok(Val { value: Some(v), ty: dest.clone() })
            }
            Rvalue::Len(p) => {
                let len = self.collection(p)?.1;
                Ok(Val { value: Some(len), ty: Ty::Int(IntTy::Usize) })
            }
            Rvalue::SliceRef { base, start, end, .. } => {
                let (ptr, len, elem) = self.collection(base)?;
                let start = match start {
                    Some(o) => self.operand(o)?.value.unwrap(),
                    None => self.b.ins().iconst(types::I64, 0),
                };
                let end = match end {
                    Some(o) => self.operand(o)?.value.unwrap(),
                    None => len,
                };
                let bad = self.b.ins().icmp(IntCC::UnsignedGreaterThan, start, end);
                self.fault_if(bad);
                let bad = self.b.ins().icmp(IntCC::UnsignedGreaterThan, end, len);
                self.fault_if(bad);
                let offset = self.b.ins().imul_imm(start, i64::from(layout::layout(self.t, &elem)?.size));
                let ptr = self.b.ins().iadd(ptr, offset);
                let len = self.b.ins().isub(end, start);
                let pair = self.pair(ptr, len);
                Ok(Val { value: Some(pair), ty: dest.clone() })
            }
            Rvalue::Coerce(CoerceKind::DynTable { interface, concrete, methods }, o, ty) => {
                let source = self.operand(o)?;
                if !matches!(&source.ty, Ty::Ref(_, inner) if inner.as_ref() == concrete) {
                    return Err(Error::bug("dynamic concrete mismatch"));
                }
                let (_, _, _, id) = self.tables.iter().find(|(i, c, ms, _)| i == interface && c == concrete && ms == methods).ok_or_else(|| Error::bug("missing vtable"))?;
                let gv = self.module.declare_data_in_func(*id, self.b.func);
                let table = self.b.ins().global_value(types::I64, gv);
                let pair = self.pair(source.value.ok_or_else(|| Error::bug("empty dynamic source"))?, table);
                Ok(Val { value: Some(pair), ty: ty.clone() })
            }
            Rvalue::Coerce(CoerceKind::Unsize, o, ty) => {
                let v = self.operand(o)?;
                let Ty::Ref(_, inner) = v.ty else {
                    return Err(Error::bug("unsize nonref"));
                };
                let Ty::Array(_, n) = *inner else {
                    return Err(Error::bug("unsize nonarray"));
                };
                let len = self.b.ins().iconst(types::I64, n as i64);
                let pair = self.pair(v.value.unwrap(), len);
                Ok(Val { value: Some(pair), ty: ty.clone() })
            }
            // A literal that fits its integer target needs no runtime check.
            Rvalue::Cast(Operand::Const(Const::Int(n, _)), Ty::Int(target)) if int_fits(*n, *target) => {
                let st = scalar(&Ty::Int(*target)).unwrap();
                Ok(Val { value: Some(self.b.ins().iconst(st, *n as i64)), ty: Ty::Int(*target) })
            }
            Rvalue::Cast(o, ty) => {
                let v = self.operand(o)?;
                self.cast(v, ty)
            }
            Rvalue::Coerce(CoerceKind::MutToShared | CoerceKind::Poller, o, ty) => {
                let v = self.operand(o)?;
                Ok(Val { value: v.value, ty: ty.clone() })
            }
            _ => Err(Error::unsupported(format!("rvalue {rv:?}"))),
        }
    }
    fn binary(&mut self, op: BinOp, a: Val, b: Val) -> Result<Val> {
        // The type checker compares through references (`&u64 == &u64`,
        // `&string == &string`): read the referents.
        let (a, b) = (self.through_refs(a)?, self.through_refs(b)?);
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            let (Ty::Int(lhs), Ty::Int(rhs)) = (&a.ty, &b.ty) else {
                return Err(Error::bug("shift operand types"));
            };
            let (x, y) = (a.value.unwrap(), b.value.unwrap());
            if rhs.signed() {
                let bad = self.b.ins().icmp_imm(IntCC::SignedLessThan, y, 0);
                self.fault_if(bad);
            }
            // The count lane may be narrower than the shifted width: widen
            // before comparing so the width constant cannot wrap in that lane.
            let y = if int_bits(*rhs) < 64 { self.b.ins().uextend(types::I64, y) } else { y };
            let bad = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, y, i64::from(int_bits(*lhs)));
            self.fault_if(bad);
            let value = if op == BinOp::Shl {
                self.b.ins().ishl(x, y)
            } else if lhs.signed() {
                self.b.ins().sshr(x, y)
            } else {
                self.b.ins().ushr(x, y)
            };
            return Ok(Val { value: Some(value), ty: a.ty });
        }
        if a.ty != b.ty {
            return Err(Error::bug("binary operand type mismatch"));
        }
        let (x, y) = (a.value.unwrap(), b.value.unwrap());
        let cmp = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        let value = match &a.ty {
            Ty::Float(_) => match op {
                BinOp::Add => self.b.ins().fadd(x, y),
                BinOp::Sub => self.b.ins().fsub(x, y),
                BinOp::Mul => self.b.ins().fmul(x, y),
                BinOp::Div => self.b.ins().fdiv(x, y),
                BinOp::Rem => self.runtime(if a.ty == Ty::Float(FloatTy::F32) { "tarn_rt_rem_f32" } else { "tarn_rt_rem_f64" }, &[x, y])[0],
                _ if cmp => self.b.ins().fcmp(
                    match op {
                        BinOp::Eq => FloatCC::Equal,
                        BinOp::Ne => FloatCC::NotEqual,
                        BinOp::Lt => FloatCC::LessThan,
                        BinOp::Le => FloatCC::LessThanOrEqual,
                        BinOp::Gt => FloatCC::GreaterThan,
                        _ => FloatCC::GreaterThanOrEqual,
                    },
                    x,
                    y,
                ),
                _ => return Err(Error::unsupported("floating-point remainder/bit operation")),
            },
            Ty::Int(_) | Ty::Bool => {
                let signed = matches!(a.ty,Ty::Int(i) if i.signed());
                if cmp {
                    let cc = match (op, signed) {
                        (BinOp::Eq, _) => IntCC::Equal,
                        (BinOp::Ne, _) => IntCC::NotEqual,
                        (BinOp::Lt, true) => IntCC::SignedLessThan,
                        (BinOp::Le, true) => IntCC::SignedLessThanOrEqual,
                        (BinOp::Gt, true) => IntCC::SignedGreaterThan,
                        (BinOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
                        (BinOp::Lt, false) => IntCC::UnsignedLessThan,
                        (BinOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
                        (BinOp::Gt, false) => IntCC::UnsignedGreaterThan,
                        _ => IntCC::UnsignedGreaterThanOrEqual,
                    };
                    self.b.ins().icmp(cc, x, y)
                } else {
                    match op {
                        // Proven by the IR's range analysis: no check (ADR 0057).
                        BinOp::AddProven => self.b.ins().iadd(x, y),
                        BinOp::SubProven => self.b.ins().isub(x, y),
                        BinOp::MulProven => self.b.ins().imul(x, y),
                        BinOp::Add | BinOp::Sub | BinOp::Mul => {
                            let (v, overflow) = match (op, signed) {
                                (BinOp::Add, true) => self.b.ins().sadd_overflow(x, y),
                                (BinOp::Add, false) => self.b.ins().uadd_overflow(x, y),
                                (BinOp::Sub, true) => self.b.ins().ssub_overflow(x, y),
                                (BinOp::Sub, false) => self.b.ins().usub_overflow(x, y),
                                (BinOp::Mul, true) => self.b.ins().smul_overflow(x, y),
                                _ => self.b.ins().umul_overflow(x, y),
                            };
                            self.fault_if(overflow);
                            v
                        }
                        BinOp::Div | BinOp::Rem => {
                            // A constant divisor needs only the checks it can fail.
                            let divisor = self.constant(y);
                            if divisor.is_none_or(|d| d == 0) {
                                let zero = self.b.ins().icmp_imm(IntCC::Equal, y, 0);
                                self.fault_if(zero);
                            }
                            if signed && divisor.is_none_or(|d| d == -1) {
                                let Ty::Int(i) = a.ty else {
                                    return Err(Error::bug("signed bool"));
                                };
                                let min = -(1i128 << (int_bits(i) - 1));
                                let lhs = self.b.ins().icmp_imm(IntCC::Equal, x, min as i64);
                                let rhs = self.b.ins().icmp_imm(IntCC::Equal, y, -1);
                                let bad = self.b.ins().band(lhs, rhs);
                                self.fault_if(bad);
                            }
                            match (op, signed) {
                                (BinOp::Div, true) => self.b.ins().sdiv(x, y),
                                (BinOp::Div, false) => self.b.ins().udiv(x, y),
                                (BinOp::Rem, true) => self.b.ins().srem(x, y),
                                _ => self.b.ins().urem(x, y),
                            }
                        }
                        BinOp::BitAnd => self.b.ins().band(x, y),
                        BinOp::BitOr => self.b.ins().bor(x, y),
                        BinOp::BitXor => self.b.ins().bxor(x, y),
                        BinOp::Shl | BinOp::Shr => {
                            return Err(Error::unsupported("shift semantics pending native validation"));
                        }
                        _ => return Err(Error::unsupported("integer operation")),
                    }
                }
            }
            _ => return Err(Error::unsupported("binary nonnumeric value")),
        };
        Ok(Val { value: Some(value), ty: if cmp { Ty::Bool } else { a.ty } })
    }
    fn pair(&mut self, a: cl::Value, b: cl::Value) -> cl::Value {
        let slot = self.stack(16, 8);
        let ptr = self.b.ins().stack_addr(types::I64, slot, 0);
        self.b.ins().store(cl::MemFlags::new(), a, ptr, 0);
        self.b.ins().store(cl::MemFlags::new(), b, ptr, 8);
        ptr
    }
    fn collection(&mut self, p: &Place) -> Result<(cl::Value, cl::Value, Ty)> {
        match self.place_ty(p)? {
            Ty::Array(elem, n) => {
                let ptr = self.addr(p)?;
                let len = self.b.ins().iconst(types::I64, n as i64);
                Ok((ptr, len, *elem))
            }
            Ty::Slice(elem) => {
                if !matches!(p.proj.last(), Some(Proj::Deref)) {
                    return Err(Error::unsupported("unsized slice storage"));
                }
                let mut owner = p.clone();
                owner.proj.pop();
                let pair = self.read(&owner)?.value.unwrap();
                let ptr = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 0);
                let len = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 8);
                Ok((ptr, len, *elem))
            }
            _ => Err(Error::bug("noncollection length")),
        }
    }
    fn cast(&mut self, v: Val, ty: &Ty) -> Result<Val> {
        let value = v.value.ok_or_else(|| Error::bug("cast void"))?;
        let target = scalar(ty).ok_or_else(|| Error::unsupported("aggregate cast"))?;
        let out = match (&v.ty, ty) {
            (Ty::Int(src), Ty::Int(dst)) => {
                let (lo, hi) = dst.range();
                let (slo, shi) = src.range();
                let st = scalar(&v.ty).unwrap();
                if slo < lo {
                    let bad = self.b.ins().icmp_imm(if src.signed() { IntCC::SignedLessThan } else { IntCC::UnsignedLessThan }, value, lo as i64);
                    self.fault_if(bad);
                }
                if shi > hi {
                    let bad = self.b.ins().icmp_imm(if src.signed() { IntCC::SignedGreaterThan } else { IntCC::UnsignedGreaterThan }, value, hi as i64);
                    self.fault_if(bad);
                }
                if target.bits() < st.bits() {
                    self.b.ins().ireduce(target, value)
                } else if target.bits() > st.bits() {
                    if src.signed() { self.b.ins().sextend(target, value) } else { self.b.ins().uextend(target, value) }
                } else {
                    value
                }
            }
            (Ty::Int(i), Ty::Float(_)) => {
                if i.signed() {
                    self.b.ins().fcvt_from_sint(target, value)
                } else {
                    self.b.ins().fcvt_from_uint(target, value)
                }
            }
            (Ty::Float(_), Ty::Int(dst)) => {
                let truncated = self.b.ins().trunc(value);
                let source_ty = scalar(&v.ty).unwrap();
                let lo = if dst.signed() { -(2f64).powi(i32::from(int_bits(*dst)) - 1) } else { 0.0 };
                let hi = (2f64).powi(i32::from(int_bits(*dst)) - i32::from(dst.signed()));
                let (lo, hi) = if source_ty == types::F32 {
                    (self.b.ins().f32const(lo as f32), self.b.ins().f32const(hi as f32))
                } else {
                    (self.b.ins().f64const(lo), self.b.ins().f64const(hi))
                };
                let bad = self.b.ins().fcmp(FloatCC::Unordered, value, value);
                self.fault_if(bad);
                let bad = self.b.ins().fcmp(FloatCC::LessThan, truncated, lo);
                self.fault_if(bad);
                let bad = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, truncated, hi);
                self.fault_if(bad);
                let integer = if dst.signed() { self.b.ins().fcvt_to_sint(types::I64, truncated) } else { self.b.ins().fcvt_to_uint(types::I64, truncated) };
                if target.bits() < 64 { self.b.ins().ireduce(target, integer) } else { integer }
            }
            (Ty::Float(src), Ty::Float(dst)) => {
                if src == dst {
                    value
                } else if *dst == FloatTy::F64 {
                    self.b.ins().fpromote(target, value)
                } else {
                    self.b.ins().fdemote(target, value)
                }
            }
            _ => {
                return Err(Error::unsupported("float-to-integer or nonnumeric checked cast"));
            }
        };
        Ok(Val { value: Some(out), ty: ty.clone() })
    }
    fn term(&mut self, term: &Terminator) -> Result<()> {
        match term {
            Terminator::Goto(next) => {
                self.b.ins().jump(self.blocks[next.0 as usize], &[]);
            }
            Terminator::Switch { discr, cases, otherwise } => {
                let v = self.operand(discr)?.value.ok_or_else(|| Error::bug("void switch"))?;
                // Comparison chain preserves signed constants and arbitrary
                // sparse cases without changing IR control flow semantics.
                for (n, target) in cases {
                    let next = self.b.create_block();
                    let eq = self.b.ins().icmp_imm(IntCC::Equal, v, *n as i64);
                    self.b.ins().brif(eq, self.blocks[target.0 as usize], &[], next, &[]);
                    self.enter(next);
                }
                self.b.ins().jump(self.blocks[otherwise.0 as usize], &[]);
            }
            Terminator::Return => {
                let ret = self.read(&Place::local(RETURN))?;
                if let Some(sret) = self.sret {
                    if let Some(ptr) = ret.value {
                        self.copy(sret, ptr, layout::layout(self.t, &ret.ty)?.size);
                    }
                    self.b.ins().return_(&[]);
                } else {
                    self.b.ins().return_(&ret.value.into_iter().collect::<Vec<_>>());
                }
            }
            Terminator::Unreachable => {
                self.b.ins().trap(cl::TrapCode::user(2).unwrap());
            }
            Terminator::Suspend { .. } | Terminator::Abandon => return Err(Error::bug("async suspension reached the backend before frame lowering")),
            Terminator::Call { callee, args, dest, next, spawn, .. } => {
                if *spawn && !matches!(callee, Callee::TaskSpawn { .. }) {
                    return Err(Error::unsupported("spawn"));
                }
                if matches!(callee, Callee::Intrinsic(name) if name == "ffi.callback") {
                    let [Operand::Const(Const::Fn(id, _))] = args.as_slice() else { return Err(Error::bug("C callback target missing")); };
                    let adapter = *self.c_callbacks.get(id).ok_or_else(|| Error::bug("C callback adapter missing"))?;
                    let adapter = self.module.declare_func_in_func(adapter, self.b.func);
                    let address = self.b.ins().func_addr(types::I64, adapter);
                    let ty = self.place_ty(dest)?;
                    self.write(dest, Val { value: Some(address), ty })?;
                    let next = next.ok_or_else(|| Error::bug("C callback missing return edge"))?;
                    self.b.ins().jump(self.blocks[next.0 as usize], &[]);
                    return Ok(());
                }
                let args = args.iter().map(|o| self.operand(o)).collect::<Result<Vec<_>>>()?;
                let dest_ty = self.place_ty(dest)?;
                if matches!(callee, Callee::Intrinsic(name) if name == "Task.join") {
                    if args.len() != 1 || !matches!(&args[0].ty, Ty::Adt(id, ts) if Some(*id) == self.t.decls.task && ts == &vec![dest_ty.clone()]) {
                        return Err(Error::bug("task join ABI mismatch"));
                    }
                    let task = self.b.ins().load(types::I64, cl::MemFlags::new(), args[0].value.unwrap(), 0);
                    let result = self.runtime("tarn_rt_task_wait", &[task])[0];
                    let value = if let Some(ty) = scalar(&dest_ty) {
                        Some(self.b.ins().load(ty, cl::MemFlags::new(), result, 0))
                    } else if layout::layout(self.t, &dest_ty)?.size > 0 { Some(result) } else { None };
                    self.write(dest, Val { value, ty: dest_ty })?;
                    self.runtime("tarn_rt_task_release", &[task]);
                    if let Some(next) = next { self.b.ins().jump(self.blocks[next.0 as usize], &[]); }
                    else { return Err(Error::bug("task join without return edge")); }
                    return Ok(());
                }
                let value = match callee {
                    Callee::TaskSpawn { worker, drop_result, type_args, scoped } => {
                        if !type_args.is_empty() || args.len() != if *scoped { 2 } else { 1 } { return Err(Error::bug("task spawn shape")); }
                        let wf = &self.p.functions[worker.0 as usize].decl;
                        let df = &self.p.functions[drop_result.0 as usize].decl;
                        if wf.param_count != 1 || wf.local(LocalId(1)).ty != args[0].ty || df.param_count != 1 || df.local(LocalId(1)).ty != wf.ret || df.ret != Ty::Void || !matches!(&dest_ty, Ty::Adt(id, ts) if Some(*id)==self.t.decls.task && ts==&vec![wf.ret.clone()]) {
                            return Err(Error::bug("task worker/result ABI mismatch"));
                        }
                        let (wa, da) = self.task_adapters[worker];
                        let wa = self.module.declare_func_in_func(wa, self.b.func);
                        let da = self.module.declare_func_in_func(da, self.b.func);
                        let wa = self.b.ins().func_addr(types::I64, wa);
                        let da = self.b.ins().func_addr(types::I64, da);
                        let size = self.b.ins().iconst(types::I64, i64::from(layout::layout(self.t, &wf.ret)?.size));
                        let pair = args[0].value.ok_or_else(|| Error::bug("task callable missing"))?;
                        let code = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 0);
                        let env = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 8);
                        let task = self.runtime("tarn_rt_task_spawn", &[wa, da, size, code, env])[0];
                        let addr = self.addr(dest)?;
                        self.b.ins().store(cl::MemFlags::new(), task, addr, 0);
                        Val { value: Some(addr), ty: dest_ty.clone() }
                    }
                    Callee::Fn(id, type_args) => {
                        if !type_args.is_empty() {
                            return Err(Error::unsupported("generic function call"));
                        }
                        let f = &self.p.functions.get(id.0 as usize).ok_or_else(|| Error::bug("missing callee"))?.decl;
                        if args.len() != f.param_count as usize || dest_ty != f.ret {
                            return Err(Error::bug("direct call/return ABI mismatch"));
                        }
                        for (arg, p) in args.iter().zip(f.params()) {
                            if arg.ty != f.local(p).ty {
                                return Err(Error::bug("direct call parameter ABI mismatch"));
                            }
                        }
                        let mut values = Vec::new();
                        let aggregate = layout::layout(self.t, &f.ret)?.size > 0 && scalar(&f.ret).is_none();
                        let ptr = if aggregate {
                            let ptr = self.addr(dest)?;
                            values.push(ptr);
                            Some(ptr)
                        } else {
                            None
                        };
                        values.extend(args.iter().filter_map(|v| v.value));
                        let id = *self.ids.get(id).ok_or_else(|| Error::unsupported("external/intrinsic function body"))?;
                        let target = self.module.declare_func_in_func(id, self.b.func);
                        let call = self.b.ins().call(target, &values);
                        if aggregate { Val { value: ptr, ty: dest_ty.clone() } } else { Val { value: self.b.inst_results(call).first().copied(), ty: dest_ty.clone() } }
                    }
                    Callee::Virtual { method, type_args } => {
                        if !type_args.is_empty() {
                            return Err(Error::unsupported("generic dynamic method"));
                        }
                        let receiver = args.first().ok_or_else(|| Error::bug("dynamic receiver missing"))?;
                        let Ty::Ref(mutable, inner) = &receiver.ty else {
                            return Err(Error::bug("dynamic receiver representation"));
                        };
                        let Ty::Any(interface) = inner.as_ref() else {
                            return Err(Error::bug("dynamic receiver interface"));
                        };
                        let order = self.t.decls.interfaces.get(interface).ok_or_else(|| Error::bug("missing dynamic interface"))?;
                        let index = order.iter().position(|m| m == method).ok_or_else(|| Error::bug("method outside vtable"))?;
                        let declared = self.t.decls.fns.get(method).ok_or_else(|| Error::bug("missing dynamic signature"))?;
                        if declared.receiver == Some(tarn_types::ReceiverKind::RefMut) && !mutable {
                            return Err(Error::bug("mutable dynamic receiver mismatch"));
                        }
                        if !matches!(declared.receiver, Some(tarn_types::ReceiverKind::Ref | tarn_types::ReceiverKind::RefMut)) {
                            return Err(Error::unsupported("dynamic by-value receiver"));
                        }
                        if declared.params.len() + 1 != args.len() || declared.ret != dest_ty || declared.params.iter().zip(&args[1..]).any(|(t, v)| *t != v.ty) {
                            return Err(Error::bug("dynamic method ABI mismatch"));
                        }
                        let pair = receiver.value.ok_or_else(|| Error::bug("empty dynamic pair"))?;
                        let data = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 0);
                        let table = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 8);
                        let code = self.b.ins().load(types::I64, cl::MemFlags::new(), table, (index * 8) as i32);
                        let mut sig = self.module.make_signature();
                        let mut values = Vec::new();
                        let aggregate = layout::layout(self.t, &dest_ty)?.size > 0 && scalar(&dest_ty).is_none();
                        let result = if aggregate {
                            let ptr = self.addr(dest)?;
                            sig.params.push(cl::AbiParam::new(types::I64));
                            values.push(ptr);
                            Some(ptr)
                        } else {
                            None
                        };
                        sig.params.push(cl::AbiParam::new(types::I64));
                        values.push(data);
                        for v in &args[1..] {
                            if let Some(ty) = abi_type(self.t, &v.ty)? {
                                sig.params.push(cl::AbiParam::new(ty));
                                values.push(v.value.unwrap());
                            }
                        }
                        if let Some(ty) = scalar(&dest_ty) {
                            sig.returns.push(cl::AbiParam::new(ty));
                        }
                        let sig = self.b.import_signature(sig);
                        let call = self.b.ins().call_indirect(sig, code, &values);
                        Val { value: if aggregate { result } else { self.b.inst_results(call).first().copied() }, ty: dest_ty.clone() }
                    }
                    Callee::Value(o) => {
                        let closure = self.operand(o)?;
                        let callable_ty = if let Ty::Ref(_, inner) = &closure.ty { inner.as_ref() } else { &closure.ty };
                        // A source async computation is polled as `mut fn(&Waker) Progress<T>`.
                        let polled;
                        let (params, ret) = match callable_ty {
                            Ty::Fn(_, params, ret) => (params, ret),
                            Ty::Async(output) => {
                                let (Some(waker), Some(progress)) = (self.t.decls.exec_waker, self.t.decls.exec_progress) else {
                                    return Err(Error::bug("async poll without trusted declarations"));
                                };
                                polled = (vec![Ty::Ref(false, Box::new(Ty::Adt(waker, Vec::new())))], Box::new(Ty::Adt(progress, vec![(**output).clone()])));
                                (&polled.0, &polled.1)
                            }
                            _ => return Err(Error::bug("indirect nonfunction")),
                        };
                        if params.len() != args.len() || **ret != dest_ty || params.iter().zip(&args).any(|(t, v)| *t != v.ty) {
                            return Err(Error::bug("indirect ABI mismatch"));
                        }
                        let pair = closure.value.unwrap();
                        let code = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 0);
                        let env = self.b.ins().load(types::I64, cl::MemFlags::new(), pair, 8);
                        let mut sig = self.module.make_signature();
                        let mut values = Vec::new();
                        let aggregate = layout::layout(self.t, ret)?.size > 0 && scalar(ret).is_none();
                        let result = if aggregate {
                            let ptr = self.addr(dest)?;
                            sig.params.push(cl::AbiParam::new(types::I64));
                            values.push(ptr);
                            Some(ptr)
                        } else {
                            None
                        };
                        sig.params.push(cl::AbiParam::new(types::I64));
                        values.push(env);
                        for v in &args {
                            if let Some(ty) = abi_type(self.t, &v.ty)? {
                                sig.params.push(cl::AbiParam::new(ty));
                                values.push(v.value.unwrap());
                            }
                        }
                        if let Some(ty) = scalar(ret) {
                            sig.returns.push(cl::AbiParam::new(ty));
                        }
                        let sig = self.b.import_signature(sig);
                        let call = self.b.ins().call_indirect(sig, code, &values);
                        Val { value: if aggregate { result } else { self.b.inst_results(call).first().copied() }, ty: dest_ty.clone() }
                    }
                    // 11A has only handle-owned unscoped tasks. Lexical scope
                    // completion has no child records until scoped spawn (11B).
                    Callee::Builtin(Builtin::JoinScope) => Val { value: None, ty: Ty::Void },
                    Callee::Builtin(Builtin::Print) => {
                        if args.len() != 1 {
                            return Err(Error::bug("print arity"));
                        }
                        // Copy primitives are passed by value; owned values
                        // are observed through the frontend's explicit borrow.
                        let (inner, v) = if let Ty::Ref(_, inner) = &args[0].ty {
                            let ty = scalar(inner).ok_or_else(|| Error::unsupported("printing aggregate"))?;
                            (inner.as_ref(), self.b.ins().load(ty, cl::MemFlags::new(), args[0].value.unwrap(), 0))
                        } else {
                            (&args[0].ty, args[0].value.ok_or_else(|| Error::unsupported("print void"))?)
                        };
                        let ty = scalar(inner).ok_or_else(|| Error::unsupported("printing aggregate"))?;
                        let (name, v) = match inner {
                            Ty::Int(i) => {
                                let v = if ty.bits() < 64 { if i.signed() { self.b.ins().sextend(types::I64, v) } else { self.b.ins().uextend(types::I64, v) } } else { v };
                                (if i.signed() { "tarn_rt_print_i64" } else { "tarn_rt_print_u64" }, v)
                            }
                            Ty::Bool => ("tarn_rt_print_bool", v),
                            Ty::Str => ("tarn_rt_print_string", v),
                            Ty::Float(f) => {
                                let v = if *f == FloatTy::F32 { self.b.ins().fpromote(types::F64, v) } else { v };
                                ("tarn_rt_print_f64", v)
                            }
                            _ => return Err(Error::unsupported("print type")),
                        };
                        self.runtime(name, &[v]);
                        Val { value: None, ty: Ty::Void }
                    }
                    Callee::Builtin(Builtin::Panic) => {
                        if args.len() != 1 {
                            return Err(Error::bug("panic arity"));
                        }
                        let v = match &args[0].ty {
                            Ty::Str => args[0].value.unwrap(),
                            Ty::Ref(_, x) if **x == Ty::Str => self.b.ins().load(types::I64, cl::MemFlags::new(), args[0].value.unwrap(), 0),
                            _ => return Err(Error::unsupported("panic nonstring")),
                        };
                        self.runtime("tarn_rt_panic", &[v]);
                        self.b.ins().trap(cl::TrapCode::user(1).unwrap());
                        return Ok(());
                    }
                    Callee::Intrinsic(name) => self.intrinsic(name, &args, &dest_ty)?,
                    _ => return Err(Error::unsupported(format!("callee {callee:?}"))),
                };
                self.write(dest, value)?;
                if let Some(next) = next {
                    self.b.ins().jump(self.blocks[next.0 as usize], &[]);
                } else {
                    self.b.ins().trap(cl::TrapCode::user(2).unwrap());
                }
            }
        }
        Ok(())
    }
    fn string_operation(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Option<Val>> {
        let flags = cl::MemFlags::new();
        let string_ref = Ty::Ref(false, Box::new(Ty::Str));
        let byte_slice = Ty::Ref(false, Box::new(Ty::Slice(Box::new(Ty::Int(IntTy::U8)))));
        if name == "string.bytes" && args.len() == 1 && args[0].ty == string_ref && *dest == byte_slice {
            let string = self.b.ins().load(types::I64, flags, args[0].value.unwrap(), 0);
            let len = self.b.ins().load(types::I64, flags, string, 0);
            let bytes = self.b.ins().iadd_imm(string, 8);
            return Ok(Some(Val { value: Some(self.pair(bytes, len)), ty: dest.clone() }));
        }
        if name == "string.copy_range" && args.len() == 3 && args[0].ty == string_ref && *dest == Ty::Str
            && args[1].ty == Ty::Int(IntTy::Usize) && args[2].ty == Ty::Int(IntTy::Usize)
        {
            let string = self.b.ins().load(types::I64, flags, args[0].value.unwrap(), 0);
            let (start, end) = (args[1].value.unwrap(), args[2].value.unwrap());
            return Ok(Some(Val { value: Some(self.runtime("tarn_rt_string_range", &[string, start, end])[0]), ty: Ty::Str }));
        }
        if name == "core.string_from_utf8" && args.len() == 1 && args[0].ty == byte_slice {
            let Ty::Adt(option, params) = dest else { return Err(Error::bug("UTF-8 result type")) };
            let def = self.t.decls.enums.get(option).ok_or_else(|| Error::bug("UTF-8 option definition"))?;
            if *option != self.t.prelude.option || params != &[Ty::Str] { return Err(Error::bug("UTF-8 option payload")); }
            let some = def.variants.iter().position(|v| v.name == "Some").ok_or_else(|| Error::bug("Option.Some"))?;
            let none = def.variants.iter().position(|v| v.name == "None").ok_or_else(|| Error::bug("Option.None"))?;
            let l = layout::layout(self.t, dest)?;
            let payload = l.variants.get(some).and_then(|v| v.first()).filter(|(_, ty)| *ty == Ty::Str).ok_or_else(|| Error::bug("UTF-8 option layout"))?.0;
            let addr = args[0].value.unwrap();
            let data = self.b.ins().load(types::I64, flags, addr, 0);
            let len = self.b.ins().load(types::I64, flags, addr, 8);
            let string = self.runtime("tarn_rt_string_utf8", &[data, len])[0];
            let valid = self.b.ins().icmp_imm(IntCC::NotEqual, string, 0);
            let yes = self.b.ins().iconst(types::I32, some as i64);
            let no = self.b.ins().iconst(types::I32, none as i64);
            let tag = self.b.ins().select(valid, yes, no);
            let slot = self.stack(l.size, l.align);
            let out = self.b.ins().stack_addr(types::I64, slot, 0);
            self.b.ins().store(flags, tag, out, 0);
            self.b.ins().store(flags, string, out, payload as i32);
            return Ok(Some(Val { value: Some(out), ty: dest.clone() }));
        }
        if matches!(name, "string.add" | "string.eq" | "string.ne" | "string.lt" | "string.le" | "string.gt" | "string.ge")
            && args.len() == 2 && args.iter().all(|a| a.ty == string_ref) {
            let a = self.b.ins().load(types::I64, flags, args[0].value.unwrap(), 0);
            let b = self.b.ins().load(types::I64, flags, args[1].value.unwrap(), 0);
            let value = if name == "string.add" && *dest == Ty::Str {
                self.runtime("tarn_rt_string_add", &[a, b])[0]
            } else if *dest == Ty::Bool && name != "string.add" {
                let order = self.runtime("tarn_rt_string_compare", &[a, b])[0];
                let cc = match name {
                    "string.eq" => IntCC::Equal, "string.ne" => IntCC::NotEqual,
                    "string.lt" => IntCC::SignedLessThan, "string.le" => IntCC::SignedLessThanOrEqual,
                    "string.gt" => IntCC::SignedGreaterThan, _ => IntCC::SignedGreaterThanOrEqual,
                };
                self.b.ins().icmp_imm(cc, order, 0)
            } else { return Err(Error::bug("string operator result")); };
            return Ok(Some(Val { value: Some(value), ty: dest.clone() }));
        }
        Ok(None)
    }
    fn intrinsic(&mut self, name: &str, args: &[Val], dest: &Ty) -> Result<Val> {
        if let Some(result) = self.string_operation(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.tasks(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.filesystem(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.process(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.networking(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.synchronization(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.vector(name, args, dest)? { return Ok(result); }
        if let Some(result) = self.ffi(name, args, dest)? { return Ok(result); }
        // Explicit modular arithmetic (ADR 0060): no overflow check by design.
        if let ([x, y], Ty::Int(i)) = (args, dest)
            && let Some(op) = name.strip_prefix(i.name()).and_then(|rest| rest.strip_prefix('.'))
            && matches!(op, "wrapping_add" | "wrapping_sub" | "wrapping_mul")
        {
            let (x, y) = (x.value.unwrap(), y.value.unwrap());
            let value = match op {
                "wrapping_add" => self.b.ins().iadd(x, y),
                "wrapping_sub" => self.b.ins().isub(x, y),
                _ => self.b.ins().imul(x, y),
            };
            return Ok(Val { value: Some(value), ty: dest.clone() });
        }
        if args.len() != 1 {
            return Err(Error::unsupported(format!("intrinsic {name}")));
        }
        let a = args[0].value.unwrap();
        let value = match (name, &args[0].ty) {
            ("f32.sqrt" | "f64.sqrt", Ty::Float(_)) => self.b.ins().sqrt(a),
            ("f32.abs" | "f64.abs", Ty::Float(_)) => self.b.ins().fabs(a),
            (_, Ty::Int(i)) if name == format!("{}.abs", i.name()) && i.signed() => {
                let min = -(1i128 << (int_bits(*i) - 1));
                let bad = self.b.ins().icmp_imm(IntCC::Equal, a, min as i64);
                self.fault_if(bad);
                let negative = self.b.ins().icmp_imm(IntCC::SignedLessThan, a, 0);
                let neg = self.b.ins().ineg(a);
                self.b.ins().select(negative, neg, a)
            }
            ("string.len" | "string.is_empty" | "string.clone", Ty::Ref(_, inner)) if **inner == Ty::Str => {
                let string = self.b.ins().load(types::I64, cl::MemFlags::new(), a, 0);
                let len = self.b.ins().load(types::I64, cl::MemFlags::new(), string, 0);
                match name {
                    "string.len" => len,
                    "string.is_empty" => self.b.ins().icmp_imm(IntCC::Equal, len, 0),
                    _ => {
                        let bytes = self.b.ins().iadd_imm(string, 8);
                        self.runtime("tarn_rt_string", &[bytes, len])[0]
                    }
                }
            }
            _ => return Err(Error::unsupported(format!("intrinsic {name}"))),
        };
        Ok(Val { value: Some(value), ty: dest.clone() })
    }
    fn drop_plan(&mut self, d: &post::Drop) -> Result<()> {
        match d {
            post::Drop::Value(p) => self.drop_value(p)?,
            post::Drop::Guard(id, d) => {
                let c = match self.flags[id.0 as usize] {
                    Flag::Bit(v) => self.b.use_var(v),
                    Flag::FrameBit(_) => {
                        let (addr, _) = self.flag_memory(*id)?;
                        self.b.ins().load(types::I8, cl::MemFlags::new(), addr, 0)
                    }
                    _ => return Err(Error::bug("guard bitmap")),
                };
                let yes = self.b.create_block();
                let next = self.b.create_block();
                self.b.ins().brif(c, yes, &[], next, &[]);
                self.enter(yes);
                self.drop_plan(d)?;
                self.b.ins().jump(next, &[]);
                self.enter(next);
            }
            post::Drop::Fields { fields, .. } => {
                for (_, d) in fields {
                    self.drop_plan(d)?;
                }
            }
            post::Drop::Variants { place, variants } => {
                let addr = self.addr(place)?;
                let tag = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
                // Same shape as enum drop glue: one range fault, then only the
                // variants whose plan destroys something.
                let invalid = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, tag, variants.len() as i64);
                self.fault_tag_if(invalid);
                let next = self.b.create_block();
                for (v, fields) in variants.iter().enumerate() {
                    if fields.is_empty() {
                        continue;
                    }
                    let yes = self.b.create_block();
                    let no = self.b.create_block();
                    let c = self.b.ins().icmp_imm(IntCC::Equal, tag, v as i64);
                    self.b.ins().brif(c, yes, &[], no, &[]);
                    self.enter(yes);
                    for (_, d) in fields {
                        self.drop_plan(d)?;
                    }
                    self.b.ins().jump(next, &[]);
                    self.enter(no);
                }
                self.b.ins().jump(next, &[]);
                self.enter(next);
            }
            post::Drop::Remaining { place, flag } => {
                if !matches!(self.flags[flag.0 as usize], Flag::Bits(..) | Flag::FrameBits(..)) {
                    return Err(Error::bug("remaining without bitmap"));
                }
                let (base, n) = self.flag_memory(*flag)?;
                for i in 0..n {
                    let c = self.b.ins().load(types::I8, cl::MemFlags::new(), base, i as i32);
                    let yes = self.b.create_block();
                    let next = self.b.create_block();
                    self.b.ins().brif(c, yes, &[], next, &[]);
                    self.enter(yes);
                    self.drop_array_element(place, i)?;
                    self.b.ins().jump(next, &[]);
                    self.enter(next);
                }
            }
        }
        Ok(())
    }
    fn drop_array_element(&mut self, place: &Place, i: u64) -> Result<()> {
        let Ty::Array(elem, _) = self.place_ty(place)? else {
            return Err(Error::bug("drop array type"));
        };
        let addr = self.addr(place)?;
        let offset = i.checked_mul(u64::from(layout::layout(self.t, &elem)?.size)).ok_or_else(|| Error::bug("drop offset overflow"))?;
        let addr = self.b.ins().iadd_imm(addr, offset as i64);
        self.drop_at(addr, &elem)
    }
    fn drop_value(&mut self, p: &Place) -> Result<()> {
        let ty = self.place_ty(p)?;
        if matches!(ty, Ty::Str) {
            let ptr = self.read(p)?.value.unwrap();
            self.runtime("tarn_rt_drop_string", &[ptr]);
            return Ok(());
        }
        if matches!(ty, Ty::Ref(..)) || self.t.decls.is_copy(&ty) {
            return Ok(());
        }
        let addr = self.addr(p)?;
        self.drop_at(addr, &ty)
    }
    /// Fault with the corrupted-enum-tag code when `condition` holds.
    fn fault_tag_if(&mut self, condition: cl::Value) {
        let fail = self.b.create_block();
        let next = self.b.create_block();
        self.b.set_cold_block(fail);
        self.b.ins().brif(condition, fail, &[], next, &[]);
        self.enter(fail);
        self.runtime("tarn_rt_fault", &[]);
        self.b.ins().trap(cl::TrapCode::user(2).unwrap());
        self.enter(next);
    }
    /// Whether `drop_at` emits any destruction for `ty`. Mirrors its cases:
    /// trusted native resources, then Copy/references (nothing), then owned
    /// strings, callables, containers and aggregates by their fields.
    fn needs_drop(&self, ty: &Ty) -> bool {
        let d = &self.t.decls;
        if let Ty::Adt(id, _) = ty {
            let native = [d.exec_waker, d.exec_state, d.net_poll, d.net_tls, d.process_owner, d.process_pipe, d.fs_file, d.fs_directory];
            if native.contains(&Some(*id)) || d.net_sockets.contains(id) {
                return true;
            }
        }
        if d.is_copy(ty) || matches!(ty, Ty::Ref(..)) {
            return false;
        }
        match ty {
            Ty::Array(elem, n) => *n > 0 && self.needs_drop(elem),
            Ty::Adt(id, _) if [d.vec, d.async_task, d.task_ref, d.mutex, d.mutex_guard, d.task].contains(&Some(*id)) || d.atomics.contains_key(id) => true,
            Ty::Adt(..) => match layout::layout(self.t, ty) {
                Ok(l) => l.fields.iter().chain(l.variants.iter().flatten()).any(|(_, ty)| self.needs_drop(ty)),
                Err(_) => true,
            },
            _ => true,
        }
    }
    fn drop_at(&mut self, addr: cl::Value, ty: &Ty) -> Result<()> {
        if let Ty::Adt(id, _) = ty {
            let helper = if Some(*id) == self.t.decls.exec_waker { Some("tarn_rt_net_waker_drop") }
                else if Some(*id) == self.t.decls.exec_state { Some("tarn_rt_net_exec_drop") } else { None };
            if let Some(helper) = helper {
                let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime(helper, &[native]);
                return Ok(());
            }
        }
        if matches!(ty, Ty::Adt(id, _) if Some(*id) == self.t.decls.net_poll) {
            let handle = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_net_poll_drop", &[handle]);
            return Ok(());
        }
        if matches!(ty, Ty::Adt(id, _) if Some(*id) == self.t.decls.net_tls) {
            let handle = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_net_tls_drop", &[handle]);
            return Ok(());
        }
        for (owner, helper) in [(self.t.decls.process_owner, "tarn_rt_process_drop"), (self.t.decls.process_pipe, "tarn_rt_process_pipe_drop")] {
            if matches!(ty, Ty::Adt(id, _) if Some(*id) == owner) {
                let handle = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
                self.runtime(helper, &[handle]);
                return Ok(());
            }
        }
        if matches!(ty, Ty::Adt(id, _) if Some(*id) == self.t.decls.fs_file) {
            let fd = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_fs_drop", &[fd]);
            return Ok(());
        }
        if matches!(ty, Ty::Adt(id, _) if Some(*id) == self.t.decls.fs_directory) {
            let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_fs_dir_drop", &[native]);
            return Ok(());
        }
        if matches!(ty, Ty::Adt(id, _) if self.t.decls.net_sockets.contains(id)) {
            let fd = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
            self.runtime("tarn_rt_net_drop", &[fd]);
            return Ok(());
        }
        // Copy, references and aggregates of them destroy nothing.
        if !self.needs_drop(ty) {
            return Ok(());
        }
        match ty {
            Ty::Fn(..) | Ty::Async(_) => {
                let env = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 8);
                self.runtime("tarn_rt_env_drop", &[env]);
            }
            Ty::Str => {
                let ptr = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime("tarn_rt_drop_string", &[ptr]);
            }
            Ty::Array(elem, n) => {
                let size = layout::layout(self.t, elem)?.size;
                for i in 0..*n {
                    let ptr = self.b.ins().iadd_imm(addr, (i * u64::from(size)) as i64);
                    self.drop_at(ptr, elem)?;
                }
            }
            Ty::Adt(id, _) if Some(*id) == self.t.decls.vec => self.drop_vector(addr, ty)?,
            Ty::Adt(id, _) if Some(*id) == self.t.decls.async_task => self.drop_async_task(addr, ty)?,
            Ty::Adt(id, _) if Some(*id) == self.t.decls.task_ref => self.drop_task_ref(addr),
            Ty::Adt(id, args) if Some(*id) == self.t.decls.mutex => {
                // Complete initialized value selected by verified post-drop.
                let l = layout::layout(self.t, ty)?;
                let payload = self.b.ins().iadd_imm(addr, i64::from(l.fields[1].0));
                self.drop_at(payload, &args[0])?;
                let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime("tarn_rt_mutex_destroy", &[native]);
            }
            Ty::Adt(id, _) if Some(*id) == self.t.decls.mutex_guard => {
                let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime("tarn_rt_mutex_unlock", &[native]);
            }
            Ty::Adt(id, _) if self.t.decls.atomics.contains_key(id) => {
                let native = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime("tarn_rt_atomic_destroy", &[native]);
            }
            Ty::Adt(id, _) if Some(*id) == self.t.decls.task => {
                let task = self.b.ins().load(types::I64, cl::MemFlags::new(), addr, 0);
                self.runtime("tarn_rt_task_drop", &[task]);
            }
            Ty::Adt(..) => {
                let l = layout::layout(self.t, ty)?;
                if l.variants.is_empty() {
                    for (offset, ty) in l.fields {
                        let ptr = self.b.ins().iadd_imm(addr, i64::from(offset));
                        self.drop_at(ptr, &ty)?;
                    }
                } else {
                    let tag = self.b.ins().load(types::I32, cl::MemFlags::new(), addr, 0);
                    // A corrupted tag still faults; only variants with fields that
                    // need destruction are tested, so glue stays proportional to
                    // what it destroys (a 1600-variant enum of integers emitted
                    // 1600 comparisons per drop site before).
                    let invalid = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, tag, l.variants.len() as i64);
                    self.fault_tag_if(invalid);
                    let next = self.b.create_block();
                    for (v, fields) in l.variants.iter().enumerate() {
                        if !fields.iter().any(|(_, ty)| self.needs_drop(ty)) {
                            continue;
                        }
                        let yes = self.b.create_block();
                        let no = self.b.create_block();
                        let c = self.b.ins().icmp_imm(IntCC::Equal, tag, v as i64);
                        self.b.ins().brif(c, yes, &[], no, &[]);
                        self.enter(yes);
                        for (offset, ty) in fields {
                            let ptr = self.b.ins().iadd_imm(addr, i64::from(*offset));
                            self.drop_at(ptr, ty)?;
                        }
                        self.b.ins().jump(next, &[]);
                        self.enter(no);
                    }
                    self.b.ins().jump(next, &[]);
                    self.enter(next);
                }
            }
            _ => return Err(Error::unsupported(format!("drop glue {ty:?}"))),
        }
        Ok(())
    }
}

fn validate_table(p: &post::Program, t: &Typed, interface: tarn_resolve::SymbolId, concrete: &Ty, methods: &[FunctionId], ty: &Ty) -> Result<()> {
    if !matches!(ty,Ty::Ref(_,inner) if **inner==Ty::Any(interface)) {
        return Err(Error::bug("invalid dynamic destination"));
    }
    let order = t.decls.interfaces.get(&interface).ok_or_else(|| Error::bug("missing vtable declaration"))?;
    if order.len() != methods.len() {
        return Err(Error::bug("vtable length mismatch"));
    }
    let target = t.decls.impl_target(concrete).ok_or_else(|| Error::bug("invalid vtable concrete type"))?;
    let resolved = t.decls.implementations.get(&(interface, target)).ok_or_else(|| Error::bug("concrete/interface mismatch"))?;
    if resolved.len() != methods.len() {
        return Err(Error::bug("incomplete resolved vtable"));
    }
    for (n, id) in methods.iter().enumerate() {
        let f = &p.functions.get(id.0 as usize).ok_or_else(|| Error::bug("vtable function outside program"))?.decl;
        let sig = t.decls.fns.get(&order[n]).ok_or_else(|| Error::bug("missing interface method signature"))?;
        if f.symbol != Some(resolved[n]) {
            return Err(Error::bug("wrong vtable implementation"));
        }
        let mutable = match sig.receiver {
            Some(tarn_types::ReceiverKind::Ref) => false,
            Some(tarn_types::ReceiverKind::RefMut) => true,
            _ => return Err(Error::unsupported("dynamic by-value receiver")),
        };
        let params: Vec<_> = f.params().map(|p| f.local(p).ty.clone()).collect();
        let expected: Vec<_> = std::iter::once(Ty::Ref(mutable, Box::new(concrete.clone()))).chain(sig.params.clone()).collect();
        if params != expected || f.ret != sig.ret {
            return Err(Error::bug("vtable signature mismatch"));
        }
    }
    Ok(())
}

#[path = "synchronization.rs"]
mod synchronization;

#[path = "vectors.rs"]
mod vectors;

#[path = "tasks.rs"]
mod tasks;

#[path = "networking.rs"]
mod networking;

#[path = "filesystem.rs"]
mod filesystem;

#[path = "ffi.rs"]
mod ffi;
#[path = "process.rs"]
mod process;

/// Compile translated functions to machine code on all cores, then define
/// them in the module sequentially in their original order, so the object
/// file is byte-identical to a sequential build.
fn compile_in_parallel(module: &mut ObjectModule, mut translated: Vec<(FuncId, &str, cranelift_codegen::Context)>, verify: bool) -> Result<()> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let isa = module.isa();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(translated.len().max(1));
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<(FuncId, &str, cranelift_codegen::Context)>>> = translated.drain(..).map(|x| Mutex::new(Some(x))).collect();
    let failure: Mutex<Option<(usize, Error)>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = slots.get(i) else { break };
                    let mut guard = slot.lock().unwrap();
                    let (_, name, ctx) = guard.as_mut().unwrap();
                    let result = if verify { cranelift_codegen::verify_function(&ctx.func, isa).map_err(|e| Error::bug(format!("{name}: {e}"))) } else { Ok(()) }
                        .and_then(|()| ctx.compile(isa, &mut Default::default()).map(|_| ()).map_err(|e| Error::bug(format!("{name}: {:?}", e.inner))));
                    if let Err(e) = result {
                        // Report the first failing function in program order.
                        let mut first = failure.lock().unwrap();
                        if first.as_ref().is_none_or(|(at, _)| i < *at) {
                            *first = Some((i, e));
                        }
                    }
                }
            });
        }
    });
    if let Some((_, e)) = failure.into_inner().unwrap() {
        return Err(e);
    }
    for slot in slots {
        let (id, name, ctx) = slot.into_inner().unwrap().unwrap();
        let code = ctx.compiled_code().ok_or_else(|| Error::bug(format!("{name}: missing compiled code")))?;
        let relocs: Vec<_> = code.buffer.relocs().iter().map(|r| cranelift_module::ModuleReloc::from_mach_reloc(r, &ctx.func, id)).collect();
        module.define_function_bytes(id, code.buffer.alignment as u64, code.buffer.data(), &relocs).map_err(|e| Error::bug(format!("{name}: {e}")))?;
    }
    Ok(())
}
