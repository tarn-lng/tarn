//! Soundness mutation harness (docs/soundness-audit.md).
//!
//! Deterministic and exhaustive, no randomness: every program is the product
//! of a few small tables, so a failure names the exact combination.
//!
//! - Linear family: `acquisition x control flow x disposition`. A resource of
//!   a `#[must_consume]` type is obtained in one of several ways, placed in a
//!   control-flow context and then disposed of. Every valid disposition must
//!   be accepted (no over-rejection); every mutated disposition (deleted
//!   consume, duplicated move, early exit, borrow instead of move, wrapping in
//!   Option/Vec/struct/closure/generic/slot, overwrite) must be rejected with
//!   an ownership diagnostic.
//! - Borrow family: `setup x invalidation`. The accepted order is
//!   `borrow; use; invalidate`; the mutant swaps the last two.
//! - Corpus family: every accepted `tests/moves/pass` program that consumes a
//!   resource with a `consume(x)` statement is mutated by deleting or
//!   duplicating that statement.
//!
//! Run: `cargo test -p tarn_driver --test soundness_mutations`.
//! `TARN_SOUNDNESS_DUMP=dir` also writes every generated program to `dir`.

use std::path::{Path, PathBuf};

/// Diagnostics that state an ownership or borrowing violation. A mutant must
/// be rejected for one of these, not for an unrelated syntax or type error.
const OWNERSHIP: &[&str] = &["E4001", "E4002", "E4003", "E4004", "E4005", "E4006", "E4007", "E4008"];
const BORROWS: &[&str] = &["E4001", "E4101", "E4102", "E4103", "E4104", "E4105", "E4201", "E4205", "E4207"];

struct Case {
    name: String,
    source: String,
    /// `None`: must be accepted. `Some(codes)`: must report one of them.
    reject: Option<&'static [&'static str]>,
}

const LINEAR_PRELUDE: &str = r#"#[must_consume]
struct Frame { addr u64 }
struct Holder<T> { v T }
struct Pair {
    a Frame
    b u64
}
fn make() Frame { return Frame{addr: 1} }
fn consume(frame Frame) { match frame { Frame{addr} => print(addr) } }
fn peek(frame &Frame) u64 { return frame.addr }
fn forward<T>(v T) T { return v }
fn forget<T>(v T) {}
fn fill<T>(slot &mut Option<T>, v T) { slot.put(v) }
fn source() Option<Frame> { return Some(make()) }
fn source_result() Result<Frame, u64> { return Ok(make()) }
fn fail() Result<u64, u64> { return Err(1) }
"#;

/// How the resource `f` comes to exist. `post` closes the construct and
/// discharges whatever else the acquisition created.
struct Acquire {
    name: &'static str,
    param: bool,
    pre: &'static str,
    post: &'static str,
    /// `post` holds obligations, so the body may not leave early.
    post_obligation: bool,
}

const ACQUIRE: &[Acquire] = &[
    Acquire { name: "local", param: false, pre: "f := make()", post: "", post_obligation: false },
    Acquire { name: "param", param: true, pre: "", post: "", post_obligation: false },
    Acquire { name: "option", param: false, pre: "match source() {\nSome(f) => {", post: "}\nNone => {}\n}", post_obligation: false },
    Acquire { name: "field", param: false, pre: "p := Pair{a: make(), b: 1}\nf := p.a", post: "", post_obligation: false },
    Acquire { name: "generic", param: false, pre: "f := forward(make())", post: "", post_obligation: false },
    Acquire { name: "try", param: false, pre: "f := try source_result()", post: "", post_obligation: false },
    Acquire {
        name: "vec",
        param: false,
        pre: "var v: Vec<Frame> = Vec.new()\nv.push(make())\nmatch v.pop() {\nSome(f) => {",
        post: "}\nNone => {}\n}\nv.finish()",
        post_obligation: true,
    },
    Acquire {
        name: "slot",
        param: false,
        pre: "var s: Option<Frame> = None\ns.put(make())\nmatch s.take() {\nSome(f) => {",
        post: "}\nNone => {}\n}\nfinish_empty(s)",
        post_obligation: true,
    },
    Acquire { name: "exchange", param: false, pre: "var cur: Frame = make()\nf := replace_owned(&mut cur, make())", post: "consume(cur)", post_obligation: true },
];

/// Control flow around the disposition `d`.
struct Flow {
    name: &'static str,
    /// Leaves the function inside the body.
    early_exit: bool,
    build: fn(&str) -> String,
}

