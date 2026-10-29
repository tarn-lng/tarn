use crate::{Result, hash, read_text, source};
use semver::{Version, VersionReq};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, TableLike};

pub const RESERVED: &[&str] = &[
    "core",
    "io",
    "time",
    "net",
    "runtime",
    "fs",
    "process",
    "ffi",
    "string",
    "path",
    "http",
    "json",
    "collections",
    "testing",
    "logging",
    "crypto",
    "encoding",
    "compression",
    "cli",
    "os",
    "tls",
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub requirement: String,
    pub registry: String,
}
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub minimum_age: u64,
    pub require_provenance: bool,
}
pub struct Manifest {
    pub root: PathBuf,
    pub document: DocumentMut,
    pub name: String,
    pub version: Version,
    pub entry: String,
    pub registry: Option<String>,
    pub dependencies: BTreeMap<String, Dependency>,
    pub policy: Policy,
    /// `[native] links`: system C libraries this package links (ADR 0091).
    pub links: Vec<String>,
}
pub fn package_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        || RESERVED.contains(&name)
    {
        return Err(format!(
            "invalid or reserved package name `{name}`; use a lowercase Tarn identifier"
        ));
    }
    Ok(())
}
pub fn safe_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 512
        || !path.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-+".contains(&b))
        })
    {
        return Err(format!("unsafe package path `{path}`"));
    }
    Ok(())
}
fn keys(table: &dyn TableLike, allowed: &[&str], context: &str) -> Result<()> {
    for (key, _) in table.iter() {
        if !allowed.contains(&key) {
            return Err(format!(
                "unsupported `{context}.{key}`; package hooks/features are not authorized"
            ));
        }
    }
    Ok(())
}
fn string(item: Option<&Item>, label: &str) -> Result<String> {
    item.and_then(Item::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a string"))
}
fn policy(document: &DocumentMut) -> Result<Policy> {
    let Some(item) = document.get("security") else {
        return Ok(Policy::default());
    };
    let table = item.as_table_like().ok_or("security must be a table")?;
    keys(
        table,
        &["minimum_release_age", "require_provenance"],
        "security",
    )?;
    let minimum_age = match table.get("minimum_release_age") {
        None => 0,
        Some(value) => value
            .as_integer()
            .and_then(|n| u64::try_from(n).ok())
            .ok_or("minimum_release_age must be nonnegative seconds")?,
    };
    let require_provenance = match table.get("require_provenance") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or("require_provenance must be boolean")?,
    };
    Ok(Policy {
        minimum_age,
        require_provenance,
    })
}
pub fn global(home: &Path) -> Result<(Option<String>, Policy)> {
    let file = home.join("config.toml");
    crate::check_parents(&file)?;
    if !file.exists() {
        return Ok((None, Policy::default()));
    }
    let document: DocumentMut = read_text(&file, 262144)?
        .parse()
        .map_err(|e| format!("invalid global config: {e}"))?;
    keys(document.as_table(), &["registry", "security"], "config")?;
    Ok((registry(&document, home)?, policy(&document)?))
}
fn registry(document: &DocumentMut, root: &Path) -> Result<Option<String>> {
    let Some(item) = document.get("registry") else {
        return Ok(None);
    };
    let table = item.as_table_like().ok_or("registry must be a table")?;
    keys(table, &["source"], "registry")?;
    Ok(Some(source(
        &string(table.get("source"), "registry.source")?,
        root,
    )?))
}
impl Manifest {
    pub fn read(root: &Path) -> Result<Self> {
        Self::parse(root, &read_text(&root.join("tarn.toml"), 262144)?)
    }
    pub fn parse(root: &Path, text: &str) -> Result<Self> {
        let document: DocumentMut = text
            .parse()
            .map_err(|e| format!("invalid tarn.toml: {e}"))?;
        keys(
            document.as_table(),
            &["package", "dependencies", "registry", "security", "native"],
            "manifest",
        )?;
        let package = document
            .get("package")
            .and_then(Item::as_table_like)
            .ok_or("tarn.toml needs [package]")?;
        keys(package, &["name", "version", "entry"], "package")?;
        let name = string(package.get("name"), "package.name")?;
        package_name(&name)?;
        let version = Version::parse(&string(package.get("version"), "package.version")?)
            .map_err(|e| format!("invalid package version: {e}"))?;
        let entry = package
            .get("entry")
            .map(|v| string(Some(v), "package.entry"))
            .transpose()?
            .unwrap_or_else(|| "main.tarn".into());
        safe_path(&entry)?;
        if !entry.ends_with(".tarn") {
            return Err("package.entry must be a .tarn source".into());
        }
        let registry = registry(&document, root)?;
        let mut dependencies = BTreeMap::new();
        if let Some(item) = document.get("dependencies") {
            let table = item.as_table_like().ok_or("dependencies must be a table")?;
            for (name, item) in table.iter() {
                package_name(name)?;
                let (requirement, origin) = if let Some(requirement) = item.as_str() {
                    (
                        requirement.to_owned(),
                        registry
                            .clone()
                            .ok_or("string dependencies require [registry].source")?,
                    )
                } else {
                    let t = item
                        .as_table_like()
                        .ok_or("dependency must be a version string or table")?;
                    keys(t, &["version", "registry"], "dependency")?;
                    (
                        string(t.get("version"), "dependency.version")?,
                        source(&string(t.get("registry"), "dependency.registry")?, root)?,
                    )
                };
                VersionReq::parse(&requirement)
                    .map_err(|e| format!("invalid requirement for `{name}`: {e}"))?;
                dependencies.insert(
                    name.into(),
                    Dependency {
                        requirement,
                        registry: origin,
                    },
                );
            }
        }
        let policy = policy(&document)?;
        let links = links(&document)?;
        Ok(Self {
            root: root.into(),
            document,
            name,
            version,
            entry,
            registry,
            dependencies,
            policy,
            links,
        })
    }
    pub fn fingerprint(&self) -> String {
        let dependencies: BTreeMap<_, _> = self
            .dependencies
            .iter()
            .map(|(n, d)| (n, json!({"version":d.requirement,"registry":d.registry})))
            .collect();
        hash(json!({"name":self.name,"version":self.version.to_string(),"entry":self.entry,"dependencies":dependencies,"age":self.policy.minimum_age,"provenance":self.policy.require_provenance}).to_string().as_bytes())
    }
    pub fn effective_policy(&self, home: &Path) -> Result<Policy> {
        let (_, global) = global(home)?;
        Ok(Policy {
            minimum_age: self.policy.minimum_age.max(global.minimum_age),
            require_provenance: self.policy.require_provenance || global.require_provenance,
        })
    }
}
/// `[native] links = ["gtk-4", ":libfoo.so.1"]`, in the spelling of `--link`.
fn links(document: &DocumentMut) -> Result<Vec<String>> {
    let Some(item) = document.get("native") else { return Ok(Vec::new()) };
    let table = item.as_table_like().ok_or("[native] must be a table")?;
    keys(table, &["links"], "native")?;
    let Some(item) = table.get("links") else { return Ok(Vec::new()) };
    let array = item.as_array().ok_or("native.links must be an array of library names")?;
    let mut out: Vec<String> = Vec::new();
    for value in array.iter() {
        let name = value.as_str().ok_or("native.links must contain strings")?;
        let file = name.strip_prefix(':').unwrap_or(name);
        if file.is_empty() || file.starts_with('-') || !file.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b)) {
            return Err(format!("invalid library `{name}` in native.links: use a name such as `sqlite3` or an exact file such as `:libsqlite3.so.0`"));
        }
        if !out.iter().any(|l| l == name) {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

pub fn find(start: &Path) -> Option<PathBuf> {
    let mut path = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    if !path.is_absolute() {
        path = std::env::current_dir().ok()?.join(path);
    }
    loop {
        if path.join("tarn.toml").is_file() {
            return Some(path);
        }
        if !path.pop() {
            return None;
        }
    }
}
