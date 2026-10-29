# Name resolution (phase 3)

Implementation: `compiler/resolve`. Decisions: ADR 0009, 0013, 0014.

## Scopes

```
Prelude                         primitives, print/panic/channel, Option/Result/Error/
 └─ Module (one per file)       Channel/Sender, Some/None/Ok/Err
     ├─ Item                    generics of struct/enum/interface (fields/variants resolved here)
     │   └─ Function            interface method signatures
     └─ Function                generics, self, params, top level of the body
         ├─ Block               if/else bodies, `{}`, unsafe, scope, while-style for
         ├─ Loop                `for x in` variable + top level of loop body
         ├─ MatchArm            pattern bindings + guard + top level of arm body
         └─ Closure             closure params + top level of closure body
```

Each scope has a kind, a parent, a span and its symbols in declaration order.
Lookup walks parents.

## Symbols

`SymbolKind`: `Primitive`, `Builtin`, `PreludeType`, `Module(Local|Std|Missing)`,
`Function`, `Struct`, `Enum`, `Interface`, `Variant{parent}`,
`Method{owner}`, `InterfaceMethod{interface}`, `ImplMethod{interface,target}`,
`GenericParam`, `Param`, `SelfParam`, `Local{mutable}`, `PatternBinding`,
`LoopBinding`, `ClosureParam`.

Each symbol stores name, kind, defining module and `NodeId`, name span, scope,
`pub`, and for locals the owning body (function or closure) — used to compute
closure captures.

## Visibility

- Inside a module: every module-level name, regardless of declaration order.
- From another module: `mod.name` only if `name` is `pub` (E2006). Imports
  are never re-exported. Embedded stdlib declarations have real members and
  visibility checks; unmodeled bootstrap APIs reject E3040 during type checking.
- Methods (`fn T.m`): reachable as `T.m` from anywhere `T` is reachable; `pub`
  on methods is recorded for the type checker (method visibility across
  modules is enforced when value method calls are resolved).
- Locals: from the next statement to the end of their scope.
- Fields: resolved by the type checker.

## Shadowing

| Situation | Result |
|-----------|--------|
| Same scope (incl. param vs. body top level, loop var vs. loop body top level) | E2003 |
| Inner scope hides an outer local, outer not used afterwards | allowed |
| Inner scope hides an outer local that is used after the inner scope | W2001 (once) |
| Inner scope hides an outer `var` | W2001 |
| Local hides a module item, import, builtin or prelude type | W2002 |

## Imports

`import "p/q"` binds `q`. Local files use `<root>/p/q.tarn`; verified package
dependencies use isolated roots, and a local/dependency ambiguity is rejected.
Trusted stdlib modules always load embedded sources. Ordinary bundled modules
can be overridden by local files. Missing imports fail E2007 (with spelling
hints); see [packages](packages.md) for dependency resolution and
[architecture](architecture.md) for stdlib layers. Local module cycles are
allowed; package dependency cycles are rejected. Unused imports: W2003.

## Methods and impls

- `fn T.m`: `T` must be a struct/enum in the same module (E2011); `m` joins
  `T`'s members together with its variants (clash: E2002).
- `impl I for T`: `I` must be an interface (E2012); each method must be
  declared by `I` (E2013); every method of `I` must be present (E2014).
- `T.m` looks up variants and inherent methods first, then impl methods for
  `T`; two impls providing `m` make `T.m` ambiguous (E2010).
- Calls on values (`x.m()`) are resolved by the type checker.

## Patterns (ADR 0014)

| Pattern | Classification |
|---------|----------------|
| `x`, `_x` (lowercase) | new binding |
| `_` | wildcard |
| `Name` / `Name(..)` (capitalized, unqualified) | lexically visible variant (prelude) or `ScrutineeVariant` for the type checker |
| `a.B(..)`, `a.B` | path; must resolve to a variant (E2018) |
| `foo(..)` (lowercase) | E2018 |
| `P{f, g: p}` | `P` must be a struct (E2016); `f` binds |

## Diagnostics

E2001 undefined name (with variant / spelling hints) · E2002 duplicate
definition · E2003 duplicate in scope · E2004 used before declaration ·
E2005 no such member · E2006 private item · E2007 module not found · E2008
not a type · E2010 ambiguous method · E2011 invalid method owner · E2012 not
an interface · E2013 not an interface member · E2014 missing interface method
· E2015 `self` outside method · E2016 not a struct · E2017 variant name case ·
E2018 expected variant · W2001 confusing shadow · W2002 shadows item · W2003
unused import.
