# ADR 0102: Function addresses in static initializers

Status: accepted (Cairn T1).

## Decision

Use the existing `ffi.callback(function)` spelling in static initializers:

```tarn
static HANDLERS [3]*u8 = [3]*u8{ffi.callback(a), ffi.callback(b), ffi.null()}
```

The argument must directly name a safe, synchronous, non-generic Tarn
function with no receiver and with scalar or raw-pointer C ABI parameters
and result. Parentheses and resolved module paths are allowed. The result
has type `*u8`.
The address is exactly the address returned by runtime `ffi.callback`: a
C ABI adapter for an ordinary or exported function, an interrupt trampoline
for `#[interrupt]`, or the generated table for `#[vector_table]`.

Each initializer retains its target as a resolved symbol. Targets are roots
of specialization even when no code calls them. The backend writes an
absolute function-address data relocation, including inside nested arrays
and structs, using the shared static layout. ELF and Mach-O use the same
Cranelift data API on both supported architectures. No code executes to
initialize the table.

## Rationale and limits

Kernel dispatch tables must exist before runtime initialization. Reusing the
trusted callback constructor avoids new syntax and keeps the existing ABI
contract. Arbitrary calls, closures, foreign functions, generic functions,
unsafe functions, and computed function pointers are rejected with E3085.
This adds no general constant execution or address arithmetic. x86_64 IDT
gates split an address into fields and still require runtime gate encoding;
a static handler-address table supplies those addresses without runtime
callback construction. Existing target restrictions on interrupts and custom
sections remain unchanged.
