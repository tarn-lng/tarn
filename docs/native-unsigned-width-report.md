# Automatic unsigned division-width optimization

## Implemented

The compiler now uses 32-bit unsigned division/remainder for an i64 operation when the immediate CLIF definitions prove that both operands fit u32. The result is zero-extended back to i64. This is enabled automatically for ordinary builds, with no source changes or user-facing optimization flag.

Eligible facts include low-bit masks, unsigned extension, small constants and a previous unsigned remainder with a bounded constant divisor. Unknown values and wide divisors retain their existing code. All ordinary overflow and divide-by-zero checks remain; signed arithmetic is unchanged.

Design and proof: [ADR 0082](adr/0082-proven-unsigned-division-width.md). Implementation: [integer_narrow.rs](../compiler/backend/src/integer_narrow.rs); integration changes only the unsigned division/remainder emission paths in codegen.rs.

## Performance

Local Linux x86_64, 100 million LCG/checksum iterations, runtime command-line inputs, nine randomized rounds on logical CPU 2. Both Rust variants use opt-level=3, target-cpu=native and panic=abort. Tarn uses its existing native speed optimization. The two Tarn compiler executables differ by this optimization; the pre-existing frontend/callback changes are included in both.

| Variant | Median ms |
|---|---:|
| Tarn before | 336.984 |
| Tarn after | 305.762 |
| Rust with overflow checks | 227.866 |
| Rust without overflow checks | 126.773 |

Tarn's duration fell by 9.26%. Its ratio to checked Rust fell from 1.479 to 1.342. This is a workload-specific runtime result, not universal parity, a compiler-build speed claim or an AWS cost measurement.

The emitted x86_64 loop uses a 32-bit reciprocal remainder sequence in place of the longer 64-bit sequence. Input multiplication/addition and accumulator overflow checks remain. No profiling-guided or unsafe arithmetic mode was added.

The isolated comparison validated 64 small program/input cases against a Python oracle and three overflow-abort cases before timing. Every long-run checksum matched. Source text, commands, exact compiler/binary hashes, host load and individual samples are in [compiler-optimization.json](../../tools/serverless/results/compiler-optimization.json).

## Validation

- Two CLIF-level tests cover constants, width boundaries, block parameters, masks, unsigned/signed extensions, zero upstream divisors and the requirement to prove both operands.
- Two native integration tests cover runtime u64 inputs and wide/bounded divisors, including u32::MAX, 2^32 and u64::MAX. Narrowed divide/remainder by zero still abort.
- Workspace suites were relaunched sequentially after the earlier external SIGTERM interruption. No performance benchmark or second workspace suite competed with either recovery run.

| Workspace run | Passed | Failed | Ignored | Wall seconds |
|---|---:|---:|---:|---:|
| 1 | 340 | 0 | 1 | 448.45 |
| 2 | 340 | 0 | 1 | 431.41 |

Command: `cargo test --workspace --offline --locked`. No build warnings were emitted. Existing parser/type-checker scaling changes, E1022 handling, incremental lexer columns and the CLI/LSP compiler-stack changes were present during both full runs. The comparison to the previously reported roughly six minutes is observational: test counts, cache/host state and timing conditions were not identical.

Logs: [run 1](../../tools/serverless/results/compiler-recovery-tests-1.log), [run 2](../../tools/serverless/results/compiler-recovery-tests-2.log), [structured validation](../../tools/serverless/results/compiler-validation.json). The old interrupted compiler-optimized-tests.log was replaced with the completed second-run log.

## Compatibility and limits

The optimization is backend-local; the existing frontend edits remain intact. Comparing codegen.rs against the saved pre-optimization working copy shows exactly the two intended unsigned-operation substitutions. Changes were left uncommitted.

The tested compiler is available at `target/debug/tarn`. This working tree reports Tarn 0.0.8, while the previously installed command reported 0.0.9; the installed toolchain was not overwritten with an older-version working-tree build. Tests and before/after measurements use explicit compiler paths. Compiler dev/release profiles do not change the existing native `opt_level=speed` setting for emitted Tarn programs.

Native execution was validated on x86_64. The lowering uses portable CLIF i32/i64 operations, but this change has not been executed on ARM64 in this session. Range facts across loops/memory, scalar promotion and more efficient checked constant multiplication remain candidates for future profiling and separately tested changes.

## Further scalar optimization

The later constant-guard, bounded loop-peeling and canonical modulo work is documented in [the scalar optimization report](native-scalar-optimization-report.md). It includes the final full-suite result and the checked/default Rust comparisons.
