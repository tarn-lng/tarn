# ADR 0112 — CPU random-number instructions with success flags

Status: accepted (Cairn phase 5B; Tarn 0.5.4).

## Evidence

Cairn's pinned 0.5.3 compiler rejects `arch_x86_64.rdrand64()` and
`arch_aarch64.rndr()` with E2005. Its entropy design requires a complete
64-bit word plus the instruction's independent success flag. Reading flags
in a later Tarn call is unsound: wrapper code can change them. Packing a flag
into the word would discard an entropy bit, and executable-byte workarounds
belong outside a kernel written against typed CPU intrinsics.

## Decision

Each architecture exposes a copyable `RandomResult { value u64, success bool }`.
All instruction and feature-query APIs are unsafe, following ADR 0101:

- x86_64: `rdrand64()` and `rdseed64()`, plus `has_rdrand()` and
  `has_rdseed()` using CPUID leaf 1 ECX[30] and leaf 7 subleaf 0 EBX[18].
  Helpers check the maximum basic leaf before querying it.
- aarch64: `rndr()` and `rndrrs()`, `read_id_aa64isar0_el1()`, and
  `has_rng()` checking ID_AA64ISAR0_EL1.RNDR[63:60] = 1. Reading the ID
  register requires an execution level/OS configuration that permits it.

Each private intrinsic takes an output pointer to one initialized local u64
and returns a boolean. The x86 template executes RDRAND/RDSEED, stores the
word with MOV (which preserves CF), then captures CF with SETB. The ARM
template executes MRS RNDR/RNDRRS, stores the word with STR (which preserves
NZCV), then captures success with CSET NE (Z = 0). Instruction, store and flag
capture are one contiguous template. No unrelated code or call can intervene.
The public wrapper returns ordinary Tarn fields, like the existing CpuidResult.

The RNG sequences exceed ADR 0104's fixed call slots and remain local calls;
the single-instruction ID read is inlined when the existing mechanism allows.
Only ABI caller-saved registers are touched. No new backend aggregate ABI,
inline-assembly language, allocator, dependency or runtime hook is introduced.

Callers must check features before executing an RNG instruction and must
discard `value` when `success` is false. A successful zero is still success.
There are no implicit retries, fallback bytes, health tests, readiness claims,
entropy credit or CSPRNG policy in these primitives. Virtualization can trap
instructions even when a feature is advertised; execution context remains the
caller's unsafe responsibility. Kernel entropy policy belongs to Cairn.

## Validation

Both architecture fixtures include every new operation in the existing stub
disassembly and freestanding-contract tests. Negative checks require unsafe.
QEMU maximum-CPU gates exercise the feature queries and both instructions with
bounded retries; non-RNG CPU gates check feature absence without executing an
unsupported instruction. They prove the interface and instruction paths, not
the statistical quality or security of the host's entropy source.

Targeted validation passed 65 tests: 47 target/disassembly/unsafe checks,
11 x86_64 QEMU tests and 7 aarch64 QEMU tests, including RNG-present (`max`)
and RNG-absent (`qemu64` / `cortex-a72`) paths. Failed instruction values are
never consumed by those gates; successful words retain their full u64 width.

Baseline maintenance discovered while running the required full suite is
included: the soundness audit's fixed-bug reproducer now explicitly expects
E4007; the interrupt type snapshot is refreshed for current architecture
source locations; both mutation harnesses cap their private worker pools at
seven, matching AGENTS.md rather than using all detected CPUs. The initial
driver mutation run was stopped to enforce that limit and is rerun in final
acceptance. No language or ownership rule is weakened by these repairs.

Final full validation on 2026-11-16:
`PATH="$PWD/tools/ci/macos:$PATH" RUST_TEST_THREADS=6 nice -n 10 cargo test
--offline --no-fail-fast` exited 0: **451 passed, 0 failed, 3 existing ignored**,
with no compiler warnings. The capped driver and native mutation corpora,
documentation examples, type snapshots and both RNG QEMU gates passed.
`git diff --check` was clean. Logs are retained under `target/phase-5-rng/`.
`nice -n 10 cargo build --offline --release -p tarn -p tarn-lsp` also passed
without warnings; both release binaries report 0.5.4. They are rebuilt after
the commit and reinstalled together so their build identities name the patch.

## Sources

- [Intel DRNG Software Implementation Guide](https://www.intel.com/content/www/us/en/developer/articles/guide/intel-digital-random-number-generator-drng-software-implementation-guide.htm):
  feature leaves, CF success and bounded-retry guidance.
- [Arm RNDR register specification](https://developer.arm.com/documentation/ddi0601/latest/AArch64-Registers/RNDR--Random-Number):
  RNDR/RNDRRS accesses and NZCV success semantics.
- [Arm Neoverse N2 cryptographic extension reference](https://documentation-service.arm.com/static/61b88976503be65e4e3ae9bb):
  ID_AA64ISAR0_EL1.RNDR feature field.
