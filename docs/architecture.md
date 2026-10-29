# Compiler architecture

```
Source → Lexer → Parser → AST → Resolve → Types → Typed IR
    → Move/init (6A) → Borrows (6B) → Post-drop IR (6C)
    → Cranelift backend → ELF object → system cc + runtime → executable
```

## Crates (Cargo workspace)

| Path | Crate | Role | Status |
|------|-------|------|--------|
| `compiler/diagnostics` | `tarn_diagnostics` | `SourceMap`, `FileId`, `Span`, `Diagnostic`, text + JSON rendering | done (v0) |
| `compiler/lexer` | `tarn_lexer` | tokens with spans, preserved newlines, comment trivia | done (v0) |
| `compiler/ast` | `tarn_ast` | syntax tree with spans + `NodeId`s, S-expr dump | done (v0) |
| `compiler/parser` | `tarn_parser` | recursive descent + precedence climbing, error recovery | done (v0) |
| `compiler/resolve` | `tarn_resolve` | scopes, symbols, `NodeId` side tables (docs/resolution.md) | done (v0) |
| `compiler/types` | `tarn_types` | type checking, local inference, mutability, exhaustiveness (docs/types.md) | done (v0) |
| `compiler/driver` | `tarn_driver` | load modules from disk, run phases, sort diagnostics | done (v0) |
| `compiler/ir` | `tarn_ir` | typed CFG IR, lowering, verifier, printer (ADR 0023) | done (v0) |
| `compiler/ownership` | `tarn_ownership` | 6A move/init + drop decisions (ADR 0024), 6B borrows + provenance (ADR 0025), 6C executable drop elaboration (ADR 0026) | done (v0) |
| `compiler/backend` | `tarn_backend` | post-drop IR → Cranelift → ELF object/link | Linux x86_64/ARM64, documented native subset (ADRs 0027/0079) |
| `stdlib/net` | — | owned TCP/UDP/Unix sockets, readiness/async I/O, and blocking TLS streams | implemented scoped APIs; see networking guide |
| `stdlib/fs` | — | owned blocking regular files, directories and explicit io.Error (ADR 0043) | done (15B Linux v0) |
| `stdlib/process` | — | owned children, piped/async I/O, bounded waits and one-shot handoffs | implemented scoped APIs (ADRs 0045/0070–0073) |
| `stdlib/ffi` | — | trusted raw pointers, CString, scalar access and static C callbacks (ADRs 0047/0080) | implemented subset |
| `stdlib/json` | — | ordinary bundled JSON writer/Encode and strict parser (ADR 0048) | done (17 v0) |
| `stdlib/core` | — | embedded prelude, ADTs, collections primitives and capability/interface declarations | implemented subset |
| `runtime` | — | embedded C primitives for storage, I/O, tasks, synchronization and abort-only faults | implemented native subset |
| `tools/cli` | `tarn` | compiler commands, tests, formatting, package commands, profiling and stdlib catalog | implemented; clean/cache commands deferred |
| `tools/packages` | `tarn_packages` | deterministic pure-source resolution, immutable local registry, HTTPS fetch, lock/cache verification; hands the driver a plain verified `PackageSet` (compiler crates never depend on it) | initial subset (ADR 0051) |
| `tools/fmt` | `tarn_fmt` | canonical whitespace over real lexer/parser with token/AST preservation checks (ADR 0050) | done (basic) |
| `tools/lsp` | `tarn-lsp` | stdio LSP: diagnostics, hover, definition, shared formatting, unsaved buffers | done within documented editor subset |

Rules:

- Dependencies only point "down" the pipeline. `diagnostics` depends on nothing.
- Only `backend` knows about Cranelift. The frontend never imports it.
- The parser produces syntax only (grammar: `docs/grammar.md`). Name binding, types and ownership state live
  in side tables keyed by node ids, never inside the AST.
- Every token, node and IR instruction carries a `Span`.
- Platform-specific syscalls live in `runtime`; native ISA selection, object
  emission and linker invocation live in `backend`, outside the frontend.

## Dependency policy

Each external crate needs a line in this table with a justification.

| Crate | Used by | Why |
|-------|---------|-----|
| `serde_json` | tools/lsp, tools/packages | JSON-RPC messages and robust JSON encoding/decoding |
| `url` | tools/lsp, tools/packages | Correct file URI encoding/decoding, including escaped paths |
| `cranelift-codegen/frontend/module/object/native` | backend | ISA/codegen, SSA builder, symbols, ELF object emission; pinned 0.125.3, dependency audit in ADR 0027 |

