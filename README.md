<p align="center"><img src="docs/assets/logo.svg" alt="Tarn" width="360"></p>

<p align="center">
<a href="https://github.com/tarn-lng/tarn/actions/workflows/ci.yml"><img src="https://github.com/tarn-lng/tarn/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License: MIT OR Apache-2.0"></a>
<img src="https://img.shields.io/badge/platform-Linux%20x86__64%20%7C%20ARM64-lightgrey" alt="Platform: Linux x86_64 | ARM64">
<img src="https://img.shields.io/badge/status-experimental-orange" alt="Status: experimental">
</p>

# Tarn

A memory-safe systems language without a mandatory GC: ownership and borrowing
checked at compile time, inferred lifetimes, small regular syntax, official
tooling. Linux x86_64 and ARM64 (aarch64).

```tarn
fn add(a i32, b i32) i32 {
    return a + b
}

fn main() {
    value := add(20, 22)
    print(value)
}
```

Status: ownership and executable drops are implemented; the Cranelift Linux x86_64/ARM64 backend supports `build` and `run` (ADRs 0027 and [0079](docs/adr/0079-linux-arm64.md)). See [docs/roadmap.md](docs/roadmap.md).

```bash
cargo build --release
./target/release/tarn check examples/02_add.tarn
cargo test
```

Docs: [language](docs/language.md) · [ownership](docs/ownership.md) ·
[errors](docs/errors.md) · [architecture](docs/architecture.md) ·
[ADRs](docs/adr/) · [packages](docs/packages.md) · [dependency security](docs/dependency-security.md). Editor: [editors/vscode](editors/vscode).

## Quickstart

Requires Rust 1.88+, a system `cc`, C11/libc headers and libm on Linux x86_64 or ARM64.

```bash
git clone https://github.com/tarn-lng/tarn && cd tarn
cargo build --locked --release
./target/release/tarn run examples/01_hello.tarn
./target/release/tarn init /tmp/hello-tarn && cd /tmp/hello-tarn
"$OLDPWD/target/release/tarn" run
```

## Packages

The integrated CLI supports `init`, `add`, `remove`, `update`, `fetch`, `deps`,
`verify`, `audit` and immutable local `publish`. Projects use `tarn.toml` and
`tarn.lock`; normal builds verify cached sources offline without changing the lock.
Choose a local registry or HTTPS origin; no public registry is configured by
default. Packages receive no installation/build hook authority. See
[the package guide](docs/packages.md) for a complete local workflow and the
explicit limits around publisher provenance and registry infrastructure.

## Native execution (Linux x86_64 and ARM64)

Requires system `cc`, C11/libc headers and libm. From a compiler checkout:

```bash
cargo run -p tarn -- run tests/native/pass/milestone.tarn
# 42
cargo run -p tarn -- build tests/native/pass/milestone.tarn -o /tmp/tarn-milestone
/tmp/tarn-milestone
```

`tarn build file.tarn` writes `file` beside the source; `tarn run` uses a temporary
executable and removes it. Scalars, concrete structs/enums/arrays, thin references,
direct calls and executable drops are supported. Reachable generic instances, borrowed
slices, borrowed nonescaping closures and escaping owned `move fn` closures also execute. Stored callables support shared, mutable and consuming invocation (ADR 0032). Borrowed dynamic interface calls also execute. Owned `spawn move fn` tasks execute on pthreads with consuming join and join on handle destruction (phase 11A). Transfer/Share bounds and scoped borrowing execute through the same ownership and loan pipeline (phase 11B). Mutex owners/guards and six concrete sequentially consistent atomics provide synchronization (phase 11C; ADR 0033). See [the complete Phase 11 report](docs/phase-11-report.md). Unmodeled external ABI
execution remains unsupported; unknown std APIs reject semantically with E3040.
See [ADR 0027](docs/adr/0027-native-backend.md) for exact ABI, layout and restrictions.

Native feature completion after phase 7 adds reachable generic specialization,
concrete generic ADTs, borrowed fat slices, nonescaping borrowed closures and
checked shifts/float-to-int casts. Unknown bootstrap stdlib APIs now fail with
E3040. See [ADR 0028](docs/adr/0028-native-feature-completeness.md) and
[ADR 0029](docs/adr/0029-checked-shifts-and-float-casts.md). Reusable callable
invocations borrow their environments; consuming callables move them. Native
optimization is enabled by default (Cranelift `speed`, runtime `-O2`), with
bounded inlining and proven-arithmetic optimization; see ADRs 0056–0057.
Borrowed dynamic interfaces and declaration-derived semantic contracts are covered
by [ADR 0030](docs/adr/0030-borrowed-dynamic-interfaces.md),
[ADR 0031](docs/adr/0031-declaration-semantic-contracts.md) and the
[phase report](docs/dynamic-interface-report.md).

Blocking and explicit nonblocking Linux TCP/UDP networking with level-triggered
epoll is available through `import "net"`, with owned sockets, safe borrowed
slices and Result errors. See [the API](docs/networking.md),
[the Phase 12B report](docs/readiness-report.md) and
[the echo example](examples/tcp_echo.tarn). Phase 12C adds
[manual suspended operations and a single-thread executor](docs/suspended-execution-report.md).
`async fn` and `await` lower onto that model (Phase 13,
[ADR 0037](docs/adr/0037-source-async-lowering.md)), with cooperative
`spawn_async` tasks, timeouts and buffered async I/O on a single-thread
executor; see [the async example](examples/28_async.tarn). Async support is
early: there are no async closures or blocks, `select`, cancellation or
multi-thread executors yet.

Scalar C FFI, raw pointers and static C callbacks are documented in the
[FFI guide](docs/ffi.md). Native linking does not verify or isolate foreign C.
For current public stdlib signatures, use `tarn stdlib --json`; the catalog
reflects the installed binary, not later source edits. Phase plans/reports
record historical scope and measurements; later guides and ADR extensions may
supersede their limitations.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
