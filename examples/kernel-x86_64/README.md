# x86_64 kernel in Tarn

A small kernel written entirely in Tarn (ADR 0101, P8). It boots under QEMU,
without a bootloader image, and exercises what the freestanding target
offers:

- the compiler-generated **PVH boot code** (`#[pvh_entry]`): 32-bit protected
  mode to long mode, identity-mapped first GiB, SSE enabled, a GDT;
- **serial output** on COM1 written in Tarn over `out8`/`in8`;
- a **heap** (first-fit free list over a static arena) behind `tarn_alloc`,
  `tarn_free` and `tarn_realloc`, used by a `Vec<i64>` and string
  concatenation, and checked for reuse of freed blocks;
- the **panic handler** (`tarn_rt_panic`, `tarn_rt_panic_at`, `tarn_rt_fault`)
  printing to the serial port and ending the machine through QEMU's
  `isa-debug-exit` device;
- an **IDT** with a breakpoint trap (`int3`), exception handlers (double
  fault, #GP, #PF with its error code, #UD) and the **PIT timer** on the
  remapped 8259 PIC (vector 32), counting ticks until `ticks ok`.

## Run it

You need `qemu-system-x86_64` and an ELF linker: `ld.lld` (LLVM), a cross GNU
`ld`, or the `ld.lld` that ships inside a Rust toolchain (found automatically).
Set `TARN_LD` to use another one. Nothing is downloaded.

```sh
tarn build examples/kernel-x86_64/main.tarn --target x86_64-none \
    --linker-script examples/kernel-x86_64/kernel.ld -o kernel.elf

qemu-system-x86_64 -kernel kernel.elf -display none -serial stdio -monitor none \
    -no-reboot -m 64 -device isa-debug-exit,iobase=0xf4,iosize=0x04
```

or `examples/kernel-x86_64/run.sh [path/to/tarn] [kernel.elf]`, which does both
and maps QEMU's exit status. Expected serial output:

```
tarn kernel: booted
pvh start info: magic ok
hello, heap world
vec of 1000 squares sums to 332833500
hello, heap world
vec of 1000 squares sums to 332833500
heap ok: 16496 bytes in use at the top, freed blocks reused
int3: breakpoint handled
10 timer interrupts
ticks ok
tarn kernel: done
```

QEMU exits with status 33 (the kernel wrote 16 to port `0xf4`; the device
reports `value * 2 + 1`). A panic or CPU exception prints a message and exits
with 35. `tarn_backend`'s `kernel_boot` test builds, boots and checks all of
this, and the failure paths (panic, page fault, failed bounds check); it
skips with a message when QEMU or a linker is missing. CI runs it in the
`kernel-qemu` job.

## How it boots

QEMU's `-kernel` accepts an ELF with a PVH note (`XEN_ELFNOTE_PHYS32_ENTRY`,
type 18, name `Xen`) and enters it in 32-bit protected mode with paging off
and `ebx` pointing at an `hvm_start_info`. `#[pvh_entry]` on `kmain` makes
the compiler emit that note and a hand-assembled 32-bit stub,
`tarn_pvh_start`, which builds the page tables, enters long mode and calls
`kmain(info)` with the `hvm_start_info` address as its argument. `kernel.ld`
places the image at 1 MiB and puts the note in a `PT_NOTE` segment (the
`PHDRS` block), which QEMU requires. Details in
[docs/freestanding.md](../../docs/freestanding.md#pvh-boot).

The boot code leaves the CPU with: long mode, paging on (first 1 GiB identity
mapped with 2 MiB pages), GDT selectors `0x08` (64-bit code) and `0x10`
(data), `CR4.OSFXSR/OSXMMEXCPT` set and `CR0.EM` clear (the interrupt
trampolines save `xmm0-15`), interrupts off, and a 16 KiB boot stack in
`.bss`.

## Limits

x86_64 and QEMU only. There is no Limine, GRUB or multiboot support yet (they
need the same kind of boot note or request section; Limine is a natural next
example), no APIC, no SMP, no user mode, no paging beyond the boot mapping and
no memory-map parsing: the heap is a fixed 1 MiB static. The PVH boot code
maps only the first GiB.
