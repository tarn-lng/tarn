# ADR 0094: the `testing` library and caller locations

Status: accepted and implemented. Amends ADR 0049, which ruled out
assertions and a test module.

## Context

Tests could only fail with `panic` or by returning `Err`. Ports wrote their
own comparison helpers, and could not skip tests whose environment was
missing (no display for GTK). They could not report more than one failure
per run. No panic said where it happened, not even a plain `panic` or
`unwrap`. Go (`testing.T`) and JS runners (`expect`, `test.skip`) show what
agents expect: assertions that show the expected and actual values and the
failing line, checks that continue, and skips.

## Decision

**Panic locations.** IR lowering receives the source map. Every `panic`
passes its call site, and the runtime prints `panic: <message>` followed by
`    at <path:line:column>`. The path is relative to the working directory.

**Caller locations.** `core.Location` is a source position. When a
function's last parameter has type `Location`, a call may omit it. The type
checker records the call, and lowering passes the call site. `panic(message,
&at)` reports a given location instead of its own call site. This makes
helpers report their caller's line. `Option`/`Result` `unwrap` and `expect`
use it. The value is a static string, so nothing is allocated on the success
path.

**`testing` module.** A test may take one parameter, `t &mut testing.T`. The
generated wrapper creates `T`, calls the test, then calls `t.finish()`.

- `eq`/`ne` (any `Eq + Display`), `check`, `some`/`none`, `ok`/`err` and
  `fail` record a failure at the caller's line and continue.
- `require` stops the test.
- `skip(reason)` stops it as skipped.
- Failures and skips are written to stderr as `tarn-test-fail:` and
  `tarn-test-skip:` records. The runner parses them. A test with failed
  checks fails even if it returns normally. A skipped test is neither passed
  nor failed. Tests without a parameter keep working.

**Report.** A failure lists every failed check with its location and values,
or `panicked at <location>: <message>`. JSON records add `location`,
`checks` and `skip_reason`, status `skipped`, and a `skipped` count in the
summary.

**Slices.** `collections.to_vec(&slice)` copies a slice of `Copy` values.
Methods on slices cannot be declared in `core` yet (ADR 0020), so it is a
free function for now.

## Consequences

- Panics and failed checks point at user code.
- `Location` is an ordinary core type. User helpers may take a trailing
  `at Location` to report their caller as well.
- `Display` for floats and a test-scoped temporary directory are not part of
  this change. Float formatting lives in `string`, above `core`, and there is
  no recursive directory removal yet.
