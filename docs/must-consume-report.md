# Must-consume types: systems readiness L1

Design: [ADR 0108](adr/0108-must-consume-types.md).

## Result

`#[must_consume]` opts a private-field, non-Copy struct into explicit
consumption. Borrowing and ownership transfer keep their existing syntax.
Every normal exit, overwrite, discarded temporary and `try` error edge is
checked. Complete owned struct matching in the defining module dismantles
the nominal resource. An ordinary field read does not.

```tarn
#[must_consume]
struct Frame { addr u64 }
fn free(frame Frame) {
    match frame { Frame{addr} => print(addr) }
}
fn main() {
    frame := Frame{addr: 4096}
    free(frame)
}
```

Omitting `free(frame)` reports, for example:

```text
error[E4007]: must-consume value `frame` of type `Frame` would be implicitly dropped
  ... this path leaves an unconsumed resource
  ... resource is held here
  = note: functions taking `Frame` by value: free
  = help: move it into a consuming call, a returned value, or an owned field on every path; only its defining module may explicitly destructure it
```

Containment propagates through struct fields, enum payloads, nonempty arrays,
and owning generic arguments. Empty Option/Result variants carry no payload
obligation; consuming their live payload on each active arm is sufficient.
Generic forwarding works; generic discarding is rejected for linear instances.
Panic aborts, so a compiler-proven panic edge needs no cleanup. There is no
unwinding or runtime linearity machinery. The backend is unchanged.

## Exact pinned Cairn follow-up

Cairn was read, not edited. Its current `kernel/mm/frames.tarn` explicitly
states that lost Frames and nonempty address-only stacks leak silently. Apply
these edits when its Tarn toolchain pin can advance to this patch:

1. Put `#[must_consume]` immediately before `pub struct Frame` and
   `pub struct FrameRange`. Keep their scalar fields private and their
   borrowing accessors unchanged. Replace the opening ownership comment's
   silent-leak limitation with the E4007 guarantee and the requirement to
   free, transfer into a mapping, or store in an explicit owner.
2. In `frame_into_phys(frame Frame)`, replace the plain `return frame.addr`
   with `match frame { Frame{addr} => return addr }`. This explicitly ends the
   token obligation as ownership passes to the page-table subsystem; it
   does not free the underlying allocation. `frame_adopt` is unchanged.
3. In `frame_free(frame Frame)`, wrap the current implementation in
   `match frame { Frame{addr} => { ... } }`, replacing every `frame.addr`
   with the bound `addr`. Keep all bitmap validation, clearing, free counts,
   and hints. In `frame_free_range(range FrameRange)`, likewise use
   `match range { FrameRange{start, frames} => { ... } }`, replacing
   `range.start` and `range.frames` with the bindings. Each consumer now
   explicitly dismantles its token before returning normally.
4. In `FrameStack.push(&mut self, frame Frame)`, first bind
   `addr := frame_into_phys(frame)`, then write the link with
   `physmem.write64(addr, self.head)`, assign `self.head = addr`, and increment
   depth as before. In `FrameRangeStack.push`, explicitly match the input
   `range` as `FrameRange{start, frames}` around its existing writes and use
   those bindings. These functions transfer token ownership into intrusive
   storage instead of silently dropping the token after observing fields.
5. Also mark `FrameStack` and `FrameRangeStack` with `#[must_consume]`. Their
   `head`/`depth` fields are u64 and cannot structurally communicate ownership
   of linked frames; a nominal marker is necessary. Add consuming
   `pub fn FrameStack.finish(self)` and
   `pub fn FrameRangeStack.finish(self)`: each completely matches its own
   `{head, depth}` and panics if either is nonzero, otherwise returns. This
   explicitly verifies and discharges an empty stack's obligation. Keep
   `pop` returning Option and its existing link updates. Optionally expose a
   separately named consuming `free_all(self)` that drains every payload
   through `frame_free`/`frame_free_range`, then calls `finish`; do not add a
   silent native destructor or a generic forget escape.
6. In `kernel/debug/frametest.tarn`, add `singles.finish()` after the singles
   drain, `chunks.finish()` after the chunks drain (save `nchunks` before
   consuming it, as the code already does), `live.finish()` after each
   stress batch's drain, and `held.finish()` after the multi-range drain.
   These are the four stack construction sites. Keep their existing
   payload-consuming match arms and free-count assertions.
