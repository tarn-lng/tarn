//! Boots the example kernel (ADR 0101, P8): builds `examples/kernel-x86_64`,
//! links it with an ELF linker, starts it in QEMU's PVH loader and checks the
//! serial output and the exit status the kernel reports through
//! `isa-debug-exit`. Skipped, with the reason printed, when QEMU or a linker
//! is missing (CI installs both).
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tarn_backend::Target;

const EXAMPLE: &str = "../../examples/kernel-x86_64";
/// `isa-debug-exit` status for a write of 16 and 17 (`value * 2 + 1`).
const EXIT_OK: i32 = 33;
const EXIT_FAILED: i32 = 35;

fn qemu() -> Option<PathBuf> {
    let name = "qemu-system-x86_64";
    if let Some(explicit) = std::env::var_os("TARN_QEMU").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("tarn-kernel-boot-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build `source` (kernel text) into an ELF with the example's linker script.
fn build(dir: &Path, source: &str, linker: &Path) -> PathBuf {
    let main = dir.join("main.tarn");
    std::fs::write(&main, source).unwrap();
    let res = tarn_driver::check(&main).unwrap();
    assert!(!res.has_errors(), "{:?}", res.diagnostics);
    let target: Target = "x86_64-none".parse().unwrap();
    let (object, elf) = (dir.join("kernel.o"), dir.join("kernel.elf"));
    tarn_backend::build_object(res.drops.as_ref().unwrap(), res.typed.as_ref().unwrap(), &object, target).unwrap_or_else(|e| panic!("{e}"));
    tarn_backend::link::run_linker(linker, &object, &elf, &Path::new(EXAMPLE).join("kernel.ld"), None).unwrap_or_else(|e| panic!("{e}"));
    elf
}

/// Boot `elf`; returns (serial output, exit status). Fails the test on timeout.
fn boot(qemu: &Path, elf: &Path) -> (String, i32) {
    let mut child = Command::new(qemu)
        .args(["-kernel"]).arg(elf)
        .args(["-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot", "-m", "64"])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
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
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            let output = reader.join().unwrap();
            panic!("the kernel did not exit within 20 s; serial output so far:\n{output}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (reader.join().unwrap().replace("\r\n", "\n"), status.code().unwrap_or(-1))
}

/// The tools a boot test needs, or the reason to skip.
fn tools() -> Option<(PathBuf, PathBuf)> {
    let Some(qemu) = qemu() else {
        eprintln!("skipping: qemu-system-x86_64 not found (install QEMU or set TARN_QEMU)");
        return None;
    };
    let Some(linker) = tarn_backend::link::find_linker("x86_64-none".parse().unwrap()) else {
        eprintln!("skipping: no ELF linker (install lld or set TARN_LD)");
        return None;
    };
    Some((qemu, linker))
}

fn example() -> String {
    std::fs::read_to_string(Path::new(EXAMPLE).join("main.tarn")).unwrap()
}

#[test]
fn the_example_kernel_boots_and_reports_success() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("ok");
    // int3 and the PIT exercise trampolines read through checked raw-array
    // projection from the static HANDLERS
    // table: a relocation to an ordinary body cannot return from an interrupt.
    let elf = build(&dir, &example(), &linker);
    let (output, status) = boot(&qemu, &elf);
    let _ = std::fs::remove_dir_all(&dir);
    let lines: Vec<&str> = output.lines().collect();
    let expected = [
        "tarn kernel: booted",
        "pvh start info: magic ok",
        "hello, heap world",
        "vec of 1000 squares sums to 332833500",
        "heap ok: ",
        "int3: breakpoint handled",
        "10 timer interrupts",
        "ticks ok",
        "tarn kernel: done",
    ];
    // The heap demo runs twice; every expected line appears, in order.
    let mut at = 0;
    for want in expected {
        let Some(found) = lines[at..].iter().position(|l| l.starts_with(want)) else { panic!("missing `{want}` after line {at}; serial output:\n{output}") };
        at += found + 1;
    }
    assert_eq!(status, EXIT_OK, "exit status; serial output:\n{output}");
}

#[test]
fn a_panic_is_printed_and_ends_the_machine_with_failure() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("panic");
    let source = example().replace("    heap_reuse()\n", "    panic(\"test panic\")\n");
    assert!(source.contains("panic(\"test panic\")"));
    let elf = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &elf);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.contains("PANIC: test panic at "), "{output}");
    assert_eq!(status, EXIT_FAILED, "{output}");
}

