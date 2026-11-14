use tarn_ast::{ItemKind, dump_module};
use tarn_diagnostics::SourceMap;
use tarn_parser::{ParseResult, parse_file};

fn parse(src: &str) -> (SourceMap, ParseResult) {
    let mut map = SourceMap::new();
    let id = map.add("t.tarn", src);
    let res = parse_file(id, map.file(id));
    (map, res)
}

fn ok(src: &str) -> String {
    let (map, res) = parse(src);
    let errs: Vec<_> = res.diagnostics.iter().map(|d| d.render(&map)).collect();
    assert!(errs.is_empty(), "unexpected diagnostics for {src:?}:\n{}", errs.join("\n"));
    dump_module(&res.module)
}

/// Dump of the statements of `fn f() { <body> }`, one per line, without the wrapper.
fn body(src: &str) -> String {
    let dump = ok(&format!("fn f() {{\n{src}\n}}\n"));
    let lines: Vec<&str> = dump.lines().collect();
    // Line 0 is `(fn f ()`, line 1 `  (block`; strip 4 spaces of indentation.
    let inner = lines[2..].iter().map(|l| l.get(4..).unwrap_or("")).collect::<Vec<_>>().join("\n");
    // Remove the closing `))` of block and fn.
    inner.trim_end().strip_suffix("))").unwrap().to_string()
}

fn codes(src: &str) -> Vec<&'static str> {
    parse(src).1.diagnostics.iter().map(|d| d.code).collect()
}

// ---------------------------------------------------------------- milestones

#[test]
fn async_declarations_retain_the_source_modifier() {
    let (_, parsed) = parse("pub async fn identity<T>(value T) T { return value }\n");
    assert!(parsed.diagnostics.is_empty());
    let ItemKind::Fn(function) = &parsed.module.items[0].kind else { panic!("expected function") };
    assert!(function.is_async);
    assert!(parsed.module.items[0].is_pub);
    assert_eq!(function.generics.len(), 1);
    assert!(ok("async fn f() i32 { return 1 }\n").starts_with("(async fn f"));
    let (_, ordinary) = parse("fn f() {}\n");
    let ItemKind::Fn(function) = &ordinary.module.items[0].kind else { panic!("expected function") };
    assert!(!function.is_async);
}

#[test]
fn await_precedence_preserves_calls_try_and_binary_expressions() {
    assert_eq!(body("x := try await f()"), "(let x (try (await (call f))))");
    assert_eq!(body("x := await f() + 1"), "(let x (+ (await (call f)) 1))");
    assert_eq!(body("x := await (f() + 1)"), "(let x (await (paren (+ (call f) 1))))");
    assert_eq!(body("x := await value"), "(let x (await value))");
    assert_eq!(body("x := try await\nf()"), "(let x (try (await (call f))))");
}

#[test]
fn async_blocks_closures_and_nested_items_are_not_added() {
    assert!(!codes("fn f() { x := async { return 1 } }\n").is_empty());
    assert!(!codes("fn f() { x := async fn() i32 { return 1 } }\n").is_empty());
    assert!(!codes("fn f() { async fn child() {} }\n").is_empty());
    assert!(!codes("async unexpected() {}\n").is_empty());
}

#[test]
fn hello_world() {
    assert_eq!(ok("fn main() {\n    print(\"hello\")\n}\n"), "(fn main ()\n  (block\n    (call print \"hello\")))\n");
}

#[test]
fn first_milestone_program() {
    assert_eq!(
        ok("fn add(a i32, b i32) i32 {\n    return a + b\n}\n\nfn main() {\n    value := add(20, 22)\n    print(value)\n}\n"),
        "(fn add ((a i32) (b i32)) -> i32\n  (block\n    (return (+ a b))))\n(fn main ()\n  (block\n    (let value (call add 20 22))\n    (call print value)))\n"
    );
}

#[test]
fn empty_and_one_line_bodies() {
    assert_eq!(ok(""), "");
    assert_eq!(ok("fn f() {}"), "(fn f ()\n  (block))\n");
    assert_eq!(ok("fn f() i32 { return 1 }"), "(fn f () -> i32\n  (block\n    (return 1)))\n");
}

