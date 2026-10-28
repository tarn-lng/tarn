# C interop (FFI)

Design: [ADR 0047](adr/0047-native-c-ffi.md) and
[ADR 0080](adr/0080-static-c-callbacks.md). Linux x86_64 (System V) and
ARM64 (AAPCS64).

## Calling C

```tarn
import "ffi"
extern "C" fn strlen(text *u8) usize

fn main() {
    match ffi.CString.new(&"héllo") {
        Some(text) => { unsafe { print(strlen(text.as_ptr())) } }   // 6
        None => {}
    }
}
```

Every `extern "C"` call requires `unsafe`. Native builds accept `bool`,
fixed-width integers, `usize`/`isize`, `f32`/`f64`, raw pointers and `void`
results. Other types (references, strings, structs, slices) remain valid in
declarations for contract checking (ADR 0031) but fail native builds. Symbols
resolve against libc and libm unless another library is granted at build time.

Tarn does not parse C source or verify a C implementation. Declarations are
the binding author's ABI promises. C `int`/GLib `gboolean` use `i32`;
C `_Bool` uses `bool`. An incorrect declaration is unsafe.

Foreign and `unsafe fn` functions cannot become ordinary `fn` values (E3082):
that would erase their unsafe call requirement. Call them directly inside
`unsafe` or write a Tarn wrapper that checks their preconditions.

## Raw pointers

`*T` and `*mut T` are Copy machine addresses. They carry no loan and have
neither Transfer nor Share, so they cannot cross a native task boundary
(E3047), even inside a Copy struct. `*[]T` and `*any I` are rejected (E3072).

Creating and converting pointers is safe because nothing is accessed:

| `ffi` | |
|---|---|
| `null<T>() *mut T` | null pointer; type from context |
| `of(&T) *T`, `of_mut(&mut T) *mut T` | address of a borrowed value; the borrow ends at the call |
| `slice(&[]T) *T`, `slice_mut(&mut []T) *mut T` | address of the first element |
| `to_const(*mut T) *T` | the only mutability conversion |
| `address(*T) usize`, `from_address<T>(usize) *mut T`, `is_null(*T)` | numeric view, e.g. `(void*)-1` sentinels |

Because a pointer does not keep its pointee borrowed, the caller must keep the
pointee alive and unmoved for as long as C may use it. Reading memory through
a pointer is an `unsafe fn`:

| `ffi` unsafe | requires |
|---|---|
| `copy_bytes(*u8, count) Vec<u8>` | valid for reading `count` bytes |
| `string_from_c(*u8) Option<string>` | non-null, NUL-terminated; None if not UTF-8 |
| `read<T: Copy>(*T) T` | one initialized scalar/raw pointer at a valid, aligned address |
| `write<T: Copy>(*mut T, T)` | valid, aligned writable scalar/raw-pointer storage |

`read`/`write` require appropriate access, respect for active loans, and
synchronization against concurrent access. A bool read requires a 0 or 1 byte.
They do not expose aggregate layout or convert raw pointers into Tarn
references. Unsupported types fail native builds.

`CString.new(&string) Option<CString>` makes an owned NUL-terminated copy
(None for interior NUL); `as_ptr()` is valid while the CString is alive and
unmoved.

## unsafe fn

`unsafe fn` declares a function whose callers must write `unsafe { ... }`
(E3071), after checking its documented requirements. Its own body is not
implicitly unsafe: unsafe operations inside still need a block.

## Callbacks from C into Tarn

`ffi.callback(handler)` returns an erased `*u8` address of a static C ABI
adapter generated as native code. The handler must directly name an ordinary,
non-generic Tarn function. Qualified names such as `handlers.clicked` work.
Closures, callable locals, async, unsafe and foreign handlers are rejected
(E3081). Arguments/results use the scalar C ABI above; a void result works.

The [complete GLib example](../examples/glib_callback/main.tarn) registers
`ffi.callback(tick)` with an explicit `ffi.of_mut(&mut count)` context. Its
handler reads/writes the count and returns 0 after three dispatches, removing
the source before the context leaves scope:

```sh
tarn run examples/glib_callback/main.tarn --link glib-2.0
# 3
```

Creating the address is safe and allocates no environment. Its code lasts for
the executable's lifetime; it owns no context. Passing it to C is unsafe:
the C prototype must match exactly, and any context must remain alive and
unmoved until the handler is disconnected or the source is removed. The
caller also controls threading and reentrancy. A raw pointer does not retain
a loan. Panics inside handlers abort the process; there is no unwinding into C.

Variadic C calls, guaranteed C aggregate layout/by-value calls, captured
callback environments and automatic foreign-resource destruction remain
unsupported.

## Linking libraries

```
tarn build app.tarn --link sqlite3
tarn run app.tarn --link :libsqlite3.so.0   # exact file when no dev symlink exists
```

Linking is a build-time grant, never something source code or an import can
request. `--link` accepts a library name (`-lname`) or `:exact-file`; option-
or path-like values are rejected. Missing symbols and libraries are reported by
name. Opaque C handles are ordinary empty structs used behind pointers:

```tarn
struct Db {}
extern "C" fn sqlite3_open(path *u8, db *mut *mut Db) i32
```

## Native binaries and the safety boundary

The backend emits machine code in an ELF object. The system C toolchain
compiles the embedded C runtime and links the object with the granted native
libraries into an executable. No C source is generated for callback adapters.
Building currently requires `cc` and runtime headers. Running a dynamically
linked executable requires compatible shared libraries; native does not mean
self-contained or statically linked.

Ownership and borrowing check Tarn code, not arbitrary C. Unsafe FFI and the
native runtime are trusted boundaries. A safe wrapper must uphold the foreign
API's contracts, but cannot prove the library implementation correct. A C bug
can corrupt the same process. Use a separate process and IPC when a native
dependency requires isolation; `unsafe` and `--link` do not provide a sandbox.

