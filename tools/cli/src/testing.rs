//! Sequential isolated test execution over one frontend-generated executable.
use std::io::Read;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

pub struct Options {
    pub json: bool,
    pub filter: String,
    pub timeout: Duration,
    pub libraries: Vec<String>,
    /// `--bench`: run `bench_*` functions and measure them (Phase 30).
    pub bench: bool,
    pub runs: usize,
}
impl Options {
    pub fn parse(args: &[String]) -> Self {
        let mut options = Self {
            json: false,
            filter: String::new(),
            timeout: Duration::from_secs(10),
            libraries: Vec::new(),
            bench: false,
            runs: 5,
        };
        let mut rest = args[1..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--json" => options.json = true,
                "--filter" => options.filter = rest.next().unwrap().clone(),
                "--timeout" => {
                    options.timeout = Duration::from_millis(rest.next().unwrap().parse().unwrap())
                }
                "--link" => options.libraries.push(rest.next().unwrap().clone()),
                "--bench" => options.bench = true,
                "--runs" => options.runs = rest.next().unwrap().parse().unwrap(),
                "--watch" => {}
                _ => unreachable!("validated test option"),
            }
        }
        options
    }
}

fn capture(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        bytes
    })
}
fn quoted(text: &str) -> String {
    let mut result = String::new();
    super::json_str(&mut result, text);
    result
}

/// Return loaded paths even after diagnostic failure so watch can refresh.
pub fn run(entry: &Path, options: &Options) -> (ExitCode, Vec<std::path::PathBuf>) {
    run_with_sources(entry, options, |_, _| {})
}

