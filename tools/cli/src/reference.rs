//! Language reference for humans and coding agents: `tarn syntax` and
//! `tarn explain`. Both are embedded in the binary, like the stdlib catalog,
//! so an agent asks the compiler it is actually using.
use std::process::ExitCode;

const GUIDE: &str = include_str!("../../../docs/llm-guide.md");
const ERRORS: &str = include_str!("../../../docs/errors.md");

pub fn syntax(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    if args.iter().any(|a| a != "--json") {
        super::native_error(json, "syntax", "usage: tarn syntax [--json]");
        return ExitCode::from(2);
    }
    let words: Vec<&str> = tarn_lexer::KEYWORDS.iter().map(|(w, _)| *w).collect();
    let index = stdlib_index();
    if json {
        let mut out = String::from("{\"schema_version\":1,\"keywords\":[");
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            super::json_str(&mut out, w);
        }
        out.push_str("],\"stdlib\":[");
        for (i, (module, summary, names)) in index.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"module\":");
            super::json_str(&mut out, module);
            out.push_str(",\"summary\":");
            super::json_str(&mut out, summary);
            out.push_str(",\"public\":[");
            for (k, name) in names.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                super::json_str(&mut out, name);
            }
            out.push_str("]}");
        }
        out.push_str("],\"guide\":");
        super::json_str(&mut out, GUIDE);
        out.push('}');
        println!("{out}");
    } else {
        println!("Keywords: {}\n", words.join(" "));
        print!("{GUIDE}");
        println!("\n## Standard library (generated from this compiler)\n");
        println!("`tarn stdlib <module>` shows signatures and doc comments.\n");
        for (module, summary, names) in &index {
            println!("- `{module}`: {summary}");
            println!("  {}", names.join(", "));
        }
    }
    ExitCode::SUCCESS
}

/// Every embedded stdlib module: its leading comment's first sentence and its
/// public names, read from the sources compiled into this binary, so the index
/// changes whenever the stdlib does.
fn stdlib_index() -> Vec<(String, String, Vec<String>)> {
    let mut out = Vec::new();
    for name in tarn_driver::stdlib_modules() {
        let Some(source) = tarn_driver::stdlib_source(name) else { continue };
        let summary = source
            .lines()
            .take_while(|l| l.starts_with("//"))
            .map(|l| l.trim_start_matches('/').trim())
            .collect::<Vec<_>>()
            .join(" ");
        let summary = summary.split(". ").next().unwrap_or("").trim_end_matches('.').to_string();
        let mut map = tarn_diagnostics::SourceMap::new();
        let id = map.add(format!("{name}.tarn"), source);
        let parsed = tarn_parser::parse_file(id, map.file(id));
        let mut names = Vec::new();
        for item in parsed.module.items.iter().filter(|i| i.is_pub) {
            let named = match &item.kind {
                tarn_ast::ItemKind::Fn(f) => Some(match &f.owner {
                    Some(owner) => format!("{}.{}", owner.name.name, f.name.name),
                    None => f.name.name.clone(),
                }),
                tarn_ast::ItemKind::Struct(x) => Some(x.name.name.clone()),
                tarn_ast::ItemKind::Enum(x) => Some(x.name.name.clone()),
                tarn_ast::ItemKind::Interface(x) => Some(x.name.name.clone()),
                tarn_ast::ItemKind::Const(x) => Some(x.name.name.clone()),
                _ => None,
            };
            if let Some(n) = named.filter(|n| !n.starts_with('_')) {
                names.push(n);
            }
        }
        out.push((name.to_string(), summary, names));
    }
    out
}

pub fn explain(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let codes: Vec<&String> = args.iter().filter(|a| *a != "--json").collect();
    let [code] = codes.as_slice() else {
        super::native_error(json, "explain", "usage: tarn explain <code> [--json], e.g. `tarn explain E3083`");
        return ExitCode::from(2);
    };
    let code = code.to_ascii_uppercase();
    let entries = entries(&code);
    if entries.is_empty() {
        super::native_error(json, "explain", &format!("no documentation for `{code}`; codes look like E3001 or W3002"));
        return ExitCode::FAILURE;
    }
    if json {
        let mut out = String::from("{\"schema_version\":1,\"code\":");
        super::json_str(&mut out, &code);
        out.push_str(",\"entries\":[");
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            super::json_str(&mut out, e);
        }
        out.push_str("]}");
        println!("{out}");
    } else {
        println!("{}", entries.join("\n\n"));
    }
    ExitCode::SUCCESS
}

/// Every passage of the diagnostics reference documenting `code`: table rows
/// (`| E0001 | kind | message |`) and list items (`- E3082 \`kind\`: ...`)
/// with their indented continuation lines.
fn entries(code: &str) -> Vec<String> {
    let lines: Vec<&str> = ERRORS.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if line.starts_with('|') && cells.get(1) == Some(&code) {
            out.push(cells[2..].iter().filter(|c| !c.is_empty()).copied().collect::<Vec<_>>().join(": "));
        } else if let Some(rest) = line.strip_prefix("- ").and_then(|l| l.strip_prefix(code))
            && rest.starts_with([' ', ':'])
        {
            let mut text = line[2..].to_string();
            while i + 1 < lines.len() && lines[i + 1].starts_with("  ") {
                i += 1;
                text.push(' ');
                text.push_str(lines[i].trim());
            }
            out.push(text);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_emitted_code_family_has_reference_text() {
        assert!(!super::entries("E1005").is_empty());
        assert!(!super::entries("E3083").is_empty());
        assert!(!super::entries("E3084").is_empty());
        assert!(super::entries("E9999").is_empty());
    }

    /// The syntax index is read from the embedded stdlib, never written by hand.
    #[test]
    fn stdlib_index_follows_the_embedded_sources() {
        let index = super::stdlib_index();
        assert_eq!(index.len(), tarn_driver::stdlib_modules().len());
        let testing = index.iter().find(|(m, _, _)| m == "testing").expect("testing module listed");
        assert!(testing.2.iter().any(|n| n == "T.eq") && testing.2.iter().any(|n| n == "T.skip"));
        assert!(index.iter().all(|(_, _, names)| names.iter().all(|n| !n.starts_with('_'))));
    }

    /// `tarn explain` must know every code the compiler can emit.
    #[test]
    fn every_emitted_code_is_explained() {
        let mut missing = Vec::new();
        for krate in std::fs::read_dir("../../compiler").unwrap() {
            let Ok(files) = std::fs::read_dir(krate.unwrap().path().join("src")) else { continue };
            for file in files {
                let text = std::fs::read_to_string(file.unwrap().path()).unwrap_or_default();
                for (i, _) in text.match_indices("\"E").chain(text.match_indices("\"W")) {
                    let code = &text[i + 1..text.len().min(i + 6)];
                    if code.len() == 5 && code[1..].bytes().all(|b| b.is_ascii_digit()) && text[i + 6..].starts_with('"') && super::entries(code).is_empty() {
                        missing.push(code.to_string());
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "document these codes in docs/errors.md: {missing:?}");
    }
}
