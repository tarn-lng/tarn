# Freestanding programs: symbols, sections, statics, linking

Design: [ADR 0101](adr/0101-freestanding-kernel-target.md). Everything here
works on every target; sections need an ELF object (Linux, `*-none`), and a
linked image is built only for `*-none`.

A kernel, bootloader or firmware image needs three things a hosted program
never asks for: names the boot code can find, data at a fixed layout in a
named section, and a link step driven by a linker script. Tarn adds
**attributes** (`#[name(argument)]` on the line before a function or static)
and **`static` items**.

```tarn
import "ffi"

// A boot-loader request: C layout, in the section the loader scans.
struct Request {
    id [4]u64
    revision u64
    response *mut u64
}

#[section(".limine_requests")]
#[align(8)]
static mut MEMORY_MAP Request = Request{
    id: [4]u64{0xc7b1dd30df4c8b88, 0x0a82e883a194f07b, 0x9d5827dcd881dd75, 0xa3148604b1f4faa0},
    revision: 0,
    response: ffi.null(),
}

#[align(16)]
static mut BOOT_STACK [16384]u8

// The freestanding contract, written in Tarn: no undefined symbols are left.
#[export("tarn_rt_fault")]
fn fault() {
    for true {}
}

#[export("tarn_rt_panic")]
fn panic_message(message *u8) {
    for true {}
}

#[export("tarn_rt_panic_at")]
fn panic_at(message *u8, location *u8) {
    for true {}
}

#[section(".text.boot")]
#[export("kmain")]
fn kmain() {
    for true {}
}
```

```sh
tarn build kernel.tarn --target x86_64-none --linker-script kernel.ld -o kernel.elf
```

## Attributes

