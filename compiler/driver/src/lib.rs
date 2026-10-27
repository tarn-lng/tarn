//! Compiler driver: loads a program from disk and runs the frontend phases.
//!
//! Module loading (ADR 0004): the entry file's directory is the project root;
//! `import "a/b"` loads `<root>/a/b.tarn` if it exists. Imports that are not
//! local files are left to the resolver (standard modules or E2007).

pub mod testing;

use std::path::{Path, PathBuf};
use tarn_ast::{ItemKind, Module};
use tarn_diagnostics::{Diagnostic, Severity, SourceMap};
use tarn_resolve::{ModuleInput, Resolved};

const ORDINARY_STDLIB: &[(&str, &str)] = &[
    ("http", include_str!("../../../stdlib/http/http.tarn")),
    ("path", include_str!("../../../stdlib/path/path.tarn")),
    ("string", include_str!("../../../stdlib/string/string.tarn")),
    ("json", include_str!("../../../stdlib/json/json.tarn")),
    ("os", include_str!("../../../stdlib/os/os.tarn")),
    ("console", include_str!("../../../stdlib/console/console.tarn")),
    ("https", include_str!("../../../stdlib/https/https.tarn")),
    ("collections", include_str!("../../../stdlib/collections/collections.tarn")),
    ("csv", include_str!("../../../stdlib/csv/csv.tarn")),
    ("websocket", include_str!("../../../stdlib/websocket/websocket.tarn")),
    ("encoding", include_str!("../../../stdlib/encoding/encoding.tarn")),
    ("crypto", include_str!("../../../stdlib/crypto/crypto.tarn")),
];

const CORE_SOURCE: &str = include_str!("../../../stdlib/core/core.tarn");

/// Trusted standard-library modules (ADR 0041), embedded so the compiler is
/// self-contained. Only these may declare intrinsics, and only their sources
/// carry trusted semantic contracts. Entry files never acquire these names.
const TRUSTED_STDLIB: &[(&str, &str)] = &[
    ("core", CORE_SOURCE),
    ("io", include_str!("../../../stdlib/io/io.tarn")),
    ("time", include_str!("../../../stdlib/time/time.tarn")),
    ("runtime", include_str!("../../../stdlib/runtime/runtime.tarn")),
    ("net", include_str!("../../../stdlib/net/net.tarn")),
    ("fs", include_str!("../../../stdlib/fs/fs.tarn")),
    ("process", include_str!("../../../stdlib/process/process.tarn")),
    ("ffi", include_str!("../../../stdlib/ffi/ffi.tarn")),
];

fn trusted_source(name: &str) -> Option<&'static str> {
    TRUSTED_STDLIB.iter().find(|(n, _)| *n == name).map(|(_, source)| *source)
}

/// Embedded public modules, sorted independently of filesystem state.
pub fn stdlib_modules() -> Vec<&'static str> {
    let mut names: Vec<_> = TRUSTED_STDLIB.iter().chain(ORDINARY_STDLIB).map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}
/// Source shipped by this compiler; this query grants no intrinsic authority.
pub fn stdlib_source(name: &str) -> Option<&'static str> {
    trusted_source(name).or_else(|| ORDINARY_STDLIB.iter().find(|(n, _)| *n == name).map(|(_, source)| *source))
}
fn official_module(name: &str) -> bool { stdlib_source(name).is_some() }

pub struct Program {
    pub import_targets: std::collections::HashMap<(String, String), String>,
    pub sources: SourceMap,
    /// Files read from disk during loading; embedded stdlib sources are excluded.
    pub disk_sources: Vec<PathBuf>,
    /// (module name, AST); the entry module is first.
    pub modules: Vec<(String, Module)>,
    /// Embedded source provenance retained for semantic contracts.
    pub trusted_modules: std::collections::HashSet<String>,
    /// Directory each module's local imports resolve against (diagnostics).
    pub import_roots: std::collections::HashMap<String, String>,
}

pub struct CheckResult {
    pub program: Program,
    /// `None` when the parse had errors (resolution is skipped).
    pub resolved: Option<Resolved>,
    /// `None` when parsing or resolution had errors.
    pub typed: Option<tarn_types::Typed>,
    /// `None` when any earlier phase had errors.
    pub ir: Option<tarn_ir::Program>,
    /// Move/init checking results (drop elaboration input).
    pub moves: Option<tarn_ownership::MoveResults>,
    /// Borrow checking results (loans, provenance).
    pub borrows: Option<tarn_ownership::BorrowResults>,
    /// Self-contained executable destruction IR, only after successful ownership checking.
    pub drops: Option<tarn_ir::post_drop::Program>,
    /// Sorted by file, then position.
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckResult {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }

