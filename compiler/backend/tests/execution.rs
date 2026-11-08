//! Manual suspended state and real native execution, before async syntax.
use std::{path::{Path, PathBuf}, process::{Command, Output}, collections::HashSet};

#[test]
fn executor_cached_owner_validation_rejects_switches_and_foreign_insertions() {
    let source = r#"import "runtime"
import "io"
fn main() Result<void, io.Error> {
    first := try runtime.Execution.new()
    second := try runtime.Execution.new()
    var executor = runtime.Executor.new()
    executor = executor.add(runtime.Operation.new(&first, move fn(waker &io.Waker) io.Progress<void> { return io.Progress.Pending }))
    try executor.turn(&first, 0)
    try executor.turn(&first, 0)
    match executor.turn(&second, 0) {
        Ok(_) => { panic("cached owner accepted another execution") }
        Err(_) => {}
    }
    try executor.turn(&first, 0)
    executor = executor.add(runtime.Operation.new(&second, move fn(waker &io.Waker) io.Progress<void> { return io.Progress.Pending }))
    match executor.turn(&first, 0) {
        Ok(_) => { panic("foreign insertion bypassed owner validation") }
        Err(_) => {}
    }
    print("ownership checks retained")
    return Ok(())
}
"#;
    let exe = compile(source, "executor-owner-cache");
    let out = run(&exe);
    balanced(&out);
    assert_eq!(out.stdout, b"ownership checks retained\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-exec-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn"); std::fs::write(&file, src).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(!result.has_errors(), "{}", result.diagnostics.iter().map(|d| d.render(&result.program.sources)).collect::<String>());
    (dir, result)
}
fn compile(src: &str, tag: &str) -> PathBuf {
    let (dir, result) = checked(src, tag); let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap(); exe
}
fn run(exe: &Path) -> Output {
    Command::new("timeout").arg("30s").arg(exe).env("TARN_TRACE_NET", "1").env("TARN_TRACE_DROPS", "1").output().unwrap()
}
fn balanced(output: &Output) {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut live = HashSet::new();
    for line in String::from_utf8_lossy(&output.stderr).lines().filter(|s| s.starts_with("net:")) {
        let parts: Vec<_> = line.split(':').collect(); let fd = parts[2].parse::<i32>().unwrap();
        if parts[1] == "open" { assert!(live.insert(fd)); } else { assert!(live.remove(&fd)); }
    }
    assert!(live.is_empty(), "fd leak: {live:?}");
}
#[test]
fn manual_read_accept_connect_udp_and_deterministic_executor_fairness() {
    for (tag, source, expected) in [
        ("io", include_str!("../../../tests/native/pass/suspended_io.tarn"), include_bytes!("../../../tests/native/pass/suspended_io.stdout").as_slice()),
        ("fairness", include_str!("../../../tests/native/pass/executor_turns.tarn"), include_bytes!("../../../tests/native/pass/executor_turns.stdout").as_slice()),
    ] {
        let exe = compile(source, tag); let out = run(&exe); balanced(&out); assert_eq!(out.stdout, expected);
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}
#[test]
fn pending_loans_result_ownership_and_execution_lifetime_remain_visible() {
    let prefix = "import \"runtime\"\nimport \"net\"\nfn bad(execution &runtime.Execution, stream &mut net.TcpStream) { var bytes = [2]u8{0, 0}\n var op = runtime.read_operation(execution, stream, &mut bytes)\n op.poll()\n";
    for (tag, tail, expected) in [
        ("mutate", "bytes[0] = 1\n}", Some("E4102")),
        ("move", "moved := bytes\n}", Some("E4104")),
        ("stream", "stream.set_nonblocking(false)\n}", Some("E4101")),
        ("released", "op.finish()\nbytes[0] = 1\n}", None),
    ] {
        let dir = std::env::temp_dir().join(format!("tarn-exec-loans-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap(); let file = dir.join("main.tarn"); std::fs::write(&file, format!("{prefix}{tail}")).unwrap();
        let result = tarn_driver::check(&file).unwrap();
        if let Some(code) = expected { assert!(result.diagnostics.iter().any(|d| d.code == code), "{tag}: {:?}", result.diagnostics.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>()); }
        else { assert!(!result.has_errors()); }
        std::fs::remove_dir_all(dir).unwrap();
    }
    let cases = [
        ("execution move", "import \"io\"\nimport \"runtime\"\nfn bad(owner runtime.Execution) { var calls = 0\n op := runtime.Operation.new(&owner, move fn(w &io.Waker) io.Progress<void> { calls = calls + 1\n w.wake()\n return io.Progress.Pending })\n moved := owner }", "E4103"),
        ("result twice", "import \"net\"\nfn bad(stream net.TcpStream) { other := stream\n stream.close() }", "E4001"),
        ("executor loan insertion", "import \"io\"\nimport \"runtime\"\nstruct C { value i32 }\nfn bad(owner &runtime.Execution) { var c = C{value: 0}\n r := &mut c\n executor := runtime.Executor.new().add(runtime.Operation.new(owner, move fn(w &io.Waker) io.Progress<void> { r.value = 1\n return io.Progress.Pending }))\n c.value = 2 }", "E4102"),
        ("escaped operation", "import \"io\"\nimport \"runtime\"\nimport \"net\"\nfn bad(owner &runtime.Execution, stream &mut net.TcpStream) runtime.Operation<Result<usize, io.Error>> { var bytes = [2]u8{0, 0}\n return runtime.read_operation(owner, stream, &mut bytes) }", "E4201"),
    ];
    for (tag, source, code) in cases {
        let dir = std::env::temp_dir().join(format!("tarn-exec-reject-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap(); let file = dir.join("main.tarn"); std::fs::write(&file, source).unwrap();
        let result = tarn_driver::check(&file).unwrap(); assert!(result.diagnostics.iter().any(|d| d.code == code), "{tag}: {:?}", result.diagnostics.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn completed_operation_cannot_transfer_a_second_owned_result_and_panic_aborts() {
    use std::os::unix::process::ExitStatusExt;
    let source = r#"import "io"
import "runtime"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var calls = 0
    var op = runtime.Operation.new(&execution, move fn(waker &io.Waker) io.Progress<string> {
        calls = calls + 1
        return io.Progress.Ready("owned-result")
    })
    match op.poll() { io.Progress.Ready(result) => print(&result)
        io.Progress.Pending => panic("unexpected Pending") }
    op.poll()
    return Ok(())
}"#;
    let exe = compile(source, "repoll"); let output = run(&exe); assert_eq!(output.status.signal(), Some(6));
    assert!(String::from_utf8_lossy(&output.stderr).contains("operation polled after completion"));
    assert_eq!(String::from_utf8_lossy(&output.stderr).matches("drop:owned-result").count(), 1);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn thousands_of_coalesced_wakes_registration_cycles_and_retired_identities() {
    let dir = std::env::temp_dir().join(format!("tarn-exec-runtime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap(); let source = dir.join("test.c");
    std::fs::write(&source, format!("{}\n{}", include_str!("../../../runtime/native.c"), include_str!("execution_runtime.c"))).unwrap();
    let exe = dir.join("program");
    let out = Command::new("cc").args(["-std=c11", "-O0", "-pthread", "-Wall", "-Wextra", "-Werror"])
        .arg(source).args(["-lm", "-o"]).arg(&exe).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run(&exe); assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn executor_abandonment_destroys_owned_state_and_generic_result_once() {
    let source = r#"import "io"
import "runtime"
struct Box<T> { value T }
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    {
        var executor = runtime.Executor.new()
        text := "abandoned-state"
        var calls = 0
        executor = executor.add(runtime.Operation.new(&execution, move fn(waker &io.Waker) io.Progress<void> {
            calls = calls + 1
            print(text.len())
            return io.Progress.Pending
        }))
        try executor.turn(&execution, 0)
    }
    {
        var calls = 0
        var op = runtime.Operation.new(&execution, move fn(waker &io.Waker) io.Progress<Box<string>> {
            calls = calls + 1
            return io.Progress.Ready(Box{value: "generic-result"})
        })
        match op.poll() { io.Progress.Ready(result) => print(result.value.len())
            io.Progress.Pending => panic("generic result pending") }
        op.finish()
    }
    return Ok(())
}"#;
    let exe = compile(source, "owned"); let out = run(&exe); balanced(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("drop:abandoned-state").count(), 1);
    assert_eq!(stderr.matches("drop:generic-result").count(), 1);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
fn fault_binary(src: &str, tag: &str) -> PathBuf {
    let (dir, result) = checked(src, tag); let object = dir.join("program.o");
    std::fs::write(&object, tarn_backend::emit_object(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap()).unwrap()).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")); let exe = dir.join("program");
    let out = Command::new("cc").args(["-std=c11", "-O0", "-pthread", if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" }])
        .arg(object).arg(root.join("../../runtime/native.c")).arg(root.join("tests/execution_faults.c"))
        .args(["-Wl,--wrap=send", "-Wl,--wrap=recv", "-lm", "-o"]).arg(&exe).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)); exe
}
#[test]
fn registration_retry_closes_lost_wakeup_window_and_write_all_retains_offset() {
    let source = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    var client = try net.TcpStream.connect_addr(try listener.local_addr())
    var server = try listener.accept()
    try server.set_nonblocking(true)
    try client.write_all(&[2]u8{41, 42})
    // Loopback delivery can be asynchronous, and this race needs the data present.
    var ready = try net.Poll.new()
    var events = [1]net.Event{net.Event.empty()}
    token := try ready.register_stream(&server, net.Interest.Readable)
    if try ready.wait(&mut events, 5000) == usize(0) { panic("data never arrived") }
    try ready.deregister_stream(&server, token)
    var bytes = [2]u8{0, 0}
    var read = runtime.read_operation(&execution, &mut server, &mut bytes)
    // Injected WouldBlock precedes registration while real data is ready.
    match read.poll() { io.Progress.Ready(value) => { if try value != usize(2) { panic("lost bytes") } }
        io.Progress.Pending => panic("missed registration retry") }
    read.finish()
    if bytes[0] != u8(41) || bytes[1] != u8(42) { panic("bad read") }
    return Ok(())
}"#;
    let exe = fault_binary(source, "race");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_ARM_RACE", "1").env("TARN_TRACE_NET", "1").output().unwrap(); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let source = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    var client = try net.TcpStream.connect_addr(try listener.local_addr())
    var server = try listener.accept()
    try client.set_nonblocking(true)
    var bytes = [4]u8{41, 42, 43, 44}
    var operation = runtime.write_all_operation(&execution, &mut client, &bytes)
    var complete = false
    var polls = 0
    for !complete {
        polls = polls + 1
        if polls > 16 { panic("write_all did not progress") }
        match operation.poll() {
            io.Progress.Ready(value) => { try value
                complete = true }
            io.Progress.Pending => { try execution.wait(0) }
        }
    }
    operation.finish()
    if polls != 4 { panic("write_all spun instead of yielding after partial progress") }
    var received = [4]u8{0, 0, 0, 0}
    var count = usize(0)
    for count != usize(4) { count = count + try server.read(&mut received[count..]) }
    for i in 0..4 { if received[i] != bytes[i] { panic("offset lost") } }
    bytes[0] = u8(99)
    return Ok(())
}"#;
    let exe = fault_binary(source, "write-all");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_WRITE_ALL", "1").env("TARN_TRACE_NET", "1").output().unwrap(); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn pending_write_and_write_all_are_safely_abandoned() {
    let source = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    var client = try net.TcpStream.connect_addr(try listener.local_addr())
    server := try listener.accept()
    try client.set_nonblocking(true)
    var bytes = [2]u8{41, 42}
    {
        var operation = runtime.write_operation(&execution, &mut client, &bytes)
        match operation.poll() { io.Progress.Pending => {}
            io.Progress.Ready(value) => panic("write did not suspend") }
    }
    bytes[0] = u8(99)
    {
        var operation = runtime.write_all_operation(&execution, &mut client, &bytes)
        match operation.poll() { io.Progress.Pending => {}
            io.Progress.Ready(value) => panic("write_all did not suspend") }
    }
    bytes[0] = u8(1)
    try client.close()
    return Ok(())
}"#;
    let exe = fault_binary(source, "abandon-write");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_BLOCK_WRITE", "1").env("TARN_TRACE_NET", "1").output().unwrap(); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn nested_readiness_wakes_executor_and_wrong_execution_is_rejected() {
    let source = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    var client = try net.TcpStream.connect_addr(try listener.local_addr())
    var server = try listener.accept()
    try server.set_nonblocking(true)
    var bytes = [1]u8{0}
    var read = runtime.read_operation(&execution, &mut server, &mut bytes)
    var executor = runtime.Executor.new()
    executor = executor.add(runtime.Operation.new(&execution, move fn(waker &io.Waker) io.Progress<void> {
        match read.poll_with(waker) {
            io.Progress.Pending => { return io.Progress.Pending }
            io.Progress.Ready(value) => { if try_count(value) != usize(1) { panic("bad read") }
                return io.Progress.Ready(()) }
        }
    }))
    try executor.turn(&execution, 0)
    // The other native task is independent of the cooperative executor.
    writer := spawn move fn() Result<void, io.Error> { try client.write_all(&[1]u8{42})
        return Ok(()) }
    try executor.run(&execution)
    try writer.join()
    if bytes[0] != u8(42) { panic("nested wake failed") }
    return Ok(())
}
fn try_count(value Result<usize, io.Error>) usize {
    match value { Ok(count) => { return count }
        Err(error) => panic("read error") }
}"#;
    let exe = compile(source, "nested"); let out = run(&exe); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let source = r#"import "io"
import "runtime"
fn main() Result<void, io.Error> {
    first := try runtime.Execution.new()
    second := try runtime.Execution.new()
    var calls = 0
    executor := runtime.Executor.new().add(runtime.Operation.new(&first, move fn(w &io.Waker) io.Progress<void> {
        calls = calls + 1
        return io.Progress.Pending
    }))
    match executor.run(&second) { Err(error) => {}
        Ok(value) => panic("wrong execution accepted") }
    return Ok(())
}"#;
    let exe = compile(source, "wrong-owner"); let out = run(&exe); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn corrupted_wake_layout_identity_and_loan_contracts_are_rejected() {
    for mutation in 0..7 {
        let (dir, mut result) = checked("import \"io\"\nimport \"runtime\"\nimport \"net\"\nfn main() Result<void, io.Error> { owner := try runtime.Execution.new()\nreturn Ok(()) }", &format!("metadata-{mutation}"));
        let typed = result.typed.as_mut().unwrap(); let wake = typed.decls.exec_waker.unwrap();
        let new_wake = typed.decls.net_intrinsics["runtime._waker_new"];
        match mutation {
            0 => typed.decls.structs.get_mut(&wake).unwrap().is_copy = true,
            1 => typed.decls.structs.get_mut(&wake).unwrap().fields[0].ty = tarn_types::Ty::Int(tarn_types::IntTy::U64),
            2 => typed.decls.native_capabilities.get_mut(&wake).unwrap().transfer = true,
            3 => typed.decls.fns.get_mut(&new_wake).unwrap().contract.result = tarn_types::ResultContract::Owned,
            4 => typed.decls.fns.get_mut(&new_wake).unwrap().params[0] = tarn_types::Ty::Int(tarn_types::IntTy::Usize),
            5 => typed.decls.fns.get_mut(&new_wake).unwrap().ret = tarn_types::Ty::Int(tarn_types::IntTy::Usize),
            6 => { let arm = typed.decls.net_intrinsics["io._wake_arm"];
                typed.decls.fns.get_mut(&arm).unwrap().params[0] = tarn_types::Ty::Int(tarn_types::IntTy::Usize); }
            _ => unreachable!(),
        }
        let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(result.drops.as_ref().unwrap(), typed)));
        assert!(output.is_ok() && output.unwrap().is_err(), "mutation {mutation}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn executor_io_rejects_blocking_sockets_without_entering_read() {
    let source = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    execution := try runtime.Execution.new()
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    client := try net.TcpStream.connect_addr(try listener.local_addr())
    var server = try listener.accept()
    var bytes = [1]u8{0}
    var operation = runtime.read_operation(&execution, &mut server, &mut bytes)
    match operation.poll() {
        io.Progress.Ready(value) => match value {
            Err(error) => { if error.native_code() != 22 { panic("wrong mode error") } }
            Ok(count) => panic("blocking read executed")
        }
        io.Progress.Pending => panic("blocking descriptor accepted")
    }
    operation.finish()
    return Ok(())
}"#;
    let exe = compile(source, "blocking"); let out = run(&exe); balanced(&out);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
