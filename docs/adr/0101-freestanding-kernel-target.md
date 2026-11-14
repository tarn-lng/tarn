# ADR 0101: Freestanding targets, toward a kernel written in Tarn

Status: accepted; P1 through P6 implemented. Delivered as a series of 0.5.x patches; no new minor.

## Evidence

Tarn 0.5.0 cannot build a kernel:

- The backend only emits Linux/macOS executables. Cranelift is built with
  `host-arch`, so the target is always the compiling machine.
- Generated code calls `malloc`, libc and the Tarn runtime (≈165 sites in
  `compiler/backend/src/codegen.rs`). A kernel has none of these below it.
- No inline assembly or CPU intrinsics (interrupts, control registers, `hlt`).
- No `volatile` loads/stores for MMIO.
- No control of entry point, sections or linker script.
- No interrupt calling convention.

What already helps: raw pointers `*T`/`*mut T`, `unsafe` blocks and functions
(ADR 0047), `extern` FFI, and a Cranelift backend that writes object files
without C.

## Decision

Add a `freestanding` profile and grow it in small, independently shippable
patches. Each patch is useful on its own (embedded, bootloaders, a C-free
runtime) and never weakens the safety guarantees of hosted Tarn: everything
new is reachable only through `unsafe` or the freestanding profile.

Goal: a Tarn `kmain` boots under QEMU through Limine, prints over the serial
port, handles a timer interrupt and allocates from its own heap.

## Patch series

Each patch lands with tests, docs and a changelog entry. Ordering is by
dependency; numbering follows whatever 0.5.x is free when it ships.

### P1 — Explicit target triple (`--target`) — implemented

- `tarn build --target {x86_64,aarch64}-{linux,macos,none}`; default stays host.
- Cranelift: replace `host-arch` with explicit `x86`/`arm64` ISA features;
  build the `TargetIsa` from the triple instead of `cranelift-native`.
- `*-none` emits a relocatable ELF object only (no link step yet).
- Tests: cross-emit both arches from one host; verify ELF headers.
- Done: `tarn_backend::Target`, `emit_object_for`, `build_object`; the
  backend's `cfg!` checks on OS/arch became target queries. Host builds keep
  `cranelift-native` CPU detection; other targets use the baseline ISA.
  `compiler/backend/tests/targets.rs` checks all six headers.

### P2 — Freestanding profile — implemented

- Disables the hosted runtime: no `main` shim, no libc, no stdlib modules
  that need an OS (fs, net, process, tasks). Using one is a compile error with
  a precise diagnostic, not a link error.
- `core` subset that stays available: integers, slices, `Option`/`Result`,
  ownership, panics routed to a user-provided `#[panic_handler]`.
- Codegen: every runtime call site goes through a table of required symbols;
  in freestanding the table is empty or user-supplied. Audit the 165 sites.
- Tests: a freestanding object has zero undefined symbols outside the
  declared set (`nm -u` check).
- Done: the profile is implied by `--target *-none` rather than a separate
  flag. Runtime functions are now declared on first use (hosted objects
  also stop importing ~150 unused symbols). A freestanding object exports
  `tarn_main` instead of the libc `main` shim and may import only
  `tarn_rt_panic`, `tarn_rt_panic_at`, `tarn_rt_fault` and `extern "C"`
  functions; anything else is rejected with the Tarn feature that needs it
  (`print`, heap strings/vectors, captured closures, async, OS services).
  Diagnostics carry no source span yet. P6 adds the way to write the panic
  handler in Tarn: `#[export("tarn_rt_panic")]` (a separate `#[panic_handler]`
  was not needed).

### P3 — User allocator — implemented

- The program provides the allocator; the object imports it only if the
  program uses the heap. Without the symbols, the link fails.
- Lines up with the C-free runtime work: the hosted runtime becomes one
  implementation of the same interface.
- Tests: heap program (strings, vectors, closure environment) compiles for
  both `*-none` targets and imports only the contract.
