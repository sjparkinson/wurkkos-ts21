//! Everything that touches the chip. There are five entry points and no state:
//! what isn't in a register isn't remembered.
//!
//! `power_up` is the only set-up there is. Boot and every wake from power-down go
//! through it, so the path that runs once a press is the path that runs at reset.
//!
//! Reset leaves every pin an input and the timer off. That's dark, so anything
//! that ends in a reset (the watchdog, a panic) fails safe by itself.

use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, write_volatile};
use ts21_clean::ramp;
use ts21_clean::torch::{Aux, Input, Output, Reading};

include!(concat!(env!("OUT_DIR"), "/regs.rs"));
use addr::*;
use bits::*;

const BUTTON: u8 = 1 << 5; // PA5
const AUX: u8 = 1 << 5; // PB5
const FET: u8 = 1 << 0; // PB0, TCA0 WO0
const LINEAR: u8 = 1 << 1; // PB1, TCA0 WO1

// Only the vectors up to RTC PIT exist, because nothing later is ever enabled,
// and the code starts straight after them. The handler needs no SREG save: push,
// ldi, sts and pop don't touch the flags. One handler serves both wake sources,
// and all it does is clear the flags, since waking up was the whole message.
global_asm!(
    r#"
    .section .vectors, "ax", @progbits
    .global reset
reset:
    jmp start // 0 RESET
    jmp start // 1 NMI
    jmp start // 2 VLM
    jmp wake  // 3 PORTA
    jmp start // 4 PORTB
    jmp start // 5 PORTC
    jmp start // 6 RTC CNT
    jmp wake  // 7 RTC PIT
start:
    clr r1
    jmp main
wake:
    push r16
    ldi r16, 0xff
    sts {porta_flags}, r16
    ldi r16, {pit_flag}
    sts {pit_flags}, r16
    pop r16
    reti
    "#,
    porta_flags = const PORTA_INTFLAGS,
    pit_flags = const RTC_PITINTFLAGS,
    pit_flag = const RTC_PITINTFLAGS_PI,
);

#[inline(always)]
fn write(address: u16, value: u8) {
    // SAFETY: every address comes from the ATDF-generated constants, and each is
    // a peripheral register, so a write has no effect on Rust's memory.
    unsafe { write_volatile(address as *mut u8, value) }
}

#[inline(always)]
fn read(address: u16) -> u8 {
    // SAFETY: as for `write`.
    unsafe { read_volatile(address as *const u8) }
}

/// Low byte first: the high byte's write is what commits both through TEMP.
#[inline(always)]
fn write16(address: u16, value: u16) {
    write(address, value as u8);
    write(address + 1, (value >> 8) as u8);
}

/// Low byte first, which latches the high byte into TEMP.
#[inline(always)]
fn read16(address: u16) -> u16 {
    let low = read(address);
    u16::from(low) | u16::from(read(address + 1)) << 8
}

/// Writes a register behind configuration change protection. The write has to
/// land within four instructions of the key, so the pair is one `asm!`.
macro_rules! protected_write {
    ($address:expr, $value:expr) => {
        // SAFETY: two I/O writes, no memory or stack involved.
        unsafe {
            asm!(
                "out {ccp}, {key}",
                "sts {address}, {value}",
                ccp = const CPU_CCP,
                address = const $address,
                key = in(reg) CPU_CCP_CCP_IOREG,
                value = in(reg) $value,
                options(nostack, preserves_flags),
            )
        }
    };
}

fn wait_while_set(address: u16, mask: u8) {
    while read(address) & mask != 0 {}
}

fn set_watchdog(period: u8) {
    wait_while_set(WDT_STATUS, WDT_STATUS_SYNCBUSY);
    protected_write!(WDT_CTRLA, period);
}

