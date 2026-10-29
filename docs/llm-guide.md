# Tarn quick guide for coding agents

Use the installed compiler as the source of truth. Start with:

```sh
tarn syntax                 # keywords and these rules, from this compiler
tarn explain E3083          # what a diagnostic code means
tarn stdlib --json
tarn stdlib string --json
tarn stdlib core --json
tarn check main.tarn --json
```

Catalogs include signatures, doc comments, source lines and a SHA-256 identity
of the compiler's embedded stdlib. Rebuild the compiler after editing stdlib
sources; an old binary still contains old declarations. The catalog does not
claim inferred provenance or thread capabilities.

## Common corrections

- Declare with `x := value` (immutable) or `var x: Type = value` (mutable).
  `var x := value` is invalid. Parameters use `x Type`.
- `if` is a statement. `match` is also an expression: `x := match o {` with
  `Some(v) => v` arms; an arm block must `return`, `break`, `continue` or panic.
- Module constants: `const PAGE u32 = 0`, `pub const NAME string = "x"`.
  Values are literals, other constants, arithmetic/bitwise/boolean operators
  and integer conversions (`u32(OTHER)`), checked at compile time. Constants
  are not patterns; use a guard: `v if v == PAGE =>`.
- Integers have `to_string()`: `code.to_string()`.
- Raw pointers: `*mut T` passes where `*T` is expected; `p == q` compares
  addresses; `ffi.offset(p, n)`/`ffi.offset_mut(p, n)` move by `n` elements;
  `ffi.vec_mut(&mut v)` points at a Vec's first element. Reads and writes
  still need `unsafe { ffi.read(p) }`.
- Propagate Result with prefix `try`. `Option.ok_or(error)` produces Result;
  `Result.map_err(callback)` converts errors explicitly. `unwrap`, `expect(msg)`,
  `unwrap_or`, `map`,
  `and_then`, `is_some` and `is_ok` already exist; inspect core for exact types.
- Generic arguments in expressions are unsupported. Let the expected type
  infer them: `var counts: collections.Map<string, u64> = collections.Map.new()`.
- No `+=`, char/byte literals, implicit numeric coercion or stored references
  inside structs/enums. String indices and cursor offsets count bytes.
- Missing map keys: `collections.get_copy` for Copy values, or `map.get_or`
  with a borrowed fallback. `get`/`get_mut` require an existing key.
- Traversal: `next_slot` and `entry_at` borrow ordinary map storage; do not
  mutate the map during traversal. `drain` consumes entries and empties it.
- `next_field`/`next_line` return ranges; `range_bytes` borrows the owner and
  `parse_u64_bytes` parses without copying. `fields`/`lines` allocate owned text.
- `sleep_blocking` needs no executor; `sleep` is async. Use Instant for elapsed
  time, now_utc for Unix timestamps.
- Buffered stdout/stderr require explicit flush or finish. No destructor writes.
- Numeric casts such as `u64(x)` are checked; `u64.wrap(x)` is not implemented.
  `wrapping_add`, `wrapping_sub` and `wrapping_mul` are explicit integer
  arithmetic methods, not wrapping conversions.

## Executable references

The native suite compiles and executes
`tests/native/pass/general_purpose_text.tarn`: reverse search, numeric
round trips, ASCII text, formatting, safe map lookup, cursor traversal, CSV
and blocking sleep. `examples/csv_report/main.tarn` is a complete bounded
CSV-to-JSON command. `evidence/status_device/collector.tarn` demonstrates
Linux proc parsing, counter resets and process identity.

Unknown-member spelling suggestions already exist. A suggestion is guidance,
not proof that the proposed API has the intended semantics. `tarn explain`
covers registered codes; structured fixes, semantic queries and MCP remain
milestones in phase-34-plan.md.
