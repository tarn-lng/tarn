# Strings (Phase 15A)

Strings own immutable UTF-8 storage. Lengths and indices count **bytes**, not
characters. Use `import "string"` for text operations.

```tarn
import "string"

fn main() {
    text := "  café,hello  "
    clean := string.trim(&text)
    words := string.split(&clean, &",")
    for word in words.as_slice() { print(word) }
    match string.parse_u16(&"8080") {
        Some(port) => { print(port) }
        None => { print("invalid port") }
    }
}
```

| API | Result and policy |
|---|---|
| `value.len()` / `string.len(&value)` | usize byte length |
| `value.is_empty()` | bool |
| `value.clone()` | independent owned copy |
| `value.bytes()` / `string.bytes(&value)` | immutable borrowed `&[]u8` |
| `string.from_utf8(data)` | `Option<string>`, validated owned copy |
| `string.slice(&value, start, end)` | `Option<string>`, owned copy, UTF-8 boundaries required |
| `string.find(&value, &pattern)` | `Option<usize>`, first byte offset, empty pattern at zero |
| `string.contains`, `starts_with`, `ends_with` | bool; empty patterns match |
| `string.trim(&value)` | owned copy, trims ASCII whitespace 09..0D and 20 |
| `string.split(&value, &separator)` | owned `Vec<string>`, keeps empty fields |
| `string.lines(&value)` | owned `Vec<string>`, LF/CRLF, no final phantom line |
| `string.parse_u16`, `parse_u64`, `parse_i64` | Option; strict decimal, overflow/invalid input are None |
| `string.from_u64`, `from_i64` | owned decimal string, full numeric ranges |
| `left + right` | new owner, both operands remain usable |
| `== != < <= > >=` | content equality / unsigned lexicographic UTF-8 byte order |

A slice of bytes keeps its owner borrowed while used. Mutable bytes are never
exposed. `from_utf8` accepts embedded NUL and rejects overlong encodings,
surrogates, truncated sequences and scalars above U+10FFFF. It copies input, so
the original buffer can be changed or destroyed after the call.

`slice` rejects reversed/out-of-range indices and indices inside a scalar.
Returned strings and split/line elements remain valid after the input owner
is destroyed. They are independent resources with ordinary move/drop behavior.

For an empty separator, split yields individual UTF-8 scalars (not graphemes)
and no leading/trailing empty fields; splitting empty text this way is empty.
A nonempty separator on empty text yields one empty field. Lines preserve bare
CR, trim CR only immediately before LF, and return no lines for empty text.

Parsing accepts optional '+'; i64 also accepts '-'. Whitespace, separators,
Unicode digits and sign-only input are invalid. No runtime arithmetic fault
is used for ordinary parse failure. These APIs use Option, not io.Error.

The module uses functions rather than adding primitive methods outside core.
See [ADR 0042](adr/0042-string-essentials.md) for ownership, native bridges,
allocation tradeoffs and bootstrap limits. `Builder` (Phase 17; in `core` since
ADR 0077) accumulates text efficiently, and `f"..."` interpolation builds on it.
Offset cursors and borrowed byte views are available (Phase 32); float parsing
and formatting are described below (ADR 0063). Borrowed substring values and
general Unicode classification remain outside the implemented surface.

`string.fields(&text)` (Phase 31) returns owned nonempty fields separated by
ASCII whitespace. Unicode whitespace classification remains separate work.

## Offset traversal (Phase 32)

`Range` contains Copy byte offsets, not a stored reference. `next_field(text,
&mut cursor)` and `next_line(text, &mut cursor)` return Option<Range>; initialize
cursor to zero. Fields use ASCII whitespace; lines preserve the existing LF/CRLF
contract. `next_split(text, separator, &mut cursor)` uses SplitCursor.new() and
preserves split's empty-separator/trailing-field behavior. `split_ranges` collects
only offsets, and `split_count` counts without materializing any parts.

`valid_range(text, range)` checks bounds and UTF-8 scalar boundaries.
`range_bytes(text, range)` returns an immutable byte-slice loan, without copying;
invalid access aborts like indexing. `copy_span` returns Option<string> when an
independent owner is needed. Offset values have no source identity; reusing them
with a different string is safe if validated but may select different content.
Keep cursor inputs unchanged throughout traversal for meaningful results.

```tarn
fn first_bytes(text &string) &[]u8 {
    var cursor: usize = 0
    match string.next_field(text, &mut cursor) {
        Some(span) => { return string.range_bytes(text, span) }
        None => { return string.range_bytes(text, string.Range{start: 0, end: 0}) }
    }
}
```

`parse_u64_bytes` parses strict ASCII decimal directly from a borrowed slice.
Builder.push_u64/push_i64 append stack-formatted digits without a temporary
owned string. Existing owned APIs still copy; the cursor additions do not change
string layout or allow references stored in aggregates. See
[ADR 0061](adr/0061-offset-text-traversal.md) and the
[textstats acceptance CLI](../examples/textstats/README.md).

`prefix_scalars(text, limit)` returns the Range of at most `limit` UTF-8
scalars from the start, without allocating. It never cuts a scalar but may
separate combining marks; it is not grapheme-aware truncation. The ticket port
uses it for the original 2,000-character message bound.

## Evidence-led additions (ADR 0063)

`rfind` returns the last byte offset (empty pattern: end). `parse_f64` accepts
finite decimal text with optional sign, dot and exponent, without whitespace,
hex or NaN/infinity. `from_f64` and `push_f64(&mut builder, value)` use 17 significant digits
for round trips, rather than shortest display; parsing validates first in Tarn
and uses a locale-independent native conversion. Overflow is None.

ASCII casing helpers preserve other UTF-8 bytes. `replace_all` replaces
nonoverlapping matches; an empty pattern inserts at Unicode scalar boundaries.
`format` takes a string slice: `{}` consumes one argument and `{{`/`}}` escape
braces. Errors include kind and byte offset; there are no implicit conversions.
Prefer `f"..."` (ADR 0077) for fixed templates; use typed Builder methods for
mixed values in loops without temporary strings.
