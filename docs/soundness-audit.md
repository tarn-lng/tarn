# Tarn soundness audit (Cairn security audit, Task B)

Scope: Tarn commit 419e669 (branch `kernel/freestanding`) plus the fixes listed
below. In Cairn a compiler soundness bug is a kernel bug, so this audit tries to
get safe Tarn (no `unsafe`) to accept programs that lose, duplicate or alias an
owned resource, or whose checked arithmetic/bounds guarantee silently does not
hold. Findings marked *demonstrated* have a reproducer that was accepted or
misbehaved on the unfixed compiler; everything else is a plausible risk or an
unverified area and is labelled as such.

## Invariants attacked

1. `#[must_consume]` (ADR 0108): a value of a marked type, or any aggregate that
   contains one, is consumed exactly once on every path (normal exit, early
   return, `try` error edge, `break`/`continue`, overwrite, discarded temporary).
2. Owning slots (ADR 0110): `Option.take/replace/put`, `replace_owned`, linear
   `Vec`/arrays (`finish`, `finish_empty`), `static mut` owners.
3. Borrows with inferred lifetimes (docs/ownership.md, ADRs 0024-0026): no
   dangling loan, no aliasing `&mut`, provenance through calls, closures,
   generics, interfaces and indirect calls; no references stored in aggregates.
4. Privacy of marked types across modules (no forging or field extraction of a
   token outside its defining module).
5. Concurrency and suspension (scoped/unscoped `spawn`, `Mutex`, async frames):
   resources and loans stay valid across the boundary.
6. Checked arithmetic, casts, shifts and bounds checks of the native backend,
   including checks the IR range analysis proves away (ADR 0057), and the
   freestanding volatile contract (ADR 0101).

## Method

* Manual negative programs, about 110, written against each invariant
  (acquisition x control flow x disposition, closures, generics, dyn, async,
  spawn/scope, cross-module). Each was checked with `tarn check`; every one that
  should be rejected was classified as accepted or rejected.
* Mutation harness, `compiler/driver/tests/soundness_mutations.rs`
  (deterministic, no randomness, about 1.3k programs, roughly 25-30 s with two
  workers, which the harness fixes at 2):
  * linear family: 9 acquisitions x 8 control-flow contexts x 9 valid and 40
    mutated dispositions (deleted consume, duplicated move, early return,
    borrow instead of move, wrapping in Option/Vec/array/struct/closure/generic,
    owning-slot fills through `put`, `replace`, `replace_owned`, callee, closure,
    nested slots, overwrite). Valid ones must be accepted (no over-rejection),
    mutants must be rejected with an ownership diagnostic (E4001-E4008);
  * borrow family: 11 setups x invalidating operations, in the valid order and
    the swapped (invalid) order, plus 9 escape/aliasing pairs;
  * corpus family: every `tests/moves/pass` fixture that uses `consume(x)` is
    mutated by deleting or duplicating that statement.
  Run: `nice -n 10 cargo test -p tarn_driver --test soundness_mutations`.
  `TARN_SOUNDNESS_DUMP=dir` writes every generated program,
  `TARN_SOUNDNESS_VERBOSE=1` prints the source of failures.
  Harness self-check: with the fix of finding S1 disabled, 330 of 1163 linear
  cases fail with "violation accepted", so the harness detects that bug class.
* Differential checks of native code against a Python oracle,
  `tools/soundness/*.py` (seeded, run from the repository root:
  `python3 tools/soundness/scalar_arithmetic.py SEED COUNT`, same for
  `loop_arithmetic.py` and `vec_bounds.py`; `TARN` selects the binary):
  * `scalar_arithmetic.py`: one operation (`+ - * / % & | ^ << >>`, compare,
    negate, cast) on boundary values of all eight integer types, as constants,
    `var`s and call parameters;
  * `loop_arithmetic.py`: counted loops with accumulators and array stores, to
    exercise the range-proof elision;
  * `vec_bounds.py`: random `Vec` operation sequences (get/at/replace/swap/
    swap_remove/take/truncate/slices) with in-range and out-of-range indices.
  About 2.4k programs were run in total; mismatches in finding S2 only.
