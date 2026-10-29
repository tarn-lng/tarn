//! `tarn` — the single official CLI.

mod args;
mod stdlib;
mod reference;
mod watch;
mod process;
mod testing;
mod formatting;
mod packages;

use std::process::ExitCode;
use tarn_diagnostics::SourceMap;

const USAGE: &str = "\
tarn — the Tarn language toolchain

usage:
    tarn lex [entry] [--json]    print the token stream (debugging)
    tarn ast [entry]             parse and print the syntax tree
    tarn check [entry] [--json] [--watch]  check syntax, types, ownership and borrows
    tarn resolve [entry] [--json] print what every name resolves to
    tarn types [entry]           print the type of every local and parameter
    tarn ir [entry] [--drops]    print typed IR or executable drops
    tarn build [entry] [-o path] [--link lib]... [--json]
                                     emit a native Linux x86_64/ARM64 executable; each
                                     --link grants a system C library
    tarn run [entry] [--link lib]... [--json] [--watch] [-- args...]
                                     build temporarily and execute
    tarn test [entry] [--filter text] [--timeout milliseconds] [--json] [--watch]
                                     run isolated tests (also accepts --link)
    tarn test [entry] --bench [--runs n] [--filter text] [--json]
                                     measure isolated bench_* functions
    tarn fmt [file.tarn | directory] [--check]
                                     format sources recursively (default: .)
    tarn fmt --stdin                 format stdin to stdout
    tarn init [directory] [--lib]      create a package manifest and source
    tarn add package [--version range] [--registry origin]
    tarn remove package              remove a direct dependency
    tarn update [package]            explicitly resolve dependency versions
    tarn fetch                       fetch locked source bytes
    tarn deps [--tree | --why package] [--trust] [--json]
    tarn verify [--json]              verify locked content and graph
    tarn audit [--json]               inspect advisories; 3 means unknown evidence
    tarn publish [--registry origin]  publish an immutable local release
    tarn profile <file.tarn> [--link lib]... [-- args...]
                                     run under callgrind and list the costliest
                                     Tarn functions (requires valgrind)
    tarn stdlib [module] [--json]     discover embedded public APIs
    tarn syntax [--json]              keywords and language rules for this compiler
    tarn explain <code> [--json]      explain a diagnostic code, e.g. E3083
    tarn version                     print the compiler version

Entry uses the nearest tarn.toml package.entry; otherwise ./main.tarn.
Exit codes: 0 success, 1 failure, 2 usage error; audit 3 means unknown evidence.
run propagates the program exit code.

planned: clean, cache
";

/// Tool arguments only: everything after `--` belongs to the program.
pub(crate) fn tool_args(args: &[String]) -> &[String] {
    args.iter().position(|a| a == "--").map_or(args, |end| &args[..end])
}

/// Verified package dependencies of `entry` (ADR 0051). Compilation reads
/// the existing lock and cache only: it never resolves, fetches or rewrites.
pub(crate) fn package_set(entry: &std::path::Path) -> Result<Option<tarn_driver::PackageSet>, String> {
    tarn_packages::graph::package_set(entry)
}

/// The package's `[native] links` (ADR 0091) followed by any `--link`
/// libraries, without repeats. Only the entry's own manifest grants links.
pub(crate) fn linked_libraries(entry: &std::path::Path, mut extra: Vec<String>) -> Result<Vec<String>, String> {
    let mut libraries = match tarn_packages::manifest::find(entry) {
        Some(root) => tarn_packages::manifest::Manifest::read(&root)?.links,
        None => Vec::new(),
    };
    extra.retain(|l| !libraries.contains(l));
    libraries.append(&mut extra);
    Ok(libraries)
}

/// Check a program together with its verified package dependencies.
pub(crate) fn check_project(entry: &std::path::Path) -> Result<tarn_driver::CheckResult, String> {
    let packages = package_set(entry)?;
    tarn_driver::check_with_packages(entry, &Default::default(), packages.as_ref())
}

