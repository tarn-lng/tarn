#!/bin/sh
# Build the kernel and boot it in QEMU. Exits 0 when the kernel reports success.
# Usage: examples/kernel-x86_64/run.sh [path/to/tarn] [kernel.elf]
# Needs qemu-system-x86_64 and an ELF linker (ld.lld, or set TARN_LD).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tarn=${1:-tarn}
elf=${2:-kernel.elf}
"$tarn" build "$here/main.tarn" --target x86_64-none --linker-script "$here/kernel.ld" -o "$elf"
status=0
limit=
if command -v timeout >/dev/null 2>&1; then limit='timeout 30'; fi
# isa-debug-exit: the kernel writes 16 (success) or 17 (failure) to port 0xf4
# and QEMU exits with status value * 2 + 1.
$limit qemu-system-x86_64 -kernel "$elf" -display none -serial stdio -monitor none -no-reboot -m 64 \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 || status=$?
case $status in
    33) echo "kernel reported success" ;;
    124) echo "kernel did not exit within 30 s" >&2; exit 1 ;;
    *) echo "kernel failed (QEMU status $status)" >&2; exit 1 ;;
esac
