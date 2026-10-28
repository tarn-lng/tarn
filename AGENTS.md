# AGENTS.md

## Project

Tarn is a new systems programming language focused on:

- memory safety without a mandatory garbage collector;
- compile-time ownership and borrowing;
- inferred lifetimes instead of explicit lifetime syntax in v0;
- native performance;
- simple, regular, predictable syntax;
- integrated tooling;
- a broad standard library;
- low dependency overhead;
- fast development iteration;
- Linux x86_64 and Linux ARM64 as supported platforms;
- first-class support for coding agents and LLM-driven development;
- xlinux as one of the first major real-world ports;
- eventual self-hosting.

Tarn is not intended to be a clone of Rust, Go, Zig, C++, or any other existing language.

The goal is to combine strong static guarantees and low-level control with a simpler development experience and tooling designed for both humans and coding agents.

---

## Application-level ergonomics

Tarn is not only a systems programming language.

High-level application code should not be forced to expose low-level
memory-management details unless those details are semantically necessary.

HTTP servers, JSON processing, database access, CLI applications and similar
software should remain concise and predictable.

Tarn is high-level by default and low-level when needed. Low-level control is a
capability, not a tax imposed on ordinary application code.

If a common application task requires significantly more ownership ceremony

than an equivalent Go program, treat that as a design problem unless the extra
ceremony is required for a concrete safety guarantee.

## Language Policy

All compiler code, internal documentation, ADRs, test names, diagnostic source text, commit messages, tooling code, and agent instructions should be written in English.

User-facing localization may be added later.

English is the canonical language of the implementation and specification.

---

## Current Platform

Current supported platform:

```text
Linux x86_64 and Linux ARM64 (aarch64), little-endian, 64-bit
```

Current Rust edition:

```text
2024
```

Do not change the Rust edition without a concrete technical reason and verified toolchain support.

Do not add Windows or macOS support during the current development phases.

Cross-platform abstractions should only be introduced when they solve a real architectural need.

---

## Current Compiler Pipeline

The current compiler architecture is:

```text
Source
  ↓
Lexer
  ↓
Parser
  ↓
AST
  ↓
Name Resolution
  ↓
Type Checker
  ↓
Typed IR
  ↓
Move / Initialization Checker
  ↓
Borrow Checker
  ↓
Drop Elaboration
  ↓
Backend
```

Each phase should consume decisions already made by earlier phases rather than recomputing them.

Do not introduce duplicate semantic logic across phases.

---

## Architectural Principles

1. Correctness comes before features.

2. Keep the workspace compiling and tests passing whenever reasonably possible.

3. Every semantic feature must have tests.

4. Every compiler bug must receive a regression test.

5. Avoid external dependencies unless they solve a real problem better than a small internal implementation.

6. Do not optimize prematurely.

7. Do not introduce syntax sugar without evidence from real Tarn code.

8. Do not redesign a stable subsystem merely because another design is aesthetically preferable.

9. Preserve strong separation between syntax, semantic analysis, ownership analysis, and code generation.

10. The backend must not discover semantic facts that should already be known by the frontend.

11. Compiler diagnostics are a first-class product feature.

12. Tarn should remain understandable to humans and machine tooling.

13. Prefer explicit compiler state over hidden semantic behavior.

14. Prefer conservative rejection over unsound acceptance.

15. Do not copy features from Rust, Go, or other languages merely because they exist there.

---

## AST

The AST must remain semantically neutral.

Semantic information should live in external tables keyed by stable identifiers such as:

```text
NodeId
SymbolId
ScopeId
FunctionId
```

Do not attach resolver, type-checker, borrow-checker, or backend state directly to AST nodes unless a future architecture decision explicitly changes this rule.

Recovered parser nodes may use explicit `Error` variants.

---

## Name Resolution

Name resolution uses explicit scopes and symbols.

Semantic references should resolve to stable symbol identifiers.

Conceptually:

```text
AST node
  ↓
NodeId
  ↓
ResolutionTable
  ↓
SymbolId
  ↓
SymbolTable
```

Avoid string-based semantic lookups after name resolution when a stable identifier is available.

---

## Syntax Already Established

### Immutable binding

```tarn
x := value
```

### Mutable binding

```tarn
var x = value
```

### Explicit type annotation

```tarn
x: Type := value
var x: Type = value
```

Immutable bindings use `:=`; mutable bindings use `var` with `=`.
`var x := value` is a parse error (E1001).

### Uninitialized mutable binding

```tarn
var x: Type
```