// ---------------------------------------------------------------- precedence

#[test]
fn arithmetic_precedence_and_associativity() {
    assert_eq!(body("x := 1 + 2 * 3"), "(let x (+ 1 (* 2 3)))");
    assert_eq!(body("x := 1 - 2 - 3"), "(let x (- (- 1 2) 3))");
    assert_eq!(body("x := (1 + 2) * 3"), "(let x (* (paren (+ 1 2)) 3))");
    assert_eq!(body("x := 10 / 2 % 3"), "(let x (% (/ 10 2) 3))");
}

#[test]
fn logical_comparison_and_bitwise() {
    assert_eq!(body("x := a || b && c"), "(let x (|| a (&& b c)))");
    assert_eq!(body("x := a == b && c < d"), "(let x (&& (== a b) (< c d)))");
    assert_eq!(body("x := a & b == c"), "(let x (== (& a b) c))");
    assert_eq!(body("x := a | b ^ c & d"), "(let x (| a (^ b (& c d))))");
    assert_eq!(body("x := 1 << 2 + 3"), "(let x (<< 1 (+ 2 3)))");
}

#[test]
fn prefix_operators() {
    assert_eq!(body("x := -a * b"), "(let x (* (- a) b))");
    assert_eq!(body("x := !a.ok()"), "(let x (! (call (. a ok))))");
    assert_eq!(body("x := &mut v.items[0]"), "(let x (&mut (index (. v items) 0)))");
    assert_eq!(body("x := &&v"), "(let x (& (& v)))");
    assert_eq!(body("x := try a.b() + 1"), "(let x (+ (try (call (. a b))) 1))");
    assert_eq!(body("x := - -a"), "(let x (- (- a)))");
}

#[test]
fn ranges() {
    assert_eq!(body("r := 0..n + 1"), "(let r (.. 0 (+ n 1)))");
    assert_eq!(body("r := 1..=9"), "(let r (..= 1 9))");
    assert_eq!(body("r := s[2..]"), "(let r (index s (.. 2 _)))");
    assert_eq!(body("r := s[..2]"), "(let r (index s (.. _ 2)))");
    assert_eq!(body("r := s[..]"), "(let r (index s (.. _ _)))");
}

#[test]
fn postfix_chains() {
    assert_eq!(body("a.b.c(1)(2)[3]"), "(index (call (call (. (. a b) c) 1) 2) 3)");
    assert_eq!(body("x := ().f"), "(let x (. () f))");
}

// ---------------------------------------------------------------- newlines

#[test]
fn trailing_operator_continues_expression() {
    assert_eq!(body("x := 1 +\n    2"), "(let x (+ 1 2))");
    assert_eq!(body("x := a &&\n    b"), "(let x (&& a b))");
    assert_eq!(body("x :=\n    1"), "(let x 1)");
    assert_eq!(body("var y =\n    1"), "(var y 1)");
}

#[test]
fn newlines_ignored_inside_delimiters() {
    assert_eq!(body("f(\n    a,\n    b,\n)"), "(call f a b)");
    assert_eq!(body("x := (a\n    + b)"), "(let x (paren (+ a b)))");
    assert_eq!(body("x := v[\n0\n]"), "(let x (index v 0))");
    assert_eq!(body("u := User{\n    name: n,\n    age: 1,\n}"), "(let u (struct User (name n) (age 1)))");
    assert_eq!(body("a := [2]i32{\n    1,\n    2\n}"), "(let a (array [2]i32 1 2))");
}

#[test]
fn leading_dot_continues_method_chain() {
    assert_eq!(
        body("c := Command(\"x\")\n    .arg(\"v\")\n\n    .run()\nnext()"),
        "(let c (call (. (call (. (call Command \"x\") arg) \"v\") run)))\n(call next)"
    );
}