#[test]
fn a_page_fault_reaches_the_handler_with_its_error_code() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("fault");
    // 0x80000000 is above the 1 GiB the boot code identity-maps.
    let source = example().replace("    heap_reuse()\n", "    install_idt()\n    unmapped: *u64 := ffi.from_address(0x80000000)\n    unsafe { print_number(ffi.read(unmapped)) }\n");
    let elf = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &elf);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.contains("cr2=0x0000000080000000"), "{output}");
    assert!(output.contains("EXCEPTION page fault code=0x0000000000000000"), "{output}");
    assert_eq!(status, EXIT_FAILED, "{output}");
}

#[test]
fn a_failed_bounds_check_calls_the_fault_handler() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("bounds");
    let source = example().replace("    heap_reuse()\n", "    var empty: Vec<i64> = Vec.new()\n    print_number(u64(empty.at(3)))\n");
    let elf = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &elf);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.contains("FAULT: "), "{output}");
    assert_eq!(status, EXIT_FAILED, "{output}");
}

#[test]
fn inline_cr3_tlb_and_port_sequences_execute_in_ring_zero() {
    let Some((qemu, linker)) = tools() else { return };
    let dir = scratch("inline-cpu");
    // Reload the current table, invalidate an identity-mapped address, and
    // exercise all port widths on the POST delay port (no serial side effect).
    let source = example().replacen("    heap_reuse()\n", "    unsafe {\n        cr3 := arch_x86_64.read_cr3()\n        arch_x86_64.write_cr3(cr3)\n        arch_x86_64.invlpg(4096)\n        arch_x86_64.out8(128, 0)\n        arch_x86_64.out16(128, 0)\n        arch_x86_64.out32(128, 0)\n        byte := arch_x86_64.in8(128)\n        half := arch_x86_64.in16(128)\n        word := arch_x86_64.in32(128)\n    }\n    heap_reuse()\n", 1);
    assert!(source.contains("arch_x86_64.invlpg(4096)"));
    let elf = build(&dir, &source, &linker);
    let (output, status) = boot(&qemu, &elf);
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(status, EXIT_OK, "{output}");
    assert!(output.contains("heap"), "{output}");
}

/// Little-endian reads for the program header walk below.
fn u16_at(b: &[u8], at: usize) -> usize { u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize }
fn u32_at(b: &[u8], at: usize) -> u32 { u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) }
fn u64_at(b: &[u8], at: usize) -> u64 { u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) }

#[test]
fn the_image_carries_a_pvh_note_naming_the_32_bit_entry() {
    let Some(linker) = tarn_backend::link::find_linker("x86_64-none".parse().unwrap()) else {
        eprintln!("skipping: no ELF linker (install lld or set TARN_LD)");
        return;
    };
    let dir = scratch("note");
    let elf = build(&dir, &example(), &linker);
    let bytes = std::fs::read(&elf).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let entry = u64_at(&bytes, 24);
    let (phoff, phentsize, phnum) = (u64_at(&bytes, 32) as usize, u16_at(&bytes, 54), u16_at(&bytes, 56));
    let mut notes = 0;
    let mut loads = Vec::new();
    for i in 0..phnum {
        let h = phoff + i * phentsize;
        let (kind, offset, filesz) = (u32_at(&bytes, h), u64_at(&bytes, h + 8) as usize, u64_at(&bytes, h + 32) as usize);
        match kind {
            1 => loads.push((u64_at(&bytes, h + 24), u64_at(&bytes, h + 40))),
            4 => {
                notes += 1;
                let note = &bytes[offset..offset + filesz];
                let (namesz, descsz, kind) = (u32_at(note, 0), u32_at(note, 4), u32_at(note, 8));
                assert_eq!((namesz, kind), (4, 18), "XEN_ELFNOTE_PHYS32_ENTRY");
                assert_eq!(&note[12..16], b"Xen\0");
                assert!(descsz >= 4);
                // The entry address: below 4 GiB, and the ELF entry (`tarn_pvh_start`) itself.
                assert_eq!(u64_at(note, 16) & 0xffff_ffff, entry & 0xffff_ffff);
                assert!(entry < 1 << 32);
            }
            _ => {}
        }
    }
    assert_eq!(notes, 1, "one PT_NOTE");
    assert!(loads.iter().any(|(paddr, size)| *paddr <= entry && entry < paddr + size), "entry is inside a loaded segment: {loads:?}");
}
