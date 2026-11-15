# ADR 0108: must-consume types

Status: accepted and implemented (systems readiness L1).

## Motivation and syntax

Move-only ownership prevents duplicate use but permits losing the last owner.
A transaction, payment authorization, physical frame, or application protocol
session may instead require an explicit disposition. This is a static language
capability, independent of any allocator, operating system, or resource name.
Ordinary application values remain affine: their implicit drops are unchanged.

```tarn
#[must_consume]
pub struct Ticket {
    id u64
}

pub fn settle(ticket Ticket) {
    match ticket {
        Ticket{id} => record_settlement(id)
    }
}
```

The existing attribute syntax avoids reserving another keyword and fits the
existing declaration metadata style. Only structs accept this marker in this
version; enums acquire the property through payloads. A marker has no arguments,
cannot repeat, and is incompatible with `copy` (E1023). Every field must be
private (E3086). Consumers and borrowing accessors are ordinary functions, with
no new implementation block, destructor protocol, or annotation on callers.

## Obligations and normal control flow

A marked value must be transferred to an owned parameter, field, binding, or
return place. A new binding transfers rather than cancels the obligation. Each
checked consumer must itself transfer or explicitly dismantle the resource.
Implicit drop is E4007, even when it would only discard Copy fields at runtime.
The rule applies to parameters, locals, statement temporaries, discarded call
results, wildcard bindings and patterns, overwrites (including through mutable
references), branch joins, loop backedges, `break`, `continue`, `return`, and
prefix `try` error propagation. Tarn has no postfix `?` or `if let`: use its
existing `match` syntax for optional resources.

```tarn
fn take(option Option<Ticket>) {
    match option {
        Some(ticket) => settle(ticket)
        None => print(0)
    }
}
```

Only live payloads carry an obligation. The None path is fine; dropping Some
without consuming its ticket is not. Known empty variants remain empty through
moves and aggregate construction. An unknown result still needs an exhaustive
match or transfer. An infinite computation need not finish an obligation; all
reachable normal exits must. Panic aborts (ADR 0026), with no unwinding or
cleanup, and therefore has no obligation-discharge edge. A panic cannot be
caught to resume with a lost resource. Async abandonment is a normal disposal
path, not panic, and is checked.

## Dismantling the representation

Only the defining module can structurally match a marked struct (E3014).
A complete, owned struct pattern on a whole local explicitly dismantles its
nominal obligation. The compiler moves/copies the bound fields, checks ordinary
drops of all remaining owned fields, then ends the source local's storage.
Moving a Copy field with `ticket.id` alone is only observation and does not
satisfy the obligation. Borrowed patterns never dismantle the owner.

All fields must be named; wildcard field patterns explicitly discard ordinary
fields, but cannot discard a linear field. Unnamed siblings retain obligations.
For a marked value nested in a field, bind that value to a local before
structurally dismantling it. Projected/nested marked patterns are conservatively
rejected at the eventual drop. Ordinary wrappers may be partially moved; only
the remaining linear fields are checked. No public-field escape is introduced.

## Containment, generics and erasure

Struct fields, enum payloads, nonempty arrays and generic owning type arguments
propagate obligations. References and raw pointers do not own their referents;
zero-length arrays have no elements to consume. Generic arguments propagate
even through opaque/native storage, conservatively. Thus a
`Box<Ticket>`, `Option<Ticket>` and `Vec<Ticket>` cannot be silently dropped.

Generic functions keep their existing source signatures. The ownership phase
checks additional instances keyed by a finite vector of linearity bits for
the function's type parameters. Direct calls, function values, generated task
workers/results, and nested closures propagate those bits to a worklist. Each
new bit instance reuses the same CFG move/init analysis; it does not generate
specialized code or compute layouts. A generic forwarding function passes; an
unconstrained generic `forget<T>(value T) {}` is rejected when T is linear.
Normal string/scalar instantiations of that same function remain valid.

This first version rejects operations where erasure or cancellation would hide
ownership (E4008), rather than trusting runtime drop glue to satisfy linearity:

- Native/intrinsic containers and operations involving linear storage, including
  Vec creation, mutation, and element-dropping methods, are rejected. An explicit
  drain protocol with a proven empty remainder is future work.
- Consuming array loops retain the array remainder as live: even a complete
  drain is conservatively rejected, because the existing move analysis does
  not prove the iteration cursor exhausted. Arrays can still be transferred
  whole. Loop bodies with ordinary individual resources work normally.
- An owned closure cannot capture a linear resource. Borrowing local closures
  work; callable types erase capture types, and reusable environments have
  implicit destructors. A future linear callable type could lift this limit.
- A call through a callable value cannot consume a linear argument. Virtual
  operations involving linear receivers/arguments/results are conservatively
  rejected. Direct checked functions and methods work, including generics.
  Borrowed dynamic dispatch without a linear resource is unchanged.
- Async functions cannot own linear inputs or outputs: a computation can be
  destroyed before its first poll. Borrowing is supported. Linear locals are
  checked along both resume and abandonment edges, so a resource created and
  consumed before suspension is fine, while one live across an await is not.
- Raw owning casts/coercions cannot erase a linear resource. Ordinary borrowing
  and access to private scalar fields do not discharge ownership. As with other
  ownership contracts, an explicit extern-C consuming declaration is a trusted
  FFI boundary; its implementation must fulfill its by-value contract.

These limits are general, diagnosed language restrictions, not special cases
for frames or a particular module. There is no unchecked safe `forget` escape.

## Implementation boundary and cost

The AST records only the source marker. The type declarations retain nominal
marker identities and one containment query. The IR reuses existing Assign,
Drop and StorageDead operations; complete struct dismantling shortens storage
only after checking field dispositions. No IR opcode, runtime flag, ABI, native
layout, allocation, or backend change is needed.

Move paths stay sparse for ordinary code. For linear aggregates, the checker
completes structural paths so unmentioned siblings and variant payloads cannot
escape checking. A parallel set of possibly live obligation bits uses the
existing forward CFG worklist and union joins. Assignments and moves transfer
these bits. Enum construction and discriminant-switch edges exclude impossible
payloads only from the obligation bits: ordinary initialization state and
executable drop plans keep their established semantics. Drop sites recurse
through contained fields and active variants, using those bits; nominal marked
structs require explicit dismantling even after field observation.

The reporting pass emits E4007 with the owner, drop location, actionable help,
and known by-value function names. E4008 names unsupported operations. Existing
move diagnostics and borrow checking remain authoritative for initialization
and aliasing. Programs without markers skip instance analysis and structural
completion. Recursive containment uses inductive cycle detection and a
conservative budget; structural descent is bounded conservatively at 64 levels.
No runtime cost is added, and the Cranelift backend is unchanged.

## Validation and proving-ground change

Move fixtures cover scope exit, return, branches, break/continue, overwrite,
discard, partial fields, arrays, Option/Result paths, try, generic forwarding
and leaking, generic function values, closures, native containers, async, and
panic. Type fixtures cover private representation and foreign dismantling.
A native fixture compiles and runs consumption through generic wrappers,
branches and an optional payload using the existing backend suite.

Cairn is pinned and is not modified in this patch. The exact follow-up is
specified in [the implementation report](../must-consume-report.md): mark Frame
and FrameRange, explicitly dismantle them in their module's consuming functions,
and require every caller to transfer or free them. Address-only intrusive
stacks also need their own nominal obligation and consuming disposal operation;
the compiler cannot infer resource ownership from a u64 link.
