# ADR 0089: match expressions

Status: accepted and implemented.

## Context

`match` was only a statement. To compute a value, code declared a placeholder
and assigned it in every arm, or wrote a helper function that returned from
each arm. The tarn-ui GTK port (`native.tarn`) has eleven
`var x = blank()` + `match` sequences and several single-purpose helpers.
That was the largest source of length compared with the C original.

## Decision

`match` keeps its statement form at the start of a line. Anywhere an
expression is parsed (`x := match`, `return match`, an argument, an operand),
`match` is an expression, `ExprKind::Match`, which shares `MatchStmt` with the
statement form.

- An arm is `pattern => value`, or a block or statement that never reaches its
  end (`return`, `break`, `continue`, or anything of type `Never`, such as
  `panic`). Blocks have no implicit last-expression value. That keeps one
  rule for blocks and avoids Rust's trailing-semicolon distinction. Otherwise
  the error is E3083 `match_arm_without_value`.
- The type is the expected type when there is one. Otherwise it is the first
  arm that does not diverge, and later arms coerce to it with the usual
  implicit coercions. If every arm diverges, the type is `Never`.
- Exhaustiveness, pattern binding and guards are those of the statement.
- Lowering reuses the statement lowering. Value arms store into the
  destination of the enclosing expression, so ownership, drop elaboration and
  borrow checking run on ordinary IR with no new rules.
- Loop divergence (`for { }` without `break`) now also finds a `break` inside
  a `match` expression. Missing it would make a function that can fall off its
  end type-check (E3009).

## Consequences

- `ui := match native.create(...) { Ok(v) => v ... }` and short value
  mappings need no helper functions.
- `if` stays a statement. Making it an expression would reuse this
  machinery, but it is a separate decision.
- The formatter indents arms inside call parentheses one level deeper, as it
  already does for closure literals in arguments.
