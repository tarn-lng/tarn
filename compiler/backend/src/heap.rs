//! Freestanding heap runtime (ADR 0101, P3). The hosted C runtime implements
//! strings, vector storage and closure environments on top of libc; a `*-none`
//! object has no libc, so the same operations are emitted here as Cranelift
//! functions inside the object. They need only the allocator the boot code
//! provides (`tarn_alloc`, `tarn_free`, `tarn_realloc`) and `tarn_rt_fault`.
//! Behavior mirrors `runtime/native.c`; overflow and exhaustion fault.
use crate::{Error, Result};
use cranelift_codegen::ir::{self as cl, InstBuilder, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataId, FuncId, Module};
use cranelift_object::ObjectModule;
use std::collections::HashMap;

/// Hosted runtime operations that freestanding objects implement locally.
pub const LOCAL: [&str; 11] = [
    "tarn_rt_string", "tarn_rt_string_utf8", "tarn_rt_string_add", "tarn_rt_string_compare", "tarn_rt_string_range",
    "tarn_rt_drop_string", "tarn_rt_env_alloc", "tarn_rt_env_free", "tarn_rt_env_drop", "tarn_rt_vec_grow", "tarn_rt_vec_extend",
];

/// Runtime functions a local helper calls; declared before its body is built.
pub fn callees(name: &str) -> &'static [&'static str] {
    match name {
        "tarn_rt_string" => &["tarn_alloc", "tarn_rt_fault"],
        "tarn_rt_string_utf8" => &["tarn_rt_string"],
        "tarn_rt_string_add" => &["tarn_alloc", "tarn_rt_fault"],
        "tarn_rt_string_range" => &["tarn_rt_string", "tarn_rt_fault"],
        "tarn_rt_drop_string" => &["tarn_free"],
        "tarn_rt_env_alloc" => &["tarn_alloc", "tarn_rt_fault"],
        "tarn_rt_env_free" => &["tarn_free"],
        "tarn_rt_vec_grow" => &["tarn_realloc", "tarn_rt_fault"],
        "tarn_rt_vec_extend" => &["tarn_rt_vec_grow", "tarn_rt_fault"],
        _ => &[],
    }
}

type B<'a, 'b> = &'a mut FunctionBuilder<'b>;

fn call(b: B, f: cl::FuncRef, args: &[cl::Value]) -> Vec<cl::Value> {
    let c = b.ins().call(f, args);
    b.inst_results(c).to_vec()
}

/// Branch to a cold faulting block when `cond` holds, else continue.
fn fault_if(b: B, cond: cl::Value, fault: cl::FuncRef) {
    let (fail, next) = (b.create_block(), b.create_block());
    b.set_cold_block(fail);
    b.ins().brif(cond, fail, &[], next, &[]);
    b.switch_to_block(fail);
    call(b, fault, &[]);
    b.ins().trap(cl::TrapCode::user(1).unwrap());
    b.switch_to_block(next);
}

fn alloc(b: B, alloc: cl::FuncRef, fault: cl::FuncRef, size: cl::Value) -> cl::Value {
    let p = call(b, alloc, &[size])[0];
    let null = b.ins().icmp_imm(IntCC::Equal, p, 0);
    fault_if(b, null, fault);
    p
}

/// Forward byte copy of `n` bytes between non-overlapping regions: words
/// first, then the tail.
fn copy(b: B, dst: cl::Value, src: cl::Value, n: cl::Value) {
    let flags = cl::MemFlags::new();
    let zero = b.ins().iconst(types::I64, 0);
    let (head, body, done) = (b.create_block(), b.create_block(), b.create_block());
    b.append_block_param(head, types::I64);
    b.ins().jump(head, &[zero.into()]);
    b.switch_to_block(head);
    let i = b.block_params(head)[0];
    let end = b.ins().iadd_imm(i, 8);
    let more = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, n);
    b.ins().brif(more, body, &[], done, &[]);
    b.switch_to_block(body);
    let (from, to) = (b.ins().iadd(src, i), b.ins().iadd(dst, i));
    let v = b.ins().load(types::I64, flags, from, 0);
    b.ins().store(flags, v, to, 0);
    b.ins().jump(head, &[end.into()]);
    b.switch_to_block(done);
    let (dst2, src2) = (b.ins().iadd(dst, i), b.ins().iadd(src, i));
    let rest = b.ins().isub(n, i);
    copy_tail(b, dst2, src2, rest);
}

