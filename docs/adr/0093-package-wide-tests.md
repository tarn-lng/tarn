# ADR 0093: package-wide tests and target selection

Status: accepted and implemented. Amends ADR 0049 (discovery and reporting).

## Context

ADR 0049 discovered tests only in the entry module and in `*_test.tarn` files
next to it, and generated the dispatcher by appending imports to the entry's
text. Porting tarn-ui showed four problems:

- A test in a subdirectory was ignored without any message, and `tarn test`
  reported `0 passed` with exit code 0.
- Pointing `tarn test` at a nested module moved the import root, so
  `import "gtk/util"` stopped resolving even though the application
  resolves it.
- An appended import of a test file that had no tests triggered W2003 at a
  line that does not exist in the user's file. In `--bench` mode every test
  file triggered it.
- A failure was reported as `terminated by signal 6`, followed by the
  captured streams one after the other. An `Err` was split over several
  lines.

## Decision

- **Discovery.** `*_test.tarn` files anywhere below the target directory are
  test roots. Hidden directories, `target`, `node_modules` and nested
  packages (any directory with its own `tarn.toml`) are skipped. The package
  entry contributes its tests when it lies inside the target. For a directory
  target, package modules below it that the tests load also contribute their
  inline `test_*`. No directory layout is required.
- **Target.** A directory runs everything below it. With no argument, the
  target is the package root. A file runs only its own tests, plus
  `x_test.tarn` when the file is a module `x.tarn`. `--filter` still selects
  by substring.
- **Imports.** The module root is the package root (the enclosing
  `tarn.toml`), or the target's directory when there is no package. Tests
  import exactly as the application does.
- **Generation.** The harness never edits the user's import list. A virtual
  root module (`__tarn_tests__.tarn`) holds the dispatcher. It reaches each
  test module through its own virtual shim (`__tarn_test_shim_<k>.tarn`), so
  `gtk/protocol_test` and `tests/protocol_test` do not collide. Test names are
  module paths (`gtk/protocol_test.test_encode`). The per-test wrappers are
  still appended to their own modules, so private tests stay callable, and
  their prelude references are bound as before. Warnings whose primary span
  lies in generated text are dropped.
- **Reporting.** A failing `Result` test panics with a single line,
  `test returned Err(Kind.Refused(3))`, built from the value's `Display`
  where it has one. The text report prints one block per failure: the reason
  (`panicked: …`, `timeout after …`, `exited with code …`, or the signal when
  no panic message exists), then labelled stdout and stderr. JSON records add
  `message` with the panic message. An empty selection prints `no tests
  found`.

## Consequences

- `tarn test path/to/x.tarn` no longer runs unrelated sibling test files.
  Run the directory for that.
- Tests that imported siblings by short name from a nested directory (for
  example `import "protocol"` inside `gtk/`) must use the package path
  (`import "gtk/protocol"`).
