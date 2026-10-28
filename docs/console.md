# Console streams (Phase 31)

`import "console"` provides blocking non-owning views of stdin, stdout and
stderr on Linux x86_64 and ARM64. They never close standard descriptors. Their fields are
private; they do not represent exclusive ownership of process-global streams.

| API | Result |
|---|---|
| `stdin()` | Input view of descriptor 0 |
| `stdout()`, `stderr()` | Output views of descriptors 1 and 2 |
| `Input.read(&mut []u8)` | Result<usize, io.Error>; zero means EOF or an empty buffer |
| `Output.write(&[]u8)` | Result<usize, io.Error>; may write only part of the buffer |
| `Output.write_all(&[]u8)` | Result<void, io.Error>; completes or reports failure |
| `Output.write_text(&string)` | Valid UTF-8 bytes written completely |
| `Output.flush()` | Flushes all libc output buffers, including primitive print |

Tarn console output has no extra buffer. Flush deliberately uses fflush(NULL)
to interoperate with print, and affects all libc output streams, not only this
view. Flush before mixing print with direct writes when output order matters.
It does not flush network/application buffers or promise disk persistence.

Reads/writes retry EINTR. Complete writes reject zero progress as WriteZero.
Broken-pipe output returns an error: a small runtime FFI adapter temporarily
blocks SIGPIPE on the calling thread and restores the mask without changing
process-wide disposition or consuming an already pending signal. The adapter
makes no ownership decisions. Multiple views do not serialize logical records;
concurrent writers may interleave.

Buffers are bytes. UTF-8 validation is explicit through string.from_utf8.
The LineReader and explicit output buffering are described below. There is no
bounded read-to-end helper, terminal control or async console API. Calling
console I/O in an async computation blocks
its executor thread. See [ADR 0060](adr/0060-general-purpose-foundations.md).

## Explicit buffering

`stdin().buffered()` creates LineReader. `read_line(limit)` returns
`Result<Option<string>, io.Error>`: UTF-8 text without LF/CRLF, None at EOF.
Limits count returned bytes and may not exceed 64 MiB. A bare final CR remains
text. Errors poison the reader; subsequent reads report InvalidData.

`stdout().buffered()` and `stderr().buffered()` hold 4096 bytes. Call flush or
consuming finish explicitly. Destruction discards pending bytes and never
performs I/O. Successful partial writes are remembered when flush is retried;
retrying a whole write_all after an error is not a transaction.
