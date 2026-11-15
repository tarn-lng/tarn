//! Typed CPU intrinsics (ADRs 0101, 0104). Cranelift has no inline assembly.
//! Native CLIF handles int3; fixed-size post-codegen substitutions inline
//! other short sequences in ABI call slots. Longer sequences remain tiny
//! hand-assembled Local functions. Only templates a program uses are emitted;
//! even inlined templates must be defined because Cranelift requires it.
//! No template touches memory other than its operand,
//! the stack, or any register outside the caller-saved set of the C ABI
//! (x86_64: rax, rcx, rdx, rsi; aarch64: x0).
//!
//! Each entry lists the bytes and the assembly they encode; the
//! `cpu_stubs_disassemble_to_their_documented_assembly` integration test
//! disassembles every emitted stub with the host `objdump` and compares.
//!
//! x86_64 (System V: arguments rdi, rsi; result rax). Intel syntax:
//!
//! ```text
//! in8    (port u16) u8     89 fa ec c3            mov edx, edi ; in al, dx ; ret
//! in16   (port u16) u16    89 fa 66 ed c3         mov edx, edi ; in ax, dx ; ret
//! in32   (port u16) u32    89 fa ed c3            mov edx, edi ; in eax, dx ; ret
//! out8   (port u16, v u8)  89 fa 89 f0 ee c3      mov edx, edi ; mov eax, esi ; out dx, al ; ret
//! out16  (port u16, v u16) 89 fa 96 66 ef c3      mov edx, edi ; xchg eax, esi ; out dx, ax ; ret
//! out32  (port u16, v u32) 89 fa 89 f0 ef c3      mov edx, edi ; mov eax, esi ; out dx, eax ; ret
//! ```
//!
//! The remaining encodings are in the table in `x86_64`. Sequences longer than
//! the call slot (`cpuid`, `call_on_stack`, ...) are never inlined, so they may
//! use the stack and any caller-saved register (ADR 0107).

use crate::{Arch, Error, Result, Target};
use cranelift_codegen::ir::{types, Type};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::ObjectModule;
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Reserved backend source-location bit; no source positions are currently
/// attached to CLIF instructions. Lower bits identify each volatile store.
pub(crate) const VOLATILE_STORE_LOC: u32 = 0x8000_0000;

/// CLIF 0.125.3 has only seq-cst atomic stores and no volatile flag. Keep
/// their compiler effects, but replace the tagged x86 store's trailing
/// mfence by an equal-sized NOP. Ordinary atomics and ffi.fence are untouched.
pub(crate) fn relax_volatile_stores(code: &mut [u8], locations: &[cranelift_codegen::MachSrcLoc<cranelift_codegen::Final>]) -> Result<()> {
    for loc in locations {
        if loc.loc.is_default() || loc.loc.bits() & VOLATILE_STORE_LOC == 0 { continue; }
        let end = loc.end as usize;
        let fence = code.get_mut(end.saturating_sub(3)..end).ok_or_else(|| Error::bug("volatile store outside function"))?;
        if loc.end - loc.start < 5 || fence != [0x0f, 0xae, 0xf0] {
            return Err(Error::bug("unexpected x86 volatile store lowering; expected trailing mfence"));
        }
        fence.copy_from_slice(&[0x0f, 0x1f, 0x00]); // nop dword ptr [rax], no memory access
    }
    Ok(())
}

/// One hand-assembled function.
pub(crate) struct Stub {
    pub params: Vec<Type>,
    pub ret: Option<Type>,
    pub code: Vec<u8>,
    /// The instructions `code` encodes, Intel syntax (x86_64) or ARM syntax
    /// (aarch64), lower case.
    pub asm: Vec<String>,
}

/// Stdlib modules that hold the intrinsics of one architecture.
pub(crate) fn module_arch(module: &str) -> Option<Arch> {
    match module {
        "arch_x86_64" => Some(Arch::X86_64),
        "arch_aarch64" => Some(Arch::Aarch64),
        _ => None,
    }
}

pub(crate) fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

fn stub(params: &[Type], ret: Option<Type>, code: &[u8], asm: &[&str]) -> Stub {
    Stub { params: params.to_vec(), ret, code: code.to_vec(), asm: asm.iter().map(|s| s.to_string()).collect() }
}

