# ADR 0080: static C callbacks and scalar pointer access

Status: accepted and implemented. Extends ADR 0047. See the
[implementation report](../c-callbacks-report.md).

## Evidence

The GTK UI backend can call ordinary GTK setters directly from Tarn, but
signals need C-callable functions. Today its clicked, changed and close handlers
are written in C because Tarn callable values use a private code/environment
pair, not a C function pointer. The UI port is concrete evidence for extending
the boundary without moving GTK-specific behavior into the compiler.

## Decisions

- `ffi.callback(function)` returns an erased `*u8` address of a static native
  adapter. Its argument must directly name an ordinary, non-generic,
  non-async, safe Tarn function without a receiver. Closures and callable
  locals are rejected with E3081. Module-qualified names are supported.
- The adapter uses the same scalar C ABI classification as external calls:
  integers, bool, floats, raw pointers, and a void result. Unsupported boundary
  types fail native builds, preserving ADR 0047's contract-only declarations.
  It forwards to the ordinary verified Tarn body, without an environment,
  heap allocation, hidden registry or generated C source.
- Creating an address is safe; passing it to C is unsafe. The author must
  match the exact C prototype (in particular C `int` is not Tarn `bool`),
  prevent invalid concurrent/reentrant accesses, and keep any explicit context
  alive and unmoved until C can no longer call the handler. The static code
  address lasts for the executable's lifetime; it owns no context.
- `unsafe ffi.read<T: Copy>(*T) T` and
  `unsafe ffi.write<T: Copy>(*mut T, T)` access one scalar or raw pointer.
  The pointer must be valid, suitably aligned, and initialized for reads;
  bool bytes must be 0 or 1. Access must respect active loans and concurrency.
  These operations do not create references or infer foreign ownership.
  Aggregate layouts, strings and callable representations are not exposed.
- Panics continue to abort the whole process. No unwinding crosses C.
  Foreign threading remains an unsafe integration responsibility, not an
  extension of Tarn's safe task model.
- An ordinary `fn` value cannot represent an `unsafe fn` or foreign function
  (E3082), even when acquired inside an unsafe block: its type would erase the
  call requirement after escape. Direct unsafe calls remain supported; a
  named Tarn wrapper may check its preconditions and explicitly call unsafe
  code. This closes a pre-existing aliasing hole exposed by the new readers.

## Alternatives and limits

A dedicated C function-pointer type would retain prototype information but
requires ABI-aware function types throughout the language. This smaller,
explicitly erased API fits existing raw-pointer FFI, including GLib's erased
callback parameter. It deliberately does not implicitly coerce closures to C
functions. A future typed API can coexist with it.

Capturing callbacks require stable storage, destruction and disconnection
contracts. They are deferred rather than exporting a dangling stack environment.
Variadics, guaranteed C aggregate layouts/by-value calls, and user destructors
remain separate work. This change does not remove GTK or the compiler's existing
C runtime dependency; it removes the need for handwritten C callback adapters.
