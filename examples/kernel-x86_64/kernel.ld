/* Linker script for the x86_64 example kernel (ADR 0101, P8).
   Loaded at 1 MiB by a PVH loader (QEMU's -kernel). `tarn_pvh_start` is the
   32-bit entry the compiler generates for `#[pvh_entry]`; the loader finds it
   through the `.note.Xen` note, which must live in a PT_NOTE segment. */
ENTRY(tarn_pvh_start)

PHDRS {
    text PT_LOAD FLAGS(5);     /* r-x */
    data PT_LOAD FLAGS(6);     /* rw- (and .bss) */
    note PT_NOTE;
}

SECTIONS {
    . = 1M;
    .text : { *(.text .text.*) } :text
    .rodata : { *(.rodata .rodata.*) } :text
    .note : { KEEP(*(.note.Xen)) } :text :note
    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data
    /DISCARD/ : { *(.comment*) *(.eh_frame*) }
}
