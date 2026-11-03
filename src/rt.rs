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

// The table below lists the handled vectors in this order, with runs of
// unhandled slots between them.
const _: () = assert!(
    0 < regs::VECTOR_PORTA_PORT
        && regs::VECTOR_PORTA_PORT < regs::VECTOR_RTC_PIT
        && regs::VECTOR_RTC_PIT < regs::VECTORS
);

// `global_asm!` drops raw assembly into the object file, outside any Rust
// function. The `{...}` placeholders are filled from the arguments at the
// bottom, so the numbers still come from regs.rs and the handlers' names
// from hw.rs.
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
    // assembler repeat the line between them. Two have handlers in hw.rs,
    // PORTA_PORT (the switch) and RTC_PIT (the tick). The rest go to
    // `__unhandled_interrupt`. Nothing enables them, so they can't fire.
    "  .rept {before_porta}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "  jmp {porta_port}",
    "  .rept {before_pit}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "  jmp {rtc_pit}",
    "  .rept {after_pit}",
    "  jmp __unhandled_interrupt",
    "  .endr",
    "",
    // `reti` returns from an interrupt handler. It pops the return address
    // the CPU pushed on the way in, and tells the interrupt controller the
    // handler has finished. If an interrupt with no handler ever did fire,
    // this would return with its flag still up, and it would fire again,
    // forever. The simulator treats that as an error.
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
    // The FET and linear pins are inputs after reset, so their gates float
    // until `hw::init` drives them. That used to be microseconds; the
    // self-tests below take tens of milliseconds, and a failure stops here
    // for good. So drive both low first. PORTB.OUT is 0 from reset, and
    // `sbi` (set bit in I/O register) needs no CPU register, which hasn't
    // been tested yet. VPORTB.DIR sits low enough in I/O space for it.
    "  sbi {vportb_dir}, {fet}",
    "  sbi {vportb_dir}, {linear}",
    //
    // # The CPU register test (AN2632 §3.1 and §5)
    //
    // Each register gets 0x55 and then 0xAA, which flips every bit both
    // ways, and is read back after each. That finds a stuck bit, and two
    // bits in one register that short together. It doesn't look for faults
    // between registers, and neither does AN2632.
    //
    // A failure jumps to `__selftest_failed`, which does nothing, forever.
    // Rust code can't run safely on a broken register file, so there's no
    // gentler way out (AN2632 §5: "if an error is found no functions should
    // be activated"). The branches lean on SREG's Z flag before SREG is
    // tested; there's no way round that.
    //
    // `or_fail` follows each compare. A conditional branch only reaches 64
    // words, so it hops over an `rjmp`, which reaches all of flash. Its label
    // is 9 so it can't clash with the loops' labels further down.
    ".macro or_fail",
    "  breq 9f",
    "  rjmp __selftest_failed",
    "9:",
    ".endm",
    // r16 to r31 can take an immediate: load it, compare against it.
    // `.irp` repeats the block once per number, with `\\reg` replaced.
    ".irp reg, 16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
    "  ldi r\\reg, {a}",
    "  cpi r\\reg, {a}",
    "  or_fail",
    "  ldi r\\reg, {b}",
    "  cpi r\\reg, {b}",
    "  or_fail",
    ".endr",
    // r0 to r15 can't, so the value goes through r16, which passed above.
    // r16 gets the opposite pattern before it reads the register back, so
    // a register that reads as r16 doesn't pass by echoing it.
    ".irp reg, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15",
    "  ldi r16, {a}",
    "  mov r\\reg, r16",
    "  ldi r16, {b}",
    "  mov r16, r\\reg",
    "  cpi r16, {a}",
    "  or_fail",
    "  com r16",
    "  mov r\\reg, r16",
    "  ldi r16, {a}",
    "  mov r16, r\\reg",
    "  cpi r16, {b}",
    "  or_fail",
    ".endr",
    // SREG, with the same patterns minus the interrupt enable, so nothing
    // could fire mid-test. Read back into r17 before the compare, since
    // `cpi` itself changes SREG.
    "  ldi r16, {sreg_a}",
    "  out {sreg}, r16",
    "  in r17, {sreg}",
    "  cpi r17, {sreg_a}",
    "  or_fail",
    "  ldi r16, {sreg_b}",
    "  out {sreg}, r16",
    "  in r17, {sreg}",
    "  cpi r17, {sreg_b}",
    "  or_fail",
    // The stack pointer, which nothing's using yet. Only the bits SRAM
    // needs are certain to exist: the datasheet says unused ones read 0
    // without saying which they are (DS40002204A §8.7.2), so SPH's patterns
    // keep to those. Then put it back where reset left it, at the top of
    // SRAM.
    "  ldi r16, {a}",
    "  out {spl}, r16",
    "  ldi r16, {sph_b}",
    "  out {sph}, r16",
    "  in r17, {spl}",
    "  cpi r17, {a}",
    "  or_fail",
    "  in r17, {sph}",
    "  cpi r17, {sph_b}",
    "  or_fail",
    "  ldi r16, {b}",
    "  out {spl}, r16",
    "  ldi r16, {sph_a}",
    "  out {sph}, r16",
    "  in r17, {spl}",
    "  cpi r17, {b}",
    "  or_fail",
    "  in r17, {sph}",
    "  cpi r17, {sph_a}",
    "  or_fail",
    "  ldi r16, lo8({stack_top})",
    "  out {spl}, r16",
    "  ldi r16, hi8({stack_top})",
    "  out {sph}, r16",
    //
    // # The RAM test (AN2632 §9.2)
    //
    // March X over all of SRAM. In the usual notation, where ⇑ is up
    // through the addresses, ⇓ down and ⇕ either way:
    //
    //   ⇕(wD); ⇑(rD, w~D); ⇓(r~D, wD); ⇕(rD)
    //
    // Write D everywhere. Then going up, check each byte still holds D and
    // write its complement. Then going down, check for the complement and
    // write D back. Then check it all holds D. Every bit is read back
    // after going each way, so a stuck or sluggish one shows. An address
    // that reaches another byte shows when the second of the two is read
    // with the first's write in it. Going down as well as up finds some
    // coupling, where a write to one byte disturbs another: whichever way
    // round they are, one pass reads the victim after the culprit's write.
    //
    // The test is destructive, which is why it runs before .data and .bss
    // exist and before anything's on the stack. AN2632 tests SRAM in
    // sections, saving each one first, so it can run while the application
    // does. That's not needed at power-on.
    //
    // AN2632 runs it with D = 0x00, which can't tell two bits in a byte
    // apart, since they always hold the same value. Its option for that is
    // the backgrounds 0x55, 0xAA, 0x33, 0xCC, 0x0F and 0xF0. This runs the
    // whole march once for each of 0x00, 0x55, 0x33 and 0x0F, and the w~D
    // steps write the other half, so every pair of bits in a byte is seen
    // both equal and different. The backgrounds all have bit 7 clear, so
    // that bit is only ever checked high by the ⇓ pass; the simulator finds
    // a stuck one slipping through if that check goes.
    //
    // It's 24 instructions a byte for each background, about 200,000 in
    // all, and by the instruction set manual's AVRxt timings about 280,000
    // cycles. That's around 85 ms at the 3.3 MHz the chip runs at until
    // `hw::init` (DS40002204A §10.3.3). Not measured on a real chip.
    //
    // r20 to r23 hold the backgrounds still to come, r19 how many, r16 D
    // and r17 its complement. X walks SRAM, r25:r24 is where it stops, and
    // r0 holds each byte read back.
    "  ldi r20, 0x00",
    "  ldi r21, 0x55",
    "  ldi r22, 0x33",
    "  ldi r23, 0x0F",
    "  ldi r19, 4",
    "5:",
    "  mov r16, r20",
    "  mov r17, r20",
    "  com r17",
    // ⇕(wD), going up.
    "  ldi r26, lo8({sram_start})",
    "  ldi r27, hi8({sram_start})",
    "  ldi r24, lo8({sram_end})",
    "  ldi r25, hi8({sram_end})",
    "1:",
    "  st X+, r16",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 1b",
    // ⇑(rD, w~D). `ld r0, X` reads without moving X; the store moves it on.
    "  ldi r26, lo8({sram_start})",
    "  ldi r27, hi8({sram_start})",
    "2:",
    "  ld r0, X",
    "  cp r0, r16",
    "  or_fail",
    "  st X+, r17",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 2b",
    // ⇓(r~D, wD), from the top. X is at the end of SRAM, and `ld r0, -X`
    // steps it back before reading, so it stops on the first byte.
    "  ldi r24, lo8({sram_start})",
    "  ldi r25, hi8({sram_start})",
    "3:",
    "  ld r0, -X",
    "  cp r0, r17",
    "  or_fail",
    "  st X, r16",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 3b",
    // ⇕(rD), going up from the first byte again.
    "  ldi r24, lo8({sram_end})",
    "  ldi r25, hi8({sram_end})",
    "4:",
    "  ld r0, X+",
    "  cp r0, r16",
    "  or_fail",
    "  cp r26, r24",
    "  cpc r27, r25",
    "  brne 4b",
    // Shift the next background down and go again, until there are none.
    // The loop is 45 words, inside `brne`'s 64; the assembler refuses it
    // if it ever grows past that.
    "  mov r20, r21",
    "  mov r21, r22",
    "  mov r22, r23",
    "  dec r19",
    "  brne 5b",
    // The registers and SRAM passed. xtask's simulator stops here to look
    // at what the tests did, and finds the .data and .bss setup after it.
    ".global __ram_tested",
    "__ram_tested:",
    // The AVR calling convention says r1 always holds zero, and compiled
    // Rust code relies on it, so set that up before calling anything.
    // `clr` is "xor a register with itself".
    //
    // `out` then writes r1 (zero) to SREG, the CPU status register, which
    // holds the flags (carry, zero and so on) and the global interrupt
    // enable bit. The register test above left its last pattern in it, so
    // this clears that.
    //
    // SP, the stack pointer, already points at the top of SRAM: the
    // register test put it back where reset left it.
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
    "",
    // Where a failed register or RAM test ends up: the FET and linear pins
    // low, interrupts off, and the watchdog off, since nothing has started
    // it and the factory WDTCFG fuse doesn't (a fuse that did would reset
    // the chip into the tests again, which stays just as dark), so nothing
    // happens until the cell comes out. The button LED stays
    // dark too, since only `hw::init` turns its pull-up on, which is how to
    // tell this from a torch that's merely off.
    ".global __selftest_failed",
    "__selftest_failed:",
    "  rjmp __selftest_failed",
    // The unhandled slots: from 1 up to PORTA_PORT, between it and
    // RTC_PIT, and from there to the end. Slot 0 is reset.
    before_porta = const regs::VECTOR_PORTA_PORT - 1,
    porta_port = sym crate::hw::porta_port,
    before_pit = const regs::VECTOR_RTC_PIT - regs::VECTOR_PORTA_PORT - 1,
    rtc_pit = sym crate::hw::rtc_pit,
    after_pit = const regs::VECTORS - regs::VECTOR_RTC_PIT - 1,
    // SREG's I/O address, small enough for `out` to reach.
    sreg = const regs::addr::CPU_SREG,
    // The pins to hold low, and their direction register.
    vportb_dir = const regs::addr::VPORTB_DIR,
    fet = const crate::hw::FET,
    linear = const crate::hw::LINEAR,
    // The register test's patterns, and the stack pointer's home.
    a = const PATTERN_A,
    b = const PATTERN_B,
    sreg_a = const PATTERN_A & !regs::CPU_SREG_I,
    sreg_b = const PATTERN_B & !regs::CPU_SREG_I,
    sph_a = const PATTERN_A & SPH_BITS,
    sph_b = const PATTERN_B & SPH_BITS,
    spl = const regs::addr::CPU_SPL,
    sph = const regs::addr::CPU_SPH,
    stack_top = const regs::addr::SRAM_END - 1,
    // What the RAM test walks.
    sram_start = const regs::addr::SRAM_START,
    sram_end = const regs::addr::SRAM_END,
);

/// The register test writes this and its complement into every register,
/// so each bit goes both ways.
const PATTERN_A: u8 = 0x55;
const PATTERN_B: u8 = !PATTERN_A;

/// The bits of SPH that must exist for SP to reach the top of SRAM, 0x3F
/// for 0x3FFF. Any above might read 0 (DS40002204A §8.7.2).
const SPH_BITS: u8 = ((regs::addr::SRAM_END.next_power_of_two() - 1) >> 8) as u8;

// `in` and `out` only reach the first 64 I/O addresses, and `sbi` the first
// 32.
const _: () = assert!(
    regs::addr::CPU_SPL < 0x40 && regs::addr::CPU_SPH < 0x40 && regs::addr::VPORTB_DIR < 0x20
);
