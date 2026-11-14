#!/bin/sh
# Build the kernel into an arm64 Image and boot it on QEMU virt.
# Exits 0 when the kernel reports success (semihosting exit status 0).
# Usage: examples/kernel-aarch64/run.sh [path/to/tarn] [Image]
# Needs qemu-system-aarch64 and an ELF linker (ld.lld, or set TARN_LD).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tarn=${1:-tarn}
image=${2:-Image}
"$tarn" build "$here/main.tarn" --target aarch64-none --linker-script "$here/kernel.ld" --binary -o "$image"
status=0
limit=
if command -v timeout >/dev/null 2>&1; then limit='timeout 30'; fi
$limit qemu-system-aarch64 -M virt,gic-version=3 -cpu cortex-a72 -m 128 -nographic -semihosting \
    -no-reboot -kernel "$image" || status=$?
case $status in
    0) echo "kernel reported success" ;;
    124) echo "kernel did not exit within 30 s" >&2; exit 1 ;;
    *) echo "kernel failed (QEMU status $status)" >&2; exit 1 ;;
esac
