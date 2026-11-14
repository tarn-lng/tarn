# Higher-half PVH kernel

This small kernel links ordinary code and data at virtual offset
`0xffffffff80000000` from their physical load addresses. The physical boot
stub maps the first 4 GiB identically and aliases physical [0, 1 GiB) at that
offset. See [ADR 0105](../../docs/adr/0105-configurable-pvh-hand-off.md) for
the versioned physical boot descriptor and memory reclamation obligations.

From the repository root, with an existing QEMU and ELF linker:

```sh
cargo run -p tarn -- build examples/kernel-higher-half/main.tarn \
  --target x86_64-none --linker-script examples/kernel-higher-half/kernel.ld \
  -o /tmp/tarn-higher-half.elf
qemu-system-x86_64 -kernel /tmp/tarn-higher-half.elf -m 64 \
  -display none -serial stdio -monitor none -no-reboot \
  -device isa-debug-exit,iobase=0xf4,iosize=0x04
```

Expected output includes `higher half: entry=0xffffffff80200000`, physical
start-info/table/stack addresses, and `pvh hand-off: magic and descriptor ok`.
The successful QEMU exit status is 33. The example does not replace CR3 or
reclaim bootstrap memory; it checks that the descriptor describes the active
CR3 and allocation sizes. It needs no heap and supplies a no-op `tarn_free`
for the compiler's static-string drop helper.

Keep `BASE` in the script equal to the attribute's `higher_half` offset.
Collect `.text.pvh.boot`, `.rodata.pvh`, `.bss.pvh` and `.note.Xen` at physical
VMAs before the high sections. The `AT()` expressions preserve low physical
load addresses; linker assertions enforce the 1 GiB physical image bound.