* Existing codegen tests for the freestanding contract were read rather than
  rewritten (`compiler/backend/tests/targets.rs`: volatile accesses are never
  merged, forwarded or reordered; repeated stores survive; fence emitted).

## Findings

| Id | Sev | Status | Summary |
|----|-----|--------|---------|
| S1 | P1 | fixed, 51f2cb2 | Payload obligation of a refined Option/Result/enum slot lost when the slot is mutably borrowed |
| S2 | P2 | fixed (commit "Check MIN / -1 for narrow signed constants") | `MIN / -1` and `MIN % -1` with a constant `-1` divisor of `i8/i16/i32` skipped the checked-arithmetic fault |

### S1 (P1, demonstrated, fixed): must-consume slot filled after `None`

* Where: `compiler/ownership/src/moves.rs`, `Rvalue::Ref` / `Rvalue::SliceRef`
  handling (about line 361 before the fix; now `forget_empty_variants`, line 610).
* Impact for Cairn: a `Task`, `Frame` or `FrameRange` stored with `put`,
  `replace_owned`, or a callee into a slot that the checker believed empty was
  silently dropped, i.e. the exact frame/task leak that `#[must_consume]` and
  ADR 0110 exist to prevent, in the owner-table pattern Cairn phase 1A uses.
* Reproducer (accepted before the fix; now rejected with E4007):

  ```tarn error=E4007
  #[must_consume]
  struct Task { id u64 }
  fn main() {
      var slot: Option<Task> = None
      slot.put(Task{id: 1})
  }
  ```

  Variants: fill through a callee in the `None` arm of a match, through a
  capturing closure, through `replace_owned` on a `Result` field.
* Fix: an exclusive borrow of a place makes every initialized path below it owe
  its payload again until a later discriminant test proves it empty. Fixtures:
  `tests/moves/fail/E4007_slot_filled_after_none`, `_filled_by_callee`,
  `_filled_by_closure`, `_field_exchanged`; positive
  `tests/moves/pass/slot_filled_then_matched`.
* The mutation harness contains 5 slot-fill mutants per context and catches this
  bug when the fix is reverted.

### S2 (P2, demonstrated, fixed): signed `MIN / -1` check skipped for narrow constants

* Where: `compiler/backend/src/codegen.rs`, `BinOp::Div | BinOp::Rem` lowering
  (line 1886). `constant()` returns the Cranelift immediate zero-extended, so a
  `-1` divisor of type `i8/i16/i32` was seen as `255/65535/4294967295`, the
  overflow guard `divisor == -1` was not emitted, and the raw `sdiv`/`srem` ran.
  `i64` was unaffected, and the non-constant divisor path was correct.
* Impact: the checked-arithmetic contract (every overflow reaches the panic
  path) did not hold for this case. On x86_64 the result was a hardware divide
  error (exit by SIGFPE, no `panic:` message); on a freestanding target that is a
  `#DE` exception instead of the kernel panic path, and `i32::MIN % -1`
  returned 0 where the non-constant path panics. No silent corruption or memory
  unsafety was found: the instruction traps. Rated P2 for that reason.
* Reproducer (before the fix: signal 8, no message):

  ```tarn
  fn main() {
      a: i32 := -2147483648
      b: i32 := -1
      print(a / b)
  }
  ```

* Fix: sign-extend the constant from the operand width before comparing with
  `-1`. Regression: `compiler/backend/tests/native.rs`
  `checked_arithmetic_and_panic_abort` gained `divide_min_i32/i16/i8` and
  `remainder_min_i32`; verified failing without the fix, passing with it.

## Cases tried and found sound (all rejected as required)

