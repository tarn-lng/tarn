//! Documentation examples are checked by the compiler they document, so a
//! language or stdlib change that breaks one fails the suite instead of
//! leaving stale docs. Fences in `docs/*.md`:
//!
//! - ```` ```tarn ````: must check. Code without a top-level item is wrapped
//!   in `fn main() { ... }`.
//! - ```` ```tarn sketch ````: illustrative (`{ ... }`, undeclared names);
//!   not checked.
//! - ```` ```tarn error=E4201 ````: must report that diagnostic code.
use std::path::Path;
use std::process::Command;

const ITEMS: &[&str] = &["fn ", "pub ", "struct ", "enum ", "import ", "const ", "copy ", "interface ", "impl ", "extern ", "async fn ", "unsafe fn "];

#[test]
fn documentation_examples_match_the_compiler() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let scratch = std::env::temp_dir().join(format!("tarn-doc-examples-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let mut failures = Vec::new();
    let mut checked = 0;
    let mut files: Vec<_> = std::fs::read_dir(&docs).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
    files.sort();
    for doc in files {
        let text = std::fs::read_to_string(&doc).unwrap();
        let mut lines = text.lines().enumerate();
        while let Some((start, line)) = lines.next() {
            let Some(info) = line.trim_start().strip_prefix("```tarn") else { continue };
            let indent = line.len() - line.trim_start().len();
            let mut code = String::new();
            for (_, body) in lines.by_ref() {
                if body.trim_start().starts_with("```") {
                    break;
                }
                code.push_str(body.get(indent..).unwrap_or(body.trim_start()));
                code.push('\n');
            }
            let info = info.trim();
            if info == "sketch" {
                continue;
            }
            let expected = info.strip_prefix("error=");
            let top = code.lines().any(|l| ITEMS.iter().any(|item| l.starts_with(item)));
            let source = if top { code.clone() } else { format!("fn main() {{\n{code}}}\n") };
            let dir = scratch.join(format!("{}-{start}", doc.file_stem().unwrap().to_string_lossy()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("main.tarn"), &source).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_tarn")).arg("check").arg(dir.join("main.tarn")).output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let place = format!("{}:{}", doc.display(), start + 1);
            checked += 1;
            match expected {
                None if !output.status.success() => {
                    let first = stderr.lines().find(|l| l.starts_with("error")).unwrap_or("").to_string();
                    failures.push(format!("{place}: example no longer checks: {first}\n  fix the example, or mark an illustrative one ```tarn sketch"));
                }
                Some(code) if !stderr.contains(&format!("error[{code}]")) => {
                    failures.push(format!("{place}: expected {code}, got: {}", stderr.lines().next().unwrap_or("success")));
                }
                _ => {}
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(checked >= 10, "only {checked} documentation examples were checked");
    assert!(failures.is_empty(), "{} stale documentation example(s):\n{}", failures.len(), failures.join("\n"));
}