/// Configures everything from whatever state it was left in.
pub fn power_up() {
    // 20 MHz / 4. The datasheet allows 5 MHz down to 1.8 V, which covers the
    // deepest sag a dying cell can put on VCC.
    protected_write!(
        CLKCTRL_MCLKCTRLB,
        CLKCTRL_MCLKCTRLB_PDIV_4X | CLKCTRL_MCLKCTRLB_PEN
    );
    set_up_pins();

    write(TCA0_SINGLE_CTRLB, TCA_SINGLE_CTRLB_WGMODE_SINGLESLOPE);
    write16(TCA0_SINGLE_PER, ramp::pwm(ramp::OFF).period);
    write(
        TCA0_SINGLE_CTRLA,
        TCA_SINGLE_CTRLA_CLKSEL_DIV1 | TCA_SINGLE_CTRLA_ENABLE,
    );

    write(VREF_CTRLA, VREF_CTRLA_ADC0REFSEL_1V1);
    write(VREF_CTRLB, VREF_CTRLB_ADC0REFEN);
    write(ADC0_CTRLB, ADC_CTRLB_SAMPNUM_ACC4);
    write(ADC0_CTRLD, ADC_CTRLD_INITDLY_DLY32);
    // The temperature sensor wants 32 µs, which is 20 at 625 kHz. This adds margin.
    write(ADC0_SAMPCTRL, 24);
    write(ADC0_CTRLA, ADC_CTRLA_ENABLE);
    // MUXPOS is the record of what a result is, so a result from before the sleep
    // has to go before it's mistaken for this battery reading.
    write(ADC0_INTFLAGS, ADC_INTFLAGS_RESRDY);
    start_battery_reading();

    wait_while_set(RTC_PITSTATUS, RTC_PITSTATUS_CTRLBUSY);
    write(RTC_PITINTCTRL, RTC_PITINTCTRL_PI);
    write(
        RTC_PITCTRLA,
        RTC_PITCTRLA_PERIOD_CYC512 | RTC_PITCTRLA_PITEN,
    );

    set_watchdog(WDT_CTRLA_PERIOD_1KCLK);
    write(SLPCTRL_CTRLA, SLPCTRL_CTRLA_SMODE_IDLE | SLPCTRL_CTRLA_SEN);
}

fn set_up_pins() {
    // Unused pins get their input buffers switched off, not pull-ups: nothing
    // floats into a buffer that isn't there, and if the board has wired one to
    // something, this can't source current into it.
    // PA0 is UPDI, the only way back in, so it's left exactly as reset leaves it.
    for pin in 1..8 {
        write(PORTA_PIN0CTRL + pin, PORT_PIN0CTRL_ISC_INPUT_DISABLE);
    }
    for pin in 0..8 {
        write(PORTB_PIN0CTRL + pin, PORT_PIN0CTRL_ISC_INPUT_DISABLE);
    }
    for pin in 0..4 {
        write(PORTC_PIN0CTRL + pin, PORT_PIN0CTRL_ISC_INPUT_DISABLE);
    }
    write(PORTA_PIN5CTRL, PORT_PIN5CTRL_PULLUPEN);
    write(PORTB_PIN5CTRL, PORT_PIN5CTRL_PULLUPEN);
    write(PORTB_OUTCLR, FET | LINEAR);
    write(PORTB_DIRSET, FET | LINEAR);
}

pub fn enable_interrupts() {
    // SAFETY: the handler only clears flags.
    unsafe { asm!("sei", options(nostack, nomem)) }
}

/// The chip's own temperature calibration, as `(gain, offset)`.
pub fn heat_calibration() -> (u8, i8) {
    (read(SIGROW_TEMPSENSE0), read(SIGROW_TEMPSENSE1) as i8)
}

fn start_battery_reading() {
    write(
        ADC0_CTRLC,
        ADC_CTRLC_SAMPCAP | ADC_CTRLC_REFSEL_VDDREF | ADC_CTRLC_PRESC_DIV8,
    );
    write(ADC0_MUXPOS, ADC_MUXPOS_MUXPOS_INTREF);
    write(ADC0_COMMAND, ADC_COMMAND_STCONV);
}

fn start_heat_reading() {
    write(
        ADC0_CTRLC,
        ADC_CTRLC_SAMPCAP | ADC_CTRLC_REFSEL_INTREF | ADC_CTRLC_PRESC_DIV8,
    );
    write(ADC0_MUXPOS, ADC_MUXPOS_MUXPOS_TEMPSENSE);
    write(ADC0_COMMAND, ADC_COMMAND_STCONV);
}

/// Reads the button and collects the ADC result, starting the other channel.
/// MUXPOS is the record of which channel this result came from.
pub fn sample() -> Input {
    let pressed = read(PORTA_IN) & BUTTON == 0;
    if read(ADC0_INTFLAGS) & ADC_INTFLAGS_RESRDY == 0 {
        return Input {
            pressed,
            reading: None,
        };
    }
    let raw = read16(ADC0_RES);
    let reading = if read(ADC0_MUXPOS) == ADC_MUXPOS_MUXPOS_INTREF {
        start_heat_reading();
        Reading::Battery(raw)
    } else {
        start_battery_reading();
        Reading::Heat(raw)
    };
    Input {
        pressed,
        reading: Some(reading),
    }
}

