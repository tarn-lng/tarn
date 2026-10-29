# Native scalar optimization: implementation and validation

## Outcome

The selected CPU kernel now outperforms checked Rust in two local 11-round comparisons, with unchanged source programs and retained overflow behavior. This is a workload-specific result. Rust without overflow checks is still faster, and the mixed PostgreSQL/CPU handler does not lead Rust in the application comparison.

## Automatic compiler changes

1. Unsigned word addition/multiplication by constants uses equivalent pre-operation overflow guards (ADR 0083). Both constant positions and high-bit constants retain their exact overflow domains.
2. Small scalar loops can peel one guarded conditional iteration. The existing IR range analysis then proves steady-state operations that cannot overflow; the initial input checks and unbounded accumulator checks remain (ADR 0084).
3. Unsigned remainder by 2^32 is represented explicitly as truncation/zero extension, allowing narrow consumers to share arithmetic (ADR 0082 follow-up).

All changes are automatic in ordinary native builds. No program edits, unsafe arithmetic mode, additional dependencies or benchmark-name/coefficient matching were introduced. Loop peeling is bounded and excludes references, projections, calls, resource/flag operations and async bodies. The executable IR is verified after transformation.

## CPU kernel: 300 million iterations

Runtime inputs, pinned logical CPU 2, 11 randomized rounds per pass. Rust uses opt-level=3, target-cpu=native and panic=abort, with overflow checks explicitly selected. Samples include startup/printing, amortized over the long loop. Every checksum matched.

| Variant | Seed 12345 median ms | Seed u32::MAX median ms |
|---|---:|---:|
| Tarn before all scalar optimizations | 812.592 | 843.135 |
| Tarn optimized | 463.641 | 480.251 |
| Rust with overflow checks | 526.738 | 546.759 |
| Rust without overflow checks | 325.412 | 335.182 |

Tarn's measured time fell by 42.94% and 43.04% relative to its original compiler. It used 11.98% and 12.16% less time than checked Rust. Tarn won 22/22 paired rounds. Executable hashes match between the two seed passes.

The passes use the same algorithm and arithmetic failure contract. The difference is that Tarn proves some steady-state checks unnecessary while keeping the unbounded first iteration guarded. It does not switch the source to wrapping arithmetic. Intermediate exploratory timing gains are not additive; the table above is the final common-baseline comparison.

Raw data: [first pass](../../tools/serverless/results/compiler-parity-final.json), [second seed](../../tools/serverless/results/compiler-parity-confirmation.json). Reproduction: [compare-codegen.py](../../tools/serverless/compare-codegen.py).

## Runtime-API handler comparison

The local emulator/PostgreSQL test ran 3 randomized rounds of 100 warm requests and 15 new-process observations per language/workload. Each complete pass contains 3,780 measured, checksum-validated responses. There was no AWS deployment.

The before/after default-stack passes kept byte-identical Go and Rust control executables. They used the same source handlers, DB image, data, query, driver versions, CPU affinity and sample counts. Host timings drifted: Go/Rust control medians also changed. Consequently, these application before/after values are observations, not a precise isolated compiler effect or AWS-cost prediction.

| Warm workload | Tarn before ms | Tarn after ms | Go before → after ms | Rust default before → after ms |
|---|---:|---:|---:|---:|
| noop | 0.410 | 0.449 | 0.496 → 0.425 | 0.425 → 0.351 |
| cpu | 2.640 | 1.289 | 1.649 → 1.411 | 1.167 → 0.988 |
| db | 0.693 | 0.699 | 0.929 → 0.656 | 0.726 → 0.640 |
| db_cpu | 2.648 | 1.580 | 2.123 → 1.755 | 1.621 → 1.268 |

A third pass retained the exact optimized Tarn/Go binaries and explicitly rebuilt Rust with overflow checks, native CPU targeting and panic=abort. It is a separately identified arithmetic-policy comparison, not a replacement for the default Rust results.

