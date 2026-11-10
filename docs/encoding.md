# Encoding and digests

Two ordinary stdlib modules (plain Tarn, no intrinsic authority; ADR 0076).

## `encoding`

| API | Behaviour |
| --- | --- |
| `base64_encode(data &[]u8) string` | Standard alphabet, `=` padding, no line breaks |
| `base64_decode(text &string, limit usize) Result<Vec<u8>, Base64Error>` | Strict RFC 4648 §4 |
| `base64url_encode(data &[]u8) string` | URL-safe alphabet, no padding (RFC 4648 §5, JWT) |
| `base64url_decode(text &string, limit usize) Result<Vec<u8>, Base64Error>` | Strict: rejects `+`, `/`, `=` and non-canonical bits |
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

Everything is ordinary Tarn except `random_bytes`, which calls `getrandom`.
Operations on secrets (RSA private exponents, Poly1305 tags) run in constant
time; see [ADR 0100](adr/0100-rsa-signatures-and-aead.md).

| API | Contract |
| --- | --- |
| `sha256(data &[]u8) [32]u8` | FIPS 180-4 |
| `hmac_sha256(key &[]u8, message &[]u8) [32]u8` | RFC 2104 |
| `equal_bytes(a &[]u8, b &[]u8) bool` | Constant time for equal lengths; use for MACs and tags |
| `random_bytes(count usize) Result<Vec<u8>, CryptoError>` | Operating-system CSPRNG |
| `rsa_private_key_from_pem(pem &string) Result<RsaPrivateKey, CryptoError>` | `RSA PRIVATE KEY` (PKCS#1) or `PRIVATE KEY` (PKCS#8); unencrypted; 2048–4096-bit moduli |
| `rsa_private_key_from_der(der &[]u8)` | The same keys in DER |
| `rsa_public_key(modulus &[]u8, exponent &[]u8) Result<RsaPublicKey, CryptoError>` | Big-endian magnitudes, e.g. from a JWK |
| `rsa_sign_pkcs1v15_sha256(key &RsaPrivateKey, message &[]u8) Result<Vec<u8>, CryptoError>` | RFC 8017 RSASSA-PKCS1-v1_5 (JWT `RS256`); deterministic; self-verified before return |
| `rsa_verify_pkcs1v15_sha256(key &RsaPublicKey, message &[]u8, signature &[]u8) bool` | Rejects wrong lengths and signatures ≥ n |
| `chacha20poly1305_seal(key, nonce, plaintext, aad) Result<Vec<u8>, CryptoError>` | RFC 8439; 32-byte key, 12-byte nonce; returns ciphertext ‖ 16-byte tag |
| `chacha20poly1305_open(key, nonce, sealed, aad) Result<Vec<u8>, CryptoError>` | Verifies the tag in constant time before decrypting |

Never reuse a nonce with the same ChaCha20-Poly1305 key; draw it with
`random_bytes(12)` and store it next to the ciphertext. `aad` binds a
ciphertext to its context (for example a row id), so it cannot be moved
between rows. Comparing digests with `!=` is not constant time: use
`equal_bytes` for anything secret.

```tarn sketch
import "crypto"

fn store(key &[]u8, row &string, secret &string) Result<Vec<u8>, crypto.CryptoError> {
    nonce := try crypto.random_bytes(12)
    sealed := try crypto.chacha20poly1305_seal(key, nonce.as_slice(), secret.bytes(), row.bytes())
    var record = nonce
    record.extend_from_slice(sealed.as_slice())
    return Ok(record)
}
```
