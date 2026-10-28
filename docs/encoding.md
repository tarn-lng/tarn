# Encoding and digests

Two ordinary stdlib modules (plain Tarn, no intrinsic authority; ADR 0076).

## `encoding`

| API | Behaviour |
| --- | --- |
| `base64_encode(data &[]u8) string` | Standard alphabet, `=` padding, no line breaks |
| `base64_decode(text &string, limit usize) Result<Vec<u8>, Base64Error>` | Strict RFC 4648 §4 |
| `hex(data &[]u8) string` | Lowercase, two digits per byte |

`base64_decode` rejects input with an explicit `Base64Error`:

- `InvalidLength`: the length is not a multiple of four, which includes
  whitespace or line breaks.
- `InvalidCharacter(at)`: the byte offset of a character outside the
  alphabet. URL-safe `-`/`_` and an interior `=` are rejected here.
- `InvalidPadding`: malformed padding, or nonzero trailing bits. `Zh==` is
  rejected, so every byte string has exactly one accepted text.
- `TooLarge`: the decoded size exceeds `limit`. The size is computed from the
  text length before any allocation.

```tarn
import "encoding"
import "crypto"

fn main() {
    match encoding.base64_decode(&"YWJj", 3) {
        Ok(data) => {
            digest := crypto.sha256(data.as_slice())
            print(encoding.hex(&digest))
        }
        Err(_) => { panic("invalid base64") }
    }
}
```

## `crypto`

`sha256(data &[]u8) [32]u8` is FIPS 180-4 SHA-256. It is tested against the
NIST vectors, the padding block boundaries and `sha256sum`. There is no
streaming, HMAC, cipher or random API. Comparing digests with `!=` is not
constant-time; that is fine for integrity checks, not for secret MACs.
