//! Phase 12B: real epoll and native executables; no sleeps or internet.
use std::{collections::HashSet, path::{Path, PathBuf}, process::{Command, Output}};
fn checked(src: &str, tag: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-ready-{}-{tag}", std::process::id()));
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
fn run(exe: &Path) -> Output {
    Command::new("timeout").arg("30s").arg(exe).env("TARN_TRACE_NET", "1").output().unwrap()
}
fn balanced(out: &Output) -> usize {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut live = HashSet::new(); let mut count = 0;
    for line in String::from_utf8_lossy(&out.stderr).lines().filter(|s| s.starts_with("net:")) {
        let parts: Vec<_> = line.split(':').collect(); let fd = parts[2].parse::<i32>().unwrap();
        if parts[1] == "open" { assert!(live.insert(fd)); count += 1; }
        else { assert_eq!(parts[1], "close"); assert!(live.remove(&fd)); }
    }
    assert!(live.is_empty(), "descriptor leak: {live:?}"); count
}
#[test]
fn real_tcp_udp_readiness_and_mode_transitions() {
    let exe = compile(include_str!("../../../tests/native/pass/network_readiness.tarn"), "loopback");
    let out = run(&exe); assert_eq!(balanced(&out), 7);
    assert_eq!(out.stdout, b"readiness ok\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn poll_tasks_multiple_producers_and_socket_transfer() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    var a = try net.UdpSocket.bind(&"127.0.0.1:0")
    var b = try net.UdpSocket.bind(&"127.0.0.1:0")
    pa := try a.local_addr()
    pb := try b.local_addr()
    var poll = try net.Poll.new()
    try a.set_nonblocking(true)
    try b.set_nonblocking(true)
    at := try poll.register_udp(&a, net.Interest.Readable)
    bt := try poll.register_udp(&b, net.Interest.Readable)
    worker := spawn move fn() Result<net.UdpSocket, io.Error> {
        var events = [2]net.Event{net.Event.empty(), net.Event.empty()}
        var buffer = [1]u8{0}
        var na = usize(0)
        var nb = usize(0)
        for na + nb < usize(256) {
            count := try poll.wait(&mut events, 5000)
            if count == usize(0) { panic("readiness deadline") }
            for event in &events[0..count] {
                if event.token.same(at) { try a.recv_from(&mut buffer)
                    na = na + usize(1)
                }
                if event.token.same(bt) { try b.recv_from(&mut buffer)
                    nb = nb + usize(1)
                }
            }
        }
        if na != usize(128) || nb != usize(128) { panic("lost datagrams") }
        try poll.deregister_udp(&a, at)
        try poll.deregister_udp(&b, bt)
        return Ok(a)
    }
    first := spawn move fn() Result<void, io.Error> {
        var sender = try net.UdpSocket.bind(&"127.0.0.1:0")
        for i in 0..128 { try sender.send_to(&[1]u8{1}, pa) }
        return Ok(())
    }
    second := spawn move fn() Result<void, io.Error> {
        var sender = try net.UdpSocket.bind(&"127.0.0.1:0")
        for i in 0..128 { try sender.send_to(&[1]u8{2}, pb) }
        return Ok(())
    }
    try first.join()
    try second.join()
    returned := try worker.join()
    try returned.close()
    return Ok(())
}"#;
    let exe = compile(src, "tasks"); let out = run(&exe); assert_eq!(balanced(&out), 5);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn poll_destruction_all_normal_exits_and_failed_connections() {
    let src = r#"import "io"
import "net"
fn early() Result<void, io.Error> { poll := try net.Poll.new()
    return Ok(())
}
fn fail() Result<void, io.Error> { poll := try net.Poll.new()
    try net.resolve(&"invalid")
    return Ok(())
}
fn main() Result<void, io.Error> {
    try early()
    match fail() { Err(error) => {}
        Ok(value) => panic("expected failure") }
    for i in 0..64 { poll := try net.Poll.new()
        if i == 63 { break }
        continue
    }
    var poll = try net.Poll.new()
    var events = [1]net.Event{net.Event.empty()}
    listener := try net.TcpListener.bind(&"127.0.0.1:0")
    address := try listener.local_addr()
    try listener.close()
    match net.TcpStream.connect_nonblocking_addr(address) {
        Err(error) => {}
        Ok(pending) => {
            var connection = pending
            token := try poll.register_connecting(&connection)
            if try poll.wait(&mut events, 5000) == usize(0) { panic("failed connect deadline") }
            match connection.poll_connected() { Err(error) => {}
                Ok(value) => panic("failed connection succeeded") }
            match connection.poll_connected() { Err(error) => {}
                Ok(value) => panic("consumed SO_ERROR lost failure") }
            try poll.deregister_connecting(&connection, token)
        }
    }
    var socket = try net.UdpSocket.bind(&"127.0.0.1:0")
    token := try poll.register_udp(&socket, net.Interest.Readable)
    match poll.register_udp(&socket, net.Interest.Readable) { Err(error) => {}
        Ok(value) => panic("duplicate registration") }
    try poll.deregister_udp(&socket, token)
    match poll.wait(&mut events[0..0], 0) { Err(error) => {}
        Ok(value) => panic("empty event storage") }
    match poll.wait(&mut events, -2) { Err(error) => {}
        Ok(value) => panic("invalid timeout") }
    try poll.close()
    return Ok(())
}"#;
    let exe = compile(src, "drops"); let out = run(&exe); assert_eq!(balanced(&out), 70);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn readiness_metadata_and_capability_corruptions_are_rejected() {
    for mutation in 0..8 {
        let (dir, mut res) = checked("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { poll := try net.Poll.new()\nreturn Ok(()) }", &format!("metadata-{mutation}"));
        let typed = res.typed.as_mut().unwrap();
        let poll = typed.decls.net_poll.unwrap();
        let wait = typed.decls.net_intrinsics["net._poll_wait"];
        match mutation {
            0 => typed.decls.structs.get_mut(&poll).unwrap().is_copy = true,
            1 => typed.decls.structs.get_mut(&poll).unwrap().fields[0].ty = tarn_types::Ty::Int(tarn_types::IntTy::I32),
            2 => typed.decls.native_capabilities.get_mut(&poll).unwrap().share = true,
            3 => typed.decls.fns.get_mut(&wait).unwrap().params[1] = tarn_types::Ty::Bool,
            4 => if let tarn_types::Ty::Ref(_, inner) = &mut typed.decls.fns.get_mut(&wait).unwrap().params[1] {
                typed.decls.fns.get_mut(&wait).unwrap().params[1] = tarn_types::Ty::Ref(false, inner.clone());
            },
            5 => { let close = typed.decls.net_intrinsics["net._close_poll"];
                typed.decls.fns.get_mut(&close).unwrap().params[0] = tarn_types::Ty::Int(tarn_types::IntTy::Usize); },
            6 | 7 => {
                let tarn_types::Ty::Ref(_, slice) = &typed.decls.fns[&wait].params[1] else { panic!() };
                let tarn_types::Ty::Slice(event) = slice.as_ref() else { panic!() };
                let tarn_types::Ty::Adt(id, _) = event.as_ref() else { panic!() };
                let id = *id;
                if mutation == 6 { typed.decls.structs.get_mut(&id).unwrap().fields[1].ty = tarn_types::Ty::Int(tarn_types::IntTy::U8); }
                else { typed.decls.structs.get_mut(&id).unwrap().fields.swap(1, 2); }
            }
            _ => unreachable!(),
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(res.drops.as_ref().unwrap(), typed)));
        assert!(result.is_ok() && result.unwrap().is_err(), "mutation {mutation}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn readiness_keeps_ordinary_move_borrow_and_intrinsic_authority_rules() {
    let cases = [
        ("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { poll := try net.Poll.new()\ntry poll.close()\ntry poll.close()\nreturn Ok(()) }", "E4001"),
        ("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { poll := try net.Poll.new()\nscope { task := spawn fn() { observe(&poll) } }\nreturn Ok(()) }\nfn observe(value &net.Poll) {}", "E3047"),
        ("pub extern \"intrinsic\" fn _poll_new() i32\nfn main() {}", "E2027"),
    ];
    for (index, (src, code)) in cases.iter().enumerate() {
        let dir = std::env::temp_dir().join(format!("tarn-ready-reject-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in if index == 2 { vec!["net", "core", "poll", "runtime"] } else { vec!["main"] } {
            let file = dir.join(format!("{name}.tarn")); std::fs::write(&file, src).unwrap();
            let res = tarn_driver::check(&file).unwrap();
            assert!(res.diagnostics.iter().any(|d| d.code == *code), "{:?}", res.diagnostics.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn runtime_fd_reuse_registration_stress_and_no_descriptor_leaks() {
    let dir = std::env::temp_dir().join(format!("tarn-ready-runtime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("test.c");
    std::fs::write(&source, format!("{}\n{}", include_str!("../../../runtime/native.c"), include_str!("readiness_runtime.c"))).unwrap();
    let exe = dir.join("program");
    let output = Command::new("cc").args(["-std=c11", "-O0", "-pthread", "-Wall", "-Wextra", "-Werror"])
        .arg(&source).args(["-lm", "-o"]).arg(&exe).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out = Command::new("timeout").arg("30s").arg(&exe).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(dir).unwrap();
}
fn fault_binary(src: &str, tag: &str) -> PathBuf {
    let (dir, res) = checked(src, tag);
    let object = dir.join("program.o");
    std::fs::write(&object, tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap()).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = dir.join("program");
    let out = Command::new("cc").args(["-std=c11", "-O0", "-pthread", if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" }])
        .arg(object).arg(root.join("../../runtime/native.c")).arg(root.join("tests/readiness_faults.c"))
        .args([if cfg!(target_os = "macos") { "-Wl,--wrap=kevent" } else { "-Wl,--wrap=epoll_wait" }, "-Wl,--wrap=clock_gettime", "-Wl,--wrap=send", "-Wl,--wrap=connect", "-lm", "-o"])
        .arg(&exe).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)); exe
}
#[test]
fn interrupted_wait_keeps_monotonic_deadline_and_partial_write_keeps_progress() {
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    var poll = try net.Poll.new()
    var events = [1]net.Event{net.Event.empty()}
    if try poll.wait(&mut events, 5) != usize(0) { panic("timeout") }
    return Ok(())
}"#;
    let exe = fault_binary(src, "eintr");
    for expired in [false, true] {
        let mut command = Command::new("timeout");
        command.arg("30s").arg(&exe).env("TEST_EINTR", "1").env("TARN_TRACE_NET", "1");
        if expired { command.env("TEST_EXPIRE", "1"); }
        assert_eq!(balanced(&command.output().unwrap()), 1);
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let src = r#"import "io"
import "net"
fn main() Result<void, io.Error> {
    var listener = try net.TcpListener.bind(&"127.0.0.1:0")
    var connecting = try net.TcpStream.connect_nonblocking_addr(try listener.local_addr())
    var poll = try net.Poll.new()
    var events = [1]net.Event{net.Event.empty()}
    token := try poll.register_connecting(&connecting)
    if try poll.wait(&mut events, 5000) == usize(0) { panic("connect deadline") }
    if !try connecting.poll_connected() { panic("connect incomplete") }
    try poll.deregister_connecting(&connecting, token)
    var client = try connecting.into_stream()
    var server = try listener.accept()
    var buffer = [2]u8{0, 0}
    for i in 0..128 {
        if try client.write(&[2]u8{42, 43}) != usize(1) { panic("lost partial count") }
        match client.write(&[2]u8{42, 43}) {
            Err(error) => match error.kind { io.ErrorKind.WouldBlock => {}
                _ => panic("wrong retry error") }
            Ok(count) => panic("expected WouldBlock")
        }
        if try server.read(&mut buffer) != usize(1) || buffer[0] != u8(42) { panic("incorrect bytes") }
    }
    return Ok(())
}"#;
    let exe = fault_binary(src, "partial");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_PARTIAL", "1").env("TARN_TRACE_NET", "1").output().unwrap();
    assert_eq!(balanced(&out), 4);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn immediate_connect_branch_transfers_an_actual_connected_socket() {
    let exe = fault_binary(include_str!("../../../tests/native/pass/network_readiness.tarn"), "immediate");
    let out = Command::new("timeout").arg("30s").arg(&exe).env("TEST_IMMEDIATE", "1").env("TARN_TRACE_NET", "1").output().unwrap();
    assert_eq!(balanced(&out), 7);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn copied_poll_consumption_and_mutable_event_buffers_fail_verification() {
    let (dir, res) = checked("import \"io\"\nimport \"net\"\nfn main() Result<void, io.Error> { poll := try net.Poll.new()\nreturn Ok(()) }", "operand-metadata");
    let source = res.drops.as_ref().unwrap();
    for (operation, index) in [("net._close_poll", 0), ("net._poll_wait", 1)] {
        let mut program = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let args = program.functions.iter_mut().flat_map(|f| &mut f.blocks).find_map(|b| match &mut b.term {
            tarn_ir::Terminator::Call { callee: tarn_ir::Callee::Intrinsic(name), args, .. } if name == operation => Some(args),
            _ => None,
        }).unwrap();
        let tarn_ir::Operand::Move(place) = &args[index] else { panic!("expected move operand") };
        args[index] = tarn_ir::Operand::Copy(place.clone());
        assert!(!tarn_ir::post_drop::verify(&program, res.typed.as_ref().unwrap()).is_empty());
    }
    std::fs::remove_dir_all(dir).unwrap();
}