/// The stub for `op` (the Tarn name without arch module and leading `_`).
pub(crate) fn lookup(arch: Arch, op: &str) -> Option<Stub> {
    match arch {
        Arch::X86_64 => x86_64(op),
        Arch::Aarch64 => aarch64(op),
    }
}

/// Every operation of an architecture, for documentation and tests.
pub fn operations(arch: Arch) -> Vec<String> {
    let names: Vec<String> = match arch {
        Arch::X86_64 => ["in8", "in16", "in32", "out8", "out16", "out32", "cli", "sti", "hlt", "pause", "int3", "rdtsc",
            "read_cr0", "read_cr2", "read_cr3", "read_cr4", "write_cr0", "write_cr3", "write_cr4", "invlpg", "rdmsr", "wrmsr", "lgdt", "lidt",
            "ltr", "str", "cpuid", "ud2", "xgetbv", "xsetbv", "wbinvd", "clflush", "invpcid", "swapgs",
            "rdfsbase", "wrfsbase", "rdgsbase", "wrgsbase", "read_rflags", "call_on_stack"]
            .iter().map(|s| s.to_string()).collect(),
        Arch::Aarch64 => {
            let mut v: Vec<String> = ["wfi", "wfe", "isb", "dsb_sy", "dmb_sy", "irq_enable", "irq_disable", "svc", "brk", "semihost_exit"].iter().map(|s| s.to_string()).collect();
            for (name, ..) in A64_SYS { v.push(name.to_string()); }
            v.push("call_on_stack".into());
            for (name, ..) in SYSREGS { v.push(format!("read_{name}")); }
            for (name, _, writable) in SYSREGS.iter().map(|(n, e, w)| (n, e, w)) { if *writable { v.push(format!("write_{name}")); } }
            v
        }
    };
    names
}

/// Symbol of a stub inside the object.
pub(crate) fn symbol(arch: Arch, op: &str) -> String {
    format!("tarn_arch_{}_{op}", arch_name(arch))
}

/// `(symbol, asm)` of every stub of `arch`, for the disassembly test.
#[doc(hidden)]
pub fn listing(arch: Arch) -> Vec<(String, Vec<String>)> {
    operations(arch).into_iter().map(|op| (symbol(arch, &op), lookup(arch, &op).expect("listed operation").asm)).collect()
}

