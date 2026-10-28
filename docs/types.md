# Type checking

Implementation: `compiler/types`. Output: side tables (expression types,
local types, method targets, pattern variants) keyed by `NodeId`/`SymbolId`.

## Types

`bool`, `i8…i64`, `u8…u64`, `isize`, `usize`, `f32`, `f64`, `string`, `void`,
`never`, structs and enums (with type arguments), `&T`, `&mut T`, `[N]T`,
`[]T` (unsized, behind a reference), `*T`, `*mut T`, `fn(A) R`, `mut fn(A) R`,
`once fn(A) R`, type parameters, borrowed `any I` and source async computations.
Owned dynamic-interface annotations are rejected (E3041).

## Inference

- Local and bidirectional: each function is checked on its own with a
  unification table; signatures are never inferred.
- Integer literals get an *integer variable*: it unifies only with integer
  types and defaults to `i64`; float literals default to `f64`. A literal
  takes the type its context needs (`b: u8 := 2` makes `b + 3` have type `u8`) and is
  range-checked after inference (E3025).
- Generic functions, methods of generic types (ADR 0015) and variants are
  instantiated with fresh variables per use; the expected type flows into the
  call (`x: Option<i32> := None`).
- A binding whose type is still unknown at the end of its function is E3010.

## Conversions and coercions

- **No implicit numeric conversions.** `i32 + i64` is E3006. Conversions are
  explicit calls: `i64(x)` (checked; trapping at run time when out of range).
- Implicit coercions, only at "expected type" sites (arguments, annotations,
  returns, assignments): `&mut T → &T`, `&[N]T → &[]T`, `&T → &any I` when `T`
  implements `I`.

## Operators

| Operators | Operands | Result |
|-----------|----------|--------|
| `+ - * / %` | two numbers of the same type | that type |
| `+` | `string` / `&string` + `string` / `&string` | `string` |
| `& \| ^` | two integers of the same type | that type |
| `<< >>` | integers | left type |
| `&& \|\|` `!` | `bool` | `bool` |
| `== !=` | same type: numbers, strings, `bool`, or a type implementing `core.Eq` | `bool` |
| `< <= > >=` | same type: numbers, strings | `bool` |

Structs/enums opt into equality through `impl Eq for T`; there is no structural
default. Conditional impl bounds are supported (ADR 0078), including the core
`Option<T: Eq>` and `Result<T: Eq, E: Eq>` implementations.

## Places and mutability (ADR 0017)

Assignment and `&mut` need a mutable place: rooted in a `var`, or reached
through `&mut`. Parameters, `self`, loop variables and pattern bindings are
immutable (E3015 / E3016). Calling a `&mut self` method borrows the receiver
mutably.

## Methods

Lookup on the receiver's type after removing references: inherent methods
(`fn T.m`), then impl methods; for a type parameter, the methods of its bound
interfaces; for `any I`, the methods of `I`. Receivers are adjusted
automatically: `&self` borrows a place, `&mut self` borrows mutably (requires
a mutable place or `&mut`), `self` moves (through a reference only for copy
types, E3030).

Methods of primitive types are declared in `core` (`stdlib/core/core.tarn`,
ADR 0020) as `extern "intrinsic"` functions. Only `len()`/`is_empty()` on
arrays and slices remain compiler-defined (closed list).

## Patterns (ADR 0014, 0018)

Unqualified capitalized names resolve against the scrutinee's enum here
(E3019 unknown variant). Bindings reached through a reference follow ADR 0018:
copy types bind by value, others by reference.

## Exhaustiveness (ADR 0019)

Usefulness over pattern matrices: nested patterns are analyzed precisely,
`E3018` names a missing value (`Some(Err(_))`), `W3001` flags arms that can
never match. Guarded arms never count as covering. Integer ranges are opaque
(a final arm is needed after `0..=127`, `128..=255`).

## Returns

A function with a non-`void` return type must not reach the end of its body
(E3009). A statement *diverges* if it is a `return`, a call returning `never`
(`panic`), an `if` whose every branch diverges, an exhaustive `match` whose
every arm diverges, or `for { }` without a `break`.

## `try`

`try e` on `Result<T, E>` in a function returning `Result<_, E>` yields `T`;
on `Option<T>` in a function returning `Option<_>` yields `T`. The error types
must be the same: **no automatic conversion in v0** (E3017).

## Unification

Unification is transactional: every binding is recorded in an undo log and a
failed `unify` restores exactly the previous state, so a failure never
influences later inference or diagnostics.

## Unsafe

Calling a foreign `extern "C"` function requires an enclosing `unsafe` block
(E3031); calling an `unsafe fn` requires one too (E3071). Trusted intrinsic
declarations do not automatically require unsafe calls. Ordinary callable types
cannot represent unsafe/foreign functions (E3082). Static C callback construction
has separate restrictions (E3081); see [FFI](ffi.md).

## Standard-library declarations and recovery types

Implemented standard-library APIs use real embedded Tarn declarations and normal
type/ownership checks. Missing members of real modules fail resolution/member
lookup. Unmodeled bootstrap standard-module APIs and channels reject with E3040;
they are not accepted as arbitrary safe operations. `Ty::Opaque` still exists
for compiler recovery, but is not a user-facing compatibility guarantee.

## Callable invocation modes (phase 10)

`fn(A) R`, `mut fn(A) R` and `once fn(A) R` require shared, mutable and consuming
invocation, respectively. Mode participates in unification and generic
substitution; annotations are invariant. Local closure modes are inferred from
typed capture uses. Mutable invocation may borrow an immutable callable binding
exclusively, but a shared reference cannot supply mutable access (E3044).
Consuming invocation requires ownership; its second use fails move checking.

Capturing syntax is independent: `fn` borrows captures; `move fn` transfers them.
Reference captures retain provenance. Explicit storage loans keep borrowed stack
environments inside their creating scope. Owned capture destruction is generated
as ordinary functions and elaborated through post-drop. See ADR 0032.
