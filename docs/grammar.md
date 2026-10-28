# Tarn grammar (implemented syntax)

EBNF-ish notation: `{x}` zero or more, `[x]` optional, `|` alternative,
`NL` a `Newline` token. Newline handling (ADR 0008) is summarized after the
grammar; inside `( )`, `[ ]`, `< >` and literal braces every `NL` is skipped.

```ebnf
module      = { NL } { item ( NL | EOF ) { NL } } EOF ;

item        = [ "pub" ] ( import | fn_decl | extern_fn | struct_decl
                        | enum_decl | interface | impl_decl ) ;
import      = "import" STRING ;
extern_fn   = "extern" STRING "fn" fn_tail ;            (* body optional *)
fn_decl     = [ "async" | "unsafe" ] "fn" fn_tail ;
fn_tail     = IDENT [ generics ] [ "." IDENT [ generics ] ]
              "(" [ params ] ")" [ type ] [ borrow_clause ] [ block ] ;
              (* `fn Pair<A, B>.m<C>(…)`: a list before `.` binds the owner's
                 type parameters (ADR 0015); the token after it decides *)
params      = param { "," param } [ "," ] ;
param       = receiver | IDENT type ;
receiver    = "self" | "&" "self" | "&" "mut" "self" ;  (* first param of a method only *)
generics    = "<" gparam { "," gparam } ">" ;
gparam      = IDENT [ ":" path { "+" path } ] ;

struct_decl = [ "copy" ] "struct" IDENT [ generics ] body(field) ;
field       = [ "pub" ] IDENT type ;
enum_decl   = [ "copy" ] "enum" IDENT [ generics ] body(variant) ;
variant     = IDENT [ "(" type { "," type } [ "," ] ")" ] ;
interface   = "interface" IDENT [ generics ] body(fn_decl) ;   (* no bodies *)
impl_decl   = "impl" path "for" impl_target body(fn_decl) ;
impl_target = IDENT { "." IDENT } [ "<" gparam { "," gparam } ">" ] ;
              (* target arguments bind parameters; bounds are allowed, ADR 0078 *)
body(m)     = "{" { NL } [ m { NL { NL } m } ] { NL } "}" ;   (* one member per line *)

type        = path
            | "&" [ "mut" ] type
            | "*" [ "mut" ] type                  (* raw FFI pointer, ADR 0047 *)
            | "[" "]" type                       (* slice *)
            | "[" expr "]" type                  (* array *)
            | [ "mut" | "once" ] "fn" "(" [ type { "," type } ] ")" [ type ]
            | "any" path ;                       (* dynamic interface *)
path        = IDENT { "." IDENT } [ "<" type { "," type } ">" ] ;

block       = "{" { NL } [ stmt { NL { NL } stmt } ] { NL } "}" ;
stmt        = "return" [ expr ]
            | "break" | "continue"
            | "var" IDENT [ ":" type ] "=" expr
            | "var" IDENT ":" type                  (* uninitialized, ADR 0024 *)
            | IDENT [ ":" type ] ":=" expr
            | expr [ "=" expr ]                  (* assignment target: name, field, index, *reference *)
            | if_stmt | for_stmt | match_stmt
            | block
            | "unsafe" block
            | "scope" block                      (* contextual *)
            | "spawn" expr ;
if_stmt     = "if" cond block [ "else" ( if_stmt | block ) ] ;  (* else on the `}` line *)
for_stmt    = "for" ( block | IDENT "in" cond block | cond block ) ;
match_stmt  = "match" cond "{" { NL } { arm NL { NL } } "}" ;
arm         = pattern [ "if" expr ] "=>" stmt ;
cond        = expr ;                             (* no bare struct literals *)

expr        = [ range_lhs ] ( ".." | "..=" ) [ binary ] | binary ;
binary      = unary { BINOP binary } ;           (* precedence climbing, see below *)
unary       = ( "-" | "!" | "*" | "&" [ "mut" ] | "try" | "await" | "spawn" ) unary | postfix ;
postfix     = primary { "." IDENT | "(" [ args ] ")" | "[" expr "]"
                      | "{" [ field_inits ] "}" } ;   (* struct literal: path-only primary *)
primary     = INT | FLOAT | STRING | FSTRING | "true" | "false" | IDENT
            | "(" ")" | "(" expr ")"
            | ( "[" "]" type | "[" expr "]" type ) "{" [ args ] "}"   (* array literal *)
            | [ "move" ] "fn" "(" [ cparam { "," cparam } ] ")" [ type ] block ;  (* closure *)
cparam      = IDENT [ type ] ;
args        = expr { "," expr } [ "," ] ;
field_inits = field_init { "," field_init } [ "," ] ;
field_init  = IDENT [ ":" expr ] ;

pattern     = "_" | literal [ ( ".." | "..=" ) literal ]
            | path [ "(" [ pattern { "," pattern } ] ")"
                   | "{" [ IDENT [ ":" pattern ] { "," ... } ] "}" ] ;
literal     = [ "-" ] ( INT | FLOAT ) | STRING | "true" | "false" ;
```

