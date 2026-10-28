//! Loopback and syscall fault injection; no internet or timing-based assertions.
use std::{
    collections::HashSet,
    path::PathBuf,
    process::{Command, Output},
};

fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-net-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let res = tarn_driver::check(&file).unwrap();
    assert!(!res.has_errors(), "{}", res.diagnostics.iter().map(|d| d.render(&res.program.sources)).collect::<String>());
    (dir, res)
}
fn compile(src: &str, tag: &str) -> PathBuf {
    let (dir, res) = checked(src, tag);
    let exe = dir.join("program");
    tarn_backend::build(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &exe).unwrap();
    exe
}
fn run(exe: &PathBuf) -> Output {
    Command::new("timeout").arg("30s").arg(exe).env("TARN_TRACE_NET", "1").output().unwrap()
}
fn balanced(out: &Output) -> usize {
    let mut alive = HashSet::new();
    let mut opens = 0;
    for line in String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.starts_with("net:")) {
        let mut parts = line.split(':');
        parts.next();
        let event = parts.next().unwrap();
        let fd: i32 = parts.next().unwrap().parse().unwrap();
        if event == "open" {
            assert!(alive.insert(fd), "duplicate live owner: {line}");
            opens += 1;
        } else {
            assert_eq!(event, "close");
            assert!(alive.remove(&fd), "double close: {line}");
        }
    }
    assert!(alive.is_empty(), "descriptor leak: {alive:?}");
    opens
}
#[test]
fn native_socket_destruction_covers_moves_control_flow_and_try() {
    let src = r#"import "io"
import "net"
fn early() Result<void, io.Error> {
    socket := try net.UdpSocket.bind(&"127.0.0.1:0")
    return Ok(())
}
fn conditional(flag bool) Result<void, io.Error> {
    var socket: net.UdpSocket
    if flag { socket = try net.UdpSocket.bind(&"127.0.0.1:0") }
    return Ok(())
}
fn failed() Result<void, io.Error> {
    socket := try net.UdpSocket.bind(&"127.0.0.1:0")
    try net.resolve(&"invalid")
    return Ok(())
}
fn main() Result<void, io.Error> {
    try early()
    try conditional(true)
    try conditional(false)
    for i in 0..4 {
        socket := try net.UdpSocket.bind(&"127.0.0.1:0")
        if i == 0 { continue }
        if i == 2 { break }
    }
    {
        first := try net.UdpSocket.bind(&"127.0.0.1:0")
        second := first
        third := second
        try third.close()
    }
    var socket = try net.UdpSocket.bind(&"127.0.0.1:0")
    socket = try net.UdpSocket.bind(&"127.0.0.1:0")
    socket = socket
    match failed() { Err(error) => print("expected")
        Ok(value) => panic("missing error") }
    return Ok(())
}"#;
    let exe = compile(src, "drops");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(balanced(&out), 9);
    assert_eq!(out.stdout, b"expected\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn ipv6_loopback_and_task_owned_results() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("IPv6 loopback unavailable");
        return;
    }
    let tcp = include_str!("../../../tests/native/pass/network_tcp.tarn").replace("127.0.0.1:0", "[::1]:0").replace("is_ipv4", "is_ipv6");
    let exe = compile(&tcp, "ipv6-tcp");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, include_bytes!("../../../tests/native/pass/network_tcp.stdout"));
    assert_eq!(balanced(&out), 3);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let udp = include_str!("../../../tests/native/pass/network_udp.tarn").replace("127.0.0.1:0", "[::1]:0").replace("is_ipv4", "is_ipv6");
    let exe = compile(&udp, "ipv6-udp");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, include_bytes!("../../../tests/native/pass/network_udp.stdout"));
    assert_eq!(balanced(&out), 2);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn sigpipe_becomes_a_broken_pipe_result() {
    let src = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    address := try listener.local_addr()
    task := spawn move fn() Result<bool, io.Error> {
        var stream = try net.TcpStream.connect_addr(address)
        try stream.shutdown(net.Shutdown.Write)
        match stream.write(&[1]u8{42}) {
            Err(error) => match error.kind {
                io.ErrorKind.BrokenPipe => return Ok(true)
                _ => panic("wrong error")
            }
            Ok(count) => panic("write after shutdown succeeded")
        }
        return Ok(false)
    }
    var accepted = try listener.accept()
    var buffer = [1]u8{0}
    print(try accepted.read(&mut buffer) == usize(0))
    if try task.join() { print("broken pipe") }
    return Ok(())
}"#;
    let exe = fault_binary(src, "sigpipe");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    assert!(text.contains("broken pipe\n") && text.contains("true\n"));
    assert_eq!(balanced(&out), 3);
    // The same fixture must detect removal of MSG_NOSIGNAL, with default SIGPIPE.
    use std::os::unix::process::ExitStatusExt;
    let mutant = Command::new(&exe).env("TEST_DROP_NOSIGNAL", "1").output().unwrap();
    assert_eq!(mutant.status.signal(), Some(13));
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn result_main_reports_errors_and_cleans_up_without_abort() {
    let exe = compile(
        "import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> {\n socket := try net.UdpSocket.bind(&\"127.0.0.1:0\")\n try net.resolve(&\"invalid\")\n return Ok(()) }",
        "main-error",
    );
    let out = run(&exe);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("I/O error: invalid address"));
    assert_eq!(balanced(&out), 1);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

