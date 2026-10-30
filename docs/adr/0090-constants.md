# ADR 0090: module constants

Status: accepted and implemented.

## Context

Tarn had no named constants. The tarn-ui GTK port declared each GTK enum
value and layout size as a function (`fn PAGE() u32 { return 0 }`) and called
it at every use. That costs a call unless it is inlined. It also hides the
value from range analysis and reads poorly.

## Decision

- `[pub] const NAME Type = value` is a module item. `const` is a reserved word.
- The type is written and is limited to integers, floats, `bool` and
  `string`. Other types need construction and destruction, so they remain
  functions.
- The type checker evaluates every constant once, after declarations are
  collected (`types::consts`). It allows literals, other constants (in any
  order, across modules), unary `-`/`!`, arithmetic, bitwise, shift and
  boolean operators, and integer conversions `T(x)`. Arithmetic uses the
  run-time checks: overflow, division by zero, oversized shifts and values out
  of range are E3084 at the declaration, as are cycles and non-constant
  expressions. An error in one constant is reported once, not at each
  constant that depends on it.
- A constant of another integer type does not convert implicitly. As with
  values, the conversion is explicit: `u32(SLOTS)`.
- A use lowers to an IR immediate (`Operand::Const`), the same as a literal.
  Range analysis and constant folding see the value, and nothing is called
  or stored.
- Constants are not patterns in this ADR. A constant used as a pattern still
  reads as a variant (ADR 0014), and the error suggests a guard.

## Consequences

- `PAGE()` becomes `PAGE`, and layout sizes can be written in terms of each
  other.
- `tarn stdlib` lists public constants. `tarn syntax` reports `const` because
  it reads the lexer's keyword table.
- Constant patterns need exhaustiveness and test-lowering support. That is a
  later decision.
