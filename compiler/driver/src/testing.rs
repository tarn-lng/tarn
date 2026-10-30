//! Test discovery and generated ordinary source harnesses (ADR 0049).
use crate::{CheckResult, PackageSet};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tarn_ast::ItemKind;
use tarn_types::Ty;

pub const ENTRY: &str = "__tarn_test_dispatch";
/// Virtual files the harness adds at the module root; never read from disk.
const ROOT_FILE: &str = "__tarn_tests__.tarn";
const SHIM_PREFIX: &str = "__tarn_test_shim_";

#[derive(Clone, Debug)]
pub struct TestCase {
    pub name: String,
    pub index: usize,
}

/// What `tarn test <target>` runs (ADR 0093). Imports always resolve from
/// `root`: the package root when a `tarn.toml` encloses the target, otherwise
/// the target's directory, exactly as the application resolves them.
#[derive(Clone, Debug)]
pub struct TestPlan {
    pub root: PathBuf,
    /// Modules whose `test_*`/`bench_*` functions are collected, sorted.
    pub files: Vec<PathBuf>,
    /// For a directory target: other package modules below it that the tests
    /// load also contribute their inline tests.
    pub scope: Option<PathBuf>,
}

fn package_root(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() { start.to_path_buf() } else { start.parent()?.to_path_buf() };
    loop {
        if dir.join("tarn.toml").is_file() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// `*_test.tarn` files below `dir`. Hidden directories (build copies such as
/// `.build`), `target` and nested packages (their own `tarn.toml`) are skipped.
fn collect(dir: &Path, top: bool, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !top && dir.join("tarn.toml").is_file() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                collect(&path, false, out)?;
            }
        } else if name.ends_with("_test.tarn") {
            out.push(path);
        }
    }
    Ok(())
}

/// A directory runs every test file below it, plus the package entry (or
/// `main.tarn`) when it lies inside. A file runs its own tests; a module
/// `x.tarn` also runs its sibling `x_test.tarn`.
pub fn plan(target: &Path) -> Result<TestPlan, String> {
    let target = target.canonicalize().map_err(|e| format!("cannot read `{}`: {e}", target.display()))?;
    let package = package_root(&target);
    let root = package.clone().unwrap_or_else(|| if target.is_dir() { target.clone() } else { target.parent().unwrap_or(Path::new("/")).to_path_buf() });
    let mut files = Vec::new();
    if target.is_dir() {
        collect(&target, true, &mut files)?;
        let entry = package
            .as_ref()
            .and_then(|r| std::fs::read_to_string(r.join("tarn.toml")).ok().and_then(|t| manifest_entry(&t)).map(|e| r.join(e)))
            .unwrap_or_else(|| target.join("main.tarn"));
        if let Ok(entry) = entry.canonicalize()
            && entry.starts_with(&target)
        {
            files.push(entry);
        }
    } else {
        files.push(target.clone());
        let stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !stem.ends_with("_test") {
            let sibling = target.with_file_name(format!("{stem}_test.tarn"));
            if sibling.is_file() {
                files.push(sibling);
            }
        }
    }
    files.sort();
    files.dedup();
    for file in &files {
        if !file.starts_with(&root) {
            return Err(format!("`{}` is outside the module root `{}`", file.display(), root.display()));
        }
    }
    let scope = target.is_dir().then_some(target);
    Ok(TestPlan { root, files, scope })
}

/// `package.entry` without depending on the package tooling (which depends on
/// this crate); the manifest was validated by the host before tests run.
fn manifest_entry(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.trim_start().starts_with("entry"))?;
    let value = line.split_once('=')?.1.trim();
    let value = value.split(['"', '\'']).nth(1)?;
    Some(value.to_string())
}

/// Files `tarn test --watch` observes for additions and removals.
pub fn test_files(target: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(plan(target)?.files)
}

pub fn check_tests(target: &Path) -> Result<(CheckResult, Vec<TestCase>), String> {
    check_tests_with_packages(target, None)
}

/// `check_tests` for a project whose package dependencies the host verified.
pub fn check_tests_with_packages(target: &Path, packages: Option<&PackageSet>) -> Result<(CheckResult, Vec<TestCase>), String> {
    check_harness(target, packages, "test_")
}