| Attribute | On | Meaning |
|---|---|---|
| `#[export("name")]` | function, static | A global symbol `name` (a C identifier) with the C ABI. For a function it is an adapter to the Tarn body, so the body keeps Tarn's checks. |
| `#[section(".name")]` | function, static | Place the code or data in the section `.name` (`.` then letters, digits, `.`, `_`, `-`). ELF only. |
| `#[align(n)]` | static | At least `n`-byte alignment; a power of two up to 65536. |
| `#[interrupt]`, `#[interrupt(n)]` | function | An interrupt or exception handler (see [Interrupt handlers](#interrupt-handlers)). `n` is an aarch64 vector table slot. |
| `#[vector_table]` | function | Stands for the generated aarch64 exception vector table. |
| `#[pvh_entry]` | function | The 64-bit kernel entry of an x86_64 PVH boot (see [PVH boot](#pvh-boot)). |

Rules, all reported as E3086 (E1023 for a malformed attribute):

- An exported function is a plain function: not generic, not `async`, not a
  method, not `extern`. Parameters and result are integers, floats, `bool` or
  raw pointers (the same set as `extern "C"`); the result may also be `void`.
- A symbol is exported once. Names beginning `tarn_` belong to the compiler,
  except that a `*-none` program may define the freestanding contract
  (`tarn_rt_fault`, `tarn_rt_panic`, `tarn_rt_panic_at`, `tarn_alloc`,
  `tarn_free`, `tarn_realloc`) and must then match the contract's signature:
  `tarn_rt_fault()`, `tarn_rt_panic(*u8)`, `tarn_rt_panic_at(*u8, *u8)`,
  `tarn_alloc(usize) *mut u8`, `tarn_free(*mut u8)`,
  `tarn_realloc(*mut u8, usize) *mut u8`. `main` is reserved on hosted
  targets.
- An exported function is a root of the program even if nothing calls it. On
  `*-none` a program with an exported function does not need `fn main`; the
  `tarn_main` shim is emitted only when `main` exists.
- `#[section]` on a function puts the exported adapter (if any) and the Tarn
  body in that section; every other function goes to `.text.<symbol>`. Several
  items may name one section: the object then holds several sections of that
  name, which a linker merges. `#[section]` for a Mach-O target is an error.
- Code in the contract cannot rely on what it implements: a `tarn_rt_fault`
  whose body overflows an integer calls itself.

## Statics

```
static NAME Type = initializer
static mut NAME Type = initializer
static mut NAME Type                 // all bytes zero (.bss)
```

A static is fixed-layout global data. `Type` is an integer, float, `bool`, raw
pointer, fixed array (not empty), instantiated struct or enum of those. Structs use
declaration order and natural alignment, which is the C layout, with no
size limit (a 1 MiB table is fine; ordinary values stay limited to 64 KiB).
The initializer is evaluated at compile time: literals, `const` names and
arithmetic, `[N]T{...}` and `Struct{...}` literals giving every element and
field, `ffi.null()`, `ffi.from_address(n)`, `ffi.callback(function)`, and the
name of another static (stored as its address, a relocation). Anything else
is E3085. Payload-free enum variants such as `None` are also const initializers; owning and enum-containing statics require an explicit initializer. Enum layout is the ordinary u32 tag plus aligned payload union (ADR 0110), rather than a C enum ABI. Function addresses work in nested arrays and structs too:

```tarn
import "ffi"
fn first() {}
fn second() {}
static HANDLERS [3]*u8 = [3]*u8{ffi.callback(first), ffi.callback(second), ffi.null()}
```

The callback must directly name a safe, synchronous, non-generic Tarn function
with scalar/pointer C ABI parameters and result. The linker fills the address;
no initialization code runs. Targets are retained even if otherwise unused.
An ordinary or exported function uses its C adapter; an `#[interrupt]`
function uses its trampoline, exactly like runtime `ffi.callback`. ELF and
Mach-O on x86_64 and aarch64 support these relocations. x86_64 IDT gates still
need runtime encoding because their address is split into fields; a static
handler table can supply their addresses (see the kernel example).
[ADR 0102](adr/0102-static-function-addresses.md) records the contract.

**Using a static's name as an expression yields its address**: `*T`, or
`*mut T` for `static mut`. Creating the address is safe; reading or writing
through it is `ffi.read`/`ffi.write` (or the volatile forms) inside `unsafe`,
like any raw pointer. Nothing ever borrows a static, so the ownership checker
has nothing to track and cannot protect you: the author owns data races,
aliasing and alignment. An immutable `static` lives in read-only memory; writing
through its address faults.

Reach a field or element by address arithmetic:

```tarn
import "ffi"

struct Header {
    magic u32
    flags u8
}

static mut HEADER Header = Header{magic: 1, flags: 2}

fn flags() u8 {
    field: *u8 := ffi.from_address(ffi.address(HEADER) + 4)
    unsafe { return ffi.read(field) }
}

fn main() {
    print(flags())
}
```

Every static is emitted whether or not it is used (a loader request is read by
no code); a static with `#[section]` is also marked retained
(`SHF_GNU_RETAIN`) so a linker's `--gc-sections` keeps it.

## Interrupt handlers

```tarn
import "arch_x86_64"
import "ffi"

#[align(4096)]
static mut IDT[4096]u8          // 256 gates of 16 bytes
static mut TICKS u64

// Breakpoint (#BP, vector 3): no error code.
#[interrupt]
fn breakpoint(frame *mut arch_x86_64.InterruptFrame) {
    unsafe { arch_x86_64.set_frame_rip(frame, arch_x86_64.frame_rip(ffi.to_const(frame))) }
}

// Page fault (#PF, vector 14): the CPU pushes an error code, so the handler
// takes it as a second `u64` parameter.
#[interrupt]
fn page_fault(frame *mut arch_x86_64.InterruptFrame, code u64) {}

// Timer (vector 32, the first remapped PIC/APIC vector).
#[interrupt]
fn timer(frame *mut arch_x86_64.InterruptFrame) {
    ticks: *mut u64 := ffi.from_address(ffi.address(TICKS))
    unsafe { ffi.write_volatile(ticks, ffi.read_volatile(ffi.to_const(ticks)) + 1) }
}

fn install_idt() {
    idt: *mut u8 := ffi.from_address(ffi.address(IDT))
    unsafe {
        arch_x86_64.idt_set_gate(idt, 3, ffi.callback(breakpoint), 8, 0, arch_x86_64.gate_trap())
        arch_x86_64.idt_set_gate(idt, 14, ffi.callback(page_fault), 8, 0, arch_x86_64.gate_interrupt())
        arch_x86_64.idt_set_gate(idt, 32, ffi.callback(timer), 8, 0, arch_x86_64.gate_interrupt())
        arch_x86_64.idt_load(ffi.to_const(idt), 256)
    }
}
```

`#[interrupt]` marks an ordinary Tarn function (all its checks stay on) that
hardware, not a call, enters. The compiler gives it a machine-code
**trampoline** that saves the interrupted state, calls the function with the
frame pointer, restores the state and returns with `iretq` (x86_64) or `eret`
(aarch64). The trampoline is what the hardware must be given, and its address
is `ffi.callback(handler)`: for an `#[interrupt]` function `ffi.callback`
returns the trampoline, not a C adapter. The function itself is still an
ordinary function; calling it from Tarn code runs the body without the
trampoline.

Signatures (anything else is E3086; `unsafe fn`, generic, `async` and methods
are rejected too; the result is always `void`):

| Target | Signature | Frame |
|---|---|---|
| x86_64 | `fn h(frame *mut Frame)` | the 40-byte hardware frame (`rip cs rflags rsp ss`); `arch_x86_64.InterruptFrame` |
| x86_64 | `fn h(frame *mut Frame, code u64)` | as above; the CPU also pushed an error code (#DF 8, #TS 10, #NP 11, #SS 12, #GP 13, #PF 14, #AC 17, #CP 21, #SX 30; #DF always 0) |
| aarch64 | `fn h(frame *mut Frame)` | the 272-byte saved register frame (`arch_aarch64.ExceptionFrame`: `x0..x30`, `elr`, `spsr`, `sp`) |

There is no attribute argument to choose the variant: the number of parameters
does. `Frame` is any sized type behind a `*mut`; the standard library frames
come with `frame_rip`, `frame_cs`, `frame_rflags`, `frame_rsp`, `frame_ss` and
`set_frame_rip` (x86_64) and `frame_x`, `set_frame_x`, `frame_elr`,
`set_frame_elr`, `frame_spsr`, `set_frame_spsr` (aarch64). Writing the frame
changes what the return instruction resumes.

`#[export("name")]` on a handler exports its **trampoline** under `name`
(for a linker script or assembly stub). `#[section]` applies to the
trampoline too. Without `#[export]` the trampoline is private to the object
and found with `ffi.callback`. Hosted targets reject handlers (a build error:
they need a `*-none` target); `tarn check` accepts them.

**x86_64 IDT.** `idt_set_gate(idt, vector, handler, selector, ist, gate)` writes
a 16-byte gate (`gate_interrupt()` clears IF on entry, `gate_trap()` keeps it,
`gate_user_interrupt()` also lets ring 3 use `int n`), `idt_load` does `lidt`.
The GDT, code selector, PIC/APIC setup and `sti` are the kernel's.

**aarch64 vector table.** Each hardware slot contains branch instructions,
so the compiler generates the executable table. Its address may also be
stored in a static with `ffi.callback(vectors)`:

```tarn
import "arch_aarch64"
import "ffi"

// slot 4: synchronous exception from the current EL using SP_ELx; slot 5: its IRQ.
#[interrupt(4)]
fn synchronous(frame *mut arch_aarch64.ExceptionFrame) {}
#[interrupt(5)]
fn irq(frame *mut arch_aarch64.ExceptionFrame) {}

#[vector_table]
fn vectors() {}

fn install() {
    unsafe {
        arch_aarch64.write_cpacr_el1(3 << 20)   // the trampoline saves q0..q31
        arch_aarch64.write_vbar_el1(u64(ffi.address(ffi.callback(vectors))))
        arch_aarch64.isb()
    }
}
```

The `#[vector_table]` function (no parameters, empty body; at most one per
program, `#[export]`/`#[section]` allowed) is replaced by a 2 KiB-aligned
table of 16 entries of 128 bytes. A slot holding `#[interrupt(n)]` is a `b`
to that handler's trampoline (`R_AARCH64_CALL26`, so the handler must be
within +-128 MiB; the linker reports an out-of-range branch); an empty slot is
`b .`. Slots: 0-3 current EL with SP_EL0, 4-7 current EL with SP_ELx, 8-11 lower
EL AArch64, 12-15 lower EL AArch32, each as synchronous, IRQ, FIQ, SError.
`ffi.callback(vectors)` is the table's address. A slot attribute without a
table, a slot used twice and slots on x86_64 are build errors.

### The trampolines

x86_64, with `rsp` on entry already 16-byte aligned by the CPU (so the call is
aligned; the sequence assumes 64-bit mode, which always aligns):

```
push rax rcx rdx rsi rdi r8 r9 r10 r11     the caller-saved integer registers
sub rsp, 256 (264 with an error code, to keep alignment)
movups [rsp + 16*i], xmm<i>                i = 0..15
cld                                        System V requires DF = 0
mov rsi, [error code]                      only with an error code
lea rdi, [hardware frame]
call <C adapter of the handler>
movups xmm<i>, [rsp + 16*i] ; add rsp ; pop r11 ... rax
add rsp, 8                                 only with an error code
iretq
```

Callee-saved registers need no saving (the C ABI callee preserves them). The
**SSE state is saved** (xmm0-15), because Cranelift may use SSE registers for
floats and bulk copies; it costs about 32 stores and loads per interrupt and
needs `CR4.OSFXSR` set (otherwise `movups` raises #UD, as any SSE code does).
MXCSR and the x87 state are not saved: handlers must not change them. The red
zone is not used by Tarn code, so a handler may interrupt any Tarn code.
Not handled: `swapgs` (a handler entered from ring 3 would need it), IST
stack switching bookkeeping, nested use of the same stack beyond what the
hardware does, and the full register file as an argument (the handler sees
the hardware frame only; a scheduler that switches contexts must save the
general registers itself, which P8 or later can do by exposing the pushed
registers).

aarch64 (SP is 16-byte aligned at an exception): `sub sp, sp, #784`; store
`x0..x30`, `elr_el1`, `spsr_el1`, the old `sp`; store `q0..q31` after the
272-byte frame; `mov x0, sp`; `bl <C adapter>`; reload `elr_el1`/`spsr_el1`
from the frame, then `q0..q31`, `x0..x30`; `add sp, sp, #784`; `eret`. FP/SIMD
must be enabled at EL1 (`write_cpacr_el1(3 << 20)`) before the first exception.
Interrupts stay masked while the handler runs (the CPU sets PSTATE.I); a
handler that unmasks them is safe because the frame holds `elr`/`spsr`.

The sequences are generated in `compiler/backend/src/interrupt.rs`, and the
test `trampolines_disassemble_to_the_documented_sequence` disassembles them
with the host `objdump`.

## PVH boot

QEMU's `-kernel`, Xen and Cloud Hypervisor start an ELF that has a PVH note
(`XEN_ELFNOTE_PHYS32_ENTRY`) in 32-bit protected mode with paging off; no
bootloader image, ISO or download is needed. Tarn emits only 64-bit code, so
`#[pvh_entry]` on the kernel's entry function makes the compiler generate the
rest:

```tarn
import "ffi"

#[pvh_entry]
#[export("kmain")]
fn kmain(info u64) {        // or zero/two parameters; info is a physical address
    // long mode, paging on, SSE on, interrupts off
}
```

- `tarn_pvh_start`, a global 32-bit code symbol (the ELF entry; name it in
  `ENTRY(tarn_pvh_start)`): sets a 16 KiB stack, builds identity-mapped page
  tables for the first GiB (2 MiB pages), sets `CR4.PAE|OSFXSR|OSXMMEXCPT` and
  `EFER.LME`, loads a GDT (selector `0x08` 64-bit code, `0x10` data), enables
  paging, clears `CR0.EM/TS` (sets `MP`, `NE`) and far-jumps to a 64-bit part
  that loads the data segments and calls the function with `edi` = the
  `ebx` the loader passed. The bytes and their assembly are in
  `compiler/backend/src/pvh.rs`.
- Tables, stack and GDT are compiler-made local data (`tarn_pvh_tables`,
  `tarn_pvh_stack` in `.bss.pvh`, GDT and descriptor in `.rodata.pvh`).
  Keep `.bss.*` and `.rodata.*`; the stub is in `.text.pvh.boot`. Bootstrap
  VMAs must be physical, below 4 GiB and inside the identity map (1 MiB is
  usual). Ordinary kernel code/data may be high-linked as described below.
- The note lives in section `.note.Xen`. A script must keep it and give it its
  own `PT_NOTE` program header, because QEMU reads the note from there:

```
ENTRY(tarn_pvh_start)
PHDRS { text PT_LOAD FLAGS(5); data PT_LOAD FLAGS(6); note PT_NOTE; }
SECTIONS {
    . = 1M;
    .text : { *(.text .text.*) } :text
    .rodata : { *(.rodata .rodata.*) } :text
    .note : { KEEP(*(.note.Xen)) } :text :note
    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data
}
```

Rules: x86_64 `*-none` only (anything else is a build error), one per
program, a function with zero, one or two `u64`/thin raw pointer parameters and no result,
not `unsafe fn` (E3086); it may also carry `#[export]` and `#[section]`. If
the function returns, the CPU halts. Interrupts are disabled; the GDT is the
stub's: `idt_set_gate` selector `8`
is its code segment. The example kernel in
[examples/kernel-x86_64](../examples/kernel-x86_64/README.md) boots this way
and is booted by CI.

### Configuring mappings and taking over paging

```tarn
#[pvh_entry(identity = "4GiB", higher_half = 0xffffffff80000000)]
fn kernel(info u64, boot u64) {
    // Both addresses are physical; cast boot to *BootInfo in the kernel.
}
```

`identity` accepts whole GiB from 1 through 512 (default 1). All leaves are
2 MiB pages, so no CPU support for 1 GiB pages is required. The optional
`higher_half` is a canonical upper-half, 1 GiB aligned **offset**: it aliases
physical [0, 1 GiB) at `higher_half + physical_address`. The high kernel's
entire physical load range must fit in that window. This bootstrap alias
shares the first identity page directory; it is writable/executable, with
no guard pages. The kernel replaces it with its own permissions and direct
map. The configured identity size describes addressability, not usable RAM.

The optional second parameter is the physical address of a 96-byte, 8-byte
aligned descriptor: twelve consecutive `u64` fields in this order:
`version` (1), `tables`, `tables_bytes`, `stack`, `stack_bytes` (16384),
`stub`, `stub_bytes`, `gdt`, `gdt_bytes` (24), `descriptor_bytes` (96),
`identity_bytes`, `higher_half` (0 without an alias). All pointer fields are
physical. `tables` equals initial CR3; its allocation is
`(2 + identity_GiB + has_higher_half) * 4096` bytes. The first parameter
remains the physical `hvm_start_info` address. See the example's `BootInfo`
struct for the exact Tarn declaration.

Reserve every descriptor range, the descriptor itself, the complete physical
kernel/boot image, and loader-owned structures from the frame allocator,
rounding outward to page boundaries. Copy the descriptor before reclaiming
it; replace CR3, stack and GDTR before releasing their storage. If removing
the low identity map, the kernel must not return to the physical stub.

[examples/kernel-higher-half](../examples/kernel-higher-half/README.md)
includes a tested `AT()` linker script: the note and bootstrap stay low;
normal sections have high VMAs and low physical LMAs. Keep its `BASE` equal
to the attribute's offset. It prints its high function address and validates
the physical boot information. [ADR 0105](adr/0105-configurable-pvh-hand-off.md)
specifies the complete hand-off state, memory layout and limitations.

## arm64 boot

`#[arm64_entry]` is the aarch64 counterpart of `#[pvh_entry]` (ADR 0106). The
compiler emits a Linux arm64 **Image header** and an EL2-to-EL1 / EL1 stub
(`tarn_arm64_start`, section `.text.arm64.boot`) so `qemu-system-aarch64 -M virt
-kernel Image` and U-Boot `booti` load the kernel; the entry gets the DTB
physical address:

```tarn
#[arm64_entry]
fn kmain(dtb u64) {      // zero or one parameter: u64 or a thin pointer
    // EL1h, MMU and caches off, interrupts masked, FP/SIMD on, .bss zero
}
```

```sh
tarn build main.tarn --target aarch64-none --linker-script kernel.ld --binary -o Image
qemu-system-aarch64 -M virt,gic-version=3 -cpu cortex-a72 -nographic -semihosting -kernel Image
```

`--binary` links with `--oformat=binary` (no `objcopy`) and needs
`--linker-script` and an `-none` target. The script must place
`.text.arm64.boot` first at RAM base + 0x80000 (0x40080000 on QEMU virt),
include `tarn_strings` in `.rodata`, and define `__arm64_image_size`,
`__bss_start`, `__bss_end`, `__arm64_stack_top`; see
`examples/kernel-aarch64/kernel.ld`. A kernel entered at EL2 is dropped to
EL1. The exact hand-off state is in ADR 0106. `arch_aarch64.semihost_exit`
ends QEMU with the kernel's status when `-semihosting` is given.

## Entry point

The ELF entry comes from the linker script (`ENTRY(kmain)`), from
`--entry kmain`, or, when neither names one and the program has `fn main`,
the default `tarn_main`. `--entry` overrides the script. The entry symbol
must be an `#[export]`ed function (or `tarn_main`).

## Linking

`tarn build --target x86_64-none --linker-script kernel.ld [--entry sym]
[-o kernel.elf]` writes a static ELF (default `<entry>.elf`). Without
`--linker-script` the relocatable object is written (default `<entry>.o`). The
build runs `LINKER -nostdlib -static -T kernel.ld [-e sym] -o kernel.elf
kernel.o`. Tarn bundles no linker and downloads nothing; the first match
of these is used:

1. the program named by the `TARN_LD` environment variable;
2. `ld.lld` on `PATH` (also looked for in Homebrew's `lld` and `llvm`
   directories);
3. a cross GNU linker for the architecture: `x86_64-elf-ld`,
   `x86_64-linux-gnu-ld`; `aarch64-elf-ld`, `aarch64-none-elf-ld`,
   `aarch64-linux-gnu-ld`;
4. `ld`, only on a Linux host of the same architecture;
5. the `ld.lld` inside the Rust toolchain
   (`$(rustc --print sysroot)/lib/rustlib/<host>/bin/gcc-ld/ld.lld`), so any
   machine that can build Tarn can link a kernel.

If none exists the build fails naming what to install, and nothing is
written. If the linker fails its stderr is shown and no output is left. Tarn
does not pass `--gc-sections`; use `KEEP(...)` or `/DISCARD/` in the script.

### Accessing static fields and elements

Raw-pointer field and fixed-array projection (ADR 0103) uses C layout without
loading an aggregate or manufacturing a reference:

```tarn
import "ffi"

struct PageTable { entries [512]u64 }
static mut ROOT PageTable
fn map(slot usize, entry u64) {
    unsafe { ffi.write(ROOT.entries[slot], entry) }
}
```

The result preserves pointer mutability. Projection and access require
`unsafe`; indices are bounds checked. Use `ffi.read_volatile` and
`ffi.write_volatile` on the projected pointers for MMIO. The example kernel
uses projection for its handler table and PVH boot-info prefix.

## Owning static slots (L1.1)

Non-Copy and must-consume values can inhabit fixed-layout static roots.
Initialize optional slots explicitly with None, for example
`static mut TASKS [2]Option<Task> = [2]Option<Task>{None, None}`.
Inside unsafe, `ffi.replace_owned(TASKS[index], incoming)` returns the previous
Option and installs incoming without dropping either owner. Consume every
returned Some payload; exchange with None to extract and leave an empty slot.
`ffi.take_owned` instead leaves uninitialized memory and must be paired with
`ffi.write_owned` before another read. write_owned requires an uninitialized
destination, so it must never overwrite a live owner. Existing scalar/Copy
ffi.read/write do not become an aggregate copy gateway.

Statics are never dropped: drain at reclamation/shutdown or document permanent
ownership. This is an unsafe root contract, including synchronization and
exclusive access; local exclusive Option.take/replace/put remain safe.
[ADR 0110](adr/0110-owning-slots.md) and [the integration report](owning-slots-report.md)
give full rules and the Cairn task-table pattern.
