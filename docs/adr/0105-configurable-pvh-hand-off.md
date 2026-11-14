# ADR 0105: Configurable PVH mappings and physical boot hand-off

Status: accepted (Cairn T4); extends ADR 0101 P8.

## Problem

Cairn phase 0C must exclude compiler-owned boot memory from its frame
allocator, and 0E must replace paging while running a higher-half kernel.
P8's fixed first-GiB identity map and undocumented local allocations made
that transition unnecessarily difficult. ADRs 0102–0104 already provide
static callback addresses, raw struct projections and CPU operations.

## Decision

Preserve `#[pvh_entry]` and its zero/one-argument entry functions. Add named
literal attribute options and an optional second entry argument:

```tarn
#[pvh_entry(identity = "4GiB", higher_half = 0xffffffff80000000)]
fn kernel(start_info u64, boot u64) {
    // Read the physical ABI v1 descriptor through a *BootInfo.
}
```

`identity` is a whole number of GiB from 1 through 512, default 1.
`higher_half` is an optional integer, canonical upper-half, 1 GiB aligned.
Unknown/duplicate options, wrong literal kinds, out-of-range sizes and bad
addresses receive E3086. The parser retains named arguments as neutral AST
values; the type checker validates them once into `PvhConfig`.
Only x86_64-none accepts PVH emission; the other targets are unchanged.

Use **2 MiB pages throughout**, available on baseline x86_64 with PAE; there
is no PDPE1GB requirement, CPUID dependency or 1 GiB-page fast path.
PML4[0] points to one PDPT with N entries, each pointing to its own PD.
The directory loop writes both halves of every 64-bit entry, so addresses
above 4 GiB work through the full 512 GiB boundary. All unused entries are
zeroed. Mapping memory does not declare it usable RAM: the kernel must
still parse the loader's memory map, including holes and reserved regions.

When configured, `higher_half` is a **virtual offset**, not the exact first
instruction address. An extra high PDPT aliases the first physical GiB at
`higher_half + physical_address`, sharing the identity map's first PD.
This deliberately maps a superset of the physical kernel load range; the
kernel's whole physical image must fit below 1 GiB. The first instruction
might therefore be `0xffffffff80200000` with offset `0xffffffff80000000`
and physical load address 2 MiB. Other high PDPT slots remain absent.

The boot stub gets its own `.text.pvh.boot` section; its note, descriptor,
GDT, tables and stack stay at physical VMAs. Kernel functions/data have high
VMAs and low LMAs using `AT(ADDR(section) - BASE)`. A `movabs` plus indirect
call reaches the entry's full 64-bit C-adapter address, with no rel32 range
assumption. The PVH note continues to name the physical 32-bit stub.
[The example script](../../examples/kernel-higher-half/kernel.ld) separates
these sections and asserts the physical image fits the alias. Arbitrary
physical/virtual offsets, a physical kernel image above 1 GiB, and a
minimal exact-range high mapping are deferred until a kernel needs them.

## Hand-off ABI v1

The entry has zero, one, or two parameters, each `u64` or a thin raw pointer,
and no result. SysV RDI is the zero-extended physical `hvm_start_info` address
originally in EBX; RSI is the physical address of this immutable descriptor.
Existing entries ignore unused arguments. Its C layout is twelve successive
little-endian u64 fields (96 bytes, aligned to 8):

| Offset | Field | Meaning |
| --- | --- | --- |
| 0 | version | 1 |
| 8 | tables | Physical PML4 / initial CR3 |
| 16 | tables_bytes | `(2 + identity_GiB + has_higher_half) * 4096` |
| 24 | stack | Physical bottom of boot stack |
| 32 | stack_bytes | 16384 |
| 40 | stub | Physical beginning of boot code |
| 48 | stub_bytes | Entire generated stub, including long-mode code |
| 56 | gdt | Physical GDT address |
| 64 | gdt_bytes | 24 |
| 72 | descriptor_bytes | 96 (including these fields) |
| 80 | identity_bytes | `identity_GiB * 1073741824` |
| 88 | higher_half | Configured virtual offset, or 0 for no alias |

The table allocation is 4 KiB aligned and consists of PML4, identity PDPT,
N consecutive PDs, then the optional high PDPT. It is 12288 bytes by default,
24576 for a 4 GiB identity map, 28672 with that map and a high alias, and at
most 2109440 bytes for 512 GiB plus an alias. The separately allocated
16 KiB stack is 4 KiB aligned; its top is 16-byte aligned before the adapter
call. Tables and stack are true NOBITS allocations in `.bss.pvh`; the stub
explicitly clears the complete table range, independently of loader zeroing.
Cranelift 0.125.3 custom data sections cannot contain zero-init allocations,
so retained per-object BSS sections are named after object generation, using
the existing ELF section-name facility rather than storing megabytes of zeros.
GDT and descriptor live in `.rodata.pvh`, and the note in `.note.Xen`.
No boot heap, additional stack, scratch page or loader-memory write is used.
The stack temporarily holds the six-byte GDTR operand and the usual calls.

All bootstrap code/data must be linked at physical VMAs below both 4 GiB
(the stub uses Abs4 relocations) and the configured identity limit. Placing
it below 1 GiB satisfies every configuration. Kernels must exclude, rounded
outward to frames, the table, stack, stub, GDT and descriptor ranges above,
as well as the complete physical kernel/boot image and loader-owned start
info, memory map, command line and modules. The example's `__boot_start` /
`__boot_end` and `__kernel_phys_start` / `__kernel_phys_end` cover the whole
loaded image, section padding and note as a conservative reservation.

Entry state: long mode, CR0.PG/PE/NE/MP on and EM/TS clear,
CR4.PAE/OSFXSR/OSXMMEXCPT on, EFER.LME on, interrupts disabled, DF clear;
GDT selector 8 is 64-bit code, selector 16 data, FS/GS selectors zero.
No IDT is installed. Mapping flags are present, writable, supervisor and
executable with ordinary cache defaults; there is no NX, W^X or stack guard.
The kernel installs its own memory types and permissions. High and low
aliases share PD0, so editing it affects both aliases.

Copy the descriptor before reclaiming it. Replace CR3 before freeing tables;
move to a kernel stack before freeing the stack; replace GDTR before freeing
the GDT. Keep the low stub and return address mapped if returning; after
removing the low map the entry must not return. A returning entry otherwise
halts in the low stub. Reclaim only frames no longer shared with live image
or boot objects, and preserve loader structures until consumed.

## Alternatives and limits

A full exact-range mapping driven by linker-provided start/end symbols would
make every linker script part of a new symbol ABI and need more runtime
range logic. A single aligned 1 GiB alias reuses an existing PD, covers small
kernels and expresses the common higher-half offset directly. This is a
bootstrap mapping, not a permanent direct map of all RAM: Cairn installs its
own tables, guard pages and permissions afterwards. Dynamic page allocation,
CPU-dependent 1 GiB leaves and arbitrary mapping windows add complexity
without helping that hand-off. The identity limit does not probe RAM or
MAXPHYADDR; accessible physical addresses must fit the CPU address width
and the loader memory map. The tradeoff is up to roughly 2 MiB of tables
at the maximum identity size and a deliberately bounded high image window.

The feature is BSP-only; Xen, Cloud Hypervisor, KVM and physical hardware
have not been tested here. QEMU TCG and ELF linker tests cover the default
boot, a 4 GiB machine reading/writing above 1 GiB, a high-linked kernel
printing its high entry address and the physical hand-off, and the maximum
identity map's directory entries. Both freestanding target backends retain
cross-compilation coverage; this does not add an aarch64 PVH protocol.
