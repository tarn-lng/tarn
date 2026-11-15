# ADR 0110: Owning slots for must-consume values

Status: accepted and implemented (systems readiness L1.1).

## Motivation

Pools, job queues, open-file caches and schedulers retain owners in stable
storage and remove them later. ADR 0108 deliberately rejected native storage
and moving through references: assigning through an exclusive reference could
destroy the previous owner, while extracting first left an uninitialized place.
An ownership-preserving exchange supplies the missing general operation.

## Initialized exclusive places

`core.replace_owned<T>(place &mut T, value T) T` transfers the previous value
to its return and the replacement into the place. The place is initialized on
both sides of the operation. There is no implicit destruction, callback,
allocation or observable intermediate empty state. This is an exclusive-place
exchange, not an atomic operation for concurrent access. Ordinary loan checking,
reborrowing, move checking and result obligations apply.

`Option<T>.take(&mut self) Option<T>` exchanges with `None`.
`Option<T>.replace(&mut self, value T) Option<T>` exchanges with `Some(value)`.
`Option<T>.put(&mut self, value T)` first checks emptiness and aborts if occupied;
it never discards the previous owner. The old value returned by either exchange
must itself be consumed. A second take returns None; using an extracted owner
again is still E4001. Plain assignment through a borrow remains E4007 when it
could overwrite a resource. Plain dereference extraction remains forbidden.

```tarn
#[must_consume]
struct Ticket { id u64 }
fn settle(ticket Ticket) { match ticket { Ticket{id} => {} } }
fn settle_slot(value Option<Ticket>) {
    match value {
        Some(ticket) => settle(ticket)
        None => {}
    }
}
fn main() {
    var slot: Option<Ticket> = None
    slot.put(Ticket{id: 1})
    settle_slot(slot.replace(Ticket{id: 2}))
    settle_slot(slot.take())
    finish_empty(slot)
}
```

The generic exchange conservatively retains reference provenance from the
place and call arguments. Existing stored-reference restrictions and escaping
store checks remain in force; this operation does not manufacture safe loans.

## Collections and explicit empty completion

`Vec<T>` may contain must-consume elements. New/with_capacity, push, pop,
get/get_mut, len/is_empty, replace, swap, swap_remove and borrowed slices have
ownership-preserving contracts. Pop returns Option<T>; replace and swap_remove
return T. `take(index)` returns None outside the vector and otherwise removes
with swap_remove, without preserving order. Removal never drops an element.
Copy-only access and extension retain their existing Copy bounds. Indirect
consuming calls, owned linear captures and other erasing native operations
remain restricted by ADR 0108.

A vector of linear elements cannot be implicitly dropped, including when source
control flow has drained it: current analysis does not prove dynamic lengths.
`clear` and `truncate` discard elements and are rejected (E4007 in their generic
linear instances). These operations remain ordinary methods for affine types.
Drain explicitly with pop/take and consume every returned payload, then call
`Vec<T>.finish(self)`. Finish delegates to `core.finish_empty<T>(value T)`.

`finish_empty` is an explicit consuming assertion, not implicit drop permission.
It aborts if any live must-consume payload remains, before destruction begins.
It checks vector length, the active variant of enums, and owning fields/array
elements recursively. A nominal marked struct is always live, even when its
representation is zero bytes; it must be explicitly dismantled instead.
Opaque generic owners whose obligation is represented only by raw/scalar fields
also reject completion: only Vec supplies a verified native empty-header protocol. Once
all checks succeed, normal destruction releases the empty allocation and any
affine fields. Panic aborts without unwinding, as in L1. Thus even a failed
assertion has no normal continuation that loses a resource.

Fixed arrays retain initialized elements and fixed length. Exchange an element
with a replacement using replace_owned; arrays of Option<T> support indexed
take/put followed by finish_empty(array) after draining. Dynamic indices retain
conservative alias rules. There are no arbitrary uninitialized holes and no
new unchecked move through an index. Consuming array iteration retains L1's
conservative remainder restriction; use optional slots for a reusable table.
Arrays of bare nominal resources can be transferred whole or exchanged with
replacement owners; finish_empty cannot successfully dispose of such an array.
No implicit destructor gains a runtime emptiness test.

## Owning static roots and raw initialized storage

Statics keep the ADR 0102/0103 model: their names yield raw addresses, projected
fields/elements yield raw addresses inside unsafe, and nothing implicitly
copies or borrows their contents. Fixed-layout structs (including instantiated
generic structs), enums, and arrays may contain non-Copy and must-consume values.
Every possible payload must have a fixed layout without loans or unsized
values. Inactive variants may reserve space for owned strings or Vec headers;
None requires no heap allocation or live payload. Direct string/native
construction is not a new const operation. The shared IR layout supplies enum tags,
padding and payload offsets. Const construction additionally accepts payload-free
enum variants, especially None; nonempty enum payload initialization is deferred.
Owning or enum-containing statics require explicit initialization, including
nested fields: zero bytes are not an initializer for an owning slot. Arrays
use the existing `[N]T{...}` spelling; no repeat-array syntax is added.

Statics are never automatically dropped. They are permanent ownership roots.
The application must explicitly drain them at shutdown/reclamation or document
that their ownership lasts for the program's lifetime. The compiler checks
owners moved into and out of the gateway, not the unsafe program's global
accounting, races, aliasing, or eventual shutdown policy.

The unsafe raw gateway is:

- `ffi.replace_owned<T>(pointer *mut T, value T) T`: initialized exclusive
  storage in, initialized storage out, previous owner returned.
- `ffi.take_owned<T>(pointer *mut T) T`: initialized exclusive storage in,
  uninitialized storage out. The caller must not read/take again until writing.
- `ffi.write_owned<T>(pointer *mut T, value T)`: consume into valid aligned
  uninitialized writable storage. Any previous owner must already be extracted.

The caller supplies valid aligned storage, initialized bytes where required,
exclusive ownership, synchronization, correct pointee lifetime and any embedded
reference validity. These operations are explicitly trusted unsafe transfers;
using write_owned to overwrite a live resource violates its contract. Safe slot
APIs never expose that hole. Existing ffi.read/write keep their Copy/scalar
restrictions, and no safe copy of a static owner is introduced.

## Compiler boundary and validation

Trusted declarations carry semantic identities; the post-drop verifier checks
the owning primitive signatures. Ownership analysis admits only the named
storage operations with preserving contracts. Generic instances still undergo
L1's finite-bit obligation analysis, including the element-dropping bodies of
clear/truncate. Exchange lowers to an intrinsic call with moved operands and an
owned result. Backend execution snapshots aggregate bytes before replacing the
place, reusing the existing synchronization payload machinery. The explicit
empty assertion implements its declared recursive contract; it is not an
ownership inference pass. No AST changes, resource-specific exemptions, custom
destructors, runtime obligation flags, or external dependencies are introduced.

Four adapted Cairn reproducers are pass fixtures. Fail fixtures cover discarded
exchange/take results, double use, aliasing, clear/truncate, unsafe omission,
copying static owners, and omitted owning static initialization. Native tests
exercise transfer payloads, repeated static address stability, raw take/write,
and aborting nonempty/zero-sized completion. Both freestanding architectures
compile the same fixed-table gateway against only the freestanding contract.
See [the implementation report](../owning-slots-report.md) for results and the
exact Cairn integration.
