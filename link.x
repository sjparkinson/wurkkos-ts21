/*
 * ATtiny1616: 16 KB flash, 2 KB SRAM, 256 B EEPROM.
 *
 * AVR ELF files put data space at 0x800000, so SRAM (data address 0x3800)
 * is 0x803800 here. tinyAVR 1-series also maps all of flash into data space
 * at 0x8000, so read-only data is linked at its flash address + 0x808000 and
 * read with ordinary loads, no progmem dance needed.
 */
MEMORY {
  FLASH  (rx)   : ORIGIN = 0x000000, LENGTH = 16K
  MAPPED (r)    : ORIGIN = 0x808000, LENGTH = 16K
  RAM    (rw!x) : ORIGIN = 0x803800, LENGTH = 2K
}

ENTRY(__reset)

SECTIONS {
  .text : {
    KEEP(*(.vectors))
    *(.text .text.*)
  } > FLASH

  .rodata (ADDR(.text) + SIZEOF(.text) + 0x808000) : AT(ADDR(.text) + SIZEOF(.text)) {
    *(.rodata .rodata.*)
  } > MAPPED

  .data : AT(LOADADDR(.rodata) + SIZEOF(.rodata)) {
    __data_start = .;
    *(.data .data.*)
    . = ALIGN(2);
    __data_end = .;
  } > RAM

  /* Where src/rt.rs copies .data from, as a mapped-flash data address. */
  __data_load = LOADADDR(.data) + 0x8000;

  /*
   * What has to survive a reset. src/rt.rs doesn't touch it, and only runs
   * its RAM test on a power-on or UPDI reset, so nothing else does either.
   * It goes before .bss so the stack checks still see .bss's end as the top
   * of everything that isn't stack.
   */
  .noinit (NOLOAD) : {
    __noinit_start = .;
    *(.noinit .noinit.*)
    __noinit_end = .;
  } > RAM

  .bss (NOLOAD) : {
    __bss_start = .;
    *(.bss .bss.*)
    . = ALIGN(2);
    __bss_end = .;
  } > RAM

  /* The stack grows down from the top of SRAM, where the CPU starts SP. */

  /DISCARD/ : {
    *(.eh_frame*)
  }
}
