//! Linking freestanding objects into a static ELF (ADR 0101, P6).
//!
//! Tarn does not bundle a linker. A `*-none` object is linked with an ELF
//! linker the user already has: `TARN_LD` if set, otherwise the first of
//! `ld.lld` (LLVM), a cross GNU `ld` for the architecture, or the host `ld`
//! when the host is Linux of the same architecture. Nothing is downloaded.
use crate::{Arch, Error, Os, Result, Target};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Names tried on `PATH`, in order.
pub fn linker_candidates(target: Target) -> Vec<&'static str> {
    let mut names = vec!["ld.lld"];
    match target.arch {
        Arch::X86_64 => names.extend(["x86_64-elf-ld", "x86_64-linux-gnu-ld", "x86_64-unknown-elf-ld"]),
        Arch::Aarch64 => names.extend(["aarch64-elf-ld", "aarch64-none-elf-ld", "aarch64-linux-gnu-ld", "aarch64-unknown-elf-ld"]),
    }
    if Target::host().is_some_and(|h| h.os == Os::Linux && h.arch == target.arch) {
        names.push("ld");
    }
    names
}

/// Directories package managers put LLVM in without adding it to `PATH`.
const EXTRA_DIRECTORIES: [&str; 4] = ["/opt/homebrew/opt/lld/bin", "/usr/local/opt/lld/bin", "/opt/homebrew/opt/llvm/bin", "/usr/local/opt/llvm/bin"];

/// The linker to use for `target`, if one exists.
pub fn find_linker(target: Target) -> Option<PathBuf> {
    find_linker_in(target, std::env::var_os("TARN_LD"), &std::env::var_os("PATH").unwrap_or_default(), &EXTRA_DIRECTORIES)
}

fn find_linker_in(target: Target, explicit: Option<std::ffi::OsString>, path: &std::ffi::OsStr, extra: &[&str]) -> Option<PathBuf> {
    if let Some(explicit) = explicit.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    let directories: Vec<PathBuf> = std::env::split_paths(path).chain(extra.iter().map(PathBuf::from)).collect();
    linker_candidates(target).into_iter().find_map(|name| directories.iter().map(|d| d.join(name)).find(|c| c.is_file()))
}

/// What to tell a user who has no linker.
pub fn missing_linker_message(target: Target) -> String {
    format!(
        "no ELF linker found for {target}: install LLVM's `ld.lld` (for example `brew install lld` or `apt install lld`), a cross `{}`, or set TARN_LD to one; \
         without --linker-script `tarn build --target {target}` writes the relocatable object instead",
        linker_candidates(target).get(1).copied().unwrap_or("ld")
    )
}

/// Does the script choose its own entry point (`ENTRY(symbol)`)?
pub fn script_names_entry(script: &str) -> bool {
    script.lines().any(|l| l.split("/*").next().unwrap_or("").contains("ENTRY("))
}

/// Link `object` into the static ELF `output` with `script`. `entry`
/// overrides the script's `ENTRY`. The output is published by atomic rename.
pub fn link_freestanding(object: &Path, output: &Path, target: Target, script: &Path, entry: Option<&str>) -> Result<()> {
    if target.os != Os::None {
        return Err(Error { message: format!("linking a kernel needs a `*-none` target, not {target}") });
    }
    if !script.is_file() {
        return Err(Error { message: format!("linker script `{}` not found", script.display()) });
    }
    let Some(linker) = find_linker(target) else { return Err(Error { message: missing_linker_message(target) }) };
    run_linker(&linker, object, output, script, entry)
}