Uninitialized immutable bindings are not currently supported.

Do not add them without evidence from real Tarn code.

---

## Parameters and Fields

In declaration lists, the type follows the name directly:

```tarn
fn add(a i32, b i32) i32
```

```tarn
struct User {
    name string
    age u32
}
```

In statement position, explicit type annotations use `:`.

This asymmetry is intentional.

---

## References

Shared reference:

```tarn
&value
```

Mutable reference:

```tarn
&mut value
```

`mut` is reserved syntax.

Explicit lifetime syntax does not exist in v0.

Do not introduce Rust-style lifetime annotations unless real evidence proves they are necessary.

---

## Functions and Return Values

`return` is explicit.

Blocks, `if`, `for`, and `match` do not currently produce values.

Do not silently convert Tarn into an expression-oriented language.

---

## Methods

Methods are declared using the same visual relationship used at call sites.

Example:

```tarn
fn User.greet(&self) string {
    return self.name
}
```

Generic owner methods use:

```tarn
fn Pair<A, B>.first(&self) A {
    return self.first
}
```

Do not introduce `impl` blocks for ordinary inherent methods.

---

## Interfaces

Interfaces use nominal implementation.

Example:

```tarn
interface Writer {
    fn write(data &[]u8) Result<usize, Error>
}
```

Implementations use:

```tarn
impl Writer for File {
    ...
}
```

Dynamic dispatch is expressed using:

```tarn
any Writer
```

Do not silently switch Tarn to structural interface implementation.

---

## Impl Coherence

An `impl I for T` must follow Tarn's coherence rules.

The current direction is intentionally stricter than necessary.

Do not loosen coherence rules casually.

A third-party module must not be able to create arbitrary conflicting implementations for foreign interfaces and foreign types.

Any change to coherence rules requires an ADR.

---

## Error Propagation

Error propagation uses prefix `try`.

Example:

```tarn
value := try operation()
```

There is no `?` syntax.

Do not add implicit error conversion yet.

Any future error conversion mechanism must be justified by real usage.

---

## Statements and Newlines

Tarn does not use semicolons.

The lexer preserves newline information.

The parser determines whether a newline ends a statement or continues an expression.

A line beginning with `.` may continue a method chain.

Binary operators must remain on the previous line when continuing an expression.

Do not move statement-boundary semantics back into the lexer.

---

## Operators

Compound assignment operators such as:

```text
+=
-=
*=
```

do not exist in v0.

Use explicit assignment:

```tarn
x = x + 1
```

Do not add syntax sugar without evidence that it materially improves real code.

---

## Generic Arguments

Explicit generic arguments in expressions are not currently supported.

This is a v0 restriction, not a permanent language guarantee.

Do not introduce syntax for explicit type arguments unless type inference proves insufficient in real Tarn code.

---

## Specialization and Native Limits

Monomorphization has no instance-count cap. Generic call cycles that grow a
type argument are rejected before specialization (E3074, ADR 0065); interface
calls on a type-parameter receiver are not modelled, and the backend type-size
limit still bounds them.

`tarn_ir::layout` is the single layout authority for the frontend and the
backend. Concrete values over the v0 limits (64 KiB, 4096 array elements, 64
nesting levels) are reported at their binding (E3075); values that become
concrete only through specialization fail in the backend naming the function
and binding. Do not duplicate layout rules in another phase.

Transfer/Share queries are coinductive: a repeated query holds, so recursive
types such as `enum Tree { Leaf(i32) Node(Vec<Tree>) }` are decided by their
non-recursive fields. Containment queries (references, guards, tasks,
wakers) are their inductive dual: a repeated query adds nothing (ADR 0074).
Exhausted depth or budget stays conservative (ADR 0033).

Dataflow passes (ranges, moves, borrows) visit blocks in reverse postorder
and keep sparse per-block states. Do not reintroduce dense per-local state
per block or unordered worklists; `compiler/driver/tests/scaling.rs` guards it.

---

## Pattern Rules

In patterns:

- a name beginning with an uppercase letter refers to an existing declaration;
- a name beginning with a lowercase letter introduces a new binding.

This is experimental v0 behavior.

Qualified patterns such as:

```tarn
Shape.Empty
```

are the escape hatch for ambiguous cases.

Do not treat the casing rule as permanently frozen.

---

## Pattern Exhaustiveness

Pattern exhaustiveness uses a usefulness / pattern-matrix style analysis.

Do not replace this with a simplistic wildcard-based fallback.

The compiler should detect unreachable arms and missing cases precisely whenever possible.

---

