//! Boots the aarch64 example kernel (ADR 0106, Cairn T5): builds
//! `examples/kernel-aarch64` into a flat arm64 Linux Image (no objcopy),
//! starts it with `qemu-system-aarch64 -M virt -kernel` and checks the serial
//! output and the exit status the kernel reports through semihosting.
//! Skipped, with the reason printed, when QEMU or a linker is missing.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tarn_backend::Target;

const EXAMPLE: &str = "../../examples/kernel-aarch64";

fn qemu() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("TARN_QEMU_AARCH64").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("qemu-system-aarch64")).find(|p| p.is_file())
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("tarn-kernel-a64-{tag}-{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir(&dir).unwrap();
    dir
}

fn tools() -> Option<(PathBuf, PathBuf)> {
    let Some(qemu) = qemu() else {
        eprintln!("skipping: qemu-system-aarch64 not found (install QEMU or set TARN_QEMU_AARCH64)");
        return None;
    };
    let Some(linker) = tarn_backend::link::find_linker("aarch64-none".parse().unwrap()) else {
        eprintln!("skipping: no ELF linker (install lld or set TARN_LD)");
        return None;
    };
    Some((qemu, linker))
}

fn example() -> String {
    std::fs::read_to_string(Path::new(EXAMPLE).join("main.tarn")).unwrap()
}

/// Build `source` into a flat Image.
fn build(dir: &Path, source: &str, linker: &Path) -> PathBuf {
    let main = dir.join("main.tarn");
    std::fs::write(&main, source).unwrap();
    let res = tarn_driver::check(&main).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    let target: Target = "aarch64-none".parse().unwrap();
    let (object, image) = (dir.join("kernel.o"), dir.join("Image"));
    tarn_backend::build_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &object, target).unwrap_or_else(|e| panic!("{e}"));
    let script = Path::new(EXAMPLE).join("kernel.ld");
    tarn_backend::link::run_linker_format(linker, &object, &image, &script, None, true).unwrap_or_else(|e| panic!("{e}"));
    image
}

/// Boot `image`; returns (serial output, exit status).
fn boot(qemu: &Path, image: &Path, machine: &str, cpu: &str) -> (String, i32) {
    let mut child = Command::new(qemu)
        .args(["-M", machine, "-cpu", cpu, "-m", "128", "-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot", "-semihosting", "-kernel"]).arg(image)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("start QEMU");
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stdout.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() { break status; }
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the kernel did not exit within 30 s; serial output so far:\n{}", reader.join().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = reader.join().unwrap().replace("\r\n", "\n");
    eprintln!("QEMU serial ({machine}, {cpu}):\n{output}");
    (output, status.code().unwrap_or(-1))
}

fn check_success(output: &str) {
    check_task_switching(output);
    assert!(output.contains("sp_el0: exception returned on task stack"), "{output}");
    assert!(output.contains("stack overflow: recursive task hit guard page"), "{output}");
    assert!(output.contains("stack overflow: reported on separate SP_EL1 exception stack"), "{output}");
    let lines: Vec<&str> = output.lines().collect();
    let expected = [
        "tarn arm64: booted",
        "dtb magic: 0x00000000d00dfeed",
        "dtb memory bytes: 134217728",
        "svc: handler reached",
        "svc: returned",
        "brk: breakpoint handled",
        "brk: returned",
        "midr_el1: ",
        "mmu: on, identity map, caches on",
        "at s1e1r: ",
        "tlbi: stale entry dropped, other forms run",
        "dc zva: zeroed 64 bytes",
        "sp_el0: round trip ok",
        "call_on_stack: ran on the new stack and returned (twice)",
        "hello, heap world",
        "vec of 1000 squares sums to 332833500",
        "heap ok: ",
        "10 timer interrupts",
        "ticks ok",
        "tarn arm64: done",
    ];
    let mut at = 0;
    for want in expected {
        let found = lines[at..].iter().position(|l| l.starts_with(want));
        let Some(i) = found else { panic!("missing line `{want}` in\n{output}") };
        at += i + 1;
    }
}

#[test]
fn the_aarch64_image_header_is_valid() {
    let Some((_, linker)) = tools() else { return };
    let dir = scratch("header");
    let image = build(&dir, &example(), &linker);
    let bytes = std::fs::read(&image).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(&bytes[56..60], b"ARM\x64");
    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()) >> 26, 0b000101, "code0 is a branch");
    assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 0x80000, "text_offset");
    let size = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    assert!(size >= bytes.len() as u64, "image_size covers the file and .bss");
    assert_eq!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()) & 1, 0, "little endian");
}

#[test]
fn the_aarch64_example_kernel_boots_at_el1() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("el1");
    let image = build(&dir, &example(), &linker);
    let (output, status) = boot(&qemu, &image, "virt,gic-version=3", "cortex-a72");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 0, "{output}");
    check_success(&output);
}

#[test]
fn the_aarch64_example_kernel_drops_from_el2_to_el1() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("el2");
    let image = build(&dir, &example(), &linker);
    let (output, status) = boot(&qemu, &image, "virt,gic-version=3,virtualization=on", "cortex-a72");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 0, "{output}");
    check_success(&output);
}

#[test]
fn a_panic_reports_failure_through_semihosting() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("panic");
    let source = example().replace("println(\"ticks ok\")", "panic(\"boom\")");
    assert_ne!(source, example());
    let image = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &image, "virt,gic-version=3", "cortex-a72");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 1, "{output}");
    assert!(output.contains("PANIC: boom"), "{output}");
}

#[test]
fn frame_x_rejects_registers_above_x30() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("frame-x");
    let source = example().replace("arch_aarch64.frame_x(frame, 30) == 0", "arch_aarch64.frame_x(frame, 31) == 0");
    assert_ne!(source, example());
    let image = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &image, "virt,gic-version=3", "cortex-a72");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 1, "{output}");
    assert!(output.contains("PANIC: frame_x: register index out of range 0..=30"), "{output}");
}

fn check_task_switching(output: &str) {
    let tasks: Vec<_> = output.lines().filter(|l| l.starts_with("coop: task ") || l.starts_with("preempt: task ")).collect();
    assert_eq!(tasks, ["coop: task 1 turn 0", "coop: task 2 turn 0", "coop: task 1 turn 1", "coop: task 2 turn 1", "coop: task 1 turn 2", "coop: task 2 turn 2",
        "preempt: task 1", "preempt: task 2", "preempt: task 1", "preempt: task 2", "preempt: task 1", "preempt: task 2"], "{output}");
    assert!(output.contains("switch_stack: entered new stack without return"), "{output}");
    assert!(output.contains("coop: contexts restored"), "{output}");
    assert!(output.contains("preempt: timer switched both busy tasks"), "{output}");
}

#[test]
fn contexts_pan_and_exception_stack_execute_on_a_max_cpu() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("contexts-pan");
    let image = build(&dir, &example(), &linker);
    let (output, status) = boot(&qemu, &image, "virt,gic-version=3", "max");
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(status, 0, "{output}");
    check_success(&output);
    assert!(output.contains("pan: protected/open/protected window ok"), "{output}");
}
