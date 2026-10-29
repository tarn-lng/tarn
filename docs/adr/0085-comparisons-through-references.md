# ADR 0085: comparisons through references

Status: accepted and implemented.

## Context

Comparing two references (`&u32 == &u32`) already worked: the checker peeled
both sides and the backend loads each operand through its own type. Comparing
a reference with a value did not. `v.get(i) == id` was E3006 ("cannot apply
`==` to `&u32` and `u32`"), and `r == 1` failed because the literal was checked
against `&u32`. Real code (a D-Bus library and Tarn UI) hit this often and
worked around it with indexing through slices or `*` noise.

## Decision

- For `== != < <= > >=`, the checker unifies the operand types after removing
  shared or mutable references from each side, at any depth. The right side
  is checked against the left side's referenced type, so literals take the
  value type.
- Lowering is unchanged. `Rvalue::Binary` operands keep their own types, and
  the backend reads through references as it already did for `&T == &T`.
  String operators and `Eq` implementations already accept either form
  (`str_ref`, `value_ref`).
- Arithmetic, bitwise and shift operators do not look through references.
  Their result is a new value, and an implicit dereference there would hide
  copies of larger types.

## Consequences

- No implicit numeric conversion is introduced: `&u32 == i64` is still E3006.
- A comparison never moves or copies its operands beyond the scalar load it
  already performed.
- Tests: `tests/native/pass/reference_comparisons.tarn` (integers on both
  sides, literals, strings, fields, floats) and
  `tests/types/fail/E3006_reference_comparison_mismatch.tarn`.