## Type System

The type checker currently supports:

- primitive types;
- generic structs;
- generic enums;
- references;
- arrays;
- slices;
- function types;
- type parameters;
- `any Interface`;
- contextual integer literal typing;
- explicit coercions where an expected type exists.

Do not introduce implicit numeric conversions.

Example:

```tarn
b: u8 := 4
x := b + 3
```

may be valid because the literal adapts to context.

But:

```tarn
a: i32
b: i64
x := a + b
```

must not silently coerce either side.

---

## Copy

`Copy` is a semantic capability.

It must never depend on the byte size of a type.

A large Copy value is still Copy.

If large copies become a performance issue, solve it using diagnostics, lints, or optimization.

Do not alter Copy semantics based on size.

---

## Generic Bounds

Bounds use syntax such as:

```tarn
Point<T: Copy>
```

Multiple bounds may use the existing bound syntax.

Do not introduce a complex Rust-style trait system unless Tarn gains a concrete need for it.

---

## Equality

Equality for user-defined types uses `core.Eq` (ADR 0054, 26C):
`pub interface Eq { fn eq(&self, other &Self) bool }`. `a == b` on a type
implementing `Eq` lowers to a statically resolved `Eq.eq(&a, &b)`; types
without an implementation keep E3006.

Do not hardcode structural equality for structs or enums.

Do not introduce automatic `derive` yet.

---

## Standard Library

The long-term direction is that user-visible standard-library APIs are declared in Tarn.

Core declarations live in Tarn source where possible.

Example concept:

```tarn
pub extern "intrinsic" fn string.len(&self) usize
```

The compiler should know as few magic intrinsic names as possible.

Do not allow hardcoded Rust-side intrinsic method lists to grow without strong justification.

---

## Typed IR

The typed IR is intentionally non-SSA.

Its immediate consumers are ownership and borrow analysis.

The IR uses explicit places.

Conceptually:

```text
Local
  + projections
```

Possible projections include:

```text
Deref
Field
Index
Downcast
```

The IR should make semantic decisions explicit.

Examples include:

```text
Move
Copy
Borrow
Deref
Coercion
Resolved Call
```

Ownership analysis must not have to rediscover method resolution or type coercions.

---

## CFG

Typed IR uses basic blocks and explicit control flow.

Conceptually:

```text
Function
  locals
  basic_blocks

BasicBlock
  statements
  terminator
```

Current terminators include concepts such as:

```text
Goto
Switch
Call
Return
Unreachable
```

Do not introduce SSA solely because it is common in compiler design.

SSA may be introduced later for optimization or backend purposes if benchmarks justify it.

---

## Move / Initialization Checking

Move analysis runs over typed IR and CFG.

It tracks move paths rather than only locals.

Tracked projections currently include fields and enum/downcast paths where possible.

Dynamic indices and dereferences remain conservative.

Relevant conceptual states include:

```text
initialized
uninitialized
moved
possibly moved
partially moved
```

The implementation may represent these using dataflow bits instead of an explicit enum.

Do not simplify the model in a way that loses partial-move precision.

---

## Borrow Checking

Borrow checking is separate from move checking.

Do not merge them into one monolithic phase without strong evidence.

Current loan kinds:

```text
SharedBorrow
MutableBorrow
```

Borrow checking operates on typed IR and CFG.

It uses real liveness / NLL behavior.

Do not regress to lexical lifetimes tied only to scopes.

---

## Loan Conflicts

Borrow conflicts are determined using place overlap.

Examples:

```text
p overlaps p.first

p.first does not overlap p.second
```

Indexes and dereferences are conservative in v0.

Do not attempt runtime-value reasoning for index disjointness yet.

---

## Reborrows

Passing or using an existing mutable reference must reborrow rather than move the reference where appropriate.

A mutable reborrow temporarily suspends incompatible use of the original mutable reference.

When the reborrow dies, the original reference becomes usable again.

Do not regress to move semantics for ordinary mutable-reference calls.

---

## Provenance

Tarn infers reference-result provenance from function bodies.

The user does not write explicit lifetime relations.

Provenance is semantic function metadata.

The compiler may infer relationships such as:

```text
result may borrow from parameter 0
result may borrow from parameters 0 or 1
```

Declarations without bodies use conservative rules.

Do not introduce explicit lifetime syntax to solve ordinary provenance cases.

---

## References in Structs and Enums

References stored inside struct or enum fields are currently forbidden in v0.

This restriction is fundamental to the current borrow model.

Do not remove it casually.

