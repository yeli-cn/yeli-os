/* Ref: https://sourceware.org/binutils/docs/ld/Scripts.html */

OUTPUT_ARCH(riscv)
ENTRY(_entry)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        *(.text.entry)

        . = ALIGN(4096);
        *(.text .text.*)

        . = ALIGN(4096);
        _trampoline = .;
        *(trampoline)

        . = ALIGN(4096);
        /* ASSERT( . - _trampoline == 4096, "trampoline larger then one page.") */
        ASSERT( . - _trampoline <= 4096, "trampoline larger then one page.")
        PROVIDE(etext = .);
    }

    .rodata : {
        . = ALIGN(16);
        *(.rodata.*)
    }

    .data : {
        . = ALIGN(16);
        *(.data*)
    }

    .bss : {
        . = ALIGN(16);
        *(.sbss .bss .bss.*)
    }

    . = ALIGN(4096);
    PROVIDE(end = .);
}