# Blocking child processes

```tarn
import "process"
import "io"
fn main() Result<void, io.Error> {
    command := process.Command.new("/bin/printf").arg("hello %s").arg("日本")
    output := try command.output()
    print(try output.stdout_text())
    print(output.status.success())
    return Ok(())
}
```

`Command.new(string)` consumes its program. `arg(string)` and
`current_dir(string)` consume the command and text; `args(&[]string)` copies
arguments. Calls can be chained. `start()`, `status()` and `output()` borrow the
configuration, so one command can launch several children. `start` is used
because `spawn` is a language keyword. No implicit shell or string tokenization
occurs; a space, dollar sign or semicolon in an argument is ordinary data.
An explicit `/bin/sh` command remains possible when the application wants one.

The environment and executable lookup PATH are inherited; `Command.env(name,
value)` overrides one child variable (Phase 25) without touching the parent. `current_dir` affects
only the child, never Tarn's global current directory. NUL, empty program and
empty explicit directory are InvalidInput; missing executable/directory is
NotFound, denied execution is PermissionDenied. Other native errors retain
`Error.native_code()`. All fallible APIs return `io.Error` with `try` support.

`start() -> Result<Process, io.Error>` inherits stdin/stdout/stderr. Process is
owned, non-Copy, Transfer and not Share. `id()` borrows and returns a diagnostic
PID; `kill()` borrows mutably and requests SIGKILL, keeping ownership.
`wait()` consumes completion authority, including on error. Destruction waits
and reaps a live child; it never detaches or implicitly kills it. Moving a child
transfers this responsibility, including into/through a native Task.

`status()` starts and waits. ExitStatus is Copy, with `Exited(i32)` and
`Signaled(i32)` cases; `success()`, `code()` and `signal()` are explicit queries.
Nonzero exit and signal termination are observations, not I/O failures.

`output()` uses /dev/null stdin and captures both streams as owned `Vec<u8>`.
Two native tasks drain the pipes concurrently before joining them, so large
stdout/stderr cannot deadlock through sequential draining. `Output` exposes
`status`, `stdout`, `stderr`; `stdout_text()` and `stderr_text()` validate UTF-8
and return owned copies or InvalidData. Binary/NUL bytes remain intact.

All operations are blocking on the current pthread. Captures are unbounded;
a child that never exits or descendants that hold pipe writers can block
completion indefinitely. Calling these APIs from an async body also blocks
its executor thread. There are no process groups or detach; stdio modes, handoffs, bounded waits and async I/O are below (ADR 0070–0072).
Reader-task creation and buffer allocation retain the runtime's existing
abort-on-resource-failure policy.

The child closes unrelated fds >=3, inherits only the configured stdio, has an
empty signal mask and default SIGPIPE. The implementation uses posix_spawnp
and GNU libc 2.34+ spawn actions on Linux x86_64 and ARM64. External C must respect child
ownership: foreign waitpid or SIGCHLD policy changes can invalidate it.
See [ADR 0045](adr/0045-blocking-processes.md).

## Stdio, handoffs and bounded waits (ADR 0070, ADR 0071)

Linux 5.3 or newer is required for bounded waits (pidfd). The supported target
is a current kernel.

Before spawning, the runtime flushes `print`'s hidden stdout buffer, so an
inheriting child cannot overtake earlier prints. Program-owned buffers such as
`console.BufferedOutput` are never flushed implicitly.

### Stdio

`stdin(mode)`, `stdout(mode)` and `stderr(mode)` take `Stdio.Inherit` (the
default), `Stdio.Null` (/dev/null) or `Stdio.Piped`. `output()` keeps its
/dev/null stdin and captured stdout/stderr.

`Piped` requires `start_child()`, which returns:

```tarn
pub struct Child {
    pub stdin Option<ChildStdin>
    pub stdout Option<ChildStdout>
    pub stderr Option<ChildStderr>
    pub process Process
}
```

`start`/`status` with a piped stream are InvalidInput. `ChildStdin` has
`write`, `write_all` and consuming `close` (end of file). A finished reader
makes writes BrokenPipe, never SIGPIPE. `ChildStdout`/`ChildStderr` have
`read` (0 is end of file), `read_all` and `close`. All three have
`set_nonblocking`, `register(&mut net.Poll)` and `deregister`. A nonblocking
pipe with no data is WouldBlock. The ends are owned, Transfer and not Share,
and close exactly once.

Fields drop in declaration order, so destroying a Child closes its pipes
before `process` waits: a child reading stdin until end of file cannot
deadlock the destruction. `Child.wait()` closes stdin and waits; stdout/stderr
stay open, so drain them first or concurrently, as with any pipe.
`Child.communicate(input Vec<u8>)` does that for you: it writes stdin and
drains stdout/stderr on three native tasks, then waits and returns `Output`.
Unread input is not an error when the child exits early (ADR 0072).

### Async

`ChildStdout.read_async`, `ChildStderr.read_async`, `ChildStdin.write_async`
and `write_all_async` use the socket Pending/Ready/Waker model. They need
`set_nonblocking(true)`. `Process.wait_async(self)` waits on an owned pidfd
without blocking the executor, then reaps. For deadlines, compose with
`runtime.run_timeout` or `AsyncTask.join_timeout`. An abandoned `wait_async`
destroys its Process, which waits as always, so kill a child that may not
exit before abandoning it.

### Handoffs

`pass(stream net.UnixStream)` and `pass_file(file fs.File)` turn a Command
into a one-shot `Launch`. The k-th handoff of either kind becomes the child's
descriptor 3 + k; nothing else is inherited. `Launch.start`, `status` and
`start_child` consume the Launch, so the parent's copies close when it
launches, on success or failure, even if the Launch was kept in a variable.
The peer sees end of stream once the child exits. Using a Launch twice is a
use-after-move error (E4001). More than 64 handoffs are InvalidInput.

```tarn
pair := try net.UnixStream.pair()
var channel = pair.first
var child = try process.Command.new("server").arg("--fd=3").stdout(process.Stdio.Null).pass(pair.second).start()
```

### Bounded waits

`try_wait(&mut self)` and `wait_timeout(&mut self, millis)` return
`Option<ExitStatus>`: None means the child is still running and still owned.
They observe completion without reaping it, so `wait()` or destruction remains
the single completion and returns at once afterwards. `terminate()` requests SIGTERM
and `kill()` SIGKILL; both keep ownership. A graceful bounded shutdown is
`terminate()`, `wait_timeout(ms)`, then `kill()` if still running, and finally
`wait()` (ADR 0073). A negative timeout is InvalidInput.
Destruction still waits and never kills: kill a child that may never exit
before an error path can destroy it.
