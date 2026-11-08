//! Explicit baseline collection, never an automatic timing threshold.
use std::{process::Command, time::Instant};
#[test]
#[ignore = "host-dependent baseline; run with --ignored --nocapture"]
fn native_baseline() {
    let dir = std::env::temp_dir().join(format!("tarn-baseline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["arithmetic", "calls", "slices", "dynamic"] {
        let res = tarn_driver::check(std::path::Path::new(&format!("../../benchmarks/native/{name}.tarn"))).unwrap();
        assert!(!res.has_errors(), "{name}: {:?}", res.diagnostics);
        let start = Instant::now();
        let object = tarn_backend::emit_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap()).unwrap();
        let codegen = start.elapsed();
        let obj = dir.join("program.o");
        let exe = dir.join("program");
        std::fs::write(&obj, &object).unwrap();
        let start = Instant::now();
        let linked = Command::new("cc")
            .args(["-std=c11", "-O0", "-fno-strict-aliasing", if cfg!(target_os = "macos") { "-pthread" } else { "-no-pie" }])
            .arg(&obj)
            .arg("../../runtime/native.c")
            .arg("-lm")
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(linked.status.success(), "{}", String::from_utf8_lossy(&linked.stderr));
        let link = start.elapsed();
        let mut times = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let out = Command::new(&exe).env_remove("TARN_TRACE_DROPS").output().unwrap();
            assert!(out.status.success());
            let expected = if name == "slices" {
                b"1000000\n".as_slice()
            } else if name == "dynamic" {
                b"42000000\n".as_slice()
            } else {
                b"499999500000\n".as_slice()
            };
            assert_eq!(out.stdout, expected);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{name}: codegen_ms={:.3} link_ms={:.3} object_bytes={} executable_bytes={} runtime_median_ms={:.3}",
            codegen.as_secs_f64() * 1000.0,
            link.as_secs_f64() * 1000.0,
            object.len(),
            std::fs::metadata(&exe).unwrap().len(),
            times[2]
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
