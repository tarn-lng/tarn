//! The freestanding contract (ADR 0101, P2). A `*-none` object may import
//! only what bare-metal code itself provides; everything else in the hosted
//! runtime is a compile error that names the Tarn feature responsible.

use crate::{Error, Result, Target};

/// Runtime symbols the freestanding program's boot code must define. Panics,
/// bounds and overflow checks stay on: these decide what a failure does.
pub const CONTRACT: [&str; 3] = ["tarn_rt_fault", "tarn_rt_panic", "tarn_rt_panic_at"];

/// The Tarn feature behind a hosted runtime symbol, for the diagnostic.
fn feature(symbol: &str) -> &'static str {
    let name = symbol.strip_prefix("tarn_rt_").unwrap_or(symbol);
    const FEATURES: &[(&str, &str)] = &[
        ("print_", "console output (`print`)"),
        ("string_", "heap-allocated strings"),
        ("drop_string", "heap-allocated strings"),
        ("vec_", "heap-allocated vectors"),
        ("env_", "closures that capture their environment (heap allocation)"),
        ("async_", "async tasks"),
        ("atomic_", "atomics from the hosted runtime"),
        ("net_", "networking"),
        ("process_", "processes"),
        ("test_", "the test harness"),
    ];
    FEATURES.iter().find(|(prefix, _)| name.starts_with(prefix)).map(|(_, f)| *f).unwrap_or("operating-system services from the hosted runtime")
}

/// Reject any imported runtime symbol outside the contract, grouped by the
/// feature that needs it.
pub fn check(target: Target, used: Vec<String>) -> Result<()> {
    let mut groups: Vec<(&'static str, Vec<String>)> = Vec::new();
    for symbol in used.into_iter().filter(|s| !CONTRACT.contains(&s.as_str())) {
        let f = feature(&symbol);
        match groups.iter_mut().find(|(g, _)| *g == f) {
            Some((_, symbols)) => symbols.push(symbol),
            None => groups.push((f, vec![symbol])),
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = groups.iter().map(|(f, symbols)| format!("  {f}: needs {}", symbols.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", "))).collect();
    Err(Error {
        message: format!(
            "target {target} is freestanding: no operating system and no hosted runtime, but this program uses\n{}\nfreestanding code may only rely on {} (defined by your boot code) and extern \"C\" functions",
            lines.join("\n"),
            CONTRACT.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_symbols_are_allowed() {
        let target: Target = "x86_64-none".parse().unwrap();
        assert!(check(target, CONTRACT.iter().map(|s| s.to_string()).collect()).is_ok());
    }

    #[test]
    fn hosted_symbols_are_grouped_by_feature() {
        let target: Target = "aarch64-none".parse().unwrap();
        let used = ["tarn_rt_print_i64", "tarn_rt_print_string", "tarn_rt_vec_grow", "tarn_rt_panic"].map(String::from).to_vec();
        let message = check(target, used).unwrap_err().message;
        assert!(message.contains("target aarch64-none is freestanding"), "{message}");
        assert!(message.contains("console output (`print`): needs `tarn_rt_print_i64`, `tarn_rt_print_string`"), "{message}");
        assert!(message.contains("heap-allocated vectors: needs `tarn_rt_vec_grow`"), "{message}");
        assert!(!message.contains("needs `tarn_rt_panic`"), "{message}");
    }
}
