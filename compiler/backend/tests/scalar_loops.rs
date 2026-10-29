//! Peeling must retain the unbounded first iteration, zero-trip behavior,
//! accumulator faults and the existing executable destruction contract.
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tarn_ir::{post_drop::Op, BinOp, Rvalue, StatementKind};

fn source(factor: u64, offset: u64, modulus: u64) -> String {
    format!(
        r#"
import "os"
import "string"
fn integer(text &string) u64 {{
    match string.parse_u64(text) {{
        Some(value) => {{ return value }}
        None => {{ panic("invalid integer") }}
    }}
}}
fn kernel(n u64, seed u64, initial u64) u64 {{
    var state = seed
    var checksum = initial
    var i: u64 = 0
    for i < n {{
        state = (state * {factor} + {offset}) % {modulus}
        checksum = checksum + state % 1000
        i = i + 1
    }}
    return checksum
}}
fn main() {{
    var args: Vec<string> = Vec.new()
    match os.args() {{
        Ok(values) => {{ args = values }}
        Err(_) => {{ panic("cannot read arguments") }}
    }}
    print(kernel(integer(args.get(1)), integer(args.get(2)), integer(args.get(3))))
}}
"#
    )
}
fn oracle(
    n: u64,
    mut state: u64,
    mut checksum: u64,
    factor: u64,
    offset: u64,
    modulus: u64,
) -> Option<u64> {
    for _ in 0..n {
        state = state.checked_mul(factor)?.checked_add(offset)? % modulus;
        checksum = checksum.checked_add(state % 1000)?;
    }
    Some(checksum)
}