Allowing stored references would require revisiting the current "loan origin is held by locals" model.

Any proposal to support references inside aggregates requires a dedicated design phase and ADR.

---

## Closures

Closure capture representation must support:

```text
SharedBorrow
MutableBorrow
Move
```

Borrowed closures use inferred shared/mutable captures and stack environments.
`move fn` transfers captures into a unique owned environment (ADR 0032).
Callable types distinguish shared, mutable and consuming invocation. Preserve
ordinary loans, capture/result provenance and post-drop destruction functions.

Owned closures may escape only while all captured loans remain valid. Borrowed
stack environments must retain their storage loan and cannot escape their frame.
Native concurrency implementation is phase 11 work; preserve this callable model
when introducing task boundaries (ADR 0033).

---

## Spawn and Concurrency

Phase 11 follows ordinary Copy/Move, callable capture ownership, loans and
provenance. Do not add a separate thread-safety checker or backend ownership
queries. Phase 11A implements handle-owned native tasks; phase 11B implements scoped loans
and Transfer/Share. Phase 11C adds Mutex guards and concrete sequentially consistent atomics.
The Phase 11 native model is complete within its documented v0 limits; design and implementation status: [ADR 0033](docs/adr/0033-safe-native-tasks.md).

Task handles must have deliberate ownership and completion semantics. The v0
policy is unique handles with join on destruction, no detach or cancellation.
Scoped completion must precede destruction of borrowed storage on every normal
exit, including return, break and continue. Retain captured loans until completion;
do not weaken E4205, E4203 or conservative indexed-place overlap.

Cross-thread safety is part of the ownership model. Moving a reference never
extends its lifetime. Unknown native resources have neither Transfer nor Share
unless their declaration catalog provides an explicit trusted contract.

Distinguish cross-thread value transfer from concurrent shared access using semantic
capabilities, not Copy or size guesses. Native handles require explicit trusted
contracts. Synchronization guards must expose ordinary loans and verified
resource destruction; do not add user-defined destructors to implement locks.
Synchronization does not bypass Tarn ownership. Mutex<T> is a non-Copy owner;
Transfer and Share both require T: Transfer, without requiring T: Share. Guards
are non-Copy, non-Transfer and non-Share ownership-bearing resources whose verified
destruction releases synchronization authority. Payload loans must not outlive
the guard; use ordinary provenance and loan tracking, never a separate guard
lifetime checker or backend ownership inference.

Atomics are explicit synchronized storage, not ordinary mutable scalars. The v0
concrete atomic set has only sequentially consistent operations; integer fetch
arithmetic checks overflow and aborts. Concurrency safety remains a compile-time
semantic property wherever possible. Mutexes do not poison or promise reentrancy.

Panic in any task remains whole-process abort. No async, futures, reactor, green
threads, scheduler optimization or concurrency platform expansion in this phase.
Do not design concurrency during unrelated phases.

---

## Blocking Networking

Phase 12A sockets are ordinary non-Copy owned resources in `stdlib/net`.
TcpListener, TcpStream and UdpSocket explicitly have Transfer and not Share;
I/O uses mutable receivers, and address queries use shared receivers. Moving
an owner transfers exactly-once close responsibility. Consuming close leaves
no live owner, including on an OS error. Shutdown preserves ownership.

Native socket destruction must follow verified post-drop IR, never a runtime
ownership registry or backend move/loan inference. Buffers use borrowed byte
slices; native storage must not escape. Public networking returns Result and
normal network errors must not abort. Linux sends suppress SIGPIPE; close must
never retry EINTR. Safe syscall retries and high-level behavior belong in Tarn.
Trusted private bridge declarations must be structurally verified and cannot
be authorized merely by a user module name. See [ADR 0034](docs/adr/0034-blocking-networking.md).

Blocking networking blocks the current native task, including DNS. Phase 12B
adds explicit nonblocking socket mode and owned level-triggered readiness (ADR 0035).
Readiness is evidence that an operation may make progress, not permission to bypass
normal socket error handling. Readiness registration never transfers ownership of
a socket or application buffer to the kernel/runtime. Tarn must not keep borrowed
application buffers pending across readiness waits in the Phase 12B model.
Keep actual I/O under ordinary exclusive loans. Tokens must not confuse recycled
file descriptors with earlier owners. No async, scheduler, io_uring, HTTP or TLS
is authorized by readiness support.

---

## Suspended Execution