const FLOW: &[Flow] = &[
    Flow { name: "straight", early_exit: false, build: |d| d.to_string() },
    Flow { name: "if_else", early_exit: false, build: |d| format!("if c {{\n{d}\n}} else {{\n{d}\n}}") },
    Flow { name: "match_bool", early_exit: false, build: |d| format!("match c {{\ntrue => {{\n{d}\n}}\nfalse => {{\n{d}\n}}\n}}") },
    Flow { name: "match_int", early_exit: false, build: |d| format!("match n {{\n0 => {{\n{d}\n}}\n_ => {{\n{d}\n}}\n}}") },
    Flow { name: "return_branch", early_exit: true, build: |d| format!("if c {{\n{d}\nreturn Ok(0)\n}}\n{d}") },
    Flow { name: "after_loop", early_exit: false, build: |d| format!("var i: u64 = 0\nfor i < n {{ i = i + 1 }}\n{d}") },
    Flow { name: "after_borrow", early_exit: false, build: |d| format!("print(peek(&f))\n{d}") },
    Flow { name: "after_closure", early_exit: false, build: |d| format!("g := fn() u64 {{ return peek(&f) }}\nprint(g())\n{d}") },
];

/// Dispositions that discharge the obligation of `f` exactly once.
const VALID: &[(&str, &str)] = &[
    ("consume", "consume(f)"),
    ("forward", "consume(forward(f))"),
    ("dismantle", "match f { Frame{addr} => print(addr) }"),
    ("struct", "h := Holder{v: f}\nconsume(h.v)"),
    ("option", "o := Some(f)\nmatch o {\nSome(x) => consume(x)\nNone => {}\n}"),
    ("vec", "var q: Vec<Frame> = Vec.new()\nq.push(f)\nmatch q.pop() {\nSome(x) => consume(x)\nNone => {}\n}\nq.finish()"),
    ("slot", "var t: Option<Frame> = None\nt.put(f)\nmatch t.take() {\nSome(x) => consume(x)\nNone => {}\n}\nfinish_empty(t)"),
    ("slot_match", "var t: Option<Frame> = None\nfill(&mut t, f)\nmatch t {\nSome(x) => consume(x)\nNone => {}\n}"),
    ("exchange", "var t: Frame = make()\nold := replace_owned(&mut t, f)\nconsume(old)\nconsume(t)"),
];

/// Mutated dispositions: each loses, duplicates or erases `f`.
const INVALID: &[(&str, &str)] = &[
    ("delete_consume", "print(1)"),
    ("duplicate_consume", "consume(f)\nconsume(f)"),
    ("borrow_only", "print(peek(&f))"),
    ("rebind", "x := f"),
    ("wildcard", "match f { _ => {} }"),
    ("generic_forget", "forget(f)"),
    ("one_branch", "if c { consume(f) }"),
    ("in_loop", "var j: u64 = 0\nfor j < n {\nconsume(f)\nj = j + 1\n}"),
    ("early_return", "if c { return Ok(0) }\nconsume(f)"),
    ("try_before", "y := try fail()\nconsume(f)"),
    ("option_dropped", "o := Some(f)"),
    ("option_observed", "o := Some(f)\nprint(o.is_some())"),
    ("option_wildcard", "match Some(f) {\nSome(_) => {}\nNone => {}\n}"),
    ("option_overwritten", "var o: Option<Frame> = Some(f)\no = None"),
    ("result_dropped", "var e: Result<u64, Frame> = Err(f)"),
    ("result_ok_dropped", "var e: Result<Frame, u64> = Ok(f)\no := e.ok()"),
    ("unwrap_or", "x := source().unwrap_or(f)\nconsume(x)"),
    ("vec_dropped", "var q: Vec<Frame> = Vec.new()\nq.push(f)"),
    ("vec_cleared", "var q: Vec<Frame> = Vec.new()\nq.push(f)\nq.clear()\nq.finish()"),
    ("array_dropped", "a := [1]Frame{f}"),
    ("struct_dropped", "h := Holder{v: f}"),
    ("pair_field_read", "p2 := Pair{a: f, b: 2}\nprint(p2.b)"),
    ("overwrite", "var t: Frame = f\nt = make()\nconsume(t)"),
    ("owned_closure", "g2 := move fn() u64 { return peek(&f) }"),
    // Owning slots: the slot is known empty, then filled through a borrow.
    ("slot_put", "var t: Option<Frame> = None\nt.put(f)"),
    ("slot_replace", "var t: Option<Frame> = None\nold := t.replace(f)\nfinish_empty(old)"),
    ("slot_exchange", "var t: Option<Frame> = None\nold := replace_owned(&mut t, Some(f))\nfinish_empty(old)"),
    ("slot_callee", "var t: Option<Frame> = None\nfill(&mut t, f)"),
    ("slot_field", "var t: Holder<Option<Frame>> = Holder{v: None}\nt.v.put(f)"),
    ("slot_nested", "var t: Option<Option<Frame>> = Some(None)\nmatch &mut t {\nSome(inner) => inner.put(f)\nNone => consume(f)\n}"),
    ("slot_refined", "var t: Option<Frame> = source()\nmatch t {\nSome(x) => {\nconsume(x)\nconsume(f)\n}\nNone => fill(&mut t, f)\n}"),
    ("slot_closure", "var t: Option<Frame> = None\nput := fn() { t.put(make()) }\nput()\nconsume(f)"),
    ("slot_result", "var t: Result<Frame, u64> = Err(0)\nold := replace_owned(&mut t, Ok(f))\nmatch old {\nOk(x) => consume(x)\nErr(code) => print(code)\n}"),
    ("slot_taken_not_finished", "var t: Option<Frame> = None\nt.put(f)\nmatch t.take() {\nSome(x) => consume(x)\nNone => {}\n}"),
];

