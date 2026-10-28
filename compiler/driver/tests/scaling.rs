//! Compile-time scaling regressions. Dataflow over one long function once
//! revisited every join per path and kept dense per-local states in every
//! block (ranges, moves, borrows): 200 branches took 28 s, 500 over ten
//! minutes, and 2000 needed over a gigabyte. Worklists now follow reverse
//! postorder and states are sparse.
//!
//! Later sweeps found more single-function blowups, each guarded below with
//! the timing it had before its fix (release build, n = 1600).

use std::time::{Duration, Instant};

/// Check `src` and fail when it is rejected or slower than the bound, which
/// is generous for unoptimized test builds; every regression took far longer.
fn checks_within(name: &str, src: &str, bound: Duration) {
    let dir = std::env::temp_dir().join(format!("tarn-scaling-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let start = Instant::now();
    let res = tarn_driver::check(&file).unwrap();
    let elapsed = start.elapsed();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!res.has_errors(), "{name}: {}", res.diagnostics.iter().map(|d| d.render(&res.program.sources)).collect::<String>());
    assert!(elapsed < bound, "{name}: checking took {elapsed:?}");
}

fn main_with(body: impl Fn(usize) -> String, n: usize) -> String {
    let mut src = String::from("fn main() {\n    var s: i64 = 0\n");
    for i in 0..n {
        src.push_str(&body(i));
    }
    src.push_str("    print(s)\n}\n");
    src
}

#[test]
fn long_branchy_function_checks_in_near_linear_time() {
    let src = main_with(|i| format!("    s = s + {i}\n    if s > 1000000 {{\n        s = 0\n    }}\n"), 2000);
    checks_within("branches", &src, Duration::from_secs(30));
}

/// Borrow states kept the loans of dead locals: every `&v` stayed held to the
/// end of the function (11.6 s).
#[test]
fn many_short_borrows_do_not_accumulate() {
    let src = main_with(|i| format!("    r{i} := &s\n    print(r{i})\n"), 1600);
    checks_within("borrows", &src, Duration::from_secs(30));
}

/// Range states kept every finished loop's index (6.5 s).
#[test]
fn sequential_loops_do_not_accumulate_ranges() {
    let src = main_with(|_| "    for i in 0..3 {\n        s = s + i\n    }\n".to_string(), 1600);
    checks_within("loops", &src, Duration::from_secs(30));
}

/// Borrow liveness needed one full round per nesting level (2.3 s at 1600).
/// Depth just under the parser's nesting limit must also fit the 2 MiB stack
/// of a test thread through every recursive phase.
#[test]
fn deeply_nested_conditionals_converge_in_one_pass() {
    let depth = 120;
    let mut src = String::from("fn main() {\n    var s: i64 = 0\n");
    for i in 0..depth {
        src.push_str(&format!("{}if s < {} {{\n{}s = s + 1\n", "    ".repeat(i + 1), 1000 + i, "    ".repeat(i + 2)));
    }
    for i in (0..depth).rev() {
        src.push_str(&format!("{}}}\n", "    ".repeat(i + 1)));
    }
    src.push_str("    print(s)\n}\n");
    checks_within("nested-if", &src, Duration::from_secs(30));
}

/// Unbounded nesting overflowed the stack and aborted the compiler (and any
/// editor using it) with no diagnostic. It is now one E1020 at the limit.
#[test]
fn excessive_nesting_is_a_diagnostic_not_a_crash() {
    let parens = format!("fn main() {{\n    x: i64 := {}1{}\n    print(x)\n}}\n", "(".repeat(20000), ")".repeat(20000));
    let mut blocks = String::from("fn main() {\n");
    for _ in 0..2000 {
        blocks.push_str("if true {\n");
    }
    for _ in 0..2000 {
        blocks.push_str("}\n");
    }
    blocks.push_str("}\n");
    for (name, src) in [("parens", parens), ("blocks", blocks)] {
        let dir = std::env::temp_dir().join(format!("tarn-nesting-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.tarn");
        std::fs::write(&file, src).unwrap();
        let res = tarn_driver::check(&file).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let codes: Vec<_> = res.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E1020"], "{name}");
    }
}

/// Move checking recorded every move path of the function at each drop
/// (1.8 s), and type checking copied the function's type tables into every
/// closure (3.8 s).
#[test]
fn many_values_and_closures_scale_linearly() {
    let mut src = String::new();
    for i in 0..1600 {
        src.push_str(&format!("struct S{i} {{\n    a i64\n    b string\n}}\n"));
    }
    src.push_str("fn main() {\n    var s: i64 = 0\n");
    for i in 0..1600 {
        src.push_str(&format!("    v{i} := S{i}{{a: {i}, b: \"x\"}}\n    c{i} := fn(y i64) i64 {{\n        return y + {i}\n    }}\n    s = c{i}(v{i}.a)\n"));
    }
    src.push_str("    print(s)\n}\n");
    checks_within("values", &src, Duration::from_secs(30));
}

/// A call on a method or callable-field result checked its base twice (once to
/// tell fields from methods, once in the call itself), doubling the work per
/// link: a 20-call chain took 14 s and 30 never finished.
#[test]
fn call_chains_check_in_linear_time() {
    // 4000 links: the semantic phases run on the compiler's own large stack.
    let chain: String = (0..4000).map(|i| format!(".add({i})")).collect();
    let src = format!(
        "struct B {{ total i32 }}\npub fn B.add(&self, x i32) B {{ return B{{total: self.total + x}} }}\nfn main() {{\n    b := B{{total: 0}}{chain}\n    print(b.total)\n}}\n"
    );
    checks_within("method-chain", &src, Duration::from_secs(30));
}

/// The same doubling through callable fields: only the time matters here,
/// since calling a field of a temporary is rejected by borrow checking.
#[test]
fn callable_field_chains_check_in_linear_time() {
    let chain: String = (0..100).map(|i| format!(".next({i})")).collect();
    let src = format!(
        "struct S {{ total i32\n    next fn(i32) S }}\nfn step(x i32) S {{ return S{{total: x, next: step}} }}\nfn main() {{\n    s := step(0){chain}\n    print(s.total)\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!("tarn-scaling-field-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let start = Instant::now();
    let res = tarn_driver::check(&file).unwrap();
    let elapsed = start.elapsed();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(res.diagnostics.iter().all(|d| d.code != "E3003"), "field calls must still type-check as calls");
    assert!(elapsed < Duration::from_secs(30), "field chain: checking took {elapsed:?}");
}

fn checked_source(name: &str, src: &str) -> (tarn_driver::CheckResult, Duration) {
    let dir = std::env::temp_dir().join(format!("tarn-scaling-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let start = Instant::now();
    let res = tarn_driver::check(&file).unwrap();
    let elapsed = start.elapsed();
    std::fs::remove_dir_all(&dir).unwrap();
    (res, elapsed)
}

/// Chains and operator sequences are parsed in loops, so MAX_NESTING never
/// saw them, and every later phase recursed through them: 1000 chained calls
/// overflowed the CLI stack. Past MAX_TREE_DEPTH they are now E1022, and the
/// rejected tree is destroyed without recursion (ADR 0081).
#[test]
fn over_deep_expressions_are_a_diagnostic_not_a_crash() {
    let chain: String = (0..100_000).map(|i| format!(".add({i})")).collect();
    let calls = format!("struct B {{ total i32 }}\npub fn B.add(&self, x i32) B {{ return B{{total: self.total + x}} }}\nfn main() {{\n    b := B{{total: 0}}{chain}\n    print(b.total)\n}}\n");
    let sums = format!("fn main() {{\n    s: i64 := 0{}\n    print(s)\n}}\n", " + 1".repeat(300_000));
    for (name, src) in [("deep-calls", calls), ("deep-sums", sums)] {
        let (res, elapsed) = checked_source(name, &src);
        assert!(res.diagnostics.iter().any(|d| d.code == "E1022"), "{name}: expected E1022");
        assert!(elapsed < Duration::from_secs(30), "{name}: rejection took {elapsed:?}");
    }
}

/// Just under the bound still checks: 8000 operator levels.
#[test]
fn long_operator_sequences_below_the_bound_check() {
    let src = format!("fn main() {{\n    s: i64 := 0{}\n    print(s)\n}}\n", " + 1".repeat(8000));
    checks_within("long-sum", &src, Duration::from_secs(30));
}
