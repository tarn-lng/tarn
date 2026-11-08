# ADR 0098: macOS port

Status: in progress. macOS is not a supported or released platform yet. Extends
the platform scope beyond ADR 0079, which did not cover macOS.

## Evidence

The maintainer asked for this port, which reverses the earlier rule against macOS
work in AGENTS.md. The port so far shows that the platform surface is small: the
embedded C runtime, the object format and link step in the backend, and a few
Linux assumptions in the stdlib and tests.

## Decision

Port the existing runtime model instead of adding a platform layer. Language
semantics, ownership analysis and safety rules do not change, and no frontend
phase knows the platform.

### Runtime

macOS differences stay in `runtime/native.c` behind `__APPLE__`.

- **Readiness.** kqueue replaces epoll, with one filter each for read and write.
  Events are merged by token, so the Tarn side still sees one event per
  descriptor. There is no second readiness or wake mechanism.
- **Timers and child exit.** A private kqueue stands in for timerfd and pidfd. A
  kqueue descriptor is itself pollable, so both work on the shared Poll.
- **Socket identity.** macOS has no `SO_COOKIE`. A disabled knote in one private
  queue witnesses each identified socket. The kernel deletes it when the
  descriptor closes, so a recycled number cannot inherit an earlier identity
  (ADR 0035). The queue is one descriptor that lives until process exit.
- **Errors.** Stdlib modules classify errors by Tarn's canonical errno numbering,
  which is Linux's. Inside the runtime `errno` and `E*` stay native. Generated
  code translates a raw result at the point where it re-enters Tarn
  (`tarn_rt_raw_canonical`), for the OS-errno domains only. A Darwin-only value
  is offset by 1000 so it cannot alias a Linux value.
- **SIGPIPE.** Sockets use `SO_NOSIGPIPE`, because the send flag is unavailable.
  XNU raises a pipe write's SIGPIPE on the process, not on the writing thread, so
  the thread mask used on Linux would let another thread take the signal and kill
  the program. Pipe writes therefore set `F_SETNOSIGPIPE`: permanently on
  parent-owned child stdin pipes, and only during the write on standard streams,
  whose open file description may be shared with other processes. Standard
  streams use a reference count so concurrent writers cannot clear the flag early.
- **Spawn.** `POSIX_SPAWN_CLOEXEC_DEFAULT` replaces closefrom, and each inherited
  stream is named explicitly. It also covers the non-atomic window between
  creating a descriptor and setting `FD_CLOEXEC`. PATH search never reruns an
  `ENOEXEC` file through a shell, which `posix_spawnp` does on macOS.
- **Threads.** Task threads request 8 MiB of stack to match Linux, instead of the
  512 KiB macOS default for secondary threads.
- **UDP.** A zero-capacity receive discards the datagram through a one-byte
  scratch buffer. macOS would otherwise leave it queued, where Linux discards it.
- **Shutdown.** `ENOTCONN` from shutting down a connected stream whose peer has
  gone is success, matching Linux. Datagram sockets keep the error.

Portable helpers replace Linux-specific constants and struct layouts in the
stdlib: `tarn_clock_read`, `tarn_net_share_port`, `tarn_os_disk_space` and
`tarn_os_args_block`. `__errno_location` and `getrandom` exist on macOS under
those names for the stdlib modules that call them.

### Backend

Differences are mechanical. Mach-O executables are position independent, string
literals live in `__TEXT,tarn_strings` with linker-provided bounds, narrow runtime
parameters are zero-extended because clang relies on the caller to do it, and
Apple linker errors are translated to source-level names. `--link :file` is a GNU
linker form and is rejected on macOS with an explanation.

### Packages

The package store forbids symbolic links in its paths. macOS ships `/var`, `/tmp`
and `/etc` as root-owned links into `/private`, so those exact links are exempt.
Changing them requires administrator access, so the exemption gives package data
no way to redirect a path.

### Tests

`tools/ci/macos` must be first on `PATH`. It supplies `timeout`, which macOS does
not ship, `stdbuf`, which macOS 15 does not ship, and a test-only `cc` that emulates GNU `--wrap`, which Apple's linker
lacks. Neither helper is used by `tarn build`. Fault tests inject Linux-numbered
errors and translate them to native values.

File descriptor audits count from after the process-lifetime descriptors: the
witness queue and libSystem's resolver state. They do not filter by descriptor
type, because that would hide a leaked Poll.

Tests ignored on macOS, each for a platform reason:

- `string_literals_do_not_allocate_per_evaluation`: counts mallocs through
  `LD_PRELOAD`, which dyld does not honor.
- `invalid_directory_names_fail_without_leaking_the_cursor`: APFS refuses to
  create the non-UTF-8 name the test needs.

## A portable bug found by the port

A nonblocking TLS write that returns `CURLE_AGAIN` must be retried from the same
address. `websocket.Client` keeps queueing frames while blocked, so its buffer
could grow and move, and the retry failed with `bad write retry` (curl code 55).
Nothing in the bridge guaranteed this on any platform. macOS exposed it in the
existing WebSocket fixture.

Each nonblocking attempt now starts from a private 64 KiB buffer owned by the TLS
stream, allocated on first use, so no caller pointer survives a call. Copying only
after the first `CURLE_AGAIN` is too late, because the retry would already come
from a different address. Bytes already committed to a TLS record cannot be
replaced, so a retry with different data marks the stream failed.

## Alternatives

- **A platform abstraction layer in Tarn.** Rejected: it would add a layer for one
  extra target, and AGENTS.md asks for cross-platform abstractions only for a real
  architectural need.
- **Exposing native errno to Tarn.** Rejected: every stdlib classification would
  then need per-platform tables, and programs would see different codes.
- **Ignoring SIGPIPE process-wide.** Rejected: ADR 0073 keeps process-wide signal
  disposition unchanged.
- **Reusing the Poll's kqueue as the identity witness.** Rejected: its real
  filters use the same identifiers, and a disabled witness would disable them.

## Consequences and risks

- `SO_REUSEPORT` allows the shared bind on macOS but does not spread connections
  evenly, so ADR 0058's load distribution does not hold there.
- `__errno_location` returns a canonical copy on macOS, not the writable native
  errno.
- `tarn_os_args_block` allocates a block that lives until exit on each `os.args`
  call. The cost is small and bounded by the number of calls.
- Apple's libcurl selects its TLS backend at load time. `CURL_SSL_BACKEND` can
  change it, and the fixture's self-signed CA fails verification under
  SecureTransport. This needs a documented position before HTTPS is called
  supported.

## Remaining before macOS is supported

- Get the `test-macos` CI job passing on x86_64 and ARM64, then make it a required
  check. Local work has run only on x86_64, so the AArch64 code paths, including
  the `cc` helper's trampoline, are unverified until that job passes.
- Run the Linux suite, because the shared runtime, stdlib and test changes need
  Linux CI before merging.
- Accept macOS in `tools/release/platform.sh` and `install.sh`, and add release
  archives and their code-signing position.
- Update `docs/releases.md`, the stdlib comments that still name Linux, and any
  module-level limits.
