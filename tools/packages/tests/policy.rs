use std::{fs, path::Path};
use tarn_packages::{
    hash,
    manifest::{Manifest, safe_path},
    store,
};
#[test]
fn manifest_rejects_execution_and_traversal_and_accepts_semver_ranges() {
    let base = "[package]\nname='sample'\nversion='1.0.0+build.1'\nentry='src/lib.tarn'\n";
    for requirement in [
        "=1.2.3",
        "^1.2",
        "~1.2",
        "1.*",
        ">=1.2, <2.0",
        "=1.2.3-beta.1",
    ] {
        let text = format!(
            "{base}[dependencies]\nhelper={{version='{requirement}',registry='https://example.test/registry/'}}\n"
        );
        assert!(Manifest::parse(Path::new("/tmp"), &text).is_ok());
    }
    for tail in [
        "[build]\nhook='bad'\n",
        "[permissions]\nnetwork=true\n",
        "[security]\nminimum_release_age=-1\n",
    ] {
        assert!(Manifest::parse(Path::new("/tmp"), &format!("{base}{tail}")).is_err());
    }
    for path in [
        "../escape",
        "/absolute",
        "a//b",
        "a/./b",
        "a/../../b",
        "a\\b",
    ] {
        assert!(safe_path(path).is_err());
    }
    assert!(safe_path("sample/1.0.0+build/files/lib.tarn").is_ok());
    assert_eq!(
        hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
#[test]
fn global_policy_cannot_be_weakened_and_source_inventory_is_bound() {
    let root = std::env::temp_dir().join(format!("tarn-package-policy-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[security]\nminimum_release_age=3600\nrequire_provenance=true\n",
    )
    .unwrap();
    let m=Manifest::parse(&root,"[package]\nname='sample'\nversion='1.0.0'\n[security]\nminimum_release_age=2\nrequire_provenance=false\n").unwrap();
    let p = m.effective_policy(&root).unwrap();
    assert_eq!(p.minimum_age, 3600);
    assert!(p.require_provenance);
    let mut files = std::collections::BTreeMap::new();
    files.insert("a.tarn".to_string(), b"hello".to_vec());
    let original = store::content(&files);
    files.insert("b.tarn".to_string(), b"hello".to_vec());
    assert_ne!(original, store::content(&files));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn writes_and_global_policy_refuse_symlink_redirection() {
    let root = std::env::temp_dir().join(format!("tarn-package-links-{}", std::process::id()));
    fs::create_dir_all(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("redirect")).unwrap();
    assert!(tarn_packages::write_atomic(&root.join("redirect/payload"), b"untrusted").is_err());
    assert!(!root.join("real/payload").exists());
    std::os::unix::fs::symlink(root.join("nonexistent"), root.join("config.toml")).unwrap();
    assert!(tarn_packages::manifest::global(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn resolver_backtracks_from_cycles_and_refuses_unsatisfied_cycles() {
    let root = std::env::temp_dir().join(format!("tarn-package-cycles-{}", std::process::id()));
    let registry = root.join("registry");
    fs::create_dir_all(&registry).unwrap();
    let origin = tarn_packages::source(registry.to_str().unwrap(), &root).unwrap();
    for (version, cyclic) in [("1.0.0", false), ("1.1.0", true)] {
        let dir = root.join(version);
        fs::create_dir(&dir).unwrap();
        let dependencies = if cyclic {
            format!("[dependencies]\nhelper={{version='^1',registry='{origin}'}}\n")
        } else {
            String::new()
        };
        let text = format!(
            "[package]\nname='helper'\nversion='{version}'\nentry='lib.tarn'\n{dependencies}"
        );
        fs::write(dir.join("tarn.toml"), &text).unwrap();
        fs::write(dir.join("lib.tarn"), "pub fn answer() i32 { return 42 }\n").unwrap();
        let manifest = Manifest::parse(&dir, &text).unwrap();
        store::publish(&manifest, &origin).unwrap();
    }
    let make = |version: &str| {
        Manifest::parse(&root,&format!("[package]\nname='application'\nversion='0.1.0'\n[dependencies]\nhelper={{version='{version}',registry='{origin}'}}\n")).unwrap()
    };
    let home = root.join("home");
    let lock = tarn_packages::graph::resolve(&make("^1"), &home, None, None).unwrap();
    assert_eq!(lock.packages["helper"].version.to_string(), "1.0.0");
    let error = tarn_packages::graph::resolve(&make("=1.1.0"), &home, None, None).unwrap_err();
    assert!(error.contains("dependency cycle"), "{error}");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn origin_normalization_does_not_need_an_online_registry() {
    assert_eq!(
        tarn_packages::source("absent/../registry", Path::new("/tmp/tarn-origin-test")).unwrap(),
        "file:///tmp/tarn-origin-test/registry/"
    );
    for origin in [
        "http://example.test/",
        "https://user:secret@example.test/",
        "https://example.test/?token=secret",
    ] {
        assert!(tarn_packages::source(origin, Path::new("/tmp")).is_err());
    }
}

#[test]
fn unfetched_package_names_cache_path_and_suggests_fetch() {
    let home = std::env::temp_dir().join(format!("tarn-unfetched-{}", std::process::id()));
    let release = store::Release {
        name: "helper".into(),
        version: semver::Version::parse("0.1.0").unwrap(),
        registry: "https://example.test/registry/".into(),
        hash: "0".repeat(64),
        entry: "lib.tarn".into(),
        published: 0,
        dependencies: Default::default(),
        files: Default::default(),
    };
    let error = store::verified_files(&home, &release).unwrap_err();
    assert!(error.contains("helper 0.1.0"), "{error}");
    assert!(error.contains(&store::cache_root(&home, &release).display().to_string()), "{error}");
    assert!(error.contains("tarn fetch"), "{error}");
}
