# HTTP/1.1 server foundation

`import "http"` loads an ordinary Tarn module over net/io/time/runtime/string.
It is not trusted, has no intrinsics and can be overridden by a local module.
Design: [ADR 0046](adr/0046-bounded-async-http.md).
Runnable application: [http_server.tarn](../examples/http_server.tarn).

## Application API

| Type | Public API |
|---|---|
| Connection | new(stream, limits, timeouts), async read_request(owner), async write_response(owner, response), async reject(owner, status), consuming close() |
| Request | method(), target(), headers(), body(), trailers(), header(name), consuming into_body() |
| Header | new(name, bytes), text(name, text), name(), value(), value_text(), clone() |
| Response | bytes(status, owned_bytes), text(status, borrowed_text), json(status, owned_text), empty(status), method_not_allowed(allow), consuming header(header), consuming close_connection(), status() |
| serve | serve(address, handler) Result<void, io.Error>: sequential, one request per connection |
| serve_parallel | serve_parallel(address, workers, handler): bounded cooperative connections on independent workers |
| serve_parallel_with_limit | serve_parallel_with_limit(address, workers, connections_per_worker, handler): explicit per-worker admission |
| Limits / Timeouts | defaults(); public Copy configuration fields |
| Error | Io, Protocol, Limit, Timeout, InvalidConfig, InvalidResponse, InvalidText, InvalidState; status() |

Connection.new consumes a TcpStream and enables nonblocking mode; failure drops
it normally. Connection owns the stream and both buffers. It is non-Copy,
Transfer and not Share, structurally inheriting the socket's capabilities.
Request/Header/Response own their data, are non-Copy and have ordinary structural
Transfer/Share. All returned views borrow their owner. header(name) returns an
owned clone of the first matching header, not a stored optional reference.
Header.value_text validates UTF-8 and returns an owned copy; bodies remain bytes.

The owner argument must be the Execution driving the current async operation.
A mismatched owner follows the existing runtime nested-wake invariant check;
HTTP does not introduce cross-execution migration.

The application uses net.TcpListener.accept_async and moves each accepted stream
into an owned connection task. The example serves GET/HEAD /health, POST /echo,
and explicit 404/405 with Allow. It owns at most 128 task handles; admission waits
for an existing handle when full. Tasks run on one Execution; `http.serve_parallel` (below) runs one
independent executor per worker instead of a shared pool.
There is no generic server/router framework.

read_request returns Result<Option<Request>, http.Error>. None is clean EOF before
any new request bytes, or an ended connection; partial-message EOF is an error.
Each complete request requires a response before another read. Take the await
result into a binding before handling an error and calling reject: statement
cleanup then releases the read future's mutable loan.

```tarn sketch
received := await conn.read_request(owner)
match received {
    Ok(result) => match result {
        Some(request) => {
            response := try http.Response.bytes(u16(200), request.into_body())
            try await conn.write_response(owner, response)
        }
        None => { return Ok(()) }
    }
    Err(error) => {
        match error {
            http.Error.Io(original) => { return Err(error) }
            _ => {}
        }
        try await conn.reject(owner, error.status())
        return Err(error)
    }
}
```

This fragment belongs in an async function returning Result<void, http.Error>.
There is no implicit io/http error conversion or new special main ABI. The
example explicitly handles HTTP errors inside connection tasks and retains the
existing Result<void, io.Error> entry point.

## Serving with a handler (Phase 17)

For small applications, `http.serve(address, handler)` runs the accept/read/
reject/write loop. The handler is `mut fn(&Request) Result<Response, Error>`, so
it may mutably capture application state:

The following fragment assumes application-defined `Store` and `route` and
imports of `http` and `io`; it is not a standalone program.

```tarn sketch
fn main() Result<void, io.Error> {
    var store = Store.load(&"items.json")
    return http.serve(&"127.0.0.1:8080", fn(request &http.Request) Result<http.Response, http.Error> {
        return route(&mut store, request)
    })
}
```

serve owns a private Execution for the duration of the call, not a hidden global
executor. It serves one connection at a time; each response carries
`Connection: close` so an idle keep-alive client cannot stall others. A handler
`Err` is answered once with `error.status()` (500 below 400). Protocol failures
are rejected exactly as with Connection. For concurrency or keep-alive control,
use Connection with owned tasks as above. Complete program:
[http_crud.tarn](../examples/http_crud.tarn).

For several cores, `http.serve_parallel(address, workers, &handler)` takes a
value implementing `http.Handler` (`fn handle(&self, request &Request)
Result<Response, Error>`) that must be `Share`; keep mutable state in a
`Mutex`. Each worker owns a `net.TcpListener.bind_shared` listener
(SO_REUSEPORT) and its own Execution; the kernel spreads connections, no task
changes threads, and every listener is bound before any worker starts
(ADR 0058).

Response helpers: `Response.json(status, body string)` (content-type
application/json; http does not depend on [json](json.md)), `Response.empty(status)`
and `Response.method_not_allowed(allow &string)` (405 with Allow).

For statuses and headers fixed in source, the module functions `http.json`,
`http.text`, `http.empty` and `http.method_not_allowed` return `Response`
directly. An invalid fixed status (outside 200–599) or header is a programming
error and aborts, like an out-of-range index. Use the `Response.*` forms,
which return `Result`, when the status comes from data (ADR 0052).

## Parallel serving (Phase 30)

