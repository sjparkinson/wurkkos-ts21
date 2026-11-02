//! The firmware: sample the switch, step the UI, set the PWM, 64 times a second.
//!
//! On the host this builds to a stub, so `cargo test` and `cargo clippy`
//! work across the workspace. `cargo fw` builds the real thing.

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
    };

    hw::init();

    let mut torch = Torch::new();
    let mut changes = Changes::new();

    loop {
        hw::wait_for_tick();
        torch.tick(hw::switch_pressed());
        if let Some(output) = changes.update(ramp::output(torch.level())) {
            hw::set_output(output);
        }
    }
}

#[cfg(target_arch = "avr")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    hw::set_output(ts21::ramp::Output::OFF);
    loop {}
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is firmware for the ATtiny1616; build it with `cargo fw`");
    std::process::exit(1);
}