fn linear_program(a: &Acquire, body: &str, looped: Option<&str>) -> String {
    let params = if a.param { "f Frame, c bool, n u64" } else { "c bool, n u64" };
    let call = if a.param { "run(make(), true, 2)" } else { "run(true, 2)" };
    let inner = format!("{}\n{body}\n{}", a.pre, a.post);
    let inner = match looped {
        Some(exit) => format!("var k: u64 = 0\nfor k < n {{\nk = k + 1\n{inner}\n{exit}\n}}"),
        None => inner,
    };
    format!("{LINEAR_PRELUDE}fn run({params}) Result<u64, u64> {{\n{inner}\nreturn Ok(0)\n}}\nfn main() {{ r := {call} }}\n")
}

/// Dispositions and acquisitions kept when a case is repeated under every
/// control-flow context (the full cross product costs minutes).
const FLOW_VALID: &[&str] = &["consume", "option", "slot_match"];
const FLOW_INVALID: &[&str] = &[
    "delete_consume", "duplicate_consume", "rebind", "one_branch", "early_return", "option_dropped", "vec_dropped",
    "overwrite", "slot_put", "slot_exchange", "slot_callee", "slot_refined", "slot_closure",
];
const FLOW_ACQUIRE: &[&str] = &["local", "param", "option", "vec", "slot", "exchange"];
const LOOP_ACQUIRE: &[&str] = &["local", "option", "slot"];

fn linear_cases() -> Vec<Case> {
    let mut out = Vec::new();
    // A suspendable computation cannot own a resource (cancellation would drop it).
    for (name, tail) in [("async_input", "async fn eat(frame Frame) { consume(frame) }"), ("async_output", "async fn mk() Frame { return make() }")] {
        out.push(Case { name: format!("linear/!{name}"), source: format!("{LINEAR_PRELUDE}{tail}\nfn main() {{}}\n"), reject: Some(OWNERSHIP) });
    }
    for a in ACQUIRE {
        for flow in FLOW {
            let full = flow.name == "straight";
            if !full && !FLOW_ACQUIRE.contains(&a.name) {
                continue;
            }
            if flow.early_exit && a.post_obligation {
                continue;
            }
            for (name, d) in VALID {
                if full || FLOW_VALID.contains(name) {
                    out.push(Case { name: format!("linear/{}/{}/{name}", a.name, flow.name), source: linear_program(a, &(flow.build)(d), None), reject: None });
                }
            }
            for (name, d) in INVALID {
                if full || FLOW_INVALID.contains(name) {
                    out.push(Case { name: format!("linear/{}/{}/!{name}", a.name, flow.name), source: linear_program(a, &(flow.build)(d), None), reject: Some(OWNERSHIP) });
                }
            }
        }
        if a.param || !LOOP_ACQUIRE.contains(&a.name) {
            continue;
        }
        // The whole acquisition and disposition inside a loop body.
        for (exit_name, exit) in [("loop", ""), ("loop_break", "if c { break }"), ("loop_continue", "if c { continue }")] {
            for (name, d) in VALID {
                if FLOW_VALID.contains(name) {
                    out.push(Case { name: format!("linear/{}/{exit_name}/{name}", a.name), source: linear_program(a, d, Some(exit)), reject: None });
                }
            }
            for (name, d) in INVALID {
                if FLOW_INVALID.contains(name) {
                    out.push(Case { name: format!("linear/{}/{exit_name}/!{name}", a.name), source: linear_program(a, d, Some(exit)), reject: Some(OWNERSHIP) });
                }
            }
        }
        // Leaving the loop iteration while the resource is still held.
        for (name, d) in [("break_before", "if c { break }\nconsume(f)"), ("continue_before", "if c { continue }\nconsume(f)")] {
            out.push(Case { name: format!("linear/{}/loop/!{name}", a.name), source: linear_program(a, d, Some("")), reject: Some(OWNERSHIP) });
        }
    }
    out
}