- Done: the contract grew by `tarn_alloc(size) -> ptr` (16-byte aligned, null
  on exhaustion), `tarn_free(ptr)` and `tarn_realloc(ptr, size) -> ptr`
  (`ptr` may be null); malloc-style, no size or alignment is passed to
  `tarn_free` because strings and closure environments do not record theirs.
  A bump allocator may ignore `tarn_free`. The heap runtime operations
  (`string`, `string_utf8/add/compare/range`, `drop_string`, `env_alloc/free/drop`,
  `vec_grow/extend`) are emitted as Cranelift functions inside the freestanding
  object (`compiler/backend/src/heap.rs`), mirror `runtime/native.c`, and
  fault via `tarn_rt_fault` on overflow or a null allocation. `drop_string`
  recognizes static literals by their address range in the object instead of
  linker section symbols. The helpers were run on the host with a malloc
  based harness (all allocations freed); no automated execution test yet
  (P8 boots under QEMU). Limitations: no custom alignment above 16; no
  `#[global_allocator]` attribute; the symbols are plain link-time names that P6
  lets a program define with `#[export("tarn_alloc")]` and friends.

### P4 — Volatile and memory intrinsics — implemented

- `ptr.read_volatile()`, `ptr.write_volatile(v)` (unsafe), lowered to
  Cranelift loads/stores with `MemFlags` that forbid elision/reordering.
- `memcpy`/`memset`/`memmove` emitted in Tarn for freestanding (no libc).
- Compiler fences: `atomic.fence(order)`.
- Tests: IR inspection proves volatile ops are not merged or removed.
- Done: the API follows the existing `ffi` helpers instead of pointer methods:
  `unsafe ffi.read_volatile(p *T) T`, `unsafe ffi.write_volatile(p *mut T, v T)`
  (`T: Copy` scalar, bool, float or raw pointer) and safe `ffi.fence()`. The
  checker treats them like `ffi.read`/`ffi.write` (E3071 outside `unsafe`).
  Cranelift has no volatile `MemFlags`: its alias analysis forwards stores to
  loads and merges repeated loads of plain memory operations whatever the
  flags. Volatile accesses are therefore emitted as `atomic_load`/
  `atomic_store`, which that analysis treats as memory fences and which Tarn's
  IR passes already keep as opaque intrinsic calls. `ffi.fence()` is
  Cranelift `fence` (`mfence`, `dmb ish`); there is no ordering parameter.
  Tests disassemble the object: two writes and two reads of one address give
  exactly `stlr, ldar, ldar, stlr` on aarch64 and two `mfence` plus two loads
  on x86_64. Limitations: volatile access is sequentially consistent, so each
  x86_64 volatile store costs an `mfence`; no sub-word-misaligned or
  aggregate volatile access; no weaker or compiler-only fence (P5 can add
  `dmb`/`dsb` stubs).
  Memory operations: Cranelift never emitted `memcpy` itself and Tarn code
  never calls it for aggregates, but copies larger than 256 bytes were
  unrolled into thousands of moves (30 s to compile a 16 KB struct in a debug
  build). They are now a word loop (also on hosted targets), so no libc
  `memcpy`/`memmove`/`memset`/`memcmp` is imported by freestanding objects (a
  test checks a large struct move). The loop assumes source and destination are
  identical or disjoint, which holds for Tarn aggregate assignments. User code
  that declares `extern "C" fn memcpy` (as `ffi.copy_bytes` does) still imports
  it, by its own choice.

### P5 — CPU intrinsics (instead of free-form `asm`)

Cranelift has no inline assembly. Rather than add a general `asm!`, expose
typed intrinsics in `arch.x86_64` / `arch.aarch64`, all `unsafe`:

- x86_64: `in8/16/32`, `out8/16/32`, `cli`, `sti`, `hlt`, `read_cr0/2/3/4`,
  `write_cr3`, `lgdt`, `lidt`, `invlpg`, `rdmsr`, `wrmsr`, `pause`.
- aarch64: `msr`/`mrs` for a whitelist of system registers, `wfi`, `isb`,
  `dsb`, `dmb`, `eret`.
- Implementation: each intrinsic is a tiny hand-assembled stub object linked
  in (bytes checked into the repo, verified by a test), called with the
  system ABI. Later patches can inline them if Cranelift gains support.
- Safety: each intrinsic documents its precondition; the type checker already
  requires `unsafe` for the call.