    /// E3078 for `tarn build`/`run` when the entry module declares no `fn main`.
    /// `tarn check` accepts libraries, so this is not part of checking.
    pub fn missing_main(&self) -> Option<Diagnostic> {
        let (p, t) = (self.drops.as_ref()?, self.typed.as_ref()?);
        let found = p.functions.iter().any(|f| f.decl.name == "main" && f.decl.symbol.and_then(|s| t.decls.fns.get(&s)).is_some_and(|sig| sig.module.0 == 0));
        (!found).then(|| Diagnostic::error("E3078", "missing_main", "no `fn main` found in the entry module")
            .note("`tarn build` and `tarn run` need an entry point")
            .help("add `fn main() { ... }`, or use `tarn check` for a library"))
    }
}

/// Dependency sources already verified by the host package tool (ADR 0051).
/// Plain data: the driver maps imports to package modules but never reads a
/// manifest, lock, registry or cache, so compiler crates carry no package
/// manager dependencies.
#[derive(Clone, Debug, Default)]
pub struct PackageSet {
    /// Packages the application's own modules may import.
    pub direct: std::collections::BTreeSet<String>,
    pub packages: std::collections::BTreeMap<String, Package>,
    /// Verified bytes of every package source, keyed by absolute path. Only
    /// these files can be package modules.
    pub sources: std::collections::HashMap<PathBuf, String>,
    /// Non-source inputs whose change invalidates a build (manifest, lock, policy).
    pub inputs: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Package {
    /// The package entry module's file; package-local imports resolve beside it.
    pub entry: PathBuf,
    /// Packages this package may import.
    pub dependencies: std::collections::BTreeSet<String>,
}

impl PackageSet {
    /// `import "name"` or `import "name/module"` from `owner` (None: the
    /// application): the private module identity and verified file.
    fn import(&self, owner: Option<&str>, path: &str) -> Result<Option<(String, PathBuf, String)>, String> {
        let (name, rest) = path.split_once('/').unwrap_or((path, ""));
        let visible = match owner {
            Some(owner) => self.packages.get(owner).is_some_and(|p| p.dependencies.contains(name)),
            None => self.direct.contains(name),
        };
        if !visible {
            return Ok(None);
        }
        let package = self.packages.get(name).ok_or_else(|| format!("package `{name}` is declared but not locked"))?;
        let file = if rest.is_empty() { package.entry.clone() } else { package.module_root().join(format!("{rest}.tarn")) };
        if !self.sources.contains_key(&file) {
            return Err(format!("package `{name}` does not contain module `{path}`"));
        }
        let module = if rest.is_empty() { format!("@package/{name}") } else { format!("@package/{name}/{rest}") };
        Ok(Some((module, file, name.to_string())))
    }
}

impl Package {
    fn module_root(&self) -> PathBuf {
        self.entry.parent().map(Path::to_path_buf).unwrap_or_default()
    }
}

/// Load the entry file and every local module it transitively imports.
pub fn load(entry: &Path) -> Result<(Program, Vec<Diagnostic>), String> {
    load_with_overlays(entry, &std::collections::HashMap::new())
}

/// Read open editor buffers before falling back to disk. Paths must be absolute.
pub fn load_with_overlays(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>) -> Result<(Program, Vec<Diagnostic>), String> {
    load_editor_sources(entry, overlays, false, None)
}

/// `load_with_overlays` plus verified package dependencies.
pub fn load_with_packages(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>, packages: Option<&PackageSet>) -> Result<(Program, Vec<Diagnostic>), String> {
    load_editor_sources(entry, overlays, false, packages)
}

fn official_stdlib_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../stdlib/{name}/{name}.tarn"))
}

