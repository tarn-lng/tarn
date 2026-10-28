# Contributing to Tarn

Thanks for helping! Tarn is experimental and currently targets Linux x86_64 and
ARM64 (aarch64). Issues and pull requests are welcome in English or Spanish.

## Setup

- Rust 1.88 or newer (the workspace declares `rust-version = "1.88"`).
- A system C compiler (`cc`), C11/libc headers and libm (used by the native backend).

```bash
cargo build --locked --release
cargo test --locked --release --workspace
./target/release/tarn run tests/native/pass/milestone.tarn   # prints 42
./target/release/tarn check examples/02_add.tarn
```

All tests must pass before a pull request is merged; CI runs the same commands
plus a `tarn check` smoke over every `examples/*.tarn` and
`tools/ci/run-examples.sh`, which runs each example and diffs its stdout against
`tests/examples/<name>.out`. A new example needs either that file or a line
with a reason in `tests/examples/skip.txt` (servers, host-dependent or
nondeterministic output, examples that need input).

## Where things live

- `compiler/`: lexer, parser, resolver, types, IR, ownership checker, Cranelift backend.
- `runtime/` and `stdlib/`: native runtime and standard packages (`fs`, `io`, `net`, `http`, ...).
- `tools/`: CLI, formatter, LSP, package manager, release tooling.
- `editors/vscode`: VS Code extension.
- `docs/` and `docs/adr/`: language docs and architecture decision records.
- `tests/native/pass` and `tests/native/fail`: end-to-end fixtures.

## Rules

- Read [AGENTS.md](AGENTS.md): it applies to humans and AI agents alike.
- Language/semantics changes need an ADR in `docs/adr/` (follow the latest one's format) before code.
- New diagnostics get a stable code documented in [docs/errors.md](docs/errors.md); never renumber existing codes.
- Every fix ships with a test (a `tests/native/pass` or `tests/native/fail` fixture, or a Rust unit test).
- AI-assisted PRs are welcome; you must have run the tests and understand the diff.
- Packages must never gain install/build hooks.

## Pull requests

Small, focused, one topic. Describe the behavior change and link the issue.
Issues labeled `good first issue` are a good place to start.

Before you start on an issue, comment on it to say you're taking it, and a
maintainer will assign it to you. If an issue is already assigned, pick another
one or ask in the issue. If an assigned issue has had no activity for two weeks,
it's fair to ask whether it's still in progress. This keeps two people from
solving the same thing at once.

## Code of conduct

This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).

See [CONTRIBUTORS.md](CONTRIBUTORS.md) for everyone who has contributed, and how to add yourself.

## License

Unless you explicitly state otherwise, any contribution you intentionally submit
for inclusion in Tarn is dual-licensed under the MIT and Apache-2.0 licenses,
without any additional terms or conditions.
