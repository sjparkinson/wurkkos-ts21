//! The vector table and what runs between reset and `main`.
//!
//! This is the job avr-libc's crt1 does in C land. Doing it here keeps
//! avr-gcc out of the build.

#![allow(unsafe_code)]

use core::arch::global_asm;

// ATtiny1616 has 31 vectors (reset plus 30 interrupts, see the ATDF or the
// datasheet's interrupt vector table). With 16 KB of flash each one is a
// two-word `jmp`. Nothing enables interrupts yet, so every slot but reset
// lands on a bare `reti`.
global_asm!(
    ".section .vectors, \"ax\", @progbits",
    ".global __vectors",
    "__vectors:",
    "  jmp __reset",
    "  .rept 30",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "",
    ".section .text.__unhandled_interrupt, \"ax\", @progbits",
    "__unhandled_interrupt:",
    "  reti",
    "",
    ".section .text.__reset, \"ax\", @progbits",
    ".global __reset",
    "__reset:",
    // r1 is the zero register in the AVR ABI. SREG starts cleared on reset,
    // but say so anyway. SP already points at the top of SRAM.
    "  clr r1",
    "  out 0x3f, r1",
    // Copy .data's initial values from flash (read through the mapped-flash
    // window, see link.x) into SRAM.
    "  ldi r26, lo8(__data_start)",
    "  ldi r27, hi8(__data_start)",
    "  ldi r30, lo8(__data_load)",
    "  ldi r31, hi8(__data_load)",
    "  ldi r24, lo8(__data_end)",
    "  ldi r25, hi8(__data_end)",
    "  rjmp 2f",
    "1:",
    "  ld r0, Z+",
    "  st X+, r0",
    "2:",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 1b",
    // Zero .bss.
    "  ldi r26, lo8(__bss_start)",
    "  ldi r27, hi8(__bss_start)",
    "  ldi r24, lo8(__bss_end)",
    "  ldi r25, hi8(__bss_end)",
    "  rjmp 4f",
    "3:",
    "  st X+, r1",
    "4:",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 3b",
    "  call main",
    "5:",
    "  rjmp 5b",
);
