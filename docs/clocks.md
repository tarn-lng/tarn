# Clocks (Phase 31)

The existing time module adds clocks without a new scheduler:

- `Instant.now() Result<Instant, io.Error>` reads CLOCK_MONOTONIC.
- `later.since(earlier) Result<Duration, io.Error>` rejects reversed instants.
- `instant.elapsed() Result<Duration, io.Error>` measures elapsed monotonic time.
- `now_utc() Result<Timestamp, io.Error>` reads CLOCK_REALTIME.
- Timestamp exposes signed `unix_seconds` and `nanos` in 0..1,000,000,000.

Instant's representation is private. Durations round down to whole milliseconds;
clock acquisition failures remain I/O errors. Elapsed arithmetic is checked and
can abort on an unrepresentable duration. Wall time can move backwards when the
system clock changes; use Instant for intervals. UTC here means a Unix epoch
representation, not leap-second/calendar/time-zone formatting support.

The bridge uses libc clock_gettime and the two-i64 timespec layout on supported
LP64 Linux x86_64/ARM64 through unsafe FFI. Public APIs remain safe. No time-zone
database, date parser, wall-clock deadline API or platform expansion is added.
Design: [ADR 0060](adr/0060-general-purpose-foundations.md).
