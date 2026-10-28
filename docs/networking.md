# Native networking (Phases 12A, 12B and 12C)

`import "net"` loads the embedded `stdlib/net/net.tarn`. Errors are `io.Error`;
execution (Execution, Operation, AsyncTask) lives in `runtime` and timers in
`time` (module layers: [ADR 0041](adr/0041-stdlib-module-layers.md)). The current target is
Linux x86_64 and ARM64. Blocking operations, including DNS, may block the calling native task.
The API uses ordinary Tarn ownership, `Result`, `try` and borrowed byte slices.

```tarn
import "io"
import "net"

fn main() Result<void, io.Error> {
    var listener = try net.listen_tcp(&"127.0.0.1:8080")
    var stream = try listener.accept()
    var buffer = [8]u8{0, 0, 0, 0, 0, 0, 0, 0}
    count := try stream.read(&mut buffer)
    try stream.write_all(&buffer[0..count])
    return Ok(())
}
```

`TcpListener.bind_shared(address)` additionally sets SO_REUSEPORT so several
listeners of this process can share an address, each with its own accept
queue (Phase 30); plain `bind` keeps rejecting a second active listener.

TCP listeners set SO_REUSEADDR before binding (Phase 17, ADR 0048): a restarted
server can rebind over connections it closed that remain in TIME_WAIT, while a
second active listener on the same address is still rejected.

The binding must be `var` for a mutable receiver. String arguments are borrowed
explicitly, and successful `Result` returns use `Ok(())`. Repeated array literals
are not part of the current syntax; examples use explicit initializers. Passing
`&buffer` or `&mut buffer` to slice parameters safely coerces the array reference.

## Public surface

All fallible operations return `Result<..., io.Error>`.

| Type | Operations | Receiver/result |
| --- | --- | --- |
| `TcpListener` | `bind(&string)`, `bind_addr(SocketAddr)` | owned listener |
| | `accept()` | `&mut self`, owned `TcpStream` |
| | `local_addr()` | `&self`, `SocketAddr` |
| | `close()` | consumes `self`, `void` |
| `TcpStream` | `connect(&string)`, `connect_addr(SocketAddr)` | owned stream |
| | `read(&mut []u8)`, `write(&[]u8)` | `&mut self`, `usize` |
| | `write_all(&[]u8)` | `&mut self`, `void` |
| | `local_addr()`, `peer_addr()` | `&self`, `SocketAddr` |
| | `shutdown(Shutdown)` | `&mut self`, `void` |
| | `close()` | consumes `self`, `void` |
| `UdpSocket` | `bind(&string)`, `bind_addr(SocketAddr)` | owned socket |
| | `recv_from(&mut []u8)` | `&mut self`, `RecvFrom { count, peer }` |
| | `send_to(&[]u8, SocketAddr)` | `&mut self`, `usize` |
| | `local_addr()` | `&self`, `SocketAddr` |
| | `close()` | consumes `self`, `void` |

`net.listen_tcp(&string)` is a small convenience for `TcpListener.bind`.
`net.resolve(&string)` returns a `SocketAddr`. The Copy address exposes `port()`,
`ip()`, `is_ipv4()`, `is_ipv6()` and `with_port(u16)`. `IpAddr` is a Copy enum with
`V4([4]u8)` and `V6([16]u8)` variants. `Shutdown` is `Read`, `Write` or `Both`.
Descriptors, address bytes and native error domain/code fields are private.

## Ownership and concurrency

Each socket owner is non-Copy and explicitly Transfer, without Share. Moving it
transfers the close responsibility; use after move, double consuming close,
move/overwrite during a live borrow and sharing it directly across tasks are
compile-time errors. I/O uses `&mut self`; address queries use `&self`.
`Mutex<TcpStream>` can serialize shared access using Phase 11's existing rules.

Automatic close follows verified post-drop IR on normal scope exit, return,
break, continue, overwrite, conditional initialization and `try` error exits.
Explicit `close` consumes its owner even if the OS returns an error. Linux close
releases a valid descriptor on EINTR, so it must never be retried. Drop discards
ordinary close errors; explicit close reports them. EBADF during destruction is
an impossible live-owner invariant and aborts. The runtime neither tracks owner
liveness nor consults the move checker. Native tasks still join on destruction.

TCP shutdown changes communication directions and preserves the owner. Reading
into a nonempty buffer returns zero for EOF. An empty TCP read returns zero
immediately and is not evidence of EOF. `write` returns the actual partial count;
`write_all` retries until complete or error and reports `WriteZero` on zero progress
with remaining bytes. An empty `write_all` succeeds without a syscall.

UDP is datagram-oriented: zero means a valid empty datagram, not EOF. Receives
truncate oversized datagrams to the buffer and discard the excess. A zero-capacity
receive still consumes one datagram. No borrowed runtime storage escapes.

## Resolution and errors

