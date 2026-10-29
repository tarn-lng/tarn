# Phase 34: Agent tooling

Status: implementation in progress. The detailed agent-tooling milestones below remain planned unless explicitly marked complete.

## Motivation

Tarn is a new language and is absent from LLM training data. Coding agents
therefore depend on the compiler for facts, not on memorised idioms. Today an
agent gets text diagnostics with a free-text `help`, JSON Lines from
`check/test/run --json`, and an LSP with hover, definition and full-document formatting. Inferred
lifetimes make this worse: signatures do not show what a result borrows from.

Principle (AGENTS.md, Tooling): every capability below reuses the real
frontend and existing compiler tables. No independent parser, resolver or type
checker is introduced. Output is machine-readable first, human-readable second.

## Documentation audit

The initial plan understated existing support. The LSP already advertises
hover, definition and full-document formatting; formatting is not a future
feature. Prose near-match suggestions and Option/Result combinators already
exist. Structured edit suggestions, semantic queries, references/completion,
MCP and explain are still absent. Public stdlib discovery now exists as the
source catalog described below, but it does not complete semantic 34C/34D.

## Delivered foundation

- Collector fixes and equivalent Go source; deterministic reset/PID tests.
- rfind, blocking sleep, recoverable map helpers and allocation-free cursors.
- Public embedded API catalogs (`tarn stdlib`) with doc comments and SHA-256
  identity, plus an agent guide using executable reference programs.
- Finite decimal conversion, ASCII text/replacement/formatting and bounded
  console/CSV input, validated by a complete CSV-to-JSON CLI.

These do not mark 34A-34J complete. Spelling suggestions already exist as prose;
structured edit suggestions, semantic queries/provenance, explain, MCP, LSP
expansion, fresh-agent evaluation and desktop adapters remain separate work.

## Evidence-led execution order

The independent `status_device` port exposed discovery friction and missing
basic APIs. Its observations are evidence, not a complete language audit.
`Option.unwrap_or`, `Option.ok_or`, `Option.map`, `Result.map_err`,
`Result.unwrap_or`, `Map.drain`, `Instant`, `now_utc` and
`tarn profile` already exist. GTK/D-Bus integration and fair Go comparisons
were not demonstrated. Never interpret an old embedded compiler as the current
source API.

Deliver the following boundaries in order, with independent validation:

1. Correct the collector: counter resets, PID reuse, runtime page size,
   documented CPU semantics, deterministic fixtures and an equivalent Go
   implementation excluding `smaps_rollup`.
2. Basic APIs: last-match string search, blocking sleep, recoverable map lookup
   within v0 aggregate-reference restrictions and allocation-free entry cursors.
3. Agent discovery: embedded-source API catalog, build/stdlib identity, checked
   guide examples and near-match diagnostics. Catalogs use the real parser;
   semantic queries must use real semantic tables.
4. Text/CSV CLI: finite decimal conversion, text replacement/ASCII casing,
   explicit console buffering and bounded line/CSV input. Benchmark allocation
   pressure before changing language syntax.
5. Option/Result ergonomics: evaluate ordinary helpers against the collector,
   CSV CLI and classifier. Do not introduce expression blocks or implicit
   numeric/error conversions from a single experiment.
6. Re-run a context-free agent port with a fixed specification and hidden
   correctness tests. Record compile attempts, repairs and elapsed time; do
   not invent results without an actual agent evaluation.
7. Complete the bounded HTTPS classifier workflow: pagination, rate-limit
   responses, caching and owned native-task concurrency, preserving read-only
   application behavior.
8. Desktop feasibility: a small D-Bus adapter prototype before proposals for
   C structs/callbacks or GUI. This is a separate phase, not a promise of full
   desktop support.

Native platforms are Linux x86_64 and ARM64 (ADR 0079); the original measurements
in the foundation reports used x86_64. All measurements compare equivalent
workloads, and failed or unexecuted validation is reported explicitly.

## Sub-phases

Order reflects value per effort. Each sub-phase is independently shippable and
needs tests, docs and an ADR where a contract is created.

### 34A: Structured diagnostic fixes

- Extend `Diagnostic` with `suggestions`: list of `{message, edits[{span,
  replacement}], applicability}` where applicability is `machine_applicable`,
  `maybe_incorrect` or `manual`.
- Keep `help` as prose. Suggestions are additive; existing golden output does
  not change unless a suggestion is rendered.
- Emit them in the JSON format and render them in text as `= fix:`.
- Start with a small set of mechanical cases: missing `try`, missing `&` or
  `&mut`, `var` needed for mutation, missing import, unknown name with a near
  match. Do not add a suggestion unless applying it is verified by a test that
  re-checks the edited source.
- Add `tarn check --fix` only for `machine_applicable` edits, after the JSON
  contract is stable. Never apply `maybe_incorrect` automatically.
- ADR: diagnostic suggestion contract. Document the schema in `errors.md`.

### 34B: `tarn explain`

- `tarn explain E4001` prints the meaning, a failing example, a corrected
  example and related codes. `--json` gives the same data.
- Source of truth is one registry next to the code table, so a code cannot be
  registered without an explanation. A test fails if any registered code lacks
  an entry and if any example does not produce the stated code.
