# ADR 0097: verified TLS after the PostgreSQL SSLRequest preface

Status: accepted; implemented in Tarn 0.3.0. Extends ADR 0066.

## Evidence

Building the registry against Neon exposed a driver bug: the PostgreSQL client
sent SSLRequest but continued with a cleartext StartupMessage after an `S` reply.
`net.TlsStream.connect` cannot fix that: it starts TLS immediately, while the
PostgreSQL wire protocol requires an eight-byte negotiation first. Bypassing the
driver with an HTTPS SQL gateway would leave this language integration gap open.

## Decision

Add `net.TlsStream.connect_postgres(authority, options)`. It sends SSLRequest,
requires exactly the server's `S` response, and performs certificate and hostname
verification before returning the existing TLS owner. It never falls back to
plaintext. Only a host and port are accepted; credentials, paths and queries are
rejected. `TlsStream.connect` retains its HTTPS-only contract.

Reuse ADR 0066's libcurl dependency and its stable opensocket/sockopt callbacks.
Curl resolves the original hostname. The opensocket callback opens a nonblocking
socket to the supplied resolved address, connects, writes the preface and reads
one byte; the sockopt callback reports that the socket is already connected.
Curl then performs TLS on that same descriptor, with the original hostname for
SNI and verification. HTTP ALPN is disabled on this path. Returning the descriptor
transfers ownership to curl; every earlier failure closes it in the callback.
All later failures go through curl cleanup. No raw descriptor escapes to Tarn.

A monotonic deadline covers TCP and preface operations in the callback. Curl's
connect/total setup timeouts bound DNS and TLS as well. EINTR retries retain the
deadline. `N`, unexpected bytes, EOF, timeout, certificate and hostname failures
are errors. Only the eight-byte preface is sent before encryption. Transfers
after connection preserve ADR 0066's existing blocking semantics; the setup
limit is not a per-query deadline.

The existing private `_tls_connect` ABI carries a private `postgresql://authority`
selector used by the new net method. There is no new intrinsic or owner shape,
no new Rust/native dependency, and no change to normal HTTPS or WebSocket setup.

## Alternatives and limits

- A generic consuming STARTTLS upgrade would be more flexible but needs a new
  ownership transfer ABI for an existing TCP owner. The concrete database need
  can be supported without exposing descriptors or changing ownership lowering.
- PostgreSQL direct TLS is newer and needs PostgreSQL-specific ALPN. Traditional
  SSLRequest also works with older servers and hosted PostgreSQL proxies.
- A driver-specific OpenSSL FFI owner would duplicate TLS policy and destruction.
- Libpq or Neon's HTTPS gateway would bypass the native Tarn driver pressure.

The driver retains legacy `prefer` compatibility through a credential-free
probe; when SSL is offered it reconnects through the verified TLS path. This
costs one extra connection. `require` and `verify-full` use a single connection
and always verify the certificate and hostname. Deployments should use these.
SCRAM channel binding, Unicode SASLprep and query deadlines remain distinct
capabilities, not silently emulated by this change.

## Verification

The local TLS fixture checks the preface, encrypted application bytes, partial
reads, CA and hostname rejection, TLS refusal, unexpected response, silent-peer
timeout and invalid authorities. The backend test invokes this fixture. Existing
HTTPS/TLS/WebSocket integration tests remain required. The driver has separate
real PostgreSQL 17 TLS/SCRAM/transaction tests and hostile protocol fixtures.