Phase 12C manual operations use verified owned closure state and a single-thread
executor before async syntax. Suspension extends ownership and borrowing
obligations; it does not suspend the memory-safety rules. Wakeup means "poll again",
not "the operation is complete". A Pending computation must remain safely
destructible in every state. Executor scheduling authority does not imply
ownership of application resources. Preserve normal provenance/loan visibility,
readiness token protection and post-drop cleanup. No async/await syntax or
multi-thread scheduler follows without a separate approved phase.
See [ADR 0036](docs/adr/0036-suspended-execution.md).

---

## Source Async

Phase 13 lowers `async fn`/`await` onto Phase-12C suspended execution (ADR 0037).

`async` does not weaken Tarn ownership; suspension extends ordinary ownership
and loan obligations across time.

`await` is compiler lowering over the same Pending/Ready/Waker model validated
before async syntax existed. Do not add a second wake or runtime mechanism.

Generated async frames are ordinary owned state whose destruction must be
correct in every suspension state. Ownership analysis runs on the source CFG with
explicit `Suspend`/`Abandon` edges; frame placement happens only afterwards.

The backend does not decide async ownership semantics; it only places frame
slots and emits mechanical poll/destruction adapters.

Calls are lazy; blocking and async I/O have distinct names; there is no hidden
global executor. Do not add async closures/blocks, select, cancellation, timers or
multi-thread executors without a separate approved phase.

## Cooperative Async Tasks

Phase 14A (ADR 0038). An async task is an owned computation run cooperatively
by an executor, not an operating-system thread; `spawn` stays native threads and
`spawn_async` is cooperative. The task handle is the only authority over its
pending result. Dropping a task never detaches it silently: a pending task is
abandoned through its verified frame destruction, an unjoined result is
destroyed once. Runtime task records store bytes only; typed result handling is
compiled code. Do not add a second scheduler or task model.

Timeouts (14B, ADR 0039) race through the same readiness model and abandon the
loser structurally; completion is checked first, so the result wins a tie.
Buffered I/O (14C, ADR 0040) is Tarn policy over verified async primitives, not
a new native I/O subsystem. Buffers are owned; delimiter reads take a limit;
destruction never performs I/O, so flushing is always explicit.

## Stdlib Module Layers

ADR 0041. Trusted stdlib modules are core, io, time, net, runtime, fs, process
and ffi, embedded in the driver (`TRUSTED_STDLIB`). Layers: time and net build
on io; runtime builds on io, time and net; fs builds on io; process builds on io,
net and fs (ADR 0070, 0071); ffi is
standalone; no import cycles. Ordinary stdlib modules (http, https, path,
string, json, os, console, collections, csv, websocket, encoding, crypto) are plain
Tarn with no intrinsic
authority (`ORDINARY_STDLIB`). Private access follows that graph downward only:
never above or beside, and never from user modules. Do not hardcode a module name in compiler phases; look declarations up
in their module through `Decls`. Native intrinsics are `<layer>._<operation>`.
Methods live in their type's module. Avoid module names that are natural
variable names (a local shadows a module).

---

## Drops

Lowering emits abstract drops.

Move analysis classifies them.

Drop elaboration is responsible for making runtime destruction explicit.

The backend must not query move-checker state to decide whether to drop a value.

After drop elaboration, the IR should contain sufficient information for code generation.

---

## Panic

Current panic semantics are abort-only.

There is no stack unwinding.

Do not introduce unwinding, exceptions, destructors during unwind, or exception personality machinery without a dedicated design phase.

---

## Custom Destructors

User-defined destructors are not part of v0.

Do not introduce custom Drop-style hooks yet.

If destructors are added in the future, their interaction with `Copy`, partial moves, panic, and ownership must be designed explicitly.

---

## Standard Library Opacity

Implemented standard-library APIs have real embedded declarations and ordinary
type/ownership contracts. Unmodeled bootstrap APIs and channels are rejected
with E3040; missing members of real modules fail lookup. Internal opaque
recovery types are not permission to accept arbitrary standard-library calls.

Do not claim complete end-to-end memory-safety guarantees while opaque stdlib values can hide borrowing behavior from the compiler.

The long-term goal is for stdlib signatures to expose sufficient semantic information.

---

## Diagnostics

Diagnostics must be actionable.

Prefer messages that explain:

- what went wrong;
- where the relevant value was declared;
- where it was moved or borrowed;
- where the conflict occurs;
- where the later use keeps a loan alive;
- what the user can do next.

Avoid compiler-internal terminology when source-level terminology is possible.

Tarn diagnostics should be useful to both humans and coding agents.

---

## Diagnostic Codes

Do not reuse existing error codes for different meanings.

