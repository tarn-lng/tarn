# ADR 0103: Field and element addresses from raw pointers

Status: accepted (Cairn T2).

## Decision

Inside `unsafe`, `p.field` projects a pointer to a struct field and `p[index]`
projects a pointer to a fixed-array element. A static name already evaluates
to a raw pointer, so the same syntax applies to statics:

```tarn
import "ffi"

struct Tables { entries [512]u64 }
static mut TABLES Tables
fn set(index usize, value u64) {
    unsafe { ffi.write_volatile(TABLES.entries[index], value) }
}
```

Given `p: *T` or `p: *mut T`, projection returns `*Field` or `*mut Field`,
respectively. Nested structs and arrays compose without reading any enclosing
value. Fields retain normal visibility checks and generic substitution.
Indices have type `usize`, evaluate once after the base, and are checked
against the fixed array length even though projection requires `unsafe`.
Out-of-range indices call the existing fault contract. Ranges, slices and
indexing pointers to single elements are not supported; `ffi.offset` remains
available for unbounded pointer arithmetic.

Projections are address values, not assignable places. Read/write them through
`ffi.read`/`ffi.write`, or their volatile variants for MMIO. Taking a shared
reference to the resulting pointer borrows a pointer temporary, never its
pointee. No implicit dereference, aggregate copy or Tarn reference to global
storage is introduced. Immutable static field reads still require `unsafe`.

## Rationale and alternatives

Cairn needs page-table entries, allocator state and boot information without
handwritten byte offsets. Reusing field/index syntax avoids a field-selector
mini-language and composes with the established scalar and volatile APIs.
Implicit field loads or direct static places would introduce a second storage
model and make reference escape and aggregate ownership harder to constrain.
A new `&raw` syntax could express more general places but is unnecessary here.

The frontend decides field identity, visibility, element type and pointer
mutability. Lowering records compiler-only projection intrinsics with resolved
field indices; post-drop verification validates their typed shapes. The backend
uses `tarn_ir::layout::static_layout`, the same C-layout authority used to emit
statics, including padding, generic fields and large arrays. Address calculations
wrap like `ffi.offset`; the caller must provide valid aligned live storage before
access, obey aliasing and synchronization rules, and preserve valid scalar bytes.
Volatility belongs to the scalar access, not the address calculation, so no
aggregate volatile loads or stronger reference promises are needed.

## Limits and future evidence

There is no safe static-read exception, raw dereference operator, atomicity,
packed layout or aggregate read/write. Projection itself needs no valid pointee
and may be used on a null/sentinel address without accessing memory, but is
conservatively unsafe so future provenance rules can tighten its obligations.
Real kernel evidence may justify safe address-only projection or explicit raw
place syntax later; neither is required by T2.
