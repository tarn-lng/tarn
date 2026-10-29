//! ADR 0086: consuming `for x in vec` destroys every element exactly once on
//! every exit (end, break, continue, return, `try`), yielded or not.
use std::collections::HashMap;
use std::process::Command;

fn drops(source: &str, tag: &str) -> (String, HashMap<String, usize>) {
    let dir = std::env::temp_dir().join(format!("tarn-vector-iteration-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, source).unwrap();
    let result = tarn_driver::check(&entry).unwrap();
    assert!(!result.has_errors(), "{}", result.diagnostics.iter().map(|d| d.render(&result.program.sources)).collect::<String>());
    let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    let output = Command::new("timeout").arg("30s").arg(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
    let mut counts = HashMap::new();
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        // Element names are `e<tag><index>`; other strings (prefixes, numbers) are ignored.
        if let Some(name) = line.strip_prefix("drop:")
            && name.len() > 2
            && name.starts_with('e')
            && name.as_bytes()[1].is_ascii_uppercase()
            && name[2..].bytes().all(|b| b.is_ascii_digit())
        {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    (String::from_utf8(output.stdout).unwrap(), counts)
}

const HELPERS: &str = r#"
import "string"
fn make(tag string, n i64) Vec<string> {
    var out: Vec<string> = Vec.new()
    var i: i64 = 0
    for i < n {
        out.push("e" + tag + string.from_i64(i))
        i = i + 1
    }
    return out
}
// Exits are chosen by position, so no other string equals an element.
fn find(v Vec<string>, at i64) bool {
    var i: i64 = 0
    for s in v {
        if i == at { return true }
        i = i + 1
    }
    return false
}
fn fail(v Vec<string>) Result<i64, string> {
    var n: i64 = 0
    for s in v {
        if n == 2 { return Err(s) }
        n = n + 1
    }
    return Ok(n)
}
"#;

#[test]
fn every_element_is_destroyed_once_on_every_exit() {
    let source = format!(
        "{HELPERS}
fn main() {{
    for s in make(\"A\", 3) {{ print(s) }}
    var i: i64 = 0
    for s in make(\"B\", 5) {{
        if i == 1 {{ break }}
        i = i + 1
    }}
    i = 0
    for s in make(\"C\", 4) {{
        i = i + 1
        if i == 2 {{ continue }}
        print(s)
    }}
    print(find(make(\"R\", 6), 2))
    match fail(make(\"T\", 5)) {{ Ok(n) => {{ print(n) }}
        Err(e) => {{ print(e) }} }}
    var empty: Vec<string> = Vec.new()
    for s in empty {{ print(s) }}
}}
"
    );
    let (stdout, counts) = drops(&source, "exits");
    assert_eq!(stdout, "eA0\neA1\neA2\neC0\neC2\neC3\ntrue\neT2\n");
    let expected = [("A", 3), ("B", 5), ("C", 4), ("R", 6), ("T", 5)];
    for (tag, n) in expected {
        for i in 0..n {
            let name = format!("e{tag}{i}");
            assert_eq!(counts.get(&name), Some(&1), "{name} destroyed {:?} times; all: {counts:?}", counts.get(&name));
        }
    }
    assert_eq!(counts.len(), expected.iter().map(|(_, n)| n).sum::<usize>(), "{counts:?}");
}

/// Borrowed iteration leaves the vector intact: nothing is destroyed until
/// the vector itself goes out of scope.
#[test]
fn borrowed_iteration_destroys_nothing_early() {
    let source = format!(
        "{HELPERS}
fn main() {{
    var v = make(\"M\", 3)
    for s in &v {{ print(s) }}
    for s in &mut v {{ print(s.len()) }}
    print(v.len())
}}
"
    );
    let (stdout, counts) = drops(&source, "borrowed");
    assert_eq!(stdout, "eM0\neM1\neM2\n3\n3\n3\n3\n");
    assert_eq!(counts.len(), 3);
    assert!(counts.values().all(|&c| c == 1), "{counts:?}");
}
