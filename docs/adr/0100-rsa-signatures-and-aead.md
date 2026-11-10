# ADR 0100: RSA signatures, ChaCha20-Poly1305 and randomness in `crypto`

Status: accepted; implemented in Tarn 0.5.0. Extends ADR 0076 and ADR 0062.

## Evidence

The Tarn registry publishes releases by calling the GitHub API. Anonymous calls
share a 60-requests-per-hour budget per outbound IP, and on Render that budget
is exhausted by other tenants: publications failed with HTTP 403. A GitHub App
authenticates with a JWT signed with RS256, then exchanges it for an
installation token. Tarn could not sign RS256, could not send `User-Agent` and
`Authorization` in one HTTPS request, and had no cipher to keep that token
encrypted at rest.

## Decision

`crypto` gains, in ordinary Tarn:

- `random_bytes`, the only native call (`getrandom`, as `websocket` already uses).
- RSA keys from PEM/DER (PKCS#1 and PKCS#8, unencrypted, 2048–4096-bit moduli,
  odd `e ≥ 3`) and RSASSA-PKCS1-v1_5 with SHA-256 signing and verification.
- ChaCha20-Poly1305 (RFC 8439) seal/open.

`encoding` gains unpadded URL-safe Base64, and `https.request_headers` sends up to
16 validated headers; the native adapter appends each LF-separated line, which
the single-header `request` still forbids, so existing callers are unchanged.

## Security properties

- Big integers are fixed-width 32-bit limbs with Montgomery multiplication. The
  private exponent is padded to the modulus length and processed in 4-bit
  windows with a masked scan of the whole table, so timing and memory access
  depend only on the key size. The final Montgomery subtraction is selected by
  mask, not by branch.
- Signing does not use CRT. It is about 4x slower (≈50 ms for 2048 bits), but
  it avoids the Bellcore fault attack on CRT entirely; every signature is also
  verified with the public key before it is returned.
- Poly1305 uses 26-bit limbs and a masked final reduction; tags are compared
  with `equal_bytes` before any plaintext is produced.
- ChaCha20 was chosen over AES-GCM because a portable AES needs table lookups
  indexed by secret data.

## Limits

No key generation, RSA encryption, PSS, CRT, encrypted PEM, ECDSA/Ed25519 or
streaming AEAD. Base blinding is not applied; the constant-time ladder is the
defense. The compiler gives no constant-time guarantee: the code avoids
secret-dependent branches and indices, which today's backend preserves.

## Verification

`tests/stdlib/rsa_aead_test.tarn` checks signatures byte-for-byte against OpenSSL
4.1 for the same key in PKCS#1 and PKCS#8, rejection of tampering, weak and
malformed keys, the RFC 8439 §2.8.2 vector, block and padding boundaries
computed with Python `cryptography`, and that flipping any bit of a sealed
message fails authentication.
