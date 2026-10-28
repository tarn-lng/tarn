# Diagnostics (draft 0)

Diagnostics are part of the product. Every diagnostic has:

| Field | Meaning |
|-------|---------|
| `code` | Stable identifier: `E` + 4 digits for errors, `W` + 4 digits for warnings |
| `severity` | `error`, `warning`, `note` |
| `kind` | snake_case machine name (e.g. `use_after_move`) |
| `message` | one line, lower case, no trailing period |
| `file`, `line`, `column` | 1-based position of the primary span |
| `labels` | spans with short messages (primary and secondary) |
| `notes` | extra facts ("`buffer` was moved on line 12") |
| `help` | an actionable suggestion, when one exists |

## Code ranges

| Range | Phase |
|-------|-------|
| E0001–E0999 | lexer |
| E1001–E1999 | parser |
| E2001–E2999 | name resolution |
| E3001–E3999 | types |
| E4001–E4099 | moves |
| E4101–E4199 | borrows |
| E4201–E4299 | lifetimes / reference escapes |
| E9001–E9999 | driver, I/O, backend |

Codes are never reused. `tarn explain E4001` (future) prints the long form from
`docs/errors/E4001.md`.

## Text format

```
error[E4001]: use of moved value `buffer`
  --> src/main.tarn:13:11
   |
12 |     b := buffer
   |          ------ value moved here
13 |     print(buffer)
   |           ^^^^^^ value used here after move
   |
   = note: `Buffer` is not a copy type
   = help: borrow it instead: `b := &buffer`
```

## JSON format

`tarn check --json` prints one JSON object per line (JSON Lines) so tools can
stream them:

```json
{"code":"E4001","severity":"error","kind":"use_after_move","message":"use of moved value `buffer`","file":"src/main.tarn","line":13,"column":11,"labels":[{"file":"src/main.tarn","line":13,"column":11,"end_line":13,"end_column":17,"primary":true,"message":"value used here after move"}],"notes":["`Buffer` is not a copy type"],"help":"borrow it instead: `b := &buffer`"}
```

## Registered codes

