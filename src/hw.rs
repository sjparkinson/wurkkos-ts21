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
use ts21::selftest::Fuses;
use ts21::sensors::{Channel, Convert};
use ts21::temp::Calibration;

use crate::regs::{self, addr as reg};

const SWITCH: u8 = 5; // PA5
const FET: u8 = 0; // PB0
const LINEAR: u8 = 1; // PB1
const AUX: u8 = 5; // PB5

/// The PORTB pins we drive. PORTB has PB0 to PB5 on the 20-pin package.
const PORTB_OUTPUTS: u8 = 1 << FET | 1 << LINEAR | 1 << AUX;
const PORTB_PINS: u8 = 6;

/// WDT clocks in a WDT.CTRLA WINDOW or PERIOD setting `n`, 1 to 11.
const fn wdt_clocks(n: u8) -> u16 {
    8 << (n - 1)
}

// The window `hw::init` sets is the one `ts21::watchdog` counts on.
const _: () = {
    use ts21::watchdog::{CLOSED_CLOCKS, OPEN_CLOCKS};
    assert!(wdt_clocks(regs::WDT_WINDOW_32CLK >> 4) == CLOSED_CLOCKS);
    assert!(wdt_clocks(regs::WDT_PERIOD_32CLK) == OPEN_CLOCKS);
};

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

/// Reads a 16-bit register the same way round: the low byte first, which
/// latches the high byte into the peripheral's TEMP, then the high byte from
/// there, so the two halves come from the same value.
fn read16(addr: usize) -> u16 {
    let lo = read(addr);
    let hi = read(addr + 1);
    u16::from_le_bytes([lo, hi])
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

    // The watchdog, in window mode, before anything that could wait on a
    // peripheral. `ts21::watchdog` has the window and when `main` feeds it.
    // Until the first feed there's no closed window, only the 32-clock
    // timeout, and the first pass of the loop feeds it. Wait for the write
    // to sync before going on, so that feed can't land while it's still
    // syncing. It keeps counting in every sleep mode, so the off tick
    // (roadmap stage 2) has to feed it too. Never set WDT.STATUS.LOCK and
    // never touch the WDTCFG fuse.
    while read(reg::WDT_STATUS) & regs::WDT_SYNCBUSY != 0 {}
    protected_write(
        reg::WDT_CTRLA,
        regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK,
    );
    while read(reg::WDT_STATUS) & regs::WDT_SYNCBUSY != 0 {}

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

    // ADC0 in single-conversion mode at 625 kHz, with the delays the
    // temperature sensor needs (DS40002204A, section 30.3.2.6). Both
    // channels use the same timing, so only the reference, the input and the
    // accumulation change between them (`adc_start`).
    // ADC0's reference stays on, rather than starting when each single
    // conversion asks for it. The datasheet doesn't say how long a request
    // lasts, and a 1.5 V reference caught starting up reads low, which
    // reads as a fuller cell than it is. It costs a little current, which
    // matters once the torch sleeps (roadmap stage 2), not before. If sleep
    // turns this or the ADC off, `sensors::Schedule` has to forget the
    // channel it last set up, so the next turn sets the ADC up again.
    write(reg::VREF_CTRLB, regs::VREF_ADC0REFEN);
    // The ATDF says CALIB resets to 50% duty, the datasheet says 25%, and
    // 25% is what it wants at this clock (section 30.5.17), so say so.
    write(reg::ADC0_CALIB, regs::ADC_DUTYCYC_DUTY25);
    write(reg::ADC0_CTRLC, regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV16);
    write(reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32);
    write(reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32US);
    write(reg::ADC0_CTRLA, regs::ADC_ENABLE);

    // `sleep` puts the CPU in idle, which stops only the CPU and flash, so
    // TCA0 keeps the PWM going and the ADC keeps converting. Standby would
    // stop TCA0, which has no RUNSTDBY (section 11.3.2.1).
    write(
        reg::SLPCTRL_CTRLA,
        regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN,
    );

    // The RTC's periodic interrupt timer, off the internal 32.768 kHz
    // oscillator (RTC.CLKSEL's reset value) divided by 512: 64 Hz, with its
    // interrupt on, in the datasheet's order (section 23.5.1). RTC.CTRLA
    // stays at its reset value: any write to it stretches the PIT's period
    // and clearing RTCEN stops the PIT (errata DS80000886C 2.6.1 and 2.6.2).
    write(reg::RTC_PITINTCTRL, regs::RTC_PI);
    while read(reg::RTC_PITSTATUS) & regs::RTC_CTRLBUSY != 0 {}
    write(reg::RTC_PITCTRLA, regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN);
}

