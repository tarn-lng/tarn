# ADR 0113 — Stack-slot liveness reuse in the native backend

Status: accepted and implemented in Tarn 0.5.5 (Cairn phase 9D).

## Evidence

In Cairn's Phase 9D installer (`services/installer.elf`), synchronous functions
with sequential scopes or sequential error-checked steps (`execute_from_journal`
with a `26,704`-byte frame and `start` with a `16,256`-byte frame) overflowed
Cairn's 64 KiB user-mode stack (`#PF` at `0x7fffffeefef0`, `0x110` bytes below
`STACK_BOTTOM = 0x7fffffef0000` inside `blk_call`).

A minimal Tarn 0.5.4 reproduction (`seq_scopes` with 8 sequential `{ x_i :=
make(i); ... }` blocks over a 64-byte struct) compiles to `subq $0x810, %rsp`
(`2,064` bytes), assigning disjoint stack offsets `0x400..0x780` instead of
reusing non-overlapping stack slots.

In `compiler/backend/src/codegen.rs`:
- Every non-scalar `LocalId` in `f.decl.locals` unconditionally allocates a
  fresh Cranelift `ExplicitSlot` (`Slot::Stack(self.stack(lyt.size, lyt.align))`).
- `StatementKind::StorageLive` and `StatementKind::StorageDead` are ignored
  (`{}`) during lowering, even though `compiler/ir/src/lower.rs` emits them and
  `compiler/ir/src/async_frame.rs` already computes backward liveness over
  `post::Function`.

## Decision

Add deterministic, conservative stack-slot interference analysis and slot
packing for synchronous `Slot::Stack` locals in `compiler/backend/src/codegen.rs`:

1. **Conservative pinning**:
   - Pin the return place (`LocalId(0)`), all function parameters
     (`1..=param_count`), and every address-taken local (`Rvalue::Ref`,
     `Rvalue::SliceRef`, or borrowed closure storage witness) for the entire
     function; pinned locals never share a `StackSlot`.
   - Async frame-resident locals (`Slot::Frame`) are unchanged.

2. **Sound backward liveness and interference**:
   - Compute backward liveness over `post::Function` (all blocks, terminators,
     projected places, drops, and element drop-flag roots).
   - `StorageDead` never kills liveness; only unprojected writes
     (`Assign`/`Call`) and `StorageLive` kill a non-pinned local on backward
     traversal.
   - Any two non-pinned locals live at the same program point—or written/read
     simultaneously across an `Assign` or `Call`—interfere.
   - Keep live-local sets sparse per block and visit a deterministic worklist
     in reverse reverse-postorder (postorder for backward analysis), revisiting
     predecessors only when a block's input changes. Include unreachable CFG
     components conservatively, using iterative traversal rather than host
     recursion. Store only actual interference edges between shareable stack
     locals; do not allocate a dense locals-by-block or locals-by-locals matrix.

3. **Deterministic packing**:
   - Sort non-pinned `Slot::Stack` locals deterministically by
     `(size DESC, align DESC, local_id ASC)` and greedily assign each to the
     first physical `StackSlot` containing no interfering local.
   - Each shared `cl::StackSlot` takes `max(size)` and `max(align)` over its
     members; every member sits at offset `0`, preserving alignment and drop
     semantics.
   - Ordered sets and stable local/block ranks make the worklist and graph
     deterministic; no `HashMap`/`HashSet` iteration order affects codegen.

## Consequences

- Sequential scopes and sequential error-checked steps reuse dead stack slots
  (`O(max live)` instead of `O(sum of all locals)`), without changing Cairn's
  kernel, ABI, MMU guard page, or 64 KiB user stack limit.
- Pinned locals remain isolated so references, derived pointers, and ABI
  out-parameters cannot alias reused storage.
