//! The firmware: wake, sample, tick, drive, sleep. All the state is in `torch`,
//! on the stack, because no interrupt handler has anything to tell this loop
//! beyond waking it up.
#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]

#[cfg(target_arch = "avr")]
mod board;

#[cfg(target_arch = "avr")]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    board::power_up();
    board::enable_interrupts();
    let (gain, offset) = board::heat_calibration();
    let mut torch = ts21_clean::torch::Torch::new(gain, offset);
    loop {
        let output = torch.tick(board::sample());
        board::drive(output);
        if output.sleep {
            board::power_down();
        } else {
            board::idle();
        }
    }
}

#[cfg(target_arch = "avr")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    board::reset()
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is firmware: run ./hex.sh");
    std::process::exit(1);
}
