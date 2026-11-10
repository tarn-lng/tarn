//! Integrated package command surface; all mutations are explicit.
use pkg::toml_edit::{DocumentMut, InlineTable, Item, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};
use tarn_packages::{
    self as pkg, Result,
    graph::Lock,
    manifest::{Dependency, Manifest},
};

pub const COMMANDS: &[&str] = &[
    "init", "add", "remove", "update", "fetch", "deps", "verify", "audit", "publish",
];
#[derive(Default)]
struct Options {
    positional: Option<String>,
    registry: Option<String>,
    requirement: Option<String>,
    name: Option<String>,
    why: Option<String>,
    tree: bool,
    trust: bool,
    json: bool,
    library: bool,
}
fn parse(command: &str, args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" if matches!(command, "deps" | "verify" | "audit") && !options.json => {
                options.json = true
            }
            "--tree" if command == "deps" && !options.tree => options.tree = true,
            "--trust" if command == "deps" && !options.trust => options.trust = true,
            "--lib" if command == "init" && !options.library => options.library = true,
            "--registry"
                if matches!(command, "init" | "add" | "publish") && options.registry.is_none() =>
            {
                options.registry = Some(
                    rest.next()
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--registry needs an origin")?
                        .clone(),
                )
            }
            "--version" if command == "add" && options.requirement.is_none() => {
                options.requirement = Some(
                    rest.next()
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--version needs a SemVer requirement")?
                        .clone(),
                )
            }
            "--name" if command == "init" && options.name.is_none() => {
                options.name = Some(
                    rest.next()
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--name needs a package name")?
                        .clone(),
                )
            }
            "--why" if command == "deps" && options.why.is_none() => {
                options.why = Some(
                    rest.next()
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--why needs a package name")?
                        .clone(),
                )
            }
            _ if arg.starts_with('-') => return Err(format!("unknown or repeated option `{arg}`")),
            _ if matches!(command, "init" | "add" | "remove" | "update")
                && options.positional.is_none() =>
            {
                options.positional = Some(arg.clone())
            }
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    if matches!(command, "add" | "remove") && options.positional.is_none() {
        return Err("a package name is required".into());
    }
    if options.tree && options.why.is_some() {
        return Err("choose --tree or --why".into());
    }
    if let Some(name) = options
        .positional
        .as_ref()
        .filter(|_| matches!(command, "add" | "remove" | "update"))
    {
        pkg::manifest::parse_package_spec(name)?;
    }
    if let Some(name) = &options.why {
        pkg::manifest::parse_package_spec(name)?;
    }
    if let Some(version) = &options.requirement {
        semver_check(version)?;
    }
    Ok(options)
}
fn semver_check(requirement: &str) -> Result<()> {
    pkg::requirement(requirement)
}
pub fn validate(command: &str, args: &[String]) -> Result<()> {
    parse(command, args).map(|_| ()).map_err(|error| {
        format!(
            "{error}\n\nusage: tarn {command}{}",
            match command {
                "init" => " [directory] [--lib] [--name name] [--registry origin]",
                "add" => " package [--version requirement] [--registry origin]",
                "remove" => " package",
                "update" => " [package]",
                "publish" => " [--registry origin]",
                "deps" => " [--tree | --why package] [--trust] [--json]",
                "audit" | "verify" => " [--json]",
                _ => "",
            }
        )
    })
}
pub fn run(command: &str, args: &[String]) -> ExitCode {
    let options = parse(command, args).expect("validated package options");
    match execute(command, &options) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            super::native_error(options.json, "package", &error);
            ExitCode::from(1)
        }
    }
}
fn default_name(root: &Path) -> String {
    let raw = root
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or("application")
        .to_ascii_lowercase()
        .replace('-', "_");
    if pkg::manifest::package_name(&raw).is_ok() {
        raw
    } else {
        "application".into()
    }
}
fn initial(
    root: &Path,
    name: Option<&str>,
    library: bool,
    registry: Option<&str>,
) -> Result<DocumentMut> {
    let mut document = DocumentMut::new();
    let name = name
        .map(str::to_owned)
        .unwrap_or_else(|| default_name(root));
    pkg::manifest::package_name(&name)?;
    document["package"]["name"] = pkg::toml_edit::value(name);
    document["package"]["version"] = pkg::toml_edit::value("0.1.0");
    document["package"]["entry"] =
        pkg::toml_edit::value(if library { "lib.tarn" } else { "main.tarn" });
    if let Some(registry) = registry {
        document["registry"]["source"] = pkg::toml_edit::value(pkg::source(registry, root)?);
    }
    Ok(document)
}
fn origin(options: &Options, manifest: &Manifest, home: &Path) -> Result<String> {
    if let Some(origin) = &options.registry {
        return pkg::source(origin, &manifest.root);
    }
    manifest.registry.clone().or(pkg::manifest::global(home)?.0).ok_or_else(||"no registry configured; choose --registry, [registry].source or TARN_HOME/config.toml; no public registry is assumed".into())
}
fn execute(command: &str, options: &Options) -> Result<u8> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let home = pkg::home()?;
    if command == "init" {
        let root = cwd.join(options.positional.as_deref().unwrap_or("."));
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let _guard = pkg::Guard::acquire(&root)?;
        if root.join("tarn.toml").exists() {
            return Err("tarn.toml already exists; initialization will not overwrite it".into());
        }
        let document = initial(
            &root,
            options.name.as_deref(),
            options.library,
            options.registry.as_deref(),
        )?;
        let entry = root.join(if options.library {
            "lib.tarn"
        } else {
            "main.tarn"
        });
        if !entry.exists() {
            pkg::write_atomic(
                &entry,
                if options.library {
                    b"pub fn greet() string {\n    return \"Hello from Tarn\"\n}\n"
                } else {
                    b"fn main() {\n    print(\"Hello, Tarn\")\n}\n"
                },
            )?;
        }
        let manifest = Manifest::parse(&root, &document.to_string())?;
        pkg::write_atomic(&root.join("tarn.toml"), document.to_string().as_bytes())?;
        Lock {
            manifest: manifest.fingerprint(),
            packages: Default::default(),
        }
        .write(&root)?;
        println!("Initialized {}", root.display());
        return Ok(0);
    }
    let root = pkg::manifest::find(&cwd).unwrap_or(cwd.clone());
    let _guard = if matches!(command, "add" | "remove" | "update" | "fetch" | "publish") {
        Some(pkg::Guard::acquire(&root)?)
    } else {
        None
    };
    let mut manifest = if root.join("tarn.toml").exists() {
        Manifest::read(&root)?
    } else if command == "add" {
        Manifest::parse(&root, &initial(&root, None, false, None)?.to_string())?
    } else {
        return Err("no tarn.toml found; use tarn init or tarn add".into());
    };
    if command == "publish" {
        // Creation is explicit publication, never normal build-time behavior.
        if let Some(registry) = options.registry.as_ref().filter(|r| !r.contains("://")) {
            fs::create_dir_all(root.join(registry)).map_err(|e| e.to_string())?;
        }
        let registry = origin(options, &manifest, &home)?;
        if url_local_path(&registry).is_some_and(|p| p.starts_with(&root)) {
            return Err("publication registry must be outside the package source tree".into());
        }
        let checked = crate::check_project(&root.join(&manifest.entry))?;
        for diagnostic in &checked.diagnostics {
            eprint!("{}", diagnostic.render(&checked.program.sources));
        }
        if checked.has_errors() {
            return Err(
                "package source failed compiler validation; release was not published".into(),
            );
        }
        let release = pkg::store::publish(&manifest, &registry)?;
        println!(
            "Published {} {} sha256:{} to {} (publisher provenance unknown)",
            release.name, release.version, release.hash, release.registry
        );
        return Ok(0);
    }
    let old = if root.join("tarn.lock").exists() {
        Some(Lock::read(&root)?)
    } else {
        None
    };
    if matches!(command, "add" | "remove" | "update") {
        let mut target_update = None;
        if command == "add" {
            let spec = options.positional.as_ref().unwrap();
            let (scope, name) = pkg::manifest::parse_package_spec(spec)?;
            let base_registry = origin(options, &manifest, &home)?;
            let registry = pkg::store::scoped_registry(&base_registry, scope.as_deref(), &name)?;
            let requirement = if let Some(requirement) = &options.requirement {
                requirement.clone()
            } else {
                let version = pkg::store::versions(&registry, spec)?
                    .into_iter()
                    .find(|v| v.pre.is_empty())
                    .ok_or(
                        "no stable package release is available; select prerelease explicitly",
                    )?;
                format!("^{version}")
            };
            let mut table = InlineTable::new();
            table.insert("version", requirement.clone().into());
            table.insert("registry", registry.clone().into());
            manifest.document["dependencies"][&name] = Item::Value(Value::InlineTable(table));
            manifest.dependencies.insert(
                name,
                Dependency {
                    requirement,
                    registry,
                },
            );
        } else if command == "remove" {
            let spec = options.positional.as_ref().unwrap();
            let (_, name) = pkg::manifest::parse_package_spec(spec)?;
            if manifest.dependencies.remove(&name).is_none() {
                return Err(format!("`{name}` is not a direct dependency"));
            }
            manifest.document["dependencies"]
                .as_table_like_mut()
                .unwrap()
                .remove(&name);
        } else if let Some(spec) = &options.positional {
            let (_, name) = pkg::manifest::parse_package_spec(spec)?;
            if old.as_ref().is_none_or(|l| !l.packages.contains_key(&name)) {
                return Err(format!("`{name}` is not in the locked graph"));
            }
            target_update = Some(name);
        }
        let update = if command == "update" {
            Some(target_update.as_deref().unwrap_or("*"))
        } else {
            None
        };
        let lock = pkg::graph::resolve(&manifest, &home, old.as_ref(), update)?;
        // The guard protects cooperating writers; an interrupted pair is detected by fingerprint.
        pkg::write_atomic(
            &root.join("tarn.toml"),
            manifest.document.to_string().as_bytes(),
        )?;
        lock.write(&root)?;
        println!(
            "Locked {} verified package(s); publisher provenance unknown",
            lock.packages.len()
        );
        return Ok(0);
    }
    let lock = old.ok_or("tarn.lock is missing; run tarn update")?;
    lock.validate(&manifest, &home, false)?;
    if command == "fetch" {
        for release in lock.packages.values() {
            pkg::store::fetch(&home, release)?;
        }
        println!(
            "Fetched and verified {} package(s); provenance unknown",
            lock.packages.len()
        );
        return Ok(0);
    }
    if command == "verify" {
        lock.validate(&manifest, &home, true)?;
        if options.json {
            println!(
                "{}",
                pkg::json!({"kind":"package_verification","integrity":"verified","packages":lock.packages.len(),"provenance":if lock.packages.is_empty(){"not_applicable"}else{"unknown"},"execution_authority":"none"})
            );
        } else {
            println!(
                "Verified content and graph for {} package(s); signed provenance {}",
                lock.packages.len(),
                if lock.packages.is_empty() {
                    "not applicable"
                } else {
                    "unknown"
                }
            );
        }
        return Ok(0);
    }
    if command == "deps" {
        if let Some(raw_name) = &options.why {
            let (_, name) = pkg::manifest::parse_package_spec(raw_name)?;
            let name = &name;
            let mut paths = Vec::new();
            let mut budget = 10_000usize;
            for direct in manifest.dependencies.keys() {
                why(
                    &lock,
                    direct,
                    name,
                    vec![manifest.name.clone()],
                    &mut paths,
                    &mut budget,
                )?;
            }
            if paths.is_empty() {
                return Err(format!("`{name}` is not in the graph"));
            }
            if options.json {
                println!(
                    "{}",
                    pkg::json!({"kind":"dependency_paths","package":name,"paths":paths})
                );
            } else {
                for path in paths {
                    println!("{}", path.join(" -> "));
                }
            }
        } else if options.json {
            println!(
                "{}",
                pkg::json!({"kind":"dependencies","direct":manifest.dependencies.keys().collect::<Vec<_>>(),"packages":lock.packages.values().map(|r|pkg::json!({"name":r.name,"version":r.version.to_string(),"registry":r.registry,"hash":r.hash,"dependencies":r.dependencies.keys().collect::<Vec<_>>(),"provenance":"unknown","execution_authority":"none"})).collect::<Vec<_>>() })
            );
        } else if options.tree {
            for name in manifest.dependencies.keys() {
                tree(&lock, name, 0, &mut BTreeSet::new());
            }
        } else {
            for release in lock.packages.values() {
                println!(
                    "{} {} {} sha256:{}{}",
                    release.name,
                    release.version,
                    release.registry,
                    release.hash,
                    if options.trust {
                        " provenance=unknown build-authority=none"
                    } else {
                        ""
                    }
                );
            }
        }
        return Ok(0);
    }
    if command == "audit" {
        lock.validate(&manifest, &home, true)?;
        let findings = pkg::audit(&lock)?;
        let vulnerable = findings.iter().any(|f| f["status"] == "vulnerable");
        let unknown = findings.iter().any(|f| f["status"] == "unknown");
        if options.json {
            println!(
                "{}",
                pkg::json!({"kind":"package_audit","findings":findings,"provenance":if lock.packages.is_empty(){"not_applicable"}else{"unknown"}})
            );
        } else {
            for finding in &findings {
                println!("{}", finding);
            }
            if !lock.packages.is_empty() {
                println!(
                    "Publisher/signature provenance remains unknown; a clean advisory list is not proof of safety."
                );
            }
        }
        return Ok(if vulnerable {
            1
        } else if unknown || !lock.packages.is_empty() {
            3
        } else {
            0
        });
    }
    unreachable!()
}
fn url_local_path(registry: &str) -> Option<PathBuf> {
    pkg::registry_path(registry)
}
fn tree(lock: &Lock, name: &str, depth: usize, seen: &mut BTreeSet<String>) {
    let release = &lock.packages[name];
    let fresh = seen.insert(name.into());
    println!(
        "{}{} {}{}",
        "  ".repeat(depth),
        name,
        release.version,
        if fresh { "" } else { " (shared)" }
    );
    if fresh && depth < 128 && !release.dependencies.is_empty() {
        for name in release.dependencies.keys() {
            tree(lock, name, depth + 1, seen);
        }
    }
}
fn why(
    lock: &Lock,
    name: &str,
    target: &str,
    mut path: Vec<String>,
    out: &mut Vec<Vec<String>>,
    budget: &mut usize,
) -> Result<()> {
    if *budget == 0 || out.len() >= 1024 {
        return Err("dependency path inspection limit exceeded; use --tree".into());
    }
    *budget -= 1;
    path.push(name.into());
    if name == target {
        out.push(path);
        return Ok(());
    }
    for child in lock.packages[name].dependencies.keys() {
        why(lock, child, target, path.clone(), out, budget)?;
    }
    Ok(())
}