fn fault_binary(src: &str, tag: &str) -> PathBuf {
    let (dir, res) = checked(src, tag);
    link_faults(&dir, tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap())
}
fn link_faults(dir: &std::path::Path, bytes: Vec<u8>) -> PathBuf {
    let object = dir.join("program.o");
    std::fs::write(&object, bytes).unwrap();
    let exe = dir.join("program");
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/native.c");
    let faults = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/network_faults.c");
    let mut cmd = Command::new("cc");
    cmd.args(["-std=c11", "-O0", "-fno-strict-aliasing", "-no-pie", "-pthread"]);
    cmd.arg(object).arg(runtime).arg(faults).args(["-lm", "-o"]).arg(&exe);
    for name in [
        "socket",
        "connect",
        "accept",
        "shutdown",
        "send",
        "recv",
        "sendto",
        "recvfrom",
        "getaddrinfo",
        "bind",
        "close",
        "listen",
        "getsockname",
        "getpeername",
    ] {
        cmd.arg(format!("-Wl,--wrap={name}"));
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    exe
}
#[test]
fn interrupted_operations_and_partial_writes_preserve_results_and_owners() {
    for (tag, src, expected, opens) in [
        ("tcp", include_str!("../../../tests/native/pass/network_tcp.tarn"), include_bytes!("../../../tests/native/pass/network_tcp.stdout").as_slice(), 4),
        ("udp", include_str!("../../../tests/native/pass/network_udp.tarn"), include_bytes!("../../../tests/native/pass/network_udp.stdout").as_slice(), 2),
    ] {
        let exe = fault_binary(src, &format!("fault-{tag}"));
        let out = Command::new("timeout")
            .arg("30s")
            .arg(&exe)
            .env("TARN_TRACE_NET", "1")
            .env("TEST_EINTR", "1")
            .env("TEST_PARTIAL", "1")
            .env("TEST_FD_AUDIT", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout, expected);
        assert_eq!(balanced(&out), opens);
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}
#[test]
fn zero_progress_is_a_write_zero_error() {
    let src = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    address := try listener.local_addr()
    task := spawn move fn() Result<void, io.Error> {
        var stream = try net.TcpStream.connect_addr(address)
        return stream.write_all(&[1]u8{42})
    }
    accepted := try listener.accept()
    match task.join() {
        Err(error) => match error.kind { io.ErrorKind.WriteZero => print("zero")
            _ => panic("wrong error") }
        Ok(value) => panic("zero write succeeded")
    }
    return Ok(())
}"#;
    let exe = fault_binary(src, "zero");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_ZERO", "1").env("TARN_TRACE_NET", "1").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"zero\n");
    assert_eq!(balanced(&out), 3);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn native_error_mapping_dns_and_close_eintr_are_explicit() {
    let src = "import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { socket := try net.UdpSocket.bind(&\"127.0.0.1:0\")\n return Ok(()) }";
    let exe = fault_binary(src, "mapping");
    for (code, label) in [
        (98, "address in use"),
        (111, "connection refused"),
        (104, "connection reset"),
        (32, "broken pipe"),
        (110, "timed out"),
        (11, "would block"),
        (22, "invalid address"),
        (5, "other OS error"),
    ] {
        let out = Command::new(&exe).env("TEST_ERRNO", code.to_string()).env("TARN_TRACE_NET", "1").output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("I/O error: {label} (native code {code})")));
        assert_eq!(balanced(&out), 1);
    }
    let out = Command::new(&exe).env("TEST_DNS", "1").env("TARN_TRACE_NET", "1").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("I/O error: DNS failure"));
    assert_eq!(balanced(&out), 0);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let src = "import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { socket := try net.UdpSocket.bind(&\"127.0.0.1:0\")\n try socket.close()\n return Ok(()) }";
    let exe = fault_binary(src, "close-eintr");
    let out = Command::new(&exe).env("TEST_CLOSE_EINTR", "1").env("TARN_TRACE_NET", "1").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(balanced(&out), 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains("native code 4"));
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn malformed_network_metadata_is_rejected_without_panics() {
    let (dir, res) =
        checked("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { socket := try net.UdpSocket.bind(&\"127.0.0.1:0\")\n return Ok(()) }", "metadata");
    let source = res.drops.as_ref().unwrap();
    let typed = res.typed.as_ref().unwrap();
    for mutation in 0..5 {
        let mut p = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let (name, args) = p
            .functions
            .iter_mut()
            .flat_map(|f| &mut f.blocks)
            .find_map(|b| match &mut b.term {
                tarn_ir::Terminator::Call { callee: tarn_ir::Callee::Intrinsic(name), args, .. } if name == "net._socket" => Some((name, args)),
                _ => None,
            })
            .unwrap();
        match mutation {
            0 => {
                args.pop();
            }
            1 => args[0] = tarn_ir::Operand::Const(tarn_ir::Const::Bool(true)),
            2 => args[1] = tarn_ir::Operand::Const(tarn_ir::Const::Int(0, tarn_types::IntTy::I32)),
            3 => *name = "net._unknown".into(),
            _ => args.push(tarn_ir::Operand::Const(tarn_ir::Const::Bool(true))),
        }
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(&p, typed)));
        assert!(out.is_ok());
        assert!(out.unwrap().is_err());
    }
    // Copying a consuming socket or mutable buffer reference weakens ownership.
    for operation in ["net._close_udp", "net._read"] {
        let mut p = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let args = p
            .functions
            .iter_mut()
            .flat_map(|f| &mut f.blocks)
            .find_map(|b| match &mut b.term {
                tarn_ir::Terminator::Call { callee: tarn_ir::Callee::Intrinsic(name), args, .. } if name == operation => Some(args),
                _ => None,
            })
            .unwrap();
        let index = usize::from(operation == "net._read");
        let tarn_ir::Operand::Move(place) = &args[index] else { panic!("expected owned ABI operand") };
        args[index] = tarn_ir::Operand::Copy(place.clone());
        assert!(!tarn_ir::post_drop::verify(&p, typed).is_empty());
    }
    // Declaration shape and explicit native capability are also verified.
    let mut res = res;
    let typed = res.typed.as_mut().unwrap();
    let socket = typed.decls.net_sockets[0];
    typed.decls.structs.get_mut(&socket).unwrap().fields[0].is_pub = true;
    assert!(!tarn_ir::post_drop::verify(res.drops.as_ref().unwrap(), typed).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn fd_trace_oracle_detects_missing_close_and_double_close_mutations() {
    let (dir, res) =
        checked("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { socket := try net.UdpSocket.bind(&\"127.0.0.1:0\")\n return Ok(()) }", "drop-mutants");
    let source = res.drops.as_ref().unwrap();
    let typed = res.typed.as_ref().unwrap();
    let exe = dir.join("program");
    for duplicate in [false, true] {
        let mut p = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let main = p.functions.iter_mut().find(|f| f.decl.name == "main").unwrap();
        let local = main.decl.locals.iter().position(|l| l.name.as_deref() == Some("socket")).unwrap();
        let block = main
            .blocks
            .iter_mut()
            .find(|b| {
                b.stmts
                    .iter()
                    .any(|s| matches!(&s.op, tarn_ir::post_drop::Op::Destroy(tarn_ir::post_drop::Drop::Value(place)) if place.local.0 as usize == local))
            })
            .unwrap();
        let index = block
            .stmts
            .iter()
            .position(|s| matches!(&s.op, tarn_ir::post_drop::Op::Destroy(tarn_ir::post_drop::Drop::Value(place)) if place.local.0 as usize == local))
            .unwrap();
        if duplicate {
            block.stmts.insert(index, block.stmts[index].clone());
        } else {
            block.stmts.remove(index);
        }
        tarn_backend::build(&p, typed, &exe).unwrap();
        let out = run(&exe);
        if duplicate {
            assert!(!out.status.success());
        } else {
            assert!(out.status.success());
            assert!(std::panic::catch_unwind(|| balanced(&out)).is_err(), "leak escaped trace oracle");
            // Independently audit the real process descriptor table before exit.
            link_faults(&dir, tarn_backend::emit_object(&p, typed).unwrap());
            let audited = Command::new(&exe).env("TEST_FD_AUDIT", "1").output().unwrap();
            assert!(!audited.status.success());
            assert!(String::from_utf8_lossy(&audited.stderr).contains("test:fd-leak"));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn local_source_cannot_acquire_network_intrinsic_authority() {
    let (dir, _) = checked("fn main() {}", "trusted-loader");
    // A filesystem module called net must not override embedded declarations.
    std::fs::write(dir.join("net.tarn"), "pub extern \"intrinsic\" fn stolen() i32\n").unwrap();
    std::fs::write(dir.join("main.tarn"), "import \"net\"\nfn main() { net.stolen() }").unwrap();
    let result = tarn_driver::check(&dir.join("main.tarn")).unwrap();
    assert!(result.diagnostics.iter().any(|d| d.code == "E2005"));
    // An entry itself named net remains ordinary, untrusted user source.
    let result = tarn_driver::check(&dir.join("net.tarn")).unwrap();
    assert!(result.diagnostics.iter().any(|d| d.code == "E2027"));
    // Reserved entry filenames can still import the real networking module.
    std::fs::write(
        dir.join("net.tarn"),
        r#"import "net"
fn main() { net.resolve(&"127.0.0.1:0") }
"#,
    )
    .unwrap();
    let result = tarn_driver::check(&dir.join("net.tarn")).unwrap();
    assert!(!result.has_errors());
    // The same trust boundary protects the core lang-item catalog.
    std::fs::write(
        dir.join("core.tarn"),
        r#"pub extern "intrinsic" fn stolen() i32
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.tarn"),
        r#"import "core"
fn main() { core.stolen() }
"#,
    )
    .unwrap();
    let result = tarn_driver::check(&dir.join("main.tarn")).unwrap();
    assert!(result.diagnostics.iter().any(|d| d.code == "E2005"));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn udp_truncation_zero_capacity_and_owned_task_roundtrip() {
    let src = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    var receiver = try net.UdpSocket.bind(&"127.0.0.1:0")
    sender := try net.UdpSocket.bind(&"127.0.0.1:0")
    task := spawn move fn() net.UdpSocket { return sender }
    var returned = task.join()
    peer := try receiver.local_addr()
    try returned.send_to(&[3]u8{11, 22, 33}, peer)
    var buffer = [2]u8{0, 0}
    packet := try receiver.recv_from(&mut buffer)
    print(packet.count)
    print(buffer[0])
    print(buffer[1])
    try returned.send_to(&[1]u8{99}, peer)
    print((try receiver.recv_from(&mut buffer[0..0])).count)
    try returned.send_to(&[1]u8{77}, peer)
    print((try receiver.recv_from(&mut buffer)).count)
    print(buffer[0])
    return Ok(())
}"#;
    let exe = compile(src, "udp-truncation");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"2\n11\n22\n0\n1\n77\n");
    assert_eq!(balanced(&out), 2);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn invalid_endpoints_and_private_address_representation() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    for address in &[8]string{"127.0.0.1:65536", "[::1]", "127.0.0.1:-1", "::1:80", ":", ":x", "x:80:90", "127.0.0.1:"} {
        match net.resolve(address) { Err(error) => match error.kind {
            io.ErrorKind.InvalidAddress => print("invalid")
            _ => panic("wrong error") }
            Ok(value) => panic("invalid endpoint accepted") }
    }
    addr := try net.resolve(&"127.0.0.1:1234")
    print(addr.with_port(65535).port())
    match addr.ip() { net.IpAddr.V4(bytes) => print(bytes[0])
        _ => panic("wrong family") }
    print((try net.resolve(&":0")).port())
    return Ok(())
}"#;
    let exe = compile(src, "invalid-endpoints");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{}65535\n127\n0\n", "invalid\n".repeat(8)));
    assert_eq!(balanced(&out), 0);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn shipped_echo_examples_serve_real_external_clients() {
    use std::io::{BufRead, Read, Write};
    for (tag, example) in [("echo", include_str!("../../../examples/tcp_echo.tarn")), ("echo-task", include_str!("../../../examples/25_concurrency.tarn"))] {
        let source = example.replace("127.0.0.1:8080", "127.0.0.1:0");
        let exe = fault_binary(&source, tag);
        let mut child = Command::new("timeout")
            .arg("30s")
            .arg(&exe)
            .env("TEST_READY", "1")
            .env("TARN_TRACE_NET", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
        let mut trace = Vec::new();
        let port = loop {
            let mut line = String::new();
            assert!(stderr.read_line(&mut line).unwrap() > 0, "echo exited before listening");
            trace.extend_from_slice(line.as_bytes());
            if let Some(port) = line.strip_prefix("test:ready:") {
                break port.trim().parse::<u16>().unwrap();
            }
        };
        let mut client = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        client.set_write_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let payload: Vec<u8> = (0..16384).map(|n| (n % 251) as u8).collect();
        client.write_all(&payload).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut echoed = Vec::new();
        client.read_to_end(&mut echoed).unwrap();
        assert_eq!(echoed, payload);
        let status = child.wait().unwrap();
        stderr.read_to_end(&mut trace).unwrap();
        assert!(status.success(), "{}", String::from_utf8_lossy(&trace));
        assert_eq!(balanced(&Output { status, stdout: Vec::new(), stderr: trace }), 2);
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

#[test]
fn tcp_stream_can_return_from_a_task_and_empty_read_does_not_block() {
    let src = r#"import "io"
import "runtime"
import "net"
fn main() Result<void, io.Error> {
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    address := try listener.local_addr()
    task := spawn move fn() Result<net.TcpStream, io.Error> { return net.TcpStream.connect_addr(address) }
    var accepted = try listener.accept()
    var client = try task.join()
    print((try accepted.local_addr()).port() == address.port())
    print((try accepted.peer_addr()).port() == (try client.local_addr()).port())
    var buffer = [4]u8{0, 0, 0, 0}
    print(try accepted.read(&mut buffer[0..0]))
    written := try client.write(&[4]u8{10, 20, 30, 40})
    print(written > usize(0) && written <= usize(4))
    var offset = usize(0)
    for offset < written { offset = offset + try accepted.read(&mut buffer[offset..written]) }
    print(buffer[0])
    try client.shutdown(net.Shutdown.Both)
    try accepted.shutdown(net.Shutdown.Read)
    return Ok(())
}"#;
    let exe = compile(src, "tcp-return");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"true\ntrue\n0\ntrue\n10\n");
    assert_eq!(balanced(&out), 3);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn mutex_serializes_a_non_share_socket_and_destroys_its_payload_once() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    var receiver = try net.UdpSocket.bind(&"127.0.0.1:0")
    peer := try receiver.local_addr()
    sender := try net.UdpSocket.bind(&"127.0.0.1:0")
    mutex := Mutex.new(sender)
    receiving := spawn move fn() Result<u64, io.Error> {
        var buffer = [1]u8{0}
        var total = u64(0)
        for i in 0..64 {
            packet := try receiver.recv_from(&mut buffer)
            total = total + u64(packet.count) * u64(buffer[0])
        }
        return Ok(total)
    }
    scope {
        first := spawn fn() Result<void, io.Error> {
            for i in 0..32 {
                var guard = mutex.lock()
                try guard.value().send_to(&[1]u8{1}, peer)
            }
            return Ok(())
        }
        second := spawn fn() Result<void, io.Error> {
            for i in 0..32 {
                var guard = mutex.lock()
                try guard.value().send_to(&[1]u8{1}, peer)
            }
            return Ok(())
        }
        try first.join()
        try second.join()
    }
    print(try receiving.join())
    return Ok(())
}"#;
    let exe = fault_binary(src, "mutex-socket");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TARN_TRACE_NET", "1").env("TEST_FD_AUDIT", "1").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"64\n");
    assert_eq!(balanced(&out), 2);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn descriptor_reuse_trace_is_ordered_even_if_close_returns_late() {
    let dir = std::env::temp_dir().join(format!("tarn-net-{}-reuse", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("reuse.c");
    let test = r#"
#include <assert.h>
static pthread_barrier_t released, acquired;
static int old_fd;
static atomic_int first_close;
int __real_close(int);
int __wrap_close(int fd) {
    int result = __real_close(fd);
    if (!atomic_exchange(&first_close, 1)) {
        int rc = pthread_barrier_wait(&released);
        assert(rc == 0 || rc == PTHREAD_BARRIER_SERIAL_THREAD);
        rc = pthread_barrier_wait(&acquired);
        assert(rc == 0 || rc == PTHREAD_BARRIER_SERIAL_THREAD);
    }
    return result;
}
static void *closer(void *unused) { (void)unused; tarn_rt_net_drop(old_fd); return NULL; }
int main(void) {
    assert(!pthread_barrier_init(&released, NULL, 2));
    assert(!pthread_barrier_init(&acquired, NULL, 2));
    TarnNetAddr address = {{0}}; address.bytes[18] = 4;
    TarnNetRaw first, second;
    tarn_rt_net_socket(&first, &address, 1); assert(!first.code);
    old_fd = (int)first.value;
    pthread_t thread; assert(!pthread_create(&thread, NULL, closer, NULL));
    int rc = pthread_barrier_wait(&released);
    assert(rc == 0 || rc == PTHREAD_BARRIER_SERIAL_THREAD);
    tarn_rt_net_socket(&second, &address, 1);
    assert(!second.code && second.value == first.value);
    rc = pthread_barrier_wait(&acquired);
    assert(rc == 0 || rc == PTHREAD_BARRIER_SERIAL_THREAD);
    assert(!pthread_join(thread, NULL));
    tarn_rt_net_drop((int)second.value);
    assert(!pthread_barrier_destroy(&released));
    assert(!pthread_barrier_destroy(&acquired));
    return 0;
}
"#;
    std::fs::write(&source, format!("{}\n{test}", include_str!("../../../runtime/native.c"))).unwrap();
    let exe = dir.join("program");
    let output = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fno-strict-aliasing",
            "-pthread",
            "-fsanitize=undefined",
            "-fno-sanitize-recover=all",
            "-Wl,--wrap=close",
        ])
        .arg(source)
        .args(["-lm", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(balanced(&out), 2);
    let events: Vec<_> = String::from_utf8_lossy(&out.stderr).lines().map(|line| line.split(':').nth(1).unwrap().to_string()).collect();
    assert_eq!(events, ["open", "close", "open", "close"]);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn unix_stream_roundtrip_errors_timeout_and_owned_transfer() {
    use std::io::{Read, Write};
    let dir = std::env::temp_dir().join(format!("tarn-unix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("echo.sock");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        // First connection echoes; the second stays silent so readiness times out.
        let (mut echo, _) = listener.accept().unwrap();
        let mut bytes = [0u8; 4];
        echo.read_exact(&mut bytes).unwrap();
        echo.write_all(&bytes).unwrap();
        let (mut silent, _) = listener.accept().unwrap();
        let mut rest = Vec::new();
        silent.read_to_end(&mut rest).unwrap();
        rest
    });
    let src = r#"import "io"
import "net"
fn kind(path &string) string {
    match net.UnixStream.connect(path) {
        Ok(stream) => { return "connected" }
        Err(error) => {
            if error.kind == io.ErrorKind.NotFound { return "not found" }
            if error.kind == io.ErrorKind.InvalidAddress { return "invalid address" }
            return "other"
        }
    }
}
fn main() Result<void, io.Error> {
    path := "SOCKET"
    var stream = try net.UnixStream.connect(&path)
    worker := spawn move fn() Result<[4]u8, io.Error> {
        var owned = stream
        try owned.write_all(&[4]u8{1, 2, 3, 4})
        var bytes = [4]u8{0, 0, 0, 0}
        var offset = usize(0)
        for offset < usize(4) { offset = offset + try owned.read(&mut bytes[offset..]) }
        try owned.close()
        return Ok(bytes)
    }
    echoed := try worker.join()
    print(echoed[0])
    print(echoed[3])
    var silent = try net.UnixStream.connect(&path)
    try silent.set_nonblocking(true)
    var poll = try net.Poll.new()
    token := try poll.register_unix(&silent, net.Interest.Readable)
    var events = [2]net.Event{net.Event.empty(), net.Event.empty()}
    print(try poll.wait(&mut events, 50))
    try poll.deregister_unix(&silent, token)
    try silent.set_nonblocking(false)
    try silent.write_all(&[2]u8{7, 9})
    print(kind(&"MISSING"))
    print(kind(&""))
    print(kind(&"a\u{0}b"))
    print(kind(&"/tmp/01234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789x"))
    return Ok(())
}
"#
    .replace("MISSING", dir.join("missing.sock").to_str().unwrap())
    .replace("SOCKET", path.to_str().unwrap());
    let exe = compile(&src, "unix");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n4\n0\nnot found\ninvalid address\ninvalid address\ninvalid address\n");
    assert_eq!(balanced(&out), 3, "both streams and the poll are opened once and closed once");
    // The silent stream wrote before it was destroyed; destruction closed it.
    assert_eq!(server.join().unwrap(), vec![7, 9]);
    let _ = std::fs::remove_file(&path);
}
#[test]
fn foreign_address_family_is_an_error_result_not_an_abort() {
    // ADR 0069: an address query that sees a non-IP family (as an AF_UNIX
    // descriptor would) returns Err; the owner is still closed exactly once.
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    socket := try net.UdpSocket.bind(&"127.0.0.1:0")
    match socket.local_addr() {
        Ok(address) => { print("address") }
        Err(error) => { print(error.native_code()) }
    }
    try socket.close()
    return Ok(())
}
"#;
    let exe = fault_binary(src, "foreign-family");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TARN_TRACE_NET", "1").env("TEST_FOREIGN_FAMILY", "1").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "97\n");
    assert_eq!(balanced(&out), 1);
}
#[test]
fn unix_pair_is_connected_owned_and_closed_once() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    pair := try net.UnixStream.pair()
    var left = pair.first
    right := pair.second
    worker := spawn move fn() Result<void, io.Error> {
        var end = right
        var bytes = [3]u8{0, 0, 0}
        var offset = usize(0)
        for offset < usize(3) { offset = offset + try end.read(&mut bytes[offset..]) }
        try end.write_all(&bytes)
        return Ok(())
    }
    try left.write_all(&[3]u8{5, 6, 7})
    var echoed = [3]u8{0, 0, 0}
    var offset = usize(0)
    for offset < usize(3) { offset = offset + try left.read(&mut echoed[offset..]) }
    try worker.join()
    print(echoed[2])
    // The peer was destroyed when its task finished: end of stream.
    print(try left.read(&mut echoed))
    return Ok(())
}
"#;
    let exe = compile(src, "unix-pair");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n0\n");
    assert_eq!(balanced(&out), 2);
}
#[test]
fn unix_raw_fd_is_the_borrowed_live_descriptor() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    pair := try net.UnixStream.pair()
    var left = pair.first
    var right = pair.second
    print(left.raw_fd())
    print(right.raw_fd())
    // Borrowing the number does not transfer ownership: the streams still work.
    try left.write_all(&[1]u8{42})
    var byte = [1]u8{0}
    print(try right.read(&mut byte))
    print(byte[0])
    return Ok(())
}
"#;
    let exe = compile(src, "unix-raw-fd");
    let out = run(&exe);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect();
    assert_eq!(&printed[2..], ["1", "42"]);
    let opened: Vec<String> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("net:open:").map(str::to_owned))
        .collect();
    assert_eq!(opened, printed[..2], "raw_fd must report the owned descriptors");
    assert_eq!(balanced(&out), 2);
}