Endpoints use `host:port`, `IPv4:port` or `[IPv6]:port`. Ports are decimal numeric
values 0–65535; port zero supports ephemeral binding. An empty host (`:port`)
resolves a wildcard address. Embedded NULs, missing ports and malformed endpoints
produce `InvalidAddress`. IPv6 scope IDs resolved by getaddrinfo remain encoded.

Blocking getaddrinfo selects one IPv4-preferred candidate, otherwise IPv6. This
API deliberately returns one address, not a candidate collection. Applications
can use explicit numeric IPv6 endpoints or `connect_addr`; connection fallback
across DNS candidates is not implemented. Tests resolve `localhost` without
internet. Address display/formatting and public scope-ID access are deferred.

`Error.kind` distinguishes `AddressInUse`, `ConnectionRefused`, `ConnectionReset`,
`BrokenPipe`, `TimedOut`, `WouldBlock`, `InvalidAddress`, `DnsFailure`, `OtherOs`,
and `WriteZero`. `native_code()` and `is_dns_code()` preserve diagnostic access
to the original errno or resolver code. There is no public errno-based API or
implicit error conversion in `try`. Network errors do not abort. A main returning
`Result<void, io.Error>` prints the error category/native code and exits with
status 1 on failure, or 0 on success.

Safe EINTR retries live in Tarn: resolution, socket creation, bind/listen,
accept, reads/writes, address queries, shutdown and UDP I/O. Interrupted connect
drops the incomplete connection and retries using a fresh descriptor because its
state is unspecified. Interrupted close is reported and never retried. EAGAIN and
EWOULDBLOCK map to WouldBlock even though 12A sockets are blocking. All sends use
Linux `MSG_NOSIGNAL`, returning an error instead of accidental SIGPIPE termination.

## Private bridge and examples

C handles syscall ABI conversion, endpoint parsing for getaddrinfo, native status
capture and address encoding. Public wrappers, retries, error categories,
write_all, owned construction and resource cleanup remain in Tarn/verified IR.
Trusted intrinsic signatures and layouts are structurally verified. A local
`net.tarn` cannot override this embedded module or gain intrinsic authority.

The fixed private outcome has `i32 domain`, `i32 code`, `i64 value` and a 24-byte
address; slice pointers/lengths are explicit ABI lanes. Native declarations are
bootstrap compiler resources, not general user-defined destructors. See
[ADR 0034](adr/0034-blocking-networking.md) for the boundary and bootstrap debt.

[`tcp_echo.tarn`](../examples/tcp_echo.tarn) serves one connection until EOF.
[`25_concurrency.tarn`](../examples/25_concurrency.tarn) moves an accepted
connection into a native task and joins it. Both listen on loopback port 8080.

12A blocking behavior remains supported alongside the explicit 12B operations below.

## Nonblocking and readiness (Phase 12B)

`set_nonblocking(bool)` changes descriptor mode through an exclusive socket
receiver without changing ownership. Existing constructors and accepted streams
remain blocking; nonblocking mode is never enabled globally. `read`, `write`,
`accept`, `recv_from` and `send_to` return WouldBlock when no operation completed.
Successful partial counts remain successful counts. An empty UDP datagram is data.
`write_all` rejects a nonblocking stream with WouldBlock **before any write**;
use explicit partial writes and readiness instead. Switching back to blocking
restores its existing behavior.

**Readiness is evidence that an operation may make progress, not permission to
bypass normal socket error handling.** A subsequent syscall can still return
WouldBlock. Error/hangup events do not discard readable bytes or replace TCP EOF.

`Poll.new()` creates an owned, non-Copy, Transfer/not-Share epoll resource.
`register_listener`, `register_stream`, `register_udp` accept a shared socket
reference and `Interest.Readable`, `.Writable` or `.ReadableWritable`. Registration
returns a Copy `Token`; compare tokens using `same`. `modify_listener/stream/udp`
and `deregister_listener/stream/udp` require the socket and token. All Poll methods
mutate `&mut self`, preventing unsynchronized registry access.

```tarn
import "io"
import "net"
fn main() Result<void, io.Error> {
    var socket = try net.UdpSocket.bind(&"127.0.0.1:8080")
    try socket.set_nonblocking(true)
    var poll = try net.Poll.new()
    token := try poll.register_udp(&socket, net.Interest.Readable)
    var events = [1]net.Event{net.Event.empty()}
    var bytes = [2]u8{0, 0}
    count := try poll.wait(&mut events, 5000)
    if count > usize(0) && events[0].token.same(token) {
        match socket.recv_from(&mut bytes) {
            Ok(packet) => print(packet.count)
            Err(error) => match error.kind {
                io.ErrorKind.WouldBlock => {}
                _ => { return Err(error) }
            }
        }
    }
    return Ok(())
}
```

Events contain `token`, `readable`, `writable`, `error`, `hangup`. Event storage is
caller-provided; `Event.empty()` initializes slots. Wait returns the number of
written events (at most 64 per call) and allocates no event buffer. Timeout is i32
milliseconds: -1 infinite, 0 immediate, positive finite; values below -1 and empty
storage are invalid. Interrupted waits transparently retry against the original
monotonic deadline. Linux scheduling can overrun finite deadlines.

