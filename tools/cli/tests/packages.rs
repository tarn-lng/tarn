//! Public package workflow and hostile-data regressions.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    registry: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tarn-packages-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let home = root.join("home");
        let registry = root.join("registry");
        fs::create_dir_all(&registry).unwrap();
        Self {
            root,
            home,
            registry,
        }
    }
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tarn"))
            .current_dir(dir)
            .env("TARN_HOME", &self.home)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, dir: &Path, args: &[&str]) -> Output {
        let o = self.run(dir, args);
        assert!(
            o.status.success(),
            "{args:?}\n{}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }
    fn library(&self, name: &str, version: &str, source: &str, deps: &str) -> PathBuf {
        let dir = self.root.join(format!("{name}-{version}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tarn.toml"),format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nentry = \"lib.tarn\"\n{deps}")).unwrap();
        fs::write(dir.join("lib.tarn"), source).unwrap();
        dir
    }
    fn publish(&self, dir: &Path) {
        if tarn_packages::manifest::Manifest::read(dir)
            .unwrap()
            .dependencies
            .len()
            > 0
        {
            self.ok(dir, &["update"]);
        }
        self.ok(
            dir,
            &["publish", "--registry", self.registry.to_str().unwrap()],
        );
    }
    fn app(&self) -> PathBuf {
        self.ok(&self.root, &["init", "app"]);
        self.root.join("app")
    }
    fn add(&self, app: &Path, name: &str, version: &str) {
        self.ok(
            app,
            &[
                "add",
                name,
                "--version",
                version,
                "--registry",
                self.registry.to_str().unwrap(),
            ],
        );
    }
    fn dependency(&self, name: &str, version: &str) -> String {
        format!(
            "[dependencies]\n{name} = {{ version = \"{version}\", registry = \"{}\" }}\n",
            tarn_packages::source(self.registry.to_str().unwrap(), &self.root).unwrap()
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn installed_style_offline_build_verification_and_explicit_updates() {
    let f = Fixture::new();
    let lib = f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    );
    fs::write(lib.join("build.sh"), "touch should_never_exist").unwrap();
    f.publish(&lib);
    assert!(
        !f.run(
            &lib,
            &["publish", "--registry", f.registry.to_str().unwrap()]
        )
        .status
        .success()
    );
    let app = f.app();
    f.add(&app, "greeting", "^1.0");
    fs::write(
        app.join("main.tarn"),
        "import \"greeting\"\nfn main() { print(greeting.answer()) }\n",
    )
    .unwrap();
    let lock = fs::read(app.join("tarn.lock")).unwrap();
    assert_eq!(f.ok(&app, &["run"]).stdout, b"42\n");
    assert_eq!(fs::read(app.join("tarn.lock")).unwrap(), lock);
    fs::write(app.join("app_test.tarn"), "import \"greeting\"\nfn test_package() { if greeting.answer() != 42 { panic(\"wrong answer\") } }\n").unwrap();
    f.ok(&app, &["test", "--json"]);
    fs::remove_file(app.join("app_test.tarn")).unwrap();
    let newer = f.library(
        "greeting",
        "1.1.0",
        "pub fn answer() i32 { return 43 }\n",
        "",
    );
    f.publish(&newer);
    assert_eq!(f.ok(&app, &["run"]).stdout, b"42\n");
    f.ok(&app, &["update", "greeting"]);
    assert_eq!(f.ok(&app, &["run"]).stdout, b"43\n");
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    let release = &lock.packages["greeting"];
    let cache = tarn_packages::store::cache_root(&f.home, release);
    assert!(!cache.join("build.sh").exists());
    fs::remove_dir_all(&cache).unwrap();
    assert!(!f.run(&app, &["check"]).status.success());
    f.ok(&app, &["fetch"]);
    f.ok(&app, &["verify", "--json"]);
    fs::rename(&f.registry, f.root.join("registry-unavailable")).unwrap();
    assert_eq!(f.ok(&app, &["run"]).stdout, b"43\n");
    fs::write(
        cache.join("lib.tarn"),
        "pub fn answer() i32 { return 99 }\n",
    )
    .unwrap();
    let o = f.run(&app, &["run"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("hash/inventory mismatch"));
    assert!(!f.run(&app, &["fetch"]).status.success());
}
#[test]
fn transitive_graph_module_namespaces_and_solver_backtracking() {
    let f = Fixture::new();
    for version in ["1.0.0", "2.0.0"] {
        f.publish(&f.library("shared", version, "pub fn answer() i32 { return 10 }\n", ""));
    }
    for (version, constraint) in [("1.0.0", "^1"), ("1.1.0", "^2")] {
        let p=f.library("alpha",version,"import \"shared\"\nimport \"util\"\npub fn answer() i32 { return shared.answer() + util.extra() }\n",&f.dependency("shared",constraint));
        fs::write(p.join("util.tarn"), "pub fn extra() i32 { return 1 }\n").unwrap();
        f.publish(&p);
    }
    let beta=f.library("beta","1.0.0","import \"shared\"\nimport \"util\"\npub fn answer() i32 { return shared.answer() + util.extra() }\n",&f.dependency("shared","^1"));
    fs::write(beta.join("util.tarn"), "pub fn extra() i32 { return 2 }\n").unwrap();
    f.publish(&beta);
    let app = f.app();
    f.add(&app, "alpha", "^1");
    f.add(&app, "beta", "^1");
    fs::write(
        app.join("main.tarn"),
        "import \"alpha\"\nimport \"beta\"\nfn main() { print(alpha.answer() + beta.answer()) }\n",
    )
    .unwrap();
    assert_eq!(f.ok(&app, &["run"]).stdout, b"23\n");
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    assert_eq!(lock.packages["alpha"].version.to_string(), "1.0.0");
    assert_eq!(lock.packages.len(), 3);
    assert!(
        String::from_utf8_lossy(&f.ok(&app, &["deps", "--why", "shared"]).stdout)
            .contains("alpha -> shared")
    );
    let before = fs::read(app.join("tarn.toml")).unwrap();
    let oldlock = fs::read(app.join("tarn.lock")).unwrap();
    assert!(
        !f.run(
            &app,
            &[
                "add",
                "shared",
                "--version",
                "^2",
                "--registry",
                f.registry.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert_eq!(fs::read(app.join("tarn.toml")).unwrap(), before);
    assert_eq!(fs::read(app.join("tarn.lock")).unwrap(), oldlock);
    fs::write(
        app.join("main.tarn"),
        "import \"shared\"\nfn main() { print(shared.answer()) }\n",
    )
    .unwrap();
    assert!(!f.run(&app, &["check"]).status.success());
    fs::write(
        app.join("main.tarn"),
        "import \"alpha\"\nimport \"@package/shared\"\nfn main() {}\n",
    )
    .unwrap();
    let internal = f.run(&app, &["check"]);
    assert!(!internal.status.success());
    assert!(String::from_utf8_lossy(&internal.stderr).contains("namespaces are private"));
    f.ok(&app, &["remove", "alpha"]);
    assert_eq!(
        tarn_packages::graph::Lock::read(&app)
            .unwrap()
            .packages
            .len(),
        2
    );
}
#[test]
fn refuse_hooks_symlinks_policy_downgrades_and_unknown_audit() {
    let f = Fixture::new();
    let lib = f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    );
    f.publish(&lib);
    let app = f.app();
    f.add(&app, "greeting", "^1");
    assert_eq!(f.run(&app, &["audit", "--json"]).status.code(), Some(3));
    fs::write(f.registry.join("advisories.json"),"[{\"package\":\"greeting\",\"affected\":\"^1\",\"id\":\"TEST-1\",\"summary\":\"test advisory\"}]").unwrap();
    assert_eq!(f.run(&app, &["audit", "--json"]).status.code(), Some(1));
    fs::write(
        f.home.join("config.toml"),
        "[security]\nrequire_provenance = true\n",
    )
    .unwrap();
    assert!(!f.run(&app, &["verify"]).status.success());
    fs::remove_file(f.home.join("config.toml")).unwrap();
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    let cache = tarn_packages::store::cache_root(&f.home, &lock.packages["greeting"]);
    fs::remove_file(cache.join("lib.tarn")).unwrap();
    std::os::unix::fs::symlink(lib.join("lib.tarn"), cache.join("lib.tarn")).unwrap();
    assert!(!f.run(&app, &["check"]).status.success());
    fs::write(lib.join("tarn.toml"),"[package]\nname=\"greeting\"\nversion=\"1.1.0\"\nentry=\"lib.tarn\"\n[build]\nhook=\"build.sh\"\n").unwrap();
    assert!(
        !f.run(
            &lib,
            &["publish", "--registry", f.registry.to_str().unwrap()]
        )
        .status
        .success()
    );
    assert!(!f.registry.join("greeting/1.1.0").exists());
}
#[test]
fn manifest_entry_and_dependency_main_do_not_replace_application_entry() {
    let f = Fixture::new();
    let lib = f.library(
        "other",
        "1.0.0",
        "pub fn main() { print(99) }\npub fn answer() i32 { return 7 }\n",
        "",
    );
    f.publish(&lib);
    let app = f.app();
    f.add(&app, "other", "^1");
    fs::write(
        app.join("main.tarn"),
        "import \"other\"\nfn work() { other.main() }\n",
    )
    .unwrap();
    assert!(!f.run(&app, &["build"]).status.success());
    fs::create_dir(app.join("src")).unwrap();
    fs::write(
        app.join("src/start.tarn"),
        "import \"other\"\nfn main() { print(other.answer()) }\n",
    )
    .unwrap();
    let manifest = fs::read_to_string(app.join("tarn.toml"))
        .unwrap()
        .replace("main.tarn", "src/start.tarn");
    fs::write(app.join("tarn.toml"), manifest).unwrap();
    f.ok(&app, &["update"]);
    assert_eq!(f.ok(&app, &["run"]).stdout, b"7\n");
}
#[test]
fn https_consumption_validates_tls_and_locked_bytes() {
    let f = Fixture::new();
    f.publish(&f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    ));
    let cert = f.root.join("cert.pem");
    let key = f.root.join("key.pem");
    assert!(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
                "-keyout"
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .output()
            .unwrap()
            .status
            .success()
    );
    let script = "import http.server,ssl,sys\ns=http.server.HTTPServer(('127.0.0.1',0),http.server.SimpleHTTPRequestHandler)\nc=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);c.load_cert_chain(sys.argv[1],sys.argv[2]);s.socket=c.wrap_socket(s.socket,server_side=True)\nprint(s.server_port,flush=True);s.serve_forever()\n";
    let mut server = Command::new("python3")
        .args(["-u", "-c", script])
        .arg(&cert)
        .arg(&key)
        .current_dir(&f.registry)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    struct Server<'a>(&'a mut std::process::Child);
    impl Drop for Server<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    use std::io::BufRead;
    let mut port = String::new();
    std::io::BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let _server = Server(&mut server);
    // The server listens on 127.0.0.1; naming that address avoids depending on
    // how the host resolves `localhost` (some sandboxes map it only to ::1).
    let origin = format!("https://127.0.0.1:{}/", port.trim());
    let app = f.app();
    assert!(
        !f.run(&app, &["add", "greeting", "--registry", &origin])
            .status
            .success()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .current_dir(&app)
        .env("TARN_HOME", &f.home)
        .env("TARN_CA_BUNDLE", &cert)
        .args(["add", "greeting", "--registry", &origin])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(
        app.join("main.tarn"),
        "import \"greeting\"\nfn main() { print(greeting.answer()) }\n",
    )
    .unwrap();
    assert_eq!(f.ok(&app, &["run"]).stdout, b"42\n");
}
#[test]
fn locked_identity_changes_and_release_age_fail_closed() {
    let f = Fixture::new();
    f.publish(&f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    ));
    let app = f.app();
    f.add(&app, "greeting", "^1");
    fs::write(
        f.home.join("config.toml"),
        "[security]\nminimum_release_age=86400\n",
    )
    .unwrap();
    assert!(!f.run(&app, &["check"]).status.success());
    fs::remove_file(f.home.join("config.toml")).unwrap();
    let path = f.registry.join("greeting/1.0.0/release.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["published"] = serde_json::json!(1);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = fs::read(app.join("tarn.lock")).unwrap();
    let output = f.run(&app, &["update"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("immutable release"));
    assert_eq!(before, fs::read(app.join("tarn.lock")).unwrap());
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    let cache = tarn_packages::store::cache_root(&f.home, &lock.packages["greeting"]);
    fs::write(cache.join("unexpected.tarn"), "fn hidden() {}\n").unwrap();
    assert!(!f.run(&app, &["verify"]).status.success());
}
#[test]
fn watch_observes_global_policy_and_verified_dependency_changes() {
    use std::io::{BufRead, BufReader};
    let f = Fixture::new();
    f.publish(&f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    ));
    let app = f.app();
    f.add(&app, "greeting", "^1");
    fs::write(
        app.join("main.tarn"),
        "import \"greeting\"\nfn main() { print(greeting.answer()) }\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tarn"))
        .current_dir(&app)
        .env("TARN_HOME", &f.home)
        .args(["check", "--watch"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let reader = child.stderr.take().unwrap();
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _stop = Stop(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let wait = |expected: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|_| panic!("missing {expected}: {seen:?}"));
            if line.contains(expected) {
                break;
            }
            seen.push(line);
        }
    };
    wait("check succeeded");
    fs::write(
        f.home.join("config.toml"),
        "[security]\nrequire_provenance=true\n",
    )
    .unwrap();
    wait("required signed provenance");
    fs::remove_file(f.home.join("config.toml")).unwrap();
    wait("check succeeded");
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    let source =
        tarn_packages::store::cache_root(&f.home, &lock.packages["greeting"]).join("lib.tarn");
    let original = fs::read(&source).unwrap();
    fs::write(&source, "pub fn answer() i32 { return 999 }\n").unwrap();
    wait("hash/inventory mismatch");
    fs::write(&source, original).unwrap();
    wait("check succeeded");
}
#[test]
fn scoped_specifiers_and_registry_api_protocol() {
    let f = Fixture::new();
    let lib = f.library(
        "greeting",
        "1.0.0",
        "pub fn answer() i32 { return 42 }\n",
        "",
    );
    f.publish(&lib);

    let api_reg = f.root.join("api-registry");
    let api_pkg_dir = api_reg.join("v1/packages/@acme/greeting/1.2.0");
    fs::create_dir_all(&api_pkg_dir).unwrap();

    let archive_src = f.library(
        "greeting",
        "1.2.0",
        "pub fn answer() i32 { return 77 }\n",
        "",
    );
    let archive_path = api_pkg_dir.join("download");
    assert!(
        Command::new("tar")
            .args(["-czf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(&archive_src)
            .arg(".")
            .status()
            .unwrap()
            .success()
    );
    let archive_hash = tarn_packages::hash(&fs::read(&archive_path).unwrap());
    let versions_json = serde_json::json!([
        {
            "version": "1.3.0",
            "hash": archive_hash,
            "yanked": true,
            "protocol": "github-release",
            "metadataUrl": null,
            "dependencies": {}
        },
        {
            "version": "1.2.0",
            "hash": archive_hash,
            "yanked": false,
            "protocol": "github-release",
            "metadataUrl": null,
            "dependencies": {}
        }
    ]);
    fs::write(
        api_reg.join("v1/packages/@acme/greeting/versions"),
        serde_json::to_vec(&versions_json).unwrap(),
    )
    .unwrap();

    let app = f.app();
    f.ok(
        &app,
        &[
            "add",
            "@acme/greeting",
            "--registry",
            api_reg.to_str().unwrap(),
        ],
    );
    fs::write(
        app.join("main.tarn"),
        "import \"greeting\"\nfn main() { print(greeting.answer()) }\n",
    )
    .unwrap();
    assert_eq!(f.ok(&app, &["run"]).stdout, b"77\n");
    let lock = tarn_packages::graph::Lock::read(&app).unwrap();
    let cache = tarn_packages::store::cache_root(&f.home, &lock.packages["greeting"]);
    fs::remove_dir_all(&cache).unwrap();
    f.ok(&app, &["fetch"]);
    assert_eq!(f.ok(&app, &["run"]).stdout, b"77\n");
}
