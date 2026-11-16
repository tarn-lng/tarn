//! Build identity for `tarn --version`: commit, branch and build date, so two
//! binaries with the same version number can still be told apart.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(env!("CARGO_MANIFEST_DIR")).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    let commit = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).map(|s| if s.is_empty() { "" } else { "+dirty" }).unwrap_or("");
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=TARN_BUILD_ID={branch} {commit}{dirty}, {date}");
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&dir);
        println!("cargo:rerun-if-changed={}", root.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", root.join("index").display());
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", root.join(r).display());
        }
    }
}
