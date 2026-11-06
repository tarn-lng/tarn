//! Phase 23A: ownership of core Option/Result combinators.
use std::process::Command;

fn run(source: &str, tag: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("tarn-combinators-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, source).unwrap();
    let result = tarn_driver::check(&entry).unwrap();
    assert!(
        !result.has_errors(),
        "{}",
        result.diagnostics.iter().map(|d| d.render(&result.program.sources)).collect::<String>()
    );
    let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    let output = Command::new("timeout").arg("30s").arg(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
    output
}

fn drops(output: &std::process::Output) -> Vec<String> {
    let mut drops: Vec<String> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("drop:"))
        .map(str::to_string)
        .collect();
    drops.sort();
    drops
}

/// Every owned value a combinator discards (unused fallback, dropped error,
/// unused Ok payload) is destroyed exactly once, and kept values once later.
/// `unused-b`/`unused-f` are parameters `some(false, ..)` itself discards.
#[test]
fn combinators_destroy_discarded_values_once() {
    let source = r#"
fn some(on bool, text string) Option<string> {
    if on { return Some(text) }
    return None
}
fn result(on bool, text string) Result<string, string> {
    if on { return Ok(text) }
    return Err(text)
}
fn main() {
    a := some(true, "kept-a").unwrap_or("unused-fallback-a")
    b := some(false, "unused-b").unwrap_or("kept-fallback-b")
    c := result(false, "dropped-error-c").unwrap_or("kept-fallback-c")
    d := result(true, "unused-ok-d").ok().is_some()
    e := result(false, "dropped-error-e").ok().is_none()
    f := some(false, "unused-f").ok_or("kept-error-f").is_err()
    g := result(false, "mapped-g").map_err(fn(m string) i32 { return 7 }).is_err()
    print(a)
    print(b)
    print(c)
    print(d && e && f && g)
}
"#;
    let output = run(source, "drops");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "kept-a\nkept-fallback-b\nkept-fallback-c\ntrue\n");
    assert_eq!(
        drops(&output),
        ["dropped-error-c", "dropped-error-e", "kept-a", "kept-error-f", "kept-fallback-b", "kept-fallback-c", "mapped-g", "unused-b", "unused-f", "unused-fallback-a", "unused-ok-d"]
    );
}

/// Phase 26B: a string literal where `&string` is expected is borrowed like
/// `&"..."`: one temporary per evaluation, destroyed exactly once.
#[test]
fn string_literal_borrows_destroy_each_temporary_once() {
    let source = r#"
fn size(text &string) usize { return text.len() }
fn main() {
    var total: usize = 0
    var n = 0
    for n < 3 {
        total = total + size("loop")
        n = n + 1
    }
    kept: &string := "kept"
    print(total + size("once") + kept.len())
}
"#;
    let output = run(source, "literal-borrow");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "20\n");
    assert_eq!(drops(&output), ["kept", "loop", "loop", "loop", "once"]);
}

/// Phase 27: string literals are static: evaluating one allocates nothing,
/// yet values made from them (moved, stored, cloned, concatenated) keep
/// ordinary ownership and are each destroyed once.
#[test]
#[cfg_attr(target_os = "macos", ignore = "counts mallocs through LD_PRELOAD, which dyld does not honor")]
fn string_literals_do_not_allocate_per_evaluation() {
    let dir = std::env::temp_dir().join(format!("tarn-literal-alloc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let counter = dir.join("count.so");
    std::fs::write(dir.join("count.c"), "#define _GNU_SOURCE\n#include <dlfcn.h>\n#include <stdio.h>\n#include <stdlib.h>\nstatic unsigned long calls;\nvoid *malloc(size_t n) { static void *(*real)(size_t); if (!real) real = dlsym(RTLD_NEXT, \"malloc\"); calls++; return real(n); }\n__attribute__((destructor)) static void report(void) { fprintf(stderr, \"mallocs:%lu\\n\", calls); }\n").unwrap();
    assert!(Command::new("cc").args(["-shared", "-fPIC", "-o"]).arg(&counter).arg(dir.join("count.c")).arg("-ldl").status().unwrap().success());
    let source = r#"
fn size(text &string) usize { return text.len() }
fn main() {
    var total: usize = 0
    var n = 0
    for n < 100000 {
        total = total + size("static")
        n = n + 1
    }
    var kept: Vec<string> = Vec.new()
    kept.push("moved")
    duplicate := "cloned".clone()
    joined := "a" + &duplicate
    print(total + kept.len() + joined.len())
}
"#;
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, source).unwrap();
    let result = tarn_driver::check(&entry).unwrap();
    assert!(!result.has_errors());
    let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    let output = Command::new(&exe).env("LD_PRELOAD", &counter).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "600008\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mallocs: u64 = stderr.lines().find_map(|l| l.strip_prefix("mallocs:")).unwrap().parse().unwrap();
    assert!(mallocs < 100, "literal evaluation allocated: {mallocs} mallocs");
    let mut traced: Vec<&str> = stderr.lines().filter_map(|l| l.strip_prefix("drop:")).filter(|d| *d != "static").collect();
    traced.sort();
    assert_eq!(traced, ["a", "acloned", "cloned", "cloned", "moved"]);
    assert_eq!(stderr.lines().filter(|l| *l == "drop:static").count(), 100000);
    std::fs::remove_dir_all(dir).unwrap();
}
