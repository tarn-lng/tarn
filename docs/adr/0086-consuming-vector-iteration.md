# ADR 0086: consuming vector iteration

Status: accepted and implemented.

## Context

`for x in v` worked for ranges, arrays, and `&`/`&mut` arrays and slices, but
not for `Vec<T>` in any form (E3027). Code wrote `for x in v.as_slice()` to
borrow. To consume a vector it reversed it and popped, or popped from the
end and reversed the result. Both are noise, and the second costs a pass over
the elements.

Owned non-copy arrays are already consumed by index (ADR 0024). An
`IterArray` local keeps a per-element bitmap, so any exit destroys exactly the
elements not yet yielded. That bitmap has a fixed size. A vector's length is
dynamic.

## Decision

- `for x in v` with `v: Vec<T>` moves `v` into a local of kind
  `IterVec(cursor)`. The cursor is a `usize` local assigned 0 *before* the
  vector, so it is initialized wherever the vector is. The length is read
  once: the loop holds the only access to the vector.
- Each iteration calls the trusted intrinsic `Vec.iter_take(&mut it, cursor)`.
  It moves the element's bytes out without changing the header, and is
  bounds-checked like `Vec.get`. The cursor is then advanced *before* the body
  runs, so a `break`, `return`, `try` or panic in the body never sees the
  yielded element as remaining.
- Drop elaboration gives an `IterVec` local the plan
  `Drop::RemainingVec { place, cursor }`. It destroys the elements from the
  cursor to the length in index order, then frees the storage. A conditionally
  initialized vector is guarded by an ordinary value flag. The post-drop
  verifier requires the place to be a whole `IterVec` local whose cursor is
  the named `usize` local. Inlining renumbers the cursor with the other
  locals, and async frames keep it live wherever the drop can run.
- `for x in &v` and `for x in &mut v` (also through more references) iterate
  `v.as_slice()` or `v.as_mut_slice()`, with the usual binding modes. Elements
  are references, or copies of copy types.
- The move checker needs no change: the vector itself is moved into the loop,
  so using it afterwards is E4001.

## Alternatives considered

- **Reverse, then pop.** No new IR, but it costs `n / 2` swaps on every loop.
  It makes the idiomatic form slower than a hand-written index loop, against
  the goal that plain safe code be fast.
- **A per-element bitmap like arrays.** It would need a heap bitmap sized at
  run time, for no benefit over a cursor: elements are yielded in order, so
  the remaining ones are always a suffix.

## Consequences

- Consuming iteration costs one bounds check and one element move per
  element, the same as `Vec.get` plus a copy, and the vector is freed once.
- Tests:
  - `compiler/backend/tests/vector_iteration.rs`: each element is destroyed
    exactly once on end, break, continue, return and `try`, and borrowed
    iteration destroys nothing early.
  - `tests/native/pass/vector_iteration*.tarn`: every form of the loop, and
    generic, inlined and async functions.
  - `tests/moves/fail/E4001_vector_consumed_by_for.tarn`.
  - Outside the suite, both native programs ran under AddressSanitizer with
    LeakSanitizer and reported nothing.
