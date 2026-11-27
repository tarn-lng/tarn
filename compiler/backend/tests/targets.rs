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
    let stores = code.lines().filter(|l| l.contains("movl\t") && l.contains(", (%")).count();
    assert_eq!((fences, loads, stores), (0, 2, 2), "{code}");
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

#[test]
fn freestanding_cpu_intrinsics_import_only_the_contract() {
    for (target, program) in [("x86_64-none", "arch_x86_64"), ("aarch64-none", "arch_aarch64")] {
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
        assert!(globals.contains(&"tarn_main".to_string()), "{target}: {globals:?}");
        // Stubs are Local functions of the same object, never exported.
        assert!(!globals.iter().any(|s| s.contains("tarn_arch_")), "{target}: {globals:?}");
    }
}

#[test]
fn cpu_intrinsics_of_the_other_architecture_are_a_compile_error() {
    for (target, program, other) in [("aarch64-none", "arch_x86_64", "x86_64"), ("x86_64-none", "arch_aarch64", "aarch64"), ("aarch64-linux", "arch_x86_64", "x86_64")] {
        let error = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap_err().to_string();
        assert!(error.contains(&format!("is an {other} CPU intrinsic but the target is {target}")), "{target}: {error}");
        assert!(error.contains("import `arch_"), "{error}");
    }
}

