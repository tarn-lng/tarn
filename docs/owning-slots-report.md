# Owning slots: systems readiness L1.1

Design: [ADR 0110](adr/0110-owning-slots.md), following ADR 0108.

## API and rules

- `replace_owned(&mut place, replacement)` returns the previous T, without
  dropping either owner. Any initialized exclusive local, field or array
  element is eligible. Ordinary mutable-loan conflicts still apply.
- `Option<T>.take()` returns the old Option and leaves None. `.replace(value)`
  installs Some(value) and returns the old Option. `.put(value)` requires
  emptiness and aborts if occupied. Returned linear owners must be consumed.
- `Vec<T>` accepts linear elements with push/pop/replace/swap/swap_remove,
  borrowed access and slices. `.take(index)` returns Option<T>, using
  swap_remove when in bounds. Dropping a linear Vec implicitly, clear and
  truncate are compile errors. Drain each payload explicitly, then `.finish()`.
- `finish_empty(value)` consumes optional slots, arrays and vectors after a
  runtime assertion that no must-consume payload remains, before any ordinary
  destruction. It releases empty vector storage. Nominal resources, including
  zero-sized ones, always fail this assertion. It never silently consumes one.
  An affine payload can be destroyed normally. Implicit drops gain no runtime
  exception; the assertion is an explicit operation when static emptiness is
  unavailable.
- Fixed arrays keep their initialized shape: exchange elements with replacements,
  or use Option<T> elements for reusable vacant slots. Drain optional arrays and
  call finish_empty. Bare-resource arrays can be exchanged or transferred whole;
  consuming array loops retain the conservative L1 remainder restriction.
- `static mut` supports fixed-layout owning structs/enums/arrays. Empty enum
  variants are const-constructible; owning/enum-containing roots require explicit
  initialization. A static name is still a raw address. Statics never drop and
  must be drained explicitly or documented as permanent roots.
- Inside unsafe, `ffi.replace_owned(pointer, value)` exchanges an initialized
  exclusive raw slot and returns its old owner. `ffi.take_owned(pointer)` leaves
  the storage uninitialized; `ffi.write_owned(pointer, value)` consumes into
  uninitialized storage. The caller owns validity, initialization, alignment,
  exclusivity, embedded-reference lifetime and synchronization. Writing over a
  live owner violates the unsafe write contract. Safe slot methods expose no
  uninitialized state. Existing ffi.read/write remain Copy/scalar operations.

The compiler continues checking ordinary generic consuming functions, early
returns, overwrites, discarded results, moves, loans and private representation.
No Cairn-specific type names or ownership exemptions enter the implementation.

## Exact Cairn phase 1A representation

Use an explicitly initialized fixed owner table. Keep architecture Context
records in fixed storage alongside it, so taking or moving a Task never moves
its saved Context. The context words are execution state; the Task still owns
its guarded-stack Frame/FrameRange and every other ownership token. A context
index identifies a stable record and does not replace any resource token.

This complete small example checks; expand the two owner entries to 64 explicit
None entries for Cairn (repeat-array syntax remains L4):

```tarn
import "ffi"
#[must_consume]
struct Task { id u64 }
struct Context { sp u64 }
static mut TASKS [2]Option<Task> = [2]Option<Task>{None, None}
static mut CONTEXTS [2]Context
fn publish(index usize, task Task) {
    unsafe {
        old := ffi.replace_owned(TASKS[index], Some(task))
        finish_empty(old)
    }
}
fn extract(index usize) Option<Task> {
    unsafe { return ffi.replace_owned(TASKS[index], None) }
}
fn reap(task Task) { match task { Task{id} => {} } }
fn main() {
    publish(0, Task{id: 1})
    match extract(0) {
        Some(task) => reap(task)
        None => {}
    }
}
```

Cairn's real Task contains its owned guarded stack and generation/state data.
In its defining module, the consuming reap/rollback function completely matches
Task, unregisters its guard and explicitly frees/transfers the stack tokens.
Use the same disposal for every allocation rollback. Publish only after full
initialization; an occupied-slot invariant failure in the example aborts while
retaining the previous owner in `old`. If replacement is expected, match and
explicitly reap/return that owner instead of calling finish_empty.

Extract with replace_owned(..., None), consume the resulting Some task exactly
once, and return None on an empty slot. A repeated extraction is safe; a repeated
use of an extracted Task is E4001. During suspend/run, retain Task in its root
slot; the scheduler uses the address of CONTEXTS[index], not the address of a
Task temporary. Initialize context words through the existing scalar raw
projections and pass its stable raw address to the architecture context gateway.
Protect owner/context tables with the scheduler's exclusive-access/IRQ discipline;
raw static access is unsafe and does not supply synchronization. Validate slot
indices and generations before lookup; this patch supplies ownership-preserving
storage rather than stale-ID policy.

On reap, the slot becomes None before disposal of the extracted Task. On a
full shutdown, drain every remaining owner through that same consuming path.
The static table itself remains a documented permanent root with no automatic
destructor. No ffi.take_owned/write_owned pair is needed for the normal task
slot path; initialized exchange avoids exposing raw holes.

Cairn was read only and remains untouched. Its toolchain pin can advance to this
commit, adapt the four reproducers from compile-fail to pass cases using this
API, and resume guarded-stack/reap/rollback/stale-ID acceptance on both targets.
This language patch does not claim Cairn's 1A milestone is complete.

## Validation

The requested package suite ran offline with `tools/ci/macos` first on PATH:

```sh
PATH="$PWD/tools/ci/macos:$PATH" cargo test --offline --no-fail-fast \
  -p tarn_types -p tarn_driver -p tarn_ir -p tarn_backend -p tarn
```

Result: **365 passed, 0 failed, 3 existing ignored tests**, across 66 test
binary/doc-test groups. The native timing/scaling test passed. The ignored
cases remain the host-dependent baseline, malloc counting through LD_PRELOAD
(which dyld does not honor), and a non-UTF-8 directory name APFS cannot create.
No tests were newly ignored and no outstanding failure remains.

A subsequent frontend re-run (`tarn_types`, `tarn_driver`, `tarn_ir`, `tarn`)
passed **101 tests with no failures**; focused ownership/ABI/zero-sized/opaque-owner checks passed.
The opaque generic-owner assertion regression was added after the full native
binary was built and passed separately. The final hosted owning_slots program
matched its expected stdout exactly with empty stderr. Both x86_64-none and
aarch64-none passed the dedicated owning-slot object test; undefined imports
were restricted to the existing freestanding contract.

The first run exposed a missing arch_aarch64 import in the existing execution
context report's checked example and snapshots affected by new public APIs.
The import and expected snapshots were corrected; the full rerun above is green.
Existing core declaration locations were retained by appending the new APIs.
Cairn was not modified, no dependencies were downloaded, and nothing was pushed.
