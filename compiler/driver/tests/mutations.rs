//! Robustness: delete each line of every example and suite program, run the
//! whole frontend, and require no panic and structurally valid IR. Exercises resolver and type checker
//! on many odd-but-parseable programs that no hand-written test covers.

use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    let root = Path::new("../..");
    let mut out = Vec::new();
    for dir in ["examples", "tests/types/pass", "tests/types/fail", "tests/resolve/pass", "tests/moves/pass", "tests/moves/fail", "tests/borrows/pass", "tests/borrows/fail", "tests/drops/pass"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|e| e == "tarn") {
                out.push(p);
            }
        }
    }
    out.push(root.join("tests/native/pass/tasks.tarn"));
    out.push(root.join("tests/native/pass/task_completion.tarn"));
    out.push(root.join("tests/native/pass/task_capabilities.tarn"));
    out.push(root.join("tests/native/pass/scoped_tasks.tarn"));
    out.push(root.join("tests/native/pass/scoped_task_completion.tarn"));
    out.push(root.join("tests/native/pass/synchronization.tarn"));
    out.push(root.join("tests/native/pass/atomic_types.tarn"));
    out.push(root.join("tests/native/pass/synchronization_owned.tarn"));
    for fixture in ["network_tcp", "network_udp", "network_errors", "network_readiness", "suspended_io", "executor_turns", "async_block_on", "async_control_flow", "async_recursion", "string_essentials", "filesystem", "paths", "processes", "http_owned"] { out.push(root.join(format!("tests/native/pass/{fixture}.tarn"))); }
    out.sort();
    out
}

/// One mutant: every line of `src` except line `skip`.
fn mutant(src: &str, skip: usize) -> String {
    let lines: Vec<&str> = src.lines().collect();
    [&lines[..skip], &lines[skip + 1..]].concat().join("\n")
}

/// Mutants are independent, so they are checked on at most seven cores; each worker
/// writes its own file. Assertions are the same as a sequential loop.
#[test]
fn line_deletions_never_panic() {
    let dir = std::env::temp_dir().join(format!("tarn-mutations-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sources: Vec<(PathBuf, String)> = corpus().into_iter().map(|p| { let s = std::fs::read_to_string(&p).unwrap(); (p, s) }).collect();
    let jobs: Vec<(usize, usize)> = sources.iter().enumerate().flat_map(|(f, (_, s))| (0..s.lines().count()).map(move |i| (f, i))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(7));
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let (dir, sources, jobs, next) = (&dir, &sources, &jobs, &next);
            scope.spawn(move || {
                let file = dir.join(format!("m{worker}.tarn"));
                while let Some(&(f, i)) = jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    let (path, src) = &sources[f];
                    let mutated = mutant(src, i);
                    std::fs::write(&file, &mutated).unwrap();
                    let result = std::panic::catch_unwind(|| tarn_driver::check(&file));
                    assert!(result.is_ok(), "panic on {} without line {}:\n{mutated}", path.display(), i + 1);
                    assert!(result.as_ref().unwrap().is_ok(), "compiler failure on {} without line {}: {:?}", path.display(), i + 1, result.as_ref().unwrap().as_ref().err());
                    if let Ok(Ok(res)) = &result
                        && let Some(ir) = &res.ir
                    {
                        let errs = tarn_ir::verify(ir);
                        assert!(errs.is_empty(), "invalid IR for {} without line {}:\n{}", path.display(), i + 1, errs.join("\n"));
                    }
                    if let Ok(Ok(res)) = &result
                        && let (Some(drops), Some(typed)) = (&res.drops, &res.typed)
                    {
                        let errs = tarn_ir::post_drop::verify(drops, typed);
                        assert!(errs.is_empty(), "invalid post-drop IR for {} without line {}:\n{}", path.display(), i + 1, errs.join("\n"));
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(jobs.len() > 1000, "{}", jobs.len());
}
