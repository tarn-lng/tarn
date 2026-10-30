//! Ahead-of-time Linux x86_64 and AArch64 backend. Ownership semantics come exclusively
//! from post-drop IR. No move checker, loans or provenance enter this crate.
mod codegen;
pub mod layout;
mod inline;
mod integer_narrow;
pub mod mono;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tarn_ir::post_drop;
use tarn_types::Typed;

#[derive(Debug)]
pub struct Error {
    pub message: String,
}
impl Error {
    pub(crate) fn unsupported(s: impl Into<String>) -> Self {
        Self { message: format!("backend not implemented: {}", s.into()) }
    }
    pub(crate) fn bug(s: impl Into<String>) -> Self {
        Self { message: format!("compiler bug in native backend: {}", s.into()) }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
const RUNTIME: &str = include_str!("../../../runtime/native.c");

/// Check executable dynamic metadata on a specialized post-drop program.
pub fn verify_dynamic(p: &post_drop::Program, t: &Typed) -> Result<()> {
    let bugs = post_drop::verify(p, t);
    if !bugs.is_empty() {
        return Err(Error::bug(bugs.join("\n")));
    }
    codegen::verify_dynamic(p, t)
}

pub fn emit_object(p: &post_drop::Program, t: &Typed) -> Result<Vec<u8>> {
    emit_entry_object(p, t, None)
}

fn emit_entry_object(p: &post_drop::Program, t: &Typed, entry: Option<tarn_ir::FunctionId>) -> Result<Vec<u8>> {
    if !cfg!(all(target_os = "linux", target_endian = "little", any(target_arch = "x86_64", target_arch = "aarch64"))) {
        return Err(Error::unsupported("target must be little-endian Linux x86_64 or aarch64"));
    }
    let bugs = post_drop::verify(p, t);
    if !bugs.is_empty() {
        return Err(Error::bug(bugs.join("\n")));
    }
    let specialized = if let Some(entry) = entry { mono::specialize_entry(p, t, entry)? } else { mono::specialize(p, t)? };
    let mut concrete = inline::inline(specialized, t);
    tarn_ir::optimize_scalar_loops(&mut concrete);
    let bugs = post_drop::verify(&concrete, t);
    if !bugs.is_empty() {
        return Err(Error::bug(bugs.join("\n")));
    }
    if entry.is_some() { codegen::emit_entry(&concrete, t, true) } else { codegen::emit(&concrete, t) }
}

/// Build to the requested executable path. Object/runtime intermediates live in
/// a unique scratch directory and are removed on every return path. No shell.
pub fn build(p: &post_drop::Program, t: &Typed, output: &Path) -> Result<()> {
    build_linked(p, t, output, &[])
}

/// Valid `--link` library: `name` (`-lname`) or `:file` (`-l:file`), never an
/// option or a path. Linking is an explicit build-time grant (ADR 0047).
pub fn valid_library(name: &str) -> bool {
    let file = name.strip_prefix(':').unwrap_or(name);
    !file.is_empty() && !file.starts_with('-') && file.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

/// `build` plus system libraries granted with `tarn build --link`.
pub fn build_linked(p: &post_drop::Program, t: &Typed, output: &Path, libraries: &[String]) -> Result<()> {
    build_entry(p, t, output, libraries, None)
}

/// Build a frontend-generated test entry with its private argv adapter.
pub fn build_tests(p: &post_drop::Program, t: &Typed, output: &Path, libraries: &[String], entry: tarn_ir::FunctionId) -> Result<()> {
    build_entry(p, t, output, libraries, Some(entry))
}

fn build_entry(p: &post_drop::Program, t: &Typed, output: &Path, libraries: &[String], entry: Option<tarn_ir::FunctionId>) -> Result<()> {
    if let Some(bad) = libraries.iter().find(|l| !valid_library(l)) {
        return Err(Error { message: format!("invalid library `{bad}`: use a name such as `sqlite3` or an exact file such as `:libsqlite3.so.0`") });
    }
    let bytes = emit_entry_object(p, t, entry)?;
    let scratch = Scratch::new()?;
    let object = scratch.0.join("program.o");
    let runtime = scratch.0.join("runtime.c");
    // Link beside the destination, then publish by rename. The compiler must
    // never open the final executable for writing: concurrent child launches
    // can otherwise inherit a writer transiently and fail with ETXTBSY.
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let publication = Scratch::in_directory(parent)?;
    let executable = publication.0.join("program");
    std::fs::write(&object, bytes).map_err(io_error)?;
    let runtime = match cached_runtime(&scratch) {
        Some(cached) => cached,
        None => {
            std::fs::write(&runtime, RUNTIME).map_err(io_error)?;
            runtime
        }
    };
    // Untranslated linker diagnostics keep linker_message deterministic.
    let linked = Command::new("cc")
        .env("LC_ALL", "C")
        .args(RUNTIME_FLAGS)
        .arg(&object)
        .arg(&runtime)
        .args(libraries.iter().map(|l| format!("-l{l}")))
        .arg("-lm")
        .arg("-o")
        .arg(&executable)
        .output()
        .map_err(io_error)?;
    if !linked.status.success() {
        return Err(Error { message: linker_message(&String::from_utf8_lossy(&linked.stderr), libraries) });
    }
    // Do not replace an existing executable until codegen and linking succeed.
    std::fs::rename(executable, output).map_err(io_error)?;
    Ok(())
}
/// Summarize the system linker's failure in source-level terms: missing C
/// symbols and missing libraries, with the raw output kept for anything else.
fn linker_message(stderr: &str, libraries: &[String]) -> String {
    let mut symbols: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.split("undefined reference to `").nth(1))
        .filter_map(|rest| rest.split('\'').next())
        .collect();
    symbols.sort_unstable();
    symbols.dedup();
    let missing: Vec<&str> = stderr.lines().filter_map(|l| l.split("cannot find -l").nth(1)).map(|l| l.split(':').next().unwrap_or(l).trim()).collect();
    if !missing.is_empty() {
        return format!("native linker cannot find library {}; install it or pass the exact file, e.g. `--link :libname.so.0`", missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", "));
    }
    if !symbols.is_empty() {
        let mut suggested: Vec<&str> = symbols.iter().filter_map(|s| likely_library(s)).filter(|l| !libraries.iter().any(|x| x == l)).collect();
        suggested.sort_unstable();
        suggested.dedup();
        let hint = if !suggested.is_empty() {
            format!(
                "these usually come from {}; add {} to `[native] links` in tarn.toml (or pass `--link`)",
                suggested.iter().map(|l| format!("`{l}`")).collect::<Vec<_>>().join(", "),
                if suggested.len() == 1 { "it" } else { "them" }
            )
        } else if libraries.is_empty() {
            "declare the library in `[native] links` in tarn.toml or with `--link <name>`".to_string()
        } else {
            "check the extern \"C\" names and the linked libraries".to_string()
        };
        return format!("native linker found no definition for extern \"C\" {}; {hint}", symbols.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", "));
    }
    format!("native linker failed: {stderr}")
}
/// The system library that usually defines a C symbol, by well-known
/// prefixes. Only a hint for the linker message; never linked implicitly.
fn likely_library(symbol: &str) -> Option<&'static str> {
    const PREFIXES: &[(&str, &str)] = &[
        ("gtk_", "gtk-4"),
        ("gdk_", "gtk-4"),
        ("gsk_", "gtk-4"),
        ("adw_", "adwaita-1"),
        ("g_application_", "gio-2.0"),
        ("g_list_model_", "gio-2.0"),
        ("g_list_store_", "gio-2.0"),
        ("g_file_", "gio-2.0"),
        ("g_settings_", "gio-2.0"),
        ("g_dbus_", "gio-2.0"),
        ("g_menu_", "gio-2.0"),
        ("g_action_", "gio-2.0"),
        ("g_simple_action_", "gio-2.0"),
        ("g_notification_", "gio-2.0"),
        ("g_icon_", "gio-2.0"),
        ("g_object_", "gobject-2.0"),
        ("g_signal_", "gobject-2.0"),
        ("g_type_", "gobject-2.0"),
        ("g_value_", "gobject-2.0"),
        ("g_closure_", "gobject-2.0"),
        ("g_param_spec_", "gobject-2.0"),
        ("g_", "glib-2.0"),
        ("pango_", "pango-1.0"),
        ("cairo_", "cairo"),
        ("sqlite3_", "sqlite3"),
        ("curl_", "curl"),
        ("SSL_", "ssl"),
        ("EVP_", "crypto"),
    ];
    PREFIXES.iter().find(|(prefix, _)| symbol.starts_with(prefix)).map(|(_, lib)| *lib)
}

