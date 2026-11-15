//! Real ELF objects, system linking, and child processes. No JIT or simulated
//! execution is substituted for native assertions.
#[path = "../../driver/tests/common/drop_machine.rs"]
mod drop_machine;
use std::path::Path;
use std::process::Command;

fn compile(src: &str, tag: &str) -> (std::path::PathBuf, tarn_driver::CheckResult) {
    let dir = std::env::temp_dir().join(format!("tarn-e2e-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.tarn");
    std::fs::write(&file, src).unwrap();
    let res = tarn_driver::check(&file).unwrap();
    assert!(!res.has_errors(), "{}", res.diagnostics.iter().map(|d| d.render(&res.program.sources)).collect::<String>());
    let exe = dir.join("program");
    tarn_backend::build(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &exe).unwrap();
    (exe, res)
}
#[test]
fn native_scalar_and_aggregate_suite() {
    let mut paths: Vec<_> = std::fs::read_dir("../../tests/native/pass").unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "tarn")).collect();
    paths.sort();
    assert!(paths.len() >= 10);
    for path in paths {
        let tag = path.file_stem().unwrap().to_str().unwrap();
        let (exe, _) = compile(&std::fs::read_to_string(&path).unwrap(), tag);
        let output = Command::new("timeout").arg("30s").arg(&exe).env_remove("TARN_TRACE_DROPS").env_remove("TARN_TRACE_SYNC").output().unwrap();
        assert!(output.status.success(), "{}: {}", path.display(), String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), std::fs::read_to_string(path.with_extension("stdout")).unwrap(), "{}", path.display());
        assert!(output.stderr.is_empty());
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}
#[test]
fn large_programs_are_not_limited_by_function_count() {
    // Functions, closures and generic instances all become separate
    // specializations; ordinary programs must not hit an arbitrary cap.
    let mut src = String::from("struct Box<T> {\n    v T\n}\nfn id<T>(x T) T {\n    return x\n}\n");
    for i in 0..600 {
        src.push_str(&format!("fn f{i}(x i32) i32 {{\n    g := fn(y i32) i32 {{\n        return y + {i}\n    }}\n    return id(g(x)) % 1000\n}}\n"));
        src.push_str(&format!("struct S{i} {{\n    v i32\n}}\n"));
    }
    src.push_str("fn main() {\n    var s: i32 = 0\n");
    for i in 0..600 {
        src.push_str(&format!("    s = f{i}(s) + id(Box{{v: S{i}{{v: 0}}}}).v.v\n"));
    }
    src.push_str("    print(s)\n}\n");
    let (exe, _) = compile(&src, "many-functions");
    let output = Command::new(&exe).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let expected = (0..600).fold(0, |s, i| (s + i) % 1000);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), format!("{expected}\n"));
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
/// Two backend blowups found by scaling sweeps (n = 1600): the inliner kept
/// scanning blocks it had just spliced, so one round followed a call chain to
/// the caller limit in every function (21 s, 1.6M statements for 2.4K); and
/// enum drop glue compared every variant at every drop site even when no
/// variant owned anything (CodeTooLarge).
#[test]
fn call_chains_and_wide_enums_build_in_linear_time() {
    let mut chain = String::new();
    for i in 0..1600 {
        let body = if i == 0 { "x".to_string() } else { format!("f{}(x) + 1", i - 1) };
        chain.push_str(&format!("fn f{i}(x i64) i64 {{\n    return {body}\n}}\n"));
    }
    chain.push_str("fn main() {\n    print(f1599(0))\n}\n");
    let mut wide = String::from("enum E {\n");
    for i in 0..1600 {
        wide.push_str(&format!("    V{i}(i64)\n"));
    }
    wide.push_str("}\nfn f(e E) i64 {\n    match e {\n");
    for i in 0..1600 {
        wide.push_str(&format!("        E.V{i}(x) => return x + {i}\n"));
    }
    wide.push_str("    }\n}\nfn main() {\n    print(f(E.V7(1)))\n}\n");
    for (tag, src, expected) in [("call-chain", chain, "1599\n"), ("wide-enum", wide, "8\n")] {
        let start = std::time::Instant::now();
        let (exe, _) = compile(&src, tag);
        let elapsed = start.elapsed();
        let output = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected, "{tag}");
        // Generous for unoptimized test builds; the regressions took minutes.
        assert!(elapsed < std::time::Duration::from_secs(120), "{tag}: build took {elapsed:?}");
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}
#[test]
fn native_drops_match_post_drop_interpreter_on_every_boolean_path() {
    let mut paths: Vec<_> = std::fs::read_dir("../../tests/drops/pass").unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "tarn")).collect();
    paths.sort();
    let mut runs = 0;
    for path in paths {
        let original = tarn_driver::check(&path).unwrap();
        let p = original.drops.as_ref().unwrap();
        let f = p.functions.iter().find(|f| f.decl.name == "main").unwrap();
        for bits in 0..(1u32 << f.decl.param_count) {
            let args: Vec<_> = (0..f.decl.param_count).map(|i| bits & (1 << i) != 0).collect();
            let (expected, aborted) = drop_machine::execute(p, f.decl.id, &args);
            let source = std::fs::read_to_string(&path).unwrap().replace("fn main(", "fn native_case(");
            let source = format!("{source}\nfn main() {{\n    native_case({})\n}}\n", args.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "));
            let tag = format!("drops-{}-{bits}", path.file_stem().unwrap().to_str().unwrap());
            let (exe, _) = compile(&source, &tag);
            let output = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let drops: Vec<_> = stderr.lines().filter_map(|l| l.strip_prefix("drop:").map(str::to_string)).collect();
            assert_eq!(drops, expected, "{} path {bits}: {stderr}", path.display());
            assert_eq!(!output.status.success(), aborted, "{} path {bits}: {stderr}", path.display());
            assert!(output.stdout.is_empty());
            if !aborted {
                assert!(stderr.lines().all(|l| l.starts_with("drop:")));
            }
            std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
            runs += 1;
        }
    }
    assert!(runs >= 54);
}
#[test]
fn unsupported_valid_features_fail_without_panics() {
    for entry in std::fs::read_dir("../../tests/native/fail").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "tarn") {
            continue;
        }
        let res = tarn_driver::check(&path).unwrap();
        if path.ends_with("opaque.tarn") {
            assert!(res.diagnostics.iter().any(|d| d.code == "E3040"));
            continue;
        }
        assert!(!res.has_errors(), "{} must be frontend-valid", path.display());
        let error = tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap_err();
        assert!(error.to_string().starts_with("backend not implemented:"), "{error}");
    }
}
#[test]
fn checked_arithmetic_and_panic_abort() {
    for (name, body) in [
        ("overflow", "a: i8 := 127\n    b: i8 := 1\n    print(a + b)"),
        ("underflow", "a: u8 := 0\n    b: u8 := 1\n    print(a - b)"),
        ("mul_overflow", "a: u64 := 18446744073709551615\n    b: u64 := 2\n    print(a * b)"),
        ("divide_zero", "a := 10\n    b := 0\n    print(a / b)"),
        ("divide_min", "a: i64 := -9223372036854775808\n    b: i64 := -1\n    print(a / b)"),
        ("cast_range", "a: i16 := 300\n    print(u8(a))"),
        ("shift_width", "a: u8 := 1\n    print(a << 8)"),
        ("shift_negative", "a := 1\n    print(a >> -1)"),
        ("float_u64_boundary", "a: f64 := 18446744073709551616.0\n    print(u64(a))"),
        ("float_i64_boundary", "a: f64 := 9223372036854775808.0\n    print(i64(a))"),
        ("float_f32_range", "a: f32 := 128.0\n    print(i8(a))"),
        ("float_negative_unsigned", "a: f64 := -1.0\n    print(u64(a))"),
        ("slice_reversed", "a := [2]i64{1, 2}\n    s := &a[2..1]\n    print(s.len())"),
        ("float_range", "a: f64 := 256.0\n    print(u8(a))"),
        ("float_nan", "a: f64 := 0.0\n    print(i64(a / a))"),
        ("float_infinity", "a: f64 := 1.0\n    b: f64 := 0.0\n    print(i64(a / b))"),
        ("slice_bounds", "a := [2]i64{1, 2}\n    s := &a[0..2]\n    print(s[2])"),
        ("slice_range", "a := [2]i64{1, 2}\n    s := &a[1..3]\n    print(s.len())"),
        ("copy_range_inside_scalar", "t := \"añ\"\n    print(t.copy_range(0, 2))"),
        ("copy_range_reversed", "t := \"abc\"\n    print(t.copy_range(2, 1))"),
        ("copy_range_past_end", "t := \"abc\"\n    print(t.copy_range(1, 9))"),
        ("divide_zero_variable", "a: i64 := 10\n    b: i64 := 0\n    print(a % b)"),
        // Range proofs (ADR 0057) must keep every check that can fail.
        ("range_le_max", "var i: u8 = 250\n    for i <= 255 {\n        i = i + 1\n    }"),
        ("range_unbounded_mul", "var x: u8 = 1\n    var n = 0\n    for n < 10 {\n        x = x * 2\n        n = n + 1\n    }"),
        ("range_accumulator", "var total: u8 = 0\n    var i: u8 = 0\n    for i < 100 {\n        total = total + i\n        i = i + 1\n    }"),
        ("range_signed_below", "var i: i8 = 0\n    for i > -128 {\n        i = i - 1\n    }\n    print(i - 1)"),
        ("panic", "x := \"live\"\n    panic(\"stop\")"),
    ] {
        let (exe, _) = compile(&format!("fn main() {{\n    {body}\n}}\n"), name);
        let out = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
        assert!(!out.status.success(), "{name} must abort");
        assert!(String::from_utf8_lossy(&out.stderr).contains("panic:"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("drop:"), "abort must not clean stack");
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}
#[test]
fn canonical_layout_offsets_and_alignment() {
    let res = tarn_driver::check(Path::new("../../tests/native/pass/struct.tarn")).unwrap();
    let t = res.typed.unwrap();
    let f = res.drops.unwrap();
    let main = f.functions.iter().find(|f| f.decl.name == "main").unwrap();
    let ty = &main.decl.locals.iter().find(|l| l.name.as_deref() == Some("p")).unwrap().ty;
    let l = tarn_backend::layout::layout(&t, ty).unwrap();
    assert_eq!((l.size, l.align), (16, 8));
    assert_eq!(l.fields.iter().map(|(o, _)| *o).collect::<Vec<_>>(), vec![0, 8]);
}

#[test]
fn native_destruction_order_uses_distinct_resources() {
    let source = "struct Pair {\n    a string\n    b string\n}\nfn scenario(c bool) {\n    var p = Pair{a: \"first\", b: \"second\"}\n    x := p.a\n    if c {\n        p.a = \"replacement\"\n    }\n    y := \"last\"\n}\nfn main() {\n    scenario(true)\n    scenario(false)\n}\n";
    let (exe, res) = compile(source, "drop-order");
    let p = res.drops.as_ref().unwrap();
    let main = p.functions.iter().find(|f| f.decl.name == "main").unwrap();
    let (expected, abort) = drop_machine::execute(p, main.decl.id, &[]);
    assert!(!abort);
    assert_eq!(expected, vec!["last", "first", "replacement", "second", "last", "first", "second"]);
    let out = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(out.status.success());
    let actual: Vec<_> = String::from_utf8_lossy(&out.stderr).lines().map(|s| s.strip_prefix("drop:").unwrap().to_string()).collect();
    assert_eq!(actual, expected);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn backend_rejects_invalid_call_and_return_abi() {
    let mut res = tarn_driver::check(Path::new("../../tests/native/pass/milestone.tarn")).unwrap();
    let p = res.drops.as_mut().unwrap();
    let main = p.functions.iter_mut().find(|f| f.decl.name == "main").unwrap();
    let call = main
        .blocks
        .iter_mut()
        .find_map(|b| match &mut b.term {
            tarn_ir::Terminator::Call { args, .. } if args.len() == 2 => Some(args),
            _ => None,
        })
        .unwrap();
    call[0] = tarn_ir::Operand::Const(tarn_ir::Const::Bool(true));
    let err = tarn_backend::emit_object(p, res.typed.as_ref().unwrap()).unwrap_err();
    assert!(err.to_string().contains("parameter ABI mismatch"));
}

#[test]
fn native_line_deletions_never_panic_or_emit_invalid_code() {
    let dir = std::env::temp_dir().join(format!("tarn-native-mutations-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut sources = Vec::new();
    for category in ["pass", "fail"] {
        for file in std::fs::read_dir(format!("../../tests/native/{category}")).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|x| x == "tarn") {
                let src = std::fs::read_to_string(&path).unwrap();
                sources.push((path, src));
            }
        }
    }
    sources.sort();
    let jobs: Vec<(usize, usize)> = sources.iter().enumerate().flat_map(|(f, (_, s))| (0..s.lines().count()).map(move |i| (f, i))).collect();
    // Independent mutants run on all cores; each worker owns its entry
    // directory because local imports resolve beside the entry file.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let runs = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let (dir, sources, jobs, next, runs) = (&dir, &sources, &jobs, &next, &runs);
            scope.spawn(move || {
                let own = dir.join(format!("w{worker}"));
                std::fs::create_dir_all(&own).unwrap();
                let entry = own.join("main.tarn");
                while let Some(&(f, i)) = jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    let (path, src) = &sources[f];
                    let lines: Vec<_> = src.lines().collect();
                    std::fs::write(&entry, [&lines[..i], &lines[i + 1..]].concat().join("\n")).unwrap();
                    let res = tarn_driver::check(&entry).unwrap();
                    if res.has_errors() {
                        continue;
                    }
                    let emitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap())));
                    assert!(emitted.is_ok(), "backend panic in {} without line {}", path.display(), i + 1);
                    if let Err(e) = emitted.unwrap() {
                        assert!(e.to_string().starts_with("backend not implemented:"), "{} without line {}: {e}", path.display(), i + 1);
                    }
                    runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
    });
    assert!(runs.into_inner() > 40);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn generic_instances_are_reused_and_deterministic() {
    let (exe, res) = compile(
        "fn a<T>(x T, n i64) T { if n == 0 { return x }\n return b(x, n - 1) }\nfn b<T>(x T, n i64) T { return a(x, n) }\nfn unused<T>(x T) T { return x }\nfn main() { print(a(42, 2))\n print(a(1, 0))\n print(a(true, 0)) }",
        "mono-reuse",
    );
    let p = tarn_backend::mono::specialize(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap();
    assert_eq!(p.functions.len(), 5); // main, a<i64>, a<bool>, b<i64>, b<bool>
    assert!(p.functions.iter().all(|f| !f.decl.name.starts_with("unused")));
    let again = tarn_backend::mono::specialize(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap();
    assert_eq!(p.functions.iter().map(|f| &f.decl.name).collect::<Vec<_>>(), again.functions.iter().map(|f| &f.decl.name).collect::<Vec<_>>());
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"42\n1\ntrue\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
#[test]
fn generic_owned_drops_match_interpreter() {
    let source = std::fs::read_to_string("../../tests/native/pass/generic_adts.tarn")
        .unwrap()
        .lines()
        .filter(|line| !line.trim().starts_with("print("))
        .map(|line| line.to_owned() + "\n")
        .collect::<String>()
        .replace("Some(x) => print(x)", "Some(x) => {}")
        .replace("None => print(0)", "None => {}")
        .replace("Ok(x) => print(x)", "Ok(x) => {}")
        .replace("Err(e) => print(e.message)", "Err(e) => {}");
    let (exe, res) = compile(&source, "generic-drops");
    let p = tarn_backend::mono::specialize(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap();
    let (expected, aborted) = drop_machine::execute(&p, p.functions[0].decl.id, &[]);
    assert!(!aborted);
    let out = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(out.status.success());
    let trace: Vec<_> = String::from_utf8_lossy(&out.stderr).lines().filter_map(|s| s.strip_prefix("drop:").map(str::to_owned)).collect();
    assert_eq!(trace, expected);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn dynamic_metadata_corruption_is_rejected() {
    use tarn_ir::{Callee, CoerceKind, Rvalue, StatementKind, Terminator, post_drop as post};
    let source = std::fs::read_to_string("../../tests/native/pass/dynamic.tarn").unwrap();
    let (exe, res) = compile(&source, "dynamic-corruption");
    let t = res.typed.as_ref().unwrap();
    let p = tarn_backend::mono::specialize(res.drops.as_ref().unwrap(), t).unwrap();
    tarn_backend::verify_dynamic(&p, t).unwrap();
    for mutation in 0..6 {
        let mut corrupt = post::Program { functions: p.functions.clone(), by_symbol: p.by_symbol.clone() };
        let table = corrupt
            .functions
            .iter_mut()
            .flat_map(|f| &mut f.blocks)
            .flat_map(|b| &mut b.stmts)
            .find_map(|s| match &mut s.op {
                post::Op::Plain(StatementKind::Assign(_, Rvalue::Coerce(CoerceKind::DynTable { interface, concrete, methods }, _, ty))) => Some((interface, concrete, methods, ty)),
                _ => None,
            })
            .unwrap();
        match mutation {
            0 => {
                table.2.pop();
            }
            1 => table.2.swap(0, 1),
            2 => *table.0 = tarn_resolve::SymbolId(u32::MAX),
            3 => *table.1 = tarn_types::Ty::Bool,
            4 => *table.3 = tarn_types::Ty::Bool,
            _ => table.2[0] = tarn_ir::FunctionId(u32::MAX),
        }
        assert!(tarn_backend::verify_dynamic(&corrupt, t).is_err(), "mutation {mutation}");
    }
    let mut corrupt = post::Program { functions: p.functions.clone(), by_symbol: p.by_symbol.clone() };
    let call = corrupt
        .functions
        .iter_mut()
        .flat_map(|f| &mut f.blocks)
        .find_map(|b| match &mut b.term {
            Terminator::Call { callee: Callee::Virtual { method, .. }, .. } => Some(method),
            _ => None,
        })
        .unwrap();
    *call = tarn_resolve::SymbolId(u32::MAX);
    assert!(tarn_backend::verify_dynamic(&corrupt, t).is_err());
    let mut corrupt = post::Program { functions: p.functions.clone(), by_symbol: p.by_symbol.clone() };
    for f in &mut corrupt.functions {
        for b in &mut f.blocks {
            b.stmts.retain(|s| !matches!(&s.op, post::Op::Plain(StatementKind::Assign(_, Rvalue::Coerce(CoerceKind::DynTable { .. }, _, _)))));
        }
    }
    assert!(tarn_backend::verify_dynamic(&corrupt, t).is_err());
    let mut corrupt = post::Program { functions: p.functions.clone(), by_symbol: p.by_symbol.clone() };
    corrupt.functions[0].decl.locals[0].ty = tarn_types::Ty::Any(tarn_resolve::SymbolId(0));
    assert!(tarn_backend::verify_dynamic(&corrupt, t).is_err());
    let out = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(out.status.success());
    let trace: Vec<_> = String::from_utf8_lossy(&out.stderr).lines().filter_map(|s| s.strip_prefix("drop:").map(str::to_owned)).collect();
    assert_eq!(trace, ["second", "first"]);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn static_bounds_do_not_instantiate_unused_interface_methods() {
    let source = "interface I {\n fn used(&self) i64\n fn unused(&self)\n}\nstruct S { n i64 }\nimpl I for S {\n fn used(&self) i64 { return self.n }\n fn unused(&self) { spawn print(0) }\n}\nfn call<T:I>(x &T) i64 { return x.used() }\nfn main() { s := S{n:42}\n print(call(&s)) }\n";
    let (exe, res) = compile(source, "static-reachability");
    let p = tarn_backend::mono::specialize(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap();
    assert!(p.functions.iter().all(|f| !f.decl.name.contains("unused")));
    assert!(
        p.functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.stmts)
            .all(|s| !matches!(&s.op, tarn_ir::post_drop::Op::Plain(tarn_ir::StatementKind::Assign(_, tarn_ir::Rvalue::Coerce(tarn_ir::CoerceKind::DynTable { .. }, _, _)))))
    );
    assert_eq!(Command::new(&exe).output().unwrap().stdout, b"42\n");
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn owned_closure_environments_destroy_each_capture_once() {
    for (name, trace) in [
        ("owned_closures", "drop:hello\ndrop:second\ndrop:first\ndrop:unused\ndrop:nested\ndrop:generic\n"),
        ("closure_lifetimes", "drop:borrow\ndrop:replacement\n"),
        ("closure_aggregates", "drop:inside\ndrop:shared\ndrop:conditional\ndrop:field\ndrop:copy-field\n"),
        ("closure_reinitialize", "drop:old\ndrop:new\ndrop:new\n"),
        ("closure_callbacks", "drop:callback\ndrop:generic-environment\n"),
        ("closure_clone", "drop:clone\ndrop:clone\ndrop:clone\n"),
        ("closure_own_callable", "drop:nested-callable\n"),
        ("closure_partial", "drop:second-field\ndrop:first-field\ndrop:first-field\ndrop:second-field\ndrop:fallback\ndrop:first-field\ndrop:second-field\n"),
        ("closure_enum_arrays", "drop:enum-consuming\ndrop:array-first\ndrop:array-second\ndrop:enum\n"),
    ] {
        let path = Path::new("../../tests/native/pass").join(format!("{name}.tarn"));
        let (exe, _) = compile(&std::fs::read_to_string(&path).unwrap(), &format!("trace-{name}"));
        let output = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
        assert!(output.status.success(), "{name}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), trace, "{name}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), std::fs::read_to_string(path.with_extension("stdout")).unwrap());
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

#[test]
fn invalid_closure_environment_metadata_is_rejected() {
    let path = Path::new("../../tests/native/pass/owned_closures.tarn");
    let result = tarn_driver::check(path).unwrap();
    assert!(!result.has_errors());
    let source = result.drops.as_ref().unwrap();
    let t = result.typed.as_ref().unwrap();
    for mutation in 0..5 {
        let mut p = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let f = p.functions.iter_mut().find(|f| matches!(f.decl.kind, tarn_ir::FnKind::Closure { owned: true, consumes: false, .. })).unwrap();
        let tarn_ir::FnKind::Closure { environment, captures, destructor, owned, .. } = &mut f.decl.kind else { unreachable!() };
        match mutation {
            0 => environment.clear(),
            1 => captures[0] = tarn_ir::CaptureMode::SharedBorrow,
            2 => *destructor = None,
            3 => *destructor = Some(tarn_ir::FunctionId(u32::MAX)),
            4 => *owned = false,
            _ => unreachable!(),
        }
        assert!(!tarn_ir::post_drop::verify(&p, t).is_empty(), "mutation {mutation}");
        assert!(tarn_backend::emit_object(&p, t).is_err(), "mutation {mutation}");
    }
}

#[test]
fn executable_publication_replaces_the_inode_before_launch() {
    let (exe, result) = compile("fn main() { print(42) }", "atomic-publication");
    // This deterministically reproduces the executable-busy hazard of copying
    // onto the destination inode. Publication must replace that inode instead.
    let writer = std::fs::OpenOptions::new().write(true).open(&exe).unwrap();
    tarn_backend::build(result.drops.as_ref().unwrap(), result.typed.as_ref().unwrap(), &exe).unwrap();
    let output = Command::new(&exe).env_remove("TARN_TRACE_DROPS").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    drop(writer);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn native_tasks_destroy_captures_and_results_once_on_all_exit_paths() {
    let path = Path::new("../../tests/native/pass/task_completion.tarn");
    let (exe, _) = compile(&std::fs::read_to_string(path).unwrap(), "task-completion-trace");
    let output = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"42\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let drops = stderr.lines().map(|line| line.strip_prefix("drop:").expect("unexpected task stderr")).collect::<Vec<_>>();
    assert_eq!(drops, ["capture-normal", "unused-normal", "outer-normal", "capture-return", "unused-return",
        "unused-break", "unused-continue", "unused-continue", "unused-inner", "unused-outer", "unused-expression",
        "unused-overwrite", "joined-replacement", "unused-conditional"]);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();

    let source = std::fs::read_to_string("../../tests/native/pass/tasks.tarn").unwrap();
    let (exe, _) = compile(&source, "task-owned-results-trace");
    let output = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let mut drops = stderr.lines().map(|line| line.strip_prefix("drop:").unwrap()).collect::<Vec<_>>();
    drops.sort();
    assert_eq!(drops, ["enum", "generic", "hello", "left", "right"]);
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn worker_panic_aborts_the_process_and_task_metadata_is_verified() {
    use std::os::unix::process::ExitStatusExt;
    let (exe, result) = compile("fn main() { t := spawn move fn() { panic(\"worker panic\") }\n t.join() }", "task-worker-panic");
    let output = Command::new(&exe).env_remove("TARN_TRACE_DROPS").output().unwrap();
    assert_eq!(output.status.signal(), Some(6));
    assert!(String::from_utf8_lossy(&output.stderr).contains("worker panic"));
    let source = result.drops.as_ref().unwrap();
    let typed = result.typed.as_ref().unwrap();
    for kind in 0..4 {
        let mut corrupted = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let block = corrupted.functions.iter_mut().flat_map(|f| &mut f.blocks).find(|b|
            matches!(b.term, tarn_ir::Terminator::Call { callee: tarn_ir::Callee::TaskSpawn { .. }, .. })).unwrap();
        if let tarn_ir::Terminator::Call { callee: tarn_ir::Callee::TaskSpawn { worker, drop_result, .. }, args, spawn, .. } = &mut block.term {
            match kind {
                0 => *worker = tarn_ir::FunctionId(u32::MAX),
                1 => *drop_result = *worker,
                2 => args.clear(),
                _ => *spawn = false,
            }
        }
        assert!(!tarn_ir::post_drop::verify(&corrupted, typed).is_empty());
        assert!(tarn_backend::emit_object(&corrupted, typed).unwrap_err().to_string().starts_with("compiler bug"));
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn scoped_completion_destroys_results_before_borrowed_storage_and_verifies_witnesses() {
    let path = "../../tests/native/pass/scoped_task_completion.tarn";
    let (exe, result) = compile(&std::fs::read_to_string(path).unwrap(), "scoped-completion-trace");
    let output = Command::new(&exe).env("TARN_TRACE_DROPS", "1").output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "42\n");
    let expected = ["normal-result", "normal-parent", "return-result", "return-parent", "try-result", "try-parent",
        "continue-result", "continue-parent", "continue-result", "continue-parent", "break-result", "break-parent",
        "inner-result", "inner-parent", "outer-result", "outer-parent"];
    let trace: Vec<_> = String::from_utf8(output.stderr).unwrap().lines().map(str::to_owned).collect();
    assert_eq!(trace, expected.map(|s| format!("drop:{s}")));
    let source = result.drops.as_ref().unwrap();
    let typed = result.typed.as_ref().unwrap();
    for kind in 0..3 {
        let mut corrupted = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        if kind == 0 {
            let local = corrupted.functions.iter_mut().flat_map(|f| &mut f.decl.locals)
                .find(|l| l.kind == tarn_ir::LocalKind::TaskScopeWitness).unwrap();
            local.kind = tarn_ir::LocalKind::User;
        } else {
            let block = corrupted.functions.iter_mut().flat_map(|f| &mut f.blocks).find(|b|
                matches!(b.term, tarn_ir::Terminator::Call { callee: tarn_ir::Callee::TaskSpawn { scoped: true, .. }, .. })).unwrap();
            if let tarn_ir::Terminator::Call { callee: tarn_ir::Callee::TaskSpawn { scoped, .. }, args, .. } = &mut block.term {
                if kind == 1 { *scoped = false; } else { args.pop(); }
            }
        }
        assert!(!tarn_ir::post_drop::verify(&corrupted, typed).is_empty());
        assert!(tarn_backend::emit_object(&corrupted, typed).is_err());
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn synchronization_destruction_traces_cover_every_normal_exit() {
    let cases = [
        ("normal", "g := m.lock()", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("owner_move", "owner := m\n g := owner.lock()", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("owner_self", "var owner = m\n owner = owner\n g := owner.lock()", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("return", "g := m.lock()\n return", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("break", "for i in 0..3 { g := m.lock()\n break }", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("continue", "for i in 0..2 { g := m.lock()\n continue }", "sync:create,sync:lock,sync:unlock,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("move", "g := m.lock()\n moved := g", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("self", "var g = m.lock()\n g = g", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("conditional_true", "var g: MutexGuard<string>\n if true { g = m.lock() }", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
        ("conditional_false", "var g: MutexGuard<string>\n if false { g = m.lock() }", "sync:create,drop:payload,sync:destroy"),
        ("nested", "g := m.lock()\n { other := Mutex.new(\"other\")\n h := other.lock() }", "sync:create,sync:lock,sync:create,sync:lock,sync:unlock,drop:other,sync:destroy,sync:unlock,drop:payload,sync:destroy"),
        ("overwrite", "other := Mutex.new(\"other\")\n var g = m.lock()\n g = other.lock()", "sync:create,sync:create,sync:lock,sync:lock,sync:unlock,sync:unlock,drop:other,sync:destroy,drop:payload,sync:destroy"),
        ("aggregate_move", "g := m.lock()\n wrapped := Box{value: g}\n moved := wrapped", "sync:create,sync:lock,sync:unlock,drop:payload,sync:destroy"),
    ];
    for (tag, body, expected) in cases {
        let source = format!("struct Box<T> {{ value T }}\nfn main() {{\n m := Mutex.new(\"payload\")\n {body}\n}}\n");
        let (exe, _) = compile(&source, &format!("sync-drops-{tag}"));
        let out = Command::new("timeout").arg("15s").arg(&exe).env("TARN_TRACE_DROPS", "1").env("TARN_TRACE_SYNC", "1").output().unwrap();
        assert!(out.status.success(), "{tag}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty(), "{tag}");
        assert_eq!(String::from_utf8(out.stderr).unwrap().lines().collect::<Vec<_>>(), expected.split(',').collect::<Vec<_>>(), "{tag}");
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

#[test]
fn synchronized_owned_payloads_destroy_exactly_once() {
    let (exe, _) = compile(include_str!("../../../tests/native/pass/synchronization_owned.tarn"), "sync-owned-drops");
    let out = Command::new("timeout").arg("15s").arg(&exe).env("TARN_TRACE_DROPS", "1").env("TARN_TRACE_SYNC", "1").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let trace = String::from_utf8(out.stderr).unwrap();
    assert_eq!(trace, "sync:create\nsync:lock\ndrop:original\nsync:unlock\nsync:lock\nsync:unlock\nsync:create\nsync:lock\ndrop:generic\nsync:unlock\nsync:create\nsync:lock\nsync:unlock\nsync:create\nsync:lock\ndrop:second\nsync:unlock\nsync:create\nsync:lock\nsync:unlock\nsync:create\nsync:lock\nsync:unlock\nsync:lock\nsync:unlock\nsync:destroy\ndrop:closure\nsync:destroy\ndrop:first\ndrop:third\nsync:destroy\ndrop:enum\nsync:destroy\ndrop:changed\nsync:destroy\ndrop:replacement\nsync:destroy\n");
    let mut payloads = trace.lines().filter_map(|l| l.strip_prefix("drop:")).collect::<Vec<_>>();
    payloads.sort();
    assert_eq!(payloads, vec!["changed", "closure", "enum", "first", "generic", "original", "replacement", "second", "third"]);
    assert_eq!(trace.lines().filter(|l| *l == "sync:create").count(), 6);
    assert_eq!(trace.lines().filter(|l| *l == "sync:destroy").count(), 6);
    assert_eq!(trace.lines().filter(|l| *l == "sync:lock").count(), trace.lines().filter(|l| *l == "sync:unlock").count());
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn atomic_checked_arithmetic_aborts_at_each_integer_boundary() {
    use std::os::unix::process::ExitStatusExt;
    for (ty, max, min) in [
        ("AtomicI32", "2147483647", "-2147483648"),
        ("AtomicI64", "9223372036854775807", "-9223372036854775808"),
        ("AtomicU32", "4294967295", "0"),
        ("AtomicU64", "18446744073709551615", "0"),
        ("AtomicUsize", "18446744073709551615", "0"),
    ] {
        for (op, boundary) in [("fetch_add", max), ("fetch_sub", min)] {
            let source = format!("fn main() {{ a := {ty}.new({boundary})\n a.{op}(1) }}\n");
            let (exe, _) = compile(&source, &format!("atomic-overflow-{ty}-{op}"));
            let out = Command::new("timeout").arg("15s").arg(&exe).env_remove("TARN_TRACE_DROPS").env_remove("TARN_TRACE_SYNC").output().unwrap();
            assert_eq!(out.status.signal(), Some(6), "{ty} {op}: {:?}", out.status);
            assert!(String::from_utf8_lossy(&out.stderr).contains("panic: checked arithmetic"));
            std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
        }
    }
}

#[test]
fn synchronization_abi_corruption_is_rejected_without_panics() {
    let (_, res) = compile("fn main() { m := Mutex.new(i64(0))\n var g = m.lock()\n g.replace(1)\n a := AtomicI64.new(0)\n a.fetch_add(1) }", "sync-metadata");
    let source = res.drops.as_ref().unwrap();
    let typed = res.typed.as_ref().unwrap();
    for mutation in 0..4 {
        let mut program = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let call = program.functions.iter_mut().flat_map(|f| &mut f.blocks).find_map(|b| match &mut b.term {
            tarn_ir::Terminator::Call { callee: tarn_ir::Callee::Intrinsic(name), args, .. } if name == "AtomicI64.fetch_add" => Some(args),
            _ => None,
        }).unwrap();
        match mutation {
            0 => { call.pop(); },
            1 => call[1] = tarn_ir::Operand::Const(tarn_ir::Const::Bool(true)),
            2 => call[0] = tarn_ir::Operand::Const(tarn_ir::Const::Int(0, tarn_types::IntTy::I64)),
            3 => { call.push(tarn_ir::Operand::Const(tarn_ir::Const::Bool(false))); },
            _ => unreachable!(),
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(&program, typed)));
        assert!(result.is_ok(), "mutation {mutation} panicked");
        assert!(result.unwrap().is_err(), "mutation {mutation} emitted an invalid synchronization ABI");
    }
}

#[test]
fn destruction_trace_oracle_detects_missing_and_duplicate_unlocks() {
    let (exe, result) = compile("fn main() { m := Mutex.new(\"payload\")\n g := m.lock() }", "sync-drop-mutations");
    let source = result.drops.as_ref().unwrap();
    let typed = result.typed.as_ref().unwrap();
    for duplicate in [false, true] {
        let mut program = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let main = program.functions.iter_mut().find(|f| f.decl.name == "main").unwrap();
        let guard = main.decl.locals.iter().position(|l| l.name.as_deref() == Some("g")).unwrap();
        let block = main.blocks.iter_mut().find(|b| b.stmts.iter().any(|s| matches!(&s.op,
            tarn_ir::post_drop::Op::Destroy(tarn_ir::post_drop::Drop::Value(p)) if p.local.0 as usize == guard))).unwrap();
        let index = block.stmts.iter().position(|s| matches!(&s.op,
            tarn_ir::post_drop::Op::Destroy(tarn_ir::post_drop::Drop::Value(p)) if p.local.0 as usize == guard)).unwrap();
        if duplicate { block.stmts.insert(index, block.stmts[index].clone()); }
        else { block.stmts.remove(index); }
        tarn_backend::build(&program, typed, &exe).unwrap();
        let out = Command::new("timeout").arg("10s").arg(&exe).env("TARN_TRACE_DROPS", "1").env("TARN_TRACE_SYNC", "1").output().unwrap();
        let expected = "sync:create\nsync:lock\nsync:unlock\ndrop:payload\nsync:destroy\n";
        assert!(!out.status.success() || out.stderr != expected.as_bytes(), "destruction mutant escaped the exact trace oracle");
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn owning_storage_rejects_nonempty_finish_and_put() {
    for (tag, body) in [
        ("vec", "var values: Vec<Token> = Vec.new()\nvalues.push(Token{id: 1})\nvalues.finish()"),
        ("option", "value: Option<Token> := Some(Token{id: 1})\nfinish_empty(value)"),
        ("array", "values: [1]Option<Token> := [1]Option<Token>{Some(Token{id: 1})}\nfinish_empty(values)"),
        ("put", "var value: Option<Token> = Some(Token{id: 1})\nvalue.put(Token{id: 2})\nfinish_empty(value)"),
    ] {
        let source = format!("#[must_consume]\nstruct Token {{ id u64 }}\nfn main() {{\n{body}\n}}\n");
        let (exe, _) = compile(&source, &format!("owning-nonempty-{tag}"));
        let output = Command::new(&exe).output().unwrap();
        assert!(!output.status.success(), "{tag} silently destroyed an owner");
        std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
    }
}

#[test]
fn zero_sized_nominal_owners_cannot_finish_empty() {
    let (exe, _) = compile("#[must_consume]\nstruct Token {}\nfn main() { finish_empty(Token{}) }\n", "zero-sized-owner");
    assert!(!Command::new(&exe).output().unwrap().status.success());
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn owning_slot_abi_corruption_is_rejected_without_panics() {
    let (exe, result) = compile("fn main() { var value = 1\nprevious := replace_owned(&mut value, 2) }", "owning-slot-abi");
    let source = result.drops.as_ref().unwrap();
    for mutation in 0..4 {
        let mut program = tarn_ir::post_drop::Program { functions: source.functions.clone(), by_symbol: source.by_symbol.clone() };
        let args = program.functions.iter_mut().flat_map(|f| &mut f.blocks).find_map(|b| match &mut b.term {
            tarn_ir::Terminator::Call { callee: tarn_ir::Callee::Intrinsic(name), args, .. } if name == "core.replace_owned" => Some(args),
            _ => None,
        }).unwrap();
        match mutation {
            0 => { args.pop(); }
            1 => args.push(tarn_ir::Operand::Const(tarn_ir::Const::Bool(false))),
            2 => args[0] = tarn_ir::Operand::Const(tarn_ir::Const::Bool(false)),
            3 => args[1] = tarn_ir::Operand::Const(tarn_ir::Const::Bool(false)),
            _ => unreachable!(),
        }
        let emitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tarn_backend::emit_object(&program, result.typed.as_ref().unwrap())));
        assert!(emitted.is_ok() && emitted.unwrap().is_err(), "mutation {mutation} accepted an invalid owning slot ABI");
    }
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}

#[test]
fn opaque_generic_owners_cannot_finish_empty() {
    let (exe, _) = compile("#[must_consume]\nstruct Token { id u64 }\nstruct Owner<T> { raw usize }\nfn main() { owner: Owner<Token> := Owner{raw: 1}\nfinish_empty(owner) }\n", "opaque-generic-owner");
    assert!(!Command::new(&exe).output().unwrap().status.success());
    std::fs::remove_dir_all(exe.parent().unwrap()).unwrap();
}
