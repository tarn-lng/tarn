//! Explicit targets (ADR 0101, P1): one host emits objects for every
//! supported target, and each object names that target in its header.
use tarn_backend::{Arch, Os, Target, TARGETS};

/// (format, machine) read straight from the object header.
fn header(bytes: &[u8]) -> (&'static str, u32) {
    if bytes.starts_with(b"\x7fELF") {
        assert_eq!(bytes[4], 2, "ELF64");
        assert_eq!(bytes[5], 1, "little endian");
        assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 1, "ET_REL: relocatable, nothing linked");
        ("elf", u16::from_le_bytes([bytes[18], bytes[19]]) as u32)
    } else if bytes.starts_with(&0xfeedfacf_u32.to_le_bytes()) {
        ("macho", u32::from_le_bytes(bytes[4..8].try_into().unwrap()))
    } else {
        panic!("unknown object format {:02x?}", &bytes[..4]);
    }
}

#[test]
fn every_target_emits_its_own_object() {
    // No hosted runtime, so the same program is valid for `*-none` too.
    let res = tarn_driver::check(std::path::Path::new("../../tests/freestanding/arithmetic.tarn")).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    for name in TARGETS {
        let target: Target = name.parse().unwrap();
        let bytes = tarn_backend::emit_object_for(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), target).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (format, machine) = header(&bytes);
        let expected = match (target.os, target.arch) {
            (Os::Macos, Arch::X86_64) => ("macho", 0x0100_0007),
            (Os::Macos, Arch::Aarch64) => ("macho", 0x0100_000c),
            (_, Arch::X86_64) => ("elf", 62),
            (_, Arch::Aarch64) => ("elf", 183),
        };
        assert_eq!((format, machine), expected, "{name}");
    }
}

#[test]
fn host_target_matches_default_emission_format() {
    let Some(host) = Target::host() else { return };
    let res = tarn_driver::check(std::path::Path::new("../../benchmarks/native/arithmetic.tarn")).unwrap();
    let (p, t) = (res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap());
    let default = tarn_backend::emit_object(p, t).unwrap();
    let explicit = tarn_backend::emit_object_for(p, t, host).unwrap();
    assert_eq!(header(&default), header(&explicit));
}

fn freestanding(path: &str, target: &str) -> tarn_backend::Result<Vec<u8>> {
    let res = tarn_driver::check(std::path::Path::new(path)).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    tarn_backend::emit_object_for(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), target.parse().unwrap())
}

/// (undefined, defined global) symbols, via the system `nm`.
fn symbols(bytes: &[u8], tag: &str) -> (Vec<String>, Vec<String>) {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!("tarn-freestanding-{tag}-{}-{}.o", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::write(&path, bytes).unwrap();
    let list = |flag: &str| -> Vec<String> {
        let out = std::process::Command::new("nm").args([flag]).arg(&path).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_whitespace().last()).map(String::from).collect()
    };
    let result = (list("-u"), list("-g"));
    let _ = std::fs::remove_file(path);
    result
}

#[test]
fn freestanding_object_imports_only_the_contract() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/arithmetic.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
        assert!(globals.contains(&"tarn_main".to_string()), "{target}: {globals:?}");
        assert!(!globals.contains(&"main".to_string()), "{target}: no libc entry shim");
    }
}

#[test]
fn freestanding_rejects_the_hosted_runtime_by_feature() {
    let error = freestanding("../../benchmarks/native/arithmetic.tarn", "x86_64-none").unwrap_err().to_string();
    assert!(error.contains("target x86_64-none is freestanding"), "{error}");
    assert!(error.contains("console output (`print`): needs `tarn_rt_print_i64`"), "{error}");
}

#[test]
fn hosted_objects_import_only_the_runtime_they_use() {
    let Some(host) = Target::host() else { return };
    let bytes = freestanding("../../tests/freestanding/arithmetic.tarn", &host.to_string()).unwrap();
    let (undefined, _) = symbols(&bytes, "host");
    let runtime: Vec<_> = undefined.iter().map(|s| s.trim_start_matches('_')).filter(|s| s.starts_with("tarn_rt_")).collect();
    assert!(runtime.len() <= 3, "{runtime:?}");
}

