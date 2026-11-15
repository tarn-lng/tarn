# Systems readiness plan (from the Cairn OS proving ground)

Status: planned. Source: friction recorded while building Cairn phase 0
(`../cairn/docs/phase-0-progress.md`), after patches T1–T6 (ADRs 0102–0107).

## Rule

Building an operating system is the proving ground. Every item here must be a
**general language or toolchain capability with its own ADR**, valuable for
applications, servers and libraries too — never a Cairn-specific shortcut,
special case or escape hatch. Items that are only workarounds are rejected.

## Language (highest value)

| # | Capability | Friction seen | Why it is general value |
|---|---|---|---|
| L1 | **Must-consume (linear) types**: a type marked so that values cannot be silently dropped; the compiler requires an explicit consuming call (`frame_free`, `close`, `commit`) on every path | A `Frame` that goes out of scope leaks physical memory silently | Leaks of any resource (frames, file handles, transactions, locks, sockets, payments) become compile errors; few languages have this |
| L2 | **Generic interfaces** (interfaces with type parameters) | `rangeset` and `bitmap` each declare their own `Words` interface (E1021) | Reusable containers, iterators, codecs, collections |
| L3 | **Type aliases** | `rangeset.RangeSet<words.MemWords>` spelled everywhere | Readability across all code |
| L4 | **Repeat array literal** (`[N]T` filled with one value) | Every local array spelled element by element | Buffers, tables, matrices |
| L5 | **Indexing on `Vec` and slices with `v[i]`**, bounds-checked | Tests use `at`/`replace` | Ordinary ergonomics |
| L6 | **Explicit integer behaviour operators/methods**: `wrapping_*`, `saturating_*`, `checked_*`, `overflowing_*` (arithmetic stays checked by default) | Hash and test patterns had to avoid multiplication | Hashing, crypto, checksums, DSP, emulators |
| L7 | **Returning string literals as `&string`** (static lifetime inferred) | E4201 on `fn name() &string { return "x" }` | Error messages, tables of names, i18n keys |

## Toolchain

| # | Capability | Friction seen | General value |
|---|---|---|---|
| C1 | **Path / workspace dependencies** locked by source hash, no publish step | Libs had to be staged by script; registry releases are immutable | Any multi-package project, monorepos |
| C2 | **Linker-defined symbols as typed extern statics** (`extern static __kernel_end u8`) | Image bounds found through marker statics | Embedded, firmware, plugins, any custom link layout |
| C3 | **Allocator contract symbols required only when the program allocates** | Kernel had to define stub `tarn_free` before it had a heap | Smallest freestanding binaries, bootloaders |

## Architecture intrinsics (complete the set)

| # | Capability | Need |
|---|---|---|
| A1 | aarch64 `SPSel` write and current `sp` read | Exceptions on a separate stack → real stack-overflow reports on aarch64 |
| A2 | aarch64 `msr pan`, x86 `stac`/`clac` | Kernel/user memory separation (PAN, SMAP) for phase 2 |
| A3 | One-way `switch_stack` (no return) next to `call_on_stack` | Context switch and task entry (phase 1) |

## Rejected (shortcuts, not language value)

- A divide intrinsic just to provoke #DE in tests (test-only; crafted bytes are fine there).
- Statement separators `;` for one-line bodies: newline-terminated statements are a deliberate design (ADR 0003).

## Order

1. L1 (must-consume types) — the flagship: turns the OS's resource discipline into a language guarantee.
2. C1 (path/workspace dependencies) — unblocks clean modular builds.
3. A1–A3 — required by Cairn phases 1–2.
4. L2, L3, L5, L6 — broad ergonomics.
5. L4, L7, C2, C3 — smaller completions.

Each lands as its own 0.5.x patch with an ADR, tests, docs, and a Cairn
change that removes the corresponding workaround (proof that it was needed).
