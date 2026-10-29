# The Tarn language (draft 0)

Tarn is a systems language with compile-time memory safety (ownership and
borrowing, no mandatory GC), inferred lifetimes, a small regular syntax and
official tooling. This document describes the implemented language and its
current limits. Examples containing `...` or application-specific names are
schematic fragments. Phase reports describe historical checkpoints; use the
current guides and the installed compiler's `tarn stdlib --json` catalog for
available APIs. Rebuild the compiler after changing embedded stdlib source.

Guiding rule for every feature: **one obvious way to write it**. If two forms
express the same thing, one of them is removed.

## 1. Lexical structure

- Source files are UTF-8, extension `.tarn`.
- Comments: `// line` and `/// doc comment` (retained as lexer trivia; the
  stdlib catalog associates adjacent preceding doc comments with declarations).
  There are no block comments (one form only; also easier for line-based tools).
- Identifiers: `[A-Za-z_][A-Za-z0-9_]*`. ASCII only in draft 0.
- There are no semicolons. Statements end at a line break, decided by the
  parser (ADR 0008):
  - inside `( )`, `[ ]` and struct/array literals, line breaks are ignored;
  - an incomplete expression continues on the next line: a line ending in a
    binary operator, `,`, `:=`, `=`, `=>`, `.` or an opening delimiter;
  - a line whose first token is `.` continues the previous expression
    (method chains);
  - a line starting with a binary operator does **not** continue the previous
    one — break lines *after* the operator;
  - `else` goes on the same line as the closing `}`.
- Integer literals: `42`, `1_000`, `0xff`, `0b1010`, `0o755`.
- Float literals: `1.5`, `2.0e-3`, `1e9`. A float needs a digit on both sides
  of the `.` (`1.` and `.5` are errors) so `0..10` is never ambiguous.
- String literals: `"text"` with escapes `\n \t \r \\ \" \0 \u{1F600}`.
  Strings may not contain raw newlines in draft 0.
- Interpolated strings: `f"n={n} user={user.name}"` (ADR 0077). Holes hold
  only a name or a field path; `{{` and `}}` are literal braces. Each value
  is borrowed (never moved) and must implement `core.Display`: integers,
  `bool`, `string` and user types with `impl Display for T { fn display(&self,
  out &mut Builder) { ... } }`. `f64` is not `Display` yet; use
  `string.from_f64`. The result is a new owned `string`.

Keywords:

```
fn async await return if else for in break continue match
struct enum interface impl copy
var pub import
true false
try spawn unsafe extern mut
```

`self`, `any` and `scope` are contextual: identifiers with a special meaning in
one position (receiver, dynamic interface type, structured-concurrency block).
`move` and `once` are contextual before `fn`; `borrows` is contextual after a
bodyless function's result type.

## 2. Modules and imports

A file is a module. A directory of files is a package-less module tree; the
module path is the file path relative to the project root, without extension.
The project root is the directory of the entry file (with a manifest, its
`entry`), so `import "core/session"` from `examples/app/main.tarn` reads
`examples/app/core/session.tarn`. Paths cannot leave that directory (`..`).
There is no `package` line: the file system already says it (one less thing to
keep in sync). See ADR 0004.

This example assumes a local `app/config.tarn` exporting `load`:

```tarn
import "fs"
import "process"
import "app/config"     // local module app/config.tarn

fn main() {
    cfg := config.load()
}
```

Items are private to their module unless marked `pub`. Module-level items can
be used before their declaration; locals cannot. Types, functions and values
share one namespace per scope. Imports are private to the importing module
and may form cycles. Full rules: `docs/resolution.md`.

External package imports are version-free, for example `import "redis"`.
The integrated package manager records intent in `tarn.toml` and exact verified
dependencies in `tarn.lock`. Normal builds consume the lock offline; explicit
package commands resolve or fetch. See [packages](packages.md) for the implemented
workflow and [dependency security](dependency-security.md) for its limits.

## 3. Bindings

```tarn
count := 10            // immutable, type inferred (i64 for a bare integer literal)
name := "hello"        // string
limit: u64 := 100     // immutable, explicit type
var total: i64 = 0     // mutable, explicit type
var index = 0          // mutable, inferred
```

