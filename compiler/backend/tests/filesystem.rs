//! Real temporary files, explicit error paths and native resource traces.
use std::{
    collections::HashSet,
    path::PathBuf,
    process::{Command, Output},
};

fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-fs-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, src.replace("ROOT", dir.to_str().unwrap())).unwrap();
    let result = tarn_driver::check(&entry).unwrap();
    assert!(
        !result.has_errors(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.render(&result.program.sources))
            .collect::<String>()
    );
    (dir, result)
}
fn build(dir: &std::path::Path, res: &tarn_driver::CheckResult) -> PathBuf {
    let exe = dir.join("program");
    tarn_backend::build(
        res.drops.as_ref().unwrap(),
        res.typed.as_ref().unwrap(),
        &exe,
    )
    .unwrap();
    exe
}
fn run(exe: &PathBuf) -> Output {
    Command::new("timeout")
        .arg("30s")
        .arg(exe)
        .env("TARN_TRACE_FS", "1")
        .output()
        .unwrap()
}
fn balanced(out: &Output) -> usize {
    let mut live = HashSet::new();
    let mut count = 0;
    for line in String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|s| s.starts_with("fs:"))
    {
        let parts: Vec<_> = line.split(':').collect();
        let fd: i32 = parts[2].parse().unwrap();
        match parts[1] {
            "open" | "dir_open" => {
                assert!(live.insert(fd), "duplicate owner: {line}");
                count += 1;
            }
            "close" | "dir_close" => {
                assert!(live.remove(&fd), "double close: {line}");
            }
            _ => panic!("unknown trace: {line}"),
        }
    }
    assert!(live.is_empty(), "descriptor leak: {live:?}");
    count
}
fn success(out: &Output, expected: &str) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .all(|s| s.starts_with("fs:"))
    );
    balanced(out);
}
#[test]
fn real_file_io_seek_append_metadata_and_text() {
    let src = r#"import "fs"
import "io"
fn main() Result<void, io.Error> {
    path := "ROOT/data/text"
    var file = try fs.File.create_new(&path)
    print(try file.write(&[3]u8{65, 0, 66}))
    print(try file.seek(fs.SeekFrom.Start(u64(0))))
    var data = [3]u8{0, 0, 0}
    try file.read_exact(&mut data)
    print(data[1])
    print((try file.read(&mut data)) == usize(0))
    print(try file.seek(fs.SeekFrom.End(i64(-1))))
    print(try file.seek(fs.SeekFrom.Current(i64(-1))))
    print((try file.metadata()).len)
    try file.sync_all()
    try file.close()
    var append = try fs.File.append(&path)
    try append.write_all(&[1]u8{67})
    try append.close()
    bytes := try fs.read(&path)
    print(bytes.len())
    print(bytes.at(usize(3)))
    try fs.write_text(&path, &"café🙂")
    print(try fs.read_text(&path))
    print((try fs.metadata(&path)).is_file)
    print((try fs.metadata(&"ROOT/data")).is_dir)
    try fs.rename(&path, &"ROOT/data/renamed")
    print(try fs.exists(&path))
    try fs.remove_file(&"ROOT/data/renamed")
    return Ok(())
}"#;
    let (dir, res) = checked(src, "io");
    let exe = build(&dir, &res);
    success(
        &run(&exe),
        "3\n0\n0\ntrue\n2\n1\n3\n4\n67\ncafé🙂\ntrue\ntrue\nfalse\n",
    );
    assert!(!dir.join("data/renamed").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn large_empty_and_invalid_utf8_files_and_owned_directory_entries() {
    let src = r#"import "fs"
import "io"
fn main() Result<void, io.Error> {
    bytes := try fs.read(&"ROOT/data/large")
    print(bytes.len())
    print(bytes.at(usize(65536)))
    try fs.write(&"ROOT/data/copy", bytes.as_slice())
    print((try fs.read_text(&"ROOT/data/empty")).len())
    match fs.read_text(&"ROOT/data/invalid") {
        Err(error) => { match error.kind { io.ErrorKind.InvalidData => { print("invalid") }
            _ => { panic("wrong UTF-8 error") } } }
        Ok(text) => { panic("accepted invalid text") }
    }
    entries := try fs.read_dir(&"ROOT/data")
    print(entries.len())
    for entry in entries.as_slice() { try entry.metadata()
        print(entry.name().is_empty()) }
    try fs.create_dir(&"ROOT/new")
    try fs.remove_dir(&"ROOT/new")
    return Ok(())
}"#;
    let (dir, res) = checked(src, "large");
    let data: Vec<u8> = (0..131_073).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("data/large"), &data).unwrap();
    std::fs::write(dir.join("data/empty"), []).unwrap();
    std::fs::write(dir.join("data/invalid"), [0xff]).unwrap();
    let exe = build(&dir, &res);
    success(
        &run(&exe),
        "131073\n25\n0\ninvalid\n4\nfalse\nfalse\nfalse\nfalse\n",
    );
    assert_eq!(std::fs::read(dir.join("data/copy")).unwrap(), data);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn automatic_close_covers_control_flow_moves_overwrite_self_assignment_try_and_tasks() {
    let src = r#"import "fs"
import "io"
fn early() Result<void, io.Error> { file := try fs.File.open(&"ROOT/data/file")
 return Ok(()) }
fn conditional(flag bool) Result<void, io.Error> { var file: fs.File
 if flag { file = try fs.File.open(&"ROOT/data/file") }
 return Ok(()) }
fn failed() Result<void, io.Error> { file := try fs.File.open(&"ROOT/data/file")
 try fs.read(&"ROOT/missing")
 return Ok(()) }
fn main() Result<void, io.Error> {
 try early()
 try conditional(true)
 try conditional(false)
 for { file := try fs.File.open(&"ROOT/data/file")
 break }
 for i in 0..4 { file := try fs.File.open(&"ROOT/data/file")
 continue }
 { var file = try fs.File.open(&"ROOT/data/file")
 file = try fs.File.open(&"ROOT/data/file")
 file = file
 moved := file }
 match failed() { Err(error) => { print("error cleaned") }
 Ok(value) => { panic("missing error") } }
 file := try fs.File.open(&"ROOT/data/file")
 task := spawn move fn() fs.File { return file }
 returned := task.join()
 print((try returned.metadata()).len)
 return Ok(())
}"#;
    let (dir, res) = checked(src, "drops");
    std::fs::write(dir.join("data/file"), b"owned").unwrap();
    let exe = build(&dir, &res);
    let out = run(&exe);
    success(&out, "error cleaned\n5\n");
    assert_eq!(balanced(&out), 11);
    let trace: Vec<_> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(trace.len(), 22);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn filesystem_errors_preserve_categories_and_invalid_paths_do_not_truncate() {
    let src = r#"import "fs"
import "io"
fn category(kind io.ErrorKind) i32 {
 match kind {
 io.ErrorKind.NotFound => { return 1 }
 io.ErrorKind.AlreadyExists => { return 2 }
 io.ErrorKind.IsDirectory => { return 3 }
 io.ErrorKind.NotDirectory => { return 4 }
 io.ErrorKind.InvalidInput => { return 5 }
 io.ErrorKind.UnexpectedEof => { return 6 }
 _ => { return 0 }
 }
}
fn expect(result Result<fs.File, io.Error>, kind io.ErrorKind) {
 match result { Err(error) => { print(category(error.kind) == category(kind)) }
 Ok(file) => { panic("expected failure") } }
}
fn main() Result<void, io.Error> {
 expect(fs.File.open(&"ROOT/missing"), io.ErrorKind.NotFound)
 expect(fs.File.create_new(&"ROOT/data/existing"), io.ErrorKind.AlreadyExists)
 expect(fs.File.open(&"ROOT/data"), io.ErrorKind.IsDirectory)
 expect(fs.File.open(&"ROOT/data/existing/child"), io.ErrorKind.NotDirectory)
 expect(fs.File.open(&"ROOT/data/fifo"), io.ErrorKind.InvalidInput)
 raw := [2]u8{65, 0}
 match string_from_utf8(&raw) {
 Some(path) => { expect(fs.File.create(&path), io.ErrorKind.InvalidInput) }
 None => { panic("valid UTF-8 with NUL") } }
 var file = try fs.File.open(&"ROOT/data/existing")
 var data = [8]u8{0, 0, 0, 0, 0, 0, 0, 0}
 match file.read_exact(&mut data) { Err(error) => { print(category(error.kind) == i32(6)) }
 Ok(value) => { panic("short file") } }
 maximum: u64 := 18446744073709551615
 match file.seek(fs.SeekFrom.Start(maximum)) { Err(error) => { print(category(error.kind) == i32(5)) }
 Ok(value) => { panic("invalid seek") } }
 return Ok(())
}"#;
    let (dir, res) = checked(src, "errors");
    std::fs::write(dir.join("data/existing"), b"safe").unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(dir.join("data/fifo"))
            .status()
            .unwrap()
            .success()
    );
    let exe = build(&dir, &res);
    success(
        &run(&exe),
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n",
    );
    assert_eq!(std::fs::read(dir.join("data/existing")).unwrap(), b"safe");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
#[cfg_attr(target_os = "macos", ignore = "APFS refuses to create the non-UTF-8 name this test needs")]
fn invalid_directory_names_fail_without_leaking_the_cursor() {
    use std::os::unix::ffi::OsStringExt;
    let src = "import \"fs\"\nimport \"io\"\nfn main() Result<void, io.Error> { entries := try fs.read_dir(&\"ROOT/data\")\n return Ok(()) }";
    let (dir, res) = checked(src, "invalid-name");
    std::fs::write(
        dir.join("data")
            .join(std::ffi::OsString::from_vec(vec![0xff])),
        [],
    )
    .unwrap();
    let exe = build(&dir, &res);
    let out = run(&exe);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("I/O error: invalid data"));
    assert_eq!(balanced(&out), 1);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn resource_layout_and_capability_corruption_are_rejected() {
    let (dir, mut res) = checked(
        "import \"fs\"\nimport \"io\"\nfn main() Result<void, io.Error> { file := try fs.File.open(&\"ROOT/data/file\")\n return Ok(()) }",
        "abi",
    );
    let drops = res.drops.as_ref().unwrap();
    let typed = res.typed.as_mut().unwrap();
    let file = typed.decls.fs_file.unwrap();
    typed
        .decls
        .native_capabilities
        .get_mut(&file)
        .unwrap()
        .share = true;
    assert!(
        tarn_backend::emit_object(drops, typed)
            .unwrap_err()
            .to_string()
            .contains("filesystem declaration ABI")
    );
    typed
        .decls
        .native_capabilities
        .get_mut(&file)
        .unwrap()
        .share = false;
    typed.decls.structs.get_mut(&file).unwrap().is_copy = true;
    assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
    typed.decls.structs.get_mut(&file).unwrap().is_copy = false;
    typed.decls.structs.get_mut(&file).unwrap().fields[0].is_pub = true;
    assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
    typed.decls.structs.get_mut(&file).unwrap().fields[0].is_pub = false;
    typed.decls.fs_intrinsics.remove("fs._read");
    assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

fn fault_binary(src: &str, tag: &str) -> (PathBuf, PathBuf) {
    let (dir, res) = checked(src, tag);
    let object = dir.join("program.o");
    std::fs::write(
        &object,
        tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap())
            .unwrap(),
    )
    .unwrap();
    let exe = dir.join("program");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("cc")
        .args([
            "-std=c11",
            "-O0",
            "-fno-strict-aliasing",
            if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" },
            "-pthread",
        ])
        .arg(object)
        .arg(root.join("../../runtime/native.c"))
        .arg(root.join("tests/filesystem_faults.c"))
        .args([
            "-Wl,--wrap=open",
            "-Wl,--wrap=read",
            "-Wl,--wrap=write",
            "-Wl,--wrap=close",
            "-lm",
            "-o",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, exe)
}
#[test]
fn eintr_partial_io_zero_progress_permission_and_close_consumption() {
    let src = r#"import "fs"
import "io"
fn main() Result<void, io.Error> {
    var file = try fs.File.create(&"ROOT/data/fixture")
    try file.write_all(&[8]u8{65, 66, 67, 68, 69, 70, 71, 72})
    try file.seek(fs.SeekFrom.Start(u64(0)))
    var data = [8]u8{0, 0, 0, 0, 0, 0, 0, 0}
    try file.read_exact(&mut data)
    print(data[7])
    try file.close()
    return Ok(())
}"#;
    let (dir, exe) = fault_binary(src, "faults");
    success(&run(&exe), "72\n");
    for (variable, label) in [
        ("TARN_FS_ZERO_WRITE", "write made no progress"),
        ("TARN_FS_CLOSE_EINTR", "other OS error"),
    ] {
        let out = Command::new("timeout")
            .arg("30s")
            .arg(&exe)
            .env("TARN_TRACE_FS", "1")
            .env(variable, "1")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains(label));
        assert_eq!(balanced(&out), 1);
    }
    std::fs::remove_dir_all(dir).unwrap();
    let (dir, exe) = fault_binary(
        "import \"fs\"\nimport \"io\"\nfn main() Result<void, io.Error> { file := try fs.File.open(&\"ROOT/denied\")\n return Ok(()) }",
        "permission",
    );
    let out = run(&exe);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("I/O error: permission denied (native code 13)")
    );
    assert_eq!(balanced(&out), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn previously_unhandled_io_error_kinds_exit_normally() {
    for (kind, label) in [
        ("UnexpectedEof", "unexpected EOF"),
        ("LimitExceeded", "limit exceeded"),
        ("NotFound", "not found"),
        ("InvalidData", "invalid data"),
        ("StorageFull", "storage full"),
    ] {
        let source = format!(
            "import \"io\"\nfn main() Result<void, io.Error> {{ return Err(io.Error.new(io.ErrorKind.{kind})) }}"
        );
        let (dir, res) = checked(&source, kind);
        let out = run(&build(&dir, &res));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("I/O error: {label} (native code 0)\n")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn private_files_and_directories_get_exact_modes_at_creation() {
    // ADR 0075: the mode is applied by the creating syscall itself, under the
    // common umask 022 that turned plain create/create_dir into 0644/0755.
    use std::os::unix::fs::PermissionsExt;
    let src = r#"import "io"
import "fs"
fn kind(result Result<fs.File, io.Error>) string {
    match result {
        Ok(file) => { return "created" }
        Err(error) => {
            if error.kind == io.ErrorKind.AlreadyExists { return "exists" }
            if error.kind == io.ErrorKind.InvalidInput { return "invalid" }
            return "other"
        }
    }
}
fn main() Result<void, io.Error> {
    try fs.create_dir_with_mode(&"ROOT/data/private", 0o700)
    var capture = try fs.File.create_new_with_mode(&"ROOT/data/private/capture.png", 0o600)
    try capture.write_all("pixels".bytes())
    try capture.close()
    print(kind(fs.File.create_new_with_mode(&"ROOT/data/private/capture.png", 0o600)))
    print(kind(fs.File.create_new_with_mode(&"ROOT/data/private/setuid", 0o4700)))
    match fs.create_dir_with_mode(&"ROOT/data/private", 0o700) {
        Ok(value) => { print("created") }
        Err(error) => { print(error.kind == io.ErrorKind.AlreadyExists) }
    }
    match fs.create_dir_with_mode(&"ROOT/data/sticky", 0o1777) {
        Ok(value) => { print("created") }
        Err(error) => { print(error.kind == io.ErrorKind.InvalidInput) }
    }
    try fs.File.create_new_with_mode(&"ROOT/data/shared", 0o666)
    try fs.create_dir(&"ROOT/data/plain")
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "modes");
    let exe = build(&dir, &res);
    let out = Command::new("sh")
        .args(["-c", "umask 022 && exec timeout 30s \"$0\""])
        .arg(&exe)
        .env("TARN_TRACE_FS", "1")
        .output()
        .unwrap();
    success(&out, "exists\ninvalid\ntrue\ntrue\n");
    let mode = |path: &str| std::fs::metadata(dir.join(path)).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode("data/private"), 0o700);
    assert_eq!(mode("data/private/capture.png"), 0o600);
    assert_eq!(std::fs::read(dir.join("data/private/capture.png")).unwrap(), b"pixels");
    // Rejected modes create nothing; the umask still removes bits as usual.
    assert!(!dir.join("data/private/setuid").exists() && !dir.join("data/sticky").exists());
    assert_eq!(mode("data/shared"), 0o644);
    assert_eq!(mode("data/plain"), 0o755);
    std::fs::remove_dir_all(dir).unwrap();
}