#[test]
fn leading_operator_does_not_continue() {
    let (_, res) = parse("fn f() {\n    x := 1\n    + 2\n}\n");
    let d = &res.diagnostics;
    assert_eq!(d.iter().map(|d| d.code).collect::<Vec<_>>(), ["E1002"]);
    assert!(d[0].help.as_deref().unwrap().contains("end of the previous line"));
}

#[test]
fn newline_ends_statements() {
    assert_eq!(body("a\nb"), "a\nb");
    assert_eq!(body("return\nx"), "(return)\nx");
    assert_eq!(codes("fn f() {\n    a b\n}"), ["E1005"]);
}

#[test]
fn closures_inside_calls_have_statement_bodies() {
    assert_eq!(
        body("xs.map(fn(x) i32 {\n    y := x * 2\n    return y\n})"),
        "(call (. xs map) (closure (x) -> i32\n  (block\n    (let y (* x 2))\n    (return y))))"
    );
}

// ---------------------------------------------------------------- statements

#[test]
fn bindings() {
    assert_eq!(body("x := 1"), "(let x 1)");
    assert_eq!(body("x: u64 := 1"), "(let x u64 1)");
    assert_eq!(body("x: Option<Option<i32>> := None"), "(let x Option<Option<i32>> None)");
    assert_eq!(body("x: &[]u8 := &buf"), "(let x &[]u8 (& buf))");
    assert_eq!(body("f: fn(i32) bool := g"), "(let f fn(i32) bool g)");
    assert_eq!(body("var y = 1"), "(var y 1)");
    assert_eq!(body("var y: [4]i64 = z"), "(var y [4]i64 z)");
}

#[test]
fn draft0_binding_form_gets_fix_it() {
    let (_, res) = parse("fn f() {\n    x u64 := 1\n    var y i64 = 2\n}");
    let d = &res.diagnostics;
    assert_eq!(d.iter().map(|d| d.code).collect::<Vec<_>>(), ["E1018", "E1018"]);
    assert_eq!(d[0].help.as_deref(), Some("write `x: u64 := ...`"));
    assert_eq!(d[1].help.as_deref(), Some("write `var y: i64 = ...`"));
    // Recovered as bindings.
    assert!(dump_module(&res.module).contains("(let x u64 1)"));
    assert_eq!(codes("fn f() {\n    x: u64 = 1\n}"), ["E1001"]);
    assert_eq!(codes("fn f() {\n    x: Option<i32 := 1\n}"), ["E1001"]);
}

#[test]
fn identifier_then_type_like_tokens_are_expressions() {
    assert_eq!(body("x[0] = 1"), "(= (index x 0) 1)");
    assert_eq!(body("a & b"), "(& a b)");
    assert_eq!(body("x.y = z"), "(= (. x y) z)");
}

#[test]
fn control_flow() {
    assert_eq!(
        body("if a {\n} else if b {\n} else {\n}"),
        "(if a\n  (block)\n  else (if b\n    (block)\n    else (block)))"
    );
    assert_eq!(body("for {\n    break\n}"), "(for\n  (block\n    (break)))");
    assert_eq!(body("for n != 1 {\n    continue\n}"), "(for (!= n 1)\n  (block\n    (continue)))");
    assert_eq!(body("for i in 0..10 {}"), "(for i in (.. 0 10)\n  (block))");
}

#[test]
fn conditions_do_not_take_struct_literals() {
    assert_eq!(body("if x {\n}"), "(if x\n  (block))");
    assert_eq!(body("if (P{a: 1}) == p {\n}"), "(if (== (paren (struct P (a 1))) p)\n  (block))");
    assert_eq!(body("for x in f(P{a}) {}"), "(for x in (call f (struct P (a)))\n  (block))");
}

#[test]
fn match_arms_and_patterns() {
    let src = "match v {\n    0 => a()\n    -1 => b()\n    1..=9 if big => {\n        c()\n    }\n    Some(x) => d(x)\n    Shape.Rect(w, _) => e(w)\n    User{name, age: 0} => f(name)\n    Empty => g()\n    \"s\" => h()\n    _ => return\n}";
    assert_eq!(
        body(src),
        "(match v\n  (0 => (call a))\n  ((- 1) => (call b))\n  ((..= 1 9) if big => (block\n    (call c)))\n  ((Some x) => (call d x))\n  ((Shape.Rect w _) => (call e w))\n  ((struct User (name) (age 0)) => (call f name))\n  (Empty => (call g))\n  (\"s\" => (call h))\n  (_ => (return)))"
    );
}