/// `linker -nostdlib -static -T script [-e entry] -o output object`.
pub fn run_linker(linker: &Path, object: &Path, output: &Path, script: &Path, entry: Option<&str>) -> Result<()> {
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let pending = parent.join(format!(".tarn-link-{}", std::process::id()));
    let mut command = Command::new(linker);
    command.env("LC_ALL", "C").args(["-nostdlib", "-static", "-T"]).arg(script);
    if let Some(entry) = entry {
        command.arg("-e").arg(entry);
    }
    let result = command.arg("-o").arg(&pending).arg(object).output().map_err(|e| Error { message: format!("cannot run linker `{}`: {e}", linker.display()) })?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&pending);
        return Err(Error { message: format!("linker `{}` failed:\n{}", linker.display(), String::from_utf8_lossy(&result.stderr).trim_end()) });
    }
    std::fs::rename(&pending, output).map_err(|e| Error { message: format!("native build I/O: {e}") })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_directive_is_found_outside_comments() {
        assert!(script_names_entry("ENTRY(kmain)\nSECTIONS { }"));
        assert!(!script_names_entry("/* ENTRY(old) */\nSECTIONS { }"));
        assert!(!script_names_entry("SECTIONS { . = 1M; }"));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tarn-link-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn linker_is_found_on_the_given_path_or_by_env_override() {
        let target: Target = "x86_64-none".parse().unwrap();
        let dir = scratch("find");
        assert_eq!(find_linker_in(target, None, dir.as_os_str(), &[]), None);
        std::fs::write(dir.join("x86_64-elf-ld"), "").unwrap();
        assert_eq!(find_linker_in(target, None, dir.as_os_str(), &[]), Some(dir.join("x86_64-elf-ld")));
        // The LLVM linker wins over a cross GNU one.
        std::fs::write(dir.join("ld.lld"), "").unwrap();
        assert_eq!(find_linker_in(target, None, dir.as_os_str(), &[]), Some(dir.join("ld.lld")));
        // TARN_LD wins over everything; an empty value is ignored.
        assert_eq!(find_linker_in(target, Some("/opt/mine/ld".into()), dir.as_os_str(), &[]), Some(PathBuf::from("/opt/mine/ld")));
        assert_eq!(find_linker_in(target, Some("".into()), dir.as_os_str(), &[]), Some(dir.join("ld.lld")));
        // The other architecture's cross linker is not a candidate.
        let arm: Target = "aarch64-none".parse().unwrap();
        std::fs::remove_file(dir.join("ld.lld")).unwrap();
        assert_eq!(find_linker_in(arm, None, dir.as_os_str(), &[]), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_linker_message_names_what_to_install() {
        let message = missing_linker_message("x86_64-none".parse().unwrap());
        assert!(message.contains("no ELF linker found for x86_64-none") && message.contains("ld.lld") && message.contains("x86_64-elf-ld") && message.contains("TARN_LD"), "{message}");
        assert!(missing_linker_message("aarch64-none".parse().unwrap()).contains("aarch64-elf-ld"));
    }

    #[cfg(unix)]
    #[test]
    fn linker_is_run_with_static_script_entry_and_published_by_rename() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("run");
        let fake = dir.join("fake-ld");
        std::fs::write(&fake, "#!/bin/sh\necho \"$@\" > \"$0.args\"\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"-o\" ]; then out=\"$2\"; fi; shift; done\nprintf 'ELF' > \"$out\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (object, script, output) = (dir.join("k.o"), dir.join("k.ld"), dir.join("kernel.elf"));
        std::fs::write(&object, "").unwrap();
        run_linker(&fake, &object, &output, &script, Some("kmain")).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"ELF");
        let args = std::fs::read_to_string(dir.join("fake-ld.args")).unwrap();
        assert!(args.starts_with("-nostdlib -static -T "), "{args}");
        assert!(args.contains("k.ld -e kmain -o ") && args.trim_end().ends_with("k.o"), "{args}");
        run_linker(&fake, &object, &output, &script, None).unwrap();
        assert!(!std::fs::read_to_string(dir.join("fake-ld.args")).unwrap().contains(" -e "));
        // A failing linker leaves no output and reports its stderr.
        let failing = dir.join("failing-ld");
        std::fs::write(&failing, "#!/bin/sh\necho 'undefined symbol: boom' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rejected = dir.join("rejected.elf");
        let error = run_linker(&failing, &object, &rejected, &script, None).unwrap_err().to_string();
        assert!(error.contains("failed") && error.contains("undefined symbol: boom"), "{error}");
        assert!(!rejected.exists());
        let missing = run_linker(&dir.join("no-such-ld"), &object, &rejected, &script, None).unwrap_err().to_string();
        assert!(missing.contains("cannot run linker"), "{missing}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn candidates_prefer_lld() {
        let target: Target = "aarch64-none".parse().unwrap();
        assert_eq!(linker_candidates(target)[0], "ld.lld");
        assert!(linker_candidates(target).contains(&"aarch64-elf-ld"));
    }
}