fn x86_64(op: &str) -> Option<Stub> {
    use types::{I16, I32, I64, I8};
    // Bytes: `mov edx, edi` = 89 fa; `mov eax, esi` = 89 f0; `mov ecx, edi` = 89 f9;
    // `mov rax, rsi` = 48 89 f0; `mov rdx, rsi` = 48 89 f2;
    // `shl rdx, 32` = 48 c1 e2 20; `shr rdx, 32` = 48 c1 ea 20; `or rax, rdx` = 48 09 d0.
    Some(match op {
        "in8" => stub(&[I16], Some(I8), &[0x89, 0xfa, 0xec, 0xc3], &["mov edx, edi", "in al, dx", "ret"]),
        "in16" => stub(&[I16], Some(I16), &[0x89, 0xfa, 0x66, 0xed, 0xc3], &["mov edx, edi", "in ax, dx", "ret"]),
        "in32" => stub(&[I16], Some(I32), &[0x89, 0xfa, 0xed, 0xc3], &["mov edx, edi", "in eax, dx", "ret"]),
        "out8" => stub(&[I16, I8], None, &[0x89, 0xfa, 0x89, 0xf0, 0xee, 0xc3], &["mov edx, edi", "mov eax, esi", "out dx, al", "ret"]),
        // xchg uses the one-byte accumulator encoding. ESI is caller-saved;
        // unlike mov this also clobbers it, but fits the five-byte call slot.
        "out16" => stub(&[I16, I16], None, &[0x89, 0xfa, 0x96, 0x66, 0xef, 0xc3], &["mov edx, edi", "xchg eax, esi", "out dx, ax", "ret"]),
        "out32" => stub(&[I16, I32], None, &[0x89, 0xfa, 0x89, 0xf0, 0xef, 0xc3], &["mov edx, edi", "mov eax, esi", "out dx, eax", "ret"]),
        "cli" => stub(&[], None, &[0xfa, 0xc3], &["cli", "ret"]),
        "sti" => stub(&[], None, &[0xfb, 0xc3], &["sti", "ret"]),
        "hlt" => stub(&[], None, &[0xf4, 0xc3], &["hlt", "ret"]),
        "int3" => stub(&[], None, &[0xcc, 0xc3], &["int3", "ret"]),
        "pause" => stub(&[], None, &[0xf3, 0x90, 0xc3], &["pause", "ret"]),
        // rdtsc leaves edx:eax; combine into rax.
        "rdtsc" => stub(&[], Some(I64), &[0x0f, 0x31, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09, 0xd0, 0xc3], &["rdtsc", "shl rdx, 0x20", "or rax, rdx", "ret"]),
        "read_cr0" => stub(&[], Some(I64), &[0x0f, 0x20, 0xc0, 0xc3], &["mov rax, cr0", "ret"]),
        "read_cr2" => stub(&[], Some(I64), &[0x0f, 0x20, 0xd0, 0xc3], &["mov rax, cr2", "ret"]),
        "read_cr3" => stub(&[], Some(I64), &[0x0f, 0x20, 0xd8, 0xc3], &["mov rax, cr3", "ret"]),
        "read_cr4" => stub(&[], Some(I64), &[0x0f, 0x20, 0xe0, 0xc3], &["mov rax, cr4", "ret"]),
        "write_cr0" => stub(&[I64], None, &[0x0f, 0x22, 0xc7, 0xc3], &["mov cr0, rdi", "ret"]),
        "write_cr3" => stub(&[I64], None, &[0x0f, 0x22, 0xdf, 0xc3], &["mov cr3, rdi", "ret"]),
        "write_cr4" => stub(&[I64], None, &[0x0f, 0x22, 0xe7, 0xc3], &["mov cr4, rdi", "ret"]),
        "invlpg" => stub(&[I64], None, &[0x0f, 0x01, 0x3f, 0xc3], &["invlpg byte ptr [rdi]", "ret"]),
        // rdmsr: ecx = msr -> edx:eax; wrmsr: ecx = msr, edx:eax = value.
        "rdmsr" => stub(&[I32], Some(I64), &[0x89, 0xf9, 0x0f, 0x32, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09, 0xd0, 0xc3],
            &["mov ecx, edi", "rdmsr", "shl rdx, 0x20", "or rax, rdx", "ret"]),
        "wrmsr" => stub(&[I32, I64], None, &[0x89, 0xf9, 0x48, 0x89, 0xf0, 0x48, 0x89, 0xf2, 0x48, 0xc1, 0xea, 0x20, 0x0f, 0x30, 0xc3],
            &["mov ecx, edi", "mov rax, rsi", "mov rdx, rsi", "shr rdx, 0x20", "wrmsr", "ret"]),
        "lgdt" => stub(&[I64], None, &[0x0f, 0x01, 0x17, 0xc3], &["lgdt [rdi]", "ret"]),
        "lidt" => stub(&[I64], None, &[0x0f, 0x01, 0x1f, 0xc3], &["lidt [rdi]", "ret"]),
        // ADR 0107 (T6). Inline-sized (<= 5 bytes before `ret`) unless noted.
        "ltr" => stub(&[I16], None, &[0x0f, 0x00, 0xdf, 0xc3], &["ltr di", "ret"]),
        "str" => stub(&[], Some(I16), &[0x0f, 0x00, 0xc8, 0xc3], &["str eax", "ret"]),
        "ud2" => stub(&[], None, &[0x0f, 0x0b, 0xc3], &["ud2", "ret"]),
        "wbinvd" => stub(&[], None, &[0x0f, 0x09, 0xc3], &["wbinvd", "ret"]),
        "clflush" => stub(&[I64], None, &[0x0f, 0xae, 0x3f, 0xc3], &["clflush byte ptr [rdi]", "ret"]),
        "swapgs" => stub(&[], None, &[0x0f, 0x01, 0xf8, 0xc3], &["swapgs", "ret"]),
        "rdfsbase" => stub(&[], Some(I64), &[0xf3, 0x48, 0x0f, 0xae, 0xc0, 0xc3], &["rdfsbase rax", "ret"]),
        "rdgsbase" => stub(&[], Some(I64), &[0xf3, 0x48, 0x0f, 0xae, 0xc8, 0xc3], &["rdgsbase rax", "ret"]),
        "wrfsbase" => stub(&[I64], None, &[0xf3, 0x48, 0x0f, 0xae, 0xd7, 0xc3], &["wrfsbase rdi", "ret"]),
        "wrgsbase" => stub(&[I64], None, &[0xf3, 0x48, 0x0f, 0xae, 0xdf, 0xc3], &["wrgsbase rdi", "ret"]),
        // invpcid type (rdi), descriptor (rsi): `invpcid rdi, [rsi]`.
        "invpcid" => stub(&[I64, I64], None, &[0x66, 0x0f, 0x38, 0x82, 0x3e, 0xc3], &["invpcid rdi, xmmword ptr [rsi]", "ret"]),
        // pushfq / pop rax uses only the stack slot below rsp.
        "read_rflags" => stub(&[], Some(I64), &[0x9c, 0x58, 0xc3], &["pushfq", "pop rax", "ret"]),
        "xgetbv" => stub(&[I32], Some(I64), &[0x89, 0xf9, 0x0f, 0x01, 0xd0, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09, 0xd0, 0xc3],
            &["mov ecx, edi", "xgetbv", "shl rdx, 0x20", "or rax, rdx", "ret"]),
        "xsetbv" => stub(&[I32, I64], None, &[0x89, 0xf9, 0x48, 0x89, 0xf0, 0x48, 0x89, 0xf2, 0x48, 0xc1, 0xea, 0x20, 0x0f, 0x01, 0xd1, 0xc3],
            &["mov ecx, edi", "mov rax, rsi", "mov rdx, rsi", "shr rdx, 0x20", "xsetbv", "ret"]),
        // cpuid(leaf edi, subleaf esi, out rdx): writes eax, ebx, ecx, edx to out[0..4].
        // rbx is callee-saved, so it is pushed. Not inline-sized: always a call.
        "cpuid" => stub(&[I32, I32, I64], None,
            &[0x53, 0x89, 0xf8, 0x89, 0xf1, 0x48, 0x89, 0xd6, 0x0f, 0xa2, 0x89, 0x06, 0x89, 0x5e, 0x04, 0x89, 0x4e, 0x08, 0x89, 0x56, 0x0c, 0x5b, 0xc3],
            &["push rbx", "mov eax, edi", "mov ecx, esi", "mov rsi, rdx", "cpuid", "mov dword ptr [rsi], eax", "mov dword ptr [rsi + 0x4], ebx",
              "mov dword ptr [rsi + 0x8], ecx", "mov dword ptr [rsi + 0xc], edx", "pop rbx", "ret"]),
        // call_on_stack(stack_top rdi, entry rsi, arg rdx): see ADR 0107. rbp is callee-saved
        // and survives the call, so it carries the old rsp.
        "call_on_stack" => stub(&[I64, I64, I64], None,
            &[0x55, 0x48, 0x89, 0xe5, 0x48, 0x89, 0xfc, 0x48, 0x83, 0xe4, 0xf0, 0x48, 0x89, 0xd7, 0xff, 0xd6, 0x48, 0x89, 0xec, 0x5d, 0xc3],
            &["push rbp", "mov rbp, rsp", "mov rsp, rdi", "and rsp, -0x10", "mov rdi, rdx", "call rsi", "mov rsp, rbp", "pop rbp", "ret"]),
        _ => return None,
    })
}

