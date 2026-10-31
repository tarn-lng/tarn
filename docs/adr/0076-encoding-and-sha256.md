# ADR 0076: `encoding` (Base64, hex) and `crypto` (SHA-256) as ordinary stdlib

Status: accepted. Extends ADR 0041 (stdlib layers). No syntax change.

## Evidence

The tarn-ui devtools capture flow (phase 4) receives a PNG as `data_base64`
together with its `sha256`. It must decode the PNG with a 1 MiB limit, reject
malformed text, and recompute and compare the digest. Tarn had a private
encoder and SHA-1 inside `websocket`, but no public decoder or SHA-256.

## Decision

- **Two ordinary modules.** `encoding` (`base64_encode`, `base64_decode` with
  a limit, `hex`) and `crypto` (`sha256`). Both are plain Tarn: arithmetic uses
  `wrapping_add` and there are no intrinsics, so they gain no authority. The
  names avoid natural variable names such as `hash` or `base64` (a local would
  shadow the module).
- **Strict decoding.** Padding is required. The decoder rejects whitespace,
  the URL-safe alphabet and noncanonical trailing bits, and reports the exact
  byte offset of an invalid character. The limit is checked against the size
  derived from the length, before allocating. A lenient decoder could accept
  two texts for the same bytes and hide corruption in an integrity check.
- **`sha256` returns `[32]u8`.** It is a fixed-size Copy value; `hex` gives
  the 64-character form for comparison with protocol text.

## Verification

- RFC 4648 vectors and an all-bytes round trip.
- Each kind of rejection, and the exact and padded limits.
- NIST SHA-256 vectors (empty, `abc`, 448-bit, one million `a`), padding
  boundaries at 55/56/63/64/119 bytes, and all 256 byte values.
- On a 4.7 MB binary the digest matches `sha256sum` and the encoding matches
  coreutils `base64 -w0` (0.13 s for hash, encode and decode).

## Not included

Streaming hashes, HMAC, ciphers, secure random bytes and constant-time
comparison: none was required by the evidence. `websocket` keeps its private
helpers for now; switching it to `encoding` is a separate cleanup.

## Registry-driven extension: HMAC-SHA-256 and byte comparison

The registry webhook receiver and PostgreSQL SCRAM both require HMAC.
`crypto.hmac_sha256(key, message)` implements RFC 2104 over SHA-256 and
`crypto.equal_bytes(a, b)` accumulates byte differences without data-dependent
early returns for equal-length input. Length remains public. These remain
ordinary Tarn functions with no new authority or native dependency. The source
loop does not constitute a formal machine-code constant-time guarantee; key
handling, compiler output auditing and streaming APIs remain separate work.
Focused tests cover a known HMAC-SHA-256 vector and byte comparison; registry tests
verify raw-byte webhook signatures and tampering rejection.
