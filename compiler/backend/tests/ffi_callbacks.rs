//! Real C callers exercise both directions of the ABI, not Tarn-only closures.
use std::{path::PathBuf, process::Command};

fn fixture(tag: &str, source: &str) -> (PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-c-callback-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.tarn");
    std::fs::write(&path, source).unwrap();
    (dir, tarn_driver::check(&path).unwrap())
}
fn checked(result: &tarn_driver::CheckResult) {
    assert!(
        !result.has_errors(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.render(&result.program.sources))
            .collect::<String>()
    );
}

#[test]
fn c_calls_static_tarn_functions_with_scalar_abi_and_explicit_context() {
    let (dir, result) = fixture(
        "native",
        r#"
import "ffi"
extern "C" fn remember(callback *u8, context *mut i32)
extern "C" fn fire() i32
extern "C" fn forget()
extern "C" fn verify_abi(integers *u8, floats *u8, bool_handler *u8, pointer_handler *u8, narrow_handler *u8, single_handler *u8, zero_handler *u8) i32
fn record(context *mut i32) {
    unsafe {
        old := ffi.read(ffi.to_const(context))
        ffi.write(context, old + 1)
    }
}
fn numbers(a i8, b u8, c i16, d u16, e i32, f u32, g i64, h u64, i isize, j usize) i64 {
    return i64(a) + i64(b) + i64(c) + i64(d) + i64(e) + i64(f) + g + i64(h) + i64(i) + i64(j)
}
fn decimals(a f64, b f64, c f64, d f64, e f64, f f64, g f64, h f64, i f64, j f64) f64 {
    return a + b + c + d + e + f + g + h + i + j
}
fn truth(value bool) bool { return !value }
fn pointer(value *mut i32) *mut i32 { return value }
fn narrow() i8 { return -128 }
fn single(value f32) f32 { return value + 0.5 }
fn zero() i32 { return 42 }
fn main() {
    var count: i32 = 0
    handler := ffi.callback(record)
    if ffi.address(handler) != ffi.address(ffi.callback((record))) { panic("adapter identity") }
    unsafe {
        remember(handler, ffi.of_mut(&mut count))
        if fire() != 1 || fire() != 2 { panic("stored callback") }
        forget()
        if verify_abi(ffi.callback(numbers), ffi.callback(decimals), ffi.callback(truth), ffi.callback(pointer), ffi.callback(narrow), ffi.callback(single), ffi.callback(zero)) != 1 { panic("callback ABI") }
    }
    if count != 2 { panic("context update") }
    // Pointer-valued memory uses a machine address, never a reference.
    var address: *mut i32 = ffi.null()
    unsafe {
        ffi.write(ffi.of_mut(&mut address), ffi.of_mut(&mut count))
        copied := ffi.read(ffi.of(&address))
        ffi.write(copied, 9)
    }
    if count != 9 { panic("pointer memory") }
    print("C callbacks passed")
}
"#,
    );
    checked(&result);
    std::fs::write(dir.join("caller.c"), r#"
#include <stdint.h>
#include <stdbool.h>
static void (*saved)(int32_t *);
static int32_t *context;
void remember(void (*callback)(int32_t *), int32_t *data) { saved = callback; context = data; }
int32_t fire(void) { saved(context); return *context; }
void forget(void) { saved = 0; context = 0; }
int32_t verify_abi(
    int64_t (*integers)(int8_t,uint8_t,int16_t,uint16_t,int32_t,uint32_t,int64_t,uint64_t,intptr_t,uintptr_t),
    double (*floats)(double,double,double,double,double,double,double,double,double,double),
    bool (*truth)(bool), int32_t *(*pointer)(int32_t *), int8_t (*narrow)(void), float (*single)(float), int32_t (*zero)(void)) {
    int32_t value = 7;
    return integers(-128,255,-32768,65535,-100000,200000,-9000000000LL,10000000000ULL,-11,12) == 1000132895LL
        && floats(1,2,3,4,5,6,7,8,9,10) == 55 && truth(false) && !truth(true)
        && pointer(&value) == &value && narrow() == -128 && single(1.25f) == 1.75f && zero() == 42;
}
"#).unwrap();
    let bytes = tarn_backend::emit_object(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("main.o"), bytes).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let executable = dir.join("program");
    let linked = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O2",
            "-fno-strict-aliasing",
            "-no-pie",
            "-pthread",
        ])
        .arg(dir.join("main.o"))
        .arg(dir.join("caller.c"))
        .arg(root.join("runtime/native.c"))
        .arg("-lm")
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let output = Command::new("timeout")
        .arg("10s")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"C callbacks passed\n");
    assert!(output.stderr.is_empty(), "{output:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn callback_construction_rejects_environments_and_nonordinary_functions() {
    for (tag, declarations, expression) in [
        ("closure", "", "fn(x i32) i32 { return x }"),
        ("capture", "", "fn(x i32) i32 { return x + seed }"),
        ("local", "", "local"),
        ("generic", "fn generic<T>(x T) T { return x }", "generic"),
        ("async", "async fn later(x i32) i32 { return x }", "later"),
        (
            "unsafe",
            "unsafe fn danger(x i32) i32 { return x }",
            "danger",
        ),
        ("foreign", "extern \"C\" fn abs(x i32) i32", "abs"),
        ("value", "", "seed"),
    ] {
        let (dir, result) = fixture(
            tag,
            &format!(
                "import \"ffi\"\n{declarations}\nfn main() {{\n seed: i32 := 1\n local := fn(x i32) i32 {{ return x }}\n address := ffi.callback({expression})\n}}\n"
            ),
        );
        assert!(
            result.diagnostics.iter().any(|d| d.code == "E3081"),
            "{tag}: {:?}",
            result.diagnostics
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    let (dir, result) = fixture(
        "constructor-value",
        "import \"ffi\"\nfn main() { constructor := ffi.callback }\n",
    );
    assert!(
        result.diagnostics.iter().any(|d| d.code == "E3081"),
        "{:?}",
        result.diagnostics
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unsupported_callback_and_memory_layouts_fail_build_without_panics() {
    for (tag, source, expected) in [
        (
            "aggregate-callback",
            "copy struct Pair { x i32 }\nfn handler(x Pair) i32 { return x.x }\nfn main() { address := ffi.callback(handler) }",
            "has no C ABI",
        ),
        (
            "reference-callback",
            "fn handler(x &i32) i32 { return *x }\nfn main() { address := ffi.callback(handler) }",
            "has no C ABI",
        ),
        (
            "aggregate-read",
            "copy struct Pair { x i32 }\nfn main() { value := Pair { x: 1 }\n unsafe { copied := ffi.read(ffi.of(&value)) } }",
            "ffi.read supports only",
        ),
        (
            "aggregate-write",
            "copy struct Pair { x i32 }\nfn main() { var value = Pair { x: 1 }\n unsafe { ffi.write(ffi.of_mut(&mut value), Pair { x: 2 }) } }",
            "ffi.write supports only",
        ),
    ] {
        let (dir, result) = fixture(tag, &format!("import \"ffi\"\n{source}\n"));
        checked(&result);
        let error = tarn_backend::emit_object(
            result.drops.as_ref().unwrap(),
            result.typed.as_ref().unwrap(),
        )
        .unwrap_err();
        assert!(error.message.contains(expected), "{tag}: {error}");
        assert!(!error.message.contains("compiler bug"), "{tag}: {error}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn raw_memory_helpers_require_unsafe_and_copy() {
    for (tag, source, expected) in [
        (
            "safe-read",
            "fn main() { x: i32 := 1\n y := ffi.read(ffi.of(&x)) }",
            "E3071",
        ),
        (
            "safe-write",
            "fn main() { var x: i32 = 1\n ffi.write(ffi.of_mut(&mut x), 2) }",
            "E3071",
        ),
        (
            "owned-read",
            "fn main() { x := \"owned\"\n unsafe { y := ffi.read(ffi.of(&x)) } }",
            "E3022",
        ),
    ] {
        let (dir, result) = fixture(tag, &format!("import \"ffi\"\n{source}\n"));
        assert!(
            result.diagnostics.iter().any(|d| d.code == expected),
            "{tag}: {:?}",
            result.diagnostics
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn qualified_callbacks_retain_ordinary_bodies_and_abort_on_panic() {
    let (dir, _) = fixture("qualified", "fn main() {}\n");
    // Real libc consumes the C callback synchronously; no helper C source.
    std::fs::write(
        dir.join("handlers.tarn"),
        r#"
import "ffi"
pub fn compare(a *u8, b *u8) i32 {
    unsafe {
        left := ffi.read(a)
        right := ffi.read(b)
        return i32(left) - i32(right)
    }
}
"#,
    )
    .unwrap();
    let path = dir.join("main.tarn");
    std::fs::write(
        &path,
        r#"
import "ffi"
import "handlers"
extern "C" fn qsort(base *mut u8, count usize, size usize, compare *u8)
fn main() {
    var data = [3]u8{3, 1, 2}
    unsafe { qsort(ffi.slice_mut(&mut data[..]), 3, 1, ffi.callback(handlers.compare)) }
    if data[0] != 1 || data[1] != 2 || data[2] != 3 { panic("sort") }
    print("sorted")
}
"#,
    )
    .unwrap();
    let result = tarn_driver::check(&path).unwrap();
    checked(&result);
    let executable = dir.join("program");
    tarn_backend::build(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
        &executable,
    )
    .unwrap();
    let output = Command::new("timeout")
        .arg("10s")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sorted\n");
    std::fs::write(
        dir.join("handlers.tarn"),
        "pub fn compare(a *u8, b *u8) i32 { panic(\"callback panic\") }\n",
    )
    .unwrap();
    let result = tarn_driver::check(&path).unwrap();
    checked(&result);
    tarn_backend::build(
        result.drops.as_ref().unwrap(),
        result.typed.as_ref().unwrap(),
        &executable,
    )
    .unwrap();
    let output = Command::new("timeout")
        .arg("10s")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output
            .stderr
            .windows(b"callback panic".len())
            .any(|w| w == b"callback panic"),
        "{output:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupted_callback_and_scalar_read_contracts_are_rejected() {
    use tarn_ir::{Callee, Const, Operand, Terminator};
    let source = "import \"ffi\"\nfn handler() i32 { return 1 }\nfn main() { address := ffi.callback(handler) }\n";
    let (dir, mut result) = fixture("corrupt-callback", source);
    checked(&result);
    let p = result.drops.as_mut().unwrap();
    let target = p
        .functions
        .iter()
        .find(|f| f.decl.name == "handler")
        .unwrap()
        .decl
        .id;
    for f in &mut p.functions {
        for b in &mut f.blocks {
            if let Terminator::Call {
                callee: Callee::Intrinsic(name),
                args,
                ..
            } = &mut b.term
                && name == "ffi.callback"
            {
                *args = vec![Operand::Const(Const::Fn(
                    target,
                    vec![tarn_types::Ty::Bool],
                ))];
            }
        }
    }
    let error = tarn_backend::emit_object(p, result.typed.as_ref().unwrap()).unwrap_err();
    assert!(
        error.message.contains("invalid static C callback metadata"),
        "{error}"
    );
    let mut result = tarn_driver::check(&dir.join("main.tarn")).unwrap();
    let t = result.typed.as_mut().unwrap();
    let id = t.decls.ffi_read.unwrap();
    t.decls.fns.get_mut(&id).unwrap().contract.result = tarn_types::ResultContract::Owned;
    let error = tarn_backend::emit_object(result.drops.as_ref().unwrap(), t).unwrap_err();
    assert!(
        error
            .message
            .contains("invalid scalar FFI memory declaration"),
        "{error}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unsafe_requirements_cannot_be_erased_by_callable_values() {
    for (tag, declarations, expression) in [
        (
            "unsafe-alias",
            "unsafe fn danger(x i32) i32 { return x }",
            "danger",
        ),
        ("foreign-alias", "extern \"C\" fn abs(x i32) i32", "abs"),
        ("read-alias", "", "ffi.read"),
        (
            "unsafe-alias-escape",
            "unsafe fn danger(x i32) i32 { return x }",
            "danger",
        ),
    ] {
        let binding = if tag == "read-alias" {
            format!("reader: fn(*i32) i32 := {expression}")
        } else {
            format!("reader := {expression}")
        };
        let body = if tag.ends_with("escape") {
            format!("unsafe {{ {binding} }}")
        } else {
            binding
        };
        let (dir, result) = fixture(
            tag,
            &format!("import \"ffi\"\n{declarations}\nfn main() {{ {body} }}\n"),
        );
        assert!(
            result.diagnostics.iter().any(|d| d.code == "E3082"),
            "{tag}: {:?}",
            result.diagnostics
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
