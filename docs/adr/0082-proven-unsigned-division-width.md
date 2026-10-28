# ADR 0082: definition-proven unsigned division width

Status: accepted for implementation.

## Context

The local serverless experiment exposed an unsigned remainder in a hot scalar
loop. Its dividend is already limited to 32 bits by `% 4294967296`, but Tarn
emits a 64-bit remainder by 1000. Cranelift optimizes the constant divisor,
yet the emitted full-width reciprocal sequence is longer than the 32-bit
sequence. Checked arithmetic and language-visible u64 values remain required.

The compiler already selects Cranelift `opt_level=speed`, inlines small direct
calls, and proves some arithmetic safety in typed IR (ADRs 0056/0057).
This change improves mechanical native representation; it does not add a
release mode, a new language type, or weaker arithmetic semantics.

## Decision

After the ordinary divide-by-zero guard, emit an i32 unsigned divide/remainder
and zero-extend its result to i64 when **both** i64 input values are known to
fit u32 from their immediate CLIF definitions. The accepted facts are:

- a constant whose unsigned bit pattern is at most u32::MAX;
- an unsigned extension from an integer lane of at most 32 bits;
- a bitwise AND with a constant mask at most u32::MAX;
- a successful unsigned remainder by a nonzero constant at most 2^32.

CLIF aliases are resolved. Unknown values, memory loads, block parameters,
signed extensions, and unrecognized definitions retain their original width.
The test does not recursively traverse a CFG or speculate about aliases,
loop invariants, overflow, ownership or provenance.

Only unsigned operations are changed. Signed division/remainder and their
MIN / -1 faults follow the existing code. The public value stays i64 and all
ordinary overflow, conversion, and divide-by-zero checks remain in place.
Narrow integer types other than i64 keep their existing lowering.

## Why this is safe

For x and y in [0, 2^32-1], with y != 0, unsigned division and remainder have
the same values at width 32 and width 64. Neither input loses a bit and the
result can be zero-extended exactly. A urem result is below its positive
divisor; a mask cannot introduce bits absent from the mask. These are facts
of the machine instructions already emitted, not inferred language contracts.
In particular, 2^32 is an allowed *upstream* remainder divisor because its
result fits u32, but it is not an allowed narrowed operand itself. Negative
encoded i64 constants are interpreted as unsigned bit patterns, so they do
not accidentally qualify as small constants.

## Validation

- CLIF-level tests cover unresolved parameters, constants at and above u32::MAX,
  high-bit and low-bit masks, unsigned versus signed extension, upstream
  remainder bounds, zero divisors, and the requirement to prove both operands.
- Native programs receive command-line values, preventing constant folding
  from replacing the operations under test. Cross-product cases cover zero,
  u32 boundaries, high u64 values, and u64::MAX with bounded and wide divisors.
  Outputs match Rust integer division/remainder for the same inputs.
- Native narrowed divide/remainder still abort when a dynamic divisor is zero,
  including a wide nonzero input whose upstream remainder becomes zero.
- The existing native boundary/fault, ownership, mutation and workspace tests
  remain required. Performance measurements must validate outputs and compare
  checked arithmetic separately from Rust's default release arithmetic.

## Tradeoffs and limits

The optimization is deliberately local and inexpensive. An equivalent range
held only in a loop phi or loaded from memory is not proven. Broader scalar
promotion, loop transformations and additional range proofs require their own
alias/fault-order evidence and tests. No speedup is promised for workloads
without an eligible division or remainder.

## Follow-up: canonical modulo at the register boundary

Unsigned i64 remainder by 2^32 is emitted explicitly as i32 truncation followed
by i64 zero extension. This is an exact identity for every u64 input, not a
claim that the divisor fits u32 or that signed remainder has the same rule.
Exposing this identity before later instruction selection lets narrow users
share arithmetic, instead of computing both i32 and masked i64 forms of the
same value. Native boundary tests include the full u64 input domain edges.