/// aarch64 system registers reachable from Tarn: `(name, (op0, op1, CRn, CRm,
/// op2), writable)`. `mrs x0, reg` is `0xd5300000 | enc`, `msr reg, x0` is
/// `0xd5100000 | enc`, with `enc = (op0 - 2) << 19 | op1 << 16 | CRn << 12 |
/// CRm << 8 | op2 << 5` (x0 is both the argument and the result register).
pub(crate) const SYSREGS: &[(&str, (u32, u32, u32, u32, u32), bool)] = &[
    ("daif", (3, 3, 4, 2, 1), true),
    ("vbar_el1", (3, 0, 12, 0, 0), true),
    ("ttbr0_el1", (3, 0, 2, 0, 0), true),
    ("ttbr1_el1", (3, 0, 2, 0, 1), true),
    ("tcr_el1", (3, 0, 2, 0, 2), true),
    ("mair_el1", (3, 0, 10, 2, 0), true),
    ("sctlr_el1", (3, 0, 1, 0, 0), true),
    ("cpacr_el1", (3, 0, 1, 0, 2), true),
    ("currentel", (3, 0, 4, 2, 2), false),
    ("esr_el1", (3, 0, 5, 2, 0), true),
    ("far_el1", (3, 0, 6, 0, 0), true),
    ("elr_el1", (3, 0, 4, 0, 1), true),
    ("spsr_el1", (3, 0, 4, 0, 0), true),
    ("cntv_ctl_el0", (3, 3, 14, 3, 1), true),
    ("cntv_tval_el0", (3, 3, 14, 3, 0), true),
    ("cntfrq_el0", (3, 3, 14, 0, 0), false),
    ("icc_sre_el1", (3, 0, 12, 12, 5), true),
    ("icc_pmr_el1", (3, 0, 4, 6, 0), true),
    ("icc_igrpen1_el1", (3, 0, 12, 12, 7), true),
    ("icc_iar1_el1", (3, 0, 12, 12, 0), false),
    ("icc_eoir1_el1", (3, 0, 12, 12, 1), true),
    ("cntvct_el0", (3, 3, 14, 0, 2), false),
    // ADR 0107 (T6): address translation result, stack pointer of EL0, ID registers.
    ("par_el1", (3, 0, 7, 4, 0), false),
    ("sp_el0", (3, 0, 4, 1, 0), true),
    ("id_aa64mmfr0_el1", (3, 0, 0, 7, 0), false),
    ("id_aa64mmfr1_el1", (3, 0, 0, 7, 1), false),
    ("id_aa64pfr0_el1", (3, 0, 0, 4, 0), false),
    ("ctr_el0", (3, 3, 0, 0, 1), false),
    ("dczid_el0", (3, 3, 0, 0, 7), false),
    ("midr_el1", (3, 0, 0, 0, 0), false),
    ("mpidr_el1", (3, 0, 0, 0, 5), false),
];