- `name := value` / `name: Type := value` declares an **immutable** binding.
- In statement position a type annotation is introduced by `:`; in
  declaration lists (parameters, fields) the type follows the name directly:
  `fn f(a i32)`, `struct S { a i32 }` (ADR 0011).
- `var name: Type` declares a mutable binding **without a value**; it must be
  assigned on every path before it is used (E4005, ADR 0024).
- `var name = value` / `var name: Type = value` declares an initialized
  **mutable** binding. Without an initializer, an explicit type is required.
- `=` assigns to an existing mutable place.
- A call or other expression written as a statement discards its value:
  `g_source_remove(id)`. There is no `_ = value` (E2001 explains this).
- **Shadowing** (ADR 0009):
  - redeclaring a name in the **same scope** is an error (E2003);
  - a **nested** scope may shadow a name from an enclosing scope;
  - the compiler warns (W2001) when the shadowed local is used again after
    the inner scope ends, or when it is a `var`; shadowing an import or a
    module-level item is W2002.

```tarn
import "string"

fn parse(line string) {
    count := 0
    // count := 1             // error[E2003]: already declared in this scope
    parts := string.split(&line, &",")
    for part in parts.as_slice() {
        line := string.trim(part) // ok: shadows the parameter inside the loop
        print(line)
    }
}
```

## 4. Types

Primitive: `bool`, `i8 i16 i32 i64`, `u8 u16 u32 u64`, `isize usize`,
`f32 f64`, `string`, `void`, `never`.

- `string` is an owned immutable UTF-8 string. Dynamic values use owned heap
  storage; literal images are static and do not allocate on each evaluation
  (ADR 0055). A string
  literal has type `string` in draft 0; a borrowed view is `&string`. Length
  and indices count bytes. `bytes()` returns immutable `&[]u8` borrowing its
  owner; validated UTF-8 conversion and text helpers live in the
  [string module](strings.md). String `+` borrows both inputs and returns a new
  owner; comparisons use content and UTF-8 byte order.
- `void` is the unit type (functions without a return type return `void`).
  Its only value is written `()`, e.g. `return Ok(())`.
- `never` is the type of expressions that do not return (`panic(...)`, `return`).

Compound:

| Form | Meaning |
|------|---------|
| `[N]T` | fixed-size array, a value |
| `[]T` | slice (unsized; only usable behind a reference: `&[]T`, `&mut []T`; `&arr[a..b]` borrows one, `arr[a..b]` alone is E3039) |
| `&T` | shared reference |
| `&mut T` | mutable (exclusive) reference |
| `*T`, `*mut T` | raw FFI pointer; no loan or ownership guarantee |
| `fn(A, B) R` | function type |
| `Name<T>` | generic instantiation (type position only) |

There is no `null` literal. Ordinary absence is `Option<T>`; raw FFI pointers
can use `ffi.null()`.