| `toml_edit` 0.25.15 | tools/packages | Standards-compliant editable manifests; parse/display only, preserves comments |
| `semver` 1.0.28 | tools/packages | SemVer constraints and prerelease rules rather than a custom incompatible resolver |
| `sha2` 0.10.9 | tools/packages | SHA-256 content and inventory verification without handwritten cryptography |

The package dependencies add 14 external transitive/direct crates in Cargo.lock;
`cargo tree -p tarn_packages --offline` records the focused graph. Reuse existing
JSON/URL crates. HTTPS uses trusted system curl rather than adding an HTTP/TLS
framework. See ADR 0051.

## Tarn package security

Implemented pure-source package operations use the single `tarn` CLI, version-free imports,
`tarn.toml` intent and a verified exact `tarn.lock` graph. Packages are data until
explicitly granted constrained execution authority. The initial manager includes deterministic resolution, local immutable publication
and HTTPS consumption. Public registry infrastructure, signed provenance and
build sandboxes are not implemented. See [the package guide](packages.md). Requirements and open choices are in
[dependency security](dependency-security.md).

The Cargo crates above and the embedded-runtime system `cc` invocation below are
existing trusted compiler/toolchain boundaries. They do not authorize future
package install hooks or imply current sandbox enforcement.

## Linking

Cranelift ELF object + embedded `runtime/native.c`, compiled/linked through system
`cc -std=c11 -O2 -fno-strict-aliasing -no-pie -pthread ... [-l<granted>] -lm`, run with
`LC_ALL=C`; `--link` libraries are validated build-time grants (ADR 0047). Linux x86_64 and ARM64;
requires a C toolchain with libc/libm headers. No installed runtime archive needed.
ABI, canonical layout, output paths and limits: [ADR 0027](adr/0027-native-backend.md).

## Caching

The native runtime object is already cached in `~/.tarn/artifacts/runtime/`,
keyed by runtime source, C flags, host architecture/OS and C compiler identity.
Cache failures fall back to a per-build runtime compile; setting
`TARN_NO_RUNTIME_CACHE` disables this cache. This does not cache application
compilation or inferred interfaces.

Use a content-addressed global store, conceptually `~/.tarn/registry/`,
`~/.tarn/sources/` and `~/.tarn/artifacts/`; `sources/<sha256>/` is implemented;
registry metadata/application-artifact caching remains open.
Deduplicate verified sources by content. Artifact keys include source hash,
compiler/toolchain identity, target, options and resolved dependency graph identity.
Interface hashes, names or mutable tags alone cannot establish cache identity.
Reuse must respect content verification and security policy, including authorized
build inputs. These are constraints for future application-artifact caching,
separate from the existing runtime object cache.
See [dependency security](dependency-security.md).

## Executable destruction boundary

After successful ownership checking, the driver produces `CheckResult.drops`: a
separate `tarn_ir::post_drop::Program` with explicit destruction plans, runtime
flags and verified CFG edges. A backend consumes it without reading move/borrow
results. See [ADR 0026](adr/0026-drop-elaboration.md). Inspect it with
`tarn ir file.tarn --drops`. Native code generation consumes this boundary; see [ADR 0027](adr/0027-native-backend.md).

Native feature completion after phase 7 adds reachable generic specialization,
concrete generic ADTs, borrowed fat slices, nonescaping borrowed closures and
checked shifts/float-to-int casts. Unknown bootstrap stdlib APIs now fail with
E3040. See [ADR 0028](adr/0028-native-feature-completeness.md) and
[ADR 0029](adr/0029-checked-shifts-and-float-casts.md). Native optimization is
enabled by default (ADR 0056), with bounded inlining and proven arithmetic
(ADR 0057). After specialization/inlining, bounded scalar-only loop peeling
reuses the IR range proof on executable control flow, followed by post-drop
verification; it does not change ownership/destruction decisions (ADR 0084).
Unsigned division width reduction (ADR 0082) and equivalent constant arithmetic
guards (ADR 0083) preserve the same runtime faults. Shared/mutable invocation borrows callable storage; consuming
invocation moves it. Owned closure destruction uses ordinary post-drop functions
(ADR 0032).
Borrowed dynamic references use private `(data, vtable)` pairs and resolved
frontend implementation IDs (ADR 0030). Declaration-derived passing/result
contracts and `borrows(...)` clauses expose invisible implementation promises
(ADR 0031); the backend still receives no move/borrow results.


## Native task boundary (phases 11A/11B/11C)