/// aarch64 barriers and SYS-class maintenance instructions: `(name, word,
/// assembly, takes x0)`. `sys #op1, Cn, Cm, #op2, Xt` is
/// `0xd5080000 | op1 << 16 | CRn << 12 | CRm << 8 | op2 << 5 | Xt`; the
/// operand operations use x0. `dsb` is `0xd503309f | CRm << 8`, `dmb`
/// `0xd50330bf | CRm << 8` (CRm: ish 0b1011, ishst 0b1010, ishld 0b1001,
/// nsh 0b0111, nshst 0b0110).
pub(crate) const A64_SYS: &[(&str, u32, &str, bool)] = &[
    ("tlbi_vmalle1", 0xd508871f, "tlbi vmalle1", false),
    ("tlbi_vmalle1is", 0xd508831f, "tlbi vmalle1is", false),
    ("tlbi_vae1", 0xd5088720, "tlbi vae1, x0", true),
    ("tlbi_vae1is", 0xd5088320, "tlbi vae1is, x0", true),
    ("tlbi_vale1", 0xd50887a0, "tlbi vale1, x0", true),
    ("tlbi_aside1", 0xd5088740, "tlbi aside1, x0", true),
    ("dsb_ish", 0xd5033b9f, "dsb ish", false),
    ("dsb_ishst", 0xd5033a9f, "dsb ishst", false),
    ("dsb_nsh", 0xd503379f, "dsb nsh", false),
    ("dsb_nshst", 0xd503369f, "dsb nshst", false),
    ("dmb_ish", 0xd5033bbf, "dmb ish", false),
    ("dmb_ishld", 0xd50339bf, "dmb ishld", false),
    ("dmb_ishst", 0xd5033abf, "dmb ishst", false),
    ("ic_iallu", 0xd508751f, "ic iallu", false),
    ("ic_ialluis", 0xd508711f, "ic ialluis", false),
    ("ic_ivau", 0xd50b7520, "ic ivau, x0", true),
    ("dc_civac", 0xd50b7e20, "dc civac, x0", true),
    ("dc_cvac", 0xd50b7a20, "dc cvac, x0", true),
    ("dc_ivac", 0xd5087620, "dc ivac, x0", true),
    ("dc_zva", 0xd50b7420, "dc zva, x0", true),
    ("dc_cvau", 0xd50b7b20, "dc cvau, x0", true),
    ("at_s1e1r", 0xd5087800, "at s1e1r, x0", true),
    ("at_s1e1w", 0xd5087820, "at s1e1w, x0", true),
];

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn aarch64(op: &str) -> Option<Stub> {
    use types::I64;
    const RET: u32 = 0xd65f03c0;
    let fixed = |word: u32, asm: &str| Some(Stub { params: vec![], ret: None, code: words(&[word, RET]), asm: vec![asm.into(), "ret".into()] });
    if let Some((_, word, asm, operand)) = A64_SYS.iter().find(|(n, ..)| *n == op) {
        return Some(Stub { params: if *operand { vec![I64] } else { vec![] }, ret: None, code: words(&[*word, RET]), asm: vec![asm.to_string(), "ret".into()] });
    }
    match op {
        // call_on_stack(stack_top x0, entry x1, arg x2): see ADR 0107. x29 is callee-saved
        // and carries the old sp; x30 is saved on the old stack.
        "call_on_stack" => return Some(Stub { params: vec![I64, I64, I64], ret: None,
            code: words(&[0xa9bf7bfd, 0x910003fd, 0x927cec03, 0x9100007f, 0xaa0203e0, 0xd63f0020, 0x910003bf, 0xa8c17bfd, RET]),
            asm: vec!["stp x29, x30, [sp, #-0x10]!", "mov x29, sp", "and x3, x0, #0xfffffffffffffff0", "mov sp, x3", "mov x0, x2", "blr x1", "mov sp, x29", "ldp x29, x30, [sp], #0x10", "ret"].into_iter().map(str::to_string).collect() }),
        "svc" => return fixed(0xd4000001, "svc #0"),
        "brk" => return fixed(0xd4200000, "brk #0"),
        "semihost_exit" => return Some(Stub { params: vec![I64], ret: None,
            code: words(&[0xd10043ff, 0xf90007e0, 0xd28004c0, 0xf2a00040, 0xf90003e0, 0x910003e1, 0xd2800300, 0xd45e0000, 0x910043ff, RET]),
            asm: vec!["sub sp, sp, #0x10", "str x0, [sp, #0x8]", "mov x0, #0x26", "movk x0, #0x2, lsl #16", "str x0, [sp]", "mov x1, sp", "mov x0, #0x18", "hlt #0xf000", "add sp, sp, #0x10", "ret"].into_iter().map(str::to_string).collect() }),
        "wfi" => return fixed(0xd503207f, "wfi"),
        "wfe" => return fixed(0xd503205f, "wfe"),
        "isb" => return fixed(0xd5033fdf, "isb"),
        "dsb_sy" => return fixed(0xd5033f9f, "dsb sy"),
        "dmb_sy" => return fixed(0xd5033fbf, "dmb sy"),
        // msr daifclr, #2 / msr daifset, #2: unmask / mask IRQ.
        "irq_enable" => return fixed(0xd50342ff, "msr daifclr, #0x2"),
        "irq_disable" => return fixed(0xd50342df, "msr daifset, #0x2"),
        _ => {}
    }
    let (write, name) = match (op.strip_prefix("read_"), op.strip_prefix("write_")) {
        (Some(n), _) => (false, n),
        (_, Some(n)) => (true, n),
        _ => return None,
    };
    let (_, (op0, op1, crn, crm, op2), writable) = SYSREGS.iter().find(|(n, ..)| *n == name)?;
    if write && !writable { return None; }
    let enc = (op0 - 2) << 19 | op1 << 16 | crn << 12 | crm << 8 | op2 << 5;
    Some(if write {
        Stub { params: vec![I64], ret: None, code: words(&[0xd5100000 | enc, RET]), asm: vec![format!("msr {name}, x0"), "ret".into()] }
    } else {
        Stub { params: vec![], ret: Some(I64), code: words(&[0xd5300000 | enc, RET]), asm: vec![format!("mrs x0, {name}"), "ret".into()] }
    })
}