/// The bit in GPIOR0 that [`rtc_pit`] sets each tick. AN2515C section 4.5
/// suggests GPIORs for flags shared with a handler, because they sit in the
/// bit-addressable I/O space, so a one-bit change can be a single `sbi` or
/// `cbi` rather than a read-modify-write that needs interrupts off. Here
/// the handler and the loop each write the whole byte, and the loop does it
/// with interrupts off anyway, so that's moot. It also keeps the flag out
/// of RAM.
const TICK: u8 = 1 << 0;

/// Sleeps until the next 64 Hz tick.
///
/// The PIT interrupt sets [`TICK`]; this checks and clears it with
/// interrupts off, so a tick can't land between the check and the clear.
/// If it isn't set yet, `sei` and `sleep` go back to back (AN2515C section
/// 4.5): the instruction after `sei` always runs before a pending
/// interrupt, so a tick that arrived after the check wakes the `sleep` at
/// once rather than being slept through. Anything else that wakes it, such
/// as the switch's pin-change interrupt, goes round the loop and back to
/// sleep.
pub fn wait_for_tick() {
    loop {
        // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a
        // compiler barrier, so the GPIOR0 read can't move above it.
        unsafe { asm!("cli", options(nostack)) };
        if read(reg::GPIO_GPIOR0) & TICK != 0 {
            write(reg::GPIO_GPIOR0, 0);
            // SAFETY: `sei` only sets SREG.I.
            unsafe { asm!("sei", options(nostack)) };
            return;
        }
        // SAFETY: sets SREG.I and sleeps until an interrupt, whose handler
        // returns here. Both handlers save what they touch.
        unsafe { asm!("sei", "sleep", options(nostack)) };
    }
}

/// RTC_PIT, vector 7: the 64 Hz tick. It clears the PIT's flag and sets
/// [`TICK`], and that's all, since everything it touches the main loop
/// only reads or clears with interrupts off. Above all it mustn't touch
/// TCA0's 16-bit registers, whose shared TEMP byte the main loop might be
/// halfway through using (section 8.5.6, Accessing 16-bit Registers).
pub extern "avr-interrupt" fn rtc_pit() {
    write(reg::RTC_PITINTFLAGS, regs::RTC_PI);
    write(reg::GPIO_GPIOR0, TICK);
}

/// PORTA_PORT, vector 3: the switch changed. `hw::init` sets PA5 to
/// interrupt on both edges, which is for waking from power-down when
/// that's done (roadmap stage 2). For now waking is all it's for: the main
/// loop samples the switch every tick anyway. It clears every PORTA flag,
/// since one left up would fire again the moment this returned.
pub extern "avr-interrupt" fn porta_port() {
    write(reg::PORTA_INTFLAGS, regs::PORT_INT_ALL);
}

/// Tells the watchdog the loop is still running. `main` calls this at the
/// end of a pass, after the output's been written, when
/// `ts21::watchdog::Feeder` says so. Too early or too late and the chip
/// resets.
pub fn feed_watchdog() {
    // SAFETY: `wdr` only resets the watchdog's counter. It touches no memory
    // and no flags. No `nomem`, so the compiler can't move it above the
    // output's volatile stores.
    unsafe { asm!("wdr", options(nostack, preserves_flags)) }
}

