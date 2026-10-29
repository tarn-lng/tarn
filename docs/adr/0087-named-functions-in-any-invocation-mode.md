# ADR 0087: named functions in any invocation mode

Status: accepted and implemented.

## Context

`Option.map`, `Result.map_err` and callback parameters such as tarn-dbus's
`with_session` take `once fn` or `mut fn`. Mode annotations are invariant
(ADR 0032). A named function was always typed as a shared `fn`, so
`map_err(wrap)` failed with E3001 ("expected `once fn(E) _`, found
`fn(E) W`"). Code wrapped every such function in a closure:
`map_err(fn(e E) W { return wrap(e) })`. Closure literals (ADRs 0048, 0052)
and variant constructors already adopted the expected mode.

## Decision

When a path names a function or method and the expected type is a callable of
some mode, the value takes that mode. A function item's environment is null
(ADR 0032). That makes it safe as a `mut fn`, whose call borrows the callable,
and as a `once fn`, whose call moves it: there is nothing to destroy either
way. Parameters and the result are unchanged, and generic functions still
infer their type arguments.

The rule applies only to function items. A general shared callable is not
widened to `once fn`. An owned shared closure has a body that does not consume
its environment, so a consuming call would never free it.

## Consequences

- `map_err(wrap)`, `o.map(double)` and `each(&mut c, Counter.bump)` type-check
  and run.
- A function item stored as `once fn` is still consumed by its first call:
  a second call is E4001.
- Unsafe and foreign functions remain non-values (E3082).
- No lowering or runtime change.
- Tests: `tests/native/pass/named_function_callbacks.tarn` and
  `tests/moves/fail/E4001_once_named_function.tarn`. The native program also
  ran without errors under AddressSanitizer with LeakSanitizer.