/// Stubs a program calls, declared on first use and defined once at the end.
#[derive(Default)]
pub(crate) struct Stubs {
    declared: RefCell<BTreeMap<String, (FuncId, String)>>,
}

impl Stubs {
    /// Replace only relocation-identified direct calls, without changing any
    /// offset. The allocator has already honored the stub's ABI clobbers.
    /// This is deliberately not a general machine-code inliner (ADR 0104).
    pub(crate) fn inline_calls(&self, arch: Arch, code: &mut [u8], relocs: &mut Vec<cranelift_module::ModuleReloc>) -> Result<()> {
        use cranelift_codegen::binemit::Reloc;
        let declared = self.declared.borrow();
        let mut keep = Vec::new();
        for reloc in relocs.drain(..) {
            let Some((_, op)) = declared.values().find(|(id, _)| matches!(reloc.name, cranelift_module::ModuleRelocTarget::User { namespace: 0, index } if index == id.as_u32())) else {
                keep.push(reloc);
                continue;
            };
            let stub = lookup(arch, op).ok_or_else(|| Error::bug("CPU stub vanished"))?;
            let (ret_size, slot_size) = match arch { Arch::X86_64 => (1, 5), Arch::Aarch64 => (4, 4) };
            let body = &stub.code[..stub.code.len() - ret_size];
            let offset = reloc.offset as usize;
            let slot = match (arch, reloc.kind, reloc.addend) {
                (Arch::X86_64, Reloc::X86CallPCRel4, -4) if offset >= 1 => {
                    let slot = code.get_mut(offset - 1..offset + 4).ok_or_else(|| Error::bug("CPU call outside function"))?;
                    if slot[0] != 0xe8 { return Err(Error::bug("unexpected x86 CPU call encoding")); }
                    Some(slot)
                }
                (Arch::Aarch64, Reloc::Arm64Call, 0) => {
                    let slot = code.get_mut(offset..offset + 4).ok_or_else(|| Error::bug("CPU call outside function"))?;
                    if u32::from_le_bytes(slot.try_into().unwrap()) != 0x94000000 { return Err(Error::bug("unexpected ARM CPU call encoding")); }
                    Some(slot)
                }
                _ => None,
            };
            if let Some(slot) = slot.filter(|_| body.len() <= slot_size) {
                if arch == Arch::X86_64 { slot.fill(0x90); }
                slot[..body.len()].copy_from_slice(body);
            } else {
                keep.push(reloc);
            }
        }
        *relocs = keep;
        Ok(())
    }

