# ADR 0101: Freestanding targets, toward a kernel written in Tarn

Status: proposed. Delivered as a series of 0.5.x patches; no new minor.

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

### P1 — Explicit target triple (`--target`)

- `tarn build --target {x86_64,aarch64}-{linux,macos,none}`; default stays host.
- Cranelift: replace `host-arch` with explicit `x86`/`arm64` ISA features;
  build the `TargetIsa` from the triple instead of `cranelift-native`.
- `*-none` emits a relocatable ELF object only (no link step yet).
- Tests: cross-emit both arches from one host; verify ELF headers.

### P2 — `#![freestanding]` / `--freestanding` profile

- Disables the hosted runtime: no `main` shim, no libc, no stdlib modules
  that need an OS (fs, net, process, tasks). Using one is a compile error with
  a precise diagnostic, not a link error.
- `core` subset that stays available: integers, slices, `Option`/`Result`,
  ownership, panics routed to a user-provided `#[panic_handler]`.
- Codegen: every runtime call site goes through a table of required symbols;
  in freestanding the table is empty or user-supplied. Audit the 165 sites.
- Tests: a freestanding object has zero undefined symbols outside the
  declared set (`nm -u` check).

### P3 — User allocator

- `#[global_allocator]`-style hook: the program provides `alloc`/`free`.
  Without one, heap-using code (Vec, String, Box) is a compile error in
  freestanding.
- Lines up with the C-free runtime work: the hosted runtime becomes one
  implementation of the same interface.
- Tests: bump allocator over a static array; Vec/String work.

### P4 — Volatile and memory intrinsics

- `ptr.read_volatile()`, `ptr.write_volatile(v)` (unsafe), lowered to
  Cranelift loads/stores with `MemFlags` that forbid elision/reordering.
- `memcpy`/`memset`/`memmove` emitted in Tarn for freestanding (no libc).
- Compiler fences: `atomic.fence(order)`.
- Tests: IR inspection proves volatile ops are not merged or removed.

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

### P6 — Entry point, sections and linking

- `#[entry]` names the symbol; `#[section(".text.boot")]`, `#[no_mangle]`,
  `#[align(n)]` for statics.
- `tarn build --linker-script kernel.ld` and a bundled `ld.lld` invocation
  (or a minimal built-in ELF linker for static, non-PIC output).
- Statics with fixed layout (`#[repr(C)]`) for GDT/IDT/page-table structures.
- Tests: produce an ELF whose entry and sections match the script.

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