## Operator precedence (loosest first)

| Level | Operators | Associativity |
|-------|-----------|---------------|
| 0 | `..` `..=` | none — `a..b..c` is E1017 |
| 1 | `\|\|` | left |
| 2 | `&&` | left |
| 3 | `==` `!=` `<` `<=` `>` `>=` | none — `a < b < c` is E1006 |
| 4 | `\|` | left |
| 5 | `^` | left |
| 6 | `&` | left |
| 7 | `<<` `>>` | left |
| 8 | `+` `-` | left |
| 9 | `*` `/` `%` | left |
| 10 | prefix `-` `!` `*` `&` `&mut` `try` `await` `spawn` | right (prefix) |
| 11 | postfix `.name` `(args)` `[i]` `Path{...}` | left |

Bitwise operators bind tighter than comparisons (unlike C), so
`a & mask == 0` means `(a & mask) == 0`. `try` is a prefix operator:
`try a.b() + 1` is `(try a.b()) + 1`.
`try await f()` is `try (await f())`; `await f() + 1` is
`(await f()) + 1`. Async blocks, async closures and nested function items are
not introduced. The modifier is recognized on free and inherent method items;
async interface/impl declarations remain outside the supported surface. Async
bodies lower onto Phase-12C suspended execution (ADR 0037).

## Newlines (ADR 0008)

1. Inside `( )`, `[ ]`, generic `< >`, struct literal and array literal braces:
   ignored.
2. Inside blocks and item bodies: they end statements/members.
3. Skipped after a binary operator, prefix operator, `:=`, `=`, `=>`, `.`.
4. In postfix position, newlines followed by `.` continue the chain.
5. A line starting with a binary operator is a new (invalid) statement:
   E1002 with a hint.
6. `else` must follow `}` on the same line: E1016.

## Disambiguation rules

- The parser is deterministic: bounded lookahead, no backtracking (ADR 0011).
- `IDENT :=` → immutable binding; `IDENT :` → annotated binding
  `x: T := e`. Nothing else starts with `IDENT :` in statement position.
- The draft-0 forms `x T := e` / `var x T = e` are diagnosed (E1018) with a
  fix-it; the check scans the line for `:=` only to produce that diagnostic.
- `scope` followed by `{` at statement start is a scope block; otherwise an
  identifier.
- `any` followed by an identifier in type position is a dynamic type.
- `self` / `&self` / `&mut self` is a receiver only as first parameter.
- A `{` after a path-only expression (`User`, `geometry.Point`) starts a
  struct literal, except in `if`/`for`/`match` heads (E1009 if one is found).
- `>>` closing two generic argument lists and `&&` in prefix/type position are
  split into two tokens.
- A bare identifier pattern is not classified by the parser (binding vs. unit
  variant is name resolution's job).

## Interpolated string tokens

`FSTRING` is the lexer's cooked `f"..."` token (ADR 0077). Interpolation holes
contain only an identifier or field path; `{{` and `}}` escape braces.
Arbitrary expressions in holes are rejected. `///` comments remain lexer trivia;
the stdlib catalog associates preceding comments without attaching them to AST
items.

## Outside the implemented subset

Explicit generic arguments in expressions remain a v0 restriction (ADR 0012).
Attributes, `const`/statics, type aliases, labeled breaks, numeric literal
suffixes and char literals are not supported. The parser accepts some broader
declaration forms for diagnostics; semantic restrictions still apply, including
no generic interfaces (E1021), no `pub impl` (E1015), and no method-owner bounds
(E2021). Interface/impl methods start with `fn`, not `async fn` or `unsafe fn`.

## Phase 9 declaration provenance extension

After the optional return type, a function/interface declaration may contain:

```
borrow_clause = "borrows" "(" identifier { "," identifier } [ "," ] ")"
```

`borrows` is contextual here, not a globally reserved keyword. Semantics require a
bodyless borrowed-result declaration and distinct reference inputs (`self` is
allowed for a borrowed receiver). A clause on a body is rejected, and normal
body provenance remains inferred. ADR 0031 defines validation and source indices.

`move` and `once` are contextual before `fn`; `mut` remains reserved syntax.
