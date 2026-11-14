//! Interrupt trampolines (ADR 0101, P7). Cranelift cannot express "save every
//! register, return with `iretq`/`eret`", so each `#[interrupt]` function gets
//! a hand-assembled entry sequence, emitted with `define_function_bytes` and a
//! relocation to a C-ABI adapter of the ordinary Tarn body. Nothing here reads
//! or writes memory other than the stack. The sequences are generated, not
//! checked-in blobs, and `trampolines_disassemble_to_the_documented_sequence`
//! disassembles them with the host `objdump` and compares.
//!
//! x86_64, no error code (entry `rsp` = 8 mod 16, the CPU aligns the stack to
//! 16 bytes before pushing the 40-byte frame; the CPU also leaves DF alone):
//!
//! ```text
//! push rax, rcx, rdx, rsi, rdi, r8, r9, r10, r11    9 pushes, rsp = 0 mod 16
//! sub rsp, 256
//! movups [rsp + 16*i], xmm<i>                       i = 0..15
//! cld
//! lea rdi, [rsp + 328]                              &InterruptFrame (rip cs rflags rsp ss)
//! call <C adapter of the handler>
//! movups xmm<i>, [rsp + 16*i]
//! add rsp, 256
//! pop r11, r10, r9, r8, rdi, rsi, rdx, rcx, rax
//! iretq
//! ```
//!
//! With an error code (entry `rsp` = 0 mod 16, 48-byte frame) the sequence is
//! the same except: `sub rsp, 264` (8 bytes of padding keep the call
//! aligned), `mov rsi, [rsp + 336]` loads the code, `lea rdi, [rsp + 344]`
//! skips it, `add rsp, 264` and `add rsp, 8` (drop the code) before `iretq`.
//!
//! aarch64: the handler gets a pointer to a 272-byte frame
//! `{ x: [31]u64, elr u64, spsr u64, sp u64 }`; the 32 SIMD registers follow
//! it (512 bytes). The frame is restored on return, so a handler can edit
//! `x`, `elr` and `spsr` (`sp` is informational).

use crate::Arch;

pub(crate) struct Trampoline {
    pub code: Vec<u8>,
    /// Instructions as the host `objdump` prints them (lower case, Intel
    /// syntax on x86_64). The call/branch to the adapter is just `call`/`bl`.
    pub asm: Vec<String>,
    /// Offset of the call instruction's field a relocation fills.
    pub call_at: usize,
}

pub(crate) fn trampoline(arch: Arch, error_code: bool) -> Trampoline {
    match arch {
        Arch::X86_64 => x86_64(error_code),
        Arch::Aarch64 => aarch64(),
    }
}

/// Assembly of the trampoline for the disassembly test.
#[doc(hidden)]
pub fn listing(arch: Arch, error_code: bool) -> Vec<String> {
    trampoline(arch, error_code).asm
}

const GPRS: [(&str, &[u8], &[u8]); 9] = [
    ("rax", &[0x50], &[0x58]),
    ("rcx", &[0x51], &[0x59]),
    ("rdx", &[0x52], &[0x5a]),
    ("rsi", &[0x56], &[0x5e]),
    ("rdi", &[0x57], &[0x5f]),
    ("r8", &[0x41, 0x50], &[0x41, 0x58]),
    ("r9", &[0x41, 0x51], &[0x41, 0x59]),
    ("r10", &[0x41, 0x52], &[0x41, 0x5a]),
    ("r11", &[0x41, 0x53], &[0x41, 0x5b]),
];

/// `movups [rsp + disp], xmmN` (store) or `movups xmmN, [rsp + disp]` (load).
fn movups(out: &mut Vec<u8>, asm: &mut Vec<String>, n: u8, disp: u32, store: bool) {
    if n >= 8 {
        out.push(0x44);
    }
    out.extend([0x0f, if store { 0x11 } else { 0x10 }]);
    let reg = (n & 7) << 3;
    match disp {
        0 => out.extend([reg | 0x04, 0x24]),
        1..=127 => out.extend([0x40 | reg | 0x04, 0x24, disp as u8]),
        _ => {
            out.extend([0x80 | reg | 0x04, 0x24]);
            out.extend(disp.to_le_bytes());
        }
    }
    let memory = if disp == 0 { "xmmword ptr [rsp]".to_string() } else { format!("xmmword ptr [rsp + {disp:#x}]") };
    asm.push(if store { format!("movups {memory}, xmm{n}") } else { format!("movups xmm{n}, {memory}") });
}

