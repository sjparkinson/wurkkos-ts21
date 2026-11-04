/* Flash at 0. The AVR ELF convention puts data space at 0x800000, so flash
   mapped into data space at 0x8000 is 0x808000 here. RAM holds only the stack:
   the firmware has no statics that change, and the asserts below keep it so. */
MEMORY
{
  FLASH (rx) : ORIGIN = 0, LENGTH = 16K
  MAPPED (r) : ORIGIN = 0x808000, LENGTH = 16K
}

ENTRY(reset)

SECTIONS
{
  .text : { KEEP(*(.vectors)) *(.text .text.*) . = ALIGN(2); } > FLASH

  .rodata (0x808000 + SIZEOF(.text)) : AT(SIZEOF(.text))
  {
    *(.rodata .rodata.*)
  } > MAPPED

  .data : { *(.data .data.*) } > MAPPED
  .bss (NOLOAD) : { *(.bss .bss.*) } > MAPPED

  /DISCARD/ : { *(.note* .comment* .eh_frame*) }
}

ASSERT(SIZEOF(.data) == 0, "no .data: nothing copies it at reset")
ASSERT(SIZEOF(.bss) == 0, "no .bss: nothing clears it at reset")
