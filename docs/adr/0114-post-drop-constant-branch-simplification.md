# ADR 0114 — Post-drop constant branch simplification in the native backend

Status: proposed / implemented locally in Tarn 0.5.x (Cairn phase 11C).

## Context

In Cairn's freestanding kernel (`kernel/debug/selftest.tarn`), boot self-tests
are dispatched on `buildcfg.CASE` (an imported `pub const CASE u64 = 0` in
normal boots) and via immutable aliases such as `c := buildcfg.CASE; if c == 0 { return }`.
Even when `buildcfg.CASE` is `0`, `tarn` emits:

```text
_1 = eq(0_u64, 1_u64)
switch copy _1 [0 => bb2, _ => bb1]
```

Because `Switch` retains both successors and their `Call` instructions in the
IR lowered to Cranelift, the emitted `.o` file contains machine code and ELF
relocations to unreachable test routines (`usertest`, `caphandle`, `security`,
etc.). Link-time `--gc-sections` follows those relocations and keeps the dead
test routines resident in the normal release `kernel.elf`, exhausting the
3,360 KiB resident budget (ADR 0049) when `NODES` is raised from 256 to 384.

## Decision

Add a post-drop, pre-codegen constant branch simplification pass
(`tarn_ir::simplify_constant_branches`) invoked in
`tarn_backend::emit_entry_object` after `mono::specialize_*`, `inline::inline`,
and `tarn_ir::optimize_scalar_loops`, and before `post_drop::verify` and
`codegen::emit_entry`:

1. **Preserve all frontend and ownership checks**:
   - Type checking, move/initialization checking, borrow checking, and drop
     elaboration still run on the complete, unpruned CFG. Dead branches
     never bypass static safety or diagnostic checks.
2. **Conservative scalar constant propagation**:
   - Track constants intra-block across `Assign` / `StorageLive` /
     `StorageDead` / `Call`, and across blocks only for non-addressed,
     non-parameter locals assigned at most once (`assign_count == 1`,
     `storage_live_count <= 1`).
   - Evaluate pure `Use`, `Unary`, `Binary`, and `Cast` over `Bool` and `Int`
     constants using checked arithmetic (`IntTy::range`). Operations that
     would overflow or divide by zero return `None`, preserving runtime traps.
   - Statements are not deleted, preserving side effects and traps on
     reachable paths.
3. **Switch simplification and unreachable-block pruning**:
   - When a `Terminator::Switch` discriminant evaluates to a constant `Bool`
     or `Int`, replace the `Switch` with `Terminator::Goto(target)` for the
     matching case (or `otherwise`).
   - Compact `function.blocks` to blocks reachable from `BlockId(0)` and
     remap `BlockId`s, eliminating dead `Call` edges and their ELF relocations.
   - Run `post_drop::verify(&concrete, t)` immediately afterward to enforce
     all post-drop IR invariants before codegen.

## Consequences

- Constant `buildcfg.CASE` checks and immutable aliases simplify to `Goto` and
  unreachable test blocks are pruned before codegen, allowing `--gc-sections`
  to collect unreachable test routines without any source-level workaround in
  Cairn.
- Runtime conditions retain both branches, and overflow/div-by-zero traps on
  reachable paths are preserved.
