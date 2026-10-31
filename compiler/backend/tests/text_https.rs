use std::{io::Write, path::{Path, PathBuf}, process::{Command, Stdio}};
fn build(file: &Path, name: &str) -> PathBuf {
    let result = tarn_driver::check(file).unwrap();
    assert!(!result.has_errors(), "{}", result.diagnostics.iter().map(|d| d.render(&result.program.sources)).collect::<String>());
    let dir = std::env::temp_dir().join(format!("tarn-text-https-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    exe
}
fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..") }
#[test]
fn text_cli_counts_utf8_pipe_input_and_rejects_invalid_data() {
    let exe = build(&root().join("examples/textstats/main.tarn"), "cli");
    for (input, expected) in [
        (b"".as_slice(), Some(b"".as_slice())),
        ("café tea café 日本 tea\n".as_bytes(), Some("2\tcafé\n2\ttea\n1\t日本\n".as_bytes())),
        (&[0xff, 0x80], None),
    ] {
        let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        if let Some(expected) = expected { assert!(output.status.success(), "{:?}", output); assert_eq!(output.stdout, expected); }
        else { assert!(!output.status.success()); }
    }
    let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    // Exceed the documented batch bound; early failure may close stdin.
    let _ = child.stdin.take().unwrap().write_all(&vec![b'x'; 8 * 1024 * 1024 + 1]);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "oversized input accepted");
    let source = root().join("tests/native/pass/text_ranges.tarn");
    let ranges = build(&source, "ranges");
    let output = Command::new(&ranges).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(output.stdout, b"text ranges ok\n");
    for executable in [exe, ranges] { std::fs::remove_dir_all(executable.parent().unwrap()).unwrap(); }
}
#[test]
fn https_and_ticket_classifier_use_verified_bounded_tls() {
    let fixture = build(&root().join("tests/native/pass/https_client_fixture.tarn"), "https");
    let classifier = build(&root().join("examples/ticket_classifier/main.tarn"), "classifier");
    let output = Command::new("python3").arg(root().join("tests/integration/https_fixture.py"))
        .arg(&fixture).arg(&classifier).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    for executable in [fixture, classifier] { std::fs::remove_dir_all(executable.parent().unwrap()).unwrap(); }
}

#[test]
fn https_missing_native_dependency_is_an_error() {
    let dir = std::env::temp_dir().join(format!("tarn-https-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("missing.c");
    std::fs::write(&source, r#"
#include <stdint.h>
#include <stddef.h>
void *__wrap_dlopen(const char *name, int flags) { (void)name; (void)flags; return NULL; }
extern int32_t tarn_https_request(const char *, const char *, const char *, const uint8_t *, uint64_t,
    const char *, uint64_t, uint8_t *, uint64_t, uint64_t *, int64_t *);
int main(void) {
    uint8_t buffer[8]; uint64_t used = 99; int64_t status = 99;
    int32_t code = tarn_https_request("https://localhost", "GET", "", (const uint8_t *)"", 0,
        "", 1000, buffer, 8, &used, &status);
    return code == -1 && used == 0 && status == 0 ? 0 : 1;
}
"#).unwrap();
    let exe = dir.join("program");
    let output = Command::new("cc").arg(&source).arg(root().join("runtime/native.c"))
        .args(["-pthread", "-lm", "-Wl,--wrap=dlopen", "-o"]).arg(&exe).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(Command::new(&exe).status().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tls_stream_is_persistent_duplex_verified_and_released_once() {
    let fixture = build(&root().join("tests/native/pass/tls_stream_fixture.tarn"), "tls-stream");
    let output = Command::new("python3").arg(root().join("tests/integration/tls_stream_fixture.py"))
        .arg(&fixture).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(fixture.parent().unwrap()).unwrap();
}

#[test]
fn websocket_client_frames_fragments_closes_and_rejects_violations() {
    let fixture = build(&root().join("tests/native/pass/websocket_fixture.tarn"), "websocket");
    let output = Command::new("python3").arg(root().join("tests/integration/websocket_fixture.py"))
        .arg(&fixture).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(fixture.parent().unwrap()).unwrap();
}

#[test]
fn hmac_sha256_matches_known_vector_and_compares_bytes() {
    let fixture = build(&root().join("tests/native/pass/crypto_hmac_fixture.tarn"), "crypto-hmac");
    let output = Command::new(&fixture).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8\ntrue\nfalse\n");
    std::fs::remove_dir_all(fixture.parent().unwrap()).unwrap();
}

#[test]
fn postgres_tls_negotiates_before_verified_handshake_and_fails_closed() {
    let fixture = build(&root().join("tests/native/pass/postgres_tls_fixture.tarn"), "postgres-tls");
    let output = Command::new("python3").arg(root().join("tests/integration/postgres_tls_fixture.py"))
        .arg(&fixture).output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    std::fs::remove_dir_all(fixture.parent().unwrap()).unwrap();
}