| Warm workload | Tarn ms | Go standard build ms | Rust checked/native/abort ms |
|---|---:|---:|---:|
| noop | 0.340 | 0.368 | 0.383 |
| cpu | 1.290 | 1.467 | 1.478 |
| db | 0.548 | 0.658 | 0.597 |
| db_cpu | 1.861 | 1.707 | 1.664 |

Tarn has the lowest median in the checked-Rust CPU-only handler case. Rust retains the lowest median in the mixed DB/CPU case. Drivers, JSON/HTTP implementations, register pressure and scheduling can affect application results; the kernel result is not a universal language ranking.

The optimized Tarn handler is 229,904 bytes, the same ELF file size as before (layout/alignment can hide text-size changes). The captured driver remains the immutable local registry PostgreSQL 0.3.0 source. Database-only and sub-millisecond differences are sensitive to emulator/host overhead.

Application evidence: [before](../../tools/serverless/results/serverless-before-optimizer/REPORT.md), [after/default Rust](../../tools/serverless/results/serverless-after-optimizer/REPORT.md), [checked Rust](../../tools/serverless/results/serverless-checked-rust/REPORT.md). Each directory includes raw per-request CSV, block statistics, exact binary hashes, versions and DB metadata.

## Overflow contract through the real handler loop

A CPU event with seed u64::MAX and zero iterations succeeded in all variants, confirming input parsing and zero-trip behavior. Reusing the handler with one iteration then made Tarn and checked Rust abort with SIGABRT. The original default Rust executable returned the wrapping result. This independently verifies the actual handler arithmetic-policy labels, not only the compiler flags.

[Contract observations](../../tools/serverless/results/serverless-overflow-contracts.json). This checks integer overflow behavior; it is not a claim that Rust loses memory safety in release mode.

## Tests and scope

Full command: `cargo test --workspace --offline --locked`: **345 passed, 0 failed, 1 ignored**, 0 warnings, 457.82 seconds. It ran without any of our performance benchmarks competing.

- Existing ownership, destruction, FFI/callback, mutation, arithmetic-fault, parser-depth and scaling suites passed with the pre-existing frontend changes retained.
- New constant-guard tests cover both operand positions and exact overflow transitions for ten constants, including 0, 1, powers of two and u64::MAX.
- New loop tests compare three recurrences with checked integer oracles across zero/one/many trips, extreme seeds and accumulator values. They require a checked prefix and proven steady state, validate the address-taken fallback/code-growth budget, and keep multiplication checks for a wide carried range where overflow really occurs.
- The existing differential CLI test compares behavior with and without TARN_NO_RANGE_PROOFS; that switch also disables peeling.

Validation: [summary](../../tools/serverless/results/compiler-scalar-final-validation.json), [full log](../../tools/serverless/results/compiler-scalar-final-tests.log), [loop tests](../../tools/serverless/results/scalar-loop-tests.log).

## Remaining work and rollout

Runtime application assembly still shows a stack-spilled checksum in the larger handler, unlike the smaller kernel. Investigating register pressure and allocations/copies in the mixed handler is a concrete next profiling target; this session does not attribute a measured percentage of the remaining gap to either.

Loops with branches, calls, references, array operations and async frames are deliberately outside the new peeling rule. Native execution was tested on x86_64; ARM64 execution and actual Lambda packaging/memory/concurrency/billing remain unmeasured.

The tested working-tree compiler is `target/debug/tarn` (version 0.0.8). The installed command previously reported 0.0.9, so it was not replaced by an older-version working-tree build. Set TARN to the explicit compiler path when reproducing the handler measurements. All edits remain uncommitted. The benchmark containers were stopped/removed; the existing project database was untouched.

Designs: [ADR 0082](adr/0082-proven-unsigned-division-width.md), [ADR 0083](adr/0083-checked-unsigned-constant-guards.md), [ADR 0084](adr/0084-bounded-scalar-loop-peeling.md).
