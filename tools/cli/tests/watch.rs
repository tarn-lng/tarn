//! Exercise a live check session with imports, failures and source-set refresh.
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

struct Session {
    child: Child,
    stderr: Receiver<String>,
    stdout: Receiver<String>,
}

fn lines(reader: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    receiver
}

impl Session {
    fn start(entry: &Path, json: bool) -> Self {
        Self::start_command(entry, json, "check")
    }
    fn start_command(entry: &Path, json: bool, kind: &str) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tarn"));
        command.args([kind, "--watch"]).arg(entry);
        if json {
            command.arg("--json");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = lines(child.stderr.take().unwrap());
        let stdout = lines(child.stdout.take().unwrap());
        Self {
            child,
            stderr,
            stdout,
        }
    }

    fn wait(&self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let line = self
                .stderr
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("missing {expected}; output: {seen:?}"));
            if line.contains(expected) {
                return;
            }
            seen.push(line);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            kill(self.child.id() as i32, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.wait();
    }
}

#[test]
fn watches_imports_recovers_and_refreshes_the_loaded_set() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    let helper = dir.join("helper.tarn");
    std::fs::write(&helper, "pub fn value() i32 { return 1 }\n").unwrap();
    std::fs::write(
        &entry,
        "import \"helper\"\nfn main() { print(helper.value()) }\n",
    )
    .unwrap();
    let mut session = Session::start(&entry, false);
    session.wait("check succeeded");
    std::fs::write(&helper, "pub fn value() i32 { return unknown_name }\n").unwrap();
    session.wait("helper.tarn");
    session.wait("check failed");
    assert!(session.child.try_wait().unwrap().is_none());
    std::fs::remove_file(&helper).unwrap();
    session.wait("check failed");
    std::fs::write(&helper, "pub fn value() i32 { return 123 }\n").unwrap();
    session.wait("check succeeded");
    let next = dir.join("next.tarn");
    std::fs::write(&next, "pub fn value() i32 { return 2 }\n").unwrap();
    std::fs::write(
        &entry,
        "import \"next\"\nfn main() { print(next.value()) }\n",
    )
    .unwrap();
    session.wait("check succeeded");
    std::fs::write(&helper, "broken unused source").unwrap();
    assert!(
        session
            .stderr
            .recv_timeout(Duration::from_millis(900))
            .is_err()
    );
    std::fs::write(&next, "pub fn value() i32 { return another_unknown }\n").unwrap();
    session.wait("check failed");
    drop(session);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn missing_entry_recovers_and_json_stdout_contains_only_records() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-watch-json-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    let session = Session::start(&entry, true);
    session.wait("check failed");
    let record = session.stdout.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(record.starts_with("{\"kind\":\"command_error\",\"stage\":\"load\""));
    std::fs::write(&entry, "fn main() { print(unknown_name) }\n").unwrap();
    session.wait("check failed");
    let record = session.stdout.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(record.starts_with("{\"code\":"));
    assert!(record.contains("\"severity\":\"error\""));
    std::fs::write(&entry, "fn main() {}\n").unwrap();
    session.wait("check succeeded");
    assert!(
        session
            .stdout
            .recv_timeout(Duration::from_millis(750))
            .is_err()
    );
    drop(session);
    std::fs::remove_dir_all(dir).unwrap();
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

fn server(label: &str, ignore_term: bool) -> String {
    format!(
        "import \"ffi\"\nextern \"C\" fn fflush(stream *mut u8) i32\nextern \"C\" fn getpid() i32\nextern \"C\" fn signal(number i32, handler usize) usize\nfn main() {{\n unsafe {{\n {}\n print(getpid())\n }}\n print(\"{label}\")\n unsafe {{ flushed := fflush(ffi.null()) }}\n for {{}}\n}}\n",
        if ignore_term {
            "ignored := signal(15, 1)"
        } else {
            ""
        }
    )
}

#[test]
fn run_watch_preserves_failed_builds_and_kills_uncooperative_children() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-run-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, server("first", true)).unwrap();
    let session = Session::start_command(&entry, false, "run");
    session.wait("program started");
    let first_pid: i32 = session
        .stdout
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        session.stdout.recv_timeout(Duration::from_secs(3)).unwrap(),
        "first"
    );
    std::fs::write(&entry, "fn main() { print(unknown_name) }\n").unwrap();
    session.wait("previous program retained");
    assert_eq!(unsafe { kill(first_pid, 0) }, 0);
    std::fs::write(&entry, "extern \"C\" fn tarn_missing_watch_symbol() i32\nfn main() { unsafe { print(tarn_missing_watch_symbol()) } }\n").unwrap();
    session.wait("previous program retained");
    assert_eq!(unsafe { kill(first_pid, 0) }, 0);
    let started = Instant::now();
    std::fs::write(&entry, server("second", false)).unwrap();
    session.wait("program started");
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_ne!(unsafe { kill(first_pid, 0) }, 0);
    let second_pid: i32 = session
        .stdout
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        session.stdout.recv_timeout(Duration::from_secs(3)).unwrap(),
        "second"
    );
    drop(session);
    assert_ne!(
        unsafe { kill(second_pid, 0) },
        0,
        "shutdown must reap the program"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_watch_observes_added_removed_and_changed_test_files() {
    let dir = std::env::temp_dir().join(format!("tarn-cli-test-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("main.tarn");
    std::fs::write(&entry, "fn main() {}\nfn test_first() {}\n").unwrap();
    let session = Session::start_command(&dir, false, "test");
    let summary = |expected: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = session
                .stdout
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if line.contains(expected) {
                break;
            }
        }
    };
    summary("1 passed; 0 failed");
    let sibling = dir.join("added_test.tarn");
    std::fs::write(&sibling, "fn test_added(value i32) {}\n").unwrap();
    session.wait("must be a safe");
    std::fs::write(&sibling, "fn test_added() {}\n").unwrap();
    summary("2 passed; 0 failed");
    std::fs::write(&sibling, "fn test_added() { panic(\"watch failure\") }\n").unwrap();
    summary("1 passed; 1 failed");
    std::fs::remove_file(&sibling).unwrap();
    summary("1 passed; 0 failed");
    drop(session);
    std::fs::remove_dir_all(dir).unwrap();
}
