//! Independent byte-level peers for the ordinary Tarn HTTP/1.1 library.
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn compile(source: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tarn-http-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.tarn");
    std::fs::write(&path, source).unwrap();
    let checked = tarn_driver::check(&path).unwrap();
    assert!(
        !checked.has_errors(),
        "{}",
        checked
            .diagnostics
            .iter()
            .map(|d| d.render(&checked.program.sources))
            .collect::<String>()
    );
    let exe = dir.join("program");
    tarn_backend::build(
        checked.drops.as_ref().unwrap(),
        checked.typed.as_ref().unwrap(),
        &exe,
    )
    .unwrap();
    exe
}
fn balanced(trace: &[u8]) {
    let mut live = HashSet::new();
    for line in String::from_utf8_lossy(trace)
        .lines()
        .filter(|l| l.starts_with("net:"))
    {
        let p: Vec<_> = line.split(':').collect();
        let fd: i32 = p[2].parse().unwrap();
        if p[1] == "open" {
            assert!(live.insert(fd), "double open: {line}");
        } else {
            assert!(live.remove(&fd), "double/unowned close: {line}");
        }
    }
    assert!(live.is_empty(), "leaked fds: {live:?}");
    let mut allocations = HashSet::new();
    for line in String::from_utf8_lossy(trace)
        .lines()
        .filter(|l| l.starts_with("exec:"))
    {
        let p: Vec<_> = line.split(':').collect();
        let key = (p[1].to_owned(), p[3].to_owned());
        if p[2] == "alloc" {
            assert!(allocations.insert(key), "duplicate allocation: {line}");
        } else {
            assert!(allocations.remove(&key), "unowned/double free: {line}");
        }
    }
    assert!(
        allocations.is_empty(),
        "leaked frames/buffers/wakers/task records: {allocations:?}"
    );
}
struct Server {
    child: Child,
    port: u16,
    expected_stdout: &'static [u8],
    stderr: Option<thread::JoinHandle<Vec<u8>>>,
    stdout: Option<thread::JoinHandle<Vec<u8>>>,
}
impl Server {
    fn start(exe: &Path) -> Self {
        // Line buffering is a test runner concern, not a public HTTP primitive.
        let mut child = Command::new("stdbuf")
            .arg("-oL")
            .arg(exe)
            .env("TARN_TRACE_NET", "1")
            .env("TARN_TRACE_DROPS", "1")
            .env("TARN_TRACE_EXEC", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut err = child.stderr.take().unwrap();
        let stderr = thread::spawn(move || {
            let mut bytes = Vec::new();
            err.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let stdout = thread::spawn(move || {
            let mut reader = BufReader::new(out);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            tx.send(line.trim().parse::<u16>()).unwrap();
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).unwrap();
            rest
        });
        let mut server = Self {
            child,
            port: 0,
            expected_stdout: b"",
            stderr: Some(stderr),
            stdout: Some(stdout),
        };
        server.port = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server did not bind")
            .expect("invalid port");
        server
    }
    fn connect(&self) -> TcpStream {
        connect(self.port)
    }
    fn finish(mut self) -> Vec<u8> {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "server did not terminate"
            );
            // Watchdog polling only; never used as evidence of protocol correctness.
            thread::sleep(Duration::from_millis(5));
        };
        let trace = self.stderr.take().unwrap().join().unwrap();
        let rest = self.stdout.take().unwrap().join().unwrap();
        assert!(status.success(), "{}", String::from_utf8_lossy(&trace));
        assert_eq!(rest, self.expected_stdout, "unexpected application output");
        balanced(&trace);
        trace
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.set_nodelay(true).unwrap();
    stream
}
fn example(count: usize, capacity: usize, timeouts_ms: Option<u64>) -> String {
    let mut source = include_str!("../../../examples/http_server.tarn")
        .replace("127.0.0.1:8080", "127.0.0.1:0")
        .replace(
            "usize(0), usize(128)",
            &format!("usize({count}), usize({capacity})"),
        );
    if let Some(ms) = timeouts_ms {
        source = source.replace("import \"http\"", "import \"http\"\nimport \"time\"").replace("http.Timeouts.defaults()", &format!("http.Timeouts{{head: time.Duration.milliseconds(u64({ms})), body: time.Duration.milliseconds(u64({ms})), write: time.Duration.milliseconds(u64({ms}))}}"));
    }
    source
}
#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}
fn response(reader: &mut BufReader<TcpStream>, head: bool) -> Reply {
    let mut first = Vec::new();
    reader.read_until(b'\n', &mut first).unwrap();
    assert!(first.ends_with(b"\r\n"), "invalid response line: {first:?}");
    let text = std::str::from_utf8(&first).unwrap();
    assert!(text.starts_with("HTTP/1.1 "));
    let status = text[9..12].parse::<u16>().unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).unwrap();
        assert!(line.ends_with(b"\r\n"), "invalid response header: {line:?}");
        if line == b"\r\n" {
            break;
        }
        let colon = line.iter().position(|b| *b == b':').unwrap();
        assert!(line[colon + 1..].starts_with(b" "));
        headers.push((
            String::from_utf8(line[..colon].to_vec()).unwrap(),
            line[colon + 2..line.len() - 2].to_vec(),
        ));
    }
    let lengths: Vec<_> = headers
        .iter()
        .filter(|(n, _)| n == "content-length")
        .collect();
    assert!(headers.iter().all(|(n, _)| n != "transfer-encoding"));
    let forbidden = status == 204 || status == 304;
    assert_eq!(lengths.len(), if forbidden { 0 } else { 1 });
    let count = if head || forbidden {
        0
    } else {
        std::str::from_utf8(&lengths[0].1)
            .unwrap()
            .parse::<usize>()
            .unwrap()
    };
    let mut body = vec![0; count];
    reader.read_exact(&mut body).unwrap();
    Reply {
        status,
        headers,
        body,
    }
}
fn closed(reader: &mut BufReader<TcpStream>) {
    let mut byte = [0];
    assert_eq!(
        reader.read(&mut byte).unwrap(),
        0,
        "unexpected trailing wire bytes"
    );
}
fn rejected_closed(reader: &mut BufReader<TcpStream>) {
    let mut byte = [0];
    match reader.read(&mut byte) {
        Ok(0) => {}
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
        result => panic!("unexpected bytes/error after rejection: {result:?}"),
    }
}