/// The temperature sensor's factory calibration from the signature row.
pub fn temperature_calibration() -> Calibration {
    Calibration {
        offset: read(reg::SIGROW_TEMPSENSE1) as i8,
        gain: read(reg::SIGROW_TEMPSENSE0),
    }
}

/// Has CRCSCAN check all of flash against the CRC `cargo xtask hex` puts in
/// its last two bytes, and says whether it matched. Run it before turning
/// anything on: the scan stalls the CPU until it's done, 8,192 words at
/// three clocks each, about 7 ms at the reset clock of 20 MHz / 6.
///
/// STATUS.OK reads 1 from reset, before any scan, and the scan only starts
/// three cycles after ENABLE while the CPU carries on (DS §27.3.1). So
/// three `nop`s go between the write and the first read, or a read could
/// see the reset value and pass a flash that's wrong. Then it waits for
/// BUSY to clear, which it should already have done, since the CPU was
/// stalled.
pub fn flash_ok() -> bool {
    write(
        reg::CRCSCAN_CTRLB,
        regs::CRCSCAN_SRC_FLASH | regs::CRCSCAN_MODE_PRIORITY,
    );
    write(reg::CRCSCAN_CTRLA, regs::CRCSCAN_ENABLE);
    // SAFETY: three `nop`s, which touch nothing.
    unsafe { asm!("nop", "nop", "nop", options(nostack, preserves_flags)) };
    while read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_BUSY != 0 {}
    read(reg::CRCSCAN_STATUS) & regs::CRCSCAN_OK != 0
}

/// The fuse bytes `selftest::Fuses::ok` checks, read from data space. This
/// only ever reads them.
pub fn fuses() -> Fuses {
    Fuses {
        wdtcfg: read(reg::FUSE_WDTCFG),
        osccfg: read(reg::FUSE_OSCCFG),
        syscfg0: read(reg::FUSE_SYSCFG0),
        append: read(reg::FUSE_APPEND),
        bootend: read(reg::FUSE_BOOTEND),
    }
}

/// Starts an ADC conversion, setting the ADC up for its channel first if
/// asked. The setups are Anduril's (`arch/attiny1616.c`) for the voltage and
/// the datasheet's (section 30.3.2.6) for the temperature.
/// `sensors::Schedule` only asks when the channel changes, so with the light
/// off the registers are written once and left alone.
///
/// It never clears RESRDY. That works because [`adc_result`] runs every
/// tick and reading the result clears it, so a stale one is never read
/// twice.
pub fn adc_start(convert: Convert) {
    if convert.setup {
        let (vref, refsel, muxpos, sampnum) = match convert.channel {
            // The 1.5 V reference measured against VDD, four conversions.
            Channel::Voltage => (
                regs::VREF_ADC0REFSEL_1V5,
                regs::ADC_REFSEL_VDDREF,
                regs::ADC_MUXPOS_INTREF,
                regs::ADC_SAMPNUM_ACC4,
            ),
            // The sensor against the 1.1 V reference, one conversion.
            Channel::Temperature => (
                regs::VREF_ADC0REFSEL_1V1,
                regs::ADC_REFSEL_INTREF,
                regs::ADC_MUXPOS_TEMPSENSE,
                regs::ADC_SAMPNUM_ACC1,
            ),
        };
        // Nothing else uses VREF, so DAC0REFSEL's bits can stay zero.
        write(reg::VREF_CTRLA, vref);
        write(
            reg::ADC0_CTRLC,
            regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV16 | refsel,
        );
        write(reg::ADC0_MUXPOS, muxpos);
        write(reg::ADC0_CTRLB, sampnum);
    }
    write(reg::ADC0_COMMAND, regs::ADC_STCONV);
}

/// The last conversion's result, if it's finished and not been read yet.
/// Reading it clears RESRDY.
pub fn adc_result() -> Option<u16> {
    if read(reg::ADC0_INTFLAGS) & regs::ADC_RESRDY == 0 {
        return None;
    }
    Some(read16(reg::ADC0_RES))
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