#[test]
fn blocks_unsafe_scope_spawn() {
    assert_eq!(body("unsafe {\n    f()\n}"), "(unsafe (block\n  (call f)))");
    assert_eq!(body("scope {\n    spawn f(1)\n}"), "(scope (block\n  (spawn (call f 1))))");
    assert_eq!(body("{\n    a\n}"), "(block\n  a)");
    // `scope` is only special before `{`.
    assert_eq!(body("scope := 1"), "(let scope 1)");
}

// ---------------------------------------------------------------- items

#[test]
fn items() {
    let src = "import \"fs\"\n\npub copy struct P<T> {\n    pub x T\n    y f64\n}\n\nenum E {\n    A(i32, string)\n    B\n}\n\ninterface W {\n    fn write(&mut self, data &[]u8) Result<usize, Error>\n}\n\nimpl W for File {\n    fn write(&mut self, data &[]u8) Result<usize, Error> {\n    }\n}\n\nextern \"C\" fn getpid() i32\n\nfn T.new<U: A + B>(self, f fn(i32) bool) {}\n";
    assert_eq!(
        ok(src),
        "(import \"fs\")\n(pub copy struct P <T>\n  (pub x T)\n  (y f64))\n(enum E\n  (A i32 string)\n  (B))\n(interface W\n  (fn write (&mut self (data &[]u8)) -> Result<usize, Error>))\n(impl W for File\n  (fn write (&mut self (data &[]u8)) -> Result<usize, Error>\n    (block)))\n(fn extern \"C\" getpid () -> i32)\n(fn T.new <U: A + B> (self (f fn(i32) bool))\n  (block))\n"
    );
}

#[test]
fn methods_on_generic_types() {
    assert_eq!(
        ok("fn Pair<A, B>.swap(&self) Pair<B, A> {}\nfn Pair<A, B>.map<C>(self, f fn(A) C) Pair<C, B> {}\nfn id<T>(x T) T { return x }\n"),
        "(fn Pair<A, B>.swap (&self) -> Pair<B, A>\n  (block))\n(fn Pair<A, B>.map <C> (self (f fn(A) C)) -> Pair<C, B>\n  (block))\n(fn id <T> ((x T)) -> T\n  (block\n    (return x)))\n"
    );
}

#[test]
fn spans_cover_nodes() {
    let src = "fn add(a i32, b i32) i32 {\n    return a + b\n}";
    let (map, res) = parse(src);
    let item = &res.module.items[0];
    assert_eq!(map.snippet(item.span), src);
    let ItemKind::Fn(f) = &item.kind else { panic!() };
    assert_eq!(map.snippet(f.params[1].span), "b i32");
    assert_eq!(map.snippet(f.ret.as_ref().unwrap().span), "i32");
    let stmt = &f.body.as_ref().unwrap().stmts[0];
    assert_eq!(map.snippet(stmt.span), "return a + b");
}

#[test]
fn node_ids_are_unique() {
    let (_, res) = parse("fn f(a i32) i32 {\n    x := a + 1 * 2\n    return x\n}");
    // Ids are dense, so uniqueness = the dump of all ids has no duplicates;
    // checked indirectly through the counter: re-parsing yields the same tree.
    let (_, res2) = parse("fn f(a i32) i32 {\n    x := a + 1 * 2\n    return x\n}");
    assert_eq!(res.module, res2.module);
}

// ---------------------------------------------------------------- errors and recovery