`Callee::TaskSpawn` records generated worker/result-destruction function IDs and
specialization arguments. Both functions pass through ordinary IR verification,
move/borrow checking and post-drop elaboration. The post-drop verifier validates
the callable/result shapes and owned argument. Backend specialization reserves
these functions through its existing work list. Small C ABI adapters bridge to
their private canonical ABI; the runtime never inspects capture/result types.

`Task<R>` has the canonical eight-byte private handle field declared in core;
its result is owned through runtime metadata, not an inline field. Native drop
of this resource waits and dispatches its verified result destruction function.
Explicit join moves result bytes to caller storage before releasing allocations.
The linker includes `-pthread`. Representation and abort semantics:
[ADR 0033](adr/0033-safe-native-tasks.md). Transfer/Share are bounded structural type-checker queries with explicit native
capability evidence. Scoped spawn carries a TaskScopeWitness reference, whose
ordinary loan flow prevents handle escape. Scope-owned handles and discarded
Task temporaries retain worker loans until consuming join/destruction. Lowering
completes task-containing resources before borrowed storage ends, including early
exits. Synchronization uses owned Mutex containers, ordinary guard loans and concrete atomic storage; no backend ownership queries are introduced.
See [the 11B report](scoped-tasks-report.md) for conservative restrictions.


## Synchronization boundary (phase 11C)

Core declares Mutex<T>, MutexGuard<T> and AtomicBool/I32/I64/U32/U64/Usize.
Resolved declaration identities supply trusted resource and capability contracts.
Mutex owns a private native pointer and an inline T payload. A guard stores private
native/payload address lanes but semantically carries its mutex loan. Declaration-
aware loan containment propagates guards through functions and aggregates. Guard
destruction is a liveness use, and a whole move transfers its held loans. Ordinary
NLL prohibits moving/destroying the owner or escaping a guard-derived reference.

Existing move/init classification and executable Value/Guard destruction plans
select exactly which complete resources are destroyed. Native type glue unlocks
a live guard; complete Mutex glue recursively destroys its T using the existing
canonical destruction path, then destroys/frees the native mutex. No alternate
runtime owner bit or backend move/loan query is involved. Private pthread helpers
never inspect or destroy T. Guard read has an explicit Copy obligation in the
trusted intrinsic catalog, reusing normal generic checking without conditional
method-owner syntax. value() borrows payload access and replace() moves values.

Concrete atomic owners hold private heap-backed C11 atomic scalar storage. All
operations are sequentially consistent, and checked fetch arithmetic uses CAS
loops without signed C overflow. Public compare_exchange returns a strong success
boolean; failed comparison leaves storage unchanged. No unsynchronized projection
of the scalar exists. See [ADR 0033](adr/0033-safe-native-tasks.md) and
[the Phase 11 report](phase-11-report.md) for the API, tests and v0 limits.

## Blocking native networking

Phase 12A loads the embedded `stdlib/net` module with explicit trusted-source
metadata. Public socket/error/address types remain outside core. Ordinary Tarn
implements the application API, retries, error mapping and write_all; private
C helpers bridge Linux socket/getaddrinfo ABI and capture native status.
Transfer/not-Share contracts belong to resolved native declarations. Socket
destruction executes verified post-drop plans; the backend does not infer owner
liveness. The IR verifier validates private signature, owner and outcome shapes.
Slice pointers/lengths are explicit ABI lanes, never escaping runtime storage.
See [ADR 0034](adr/0034-blocking-networking.md) and [networking](networking.md).

## Readiness boundary (Phase 12B)

The trusted net catalog adds Poll and exact Token/Event bridge shapes. Poll drop
uses existing post-drop resource destruction; no ownership registry is added.
C owns epoll bookkeeping with nonreused tokens and Linux socket cookies. Public
Tarn code controls mode changes, connect progression, WouldBlock and monotonic
EINTR retry policy. No application pointer remains pending after a syscall.
See [ADR 0035](adr/0035-nonblocking-readiness.md).

## Manual suspended execution (Phase 12C)

Operation<R> stores an ordinary owned mutable closure and a loan-bearing Waker.
Existing capture metadata exposes across-poll storage and loans; the generic
loan-resource destruction/liveness mechanism includes Waker-containing holders.
Execution owns Poll/mechanical wake bookkeeping; a separate Executor owns a
bounded operation table, avoiding self-reference. Task insertion consumes and
returns that table through ordinary provenance. Native helpers only manage
identity, registration and coalesced wake bits. Tarn schedules fair turns and
implements all network state transitions.
See [ADR 0036](adr/0036-suspended-execution.md) and
[the Phase 12C report](suspended-execution-report.md).

