//! Not firmware: a program that runs the real `hw::set_output` through every
//! row of `ramp::output`, for `cargo xtask hex` to watch. The UI only reaches
//! level 50 for now, so the firmware's own runs never move the FET's compare.
//!
//! After `hw::init`, `main` goes through each level from 0 up, and from each
//! one down to level 1, since a tear shows best where the FET's compare
//! changes along with the period. It calls `pwm_probe_landed` after every
//! write with the level in `PWM_PROBE_LEVEL`. xtask/src/sim.rs stops there,
//! brings on a BOTTOM and checks that row went live whole. It's never
//! flashed.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

// Only `init` and `set_output` get used, and what rt.rs needs.
#[cfg(target_arch = "avr")]
#[allow(dead_code)]
#[path = "../hw.rs"]
mod hw;
#[cfg(target_arch = "avr")]
use ts21::regs;
#[cfg(target_arch = "avr")]
#[path = "../rt.rs"]
mod rt;

#[cfg(target_arch = "avr")]
mod probe {
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    use ts21::ramp;

    use crate::hw;

    /// The level just written. `no_mangle` so sim.rs can find it by name.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    static PWM_PROBE_LEVEL: AtomicU8 = AtomicU8::new(0);

    fn set(level: u8) {
        hw::set_output(ramp::output(level));
        PWM_PROBE_LEVEL.store(level, Relaxed);
        pwm_probe_landed();
    }

    // src/rt.rs calls `main` by name once memory is set up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    extern "C" fn main() -> ! {
        // WDT.CTRLA at 0: nothing here feeds a watchdog. Whether the 32 kHz
        // oscillator's alive is for the real firmware to act on.
        hw::init(0);
        for level in 0..=ramp::LEVELS {
            set(level);
            set(1);
        }
        pwm_probe_done()
    }

    /// Where sim.rs stops after each write.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    extern "C" fn pwm_probe_landed() {
        // SAFETY: an empty block with no operands. It keeps the compiler
        // from dropping the call.
        unsafe { core::arch::asm!("", options(nostack, preserves_flags)) }
    }

    /// Where sim.rs stops for good. Nothing runs it on a real chip, so
    /// spinning costs nothing.
    #[allow(unsafe_code, clippy::empty_loop)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    extern "C" fn pwm_probe_done() -> ! {
        loop {}
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is a PWM check for the ATtiny1616; `cargo xtask hex` runs it");
    std::process::exit(1);
}