`serve_parallel<H: Handler + Share>(address, workers, &handler)` binds one
SO_REUSEPORT listener per native worker before starting workers. Each owns an
independent Execution and existing Executor, driving up to 64 connections
cooperatively and closing each after one response. To configure admission, use
`serve_parallel_with_limit(address, workers, connections_per_worker, &handler)`.
At capacity, new connections remain in the kernel accept queue until an active
operation finishes. Implement `Handler.handle(&self, &Request) Result<Response, Error>`
on a nominal shared type; protect mutable state with Mutex. Zero workers or a zero
connection limit is InvalidInput. A blocking handler still blocks its worker.
This is not shared-executor migration or work stealing, and it
has no graceful shutdown API. See [the Phase 30 report](phase-30-report.md).
The bounded connection model is specified in [ADR 0064](adr/0064-bounded-cooperative-http-workers.md).

## Ownership and connection state

Normal move/drop transfers or discharges exactly-once socket close. close consumes
the connection, including on error. No user destructor, owner registry, backend
move query or public mutable stream/buffer projection exists. Buffered output is
never flushed on drop: write_response explicitly writes and flushes successfully.

Connection retains HEAD/close metadata independently of the returned Request.
Dropping/consuming a Request cannot change how its response is framed. Transactions
and pipelined requests execute sequentially and preserve prefetched bytes.
Protocol state prevents a second read before response, reuse after bad input,
or a second response after output failure. It is not socket ownership state.
A dropped pending read/write future leaves the connection failed; close/drop
still releases its ordinary live stream. Invalid response construction/preflight
has emitted no bytes, allowing a corrected response or rejection.

reject accepts only 400–599, writes one empty response with Connection: close
and ends the connection. It is allowed before transmission, including after
read failure. It cannot recover failed input or follow partial output.
Rejection is best effort: closing a socket with unread peer data may produce a
Linux TCP reset even after the error response. Do not drain hostile input merely
to promise graceful EOF. Transport failure and write timeout close without a
second response.

## Protocol subset

Only server-side HTTP/1.1 is implemented. Require exact CRLF, token methods,
origin-form targets or OPTIONS *, valid ASCII URI characters/percent escapes,
and exactly one Host. Host accepts lexical hostname labels, strict IPv4 or
bracketed IPv6 and an optional port 0–65535. No DNS occurs during head parsing.
Numeric IPv4 components reject leading zeros; trailing-dot DNS names, IPvFuture,
IPv6 zones, userinfo and percent-encoded hosts are outside this initial subset.

Header names normalize ASCII case. Trim only surrounding SP/HTAB from values;
keep allowed opaque bytes and duplicates in order. Reject controls/DEL, obs-fold,
whitespace before a colon and framing/routing/auth/content-critical Connection
nominations. Reject Upgrade/CONNECT/unsupported coding with 501, Expect with 417
before body waiting, unsupported valid versions with 505 and malformed input with
400. No interim 100-continue, proxy forms or HTTP/1.0 fallback.

Requests without length/coding have empty bodies. Content-Length is one checked
decimal field; reject duplicates even when identical, comma lists, signs and
overflow. TE+CL is invalid. Transfer-Encoding supports exactly one chunked token;
multiple chunked tokens are invalid regardless of spacing/case. Other bare coding
tokens/chains get 501; parameters/malformed coding syntax get 400 in this subset.
Chunk sizes are checked hex; chunk extensions are validated and ignored. Require
all data terminators and the final zero chunk. Store bounded trailers separately;
never let them override the head. Validate Trailer declarations and retain safe
undeclared extension trailers. Forbidden trailer names include framing, routing,
authentication and content-interpretation fields listed in the plan.

Responses accept final 200–599 statuses. Header builders reject framing/control
fields and injection. Generate Content-Length; no outbound chunking/trailers.
HEAD sends no body and advertises representation length. 204/304 require empty
bodies and omit length/coding; 205 requires empty body and length zero. Empty
reason phrases are permitted for unlisted statuses. Preflight checks the complete
response before any output, including header/body budgets. Requests asking to
close, explicit response preference and the request-count cap end persistence.

## Limits and errors

Defaults: request/individual lines 8 KiB, aggregate head 32 KiB/100 fields, request
and response bodies 1 MiB, response head 32 KiB/100 fields, chunk metadata including
trailers 64 KiB, 4096 data chunks/20 trailers, 8 KiB reader/writer and 100 requests
per connection. Configurations require positive budgets; head covers request-line
and header-line caps, chunk metadata covers its line cap, and response head is at
least 64 bytes. Byte capacities are bounded at 1 GiB to keep allocation arithmetic
representable. These are bounds, not guarantees that allocation cannot fail.

Whole-stage monotonic deadlines: head including idle 10 s, body including chunks
and trailers 30 s, write plus flush 10 s. No per-byte reset; no CPU/handler
preemption. Durations must be 1–4294967295 milliseconds. There is no infinite
HTTP deadline. Allocation exhaustion retains existing abort semantics.

`runtime.run_timeout(operation, duration)` consumes an
Operation<Result<void, io.Error>> and returns Result<bool, io.Error>: true means
complete, false means timeout. Completion wins ties. Owned stage results are
written through ordinary output loans; completion/abandonment releases those
loans before access. This deliberately does not consume arbitrary generic-result
pollers, which may return a reference into their own storage. No borrowing rules,
scoped tasks, select API or cancellation facility were added.

Error.status gives a candidate rejection status, not permission to write another
response. Parsing maps to 400/501/505/417; read deadline 408; body/chunk metadata
413; start-line 414; header/trailer line/count/head 431; invalid application
response 500. I/O errors preserve io.Error and must be handled explicitly.
Connection state and transport viability decide whether rejection is possible.

## Deferred work

This server module has no TLS termination or client API. A separate blocking
client is now available in [https](https.md) (Phase 33). HTTP/2/3, WebSocket, compression, multipart, streaming application
bodies, routing/middleware framework, cookie/auth framework, scoped async tasks,
generic async I/O, work-stealing scheduler, cancellation and platform expansion
remain deferred. This is a
bounded server subset, not a blanket RFC-conformance claim.