- Done: modules are flat stdlib names (`arch_x86_64`, `arch_aarch64`; the
  module system has no nesting), each function an `unsafe fn` wrapping a
  private `extern "intrinsic"`. x86_64: `in8/16/32`, `out8/16/32`, `cli`,
  `sti`, `hlt`, `pause`, `rdtsc`, `read_cr0/2/3/4`, `write_cr0/3/4`,
  `invlpg(u64)`, `rdmsr`, `wrmsr`, `lgdt(*u8)`, `lidt(*u8)`. aarch64: `wfi`,
  `wfe`, `isb`, `dsb_sy`, `dmb_sy`, `irq_enable/disable`, and
  `read_`/`write_` for `daif`, `vbar_el1`, `ttbr0/1_el1`, `tcr_el1`,
  `mair_el1`, `sctlr_el1`, `esr_el1`, `far_el1`, `elr_el1`, `spsr_el1`,
  `cntv_ctl_el0`, `cntv_tval_el0` plus read-only `currentel`, `cntfrq_el0`,
  `cntvct_el0`. Instead of a separate stub object, the bytes are defined as
  Local functions inside the emitted object with Cranelift's
  `define_function_bytes` (`compiler/backend/src/arch.rs`), declared on first
  use like the runtime table, so a program that uses none carries none and the
  object still imports only the contract. Stubs are never exported and use
  only caller-saved registers (x86_64 rax/rcx/rdx, aarch64 x0). The `arch.rs`
  tables hold bytes and the assembly they encode; a test emits every stub for
  both architectures, disassembles with the host LLVM `objdump` and compares
  (skipped without `objdump`). Using the other architecture's module is a
  build error (`tarn check` has no target, so it surfaces at `tarn build
  --target`); outside `unsafe` is E3071. A hosted run of `pause`/`rdtsc` (x86_64)
  or `cntvct_el0` (aarch64) checks the call ABI. Limitations: each intrinsic
  is a real call (no inlining, so a `cli` is separated from the next
  instruction by a `ret`); the intrinsics are not compiler memory barriers
  (use `ffi.fence()`/volatile access); no `eret` and no `write_cr2` (P7 builds
  trampolines from stubs); no `cpuid`, `xsave`, `swapgs`, SIMD or MSR whitelist
  (any `u32` MSR is accepted); aarch64 system registers are limited to the
  whitelist and values are not validated; the encodings follow the
  Intel SDM and Arm ARM and are checked only by disassembly, not on hardware
  (P8 boots them under QEMU).

### P6 — Entry point, exported names, sections, statics and linking — implemented

- `#[export("name")]`, `#[section(".name")]`, `#[align(n)]` attributes and
  `static` items; `tarn build --linker-script kernel.ld [--entry sym]`.
- Tests: the freestanding contract written in Tarn leaves zero undefined
  symbols; sections, alignment, symbol kinds, static bytes and relocations are
  read back from the ELF; linking runs a real linker when one exists.
