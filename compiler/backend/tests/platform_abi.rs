//! Exercise the C boundary on each supported native runner, including stack
//! arguments beyond the register budget and signed narrow return values.
use std::{path::PathBuf, process::Command};

#[test]
fn native_scalar_c_abi_covers_registers_stack_and_extensions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = std::env::temp_dir().join(format!("tarn-platform-abi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.tarn");
    std::fs::write(&source, r#"
extern "C" fn integers(a i8, b u8, c i16, d u16, e i32, f u32, g i64, h u64, i isize, j usize) i64
extern "C" fn floats(a f64, b f64, c f64, d f64, e f64, f f64, g f64, h f64, i f64, j f64) f64
extern "C" fn narrow() i8
extern "C" fn truth(x bool) bool
fn main() {
    unsafe {
        if integers(-128, 255, -32768, 65535, -100000, 200000, -9000000000, 10000000000, -11, 12) != 1000132895 { panic("integer ABI") }
        if floats(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0) != 55.0 { panic("float ABI") }
        if narrow() != -128 { panic("narrow return ABI") }
        if !truth(false) || truth(true) { panic("bool ABI") }
    }
    print("native ABI passed")
}
"#).unwrap();
    std::fs::write(dir.join("helper.c"), r#"
#include <stdint.h>
#include <stdbool.h>
int64_t integers(int8_t a, uint8_t b, int16_t c, uint16_t d, int32_t e, uint32_t f,
                 int64_t g, uint64_t h, intptr_t i, uintptr_t j) {
    return (int64_t)a + b + c + d + e + f + g + (int64_t)h + i + (int64_t)j;
}
double floats(double a, double b, double c, double d, double e, double f,
              double g, double h, double i, double j) { return a+b+c+d+e+f+g+h+i+j; }
int8_t narrow(void) { return -128; }
bool truth(bool x) { return !x; }
"#).unwrap();
    let checked = tarn_driver::check(&source).unwrap();
    assert!(!checked.has_errors(), "{:?}", checked.diagnostics);
    let object = tarn_backend::emit_object(checked.drops.as_ref().unwrap(), checked.typed.as_ref().unwrap()).unwrap();
    std::fs::write(dir.join("main.o"), object).unwrap();
    let executable = dir.join("program");
    let linked = Command::new("cc").args(["-std=c11", "-O2", "-fno-strict-aliasing", if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" }, "-pthread"])
        .arg(dir.join("main.o")).arg(dir.join("helper.c")).arg(root.join("runtime/native.c"))
        .arg("-lm").arg("-o").arg(&executable).output().unwrap();
    assert!(linked.status.success(), "{}", String::from_utf8_lossy(&linked.stderr));
    let result = Command::new(&executable).output().unwrap();
    assert!(result.status.success(), "{:?}", result);
    assert_eq!(result.stdout, b"native ABI passed\n");
    std::fs::remove_dir_all(dir).unwrap();
}
