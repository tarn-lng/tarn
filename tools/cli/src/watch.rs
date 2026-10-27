//! Dependency-free source polling. Compiler errors leave the watch session alive.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTERVAL: Duration = Duration::from_millis(250);
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[derive(Default)]
struct Sources {
    observed: BTreeMap<PathBuf, Stamp>,
    pending: BTreeSet<PathBuf>,
}

impl Sources {
    fn replace(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.observed = paths
            .into_iter()
            .map(|path| {
                let value = stamp(&path);
                (path, value)
            })
            .collect();
        self.pending.clear();
    }

    /// Emit one batch only after all watched files stay stable for an interval.
    fn poll(&mut self) -> Option<BTreeSet<PathBuf>> {
        let mut changed = false;
        for (path, previous) in &mut self.observed {
            let current = stamp(path);
            if current != *previous {
                *previous = current;
                self.pending.insert(path.clone());
                changed = true;
            }
        }
        if !changed && !self.pending.is_empty() {
            Some(std::mem::take(&mut self.pending))
        } else {
            None
        }
    }
}

pub fn check(entry: &Path, json: bool) -> ExitCode {
    super::process::install_handlers();
    let mut sources = Sources::default();
    sources.replace([entry.to_path_buf()]);
    let mut changed = BTreeSet::from([entry.to_path_buf()]);
    loop {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        eprintln!(
            "[watch {seconds}] checking after changes: {}",
            changed
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        // Keep the pre-check stamps: edits during compilation must cause a new check.
        match crate::check_project(entry) {
            Ok(result) => {
                let mut paths: BTreeSet<_> = result.program.disk_sources.iter().cloned().collect();
                paths.insert(entry.to_path_buf());
                if result.has_errors() {
                    // Retain deleted imports until recovery; the loader cannot read them.
                    paths.extend(sources.observed.keys().cloned());
                }
                sources.observed.retain(|path, _| paths.contains(path));
                for path in paths {
                    sources
                        .observed
                        .entry(path.clone())
                        .or_insert_with(|| stamp(&path));
                }
                for diagnostic in &result.diagnostics {
                    if json {
                        println!("{}", diagnostic.to_json(&result.program.sources));
                    } else {
                        eprint!("{}", diagnostic.render(&result.program.sources));
                    }
                }
                eprintln!(
                    "[watch] check {}",
                    if result.has_errors() {
                        "failed"
                    } else {
                        "succeeded"
                    }
                );
            }
            Err(error) => {
                if json {
                    super::native_error(true, "load", &error);
                } else {
                    eprintln!("error[E9001]: {error}");
                }
                eprintln!("[watch] check failed");
            }
        }
        let _ = std::io::stdout().flush();
        loop {
            std::thread::sleep(INTERVAL);
            if super::process::stopping() {
                return ExitCode::SUCCESS;
            }
            if let Some(paths) = sources.poll() {
                changed = paths;
                break;
            }
        }
    }
}

fn header(changed: &BTreeSet<PathBuf>, action: &str) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    eprintln!(
        "[watch {seconds}] {action} after changes: {}",
        changed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
fn refresh(sources: &mut Sources, paths: Vec<PathBuf>, entry: &Path, failed: bool) {
    let mut paths: BTreeSet<_> = paths.into_iter().collect();
    paths.insert(entry.to_path_buf());
    if failed {
        paths.extend(sources.observed.keys().cloned());
    }
    sources.observed.retain(|p, _| paths.contains(p));
    for path in paths {
        sources
            .observed
            .entry(path.clone())
            .or_insert_with(|| stamp(&path));
    }
}
fn wait(
    sources: &mut Sources,
    entry: &Path,
    tests: bool,
    siblings: &mut Vec<PathBuf>,
    child: &mut Option<std::process::Child>,
) -> Option<BTreeSet<PathBuf>> {
    loop {
        if super::process::stopping() {
            return None;
        }
        std::thread::sleep(INTERVAL);
        if let Some(process) = child.as_mut() {
            if let Ok(Some(status)) = process.try_wait() {
                super::process::send(process, 9);
                eprintln!("[watch] program exited: {status}");
                child.take();
            }
        }
        if tests {
            if let Ok(current) = tarn_driver::testing::test_files(entry) {
                if current != *siblings {
                    sources
                        .pending
                        .extend(current.iter().chain(siblings.iter()).cloned());
                    *siblings = current;
                    // A discovery change also needs a stable polling interval.
                    continue;
                }
            }
        }
        if let Some(changed) = sources.poll() {
            return Some(changed);
        }
    }
}

fn build_failed(has_child: bool) {
    eprintln!(
        "[watch] build failed; {}",
        if has_child {
            "previous program retained"
        } else {
            "waiting for source changes"
        }
    );
}

pub fn run(args: &[String]) -> ExitCode {
    super::process::install_handlers();
    let entry = Path::new(&args[0]);
    let json = crate::tool_args(args).iter().any(|a| a == "--json");
    let mut libraries = Vec::new();
    let mut program = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        if arg == "--link" {
            libraries.push(rest.next().unwrap().clone());
        } else if arg == "--" {
            program.extend(rest.by_ref().cloned());
        }
    }
    let scratch = match super::process::Scratch::new() {
        Ok(scratch) => scratch,
        Err(error) => {
            super::native_error(json, "output", &error.to_string());
            return ExitCode::from(1);
        }
    };
    let mut child: Option<std::process::Child> = None;
    let mut generation = 0;
    let mut sources = Sources::default();
    sources.replace([entry.to_path_buf()]);
    let mut changed = BTreeSet::from([entry.to_path_buf()]);
    loop {
        header(&changed, "building");
        match crate::check_project(entry) {
            Ok(result) => {
                refresh(
                    &mut sources,
                    result.program.disk_sources.clone(),
                    entry,
                    result.has_errors(),
                );
                for diagnostic in &result.diagnostics {
                    if json {
                        println!("{}", diagnostic.to_json(&result.program.sources));
                    } else {
                        eprint!("{}", diagnostic.render(&result.program.sources));
                    }
                }
                let missing_main = result.missing_main();
                if let Some(diagnostic) = &missing_main {
                    if json {
                        println!("{}", diagnostic.to_json(&result.program.sources));
                    } else {
                        eprint!("{}", diagnostic.render(&result.program.sources));
                    }
                }
                if !result.has_errors() && missing_main.is_none() {
                    generation += 1;
                    let output = scratch.0.join(format!("run-{generation}"));
                    match tarn_backend::build_linked(
                        result.drops.as_ref().unwrap(),
                        result.typed.as_ref().unwrap(),
                        &output,
                        &libraries,
                    ) {
                        Ok(()) => {
                            let _ = std::io::stdout().flush();
                            if let Some(mut previous) = child.take() {
                                super::process::terminate(&mut previous, Duration::from_secs(2));
                            }
                            if !super::process::stopping() {
                                match super::process::command(&output).args(&program).spawn() {
                                    Ok(process) => {
                                        child = Some(process);
                                        eprintln!("[watch] program started");
                                    }
                                    Err(error) => {
                                        super::native_error(json, "execute", &error.to_string())
                                    }
                                }
                            }
                            // Linux keeps a running executable mapped after unlink.
                            let _ = std::fs::remove_file(&output);
                        }
                        Err(error) => {
                            super::native_error(json, "native", &error.to_string());
                            build_failed(child.is_some());
                        }
                    }
                } else {
                    build_failed(child.is_some());
                }
            }
            Err(error) => {
                super::native_error(json, "load", &error);
                build_failed(child.is_some());
            }
        }
        let _ = std::io::stdout().flush();
        let Some(paths) = wait(&mut sources, entry, false, &mut Vec::new(), &mut child) else {
            break;
        };
        changed = paths;
    }
    if let Some(mut child) = child {
        super::process::terminate(&mut child, Duration::from_secs(2));
    }
    ExitCode::SUCCESS
}

pub fn test(entry: &Path, options: &super::testing::Options) -> ExitCode {
    super::process::install_handlers();
    let mut sources = Sources::default();
    sources.replace([entry.to_path_buf()]);
    let mut siblings = tarn_driver::testing::test_files(entry).unwrap_or_default();
    let mut changed = BTreeSet::from([entry.to_path_buf()]);
    loop {
        header(&changed, "testing");
        super::testing::run_with_sources(entry, options, |paths, failed| {
            refresh(&mut sources, paths.to_vec(), entry, failed);
        });
        let _ = std::io::stdout().flush();
        let Some(paths) = wait(&mut sources, entry, true, &mut siblings, &mut None) else {
            return ExitCode::SUCCESS;
        };
        changed = paths;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_edits_and_observes_deletion_and_recreation() {
        let dir = std::env::temp_dir().join(format!("tarn-watch-poll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("main.tarn");
        let second = dir.join("helper.tarn");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();
        let mut sources = Sources::default();
        sources.replace([first.clone(), second.clone()]);
        assert!(sources.poll().is_none());
        std::fs::write(&first, "first edit").unwrap();
        assert!(sources.poll().is_none());
        std::fs::write(&second, "second edit").unwrap();
        assert!(sources.poll().is_none());
        assert_eq!(
            sources.poll(),
            Some(BTreeSet::from([first.clone(), second.clone()]))
        );
        assert!(sources.poll().is_none());
        std::fs::remove_file(&second).unwrap();
        assert!(sources.poll().is_none());
        assert_eq!(sources.poll(), Some(BTreeSet::from([second.clone()])));
        std::fs::write(&second, "restored").unwrap();
        assert!(sources.poll().is_none());
        assert_eq!(sources.poll(), Some(BTreeSet::from([second])));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