const BORROW_PRELUDE: &str = r#"struct Holder<T> { v T }
fn eat(s string) {}
fn eat_vec(v Vec<string>) {}
fn size(s &string) usize { return s.len() }
fn first(v &Vec<string>) &string { return v.get(0) }
fn pick(a &string, b &string) &string {
    if a.len() > 0 { return a }
    return b
}
fn fresh() string { return "new".clone() }
"#;

/// A borrow `r`, one statement using it, and operations that end the
/// borrowed value. `borrow; use; end` is valid, `borrow; end; use` is not.
struct Borrow {
    name: &'static str,
    setup: &'static str,
    usage: &'static str,
    close: &'static str,
    ends: &'static [(&'static str, &'static str)],
}

const BORROW: &[Borrow] = &[
    Borrow {
        name: "shared_local",
        setup: "var x: string = fresh()\nr := &x",
        usage: "print(size(r))",
        close: "",
        ends: &[("move", "eat(x)"), ("assign", "x = fresh()"), ("mutable", "m := &mut x\n*m = fresh()"), ("exchange", "old := replace_owned(&mut x, fresh())")],
    },
    Borrow {
        name: "mutable_local",
        setup: "var x: string = fresh()\nr := &mut x",
        usage: "*r = fresh()",
        close: "",
        ends: &[("move", "eat(x)"), ("assign", "x = fresh()"), ("read", "print(size(&x))"), ("mutable", "m := &mut x\n*m = fresh()")],
    },
    Borrow {
        name: "vec_element",
        setup: "var v: Vec<string> = Vec.new()\nv.push(fresh())\nr := v.get(0)",
        usage: "print(size(r))",
        close: "",
        ends: &[("push", "v.push(fresh())"), ("pop", "p := v.pop()"), ("move", "eat_vec(v)"), ("clear", "v.clear()"), ("remove", "e := v.swap_remove(0)"), ("assign", "v = Vec.new()")],
    },
    Borrow {
        name: "vec_through_call",
        setup: "var v: Vec<string> = Vec.new()\nv.push(fresh())\nr := first(&v)",
        usage: "print(size(r))",
        close: "",
        ends: &[("push", "v.push(fresh())"), ("pop", "p := v.pop()"), ("move", "eat_vec(v)"), ("replace", "e := v.replace(0, fresh())")],
    },
    Borrow {
        name: "vec_slice",
        setup: "var v: Vec<string> = Vec.new()\nv.push(fresh())\nr := v.as_slice()",
        usage: "print(size(&r[0]))",
        close: "",
        ends: &[("push", "v.push(fresh())"), ("pop", "p := v.pop()"), ("move", "eat_vec(v)")],
    },
    Borrow {
        name: "field",
        setup: "var h: Holder<string> = Holder{v: fresh()}\nr := &h.v",
        usage: "print(size(r))",
        close: "",
        ends: &[("move_field", "eat(h.v)"), ("assign_field", "h.v = fresh()"), ("move_owner", "h2 := h"), ("assign_owner", "h = Holder{v: fresh()}")],
    },
    Borrow {
        name: "option_payload",
        setup: "var o: Option<string> = Some(fresh())\nmatch &o {\nSome(r) => {",
        usage: "print(size(r))",
        close: "}\nNone => {}\n}",
        ends: &[("assign", "o = None"), ("take", "t := o.take()"), ("replace", "t := o.replace(fresh())")],
    },
    Borrow {
        name: "array_element",
        setup: "var a: [2]string = [2]string{fresh(), fresh()}\nr := &a[0]",
        usage: "print(size(r))",
        close: "",
        ends: &[("assign", "a[0] = fresh()"), ("exchange", "old := replace_owned(&mut a[0], fresh())"), ("move", "b := a")],
    },
    Borrow {
        name: "either_argument",
        setup: "var x: string = fresh()\nvar y: string = fresh()\nr := pick(&x, &y)",
        usage: "print(size(r))",
        close: "",
        ends: &[("move_first", "eat(x)"), ("move_second", "eat(y)"), ("assign_second", "y = fresh()")],
    },
    Borrow {
        name: "closure_capture",
        setup: "var x: string = fresh()\nr := fn() usize { return size(&x) }",
        usage: "print(r())",
        close: "",
        ends: &[("move", "eat(x)"), ("assign", "x = fresh()")],
    },
    Borrow {
        name: "stored_reference",
        setup: "var x: string = fresh()\nvar refs: Vec<&string> = Vec.new()\nrefs.push(&x)\nr := *refs.get(0)",
        usage: "print(size(r))",
        close: "",
        ends: &[("move", "eat(x)"), ("assign", "x = fresh()")],
    },
];

