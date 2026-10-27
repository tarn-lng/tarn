//! Each diagnostic code has exactly one meaning: every literal
//! `Diagnostic::<severity>("CODE", "kind", ...)` site must agree on the kind.
use std::collections::BTreeMap;
use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target" && n != "tests") { sources(&path, out); }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The next string literal after optional whitespace and one optional comma.
fn literal(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let text = text.strip_prefix(',').unwrap_or(text).trim_start();
    let body = text.strip_prefix('"')?;
    let end = body.find('"')?;
    Some((&body[..end], &body[end + 1..]))
}

#[test]
fn diagnostic_codes_have_one_kind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sources(&root.join("compiler"), &mut files);
    sources(&root.join("tools"), &mut files);
    let mut kinds: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut sites = 0;
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (at, _) in text.match_indices("Diagnostic::") {
            let rest = &text[at + "Diagnostic::".len()..];
            let Some(open) = rest.find('(').filter(|&i| rest[..i].chars().all(|c| c.is_ascii_lowercase() || c == '_')) else { continue };
            let Some((code, rest)) = literal(&rest[open + 1..]) else { continue };
            if !(code.len() == 5 && code.starts_with(['E', 'W']) && code[1..].chars().all(|c| c.is_ascii_digit())) { continue }
            let Some((kind, _)) = literal(rest) else { continue };
            sites += 1;
            let line = text[..at].lines().count();
            kinds.entry(code.to_owned()).or_default().entry(kind.to_owned()).or_insert_with(|| format!("{}:{line}", file.display()));
        }
    }
    assert!(sites > 100, "scanner found only {sites} diagnostic sites; did the constructor syntax change?");
    let conflicts: Vec<String> = kinds.iter().filter(|(_, k)| k.len() > 1)
        .map(|(code, k)| format!("{code}: {}", k.iter().map(|(kind, at)| format!("`{kind}` at {at}")).collect::<Vec<_>>().join(", ")))
        .collect();
    assert!(conflicts.is_empty(), "diagnostic codes reused for different meanings:\n{}", conflicts.join("\n"));
}