/// Every command walks syntax trees recursively (formatting, `ast`, checks),
/// so the whole CLI runs on the compiler's large stack (ADR 0081).
fn main() -> ExitCode {
    tarn_driver::with_compiler_stack(run)
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match args::normalize(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    if let Some(command) = args.first().filter(|c| packages::COMMANDS.contains(&c.as_str())) {
        return packages::run(command, &args[1..]);
    }
    match args.first().map(String::as_str) {
        Some("stdlib") => stdlib::run(&args[1..]),
        Some("syntax") => reference::syntax(&args[1..]),
        Some("explain") => reference::explain(&args[1..]),
        Some("lex") => cmd_lex(&args[1..]),
        Some("ast") => cmd_ast(&args[1..]),
        Some("check") => cmd_check(&args[1..], false),
        Some("resolve") => cmd_check(&args[1..], true),
        Some("types") => cmd_types(&args[1..]),
        Some("ir") => cmd_ir(&args[1..]),
        Some("build") => cmd_native(&args[1..], false),
        Some("run") if tool_args(&args).iter().any(|a| a == "--watch") => watch::run(&args[1..]),
        Some("run") => cmd_native(&args[1..], true),
        Some("profile") => cmd_profile(&args[1..]),
        Some("test") => {
            let options = testing::Options::parse(&args[1..]);
            if args.iter().any(|a| a == "--watch") { watch::test(std::path::Path::new(&args[1]), &options) }
            else { process::install_handlers(); testing::run(std::path::Path::new(&args[1]), &options).0 }
        },
        Some("version" | "--version" | "-V") => {
            println!("tarn {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") | None => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("fmt") => formatting::run(&args[1..]),
        Some(cmd @ ("clean" | "cache")) => {
            eprintln!("error: `tarn {cmd}` is not implemented yet (see docs/roadmap.md)");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn cmd_lex(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("error: missing file\n\nusage: tarn lex <file.tarn> [--json]");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error[E9001]: cannot read `{path}`: {e}");
            return ExitCode::from(1);
        }
    };
    let mut map = SourceMap::new();
    let id = map.add(path.clone(), text);
    let res = tarn_lexer::lex(id, map.file(id));

    for t in &res.tokens {
        let text = map.snippet(t.span);
        if json {
            let mut s = String::new();
            json_str(&mut s, text);
            println!(
                "{{\"kind\":\"{}\",\"line\":{},\"column\":{},\"start\":{},\"end\":{},\"text\":{s}}}",
                t.kind.name(),
                t.line,
                t.column,
                t.span.start,
                t.span.end
            );
        } else {
            println!("{}:{}\t{}\t{}", t.line, t.column, t.kind.name(), text.escape_debug());
        }
    }
    for d in &res.diagnostics {
        if json {
            println!("{}", d.to_json(&map));
        } else {
            eprint!("{}", d.render(&map));
        }
    }
    if res.diagnostics.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn cmd_ast(args: &[String]) -> ExitCode {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("error: missing file\n\nusage: tarn ast <file.tarn>");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error[E9001]: cannot read `{path}`: {e}");
            return ExitCode::from(1);
        }
    };
    let mut map = SourceMap::new();
    let id = map.add(path.clone(), text);
    let res = tarn_parser::parse_file(id, map.file(id));
    print!("{}", tarn_ast::dump_module(&res.module));
    for d in &res.diagnostics {
        eprint!("{}", d.render(&map));
    }
    if res.diagnostics.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn cmd_check(args: &[String], dump: bool) -> ExitCode {
    if args.iter().any(|arg| arg == "--watch") {
        return watch::check(std::path::Path::new(&args[0]), args.iter().any(|arg| arg == "--json"));
    }
    let json = args.iter().any(|a| a == "--json");
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("error: missing file\n\nusage: tarn check <file.tarn> [--json]");
        return ExitCode::from(2);
    };
    let res = match check_project(std::path::Path::new(path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error[E9001]: {e}");
            return ExitCode::from(1);
        }
    };
    let map = &res.program.sources;
    if dump && let Some(r) = &res.resolved {
        print!("{}", tarn_resolve::dump_resolution(r, map));
    }
    for d in &res.diagnostics {
        if json {
            println!("{}", d.to_json(map));
        } else {
            eprint!("{}", d.render(map));
        }
    }
    if res.has_errors() { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn cmd_types(args: &[String]) -> ExitCode {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("error: missing file\n\nusage: tarn types <file.tarn>");
        return ExitCode::from(2);
    };
    let res = match check_project(std::path::Path::new(path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error[E9001]: {e}");
            return ExitCode::from(1);
        }
    };
    let map = &res.program.sources;
    if let (Some(r), Some(t)) = (&res.resolved, &res.typed) {
        print!("{}", tarn_types::dump_types(t, r, map));
    }
    for d in &res.diagnostics {
        eprint!("{}", d.render(map));
    }
    if res.has_errors() { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn cmd_ir(args: &[String]) -> ExitCode {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("error: missing file\n\nusage: tarn ir <file.tarn>");
        return ExitCode::from(2);
    };
    let res = match check_project(std::path::Path::new(path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error[E9001]: {e}");
            return ExitCode::from(1);
        }
    };
    let map = &res.program.sources;
    if let (Some(r), Some(t), Some(ir)) = (&res.resolved, &res.typed, &res.ir) {
        if args.iter().any(|a| a == "--drops") {
            if let Some(drops) = &res.drops { print!("{}", tarn_ir::post_drop::print_program(drops, r, t)); }
        } else {
            let notes = res.moves.as_ref().map(|m| m.drop_notes()).unwrap_or_default();
            print!("{}", tarn_ir::print_program_annotated(ir, r, t, &notes));
        }
    }
    for d in &res.diagnostics {
        eprint!("{}", d.render(map));
    }
    if res.has_errors() { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn cmd_native(args: &[String], run: bool) -> ExitCode {
    let Some(path) = args.first().filter(|p| !p.starts_with('-')) else {
        eprintln!("error: usage: tarn {} file.tarn{} [--link library]...", if run { "run" } else { "build" }, if run { "" } else { " [-o path]" });
        return ExitCode::from(2);
    };
    // Arguments have already been validated by the shared parser.
    let json = tool_args(args).iter().any(|arg| arg == "--json");
    let mut explicit = None;
    let mut libraries = Vec::new();
    let mut program = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-o" => explicit = rest.next().map(std::path::PathBuf::from),
            "--link" => libraries.push(rest.next().expect("validated library").clone()),
            "--" => program.extend(rest.by_ref().cloned()),
            "--json" => {},
            _ => unreachable!("validated native option"),
        }
    }
    let libraries = match linked_libraries(std::path::Path::new(path), libraries) {
        Ok(l) => l, Err(e) => { native_error(json, "manifest", &e); return ExitCode::from(1); }
    };
    let res = match check_project(std::path::Path::new(path)) {
        Ok(r) => r, Err(e) => { native_error(json, "load", &e.to_string()); return ExitCode::from(1); }
    };
    for d in &res.diagnostics {
        if json { println!("{}", d.to_json(&res.program.sources)); }
        else { eprint!("{}", d.render(&res.program.sources)); }
    }
    if res.has_errors() { return ExitCode::from(1); }
    let (Some(p), Some(t)) = (&res.drops, &res.typed) else { native_error(json, "internal", "compiler bug: missing post-drop IR"); return ExitCode::from(1); };
    if let Some(d) = res.missing_main() {
        if json { println!("{}", d.to_json(&res.program.sources)); } else { eprint!("{}", d.render(&res.program.sources)); }
        return ExitCode::from(1);
    }
    let output = if run { std::env::temp_dir().join(format!("tarn-run-{}", std::process::id())) }
        else { explicit.unwrap_or_else(|| std::path::Path::new(path).with_extension("")) };
    if std::path::Path::new(path).canonicalize().ok() == output.canonicalize().ok() && output.exists() {
        native_error(json, "output", "output would overwrite the source file"); return ExitCode::from(1);
    }
    if let Err(e) = tarn_backend::build_linked(p, t, &output, &libraries) { native_error(json, "native", &e.to_string()); return ExitCode::from(1); }
    if !run { return ExitCode::SUCCESS; }
    let status = std::process::Command::new(&output).args(&program).status();
    let _ = std::fs::remove_file(&output);
    match status {
        Ok(s) => { use std::os::unix::process::ExitStatusExt; ExitCode::from(s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(1)).clamp(0,255) as u8) }
        Err(e) => { native_error(json, "execute", &format!("cannot run executable: {e}")); ExitCode::from(1) }
    }
}

/// CLI failures without source spans use a separate, stable JSON Lines record.
fn native_error(json: bool, stage: &str, message: &str) {
    if json {
        let mut encoded = String::new();
        json_str(&mut encoded, message);
        println!("{{\"kind\":\"command_error\",\"stage\":\"{stage}\",\"message\":{encoded}}}");
    } else {
        eprintln!("error: {message}");
    }
}

/// `tarn profile` (Phase 30): build like `run`, execute under callgrind and
/// print the functions with the most instructions, by Tarn name.
fn cmd_profile(args: &[String]) -> ExitCode {
    if std::process::Command::new("valgrind").arg("--version").output().is_err() {
        eprintln!("error: `tarn profile` needs valgrind (callgrind); install it, e.g. `sudo apt install valgrind`");
        return ExitCode::from(1);
    }
    let path = std::path::Path::new(&args[0]);
    let mut libraries = Vec::new();
    let mut program = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--link" => libraries.push(rest.next().expect("validated library").clone()),
            "--" => program.extend(rest.by_ref().cloned()),
            _ => {}
        }
    }
    let libraries = match linked_libraries(path, libraries) {
        Ok(l) => l,
        Err(e) => { eprintln!("error: {e}"); return ExitCode::from(1); }
    };
    let res = match check_project(path) {
        Ok(r) => r,
        Err(e) => { eprintln!("error: {e}"); return ExitCode::from(1); }
    };
    for d in &res.diagnostics { eprint!("{}", d.render(&res.program.sources)); }
    if res.has_errors() { return ExitCode::from(1); }
    let (Some(p), Some(t)) = (&res.drops, &res.typed) else { eprintln!("error: compiler bug: missing post-drop IR"); return ExitCode::from(1); };
    if let Some(d) = res.missing_main() { eprint!("{}", d.render(&res.program.sources)); return ExitCode::from(1); }
    let scratch = match process::Scratch::new() {
        Ok(s) => s,
        Err(e) => { eprintln!("error: {e}"); return ExitCode::from(1); }
    };
    let executable = scratch.0.join("program");
    let profile = scratch.0.join("callgrind.out");
    if let Err(e) = tarn_backend::build_linked(p, t, &executable, &libraries) { eprintln!("error: {e}"); return ExitCode::from(1); }
    let status = std::process::Command::new("valgrind")
        .arg("--tool=callgrind").arg("--quiet").arg(format!("--callgrind-out-file={}", profile.display()))
        .arg(&executable).args(&program).status();
    let Ok(status) = status else { eprintln!("error: cannot start valgrind"); return ExitCode::from(1); };
    let annotate = std::process::Command::new("callgrind_annotate").arg("--inclusive=no").arg(&profile).output();
    let Ok(annotate) = annotate else { eprintln!("error: cannot run callgrind_annotate"); return ExitCode::from(1); };
    let text = String::from_utf8_lossy(&annotate.stdout);
    let mut total = 0u64;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((count, rest)) = line.split_once(' ') else { continue };
        let Ok(count) = count.replace(',', "").parse::<u64>() else { continue };
        if rest.contains("PROGRAM TOTALS") { total = count; continue; }
        let symbol = rest.rsplit_once(':').map_or(rest, |(_, s)| s);
        let symbol = symbol.split(" [").next().unwrap_or(symbol).trim();
        rows.push((count, readable_symbol(symbol)));
    }
    eprintln!();
    eprintln!("{:>8}  {:>14}  function", "share", "instructions");
    for (count, name) in rows.iter().take(15) {
        let share = if total > 0 { *count as f64 * 100.0 / total as f64 } else { 0.0 };
        eprintln!("{share:>7.2}%  {count:>14}  {name}");
    }
    eprintln!("total {total} instructions (callgrind; times are proportional, not wall clock)");
    ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8)
}

/// `tarn_fn_12__Vec_push__instance_3` → `Vec.push` (instance suffix dropped);
/// runtime and libc symbols are shown as they are.
fn readable_symbol(symbol: &str) -> String {
    let Some(rest) = symbol.strip_prefix("tarn_fn_") else { return symbol.to_string() };
    let name = rest.split_once("__").map_or(rest, |(_, n)| n);
    let name = name.split("__instance").next().unwrap_or(name);
    name.replacen('_', ".", if name.starts_with('_') { 0 } else { 1 })
}