    /// Function id of the stub for `module.op`, after checking it exists for
    /// `target`'s architecture.
    pub(crate) fn id(&self, module: &mut ObjectModule, target: Target, intrinsic: &str) -> Result<(FuncId, Stub)> {
        let (module_name, operation) = intrinsic.split_once('.').ok_or_else(|| Error::bug("arch intrinsic name"))?;
        let arch = module_arch(module_name).ok_or_else(|| Error::bug("arch intrinsic module"))?;
        let operation = operation.strip_prefix('_').unwrap_or(operation);
        if arch != target.arch {
            return Err(Error {
                message: format!(
                    "`{module_name}.{operation}` is an {} CPU intrinsic but the target is {target} ({}); import `arch_{}` for this target instead",
                    arch_name(arch), arch_name(target.arch), arch_name(target.arch)
                ),
            });
        }
        let stub = lookup(arch, operation).ok_or_else(|| Error::bug(format!("unknown CPU intrinsic {intrinsic}")))?;
        let name = symbol(arch, operation);
        if let Some((id, _)) = self.declared.borrow().get(&name) {
            return Ok((*id, stub));
        }
        let mut sig = module.make_signature();
        sig.params = stub.params.iter().map(|t| cranelift_codegen::ir::AbiParam::new(*t)).collect();
        sig.returns = stub.ret.iter().map(|t| cranelift_codegen::ir::AbiParam::new(*t)).collect();
        let id = module.declare_function(&name, Linkage::Local, &sig).map_err(|e| Error::bug(e.to_string()))?;
        self.declared.borrow_mut().insert(name, (id, operation.to_owned()));
        Ok((id, stub))
    }

