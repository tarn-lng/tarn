//! Exercise the public commands, output placement, exit propagation and errors.
use std::process::Command;
#[test]
fn build_run_and_failure_paths() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("hello.tarn");
    std::fs::write(&source, "fn main() {\n    print(42)\n}\n").unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    let output = Command::new(cli).arg("build").arg(&source).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let executable = source.with_extension("");
    let bytes = std::fs::read(&executable).unwrap();
    assert_eq!(&bytes[..4], b"\x7fELF");
    let output = Command::new(&executable).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    let output = Command::new(cli).arg("run").arg(&source).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    let explicit = dir.join("custom program");
    assert!(Command::new(cli).arg("build").arg(&source).arg("-o").arg(&explicit).status().unwrap().success());
    assert!(explicit.is_file());
    let original = std::fs::read(&source).unwrap();
    assert!(!Command::new(cli).arg("build").arg(&source).arg("-o").arg(&source).output().unwrap().status.success());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    // A rejected feature must preserve an existing compiled output.
    std::fs::write(&source, "fn work() {\n}\nfn main() {\n    scope {\n        spawn work()\n    }\n}\n").unwrap();
    let output = Command::new(cli).arg("build").arg(&source).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("backend not implemented"));
    assert_eq!(std::fs::read(&executable).unwrap(), bytes);
    std::fs::write(&source, "fn main() {\n    panic(\"stop\")\n}\n").unwrap();
    let output = Command::new(cli).arg("run").arg(&source).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("panic: stop"));
    std::fs::remove_dir_all(dir).unwrap();
}

/// Phase 18C: `--link` grants a system library; names cannot inject linker
/// options; missing symbols and libraries are summarized in C-source terms.
#[test]
fn link_grants_validate_and_explain_failures() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    let source = dir.join("floor.tarn");
    std::fs::write(&source, "extern \"C\" fn floor(x f64) f64\nfn main() {\n    unsafe { print(floor(3.75)) }\n}\n").unwrap();
    let output = Command::new(cli).args(["run"]).arg(&source).args(["--link", "m"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"3\n");
    for bad in ["-Wl,--wrap=main", "../lib", "", "a/b", ":"] {
        let output = Command::new(cli).arg("build").arg(&source).args(["--link", bad]).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{bad}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid library"), "{bad}");
    }
    std::fs::write(&source, "extern \"C\" fn tarn_no_such_symbol() i32\nfn main() {\n    unsafe { print(tarn_no_such_symbol()) }\n}\n").unwrap();
    let output = Command::new(cli).arg("build").arg(&source).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("no definition for extern \"C\" `tarn_no_such_symbol`"), "{stderr}");
    assert!(stderr.contains("--link"), "{stderr}");
    let output = Command::new(cli).arg("build").arg(&source).args(["--link", "tarn_missing_library"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot find library `tarn_missing_library`"), "{stderr}");
    // SQLite is present on the reference machine only as its runtime soname.
    let multiarch = if cfg!(target_arch = "aarch64") { "aarch64-linux-gnu" } else { "x86_64-linux-gnu" };
    let sqlite = std::path::PathBuf::from(format!("/usr/lib/{multiarch}/libsqlite3.so.0"));
    if sqlite.exists() {
        std::fs::write(&source, "extern \"C\" fn sqlite3_libversion_number() i32\nfn main() {\n    unsafe { print(sqlite3_libversion_number() >= 3000000) }\n}\n").unwrap();
        let output = Command::new(cli).arg("run").arg(&source).args(["--link", ":libsqlite3.so.0"]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"true\n");
        // Phase 18D: the safe wrapper example end to end.
        let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sqlite/main.tarn");
        let output = Command::new(cli).arg("run").arg(&example).args(["--link", ":libsqlite3.so.0"]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "3\n1\n1\npan integral\n2\nleche ñ\n3\ncafé 😀\nno such table: missing\n");
    } else {
        eprintln!("skipped: libsqlite3.so.0 is not installed");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Phase 24A: `tarn run -- args` reaches `os.args()` verbatim, including
/// arguments that look like tool flags; `os.env` reports invalid UTF-8.
#[test]
fn program_arguments_and_environment() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-os-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    let source = dir.join("main.tarn");
    std::fs::write(&source, "import \"os\"\nimport \"io\"\nfn main() Result<void, io.Error> {\n    args := try os.args()\n    var i = 1\n    for i < args.len() {\n        print(args.get(i))\n        i = i + 1\n    }\n    print(try os.env_or(&\"TARN_OS_TEST\", &\"unset\"))\n    print(os.env_bytes(&\"A=B\").is_none())\n    return Ok(())\n}\n").unwrap();
    let output = Command::new(cli).arg("run").arg(&source).args(["--", "plain", "with space", "ñ", "--watch", "--json", ""]).env_remove("TARN_OS_TEST").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "plain\nwith space\nñ\n--watch\n--json\n\nunset\ntrue\n");
    let output = Command::new(cli).arg("run").arg(&source).env("TARN_OS_TEST", "value").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "value\ntrue\n");
    use std::os::unix::ffi::OsStrExt;
    let invalid = std::ffi::OsStr::from_bytes(b"a\xff");
    let output = Command::new(cli).arg("run").arg(&source).env("TARN_OS_TEST", invalid).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid data"));
    // `--` is only meaningful for run.
    let output = Command::new(cli).arg("build").arg(&source).args(["--", "x"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    std::fs::remove_dir_all(dir).unwrap();
}

/// Phase 25: `os.exit` ends the process with its code, flushes earlier
/// output (also into a pipe) and runs nothing after it.
#[test]
fn os_exit_sets_the_status_and_flushes_output() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-exit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.tarn");
    std::fs::write(&source, "import \"os\"\nfn main() {\n    print(\"before\")\n    os.exit(7)\n    print(\"after\")\n}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn")).arg("run").arg(&source).output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"before\n");
    assert!(output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}

