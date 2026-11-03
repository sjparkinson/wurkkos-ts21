//! The firmware: sample the switch, step the UI, set the PWM, 64 times a second.
//!
//! On the host this builds to a stub, so `cargo test` and `cargo clippy`
//! work across the workspace. `cargo fw` builds the real thing, and
//! `cargo xtask hex` builds it and writes the hex.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]

#[cfg(target_arch = "avr")]
mod hw;
#[cfg(target_arch = "avr")]
mod regs;
#[cfg(target_arch = "avr")]
mod rt;

// src/rt.rs calls `main` by name once memory is set up.
#[cfg(target_arch = "avr")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    use ts21::{
        ramp,
        torch::{Changes, Torch},
        watchdog::Feeder,
    };

    hw::init();

    let mut torch = Torch::new(hw::temperature_calibration());
    let mut changes = Changes::new();
    let mut feeder = Feeder::new();

    loop {
        hw::wait_for_tick();
        torch.tick(hw::switch_pressed(), hw::adc_result());
        if let Some(convert) = torch.convert() {
            hw::adc_start(convert);
        }
        if let Some(output) = changes.update(ramp::output(torch.level())) {
            hw::set_output(output);
        }
        // The only place the watchdog's fed.
        if feeder.tick() {
            hw::feed_watchdog();
        }
    }
}

// Turns the light off and spins. The watchdog isn't fed here, so it resets
// the chip (to off, as it boots) 64 WDT clocks after the last feed, about
// 63 ms nominal, 48 to 95 ms over the oscillator's full spread.
#[cfg(target_arch = "avr")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    hw::set_output(ts21::ramp::Output::OFF);
    loop {}
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!(
        "this is firmware for the ATtiny1616, so it doesn't run here.\n\
         `cargo xtask hex` builds target/wurkkos-ts21-0614.hex, \
         `cargo xtask simulate` tries the UI in your terminal, \
         and `cargo xtask` lists the rest."
    );
    std::process::exit(1);
}
