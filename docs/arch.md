# CPU intrinsics (`arch_x86_64`, `arch_aarch64`)

Kernel code needs instructions that no expression can name: port I/O,
control registers, `lgdt`, system-register moves, `wfi`. The native backend
(Cranelift) has no inline assembly, and Tarn does not add a free-form `asm`
(ADR 0101). Instead each architecture has a standard module of typed,
`unsafe` functions, one per instruction.

```tarn sketch
import "arch_x86_64"
import "ffi"

fn serial_write(byte u8) {
    unsafe {
        arch_x86_64.out8(0x3f8, byte)       // COM1 data register
    }
}

fn halt_forever() {
    unsafe {
        arch_x86_64.cli()
        for true { arch_x86_64.hlt() }
    }
}
```

Rules

- Every function is `unsafe` (outside `unsafe` is E3071). The caller owns the
  precondition: privileged instructions need ring 0 (x86_64) or EL1
  (aarch64) and fault in user mode; a bad control-register, MSR or
  translation-table value can break memory safety.
- A module works only for its own architecture. Compiling a program that calls
  `arch_x86_64` for an aarch64 target (or the reverse) is a build error
  naming the intrinsic and the target. `tarn check` does not know the target,
  so the error appears at `tarn build --target`.
- They are allowed on hosted targets of the same architecture too, which
  is how unprivileged ones (`pause`, `rdtsc`, `cntvct_el0`, barriers) are tested.
- Each call is an ordinary call to a tiny function defined inside the emitted
  object, with the platform C ABI. Only the stubs a program calls are
  emitted; they are never exported, so the object still imports only the
  freestanding contract. There is no inlining yet, so these are not for hot
  loops.
- Intrinsics are not memory fences for the compiler's own accesses. Order
  device accesses with `ffi.read_volatile`/`write_volatile`/`fence()`.

## `arch_x86_64`

| Function | Instruction |
|---|---|
| `in8/in16/in32(port u16) u8/u16/u32` | `in al/ax/eax, dx` |
| `out8/out16/out32(port u16, value)` | `out dx, al/ax/eax` |
| `cli()`, `sti()`, `hlt()`, `pause()` | same name |
| `int3()` | `int3` (software breakpoint, vector 3) |
| `rdtsc() u64` | `rdtsc` (edx:eax combined) |
| `read_cr0/cr2/cr3/cr4() u64` | `mov rax, crN` |
| `write_cr0/cr3/cr4(value u64)` | `mov crN, value` |
| `invlpg(address u64)` | `invlpg [address]` |
| `rdmsr(msr u32) u64`, `wrmsr(msr u32, value u64)` | `rdmsr`, `wrmsr` |
| `lgdt(descriptor *u8)`, `lidt(descriptor *u8)` | load GDTR / IDTR from a 10-byte (u16 limit, u64 base) descriptor |

`lgdt` does not reload segment registers and `sti` takes effect after the next
instruction.

## `arch_aarch64`

| Function | Instruction |
|---|---|
| `wfi()`, `wfe()`, `isb()`, `dsb_sy()`, `dmb_sy()` | same name (`dsb sy`, `dmb sy`) |
| `irq_enable()`, `irq_disable()` | `msr daifclr, #2`, `msr daifset, #2` |
| `read_<reg>() u64` | `mrs x0, <reg>` |
| `write_<reg>(value u64)` | `msr <reg>, x0` |

Registers (a whitelist, so a typo is a compile error, not a trap): `daif`,
`vbar_el1`, `ttbr0_el1`, `ttbr1_el1`, `tcr_el1`, `mair_el1`, `sctlr_el1`, `cpacr_el1`,
`esr_el1`, `far_el1`, `elr_el1`, `spsr_el1`, `cntv_ctl_el0`, `cntv_tval_el0`
(read and write); `currentel`, `cntfrq_el0`, `cntvct_el0` (read only). A
system-register write takes effect only after a context-synchronization
event: call `isb()` where it matters.

## Interrupts and exceptions

Entering a handler from hardware needs register save/restore and `iretq` or
`eret`, which no typed intrinsic can express. `#[interrupt]` functions get
generated trampolines instead (see
[freestanding.md](freestanding.md#interrupt-handlers)). The pieces in these
modules:

- `arch_x86_64`: `InterruptFrame` (`rip cs rflags rsp ss`), `frame_rip/cs/rflags/rsp/ss`,
  `set_frame_rip`, `idt_set_gate(idt, vector, handler, selector, ist, gate)`,
  `idt_load(idt, entries)`, `gate_interrupt()`, `gate_trap()`, `gate_user_interrupt()`.
- `arch_aarch64`: `ExceptionFrame` (`x[31] elr spsr sp`), `frame_x`/`set_frame_x`,
  `frame_elr`/`set_frame_elr`, `frame_spsr`/`set_frame_spsr`, and
  `read_/write_cpacr_el1` (enable FP/SIMD before the first exception).

## How the stubs are built

Each stub is a few hand-assembled instructions in `compiler/backend/src/arch.rs`
(bytes plus the assembly they encode). The test
`cpu_stubs_disassemble_to_their_documented_assembly` emits every stub for both
architectures, disassembles the object with the host LLVM `objdump` and
compares each stub with its documented assembly (skipped when `objdump` is
missing). x86_64 stubs use only `rax`, `rcx` and `rdx`; aarch64 stubs only `x0`.