/// ADR 0057: range proofs only remove checks that cannot fail, so every
/// native program behaves identically with and without them (same stdout,
/// stderr and status), including programs that abort.
#[test]
fn range_proofs_do_not_change_behavior() {
    let cli = env!("CARGO_BIN_EXE_tarn");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/native/pass");
    let mut compared = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "tarn") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        // Networking and timing fixtures depend on the environment.
        if ["net", "time", "http", "process", "fs", "spawn", "async", "task", "executor", "suspended", "readiness"].iter().any(|m| source.contains(&format!("import \"{m}\""))) {
            continue;
        }
        let run = |proofs: bool| {
            let mut command = Command::new(cli);
            command.arg("run").arg(&path);
            if !proofs {
                command.env("TARN_NO_RANGE_PROOFS", "1");
            }
            command.output().unwrap()
        };
        let (with, without) = (run(true), run(false));
        assert_eq!(with.status.code(), without.status.code(), "{}", path.display());
        assert_eq!(with.stdout, without.stdout, "{}", path.display());
        compared += 1;
    }
    assert!(compared > 20, "{compared}");
}

/// Phase 30: `tarn test --bench` measures `bench_*` functions in isolated
/// processes, reports failures, and plain `tarn test` ignores them.
#[test]
fn bench_runner_measures_and_reports() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.tarn");
    std::fs::write(&source, "fn main() {\n}\nfn bench_sum() {\n    var t: u64 = 0\n    var i: u64 = 0\n    for i < 100000 {\n        t = t + i\n        i = i + 1\n    }\n    print(t)\n}\nfn bench_fails() {\n    panic(\"broken bench\")\n}\nfn test_plain() {\n}\n").unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    let output = Command::new(cli).args(["test", "--bench", "--runs", "2", "--json"]).arg(&source).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "a failing benchmark fails the run");
    let ok = stdout.lines().find(|l| l.contains("main.bench_sum")).unwrap();
    assert!(ok.contains("\"status\":\"ok\"") && ok.contains("\"runs\":2") && ok.contains("\"wall_ms\"") && ok.contains("\"cpu_ms\"") && ok.contains("\"peak_rss_kb\""), "{ok}");
    let failed = stdout.lines().find(|l| l.contains("main.bench_fails")).unwrap();
    assert!(failed.contains("FAILED") && failed.contains("broken bench"), "{failed}");
    assert!(!stdout.contains("test_plain"));
    let output = Command::new(cli).arg("test").arg(&source).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success() && stdout.contains("main.test_plain ... ok") && !stdout.contains("bench_"), "{stdout}");
    let output = Command::new(cli).args(["test", "--runs", "0", "--bench"]).arg(&source).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    std::fs::remove_dir_all(dir).unwrap();
}

/// Phase 30: `tarn profile` names Tarn functions in its report.
#[test]
fn profile_names_tarn_functions() {
    if Command::new("valgrind").arg("--version").output().is_err() {
        eprintln!("skipped: valgrind is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tarn-cli-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.tarn");
    std::fs::write(&source, "fn heavy(n u64) u64 {\n    var t: u64 = 0\n    var i: u64 = 0\n    for i < n {\n        t = t + i % 7\n        i = i + 1\n    }\n    return t\n}\nfn main() {\n    print(heavy(200000))\n}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn")).arg("profile").arg(&source).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    // `heavy` may be inlined into `main`; either way the report uses Tarn
    // names, never raw `tarn_fn_<id>__` symbols.
    assert!(stderr.lines().any(|l| l.trim_end().ends_with(" main") || l.trim_end().ends_with(" heavy")), "{stderr}");
    assert!(!stderr.contains("tarn_fn_"), "{stderr}");
    assert!(stderr.contains("total ") && stderr.contains("instructions"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_main_is_a_diagnostic_for_native_commands_only() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-no-main-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("library.tarn");
    std::fs::write(&source, "fn helper() i32 {\n    return 1\n}\n").unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    for command in ["build", "run"] {
        let output = Command::new(cli).arg(command).arg(&source).output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E3078") && stderr.contains("no `fn main`") && stderr.contains("tarn check"), "{stderr}");
        assert!(!stderr.contains("backend not implemented"), "{stderr}");
    }
    let output = Command::new(cli).args(["build", "--json"]).arg(&source).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"E3078\""));
    assert!(Command::new(cli).arg("check").arg(&source).status().unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}