Document new codes.

Golden diagnostic tests should be used for important errors.

Any diagnostic wording regression discovered during review should receive a test when reasonable.

---

## Tooling

Tarn tooling must reuse the real compiler frontend.

The VS Code extension and LSP must not implement independent parsing, name resolution, or type checking.

Current tooling may consume:

```text
parser
resolver
type tables
symbol tables
source spans
driver APIs
```

Build tooling around compiler facts rather than reconstructing them.

---

## LSP

The LSP is allowed to use small justified external dependencies for protocol-level concerns such as JSON-RPC and URI handling.

Do not apply the compiler-core dependency policy dogmatically to editor tooling.

However, keep tooling dependencies controlled and understandable.

The current LSP may initially use full-buffer synchronization.

Incremental synchronization should only be introduced when there is evidence that full sync becomes a real performance problem.

---

## Agent-First Tooling

Tarn is designed to work well with coding agents.

Future compiler tooling may expose machine-readable operations such as:

```text
tarn check --json
tarn ast --json
tarn symbols --json
tarn refs --json
tarn type-at --json
tarn impact --json
tarn explain
tarn fix --safe
```

Design internal APIs so these capabilities remain possible.

Do not make semantic information accessible only through human-formatted text.

---

## Dependencies

Before adding an external dependency:

1. Explain the problem it solves.
2. Explain why the standard library or a small internal implementation is insufficient.
3. Inspect its transitive dependency weight.
4. Prefer focused libraries over large frameworks.
5. Avoid convenience dependencies that bring large dependency trees.

Do not pursue "zero dependencies" as a dogma.

Reimplementing complex standards incorrectly merely to avoid one dependency is not a project goal.


### Tarn package management and supply-chain security

The preceding dependency review applies to the compiler's existing dependencies.
The following requirements govern Tarn packages. The initial pure-source manager
is implemented in Phase 22 (ADR 0051 and docs/packages.md). Public registry
infrastructure, signed provenance and build sandboxes remain future work. Do not
implement those systems merely as part of documenting this policy.

“Dependency resolution should be boring, deterministic and auditable.”

“Dependencies are data until explicitly granted execution authority.”

“Tarn should assume that any dependency, including a transitive dependency, may become hostile.”

- Keep one integrated CLI: `tarn add`, `remove`, `update`, `deps`, `audit`,
  `verify`, `publish`. Do not introduce `tarnpkg`. Normal future application use
  should be concise: `tarn add postgres`, `tarn add redis`, `tarn build`, `tarn test`.
- Keep source imports version-free (`import "redis"`, `import "postgres"`), never
  `redis/v2` or `postgres@4`. Put dependency intent in `tarn.toml` and exact graph,
  versions, origins and hashes in `tarn.lock`.
- Normal builds must not silently rewrite locks. Updates must be intentional:
  `tarn update` or `tarn update <package>`. The same source tree, compiler/toolchain
  identity and lock must resolve the same dependency bytes and graph; this does
  not promise bit-identical native binaries or describe implemented support.
- Express exact, compatible-major, compatible minor/patch and explicit range
  requirements using ordinary SemVer; explain conflicts. Prefer one compatible
  version and avoid unnecessary duplicates. Incompatible majors require an
  explicit future coexistence design, never versioned import paths.
- Model malicious/compromised direct and transitive packages, compromised
  publishers, malicious releases, typosquatting and dependency confusion.
  Official status, popularity, known publishers and previous safety are not trust.
- Never execute package-provided installation/build hooks by default, including
  preinstall/install/postinstall/prepare/setup.py/build.rs equivalents. Pure Tarn
  packages normally need none. Exceptional build execution requires explicit
  authorization and sandbox enforcement for filesystem, network, processes and
  environment. Declaration is not a grant; no ambient home, SSH/cloud/package
  credentials, arbitrary environment, network or whole-filesystem access.
- Keep releases immutable. Identity/version/content hash must never acquire
  replacement bytes; yank or deprecate without rewriting releases. Changes need
  new versions. Verify locked content before use, including cached sources.
  Minimum lock evidence is package identity, version, source registry, content
  hash and graph. Plan publisher/repository/commit/publication evidence and
  signatures/provenance; bind identities to origins to prevent silent substitution.
- Plan a content-addressed global store, conceptually `~/.tarn/registry/`,
  `~/.tarn/sources/`, `~/.tarn/artifacts/`. Deduplicate sources; key artifacts by
  source hash, compiler/toolchain identity, target, options and graph identity,
  not names or mutable tags alone. Cache reuse must respect verification/policy.