/// Benchmarks (Phase 30): `bench_*` functions, discovered, checked and
/// dispatched exactly like tests; the CLI measures each run.
pub fn check_benches_with_packages(target: &Path, packages: Option<&PackageSet>) -> Result<(CheckResult, Vec<TestCase>), String> {
    check_harness(target, packages, "bench_")
}

fn module_path(root: &Path, file: &Path) -> Result<String, String> {
    let relative = file.strip_prefix(root).map_err(|_| "test file outside the module root")?;
    let path = relative.with_extension("").to_str().ok_or("invalid test file name")?.replace('\\', "/");
    if !path.split('/').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') && !s.as_bytes()[0].is_ascii_digit()) {
        return Err(format!("invalid test module path `{path}`"));
    }
    Ok(path)
}

fn load(root: &Path, overlays: &HashMap<PathBuf, String>, packages: Option<&PackageSet>) -> Result<(crate::Program, Vec<tarn_diagnostics::Diagnostic>), String> {
    crate::load_sources(&root.join(ROOT_FILE), Some(root), overlays, false, packages)
}

/// Drop warnings that point into generated text: they describe the harness,
/// not the user's code.
fn without_generated(mut result: CheckResult, starts: &HashMap<PathBuf, usize>) -> CheckResult {
    let sources = &result.program.sources;
    result.diagnostics.retain(|d| {
        d.severity == tarn_diagnostics::Severity::Error
            || !d.primary_span().is_some_and(|span| starts.get(&PathBuf::from(&sources.file(span.file).name)).is_some_and(|start| span.start as usize >= *start))
    });
    result
}