Integer literals take the integer type their context requires and default to
`i64`; float literals default to `f64`. Numeric conversions are always explicit:
`u64(x)` checks the destination range and panics if the value does not fit,
in all build profiles. Float-to-integer conversions truncate toward zero before
the range check. Wrapping numeric conversions such as `u64.wrap(x)` are not
implemented. Integer arithmetic overflow panics in both debug and release;
the explicit integer methods `wrapping_add`, `wrapping_sub` and `wrapping_mul`
instead compute modulo the integer width (see [collections](collections.md#reference-places-and-modular-arithmetic)).

Comparison operators (`== != < <= > >=`) see through shared references on
either side: `v.get(i) == id`, `r >= 3` and `names.get(0) == "ana"` compare
the referenced values. Only references are looked through; numbers still never
convert implicitly (`&u32 == i64` is E3006). Arithmetic does not look through
references: write `*r + 1` ([ADR 0085](adr/0085-comparisons-through-references.md)).

## 5. Functions

```tarn
fn add(a i32, b i32) i32 {
    return a + b
}

fn log(message string) {        // returns void
    print(message)
}
```

- Parameter syntax is `name Type`; return type follows the parameter list.
- `return` is required to return a value; the last expression of a block is
  **not** an implicit return (one way to return).
- Functions may be declared in any order within a module.

## 6. Control flow

```tarn
if x > 0 {
    print("positive")
} else if x < 0 {
    print("negative")
} else {
    print("zero")
}

for i in 0..10 { }       // range, exclusive end; 0..=10 inclusive
for item in &list { }    // iterate by reference: `list` stays usable
for item in list { }     // copy arrays: by value; non-copy arrays: consumes `list`
for item in vec { }      // consumes a `Vec<T>`: each element is moved out in order
for item in &mut vec { } // `&T` / `&mut T` elements; the vector stays usable
for running { }          // while-style
for { }                  // infinite
```

- `for` is the only loop keyword.
- Consuming a `Vec<T>` moves each element into the loop variable in index
  order. Leaving early with `break`, `return` or `try` destroys the elements not
  yet yielded, once and in order, and frees the storage. There is no extra pass
  over the elements ([ADR 0086](adr/0086-consuming-vector-iteration.md)).
- Braces are mandatory; parentheses around conditions are not used.
- `if`, `for` and `match` are statements; they do not produce values. A
  value-dependent result is written with a mutable binding or a `return`
  from each branch (provisional; see open questions in the roadmap).
- Struct literals are not allowed directly in `if`/`for`/`match` heads; wrap
  them in parentheses (avoids the `if x == P{...} {` ambiguity).

## 7. Structs and methods

```tarn
struct User {
    name string
    age  u32
}

copy struct Point {        // opt-in Copy: all fields must be copy
    x f64
    y f64
}

fn User.new(name string) User {          // associated function
    return User{name: name, age: 0}
}

fn User.greet(&self) {                   // shared borrow of the receiver
    print(self.name)
}

fn User.birthday(&mut self) {            // exclusive borrow
    self.age = self.age + 1
}

fn User.into_name(self) string {         // consumes the receiver
    return self.name
}
```

Enums follow the same rule: a `copy enum` is copied on assignment; a plain
`enum` moves. Put each variant on its own line. Copy types must contain only
copy types.

The declaration mirrors the call site: `User.new("ana")`, `u.greet()`.
There is no `impl` block for inherent methods.

Methods of a **generic type** name the type's parameters right after the type
(ADR 0015); the names are binders, one per parameter, in order:

```tarn
struct Pair<A, B> {
    first  A
    second B
}

fn Pair<A, B>.swap(self) Pair<B, A> {
    return Pair{first: self.second, second: self.first}
}

fn Pair<A, B>.map_first<C>(self, f fn(A) C) Pair<C, B> {   // own generics after the name
    return Pair{first: f(self.first), second: self.second}
}
```

Methods can only be declared in the module that declares the type.

Struct literal: `User{name: "ana", age: 30}`. All fields must be given.
Field shorthand `User{name, age}` is allowed when a local of the same name exists.

## 8. Enums and pattern matching

```tarn
enum Shape {
    Circle(f64)
    Rect(f64, f64)
    Empty
}

fn area(s &Shape) f64 {
    match s {
        Circle(r) => return 3.14159 * r * r
        Rect(w, h) => return w * h
        Empty => return 0.0
    }
}
```

- In a pattern, variants of the scrutinee's enum are written unqualified (the
  type is known). In expressions, variants are qualified: `Shape.Circle(2.0)`.
- Exception: the prelude variants `Some`, `None`, `Ok`, `Err` are always
  available unqualified.
- `match` must be exhaustive. `_` is the wildcard.
- Arms are `pattern => statement` or `pattern => { block }`, one per line.
- In patterns, a **capitalized** name (`Empty`, `Circle(r)`) is a variant and
  a lowercase name (`n`) is a new binding (ADR 0014). Variant names must
  start with an uppercase letter.
- Patterns: literals, `_`, bindings, variants, struct patterns
  `User{name, age: 0}`, ranges `0..=9`. Guards: `n if n > 0 =>`;
  guarded arms do not count toward exhaustiveness.

## 9. Errors

`Result<T, E>` and `Option<T>` are ordinary prelude enums.

```tarn
import "fs"
import "io"

fn read_config(path &string) Result<string, io.Error> {
    text := try fs.read_text(path)
    return Ok(text)
}
```

`try expr` evaluates `expr`; on `Err(e)` (or `None`) it returns early from the
enclosing function. In v0 the error type of `expr` must be exactly the
function's error type: there is no automatic conversion (E3017). Convert
explicitly on the same line with the core combinators, passing a variant as
the conversion function: `try store.save().map_err(http.Error.Io)`, or
`try find(id).ok_or(Error.Missing)` for an Option (ADR 0052). `try` is a prefix keyword rather than a postfix `?`
because it is visible at the start of the line, reads the same to people and
models, and makes control flow greppable. See ADR 0005.

`panic("msg")` aborts the process. There are no exceptions.

## 10. Ownership and borrowing

Summary (full rules in `ownership.md`):

- Every value has one owner. Assignment, passing by value and returning **move**
  non-copy values. Using a moved value is error E4001.
- `&x` borrows shared, `&mut x` borrows exclusively. At any point: many `&` or
  one `&mut`, never both.
- Lifetimes are **inferred**; there is no lifetime syntax. A function that
  returns a reference has an inferred *provenance* (which reference parameters
  the result may borrow from), stored in its interface (ADR 0010). When the
  compiler cannot determine or prove it, the program is rejected.

## 11. Generics

```tarn
fn identity<T>(value T) T {
    return value
}

fn largest<T: Ordered>(items &[]T) &T { ... }
```

Generic arguments are written only in type positions. Inside expressions they
are inferred; when they can't be, annotate the binding: `x: Option<i32> := None`.
This is a **v0 restriction, not a permanent promise** (ADR 0012): a syntax
for explicit generic arguments may be added once real code shows the need.
No explicit turbofish; this keeps `<` unambiguous for the parser.

## 12. Interfaces

```tarn
interface Writer {
    fn write(&mut self, data &[]u8) Result<usize, Error>
}

impl Writer for File {
    fn write(&mut self, data &[]u8) Result<usize, Error> { ... }
}

fn save<W: Writer>(w &mut W) { ... }     // static dispatch (monomorphized or shared)
fn save_any(w &mut any Writer) { ... }    // dynamic dispatch, explicit with `any`
```

Interfaces take no type parameters in v0 (`interface Getter<T>` is E1021);
methods may still be generic.

An implementation for a generic type may require bounds of its arguments
(ADR 0078): `impl Eq for Pair<A: Eq, B: Eq>` makes `Pair<X, Y>` implement
`Eq` exactly when `X` and `Y` do. `core` uses this for `Option<T: Eq>`,
`Result<T: Eq, E: Eq>` and `Hash for Option<T: Hash>`. Coherence is
unchanged: still one impl per (interface, type). A failing use reports
E3077 with the innermost cause and the chain that required it.

`Self` in an interface is the implementing type (in an impl, its target).
A method using `Self` beyond its receiver cannot be called through `any I`
(E3073). Equality: `core.Eq` (`fn eq(&self, other &Self) bool`); `==`/`!=`
on a type implementing it call `eq`, with no structural default (ADR 0054).
A string literal may be passed where `&string` is expected; it is borrowed
exactly as `&"..."`.

Implementations are explicit (`impl I for T`), not structural: intent is
visible and greppable, and adding a method never silently changes which
interfaces a type satisfies.

Coherence (ADR 0016): an `impl I for T` must be written in the module that
declares `I` or the one that declares `T`; `T` is a struct or enum; its type
arguments are binders (`impl I for Pair<A, B>`); at most one impl per pair.

## 13. Native tasks (phase 11A)

```tarn
fn main() {
    task := spawn move fn() i32 { return 42 }
    value := task.join()
    print(value)
}
```

`spawn` creates native tasks on Linux x86_64 and ARM64. Outside a `scope`, its
operand must be a zero-parameter `move fn` literal, with transferable captures
and result. `Task<R>` is owned, non-Copy and movable;
`join()` consumes it and returns R directly. Joining twice or using a moved
handle is rejected by ordinary move checking. If an initialized handle is dropped,
its destruction waits and destroys the unused result. This applies on normal,
return, break, continue and nested exits; discarding a spawn expression therefore
joins its temporary at statement end. There is no detach or cancellation.

Ownership captures transfer through ordinary closure semantics; moved references
retain their loans and are rejected at this unscoped boundary in 11A. Borrowed
results whose storage would die in the worker are rejected. Panic in any worker
aborts the process. Allocation, thread creation/join failure and self-join are
runtime faults that abort, not recoverable Result values.

Phase 11B implements Transfer/Share enforcement and lexical scoped completion.
Captured loans remain live until verified task completion; scopes join before
borrowed storage is destroyed. Phase 11C implements owned Mutex/guard resources
and six concrete sequentially consistent atomics. Mutex Transfer and Share both
require a Transfer payload; guards have neither capability. The legacy
`spawn call(...)` form remains frontend-provisional and rejected by native codegen.
Channels remain unavailable. Phase 12A adds blocking networking; Phase 13 adds
`async fn`/`await` (section 13c).
See [ADR 0033](adr/0033-safe-native-tasks.md), accepted for Phase 11, and
[ADR 0034](adr/0034-blocking-networking.md) for networking.

## 13c. Async functions (Phase 13)

```tarn
async fn handle(stream &mut net.TcpStream) Result<usize, io.Error> {
    var bytes = [16]u8{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}
    count := try await stream.read_async(&mut bytes)
    try await stream.write_all_async(&bytes[0..count])
    return Ok(count)
}
```

An `async fn f(...) T` declares its output `T`; calling it runs no body code and
returns an owned `async computation<T>` holding the moved arguments (and the
loans they carry) in a stable heap frame. `await e` polls `e` with the current
waker until Ready and evaluates to `T`; it is only valid in an async body (E3060)
and only on a source computation or the trusted `runtime.Operation<R>` (E3061).
`try await e` is `try (await e)`.

Suspension does not change ownership rules: a value moved before an `await` is
moved after it, a borrow used after an `await` is live across it, and a pending
computation keeps every borrowed input unavailable until it is destroyed.
Destroying a computation that has not completed destroys exactly the values its
current state owns. A completed computation must not be polled again (abort).

Computations run on the Phase-12C executor model. Drive one explicitly:

```tarn
execution := try runtime.Execution.new()
var app = runtime.Operation.new(&execution, handle(&mut stream))
count := try try execution.block_on(&mut app)
```

or add `runtime.Operation.new(&execution, task())` to a `runtime.Executor`. Async socket
operations have distinct names (`read_async`, `write_async`, `write_all_async`,
`accept_async`, `finish_async`, `recv_from_async`) and require nonblocking
sockets; blocking methods (and DNS) keep blocking the whole executor if called
from async code. See [ADR 0037](adr/0037-source-async-lowering.md).

## 13b. Closures

`fn(x) T { ... }` is an anonymous function; parameter types may be omitted when
inferred from context. Captures are inferred shared/mutable borrows. `move fn`
takes ownership of captured values, including copies of Copy values; captured
references retain their loans. Borrowing closures cannot escape their stack
environment. Owned closures can be returned when their captured loans remain valid.

Callable types are `fn(A) R` (shared reusable), `mut fn(A) R` (mutable reusable)
and `once fn(A) R` (consuming). Local bindings infer the mode from capture uses;
ordinary invocation does not consume reusable callables. Mutable invocation
borrows the environment exclusively and does not require replacing the binding.
Moving a non-Copy capture from a body makes it consuming. Its second invocation
is an E4001 use-after-move. Mode annotations are invariant, with one exception:
a closure literal written where `mut fn` is expected and that does not consume
its captures is typed `mut fn` (Phase 17, ADR 0048); a consuming closure is still
rejected there. A non-`move` closure literal likewise adopts an expected
`once fn` (ADR 0052). Native spawning follows section 13; ordinary callable
values are not C function pointers.

Where a function value is expected, a payload enum variant is its
constructor: `map_err(AppError.Io)`, `apply(Some, 3)` (ADR 0052). A named
function or method adopts the expected mode too, since it has no
environment: `map_err(wrap)`, `o.map(double)` and `each(&mut c, Counter.bump)`
for a `mut fn(&mut Counter)` parameter (ADR 0087). Unsafe and foreign functions
are still not values (E3082).

```tarn
fn make() fn() usize {
    message := "callback"
    return move fn() usize { return message.len() }
}
fn main() {
    callback := make()
    print(callback())
    print(callback())
}
```

Borrowed environments stay on the stack. Capturing owned closures use unique heap
environments; capture-free closures use function items. Destruction goes through
post-drop IR and destroys each remaining capture once. See ADR 0032.

## 14. Unsafe and FFI

```tarn
extern "C" fn getpid() i32

fn pid() i32 {
    unsafe {
        return getpid()
    }
}
```

`unsafe { ... }` is a block statement (blocks never produce values).

Native calls (ADRs 0047/0079) support Linux x86_64 System V and ARM64 AAPCS64 scalar signatures:
`bool`, fixed-width integers, `usize`/`isize`, `f32`/`f64`, raw pointers and
`void` results.
The symbol is the declared name, resolved by the system linker against libc
and libm. Other parameter or result types still fail native builds with
"has no C ABI in v0"; they remain usable for semantic contract checks.

`unsafe` is required to call `extern "C"` functions and `unsafe fn` functions
(E3031, E3071). Raw pointers `*T` / `*mut T` exist (Phase 18B); unlike references,
they have no `*pointer` dereference operator. Memory is read/written through
`unsafe fn` helpers in `ffi`.
Ownership and type checks still apply inside `unsafe`. Static C callbacks use
`ffi.callback(handler)` (ADR 0080). Ordinary `fn` values cannot erase the unsafe
requirement of unsafe or foreign functions (E3082). See [C interop](ffi.md)
for supported pointer operations, callback restrictions and the unverified C
safety boundary.

## 15. Builtins (draft 0)

- `print(value)` — prints a primitive or string followed by a newline.
- `panic(message)`.

These remain compiler-defined builtins. The prelude also includes public
declarations from the existing `core` standard-library module.

## Appendix: notes for implementers

- `>>` is lexed as one token; the parser splits it when closing nested generic
  arguments in type position (`Option<Option<i32>>`). Same for `&&` in
  prefix/type position (`&&x` is a reference to a reference).
- Compound assignment (`+=`) does not exist: `x = x + 1` is the one form.
- The grammar actually implemented by the parser is in `docs/grammar.md`.

### Borrowed dynamic interfaces and bodyless contracts

Native dynamic interfaces support `&any I` / `&mut any I`; owned `any I` annotations
reject E3041. Method slots follow interface declaration order and use resolved
concrete implementations. Static generic bounds remain direct dispatch.

A bodyless declaration may specify borrowed-result sources after its return type:
`extern "C" fn choose(a &string, b &string) &string borrows(a, b)`.
Only distinct reference inputs or a borrowed `self` may be listed. Bodies infer
provenance and cannot override it with a clause. Missing ambiguous contracts still
reject E4202; malformed clauses reject E3042. External C calls still need unsafe,
and native C reference/aggregate ABI support remains deferred. See ADRs 0030–0031.

## Blocking networking

Phase 12A implements imported `net` TCP/UDP owners, borrowed byte-slice I/O,
blocking address resolution and explicit Result errors. Socket moves transfer
close responsibility; I/O uses mutable receivers and sockets have Transfer but
not Share. Native destruction follows ordinary post-drop IR. See
[networking](networking.md) and [ADR 0034](adr/0034-blocking-networking.md).

Blocking owned regular files and safe byte/text helpers are documented in
[the filesystem module](filesystem.md) (Phase 15B). File operations return
`Result<..., io.Error>` and use ordinary ownership, loans and post-drop.

Owned lexical POSIX paths are documented in [the path module](path.md)
(Phase 15C). Manipulation never performs filesystem I/O or implicitly cancels
`..`; borrowed text and owned components use ordinary provenance.

Blocking child execution is documented in [the process module](process.md)
(Phase 15D). Command configuration owns its text; Process is a non-Copy,
Transfer owner whose consuming wait or verified destruction reaps the child.

### Bounded async HTTP

`import "http"` supplies an ordinary server-side HTTP/1.1 module. Owned
Connection/Request/Response/Header use the existing move, loan, capability and
post-drop model; no HTTP syntax or native contracts are added. Complete bounded
binary bodies, sequential keep-alive, strict framing and stage deadlines are
specified in [HTTP](http.md) and [ADR 0046](adr/0046-bounded-async-http.md).
See [the native server example](../examples/http_server.tarn). This phase adds no
implicit try conversion or HTTP-specific main ABI.