fn load_editor_sources(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>, editor: bool, packages: Option<&PackageSet>) -> Result<(Program, Vec<Diagnostic>), String> {
    let official_entry = if editor {
        entry.canonicalize().ok().and_then(|entry| stdlib_modules().into_iter().find(|name|
            official_stdlib_path(name).canonicalize().ok().as_ref() == Some(&entry)))
    } else { None };
    let packages = if official_entry.is_none() { packages } else { None };
    let mut import_targets = std::collections::HashMap::new();
    let root = entry.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let stem = entry.file_stem().and_then(|s| s.to_str()).ok_or("invalid file name")?.to_string();
    // Entry filenames do not confer reserved stdlib module identity.
    let stem = if let Some(name) = official_entry { name.to_string() }
        else if trusted_source(&stem).is_some() { format!("entry/{stem}") } else { stem };
    let mut sources = SourceMap::new();
    let mut disk_sources = Vec::new();
    if let Some(packages) = packages {
        disk_sources.extend(packages.sources.keys().cloned());
        disk_sources.extend(packages.inputs.iter().cloned());
    }
    let mut modules: Vec<(String, Module)> = Vec::new();
    let mut diags = Vec::new();
    let mut trusted_modules = std::collections::HashSet::new();
    let mut import_roots = std::collections::HashMap::new();
    let mut queue = vec![(stem, entry.to_path_buf(), None::<String>, root.clone())];
    while let Some((name, path, owner, module_root)) = queue.pop() {
        if modules.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let official_path = official_stdlib_path(&name).canonicalize().ok();
        let editor_text = if editor && (path != entry || official_entry == Some(name.as_str())) && official_module(&name) {
            official_path.as_ref().and_then(|p| overlays.get(p)).cloned()
        } else { None };
        let has_editor_text = editor_text.is_some();
        let text = if let Some(text) = editor_text {
            if trusted_source(&name).is_some() { trusted_modules.insert(name.clone()); }
            text
        } else if let Some(source) = trusted_source(&name) {
            trusted_modules.insert(name.clone());
            source.to_string()
        } else if let Some(text) = packages.and_then(|p| p.sources.get(&path)) {
            if overlays.get(&path).is_some_and(|overlay| overlay != text) { return Err("locked package source cannot be overridden by an editor buffer; edit the development checkout instead".into()); }
            disk_sources.push(path.clone());
            text.clone()
        } else {
            match overlays.get(&path) {
                Some(text) => text.clone(),
                None if !path.is_file() && stdlib_source(&name).is_some() => stdlib_source(&name).unwrap().to_string(),
                None => {
                    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
                    disk_sources.push(path.clone());
                    text
                },
            }
        };
        let display = if editor && (official_entry == Some(name.as_str()) || has_editor_text) {
            official_path.as_ref().unwrap_or(&path).display().to_string()
        } else if trusted_modules.contains(&name) { format!("stdlib/{name}/{name}.tarn") } else { path.strip_prefix(".").unwrap_or(&path).display().to_string() };
        let id = sources.add(display, text);
        let res = tarn_parser::parse_file(id, sources.file(id));
        diags.extend(res.diagnostics);
        for item in &res.module.items {
            // Async syntax depends on the validated execution declarations,
            // even when application source does not explicitly import net.
            let needs_execution = match &item.kind {
                ItemKind::Fn(function) => function.is_async,
                ItemKind::Impl(implementation) => implementation.methods.iter().any(|f| f.is_async),
                _ => false,
            };
            if needs_execution {
                queue.push(("runtime".to_string(), root.join("runtime.tarn"), None, root.clone()));
            }
            if let ItemKind::Import(imp) = &item.kind {
                if imp.path.starts_with("@package/") { return Err("package namespaces are private; import a declared package name".into()); }
                let file = module_root.join(format!("{}.tarn", imp.path));
                let valid = imp.path.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
                if !valid { continue; }
                let dependency = packages.map(|p| p.import(owner.as_deref(), &imp.path)).transpose()?.flatten();
                let local = file.is_file() || overlays.contains_key(&file);
                if local && dependency.is_some() { return Err(format!("import `{}` is ambiguous between a local module and a declared package", imp.path)); }
                if let Some((target, file, package)) = dependency {
                    let module_root = packages.and_then(|p| p.packages.get(&package)).map(Package::module_root).unwrap_or_default();
                    import_targets.insert((name.clone(), imp.path.clone()), target.clone());
                    queue.push((target, file, Some(package), module_root));
                } else if trusted_source(&imp.path).is_some() || (official_module(&imp.path) && !local) {
                    queue.push((imp.path.clone(), root.join(format!("{}.tarn", imp.path)), None, root.clone()));
                } else if local {
                    let target = if let Some(package) = &owner { format!("@package/{package}/{}", imp.path) } else { imp.path.clone() };
                    if owner.is_some() && !packages.is_some_and(|p| p.sources.contains_key(&file)) {
                        return Err("package import is absent from its verified inventory".into());
                    }
                    import_targets.insert((name.clone(), imp.path.clone()), target.clone());
                    queue.push((target, file, owner.clone(), module_root.clone()));
                } else if owner.is_some() {
                    return Err(format!("package module `{name}` imports undeclared or missing dependency `{}`",imp.path));
                }
            }
        }
        let shown = module_root.strip_prefix(".").unwrap_or(&module_root).display().to_string();
        import_roots.insert(name.clone(), if shown.is_empty() { ".".to_string() } else { shown });
        modules.push((name, res.module));
    }
    // `core` is always part of the program (ADR 0020). Embedded in the
    // binary so the compiler is self-contained.
    if !modules.iter().any(|(n, _)| n == "core") {
        let official_core = official_stdlib_path("core").canonicalize().ok();
        let buffer = if editor { official_core.as_ref().and_then(|p| overlays.get(p)) } else { None };
        let display = if buffer.is_some() { official_core.as_ref().unwrap().display().to_string() } else { "stdlib/core/core.tarn".to_string() };
        let id = sources.add(display, buffer.map(String::as_str).unwrap_or(CORE_SOURCE));
        let res = tarn_parser::parse_file(id, sources.file(id));
        diags.extend(res.diagnostics);
        modules.push(("core".to_string(), res.module));
        trusted_modules.insert("core".to_string());
    }
    Ok((Program { import_targets, sources, disk_sources, modules, trusted_modules, import_roots }, diags))
}