- Support project/global minimum release age; its default is open. Detect missing
  provenance, publisher changes, unverified publication and unexpected repository
  changes. SemVer is not approval: require explicit acknowledgement of trust
  downgrades and enforce required policy even after acknowledgement.
- Make `tarn deps --tree`, `--why <package>` and `--trust` explain dependency
  introducers, selected versions, origins, hashes, publisher/provenance changes,
  build authority and advisories. `audit` and `verify` must distinguish unknown
  evidence from success. Surface security decisions when trust or authority changes.
- Require registry immutability, publisher authentication, strong MFA, namespace
  ownership, typosquatting defenses, yanking, attestable/signed provenance and
  advisories. Authentication does not establish trustworthy code.
- Prefer explicit refusal for insufficient required trust/provenance, hash
  mismatches or permissions. Never silently select a trusted fallback for convenience
  or execute without required containment.

Keep exact SemVer resolution, incompatible-major coexistence, default release
age, signing/provenance formats, federation, sandbox implementation and package
features/configuration explicitly open. Capability/configuration examples are
conceptual, not finalized schemas. Existing Cargo bootstrap dependencies and the
compiler's embedded-runtime `cc` invocation are trusted toolchain boundaries;
this policy does not claim they are sandboxed today.

Detailed requirements and open choices: [dependency security](docs/dependency-security.md).

---

## Stable process rules (Phase 15D)

- Child completion is owned: Process is non-Copy, Transfer and not Share.
  Wait consumes; destruction waits/reaps. Never detach or implicitly kill.
- Command arguments are separate strings, never implicit shell input.
  Configuration affects the child, not parent globals. NUL is rejected.
- Capture stdout/stderr concurrently; sequential pipe draining can deadlock.
  Buffers are bytes; text conversion validates UTF-8 without loss.
- Verified resource catalogs bind private shapes, capabilities and consuming
  signatures. Native ABI helpers never infer ownership or maintain owner bits.
- EINTR retry for consuming wait stays within its native bridge; Linux close
  is never retried. See [ADR 0045](docs/adr/0045-blocking-processes.md).
- Bounded waits observe completion without reaping (WNOWAIT); the owning
  wait/destruction stays the single reap. Handoffs make a one-shot Launch
  that consumes them at launch (child fd 3 + k); nothing else is inherited.
  Child fields drop pipes before the wait. See [ADR 0070](docs/adr/0070-process-handoff-and-bounded-waits.md)
  and [ADR 0071](docs/adr/0071-one-shot-launch-and-piped-stdio.md). Async
  waits own a pidfd; abandonment destroys the Process, which still waits
  ([ADR 0072](docs/adr/0072-communicate-and-async-processes.md)).

## Stable path rules (Phase 15C)

- Path is an ordinary owned string wrapper; provenance and structural
  Transfer/Share remain authoritative. No native ownership exception is needed.
- Path manipulation is lexical. Join and filesystem I/O never implicitly
  normalize `..`; symlink resolution is a separate filesystem operation.
- Components and optional path results own their text. Do not invent stored
  borrowed path views or a separate lifetime system.
- POSIX slash syntax and strict UTF-8 are the current boundary. See
  [ADR 0044](docs/adr/0044-lexical-owned-paths.md).

## Stable filesystem rules (Phase 15B)

- `stdlib/fs` owns regular files; File is non-Copy, Transfer and not Share.
  Explicit close consumes ownership even when it reports an error.
- Verified post-drop closes live files/cursors once. Never retry Linux close,
  query move checking in the backend, or add runtime ownership registries.
- File I/O borrows slices and reports partial counts; complete operations and
  EINTR retries live in Tarn. EOF/errors do not bypass normal destruction.
- Use io.Error; filesystem error mapping must preserve networking categories.
  Invalid UTF-8 text/names and NUL paths are explicit errors, not lossy decoding.
- Directory entries own copied names/paths; borrowed strings retain provenance.
- fs depends downward on io, with separately validated resource/intrinsic IDs.
  Async filesystem remains a separate phase. See
  [ADR 0043](docs/adr/0043-blocking-filesystem.md).

## Stable string rules (Phase 15A)

- Strings own immutable UTF-8 storage; lengths and indices count bytes.
- Byte views are immutable loans of the owner, with ordinary provenance and
  move/overwrite checks. Never expose mutable string storage.
- Byte-to-string conversion validates UTF-8 and returns a new owned copy.
- Split/line elements are owned strings. Do not invent stored borrowed
  references or a separate text lifetime checker to avoid copying.
