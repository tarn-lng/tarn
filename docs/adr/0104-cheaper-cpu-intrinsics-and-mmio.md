# ADR 0104: Cheaper CPU intrinsics and MMIO access

Status: accepted (Cairn T3).

## Problem and research

ADR 0101 P5 emits every CPU intrinsic as a C ABI call to a hand-assembled
local function. Cairn needs `invlpg`, CR3 moves and port I/O in hot loops.
P4 also gives every x86_64 volatile store an unnecessary `mfence`.

The implementation was checked against the installed **Cranelift 0.125.3**
sources, rather than the newer online API. Relevant upstream entry points:
[Context](https://docs.rs/cranelift-codegen/0.125.3/cranelift_codegen/struct.Context.html),
[CallConv](https://docs.rs/cranelift-codegen/0.125.3/cranelift_codegen/isa/enum.CallConv.html).
The decisive source files in that crate are `inline.rs`, `isa/call_conv.rs`,
`isa/{x64,aarch64}/abi.rs`, `isa/{x64,aarch64}/lower.isle`,
`ir/memflags.rs`, and `inst_predicates.rs`.

- Native CLIF `debugtrap` emits `int3` on x86_64 and resumes afterwards.
  `fence` emits `mfence` / `dmb ish`; the latter is not `dmb sy` or `dsb sy`.
  There is no CLIF pause, port-I/O, control-register, system-register or
  arbitrary inline-byte instruction in this version's generated instruction
  builder. Internal ISA instructions/ISLE rules are not a public custom-op
  extension mechanism; adding operations would require a Cranelift fork.
- `get_pinned_reg`/`set_pinned_reg` access the ISA's one reserved general
  register, not arbitrary registers, CR3, MSRs or system registers. Reserving
  that register does not solve instruction emission.
- `Context::inline` accepts a legalized CLIF `Function`, not machine bytes.
  It therefore cannot inline a `define_function_bytes` template.
- `CallConv` has Fast, Cold, Tail, SystemV, WindowsFastcall, AppleAarch64,
  Probestack and Winch. There is no preserve-all or custom clobber mask in a
  signature. x64 `get_regs_clobbered_by_call` gives Fast/Cold/Tail/Probestack
  the same ordinary SysV clobbers; Winch clobbers more. ARM similarly uses
  AAPCS clobbers. Tail supports tail calls, not spill-free calls inside loops.
  The special stack-probe emission path is not a general typed intrinsic ABI.
- CLIF has only sequentially consistent atomic loads/stores here and no
  volatile `MemFlags`. Plain loads can be merged/forwarded; disabling an
  optimization globally would lose performance and would not establish a
  durable per-access volatile contract.

## Decision

Use native `debugtrap` for x86_64 `int3`. For other CPU operations, first
compile the existing typed ABI call, then replace a **relocation-identified**
direct call with the template body, omitting its `ret`, if it fits in the
call's existing slot. x86 has five bytes (`e8 rel32`); ARM has four (`bl`).
Fill unused x86 bytes with NOPs and remove only that relocation. Validate the
call opcode, relocation kind/addend, and bounds. An unrecognized call form
or a longer body retains its normal call. Malformed recognized encodings
fail compilation. There is no disassembly, arbitrary byte search, size
change, branch repair or dependency on host ISA features in this pass.

| Architecture | Native CLIF | Fixed-size substitution | Still a call |
|---|---|---|---|
| x86_64 | `int3` | all other current operations except the three at right | `rdtsc`, `rdmsr`, `wrmsr` |
| aarch64 | none of the current CPU API | every current operation | none for the recognized direct call form |

`out16` uses `mov edx, edi; xchg eax, esi; out dx, ax`: the one-byte
accumulator exchange makes the body fit five bytes. ESI is caller-saved.
Every template is straight-line, has no PC-relative operands, and uses only
ABI caller-saved registers. The compiler has already arranged ABI arguments
and results, so replacing the call cannot increase its register clobbers.
ARM no longer writes LR at the substituted instruction; preserving an extra
register is harmless. No template needs the call's return-address stack
slot. Normal and interrupt return addresses outside the slot are unchanged.

This is deliberately **partial machine-level inlining**. Call/return overhead
is gone, but ABI argument moves, compiler-generated frames and conservative
register preservation remain. It does not promise spill-free arbitrary
loops. The three remaining x86 stubs are already frameless and use only
registers, but their callers may still spill live values. All declared local
templates must still be defined for Cranelift's object module, even when all
their calls were substituted; object size is not the primary improvement.
Only templates the program uses are declared. `int3` does not need an ABI
call at all, though its declared template is retained with the others.

Padding and compiler-generated code mean separate Tarn intrinsic calls do
not promise adjacent machine instructions. In particular, `sti`'s interrupt
shadow can end on a NOP; `sti(); hlt()` is not an atomic enable-and-sleep API.
This was already true with the previous call/ret lowering. A future combined
intrinsic would need its own audited semantics.

## Cheaper x86 volatile stores

Keep CLIF atomic loads/stores for compiler ordering and exactly-once effects.
Tag only `ffi.write_volatile` atomic-store instructions with a unique reserved
backend source location. The source location follows the instruction through
codegen. For each tagged x86 machine range, check that the range ends in the
expected `0f ae f0` (`mfence`) and replace those three bytes with `0f 1f 00`
(a NOP that does not access memory). Unexpected lowering fails compilation;
no untagged fence, ordinary atomic, or explicit `ffi.fence` is changed.
The reserved source-location namespace is internal; real source positions
are not currently attached to CLIF. Future debug-info work must preserve a
separate tag map or explicitly reserve this namespace.

Cranelift optimizes and allocates the original atomic operation, which is
neither removable nor mergeable and has memory-fence semantics for alias
analysis and instruction scheduling. Weakening only the final hardware
sequence therefore cannot undo compiler volatility. Naturally aligned
8/16/32/64-bit scalars and pointer/float bitcasts retain exactly one access.
The change adds no synchronization guarantee: Tarn volatile operations are
not a substitute for atomics.

On x86, plain scalar MOV accesses are appropriate for UC MMIO; they require
no sequentially consistent StoreLoad fence after every write. The kernel
must select the correct PAT/MTRR memory type and follow its device's protocol.
The [Intel SDM memory-ordering discussion](https://cdrdv2-public.intel.com/671294/252046-sdm-change-document.pdf)
specifies strong ordering for UC regions with no speculative device accesses.
This change does not make write-combining or normal-memory/device ordering
universally safe, drain posted device writes, or turn volatile into a cache
configuration API. Use explicit `ffi.fence()` when needed. It remains a full
compiler and hardware fence. aarch64 is unchanged (`ldar`/`stlr`, `dmb ish`);
weakening its device accesses requires separate architecture-specific work.

Both substitutions preserve offsets, so existing branches, relocations,
static function addresses and interrupt trampolines stay valid. There is no
dependency upgrade, free-form assembly API, or change to the frontend's
unsafe/target checks.

## Evidence and measurements

Baseline: commit `81e4852b53a16f2fc2b56d3d33bc78ccf470985e`. Host: macOS x86_64,
Intel Core i5-1135G7 at 2.40 GHz, Rust 1.99.1, Cranelift speed optimization.
Objects were built with `tarn build tests/freestanding/<fixture>.tarn
--target <target> -o <unique-path>` before and after, then disassembled with
host LLVM `objdump -d --no-show-raw-insn`. Counts below are for `main`, not
the retained template definitions.

| Fixture | Before | After |
|---|---|---|
| `cpu_hot_loop_x86_64`, calls per iteration | 4 | 0 |
| same, loop spill memory accesses | 0 | 0 |
| same, object bytes | 2032 | 1840 |
| `cpu_hot_loop_aarch64`, calls per iteration | 4 | 0 |
| same, object bytes | 2072 | 1880 |
| `volatile_order`, volatile stores / loads | 2 / 2 | 2 / 2 |
| same, implicit `mfence` instructions | 2 | 0 |
| same, object bytes | 1384 | 1384 |

The x86 loop retains its frame pointer; ARM retains its 16-byte FP/LR frame.
The object-size reductions above are relocation records, not shorter call
slots. A program using all x86 intrinsics retains exactly three direct CPU
calls (plus unrelated fault paths).

The new native microbenchmarks perform ten million iterations each. Seven
alternating before/after process executions, also alternating which version
runs first, checked every output. Median elapsed time includes startup and
printing. Other compiler tests ran concurrently on this uncontrolled host:
these observations are **not an isolated latency measurement or a performance
guarantee**, especially for `pause`, whose ranges overlap substantially.

| Benchmark | Before median ms | After median ms |
|---|---:|---:|
| `cpu-pause.tarn` | 801.281 | 731.497 |
| `volatile-store.tarn` (RAM) | 149.737 | 11.551 |

The pause ranges were 644–1304 ms before and 656–1037 ms after; the volatile
ranges were 124–389 ms before and 9–366 ms after. These measurements establish
neither real MMIO latency nor privileged-instruction latency. Reproduce with
the native benchmark runner and compiler builds from the baseline and this
patch; [raw samples](../../benchmarks/native/intrinsics-2026-11-14.json) record
the paired runs and object observations.

Verification covers relocation/encoding rejection guards, both `*-none`
targets, x86_64 Mach-O hot loops, exactly-once repeated volatile stores and
unused loads, scalar widths/float/pointer bitcasts, explicit fences, the
remaining frameless stubs, and hosted unprivileged CPU/scalar execution.
Disassembly follows the existing optional-host-objdump pattern; it was
available and used for all observations here. QEMU 11.1.3 (TCG) executes the
example, panic, bounds-fault and page-fault kernels, plus a new test reloading
CR3, invalidating a page and exercising every port width at port 0x80.
The sixth QEMU-suite test checks the PVH note. No ARM execution is claimed.
All six fixtures in `benchmarks/native/run.py` built, ran and produced their
checked expected output after the patch.

Final validation on this host:

- `PATH="$PWD/tools/ci/macos:$PATH" cargo test --offline --no-fail-fast
  -p tarn_backend -p tarn -p tarn_driver`: 337 passed, zero failed, three
  existing ignored tests, no compiler warnings. The baseline backend/CLI run
  passed 284 tests with zero failures and three ignored tests; none of the
  listed known macOS failures occurred in either run.
- The final additions also passed separately: `cargo test --offline
  -p tarn_backend --test targets -- --nocapture` (41 tests), and
  `cargo test --offline -p tarn_backend --test kernel_boot -- --nocapture`
  (six tests, actual QEMU execution with the available ELF linker).
- `TARN="$PWD/target/debug/tarn" python3 benchmarks/native/run.py`: all six
  fixtures returned their expected output. `git diff --check` was clean.

## Limits and alternatives

The pass is intentionally coupled to the pinned Cranelift encoding. Tests
must be rerun when upgrading it; the guard rejects changed recognized
encodings. A future upstream volatile operation, inline ISA operation or
preserve-all ABI could remove this adaptation and its residual overhead.
Growing instruction slots would require moving code and repairing all
branches/metadata/relocations; a general binary inliner is outside this patch.
Vendoring Cranelift or switching to LLVM is disproportionate to this patch.
No claims are made about real-device latency, KVM, SMP or real hardware from
the hosted RAM microbenchmark and QEMU tests.