- Examples are compiled by the test suite, so they cannot rot.

### 34C: `tarn query` (semantic queries, JSON)

Read-only commands over the real compiler tables, all with `--json`:

- `tarn query symbols [module]`: declarations with kind, visibility, span.
- `tarn query type <file>:<line>:<col>`: type of the expression at a position.
- `tarn query refs <file>:<line>:<col>`: resolved references to a symbol.
- `tarn query sig <path>`: signature of a function or method, including the
  inferred **provenance** of the result (for example "may borrow from
  parameter 0") and Copy/Transfer/Share capabilities of the types involved.
- `tarn query module <name>`: public API of a module, including embedded stdlib.

Provenance exposure is the main value: it makes inferred lifetimes visible
without introducing lifetime syntax. Must only report what the borrow
checker already computed. Conservative results are labelled as such.

### 34D: `tarn doc`

- Generate API documentation for a module or package from declarations and doc
  comments, in text and JSON. Include provenance from 34C.
- Embedded stdlib is covered, so an agent can look up an API without reading
  runtime or stdlib sources.
- Doc comment conventions are specified before generation. Examples in doc
  comments are checked by `tarn test`.

### 34E: LSP expansion

Extend the existing server, reusing the same tables as 34C:

1. `textDocument/references`, `documentSymbol`, `workspace/symbol`.
2. `signatureHelp` and basic scope-aware `completion`.
3. `codeAction` backed by 34A suggestions (no separate fix logic).
4. Inlay hints for inferred types and result provenance.
5. `rename`, last, only if reference resolution proves exact.

Update the VS Code extension capability documentation. Each feature needs a
real stdio smoke test, as in Phase 21.

### 34F: MCP server

- `tarn mcp` (stdio) exposing tools: `check`, `test`, `fmt`, `explain`,
  `query_*` and `doc`. It wraps the driver APIs and the same JSON, not
  shell-parsing of CLI output.
- Read-only by default. Tools that execute user code (`run`, `test`) require an
  explicit flag and report resource limits (timeout, output cap).
- Small justified protocol dependency is allowed (AGENTS.md, LSP policy
  applies to editor and agent tooling); otherwise implement minimal JSON-RPC
  shared with the LSP.

### 34G: Stable machine output

- Add `schema_version` to every JSON record kind and publish JSON Schemas in
  `docs/schema/`. Tests validate real output against them.
- Mark cascading diagnostics (`primary: true/false`, or `caused_by`) so agents
  fix root causes first.
- Define a compatibility policy: additive fields allowed; removals bump the
  version.
- Do this early in practice: 34A and 34C should be written against the schema
  from the start, so land the schema skeleton alongside 34A.

### 34H: Safe execution for agents

- Resolve `clean` (currently a stub): builds go to a dedicated build directory
  (for example `.tarn/build/`) with an inventory, so `clean` can delete exactly
  what Tarn created and never guesses.
- Document and test that `run`/`test` use isolated process groups, timeouts and
  bounded output capture.
- ADR required because it changes where `build` writes by default.

### 34I: Agent-facing documentation and templates

- `docs/llm-guide.md`: short cheatsheet of syntax, ownership rules, error
  handling, and the common mistakes with their fixes, every snippet compiled by
  the test suite.
- `tarn init` writes an `AGENTS.md` template for user projects.
- A corpus of failing programs with the expected code and the fix, reusable by
  34B and 34J.

### 34J: Agent evaluation

- A benchmark of tasks (specification plus hidden tests) measuring whether an
  agent produces a program that compiles and passes on the first try, and the
  number of repair iterations given diagnostics.
- Run with and without 34A–34D to measure which tooling actually helps.
  Results guide further tooling work; features without measured benefit are
  not expanded.
- Harness is model-agnostic and stores results as JSON in `evidence/`.

## Dependencies

```text
34G (schema) ─▶ 34A ─▶ 34E(codeAction)
        │        └────▶ 34B
        └──▶ 34C ─▶ 34D
                └──▶ 34E(refs/symbols/hints)
34A,34B,34C,34D ─▶ 34F (MCP)
34I ─▶ 34J        34H independent
```

## Suggested order

1. 34G skeleton + 34A (fixes) + 34B (`explain`).
2. 34C (`query`) and 34D (`doc`).
3. 34F (MCP) and 34E (LSP).
4. 34H, 34I, then 34J to measure.

## Acceptance

- Every new command has `--json` output validated against a published schema.
- Golden tests for text and JSON; regression test for any bug found.
- Every suggestion, explanation example and guide snippet is executed by tests.
- No new compiler semantics. Any tool needing a fact the frontend lacks first
  gets that fact in a compiler table, not a reimplementation in tooling.

## Non-goals

- Natural-language error localisation.
- A built-in LLM client or network calls from the compiler.
- Auto-applying non-mechanical fixes.
- Debugger, marketplace publication, Windows or macOS support.
- Changing language syntax or semantics.

## Risks

- Suggestions that compile but change meaning: restrict to mechanical cases and
  verify by re-checking.
- Schema churn: freeze the schema before building consumers.
- Provenance reported more precisely than the checker guarantees: label
  conservative results and never invent relationships.
- Scope growth in the LSP: rename and completion stay behind exact resolution.
