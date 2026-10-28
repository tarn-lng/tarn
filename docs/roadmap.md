# Roadmap

Work proceeds in vertical slices. A phase is done only when it is functional,
tested and documented.

| # | Phase | Status |
|---|-------|--------|
| 0 | Design docs, examples, name, extension, workspace | **done** |
| 1 | Lexer | **done** |
| 2 | Parser + AST | **done** |
| 3 | Name resolution (incl. shadowing rules, ADR 0009) | **done** |
| 4 | Types (primitives, structs, enums, generics, interfaces, mutability) | **done** |
| 5 | Diagnostics (text + JSON, golden tests) | done for phases 1–4 |
| 6 | Typed IR (ADR 0023) | **done** |
| 6A | Move/init checking on the IR (ADR 0024) | **done** |
| 6B | Borrow checking on the IR (ADR 0025) | **done** |
| 6C | Drop elaboration (ADR 0026) | **done** |
| 7 | Cranelift backend, `tarn build`/`run` (ADR 0027) | **done for the initial Linux x86_64 subset** |
| 7A | Native feature completeness (ADRs 0028–0029) | **done within documented subset; dynamic dispatch deferred** |
| 8 | Native feature completeness (ADRs 0028–0029) | **done within documented subset** |
| 9 | Borrowed dynamic interfaces and semantic contracts (ADRs 0030–0031) | **done within documented subset** |
| 10 | Callable invocation and owned closures (ADR 0032) | **done within documented subset** |
| 11 | Safe native concurrency (ADR 0033) | **done within documented v0 subset: tasks, capabilities, scoped loans, Mutex guards and sequentially consistent atomics** |
| 12A | Blocking native TCP/UDP, owned sockets, resolution and Result errors (ADR 0034) | **done within documented blocking Linux v0 subset** |
| 12B | Nonblocking I/O and level-triggered epoll (ADR 0035) | **done within documented Linux v0 subset** |
| 12C | Manual suspended operations and single-thread execution (ADR 0036) | **done within documented bootstrap limits; no async syntax** |
| 13 | Source async lowering over Phase 12C (ADR 0037) | **done within documented v0 limits; see [the Phase 13 report](phase-13-report.md)** |
| 14A | Vec, cooperative async tasks, AsyncTask join and executor storage (ADR 0038) | **done; [report](phase-14a-report.md)** |
| 14B | Monotonic timers and timeouts (ADR 0039) | **done; [report](phase-14b-report.md)** |
| 14C | Buffered async I/O and a concurrent buffered server (ADR 0040) | **done; [report](phase-14c-report.md)** |
| 14R | Trusted stdlib module layers: io, time, net, runtime (ADR 0041) | **done; [report](phase-14r-report.md)** |
| 15A | Owned UTF-8 string essentials (ADR 0042) | **done; [report](phase-15a-report.md)** |
| 15B | Owned blocking filesystem (ADR 0043) | **done; [report](phase-15b-report.md)** |
| 15C | Owned lexical paths (ADR 0044) | **done; [report](phase-15c-report.md)** |
| 15D | Owned blocking child processes (ADR 0045) | **done; [report](phase-15d-report.md)** |
| 16 | Bounded async HTTP/1.1 server over buffered I/O | **done within documented server subset**: [16A–F plan](phase-16-plan.md), [report](phase-16-report.md), [ADR 0046](adr/0046-bounded-async-http.md) |
| 17 | Application ergonomics: Builder, json, http.serve and response helpers (ADR 0048) | **done**: [plan](phase-17-plan.md), [report](phase-17-report.md); Builder now lives in core (ADR 0077) |
| 18 | Minimal native C FFI: scalar extern calls, raw pointers, `--link`, SQLite acceptance (ADR 0047) | **done within documented v0 limits**: [plan](phase-18-plan.md), [report](phase-18-report.md) |
| 19 | CLI: command surface, `test`, `--watch`, `clean` | implemented (19A/B/C); `clean` explicitly deferred; [plan](phase-19-plan.md), [ADR](adr/0049-integrated-test-runner.md) |
| 20 | Formatter (basic) | **done within documented basic subset**: [plan](phase-20-plan.md), [report](phase-20-report.md), [ADR 0050](adr/0050-conservative-source-formatting.md) |
| 21 | VS Code extension (TextMate) | **done within documented editor subset**: [plan](phase-21-plan.md), [report](phase-21-report.md); highlighting, snippets, compiler LSP and shared formatting |

| 22 | Verified pure-source package manager | implemented initial subset: [guide](packages.md), [ADR 0051](adr/0051-verified-source-packages.md); public registry infrastructure and signed provenance deferred |
| 23 | Explicit error conversion: core combinators, variant constructors as functions, then measured syntax decision | **done**: [plan](phase-23-plan.md), [report](phase-23-report.md), [ADR 0052](adr/0052-explicit-error-conversion.md) |
| 24 | `os` arguments/environment, `tarn run -- args`, `xlinux doctor` port (milestone 5) | **done**: [report](phase-24-report.md), [ADR 0053](adr/0053-process-arguments-and-environment.md) |
| 25 | Port-driven stdlib: `os.exit`, `os.can_execute`, `os.disk_space`, `io.Error.from_os`, `Command.env` | **done**: [report](phase-25-report.md), ADR 0053 (Phase 25 extension) |
| 26 | Less ceremony: redundant casts removed, literals as `&string`, `Self` + `core.Eq` for `==` | **done**: [report](phase-26-report.md), [ADR 0054](adr/0054-less-ceremony.md); formatting was deferred at this checkpoint and is now implemented (ADR 0077) |
| 27 | Static string literals: no allocation per literal evaluation | **done**: [report](phase-27-report.md), [ADR 0055](adr/0055-static-string-literals.md) |
| 28 | Native performance vs Go: optimizer on, trace switches cached, bulk primitives, constant-divisor checks | **done**: [report](phase-28-report.md), [ADR 0056](adr/0056-native-performance-baseline.md) |
| 29 | Fair Go benchmarks, thread cache for `spawn`, proven integer arithmetic | **done**: [report](phase-29-report.md), [ADR 0057](adr/0057-proven-arithmetic.md) |
| 30 | `tarn test --bench`, `tarn profile`, multi-core `http.serve_parallel` | **done**: [report](phase-30-report.md), [ADR 0058](adr/0058-multicore-executor-design.md), [ADR 0059](adr/0059-performance-tooling.md) |