fn copy_tail(b: B, dst: cl::Value, src: cl::Value, n: cl::Value) {
    let flags = cl::MemFlags::new();
    let zero = b.ins().iconst(types::I64, 0);
    let (head, body, done) = (b.create_block(), b.create_block(), b.create_block());
    b.append_block_param(head, types::I64);
    b.ins().jump(head, &[zero.into()]);
    b.switch_to_block(head);
    let i = b.block_params(head)[0];
    let more = b.ins().icmp(IntCC::UnsignedLessThan, i, n);
    b.ins().brif(more, body, &[], done, &[]);
    b.switch_to_block(body);
    let (from, to) = (b.ins().iadd(src, i), b.ins().iadd(dst, i));
    let v = b.ins().load(types::I8, flags, from, 0);
    b.ins().store(flags, v, to, 0);
    let next = b.ins().iadd_imm(i, 1);
    b.ins().jump(head, &[next.into()]);
    b.switch_to_block(done);
}

/// Define the local helper `name`. `pool` is the string-literal image
/// (data object, byte length): `drop_string` must not free those.
pub fn define(module: &mut ObjectModule, id: FuncId, name: &str, ids: &HashMap<&'static str, FuncId>, pool: (DataId, u64)) -> Result<()> {
    let mut ctx = module.make_context();
    ctx.func.signature = module.declarations().get_function_decl(id).signature.clone();
    let mut fb = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb);
        let b = &mut builder;
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let p = b.block_params(entry).to_vec();
        let mut f = |b: B, n: &str| module.declare_func_in_func(ids[n], b.func);
        let flags = cl::MemFlags::new();
        match name {
            "tarn_rt_string" => {
                let (a, fault) = (f(b, "tarn_alloc"), f(b, "tarn_rt_fault"));
                let total = b.ins().iadd_imm(p[1], 8);
                let wrapped = b.ins().icmp(IntCC::UnsignedLessThan, total, p[1]);
                fault_if(b, wrapped, fault);
                let s = alloc(b, a, fault, total);
                b.ins().store(flags, p[1], s, 0);
                let body = b.ins().iadd_imm(s, 8);
                copy(b, body, p[0], p[1]);
                b.ins().return_(&[s]);
            }
            "tarn_rt_string_add" => {
                let (a, fault) = (f(b, "tarn_alloc"), f(b, "tarn_rt_fault"));
                let la = b.ins().load(types::I64, flags, p[0], 0);
                let lb = b.ins().load(types::I64, flags, p[1], 0);
                let len = b.ins().iadd(la, lb);
                let wrapped = b.ins().icmp(IntCC::UnsignedLessThan, len, la);
                fault_if(b, wrapped, fault);
                let total = b.ins().iadd_imm(len, 8);
                let wrapped = b.ins().icmp(IntCC::UnsignedLessThan, total, len);
                fault_if(b, wrapped, fault);
                let s = alloc(b, a, fault, total);
                b.ins().store(flags, len, s, 0);
                let (body, from_a, from_b) = (b.ins().iadd_imm(s, 8), b.ins().iadd_imm(p[0], 8), b.ins().iadd_imm(p[1], 8));
                copy(b, body, from_a, la);
                let after = b.ins().iadd(body, la);
                copy(b, after, from_b, lb);
                b.ins().return_(&[s]);
            }
            "tarn_rt_string_compare" => {
                let la = b.ins().load(types::I64, flags, p[0], 0);
                let lb = b.ins().load(types::I64, flags, p[1], 0);
                let shorter = b.ins().icmp(IntCC::UnsignedLessThan, la, lb);
                let len = b.ins().select(shorter, la, lb);
                let zero = b.ins().iconst(types::I64, 0);
                let (head, body, differ, done) = (b.create_block(), b.create_block(), b.create_block(), b.create_block());
                b.append_block_param(head, types::I64);
                b.ins().jump(head, &[zero.into()]);
                b.switch_to_block(head);
                let i = b.block_params(head)[0];
                let more = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                b.ins().brif(more, body, &[], done, &[]);
                b.switch_to_block(body);
                let (pa, pb) = (b.ins().iadd(p[0], i), b.ins().iadd(p[1], i));
                let x = b.ins().load(types::I8, flags, pa, 8);
                let y = b.ins().load(types::I8, flags, pb, 8);
                let same = b.ins().icmp(IntCC::Equal, x, y);
                let next = b.ins().iadd_imm(i, 1);
                b.append_block_param(differ, types::I8);
                b.append_block_param(differ, types::I8);
                b.ins().brif(same, head, &[next.into()], differ, &[x.into(), y.into()]);
                b.switch_to_block(differ);
                let (x, y) = (b.block_params(differ)[0], b.block_params(differ)[1]);
                let less = b.ins().icmp(IntCC::UnsignedLessThan, x, y);
                let (m1, one) = (b.ins().iconst(types::I32, -1), b.ins().iconst(types::I32, 1));
                let r = b.ins().select(less, m1, one);
                b.ins().return_(&[r]);
                b.switch_to_block(done);
                let gt = b.ins().icmp(IntCC::UnsignedGreaterThan, la, lb);
                let lt = b.ins().icmp(IntCC::UnsignedLessThan, la, lb);
                let (gt, lt) = (b.ins().uextend(types::I32, gt), b.ins().uextend(types::I32, lt));
                let r = b.ins().isub(gt, lt);
                b.ins().return_(&[r]);
            }
            "tarn_rt_string_range" => {
                let (make, fault) = (f(b, "tarn_rt_string"), f(b, "tarn_rt_fault"));
                let len = b.ins().load(types::I64, flags, p[0], 0);
                let bad = b.ins().icmp(IntCC::UnsignedGreaterThan, p[1], p[2]);
                fault_if(b, bad, fault);
                let bad = b.ins().icmp(IntCC::UnsignedGreaterThan, p[2], len);
                fault_if(b, bad, fault);
                // Cuts must fall on scalar boundaries (not on a continuation byte).
                for at in [p[1], p[2]] {
                    let (check, next) = (b.create_block(), b.create_block());
                    let inside = b.ins().icmp(IntCC::UnsignedLessThan, at, len);
                    b.ins().brif(inside, check, &[], next, &[]);
                    b.switch_to_block(check);
                    let addr = b.ins().iadd(p[0], at);
                    let byte = b.ins().load(types::I8, flags, addr, 8);
                    let masked = b.ins().band_imm(byte, 0xc0);
                    let cont = b.ins().icmp_imm(IntCC::Equal, masked, 0x80);
                    fault_if(b, cont, fault);
                    b.ins().jump(next, &[]);
                    b.switch_to_block(next);
                }
                let start = b.ins().iadd(p[0], p[1]);
                let start = b.ins().iadd_imm(start, 8);
                let n = b.ins().isub(p[2], p[1]);
                let s = call(b, make, &[start, n])[0];
                b.ins().return_(&[s]);
            }
            "tarn_rt_string_utf8" => {
                let make = f(b, "tarn_rt_string");
                let (data, len) = (p[0], p[1]);
                let zero = b.ins().iconst(types::I64, 0);
                let (head, lead, scan, finish, bad, ok) = (b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block(), b.create_block());
                b.append_block_param(head, types::I64);
                // scan params: remaining continuation count, code point, next index, minimum
                for _ in 0..4 { b.append_block_param(scan, types::I64); }
                b.append_block_param(finish, types::I64); // code point
                b.append_block_param(finish, types::I64); // index
                b.append_block_param(finish, types::I64); // minimum
                b.append_block_param(lead, types::I64);
                b.ins().jump(head, &[zero.into()]);
                b.switch_to_block(head);
                let i = b.block_params(head)[0];
                let more = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
                b.ins().brif(more, lead, &[i.into()], ok, &[]);
                b.switch_to_block(lead);
                let i = b.block_params(lead)[0];
                let at = b.ins().iadd(data, i);
                let c8 = b.ins().load(types::I8, flags, at, 0);
                let c = b.ins().uextend(types::I64, c8);
                let i1 = b.ins().iadd_imm(i, 1);
                let (ascii, multi) = (b.create_block(), b.create_block());
                let is_ascii = b.ins().icmp_imm(IntCC::UnsignedLessThan, c, 0x80);
                b.ins().brif(is_ascii, ascii, &[], multi, &[]);
                b.switch_to_block(ascii);
                b.ins().jump(head, &[i1.into()]);
                b.switch_to_block(multi);
                let d2 = b.ins().iadd_imm(c, -0xc2);
                let l2 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, d2, 0xdf - 0xc2);
                let d3 = b.ins().iadd_imm(c, -0xe0);
                let l3 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, d3, 0x0f);
                let d4 = b.ins().iadd_imm(c, -0xf0);
                let l4 = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, d4, 4);
                let any = b.ins().bor(l2, l3);
                let any = b.ins().bor(any, l4);
                let (valid, _) = (b.create_block(), ());
                b.ins().brif(any, valid, &[], bad, &[]);
                b.switch_to_block(valid);
                let one = b.ins().iconst(types::I64, 1);
                let two = b.ins().iconst(types::I64, 2);
                let three = b.ins().iconst(types::I64, 3);
                let n3 = b.ins().select(l3, two, three);
                let n = b.ins().select(l2, one, n3);
                let (m2, m3, m4) = (b.ins().band_imm(c, 0x1f), b.ins().band_imm(c, 0x0f), b.ins().band_imm(c, 0x07));
                let cp3 = b.ins().select(l3, m3, m4);
                let cp = b.ins().select(l2, m2, cp3);
                let (k2, k3, k4) = (b.ins().iconst(types::I64, 0x80), b.ins().iconst(types::I64, 0x800), b.ins().iconst(types::I64, 0x10000));
                let min3 = b.ins().select(l3, k3, k4);
                let min = b.ins().select(l2, k2, min3);
                let room = b.ins().isub(len, i1);
                let short = b.ins().icmp(IntCC::UnsignedGreaterThan, n, room);
                let go = b.create_block();
                b.ins().brif(short, bad, &[], go, &[]);
                b.switch_to_block(go);
                // scan(remaining, cp, index, min)
                b.ins().jump(scan, &[n.into(), cp.into(), i1.into(), min.into()]);
                b.switch_to_block(scan);
                let sp = b.block_params(scan).to_vec();
                let (remaining, cp, index, min) = (sp[0], sp[1], sp[2], sp[3]);
                let body = b.create_block();
                let done = b.ins().icmp_imm(IntCC::Equal, remaining, 0);
                b.ins().brif(done, finish, &[cp.into(), index.into(), min.into()], body, &[]);
                b.switch_to_block(body);
                let at = b.ins().iadd(data, index);
                let next8 = b.ins().load(types::I8, flags, at, 0);
                let next = b.ins().uextend(types::I64, next8);
                let tag = b.ins().band_imm(next, 0xc0);
                let cont = b.ins().icmp_imm(IntCC::Equal, tag, 0x80);
                let step = b.create_block();
                b.ins().brif(cont, step, &[], bad, &[]);
                b.switch_to_block(step);
                let shifted = b.ins().ishl_imm(cp, 6);
                let low = b.ins().band_imm(next, 0x3f);
                let cp = b.ins().bor(shifted, low);
                let remaining = b.ins().iadd_imm(remaining, -1);
                let index = b.ins().iadd_imm(index, 1);
                b.ins().jump(scan, &[remaining.into(), cp.into(), index.into(), min.into()]);
                b.switch_to_block(finish);
                let fp = b.block_params(finish).to_vec();
                let (cp, index, min) = (fp[0], fp[1], fp[2]);
                let low = b.ins().icmp(IntCC::UnsignedLessThan, cp, min);
                let sur = b.ins().iadd_imm(cp, -0xd800);
                let sur = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, sur, 0x7ff);
                let high = b.ins().icmp_imm(IntCC::UnsignedGreaterThan, cp, 0x10ffff);
                let invalid = b.ins().bor(low, sur);
                let invalid = b.ins().bor(invalid, high);
                let next = b.create_block();
                b.ins().brif(invalid, bad, &[], next, &[]);
                b.switch_to_block(next);
                b.ins().jump(head, &[index.into()]);
                b.switch_to_block(bad);
                b.ins().return_(&[zero]);
                b.switch_to_block(ok);
                let s = call(b, make, &[data, len])[0];
                b.ins().return_(&[s]);
            }
            "tarn_rt_drop_string" => {
                let free = f(b, "tarn_free");
                let gv = module.declare_data_in_func(pool.0, b.func);
                let base = b.ins().global_value(types::I64, gv);
                let off = b.ins().isub(p[0], base);
                let (release, skip) = (b.create_block(), b.create_block());
                let inside = b.ins().icmp_imm(IntCC::UnsignedLessThan, off, pool.1 as i64);
                b.ins().brif(inside, skip, &[], release, &[]);
                b.switch_to_block(release);
                call(b, free, &[p[0]]);
                b.ins().return_(&[]);
                b.switch_to_block(skip);
                b.ins().return_(&[]);
            }
            "tarn_rt_env_alloc" => {
                let (a, fault) = (f(b, "tarn_alloc"), f(b, "tarn_rt_fault"));
                let s = alloc(b, a, fault, p[0]);
                b.ins().return_(&[s]);
            }
            "tarn_rt_env_free" => {
                let free = f(b, "tarn_free");
                let (release, skip) = (b.create_block(), b.create_block());
                b.ins().brif(p[0], release, &[], skip, &[]);
                b.switch_to_block(release);
                call(b, free, &[p[0]]);
                b.ins().return_(&[]);
                b.switch_to_block(skip);
                b.ins().return_(&[]);
            }
            "tarn_rt_env_drop" => {
                let (load, call_it, skip) = (b.create_block(), b.create_block(), b.create_block());
                b.ins().brif(p[0], load, &[], skip, &[]);
                b.switch_to_block(load);
                let thunk = b.ins().load(types::I64, flags, p[0], 0);
                b.ins().brif(thunk, call_it, &[], skip, &[]);
                b.switch_to_block(call_it);
                let mut sig = cl::Signature::new(module.isa().default_call_conv());
                sig.params.push(cl::AbiParam::new(types::I64));
                let sig = b.import_signature(sig);
                b.ins().call_indirect(sig, thunk, &[p[0]]);
                b.ins().return_(&[]);
                b.switch_to_block(skip);
                b.ins().return_(&[]);
            }
            "tarn_rt_vec_grow" => {
                let (realloc, fault) = (f(b, "tarn_realloc"), f(b, "tarn_rt_fault"));
                let high = b.ins().umulhi(p[1], p[2]);
                let overflow = b.ins().icmp_imm(IntCC::NotEqual, high, 0);
                fault_if(b, overflow, fault);
                let bytes = b.ins().imul(p[1], p[2]);
                let empty = b.ins().icmp_imm(IntCC::Equal, bytes, 0);
                let one = b.ins().iconst(types::I64, 1);
                let bytes = b.ins().select(empty, one, bytes);
                let r = call(b, realloc, &[p[0], bytes])[0];
                let null = b.ins().icmp_imm(IntCC::Equal, r, 0);
                fault_if(b, null, fault);
                b.ins().return_(&[r]);
            }
            "tarn_rt_vec_extend" => {
                let (grow, fault) = (f(b, "tarn_rt_vec_grow"), f(b, "tarn_rt_fault"));
                let (vec, src, count, size) = (p[0], p[1], p[2], p[3]);
                let len = b.ins().load(types::I64, flags, vec, 8);
                let cap = b.ins().load(types::I64, flags, vec, 16);
                let need = b.ins().iadd(len, count);
                let wrapped = b.ins().icmp(IntCC::UnsignedLessThan, need, len);
                fault_if(b, wrapped, fault);
                let (grow_block, copy_block) = (b.create_block(), b.create_block());
                let full = b.ins().icmp(IntCC::UnsignedGreaterThan, need, cap);
                b.ins().brif(full, grow_block, &[], copy_block, &[]);
                b.switch_to_block(grow_block);
                let four = b.ins().iconst(types::I64, 4);
                let doubled = b.ins().ishl_imm(cap, 1);
                let empty = b.ins().icmp_imm(IntCC::Equal, cap, 0);
                let next = b.ins().select(empty, four, doubled);
                let top = b.ins().ushr_imm(cap, 63);
                let top = b.ins().icmp_imm(IntCC::NotEqual, top, 0);
                let small = b.ins().icmp(IntCC::UnsignedLessThan, next, need);
                let use_need = b.ins().bor(top, small);
                let next = b.ins().select(use_need, need, next);
                let old = b.ins().load(types::I64, flags, vec, 0);
                let storage = call(b, grow, &[old, next, size])[0];
                b.ins().store(flags, storage, vec, 0);
                b.ins().store(flags, next, vec, 16);
                b.ins().jump(copy_block, &[]);
                b.switch_to_block(copy_block);
                let storage = b.ins().load(types::I64, flags, vec, 0);
                let at = b.ins().imul(len, size);
                let dst = b.ins().iadd(storage, at);
                let bytes = b.ins().imul(count, size);
                copy(b, dst, src, bytes);
                b.ins().store(flags, need, vec, 8);
                b.ins().return_(&[]);
            }
            _ => return Err(Error::bug(format!("no freestanding helper {name}"))),
        }
        builder.seal_all_blocks();
        builder.finalize();
    }
    cranelift_codegen::verify_function(&ctx.func, module.isa()).map_err(|e| Error::bug(format!("{name}: {e}")))?;
    module.define_function(id, &mut ctx).map_err(|e| Error::bug(format!("{name}: {e}")))?;
    Ok(())
}