7. Three impossible-success test arms currently bind a token and call the
   void-returning helper `fail`: single-frame allocation at exhaustion,
   contiguous allocation at exhaustion, and allocation above `available()`.
   Add `frames.frame_free(frame)` or `frames.frame_free_range(range)` before
   the existing `fail(...)` in those arms. The compiler does not infer that
   a void helper never returns merely because its implementation panics;
   a direct `panic` arm is another explicit option. Do not depend on
   interprocedural panic inference for resource disposition.
8. Existing normal callers in `kernel/boot/boot.tarn`, `kernel/mm/heap.tarn`,
   `kernel/mm/vm.tarn`, and `kernel/debug/vmtest.tarn` already transfer frames
   to `frame_free`, `vm.map`, or `frame_into_phys`, or return them through
   `vm.unmap`/`frame_adopt`. Preserve those paths and audit diagnostics after
   changing the pin. Borrowing `address`/read/write calls stay unchanged.
   Add Cairn compile-fail cases for a lost Frame, early return, discarded
   allocation result, and unfinished FrameStack, alongside its existing
   double-free/use-after-free tests; keep positive boot/frame/vm tests.

These edits remove the documented silent-leak workaround. Intrusive stack
contents and page-table ownership are deliberately represented by explicit
module-level contracts: the compiler cannot infer ownership from a copied
physical address. Do not claim that the marker verifies arbitrary bitmap or
page-table accounting.

## Supported subset and limits

L1.1 supersedes the original native-storage limitation: see [owning slots](owning-slots-report.md) and [ADR 0110](adr/0110-owning-slots.md).

Only structs have the explicit marker; enums propagate it through payloads.
Other erasing native/virtual
operations on linear storage, owned closure captures, indirect consuming
calls, and linear async inputs/outputs reject E4008. Borrowing closures and
async inputs work. Async local resources must also satisfy abandonment paths.
Consuming array loops conservatively retain the remainder and therefore do
not yet support draining linear elements, even on an exhaustive iteration.
Nested marked destructuring must first move the resource into a local.
FFI consuming declarations retain the existing trusted by-value contract.
See the ADR for the complete rationale and future extensions.

## Validation

The final run used the macOS CI helpers on PATH and Cargo offline:

```sh
PATH="$PWD/tools/ci/macos:$PATH" cargo test --offline --no-fail-fast \
  -p tarn_types -p tarn_driver -p tarn_ir -p tarn \
  -p tarn_ownership -p tarn_parser -p tarn_ast
```

Result: **138 passed, 0 failed**, across 42 test binaries/doc-test groups.
The existing type, move, borrow, drop, IR, mutation, scaling, CLI and parser
suites pass. There are 59 new fixture cases: 20 move passes, 31 move failures,
five parser cases, two type cases (one multi-module), and one native case.
The one updated existing parser diagnostic now describes the supported struct
marker when rejecting a function-only attribute on a struct.

Additional backend verification:

```sh
PATH="$PWD/tools/ci/macos:$PATH" cargo test --offline --no-fail-fast \
  -p tarn_backend --test native --test vector_iteration
```

Result: **26 passed, 1 known baseline timing failure**. The native suite passed
24 of 25 tests, including every scalar/aggregate fixture (and the new linear
fixture), executable drops, closures, tasks, ABI validation and mutation checks.
The pre-existing `call_chains_and_wide_enums_build_in_linear_time` threshold
failed with `wide-enum: build took 131.42367581s`; this is the documented
nonblocking macOS baseline. Both vector-iteration tests passed. No new failure
was observed, and no backend source changed.

The final compiler also ran `tests/native/pass/linear_consumption.tarn`
directly and matched its expected `1\n1\n` output with empty stderr.
Additional check-only probes passed for a linear Result error propagated by
`try`, generic Option unwrapping, and indirect calls borrowing a Frame before
final consumption. Golden diagnostic files retain the suite's canonical blank
separator at EOF; the staged whitespace check passes with that expected
`blank-at-eof` check disabled.