| 31 | Port-driven Hash/Map/Set, comparator algorithms, console and clocks | implemented scoped subset: [report](phase-31-report.md), [ADR 0060](adr/0060-general-purpose-foundations.md); partial network metrics port |

| 32 | Offset text traversal, integer Builder allocation removal and UTF-8 CLI | implemented scoped subset: [report](phase-32-33-report.md), [ADR 0061](adr/0061-offset-text-traversal.md) |
| 33 | Bounded system HTTPS and selected-ticket classification CLI | implemented scoped subset: [report](phase-32-33-report.md), [ADR 0062](adr/0062-bounded-system-https.md); local TLS acceptance, not a full Go application port |
| 34 | Agent-tooling foundations and evidence-led stdlib ergonomics | implemented foundations: [report](phase-34-foundations-report.md), [agent guide](llm-guide.md); semantic queries, structured fixes and MCP remain [planned](phase-34-plan.md) |
| 35 | Persistent TLS client streams (`net.TlsStream`) over libcurl connect-only | implemented: [ADR 0066](adr/0066-persistent-tls-streams.md); local TLS echo acceptance; WebSocket follows in phase 36; async handshake and ALPN remain outside scope |
| 36 | RFC 6455 WebSocket client (`websocket`) as ordinary Tarn over `TlsStream` | implemented: [ADR 0067](adr/0067-websocket-client.md), [guide](websocket.md); local TLS WebSocket acceptance; no compression or async variants |

## Milestones

1. `add(20, 22)` program compiles to a native binary through the full pipeline.
2. Structs + move semantics; `print(a)` after `b := a` is rejected (E4001).
3. Borrowing basics with conflict detection.
4. Result/Option/enums/match/try.
5. fs/path/process ─▶ start porting `xlinux doctor`.

The original milestone-2 checkpoint called for an architecture, syntax and
ownership review before expanding the language. It is historical sequencing,
not a claim that borrowing, generics or the stdlib are still unimplemented.

## Native C interop extension

Static scalar C callbacks and unsafe scalar pointer access extend the existing
FFI: [ADR 0080](adr/0080-static-c-callbacks.md), [guide](ffi.md). This removes
the need for handwritten C callback adapters in integrations such as GTK.
Guaranteed C aggregate layouts, variadic calls, capturing callbacks and
automatic foreign-resource destruction remain separate design work.

## Open design questions

- Value-producing `if` (currently: no).
- Automatic error conversion in `try` (currently exact match only; explicit
  `map_err`/`ok_or` combinators already work).
- Method-owner syntax for `[]T` / `[N]T` so `len` can move to `core` (ADR 0020).

- Well-formedness of type annotations w.r.t. bounds (`x: Point<string>`).
- Exact iteration protocol for `for x in xs`.
- Reachable monomorphization is implemented (ADR 0028); shared code remains a future measured alternative.
- Interfaces: explicit `impl I for T` chosen; revisit after xlinux.
- A general stored-reference iterator framework (current loops copy Copy
  elements and borrow non-Copy elements when iterating borrowed collections).

Native feature completion after phase 7 adds reachable generic specialization,
concrete generic ADTs, borrowed fat slices, nonescaping borrowed closures and
checked shifts/float-to-int casts. Unknown bootstrap stdlib APIs now fail with
E3040. See [ADR 0028](adr/0028-native-feature-completeness.md) and
[ADR 0029](adr/0029-checked-shifts-and-float-casts.md). Borrowed dynamic dispatch now executes (ADR 0030); owned dynamic objects remain
rejected. Native optimization is enabled by default (ADR 0056), with bounded
inlining and proven arithmetic (ADR 0057). Phase 10 adds shared, mutable and
consuming callable modes, `move fn` ownership captures and safe returned owned
closures (ADR 0032). Conditional `Eq` implementations for `Option`/`Result`
are implemented by ADR 0078.


## Dependency management and supply-chain security

The initial manager implements integrated commands, manifest/lock separation,
SemVer resolution, immutable local publication, HTTPS consumption, verified
content-addressed source storage and dependency-scoped imports. See
[package management](packages.md). [Dependency security](dependency-security.md)
remains the long-term contract. Public registry deployment, signing/provenance,
federation, build sandboxes, features and incompatible-major coexistence remain
open. Dependencies receive no build execution authority.

Blocking networking is covered by Phase 12A and [ADR 0034](adr/0034-blocking-networking.md);
readiness by [ADR 0035](adr/0035-nonblocking-readiness.md) and
[the Phase 12B report](readiness-report.md).
The manual execution model is covered by [ADR 0036](adr/0036-suspended-execution.md)
and [the Phase 12C report](suspended-execution-report.md); source-level async
lowering by [ADR 0037](adr/0037-source-async-lowering.md) and
[the Phase 13 report](phase-13-report.md).
