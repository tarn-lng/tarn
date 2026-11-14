# ADR 0101: Freestanding targets, toward a kernel written in Tarn

Status: accepted; P1, P2 and P3 implemented. Delivered as a series of 0.5.x patches; no new minor.

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
  Diagnostics carry no source span yet; `#[panic_handler]` in Tarn waits for
  P6 (exported symbol names).

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
  `#[global_allocator]` attribute, the symbols are plain link-time names
  until P6 adds exported names.

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