pub fn drive(output: Output) {
    let pwm = ramp::pwm(output.level);
    // Locked, so a period boundary can't land between these and load half of them.
    write(TCA0_SINGLE_CTRLESET, TCA_SINGLE_CTRLESET_LUPD);
    write16(TCA0_SINGLE_PERBUF, pwm.period);
    write16(TCA0_SINGLE_CMP0BUF, pwm.fet);
    write16(TCA0_SINGLE_CMP1BUF, pwm.linear);
    write(TCA0_SINGLE_CTRLECLR, TCA_SINGLE_CTRLECLR_LUPD);
    // Off hands the pins back to PORT, which holds them low, so the light goes
    // out now and not at the end of a period that might be 800 µs long.
    let channels = if output.level == ramp::OFF {
        0
    } else {
        TCA_SINGLE_CTRLB_CMP0EN | TCA_SINGLE_CTRLB_CMP1EN
    };
    write(
        TCA0_SINGLE_CTRLB,
        channels | TCA_SINGLE_CTRLB_WGMODE_SINGLESLOPE,
    );
    match output.aux {
        Aux::Off => write(PORTB_DIRSET, AUX),
        Aux::Low => write(PORTB_DIRCLR, AUX),
    }
    // SAFETY: resets the watchdog, nothing else.
    unsafe { asm!("wdr", options(nostack, nomem, preserves_flags)) }
}

/// Sleeps until the next tick. The timer keeps running, so the light stays on.
pub fn idle() {
    // SAFETY: returns after the next interrupt, and the handler only clears flags.
    unsafe { asm!("sleep", options(nostack, nomem, preserves_flags)) }
}

/// Sleeps until the button changes, then comes back up as if from reset.
pub fn power_down() {
    // SAFETY: only masks interrupts, until `sei` below.
    unsafe { asm!("cli", options(nostack, nomem, preserves_flags)) }
    // The datasheet doesn't say what a stopped timer does to its pins, so they go
    // back to PORT first.
    write(TCA0_SINGLE_CTRLB, 0);
    write(TCA0_SINGLE_CTRLA, 0);
    write(ADC0_CTRLA, 0);
    write(VREF_CTRLB, 0);
    wait_while_set(RTC_PITSTATUS, RTC_PITSTATUS_CTRLBUSY);
    write(RTC_PITCTRLA, 0);
    wait_while_set(RTC_PITSTATUS, RTC_PITSTATUS_CTRLBUSY);
    // Both run in power-down, and the watchdog would reset the chip, so wait to
    // see the disable land. A tick flagged by now would only be a spurious wake.
    set_watchdog(0);
    wait_while_set(WDT_STATUS, WDT_STATUS_SYNCBUSY);
    write(RTC_PITINTFLAGS, RTC_PITINTFLAGS_PI);
    write(PORTA_INTFLAGS, BUTTON);
    // PA5 isn't fully asynchronous, so only BOTHEDGES wakes it from power-down.
    write(
        PORTA_PIN5CTRL,
        PORT_PIN5CTRL_PULLUPEN | PORT_PIN5CTRL_ISC_BOTHEDGES,
    );
    // Armed last, then checked: a press before this would sleep through its edge.
    if read(PORTA_IN) & BUTTON != 0 {
        write(SLPCTRL_CTRLA, SLPCTRL_CTRLA_SMODE_PDOWN | SLPCTRL_CTRLA_SEN);
        // SAFETY: `sei` delays interrupts by one instruction, so an edge since the
        // `cli` is taken after `sleep` starts, and wakes it rather than being lost.
        unsafe { asm!("sei", "sleep", options(nostack, nomem, preserves_flags)) }
    } else {
        enable_interrupts();
    }
    write(PORTA_PIN5CTRL, PORT_PIN5CTRL_PULLUPEN);
    power_up();
}

/// Restarts the chip, which leaves everything dark.
pub fn reset() -> ! {
    protected_write!(RSTCTRL_SWRR, RSTCTRL_SWRR_SWRE);
    loop {
        core::hint::spin_loop();
    }
}
