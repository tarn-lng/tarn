/* ADR 0105: upper-half offset aliases physical [0, 1 GiB).
   Boot code/data stay physical; every other kernel VMA is BASE + LMA. */
ENTRY(tarn_pvh_start)
BASE = 0xffffffff80000000;
PHDRS {
    boot PT_LOAD FLAGS(7);
    note PT_NOTE;
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}
SECTIONS {
    . = 1M;
    __boot_start = .;
    .boot : {
        *(.text.pvh.boot)
        *(.rodata.pvh)
    } :boot
    .note : { KEEP(*(.note.Xen)) } :boot :note
    .boot_bss (NOLOAD) : { *(.bss.pvh) } :boot
    . = ALIGN(2M);
    __boot_end = .;
    __kernel_phys_start = .;
    . = BASE + __kernel_phys_start;
    .text : AT(ADDR(.text) - BASE) { *(.text .text.*) } :text
    .rodata : AT(ADDR(.rodata) - BASE) { *(.rodata .rodata.*) *(tarn_strings) } :text
    . = ALIGN(4096);
    .data : AT(ADDR(.data) - BASE) { *(.data .data.*) } :data
    .bss : AT(ADDR(.bss) - BASE) { *(.bss .bss.*) *(COMMON) } :data
    __kernel_phys_end = . - BASE;
    ASSERT(__boot_end < 0x40000000, "PVH bootstrap must be below 1 GiB")
    ASSERT(__kernel_phys_end <= 0x40000000, "PVH higher-half alias covers physical [0, 1 GiB)")
    /DISCARD/ : { *(.comment*) *(.eh_frame*) }
}
