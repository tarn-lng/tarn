# ADR 0047: minimal native C FFI

Status: accepted. Phase 18 implements all stages within documented v0 limits;
[report](../phase-18-report.md). Detailed stages, gates and tests:
[Phase 18 plan](../phase-18-plan.md).

Extension: [ADR 0080](0080-static-c-callbacks.md) adds static native callbacks
and unsafe scalar pointer access. Its callback support supersedes the original
callback exclusion below; aggregate ABI and variadic limits remain.

## Context

`extern "C"` declarations parse, type check, require `unsafe` (E3031) and carry
ADR 0031 contracts, but the native backend rejects every call:
`function getpid::instance#1 has no native body`. The linker invocation
(`compiler/backend/src/lib.rs`) is fixed to the object, the embedded runtime and
`-lm`. Raw pointer types `*T`, described as provisional in `docs/language.md`
section 14, do not exist in the parser or in `Ty`.

Evidence: a CRUD application written over Phase 16 HTTP (outside this
repository) needed persistent storage. SQLite is the obvious candidate and is
unreachable. Writing database protocols, compression, crypto or system APIs in
Tarn first would be the wrong order; xlinux will meet the same wall with libc
APIs that have no stdlib wrapper. Every intermediate workaround so far has been
a new trusted runtime intrinsic, which does not scale and grows the compiler's
magic surface (contrary to ADR 0020).

## Decisions

1. **Scalar C ABI first.** Native calls to user `extern "C"` functions support
   exactly: `bool`, all fixed-width integers, `usize`/`isize`, `f32`/`f64`,
   `void` results and (after 18B) raw pointers. Narrow integers and bool are
   sign/zero-extended by the caller. Variadic C functions are not expressible.
   System V x86_64 only. Other boundary types fail the **native build** with a
   message naming the function and type. The frontend keeps accepting them,
   because ADR 0031 uses bodyless `extern "C"` declarations with reference
   types to express and test provenance contracts without execution. A
   frontend error would invalidate that accepted decision and its tests for no
   safety gain, since such calls already require `unsafe` (decided in Phase 18A).
2. **Raw pointers `*T` and `*mut T`.** Copy, neither Transfer nor Share
   (even inside Copy structs), never carry loans and never extend provenance.
   Pointers to unsized values (`*[]T`, `*any I`) are rejected (E3072). There
   is no dereference syntax: a trusted `ffi` layer (no dependencies) provides
   safe creation/conversion intrinsics (`null`, `of`, `of_mut`, `slice`,
   `slice_mut`, `to_const`, `address`, `from_address`) and `unsafe fn`
   readers (`copy_bytes`, `string_from_c`) over libc. `unsafe fn` is a new
   declaration modifier: callers need an `unsafe` block (E3071); its body is
   not implicitly unsafe. Pointer-to-reference conversion is not provided:
   data returns from C through owned copies.
3. **Opaque C types use ordinary empty structs.** The proposed
   `extern "C" type Name` syntax was not added: Phase 18C showed that
   `struct Db {}` with `*mut Db` already gives a distinct, non-constructible-in-
   practice pointee with no layout use (tested with `fopen`/`fclose` and
   SQLite). Syntax would be justified only if empty structs cause real misuse,
   for example construction or by-value passing of a C handle type.
4. **Strings at the boundary are explicit.** `ffi.CString` is an owned
   NUL-terminated copy of `&string` (rejecting interior NUL);
   `unsafe fn string_from_c(*u8) Option<string>` copies and validates UTF-8.
   Tarn `string` keeps no ABI promise.
5. **Linking is a build-time grant, not a source declaration.** Libraries are
   linked only through `tarn build/run --link <name>` (and later the planned
   `tarn.toml`). Source code cannot request a native library: an imported
   module must not acquire native authority by being imported (dependency
   security policy: declarations are not grants). Unknown symbols fail at link
   time with a summary naming the missing C symbols or libraries (the linker
   runs with `LC_ALL=C` so the summary does not depend on the user's locale).
   Library names are validated (`name` or `:exact-file`), so a grant cannot
   inject linker options or paths.
6. **Resources wrapped by users have explicit close.** v0 has no user
   destructors. A Tarn struct holding `*mut sqlite3` is an ordinary non-Copy
   value; release is a consuming `close(self)`. Dropping without close leaks
   the C resource but is not a memory-safety violation in Tarn. Verified
   destruction of foreign resources (trusted catalog contracts, ADR 0034
   style) is deferred until real code proves leaks matter.
7. **Safety claim.** Everything behind `unsafe` and every C implementation is
   outside the safety proof. Safe Tarn wrappers are the author's promise, as
   with intrinsics (ADR 0031). Callbacks from C into Tarn, panics crossing
   the boundary, threads created by C and errno access are excluded.

## Alternatives considered

- **Trusted `sqlite` stdlib module with runtime C bridges** (Phase 15B model).
  Gives verified destruction and no raw pointers, but hardcodes one
  third-party library into the toolchain, embeds a dependency in every
  build, and must be repeated for each next library. Rejected as the general
  answer; still possible later for libraries that deserve stdlib status.
- **Source-level `#link("sqlite3")`.** Convenient but lets any imported module
  grant itself native authority. Rejected (decision 5).
- **dlopen at runtime.** Avoids link flags but moves failures to run time and
  needs function-pointer calls. Rejected for v0.
- **Full C ABI including structs by value.** Larger ABI surface without
  evidence; SQLite and most libc APIs need only scalars and pointers.

## Consequences and risks

- Raw pointers are new syntax and a new type kind; every phase (moves,
  borrows, capabilities, drops, backend layout) must treat them as plain Copy
  scalars. This must not leak into ordinary application code: the goal is
  that a `sqlite` package written in Tarn hides them entirely.
- Explicit close without destructors is ceremony relative to Go. Measure it
  in the CRUD port before designing foreign-resource destruction.
- A `--link` flag is a temporary interface until `tarn.toml` exists.

## Evidence that would change this

- Libraries that require structs by value or callbacks (e.g. sqlite3_exec
  callbacks, qsort) in real ports.
- Leaks or close-ordering bugs in real wrapper code, justifying verified
  foreign-resource destruction.
- Package-manager design (`tarn.toml`) replacing `--link`.

## Platform extension

ADR [0079](0079-linux-arm64.md) extends native execution to little-endian Linux
ARM64 (AAPCS64) while preserving the existing scalar-only C FFI boundary.
