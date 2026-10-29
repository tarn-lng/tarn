# Static C callbacks implementation report

Design: [ADR 0080](adr/0080-static-c-callbacks.md). API: [C interop](ffi.md).

## Implemented

- `ffi.callback(handler)`: static native C adapter for a directly named,
  ordinary non-generic Tarn function. No generated C or heap environment.
- Unsafe scalar/raw-pointer `ffi.read` and `ffi.write`, with Copy bounds
  and a structurally verified trusted result contract that creates no loans.
- Frontend E3081 for unsupported callback construction and E3082 for erasing
  unsafe/foreign function requirements into ordinary callable values.
- Explicit function identity survives lowering, specialization, post-drop
  verification and code generation. C ABI classification reuses external calls.
- A runnable GLib dispatch example with caller-owned scalar context.

## Validation

The dedicated native suite exercises stored callbacks after registration,
explicit disconnection, context mutation, stable adapter identity, scalar and
pointer memory access, narrow signed arguments/results, bool, floats and stack
arguments. Real libc `qsort` calls a module-qualified Tarn comparator; a panic
inside it aborts the process. Negative tests cover captures, callable locals,
unsafe/async/foreign/generic functions, unsupported ABI types, Copy/unsafe
requirements and corrupted callback/declaration metadata. Golden diagnostics
cover E3081 and E3082.

The GLib example prints `3` with no handwritten C source. A separate local GTK
smoke program connected a Tarn handler to a window's `map` signal, received it,
disconnected the handler and destroyed the window with `G_DEBUG=fatal-warnings`.
It printed `GTK signal handled in Tarn`. GTK is not a mandatory test dependency.

`cargo test --workspace --no-fail-fast`: 331 passed, 0 failed, 1 ignored;
no compiler warnings. This includes all seven new native integration tests,
the existing safety/drop/ABI suites and input-mutation tests. `git diff --check`
and the GLib example's formatter check also pass.

## Bugs found

Ordinary callable types erased the unsafe requirement when an `unsafe fn` or
foreign function was assigned to a local, permitting an unchecked indirect
call. Such conversions now fail with E3082, including inside unsafe blocks
where the value could later escape. Direct calls retain their existing rules.

## Decisions and limits

Static adapters have executable lifetime; foreign contexts have explicit caller
lifetime and disconnection obligations. Rejecting captured environments avoids
exporting stack loans or inventing foreign ownership rules. The erased pointer
fits GLib's erased callback parameter, but cannot validate a library's actual
prototype; that remains an unsafe promise. A future ABI-aware function-pointer
type could retain more information.

The backend supports the existing Linux x86_64/ARM64 scalar ABI; this session
executes on x86_64 only. No guarantee is added for C aggregate layout, variadics,
captured callbacks, automatic foreign cleanup, foreign-thread safety or isolation.
The compiler remains Rust, the runtime still includes C, and dynamically linked
executables still require compatible native libraries. The existing UI bridge
has not been migrated by this compiler change.