| Code | Kind | Message |
|------|------|---------|
| E0001 | `unexpected_character` | unexpected character `X` |
| E0002 | `unterminated_string` | unterminated string literal |
| E0003 | `invalid_escape` | invalid escape sequence |
| E0004 | `invalid_number` | invalid numeric literal |
| E0005 | `number_out_of_range` | integer literal is too large |
| E0006 | `invalid_interpolation` | `f"..."` holes accept only `{name}` / `{name.field}`; unmatched `}` (ADR 0077) |
| E1001 | `unexpected_token` | expected X, found Y |
| E1002 | `expected_expression` | expected expression, found X |
| E1003 | `expected_type` | expected type, found X |
| E1004 | `expected_pattern` | expected pattern, found X |
| E1005 | `expected_statement_end` | expected end of statement, found X |
| E1006 | `chained_comparison` | comparison operators cannot be chained |
| E1007 | `compound_assignment` | compound assignment `+=` is not supported |
| E1008 | `invalid_assignment_target` | cannot assign to this expression |
| E1009 | `struct_literal_in_condition` | struct literal not allowed here |
| E1010 | `unclosed_delimiter` | unclosed delimiter `{` |
| E1011 | `missing_function_body` | function `f` has no body |
| E1012 | `unexpected_function_body` | interface methods cannot have a body |
| E1013 | `expected_item` | expected item, found X |
| E1014 | `misplaced_receiver` | `self` is only allowed as the first parameter of a method |
| E1015 | `invalid_pub` | `pub` is not allowed here |
| E1016 | `else_on_new_line` | `else` must be on the same line as `}` |
| E1017 | `chained_range` | range operators cannot be chained |
| E1018 | `missing_type_colon` | a type annotation on a binding needs `:` |
| E1019 | `uninit_var_needs_type` | `var x` without a value needs a type |
| E1020 | `nesting_too_deep` | expressions, blocks, types or patterns nested more than 128 levels |
| E1021 | `generic_interface` | interfaces cannot have type parameters |
| E2001 | `undefined_name` | cannot find `x` in this scope |
| E2002 | `duplicate_definition` | `x` is defined more than once |
| E2003 | `duplicate_in_scope` | `x` is already declared in this scope |
| E2004 | `used_before_declaration` | `x` is used before its declaration |
| E2005 | `no_such_member` | enum `E` has no member `x` |
| E2006 | `private_item` | `x` is private to module `m` |
| E2007 | `module_not_found` | cannot find module `m` |
| E2008 | `not_a_type` | expected a type, found function `f` |
| E2010 | `ambiguous_method` | `m` is implemented by more than one interface for this type |
| E2011 | `invalid_method_owner` | cannot declare method `m` on interface `I` |
| E2012 | `not_an_interface` | expected an interface in `impl`, found struct `S` |
| E2013 | `not_an_interface_member` | `m` is not a method of interface `I` |
| E2014 | `missing_interface_method` | `impl I` is missing `m` |
| E2015 | `self_outside_method` | `self` is only available in methods with a receiver |
| E2016 | `not_a_struct` | expected a struct, found variant `E.V` |
| E2017 | `variant_name_case` | variant `v` must start with an uppercase letter |
| E2018 | `expected_variant` | expected a variant, found `f` |
| E2019 | `owner_arity` | `Pair` has 2 type parameters, but the method declares 0 |
| E2020 | `duplicate_impl` | `I` is implemented more than once for `T` |
| E2021 | `owner_binder_bound` | bounds are not allowed on method owner parameters |
| E2022 | `specialized_impl` | impl/method type arguments must be fresh parameter names |
| E2023 | `impl_coherence` | `impl I for T` must be in the module of `I` or `T` |
| E2024 | `invalid_impl_target` | interfaces can only be implemented for structs and enums |
| E2025 | `type_arity` | `Pair` takes 2 type arguments, but 1 was given |
| E3001 | `type_mismatch` | expected `A`, found `B` |
| E3002 | `wrong_arg_count` | `f` takes 2 arguments, but 1 was given |
| E3003 | `not_callable` | struct `User` cannot be called |
| E3004 | `no_field` | `User` has no field `x` |
| E3005 | `no_method` | `T` has no method `m` |
| E3006 | `invalid_operands` | cannot apply `+` to `i32` and `i64` |
| E3007 | `invalid_unary` | cannot negate an unsigned `u32` |
| E3008 | `not_indexable` | cannot index into a value of type `T` |
| E3009 | `missing_return` | function `f` may end without returning `T` |
| E3010 | `cannot_infer` | cannot infer the type of `x` |
| E3011 | `missing_fields` | missing field `age` in `User` |
| E3012 | `unknown_field` | struct `User` has no field `x` |
| E3013 | `duplicate_field_init` | field `x` is given twice |
| E3014 | `private_field` | field `x` of `T` is private |
| E3015 | `assign_immutable` | cannot assign to `x`, because `x` is not declared with `var` |
| E3016 | `borrow_mut_immutable` | cannot borrow `x` as mutable, because `x` is not declared with `var` |
| E3017 | `try_mismatch` | `try` would return an error of type `A` from a function whose error type is `B` |
| E3018 | `non_exhaustive_match` | `match` does not cover `V` |
| E3019 | `unknown_variant` | enum `E` has no variant `V` |
| E3020 | `pattern_mismatch` | variant `E.V` has 1 field, but the pattern has 2 |
| E3022 | `bound_not_satisfied` | `T` does not implement `I` |
| E3023 | `impl_signature_mismatch` | method `m` does not match its declaration in `I` |
| E3024 | `copy_with_non_copy` | `copy` type `T` contains `string`, which is not copy |
| E3025 | `literal_out_of_range` | literal `256` does not fit in `u8` |
| E3026 | `not_printable` | `print` cannot print a value of type `T` |
| E3027 | `invalid_for_iter` | cannot iterate over `T` |
| E3028 | `return_value_mismatch` | this function returns `void`, but a value is returned |
| E3029 | `invalid_conversion` | cannot convert `string` to `u64` |
| E3030 | `move_out_of_reference` | `m` takes `self` by value, but only a reference is available |
| E3031 | `extern_call_outside_unsafe` | calling the extern function `f` requires `unsafe` |
| E3032 | `interface_as_type` | interface `I` cannot be used as a type directly |
| E3033 | `array_length` | array length must be an integer literal |
| E3034 | `range_outside_for` | ranges can only be used in `for` loops and slicing |
| E3035 | `not_a_value` | struct `User` is not a value |
| E3036 | `spawn_needs_call` | invalid spawn operand (legacy call or native zero-parameter owned closure) |
| E3037 | `array_length_mismatch` | array of length 3 has 2 elements |
| E3038 | `variant_needs_args` | variant `E.V` takes 1 value |
| E2026 | `impl_copy` | `Copy` is not implemented with `impl` |
| E2027 | `intrinsic_outside_core` | `extern "intrinsic"` functions require trusted embedded stdlib declarations |
| W3001 | `unreachable_arm` | this arm can never match |
| W3002 | `unreachable_code` | unreachable code |
| E3039 | `slice_by_value` | a slice can only be used through a reference |
| E3040 | `unmodeled_std_api` | stdlib API/type lacks an ownership and provenance contract; use explicit Tarn declarations |
| E3041 | `owned_dynamic_interface` | dynamic interfaces are borrowed only; use `&any I` or `&mut any I` |
| E3044 | `callable_access` | mutable invocation through shared storage, or consuming invocation through a reference |
| E3042 | `invalid_semantic_contract` | invalid borrowed-result source clause on a declaration |
| E3047 | `task_capability_required` | a task capture or result lacks cross-thread capability evidence |
| E3048 | `semantic_capability_impl` | Transfer/Share authority cannot be granted by an ordinary impl |
| E3049 | `scoped_task_borrowed_result` | borrowed scoped task results remain unsupported |
| E3050 | `semantic_capability_dynamic` | Transfer/Share cannot be used as dynamic interface objects |
| E4001 | `use_after_move` | use of (possibly) moved value `x` |
| E4002 | `use_of_partially_moved` | use of (possibly) partially moved value `p` |
| E4003 | `move_out_of_reference` | cannot move a value out from behind a reference |
| E4004 | `move_out_of_index` | cannot move an element out of an array by index |
| E4005 | `uninitialized` | use of (possibly) uninitialized `x` |
| E4006 | `assign_into_moved` | cannot assign to `p.f`: `p` has been moved |
| W2001 | `confusing_shadow` | `x` is shadowed in an inner scope and used again after it |
| W2002 | `shadows_item` | `x` shadows the builtin function `x` |
| W2003 | `unused_import` | unused import `m` |
| E4101 | `conflicting_borrow` | cannot borrow `x` as mutable because it is also borrowed as shared |
| E4102 | `assign_while_borrowed` | cannot assign to `x` because it is borrowed |
| E4103 | `move_while_borrowed` | cannot move out of `x` because it is borrowed |
| E4104 | `use_while_mutably_borrowed` | cannot use `x` while it is mutably borrowed |
| E4105 | `does_not_live_long_enough` | `x` does not live long enough |
| E4201 | `reference_escapes` | cannot return a reference to `x` |
| E4202 | `ambiguous_provenance` | cannot infer where the returned reference comes from |
| E4203 | `reference_in_field` | `T` cannot hold a reference in its fields |
| E4204 | `provenance_mismatch` | implementation returns a borrow its interface does not allow |
| E4205 | `closure_escapes_borrow` | returned closure captures `k` by reference |
| E4206 | `reference_to_spawned_task` | unscoped spawn cannot receive borrowed captures/references in 11A |
| E4208 | `scoped_task_escape` | scoped task handles must remain and complete in their creating scope |
| E4207 | `borrow_escapes_through_reference` | cannot store a reference to `x` into `out.f`, directly or through a call that may keep it (ADR 0068) |
| E4209 | `borrow_in_async_task` | a spawned async task may borrow only the Execution it runs on |
| E4210 | `callable_parameter_escapes` | a closure keeps one of its reference parameters in captured state (ADR 0068) |