/// Flags for the embedded runtime and the final link, shared by both paths.
const RUNTIME_FLAGS: [&str; 5] = ["-std=c11", "-O2", "-fno-strict-aliasing", "-no-pie", "-pthread"];

/// The embedded C runtime never changes for a given compiler, so its object
/// is compiled once per (runtime source, flags, C compiler identity) and kept
/// in `~/.tarn/artifacts/runtime/`. Publication is an atomic rename, so
/// concurrent builds never observe a partial object. Any failure falls back
/// to compiling the runtime inside this build; `TARN_NO_RUNTIME_CACHE` opts out.
fn cached_runtime(scratch: &Scratch) -> Option<PathBuf> {
    if std::env::var_os("TARN_NO_RUNTIME_CACHE").is_some() {
        return None;
    }
    // The C compiler is identified by its resolved executable (path, size and
    // modification time) without running it: an upgrade changes the key.
    let compiler = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join("cc")).find(|c| c.is_file()))?;
    let compiler = std::fs::canonicalize(compiler).ok()?;
    let metadata = std::fs::metadata(&compiler).ok()?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    let key = {
        use std::hash::{Hash, Hasher};
        let half = |seed: u8| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (seed, std::env::consts::ARCH, std::env::consts::OS, RUNTIME, RUNTIME_FLAGS, &compiler, metadata.len(), modified).hash(&mut h);
            h.finish()
        };
        format!("{:016x}{:016x}", half(0), half(1))
    };
    let directory = PathBuf::from(std::env::var_os("HOME")?).join(".tarn/artifacts/runtime");
    let cached = directory.join(format!("{key}.o"));
    if cached.is_file() {
        return Some(cached);
    }
    std::fs::create_dir_all(&directory).ok()?;
    let source = scratch.0.join("runtime.c");
    std::fs::write(&source, RUNTIME).ok()?;
    let compiled = scratch.0.join("runtime.o");
    let status = Command::new("cc").env("LC_ALL", "C").args(RUNTIME_FLAGS).arg("-c").arg(&source).arg("-o").arg(&compiled).status().ok()?;
    if !status.success() {
        return None;
    }
    let pending = directory.join(format!(".{key}.{}.tmp", std::process::id()));
    std::fs::copy(&compiled, &pending).ok()?;
    if std::fs::rename(&pending, &cached).is_err() {
        let _ = std::fs::remove_file(&pending);
        return Some(compiled);
    }
    Some(cached)
}
fn io_error(e: std::io::Error) -> Error {
    Error { message: format!("native build I/O: {e}") }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        Self::in_directory(&std::env::temp_dir())
    }
    fn in_directory(directory: &Path) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..1000 {
            let p = directory.join(format!(".tarn-native-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            match std::fs::create_dir(&p) {
                Ok(()) => return Ok(Self(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error(e)),
            }
        }
        Err(Error { message: "cannot allocate native build directory".into() })
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod linker_tests {
    #[test]
    fn missing_symbols_suggest_their_library() {
        let stderr = "main.o: in function `main':\n(.text+0x1): undefined reference to `g_list_model_get_n_items'\n(.text+0x2): undefined reference to `gtk_window_new'\n";
        let message = super::linker_message(stderr, &["gtk-4".into()]);
        assert!(message.contains("`gio-2.0`") && !message.contains("`gtk-4`;"), "{message}");
        assert!(super::linker_message("undefined reference to `my_own_symbol'\n", &[]).contains("[native] links"));
    }
}
