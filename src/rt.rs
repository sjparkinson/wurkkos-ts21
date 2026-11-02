//! The vector table and what runs between reset and `main`.
//!
//! This is the job avr-libc's crt1 does in C land. Doing it here keeps
//! avr-gcc out of the build.
//!
//! # A short AVR primer for reading this file
//!
//! The CPU has 32 eight-bit registers, `r0` to `r31`. The top six pair up
//! into three 16-bit pointer registers, which the load and store
//! instructions use to address memory.
//!
//! | Pair        | Name |
//! |-------------|------|
//! | `r27:r26`   | X    |
//! | `r29:r28`   | Y    |
//! | `r31:r30`   | Z    |
//!
//! Everything (SRAM, the peripherals' registers, and a read-only copy of
//! flash) lives in one 64 KB "data space", so a pointer is a plain 16-bit
//! number and `ld`/`st` reach all of it. The first 64 addresses are also
//! "I/O space", which the shorter `in`/`out` instructions can reach in one
//! cycle. The CPU's own status register (SREG) sits there.
//!
//! The assembler syntax is GNU `as`. `lo8(sym)` and `hi8(sym)` are the low
//! and high bytes of a symbol's address, filled in by the linker. Numeric
//! labels like `1:` can be reused; `1b` means "the nearest `1:` going
//! backwards" and `2f` "the nearest `2:` going forwards".
//!
//! The symbols `__data_start`, `__data_load` and friends come from `link.x`,
//! which decides where each section ends up in flash and SRAM.

#![allow(unsafe_code)]

use core::arch::global_asm;

use crate::regs;

// `global_asm!` drops raw assembly into the object file, outside any Rust
// function. The `{irqs}` and `{sreg}` placeholders are filled from the
// `const` arguments at the bottom, so the numbers still come from regs.rs.
global_asm!(
    // The vector table. On reset, and whenever an interrupt fires, the CPU
    // jumps to a fixed flash address without looking anything up. Reset
    // goes to address 0, and interrupt n goes to n slots after that. Each
    // slot is only big enough for one jump instruction, so the table is a
    // row of jumps to the real code.
    //
    // The ATtiny1616 has 31 slots (reset plus 30 interrupts,
    // `regs::VECTORS`). With 16 KB of flash each one is a two-word (4-byte)
    // `jmp`, which can reach anywhere in flash.
    //
    // `.section .vectors` puts this in its own section, and link.x pins
    // that section to the very start of flash. "ax" marks it allocated
    // (it takes space in the image) and executable, and @progbits means it
    // has real bytes in the file.
    ".section .vectors, \"ax\", @progbits",
    ".global __vectors",
    "__vectors:",
    // Slot 0 is reset.
    "  jmp __reset",
    // Slots 1 to 30 are the interrupts. `.rept` and `.endr` make the
    // assembler repeat the line between them. Nothing enables interrupts yet, so these can't fire, but if one did
    // it would land on a harmless `reti` rather than random code.
    "  .rept {irqs}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "",
    // `reti` returns from an interrupt handler. It pops the return address
    // the CPU pushed on the way in, and tells the interrupt controller the
    // handler has finished.
    ".section .text.__unhandled_interrupt, \"ax\", @progbits",
    "__unhandled_interrupt:",
    "  reti",
    "",
    // Reset, which gets the chip ready for Rust. `.global` exports the
    // symbol so the linker can see it, and link.x also names `__reset` as
    // the ELF entry point.
    ".section .text.__reset, \"ax\", @progbits",
    ".global __reset",
    "__reset:",
    // The AVR calling convention says r1 always holds zero, and compiled
    // Rust code relies on it, so set that up before calling anything.
    // `clr` is "xor a register with itself".
    //
    // `out` then writes r1 (zero) to SREG, the CPU status register, which
    // holds the flags (carry, zero and so on) and the global interrupt
    // enable bit. It's already zero after reset, but say so anyway.
    //
    // SP, the stack pointer, already points at the top of SRAM. The
    // hardware sets that on reset, so there's nothing to do for it.
    "  clr r1",
    "  out {sreg}, r1",
    // Copy .data into SRAM.
    //
    // .data holds statics with a non-zero starting value, such as
    // `static mut X: u8 = 5`. Rust expects to find them in SRAM, but SRAM
    // is blank at power-up, so the starting values are stored in flash and
    // copied over here. tinyAVR maps flash into data space at 0x8000 (see
    // link.x), so the copy reads it with an ordinary `ld`.
    //
    // `ldi` (load immediate) puts a constant in a register, one byte at a
    // time. X starts at __data_start, the SRAM address to write to. Z starts
    // at __data_load, the flash address to read from. r25:r24 holds
    // __data_end, where to stop.
    "  ldi r26, lo8(__data_start)",
    "  ldi r27, hi8(__data_start)",
    "  ldi r30, lo8(__data_load)",
    "  ldi r31, hi8(__data_load)",
    "  ldi r24, lo8(__data_end)",
    "  ldi r25, hi8(__data_end)",
    // Jump to the test first, so an empty .data copies nothing.
    "  rjmp 2f",
    "1:",
    // Load a byte from Z into r0 and bump Z, then store r0 at X and bump X.
    "  ld r0, Z+",
    "  st X+, r0",
    "2:",
    // Compare X with the end as a 16-bit number. `cp` compares the low
    // bytes, then `cpc` the high bytes, carrying over from the first. The zero
    // flag ends up set only if both bytes matched, and `brne` (branch if not
    // equal) loops back while they differ.
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 1b",
    // Zero .bss.
    //
    // .bss holds statics that start at zero. They take no space in flash,
    // the linker only records how big the block is, so all that's needed is
    // to write zeros over it. Same loop shape as above, storing r1 (always
    // zero) instead of a byte from flash.
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
    // Memory looks how Rust expects it, so hand over to `main` in
    // src/main.rs. `call` pushes a return address onto the stack and jumps.
    "  call main",
    // `main` never returns. If it somehow did, spin here forever rather
    // than run off the end into whatever comes next in flash.
    "5:",
    "  rjmp 5b",
    // Slot 0 is written out by hand above, so repeat the rest.
    irqs = const regs::VECTORS - 1,
    // SREG's I/O address, small enough for `out` to reach.
    sreg = const regs::addr::CPU_SREG,
);
