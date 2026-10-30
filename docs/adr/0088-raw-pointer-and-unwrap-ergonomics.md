# ADR 0088: raw pointer and unwrap ergonomics

Status: accepted and implemented.

## Context

Porting the GTK backend of tarn-ui (`ui/backends/linux/gtk/native.tarn`)
showed how much ceremony raw-pointer code needs. `ffi.to_const` appeared 29
times, only because `is_null`, `address` and `read` take `*T`. Comparing two
pointers took `ffi.address(ffi.to_const(a)) == ffi.address(ffi.to_const(b))`.
Moving a pointer by `n` elements took `ffi.from_address(ffi.address(...) + n * size)`.
The port also wrote small `panic` helpers for fixed C strings because `Option`
and `Result` had no `unwrap` or `expect`. Integers had no `to_string`, and
passing a `Vec` to C took `ffi.slice_mut(v.as_mut_slice())`.

None of this protects anything: the `ffi` module already treats creating,
converting and comparing pointers as safe. Only reads and writes are `unsafe`.

## Decision

- **`*mut T → *T` coerces implicitly**, like `&mut T → &T`. It is the same
  address with fewer permissions, exactly what `ffi.to_const` did. The reverse
  is never implicit. The checker records the coercion and lowering emits the
  existing `MutToShared` IR coercion, which costs nothing in the backend.
- **`==` and `!=` on raw pointers of the same type** compare addresses.
  Ordering (`<`, `>`, ...) is still rejected: it is meaningful only within one
  object, and `ffi.address` stays available for that.
- **`ffi.offset(p *T, n usize) *T` and `ffi.offset_mut(p *mut T, n usize) *mut T`**
  move a pointer by `n` elements of `T`. The stride comes from the shared
  layout authority. Like `address`/`from_address`, they only compute an
  address (wrapping) and are safe. Validity is still checked at the `unsafe`
  read or write.
- **`ffi.vec(&v)` and `ffi.vec_mut(&mut v)`** return a pointer to a vector's
  first element, under the same rule as `ffi.slice`.
- **`Option.unwrap/expect` and `Result.unwrap/expect`** panic on the empty
  case. `expect` takes the panic message. `Result.unwrap` destroys the error
  and does not print it, because `E` has no formatting interface yet.
- **`to_string()` on every integer type.** The decimal encoders moved from
  `string.from_u64/from_i64` into `core` as `u64.to_string` and
  `i64.to_string`. Methods on primitives may only be declared in `core`
  (E2011). The `string` functions now delegate to them, and narrower types
  widen first.

## Consequences

- `same(a, b)` becomes `a == b`, `is_null(w)` becomes `ffi.is_null(w)`, and
  the slot accessors in tarn-ui become `ffi.read(ffi.offset(cell, slot))`.
- Implicit pointer weakening can hide an intended `*mut` at a call that only
  needs `*T`. That is harmless, since the callee could not write through it.
- `match` as an expression, `const` items and manifest-declared native
  libraries are separate decisions (later phases).