fn x86_64(error_code: bool) -> Trampoline {
    let mut code = Vec::new();
    let mut asm = Vec::new();
    for (name, push, _) in GPRS {
        code.extend(push);
        asm.push(format!("push {name}"));
    }
    let area: u32 = if error_code { 264 } else { 256 };
    code.extend([0x48, 0x81, 0xec]);
    code.extend(area.to_le_bytes());
    asm.push(format!("sub rsp, {area:#x}"));
    for n in 0..16u8 {
        movups(&mut code, &mut asm, n, u32::from(n) * 16, true);
    }
    code.push(0xfc);
    asm.push("cld".into());
    let saved = 72 + area;
    if error_code {
        code.extend([0x48, 0x8b, 0xb4, 0x24]);
        code.extend(saved.to_le_bytes());
        asm.push(format!("mov rsi, qword ptr [rsp + {saved:#x}]"));
    }
    let frame = saved + if error_code { 8 } else { 0 };
    code.extend([0x48, 0x8d, 0xbc, 0x24]);
    code.extend(frame.to_le_bytes());
    asm.push(format!("lea rdi, [rsp + {frame:#x}]"));
    code.push(0xe8);
    let call_at = code.len();
    code.extend([0; 4]);
    asm.push("call".into());
    for n in 0..16u8 {
        movups(&mut code, &mut asm, n, u32::from(n) * 16, false);
    }
    code.extend([0x48, 0x81, 0xc4]);
    code.extend(area.to_le_bytes());
    asm.push(format!("add rsp, {area:#x}"));
    for (name, _, pop) in GPRS.iter().rev() {
        code.extend(*pop);
        asm.push(format!("pop {name}"));
    }
    if error_code {
        code.extend([0x48, 0x83, 0xc4, 0x08]);
        asm.push("add rsp, 0x8".into());
    }
    code.extend([0x48, 0xcf]);
    asm.push("iretq".into());
    Trampoline { code, asm, call_at }
}

/// Bytes of the frame the aarch64 trampoline reserves: 272 of registers, 512 of SIMD.
pub const AARCH64_FRAME: u32 = 784;

fn aarch64() -> Trampoline {
    const SP: u32 = 31;
    let pair = |load: bool, simd: bool, a: u32, b: u32, off: u32| -> (u32, String) {
        let (scale, base) = if simd { (16, 0xad000000u32) } else { (8, 0xa9000000u32) };
        let w = base | if load { 1 << 22 } else { 0 } | ((off / scale) & 0x7f) << 15 | b << 10 | SP << 5 | a;
        let r = if simd { 'q' } else { 'x' };
        let place = if off == 0 { "[sp]".to_string() } else { format!("[sp, #{off:#x}]") };
        (w, format!("{} {r}{a}, {r}{b}, {place}", if load { "ldp" } else { "stp" }))
    };
    let single = |load: bool, reg: u32, off: u32| -> (u32, String) {
        let w = 0xf9000000u32 | if load { 1 << 22 } else { 0 } | (off / 8) << 10 | SP << 5 | reg;
        (w, format!("{} x{reg}, [sp, #{off:#x}]", if load { "ldr" } else { "str" }))
    };
    let sysreg = |name: &str, write: bool, reg: u32| -> (u32, String) {
        let (_, (op0, op1, crn, crm, op2), _) = crate::arch::SYSREGS.iter().find(|(n, ..)| *n == name).expect("system register");
        let enc = (op0 - 2) << 19 | op1 << 16 | crn << 12 | crm << 8 | op2 << 5;
        if write { (0xd5100000 | enc | reg, format!("msr {name}, x{reg}")) } else { (0xd5300000 | enc | reg, format!("mrs x{reg}, {name}")) }
    };
    let frame = AARCH64_FRAME;
    let mut out: Vec<(u32, String)> = vec![(0xd1000000 | frame << 10 | SP << 5 | SP, format!("sub sp, sp, #{frame:#x}"))];
    for n in 0..15u32 {
        out.push(pair(false, false, n * 2, n * 2 + 1, n * 16));
    }
    out.push(single(false, 30, 240));
    out.push(sysreg("elr_el1", false, 0));
    out.push(sysreg("spsr_el1", false, 1));
    out.push(pair(false, false, 0, 1, 248));
    out.push((0x91000000 | frame << 10 | SP << 5 | 2, format!("add x2, sp, #{frame:#x}")));
    out.push(single(false, 2, 264));
    for n in 0..16u32 {
        out.push(pair(false, true, n * 2, n * 2 + 1, 272 + n * 32));
    }
    out.push((0x91000000 | SP << 5, "mov x0, sp".into()));
    let call_at = out.len() * 4;
    out.push((0x94000000, "bl".into()));
    out.push(pair(true, false, 0, 1, 248));
    out.push(sysreg("elr_el1", true, 0));
    out.push(sysreg("spsr_el1", true, 1));
    for n in 0..16u32 {
        out.push(pair(true, true, n * 2, n * 2 + 1, 272 + n * 32));
    }
    for n in 0..15u32 {
        out.push(pair(true, false, n * 2, n * 2 + 1, n * 16));
    }
    out.push(single(true, 30, 240));
    out.push((0x91000000 | frame << 10 | SP << 5 | SP, format!("add sp, sp, #{frame:#x}")));
    out.push((0xd69f03e0, "eret".into()));
    Trampoline { code: out.iter().flat_map(|(w, _)| w.to_le_bytes()).collect(), asm: out.into_iter().map(|(_, t)| t).collect(), call_at }
}

/// Instruction `b .` (also the encoding of `b <reloc>` before relocation).
pub(crate) const BRANCH: u32 = 0x1400_0000;