pub fn run_with_sources(
    entry: &Path,
    options: &Options,
    mut loaded: impl FnMut(&[std::path::PathBuf], bool),
) -> (ExitCode, Vec<std::path::PathBuf>) {
    let linked = super::linked_libraries(entry, options.libraries.clone());
    let tested = crate::package_set(entry).and_then(|packages| {
        if options.bench {
            tarn_driver::testing::check_benches_with_packages(entry, packages.as_ref())
        } else {
            tarn_driver::testing::check_tests_with_packages(entry, packages.as_ref())
        }
    });
    let (result, cases) = match tested {
        Ok(result) => result,
        Err(error) => {
            let mut paths = tarn_driver::load(entry)
                .map(|(program, _)| program.disk_sources)
                .unwrap_or_default();
            paths.push(entry.to_path_buf());
            paths.extend(tarn_driver::testing::test_files(entry).unwrap_or_default());
            loaded(&paths, true);
            super::native_error(options.json, "test_discovery", &error);
            return (ExitCode::from(1), paths);
        }
    };
    let paths = result.program.disk_sources.clone();
    loaded(&paths, result.has_errors());
    for diagnostic in &result.diagnostics {
        if options.json {
            println!("{}", diagnostic.to_json(&result.program.sources));
        } else {
            eprint!("{}", diagnostic.render(&result.program.sources));
        }
    }
    if result.has_errors() {
        return (ExitCode::from(1), paths);
    }
    let mut selected: Vec<_> = cases
        .iter()
        .filter(|case| case.name.contains(&options.filter))
        .collect();
    selected.sort_by(|left, right| left.name.cmp(&right.name));
    let scratch = match super::process::Scratch::new() {
        Ok(scratch) => scratch,
        Err(error) => {
            super::native_error(options.json, "output", &error.to_string());
            return (ExitCode::from(1), paths);
        }
    };
    let executable = scratch.0.join("tests");
    if !selected.is_empty() {
        let drops = result.drops.as_ref().expect("checked post-drop IR");
        let entry = drops
            .functions
            .iter()
            .find(|function| function.decl.name == tarn_driver::testing::ENTRY)
            .expect("generated test entry")
            .decl
            .id;
        let libraries = match linked {
            Ok(libraries) => libraries,
            Err(error) => {
                super::native_error(options.json, "manifest", &error);
                return (ExitCode::from(1), paths);
            }
        };
        if let Err(error) = tarn_backend::build_tests(
            drops,
            result.typed.as_ref().unwrap(),
            &executable,
            &libraries,
            entry,
        ) {
            super::native_error(options.json, "native", &error.to_string());
            return (ExitCode::from(1), paths);
        }
    }
    if options.bench {
        let ok = run_benchmarks(&executable, &selected, options);
        return (if ok && !super::process::stopping() { ExitCode::SUCCESS } else { ExitCode::from(1) }, paths);
    }
    let mut failed = 0;
    let mut completed = 0;
    let mut skipped_count = 0;
    for case in selected {
        if super::process::stopping() {
            break;
        }
        let started = Instant::now();
        let mut child = match super::process::command(&executable)
            .arg(case.index.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                super::native_error(options.json, "execute", &error.to_string());
                return (ExitCode::from(1), paths);
            }
        };
        let stdout = capture(child.stdout.take().unwrap());
        let stderr = capture(child.stderr.take().unwrap());
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    super::process::send(&child, 9);
                    break Some(status);
                }
                Ok(None) if started.elapsed() < options.timeout && !super::process::stopping() => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    timed_out = !super::process::stopping();
                    super::process::terminate(&mut child, Duration::ZERO);
                    break None;
                }
            }
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        let stdout_text = std::str::from_utf8(&stdout).ok();
        let stderr_text = std::str::from_utf8(&stderr).ok();
        use std::os::unix::process::ExitStatusExt;
        let exit_code = status.and_then(|status| status.code());
        let signal = status.and_then(|status| status.signal());
        let outcome = Outcome::parse(stderr_text.unwrap_or(""));
        let skipped = !timed_out && outcome.skip.is_some();
        let ok = !skipped && outcome.checks.is_empty() && status.is_some_and(|status| status.success());
        completed += 1;
        if skipped {
            skipped_count += 1;
        } else if !ok {
            failed += 1;
        }
        let label = if skipped {
            "skipped"
        } else if ok {
            "ok"
        } else if timed_out {
            "timeout"
        } else {
            "FAILED"
        };
        if options.json {
            let checks: Vec<String> = outcome.checks.iter().map(|c| quoted(c)).collect();
            println!(
                "{{\"kind\":\"test\",\"name\":{},\"status\":{},\"duration_ms\":{},\"message\":{},\"location\":{},\"checks\":[{}],\"skip_reason\":{},\"stdout\":{},\"stderr\":{},\"exit_code\":{},\"signal\":{},\"stdout_bytes\":{},\"stderr_bytes\":{}}}",
                quoted(&case.name),
                quoted(label),
                started.elapsed().as_millis(),
                outcome.panic.as_deref().map_or_else(|| "null".into(), quoted),
                outcome.panic_at.as_deref().map_or_else(|| "null".into(), quoted),
                checks.join(","),
                outcome.skip.as_deref().map_or_else(|| "null".into(), quoted),
                stdout_text.map_or_else(|| "null".into(), quoted),
                stderr_text.map_or_else(|| "null".into(), quoted),
                exit_code.map_or_else(|| "null".into(), |value| value.to_string()),
                signal.map_or_else(|| "null".into(), |value| value.to_string()),
                if stdout_text.is_some() { "null".into() } else { format!("{stdout:?}") },
                if stderr_text.is_some() { "null".into() } else { format!("{stderr:?}") }
            );
        } else {
            println!("{} ... {label}", case.name);
            if let Some(reason) = outcome.skip.as_deref().filter(|_| skipped) {
                println!("    {reason}");
            } else if !ok {
                // One block per failure, on one stream, so it reads in order.
                if !outcome.checks.is_empty() {
                    let n = outcome.checks.len();
                    println!("    {n} failed check{}", if n == 1 { "" } else { "s" });
                    for check in &outcome.checks {
                        for (k, line) in check.lines().enumerate() {
                            println!("    {}{line}", if k == 0 { "- " } else { "  " });
                        }
                    }
                } else if timed_out {
                    println!("    timeout after {} ms", options.timeout.as_millis());
                } else if let Some(message) = &outcome.panic {
                    match &outcome.panic_at {
                        Some(at) => println!("    panicked at {at}: {message}"),
                        None => println!("    panicked: {message}"),
                    }
                } else if let Some(signal) = signal {
                    println!("    terminated by signal {signal}");
                } else {
                    println!("    exited with code {}", exit_code.unwrap_or(-1));
                }
                let section = |name: &str, text: Option<&str>, bytes: &[u8]| {
                    let text = text.map_or_else(|| format!("(bytes) {bytes:?}"), str::to_string);
                    if !text.trim().is_empty() {
                        println!("    {name}:");
                        for line in text.lines() {
                            println!("      | {line}");
                        }
                    }
                };
                section("stdout", stdout_text, &stdout);
                if stderr_text.is_some() {
                    section("stderr", Some(&outcome.rest), &stderr);
                } else {
                    section("stderr", None, &stderr);
                }
            }
        }
    }
    if completed == 0 && !options.json {
        println!("no tests found; tests are `fn test_*()` in `*_test.tarn` files or in the file given to `tarn test`");
    }
    if options.json {
        println!(
            "{{\"kind\":\"test_summary\",\"total\":{completed},\"passed\":{},\"failed\":{failed},\"skipped\":{skipped_count}}}",
            completed - failed - skipped_count
        );
    } else {
        let skipped = if skipped_count > 0 { format!("; {skipped_count} skipped") } else { String::new() };
        println!("test result: {} passed; {failed} failed{skipped}", completed - failed - skipped_count);
    }
    (
        if failed == 0 && !super::process::stopping() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        },
        paths,
    )
}

