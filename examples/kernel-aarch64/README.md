# aarch64 kernel in Tarn

A small kernel for QEMU `virt` (ADR 0106). It uses `#[arm64_entry]` (Linux arm64
Image header, EL2 to EL1 drop, FP/SIMD on, `.bss` zeroed, x0 = DTB), PL011 serial,
a minimal FDT parser (magic and memory size), the P7 vector table (`svc`, `brk`),
the heap through the allocator contract (Vec and strings), and ten GICv3
generic-timer interrupts. It ends QEMU through semihosting.

```sh
tarn build examples/kernel-aarch64/main.tarn --target aarch64-none \
    --linker-script examples/kernel-aarch64/kernel.ld --binary -o Image
qemu-system-aarch64 -M virt,gic-version=3 -cpu cortex-a72 -m 128 -nographic -semihosting -kernel Image
```

`examples/kernel-aarch64/run.sh` does both. Needs `qemu-system-aarch64` and an ELF
linker (`ld.lld`, `TARN_LD`, or the one in the Rust sysroot). `-M virt` alone is
GICv2; the kernel needs `gic-version=3`. Add `virtualization=on` to start it at EL2.
