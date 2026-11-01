//! The TS21 driver's pins and the ATtiny1616 registers that drive them.
//!
//! No PAC: the handful of registers we touch are written out by hand, each
//! checked against Microchip's ATDF for the part. docs/hardware.md has the
//! pin map and where it comes from.
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

/// Data-space addresses, from the ATtiny1617 ATDF (the 1616 is the same die
/// in a 20-pin package).
mod reg {
    pub const VPORTA_IN: usize = 0x0002;
    pub const VPORTB_DIR: usize = 0x0004;

    pub const CLKCTRL_MCLKCTRLB: usize = 0x0061;

    pub const RTC_PITCTRLA: usize = 0x0150;
    pub const RTC_PITSTATUS: usize = 0x0151;
    pub const RTC_PITINTFLAGS: usize = 0x0153;

    pub const PORTA_PIN0CTRL: usize = 0x0410;
    pub const PORTB_PIN0CTRL: usize = 0x0430;
    pub const PORTC_PIN0CTRL: usize = 0x0450;

    pub const TCA0_CTRLA: usize = 0x0A00;
    pub const TCA0_CTRLB: usize = 0x0A01;
    pub const TCA0_CNT: usize = 0x0A20;
    pub const TCA0_PERBUF: usize = 0x0A36;
    pub const TCA0_CMP0BUF: usize = 0x0A38;
    pub const TCA0_CMP1BUF: usize = 0x0A3A;
}

const SWITCH: u8 = 5; // PA5
const FET: u8 = 0; // PB0
const LINEAR: u8 = 1; // PB1
const AUX: u8 = 5; // PB5

/// The PORTB pins we drive. PORTB has PB0 to PB5 on the 20-pin package.
const PORTB_OUTPUTS: u8 = 1 << FET | 1 << LINEAR | 1 << AUX;
const PORTB_PINS: u8 = 6;

const PORT_PULLUPEN: u8 = 0x08;
const PORT_ISC_BOTHEDGES: u8 = 0x01;

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
    // SAFETY: writes the IOREG key (0xD8) to CPU.CCP (I/O address 0x34),
    // then `value` to `addr`. Touches no memory Rust knows about.
    unsafe {
        asm!(
            "out 0x34, {key}",
            "st Z, {value}",
            key = in(reg) 0xD8u8,
            value = in(reg) value,
            in("Z") addr as u16,
            options(nostack, preserves_flags),
        );
    }
}

pub fn init() {
    // 20 MHz oscillator (the factory OSCCFG fuse) divided by 2. 10 MHz is
    // safe down to 2.7 V, below anywhere a lithium cell should be run.
    protected_write(reg::CLKCTRL_MCLKCTRLB, 0x01); // PDIV = 2, PEN

    // Pull every pin we don't drive up, so nothing floats and leaks current.
    for pin in 0..8 {
        let ctrl = match pin {
            SWITCH => PORT_PULLUPEN | PORT_ISC_BOTHEDGES,
            _ => PORT_PULLUPEN,
        };
        write(reg::PORTA_PIN0CTRL + pin as usize, ctrl);
    }
    for pin in 0..PORTB_PINS {
        if PORTB_OUTPUTS & 1 << pin == 0 {
            write(reg::PORTB_PIN0CTRL + pin as usize, PORT_PULLUPEN);
        }
    }
    for pin in 0..4u8 {
        write(reg::PORTC_PIN0CTRL + pin as usize, PORT_PULLUPEN);
    }

    write(reg::VPORTB_DIR, PORTB_OUTPUTS);

    // Dual-slope PWM on WO0 (FET) and WO1 (linear), straight off the
    // 10 MHz clock. The BUF registers apply at the end of a cycle, so level
    // changes never glitch.
    write16(reg::TCA0_PERBUF, PWM_TOP);
    write16(reg::TCA0_CMP0BUF, 0);
    write16(reg::TCA0_CMP1BUF, 0);
    write(reg::TCA0_CTRLB, 0x10 | 0x20 | 0x07); // CMP0EN, CMP1EN, DSBOTTOM
    write(reg::TCA0_CTRLA, 0x01); // DIV1, ENABLE

    // The RTC's periodic interrupt timer, off the internal 32.768 kHz
    // oscillator (RTC.CLKSEL's reset value) divided by 512: 64 Hz.
    while read(reg::RTC_PITSTATUS) & 0x01 != 0 {}
    write(reg::RTC_PITCTRLA, 0x08 << 3 | 0x01); // CYC512, PITEN
}

/// Spins until the next 64 Hz tick.
///
/// The flag goes up whether or not the interrupt is enabled, so for now
/// this busy-waits with the CPU awake. Sleeping between ticks (and properly
/// when off) is on the roadmap, and until then the torch drains its cell
/// in a few days.
pub fn wait_for_tick() {
    while read(reg::RTC_PITINTFLAGS) & 0x01 == 0 {}
    write(reg::RTC_PITINTFLAGS, 0x01);
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