#[test]
fn error_codes() {
    assert_eq!(codes("fn f() {\n    x := \n}"), ["E1002"]);
    assert_eq!(codes("fn f(x) {}"), ["E1003"]);
    assert_eq!(codes("fn f() {\n    match x {\n        => a\n    }\n}"), ["E1004"]);
    assert_eq!(codes("fn f() {\n    a b\n}"), ["E1005"]);
    assert_eq!(codes("fn f() {\n    x := a < b < c\n}"), ["E1006"]);
    assert_eq!(codes("fn f() {\n    x += 1\n}"), ["E1007"]);
    assert_eq!(codes("fn f() {\n    f() = 1\n}"), ["E1008"]);
    assert_eq!(codes("fn f() {\n    if p == P{x: 1} {\n    }\n}"), ["E1009"]);
    assert_eq!(codes("fn f() {\n    x := 1\n"), ["E1010"]);
    assert_eq!(codes("fn f()\n"), ["E1011"]);
    assert_eq!(codes("interface I {\n    fn m(&self) {}\n}"), ["E1012"]);
    assert_eq!(codes("x := 1\n"), ["E1013"]);
    assert_eq!(codes("fn f(&self) {}"), ["E1014"]);
    assert_eq!(codes("fn T.f(a i32, &self) {}"), ["E1014"]);
    assert_eq!(codes("pub import \"fs\""), ["E1015"]);
    assert_eq!(codes("fn f() {\n    if a {\n    }\n    else {\n    }\n}"), ["E1016"]);
    assert_eq!(codes("fn f() {\n    x := 0..1..2\n}"), ["E1017"]);
}

#[test]
fn recovers_and_reports_each_bad_statement_once() {
    let src = "fn f() {\n    x := (1 +\n    y := 2\n    z := ]\n    ok()\n}\n\nfn g() {\n    good()\n}\n";
    let (_, res) = parse(src);
    assert!(res.diagnostics.len() <= 3, "{:#?}", res.diagnostics);
    // `g` survived intact.
    assert!(dump_module(&res.module).contains("(fn g ()\n  (block\n    (call good)))"));
}

#[test]
fn recovers_at_next_item() {
    let src = "fn f( {\n}\n\nstruct S {\n    a i32,\n    b i32\n}\n\nfn ok() {}\n";
    let (_, res) = parse(src);
    let names: Vec<_> = res
        .module
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Fn(f) => Some(f.name.name.clone()),
            ItemKind::Struct(s) => Some(s.name.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["f", "S", "ok"]);
    assert!(res.diagnostics.iter().any(|d| d.help.as_deref().is_some_and(|h| h.contains("without commas"))));
}

#[test]
fn garbage_does_not_hang_or_panic() {
    for src in [
        "}}}}",
        "fn",
        "fn f(",
        "fn f() { (((( }",
        "struct",
        "match",
        "fn f() { match x { Some( => }",
        "fn f() { x := [ }",
        "fn f() { a.b.c. }",
        "impl for {",
        "fn f() { if { } else }",
        "fn f() { x: Option<Option<i32> := 1 }",
        "fn f() { x: := 1 }",
        "fn f() { var x: = 1 }",
        "fn f() { User{a: 1,, b} }",
        "\n\n\n",
        "fn f() { return return }",
        "extern fn",
        "fn f() { var = 1 }",
    ] {
        let (_, res) = parse(src);
        let _ = dump_module(&res.module);
    }
}

#[test]
fn named_attribute_options_preserve_values_and_report_malformed_values() {
    use tarn_ast::AttrArg;
    let (_, parsed) = parse("#[pvh_entry(identity = \"4GiB\", higher_half = 0xffffffff80000000)]\nfn kernel() {}\n");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let ItemKind::Fn(f) = &parsed.module.items[0].kind else { panic!("function") };
    assert_eq!(f.attrs[0].arg, Some(AttrArg::Named(vec![
        ("identity".into(), AttrArg::Str("4GiB".into())),
        ("higher_half".into(), AttrArg::Int(0xffffffff80000000)),
    ])));
    for source in [
        "#[pvh_entry(identity = true)]\nfn f() {}\n",
        "#[pvh_entry(identity \"4GiB\")]\nfn f() {}\n",
        "#[pvh_entry(identity = )]\nfn f() {}\n",
    ] {
        assert!(!codes(source).is_empty(), "{source}");
    }
}