Golden tests in `tests/**/fail/*.tarn` pin the rendered text of each code.

## Phase-13 checkpoint diagnostics

| Code | Name | Meaning |
|------|------|---------|
| E3051 | `unmodeled_replacement_loans` | a native guard replacement cannot accept new payload loans without a stored-loan effect contract |
| E3076 | `not_displayable` | an interpolated value's type does not implement `Display` (ADR 0077) |
| E3077 | `impl_bound_not_satisfied` | a bounded impl applies only when its type arguments satisfy its bounds; reports the root cause and chain (ADR 0078) |
| E3078 | `missing_main` | `tarn build`/`tarn run` found no `fn main` in the entry module; `tarn check` still accepts libraries |
| E3073 | `self_through_dynamic_interface` | a method using `Self` beyond its receiver cannot be called through `any I`; use `<T: I>` |
| E3074 | `expanding_instantiation` | recursive generic calls grow a type argument without bound, so specialization would never finish |
| E3075 | `value_too_large` | a concrete value exceeds the v0 native limits (64 KiB, 4096 array elements, 64 nesting levels) |
| E3071 | `unsafe_call_outside_unsafe` | calling an `unsafe fn` requires an `unsafe` block (ADR 0047) |
| E3072 | `unsized_pointee` | raw pointers point to sized values; use `ffi.slice` for a slice's first element |
| E3060 | `await_outside_async` | await requires an async function context; an ordinary closure does not inherit that context |
| E3061 | `not_awaitable` | the operand is neither a source async computation nor the trusted Phase-12C Operation declaration |
| E3062 | `async_lowering_unavailable` | retired: temporary Phase-13 checkpoint gate, removed when frame lowering landed; never reused |

E3062 was a temporary gate that prevented native compilation of async
declarations before their state machines and destruction were implemented.
Async bodies now lower onto verified frames (ADR 0037); the code is retired.

## Phase 31 reference places

- E3079 `deref_move`: a non-Copy value cannot be moved out of a reference; borrow or clone it instead.
- E3080 `deref_non_reference`: `*` requires a reference; raw pointers use unsafe FFI helpers.

These two were briefly emitted as E3074 and E3075, which already meant
`expanding_instantiation` and `value_too_large`; they were renumbered so each
code has one meaning.

## Static C callbacks

- E3081 `invalid_c_callback`: `ffi.callback` must be called directly with a
  named ordinary non-generic, non-async, safe Tarn function. Closures and
  callable locals cannot supply a static C entry point.
- E3082 `unsafe_function_value`: an ordinary `fn` value cannot preserve the
  unsafe requirement of an `unsafe fn` or foreign function. Call directly
  inside `unsafe`, or implement a wrapper that checks its preconditions.