/// Escapes: `(name, accepted body, rejected body)` of `fn main()` or items.
const ESCAPES: &[(&str, &str, &str)] = &[
    (
        "return_local",
        "fn get(s &string) &string { return s }\nfn main() { x := fresh()\nprint(size(get(&x))) }",
        "fn get(s string) &string { return &s }\nfn main() { print(size(get(fresh()))) }",
    ),
    (
        "inner_scope",
        "fn main() {\nx := fresh()\nvar r: &string = &x\nif true {\ny := fresh()\nprint(size(&y))\n}\nprint(size(r))\n}",
        "fn main() {\nx := fresh()\nvar r: &string = &x\nif true {\ny := fresh()\nr = &y\n}\nprint(size(r))\n}",
    ),
    (
        "loop_scope",
        "fn main() {\nx := fresh()\nvar r: &string = &x\nvar i: u64 = 0\nfor i < 2 {\ny := fresh()\nprint(size(&y))\ni = i + 1\n}\nprint(size(r))\n}",
        "fn main() {\nx := fresh()\nvar r: &string = &x\nvar i: u64 = 0\nfor i < 2 {\ny := fresh()\nr = &y\ni = i + 1\n}\nprint(size(r))\n}",
    ),
    (
        "vec_of_references",
        "fn main() {\nx := fresh()\nvar v: Vec<&string> = Vec.new()\nif true {\nv.push(&x)\n}\nprint(size(*v.get(0)))\n}",
        "fn main() {\nvar v: Vec<&string> = Vec.new()\nif true {\ny := fresh()\nv.push(&y)\n}\nprint(size(*v.get(0)))\n}",
    ),
    (
        "option_of_reference",
        "fn main() {\nx := fresh()\nvar o: Option<&string> = None\nif true {\no = Some(&x)\n}\nmatch o {\nSome(r) => print(size(r))\nNone => {}\n}\n}",
        "fn main() {\nvar o: Option<&string> = None\nif true {\ny := fresh()\no = Some(&y)\n}\nmatch o {\nSome(r) => print(size(r))\nNone => {}\n}\n}",
    ),
    (
        "store_through_parameter",
        "fn store(slot &mut &string, s &string) { *slot = s }\nfn main() {\nx := fresh()\ny := fresh()\nvar r: &string = &x\nstore(&mut r, &y)\nprint(size(r))\n}",
        "fn store(slot &mut &string, s &string) { *slot = s }\nfn main() {\nx := fresh()\nvar r: &string = &x\nif true {\ny := fresh()\nstore(&mut r, &y)\n}\nprint(size(r))\n}",
    ),
    (
        "temporary_owner",
        "fn main() {\nx := fresh()\nr := x.view()\nprint(size(r))\n}",
        "fn main() {\nr := fresh().view()\ny := fresh()\nprint(size(r))\n}",
    ),
    (
        "wrapped_return",
        "fn wrap(s &string) Option<&string> { return Some(s) }\nfn main() {\nx := fresh()\nmatch wrap(&x) {\nSome(r) => print(size(r))\nNone => {}\n}\n}",
        "fn wrap(s &string) Option<&string> { return Some(s) }\nfn bad() Option<&string> {\ny := fresh()\nreturn wrap(&y)\n}\nfn main() {\nmatch bad() {\nSome(r) => print(size(r))\nNone => {}\n}\n}",
    ),
    (
        "aliased_mutable",
        "fn main() {\nvar x: string = fresh()\na := &mut x\n*a = fresh()\nb := &mut x\n*b = fresh()\n}",
        "fn main() {\nvar x: string = fresh()\na := &mut x\nb := &mut x\n*a = fresh()\n*b = fresh()\n}",
    ),
    (
        "aliased_arguments",
        "fn both(a &mut string, b &string) { *a = fresh()\nprint(size(b)) }\nfn main() {\nvar x: string = fresh()\ny := fresh()\nboth(&mut x, &y)\n}",
        "fn both(a &mut string, b &string) { *a = fresh()\nprint(size(b)) }\nfn main() {\nvar x: string = fresh()\nboth(&mut x, &x)\n}",
    ),
];