/// Lex, parse and resolve. Resolution only runs on syntactically valid
/// programs so that recovery artifacts never produce name errors.
pub fn check(entry: &Path) -> Result<CheckResult, String> {
    check_with_overlays(entry, &std::collections::HashMap::new())
}

/// Check a program including unsaved buffers of imported modules.
pub fn check_with_overlays(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>) -> Result<CheckResult, String> {
    check_loaded(load_with_overlays(entry, overlays)?)
}

/// Check a program whose package dependencies the host tool has verified.
pub fn check_with_packages(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>, packages: Option<&PackageSet>) -> Result<CheckResult, String> {
    check_loaded(load_with_packages(entry, overlays, packages)?)
}

/// Editor-only checking of this checkout's official stdlib source buffers.
/// Normal CLI entry files never gain intrinsic authority from their path/name.
pub fn check_editor_with_overlays(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>) -> Result<CheckResult, String> {
    check_loaded(load_editor_sources(entry, overlays, true, None)?)
}

/// Editor checking with verified package dependencies.
pub fn check_editor_with_packages(entry: &Path, overlays: &std::collections::HashMap<PathBuf, String>, packages: Option<&PackageSet>) -> Result<CheckResult, String> {
    check_loaded(load_editor_sources(entry, overlays, true, packages)?)
}

fn check_loaded(loaded: (Program, Vec<Diagnostic>)) -> Result<CheckResult, String> {
    check_loaded_with_bindings(loaded, &std::collections::HashMap::new())
}

fn check_loaded_with_bindings((program, mut diagnostics): (Program, Vec<Diagnostic>), generated_starts: &std::collections::HashMap<PathBuf, usize>) -> Result<CheckResult, String> {
    let mut resolved = None;
    let mut typed = None;
    let mut ir = None;
    let mut moves = None;
    let mut borrows = None;
    let mut drops = None;
    let errors = |ds: &[Diagnostic]| ds.iter().any(|d| d.severity == Severity::Error);
    if !errors(&diagnostics) {
        let inputs: Vec<ModuleInput> = program.modules.iter().map(|(n, m)| ModuleInput { name: n.clone(), ast: m, trusted_stdlib: program.trusted_modules.contains(n), import_root: program.import_roots.get(n).cloned() }).collect();
        let (mut r, d) = tarn_resolve::resolve_with_imports(&inputs, &program.import_targets);
        testing::bind_generated_prelude(&program, &mut r, generated_starts);
        diagnostics.extend(d);
        // Types only on name-clean programs, for the same reason as above.
        if !errors(&diagnostics) {
            let (t, d) = tarn_types::check(&inputs, &r);
            diagnostics.extend(d);
            if !errors(&diagnostics) {
                let (mut p, d) = tarn_ir::lower_program(&inputs, &r, &t);
                tarn_ir::prove_arithmetic(&mut p);
                diagnostics.extend(d);
                diagnostics.extend(tarn_ir::check_instantiation_cycles(&p, &t, &r));
                diagnostics.extend(tarn_ir::check_value_limits(&p, &t, &r));
                let (m, d) = tarn_ownership::check_moves(&p, &r, &t);
                diagnostics.extend(d);
                let (bo, d) = tarn_ownership::check_borrows(&p, &r, &t, &m.failed());
                diagnostics.extend(d);
                if !errors(&diagnostics) {
                    drops = Some(tarn_ownership::elaborate_drops(&p, &t, &m).map_err(|bugs| format!("compiler bug in drop elaboration:\n{}", bugs.join("\n")))?);
                }
                borrows = Some(bo);
                moves = Some(m);
                ir = Some(p);
            }
            typed = Some(t);
        }
        resolved = Some(r);
    }
    diagnostics.sort_by_key(|d| d.primary_span().map(|s| (s.file, s.start)));
    Ok(CheckResult { program, resolved, typed, ir, moves, borrows, drops, diagnostics })
}
