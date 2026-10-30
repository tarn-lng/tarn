# JSON

`import "json"` loads an ordinary bundled Tarn module: no intrinsics, and a
local `json.tarn` overrides it. Design: [ADR 0048](adr/0048-application-ergonomics.md).

## Encoding

Types opt in explicitly; there is no reflection or derive.

```tarn sketch
impl json.Encode for Item {
    fn encode(&self, out &mut json.Writer) {
        out.begin_object()
        out.field_u64(&"id", self.id)
        out.field_string(&"name", &self.name)
        out.end_object()
    }
}
body := json.encode(&item)                      // {"id":1,"name":"pan"}
list := json.encode_list(items.as_slice())      // [{...},{...}]
```

| Writer | |
|---|---|
| structure | begin_object, end_object, begin_array, end_array, key(name) |
| values | string, u64, i64, bool, null, value(&T: Encode), list(&[]T: Encode) |
| fields | field_string, field_u64, field_i64, field_bool (key plus value) |
| result | new(), consuming finish() string |

Commas are inserted automatically. Balanced begin/end calls and one value per
key are the caller's responsibility. Strings escape `"`, `\` and control
characters (`\n \r \t \b \f`, otherwise `\u00XX`). Other UTF-8, including `/`,
is written unchanged. Output is compact.

## Parsing

`parse(&string)` and `parse_bytes(&[]u8)` return `Result<Value, Error>`.
`Error.offset` is the byte offset of the first problem. Parsing is strict
RFC 8259: exactly one value with optional surrounding whitespace, no trailing
commas, comments, leading zeros, `NaN`, raw control characters or lone
surrogate escapes. Surrogate pairs decode to one scalar value. Nesting deeper
than 128 is rejected rather than exhausting the stack. Invalid UTF-8 inside a
string fails at that string's opening quote.

```tarn
enum Value {
    Null
    Bool(bool)
    Number(string)
    Text(string)
    List(Vec<Value>)
    Object(Vec<Member>)
}
struct Member {
    key string
    value Value
}
```

Numbers keep their validated source text: `as_u64`/`as_i64` convert integral
values in range and return None for fractions, exponents or overflow. Objects
keep members in source order, including duplicate names; lookups use the first.

Accessors return owned values, because v0 does not allow references inside
`Option`: `get(key) Option<Value>` clones, and `text`, `u64`, `i64`, `bool`
read a member directly. `as_text`, `as_u64`, `as_i64`, `as_bool` and `is_null`
read the value itself, `clone()` copies a tree, and consuming `into_list()`
moves array elements out (empty for any other value).

The parser copies its input once and accessors clone. These costs are
deliberate until benchmarks justify borrowed views.

## Lossless Value encoding (Phase 33)

Parsed `json.Value` implements `json.Encode`, so `json.encode(&value)` and
`writer.value(&value)` support nested owned trees. Number spelling/precision and
object-member order, including duplicates, are preserved. Escapes may normalize
while text content remains equivalent. `Writer.number(text)` validates a strict
JSON number and returns false without writing on invalid input. Encoding a
manually constructed invalid `Value.Number` aborts instead of injecting raw JSON.