    /// Define the bytes of every declared stub.
    pub(crate) fn define(&self, module: &mut ObjectModule, arch: Arch) -> Result<()> {
        for (id, operation) in self.declared.borrow().values() {
            let stub = lookup(arch, operation).ok_or_else(|| Error::bug("CPU stub vanished"))?;
            let alignment = if arch == Arch::Aarch64 { 4 } else { 16 };
            module.define_function_bytes(*id, alignment, &stub.code, &[]).map_err(|e| Error::bug(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::{Final, MachSrcLoc, binemit::Reloc, ir::SourceLoc};
    use cranelift_module::{ModuleReloc, ModuleRelocTarget};

    #[test]
    fn call_substitution_preserves_offsets_and_unrelated_relocations() {
        let stubs = Stubs::default();
        stubs.declared.borrow_mut().insert("cpu".into(), (FuncId::from_u32(7), "invlpg".into()));
        let mut bytes = vec![0xe8, 0, 0, 0, 0, 0xe8, 0, 0, 0, 0, 0xc3];
        let original_len = bytes.len();
        let mut relocs = vec![
            ModuleReloc { offset: 1, kind: Reloc::X86CallPCRel4, name: ModuleRelocTarget::user(0, 7), addend: -4 },
            ModuleReloc { offset: 6, kind: Reloc::X86CallPCRel4, name: ModuleRelocTarget::user(0, 8), addend: -4 },
        ];
        stubs.inline_calls(Arch::X86_64, &mut bytes, &mut relocs).unwrap();
        assert_eq!(bytes.len(), original_len);
        assert_eq!(&bytes[..5], &[0x0f, 0x01, 0x3f, 0x90, 0x90]);
        assert_eq!(&bytes[5..], &[0xe8, 0, 0, 0, 0, 0xc3]);
        assert_eq!(relocs.len(), 1);
        assert_eq!(relocs[0].offset, 6);
    }

    #[test]
    fn changed_call_encoding_is_rejected_and_unrecognized_forms_keep_the_call() {
        let stubs = Stubs::default();
        stubs.declared.borrow_mut().insert("cpu".into(), (FuncId::from_u32(7), "pause".into()));
        let reloc = |kind| ModuleReloc { offset: 1, kind, name: ModuleRelocTarget::user(0, 7), addend: -4 };
        let mut bytes = [0x90; 9];
        assert!(stubs.inline_calls(Arch::X86_64, &mut bytes, &mut vec![reloc(Reloc::X86CallPCRel4)]).is_err());
        let mut relocs = vec![reloc(Reloc::Abs8)];
        stubs.inline_calls(Arch::X86_64, &mut bytes, &mut relocs).unwrap();
        assert_eq!(relocs.len(), 1);
        assert_eq!(bytes, [0x90; 9]);
    }

    #[test]
    fn volatile_relaxation_requires_the_tag_and_exact_trailing_fence() {
        let mut bytes = [0x89, 0x07, 0x0f, 0xae, 0xf0, 0x0f, 0xae, 0xf0];
        let locations: [MachSrcLoc<Final>; 2] = [
            MachSrcLoc { start: 0, end: 5, loc: SourceLoc::new(VOLATILE_STORE_LOC | 1) },
            MachSrcLoc { start: 5, end: 8, loc: SourceLoc::default() },
        ];
        relax_volatile_stores(&mut bytes, &locations).unwrap();
        assert_eq!(bytes, [0x89, 0x07, 0x0f, 0x1f, 0, 0x0f, 0xae, 0xf0]);
        // Never silently accept a dependency change or an incorrectly tagged
        // range (including trying to patch the already-relaxed instruction).
        assert!(relax_volatile_stores(&mut bytes, &locations).is_err());
    }
}
