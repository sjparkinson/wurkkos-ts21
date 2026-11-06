//! The TS21 driver's pins and the ATtiny1616 registers that drive them.
//!
//! No PAC: `src/regs.rs` has plain constants, generated from Microchip's ATDF
//! at build time. docs/hardware.md has the pin map and where it comes from.
//!
//! | Pin | Job                                   |
//! |-----|---------------------------------------|
//! | PA5 | Side switch, active low, pulled up    |
//! | PB0 | FET channel, TCA0 WO0                 |
//! | PB1 | Linear (7135) channel, TCA0 WO1       |
//! | PB5 | Amber button LED, lit by the pull-up  |

#![allow(unsafe_code)]

use core::arch::asm;
use core::mem::MaybeUninit;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use ts21::ramp::{Output, PWM_TOP};
use ts21::selftest::{CLOCKS_MAX, Fuses, NO_TICK};
use ts21::sensors::{Channel, Convert};
use ts21::temp::Calibration;

use crate::regs::{self, addr as reg};

const UPDI: u8 = 0; // PA0
const SWITCH: u8 = 5; // PA5
// src/rt.rs drives these two low before its self-tests.
pub(crate) const FET: u8 = 0; // PB0
pub(crate) const LINEAR: u8 = 1; // PB1
const AUX: u8 = 5; // PB5

/// The PORTB pins we drive. PORTB has PB0 to PB5 on the 20-pin package.
const PORTB_OUTPUTS: u8 = 1 << FET | 1 << LINEAR;
const PORTB_PINS: u8 = 6;

// The amber button LED stays on, dimly, whenever there's a cell in. PB5 is
// an input with its pull-up on, like every other pin we don't drive, and
// the pull-up's trickle through the LED is enough to see it. That's
// Anduril's low aux setting. Driving the pin high would cost milliamps.
const _: () = assert!(PORTB_OUTPUTS & 1 << AUX == 0);

/// PORTx.PINnCTRL for a pin nothing reads. The pull-up doesn't need the input
/// buffer, so this still lights the button LED on PB5.
const UNUSED_PIN: u8 = regs::PORT_PULLUPEN | regs::PORT_ISC_INPUT_DISABLE;

/// TCA0.CTRLB with the linear channel on its pin.
const TCA_LINEAR: u8 = regs::TCA_CMP1EN | regs::TCA_WGMODE_SINGLESLOPE;

/// TCA0.CTRLB for `output`. WO0 only gets the FET's pin at a level that uses
/// it, so a stopped TCA0 holding WO0 high can't turn the FET on at a linear
/// level, or on a chip that's refused it.
const fn tca_driving(output: Output) -> u8 {
    if output.fet != 0 {
        TCA_LINEAR | regs::TCA_CMP0EN
    } else {
        TCA_LINEAR
    }
}

/// The buffer valid flags of a whole row: PERBUF and both CMPnBUFs.
const TCA_ROW: u8 = regs::TCA_PERBV | regs::TCA_CMP0BV | regs::TCA_CMP1BV;

/// Goes at a row under LUPD before `set_output` lets it go for this pass. A
/// go takes about 23 cycles, and BOTTOMs are at least 255 apart, so one or two
/// is all it should ever need.
const ROW_TRIES: u8 = 4;

/// WDT clocks in a WDT.CTRLA WINDOW or PERIOD setting `n`, 1 to 11.
const fn wdt_clocks(n: u8) -> u16 {
    8 << (n - 1)
}

