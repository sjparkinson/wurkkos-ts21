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
fn write16(addr: usize, value: u16) {
    let [lo, hi] = value.to_le_bytes();
    write(addr, lo);
    write(addr + 1, hi);
}

/// Writes a register behind configuration change protection. The CPU only
/// accepts the write within four instructions of the CCP key, so it has to
/// be assembly.
fn protected_write(addr: usize, value: u8) {
    // SAFETY: writes the IOREG key to CPU.CCP, then `value` to `addr`.
    // Touches no memory Rust knows about.
    unsafe {
        asm!(
            "out {ccp}, {key}",
            "st Z, {value}",
            ccp = const reg::CPU_CCP,
            key = in(reg) regs::CPU_CCP_IOREG,
            value = in(reg) value,
            in("Z") addr as u16,
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
