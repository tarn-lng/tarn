ENTRY(kmain)

SECTIONS {
    . = 0x100000;
    .text : { *(.text.boot) *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.limine_requests) *(.data .data.*) }
    .bss : { *(.bss .bss.*) *(COMMON) }
    /DISCARD/ : { *(.note*) *(.comment*) *(.eh_frame*) }
}
