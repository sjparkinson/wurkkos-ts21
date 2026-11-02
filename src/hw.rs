//! The TS21 driver's pins and the ATtiny1616 registers that drive them.
//!
//! No PAC: the handful of registers we touch are written out by hand in
//! `src/regs.rs`, and xtask's tests check them against Microchip's ATDF.
//! docs/hardware.md has the pin map and where it comes from.
//!
//! | Pin | Job                                   |
//! |-----|---------------------------------------|
//! | PA5 | Side switch, active low, pulled up    |
//! | PB0 | FET channel, TCA0 WO0                 |
//! | PB1 | Linear (7135) channel, TCA0 WO1       |
//! | PB5 | Aux / button LED                      |

#![allow(unsafe_code)]

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

use ts21::ramp::{Output, PWM_TOP};

use crate::regs::{self, addr as reg};

const SWITCH: u8 = 5; // PA5
const FET: u8 = 0; // PB0
const LINEAR: u8 = 1; // PB1
const AUX: u8 = 5; // PB5

/// The PORTB pins we drive. PORTB has PB0 to PB5 on the 20-pin package.
const PORTB_OUTPUTS: u8 = 1 << FET | 1 << LINEAR | 1 << AUX;
const PORTB_PINS: u8 = 6;

// Every peripheral (the ports, the timers, the clock controller) exposes
// its settings as bytes at fixed addresses, in the same address space as
// SRAM. Storing a byte at `VPORTB_DIR` doesn't put it in memory, it flips
// the direction of PORTB's pins. Loading from `VPORTA_IN` reads the voltage on PORTA's pins right
// now. These two helpers are the only way the rest of the file does it.
//
// They're `volatile` because the compiler would otherwise treat these as
// ordinary memory, and could drop a write nobody reads back or read once
// and reuse the value in a loop. `read_volatile` and `write_volatile`
// promise each access happens, in order, exactly as written.

fn write(addr: usize, value: u8) {
    // SAFETY: every caller passes one of the `reg` constants, which are
    // memory-mapped I/O registers on this part.
    unsafe { write_volatile(addr as *mut u8, value) }
}

fn read(addr: usize) -> u8 {
    // SAFETY: as for `write`.
    unsafe { read_volatile(addr as *const u8) }
}

/// 16-bit peripheral registers go through a shared TEMP byte: write the low
/// byte first, then the high byte. Spelling it out keeps the order from
/// depending on the compiler.
///
/// The CPU only moves 8 bits at a time, so a 16-bit value takes two
/// stores. If the timer saw the halves land separately it could briefly
/// run with a new low byte and an old high byte. Instead, the low-byte
/// write is parked in TEMP, and the high-byte write copies both into the
/// register in the same clock cycle.
fn write16(addr: usize, value: u16) {
    let [lo, hi] = value.to_le_bytes();
    write(addr, lo);
    write(addr + 1, hi);
}

/// Writes a register behind configuration change protection. The CPU only
/// accepts the write within four instructions of the CCP key, so it has to
/// be assembly.
///
/// Some registers, like the clock settings, could brick a running program
/// if a stray pointer hit them, so the chip guards them. Writing the magic
/// value 0xD8 (`CPU_CCP_IOREG`) to the CCP register unlocks them for the
/// next four instructions, and interrupts are held off for that window.
/// Rust gives no guarantee about how many instructions it puts between two
/// `write` calls, so the pair is written out by hand.
fn protected_write(addr: usize, value: u8) {
    // SAFETY: writes the IOREG key to CPU.CCP, then `value` to `addr`.
    // Touches no memory Rust knows about.
    unsafe {
        asm!(
            // Unlock. `out` writes a register to an I/O address. CCP sits
            // at 0x34, low enough for `out` to reach.
            "out {ccp}, {key}",
            // Write the protected register, the very next instruction.
            // `st Z, r` stores register r at the address held in Z (the
            // r31:r30 pair). It's used here because `out` only reaches the
            // first 64 addresses and the clock registers are further up.
            "st Z, {value}",
            // The operands. `const` pastes a number into the text, so
            // `{ccp}` becomes 0x34.
            ccp = const reg::CPU_CCP,
            // `in(reg)` asks the compiler to load these values into
            // whichever spare registers it likes, and substitute their
            // names for `{key}` and `{value}`.
            key = in(reg) regs::CPU_CCP_IOREG,
            value = in(reg) value,
            // The target address has to be in Z specifically, because
            // that's what `st Z` reads.
            in("Z") addr as u16,
            // Promises the compiler can optimise around. The block doesn't
            // touch the stack, and neither `out` nor `st` changes the status
            // flags in SREG.
            options(nostack, preserves_flags),
        );
    }
}

pub fn init() {
    // 20 MHz oscillator (the factory OSCCFG fuse) divided by 2. 10 MHz is
    // safe down to 2.7 V, below anywhere a lithium cell should be run.
    protected_write(
        reg::CLKCTRL_MCLKCTRLB,
        regs::CLKCTRL_PDIV_2X | regs::CLKCTRL_PEN,
    );

    // Pull every pin we don't drive up, so nothing floats and leaks current.
    for pin in 0..8 {
        let ctrl = match pin {
            SWITCH => regs::PORT_PULLUPEN | regs::PORT_ISC_BOTHEDGES,
            _ => regs::PORT_PULLUPEN,
        };
        write(reg::PORTA_PIN0CTRL + pin as usize, ctrl);
    }
    for pin in 0..PORTB_PINS {
        if PORTB_OUTPUTS & 1 << pin == 0 {
            write(reg::PORTB_PIN0CTRL + pin as usize, regs::PORT_PULLUPEN);
        }
    }
    for pin in 0..4u8 {
        write(reg::PORTC_PIN0CTRL + pin as usize, regs::PORT_PULLUPEN);
    }

    write(reg::VPORTB_DIR, PORTB_OUTPUTS);

    // Dual-slope PWM on WO0 (FET) and WO1 (linear), straight off the
    // 10 MHz clock. The BUF registers apply at the end of a cycle, so level
    // changes never glitch.
    write16(reg::TCA0_PERBUF, PWM_TOP);
    write16(reg::TCA0_CMP0BUF, 0);
    write16(reg::TCA0_CMP1BUF, 0);
    write(
        reg::TCA0_CTRLB,
        regs::TCA_CMP0EN | regs::TCA_CMP1EN | regs::TCA_WGMODE_DSBOTTOM,
    );
    write(reg::TCA0_CTRLA, regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE);

    // The RTC's periodic interrupt timer, off the internal 32.768 kHz
    // oscillator (RTC.CLKSEL's reset value) divided by 512: 64 Hz.
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
    write(reg::RTC_PITCTRLA, regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN);
}

/// Spins until the next 64 Hz tick.
///
/// The flag goes up whether or not the interrupt is enabled, so for now
/// this busy-waits with the CPU awake. Sleeping between ticks (and properly
/// when off) is on the roadmap, and until then the torch drains its cell
/// in a few days.
pub fn wait_for_tick() {
    while read(reg::RTC_PITINTFLAGS) & regs::RTC_PI == 0 {}
    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);
}

pub fn switch_pressed() -> bool {
    read(reg::VPORTA_IN) & (1 << SWITCH) == 0
}

pub fn set_output(output: Output) {
    write16(reg::TCA0_CMP1BUF, u16::from(output.linear));
    write16(reg::TCA0_CMP0BUF, u16::from(output.fet));
    if output == Output::OFF {
        // Restart the cycle so the next turn-on is consistent.
        write16(reg::TCA0_CNT, 0);
    }
}
