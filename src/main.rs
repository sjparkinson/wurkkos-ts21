//! The firmware: sample the switch, step the UI, set the PWM, 64 times a second.
//!
//! On the host this builds to a stub, so `cargo test` and `cargo clippy`
//! work across the workspace. `cargo xtask hex` builds the real thing and
//! writes the hex.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

#[cfg(target_arch = "avr")]
mod hw;
// The library owns src/regs.rs, since its self-tests check fuse fields with
// the same masks; hw.rs and rt.rs reach it as `crate::regs`.
#[cfg(target_arch = "avr")]
use ts21::regs;
#[cfg(target_arch = "avr")]
mod rt;

// src/rt.rs calls `main` by name once memory is set up.
#[cfg(target_arch = "avr")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    use ts21::{
        ramp,
        selftest::{Faults, NO_TICK, RamSweep},
        torch::{Changes, Torch},
        watchdog::{Feeder, Proof},
    };

    // What this reset means comes first, so the flags are clear and the
    // marker's right before anything can reset the chip again.
    let proof = Proof::after_reset(hw::take_reset_flags(), hw::marker());
    hw::set_marker(proof.marker());
    if proof.proving() {
        prove(proof);
    }

    // The self-tests come before anything's turned on. A failure doesn't
    // stop the torch, it quietly refuses the FET (src/selftest.rs).
    let faults = Faults {
        flash: !hw::flash_ok(),
        fuses: !hw::fuses().ok(),
        watchdog: proof.refused(),
    };

    let alive = hw::init(proof.watchdog());

    let mut torch = Torch::new(hw::temperature_calibration(), faults);
    let mut changes = Changes::new();
    let mut feeder = Feeder::new();
    let mut sweep = RamSweep::new();
    let mut owed_press = proof.owed_press();
    if !alive {
        torch.clock(NO_TICK);
    }

    loop {
        let ticked = next_tick(&mut torch);
        // First in the pass, so the CPU's quiet again before `adc_start`.
        // Not while resting: 0.5 ms awake every tick would add more to the
        // off current than the rest of the pass does, and there's no output
        // to guard. It picks up where it left off when the torch wakes. A
        // count ends on the tick, so a pass after one is no longer than any
        // other, and it reaches the feed as soon as it would have.
        if !torch.resting() {
            rt::test_ram_section(sweep.next_section());
        }
        torch.tick(hw::switch_pressed() || owed_press, hw::adc_result());
        owed_press = false;
        if let Some(convert) = torch.convert() {
            hw::adc_start(convert);
        }
        match changes.update(torch.level()) {
            Some(level) => hw::set_output(ramp::output(level)),
            None => hw::keep_output(ramp::output(torch.level())),
        }
        // The only place the running torch feeds the watchdog, and only on a
        // real tick, or a pass that a late tick woke would land it out of
        // step. A counted tick is a real one.
        if ticked && feeder.tick() {
            hw::feed_watchdog();
        }
    }
}

/// One step of the watchdog's proof (src/watchdog.rs). It never returns: every
/// step ends in a reset, on purpose or not, and the marker tells the next boot
/// which it was. The torch isn't running, so until that reset the outputs stay
/// at the 0 that `hw::init` leaves them.
///
/// Every wait here is a wait for a tick, and a dead 32 kHz oscillator stops
/// the WDT as well, so nothing would ever reset the chip. So a tick that never
/// comes ends the proof like a bite that never comes. The boot after that
/// finds the same dead oscillator in `hw::init`, and fails the clock check.
#[cfg(target_arch = "avr")]
fn prove(mut proof: ts21::watchdog::Proof) -> ! {
    use ts21::watchdog::Action;

    let mut alive = hw::init(proof.watchdog());
    // Step 1 can be over before its first tick.
    proof.sample(hw::switch_pressed());
    hw::set_marker(proof.marker());
    loop {
        // Not `rest`: nobody knows yet that a `wdr` survives power-down, and
        // TCB0's deadline stops in it.
        alive = alive && hw::wait_for_tick(false);
        let action = if alive {
            proof.tick(hw::switch_pressed())
        } else {
            proof.no_tick()
        };
        hw::set_marker(proof.marker());
        match action {
            Action::Wait => {}
            Action::Feed => hw::feed_watchdog(),
            Action::GiveUp => hw::software_reset(),
        }
    }
}

/// Waits for the next tick, counting the clock on the way if that's due, and
/// says whether it came.
#[cfg(target_arch = "avr")]
fn next_tick(torch: &mut ts21::torch::Torch) -> bool {
    use ts21::selftest::NO_TICK;

    if torch.clock_due() {
        let clocks = hw::count_tick();
        torch.clock(clocks);
        return clocks != NO_TICK;
    }
    let ticked = hw::wait_for_tick(torch.resting());
    if !ticked {
        torch.clock(NO_TICK);
    }
    ticked
}

// Turns the light off and spins. The watchdog isn't fed here, so it resets
// the chip 64 WDT clocks after the last feed, about 63 ms nominal, 48 to
// 89 ms over the oscillator's full spread, or about 8 s on a chip that failed
// its proof. It boots to off, and the next boot takes that as a hang and
// refuses the FET.
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
