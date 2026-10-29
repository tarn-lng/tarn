//! Public API discovery from the compiler's embedded sources and real parser.
use std::process::ExitCode;
use tarn_ast::{FnDecl, ItemKind};
use tarn_diagnostics::{SourceMap, Span};

pub fn run(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let mut module = None;
    let mut json_seen = false;
    for arg in args {
        if arg == "--json" && !json_seen {
            json_seen = true;
        } else if !arg.starts_with('-') && module.is_none() {
            module = Some(arg.as_str());
        } else {
            super::native_error(json, "stdlib", "usage: tarn stdlib [module] [--json]");
            return ExitCode::from(2);
        }
    }
    let names = tarn_driver::stdlib_modules();
    let mut identity = Vec::new();
    for name in &names {
        identity.extend_from_slice(name.as_bytes());
        identity.push(0);
        identity.extend_from_slice(tarn_driver::stdlib_source(name).unwrap().as_bytes());
        identity.push(0);
    }
    let digest = tarn_packages::hash(&identity);
    let Some(name) = module else {
        if json {
            let mut out = String::from("{\"schema_version\":1,\"modules\":[");
            for (i, name) in names.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                super::json_str(&mut out, name);
            }
            out.push_str("],\"stdlib_hash\":");
            super::json_str(&mut out, &digest);
            out.push('}');
            println!("{out}");
        } else {
            println!("{}", names.join("\n"));
        }
        return ExitCode::SUCCESS;
    };
    let Some(source) = tarn_driver::stdlib_source(name) else {
        super::native_error(
            json,
            "stdlib",
            &format!("unknown standard module `{name}`; use `tarn stdlib` to list modules"),
        );
        return ExitCode::from(2);
    };
    let mut sources = SourceMap::new();
    let path = format!("stdlib/{name}/{name}.tarn");
    let id = sources.add(&path, source);
    let file = sources.file(id);
    let parsed = tarn_parser::parse_file(id, file);
    if !parsed.diagnostics.is_empty() {
        super::native_error(
            json,
            "stdlib",
            "embedded standard module has parse diagnostics",
        );
        return ExitCode::FAILURE;
    }
    let lexed = tarn_lexer::lex(id, file);
    let mut entries = Vec::new();
    let mut add = |qualified: String, kind: &str, span: Span, end: u32| {
        let signature = source[span.start as usize..end as usize]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut docs = Vec::new();
        let mut before = span.start as usize;
        for comment in parsed
            .comments
            .iter()
            .rev()
            .filter(|c| c.span.end <= span.start)
        {
            if !comment.doc || !source[comment.span.end as usize..before].trim().is_empty() {
                break;
            }
            docs.push(comment.text.trim().to_string());
            before = comment.span.start as usize;
        }
        docs.reverse();
        entries.push((
            qualified,
            kind.to_string(),
            signature,
            docs.join("\n"),
            file.line_col(span.start).line,
        ));
    };
    let fn_end = |f: &FnDecl| f.body.as_ref().map_or(f.span.end, |b| b.span.start);
    for item in parsed.module.items.iter().filter(|i| i.is_pub) {
        let header_end = || {
            lexed
                .tokens
                .iter()
                .find(|t| {
                    t.span.start >= item.span.start
                        && t.span.end <= item.span.end
                        && t.kind == tarn_lexer::TokenKind::LBrace
                })
                .map_or(item.span.end, |t| t.span.start)
        };
        match &item.kind {
            ItemKind::Fn(f) => {
                let owner = f
                    .owner
                    .as_ref()
                    .map_or(String::new(), |o| format!("{}.", o.name.name));
                add(
                    format!("{name}.{owner}{}", f.name.name),
                    "function",
                    item.span,
                    fn_end(f),
                );
            }
            ItemKind::Struct(s) => {
                add(
                    format!("{name}.{}", s.name.name),
                    "struct",
                    item.span,
                    header_end(),
                );
                for field in s.fields.iter().filter(|f| f.is_pub) {
                    add(
                        format!("{name}.{}.{}", s.name.name, field.name.name),
                        "field",
                        field.span,
                        field.span.end,
                    );
                }
            }
            ItemKind::Enum(e) => {
                add(
                    format!("{name}.{}", e.name.name),
                    "enum",
                    item.span,
                    header_end(),
                );
                for variant in &e.variants {
                    add(
                        format!("{name}.{}.{}", e.name.name, variant.name.name),
                        "variant",
                        variant.span,
                        variant.span.end,
                    );
                }
            }
            ItemKind::Const(c) => {
                add(
                    format!("{name}.{}", c.name.name),
                    "constant",
                    item.span,
                    item.span.end,
                );
            }
            ItemKind::Interface(i) => {
                add(
                    format!("{name}.{}", i.name.name),
                    "interface",
                    item.span,
                    header_end(),
                );
                for f in &i.methods {
                    add(
                        format!("{name}.{}.{}", i.name.name, f.name.name),
                        "method",
                        f.span,
                        fn_end(f),
                    );
                }
            }
            _ => {}
        }
    }
    if json {
        let mut out = String::from("{\"schema_version\":1,\"module\":");
        super::json_str(&mut out, name);
        out.push_str(",\"stdlib_hash\":");
        super::json_str(&mut out, &digest);
        out.push_str(",\"source\":");
        super::json_str(&mut out, &path);
        out.push_str(",\"entries\":[");
        for (i, (name, kind, signature, docs, line)) in entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            for (j, (key, value)) in [
                ("name", name),
                ("kind", kind),
                ("signature", signature),
                ("docs", docs),
            ]
            .iter()
            .enumerate()
            {
                if j > 0 {
                    out.push(',');
                }
                super::json_str(&mut out, key);
                out.push(':');
                super::json_str(&mut out, value);
            }
            out.push_str(&format!(",\"line\":{line}}}"));
        }
        out.push_str("]}");
        println!("{out}");
    } else {
        println!("{path}");
        for (_, _, signature, docs, line) in entries {
            if !docs.is_empty() {
                println!("\n{docs}");
            }
            println!("{line}: {signature}");
        }
    }
    ExitCode::SUCCESS
}
