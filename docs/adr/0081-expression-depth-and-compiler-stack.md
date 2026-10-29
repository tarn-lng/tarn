# ADR 0081: expression tree depth bound and compiler stack

Status: accepted and implemented.

## Evidence

Porting a D-Bus library to pure Tarn exposed three compiler problems with long
expressions. The first, in the type checker, is fixed independently: checking a
call on a method or callable-field result typed its base twice, so a call
chain took exponential time (20 links: 14 s). The other two motivate this ADR:

- `MAX_NESTING` (128, E1020) bounds the parser's own recursion, but method
  chains (`a.f().g()`) and binary operator sequences (`1 + 1 + ...`) are parsed
  in loops. They still build trees one level deeper per link, and every later
  phase (resolution, types, IR lowering, ownership, formatting, `tarn ast`)
  recurses over them. A 1000-call chain overflowed the 8 MiB CLI stack, and a
  300000-term sum overflowed it even after being rejected, because dropping
  the tree recurses too. A compiler abort is never an acceptable diagnostic.
- The lexer computed each token's column by counting characters from the
  start of its line, so one long generated line was quadratic: 300000 terms
  took 19 s before any diagnostic.

Project size is not affected by these bounds. They limit the depth of a single
expression, not the number of files, items or statements. Hand-written chains
rarely exceed a few dozen links; the case that matters is generated code.

## Decisions

- Every semantic phase runs on a dedicated thread with a 256 MiB stack
  (`tarn_driver::COMPILER_STACK`, `with_compiler_stack`). The CLI and the LSP
  run their whole `main` on it, so formatting and syntax dumps are covered as
  well. The size is address space only; Linux commits pages on use.
- After parsing, an iterative walk measures the depth of each body's
  statement and expression tree. Past `MAX_TREE_DEPTH` (8192 AST levels, about
  4096 chained method calls: a call is a call node plus a field node), the file
  gets E1022 `expression_too_deep`. The rejected bodies are then dismantled
  with an explicit stack so that dropping them cannot overflow either.
- 8192 is calibrated, not arbitrary: an unoptimized build checked and compiled
  14000-call chains (about 28000 levels) on the 256 MiB stack, leaving over
  three times room. Optimized builds use less stack per level.
- The lexer caches the previous token's position and counts characters only
  from there when the next token is on the same line.

## Alternatives considered

- **Count chain links in MAX_NESTING.** Simple, but nesting and chains
  multiply: 128 nested levels, each with a long chain, still overflow. Only a
  measurement of the real tree depth bounds what later phases recurse over.
- **A bigger stack alone.** Moves the cliff but keeps the abort.
- **Iterative phases everywhere, or growing the stack on demand.** The only
  ways to remove the bound entirely. Rewriting every recursive phase is large
  and risky; on-demand stack growth needs native code or a dependency. Neither
  is justified until real generated code needs more than 4096 chained calls.

## Consequences

- Programs deeper than the bound get an actionable diagnostic instead of an
  abort; everything below it checks and compiles.
- Every check pays one thread spawn and one linear walk of the bodies.
- Tests in `compiler/driver/tests/scaling.rs` guard 4000-call chains,
  8000-term sums, rejection of 100000-call chains and 300000-term sums without
  an abort, and the linear lexer.
- Revisit with evidence: generated code that needs deeper single expressions,
  or measurements that make the per-check thread noticeable.