Phase 13 source async (ADR 0037): lowering keeps an async body as an ordinary IR
function with explicit `Suspend { resume, abandon }`/`Abandon` edges, so move,
borrow and drop elaboration analyze it unchanged. After verified drop
elaboration, `ir::async_frame` relocates suspension-live locals and all flags
into a stable heap frame and makes the state dispatch explicit. The backend only
places slots at frame offsets and emits poll/destruction adapters.

## Lexical paths (Phase 15C)

`stdlib/path` is an ordinary Tarn module over owned UTF-8 strings. Bundled source
is a fallback, not a trusted intrinsic provider; local modules can override it.
Path capabilities, loans and destruction are entirely structural. No runtime
or backend ABI changes are needed. Applications pass borrowed path text to fs.
Explicit lexical normalization is separate from filesystem resolution. See
[ADR 0044](adr/0044-lexical-owned-paths.md).

## Blocking child processes (Phase 15D)

`stdlib/process` is a trusted layer above io. Tarn owns configuration, child
completion, capture buffers and two native pipe-reading tasks. Private bridges
use posix_spawnp, GNU libc spawn actions, read, waitpid and kill. Dedicated
resolved resource/intrinsic metadata is structurally verified, including
consuming signatures binding resource identities. Process drop waits; pipe
drop closes. Backend code executes verified plans without inferring ownership.
There is no shell interpreter, process registry, detach or async integration.
See [ADR 0045](adr/0045-blocking-processes.md).

## Bounded HTTP/1.1 (Phase 16)

`stdlib/http` is an ordinary bundled Tarn module, with local override and editor
source support but no native privilege. It owns bounded request/response/header
bytes and a Connection containing TcpStream plus both existing net buffers.
Protocol states govern transaction ordering, never descriptor liveness. Parsing,
framing, preflight, deadlines and response encoding remain in Tarn; runtime/backend
execute only existing I/O, timers, wake operations and verified post-drop.
The concrete-result runtime.run_timeout consumes a local Operation and writes
stage data through ordinary caller loans. No generic result can escape a destroyed
manual poller. Native test tracing observes frames/buffers/wakers/task allocations
without providing an ownership registry. See [HTTP](http.md) and
[ADR 0046](adr/0046-bounded-async-http.md).

## Application ergonomics (Phase 17)

`stdlib/json` and the `Builder`/`http.serve` additions are ordinary Tarn
with no runtime or intrinsic changes. Two backend changes support them: `[0]T`
markers no longer inspect `T` for layout, and vector element destruction calls
on-demand per-type functions, so owned types may recurse through `Vec`. The
type checker lets a non-consuming closure literal adopt an expected `mut fn`.
The native bind bridge sets SO_REUSEADDR for TCP. See
[ADR 0048](adr/0048-application-ergonomics.md).

Builder originally lived in `string`; ADR 0077 moved it into `core`, so current
programs use `Builder.new()` (or `core.Builder.new()`), not `string.Builder`.

## General-purpose foundations (Phase 31)

Ordinary bundled `collections` and `console` modules add owned Map/Set,
comparator algorithms and non-owning standard-stream views. Hash/Eq primitive
impls are confined to core; resolved implementation targets are reused by static
and dynamic backend dispatch. Explicit reference places use normal IR Deref
projections and ownership/drop checking. A small console C adapter only converts
SIGPIPE to a reported errno using a thread-local signal mask. Clocks use the
existing FFI layer and the supported LP64 Linux timespec layout; no scheduler or ownership catalog
is added. See [ADR 0060](adr/0060-general-purpose-foundations.md).

## C interoperability

Static C callbacks (ADR 0080) use explicit function identities in typed IR.
Post-drop verification validates the target; specialization keeps its ordinary
Tarn body reachable. The backend emits a static adapter using the same scalar
C ABI classifier as external calls, without generated C source or a hidden
closure environment. Unsafe scalar pointer reads/writes have a separately
verified trusted declaration contract and never manufacture Tarn references.
The compiler still uses the embedded C runtime and system linker. Neither
unsafe FFI nor native linking verifies or isolates foreign implementations.

## Native platforms

The backend supports native builds on little-endian Linux x86_64 and ARM64
(aarch64). They share the 64-bit layout and Linux runtime; Cranelift selects
the host ISA and calling convention. Distribution uses per-architecture archives
and a multi-platform builder image. See [ADR 0079](adr/0079-linux-arm64.md).
