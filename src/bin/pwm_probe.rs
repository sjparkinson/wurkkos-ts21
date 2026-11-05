//! Not firmware: a program that runs the real `hw::set_output` through every
//! row of `ramp::output`, for `cargo xtask hex` to watch. The UI only reaches
//! level 50 for now, so the firmware's own runs never move the FET's compare.
//!
//! After `hw::init`, `main` goes through each level from 0 up, and from each
//! one down to level 1, since a tear shows best where the FET's compare
//! changes along with the period. It calls `pwm_probe_landed` after every
//! write with the level in `PWM_PROBE_LEVEL`. xtask/src/sim.rs stops there,
//! brings on a BOTTOM and checks that row went live whole. Then it runs the
//! real `hw::count_tick` on the longest and shortest cycles and around the
//! jumps between them, for sim.rs to check against the clocks it set. It's
//! never flashed.

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

    /// What the firmware does around a count: a tick, a pass that sets the
    /// output, then the counted wait for the next tick, so the first cycle
    /// counted can still be the last level's.
    fn count(level: u8) {
        hw::wait_for_tick(false);
        hw::set_output(ramp::output(level));
        pwm_probe_counting(level);
        pwm_probe_counted(hw::count_tick(), level);
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
        // The longest cycle, then 16 times shorter straight after it, and back.
        for level in [1, 50, 1, ramp::LEVELS, 1, 0] {
            count(level);
        }
        pwm_probe_done()
    }

    /// Where sim.rs notes when the count starts, and how long a cycle was
    /// live going into it. `level` is in r24.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    extern "C" fn pwm_probe_counting(level: u8) {
        // SAFETY: as in `pwm_probe_landed`, with the argument kept live.
        unsafe { core::arch::asm!("", in("r24") level, options(nostack, preserves_flags)) }
    }

    /// Where sim.rs reads what a count came to: `clocks` in r22 to r25 and
    /// `level` in r20.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    extern "C" fn pwm_probe_counted(clocks: u32, level: u8) {
        let [a, b, c, d] = clocks.to_le_bytes();
        // SAFETY: as in `pwm_probe_landed`, with the arguments kept live.
        unsafe {
            core::arch::asm!(
                "",
                in("r22") a,
                in("r23") b,
                in("r24") c,
                in("r25") d,
                in("r20") level,
                options(nostack, preserves_flags),
            );
        }
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