Must-consume and owning slots:
lost value by deleted consume, duplicated move, borrow only, wildcard, `Some(_)`,
rebind, generic `forget`, one branch, loop, early return, `break`/`continue`
while holding, `try` before consume, option/result/struct/array/Vec dropped,
`Vec.clear/truncate/drop`, overwrite of local, field, array element, `*r = ...`,
`&mut` match binding overwrite, `Option.take`/`replace` result discarded or
wildcard-matched, slot refilled in a loop, slot filled through nested option,
array of options, struct field, closure capture, callee, slice of slots,
`Result<Frame,_>` exchange, moving a value out of an array index, out of a
reference, out of `Vec.get`, `for x in vec/array`, `Option.map` with a consuming
closure, `unwrap()` discarded, owned closure capturing a linear value
(E4008), linear async inputs/outputs/locals across `await`, indirect and
interface-dispatched consuming calls (E4008), generic `T: Copy` instantiated with
a linear type, `any Interface` by value, uninitialized `var` of a linear or
generic type, shadowing, match guards.
Cross-module: forging a marked struct, reading, writing, destructuring a marked
value from another module, dropping or wildcarding a struct that wraps one.
Borrows: every ordering of borrow/use/invalidate for locals, fields, option
payloads, array and Vec elements, slices, string views, closures, stored
references; provenance through generic functions, function values, `any`
dispatch, bounded generics, `Option`/`Result` wrapping; returning references to
locals or temporaries; closures that escape their frame; references stored in
`Vec`, `Option`, enums, generic structs, arrays, closures in structs/Option;
aliasing arguments, array element aliasing, two-phase borrows (accepted, valid);
`Mutex`, scoped `spawn` races, unscoped spawn of references, linear values or
guards, task handle escaping a scope, `return` inside `scope`.
Native: integer overflow, underflow, division by zero and `MIN / -1` for all
widths on non-constant operands, narrowing/float casts, shifts >= width, slice
and `Vec` bounds, reversed ranges.

Over-rejection that is documented and intentional, not counted as findings:
async functions cannot own must-consume values, calls through interfaces,
function values and `any` cannot consume a linear value (E4008), consuming
`for` loops over linear arrays, generic discards of linear instances.

## Residual risks (plausible, not demonstrated)

* The checker tracks "payload absent" knowledge per move path. S1 showed that
  this knowledge can go stale through any operation that can write the slot
  without an assignment. The fix covers `&mut` borrows, slices and closure
  captures that lower to `Ref`. Any future lowering that writes a place through a
  different Rvalue (new intrinsic, new capture mode) must also call
  `forget_empty_variants`; there is no structural guard for that.
* Range analysis (`compiler/ir/src/ranges.rs`, loop peeling) removes checks.
  About 700 random loop/scalar programs agree with an oracle, but this is
  sampling, not proof; loops with data-dependent bounds, nested loops and
  mixed-width arithmetic are only lightly covered.
* Left shifts that discard high bits are not checked by the oracle (semantics not
  pinned down by a test in this audit).
* `ffi.read_volatile/write_volatile` are lowered to atomic accesses with a
  post-pass that rewrites an x86 trailing `mfence` into a NOP
  (`compiler/backend/src/arch.rs:44`); it returns an error on an unexpected
  lowering, so a Cranelift upgrade fails loudly rather than silently. Alignment
  of volatile addresses is the caller's `unsafe` contract.
* `unsafe`, raw pointers, `ffi.take_owned/write_owned` and `static mut` access
  were not attacked beyond reading ADR 0110: they are outside what safe Tarn
  can break and belong to Cairn's `unsafe` ledger.
* Cross-thread effects (`context_switch`, `switch_stack`) are implemented in
  Cairn assembly, not in Tarn semantics, and are covered by Task A.

## Not verified

* Freestanding aarch64 codegen of S2 was not executed (no QEMU, per the task
  limits); the fix is in the target-neutral lowering and the macOS/x86_64
  native test covers it. aarch64 `sdiv` semantics were reasoned about, not run.
* Differential checks ran against the debug `tarn` on macOS only.
* No exhaustive or coverage-guided fuzzing of the parser, resolver or type
  checker; the existing `*_never_panic` mutation tests were not extended.
* Interface-method provenance and `any` dispatch were tested by hand only, not by
  the generated harness.