/// What a test process reported on stderr: the runtime's `panic:` line and
/// its `    at` location, `testing` check failures and skips (ADR 0094), and
/// everything else, in order.
#[derive(Default)]
struct Outcome {
    panic: Option<String>,
    panic_at: Option<String>,
    skip: Option<String>,
    checks: Vec<String>,
    rest: String,
}

impl Outcome {
    fn parse(stderr: &str) -> Self {
        let mut outcome = Outcome::default();
        let mut lines = stderr.lines().peekable();
        while let Some(line) = lines.next() {
            if let Some(message) = line.strip_prefix("panic: ") {
                outcome.panic = Some(message.to_string());
                if let Some(at) = lines.peek().and_then(|l| l.strip_prefix("    at ")) {
                    outcome.panic_at = Some(at.to_string());
                    lines.next();
                }
            } else if let Some(reason) = line.strip_prefix("tarn-test-skip: ") {
                outcome.skip = Some(reason.to_string());
            } else if let Some(check) = line.strip_prefix("tarn-test-fail: ") {
                let mut text = check.to_string();
                while let Some(more) = lines.next_if(|l| l.starts_with("  ")) {
                    text.push('\n');
                    text.push_str(more.trim_start());
                }
                outcome.checks.push(text);
            } else {
                outcome.rest.push_str(line);
                outcome.rest.push('\n');
            }
        }
        // The harness raises these from generated code: its location would
        // name a line that is not in the user's file.
        if outcome.panic.as_deref().is_some_and(|m| m.starts_with("test returned Err(") || m == "test had failed checks") {
            outcome.panic_at = None;
        }
        outcome
    }
}

/// One measured benchmark process: wall time from the runner, CPU time and
/// peak resident memory reported by the runtime's `getrusage` at exit.
struct Sample {
    wall: Duration,
    user_us: u64,
    system_us: u64,
    max_rss_kb: u64,
}

fn run_once(executable: &Path, index: usize, timeout: Duration) -> Result<Sample, String> {
    let started = Instant::now();
    let mut child = super::process::command(executable)
        .arg(index.to_string())
        .env("TARN_BENCH_RUSAGE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stderr = capture(child.stderr.take().unwrap());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout && !super::process::stopping() => std::thread::sleep(Duration::from_millis(1)),
            _ => {
                super::process::terminate(&mut child, Duration::ZERO);
                return Err(format!("timeout after {} ms", timeout.as_millis()));
            }
        }
    };
    let wall = started.elapsed();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    if !status.success() {
        let shown: String = stderr.lines().filter(|l| !l.starts_with("tarn-bench-usage:")).collect::<Vec<_>>().join("\n");
        return Err(format!("{status}{}{shown}", if shown.is_empty() { "" } else { "\n" }));
    }
    let usage = stderr.lines().find_map(|l| l.strip_prefix("tarn-bench-usage:")).ok_or("missing runtime usage report")?;
    let fields: Vec<u64> = usage.split(':').filter_map(|v| v.parse().ok()).collect();
    let [user_us, system_us, max_rss_kb] = fields[..] else { return Err("malformed runtime usage report".into()) };
    Ok(Sample { wall, user_us, system_us, max_rss_kb })
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

fn run_benchmarks(executable: &Path, cases: &[&tarn_driver::testing::TestCase], options: &Options) -> bool {
    let runs = options.runs.max(1);
    let mut failed = 0;
    if !options.json {
        println!("{:<40} {:>12} {:>12} {:>10}", "benchmark", "wall", "cpu", "peak rss");
    }
    for case in cases {
        let mut samples = Vec::new();
        let mut error = None;
        for _ in 0..runs {
            match run_once(executable, case.index, options.timeout) {
                Ok(sample) => samples.push(sample),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = error {
            failed += 1;
            if options.json {
                println!("{{\"kind\":\"bench\",\"name\":{},\"status\":\"FAILED\",\"error\":{}}}", quoted(&case.name), quoted(&e));
            } else {
                println!("{:<40} FAILED", case.name);
                eprintln!("{e}");
            }
            continue;
        }
        let wall = median(samples.iter().map(|s| s.wall.as_secs_f64() * 1000.0).collect());
        let cpu = median(samples.iter().map(|s| (s.user_us + s.system_us) as f64 / 1000.0).collect());
        let rss = samples.iter().map(|s| s.max_rss_kb).max().unwrap_or(0);
        if options.json {
            println!("{{\"kind\":\"bench\",\"name\":{},\"status\":\"ok\",\"runs\":{runs},\"wall_ms\":{wall:.3},\"cpu_ms\":{cpu:.3},\"peak_rss_kb\":{rss}}}", quoted(&case.name));
        } else {
            println!("{:<40} {:>9.2} ms {:>9.2} ms {:>7.1} MB", case.name, wall, cpu, rss as f64 / 1024.0);
        }
    }
    if !options.json {
        println!("bench result: {} measured; {failed} failed ({runs} runs each, median)", cases.len() - failed);
    }
    failed == 0
}

