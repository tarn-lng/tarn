ENTRY(tarn_arm64_start)
SECTIONS {
    . = 0x40080000;
    __image_start = .;
    .boot : { KEEP(*(.text.arm64.boot)) }
    .text : { *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) *(tarn_strings) }
    .data : { *(.data .data.*) }
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        . += 16384;
        __arm64_stack_top = .;
        __bss_end = .;
    }
    __arm64_image_size = . - __image_start;
    /DISCARD/ : { *(.eh_frame*) *(.comment) *(.note*) }
}
