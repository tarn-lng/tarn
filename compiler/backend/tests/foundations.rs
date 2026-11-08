use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::Write;

fn compile(source: &str, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tarn-foundations-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, source).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(!result.has_errors(), "{}", result.diagnostics.iter().map(|d| d.render(&result.program.sources)).collect::<String>());
    let exe = dir.join("program");
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    exe
}

#[test]
fn console_pipes_utf8_and_separate_output_streams() {
    let exe = compile(r#"import "console"
import "io"
fn main() Result<void, io.Error> {
    var input = console.stdin()
    var out = console.stdout()
    var err = console.stderr()
    var buffer = [32]u8{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0}
    for {
        count := try input.read(&mut buffer)
        if count == 0 { break }
        try out.write_all(&buffer[0..count])
    }
    try err.write_text("stderr ok\n")
    try out.flush()
    try err.flush()
    return Ok(())
}
"#, "console");
    let mut data = vec![0, 255, 128];
    data.extend_from_slice("café 日本\n".repeat(100).as_bytes());
    let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&data).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, data);
    assert_eq!(output.stderr, b"stderr ok\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn console_broken_pipe_is_a_result_without_changing_signal_disposition() {
    let exe = compile(r#"import "console"
import "io"
fn main() Result<void, io.Error> {
    var out = console.stdout()
    var i: usize = 0
    for i < 1000000 {
        match out.write_text("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx") {
            Err(error) => {
                if error.native_code() != 32 { panic("expected EPIPE") }
                var err = console.stderr()
                try err.write_text("broken pipe handled\n")
                return Ok(())
            }
            Ok(done) => {}
        }
        i = i + 1
    }
    panic("pipe unexpectedly stayed open")
}
"#, "broken-pipe");
    let mut child = Command::new(&exe).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stderr, b"broken pipe handled\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn metrics_port_parses_fixture_and_calculates_sorted_rates() {
    let source = r#"import "metrics"
fn check(ok bool) { if !ok { panic("metrics assertion") } }
fn main() {
    before := metrics.parse_network(&"headers\n lo: 1 0 0 0 0 0 0 0 2\n eth1: 200 0 0 0 0 0 0 0 500\n eth0: 100 0 0 0 0 0 0 0 300\n invalid: x\n")
    after := metrics.parse_network(&"eth1: 100 0 0 0 0 0 0 0 600\neth0: 150 0 0 0 0 0 0 0 350\nnew: 9 0 0 0 0 0 0 0 10\n")
    check(before.len() == 2)
    rows := metrics.rates(&before, &after, 500)
    check(rows.len() == 3)
    check(rows.get(0).name == "eth0")
    check(rows.get(0).rx_per_second == 100)
    check(rows.get(0).tx_per_second == 100)
    check(rows.get(1).name == "eth1")
    check(rows.get(1).rx_per_second == 0)
    check(rows.get(1).tx_per_second == 200)
    check(rows.get(2).name == "new")
    check(rows.get(2).rx_per_second == 0)
    print("metrics ok")
}
"#;
    let dir = std::env::temp_dir().join(format!("tarn-foundations-{}-metrics", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("metrics.tarn"), include_str!("../../../examples/status_metrics/metrics.tarn")).unwrap();
    let exe = compile(source, "metrics");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"metrics ok\n");
    std::fs::remove_dir_all(dir).unwrap();
}

// XNU raises a pipe write's SIGPIPE on the process, so an idle thread that does
// not block it must not die for the writer's EPIPE (ADR 0098 regression).
#[test]
fn console_broken_pipe_spares_other_threads_and_restores_the_stream() {
    let dir = std::env::temp_dir().join(format!("tarn-foundations-{}-other-thread", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("other_thread.c");
    std::fs::write(&file, r#"
#ifdef __APPLE__
#define _DARWIN_C_SOURCE
#else
#define _POSIX_C_SOURCE 200809L
#endif
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
extern int64_t tarn_console_write(int32_t, const uint8_t *, uint64_t);
static void *idle(void *unused) { (void)unused; for (;;) pause(); }
int main(void) {
    int fds[2];
    if (pipe(fds) != 0 || dup2(fds[1], 1) < 0) return 1;
    close(fds[0]);
    pthread_t thread;
    if (pthread_create(&thread, NULL, idle, NULL) != 0) return 2;
    for (int i = 0; i < 100; ++i) {
        if (tarn_console_write(1, (const uint8_t *)"x", 1) != -1 || errno != EPIPE) return 5;
    }
#ifdef __APPLE__
    if (fcntl(1, F_GETNOSIGPIPE) != 0) return 6;
#endif
    usleep(50000);
    _exit(0);
}
"#).unwrap();
    let exe = dir.join("other_thread");
    let status = Command::new("cc").arg(&file).arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/native.c"))
        .args(["-pthread", "-lm", "-o"]).arg(&exe).status().unwrap();
    assert!(status.success());
    let status = Command::new("timeout").arg("30s").arg(&exe).status().unwrap();
    assert_eq!(status.code(), Some(0), "{status:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn console_bridge_preserves_pending_sigpipe_and_thread_mask() {
    let dir = std::env::temp_dir().join(format!("tarn-foundations-{}-signals", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("signals.c");
    std::fs::write(&file, r#"
#define _POSIX_C_SOURCE 200809L
#include <signal.h>
#include <pthread.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
extern int64_t tarn_console_write(int32_t, const uint8_t *, uint64_t);
int main(void) {
    int fds[2];
    if (pipe(fds) != 0) return 1;
    close(fds[0]);
    sigset_t blocked, old, pending, after;
    sigemptyset(&blocked); sigaddset(&blocked, SIGPIPE);
    if (pthread_sigmask(SIG_BLOCK, &blocked, &old) != 0) return 2;
    if (raise(SIGPIPE) != 0) return 3;
    if (tarn_console_write(fds[1], (const uint8_t *)"x", 1) != -1 || errno != EPIPE) return 4;
    if (sigpending(&pending) != 0 || !sigismember(&pending, SIGPIPE)) return 5;
    if (pthread_sigmask(SIG_SETMASK, NULL, &after) != 0 || !sigismember(&after, SIGPIPE)) return 6;
    int taken;
    if (sigwait(&blocked, &taken) != 0 || taken != SIGPIPE) return 7;
    if (pthread_sigmask(SIG_SETMASK, &old, NULL) != 0) return 8;
    struct sigaction before, restored;
    if (sigaction(SIGPIPE, NULL, &before) != 0) return 9;
    if (tarn_console_write(fds[1], (const uint8_t *)"x", 1) != -1 || errno != EPIPE) return 10;
    if (sigaction(SIGPIPE, NULL, &restored) != 0 || before.sa_handler != restored.sa_handler) return 11;
    if (pthread_sigmask(SIG_SETMASK, NULL, &after) != 0 || sigismember(&old, SIGPIPE) != sigismember(&after, SIGPIPE)) return 12;
    close(fds[1]);
    return 0;
}
"#).unwrap();
    let exe = dir.join("signals");
    let status = Command::new("cc").arg(&file).arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/native.c"))
        .args(["-pthread", "-lm", "-o"]).arg(&exe).status().unwrap();
    assert!(status.success());
    assert!(Command::new(&exe).status().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
}
