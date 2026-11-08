//! Verified pure-source packages; registry content never receives execution authority.
pub use toml_edit;
pub mod graph;
pub mod manifest;
pub mod store;

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn home() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TARN_HOME") {
        let path = PathBuf::from(path);
        return Ok(if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        });
    }
    std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join(".tarn"))
        .ok_or_else(|| "set TARN_HOME; HOME is unavailable".into())
}
/// macOS ships `/var`, `/tmp` and `/etc` as root-owned symlinks into `/private`,
/// so every temporary path crosses one. Only that exact system layout is exempt;
/// it cannot redirect package data without administrator access (ADR 0098).
fn is_system_alias(path: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    ["var", "tmp", "etc"].iter().any(|name| {
        path == Path::new("/").join(name)
            && fs::read_link(path).is_ok_and(|target| target == Path::new("private").join(name))
    })
}
pub fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    // Check each component, not just the leaf: cached parents cannot redirect reads.
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        if fs::symlink_metadata(&prefix)
            .map_err(|e| format!("cannot inspect `{}`: {e}", prefix.display()))?
            .file_type()
            .is_symlink()
            && !is_system_alias(&prefix)
        {
            return Err(format!(
                "symbolic links are forbidden in package data: {}",
                prefix.display()
            ));
        }
    }
    let mut file =
        fs::File::open(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("package data must be regular files".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "package file exceeds {limit} bytes: {}",
            path.display()
        ));
    }
    Ok(bytes)
}
pub fn read_text(path: &Path, limit: u64) -> Result<String> {
    String::from_utf8(read_bytes(path, limit)?)
        .map_err(|_| format!("invalid UTF-8 in {}", path.display()))
}
pub fn source(raw: &str, root: &Path) -> Result<String> {
    let mut url = if raw.contains("://") {
        url::Url::parse(raw).map_err(|e| e.to_string())?
    } else {
        let path = root.join(raw);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        url::Url::from_directory_path(path).map_err(|_| "invalid registry path")?
    };
    if !matches!(url.scheme(), "file" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("registries require a local path/file URL or credential-free HTTPS URL".into());
    }
    if url.scheme() == "file" {
        let path = url.to_file_path().map_err(|_| "invalid file registry")?;
        if !path.is_absolute() {
            return Err("registry file URL must be absolute".into());
        }
        url = url::Url::from_directory_path(path).map_err(|_| "invalid registry path")?;
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    // Reparse constructed file URLs to normalize lexical dot segments without I/O.
    Ok(url::Url::parse(url.as_str())
        .map_err(|e| e.to_string())?
        .to_string())
}
pub fn check_parents(path: &Path) -> Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() && !is_system_alias(&prefix) => {
                return Err(format!(
                    "symbolic links are forbidden in package data: {}",
                    prefix.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("file needs a parent directory")?;
    check_parents(path)?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(
        ".tarn-write-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub struct Guard(PathBuf);
impl Guard {
    pub fn acquire(root: &Path) -> Result<Self> {
        check_parents(root)?;
        let path = root.join(".tarn-package-transaction");
        let mut f=fs::OpenOptions::new().create_new(true).write(true).open(&path).map_err(|e| format!("package mutation is locked at {}; inspect/remove a stale guard only after checking its PID ({e})",path.display()))?;
        writeln!(f, "{}", std::process::id()).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub use serde_json::json;
pub fn requirement(value: &str) -> Result<()> {
    semver::VersionReq::parse(value)
        .map(|_| ())
        .map_err(|e| format!("invalid SemVer requirement: {e}"))
}
pub fn registry_path(value: &str) -> Option<PathBuf> {
    url::Url::parse(value).ok()?.to_file_path().ok()
}
pub fn audit(lock: &graph::Lock) -> Result<Vec<serde_json::Value>> {
    let mut findings = Vec::new();
    for release in lock.packages.values() {
        let bytes = match store::obtain(&release.registry, "advisories.json", 4 * 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(_) => {
                findings.push(json!({"package":release.name,"status":"unknown","reason":"origin advisory evidence unavailable"}));
                continue;
            }
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid advisory evidence: {e}"))?;
        let entries = value.as_array().ok_or("advisories must be an array")?;
        if entries.len() > 10000 {
            return Err("advisory limit exceeded".into());
        }
        let mut affected = false;
        for advisory in entries {
            if advisory.as_object().is_none_or(|o| o.len() != 4) {
                return Err("unsupported advisory schema".into());
            }
            let name = advisory["package"]
                .as_str()
                .ok_or("advisory package missing")?;
            let requirement = advisory["affected"]
                .as_str()
                .ok_or("advisory version constraint missing")?;
            let id = advisory["id"].as_str().ok_or("advisory ID missing")?;
            let summary = advisory["summary"]
                .as_str()
                .ok_or("advisory summary missing")?;
            let requirement = semver::VersionReq::parse(requirement).map_err(|e| e.to_string())?;
            if name == release.name && requirement.matches(&release.version) {
                affected = true;
                findings.push(json!({"package":name,"status":"vulnerable","id":id,"summary":summary,"origin":release.registry}));
            }
        }
        if !affected {
            findings.push(json!({"package":release.name,"status":"no_known_advisory","evidence":"registry_reported_unsigned"}));
        }
    }
    Ok(findings)
}