#[test]
fn scalar_loop_prefix_and_carried_proofs_match_checked_execution() {
    let dir = std::env::temp_dir().join(format!("tarn-scalar-loops-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (factor, offset, modulus) in [
        (1664525, 1013904223, 4294967296),
        (1103515245, 12345, 65536),
        (17, 23, 1009),
    ] {
        let file = dir.join("main.tarn");
        std::fs::write(&file, source(factor, offset, modulus)).unwrap();
        let result = tarn_driver::check(&file).unwrap();
        assert!(
            !result.has_errors(),
            "{}",
            result
                .diagnostics
                .iter()
                .map(|d| d.render(&result.program.sources))
                .collect::<String>()
        );
        let mut executable = tarn_backend::mono::specialize(
            result.drops.as_ref().unwrap(),
            result.typed.as_ref().unwrap(),
        )
        .unwrap();
        let index = executable
            .functions
            .iter()
            .position(|f| f.decl.name.starts_with("kernel"))
            .unwrap();
        let original_blocks = executable.functions[index].blocks.len();
        tarn_ir::optimize_scalar_loops(&mut executable);
        assert!(tarn_ir::post_drop::verify(&executable, result.typed.as_ref().unwrap()).is_empty());
        let function = &executable.functions[index];
        assert_eq!(function.blocks.len(), original_blocks + 2);
        let operations: Vec<_> = function
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter_map(|s| match &s.op {
                Op::Plain(StatementKind::Assign(_, Rvalue::Binary(op, _, _))) => Some(*op),
                _ => None,
            })
            .collect();
        assert!(
            operations.contains(&BinOp::Mul),
            "initial seed must remain checked"
        );
        assert!(
            operations.contains(&BinOp::MulProven),
            "the carried range must be proven"
        );
        assert!(
            operations.contains(&BinOp::Add),
            "the accumulator can still overflow"
        );
        let binary = dir.join("program");
        tarn_backend::build(
            result.drops.as_ref().unwrap(),
            result.typed.as_ref().unwrap(),
            &binary,
        )
        .unwrap();
        let limit = u64::MAX / factor;
        for n in [0u64, 1, 2, 1000] {
            for seed in [
                0,
                1,
                u64::from(u32::MAX),
                limit - 1,
                limit,
                limit + 1,
                u64::MAX,
            ] {
                for initial in [0, u64::MAX - 500, u64::MAX] {
                    let expected = oracle(n, seed, initial, factor, offset, modulus);
                    let output = Command::new(&binary)
                        .args([n.to_string(), seed.to_string(), initial.to_string()])
                        .output()
                        .unwrap();
                    if let Some(value) = expected {
                        assert!(
                            output.status.success(),
                            "n={n} seed={seed} initial={initial}: {:?}",
                            output
                        );
                        assert_eq!(output.stdout, format!("{value}\n").as_bytes());
                        assert!(output.stderr.is_empty());
                    } else {
                        assert_eq!(
                            output.status.signal(),
                            Some(6),
                            "n={n} seed={seed} initial={initial}: {:?}",
                            output
                        );
                        assert!(output.stdout.is_empty());
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn addressed_scalar_loops_keep_the_original_cfg() {
    let dir = std::env::temp_dir().join(format!("tarn-addressed-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    let source = source(17, 23, 1009).replace(
        "    return checksum",
        "    reference := &checksum\n    return *reference",
    );
    std::fs::write(&file, source).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(!result.has_errors());
    let mut executable = tarn_backend::mono::specialize(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
    )
    .unwrap();
    let before: Vec<_> = executable
        .functions
        .iter()
        .map(|f| f.blocks.len())
        .collect();
    tarn_ir::optimize_scalar_loops(&mut executable);
    assert_eq!(
        before,
        executable
            .functions
            .iter()
            .map(|f| f.blocks.len())
            .collect::<Vec<_>>()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scalar_loop_growth_is_bounded_per_function() {
    let dir = std::env::temp_dir().join(format!("tarn-loop-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    let mut source = String::from(
        "fn many(n u64, seed u64) u64 {\n var state = seed\n var sum: u64 = 0\n var i: u64 = 0\n",
    );
    for _ in 0..64 {
        source.push_str("i = 0\n for i < n {\n state = (state * 17 + 23) % 1009\n sum = sum + state\n i = i + 1\n }\n");
    }
    source.push_str("return sum\n}\nfn main() { print(many(0, 1)) }\n");
    std::fs::write(&file, source).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(!result.has_errors());
    let mut executable = tarn_backend::mono::specialize(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
    )
    .unwrap();
    let index = executable
        .functions
        .iter()
        .position(|f| f.decl.name.starts_with("many"))
        .unwrap();
    let before = executable.functions[index].blocks.len();
    tarn_ir::optimize_scalar_loops(&mut executable);
    let added = executable.functions[index].blocks.len() - before;
    assert!(added > 0 && added <= 32, "unexpected growth: {added}");
    assert!(tarn_ir::post_drop::verify(&executable, result.typed.as_ref().unwrap()).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn wide_carried_ranges_do_not_erase_real_overflow() {
    let dir = std::env::temp_dir().join(format!("tarn-wide-loop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, source(2, 0, u64::MAX)).unwrap();
    let result = tarn_driver::check(&file).unwrap();
    assert!(!result.has_errors());
    let mut executable = tarn_backend::mono::specialize(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
    )
    .unwrap();
    tarn_ir::optimize_scalar_loops(&mut executable);
    let kernel = executable
        .functions
        .iter()
        .find(|f| f.decl.name.starts_with("kernel"))
        .unwrap();
    let multiplications: Vec<_> = kernel
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter_map(|s| match &s.op {
            Op::Plain(StatementKind::Assign(
                _,
                Rvalue::Binary(op @ (BinOp::Mul | BinOp::MulProven), _, _),
            )) => Some(*op),
            _ => None,
        })
        .collect();
    assert!(multiplications.len() >= 2);
    assert!(multiplications.iter().all(|op| *op == BinOp::Mul));
    let binary = dir.join("program");
    tarn_backend::build(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
        &binary,
    )
    .unwrap();
    for n in [0u64, 1, 2, 63, 64, 65] {
        for seed in [0, 1, 2, u64::MAX / 2, u64::MAX] {
            let output = Command::new(&binary)
                .args([n.to_string(), seed.to_string(), "0".to_string()])
                .output()
                .unwrap();
            if let Some(expected) = oracle(n, seed, 0, 2, 0, u64::MAX) {
                assert!(output.status.success(), "n={n} seed={seed}: {:?}", output);
                assert_eq!(output.stdout, format!("{expected}\n").as_bytes());
            } else {
                assert_eq!(
                    output.status.signal(),
                    Some(6),
                    "n={n} seed={seed}: {:?}",
                    output
                );
                assert!(output.stdout.is_empty());
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
