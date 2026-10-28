//! Width reduction must preserve all high bits, unsigned boundaries and faults.
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

fn compile(source: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tarn-narrow-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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
    binary
}

const INPUT: &str = r#"
import "os"
import "string"
fn value(s &string) u64 {
    match string.parse_u64(s) {
        Some(n) => { return n }
        None => { panic("invalid argument") }
    }
}
fn main() {
    var args: Vec<string> = Vec.new()
    match os.args() {
        Ok(arguments) => { args = arguments }
        Err(_) => { panic("cannot read arguments") }
    }
    x := value(args.get(1))
    y := value(args.get(2))
"#;

#[test]
fn native_narrow_and_wide_division_preserve_integer_boundaries() {
    let binary = compile(
        &format!(
            r#"{INPUT}
    small := x % 4294967296
    masked := x & 4294967295
    extended := u64(u32(small))
    bounded_divisor := y % 4294967296
    print(small / 1000)
    print(small % 1000)
    print(masked / 3)
    print(masked % 3)
    print(extended / 7)
    print(extended % 7)
    print(small / y)
    print(small % y)
    print(x / y)
    print(x % y)
    print(small / bounded_divisor)
    print(small % bounded_divisor)
}}
"#
        ),
        "boundaries",
    );
    let inputs = [
        0u64,
        1,
        999,
        1000,
        u64::from(u32::MAX),
        1u64 << 32,
        (1u64 << 32) + 1,
        u64::MAX,
    ];
    let divisors = [
        1u64,
        3,
        1000,
        u64::from(u32::MAX),
        (1u64 << 32) + 1,
        u64::MAX,
    ];
    for x in inputs {
        for y in divisors {
            let small = x % (1u64 << 32);
            let bounded = y % (1u64 << 32);
            let expected = [
                small / 1000,
                small % 1000,
                small / 3,
                small % 3,
                small / 7,
                small % 7,
                small / y,
                small % y,
                x / y,
                x % y,
                small / bounded,
                small % bounded,
            ];
            let output = Command::new(&binary)
                .args([x.to_string(), y.to_string()])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "x={x} y={y}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = expected
                .iter()
                .map(|v| format!("{v}\n"))
                .collect::<String>();
            assert_eq!(output.stdout, expected.as_bytes(), "x={x} y={y}");
            assert!(output.stderr.is_empty());
        }
    }
    std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
}

#[test]
fn narrowed_division_and_remainder_still_abort_on_zero() {
    for operator in ["/", "%"] {
        let binary = compile(
            &format!(
                r#"{INPUT}
    print((x % 4294967296) {operator} (y % 4294967296))
}}
"#
            ),
            if operator == "/" {
                "zero-div"
            } else {
                "zero-rem"
            },
        );
        for divisor in [0u64, 1u64 << 32] {
            let output = Command::new(&binary)
                .args([u64::MAX.to_string(), divisor.to_string()])
                .output()
                .unwrap();
            assert_eq!(
                output.status.signal(),
                Some(6),
                "{operator} divisor={divisor}: {:?}",
                output
            );
            assert!(output.stdout.is_empty());
        }
        std::fs::remove_dir_all(binary.parent().unwrap()).unwrap();
    }
}