#[test]
fn freestanding_heap_imports_only_the_allocator_contract() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/heap.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
        for needed in ["tarn_alloc", "tarn_free", "tarn_realloc"] {
            assert!(undefined.contains(&needed.to_string()), "{target}: {needed} in {undefined:?}");
        }
        assert!(!undefined.iter().any(|s| s.starts_with("tarn_rt_string") || s.starts_with("tarn_rt_vec") || s.starts_with("tarn_rt_env")), "{target}: {undefined:?}");
        assert!(globals.contains(&"tarn_main".to_string()), "{target}: {globals:?}");
    }
}

#[test]
fn freestanding_program_without_heap_does_not_import_the_allocator() {
    let bytes = freestanding("../../tests/freestanding/arithmetic.tarn", "x86_64-none").unwrap();
    let (undefined, _) = symbols(&bytes, "no-heap");
    assert!(!undefined.contains(&"tarn_alloc".to_string()), "{undefined:?}");
}

/// Disassembly of the `tarn_fn_0__main` body, via the system `objdump`.
fn main_disassembly(bytes: &[u8], tag: &str) -> Option<String> {
    let path = std::env::temp_dir().join(format!("tarn-freestanding-dis-{tag}-{}.o", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new("objdump").args(["-d", "--no-show-raw-insn"]).arg(&path).output().ok();
    let _ = std::fs::remove_file(path);
    let text = String::from_utf8_lossy(&out.filter(|o| o.status.success())?.stdout).into_owned();
    let start = text.find("<tarn_fn_0__main>:")?;
    Some(text[start..].split("\n\n").next().unwrap().to_string())
}

#[test]
fn freestanding_volatile_imports_only_the_contract() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/volatile.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
        assert!(globals.contains(&"tarn_main".to_string()), "{target}: {globals:?}");
    }
}

#[test]
fn volatile_accesses_are_never_merged_forwarded_or_reordered() {
    // 2 writes + 2 reads of the same address, with no fence between them.
    let bytes = freestanding("../../tests/freestanding/volatile_order.tarn", "aarch64-none").unwrap();
    let Some(code) = main_disassembly(&bytes, "arm") else { return };
    let mnemonics: Vec<&str> = code.lines().filter_map(|l| l.split('\t').nth(1)).filter_map(|i| i.split_whitespace().next()).filter(|m| ["stlr", "ldar"].contains(m)).collect();
    assert_eq!(mnemonics, ["stlr", "ldar", "ldar", "stlr"], "{code}");

    let bytes = freestanding("../../tests/freestanding/volatile_order.tarn", "x86_64-none").unwrap();
    let Some(code) = main_disassembly(&bytes, "x86") else { return };
    let fences = code.lines().filter(|l| l.contains("mfence")).count();
    let loads = code.lines().filter(|l| l.contains("movl\t(%")).count();
    assert_eq!((fences, loads), (2, 2), "{code}");
}

#[test]
fn volatile_fence_lowers_to_a_machine_fence() {
    let bytes = freestanding("../../tests/freestanding/volatile.tarn", "aarch64-none").unwrap();
    let path = std::env::temp_dir().join(format!("tarn-freestanding-fence-{}.o", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let out = std::process::Command::new("objdump").args(["-d", "--no-show-raw-insn"]).arg(&path).output();
    let _ = std::fs::remove_file(path);
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return };
    assert!(String::from_utf8_lossy(&out.stdout).contains("dmb"), "no dmb in the aarch64 object");
}

#[test]
fn freestanding_large_copies_do_not_import_libc_memory_functions() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/large_copy.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, _) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
        assert!(!undefined.iter().any(|s| ["memcpy", "memmove", "memset", "memcmp", "bcmp"].iter().any(|m| s.trim_start_matches('_') == *m)), "{target}: {undefined:?}");
    }
}