fn check_harness(target: &Path, packages: Option<&PackageSet>, prefix: &str) -> Result<(CheckResult, Vec<TestCase>), String> {
    let kind = if prefix == "bench_" { "benchmark" } else { "test" };
    let TestPlan { root, mut files, scope } = plan(target)?;
    for name in [ROOT_FILE.to_string(), format!("{SHIM_PREFIX}0.tarn")] {
        if root.join(&name).exists() {
            return Err(format!("`{name}` is reserved for the generated test harness"));
        }
    }
    let mut text: HashMap<PathBuf, String> = HashMap::new();
    let mut starts: HashMap<PathBuf, usize> = HashMap::new();
    let mut paths = Vec::new();
    for file in &files {
        text.insert(file.clone(), std::fs::read_to_string(file).map_err(|e| format!("cannot read `{}`: {e}", file.display()))?);
        paths.push(module_path(&root, file)?);
    }
    // Pass 1: every candidate module, each behind its own shim so that equal
    // file names in different directories never collide as import names.
    let mut overlays: HashMap<PathBuf, String> = text.iter().map(|(p, s)| (p.clone(), s.clone())).collect();
    let mut root_text = String::new();
    for (k, path) in paths.iter().enumerate() {
        let shim = root.join(format!("{SHIM_PREFIX}{k}.tarn"));
        overlays.insert(shim.clone(), format!("import \"{path}\"\n"));
        starts.insert(shim, 0);
        root_text.push_str(&format!("import \"{SHIM_PREFIX}{k}\"\n"));
    }
    overlays.insert(root.join(ROOT_FILE), root_text);
    starts.insert(root.join(ROOT_FILE), 0);
    let (mut initial_program, diagnostics) = load(&root, &overlays, packages)?;
    initial_program.disk_sources.extend(files.iter().cloned());
    let initial = without_generated(crate::check_loaded((initial_program, diagnostics))?, &starts);
    if initial.has_errors() {
        return Ok((initial, Vec::new()));
    }
    let typed = initial.typed.as_ref().ok_or("missing test type tables")?;
    let resolved = initial.resolved.as_ref().ok_or("missing test resolution tables")?;
    // Inline tests of the package modules the tests load, inside the scope.
    if let Some(scope) = &scope {
        for (name, module) in &initial.program.modules {
            if paths.contains(name) || initial.program.trusted_modules.contains(name) || name.starts_with('@') || name.starts_with(SHIM_PREFIX) || name.starts_with("__tarn_tests__") {
                continue;
            }
            let file = PathBuf::from(&initial.program.sources.file(module.file).name);
            if file.starts_with(scope) && file.starts_with(&root) && module.items.iter().any(|i| matches!(&i.kind, ItemKind::Fn(f) if f.owner.is_none() && f.name.name.starts_with(prefix))) {
                text.insert(file.clone(), std::fs::read_to_string(&file).map_err(|e| format!("cannot read `{}`: {e}", file.display()))?);
                paths.push(name.clone());
                files.push(file);
            }
        }
    }
    for file in &files {
        starts.insert(file.clone(), text[file].len());
    }
    let mut cases = Vec::new();
    let mut shims: Vec<String> = vec![String::new(); paths.len()];
    for (module_index, (module_name, module)) in initial.program.modules.iter().enumerate() {
        let Some(k) = paths.iter().position(|p| p == module_name) else { continue };
        let file = &files[k];
        if module.items.iter().any(|item| matches!(&item.kind, ItemKind::Fn(f) if f.name.name.starts_with("__tarn_test_"))) {
            return Err("function names beginning with __tarn_test_ are reserved for generated test harnesses".into());
        }
        for item in &module.items {
            let ItemKind::Fn(function) = &item.kind else { continue };
            if function.owner.is_some() || !function.name.name.starts_with(prefix) {
                continue;
            }
            let symbol = resolved.tables[module_index]
                .defs
                .get(&item.id)
                .or_else(|| resolved.tables[module_index].defs.get(&function.id))
                .ok_or("missing test symbol")?;
            let signature = &typed.decls.fns[symbol];
            let name = format!("{module_name}.{}", function.name.name);
            if function.is_async || function.is_unsafe || function.abi.is_some() || !signature.generics.is_empty() || !signature.params.is_empty() || function.receiver.is_some() {
                return Err(format!("{kind} `{name}` must be a safe, synchronous, non-generic function with no parameters"));
            }
            let index = cases.len();
            let body = if signature.ret == Ty::Void {
                format!("{}()\n", function.name.name)
            } else if let Ty::Adt(id, args) = &signature.ret {
                if Some(*id) != typed.decls.result || args.len() != 2 || args[0] != Ty::Void {
                    return Err(format!("{kind} `{name}` must return void or Result<void, E>"));
                }
                format!(
                    "match {}() {{\nOk(_) => {{}}\nErr(error) => {{\n{}}}\n}}\n",
                    function.name.name,
                    err_report(typed, resolved, function, &args[1], module_index)?
                )
            } else {
                return Err(format!("{kind} `{name}` must return void or Result<void, E>"));
            };
            text.get_mut(file).unwrap().push_str(&format!("\npub fn __tarn_test_{index}() {{\n{body}}}\n"));
            let alias = module_name.rsplit('/').next().unwrap_or(module_name);
            shims[k].push_str(&format!("if selected == {index} {{\n{alias}.__tarn_test_{index}()\nreturn true\n}}\n"));
            cases.push(TestCase { name, index });
        }
    }
    // Pass 2: only modules with tests are dispatched; the rest are reached
    // through ordinary imports, if at all.
    let mut overlays: HashMap<PathBuf, String> = text.iter().map(|(p, s)| (p.clone(), s.clone())).collect();
    let mut root_text = String::new();
    let mut dispatch = String::from("\nfn __tarn_test_dispatch(selected i32) {\n");
    for (k, body) in shims.iter().enumerate().filter(|(_, b)| !b.is_empty()) {
        overlays.insert(
            root.join(format!("{SHIM_PREFIX}{k}.tarn")),
            format!("import \"{}\"\npub fn run(selected i32) bool {{\n{body}return false\n}}\n", paths[k]),
        );
        root_text.push_str(&format!("import \"{SHIM_PREFIX}{k}\"\n"));
        dispatch.push_str(&format!("if {SHIM_PREFIX}{k}.run(selected) {{\nreturn\n}}\n"));
    }
    dispatch.push_str("panic(\"invalid test index\")\n}\n");
    root_text.push_str(&dispatch);
    overlays.insert(root.join(ROOT_FILE), root_text);
    let (mut program, diagnostics) = load(&root, &overlays, packages)?;
    // Real files remain watch dependencies; generated ones are not on disk.
    program.disk_sources.extend(files.iter().cloned());
    program.disk_sources.sort();
    program.disk_sources.dedup();
    let result = crate::check_loaded_with_bindings((program, diagnostics), &starts)?;
    Ok((without_generated(result, &starts), cases))
}

