//! Constant arithmetic guards must agree with checked word arithmetic at every
//! transition, including constants represented by negative CLIF immediates.
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

#[test]
fn unsigned_constant_guards_preserve_values_and_exact_overflow_boundaries() {
    let dir = std::env::temp_dir().join(format!("tarn-checked-constants-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let constants = [
        0u64,
        1,
        2,
        3,
        7,
        1664525,
        1013904223,
        1u64 << 32,
        1u64 << 63,
        u64::MAX,
    ];
    for operator in ["+", "*"] {
        for constant in constants {
            let source = format!(
                r#"
import "os"
import "string"
fn integer(text &string) u64 {{
    match string.parse_u64(text) {{
        Some(n) => {{ return n }}
        None => {{ panic("invalid argument") }}
    }}
}}
fn main() {{
    var args: Vec<string> = Vec.new()
    match os.args() {{
        Ok(arguments) => {{ args = arguments }}
        Err(_) => {{ panic("cannot read arguments") }}
    }}
    x := integer(args.get(1))
    side := integer(args.get(2))
    if side == 0 {{ print(x {operator} {constant}) }} else {{ print({constant} {operator} x) }}
}}
"#
            );
            let file = dir.join("main.tarn");
            std::fs::write(&file, source).unwrap();
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
            let binary = dir.join("program");
            tarn_backend::build(
                result.drops.as_ref().unwrap(),
                result.typed.as_ref().unwrap(),
                &binary,
            )
            .unwrap();
            let limit = if operator == "+" {
                u64::MAX - constant
            } else if constant == 0 {
                u64::MAX
            } else {
                u64::MAX / constant
            };
            let mut inputs = vec![0, 1, u64::MAX, limit];
            inputs.extend(limit.checked_sub(1));
            inputs.extend(limit.checked_add(1));
            inputs.sort_unstable();
            inputs.dedup();
            for x in inputs {
                let expected = if operator == "+" {
                    x.checked_add(constant)
                } else {
                    x.checked_mul(constant)
                };
                for side in [0, 1] {
                    let output = Command::new(&binary)
                        .args([x.to_string(), side.to_string()])
                        .output()
                        .unwrap();
                    if let Some(value) = expected {
                        assert!(
                            output.status.success(),
                            "{x} {operator} {constant} side={side}: {:?}",
                            output
                        );
                        assert_eq!(output.stdout, format!("{value}\n").as_bytes());
                        assert!(output.stderr.is_empty());
                    } else {
                        assert_eq!(
                            output.status.signal(),
                            Some(6),
                            "{x} {operator} {constant} side={side}: {:?}",
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
