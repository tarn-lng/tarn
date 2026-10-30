//! Discovery, one-build dispatch and abort/timeout isolation through the public CLI.
use std::process::Command;

#[test]
fn reports_all_outcomes_filters_and_private_module_tests() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "fn main() { panic(\"application main must not run\") }\nfn test_ok() {}\nfn test_err() Result<void, string> { return Err(\"error value\") }\nfn test_panic() { panic(\"panic value\") }\nfn test_hang() { for {} }\n").unwrap();
    std::fs::write(dir.join("side_test.tarn"), "fn private_value() i32 { return 42 }\nfn test_private() { if private_value() != 42 { panic(\"wrong\") } }\n").unwrap();
    let cli = env!("CARGO_BIN_EXE_tarn");
    // Count the real native link invocations: all selected tests share one
    // build. A one-time `cc -c` of the cached runtime object is not a link.
    use std::os::unix::fs::PermissionsExt;
    let count = dir.join("link-count");
    let wrapper = dir.join("cc");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *' -c '*) ;; *) printf x >> '{}' ;; esac\nexec /usr/bin/cc \"$@\"\n",
            count.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let search = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
    let output = Command::new(cli)
        .env("PATH", search)
        .arg("test")
        .arg(&dir)
        .args(["--timeout", "200", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        std::fs::read(&count).unwrap(),
        b"x",
        "compile/link exactly once"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout.lines().count(), 6, "{stdout}");
    assert!(stdout.contains("\"status\":\"timeout\""));
    assert_eq!(
        stdout.matches("\"status\":\"FAILED\"").count(),
        2,
        "{stdout}"
    );
    assert!(stdout.contains("error value"));
    assert!(stdout.contains("panic value"));
    assert_eq!(stdout.matches("\"signal\":6").count(), 2);
    assert!(stdout.contains("side_test.test_private"));
    assert!(stdout.contains("\"passed\":2,\"failed\":3"));
    let output = Command::new(cli)
        .arg("test")
        .arg(&dir)
        .args(["--filter", "private"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    let output = Command::new(cli)
        .arg("test")
        .arg(&entry)
        .args(["--filter", "no_match"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 passed; 0 failed"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejects_signatures_and_keeps_ordinary_visibility() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-signatures-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    for signature in [
        "fn test_bad(value i32) {}",
        "fn test_bad() i32 { return 1 }",
        "fn test_bad<T>() {}",
        "unsafe fn test_bad() {}",
    ] {
        std::fs::write(&entry, format!("fn main() {{}}\n{signature}\n")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
            .arg("test")
            .arg(&entry)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{signature}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("test `main.test_bad`"));
    }
    std::fs::write(&entry, "fn main() {}\nfn secret() i32 { return 1 }\n").unwrap();
    std::fs::write(
        dir.join("other_test.tarn"),
        "import \"main\"\nfn test_private() { print(main.secret()) }\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("private"));
    for args in [
        vec!["--timeout", "0"],
        vec!["--timeout", "bad"],
        vec!["--timeout"],
        vec!["--filter"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
            .arg("test")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn drains_both_streams_without_deadlock() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-pipes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "import \"ffi\"\nextern \"C\" fn write(fd i32, bytes *u8, count usize) isize\nfn test_pipes() {\n for i in 0..3000 {\n print(\"stdout filling a pipe with several hundred kilobytes of output\")\n text := \"stderr filling a pipe with several hundred kilobytes of output\\n\"\n unsafe { written := write(2, ffi.slice(text.bytes()), text.len()) }\n }\n}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&entry)
        .args(["--json", "--timeout", "5000"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stdout filling"));
    assert!(stdout.contains("stderr filling"));
    assert!(stdout.contains("\"status\":\"ok\""));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stdlib_behaviors_are_tested_in_tarn() {
    let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/stdlib");
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(entry)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("15 passed; 0 failed"));
}

#[test]
fn reports_enum_variants_and_owned_struct_fields() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-errors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "enum TestError {\n Broken(string)\n Other\n}\nstruct TestFailure { message string }\nfn test_enum() Result<void, TestError> { return Err(TestError.Broken(\"enum payload\")) }\nfn test_struct() Result<void, TestFailure> { return Err(TestFailure{message: \"owned field value\"}) }\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&entry)
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("TestError.Broken"), "{stdout}");
    assert!(stdout.contains("enum payload"), "{stdout}");
    assert!(stdout.contains("owned field value"), "{stdout}");
    assert!(stdout.contains("\"passed\":0,\"failed\":2"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preserves_invalid_utf8_output_as_bytes() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-binary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "import \"ffi\"\nextern \"C\" fn write(fd i32, data *u8, count usize) isize\nfn test_bytes() {\n bytes := [3]u8{0, 255, 10}\n unsafe { written := write(1, ffi.slice(&bytes), 3) }\n}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&entry)
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"stdout\":null"), "{stdout}");
    assert!(stdout.contains("\"stdout_bytes\":[0, 255, 10]"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn generated_imports_do_not_heal_unterminated_source() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "fn test_bad() { print(\"unterminated").unwrap();
    std::fs::write(dir.join("side_test.tarn"), "fn test_side() {}\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&entry)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("unterminated string"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test_summary"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn application_shadowing_cannot_turn_error_into_success() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-shadow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "fn panic(message string) {}\nfn print(value i32) {}\nfn test_application_names() { panic(\"ordinary user function\")\n print(7) }\nfn test_failure() Result<void, string> { return Err(\"must fail\") }\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .arg("test")
        .arg(&entry)
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("must fail"), "{stdout}");
    assert!(stdout.contains("\"passed\":1,\"failed\":1"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// ADR 0093: tests live anywhere in the package, imports resolve from the
/// package root, and the target selects what runs.
#[test]
fn package_wide_discovery_and_target_selection() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["gtk", "tests", ".build", "vendored"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let write = |path: &str, text: &str| std::fs::write(dir.join(path), text).unwrap();
    write("tarn.toml", "[package]\nname = \"layout\"\nversion = \"0.1.0\"\n");
    write("main.tarn", "import \"gtk/protocol\"\nfn main() { print(protocol.encode(1)) }\nfn test_entry() {}\n");
    write("gtk/util.tarn", "pub fn twice(x i32) i32 { return x * 2 }\n");
    write("gtk/protocol.tarn", "import \"gtk/util\"\npub fn encode(x i32) i32 { return util.twice(x) + 1 }\nfn test_inline() {}\n");
    write("gtk/protocol_test.tarn", "import \"gtk/protocol\"\nfn test_encode() { if protocol.encode(2) != 5 { panic(\"bad\") } }\n");
    write("tests/protocol_test.tarn", "import \"gtk/protocol\"\nfn test_same_stem() { if protocol.encode(0) != 1 { panic(\"bad\") } }\n");
    write("tests/helpers_test.tarn", "pub fn helper() i32 { return 1 }\n");
    write(".build/stale_test.tarn", "fn test_stale() { panic(\"hidden directories are not scanned\") }\n");
    write("vendored/tarn.toml", "[package]\nname = \"vendored\"\nversion = \"0.1.0\"\n");
    write("vendored/other_test.tarn", "fn test_nested() { panic(\"nested packages are not scanned\") }\n");
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_tarn")).current_dir(&dir).arg("test").args(args).output().unwrap();
        (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (code, stdout, stderr) = run(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stderr.is_empty(), "no warnings about generated imports: {stderr}");
    assert!(stdout.contains("4 passed; 0 failed"), "{stdout}");
    for name in ["main.test_entry", "gtk/protocol_test.test_encode", "tests/protocol_test.test_same_stem"] {
        assert!(stdout.contains(name), "{name}: {stdout}");
    }
    let (_, stdout, _) = run(&["gtk/protocol_test.tarn"]);
    assert!(stdout.contains("1 passed") && stdout.contains("test_encode"), "{stdout}");
    let (_, stdout, _) = run(&["gtk/protocol.tarn"]);
    assert!(stdout.contains("2 passed") && stdout.contains("gtk/protocol.test_inline"), "{stdout}");
    let (_, stdout, _) = run(&["tests"]);
    assert!(stdout.contains("1 passed") && stdout.contains("test_same_stem"), "{stdout}");
    let (_, stdout, _) = run(&["--filter", "same_stem"]);
    assert!(stdout.contains("1 passed"), "{stdout}");
    write("gtk/protocol_test.tarn", "fn test_err() Result<void, i32> { return Err(7) }\nfn test_panic() {\n print(\"before\")\n panic(\"stop\")\n}\n");
    let (code, stdout, _) = run(&["gtk"]);
    assert_eq!(code, Some(1));
    assert!(stdout.contains("    panicked: test returned Err(7)"), "{stdout}");
    assert!(stdout.contains("    panicked at gtk/protocol_test.tarn:4:2: stop\n    stdout:\n      | before"), "{stdout}");
    write("gtk/protocol_test.tarn", "fn helper() {}\n");
    let (code, stdout, _) = run(&["gtk/protocol_test.tarn"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("no tests found"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// ADR 0094: `t &mut testing.T` checks continue and report the test's line;
/// `require` stops, `skip` is neither passed nor failed; panics carry `at`.
#[test]
fn testing_library_reports_checks_skips_and_locations() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib_test.tarn"),
        "import \"testing\"\nfn test_checks(t &mut testing.T) {\n    t.eq(1 + 1, 3)\n    t.eq(\"a\", \"b\")\n    t.check(true, &\"fine\")\n}\nfn test_require(t &mut testing.T) {\n    t.require(false, &\"needs service\")\n    t.fail(&\"not reached\")\n}\nfn test_skip(t &mut testing.T) {\n    t.skip(&\"no display\")\n}\nfn test_pass(t &mut testing.T) {\n    t.eq(2, 2)\n    t.ne(1, 2)\n}\nfn test_unwrap() {\n    missing: Option<i32> := None\n    missing.unwrap()\n}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tarn")).current_dir(&dir).arg("test").arg(".").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("test_checks ... FAILED\n    2 failed checks\n    - lib_test.tarn:3:5: expected left == right\n      left: 2\n      right: 3\n    - lib_test.tarn:4:5:"), "{stdout}");
    assert!(stdout.contains("test_require ... FAILED\n    1 failed check\n    - lib_test.tarn:8:5: needs service\n"), "{stdout}");
    assert!(!stdout.contains("not reached"), "{stdout}");
    assert!(stdout.contains("test_skip ... skipped\n    no display"), "{stdout}");
    assert!(stdout.contains("test_unwrap ... FAILED\n    panicked at lib_test.tarn:20:5: called `unwrap` on `None`"), "{stdout}");
    assert!(stdout.contains("1 passed; 3 failed; 1 skipped"), "{stdout}");
    let output = Command::new(env!("CARGO_BIN_EXE_tarn")).current_dir(&dir).args(["test", ".", "--json", "--filter", "skip"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("\"status\":\"skipped\"") && stdout.contains("\"skip_reason\":\"no display\""), "{stdout}");
    assert!(stdout.contains("\"skipped\":1"), "{stdout}");
    std::fs::remove_dir_all(dir).unwrap();
}
