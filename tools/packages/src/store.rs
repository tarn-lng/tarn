//! Bounded immutable releases and verified content-addressed source storage.
use crate::{
    Result, hash,
    manifest::{Dependency, Manifest, Policy, package_name, parse_package_spec, safe_path},
    read_bytes, write_atomic,
};
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct Release {
    pub name: String,
    pub version: Version,
    pub registry: String,
    pub hash: String,
    pub entry: String,
    pub published: u64,
    pub dependencies: BTreeMap<String, Dependency>,
    pub files: BTreeMap<String, String>,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn text(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("release.{key} must be a string"))
}
pub fn release(value: &Value, registry: &str) -> Result<Release> {
    let allowed = [
        "schema",
        "name",
        "version",
        "hash",
        "entry",
        "published",
        "dependencies",
        "files",
    ];
    if value
        .as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
        || value["schema"] != 1
    {
        return Err("unsupported release schema or evidence; refusing a trust downgrade".into());
    }
    let name = text(value, "name")?;
    package_name(&name)?;
    let version = Version::parse(&text(value, "version")?).map_err(|e| e.to_string())?;
    let entry = text(value, "entry")?;
    safe_path(&entry)?;
    let digest = text(value, "hash")?;
    valid_hash(&digest)?;
    let published = value["published"]
        .as_u64()
        .ok_or("invalid release timestamp")?;
    let mut files = BTreeMap::new();
    for (path, hash) in value["files"]
        .as_object()
        .ok_or("release.files must be an object")?
    {
        safe_path(path)?;
        let digest = hash.as_str().ok_or("file hash must be a string")?;
        valid_hash(digest)?;
        if !(path.ends_with(".tarn")
            || matches!(
                path.as_str(),
                "tarn.toml" | "README.md" | "LICENSE" | "LICENSE.md"
            ))
        {
            return Err(format!("unsupported package file `{path}`"));
        }
        files.insert(path.clone(), digest.into());
    }
    if files.len() > 1024
        || !files.contains_key("tarn.toml")
        || !files.contains_key(&entry)
        || !entry.ends_with(".tarn")
    {
        return Err(
            "release needs a manifest and a Tarn entry within the bounded inventory".into(),
        );
    }
    let mut dependencies = BTreeMap::new();
    for (name, dep) in value["dependencies"]
        .as_object()
        .ok_or("release.dependencies must be an object")?
    {
        package_name(name)?;
        if dep.as_object().is_none_or(|o| {
            o.len() != 2 || !o.contains_key("version") || !o.contains_key("registry")
        }) {
            return Err("invalid dependency schema".into());
        }
        let requirement = text(dep, "version")?;
        semver::VersionReq::parse(&requirement).map_err(|e| e.to_string())?;
        let origin = text(dep, "registry")?;
        let normalized = crate::source(&origin, Path::new("."))?;
        if origin != normalized {
            return Err("release dependency registry must be normalized".into());
        }
        dependencies.insert(
            name.clone(),
            Dependency {
                requirement,
                registry: origin,
            },
        );
    }
    Ok(Release {
        name,
        version,
        registry: registry.into(),
        hash: digest,
        entry,
        published,
        dependencies,
        files,
    })
}
pub fn valid_hash(digest: &str) -> Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid SHA-256 content hash".into());
    }
    Ok(())
}
impl Release {
    pub fn json(&self) -> Value {
        let deps: BTreeMap<_, _> = self
            .dependencies
            .iter()
            .map(|(n, d)| (n, json!({"version":d.requirement,"registry":d.registry})))
            .collect();
        json!({"schema":1,"name":self.name,"version":self.version.to_string(),"hash":self.hash,"entry":self.entry,"published":self.published,"dependencies":deps,"files":self.files})
    }
    pub fn policy(&self, policy: &Policy) -> Result<()> {
        if policy.require_provenance {
            return Err(format!(
                "{}: required signed provenance is unavailable; content integrity is not publication trust",
                self.name
            ));
        }
        let current = now();
        if self.published > current || current - self.published < policy.minimum_age {
            return Err(format!(
                "{} {} does not meet minimum release age {} seconds",
                self.name, self.version, policy.minimum_age
            ));
        }
        Ok(())
    }
}
fn obtain_url(url: &url::Url, limit: u64, allow_redirects: bool) -> Result<Vec<u8>> {
    if url.scheme() == "file" {
        return read_bytes(
            &url.to_file_path().map_err(|_| "invalid local registry")?,
            limit,
        );
    }
    if url.scheme() != "https" {
        return Err("HTTPS is required for registry downloads".into());
    }
    let mut command = Command::new("curl");
    command
        .args([
            "-q",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "30",
            "--max-filesize",
            &limit.to_string(),
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if allow_redirects {
        command.args(["--location", "--max-redirs", "5"]);
    } else {
        command.args(["--max-redirs", "0"]);
    }
    // An explicit caller-provided CA bundle grants TLS trust, never package authority.
    if let Some(ca) = std::env::var_os("TARN_CA_BUNDLE") {
        command.arg("--cacert").arg(ca);
    }
    let mut child = command
        .arg(url.as_str())
        .spawn()
        .map_err(|e| format!("cannot start trusted curl download tool: {e}"))?;
    let mut bytes = Vec::new();
    if let Err(e) = child
        .stdout
        .take()
        .unwrap()
        .take(limit + 1)
        .read_to_end(&mut bytes)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.to_string());
    }
    if bytes.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err("registry response exceeds size limit".into());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "HTTPS registry download failed (curl status {:?}); TLS validation and redirects remain enforced",
            status.code()
        ));
    }
    Ok(bytes)
}
pub fn obtain(registry: &str, relative: &str, limit: u64) -> Result<Vec<u8>> {
    safe_path(relative)?;
    let base = url::Url::parse(registry).map_err(|e| e.to_string())?;
    let target = base.join(relative).map_err(|e| e.to_string())?;
    obtain_url(&target, limit, false)
}
pub fn scoped_registry(base_registry: &str, scope: Option<&str>, name: &str) -> Result<String> {
    let Some(scope) = scope.filter(|s| *s != "tarn-lng") else {
        return Ok(base_registry.to_owned());
    };
    let trimmed = base_registry.trim_end_matches('/');
    if trimmed.ends_with(&format!("/@{scope}")) {
        return Ok(base_registry.to_owned());
    }
    if obtain(base_registry, &format!("{name}/index.json"), 4 * 1024 * 1024).is_ok() {
        return Ok(base_registry.to_owned());
    }
    Ok(format!("{trimmed}/@{scope}/"))
}
fn split_registry_scope(registry: &str, spec: &str) -> Result<(String, String, String)> {
    let (spec_scope, name) = parse_package_spec(spec)?;
    let trimmed = registry.trim_end_matches('/');
    let (api_base, reg_scope) = if let Some((prefix, last)) = trimmed.rsplit_once('/') {
        if let Some(slug) = last.strip_prefix('@') {
            if !slug.is_empty()
                && slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                (format!("{prefix}/"), Some(slug.to_owned()))
            } else {
                (registry.to_owned(), None)
            }
        } else {
            (registry.to_owned(), None)
        }
    } else {
        (registry.to_owned(), None)
    };
    let scope = spec_scope
        .or(reg_scope)
        .unwrap_or_else(|| "tarn-lng".to_owned());
    Ok((api_base, scope, name))
}
fn api_versions_list(api_base: &str, scope: &str, name: &str) -> Result<Vec<Value>> {
    let base = url::Url::parse(api_base).map_err(|e| e.to_string())?;
    let url = base
        .join(&format!("v1/packages/@{scope}/{name}/versions"))
        .map_err(|e| e.to_string())?;
    let bytes = obtain_url(&url, 4 * 1024 * 1024, false)?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let list = data
        .as_array()
        .ok_or("registry API versions response must be an array")?;
    if list.len() > 512 {
        return Err("registry version limit exceeded".into());
    }
    Ok(list.clone())
}
fn extract_tar_gz(archive: &[u8], default_registry: &str) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    let temp = std::env::temp_dir().join(format!(
        "tarn-archive-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let extracted = temp.join("extracted");
    fs::create_dir_all(&extracted).map_err(|e| e.to_string())?;
    let result = (|| {
        let tar_path = temp.join("package.tar.gz");
        fs::write(&tar_path, archive).map_err(|e| e.to_string())?;
        let status = Command::new("tar")
            .args(["-xzf"])
            .arg(&tar_path)
            .arg("-C")
            .arg(&extracted)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run tar to extract package archive: {e}"))?;
        if !status.success() {
            return Err("failed to extract package .tar.gz archive".into());
        }
        let mut pkg_root = extracted.clone();
        if !pkg_root.join("tarn.toml").is_file() {
            let mut subdirs = Vec::new();
            for entry in fs::read_dir(&extracted).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                    subdirs.push(entry.path());
                }
            }
            if subdirs.len() == 1 && subdirs[0].join("tarn.toml").is_file() {
                pkg_root = subdirs.remove(0);
            }
        }
        let mut files = BTreeMap::new();
        inventory(&pkg_root, &pkg_root, &mut files, false)?;
        let raw_toml = std::str::from_utf8(
            files
                .get("tarn.toml")
                .ok_or("release archive is missing tarn.toml")?,
        )
        .map_err(|_| "invalid UTF-8 manifest")?;
        let mut doc: toml_edit::DocumentMut =
            raw_toml.parse().map_err(|e| format!("invalid tarn.toml: {e}"))?;
        if doc.get("registry").is_none() {
            doc["registry"]["source"] = toml_edit::value(default_registry);
        }
        let manifest = Manifest::parse(&pkg_root, &doc.to_string())?;
        let mut normalized_doc = manifest.document.clone();
        if let Some(reg) = &manifest.registry {
            normalized_doc["registry"]["source"] = toml_edit::value(reg);
        }
        for (dep_name, dep) in &manifest.dependencies {
            let mut table = toml_edit::InlineTable::new();
            table.insert("version", dep.requirement.clone().into());
            table.insert("registry", dep.registry.clone().into());
            normalized_doc["dependencies"][dep_name] =
                toml_edit::Item::Value(toml_edit::Value::InlineTable(table));
        }
        files.insert("tarn.toml".into(), normalized_doc.to_string().into_bytes());
        Ok((manifest, files))
    })();
    let _ = fs::remove_dir_all(&temp);
    result
}
pub fn versions(registry: &str, spec: &str) -> Result<Vec<Version>> {
    let (api_base, scope, name) = split_registry_scope(registry, spec)?;
    if let Ok(bytes) = obtain(&api_base, &format!("{name}/index.json"), 4 * 1024 * 1024) {
        let data: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let list = data
            .as_array()
            .ok_or("registry index must be an array of versions")?;
        if list.len() > 512 {
            return Err("registry version limit exceeded".into());
        }
        let mut versions = Vec::new();
        for version in list {
            versions.push(
                Version::parse(version.as_str().ok_or("invalid registry version")?)
                    .map_err(|e| e.to_string())?,
            );
        }
        versions.sort();
        versions.reverse();
        versions.dedup();
        return Ok(versions);
    }
    let list = api_versions_list(&api_base, &scope, &name)?;
    let mut versions = Vec::new();
    for item in list {
        if item["yanked"].as_bool().unwrap_or(false) {
            continue;
        }
        let ver_str = item["version"]
            .as_str()
            .ok_or("invalid registry API version entry")?;
        versions.push(Version::parse(ver_str).map_err(|e| e.to_string())?);
    }
    versions.sort();
    versions.reverse();
    versions.dedup();
    Ok(versions)
}
pub fn load_release(registry: &str, spec: &str, version: &Version) -> Result<Release> {
    let (api_base, scope, name) = split_registry_scope(registry, spec)?;
    if let Ok(bytes) = obtain(
        &api_base,
        &format!("{name}/{version}/release.json"),
        4 * 1024 * 1024,
    ) {
        let value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let release = release(&value, registry)?;
        if release.name != name || &release.version != version {
            return Err("registry release identity mismatch".into());
        }
        return Ok(release);
    }
    let list = api_versions_list(&api_base, &scope, &name)?;
    let ver_string = version.to_string();
    let entry = list
        .into_iter()
        .find(|item| {
            item["version"].as_str() == Some(ver_string.as_str())
                && !item["yanked"].as_bool().unwrap_or(false)
        })
        .ok_or_else(|| format!("release {name} {version} is unavailable or yanked"))?;
    match entry["protocol"].as_str() {
        Some("source-files") => {
            let meta_url_str = entry["metadataUrl"]
                .as_str()
                .ok_or("source-files release requires metadataUrl")?;
            let meta_url = url::Url::parse(meta_url_str).map_err(|e| e.to_string())?;
            let bytes = obtain_url(&meta_url, 4 * 1024 * 1024, false)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let rel = release(&value, registry)?;
            if rel.name != name || &rel.version != version {
                return Err("registry release identity mismatch".into());
            }
            if let Some(expected_hash) = entry["hash"].as_str() {
                if rel.hash != expected_hash {
                    return Err("registry API hash does not match release metadata".into());
                }
            }
            Ok(rel)
        }
        Some("github-release") => {
            let expected_archive_hash = text(&entry, "hash")?;
            valid_hash(&expected_archive_hash)?;
            let base = url::Url::parse(&api_base).map_err(|e| e.to_string())?;
            let download_url = base
                .join(&format!("v1/packages/@{scope}/{name}/{version}/download"))
                .map_err(|e| e.to_string())?;
            let archive = obtain_url(&download_url, 64 * 1024 * 1024, true)?;
            if hash(&archive) != expected_archive_hash {
                return Err("downloaded release archive SHA-256 mismatch".into());
            }
            let (manifest, files) = extract_tar_gz(&archive, registry)?;
            if manifest.name != name || &manifest.version != version {
                return Err("archive manifest identity mismatch".into());
            }
            let rel = Release {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                registry: registry.to_owned(),
                hash: content(&files),
                entry: manifest.entry.clone(),
                published: entry["published"].as_u64().unwrap_or(0),
                dependencies: manifest.dependencies.clone(),
                files: files.iter().map(|(n, b)| (n.clone(), hash(b))).collect(),
            };
            if !rel.files.contains_key(&rel.entry) {
                return Err("package entry is missing in archive".into());
            }
            if let Ok(home) = crate::home() {
                let _ = write_cache_files(&home, &rel, files);
            }
            Ok(rel)
        }
        _ => Err("unsupported registry release protocol".into()),
    }
}
pub fn content(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"Tarn package source v1\0");
    for (name, bytes) in files {
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}
pub fn cache_root(home: &Path, release: &Release) -> PathBuf {
    home.join("sources").join(&release.hash)
}
fn inventory(
    root: &Path,
    base: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
    all: bool,
) -> Result<()> {
    crate::check_parents(root)?;
    if root
        .strip_prefix(base)
        .map_err(|e| e.to_string())?
        .components()
        .count()
        > 32
    {
        return Err("package directory depth limit exceeded".into());
    }
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "symbolic links are forbidden in packages: {}",
                path.display()
            ));
        }
        if kind.is_dir() {
            if !all
                && (entry.file_name().to_string_lossy().starts_with('.')
                    || matches!(entry.file_name().to_str(), Some("target" | "node_modules")))
            {
                continue;
            }
            inventory(&path, base, files, all)?;
        } else if kind.is_file() {
            let name = path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("package paths must be UTF-8")?
                .to_string();
            if all
                || name.ends_with(".tarn")
                || matches!(
                    name.as_str(),
                    "tarn.toml" | "README.md" | "LICENSE" | "LICENSE.md"
                )
            {
                safe_path(&name)?;
                let bytes = read_bytes(&path, 4 * 1024 * 1024)?;
                if name.ends_with(".tarn") || name == "tarn.toml" {
                    std::str::from_utf8(&bytes).map_err(|_| "package sources must be UTF-8")?;
                }
                files.insert(name, bytes);
                if files.len() > 1024
                    || files.values().map(Vec::len).sum::<usize>() > 32 * 1024 * 1024
                {
                    return Err("package size/file limit exceeded".into());
                }
            }
        } else {
            return Err("package contents must be regular files/directories".into());
        }
    }
    Ok(())
}
pub fn verified_files(
    home: &Path,
    release: &Release,
) -> Result<(PathBuf, BTreeMap<String, Vec<u8>>)> {
    let root = cache_root(home, release);
    if !root.exists() {
        return Err(format!(
            "package {} {} is locked but not in the local cache (expected at `{}`); run `tarn fetch`",
            release.name,
            release.version,
            root.display()
        ));
    }
    let mut files = BTreeMap::new();
    inventory(&root, &root, &mut files, true)?;
    let hashes: BTreeMap<_, _> = files.iter().map(|(n, b)| (n.clone(), hash(b))).collect();
    if hashes != release.files || content(&files) != release.hash {
        return Err(format!(
            "content hash/inventory mismatch for {} {}; refusing cached data",
            release.name, release.version
        ));
    }
    let manifest = Manifest::parse(
        &root,
        std::str::from_utf8(&files["tarn.toml"]).map_err(|_| "invalid UTF-8 manifest")?,
    )?;
    if manifest.name != release.name
        || manifest.version != release.version
        || manifest.entry != release.entry
        || manifest.dependencies != release.dependencies
    {
        return Err("release manifest disagrees with locked identity/graph".into());
    }
    Ok((root, files))
}
pub fn verify(home: &Path, release: &Release) -> Result<PathBuf> {
    verified_files(home, release).map(|(root, _)| root)
}
fn download_files_from_origin(origin: &str, release: &Release) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut total = 0;
    for (name, expected) in &release.files {
        let bytes = obtain(
            origin,
            &format!("{}/{}/files/{name}", release.name, release.version),
            4 * 1024 * 1024,
        )?;
        total += bytes.len();
        if total > 32 * 1024 * 1024 || hash(&bytes) != *expected {
            return Err(
                "downloaded package exceeds limits or its locked hash does not match".into(),
            );
        }
        files.insert(name.clone(), bytes);
    }
    Ok(files)
}
fn download_files_from_api(release: &Release) -> Result<BTreeMap<String, Vec<u8>>> {
    let (api_base, scope, name) = split_registry_scope(&release.registry, &release.name)?;
    let list = api_versions_list(&api_base, &scope, &name)?;
    let ver_string = release.version.to_string();
    let entry = list
        .into_iter()
        .find(|item| {
            item["version"].as_str() == Some(ver_string.as_str())
                && !item["yanked"].as_bool().unwrap_or(false)
        })
        .ok_or_else(|| format!("release {name} {} is unavailable or yanked", release.version))?;
    match entry["protocol"].as_str() {
        Some("source-files") => {
            let meta_url = entry["metadataUrl"]
                .as_str()
                .ok_or("source-files release requires metadataUrl")?;
            let suffix = format!("{}/{}/release.json", release.name, release.version);
            let source_origin = meta_url
                .strip_suffix(&suffix)
                .ok_or("invalid source-files metadataUrl")?;
            download_files_from_origin(source_origin, release)
        }
        Some("github-release") => {
            let expected_archive_hash = text(&entry, "hash")?;
            valid_hash(&expected_archive_hash)?;
            let base = url::Url::parse(&api_base).map_err(|e| e.to_string())?;
            let download_url = base
                .join(&format!(
                    "v1/packages/@{scope}/{}/{}/download",
                    release.name, release.version
                ))
                .map_err(|e| e.to_string())?;
            let archive = obtain_url(&download_url, 64 * 1024 * 1024, true)?;
            if hash(&archive) != expected_archive_hash {
                return Err("downloaded release archive SHA-256 mismatch".into());
            }
            let (_, files) = extract_tar_gz(&archive, &release.registry)?;
            for (fname, expected) in &release.files {
                let Some(bytes) = files.get(fname) else {
                    return Err(format!("missing file `{fname}` in downloaded archive"));
                };
                if &hash(bytes) != expected {
                    return Err("downloaded archive file hash mismatch".into());
                }
            }
            Ok(files)
        }
        _ => Err("unsupported registry release protocol".into()),
    }
}
fn write_cache_files(
    home: &Path,
    release: &Release,
    files: BTreeMap<String, Vec<u8>>,
) -> Result<PathBuf> {
    crate::check_parents(&cache_root(home, release))?;
    if cache_root(home, release).exists() {
        return verify(home, release);
    }
    if content(&files) != release.hash {
        return Err("downloaded package content identity mismatch".into());
    }
    let manifest = Manifest::parse(
        &cache_root(home, release),
        std::str::from_utf8(&files["tarn.toml"]).map_err(|_| "invalid UTF-8 manifest")?,
    )?;
    if manifest.name != release.name
        || manifest.version != release.version
        || manifest.entry != release.entry
        || manifest.dependencies != release.dependencies
    {
        return Err("downloaded manifest disagrees with locked graph".into());
    }
    crate::check_parents(&home.join("sources"))?;
    fs::create_dir_all(home.join("sources")).map_err(|e| e.to_string())?;
    let temporary = home.join("sources").join(format!(
        ".fetch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        for (name, bytes) in files {
            write_atomic(&temporary.join(name), &bytes)?;
        }
        if let Err(error) = fs::rename(&temporary, cache_root(home, release)) {
            if !cache_root(home, release).is_dir() {
                return Err(error.to_string());
            }
            // A concurrent installer is acceptable only after full verification.
            let root = verify(home, release)?;
            fs::remove_dir_all(&temporary).map_err(|e| e.to_string())?;
            return Ok(root);
        }
        verify(home, release)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}
pub fn fetch(home: &Path, release: &Release) -> Result<PathBuf> {
    crate::check_parents(&cache_root(home, release))?;
    if cache_root(home, release).exists() {
        return verify(home, release);
    }
    let (api_base, _, _) = split_registry_scope(&release.registry, &release.name)?;
    let files = match download_files_from_origin(&api_base, release) {
        Ok(files) => files,
        Err(_) => download_files_from_api(release)?,
    };
    write_cache_files(home, release, files)
}
pub fn publish(manifest: &Manifest, registry: &str) -> Result<Release> {
    let base=url::Url::parse(registry).map_err(|e|e.to_string())?.to_file_path().map_err(|_|"network publication needs authenticated registry infrastructure; publish supports local registries")?;
    let _guard = crate::Guard::acquire(&base)?;
    let mut files = BTreeMap::new();
    inventory(&manifest.root, &manifest.root, &mut files, false)?;
    // Published manifests bind origins absolutely, independent of cache location.
    let mut document = manifest.document.clone();
    if let Some(registry) = &manifest.registry {
        document["registry"]["source"] = toml_edit::value(registry);
    }
    for (name, dep) in &manifest.dependencies {
        let mut table = toml_edit::InlineTable::new();
        table.insert("version", dep.requirement.clone().into());
        table.insert("registry", dep.registry.clone().into());
        document["dependencies"][name] =
            toml_edit::Item::Value(toml_edit::Value::InlineTable(table));
    }
    files.insert("tarn.toml".into(), document.to_string().into_bytes());
    let release = Release {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        registry: registry.into(),
        hash: content(&files),
        entry: manifest.entry.clone(),
        published: now(),
        dependencies: manifest.dependencies.clone(),
        files: files.iter().map(|(n, b)| (n.clone(), hash(b))).collect(),
    };
    if !release.files.contains_key(&release.entry) {
        return Err("package entry is missing".into());
    }
    let destination = base.join(&release.name).join(release.version.to_string());
    if destination.exists() {
        return Err("release already exists; immutable versions cannot be republished".into());
    }
    crate::check_parents(&destination)?;
    let mut published_versions = if base.join(&release.name).join("index.json").exists() {
        versions(registry, &release.name)?
    } else {
        Vec::new()
    };
    published_versions.push(release.version.clone());
    published_versions.sort();
    published_versions.dedup();
    let temporary = base.join(format!(".publish-{}", std::process::id()));
    fs::create_dir(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        for (name, bytes) in files {
            write_atomic(&temporary.join("files").join(name), &bytes)?;
        }
        write_atomic(
            &temporary.join("release.json"),
            serde_json::to_string_pretty(&release.json())
                .unwrap()
                .as_bytes(),
        )?;
        fs::create_dir_all(destination.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
        write_atomic(
            &base.join(&release.name).join("index.json"),
            serde_json::to_vec_pretty(
                &published_versions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
            .as_slice(),
        )?;
        Ok(release.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}