**Readiness registration never transfers ownership of a socket or application
buffer to the kernel/runtime. Tarn must not keep borrowed application buffers
pending across readiness waits in the Phase 12B model.** Registration retains no
socket loan after returning, and wait retains no event-buffer pointer afterward.
Read/write still require ordinary exclusive socket/buffer access. Tokens contain
no pointers or public fds. Dropping Poll closes only epoll, not sockets. Socket
close removes kernel interest; previously copied events are inert old tokens.
Process-wide nonreused tokens and SO_COOKIE checks prevent an old token from
modifying/deregistering a new socket at a recycled descriptor. Re-registering a
new socket receives a new token. Closed bookkeeping can remain until fd reuse or
Poll destruction; it never keeps socket resources alive.

`TcpStream.connect_nonblocking(&string)` / `connect_nonblocking_addr(SocketAddr)`
return an owned `Connecting`. DNS still blocks. Register it through
`register_connecting(&connection)`, wait for writable/error readiness, then call
`connection.poll_connected()`: false means pending, true means confirmed success,
Err is a latched terminal error. SO_ERROR and peer status determine completion;
writable alone is insufficient. `deregister_connecting` removes interest and
`into_stream()` consumes a successfully completed connection. Consuming one before
completion returns WouldBlock and closes it; repeated failure checks preserve the
original failure. Dropping Connecting closes the incomplete stream normally.

See [ADR 0035](adr/0035-nonblocking-readiness.md). No async syntax, pending kernel
buffer I/O, io_uring, HTTP, TLS or channels are implemented by Phase 12B.

## Persistent TLS streams (Phase 35)

`net.TlsStream.connect(&"https://host:port", &net.TlsOptions.new())` opens a
verified TLS client stream through system libcurl in connect-only mode. It is
owned, Transfer and not Share; `read`/`write` are partial, `write_all` is
blocking-only, and `set_nonblocking` changes stream state rather than the
descriptor. Register it with `Poll.register_tls`, or use `read_async`,
`write_async` and `write_all_async`. TLS may hold decrypted bytes while the
descriptor is not readable: after a readable event, read until WouldBlock
before waiting again. Errors use `io.Error` domain 5 with libcurl codes
preserved. See [ADR 0066](adr/0066-persistent-tls-streams.md).

## Unix-domain stream clients

`net.UnixStream.connect(&"/run/app/control.sock")` connects to a filesystem
Unix-domain stream socket. It is owned, Transfer and not Share; `read`/`write`
are partial, `write_all` is blocking-only, `shutdown` and consuming `close`
behave as for TcpStream, and destruction closes the descriptor once. Deadlines
use readiness: `set_nonblocking(true)`, `Poll.register_unix`, `Poll.wait` with a
timeout. A missing path is NotFound, a denied one PermissionDenied, and an
empty, NUL-containing or 108-byte-or-longer path InvalidAddress. `UnixStream.pair()` returns a
connected `UnixPair { first, second }` with no filesystem name (ADR 0070); hand
one end to a child with `process.Command.pass`. There is no listener, abstract
namespace, datagram socket, address query, credential passing or async method yet. See [ADR 0069](adr/0069-unix-stream-client.md).

## Manual suspended execution (Phase 12C)

`Execution.new()` owns a single-thread readiness context. An `Operation<R>`
stores a mutable owned poll callback and returns `Progress.Pending` or
`Progress.Ready(R)`. Polling after Ready aborts. Consume the holder with
`finish()` after taking the result to release its captured loans; dropping it
also destroys its state and retires its wake registration.

`read_operation`, `write_operation`, `write_all_operation`, `accept_operation`,
`connect_operation` and `recv_operation` require nonblocking sockets. They retain
ordinary buffer/socket loans across Pending. Read reports partial counts and EOF;
write reports partial counts; write_all retains its offset. UDP preserves empty
datagrams. These operations attempt I/O, arm readiness after WouldBlock, then
retry before returning Pending to close the lost-wakeup window.

`Executor.new()` holds up to 16 void-result operations. `executor.add(operation)`
consumes and returns the executor so transferred loans remain explicit. `turn`
polls each runnable slot at most once, and `run` consumes the executor until all
slots complete. Callbacks can use `waker.wake()` for another turn. Repeated wakes
coalesce; nested operations use `poll_with` to forward readiness to their parent.
All wake use and polling are confined to the creating execution context/thread.

See [the manual I/O fixture](../tests/native/pass/suspended_io.tarn),
[the executor example](../tests/native/pass/executor_turns.tarn),
[ADR 0036](adr/0036-suspended-execution.md) and
[the Phase 12C report](suspended-execution-report.md). This is a manual bootstrap
model. Phase 13 adds `async fn`/`await` over it and explicit `*_async` socket
methods ([ADR 0037](adr/0037-source-async-lowering.md)); public cancellation
remains deferred.