// The window `hw::init` sets is the one `ts21::watchdog` counts on.
const _: () = {
    use ts21::watchdog::{CLOSED_CLOCKS, OPEN_CLOCKS};
    assert!(
        wdt_clocks(regs::WDT_WINDOW_32CLK >> regs::WDT_WINDOW.trailing_zeros()) == CLOSED_CLOCKS
    );
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

/// Polls of a sync flag before giving up on it. The datasheet gives no time
/// for WDT.CTRLA's sync. Going by the 2 to 3 WDT clocks a `wdr` takes
/// (section 19.3.6), it's likely 3 ms, or 4.3 ms with the oscillator 30% slow,
/// so about 22,000 cycles. A poll compiles to about 12, so this is about 40 ms,
/// nine times that.
const SYNC_POLLS: u16 = 16_384;

/// TCB0's TOP, as far as it goes. At CLK_PER / 2 it fires about 131,072 cycles
/// after CNT was last reset: 26.2 ms, against the 22.3 ms a tick can take with
/// the 32 kHz oscillator 30% slow, and 25 ms with the main clock 5% fast.
const TICK_DEADLINE: u16 = u16::MAX;

// A counted wait restarts the deadline, and on short cycles hits its ceiling
// before that goes off. On the longest, the deadline can get there first.
// Either way it's `NO_TICK`, and a good chip's tick is well short of both.
const _: () = assert!(CLOCKS_MAX < 2 * (TICK_DEADLINE as u32));

/// Whether `mask` in `addr` clears soon enough. Everything that syncs to the
/// 32 kHz oscillator waits here, since with that oscillator dead nothing
/// would ever clear it, not even the watchdog, which runs off the same one.
fn synced(addr: usize, mask: u8) -> bool {
    (0..SYNC_POLLS).any(|_| read(addr) & mask == 0)
}

/// Sets everything up, and says whether the 32 kHz oscillator's alive. If it
/// isn't, the PIT is left off.
///
/// `watchdog` is `WDT.CTRLA` for this boot, from `ts21::watchdog::Proof`. It's
/// the only thing that changes between a step of the proof and normal use.
/// Either way TCA0 starts with both compares at 0, and nothing but
/// `set_output` changes them, so on every boot of the proof the pins are held
/// low as they are with the light off. During a reset itself nothing holds
/// them: every output tri-states (section 16.3.1).
pub fn init(watchdog: u8) -> bool {
    // 20 MHz oscillator (the factory OSCCFG fuse) divided by 4. 5 MHz is in
    // spec down to 1.8 V. 10 MHz needs 2.7 V (table 36-3), and VCC can go
    // under that near the cut-off, or on the FET quicker than the low-voltage
    // protection's filter sees it.
    protected_write(
        reg::CLKCTRL_MCLKCTRLB,
        regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN,
    );

    // The watchdog, before anything that could wait on a peripheral. In
    // normal use that's window mode, and `ts21::watchdog` has the window,
    // when `main` feeds it and the other settings the proof uses.
    // Until the first feed there's no closed window, only the 32-clock
    // timeout, and the first pass of the loop feeds it. Wait for the write
    // to sync before going on, so that feed can't land while it's still
    // syncing. It keeps counting in every sleep mode, which is fine, since
    // the PIT does too and the loop runs every tick, on or off. Never set
    // WDT.STATUS.LOCK and never touch the WDTCFG fuse.
    let mut alive = synced(reg::WDT_STATUS, regs::WDT_SYNCBUSY);
    protected_write(reg::WDT_CTRLA, watchdog);
    alive &= synced(reg::WDT_STATUS, regs::WDT_SYNCBUSY);

    // Pull every pin we don't drive up, so nothing floats and leaks current,
    // and so the button LED on PB5 lights. Turn off the input buffers we
    // don't read too, which saves a little more (AN2515C section 9). PA0 only
    // gets the pull-up: it's UPDI, and nothing's worth risking that for. So
    // does the switch, with no interrupt: the PIT wakes the chip every tick
    // on or off, and the loop reads the pin then.
    for pin in 0..8 {
        let ctrl = match pin {
            SWITCH | UPDI => regs::PORT_PULLUPEN,
            _ => UNUSED_PIN,
        };
        write(reg::PORTA_PIN0CTRL + pin as usize, ctrl);
    }
    for pin in 0..PORTB_PINS {
        if PORTB_OUTPUTS & 1 << pin == 0 {
            write(reg::PORTB_PIN0CTRL + pin as usize, UNUSED_PIN);
        }
    }
    for pin in 0..4u8 {
        write(reg::PORTC_PIN0CTRL + pin as usize, UNUSED_PIN);
    }

    write(reg::VPORTB_DIR, PORTB_OUTPUTS);

    // Single-slope PWM on WO0 (FET) and WO1 (linear), straight off the
    // 5 MHz clock, in TCA's single mode: CTRLD.SPLITM stays at 0. The BUF registers apply at the end of a cycle, so level
    // changes never glitch. A compare of 0 holds the pin low and one over
    // PER holds it high (section 20.3.3.4.3), so PER is one under the top
    // duty. PER goes in directly, with the timer stopped, or it'd run its
    // first cycle at the reset value of 0xFFFF, 13 ms before the first
    // UPDATE (section 20.3.3.3). That's enough to fail the clock check at
    // power-on, and power-down could freeze it there.
    write16(reg::TCA0_PER, PWM_TOP - 1);
    write16(reg::TCA0_CMP0BUF, 0);
    write16(reg::TCA0_CMP1BUF, 0);
    // The light's off, so the pins stay PORT's until `set_output` asks for a
    // level.
    write(reg::TCA0_CTRLB, regs::TCA_WGMODE_SINGLESLOPE);
    write(reg::TCA0_CTRLA, regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE);

    // ADC0 in single-conversion mode at 625 kHz, with the delays the
    // temperature sensor needs (DS40002204A, section 30.3.2.6). Both
    // channels use the same timing and accumulate four conversions, so only
    // the reference and the input change between them (`adc_start`).
    // Turning it and the reference on waits for `adc_start`.
    // The ATDF says CALIB resets to 50% duty, the datasheet says 25%, and
    // 25% is what it wants at ADC clocks of 1.5 MHz and under (section
    // 30.5.17), so say so.
    write(reg::ADC0_CALIB, regs::ADC_DUTYCYC_DUTY25);
    // 5 MHz / 8 is 625 kHz, inside the 200 kHz to 1.5 MHz the datasheet wants
    // for 10 bits (table 36-26). SAMPCAP is the smaller sample capacitor, for
    // references of 1 V and up.
    write(reg::ADC0_CTRLC, regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV8);
    // 32 ADC clocks, 51 µs at 625 kHz, over the 32 µs the temperature sensor
    // needs (section 30.3.2.6).
    write(reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32);
    write(reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32US);

    // The RTC's periodic interrupt timer, off the internal 32.768 kHz
    // oscillator (RTC.CLKSEL's reset value) divided by 512: 64 Hz, with its
    // interrupt on, in the datasheet's order (section 23.5.1). RTC.CTRLA
    // stays at its reset value: any write to it stretches the PIT's period
    // and clearing RTCEN stops the PIT (errata DS80000886C 2.6.1 and 2.6.2).
    write(reg::RTC_PITINTCTRL, regs::RTC_PI);
    alive &= synced(reg::RTC_PITSTATUS, regs::RTC_CTRLBUSY);
    if alive {
        write(reg::RTC_PITCTRLA, regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN);
        // The first sleep is power-down, and the datasheet doesn't say this
        // write finishes syncing with the peripheral clock stopped. If it
        // didn't, there'd be no ticks, so see it through.
        alive = synced(reg::RTC_PITSTATUS, regs::RTC_CTRLBUSY);
    }

    // The deadline for the next tick, in TCB0's periodic interrupt mode,
    // which is CTRLB's reset value. No other enabled interrupt comes off the
    // main clock, and TCA0's would come 306 times a tick, so without this a
    // dead 32 kHz oscillator would leave the light on with nothing running: no
    // tick, no low-voltage protection, and no watchdog, which stops with it.
    write16(reg::TCB0_CCMP, TICK_DEADLINE);
    write(reg::TCB0_INTCTRL, regs::TCB_CAPT);
    write(reg::TCB0_CTRLA, regs::TCB_CLKSEL_CLKDIV2 | regs::TCB_ENABLE);
    alive
}

/// The bit in GPIOR0 that [`rtc_pit`] sets each tick. AN2515C section 4.5
/// suggests GPIORs for flags shared with a handler, because they sit in the
/// bit-addressable I/O space, so a one-bit change can be a single `sbi` or
/// `cbi` rather than a read-modify-write that needs interrupts off. Here
/// the handler and the loop each write the whole byte, and the loop does it
/// with interrupts off anyway, so that's moot. It also keeps the flag out
/// of RAM.
const TICK: u8 = 1 << 0;

/// What [`tcb0_int`] leaves in GPIOR1. It has a byte to itself because
/// [`rtc_pit`] and the loop write all of GPIOR0.
const OVERDUE: u8 = 1 << 0;

/// Whether both PWM channels are really at 0, not only their BUF registers,
/// which reach CMPn at the next BOTTOM. No BV can be set either, or the chip
/// would freeze with an update still pending, as the datasheet only promises
/// TCA0 runs in idle (section 20.3.6), and PER has to be the off row's, in
/// case a row was lost. `set_output`'s UPDATE command clears BV as the output
/// goes off, so this is normally true on that pass. If TCA0 ignores the
/// command, it stays false, and the chip idles with the pins off TCA0, which
/// is still off.
fn output_settled() -> bool {
    read(reg::TCA0_CTRLFSET) & TCA_ROW == 0
        && read16(reg::TCA0_PER).wrapping_add(1) == Output::OFF.top
        && read16(reg::TCA0_CMP0) == 0
        && read16(reg::TCA0_CMP1) == 0
}

/// Sets up the sleep that [`wait_for_tick`] goes on to do. `rest` is
/// `Torch::resting`.
///
/// Power-down stops TCA0 where it is, so it waits until the output's settled,
/// which `set_output` sees to as it goes off. The pins are PORT's by then:
/// CMPnEN goes with the output, so the datasheet not saying what level a
/// waveform output holds in sleep can't matter.
/// It also turns the ADC and its forced reference off. The datasheet gives
/// no current for either left on in power-down, and Anduril's
/// `sleep_until_eswitch_pressed` does the same. `adc_start` turns them back
/// on, and a rest always makes `sensors::Schedule` ask it to.
///
/// Otherwise it's idle, which stops only the CPU and flash, so TCA0 keeps the
/// PWM going and the ADC keeps converting. Standby would stop TCA0, which has
/// no RUNSTDBY (section 11.3.2.1).
///
/// SEN has to stay set in both: with it clear, `sleep` does nothing.
fn prepare_sleep(rest: bool) {
    if rest && output_settled() {
        write(reg::ADC0_CTRLA, 0);
        write(reg::VREF_CTRLB, 0);
        write(
            reg::SLPCTRL_CTRLA,
            regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN,
        );
    } else {
        write(
            reg::SLPCTRL_CTRLA,
            regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN,
        );
    }
}

/// Sleeps until the next 64 Hz tick, in power-down if `rest` allows it. The
/// PIT and the watchdog carry on in there (sections 23.8 and 19.3.4), so the
/// loop runs every tick, on or off, and that's how it sees the switch.
///
/// The PIT interrupt sets [`TICK`]; this checks and clears it with
/// interrupts off, so a tick can't land between the check and the clear.
/// If it isn't set yet, `sei` and `sleep` go back to back (AN2515C section
/// 4.5): the instruction after `sei` always runs before a pending
/// interrupt, so a tick that arrived after the check wakes the `sleep` at
/// once rather than being slept through. The flag earns its place: without
/// it, a tick that landed while the loop was busy would go uncounted, and the
/// next feed would come a tick late, at the very end of the watchdog's open
/// window.
///
/// Returns false if [`tcb0_int`] gave up on the tick instead, which only
/// happens in idle, since TCB0 stops in power-down. With the PIT the only
/// thing that wakes it from there, a chip that's powered down when its 32 kHz
/// oscillator dies stays down, which is off, until the cell comes out.
pub fn wait_for_tick(rest: bool) -> bool {
    prepare_sleep(rest);
    loop {
        // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a
        // compiler barrier, so the GPIOR0 read can't move above it.
        unsafe { asm!("cli", options(nostack)) };
        let ticked = read(reg::GPIO_GPIOR0) & TICK != 0;
        let overdue = read(reg::GPIO_GPIOR1) & OVERDUE != 0;
        if ticked || overdue {
            write(reg::GPIO_GPIOR0, 0);
            write(reg::GPIO_GPIOR1, 0);
            write16(reg::TCB0_CNT, 0);
            // SAFETY: `sei` only sets SREG.I.
            unsafe { asm!("sei", options(nostack)) };
            return ticked;
        }
        // SAFETY: sets SREG.I and sleeps until an interrupt, whose handler
        // returns here. The handler saves what it touches.
        unsafe { asm!("sei", "sleep", options(nostack)) };
    }
}

/// Waits for the next tick like [`wait_for_tick`], awake, and counts TCA0's
/// clocks on the way (`selftest::ClockCheck`). The firmware has no use for an
/// interrupt every PWM cycle, so this polls, with interrupts off so the PIT's
/// flag stays up for it to see, and returns as soon as it's seen it. It never
/// sleeps, so TCA0 keeps running even if the torch is resting.
///
/// Each overflow adds the cycle that just ended, so it's addition only, with
/// no 32-bit multiply. That length comes from PER, not from the level: read
/// after an overflow, PER is already the next cycle's, so `cycle` holds it
/// back until that cycle ends. The pass before this can have changed the
/// output, and a new period only lands at BOTTOM, so the first cycle here can
/// still be the old one, up to 16 times the length of the new.
///
/// The count starts where the last pass ended, not on the tick, so it's short
/// by that pass. RTC.CTRLA stays out of it: the PIT's flag is the RTC tick
/// here, and setting RTCEN to read RTC.CNT would stretch a PIT period (errata
/// DS80000886C 2.6.1). A count over the ceiling stops there, so a dead 32 kHz
/// oscillator can't hold it, and returns [`NO_TICK`]. The deadline restarts
/// with the count, so it can't cut a good one short, and it's what ends this
/// if TCA0 has stopped too.
pub fn count_tick() -> u32 {
    prepare_sleep(false);
    // SAFETY: `cli` only clears SREG.I. With no `nomem`, it's also a compiler
    // barrier, so nothing below can move above it.
    unsafe { asm!("cli", options(nostack)) };
    write(reg::TCA0_INTFLAGS, regs::TCA_OVF);
    write16(reg::TCB0_CNT, 0);
    let mut cycle = u32::from(read16(reg::TCA0_PER)) + 1;
    let mut clocks = 0;
    while read(reg::RTC_PITINTFLAGS) & regs::RTC_PI == 0 && clocks != NO_TICK {
        if read(reg::TCB0_INTFLAGS) & regs::TCB_CAPT != 0 {
            clocks = NO_TICK;
        } else if read(reg::TCA0_INTFLAGS) & regs::TCA_OVF != 0 {
            write(reg::TCA0_INTFLAGS, regs::TCA_OVF);
            clocks += cycle;
            cycle = u32::from(read16(reg::TCA0_PER)) + 1;
            if clocks > CLOCKS_MAX {
                clocks = NO_TICK;
            }
        }
    }
    if clocks != NO_TICK {
        write(reg::RTC_PITINTFLAGS, regs::RTC_PI);
        write16(reg::TCB0_CNT, 0);
    }
    // SAFETY: `sei` only sets SREG.I.
    unsafe { asm!("sei", options(nostack)) };
    clocks
}

/// TCB0_INT, vector 13: the tick's more than 26 ms late. Like [`rtc_pit`] it
/// leaves a flag for [`wait_for_tick`], which is all it needs to do, since the
/// point of it is to wake the CPU. Writing nothing wider than a byte, it can't
/// upset a 16-bit write in the main loop.
pub extern "avr-interrupt" fn tcb0_int() {
    write(reg::TCB0_INTFLAGS, regs::TCB_CAPT);
    write(reg::GPIO_GPIOR1, OVERDUE);
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

/// Where the watchdog's proof keeps its place across a reset (`ts21::watchdog`
/// says what the bytes mean). Startup doesn't copy or zero `.noinit`, and puts
/// it back after a RAM test on a hang, so it holds what the last boot stored.
/// After a power-on or UPDI reset it holds no step at all, and if SRAM doesn't
/// survive a reset it holds junk, which is why it's `MaybeUninit`.
#[unsafe(link_section = ".noinit")]
static mut MARKER: MaybeUninit<[u8; 2]> = MaybeUninit::uninit();

/// What the marker holds. Only ever read before anything's stored on this
/// boot, or by the proof's own loop, so nothing shares it with a handler.
pub fn marker() -> [u8; 2] {
    let at = addr_of!(MARKER).cast::<u8>();
    // SAFETY: both bytes are in the static, and any value is a valid `u8`.
    // Volatile, because startup never wrote it, so the compiler has nothing
    // to go on but the `uninit` above.
    unsafe { [read_volatile(at), read_volatile(at.add(1))] }
}

/// Stores the marker. A reset between the two bytes leaves a marker that
/// doesn't check out, which `ts21::watchdog` treats as a failed proof.
pub fn set_marker(marker: [u8; 2]) {
    let at = addr_of_mut!(MARKER).cast::<u8>();
    // SAFETY: both bytes are in the static, and nothing else is using it.
    unsafe {
        write_volatile(at, marker[0]);
        write_volatile(at.add(1), marker[1]);
    }
}

/// What caused this reset, from `RSTCTRL.RSTFR`, leaving it clear for the
/// next one. The flags are sticky: writing 1 clears any of them, and a
/// power-on reset clears them all but PORF (section 12.5.1). No other reset
/// does, so this has to run on every boot before anything could reset the
/// chip. Left up, PORF would make the next bite look like another power-on,
/// and the proof would start over, forever.
pub fn take_reset_flags() -> u8 {
    let flags = read(reg::RSTCTRL_RSTFR);
    write(reg::RSTCTRL_RSTFR, flags);
    flags
}

/// Resets the chip, for a proof that waited for a bite that never came. It
/// takes effect at once (section 12.3.2.1.3), so this doesn't really return.
pub fn software_reset() {
    protected_write(reg::RSTCTRL_SWRR, regs::RSTCTRL_SWRE);
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
/// `nop`s go between the write and the first read, or a read could see the
/// reset value and pass a flash that's wrong. Three would do if the three
/// cycles count from the end of the `sts`; the datasheet doesn't say, so
/// there's a fourth. Then it waits for BUSY to clear, which it should
/// already have done, since the CPU was stalled.
///
/// SRC = FLASH is the only choice with BOOTEND and APPEND at 0, and PRIORITY
/// the only mode. Writing ENABLE starts a scan even when it's already 1.
pub fn flash_ok() -> bool {
    write(
        reg::CRCSCAN_CTRLB,
        regs::CRCSCAN_SRC_FLASH | regs::CRCSCAN_MODE_PRIORITY,
    );
    write(reg::CRCSCAN_CTRLA, regs::CRCSCAN_ENABLE);
    // SAFETY: four `nop`s, which touch nothing.
    unsafe {
        asm!(
            "nop",
            "nop",
            "nop",
            "nop",
            options(nostack, preserves_flags)
        )
    };
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
/// asked. The setups are trunk Anduril's (`arch/attiny1616.c`) for the voltage and
/// the datasheet's (section 30.3.2.6) for the temperature, plus accumulating
/// four conversions, which that section doesn't mention either way.
/// `sensors::Schedule` only asks when the channel changes, or after a rest.
///
/// Setting up also turns the ADC on, and forces its reference on, rather
/// than leaving it to start when each single conversion asks for it. The
/// datasheet doesn't say how long a request lasts, and a 1.5 V reference
/// caught starting up reads low, which reads as a fuller cell than it is.
/// After a rest it's back well before it counts: `Schedule` throws this
/// conversion away, and the next is a tick later, against 60 µs at most to
/// start (section 29.3.1).
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
            // The sensor against the 1.1 V reference, four conversions.
            Channel::Temperature => (
                regs::VREF_ADC0REFSEL_1V1,
                regs::ADC_REFSEL_INTREF,
                regs::ADC_MUXPOS_TEMPSENSE,
                regs::ADC_SAMPNUM_ACC4,
            ),
        };
        // Keep the reference on between conversions, so a single conversion
        // never catches it starting up (section 18.5.2). A cold 1.5 V
        // reference reads low, which reads as a fuller cell.
        write(reg::VREF_CTRLB, regs::VREF_ADC0REFEN);
        // Nothing else uses VREF, so DAC0REFSEL's bits can stay zero.
        write(reg::VREF_CTRLA, vref);
        write(
            reg::ADC0_CTRLC,
            regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV8 | refsel,
        );
        write(reg::ADC0_MUXPOS, muxpos);
        write(reg::ADC0_CTRLB, sampnum);
        write(reg::ADC0_CTRLA, regs::ADC_ENABLE);
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

/// Writes `output` to the buffers, with LUPD set, and says whether all three
/// are valid at the end of it. A BOTTOM can land between these writes. Section
/// 20.5.6 says of LUPD that nothing's updated "even though an UPDATE condition
/// has occurred", and section 20.3.3.2 says BV clears on an UPDATE condition,
/// so the buffers written so far may lose their BV with nothing copied. Letting
/// go of a part of a row then would be the tear LUPD is there to stop, so the
/// flags get read back while it's still locked, and it goes again. If it never
/// gets all three, it clears them, so that none of the row lands.
fn write_row(output: Output) -> bool {
    for _ in 0..ROW_TRIES {
        write16(reg::TCA0_PERBUF, output.top - 1);
        write16(reg::TCA0_CMP1BUF, output.linear);
        write16(reg::TCA0_CMP0BUF, output.fet);
        if read(reg::TCA0_CTRLFSET) & TCA_ROW == TCA_ROW {
            return true;
        }
    }
    write(reg::TCA0_CTRLFCLR, TCA_ROW);
    false
}

/// Puts `output` right if it's been lost. A locked BOTTOM after `write_row`
/// checked its flags can clear all three, which tears nothing, but it drops
/// the row, and so does `write_row` giving up. So each pass compares what's
/// live with what was asked for, unless an update's still on its way. A TCA0
/// that's stopped leaves its flags up, so this leaves that alone.
pub fn keep_output(output: Output) {
    if read(reg::TCA0_CTRLFSET) & TCA_ROW == 0
        && (read16(reg::TCA0_PER).wrapping_add(1) != output.top
            || read16(reg::TCA0_CMP0) != output.fet
            || read16(reg::TCA0_CMP1) != output.linear)
    {
        set_output(output);
    }
}

/// Writes one level's period and compares. LUPD holds them all back until
/// they're all written, so they land on the same UPDATE (section 20.5.6).
/// Without it a BOTTOM between two writes would run a compare against the
/// wrong period for a cycle. Today's table only makes that a wrong duty, such
/// as 255 of the FET out of 4095, but a compare over PER would hold the pin
/// high, the FET fully on, and the lock doesn't depend on the table. If a
/// BOTTOM comes while it's locked, the update waits for the next one, up to
/// [`ts21::ramp::MAX_TOP`] clocks later. That the buffers stay valid over a
/// locked BOTTOM is likely, not stated: section 20.5.2's ALUPD can't work
/// otherwise. This doesn't depend on it, though: see [`write_row`] and
/// [`keep_output`]. LUPD clears before CNT's written, in case that counts as
/// BOTTOM.
///
/// Going off starts by handing the pins to PORT, whose OUT is 0, by clearing
/// CMPnEN. That's what really turns the light off. A waveform output is a
/// latch, set at BOTTOM and cleared on a compare match (section 20.3.3.4.3),
/// and CMPnEN overrides PORT (section 20.3.3.4.5), so a TCA0 that's stopped
/// with its output high holds the pin high whatever's written to CMPn. The
/// channels only come back with a level asked for, so a press can't bring
/// that back while the torch is off. That a stopped TCA0 holds its output like
/// this is likely, not stated, though section 20.5.3's CMPnOV does give the
/// waveform generator's output a value of its own while the timer's off.
///
/// Off doesn't wait for BOTTOM either. An UPDATE command lands the whole row
/// at once, LUPD or no LUPD (section 20.3.3.5), and BV clears on it, so
/// `prepare_sleep` can power down on this pass. Every level but off still
/// waits for BOTTOM, so a cycle's never cut short with the light on. The
/// compares get a direct write too, for a stopped TCA0, which never reaches
/// BOTTOM and which the datasheet doesn't promise takes a command either.
/// That write is always 0, so it can't go over PER, whichever row PER's still
/// in.
pub fn set_output(output: Output) {
    let off = output == Output::OFF;
    if off {
        write(reg::TCA0_CTRLB, regs::TCA_WGMODE_SINGLESLOPE);
    }
    write(reg::TCA0_CTRLESET, regs::TCA_LUPD);
    let whole = write_row(output);
    if off && whole {
        write(reg::TCA0_CTRLESET, regs::TCA_CMD_UPDATE);
    }
    write(reg::TCA0_CTRLECLR, regs::TCA_LUPD);
    if off {
        // The forced UPDATE can leave CNT over the off row's PER, where it'd
        // run on to 0xFFFF before it wrapped. This brings it back, and the
        // next turn-on starts from the same place.
        write16(reg::TCA0_CNT, 0);
        write16(reg::TCA0_CMP0, 0);
        write16(reg::TCA0_CMP1, 0);
    } else {
        write(reg::TCA0_CTRLB, tca_driving(output));
    }
}
