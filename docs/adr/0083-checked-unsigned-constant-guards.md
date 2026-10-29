# ADR 0083: equivalent guards for checked unsigned word arithmetic

Status: accepted for implementation.

## Context

Native unsigned addition/multiplication with a constant used general overflow
instructions. The x86_64 output produced separate overflow booleans and, for
multiplication, constrained the result to the hardware multiply registers.
The arithmetic must continue to abort on precisely the same inputs.

## Decision

For unsigned i64-represented addition/multiplication with one constant k:

- x + k: abort when x > u64::MAX - k, then emit an ordinary immediate add;
- x * k, k > 1: abort when x > u64::MAX / k, then emit an immediate multiply.

Both constant positions are supported. Zero/one multiplication and zero
addition keep the existing lowering and its normal constant folding. Signed
operations, other widths, subtraction and nonconstant operands keep their
existing overflow lowering. Constants are interpreted as unsigned bit patterns,
including those represented by negative CLIF immediates.

This is a different implementation of the same checked operation, not a proof
that checking is unnecessary. Operand evaluation is already complete before
these guards. The arithmetic instruction itself has no observable side effects;
the existing fault helper is invoked on exactly the original overflow inputs.
On the successful path the mathematical result fits the word, so its low-word
machine representation is exact.

## Validation and tradeoffs

Native runtime-input tests cover both constant positions, addition and
multiplication, 0/1 constants, ordinary constants, powers of two, u64::MAX,
and the exact last-success/first-overflow inputs. Results match checked Rust
word arithmetic; overflow still aborts. Existing signed and nonconstant fault
coverage remains required. Instruction selection is target-dependent, so the
change is benchmarked rather than assumed faster on every CPU.