- Text parsing checks ranges before checked arithmetic; invalid input and
  overflow return Option failure. Comparisons inspect content, not pointers.
- High-level text functions belong in `stdlib/string`; primitive methods
  remain in core. See [ADR 0042](docs/adr/0042-string-essentials.md).

## Stable HTTP rules (Phase 16)

- HTTP lives in ordinary Tarn stdlib/http; module names never confer intrinsic
  authority. Keep protocol parsing/encoding/state out of the native runtime.
- Connection owns its TcpStream and buffers. Protocol state is not resource
  liveness. Drop closes and discards output; response completion requires flush.
- Requests/headers/bodies are bounded owned data. Views borrow owners; trailers
  stay separate and cannot override routing/framing/authentication headers.
- Reject ambiguous framing and never reuse failed input. Preflight all outbound
  fields before transmission; never send another response after partial failure.
- Deadlines cover whole stages, not individual bytes. Local timeout abandonment
  uses ordinary loans and verified frame destruction; no detached loser.
- Generic manual pollers can return borrows of their own storage. Do not consume
  them while returning such results or weaken provenance to make a timeout fit.
- HTTP errors use explicit Result handling; no implicit io conversion, special
  HTTP main ABI or capability inference. See [ADR 0046](docs/adr/0046-bounded-async-http.md).

## Tests

Tests are part of the language specification.

Maintain pass/fail suites where appropriate.

Current important categories include:

```text
parser
resolution
types
moves
borrows
memory safety
IR
```

Use golden snapshots where they improve regression detection.

Mutation tests should continue to verify that malformed programs do not panic or hang the compiler.

---

## Memory Safety Contract

Maintain a small canonical contract suite that runs programs through the complete implemented pipeline.

Unsafe programs should be rejected with the expected diagnostic code.

Safe programs should continue through the pipeline.

Do not claim Tarn's memory model is complete until all required runtime semantics, including drop elaboration, are implemented and known bootstrap holes are closed.

---

## ADRs

Architectural decisions belong in:

```text
docs/adr/
```

Before changing a significant existing decision:

1. Read the relevant ADR.
2. Understand why the decision was made.
3. Gather concrete evidence for changing it.
4. Update the ADR or create a superseding ADR.
5. Update tests and documentation.

Never silently invalidate an architectural decision.

---

## Working as an Agent

Do not behave as a passive instruction executor.

Act as a member of Tarn's engineering team.

When making a non-trivial design decision, explain:

- the problem;
- the chosen approach;
- alternatives considered;
- advantages and disadvantages;
- why the chosen approach fits Tarn;
- risks or technical debt introduced;
- what future evidence could justify changing it.

Challenge existing instructions when there is a concrete technical reason.

Do not disagree merely for novelty.

---

## Phase Reports

At the end of each significant phase, report:

### What was implemented

A factual summary.

### Tests

What was added and what passes.

### Bugs found

Especially bugs discovered through tests or manual IR/diagnostic inspection.

### Decisions I would defend

The most important design choices and why they are appropriate.

### Decisions I still question

Areas where more evidence is needed.

### Known limitations

Intentional restrictions or bootstrap holes.

Do not hide uncertainty.

---

## Before Starting Work

Before starting a new phase:

1. Read this file.
2. Read the relevant ADRs.
3. Read `docs/architecture.md`.
4. Read the documentation for the previous phase.
5. Inspect the current implementation rather than assuming the documentation is perfectly current.
6. Run:

```bash
cargo test
```

7. Ensure the workspace compiles without warnings.

Do not build new work on top of a broken baseline unless the task is explicitly to repair that baseline.

---

## Git

Do not commit automatically unless explicitly instructed.

Do not rewrite unrelated user changes.

Do not discard modifications simply because they were not produced by the current agent.

If a surprising change exists, inspect it before modifying it.

---

## Scope Discipline

Do not implement future phases opportunistically.

Examples:

- do not add a backend while implementing drop elaboration;
- do not add explicit lifetime syntax while fixing borrow checking;
- do not build a package manager while working on modules;
- do not add Windows support while fixing Linux runtime behavior.

Finish the current abstraction boundary first.

---

## Design Philosophy

Tarn should grow from evidence gathered through:

- compiler tests;
- diagnostics;
- benchmarks;
- real Tarn programs;
- editor/tooling usage;
- the xlinux port;
- future self-hosting.

Prefer real pressure from actual programs over speculative complexity.

A small coherent language is better than a large language made of prematurely copied features.
