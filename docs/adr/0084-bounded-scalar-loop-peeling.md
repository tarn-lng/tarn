# ADR 0084: bounded scalar loop peeling with executable range proofs

Status: accepted for implementation.

## Context

A loop-carried value can become bounded after its first iteration even when its
initial caller input is unbounded. Merging both states at the same loop header
prevents the existing conservative range analysis from using the steady-state
invariant. Deleting the first iteration's checks would be unsound.

## Decision

After specialization and bounded inlining, peel one conditional iteration of a
small, two-block, scalar-only loop in verified post-drop IR. Copy its condition
and body; redirect external entries to the copied condition and the copied body
back to the original header. The original body's back edge stays unchanged.
Zero-trip execution reaches the same exit without running a body. Every later
iteration executes the same statements in the same order as before.

Only ordinary synchronous bodies are eligible. The header is a false/otherwise
switch, its sole-entry body ends in a goto back to that header, and both blocks
contain only plain operations on unaddressed integer/bool locals. References,
projections, calls, resource/drop/flag operations, suspension and multi-block
bodies are excluded. A constant unsigned remainder and remaining checked
arithmetic provide a reason to seek new bounds. No particular recurrence or
benchmark constants are recognized.

Budgets: at most 48 statements/terminators per copied loop, 256 per function,
and 16 loops per function. Only original blocks are candidates. These are
optimization budgets, not language or program-size limits.

The existing IR interval analysis is then reused on a scalar mirror of the
executable CFG. Plain statements and terminators retain their positions; opaque
flag/destruction operations stay unchanged in the executable IR. The analysis
still excludes addressed locals, including those captured by reference. Only
arithmetic proof annotations are copied back. The post-drop verifier runs again
before code generation. No ownership, borrow or destruction decision is
recomputed or weakened, and the signature/locals declaration retains no second
CFG.

This placement avoids duplicating diagnostics or borrow-analysis paths in
invalid source programs: peeling operates only after successful ownership and
executable-IR verification. Generic specializations and inlined callees already
have their concrete locals and explicit executable operations.

`TARN_NO_RANGE_PROOFS` disables both arithmetic proofs and peeling, preserving
the existing differential-testing escape hatch. There is no source annotation
or opt-in performance mode.

## Safety

The first copy retains unproven checks on the initial input. Only subsequent
entries may rely on ranges produced by that copy and the ordinary back edge.
For a recurrence reduced modulo m, the next state is in [0,m-1], but the first
multiplication/addition still aborts when the unbounded seed requires it. An
unbounded checksum/accumulator likewise retains its check. An operation becomes
Proven only through the same conservative IR proof used before this change.

The copied blocks have no references, flags or resource effects. Existing
resource operations and flag state therefore see exactly the same scalar
updates and iteration count; their explicit plans are not cloned or changed.
The mirrored range analysis does not grant facts about aliased storage or
consult ownership metadata. Existing destruction, concurrency, native fault
and mutation suites remain mandatory guards.

## Evidence and limits

Native tests compare several different recurrences with checked arithmetic,
including zero trips, extreme initial seeds, the first multiplication/addition
faults and checksum overflow. Structural assertions require both the checked
prefix and proven steady-state multiplication. Additional tests exercise the
address-taken fallback and per-function growth budget.

This does not optimize arbitrary loops, references, arrays or async frames.
Broader peeling or loop versioning needs separate control-flow, aliasing,
compile-time and code-size evidence. The scalar-only restriction is deliberate.