- Done: user guide in [docs/freestanding.md](../freestanding.md).
  - **Syntax and why.** Tarn had no attributes, `static` items or mutable
    globals. The smallest forms that fit the grammar are `#[name]` /
    `#[name("text")]` / `#[name(16)]` on the line before a function or static
    (lexer token `#`; E1023 for malformed or misplaced ones) and `static` /
    `static mut` (a contextual keyword, so no existing identifier named
    `static` breaks; the item is `static NAME Type [= value]`, matching
    `const NAME Type = value`). Attributes beat a keyword prefix such as
    `export "x" fn` because three independent properties (symbol, section,
    alignment) combine on one item, and they are the form kernel authors expect.
    The earlier plan's `#[entry]` and `#[no_mangle]` and `#[repr(C)]` were not
    needed: `#[export]` on a function is the entry (the linker script's
    `ENTRY(kmain)` or `--entry` names it), exported names are always
    unmangled, and struct layout was already C layout (declaration order,
    natural alignment; ADR 0047 types), now also guaranteed for statics.
  - **Exports.** `#[export]` generates a C-ABI adapter that calls the ordinary
    Tarn body (the body keeps Tarn's private ABI and all its checks). Parameters
    and result reuse the `extern "C"` scalar set (integers, floats, `bool`,
    raw pointers, `void`); anything else is E3086 at `tarn check`. Exported
    functions are roots of monomorphization, so a program needs no `main` on
    `*-none`; `tarn_main` is emitted only when `main` exists. A program may
    define the contract (`tarn_rt_fault`, `tarn_rt_panic`, `tarn_rt_panic_at`,
    `tarn_alloc`, `tarn_free`, `tarn_realloc`) on `*-none`: the signature must
    equal what generated code calls (checked, with a message showing both), and
    the object then imports nothing. Other `tarn_*` names and hosted `main`
    are rejected. Tests define the panic handler and a bump allocator over a
    static arena in Tarn, for both architectures.
  - **Sections.** Data uses cranelift-object's custom sections (any ELF
    section name, flags from mutability). Functions: cranelift-object can give
    each function its own section but not name it, and the `object` writer
    keeps the name private, so `#[section]` on any function turns on
    one-section-per-function and `backend/src/elf.rs` renames sections by
    appending names to a copy of `.shstrtab` at the end of the finished file
    (`.text.<symbol>` for the rest, `.rela.<name>` for relocation sections).
    Nothing else moves. Without `#[section]` the object is unchanged. Mach-O
    targets reject `#[section]`.
  - **Statics.** `static` holds integers, floats, `bool`, raw pointers, fixed
    arrays and non-generic structs of those, initialized at compile time (the
    `const` evaluator plus struct/array literals, `ffi.null()`,
    `ffi.from_address(n)` and the name of another static, which becomes a
    relocation) or zero-filled (`.bss`). Ordinary values keep the 64 KiB and
    4096-element limits; statics use `layout::static_layout`, without them. A
    static's name is its address (`*T`, `*mut T` for `static mut`), so
    ownership stays sound by construction: no reference or loan to static
    storage can exist, every access is an `unsafe` raw-pointer read or
    write, and races and aliasing are the author's (documented). Immutable
    statics go to read-only memory. All statics are emitted even if unused.
    No enums, strings, generics or functions in initializers (E3085).
  - **Linking.** `--linker-script` (with `--entry`) is valid only for `*-none`
    and runs an existing ELF linker: `TARN_LD`, `ld.lld`, a cross GNU `ld`, or
    the host `ld` on a same-architecture Linux host, as
    `-nostdlib -static -T script [-e entry] -o out object`. The default entry is
    the script's `ENTRY`, else `tarn_main` when `main` exists. Without a
    linker the error names what to install; Tarn bundles and downloads none.
    A built-in ELF linker was not written: it would duplicate lld for the
    one feature (linker scripts) that makes it worth having.
  - **Verification limits.** This machine had no ELF linker, so the real link
    test (`linking_a_kernel_with_an_elf_linker`, which checks `ET_EXEC`, the
    entry address and the request bytes in the image) skips here; the
    invocation, entry selection, failure handling and publication are
    covered with a recording stand-in linker. The link was not run
    against lld or GNU ld by the author of this patch.
  - **Limitations.** No function addresses in static initializers (an IDT
    must be filled at run time, P7 adds handler addresses); no field-access
    syntax on statics (offsets by address arithmetic); same-named
    sections from several items stay separate input sections; no `#[used]`
    for functions, no `#[naked]`, no `#[link_section]` on locals; no
    `--gc-sections`; a static's alignment above the section's is raised for
    the section, not page-aligned in the image unless the script says so;
    aarch64 data relocations are untested beyond the object (no linker);
    the formatter prints `static mut NAME[16]u8` (the same spacing it already
    gives struct fields of array type).

### P7 — Interrupt handlers

- `#[interrupt] fn timer(frame: *InterruptFrame)`: generated prologue/epilogue
  that saves all caller- and callee-saved registers and returns with `iretq`
  (x86_64) or `eret` (aarch64). Built from P5 stubs plus a trampoline.
- Tests: QEMU test where a software interrupt runs a handler and returns.

### P8 — Example kernel and CI

- `examples/kernel-x86_64`: Limine boot, serial `println`, GDT/IDT, PIT/APIC
  timer, bump-then-free-list heap, panic over serial.
- CI job: build the kernel and boot it in `qemu-system-x86_64 -nographic`;
  pass when the serial output contains the expected line.
- aarch64 (`qemu -M virt`, PL011 UART) follows once x86_64 is green.

## Alternatives

- **Emit C and use a cross C compiler.** Fast to start, but contradicts the
  C-free runtime goal and moves safety guarantees into a toolchain Tarn does
  not control.
- **Switch the backend to LLVM.** Gives inline asm and every target for free,
  but costs compile time, binary size of the toolchain and a large rewrite.
  Revisit only if P5's intrinsic approach proves insufficient.
- **General `asm!`.** Unverifiable by the compiler; typed intrinsics keep
  the "compiler proves it" stance and cover what a small kernel needs.

## Consequences and risks

- The runtime-call audit in P2 is the largest piece of work and also benefits
  the C-free runtime: it makes every runtime dependency explicit.
- Stub objects in P5/P7 are binary blobs; they must be reproducible from a
  documented source and checked by tests.
- Panics, bounds checks and overflow checks stay on in freestanding; the
  handler decides what to do. No safety check is removed to fit the target.
- Async/tasks remain hosted-only until a kernel scheduler exists.