#[test]
fn strict_head_and_chunk_codec_tables_and_response_wire_rules() {
    let valid = [
        "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n",
        "OPTIONS * HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n",
        "X-Method /a%20b?q=1 HTTP/1.1\r\nHost: [2001:db8::1]\r\nX-Test: one\r\nX-Test: two\r\n\r\n",
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0003\r\n\r\n",
    ];
    let invalid = [
        ("GET / HTTP/1.1\nHost: localhost\n\n", 400),
        ("GET / HTTP/1.1\r\n\r\n", 400),
        (
            "GET / HTTP/1.1\r\nHost: localhost\r\nHost: localhost\r\n\r\n",
            400,
        ),
        ("GET / HTTP/1.0\r\nHost: localhost\r\n\r\n", 505),
        ("GET / HTTP/1.x\r\nHost: localhost\r\n\r\n", 400),
        ("GET /%zz HTTP/1.1\r\nHost: localhost\r\n\r\n", 400),
        ("GET /#fragment HTTP/1.1\r\nHost: localhost\r\n\r\n", 400),
        (
            "GET http://localhost/ HTTP/1.1\r\nHost: localhost\r\n\r\n",
            400,
        ),
        (
            "CONNECT localhost:80 HTTP/1.1\r\nHost: localhost\r\n\r\n",
            501,
        ),
        ("GET / HTTP/1.1\r\nHost : localhost\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: localhost\r\n folded\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: [1::2::3]\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: 999.1.1.1\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: user@host\r\n\r\n", 400),
        ("GET / HTTP/1.1\r\nHost: localhost:\r\n\r\n", 400),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1, 1\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: +1\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 18446744073709551616\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048577\r\n\r\n",
            413,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: gzip\r\n\r\n",
            501,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked, chunked\r\n\r\n",
            400,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nExpect: 100-continue\r\nContent-Length: 10\r\n\r\n",
            417,
        ),
        (
            "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: content-length\r\n\r\n",
            400,
        ),
        (
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\r\n",
            501,
        ),
        (
            "POST / HTTP/1.1\r\nHost: localhost\r\nTrailer: Authorization\r\nTransfer-Encoding: chunked\r\n\r\n",
            400,
        ),
    ];
    let mut main = String::from(
        "\nfn check(input &string, expected u16) {\n match _parse_head(input.bytes(), Limits.defaults()) {\n Ok(head) => { if expected != u16(200) { panic(\"accepted malformed head\") } }\n Err(error) => { if error.status() != expected { print(input)\n print(error.status())\n panic(\"wrong rejection\") } }\n }\n}\nfn main() {\n",
    );
    for v in valid {
        main.push_str(&format!("check(&{v:?}, u16(200))\n"));
    }
    for (v, code) in invalid {
        main.push_str(&format!("check(&{v:?}, u16({code}))\n"));
    }
    for v in ["a", "0", "000f;name=token", "1; name=\"x\\\"y\";flag"] {
        main.push_str(&format!("match _chunk_size({v:?}.bytes()) {{ Ok(n) => {{}}\n Err(e) => {{ panic(\"valid chunk rejected\") }} }}\n"));
    }
    for v in [
        "",
        "-1",
        "10000000000000000",
        "1x",
        "1;",
        "1;a=",
        "1;a=\"unterminated",
        "1;a=\"x\r\n\"",
    ] {
        main.push_str(&format!("match _chunk_size({v:?}.bytes()) {{ Err(e) => {{}}\n Ok(n) => {{ panic(\"invalid chunk accepted\") }} }}\n"));
    }
    main.push_str(r#"
    var l = Limits.defaults()
    l.request_line = usize(16)
    match _parse_head("GET / HTTP/1.1\r\nHost: localhost\r\n\r\n".bytes(), l) { Ok(h) => {}
        Err(e) => { panic("exact limit failed") } }
    l.request_line = usize(15)
    match _parse_head("GET / HTTP/1.1\r\nHost: localhost\r\n\r\n".bytes(), l) { Err(e) => { if e.status() != u16(414) { panic("line status") } }
        Ok(h) => { panic("line limit bypass") } }
    response := (try Response.text(u16(200), &"abc"))
    normal := try _encode(&response, Limits.defaults(), false, false)
    if !_eq(normal.as_slice(), "HTTP/1.1 200 OK\r\ncontent-length: 3\r\ncontent-type: text/plain; charset=utf-8\r\n\r\nabc".bytes()) { panic("normal response wire") }
    head := try _encode(&response, Limits.defaults(), true, true)
    if !_eq(head.as_slice(), "HTTP/1.1 200 OK\r\ncontent-length: 3\r\nconnection: close\r\ncontent-type: text/plain; charset=utf-8\r\n\r\n".bytes()) { panic("HEAD response wire") }
    for status in [3]u16{204, 304, 205} {
        empty := try Response.bytes(status, Vec.new())
        wire := try _encode(&empty, Limits.defaults(), false, false)
        if status == u16(205) {
            if !_eq(wire.as_slice(), "HTTP/1.1 205 Reset Content\r\ncontent-length: 0\r\n\r\n".bytes()) { panic("205 wire") }
        } else { if _find(wire.as_slice(), u8(58)) < wire.len() { panic("body-forbidden header") } }
        nonempty := try Response.text(status, &"x")
        match _encode(&nonempty, Limits.defaults(), false, false) { Err(e) => {}
            Ok(v) => { panic("body forbidden") } }
    }
    for name in [5]string{"content-length", "transfer-encoding", "connection", "trailer", "upgrade"} {
        h := try Header.text(&name, &"x")
        match (try Response.bytes(u16(200), Vec.new())).header(h) { Err(e) => {}
            Ok(r) => { panic("reserved field bypass") } }
    }

    var limits = Limits.defaults()
    input := "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"
    limits.head = input.len()
    custom(&input, limits, u16(200))
    limits.head = limits.head - usize(1)
    custom(&input, limits, u16(431))
    limits = Limits.defaults()
    limits.line = "Host: localhost\r\n".len()
    custom(&input, limits, u16(200))
    limits.line = limits.line - usize(1)
    custom(&input, limits, u16(431))
    limits = Limits.defaults()
    limits.headers = usize(1)
    custom(&input, limits, u16(200))
    custom(&"GET / HTTP/1.1\r\nHost: localhost\r\nX: y\r\n\r\n", limits, u16(431))
    limits = Limits.defaults()
    limits.response_body = usize(3)
    try _encode(&response, limits, false, false)
    limits.response_body = usize(2)
    match _encode(&response, limits, false, false) { Err(e) => {}
        Ok(v) => { panic("response body limit") } }
    limits = Limits.defaults()
    limits.response_head = normal.len() - usize(3)
    try _encode(&response, limits, false, false)
    limits.response_head = limits.response_head - usize(1)
    match _encode(&response, limits, false, false) { Err(e) => {}
        Ok(v) => { panic("response head limit") } }
    limits = Limits.defaults()
    limits.response_headers = usize(1)
    try _encode(&response, limits, false, false)
    extra := try Response.text(u16(200), &"abc")
    with_extra := try extra.header(try Header.text(&"x", &"y"))
    match _encode(&with_extra, limits, false, false) { Err(e) => {}
        Ok(v) => { panic("response header count") } }
    match Header.text(&"x", &"inject\r\nforged: yes") { Err(e) => {}
        Ok(h) => { panic("response splitting") } }
    binary := try Header.new(&"x-binary", &[1]u8{255})
    match binary.value_text() { Err(e) => {}
        Ok(text) => { panic("lossy header text") } }
    print("codec ok")
}
"#);
    // The test main uses try, with an explicit HTTP error rather than a new main ABI.
    main = main
        .replace("fn main() {", "fn codec() Result<void, Error> {")
        .replace("print(\"codec ok\")", "print(\"codec ok\")\n return Ok(())");
    main.push_str("\nfn custom(input &string, limits Limits, expected u16) { match _parse_head(input.bytes(), limits) { Ok(h) => { if expected != u16(200) { panic(\"limit bypass\") } }\n Err(e) => { if e.status() != expected { panic(\"limit diagnostic\") } } } }\n");
    main.push_str(
        "\nfn main() { match codec() { Ok(v) => {}\n Err(e) => { panic(\"codec error\") } } }\n",
    );
    let exe = compile(
        &(include_str!("../../../stdlib/http/http.tarn").to_owned() + &main),
        "codec",
    );
    let out = Command::new("timeout")
        .args(["20s"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"codec ok\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn real_keep_alive_pipelining_binary_chunking_and_fragmentation() {
    let exe = compile(&example(5, 128, None), "loopback");
    let server = Server::start(&exe);
    // Coalesced HEAD + POST verifies suppression, ordering and retained bytes.
    let mut peer = server.connect();
    peer.write_all(b"HEAD /health HTTP/1.1\r\nHost: localhost\r\n\r\nPOST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\0\xffab").unwrap();
    let mut peer = BufReader::new(peer);
    let head = response(&mut peer, true);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert!(
        head.headers
            .contains(&("content-length".into(), b"3".to_vec()))
    );
    let echo = response(&mut peer, false);
    assert_eq!(echo.body, b"\0\xffab");
    closed(&mut peer);
    let mut peer = server.connect();
    for byte in b"POST /echo HTTP/1.1\r\nHost: [::1]\r\nTransfer-Encoding: ChUnKeD\r\nTrailer: X-Checksum\r\nConnection: close\r\n\r\n2;f=\"x\"\r\n\0\xff\r\n2\r\nab\r\n0\r\nX-Checksum: yes\r\n\r\n" { peer.write_all(&[*byte]).unwrap(); }
    let mut peer = BufReader::new(peer);
    assert_eq!(response(&mut peer, false).body, b"\0\xffab");
    closed(&mut peer);
    for request in [
        b"OPTIONS * HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".as_slice(),
        b"PUT /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    ] {
        let mut peer = server.connect();
        peer.write_all(request).unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let mut peer = BufReader::new(peer);
        let result = response(&mut peer, false);
        assert_eq!(
            result.status,
            if request.starts_with(b"OPTIONS") {
                404
            } else if request.starts_with(b"PUT") {
                405
            } else {
                200
            }
        );
        closed(&mut peer);
    }
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn peer_errors_reject_once_without_waiting_for_expect_body_or_reusing_input() {
    let cases: Vec<(Vec<u8>, u16)> = vec![
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nExpect: 100-continue\r\nContent-Length: 100\r\n\r\n".to_vec(),417),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\nxGET /health HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),400),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx!\r\n0\r\n\r\n".to_vec(),400),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nHost: evil\r\n\r\n".to_vec(),400),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\nx".to_vec(),400),
        (b"GET / HTTP/1.1\r\nHost: localhost\r\nX: \0\r\n\r\n".to_vec(),400),
        (b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n".to_vec(),505),
        (b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048577\r\n\r\n".to_vec(),413),
    ];
    let exe = compile(&example(cases.len(), 128, None), "rejections");
    let server = Server::start(&exe);
    for (request, status) in cases {
        let mut peer = server.connect();
        peer.write_all(&request).unwrap();
        // Except Expect: the server must reject that without a body or EOF.
        if status != 417 {
            match peer.shutdown(Shutdown::Write) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotConnected | std::io::ErrorKind::ConnectionReset
                    ) => {}
                Err(e) => panic!("shutdown failed: {e}"),
            }
        }
        let mut peer = BufReader::new(peer);
        let result = response(&mut peer, false);
        assert_eq!(result.status, status);
        assert!(result.body.is_empty());
        assert!(
            result
                .headers
                .contains(&("connection".into(), b"close".to_vec()))
        );
        rejected_closed(&mut peer);
    }
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn deterministic_concurrent_clients_and_admission_backpressure() {
    let exe = compile(&example(40, 8, None), "stress");
    let server = Server::start(&exe);
    let port = server.port;
    let clients: Vec<_> = (0..40).map(|id| thread::spawn(move || {
        let mut peer = BufReader::new(connect(port));
        for n in 0..25 {
            let body = format!("client-{id}-request-{n}");
            let close = if n == 24 { "Connection: close\r\n" } else { "" };
            let request = format!("POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{close}\r\n{body}",body.len());
            peer.get_mut().write_all(request.as_bytes()).unwrap();
            let reply = response(&mut peer,false); assert_eq!(reply.status,200); assert_eq!(reply.body,body.as_bytes());
        }
        closed(&mut peer);
    })).collect();
    for client in clients {
        client.join().unwrap();
    }
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn whole_stage_deadlines_close_slow_head_and_body_and_do_not_block_other_tasks() {
    let exe = compile(&example(3, 128, Some(300)), "deadlines");
    let server = Server::start(&exe);
    let mut slow_head = BufReader::new(server.connect());
    slow_head
        .get_mut()
        .write_all(b"GET /health HTTP/1.1\r\nHost:")
        .unwrap();
    let mut slow_body = BufReader::new(server.connect());
    slow_body
        .get_mut()
        .write_all(b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\nx")
        .unwrap();
    let mut fast = BufReader::new(server.connect());
    fast.get_mut()
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    assert_eq!(response(&mut fast, false).body, b"ok\n");
    closed(&mut fast);
    assert_eq!(response(&mut slow_head, false).status, 408);
    closed(&mut slow_head);
    assert_eq!(response(&mut slow_body, false).status, 408);
    closed(&mut slow_body);
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn every_request_fragment_boundary_and_large_partial_writes_match_wire_oracle() {
    let input = b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\n\0\xffab";
    let exe = compile(&example(input.len() + 1, 128, None), "splits");
    let server = Server::start(&exe);
    for split in 0..=input.len() {
        let mut peer = server.connect();
        peer.write_all(&input[..split]).unwrap();
        peer.write_all(&input[split..]).unwrap();
        let mut reader = BufReader::new(peer);
        let reply = response(&mut reader, false);
        assert_eq!(reply.body, b"\0\xffab");
        closed(&mut reader);
    }
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    let exe = compile(&example(1, 128, None), "large-write");
    let server = Server::start(&exe);
    let body: Vec<u8> = (0..1_048_576).map(|i| (i % 251) as u8).collect();
    let mut peer = server.connect();
    peer.write_all(b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n").unwrap();
    peer.write_all(&body).unwrap();
    let mut reader = BufReader::with_capacity(257, peer);
    assert_eq!(response(&mut reader, false).body, body);
    closed(&mut reader);
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn write_deadline_abandons_pending_output_without_a_second_response() {
    let source = example(1,128,Some(500)).replace("return http.Response.bytes(u16(404), Vec.new())", "var bytes = Vec.new()\n    for i in 0..1048576 { bytes.push(u8(120)) }\n    return http.Response.bytes(u16(200), bytes)");
    let source = source.replace("Err(error) => { return }", "Err(error) => { match error { http.Error.Timeout(http.Stage.Write) => { print(\"write timeout\") }\n _ => { panic(\"unexpected write failure\") } }\n return }");
    let exe = compile(&source, "write-deadline");
    let mut server = Server::start(&exe);
    server.expected_stdout = b"write timeout\n";
    let mut peer = server.connect();
    // A small receive window and many tiny requests force send-buffer pressure.
    unsafe extern "C" {
        fn setsockopt(fd: i32, level: i32, name: i32, value: *const i32, length: u32) -> i32;
    }
    use std::os::fd::AsRawFd;
    let size = 1024i32;
    let (sol_socket, so_rcvbuf) = if cfg!(target_os = "macos") { (0xffff, 0x1002) } else { (1, 8) };
    assert_eq!(unsafe { setsockopt(peer.as_raw_fd(), sol_socket, so_rcvbuf, &size, 4) }, 0);
    let request = b"GET /large HTTP/1.1\r\nHost: localhost\r\n\r\n";
    for _ in 0..20 {
        peer.write_all(request).unwrap();
    }
    // Completion is observed from the server, not inferred from a sleep.
    let trace = server.finish();
    assert!(String::from_utf8_lossy(&trace).contains("net:close:"));
    // The exact timeout outcome was reported above; inspect only the prefix
    // rather than draining a deliberately tiny window as a timing assertion.
    let mut prefix = [0; 48];
    peer.read_exact(&mut prefix).unwrap();
    assert!(prefix.starts_with(b"HTTP/1.1 200 OK\r\ncontent-length: 1048576\r\n"));
    drop(peer);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn dropped_pending_http_frame_releases_socket_timer_and_result_storage() {
    let source = example(1,128,None).replace("tasks.push(owner.spawn_async(handle(stream, owner)))", "var conn: http.Connection\n        match http.Connection.new(stream, http.Limits.defaults(), http.Timeouts.defaults()) { Ok(value) => { conn = value }\n            Err(error) => { panic(\"connection\") } }\n        var pending = runtime.Operation.new(owner, conn.read_request(owner))\n        match pending.poll() { io.Progress.Pending => {}\n            _ => { panic(\"read should be pending\") } }\n        pending.finish()\n        moved := conn\n        moved.close()");
    let exe = compile(&source, "abandon");
    let server = Server::start(&exe);
    let mut peer = server.connect();
    let mut byte = [0];
    assert_eq!(peer.read(&mut byte).unwrap(), 0);
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn configured_body_chunk_and_trailer_limits_are_enforced_at_boundaries() {
    let mut cases: Vec<(Vec<u8>,u16,Vec<u8>)> = vec![
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\n\r\nabcd".to_vec(),200,b"abcd".to_vec()),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\n".to_vec(),413,vec![]),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n2\r\ncd\r\n0\r\nX: yes\r\n\r\n".to_vec(),200,b"abcd".to_vec()),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n".to_vec(),413,vec![]),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n1\r\nb\r\n1\r\nc\r\n0\r\n\r\n".to_vec(),413,vec![]),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX: yes\r\nY: yes\r\n\r\n".to_vec(),431,vec![]),
        (b"POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n1;long=123456789012345678901234567890\r\na\r\n0\r\n\r\n".to_vec(),413,vec![]),
    ];
    for (extra, status) in [(21, 200), (22, 413)] {
        let request = format!(
            "POST /echo HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n1;a={}\r\na\r\n1;b={}\r\nb\r\n0\r\n\r\n",
            "x".repeat(22),
            "y".repeat(extra)
        );
        cases.push((
            request.into_bytes(),
            status,
            if status == 200 {
                b"ab".to_vec()
            } else {
                vec![]
            },
        ));
    }
    let source =
        example(cases.len(), 128, None).replace("http.Limits.defaults()", "small_limits()");
    let source = source
        + r#"
fn small_limits() http.Limits {
    var limits = http.Limits.defaults()
    limits.body = usize(4)
    limits.line = usize(32)
    limits.chunk_metadata = usize(64)
    limits.chunks = usize(2)
    limits.trailers = usize(1)
    limits.buffer = usize(4)
    return limits
}
"#;
    let exe = compile(&source, "limits");
    let server = Server::start(&exe);
    for (request, status, body) in cases {
        let mut peer = server.connect();
        peer.write_all(&request).unwrap();
        if status == 200 {
            peer.shutdown(Shutdown::Write).unwrap();
        }
        let mut reader = BufReader::new(peer);
        let result = response(&mut reader, false);
        assert_eq!(result.status, status);
        assert_eq!(result.body, body);
        if status == 200 {
            closed(&mut reader);
        } else {
            rejected_closed(&mut reader);
        }
    }
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn request_resources_drop_once_on_success_and_early_body_timeout() {
    let exe = compile(&example(2, 128, Some(300)), "owned-traces");
    // Enable the existing string destruction oracle for this server only.
    let server = Server::start(&exe);
    let mut first = server.connect();
    first
        .write_all(
            b"GET /unique-normal-drop HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    let mut first = BufReader::new(first);
    assert_eq!(response(&mut first, false).status, 404);
    closed(&mut first);
    let mut second = server.connect();
    second
        .write_all(
            b"POST /unique-timeout-drop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\nx",
        )
        .unwrap();
    let mut second = BufReader::new(second);
    assert_eq!(response(&mut second, false).status, 408);
    closed(&mut second);
    let trace = server.finish();
    let text = String::from_utf8(trace).unwrap();
    for marker in ["drop:/unique-normal-drop", "drop:/unique-timeout-drop"] {
        assert_eq!(text.lines().filter(|l| *l == marker).count(), 1, "{marker}");
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn trailers_remain_separate_and_chunked_leftovers_form_the_next_request() {
    let source = example(1, 128, None).replace(
        "fn reply(request http.Request) Result<http.Response, http.Error> {",
        r#"fn reply(request http.Request) Result<http.Response, http.Error> {
    if request.target() == &"/trailer" {
        match request.header(&"x-checksum") { None => {}
            Some(h) => { panic("trailer replaced main header") } }
        if request.trailers().len() != usize(1) { panic("missing trailer") }
        trailer := request.trailers()
        header := try http.Header.new(&"x-echo-trailer", trailer[0].value())
        body := request.into_body()
        return (try http.Response.bytes(u16(200), body)).header(header)
    }
"#,
    );
    let exe = compile(&source, "trailers");
    let server = Server::start(&exe);
    let mut peer = server.connect();
    peer.write_all(b"POST /trailer HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\nX-Checksum: yes\r\n\r\nGET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut reader = BufReader::new(peer);
    let first = response(&mut reader, false);
    assert_eq!(first.body, b"x");
    assert!(
        first
            .headers
            .contains(&("x-echo-trailer".into(), b"yes".to_vec()))
    );
    assert_eq!(response(&mut reader, false).body, b"ok\n");
    closed(&mut reader);
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn connection_transaction_order_and_request_cap_use_protocol_state_only() {
    let source = example(1, 128, None).replace("http.Limits.defaults()", "two_requests()");
    let source = source.replace(
        "    for {\n        var request: http.Request",
        r#"
    invalid := await conn.write_response(owner, reply_dummy())
    match invalid { Err(http.Error.InvalidState) => {}
        _ => { panic("write before request accepted") } }
    invalid_reject := await conn.reject(owner, u16(200))
    match invalid_reject { Err(http.Error.InvalidResponse) => {}
        _ => { panic("non-error rejection accepted") } }
    for {
        var request: http.Request"#,
    );
    let source = source.replace(
        "        var response: http.Response",
        r#"
        overlapping := await conn.read_request(owner)
        match overlapping { Err(http.Error.InvalidState) => {}
            _ => { panic("read before response accepted") } }
        var response: http.Response"#,
    );
    let source = source
        + r#"
fn two_requests() http.Limits { var limits = http.Limits.defaults()
    limits.requests = usize(2)
    return limits
}
fn reply_dummy() http.Response {
    match http.Response.bytes(u16(200), Vec.new()) { Ok(value) => { return value }
        Err(e) => { panic("dummy") } }
}
"#;
    let exe = compile(&source, "state");
    let server = Server::start(&exe);
    let mut peer = server.connect();
    peer.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\nGET /health HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut reader = BufReader::new(peer);
    let first = response(&mut reader, false);
    assert!(first.headers.iter().all(|(name, _)| name != "connection"));
    let second = response(&mut reader, false);
    assert!(
        second
            .headers
            .contains(&("connection".into(), b"close".to_vec()))
    );
    closed(&mut reader);
    server.finish();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn cleanup_oracle_rejects_missing_and_duplicate_destruction_events() {
    balanced(b"net:open:3\nnet:close:3\nexec:storage:alloc:abc\nexec:storage:free:abc\n");
    for trace in [
        b"net:open:3\n".as_slice(),
        b"net:open:3\nnet:close:3\nnet:close:3\n",
        b"exec:storage:alloc:abc\n",
        b"exec:waker:alloc:abc\nexec:waker:free:abc\nexec:waker:free:abc\n",
        b"exec:task:alloc:abc\n",
    ] {
        assert!(std::panic::catch_unwind(|| balanced(trace)).is_err());
    }
}

/// Phase 17 `http.serve`: one request per connection, handler state shared by
/// mutable capture, handler errors and protocol errors answered once, JSON and
/// empty responses framed by the library.
#[test]
fn serve_runs_a_mutable_handler_sequentially_and_closes_each_connection() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let source = format!(
        r#"import "http"
import "io"
import "string"
fn main() Result<void, io.Error> {{
    var hits: u64 = 0
    return http.serve(&"127.0.0.1:{port}", fn(request &http.Request) Result<http.Response, http.Error> {{
        hits = hits + 1
        if request.target() == &"/fail" {{ return Err(http.Error.InvalidState) }}
        if request.target() == &"/empty" {{ return http.Response.empty(204) }}
        if request.method() != &"GET" {{ return http.Response.method_not_allowed(&"GET") }}
        return http.Response.json(200, "{{\"hits\":" + &string.from_u64(hits) + &"}}")
    }})
}}
"#
    );
    let exe = compile(&source, "serve");
    let mut child = Command::new(&exe).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    let connect_ready = || loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            return stream;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "server did not bind");
        // Startup polling only.
        thread::sleep(Duration::from_millis(5));
    };
    let exchange = |request: &[u8]| {
        let mut peer = connect_ready();
        peer.write_all(request).unwrap();
        let mut reader = BufReader::new(peer);
        let reply = response(&mut reader, false);
        reply
    };
    let first = exchange(b"GET /a HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(first.status, 200);
    assert_eq!(first.body, b"{\"hits\":1}");
    assert!(first.headers.contains(&("content-type".into(), b"application/json".to_vec())));
    assert!(first.headers.contains(&("connection".into(), b"close".to_vec())));
    // A pipelined second request is not answered: each response closes.
    let mut peer = connect_ready();
    peer.write_all(b"GET /b HTTP/1.1\r\nHost: localhost\r\n\r\nGET /c HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut reader = BufReader::new(peer);
    assert_eq!(response(&mut reader, false).body, b"{\"hits\":2}");
    rejected_closed(&mut reader);
    let empty = exchange(b"GET /empty HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(empty.status, 204);
    assert!(empty.body.is_empty());
    let post = exchange(b"POST /a HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(post.status, 405);
    assert!(post.headers.contains(&("allow".into(), b"GET".to_vec())));
    assert_eq!(exchange(b"GET /fail HTTP/1.1\r\nHost: localhost\r\n\r\n").status, 500);
    assert_eq!(exchange(b"GET /a HTTP/1.1\r\n\r\n").status, 400);
    // The server still answers after handler and protocol failures.
    // The server still answers after handler and protocol failures; every
    // request reached the handler except the one rejected before parsing.
    assert_eq!(exchange(b"GET /a HTTP/1.1\r\nHost: localhost\r\n\r\n").body, b"{\"hits\":6}");
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

/// The infallible `http.json/text/empty/method_not_allowed` shorthands abort on
/// an invalid fixed status: a programming error, not a peer or I/O error.
#[test]
fn fixed_response_shorthands_abort_on_invalid_status() {
    for (name, call) in [("status_low", "http.empty(99)"), ("status_high", "http.json(600, \"{}\")"), ("allow_control", "http.method_not_allowed(&\"GET\\n\")")] {
        let exe = compile(&format!("import \"http\"\nfn main() {{\n    r := {call}\n    print(r.status())\n}}\n"), name);
        let out = Command::new(&exe).output().unwrap();
        assert!(!out.status.success(), "{name} must abort");
        assert!(String::from_utf8_lossy(&out.stderr).contains("panic: http: invalid fixed response"), "{name}");
        assert!(out.stdout.is_empty(), "{name}");
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

/// Phase 30: `serve_parallel` runs workers on SO_REUSEPORT listeners and a
/// shared `Handler`; concurrent clients are all answered and the Mutex-held
/// counter loses no increment.
#[test]
fn serve_parallel_answers_concurrent_clients_with_shared_state() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let source = format!(
        r#"import "http"
import "io"
import "string"
struct App {{
    hits Mutex<u64>
}}
impl http.Handler for App {{
    fn handle(&self, request &http.Request) Result<http.Response, http.Error> {{
        var guard = self.hits.lock()
        next := guard.read() + 1
        previous := guard.replace(next)
        return Ok(http.text(200, &string.from_u64(next)))
    }}
}}
fn main() Result<void, io.Error> {{
    app := App{{hits: Mutex.new(u64(0))}}
    return http.serve_parallel("127.0.0.1:{port}", 4, &app)
}}
"#
    );
    let exe = compile(&source, "serve-parallel");
    let mut child = Command::new(&exe).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed() < Duration::from_secs(10), "server did not bind");
        thread::sleep(Duration::from_millis(5));
    }
    // A plain listener still cannot take the shared address.
    assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
    let clients: Vec<_> = (0..8)
        .map(|_| {
            thread::spawn(move || {
                let mut seen = Vec::new();
                for _ in 0..50 {
                    let mut peer = connect(port);
                    peer.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
                    let mut reader = BufReader::new(peer);
                    let reply = response(&mut reader, false);
                    assert_eq!(reply.status, 200);
                    seen.push(String::from_utf8(reply.body).unwrap().parse::<u64>().unwrap());
                }
                seen
            })
        })
        .collect();
    let mut all: Vec<u64> = clients.into_iter().flat_map(|c| c.join().unwrap()).collect();
    all.sort_unstable();
    // 400 requests saw 400 distinct counter values 1..=400: no lost update.
    assert_eq!(all, (1..=400).collect::<Vec<_>>());
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn cooperative_workers_progress_past_slow_clients_and_enforce_capacity() {
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for limit in [1, 2] {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let source = format!(
            r#"import "http"
import "io"
struct App {{}}
impl http.Handler for App {{
    fn handle(&self, request &http.Request) Result<http.Response, http.Error> {{
        return http.Response.text(200, "ready")
    }}
}}
fn main() Result<void, io.Error> {{
    app := App{{}}
    return http.serve_parallel_with_limit("127.0.0.1:{port}", 1, {limit}, &app)
}}
"#
        );
        let exe = compile(&source, &format!("cooperative-limit-{limit}"));
        let server = Running(
            Command::new(&exe)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        let mut slow = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(peer) => break peer,
                Err(_) => {
                    assert!(
                        start.elapsed() < Duration::from_secs(10),
                        "server did not bind"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            }
        };
        // First connection never completes its request head. One worker must
        // still answer the next client when it has spare cooperative capacity.
        slow.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
        let mut fast = connect(port);
        fast.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut reader = BufReader::new(fast);
        if limit == 1 {
            reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            let mut byte = [0];
            let error = reader
                .read(&mut byte)
                .expect_err("capacity-one worker accepted another active connection");
            assert!(matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ));
            drop(slow);
        } else {
            reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
        }
        if limit == 1 {
            reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
        }
        let reply = response(&mut reader, false);
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"ready");
        drop(server);
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

#[test]
fn cooperative_server_rejects_zero_workers_or_capacity_before_binding() {
    let exe = compile(
        r#"import "http"
import "io"
struct App {}
impl http.Handler for App {
    fn handle(&self, request &http.Request) Result<http.Response, http.Error> {
        return http.Response.text(200, "ready")
    }
}
fn main() {
    app := App{}
    for capacity in [2]usize{0, 1} {
        workers := usize(1) - capacity
        match http.serve_parallel_with_limit("not an address", workers, capacity, &app) {
            Ok(_) => { panic("invalid configuration accepted") }
            Err(error) => {
                match error.kind {
                    io.ErrorKind.InvalidInput => {}
                    _ => { panic("configuration was not checked before binding") }
                }
            }
        }
    }
    print("invalid limits rejected")
}
"#,
        "cooperative-invalid-limits",
    );
    let output = Command::new(&exe).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"invalid limits rejected\n");
    assert!(output.stderr.is_empty());
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
