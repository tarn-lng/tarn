# Blocking HTTPS client

`import "https"` provides bounded synchronous HTTPS through system libcurl.so.4.
Installation/security updates are the OS package manager's responsibility. The
library is loaded lazily; unrelated Tarn applications do not require it.

```tarn
import "https"
fn main() {
    options := https.Options.new()
    match https.get("https://example.org/", &options) {
        Ok(response) => { print(response.status)
            print(response.body.len()) }
        Err(error) => { print(error.code) }
    }
}
```

| API | Contract |
|---|---|
| `Options.new()` | 30,000 ms timeout, 1 MiB response limit, system CA trust |
| `Options.timeout_millis` | Positive u64 up to i64 maximum |
| `Options.response_limit` | usize, maximum 64 MiB; zero permits empty bodies only |
| `Options.ca_file` | Owned PEM trust path; empty selects system trust |
| `get(url, options)` | Result<Response, Error> |
| `request(url, method, header, body, options)` | GET or POST; one optional application header and JSON Content-Type |
| `Response.status`, `Response.body` | HTTP status and owned raw byte vector; HTTP error statuses are responses |
| `encode_query(value)` | RFC 3986 percent-encoded UTF-8 query component; space is %20 |

Only HTTPS is accepted. TLS certificate and hostname verification cannot be
switched off. NUL C strings and newline header injection are rejected. Redirects,
proxy environment, cookies, retries and transparent decompression are disabled.
Response headers/streaming are not exposed; for a persistent duplex TLS byte
stream use `net.TlsStream` ([ADR 0066](adr/0066-persistent-tls-streams.md)). The response buffer is allocated to
the selected bound before the transfer; do not select unnecessarily large bounds.

`https.request_headers(url, method, headers &[]string, body, options)` sends up
to 16 `Name: value` headers (for example `User-Agent` plus `Authorization`);
each one is rejected if it contains CR, LF or NUL or has an empty name.

Errors preserve libcurl codes (e.g. 28 timeout, 60 certificate verification);
-1 means native dependency/global initialization unavailable, -2 response bound
exceeded, -3 invalid input. Truncated transfers fail; exact libcurl error may
vary with the transport shutdown (partial file or receive error). DNS timeout
behavior depends on the system resolver build; this is a blocking library timeout,
not Tarn's async deadline mechanism.

See [ADR 0062](adr/0062-bounded-system-https.md) for dependency review, ownership
and limits; [ticket classifier](../examples/ticket_classifier/README.md) for the
acceptance application. It uses safe Tarn APIs over a synchronous C adapter.
