# Ownership, borrowing and inferred lifetimes (draft 0)

Goal: no use-after-free, no double free, no invalid mutable aliasing, no
dangling references — without a garbage collector and without lifetime syntax.

Principle: **when in doubt, reject**. Rejecting a valid program is a bug to fix
later; accepting an unsafe one is a soundness hole.

## 1. Values, owners, moves

- Every value has exactly one owner: a local binding, a field, an element of a
  collection, or a temporary.
- Primitive scalars (`bool`, integers, floats) and `copy struct`s are **copy**:
  using them never moves.
- Everything else (`string`, structs, enums containing non-copy data, …) is
  **move**: `b := a`, `f(a)`, `return a` and `Struct{field: a}` move `a`.
- After a move the source is *Moved*; any use is error **E4001**.
- Moving out of a field moves only that field: the owner is *PartiallyMoved*;
  the whole owner can no longer be used, but other fields can. Drop elaboration
  destroys only remaining initialized fields.
- Moves inside a loop body of a binding declared outside the loop are errors
  (the second iteration would use a moved value) — E4002.
- A value is dropped at the end of its owner's scope, in reverse declaration
  order, unless it was moved. Conditional moves (`if c { consume(a) }`) are
  handled with drop flags; using `a` after the `if` is E4001 ("may have been moved").

State machine per place (tracked on the IR CFG):

```
Available ──move──▶ Moved
Available ──&────▶ Borrowed(n)        ──last use of all borrows──▶ Available
Available ──&mut─▶ MutablyBorrowed     ──last use of the borrow───▶ Available
Available ──move field──▶ PartiallyMoved
```

At CFG join points the state is the "worst" of the incoming states.

## 2. Borrowing

- `&x` creates a shared reference; `&mut x` an exclusive one (requires `x` to
  be `var`).
- At every program point, for each place: any number of live shared borrows
  **or** one live mutable borrow, never both (E4101, E4102).
- A borrow is *live* from its creation to its last use (non-lexical), computed
  by liveness on the IR.
- While a place is borrowed it cannot be moved (E4103) or assigned (E4104).
- Method receivers follow the same rules: `x.len()` with `&self` borrows `x`
  for the duration of the call.

## 3. Inferred lifetimes: reference provenance

There is no lifetime syntax (ADR 0007). The compiler tracks lifetimes
internally and summarizes each function that returns a reference by its
**provenance**: the set of reference parameters (including `self`) the result
may borrow from. Full rules: ADR 0010. Summary:

**Functions with a body.** Provenance is inferred from the body by dataflow
on the IR; calls use the callee's provenance; recursion is solved by a fixed
point. It may be any subset of the candidates:

```tarn
fn first(value &string) &string { return value }            // prov = {value}
fn longest(a &string, b &string) &string {                  // prov = {a, b}
    if a.len() >= b.len() { return a }
    return b
}
fn pick_left(a &string, b &string) &string { return a }     // prov = {a}
```

A caller of `longest(&x, &y)` gets a result that keeps both `x` and `y`
borrowed while it is alive; with `pick_left(&x, &y)` only `x` stays borrowed.

**Declarations without a body** (interface methods, extern declarations):

The defaults below apply to borrowed results without an explicit clause.
`borrows(a, b)` can name distinct reference inputs (or a borrowed `self`) and
therefore permits multiple declared sources; malformed clauses reject E3042.
See [ADR 0031](adr/0031-declaration-semantic-contracts.md).

| Signature | Provenance |
|-----------|------------|
| has `&self` / `&mut self` | `{self}` |
| exactly one reference parameter | that parameter |
| several reference parameters, no receiver | **rejected, E4202** |
| no reference parameter | **rejected, E4201** |

An `impl` method must not return borrows from more parameters than its
interface declaration allows (E4204).

**Errors.**

- E4201 `returned reference may outlive its owner` — the result may come from
  a local, a temporary, or nothing.
- E4202 `cannot infer where the returned reference comes from` — bodyless
  declaration with several candidates.
- E4203 `struct fields cannot hold references` (draft 0).
- E4204 `implementation returns a borrow its interface does not allow`.

Provenance is compiler semantic metadata used when checking calls. Borrow-suite
snapshots exercise its inferred sources. `tarn check --json` reports diagnostics,
not a public provenance dump; an interface-hash-based incremental compiler cache
is not implemented.

## 4. Interaction with other features

- `try` and early `return` run drops for live owners.
- Closures capture by borrow unless marked `move`.
- `spawn` requires captured values to be moved and transferable (`Send`-like
  property, inferred structurally).
- `unsafe` does not disable ownership or type checking; it authorizes foreign
  and unsafe calls whose invariants the programmer must uphold. Raw pointers
  carry no loans. C implementations and unsafe FFI are outside Tarn's safety
  proof; see [FFI](ffi.md).

## 5. Implementation

- Moves and initialization: phase 6A, ADR 0024.
- Borrows, non-lexical lifetimes, provenance: phase 6B, ADR 0025.
- The joint contract of both is `compiler/driver/tests/memory_safety.rs`.
- Drop elaboration: phase 6C, [ADR 0026](adr/0026-drop-elaboration.md), before any
  backend executes the IR.

## Executable drops (6C)

Drop elaboration preserves reverse scope-binding order and destroys aggregate
fields in increasing declaration order, recursively. Static drops execute, dead
drops disappear, conditional drops use explicit runtime bits, and partial drops
execute only live child plans. Reinitialization restores the affected child bits;
overwrites destroy the old live value first. Consuming arrays use element
bitmaps so moved elements are never destroyed twice. Panic aborts, with no unwind
or stack cleanup. Copy values have no owned-resource drop; user destructors are
not implemented. The post-drop IR is independently verified and printed with
`tarn ir file.tarn --drops`.
