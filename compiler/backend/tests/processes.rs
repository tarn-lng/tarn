//! Real child processes, dual-pipe capture, owned completion and private ABI.
use std::{
    collections::HashSet,
    path::PathBuf,
    process::{Command, Output},
};

fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-process-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(dir.join("data")).unwrap();
    // The children report getcwd, which resolves macOS's /var -> /private/var.
    let dir = dir.canonicalize().unwrap();
    let helper = dir.join("child");
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/process_child.c"))
        .arg("-o")
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let src = src.replace("HELPER", helper.to_str().unwrap());
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
    let input = exe.parent().unwrap().join("parent-stdin");
    std::fs::write(&input, "parent input").unwrap();
    Command::new("timeout")
        .arg("30s")
        .arg(exe)
        .env("TARN_TRACE_PROCESS", "1")
        .env("TARN_PROCESS_TEST_ENV", "inherited")
        .stdin(std::fs::File::open(input).unwrap())
        .output()
        .unwrap()
}
fn balanced(out: &Output) -> usize {
    let mut children = HashSet::new();
    let mut waiting = HashSet::new();
    let mut live = HashSet::new();
    let mut count = 0;
    for line in String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|s| s.starts_with("process:"))
    {
        let parts: Vec<_> = line.split(':').collect();
        let fd: i32 = parts[2].parse().unwrap();
        match parts[1] {
            "fd_open" => {
                assert!(live.insert(fd), "duplicate owner: {line}");
                count += 1;
            }
            "fd_close" => {
                assert!(live.remove(&fd), "double close: {line}");
            }
            "spawn" => {
                assert!(children.insert(fd), "duplicate child: {line}");
            }
            "wait" => {
                assert!(
                    children.contains(&fd) && waiting.insert(fd),
                    "double/unowned wait: {line}"
                );
            }
            "reap" => {
                assert!(
                    children.remove(&fd) && waiting.remove(&fd),
                    "double/unowned reap: {line}"
                );
            }
            _ => panic!("unknown trace: {line}"),
        }
    }
    assert!(waiting.is_empty(), "unfinished waits: {waiting:?}");
    assert!(children.is_empty(), "unreaped children: {children:?}");
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
            .all(|s| s.starts_with("process:"))
    );
    balanced(out);
}
#[test]
fn arguments_environment_working_directory_and_status_are_application_level() {
    let (dir, res) = checked(
        r#"import "process"
import "io"
fn main() Result<void, io.Error> {
 args := [3]string{"space arg", ";$(literal)", "日本"}
 print((try process.Command.new("/bin/sh").arg("-c").arg("read value; test \"$value\" = \"parent input\"").status()).success())
 command := process.Command.new("HELPER").arg("args").args(&args)
 output := try command.output()
 print((try output.stdout_text()) == "space arg\n;$(literal)\n日本\n")
 print(output.status.success())
 print(output.stderr.len())
 before := try process.Command.new("HELPER").arg("cwd").output()
 cwd := try process.Command.new("HELPER").arg("cwd").current_dir("ROOT/data").output()
 print((try cwd.stdout_text()) == "ROOT/data\n")
 after := try process.Command.new("HELPER").arg("cwd").output()
 print((try before.stdout_text()) == (try after.stdout_text()))
 env := try process.Command.new("HELPER").arg("env").output()
 print((try env.stdout_text()) == "inherited\n")
 input := try process.Command.new("HELPER").arg("stdin").output()
 print((try input.stdout_text()) == "eof\n")
 reusable := process.Command.new("true")
 print((try reusable.status()).success())
 print((try reusable.status()).success())
 print((try process.Command.new("/usr/bin/false").status()).success())
 status := try process.Command.new("/bin/sh").arg("-c").arg("kill -TERM $$").status()
 match status.signal() { Some(value) => { print(value) }
 None => { panic("expected signal") } }
 var child = try process.Command.new("HELPER").arg("pause").start()
 print(child.id() > u32(0))
 try child.kill()
 match (try child.wait()).signal() { Some(value) => { print(value) }
 None => { panic("expected kill") } }
 return Ok(())
}
"#,
        "application",
    );
    success(
        &run(&build(&dir, &res)),
        "true\ntrue\ntrue\n0\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\nfalse\n15\ntrue\n9\n",
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn large_dual_output_and_binary_capture_do_not_deadlock_or_decode_lossily() {
    let (dir, res) = checked(
        r#"import "process"
import "io"
fn main() Result<void, io.Error> {
 output := try process.Command.new("HELPER").arg("large").output()
 print(output.stdout.len())
 print(output.stderr.len())
 print(output.stdout.get(usize(0)))
 print(output.stderr.get(usize(131072)))
 match output.status.code() { Some(value) => { print(value) }
 None => { panic("expected exit") } }
 binary := try process.Command.new("HELPER").arg("binary").output()
 print(binary.stdout.len())
 print(binary.stderr.len())
 print((try binary.stderr_text()).len())
 match binary.stdout_text() { Err(error) => { match error.kind { io.ErrorKind.InvalidData => { print("invalid UTF-8") }
 _ => { panic("wrong error") } } }
  Ok(text) => { panic("lossy conversion") } }
 return Ok(())
}
"#,
        "capture",
    );
    success(
        &run(&build(&dir, &res)),
        "131073\n131073\n111\n101\n7\n3\n1\n1\ninvalid UTF-8\n",
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn acquisition_errors_preserve_categories_and_close_all_temporary_fds() {
    let (dir, res) = checked(
        r#"import "process"
import "io"
fn check(command process.Command) {
 match command.output() { Err(error) => { print(error.native_code())
  match error.kind { io.ErrorKind.NotFound => { print("missing") }
   io.ErrorKind.InvalidInput => { print("invalid") }
   io.ErrorKind.InvalidData => { print("format") }
   io.ErrorKind.PermissionDenied => { print("denied") }
   _ => { panic("unexpected error") } } }
  Ok(output) => { panic("unexpected process") } }
}
fn main() {
 check(process.Command.new("/tarn-missing-process-15d"))
 check(process.Command.new(""))
 check(process.Command.new("/usr/bin/true\0ignored"))
 check(process.Command.new("/usr/bin/true").arg("bad\0arg"))
 check(process.Command.new("/usr/bin/true").current_dir(""))
 check(process.Command.new("/usr/bin/true").current_dir("ROOT/missing"))
 check(process.Command.new("ROOT/data/not-a-binary"))
 check(process.Command.new("ROOT/data/not-executable"))
}
"#,
        "errors",
    );
    use std::os::unix::fs::PermissionsExt;
    for (name, mode) in [("not-a-binary", 0o755), ("not-executable", 0o644)] {
        let file = dir.join("data").join(name);
        std::fs::write(&file, "echo implicit-shell-must-not-run\n").unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    success(
        &run(&build(&dir, &res)),
        "2\nmissing\n22\ninvalid\n22\ninvalid\n22\ninvalid\n22\ninvalid\n2\nmissing\n8\nformat\n13\ndenied\n",
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn completion_on_all_normal_exits_and_task_transfer_is_exactly_once() {
    let (dir, res) = checked(
        r#"import "process"
import "io"
fn child() process.Process {
 match process.Command.new("/usr/bin/true").start() { Ok(value) => { return value }
  Err(error) => { panic("spawn failed") } }
}
fn early() { p := child()
 return }
fn failure() Result<void, io.Error> { p := child()
 try process.Command.new("/missing-tarn-child").status()
 return Ok(()) }
fn main() {
 { p := child() }
 early()
 for { p := child()
  break }
 var count = 0
 for count < 2 { p := child()
  count = count + 1
  continue }
 for condition in &[2]bool{true, false} { var p: process.Process
  if condition { p = child() } }
 { a := child()
  b := a
  b.wait() }
 { var p = child()
  p = child()
  p = p }
 failure()
 t := spawn move fn() process.Process { return child() }
 returned := t.join()
 returned.wait()
 p := child()
 worker := spawn move fn() { p.wait() }
 worker.join()
 scope { spawn fn() { process.Command.new("HELPER").arg("large").output() }
  spawn fn() { process.Command.new("HELPER").arg("large").output() } }
 print("complete")
}
"#,
        "completion",
    );
    let output = run(&build(&dir, &res));
    success(&output, "complete\n");
    let trace = String::from_utf8(output.stderr.clone()).unwrap();
    let wait = trace
        .lines()
        .find(|line| line.starts_with("process:wait:"))
        .unwrap();
    let mut duplicated = output.clone();
    duplicated
        .stderr
        .extend_from_slice(format!("{wait}\n").as_bytes());
    assert!(std::panic::catch_unwind(|| balanced(&duplicated)).is_err());
    let mut missing = output.clone();
    missing.stderr = trace
        .lines()
        .filter(|line| *line != wait)
        .map(|line| format!("{line}\n"))
        .collect::<String>()
        .into_bytes();
    assert!(std::panic::catch_unwind(|| balanced(&missing)).is_err());

    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("process:spawn:"))
            .count(),
        14
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupted_resource_and_intrinsic_contracts_are_rejected() {
    let (dir, mut res) = checked(
        "import \"process\"\nimport \"fs\"\nfn observe(f &fs.File) { f.metadata() }\nfn main() { process.Command.new(\"/usr/bin/true\").status() }",
        "abi",
    );
    let drops = res.drops.as_ref().unwrap();
    let typed = res.typed.as_mut().unwrap();
    for owner in [
        typed.decls.process_owner.unwrap(),
        typed.decls.process_pipe.unwrap(),
    ] {
        typed
            .decls
            .native_capabilities
            .get_mut(&owner)
            .unwrap()
            .share = true;
        assert!(
            tarn_backend::emit_object(drops, typed)
                .unwrap_err()
                .to_string()
                .contains("process declaration ABI")
        );
        typed
            .decls
            .native_capabilities
            .get_mut(&owner)
            .unwrap()
            .share = false;
        typed.decls.structs.get_mut(&owner).unwrap().is_copy = true;
        assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
        typed.decls.structs.get_mut(&owner).unwrap().is_copy = false;
        typed.decls.structs.get_mut(&owner).unwrap().fields[0].is_pub = true;
        assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
        typed.decls.structs.get_mut(&owner).unwrap().fields[0].is_pub = false;
    }
    let pipe = typed.decls.process_pipe.unwrap();
    // Both resources have scalar storage; identities must still be tied to
    // consuming signatures rather than accepted just for their field layout.
    typed.decls.process_pipe = Some(typed.decls.fs_file.expect("same-shaped filesystem owner"));
    assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
    typed.decls.process_pipe = Some(pipe);
    typed.decls.process_intrinsics.remove("process._wait");
    assert!(!tarn_ir::post_drop::verify(drops, typed).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn simultaneous_native_workers_capture_large_children_deterministically() {
    let (dir, res) = checked(
        r#"import "process"
import "io"
fn main() {
 var workers: Vec<Task<usize>> = Vec.new()
 var index: usize = 0
 for index < usize(16) {
  worker := spawn move fn() usize {
   match process.Command.new("HELPER").arg("large").output() { Ok(output) => { return output.stdout.len() }
    Err(error) => { panic("capture failed") } }
  }
  workers.push(worker)
  index = index + 1
 }
 var total: usize = 0
 for !workers.is_empty() { match workers.pop() { Some(worker) => { total = total + worker.join() }
  None => { panic("worker count invariant") } } }
 print(total)
}
"#,
        "stress",
    );
    success(&run(&build(&dir, &res)), "2097168\n");
    std::fs::remove_dir_all(dir).unwrap();
}

fn link_faults(dir: &std::path::Path, res: &tarn_driver::CheckResult) -> PathBuf {
    let object = dir.join("program.o");
    std::fs::write(
        &object,
        tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap())
            .unwrap(),
    )
    .unwrap();
    let exe = dir.join("program");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("cc")
        .args([
            "-std=c11",
            "-O0",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fno-strict-aliasing",
            if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" },
            "-pthread",
        ])
        .arg(object)
        .arg(root.join("../../runtime/native.c"))
        .arg(root.join("tests/process_faults.c"))
        .args([
            if cfg!(target_os = "macos") { "-Wl,--wrap=pipe" } else { "-Wl,--wrap=pipe2" },
            "-Wl,--wrap=fcntl",
            "-Wl,--wrap=read",
            "-Wl,--wrap=waitpid",
            "-Wl,--wrap=close",
            if cfg!(target_os = "macos") { "-Wl,--wrap=posix_spawn" } else { "-Wl,--wrap=posix_spawnp" },
            "-lm",
            "-o",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    exe
}

#[test]
fn interrupted_partial_reads_and_acquisition_failures_preserve_cleanup() {
    let (dir, res) = checked(
        "import \"process\"\nimport \"io\"\nfn main() Result<void, io.Error> { try process.Command.new(\"/denied-process\").output()\n return Ok(()) }",
        "permission",
    );
    let output = run(&link_faults(&dir, &res));
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("permission denied"));
    balanced(&output);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn private_runtime_never_leaks_fds_or_inherits_unrelated_descriptors() {
    let (dir, _) = checked("fn main() {}", "runtime");
    let source = dir.join("runtime.c");
    std::fs::write(
        &source,
        format!(
            "{}\n{}",
            include_str!("../../../runtime/native.c"),
            include_str!("process_runtime.c")
        ),
    )
    .unwrap();
    let exe = dir.join("runtime-test");
    let output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-pthread"])
        .arg(source)
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/process_faults.c"))
        .args([
            if cfg!(target_os = "macos") { "-Wl,--wrap=pipe" } else { "-Wl,--wrap=pipe2" },
            "-Wl,--wrap=fcntl",
            "-Wl,--wrap=read",
            "-Wl,--wrap=waitpid",
            "-Wl,--wrap=close",
            if cfg!(target_os = "macos") { "-Wl,--wrap=posix_spawn" } else { "-Wl,--wrap=posix_spawnp" },
            "-lm",
            "-o",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("timeout")
        .arg("30s")
        .arg(exe)
        .arg(dir.join("child"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn unix_pair_handoff_null_stdio_and_bounded_waits() {
    // ADR 0070: a socketpair end becomes the child's fd 3, its stdout/stderr
    // go to /dev/null, and completion is observed with and without timeouts
    // without reaping before the owning wait.
    let src = r#"import "io"
import "net"
import "process"
import "string"
fn describe(result Option<process.ExitStatus>) string {
    match result {
        None => { return "running" }
        Some(status) => {
            match status.code() { Some(code) => { return "exited " + &string.from_i64(i64(code)) }
                None => {} }
            match status.signal() { Some(signal) => { return "signaled " + &string.from_i64(i64(signal)) }
                None => { return "?" } }
        }
    }
}
fn main() Result<void, io.Error> {
    pair := try net.UnixStream.pair()
    var parent = pair.first
    var child = try process.Command.new("HELPER").arg("channel").stdout(process.Stdio.Null).stderr(process.Stdio.Null).pass(pair.second).start()
    try parent.write_all(&[4]u8{1, 2, 3, 4})
    var echoed = [4]u8{0, 0, 0, 0}
    var offset = usize(0)
    for offset < usize(4) { offset = offset + try parent.read(&mut echoed[offset..]) }
    print(echoed[3])
    print(describe(try child.wait_timeout(10000)))
    print((try child.wait()).success())
    // The child exited and the Command was destroyed: no writer is left.
    print(try parent.read(&mut echoed))
    var sleeper = try process.Command.new("HELPER").arg("pause").start()
    print(describe(try sleeper.try_wait()))
    print(describe(try sleeper.wait_timeout(50)))
    try sleeper.kill()
    print(describe(try sleeper.wait_timeout(10000)))
    print(describe(try sleeper.try_wait()))
    print(describe(Some(try sleeper.wait())))
    var quick = try process.Command.new("HELPER").arg("exit").arg("3").start()
    print(describe(try quick.wait_timeout(10000)))
    match quick.wait_timeout(-1) {
        Ok(result) => { print("accepted") }
        Err(error) => { print(error.kind == io.ErrorKind.InvalidInput) }
    }
    print(describe(Some(try quick.wait())))
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "handoff");
    let exe = build(&dir, &res);
    let out = run(&exe);
    success(&out, "4\nexited 0\ntrue\n0\nrunning\nrunning\nsignaled 9\nsignaled 9\nsignaled 9\nexited 3\ntrue\nexited 3\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn handoff_limits_and_inherited_stdio_are_explicit() {
    let src = r#"import "io"
import "net"
import "process"
fn main() Result<void, io.Error> {
    first := try net.UnixStream.pair()
    var command = process.Command.new("HELPER").arg("args").arg("visible").pass(first.second)
    var index = 1
    for index < 65 {
        pair := try net.UnixStream.pair()
        command = command.pass(pair.second)
        index = index + 1
    }
    var rejected = false
    match command.start() {
        Ok(child) => {}
        Err(error) => { rejected = error.kind == io.ErrorKind.InvalidInput }
    }
    status := try process.Command.new("HELPER").arg("args").arg("inherited").stdout(process.Stdio.Null).stdout(process.Stdio.Inherit).status()
    print(rejected)
    print(status.success())
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "handoff-limits");
    let exe = build(&dir, &res);
    let out = run(&exe);
    success(&out, "inherited\ntrue\ntrue\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn earlier_prints_reach_inherited_stdout_before_the_child() {
    let src = r#"import "io"
import "process"
fn main() Result<void, io.Error> {
    print("before")
    try process.Command.new("HELPER").arg("args").arg("child").status()
    print("after")
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "flush-order");
    let exe = build(&dir, &res);
    // stdout is a pipe here, so C stdio buffers fully: the old order was child first.
    success(&run(&exe), "before\nchild\nafter\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn launch_consumes_handoffs_on_success_and_failure() {
    // ADR 0071: a Launch held in a variable still gives up the parent's copy
    // at launch, so the peer sees end of stream once the child exits; a
    // failed launch closes it too.
    let src = r#"import "io"
import "net"
import "process"
fn main() Result<void, io.Error> {
    pair := try net.UnixStream.pair()
    var parent = pair.first
    launch := process.Command.new("HELPER").arg("channel").pass(pair.second).stdout(process.Stdio.Null).stderr(process.Stdio.Null)
    child := try launch.start()
    try parent.write_all(&[4]u8{1, 2, 3, 4})
    var echoed = [4]u8{0, 0, 0, 0}
    var offset = usize(0)
    for offset < usize(4) { offset = offset + try parent.read(&mut echoed[offset..]) }
    print((try child.wait()).success())
    print(try parent.read(&mut echoed))
    failed := try net.UnixStream.pair()
    var peer = failed.first
    match process.Command.new("ROOT/missing-program").pass(failed.second).start() {
        Ok(process) => { print("started") }
        Err(error) => { print(error.kind == io.ErrorKind.NotFound) }
    }
    print(try peer.read(&mut echoed))
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "launch");
    let exe = build(&dir, &res);
    success(&run(&exe), "true\n0\ntrue\n0\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn a_launch_cannot_be_reused_after_it_starts() {
    let dir = std::env::temp_dir().join(format!("tarn-process-{}-launch-reuse", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, r#"import "io"
import "net"
import "process"
fn main() Result<void, io.Error> {
    pair := try net.UnixStream.pair()
    launch := process.Command.new("/usr/bin/true").pass(pair.second)
    first := try launch.start()
    second := try launch.start()
    return Ok(())
}
"#).unwrap();
    let res = tarn_driver::check(&entry).unwrap();
    assert!(res.diagnostics.iter().any(|d| d.code == "E4001"), "{:?}", res.diagnostics.iter().map(|d| d.code.to_string()).collect::<Vec<_>>());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn piped_stdio_streams_closes_before_wait_and_reports_broken_pipe() {
    let src = r#"import "io"
import "net"
import "process"
import "string"
fn text(bytes Vec<u8>) string {
    match string.from_utf8(bytes.as_slice()) { Some(value) => { return value }
        None => { return "?" } }
}
fn watch(pipe Option<process.ChildStdout>) Result<void, io.Error> {
    match pipe {
        Some(value) => {
            var reader = value
            try reader.set_nonblocking(true)
            var poll = try net.Poll.new()
            token := try reader.register(&mut poll)
            var events = [1]net.Event{net.Event.empty()}
            print(try poll.wait(&mut events, 50))
            var buffer = [8]u8{0, 0, 0, 0, 0, 0, 0, 0}
            match reader.read(&mut buffer) {
                Ok(count) => { print("read") }
                Err(error) => { print(error.kind == io.ErrorKind.WouldBlock) }
            }
            try reader.deregister(&mut poll, token)
            return Ok(())
        }
        None => { return Err(io.Error.new(io.ErrorKind.InvalidInput)) }
    }
}
fn main() Result<void, io.Error> {
    // Streaming round trip: stdin closes, stdout and stderr drain.
    var child = try process.Command.new("HELPER").arg("echo-stdin").stdin(process.Stdio.Piped).stdout(process.Stdio.Piped).stderr(process.Stdio.Piped).start_child()
    var input = child.stdin
    match input { Some(pipe) => {
            var writer = pipe
            try writer.write_all("hello".bytes())
            writer.close()
        }
        None => { print("no stdin") } }
    child.stdin = None
    match child.stdout { Some(pipe) => { print(text(try pipe.read_all())) }
        None => { print("no stdout") } }
    child.stdout = None
    match child.stderr { Some(pipe) => { print(text(try pipe.read_all())) }
        None => { print("no stderr") } }
    child.stderr = None
    print((try child.wait()).success())
    // Destroying a Child whose stdin is still open must not deadlock.
    {
        reader := try process.Command.new("HELPER").arg("echo-stdin").stdin(process.Stdio.Piped).stdout(process.Stdio.Null).stderr(process.Stdio.Null).start_child()
    }
    print("destroyed")
    // A finished reader makes writes BrokenPipe instead of killing us.
    var gone = try process.Command.new("HELPER").arg("exit").arg("0").stdin(process.Stdio.Piped).start_child()
    var observed = try gone.process.wait_timeout(10000)
    match gone.stdin {
        Some(pipe) => {
            var writer = pipe
            match writer.write("late".bytes()) {
                Ok(count) => { print("written") }
                Err(error) => { print(error.kind == io.ErrorKind.BrokenPipe) }
            }
        }
        None => {}
    }
    gone.stdin = None
    print((try gone.wait()).success())
    // Piped streams require start_child.
    match process.Command.new("HELPER").arg("args").stdout(process.Stdio.Piped).start() {
        Ok(process) => { print("started") }
        Err(error) => { print(error.kind == io.ErrorKind.InvalidInput) }
    }
    // Readiness with a deadline on a silent child's stdout.
    var silent = try process.Command.new("HELPER").arg("pause").stdout(process.Stdio.Piped).start_child()
    watched := silent.stdout
    silent.stdout = None
    // Kill before propagating any readiness error: a paused child would
    // otherwise be awaited forever by destruction (ADR 0045).
    outcome := watch(watched)
    try silent.process.kill()
    try outcome
    print((try silent.wait()).success())
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "piped");
    let exe = build(&dir, &res);
    success(&run(&exe), "hello\ne\ntrue\ndestroyed\ntrue\ntrue\ntrue\n0\ntrue\nfalse\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn files_and_streams_share_handoff_numbering() {
    // ADR 0071: handoffs of either kind are numbered 3 + k by call order.
    let src = r#"import "io"
import "fs"
import "net"
import "process"
fn main() Result<void, io.Error> {
    log := try fs.File.create(&"ROOT/data/child.log")
    pair := try net.UnixStream.pair()
    var parent = pair.first
    status := try process.Command.new("HELPER").arg("write-fds").pass_file(log).pass(pair.second).status()
    print(status.success())
    var byte = [1]u8{0}
    print(try parent.read(&mut byte))
    print(byte[0])
    print(try fs.read_text(&"ROOT/data/child.log"))
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "pass-file");
    let exe = build(&dir, &res);
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n1\n115\nfile\n\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn communicate_drains_large_streams_without_deadlock() {
    // ADR 0072: 512 KiB through stdin while the child echoes it to stdout far
    // beyond the pipe capacity; sequential write-then-read would deadlock.
    let src = r#"import "io"
import "process"
fn main() Result<void, io.Error> {
    var input: Vec<u8> = Vec.new()
    var index = 0
    for index < 524288 {
        input.push(u8(index % 251))
        index = index + 1
    }
    child := try process.Command.new("HELPER").arg("echo-stdin").stdin(process.Stdio.Piped).stdout(process.Stdio.Piped).stderr(process.Stdio.Piped).start_child()
    output := try child.communicate(input)
    print(output.status.success())
    print(output.stdout.len())
    print(output.stdout.as_slice()[524287])
    print(try output.stderr_text())
    // A child that exits without reading its input is not an error.
    quick := try process.Command.new("HELPER").arg("exit").arg("4").stdin(process.Stdio.Piped).start_child()
    var large: Vec<u8> = Vec.new()
    index = 0
    for index < 262144 {
        large.push(1)
        index = index + 1
    }
    early := try quick.communicate(large)
    print(early.status.success())
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "communicate");
    let exe = build(&dir, &res);
    success(&run(&exe), "true\n524288\n199\ne\nfalse\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn async_pipes_and_completion_do_not_block_the_executor() {
    let src = r#"import "io"
import "process"
import "runtime"
async fn session() Result<usize, io.Error> {
    var child = try process.Command.new("HELPER").arg("echo-stdin").stdin(process.Stdio.Piped).stdout(process.Stdio.Piped).stderr(process.Stdio.Null).start_child()
    input := child.stdin
    child.stdin = None
    match input {
        Some(pipe) => {
            var writer = pipe
            try writer.set_nonblocking(true)
            message := "ping-pong"
            try await writer.write_all_async(message.bytes())
            writer.close()
        }
        None => {}
    }
    output := child.stdout
    child.stdout = None
    var total = usize(0)
    match output {
        Some(pipe) => {
            var reader = pipe
            try reader.set_nonblocking(true)
            var buffer = [4]u8{0, 0, 0, 0}
            for {
                count := try await reader.read_async(&mut buffer)
                if count == 0 { break }
                total = total + count
            }
        }
        None => {}
    }
    status := try await child.process.wait_async()
    if !status.success() { return Err(io.Error.new(io.ErrorKind.OtherOs)) }
    return Ok(total)
}
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var op = runtime.Operation.new(&execution, session())
    print(try (try execution.block_on(&mut op)))
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "async");
    let exe = build(&dir, &res);
    success(&run(&exe), "9\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn terminate_allows_graceful_exit_and_escalates_to_kill() {
    // ADR 0073: SIGTERM first; a child that ignores it is killed after a
    // bounded wait. Both remain owned and are reaped exactly once.
    let src = r#"import "io"
import "process"
fn ready(stdout Option<process.ChildStdout>) Result<void, io.Error> {
    match stdout {
        Some(pipe) => {
            var reader = pipe
            var bytes = [6]u8{0, 0, 0, 0, 0, 0}
            var offset = usize(0)
            for offset < usize(6) { offset = offset + try reader.read(&mut bytes[offset..]) }
            return Ok(())
        }
        None => { return Err(io.Error.new(io.ErrorKind.InvalidInput)) }
    }
}
fn stop(child process.Child) Result<string, io.Error> {
    var owned = child
    var how = "terminated"
    try owned.process.terminate()
    match try owned.process.wait_timeout(500) {
        Some(status) => {}
        None => {
            how = "killed"
            try owned.process.kill()
        }
    }
    status := try owned.wait()
    match status.code() { Some(code) => { if code == 42 { return Ok(how + " 42") } }
        None => {} }
    match status.signal() { Some(signal) => { if signal == 9 { return Ok(how + " 9") } }
        None => {} }
    return Ok(how + " ?")
}
fn main() Result<void, io.Error> {
    var graceful = try process.Command.new("HELPER").arg("graceful").stdout(process.Stdio.Piped).start_child()
    out := graceful.stdout
    graceful.stdout = None
    readiness := ready(out)
    print(try stop(graceful))
    try readiness
    var stubborn = try process.Command.new("HELPER").arg("stubborn").stdout(process.Stdio.Piped).start_child()
    pipe := stubborn.stdout
    stubborn.stdout = None
    waiting := ready(pipe)
    print(try stop(stubborn))
    try waiting
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "terminate");
    let exe = build(&dir, &res);
    success(&run(&exe), "terminated 42\nkilled 9\n");
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn reader_vanishing_mid_write_is_broken_pipe_not_sigpipe() {
    // Regression: a blocked pipe write whose reader exits returns a partial
    // count while Linux still raises SIGPIPE; it used to kill the parent.
    let src = r#"import "io"
import "process"
fn main() Result<void, io.Error> {
    var data: Vec<u8> = Vec.new()
    var index = 0
    for index < 1048576 {
        data.push(7)
        index = index + 1
    }
    var child = try process.Command.new("HELPER").arg("read-one-late").stdin(process.Stdio.Piped).start_child()
    input := child.stdin
    child.stdin = None
    match input {
        Some(pipe) => {
            var writer = pipe
            match writer.write_all(data.as_slice()) {
                Ok(value) => { print("written") }
                Err(error) => { print(error.kind == io.ErrorKind.BrokenPipe) }
            }
        }
        None => {}
    }
    print((try child.wait()).success())
    return Ok(())
}
"#;
    let (dir, res) = checked(src, "mid-write");
    let exe = build(&dir, &res);
    for _ in 0..3 {
        success(&run(&exe), "true\ntrue\n");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