fn borrow_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for b in BORROW {
        for (name, end) in b.ends {
            let program = |first: &str, second: &str| format!("{BORROW_PRELUDE}fn main() {{\n{}\n{first}\n{second}\n{}\n}}\n", b.setup, b.close);
            out.push(Case { name: format!("borrow/{}/{name}", b.name), source: program(b.usage, end), reject: None });
            out.push(Case { name: format!("borrow/{}/!{name}", b.name), source: program(end, b.usage), reject: Some(BORROWS) });
        }
    }
    for (name, valid, invalid) in ESCAPES {
        out.push(Case { name: format!("escape/{name}"), source: format!("{BORROW_PRELUDE}{valid}\n"), reject: None });
        out.push(Case { name: format!("escape/!{name}"), source: format!("{BORROW_PRELUDE}{invalid}\n"), reject: Some(BORROWS) });
    }
    out
}

/// Accepted fixtures whose resource is consumed by a `consume(x)` statement:
/// deleting the statement loses the resource, duplicating it uses it twice.
fn corpus_cases() -> Vec<Case> {
    let dir = Path::new("../../tests/moves/pass");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "tarn")).collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        if !source.contains("#[must_consume]") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let lines: Vec<&str> = source.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let statement = line.trim();
            let Some(argument) = statement.strip_prefix("consume(").and_then(|rest| rest.strip_suffix(')')) else { continue };
            if argument.is_empty() || !argument.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let deleted = [&lines[..i], &lines[i + 1..]].concat().join("\n");
            let duplicated = [&lines[..=i], &lines[i..]].concat().join("\n");
            out.push(Case { name: format!("corpus/{stem}/!delete_line_{}", i + 1), source: deleted, reject: Some(OWNERSHIP) });
            out.push(Case { name: format!("corpus/{stem}/!duplicate_line_{}", i + 1), source: duplicated, reject: Some(OWNERSHIP) });
        }
    }
    out
}

fn run(cases: Vec<Case>, minimum: usize) {
    assert!(cases.len() >= minimum, "only {} cases", cases.len());
    let dir = std::env::temp_dir().join(format!("tarn-soundness-{}-{}", std::process::id(), cases[0].name.split('/').next().unwrap()));
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(dump) = std::env::var_os("TARN_SOUNDNESS_DUMP") {
        let dump = PathBuf::from(dump);
        std::fs::create_dir_all(&dump).unwrap();
        for case in &cases {
            std::fs::write(dump.join(format!("{}.tarn", case.name.replace('/', "__").replace('!', "not_"))), &case.source).unwrap();
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    let workers = 2;
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let (dir, cases, next, failures) = (&dir, &cases, &next, &failures);
            scope.spawn(move || {
                let file = dir.join(format!("case{worker}.tarn"));
                while let Some(case) = cases.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    std::fs::write(&file, &case.source).unwrap();
                    let result = tarn_driver::check(&file).unwrap();
                    let codes: Vec<&str> = result.diagnostics.iter().filter(|d| d.code.starts_with('E')).map(|d| d.code).collect();
                    let problem = match case.reject {
                        None if result.has_errors() => Some(format!("valid program rejected with {codes:?}")),
                        Some(expected) if !codes.iter().any(|c| expected.contains(c)) => {
                            Some(if codes.is_empty() { "violation accepted".to_string() } else { format!("rejected only with unrelated {codes:?}") })
                        }
                        _ => None,
                    };
                    if let Some(problem) = problem {
                        failures.lock().unwrap().push(format!("{}: {problem}", case.name));
                        if std::env::var_os("TARN_SOUNDNESS_VERBOSE").is_some() {
                            eprintln!("---- {}\n{}", case.name, case.source);
                        }
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

#[test]
fn linear_dispositions() {
    run(linear_cases(), 600);
}

#[test]
fn borrow_orderings_and_escapes() {
    run(borrow_cases(), 90);
}

#[test]
fn corpus_consume_mutations() {
    run(corpus_cases(), 20);
}
