//! Shared command argument validation and entry discovery.

pub fn normalize(args: Vec<String>) -> Result<Vec<String>, String> {
    let Some(command) = args.first() else {
        return Ok(args);
    };
    if super::packages::COMMANDS.contains(&command.as_str()) { super::packages::validate(command,&args[1..])?; return Ok(args); }
    if matches!(command.as_str(), "stdlib" | "syntax" | "explain") { return Ok(args); }
    if command == "fmt" { return formatter_args(args); }
    let native = matches!(command.as_str(), "build" | "run" | "test" | "profile");
    let source = native
        || matches!(
            command.as_str(),
            "lex" | "ast" | "check" | "resolve" | "types" | "ir"
        );
    let usage = match command.as_str() {
        "check" => "tarn check [file.tarn | directory] [--json] [--watch]".into(),
        "lex" | "resolve" => format!("tarn {command} [file.tarn | directory] [--json]"),
        "ir" => "tarn ir [file.tarn | directory] [--drops]".into(),
        "build" => {
            "tarn build [file.tarn | directory] [-o path] [--link library]... [--json]".into()
        }
        "run" => "tarn run [file.tarn | directory] [--link library]... [--json] [--watch] [-- program arguments...]".into(),
        "profile" => "tarn profile [file.tarn | directory] [--link library]... [-- program arguments...]".into(),
        "test" => "tarn test [file.tarn | directory] [--bench [--runs n]] [--filter text] [--timeout milliseconds] [--json] [--watch] [--link library]...".into(),
        _ => format!(
            "tarn {command}{}",
            if source {
                " [file.tarn | directory]"
            } else {
                ""
            }
        ),
    };
    let fail = |message: String| format!("{message}\n\nusage: {usage}");
    let mut entry = None;
    let mut options = Vec::new();
    let mut output_seen = false;
    let mut program = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            // Everything after `--` belongs to the program, verbatim (os.args()).
            "--" if command == "run" || command == "profile" => {
                program.push(arg.clone());
                program.extend(rest.by_ref().cloned());
            }
            "--json" if matches!(command.as_str(), "lex" | "check" | "resolve" | "build" | "run" | "test") => options.push(arg.clone()),
            "--watch" if matches!(command.as_str(), "check" | "run" | "test") => options.push(arg.clone()),
            "--filter" | "--timeout" if command == "test" => {
                let value = rest.next().filter(|v| !v.starts_with('-')).ok_or_else(|| fail(format!("{arg} requires a value")))?;
                if arg == "--timeout" && !value.parse::<u64>().is_ok_and(|v| v > 0) { return Err(fail("--timeout requires positive integer milliseconds".into())); }
                if options.iter().any(|option| option == arg) { return Err(fail(format!("repeated option `{arg}`"))); }
                options.extend([arg.clone(), value.clone()]);
            }
            "--drops" if command == "ir" => options.push(arg.clone()),
            "--bench" if command == "test" && !options.contains(arg) => options.push(arg.clone()),
            "--runs" if command == "test" && !options.contains(arg) => {
                let value = rest.next().filter(|v| v.parse::<usize>().is_ok_and(|n| n > 0)).ok_or_else(|| fail("--runs requires a positive integer".into()))?;
                options.extend([arg.clone(), value.clone()]);
            }
            "-o" if command == "build" && !output_seen => {
                let value = rest.next().filter(|v| !v.starts_with('-')).ok_or_else(|| fail("-o requires an output path".into()))?;
                output_seen = true;
                options.extend([arg.clone(), value.clone()]);
            }
            "--link" if native => {
                let value = rest.next().ok_or_else(|| fail("--link requires a library".into()))?;
                if !tarn_backend::valid_library(value) {
                    return Err(fail(format!("invalid library `{value}`: use a name such as `sqlite3` or an exact file such as `:libsqlite3.so.0`")));
                }
                options.extend([arg.clone(), value.clone()]);
            }
            _ if arg.starts_with('-') => return Err(fail(format!("unknown or repeated option `{arg}`"))),
            _ if source && entry.is_none() => entry = Some(arg.clone()),
            _ => return Err(fail(format!("unexpected argument `{arg}`"))),
        }
    }
    if !source {
        return Ok(args);
    }
    let default_entry = entry.is_none();
    let mut path = std::path::PathBuf::from(entry.unwrap_or_else(|| ".".into()));
    if path.is_dir() || default_entry {
        let manifest = tarn_packages::manifest::find(&path).and_then(|root| tarn_packages::manifest::Manifest::read(&root).ok());
        path = match manifest {
            Some(manifest) => manifest.root.join(manifest.entry),
            None => path.join("main.tarn"),
        };
        if default_entry && manifestless_relative(&path) { path = "main.tarn".into(); }
    }
    let mut normalized = vec![command.clone(), path.to_string_lossy().into_owned()];
    normalized.extend(options);
    normalized.extend(program);
    Ok(normalized)
}

fn formatter_args(args: Vec<String>) -> Result<Vec<String>, String> {
    let usage = "usage: tarn fmt [file.tarn | directory] [--check] | tarn fmt --stdin";
    let fail = |message: &str| format!("{message}\n\n{usage}");
    let mut path = None;
    let mut check = false;
    let mut stdin = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--check" if !check => check = true,
            "--stdin" if !stdin => stdin = true,
            _ if arg.starts_with('-') => return Err(fail("unknown or repeated formatter option")),
            _ if path.is_none() => path = Some(arg.clone()),
            _ => return Err(fail("unexpected formatter argument")),
        }
    }
    if stdin && (check || path.is_some()) { return Err(fail("--stdin cannot be combined with a path or --check")); }
    if stdin { return Ok(vec!["fmt".into(), "--stdin".into()]); }
    let mut result = vec!["fmt".into(), path.unwrap_or_else(|| ".".into())];
    if check { result.push("--check".into()); }
    Ok(result)
}

fn manifestless_relative(path: &std::path::Path) -> bool { path == std::path::Path::new("./main.tarn") }