#[test]
fn only_the_stubs_a_program_calls_are_emitted() {
    // A program without intrinsics carries no stub.
    let bytes = freestanding("../../tests/freestanding/arithmetic.tarn", "x86_64-none").unwrap();
    let path = std::env::temp_dir().join(format!("tarn-freestanding-nostub-{}.o", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let out = std::process::Command::new("nm").arg(&path).output().unwrap();
    let _ = std::fs::remove_file(path);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("tarn_arch_"), "unused stubs emitted");
}

/// Disassemble `bytes` with the host `objdump` (llvm; reads either ELF
/// architecture) in a stable syntax: `symbol -> lowercase instructions`.
fn stub_disassembly(bytes: &[u8], tag: &str, intel: bool) -> Option<std::collections::HashMap<String, Vec<String>>> {
    disassembly_of(bytes, tag, intel, "tarn_arch_")
}

/// Like `stub_disassembly` for the functions whose symbol starts with `prefix`.
fn disassembly_of(bytes: &[u8], tag: &str, intel: bool, prefix: &str) -> Option<std::collections::HashMap<String, Vec<String>>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("tarn-freestanding-stubs-{tag}-{prefix}-{}-{}.o", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::write(&path, bytes).unwrap();
    let mut command = std::process::Command::new("objdump");
    command.args(["-d", "--no-show-raw-insn"]);
    if intel { command.arg("--x86-asm-syntax=intel"); }
    let out = command.arg(&path).output().ok();
    let _ = std::fs::remove_file(path);
    let text = String::from_utf8_lossy(&out.filter(|o| o.status.success())?.stdout).into_owned();
    let mut map = std::collections::HashMap::new();
    for block in text.split("\n\n") {
        let Some(start) = block.find(&format!("<{prefix}")) else { continue };
        let name = block[start + 1..].split(">:").next().unwrap().to_string();
        let instructions: Vec<String> = block.lines().skip_while(|l| !l.contains(">:")).skip(1)
            .filter_map(|l| l.split_once(':').filter(|(address, _)| !address.trim().is_empty() && address.trim().bytes().all(|b| b.is_ascii_hexdigit())).map(|(_, i)| i))
            .map(|i| i.split("//").next().unwrap().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().replace(" ,", ","))
            .collect();
        map.insert(name, instructions);
    }
    Some(map)
}

#[test]
fn hot_cpu_loops_have_instructions_in_place_of_calls() {
    for target in ["x86_64-none", "x86_64-macos", "aarch64-none"] {
        let intel = target.starts_with("x86_64");
        let program = if intel { "cpu_hot_loop_x86_64" } else { "cpu_hot_loop_aarch64" };
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap();
        let Some(functions) = disassembly_of(&bytes, &format!("hot-{target}"), intel, "") else { eprintln!("objdump unavailable; skipping {target}"); continue };
        // Mach-O adds an underscore to the symbol.
        let code = functions.iter().find(|(name, _)| name.ends_with("tarn_fn_0__main")).unwrap().1;
        assert!(!code.iter().any(|i| i.starts_with("call") || i.starts_with("bl ")), "{target}: {code:?}");
        if intel {
            for instruction in ["out dx, al", "invlpg byte ptr [rdi]", "mov cr3, rdi", "pause"] {
                assert!(code.iter().any(|i| i == instruction), "{target}: missing {instruction}: {code:?}");
            }
            // This constant-operand hot loop needs no spill slots. The frame
            // pointer remains because register allocation still sees calls.
            assert!(!code.iter().any(|i| i.contains("[rsp") || i.contains("[rbp")), "{target}: {code:?}");
        } else {
            for instruction in ["mrs x0, ttbr0_el1", "msr ttbr0_el1, x0", "dsb sy", "isb"] {
                assert!(code.iter().any(|i| i == instruction), "{target}: missing {instruction}: {code:?}");
            }
        }
    }
}

#[test]
fn oversized_x86_sequences_keep_their_frameless_stub_calls() {
    let bytes = freestanding("../../tests/freestanding/arch_x86_64.tarn", "x86_64-none").unwrap();
    let Some(functions) = disassembly_of(&bytes, "remaining-calls", true, "") else { eprintln!("objdump unavailable; skipping x86_64"); return };
    let code = &functions["tarn_fn_0__main"];
    // Checked arithmetic also emits indirect calls to the fault contract.
    // rdtsc, rdmsr, wrmsr, cpuid, xgetbv, xsetbv and call_on_stack are too long to inline (ADR 0107).
    assert_eq!(code.iter().filter(|i| i.starts_with("call 0x")).count(), 7, "{code:?}");
    // The short T6 instructions are in place of their calls.
    for instruction in ["ltr di", "str eax", "ud2", "wbinvd", "clflush byte ptr [rdi]", "swapgs", "rdfsbase rax", "rdgsbase rax", "wrfsbase rdi", "wrgsbase rdi", "pushfq", "invpcid rdi, xmmword ptr [rsi]"] {
        assert!(code.iter().any(|i| i == instruction), "missing {instruction}: {code:?}");
    }
    for name in ["rdtsc", "rdmsr", "wrmsr", "xgetbv", "xsetbv"] {
        let stub = &functions[&format!("tarn_arch_x86_64_{name}")];
        assert!(!stub.iter().any(|i| i.starts_with("push") || i.starts_with("pop") || i.contains("rsp") || i.contains("rbp")), "{name}: {stub:?}");
    }
}

#[test]
fn aarch64_maintenance_instructions_replace_their_calls() {
    let bytes = freestanding("../../tests/freestanding/arch_aarch64.tarn", "aarch64-none").unwrap();
    let Some(functions) = disassembly_of(&bytes, "t6-aarch64", false, "") else { eprintln!("objdump unavailable; skipping aarch64"); return };
    let code = &functions["tarn_fn_0__main"];
    for instruction in ["tlbi vmalle1", "tlbi vmalle1is", "tlbi vae1, x0", "tlbi vae1is, x0", "tlbi vale1, x0", "tlbi aside1, x0",
        "dsb ish", "dsb ishst", "dsb nsh", "dsb nshst", "dmb ish", "dmb ishld", "dmb ishst", "ic iallu", "ic ialluis", "ic ivau, x0",
        "dc civac, x0", "dc cvac, x0", "dc ivac, x0", "dc zva, x0", "dc cvau, x0", "at s1e1r, x0", "at s1e1w, x0",
        "mrs x0, par_el1", "mrs x0, ctr_el0", "mrs x0, dczid_el0", "mrs x0, midr_el1", "mrs x0, mpidr_el1", "mrs x0, sp_el0", "msr sp_el0, x0"] {
        assert!(code.iter().any(|i| i == instruction), "missing {instruction}: {code:?}");
    }
    // The only remaining branch-and-link calls are the long stubs: semihost_exit and call_on_stack.
    assert_eq!(code.iter().filter(|i| i.starts_with("bl ")).count(), 2, "{code:?}");
}

#[test]
fn repeated_volatile_stores_and_unused_loads_survive_with_an_explicit_fence() {
    for target in ["x86_64-none", "x86_64-macos", "aarch64-none"] {
        let intel = target.starts_with("x86_64");
        let bytes = freestanding("../../tests/freestanding/volatile_repeated.tarn", target).unwrap();
        let Some(functions) = disassembly_of(&bytes, &format!("repeated-{target}"), intel, "") else { eprintln!("objdump unavailable; skipping {target}"); continue };
        let code = functions.iter().find(|(name, _)| name.ends_with("tarn_fn_0__main")).unwrap().1;
        let effects: Vec<_> = code.iter().filter_map(|i| {
            if intel {
                if i.starts_with("mov qword ptr [") { Some("store") }
                else if i.starts_with("mov ") && i.contains(", qword ptr [") { Some("load") }
                else if i == "mfence" { Some("fence") }
                else { None }
            } else if i.starts_with("stlr ") { Some("store") }
            else if i.starts_with("ldar ") { Some("load") }
            else if i == "dmb ish" { Some("fence") }
            else { None }
        }).collect();
        assert_eq!(effects, ["store", "store", "load", "load", "fence", "store"], "{target}: {code:?}");
    }
}

#[test]
fn volatile_scalar_widths_and_bitcasts_emit_and_run() {
    let path = std::path::Path::new("../../tests/freestanding/volatile_scalars.tarn");
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding(path.to_str().unwrap(), target).unwrap();
        let Some(code) = disassembly_of(&bytes, &format!("scalars-{target}"), target.starts_with("x86"), "") else { eprintln!("objdump unavailable; skipping disassembly for {target}"); continue };
        if target.starts_with("x86") {
            assert!(!code.values().flatten().any(|i| i == "mfence"), "{code:?}");
        }
    }
    if Target::host().is_none() { return; }
    let checked = tarn_driver::check(path).unwrap();
    let dir = std::env::temp_dir().join(format!("tarn-volatile-scalars-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    let exe = dir.join("program");
    tarn_backend::build(checked.drops.as_ref().unwrap(), checked.typed.as_ref().unwrap(), &exe).unwrap();
    let output = std::process::Command::new(exe).output().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(output.status.success(), "{output:?}");
}

// GNU and LLVM objdump print AArch64 immediates in different bases.
fn normalize_immediates(instruction: &str) -> String {
    let mut parts = instruction.split('#');
    let mut normalized = parts.next().unwrap_or_default().to_string();
    for operand in parts {
        normalized.push('#');
        if let Some(hex) = operand.strip_prefix("0x") {
            let digits = hex.chars().take_while(|c| c.is_ascii_hexdigit()).count();
            if digits > 0 {
                let value = u64::from_str_radix(&hex[..digits], 16).unwrap();
                normalized.push_str(&value.to_string());
                normalized.push_str(&hex[digits..]);
                continue;
            }
        }
        normalized.push_str(operand);
    }
    normalized
}

#[test]
fn immediate_normalization_preserves_operands_and_instruction_order() {
    assert_eq!(normalize_immediates("stp x2, x3, [sp, #0x10]"), "stp x2, x3, [sp, #16]");
    assert_eq!(normalize_immediates("svc #0x0"), "svc #0");
    assert_eq!(normalize_immediates("tbnz w1, #0x0, #0x10"), "tbnz w1, #0, #16");
    assert_eq!(normalize_immediates("sub sp, sp, #784"), "sub sp, sp, #784");
}

#[test]
fn cpu_stubs_disassemble_to_their_documented_assembly() {
    for (target, program, arch, intel) in [("x86_64-none", "arch_x86_64", Arch::X86_64, true), ("aarch64-none", "arch_aarch64", Arch::Aarch64, false)] {
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap();
        let Some(disassembly) = stub_disassembly(&bytes, target, intel) else { eprintln!("objdump unavailable; skipping {target}"); continue };
        let listing = tarn_backend::cpu_stub_listing(arch);
        assert!(listing.len() >= 20, "{target}: {} stubs", listing.len());
        let operations = tarn_backend::cpu_operations(arch);
        assert_eq!(operations.len(), listing.len());
        let mut checked = 0;
        for (symbol, asm) in listing {
            let Some(actual) = disassembly.get(&symbol) else { panic!("{target}: stub {symbol} missing from the object") };
            // Alignment padding follows the stub; compare the documented prefix.
            // Disassemblers differ in knowing the GICv3 register names; compare the encodings.
            let generic = |i: &String| [("icc_sre_el1", "s3_0_c12_c12_5"), ("icc_pmr_el1", "s3_0_c4_c6_0"), ("icc_igrpen1_el1", "s3_0_c12_c12_7"), ("icc_iar1_el1", "s3_0_c12_c12_0"), ("icc_eoir1_el1", "s3_0_c12_c12_1")]
                .iter().fold(i.clone(), |i, (name, encoding)| i.replace(name, encoding));
            let (actual, expected): (Vec<_>, Vec<_>) = (actual.iter().map(generic).map(|i| normalize_immediates(&i)).collect(), asm.iter().map(generic).map(|i| normalize_immediates(&i)).collect());
            assert_eq!(&actual[..expected.len().min(actual.len())], expected.as_slice(), "{target}: {symbol}");
            checked += 1;
        }
        assert_eq!(checked, operations.len());
    }
}

#[test]
fn hosted_build_runs_an_unprivileged_cpu_intrinsic() {
    let Some(host) = Target::host() else { return };
    let program = match host.arch {
        Arch::X86_64 => "arch_host_x86_64",
        Arch::Aarch64 => "arch_host_aarch64",
    };
    let res = tarn_driver::check(std::path::Path::new(&format!("../../tests/freestanding/{program}.tarn"))).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    let dir = std::env::temp_dir().join(format!("tarn-cpu-host-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("program");
    tarn_backend::build(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &exe).unwrap_or_else(|e| panic!("{e}"));
    let out = std::process::Command::new(&exe).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{:?}", out);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "cpu intrinsics ok");
}

#[test]
fn rng_instructions_and_feature_queries_require_unsafe() {
    let dir = std::env::temp_dir().join(format!("tarn-rng-safety-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.tarn");
    for (module, calls) in [
        ("arch_x86_64", vec!["rdrand64", "rdseed64", "has_rdrand", "has_rdseed"]),
        ("arch_aarch64", vec!["rndr", "rndrrs", "has_rng", "read_id_aa64isar0_el1"]),
    ] {
        for call in calls {
            std::fs::write(&main, format!("import \"{module}\"\nfn main() {{ {module}.{call}() }}\n")).unwrap();
            let checked = tarn_driver::check(&main).unwrap();
            assert!(checked.diagnostics.iter().any(|d| d.code == "E3071"), "{module}.{call}: {:?}", checked.diagnostics);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------- ADR 0101, P6

/// A just-enough ELF64 little-endian reader: section headers, symbols and
/// RELA entries, so tests assert what the object says rather than what a
/// host tool prints.
struct Elf<'a> {
    bytes: &'a [u8],
    sections: Vec<ElfSection>,
    symbols: Vec<ElfSymbol>,
}
#[derive(Debug)]
struct ElfSection {
    name: String,
    kind: u32,
    flags: u64,
    offset: usize,
    size: usize,
    link: usize,
    info: usize,
    align: u64,
}
#[derive(Debug)]
struct ElfSymbol {
    name: String,
    global: bool,
    kind: u8,
    section: usize,
    value: u64,
}
impl<'a> Elf<'a> {
    fn parse(bytes: &'a [u8]) -> Elf<'a> {
        let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as usize;
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        assert_eq!(&bytes[..6], b"\x7fELF\x02\x01");
        let (shoff, entry, count, names) = (u64_at(0x28) as usize, u16_at(0x3a), u16_at(0x3c), u16_at(0x3e));
        let raw = |i: usize| {
            let h = shoff + i * entry;
            (u32_at(h) as usize, u32_at(h + 4), u64_at(h + 8), u64_at(h + 24) as usize, u64_at(h + 32) as usize, u32_at(h + 40) as usize, u32_at(h + 44) as usize, u64_at(h + 48))
        };
        let table = raw(names);
        let c_string = |table: &[u8], at: usize| String::from_utf8_lossy(&table[at..at + table[at..].iter().position(|c| *c == 0).unwrap()]).into_owned();
        let shstrtab = &bytes[table.3..table.3 + table.4];
        let sections: Vec<ElfSection> = (0..count)
            .map(|i| {
                let (name, kind, flags, offset, size, link, info, align) = raw(i);
                ElfSection { name: c_string(shstrtab, name), kind, flags, offset, size, link, info, align }
            })
            .collect();
        let mut symbols = Vec::new();
        if let Some(symtab) = sections.iter().find(|s| s.kind == 2) {
            let strtab = &sections[symtab.link];
            let strings = &bytes[strtab.offset..strtab.offset + strtab.size];
            for n in 1..symtab.size / 24 {
                let at = symtab.offset + n * 24;
                symbols.push(ElfSymbol { name: c_string(strings, u32_at(at) as usize), global: bytes[at + 4] >> 4 != 0, kind: bytes[at + 4] & 15, section: u16_at(at + 6), value: u64_at(at + 8) });
            }
        }
        Elf { bytes, sections, symbols }
    }
    fn section(&self, name: &str) -> &ElfSection {
        self.sections.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no section {name}: {:?}", self.sections.iter().map(|s| &s.name).collect::<Vec<_>>()))
    }
    fn symbol(&self, name: &str) -> &ElfSymbol {
        self.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no symbol {name}"))
    }
    /// The one symbol whose name ends in `__{name}` (a Local static).
    fn local_static(&self, name: &str) -> &ElfSymbol {
        self.symbols.iter().find(|s| s.name.starts_with("tarn_static_") && s.name.ends_with(&format!("__{name}"))).unwrap_or_else(|| panic!("no static {name}"))
    }
    /// `len` bytes of the object a symbol names.
    fn contents(&self, symbol: &ElfSymbol, len: usize) -> &[u8] {
        let section = &self.sections[symbol.section];
        let start = section.offset + symbol.value as usize;
        &self.bytes[start..start + len]
    }
    /// (offset in `section`, relocation type, target symbol name) of its RELA entries.
    fn relocations(&self, section: usize) -> Vec<(u64, u32, String)> {
        let mut out = Vec::new();
        for rela in self.sections.iter().filter(|s| s.kind == 4 && s.info == section) {
            for n in 0..rela.size / 24 {
                let at = rela.offset + n * 24;
                let (offset, info) = (u64::from_le_bytes(self.bytes[at..at + 8].try_into().unwrap()), u64::from_le_bytes(self.bytes[at + 8..at + 16].try_into().unwrap()));
                out.push((offset, info as u32, self.symbols[(info >> 32) as usize - 1].name.clone()));
            }
        }
        out
    }
}

fn freestanding_source(tag: &str, source: &str, target: &str) -> tarn_backend::Result<Vec<u8>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("tarn-freestanding-src-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, source).unwrap();
    let result = freestanding(file.to_str().unwrap(), target);
    let _ = std::fs::remove_dir_all(dir);
    result
}

#[test]
fn contract_defined_in_tarn_leaves_no_undefined_symbols() {
    for target in ["x86_64-none", "aarch64-none"] {
        // Entry and panic handler, no `main`.
        let bytes = freestanding("../../tests/freestanding/kernel.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.is_empty(), "{target}: {undefined:?}");
        for name in ["kmain", "tarn_rt_fault", "tarn_rt_panic", "tarn_rt_panic_at", "kernel_magic"] {
            assert!(globals.contains(&name.to_string()), "{target}: {name} in {globals:?}");
        }
        assert!(!globals.contains(&"tarn_main".to_string()), "{target}: no main, no tarn_main: {globals:?}");

        // Heap, panics and the allocator itself, all defined in Tarn.
        let bytes = freestanding("../../tests/freestanding/bump_heap.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.is_empty(), "{target}: {undefined:?}");
        for name in ["tarn_alloc", "tarn_free", "tarn_realloc", "tarn_rt_fault", "tarn_main"] {
            assert!(globals.contains(&name.to_string()), "{target}: {name} in {globals:?}");
        }
    }
}

#[test]
fn exported_names_are_global_symbols_of_the_right_kind() {
    let bytes = freestanding("../../tests/freestanding/kernel.tarn", "x86_64-none").unwrap();
    let elf = Elf::parse(&bytes);
    const FUNC: u8 = 2;
    const OBJECT: u8 = 1;
    for name in ["kmain", "tarn_rt_fault", "tarn_rt_panic", "tarn_rt_panic_at"] {
        let symbol = elf.symbol(name);
        assert!(symbol.global && symbol.kind == FUNC, "{name}: {symbol:?}");
    }
    let magic = elf.symbol("kernel_magic");
    assert!(magic.global && magic.kind == OBJECT, "{magic:?}");
    assert_eq!(elf.contents(magic, 4), 0x1badb002u32.to_le_bytes());
    // Everything else stays private to the object.
    let boot = elf.local_static("BOOT");
    assert!(!boot.global && boot.kind == OBJECT, "{boot:?}");
    assert!(elf.symbols.iter().filter(|s| s.global && s.section != 0).all(|s| ["kmain", "tarn_rt_fault", "tarn_rt_panic", "tarn_rt_panic_at", "kernel_magic"].contains(&s.name.as_str())), "{:?}", elf.symbols);
}

#[test]
fn sections_carry_the_requested_names_flags_and_alignment() {
    const WRITE: u64 = 1;
    const ALLOC: u64 = 2;
    const EXEC: u64 = 4;
    const NOBITS: u32 = 8;
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/kernel.tarn", target).unwrap();
        let elf = Elf::parse(&bytes);
        // A boot-loader request: its own writable data section, 0x30 bytes of C layout.
        let requests = elf.section(".limine_requests");
        assert_eq!((requests.size, requests.align, requests.flags & (WRITE | ALLOC | EXEC)), (0x30, 8, WRITE | ALLOC), "{target}");
        let boot = elf.local_static("BOOT");
        let mut expected = Vec::new();
        for word in [0xc7b1dd30df4c8b88u64, 0x0a82e883a194f07b, 0x9d5827dcd881dd75, 0xa3148604b1f4faa0, 3, 0] {
            expected.extend_from_slice(&word.to_le_bytes());
        }
        assert_eq!(elf.contents(boot, 0x30), expected.as_slice(), "{target}");
        assert_eq!(elf.sections[boot.section].name, ".limine_requests");
        // A function in a named code section; its section is executable.
        let kmain = elf.symbol("kmain");
        let code = &elf.sections[kmain.section];
        assert_eq!(code.name, ".text.boot", "{target}");
        assert_eq!(code.flags & (ALLOC | EXEC | WRITE), ALLOC | EXEC, "{target}");
        // Zero-filled statics are .bss, aligned as asked, without file contents.
        let stack = elf.local_static("STACK");
        let bss = &elf.sections[stack.section];
        assert_eq!((bss.name.as_str(), bss.kind, bss.size, stack.value % 4096), (".bss", NOBITS, 16384, 0), "{target}");
        assert!(bss.align >= 4096, "{target}: {bss:?}");
        // An immutable static is read-only data.
        let rodata = &elf.sections[elf.symbol("kernel_magic").section];
        assert_eq!((rodata.name.as_str(), rodata.flags & (WRITE | ALLOC)), (".rodata", ALLOC), "{target}");
    }
}

#[test]
fn static_data_follows_c_layout_with_relocations_and_alignment() {
    for (target, absolute) in [("x86_64-none", 1u32), ("aarch64-none", 257)] {
        let bytes = freestanding("../../tests/freestanding/statics.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let elf = Elf::parse(&bytes);
        // struct Header { magic u32, flags u8, next *Header, ratio f32, ok bool }: 24 bytes, align 8.
        let first = elf.symbol("first");
        let mut want = Vec::new();
        want.extend_from_slice(&0xdeadbeefu32.to_le_bytes());
        want.extend_from_slice(&[7, 0, 0, 0]);
        want.extend_from_slice(&[0; 8]); // `next`: filled in by the relocation
        want.extend_from_slice(&(-2.0f32).to_le_bytes());
        want.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(elf.contents(first, 24), want.as_slice(), "{target}");
        let relocations = elf.relocations(first.section);
        assert!(relocations.iter().any(|(offset, kind, name)| *offset == first.value + 8 && *kind == absolute && name == "second"), "{target}: {relocations:?}");

        let second = elf.symbol("second");
        let mut want = Vec::new();
        want.extend_from_slice(&2u32.to_le_bytes());
        want.extend_from_slice(&[0, 0, 0, 0]);
        want.extend_from_slice(&[0; 8]);
        want.extend_from_slice(&1.5f32.to_le_bytes());
        want.extend_from_slice(&[1, 0, 0, 0]);
        assert_eq!(elf.contents(second, 24), want.as_slice(), "{target}");
        assert_ne!(first.section, second.section, "{target}: read-only and writable data are separate");

        let table = elf.local_static("TABLE");
        assert_eq!(elf.contents(table, 6), [0xff, 0xff, 2, 0, 0x2c, 1], "{target}");

        let aligned = elf.local_static("ALIGNED");
        assert_eq!(aligned.value % 64, 0, "{target}");
        assert!(elf.sections[aligned.section].align >= 64, "{target}");
        assert_eq!(elf.contents(aligned, 1), [9], "{target}");
    }
}

#[test]
fn every_function_gets_its_own_section_only_when_one_is_named() {
    // Without `#[section]` the layout of the object is unchanged: one `.text`.
    let bytes = freestanding("../../tests/freestanding/arithmetic.tarn", "x86_64-none").unwrap();
    let elf = Elf::parse(&bytes);
    assert_eq!(elf.sections.iter().filter(|s| s.name.starts_with(".text")).count(), 1, "{:?}", elf.sections);
    // With one, the others are named after their symbol and the ELF stays valid.
    let bytes = freestanding("../../tests/freestanding/kernel.tarn", "x86_64-none").unwrap();
    let elf = Elf::parse(&bytes);
    let text: Vec<&str> = elf.sections.iter().filter(|s| s.name.starts_with(".text")).map(|s| s.name.as_str()).collect();
    assert!(text.contains(&".text.boot") && text.contains(&".text.tarn_rt_fault"), "{text:?}");
    // Relocation sections follow the section they patch.
    assert!(elf.sections.iter().any(|s| s.name == ".rela.text.boot"), "{:?}", elf.sections.iter().map(|s| &s.name).collect::<Vec<_>>());
    if let Some(listing) = main_disassembly_of(&bytes, "<kmain>:") {
        assert!(listing.contains("call"), "{listing}");
    }
}

fn main_disassembly_of(bytes: &[u8], marker: &str) -> Option<String> {
    let path = std::env::temp_dir().join(format!("tarn-freestanding-sec-{}.o", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new("objdump").args(["-d", "--no-show-raw-insn"]).arg(&path).output().ok();
    let _ = std::fs::remove_file(path);
    let text = String::from_utf8_lossy(&out.filter(|o| o.status.success())?.stdout).into_owned();
    let start = text.find(marker)?;
    Some(text[start..].split("\n\n").next().unwrap().to_string())
}

#[test]
fn sections_are_an_elf_feature() {
    for target in ["x86_64-macos", "aarch64-macos"] {
        let error = freestanding("../../tests/freestanding/section_fn_hosted.tarn", target).unwrap_err().to_string();
        assert!(error.contains("`#[section]` is supported on ELF targets only") && error.contains(target), "{target}: {error}");
        let error = freestanding("../../tests/freestanding/section_static_hosted.tarn", target).unwrap_err().to_string();
        assert!(error.contains("`#[section]` is supported on ELF targets only"), "{target}: {error}");
    }
    // Hosted Linux objects are ELF too.
    let bytes = freestanding("../../tests/freestanding/section_fn_hosted.tarn", "x86_64-linux").unwrap();
    let elf = Elf::parse(&bytes);
    assert!(elf.sections.iter().any(|s| s.name == ".text.hot"), "{:?}", elf.sections);
}

#[test]
fn kernel_without_main_is_only_valid_freestanding() {
    let res = tarn_driver::check(std::path::Path::new("../../tests/freestanding/kernel.tarn")).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    assert!(res.missing_main().is_some());
    assert!(res.missing_main_for(true).is_none());
    let error = freestanding("../../tests/freestanding/kernel.tarn", "x86_64-linux").unwrap_err().to_string();
    assert!(error.contains("program requires fn main()"), "{error}");
}

#[test]
fn exports_are_validated_against_the_target_and_the_contract() {
    let wrong_signature = "#[export(\"tarn_alloc\")]\nfn a(size u32) *mut u8 {\n    return ffi.null()\n}\nimport \"ffi\"\n";
    let error = freestanding_source("sig", wrong_signature, "x86_64-none").unwrap_err().to_string();
    assert!(error.contains("`tarn_alloc` is part of the freestanding contract and must have the signature (i64) -> i64"), "{error}");

    let reserved = "#[export(\"tarn_helper\")]\nfn helper() {}\n";
    let error = freestanding_source("reserved", reserved, "x86_64-none").unwrap_err().to_string();
    assert!(error.contains("names starting with `tarn_` belong to the compiler"), "{error}");

    // The contract may be defined only where it is a contract.
    let hosted = "#[export(\"tarn_rt_fault\")]\nfn fault() {}\nfn main() {}\n";
    let error = freestanding_source("hosted-contract", hosted, "x86_64-linux").unwrap_err().to_string();
    assert!(error.contains("belong to the compiler"), "{error}");

    let main = "#[export(\"main\")]\nfn entry() {}\nfn main() {}\n";
    let error = freestanding_source("main", main, "x86_64-linux").unwrap_err().to_string();
    assert!(error.contains("C `main` shim"), "{error}");

    // `tarn_main` is the compiler's entry shim; a program naming its own entry does not clash.
    let own_entry = "#[export(\"_start\")]\nfn start() {}\nfn main() {}\n";
    let bytes = freestanding_source("entry", own_entry, "x86_64-none").unwrap();
    let (_, globals) = symbols(&bytes, "entry");
    assert!(globals.contains(&"_start".to_string()) && globals.contains(&"tarn_main".to_string()), "{globals:?}");
    let colliding = "#[export(\"tarn_main\")]\nfn start() {}\nfn main() {}\n";
    assert!(freestanding_source("clash", colliding, "x86_64-none").is_err());
}

#[test]
fn exported_functions_forward_to_their_tarn_body_with_the_c_abi() {
    // A narrow signature keeps its extension attributes at the boundary.
    let bytes = freestanding_source("abi", "#[export(\"add_bytes\")]\nfn add(a u8, b i16, flag bool) u32 {\n    if flag {\n        return u32(a) + 1\n    }\n    return u32(b)\n}\n", "x86_64-none").unwrap();
    let (undefined, globals) = symbols(&bytes, "abi");
    // Overflow checks may call the contract's fault handler; nothing else is imported.
    assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{undefined:?}");
    assert!(globals.contains(&"add_bytes".to_string()));
}

#[test]
fn linking_a_kernel_with_an_elf_linker() {
    let target: Target = "x86_64-none".parse().unwrap();
    let Some(linker) = tarn_backend::link::find_linker(target) else {
        eprintln!("no ELF linker on this machine; skipping (set TARN_LD or install ld.lld)");
        return;
    };
    let res = tarn_driver::check(std::path::Path::new("../../tests/freestanding/kernel.tarn")).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    let dir = std::env::temp_dir().join(format!("tarn-link-kernel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (object, elf_path) = (dir.join("kernel.o"), dir.join("kernel.elf"));
    tarn_backend::build_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &object, target).unwrap();
    let script = std::path::Path::new("../../tests/freestanding/kernel.ld");
    tarn_backend::link::run_linker(&linker, &object, &elf_path, script, None).unwrap_or_else(|e| panic!("{e}"));
    let bytes = std::fs::read(&elf_path).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    // ET_EXEC for x86_64, entered at `kmain`, the first thing in `.text`.
    assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 2, "ET_EXEC");
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 62);
    assert_eq!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), 0x100000, "entry is kmain at the start of .text");
    let magic = 0xc7b1dd30df4c8b88u64.to_le_bytes();
    assert!(bytes.windows(8).any(|w| w == magic), "the Limine request is in the image");
}

// ---------------------------------------------------------------- ADR 0101, P7

fn symbol_named<'a>(elf: &'a Elf, middle: &str) -> &'a ElfSymbol {
    if !middle.starts_with('_') { return elf.symbol(middle); }
    elf.symbols.iter().find(|s| s.name.starts_with("tarn_interrupt_") && s.name.ends_with(middle)).unwrap_or_else(|| panic!("no trampoline for {middle}: {:?}", elf.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()))
}

#[test]
fn interrupt_programs_import_only_the_contract() {
    for (target, program) in [("x86_64-none", "interrupts_x86_64"), ("aarch64-none", "interrupts_aarch64")] {
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, globals) = symbols(&bytes, target);
        assert!(undefined.is_empty(), "{target}: {undefined:?}");
        assert!(globals.contains(&"kmain".to_string()), "{target}: {globals:?}");
    }
}

#[test]
fn trampolines_disassemble_to_the_documented_sequence() {
    for (target, program, arch, intel, variants) in [
        ("x86_64-none", "interrupts_x86_64", Arch::X86_64, true, vec![("_breakpoint", false), ("_page_fault", true), ("timer_interrupt", false)]),
        ("aarch64-none", "interrupts_aarch64", Arch::Aarch64, false, vec![("_synchronous", false), ("_irq", false), ("unused_handler", false)]),
    ] {
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap();
        let elf = Elf::parse(&bytes);
        let Some(disassembly) = disassembly_of(&bytes, target, intel, "") else { eprintln!("objdump unavailable; skipping {target}"); continue };
        for (name, error_code) in variants {
            let symbol = symbol_named(&elf, name);
            let expected: Vec<_> = tarn_backend::interrupt_listing(arch, error_code).into_iter().map(|l| if l.starts_with("tbnz ") { l.rsplit_once(", ").unwrap().0.to_string() } else { l }).collect();
            let actual = disassembly.get(&symbol.name).unwrap_or_else(|| panic!("{target}: {} not disassembled: {:?}", symbol.name, disassembly.keys().collect::<Vec<_>>()));
            // The call or branch to the adapter prints its address; compare the mnemonic.
            let actual: Vec<String> = actual.iter().take(expected.len()).map(|l| if l.starts_with("call ") || l.starts_with("bl ") { l.split(' ').next().unwrap().to_string() } else if l.starts_with("tbnz ") { l.rsplit_once(", ").unwrap().0.to_string() } else { l.clone() }).collect();
            let actual: Vec<_> = actual.iter().map(|i| normalize_immediates(i)).collect();
            let expected: Vec<_> = expected.iter().map(|i| normalize_immediates(i)).collect();
            assert_eq!(actual, expected, "{target}: {}", symbol.name);
        }
    }
}

#[test]
fn trampolines_call_the_c_adapter_of_their_handler() {
    // R_X86_64_PLT32 = 4, R_AARCH64_CALL26 = 283.
    for (target, program, kind, handlers) in [
        ("x86_64-none", "interrupts_x86_64", 4u32, ["_breakpoint", "_page_fault", "timer_interrupt"]),
        ("aarch64-none", "interrupts_aarch64", 283, ["_synchronous", "_irq", "unused_handler"]),
    ] {
        let bytes = freestanding(&format!("../../tests/freestanding/{program}.tarn"), target).unwrap();
        let elf = Elf::parse(&bytes);
        for name in handlers {
            let symbol = symbol_named(&elf, name);
            assert_eq!(symbol.kind, 2, "{target}: {name} is a function");
            let relocations: Vec<_> = elf.relocations(symbol.section).into_iter().filter(|(offset, ..)| *offset >= symbol.value && symbol_end(&elf, symbol) > *offset).collect();
            assert_eq!(relocations.len(), 1, "{target}: {name}: {relocations:?}");
            let (offset, found, callee) = &relocations[0];
            assert_eq!(*found, kind, "{target}: {name}");
            assert!(callee.starts_with("tarn_c_callback_"), "{target}: {name} calls {callee}");
            // The relocated field is the call's operand: the last 4 bytes of x86's 5-byte call, a whole aarch64 `bl`.
            let at = (offset - symbol.value) as usize;
            let code = elf.contents(symbol, at + 4);
            if target == "x86_64-none" { assert_eq!(code[at - 1], 0xe8, "{name}: call opcode"); } else { assert_eq!(code[at + 3], 0x94, "{name}: bl"); }
        }
        // `#[export]` makes the trampoline itself the global symbol.
        let exported = elf.symbol(if target == "x86_64-none" { "timer_interrupt" } else { "unused_handler" });
        assert!(exported.global && exported.kind == 2, "{target}: {exported:?}");
        assert!(elf.relocations(exported.section).iter().any(|(_, _, callee)| callee.starts_with("tarn_c_callback_")), "{target}");
        // No stray C adapter or C-ABI symbol takes the exported name.
        assert_eq!(elf.symbols.iter().filter(|s| s.name == exported.name).count(), 1);
    }
}

/// End of a function symbol: the next symbol of its section, or the section's end.
fn symbol_end(elf: &Elf, symbol: &ElfSymbol) -> u64 {
    elf.symbols.iter().filter(|s| s.section == symbol.section && s.value > symbol.value).map(|s| s.value).min().unwrap_or(elf.sections[symbol.section].size as u64)
}

#[test]
fn the_aarch64_vector_table_is_aligned_and_branches_to_its_handlers() {
    let bytes = freestanding("../../tests/freestanding/interrupts_aarch64.tarn", "aarch64-none").unwrap();
    let elf = Elf::parse(&bytes);
    let table = elf.symbol("exception_vectors");
    assert!(table.global && table.kind == 2);
    assert_eq!(table.value % 2048, 0, "VBAR_EL1 needs 2 KiB alignment");
    assert!(elf.sections[table.section].align >= 2048, "{:?}", elf.sections[table.section]);
    let words: Vec<u32> = elf.contents(table, 2048).chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    // Every entry is `b` (target filled in by the relocation) or `b .`, and the other 31 words are zero.
    for slot in 0..16 {
        assert_eq!(words[slot * 32], 0x1400_0000, "slot {slot}");
        assert!(words[slot * 32 + 1..(slot + 1) * 32].iter().all(|w| *w == 0));
    }
    let relocations: Vec<_> = elf.relocations(table.section).into_iter().filter(|(o, ..)| *o >= table.value && *o < table.value + 2048).collect();
    let slot_of = |o: u64| (o - table.value) / 128;
    assert_eq!(relocations.len(), 2, "{relocations:?}");
    for (offset, kind, callee) in relocations {
        assert_eq!((offset - table.value) % 128, 0);
        assert_eq!(kind, 283, "R_AARCH64_CALL26");
        match slot_of(offset) {
            4 => assert!(callee.ends_with("_synchronous"), "{callee}"),
            5 => assert!(callee.ends_with("_irq"), "{callee}"),
            other => panic!("unexpected slot {other}"),
        }
    }
}

#[test]
fn handler_addresses_come_from_callback_and_are_the_trampolines() {
    // `ffi.callback(handler)` yields the trampoline, not a C adapter: kmain's
    // address operand relocates against the trampoline symbols.
    let bytes = freestanding("../../tests/freestanding/interrupts_x86_64.tarn", "x86_64-none").unwrap();
    let elf = Elf::parse(&bytes);
    let referenced: std::collections::HashSet<String> = elf.sections.iter().enumerate().flat_map(|(i, _)| elf.relocations(i)).map(|(_, _, name)| name).collect();
    for name in ["_breakpoint", "_page_fault"] {
        let symbol = symbol_named(&elf, name);
        assert!(referenced.contains(&symbol.name), "{} is never referenced: {referenced:?}", symbol.name);
    }
    assert!(referenced.contains("timer_interrupt"));
}

#[test]
fn interrupt_attribute_misuse_is_a_build_error() {
    let handler = "#[interrupt]\nfn h(frame *mut u64) {}\n";
    let contract = "#[export(\"tarn_rt_fault\")]\nfn f() {}\n#[export(\"tarn_rt_panic\")]\nfn p(m *u8) {}\n#[export(\"tarn_rt_panic_at\")]\nfn q(m *u8, l *u8) {}\n";
    let expect = |tag: &str, source: String, target: &str, message: &str| {
        let error = freestanding_source(tag, &source, target).expect_err(tag);
        assert!(error.message.contains(message), "{tag}: {}", error.message);
    };
    // Handlers need a freestanding target.
    for target in ["x86_64-linux", "aarch64-linux"] {
        expect("hosted", format!("import \"ffi\"\n{handler}fn main() {{\n    a := ffi.callback(h)\n}}\n"), target, "freestanding");
    }
    // An error code exists only on x86_64.
    expect("code-arm", format!("{contract}#[interrupt]\n#[export(\"h\")]\nfn h(frame *mut u64, code u64) {{}}\n"), "aarch64-none", "error code");
    // Vector table slots exist only on aarch64.
    expect("slot-x86", format!("{contract}#[interrupt(3)]\nfn h(frame *mut u64) {{}}\n"), "x86_64-none", "aarch64");
    expect("table-x86", format!("{contract}#[vector_table]\nfn v() {{}}\n"), "x86_64-none", "aarch64");
    // A slot needs a table, and a slot has one handler.
    expect("no-table", format!("{contract}#[interrupt(3)]\nfn h(frame *mut u64) {{}}\n"), "aarch64-none", "#[vector_table]");
    expect("same-slot", format!("{contract}#[vector_table]\nfn v() {{}}\n#[interrupt(3)]\nfn a(frame *mut u64) {{}}\n#[interrupt(3)]\nfn b(frame *mut u64) {{}}\n"), "aarch64-none", "slot 3");
    expect("two-tables", format!("{contract}#[vector_table]\nfn v() {{}}\n#[vector_table]\nfn w() {{}}\n"), "aarch64-none", "at most one");
}

#[test]
fn a_handler_without_attributes_beyond_interrupt_stays_private() {
    // No `#[export]`: nothing but the contract and entry is global.
    let source = "import \"ffi\"\n#[export(\"tarn_rt_fault\")]\nfn f() {}\n#[export(\"tarn_rt_panic\")]\nfn p(m *u8) {}\n#[export(\"tarn_rt_panic_at\")]\nfn q(m *u8, l *u8) {}\n#[interrupt]\nfn h(frame *mut u64) {}\n#[export(\"kmain\")]\nfn k() {\n    a := ffi.callback(h)\n}\n";
    let bytes = freestanding_source("private", source, "x86_64-none");
    let bytes = match bytes { Ok(b) => b, Err(e) => panic!("{e}") };
    let elf = Elf::parse(&bytes);
    let trampoline = symbol_named(&elf, "_h");
    assert!(!trampoline.global);
}

// ADR 0102: addresses retain otherwise-unused bodies and relocate to callbacks.
#[test]
fn static_function_addresses_relocate_to_adapters_and_interrupt_trampolines() {
    for (target, absolute) in [("x86_64-none", 1u32), ("aarch64-none", 257)] {
        let bytes = freestanding("../../tests/freestanding/static_function_addresses.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let elf = Elf::parse(&bytes);
        let table = elf.local_static("TABLE");
        assert_eq!(elf.contents(table, 40)[0], 7);
        assert_eq!(&elf.contents(table, 40)[8..], &[0; 32]);
        let relocations = elf.relocations(table.section);
        let at = |offset| relocations.iter().find(|(o, ..)| *o == table.value + offset).unwrap();
        let plain = at(8);
        let exported = at(16);
        let irq = at(24);
        for r in [plain, exported, irq] { assert_eq!(r.1, absolute, "{target}: {r:?}"); }
        assert!(plain.2.starts_with("tarn_c_callback_"), "{plain:?}");
        assert!(exported.2.starts_with("tarn_c_callback_"), "{exported:?}");
        assert_ne!(plain.2, exported.2);
        assert_eq!(irq.2, "irq_callback");
        assert_eq!(elf.symbol(&irq.2).kind, 2, "STT_FUNC");
        assert!(!relocations.iter().any(|(o, ..)| *o == table.value + 32), "null has no relocation");
        let repeated = elf.local_static("REPEATED");
        let rs = elf.relocations(repeated.section);
        assert!(rs.iter().any(|(o, k, name)| *o == repeated.value && *k == absolute && name == &plain.2));
        assert!(rs.iter().any(|(o, k, name)| *o == repeated.value + 8 && *k == absolute && name == "irq_callback"));
        assert!(elf.symbols.iter().filter(|s| s.section == 0).all(|s| s.name.is_empty() || ["tarn_rt_fault", "tarn_rt_panic", "tarn_rt_panic_at"].contains(&s.name.as_str())), "{target}: unexpected imports: {:?}", elf.symbols);
    }
}

#[test]
fn static_function_addresses_emit_on_all_hosted_object_formats() {
    let source = "import \"ffi\"\nfn f() {}\nstatic ADDRESS *u8 = ffi.callback(f)\nfn main() {}\n";
    for target in ["x86_64-linux", "aarch64-linux", "x86_64-macos", "aarch64-macos"] {
        let bytes = freestanding_source(&format!("static-callback-{target}"), source, target).unwrap_or_else(|e| panic!("{target}: {e}"));
        assert_eq!(header(&bytes).0, if target.ends_with("macos") { "macho" } else { "elf" });
    }
}

#[test]
fn static_callbacks_resolve_module_paths_and_vector_tables() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/types/pass/static_function_paths/main.tarn", target).unwrap();
        let elf = Elf::parse(&bytes);
        let data = elf.local_static("CALLBACK");
        assert!(elf.relocations(data.section).iter().any(|(o, _, name)| *o == data.value && name.starts_with("tarn_c_callback_")));
    }
    let bytes = freestanding_source("static-vector-address", "import \"ffi\"\n#[vector_table]\n#[export(\"vectors\")]\nfn vectors() {}\nstatic ADDRESS *u8 = ffi.callback(vectors)\nfn main() {}\n", "aarch64-none").unwrap();
    let elf = Elf::parse(&bytes);
    let data = elf.local_static("ADDRESS");
    assert!(elf.relocations(data.section).iter().any(|(o, k, name)| *o == data.value && *k == 257 && name == "vectors"));
}

#[test]
fn raw_projections_emit_on_both_freestanding_targets() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/raw_projections.tarn", target).unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, _) = symbols(&bytes, &format!("raw-projection-{target}"));
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{undefined:?}");
    }
}

#[test]
fn context_storage_matches_the_documented_c_abi_size() {
    for (target, module, size) in [("x86_64-none", "arch_x86_64", 72), ("aarch64-none", "arch_aarch64", 176)] {
        let bytes = freestanding(&format!("../../tests/freestanding/{module}.tarn"), target).unwrap();
        let elf = Elf::parse(&bytes);
        let context = elf.section(".context_from");
        assert_eq!(context.size, size, "{target}");
        assert_eq!(context.align, 8, "{target}");
    }
}

#[test]
fn execution_context_and_user_access_apis_require_unsafe() {
    let dir = std::env::temp_dir().join(format!("tarn-context-safety-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.tarn");
    for (module, calls) in [
        ("arch_x86_64", vec!["stac()", "clac()", "switch_stack(0, ffi.null(), 0)", "context_switch(CONTEXT, ffi.to_const(CONTEXT))", "context_init(CONTEXT, 0, ffi.null(), 0)"]),
        ("arch_aarch64", vec!["pan_enable()", "pan_disable()", "read_pan()", "read_spsel()", "write_spsel(0)", "read_sp()", "switch_stack(0, ffi.null(), 0)", "switch_stack_el0(0, ffi.null(), 0)", "context_switch(CONTEXT, ffi.to_const(CONTEXT))", "context_init(CONTEXT, 0, ffi.null(), 0)"]),
    ] {
        for call in calls {
            std::fs::write(&main, format!("import \"{module}\"\nimport \"ffi\"\nstatic mut CONTEXT {module}.Context\nfn main() {{ {module}.{call} }}\n")).unwrap();
            let checked = tarn_driver::check(&main).unwrap();
            assert!(checked.diagnostics.iter().any(|d| d.code == "E3071"), "{module}.{call}: {:?}", checked.diagnostics);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn freestanding_owning_slots_preserve_the_contract() {
    for target in ["x86_64-none", "aarch64-none"] {
        let bytes = freestanding("../../tests/freestanding/owning_slots.tarn", target)
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        let (undefined, _) = symbols(&bytes, target);
        assert!(undefined.iter().all(|s| tarn_backend::FREESTANDING_CONTRACT.contains(&s.as_str())), "{target}: {undefined:?}");
    }
}

#[test]
fn idle_enable_and_halt_are_adjacent_in_the_caller() {
    for target in ["x86_64-none", "x86_64-macos"] {
        let bytes = freestanding("../../tests/freestanding/sti_hlt.tarn", target).unwrap();
        // Unlike two intrinsic calls, a single FB F4 template cannot acquire
        // padding or register-allocation instructions between STI and HLT.
        assert!(bytes.windows(5).any(|w| w == [0xfb, 0xf4, 0x90, 0x90, 0x90]), "{target}: missing contiguous inlined template");
        let Some(functions) = disassembly_of(&bytes, &format!("idle-{target}"), true, "") else { eprintln!("objdump unavailable; skipping disassembly for {target}"); continue };
        let code = functions.iter().find(|(name, _)| name.ends_with("tarn_fn_0__main")).unwrap().1;
        let at = code.iter().position(|i| i == "sti").expect("STI in caller");
        assert_eq!(code[at + 1], "hlt", "{target}: {code:?}");
    }
}
