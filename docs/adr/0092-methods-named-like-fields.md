# ADR 0092: methods named like fields

Status: accepted and implemented.

## Context

`x.name(...)` was a field call whenever the struct had a field `name`, even
when that field was not callable. A struct with a data field `window` could
not have a `window()` accessor: the call was rejected with E3003 ("a value of
type `*mut Window` cannot be called"). The tarn-ui port renamed the method to
`native_window()` to get around it.

## Decision

A call `x.name(...)` on a struct calls the field when the field's type is a
callable (`fn ...`) or when no method `name` exists. Otherwise it calls the
method. `x.name` without a call is always the field.

- Callable fields keep their meaning, including when a method with the same
  name exists. That is the only remaining shadowing, and it only happens when
  both are callable.
- The checker records the choice in the existing tables (`callable_calls`
  and `method_calls`). Capture analysis and lowering read those tables, so
  nothing after type checking changes.

## Consequences

- Getter-style accessors such as `fn Native.window(&self)` next to a
  `window` field work.
- A field of a generic type parameter that is instantiated with a function
  type is called as a method when a method of that name exists. In that case
  the field call is written `(x.name)(...)`.
