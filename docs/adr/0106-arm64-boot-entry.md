# ADR 0106: arm64 Image boot entry (`#[arm64_entry]`) and the QEMU virt example

Status: accepted (Cairn T5); extends ADR 0101 P5/P7 and mirrors ADR 0101 P8 / 0105 for aarch64.

## Problem

Cairn's ADR 0007 makes aarch64 first-class from phase 0: every phase must
boot under `qemu-system-aarch64 -M virt` (PL011, GICv3, generic timer) and
later under U-Boot `booti`. P5 (aarch64 intrinsics) and P7 (aarch64 vector
table and trampolines) had only been checked by disassembly. Nothing could
boot an aarch64 Tarn program, and nothing had ever executed those stubs.

## Decision

`#[arm64_entry]` on an ordinary safe function, with no options and zero or one
parameter (`u64` or a thin raw pointer: the DTB physical address), returning
nothing. At most one per program; only `aarch64-none` accepts it. The
function is a root and gets a C-ABI adapter; the compiler emits
`tarn_arm64_start` in section `.text.arm64.boot`, which the linker script must
place first at the load address.

```tarn
#[arm64_entry]
fn kmain(dtb u64) { ... }
```

### Image format

`tarn build main.tarn --target aarch64-none --linker-script kernel.ld --binary -o Image`
writes a flat binary. No `objcopy` is used: the linker (`ld.lld`/GNU ld) is
run with `--oformat=binary`. The first 64 bytes are the Linux arm64 header:
`code0` = `b +64`, `text_offset` = 0x80000, `image_size` = the linker symbol
`__arm64_image_size`, flags 0 (little endian), magic `ARM\x64` at offset 56.
The script (see `examples/kernel-aarch64/kernel.ld`) loads at RAM base +
`text_offset`, and defines `__arm64_image_size` (file plus `.bss` plus
stack), `__bss_start`, `__bss_end` and `__arm64_stack_top`, which are
imported by the stub through absolute relocations. The `.bss` is `NOLOAD`
so it is absent from the file but counted in `image_size`, so the loader
reserves it. Without `--binary` the same script gives an ELF (QEMU also
boots that; U-Boot `booti` needs the Image).

### Boot contract

On entry (QEMU `-kernel` and U-Boot `booti`): x0 = physical address of the
DTB, MMU off, D-cache off, interrupts masked, at EL1 or EL2, as the arm64
boot protocol says. The stub:

1. masks DAIF; if at EL2: `HCR_EL2.RW=1` (EL1 is AArch64), `CPTR_EL2=0`,
   `CNTHCTL_EL2` = 3 (EL1 may use the physical and virtual timer),
   `CNTVOFF_EL2=0`, `ICC_SRE_EL2` = SRE|Enable (GICv3 system registers
   usable from EL1), then `eret` to EL1h with DAIF masked;
2. at EL1: `SPSel=1`, `SCTLR_EL1` = RES1 value with M, C, I clear (MMU and
   caches stay **off**), `CPACR_EL1.FPEN=3` (FP/SIMD allowed, which the
   P7 trampolines need to save q0-q31);
3. sets SP to `__arm64_stack_top` (16 KiB reserved at the end of `.bss`);
4. zeroes `[__bss_start, __bss_end)` (so the stack is cleared too; it is
   unused at that point);
5. calls the entry with x0 still the DTB address. A returning entry
   reaches a `mask daif; wfi` loop.

**State at hand-off to Tarn code:** EL1h, MMU off (everything is physical,
device memory accesses are Device-nGnRnE and normal memory accesses are
non-cacheable, so no cache maintenance is required for DMA-less code),
D/I caches off, DAIF masked, FP/SIMD enabled, SP valid and 16-byte aligned,
`.bss` zero, x0 (and the argument) = DTB. The kernel owns the exception
vectors (VBAR_EL1 is unset until it installs a `#[vector_table]`), the GIC
and paging. Only the boot CPU runs (secondary CPUs stay parked by
firmware/PSCI). The stub is machine code in `compiler/backend/src/arm64.rs`
with each word next to its instruction, like the P5/P7 stubs; no external
assembler.

### Additions to the intrinsics

`svc()` (immediate 0), `brk()` (immediate 0; the saved ELR points at the
`brk`), `semihost_exit(status)` (semihosting `SYS_EXIT`, operation 0x18, on AArch64: parameter block
{ADP_Stopped_ApplicationExit, status} through `hlt #0xf000`), and the GICv3
CPU interface registers `icc_sre_el1`, `icc_pmr_el1`, `icc_igrpen1_el1`,
`icc_iar1_el1` (read), `icc_eoir1_el1` (write). `tarn build --binary`
requires `--linker-script` and an `-none` target.

### Exiting QEMU

The example uses ARM semihosting (`-semihosting`), so QEMU's exit status is
the kernel's (0 or 1), mirroring x86's `isa-debug-exit`. PSCI `SYSTEM_OFF`
would also work (QEMU exits 0 but cannot report a failure status); it needs
`hvc`/`smc` and was not added. Without `-semihosting` the `hlt` traps.

## The example kernel and what running it found

`examples/kernel-aarch64` boots to PL011 (0x09000000), parses the DTB
(magic, `/memory@...` `reg`), installs the P7 vector table through
`VBAR_EL1`, takes an `svc` and a `brk`, runs the heap demo (Vec and string
through the allocator contract), and counts ten generic-virtual-timer PPI
(27) interrupts through GICv3 (GICD 0x08000000, GICR 0x080A0000). It needs
`-M virt,gic-version=3`; QEMU's default is GICv2.

First execution of P5/P7 aarch64 code:

1. *P5/P7 held up.* The P7 vector table, the 784-byte trampolines (including
   x0-x30, ELR, SPSR, FP saves), `eret`, `set_frame_elr`, and the `mrs/msr`
   stubs ran correctly on the first try, at EL1 and after an EL2 drop.
   Interrupt return preserved all live registers (the timer test counts
   inside a `wfi` loop).
2. *Missing intrinsics:* no `svc`/`brk`, no semihosting exit and no GICv3
   CPU-interface registers (see above).
3. *Linker script:* the `tarn_strings` literal pool is an orphan section and
   lld put it **before** `.boot`, so the Image began with string data, not
   the header (QEMU then fell into an undefined-instruction loop at the
   first word). The aarch64 script now places `tarn_strings` inside
   `.rodata`; the header test asserts the magic.
4. *Hardware detail:* PL011 output before `UARTEN` is dropped by QEMU
   ("PL011 data written to disabled UART"); the kernel writes `CR` first.
5. *Example bug:* `brk` leaves ELR on the `brk`, so a returning handler has
   to add 4 to the frame's ELR; the example does it.

## Alternatives and limits

A U-Boot-style EFI stub (PE/COFF) is not generated; the plain Image header
suffices for `booti` and `-kernel`. A relocatable/PIE image is not
produced: the script fixes the load address (`0x40080000` for QEMU virt) and
the header uses text_offset 0x80000, so U-Boot must load at RAM base +
0x80000 (the normal `kernel_addr_r` convention). Not verified: real
hardware, U-Boot, KVM/HVF, other GIC versions, secondary CPUs, cache
maintenance (MMU is off throughout), EL3 entry. QEMU 11.1.3 TCG only.
Tests: `compiler/backend/tests/kernel_boot_aarch64.rs` (header, EL1 boot, EL2 boot,
failing kernel exit status); they skip when QEMU or a linker is missing.
