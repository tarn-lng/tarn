# Phase 23 plan: explicit error conversion

Status: complete; [ADR 0052](adr/0052-explicit-error-conversion.md),
[report](phase-23-report.md).
Baseline: Phase 22 (`b8e193b`) plus parallel mutation tests.

## 1. Problem and evidence

`try` requires the operand's error type to equal the function's error type
(E3017, ADR 0005). AGENTS.md: "Do not add implicit error conversion yet. Any
future error conversion mechanism must be justified by real usage." Two
independent programs now provide that usage:

- `examples/http_crud.tarn` (Phase 17): persisting inside an HTTP handler
  converts `io.Error` to `http.Error` with a hand-written `match` helper.
- `examples/sqlite/crud.tarn` (Phase 18): a handler returning
  `Result<_, sqlite.Error>` needs a `try_response` helper, used 12 times,
  because the fallible `http.Response` constructors return `http.Error`.

Both helpers exist only to change an error type. Today there is no way to
express that conversion compactly:

- `Result` and `Option` have no methods in `core` (no `map_err`, `ok_or`).
- Enum variant constructors are not function values: `f(AppError.Io)` is
  E3038, so even a future `map_err` would need a full closure literal.

## 2. Principles

- Conversion stays **explicit at the use site**. No global `From`-style
  registry that changes what `try` does depending on which impls exist. A
  reader of `try x` must see from that line which error is returned.
- Prefer library code and existing language rules over new syntax; add
  syntax only if stages A and B leave measurable ceremony.
- No change to `try` semantics for same-typed errors; E3017 still rejects
  mismatches without an explicit conversion.

## 3. Stages

### 23A: core combinators (library only)

Declare in `core`, implemented in Tarn:

- `Result<T, E>.map_err<F>(self, f once fn(E) F) Result<T, F>`
- `Result<T, E>.map<U>(self, f once fn(T) U) Result<U, E>`
- `Result<T, E>.ok(self) Option<T>`, `Option<T>.ok_or<E>(self, error E) Result<T, E>`
- `Option<T>.unwrap_or(self, fallback T) T`, `Result<T, E>.unwrap_or(self, fallback T) T`
- `is_ok`, `is_err`, `is_some`, `is_none` (shared receivers)

Generic owner methods on prelude enums need the existing owner-method rules
(ADR 0015) and monomorphization; confirm they work for `Result<T, E>` in
`core` before adding all of them.

Gate: native tests for every combinator with owned and Copy payloads,
including destruction counts for discarded owned values (`unwrap_or` on Ok
must destroy the fallback exactly once).

### 23B: variant constructors as function values

Allow a payload-carrying variant constructor where a function value is
expected: `map_err(http.Error.Io)`. Typing: `Enum.Variant` with payload types
`(A, B)` has type `fn(A, B) Enum<...>`; generic enums infer arguments from
the expected type. Lower it to a generated capture-free function item, like
existing capture-free closures. Unit variants are not functions.

This is a regularity rule, not new syntax: the expression already parses and
today produces E3038. Struct literals are not included.

Gate: type tests (generic and non-generic enums, arity mismatch still E3038
when called incorrectly), native tests passing constructors to
`map_err` and to ordinary higher-order functions.

### 23C: measure, then decide on syntax (ADR)

Rewrite both CRUDs with 23A+23B, for example:

```tarn sketch
try store.save().map_err(http.Error.Io)
```

Measure lines and helpers removed. Only if a pattern remains repetitive,
write the ADR for a dedicated form. Candidates, in order of preference:

1. Nothing more (combinators suffice).
2. `try expr else conversion` where `conversion` is a function value:
   explicit, local, greppable, and it keeps the error mapping on the line.
3. A declared conversion interface used implicitly by `try` (rejected unless
   evidence is overwhelming: it hides control flow behind impl lookup and
   interacts with coherence, ADR 0016).

The ADR must also settle whether `Option` to `Result` conversion through
`try` deserves the same form (`try opt else Error.Missing`).

## 4. Out of scope

Error hierarchies, stack traces or error context chains, `?` syntax,
exceptions, panics as errors, and changes to `Result`/`Option`
representation. Value-producing `if`/`match` stays a separate question even
though it would also shorten these programs.

## 5. Risks

- Generic methods on prelude enums may expose monomorphization or
  provenance gaps (combinators taking closures that capture borrows).
- Variant constructors as values add a new kind of function item; the
  backend must treat it like existing capture-free closures, never as a
  special case discovered late.
- `once fn` parameters in combinators require callers to pass consuming
  closures; verify that ordinary closure literals adopt that mode the same
  way Phase 17 adopted `mut fn`.