/// The body that reports a test's `Err` value in one line, through `panic`
/// so it appears as the failure reason: `test returned Err(Kind.Refused(3))`.
/// Values implementing `Display` are shown; others keep their type boundary.
fn err_report(
    typed: &tarn_types::Typed,
    resolved: &tarn_resolve::Resolved,
    function: &tarn_ast::FnDecl,
    error: &Ty,
    module_index: usize,
) -> Result<String, String> {
    let shown = |ty: &Ty| matches!(ty, Ty::Bool | Ty::Int(_) | Ty::Str);
    let escape = |s: &str| s.replace('{', "{{").replace('}', "}}").replace('"', "\\\"");
    Ok(match error {
        ty if shown(ty) => "panic(f\"test returned Err({error})\")\n".to_string(),
        Ty::Adt(id, arguments) => {
            let type_name = escape(&resolved.symbols[id.0 as usize].name);
            if let Some(def) = typed.decls.structs.get(id) {
                let substitution: HashMap<_, _> = def.generics.iter().copied().zip(arguments.iter().cloned()).collect();
                let fields: Vec<String> = def
                    .fields
                    .iter()
                    .filter(|f| (f.is_pub || def.module.0 as usize == module_index) && shown(&tarn_types::subst(&f.ty, &substitution)))
                    .map(|f| format!("{}: {{error.{}}}", f.name, f.name))
                    .collect();
                format!("panic(f\"test returned Err({type_name}{{{{{}}}}})\")\n", fields.join(", "))
            } else if let Some(def) = typed.decls.enums.get(id) {
                let type_path = function
                    .ret
                    .as_ref()
                    .and_then(|ret| match &ret.kind {
                        tarn_ast::TypeKind::Path(path) => path.args.get(1).and_then(|error| match &error.kind {
                            tarn_ast::TypeKind::Path(path) => Some(path.segments.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(".")),
                            _ => None,
                        }),
                        _ => None,
                    })
                    .ok_or("enum error type must have a source path")?;
                let substitution: HashMap<_, _> = def.generics.iter().copied().zip(arguments.iter().cloned()).collect();
                let mut out = String::from("match error {\n");
                for variant in &def.variants {
                    let bindings: Vec<String> = (0..variant.fields.len()).map(|i| format!("error_part_{i}")).collect();
                    let pattern = if bindings.is_empty() { String::new() } else { format!("({})", bindings.join(", ")) };
                    let parts: Vec<String> = bindings
                        .iter()
                        .zip(&variant.fields)
                        .map(|(b, ty)| if shown(&tarn_types::subst(ty, &substitution)) { format!("{{{b}}}") } else { "_".into() })
                        .collect();
                    let payload = if parts.is_empty() { String::new() } else { format!("({})", parts.join(", ")) };
                    out.push_str(&format!(
                        "{type_path}.{}{pattern} => {{\npanic(f\"test returned Err({type_name}.{}{payload})\")\n}}\n",
                        variant.name, variant.name
                    ));
                }
                out.push_str("}\n");
                out
            } else {
                format!("panic(\"test returned Err({type_name})\")\n")
            }
        }
        _ => "panic(\"test returned Err(non-scalar value)\")\n".to_string(),
    })
}

/// Compiler-generated scaffolding binds its prelude references explicitly.
/// Only appended source is affected; application shadowing retains normal semantics.
pub(crate) fn bind_generated_prelude(
    program: &crate::Program,
    resolved: &mut tarn_resolve::Resolved,
    starts: &HashMap<PathBuf, usize>,
) {
    let bindings: HashMap<_, _> = ["print", "panic", "i32", "Ok", "Err"]
        .into_iter()
        .filter_map(|name| {
            resolved
                .scope(tarn_resolve::ScopeId(0))
                .get(name)
                .map(|id| (name, id))
        })
        .collect();
    for table in &mut resolved.tables {
        for usage in table.uses.values_mut() {
            let source = program.sources.file(usage.span.file);
            if starts
                .get(&PathBuf::from(&source.name))
                .is_some_and(|start| usage.span.start as usize >= *start)
            {
                if let Some(symbol) = bindings.get(program.sources.snippet(usage.span)) {
                    usage.res = tarn_resolve::Res::Symbol(*symbol);
                }
            }
        }
    }
}
