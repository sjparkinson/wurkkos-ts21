//! A fake ATtiny1616 around `avr.rs`'s CPU, and the checks that run the
//! real firmware on it. `build()` in main.rs runs them after `layout.rs`'s,
//! so `size`, `hex` and `flash` all refuse firmware that fails, and CI runs
//! them through `size`.
//!
//! The fake chip models only the registers the firmware touches, and turns
//! the rules the real chip enforces quietly into errors:
//!
//! - A protected register only takes a write within four instructions of
//!   the CCP key. The real chip ignores a late write. Interrupts wait out
//!   those four instructions, or until the first store or `sleep` (§8.5.7).
//! - TCA0's 16-bit registers are written low byte, then high byte, through
//!   the shared TEMP byte. The other way round, the real chip pairs the high
//!   byte with a stale low one. A handler that lands with one of them
//!   halfway through an access, reading or writing, can't touch any of them,
//!   since they all share that byte (§8.5.6, §20.5.13).
//! - A handler gives back every register, SREG and SP as it found them.
//! - RTC.PITCTRLA isn't written while PITSTATUS.CTRLBUSY is set. The real
//!   chip drops that write.
//! - The watchdog: WDT.CTRLA takes the CCP key and isn't written while
//!   WDT.STATUS.SYNCBUSY is set, reserved settings and LOCK are refused,
//!   and so is a `wdr` before the CTRLA write has synced. A `wdr` that lands
//!   in the closed window fails, and so does running out the open window
//!   without one, as the real chip would reset either way. Window mode
//!   starts at the first `wdr`, as on the chip (§19.3.3.2). A feed exactly
//!   on a window edge fails too: `wdr` takes two to three WDT clocks to
//!   sync, so on the chip it's a coin toss. `hw::init` has to turn it on,
//!   and nothing may change it after that, off included. Those are errors
//!   only while nothing's expecting a bite; otherwise it resets the chip.
//! - Reset: RSTCTRL.RSTFR is sticky, with a flag for each reset since it was
//!   last cleared by writing 1, and RSTCTRL.SWRR takes the CCP key. A reset
//!   puts every register back and restarts the CPU, but leaves SRAM alone.
//!   The datasheet doesn't promise that; AN2632 §6 leans on it.
//! - ADC0 only converts when it's set up for one of the two measurements the
//!   firmware makes (the 1.5 V reference against VDD, or the temperature
//!   sensor against 1.1 V with the datasheet's delays and clock, each four
//!   accumulated), and then reads the scripted world (`world.rs`). Starting a
//!   conversion mid-conversion, changing its setup mid-conversion, reading
//!   RES before RESRDY or its high byte first are errors. The first
//!   conversion after a change of reference or input reads 0, which the
//!   firmware throws away. One that didn't would see garbage the library
//!   never does and cap itself at level 75, which only shows against the
//!   library once the UI goes above that.
//! - SIGROW.TEMPSENSE0 and 1 hold `world::CALIBRATION`.
//! - The fuses read as the world's (`world::FACTORY_FUSES` unless a script
//!   says otherwise), and writing any of them is an error.
//! - The PIT's flag goes up once a period of simulated time, which counts
//!   an instruction at a time while the CPU's awake and skips to the end of
//!   the period while it sleeps. Reading the flag doesn't move time on, so
//!   firmware that busy-waited on it would run out of instructions.
//! - Interrupts come from the PIT (with RTC.PITINTCTRL.PI) and from PORTA's
//!   pin-change flags, which a switch edge sets when PA5's ISC asks for that
//!   edge. Enabling any interrupt, the PIT's or a port pin's, with SREG.I
//!   set and its vector going to `__unhandled_interrupt` is an error, and
//!   so is taking one: the real chip would return with the flag still up
//!   and take it again, forever.
//! - `sleep` needs SLPCTRL.SEN (the chip treats it as a nop otherwise), idle
//!   or power-down, the only modes modelled, and an enabled interrupt that
//!   could wake it, which in power-down leaves out RISING and FALLING on all
//!   but pins 2 and 6 (table 16-3). `avr.rs` adds that SREG.I is set and no handler's
//!   running. Power-down also wants nothing left that it would freeze or
//!   keep burning: no conversion running (§30.3.5 resumes it on waking, with
//!   a result that's invalid), the ADC off, the reference not forced on, and
//!   TCA0's channels off the pins with both compares at 0.
//! - The clock has to be 5 MHz, and TCA0 single-slope with no prescaler:
//!   writes of anything else are refused, and after `hw::init` a write that
//!   never happened fails too. PER has to have left its reset value by then.
//! - TCA0 counts to the live PER, one count per step of simulated time, and
//!   at each BOTTOM PER, CMP0 and CMP1 take whichever of their
//!   BUF registers have been written, unless CTRLE.LUPD is set
//!   (DS40002204A §20.3.3.2 and table 20-6). Whatever goes live has to be a
//!   whole row of `ramp::output`: a compare from one level run against
//!   another's period is an error, and above all a compare over PER, which
//!   holds the pin high for the cycle, the FET fully on. A BOTTOM only lands
//!   between two instructions now and then, so writing PERBUF or a CMPnBUF
//!   with the timer running and LUPD clear is checked as if one landed
//!   straight after that write. `check` shows that catches firmware with the
//!   lock taken out. Table 20-1 has BOTTOM reached "when it becomes 0x0000",
//!   which doesn't settle whether writing CNT counts, so this guesses that it
//!   doesn't and pushes BOTTOM a full cycle away. The real wait can be far shorter, so the firmware mustn't lean on
//!   a pass that writes the output staying in idle: `Torch::tick` rests in
//!   that same pass. CNT only takes 0 while running. Sleeping with LUPD set
//!   is an error. TCA0 stops in power-down, since §20.3.6 only promises idle,
//!   so powering down also wants no BV set and PER back at the off output's.
//!   CTRLA only takes the one setting, and CTRLB refuses ALUPD. WO0 and WO1
//!   are latches, set at BOTTOM and cleared on a match, that hold when the
//!   timer isn't counting, and `Chip::bv_adverse` is the other reading of a
//!   BOTTOM under LUPD.
//! - CRCSCAN runs CRC-16-CCITT over all of flash and sets STATUS.OK if it
//!   comes to 0, which is what a correct checksum in the last two bytes
//!   does (`crc.rs`). The firmware runs on the image `cargo xtask hex`
//!   writes, so this checks xtask's sum too. STATUS reads as it was for the
//!   three instructions after ENABLE, since the scan only starts three
//!   cycles later, then BUSY for a couple of reads. Only the whole-flash
//!   scan with STATUS polled is modelled; NMIEN is an error.
//! - The world sets how fast the 32 kHz oscillator and the main clock run.
//!   Time here counts main clocks, so the two only change how many of them
//!   fit in a PIT period (`Chip::pit_cycles`); a PIT period is always 16 WDT
//!   clocks. A dead oscillator stops the PIT and the WDT, and leaves their
//!   sync flags up for good. The time a tick would've ended still counts
//!   in `Chip::ticks`, or nothing here would ever finish.
//! - TCA0.INTFLAGS.OVF goes up at each BOTTOM, a whole cycle of the live PER
//!   apart, unless the world's stopped TCA0.
//! - TCB0 only runs as the deadline for a tick: periodic interrupt mode at
//!   CLK_PER / 2 with TOP at its maximum, and it stops in power-down.
//!
//! Left to itself, the chip nearly always interrupts while the CPU's asleep,
//! since that's where the PIT's period ends. The exception is the end of a
//! counted wait, where TCB0 can be waiting at its `sei`. `Landings` makes them
//! land everywhere else: see it for how.
//!
//! Touching a register it doesn't model is an error too, so new peripheral
//! code fails here until the model learns about it.
//!
//! It can also break the chip for src/rt.rs's register and RAM tests to
//! find: a stuck bit or two shorted bits in a CPU register, forced before
//! every instruction, or a byte of SRAM with a stuck bit, two shorted bits,
//! an address that reaches another byte, or a store that disturbs another
//! byte (`world::RamFault`). The stack checks wait until those tests are
//! done, since they point SP all over SRAM.
//!
//! And it can break the watchdog for the proof in `ts21::watchdog` to find
//! (`world::WdtFault`), or stop the CPU dead, for a hang to be reset from.
//!
//! With that in place it runs eight things:
//!
//! 1. Reset to `main`, with SRAM full of junk, checking the self-tests pass
//!    and .data and .bss come out right.
//! 2. The same startup code with .data and .bss moved and given contents,
//!    because the firmware has no statics yet, so as linked both loops in
//!    src/rt.rs run zero times.
//! 3. Power-on with every stuck register bit and a spread of RAM faults
//!    (`startup_faults`), then every short between two bits of a register
//!    (`register_shorts`) and coupling between bytes (`ram_coupling`),
//!    checking each one stops in `__selftest_failed` with the light dark.
//! 4. The periodic RAM test with the light on: a whole sweep on a good chip
//!    (`periodic_passes`), what each of its checks is there for
//!    (`periodic_checks`), then RAM going bad mid-run (`periodic_faults`).
//! 5. The watchdog's proof (`watchdog_proof`): on a good chip, after a UPDI
//!    reset, with every broken watchdog and with a press on each of its
//!    boots, checking the pins on every instruction and that the right
//!    step found each fault. Every example and the random switching run
//!    the proof again first.
//! 6. Every example in docs/ui.md (see `spec.rs`), each on a fresh chip,
//!    with the cell, temperature, broken ADC, fuses, corrupt flash or
//!    broken RAM, register or watchdog the example scripts.
//! 7. Twenty thousand ticks of random clicks and holds, with a good cell at
//!    room temperature.
//! 8. Both of those again with `Landings` on, which has to catch a handler
//!    between a TCA0 low and high byte at least once.
//!
//! `probe` separately runs src/bin/link_probe.rs, which has real statics,
//! and checks it reads them from where link.x put them. `pwm_probe` runs
//! src/bin/pwm_probe.rs, the real `hw::set_output` through every level.
//!
//! The last three drive `ts21::torch` alongside the firmware and check the
//! LEDs show the library's level on every tick; the examples then check
//! where the library ended up.
//!
//! Throughout, it measures how deep the stack goes and checks every pass of
//! the main loop fits in a 64 Hz tick.

use std::iter;
use std::mem;
use std::ops::Range;

use ts21::button::RELEASE_TICKS;
use ts21::ramp;
use ts21::selftest::{self, RamSweep};
use ts21::sensors::Channel;
use ts21::watchdog::{self, Step};

use crate::avr::{Bus, Cpu, I, VECTOR_WORDS, is_two_words};
use crate::crc;
use crate::elf::{DATA_SPACE, Elf, RAM};
use crate::simulate::{self, Sample};
use crate::spec::{self, Example};
use crate::switching::RandomSwitching;
use crate::updi::map;
use crate::world::{self, CALIBRATION, RamFault, Register, RegisterFault, Rig, WdtFault, World};

use ts21::regs::{self, addr as reg};
// Beyond what the firmware uses, for the simulator to refuse or to time.
use ts21::regs::{
    PORT_INVEN, PORT_ISC as ISC, PORT_ISC_BOTHEDGES as ISC_BOTHEDGES,
    PORT_ISC_FALLING as ISC_FALLING, PORT_ISC_INPUT_DISABLE as ISC_INPUT_DISABLE,
    PORT_ISC_INTDISABLE as ISC_INTDISABLE, PORT_ISC_LEVEL as ISC_LEVEL,
    PORT_ISC_RISING as ISC_RISING, TCA_WGMODE, VECTOR_PORTA_PORT, VECTOR_PORTB_PORT,
    VECTOR_PORTC_PORT, WDT_LOCK, WDT_PERIOD, WDT_PERIOD_8KCLK as WDT_PERIOD_MAX, WDT_WINDOW,
    addr::PORTA_INTFLAGS,
};

/// SRAM as data-space addresses.
const SRAM: Range<u16> = (RAM.start - DATA_SPACE) as u16..(RAM.end - DATA_SPACE) as u16;
/// Where flash appears in data space, and how much of it there is.
const MAPPED: u16 = map::FLASH;
const FLASH_SIZE: usize = map::FLASH_SIZE;

/// SRAM's contents before startup. Real SRAM powers up holding anything; a
/// fixed pattern makes a byte that should have been written stand out.
const JUNK: u8 = 0xA5;

// The pins, from docs/hardware.md, as in src/hw.rs.
const SWITCH: u8 = 1 << 5; // PA5, active low
const FET: u8 = 1 << 0; // PB0, TCA0 WO0
const LINEAR: u8 = 1 << 1; // PB1, TCA0 WO1

/// Instructions startup may take before reaching `main`. Nearly all of it
/// is src/rt.rs's RAM test, about 200,000.
const BOOT_BUDGET: u64 = 400_000;
/// CPU cycles in a 64 Hz tick at 5 MHz.
const TICK_CYCLES: u64 = 5_000_000 / 64;
/// Instructions one pass of the main loop may take. No AVRxt instruction
/// takes more than 4 cycles, so this many always fit in a tick.
const TICK_BUDGET: u64 = TICK_CYCLES / 4;
/// The PIT's period on the simulated chip's clock, which counts an
/// instruction a tick of `Chip::now`: no instruction takes less than a
/// cycle, so this is the most a real tick could fit. It's well past
/// `TICK_BUDGET`, so a pass that overran a tick fails on that first.
const PIT_PERIOD: u64 = TICK_CYCLES;

/// How many reads of RTC.PITSTATUS report busy after reset or a write to
/// PITCTRLA. The real chip is busy for a couple of 32 kHz clocks while it
/// syncs; any non-zero number makes the firmware wait.
const PIT_SYNC_READS: u8 = 2;

/// Reads of WDT.STATUS that report SYNCBUSY after a write to CTRLA, like
/// `PIT_SYNC_READS`.
const WDT_SYNC_READS: u8 = 2;
/// WDT clocks in a tick: the PIT divides the same oscillator by 512, and
/// the WDT by 32. Only at CYC512, so a tick refuses any other PIT period
/// while the WDT's on.
const WDT_CLOCKS_PER_TICK: u32 = 16;
/// What the error says when the watchdog runs out, which a `wdr` in the
/// closed window's error doesn't.
const WDT_TIMED_OUT: &str = "without wdr";
// The WDT model counts whole ticks, which only works while a pass can't
// overrun one and drop a tick unseen.
const _: () = assert!(TICK_BUDGET < TICK_CYCLES / 2);

/// TCA0's 16-bit registers the firmware writes, by the low byte's address.
const WIDE: [usize; 5] = [
    reg::TCA0_CNT,
    reg::TCA0_PER,
    reg::TCA0_PERBUF,
    reg::TCA0_CMP0BUF,
    reg::TCA0_CMP1BUF,
];

#[derive(Default)]
pub struct Report {
    /// The most stack the firmware used, in bytes.
    pub stack: u16,
    /// The most instructions one pass of the main loop took.
    pub loop_instructions: u64,
    /// What that's measured against.
    pub loop_budget: u64,
    /// The most instructions one pass took that started off resting and
    /// ended in power-down, which is what the chip stays awake for, 64 times
    /// a second, when it's off.
    pub off_instructions: u64,
    /// Handlers that landed with a TCA0 16-bit access half done.
    pub mid_access: u32,
    /// The longest pass of the runs with something extra in them, which
    /// `loop_instructions` leaves out.
    pub worst_pass: u64,
    /// Real ticks that `Landings` put on `hw::wait_for_tick`'s loop.
    pub ticks_landed: u32,
    /// `tcb0_int`s that `Landings` set off.
    pub deadlines_landed: u32,
    /// Times a locked BOTTOM cleared a buffer's BV, under `Chip::bv_adverse`.
    pub bv_cleared: u32,
}

/// Ticks the library can be resting before the firmware has to be powering
/// down: the pass that turns the light off stays in idle while the compares
/// settle.
const REST_GRACE_TICKS: u32 = 1;

pub fn check(elf: &Elf) -> Result<Report, String> {
    let symbol = |name| {
        elf.symbol(name)
            .ok_or_else(|| format!("no `{name}` symbol"))
    };
    let sram = |name| -> Result<u16, String> { Ok((symbol(name)? - DATA_SPACE) as u16) };
    let main = symbol("main")? / 2;
    let failed = symbol("__selftest_failed")? / 2;
    let ram_tested = symbol("__ram_tested")?;
    let unhandled = symbol("__unhandled_interrupt")?;
    let image = elf.flash_image();
    // What `cargo xtask hex` and `flash` write: the image padded to all of
    // flash, with its CRC at the end for the firmware's CRCSCAN check.
    let signed = crc::sign(&image)?;
    let linked = Layout {
        data: sram("__data_start")?..sram("__data_end")?,
        bss: sram("__bss_start")?..sram("__bss_end")?,
        load: symbol("__data_load")? as u16,
    };
    let marker = sram("__noinit_start")?;
    if sram("__noinit_end")? - marker != 2 {
        return Err("`.noinit` isn't the watchdog's two-byte marker".into());
    }

    // A chip that's just had its cell put in, in `world`, run to the end of
    // startup.
    let power_on_image = |image: Vec<u8>, world: &World| -> Result<(Sim, Boot), String> {
        let mut sim = Sim::new(image, linked.bss.end, unhandled)?.testing_until(ram_tested);
        sim.marker = marker;
        sim.chip.world = *world;
        let boot = sim.boot(main, failed)?;
        Ok((sim, boot))
    };
    let power_on = |world: &World| power_on_image(signed.clone(), world);

    let (sim, boot) = power_on(&World::NORMAL).map_err(|e| format!("startup: {e}"))?;
    if let Boot::Failed { from } = boot {
        return Err(format!(
            "startup: the self-tests in src/rt.rs failed on a good chip, at {:#06x}",
            from * 2
        ));
    }
    check_startup(&sim.chip, &linked).map_err(|e| format!("startup: {e}"))?;

    startup_with_statics(&image, ram_tested, main, &linked, unhandled)
        .map_err(|e| format!("startup with statics: {e}"))?;

    startup_faults(&power_on, symbol("__registers_tested")? / 2, ram_tested / 2)
        .map_err(|e| format!("startup self-tests: {e}"))?;

    let marks = Marks {
        march_down_check: symbol("__march_down_check")? / 2,
        march_last: symbol("__march_last")? / 2,
        ram_tested: ram_tested / 2,
        register_shorts: symbol("__register_shorts")? / 2,
        register_shorts_end: symbol("__register_shorts_end")? / 2,
        registers_tested: symbol("__registers_tested")? / 2,
    };
    register_shorts(&power_on, &power_on_image, &signed, &marks)
        .map_err(|e| format!("register shorts: {e}"))?;
    ram_coupling(&power_on, &power_on_image, &signed, &marks)
        .map_err(|e| format!("RAM coupling: {e}"))?;

    let section = symbol("__ram_section")? / 2;
    let section_failed = symbol("__ram_section_failed")? / 2;
    let section_marks = SectionMarks {
        section,
        up: symbol("__ram_section_up_check")? / 2,
        down: symbol("__ram_section_down_check")? / 2,
        last: symbol("__ram_section_last_check")? / 2,
        failed: section_failed,
    };
    periodic_passes(&power_on, section).map_err(|e| format!("periodic RAM test: {e}"))?;
    periodic_checks(&power_on, &section_marks).map_err(|e| format!("periodic RAM test: {e}"))?;
    let late = LateMarks {
        section,
        section_failed,
        main,
        selftest_failed: failed,
        march_after_hang: symbol("__march_after_hang")? / 2,
        ram_tested: ram_tested / 2,
    };
    periodic_faults(&power_on, &late).map_err(|e| format!("periodic RAM test: {e}"))?;
    jump_to_reset(&power_on, &late).map_err(|e| format!("a jump to the reset vector: {e}"))?;

    let mut report = Report {
        stack: 0,
        loop_instructions: 0,
        loop_budget: TICK_BUDGET,
        off_instructions: 0,
        mid_access: 0,
        worst_pass: 0,
        ticks_landed: 0,
        deadlines_landed: 0,
        bv_cleared: 0,
    };
    watchdog_proof(&power_on, main, &mut report).map_err(|e| format!("watchdog proof: {e}"))?;
    for example in spec::examples(spec::UI_MD)? {
        let samples = simulate::samples(&example.actions);
        let world = samples.first().map_or(World::NORMAL, |s| s.world);
        let (sim, boot) = power_on(&world).map_err(|e| format!("{}: startup: {e}", example.row))?;
        ui_example(sim, boot, samples, &example, &mut report)
            .map_err(|e| format!("{}: {e}", example.row))?;
    }
    against_the_library(sim, &mut report)
        .map_err(|e| format!("random switching, against the library: {e}"))?;
    mid_pass(&power_on, &mut report).map_err(|e| format!("with interrupts mid-pass: {e}"))?;
    if report.off_instructions == 0 {
        return Err("never powered down".into());
    }
    lock_taken_out(&power_on)?;
    off_with_tca0_dead(&power_on).map_err(|e| format!("off with TCA0 dead: {e}"))?;
    linear_with_wo0_high(&power_on).map_err(|e| format!("a linear level, WO0 high: {e}"))?;
    adverse_bv(&power_on, &mut report).map_err(|e| format!("with BV cleared under LUPD: {e}"))?;
    // The clock check's floor allows for this much of a tick going uncounted,
    // on any run.
    let longest = report.loop_instructions.max(report.worst_pass);
    if longest > u64::from(selftest::PASS_INSTRUCTIONS) {
        return Err(format!(
            "a pass took {longest} instructions, and `selftest::PASS_INSTRUCTIONS` is {}, \
             so the clock check's floor doesn't cover a good chip",
            selftest::PASS_INSTRUCTIONS
        ));
    }
    Ok(report)
}

/// A torch that's on when TCA0 stops with a waveform output high, never reaches
/// BOTTOM again and ignores the UPDATE command as well, then gets clicked off.
/// Writing a compare doesn't move a stopped output, so the light only goes out
/// if `hw::set_output` takes the pins off TCA0. With the buffers stuck full it
/// can't power down, so that's all this asks for, along with a press not
/// bringing the output back while it counts.
///
/// Once at level 150, with the FET fully on. The UI can't get there yet, so
/// that row's put in by hand. Once at the UI's own level, a lower duty,
/// stopped in the high part of a cycle.
fn off_with_tca0_dead(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
) -> Result<(), String> {
    for top_level in [true, false] {
        let mut sim = switched_on(power_on)?;
        if top_level {
            let row = ramp::output(ramp::LEVELS);
            sim.chip.compare = [row.fet, row.linear];
            sim.chip.wide[1] = row.top - 1;
            sim.chip.next_bottom = sim.chip.now;
            sim.chip.io[reg::TCA0_CTRLB] |= regs::TCA_CMP0EN;
        }
        // A cycle at the most, to catch the outputs high.
        let want = |c: &Chip| if top_level { c.wo[0] } else { c.wo[1] };
        for _ in 0..ramp::MAX_TOP {
            sim.chip.advance_to(sim.chip.now + 1)?;
            if want(&sim.chip) {
                break;
            }
        }
        let world = World {
            tca_stopped: true,
            ..sim.chip.world
        };
        sim.chip.set_world(world);
        sim.chip.tca_deaf = true;
        let on = sim.chip.leds();
        if on == (0, 0) || top_level && on.1 == 0 {
            return Err(format!("the light should be stuck on, but it's at {on:?}"));
        }
        for sample in simulate::samples(&simulate::parse(&["1C", "wait:1s"])?) {
            sim.tick(sample.pressed)?;
        }
        if sim.chip.leds() != (0, 0) {
            return Err(format!("the light's still at {:?}", sim.chip.leds()));
        }
        for _ in 0..4 {
            sim.tick(true)?;
            if sim.chip.leds() != (0, 0) {
                return Err(format!(
                    "a press brought the light back, at {:?}",
                    sim.chip.leds()
                ));
            }
        }
    }
    Ok(())
}

/// TCA0 stops with WO0 high at a linear level. WO0 doesn't have the FET's pin
/// there, so that mustn't light it, whatever the count does after.
fn linear_with_wo0_high(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
) -> Result<(), String> {
    let mut sim = switched_on(power_on)?;
    if sim.chip.leds().0 == 0 {
        return Err("the linear channel isn't on".into());
    }
    let world = World {
        tca_stopped: true,
        ..sim.chip.world
    };
    sim.chip.set_world(world);
    sim.chip.tca_deaf = true;
    sim.chip.wo[0] = true;
    for _ in 0..ts21::TICKS_PER_SECOND {
        if sim.chip.leds().1 != 0 {
            return Err(format!("the FET's on, at {:?}", sim.chip.leds()));
        }
        sim.tick(false)?;
    }
    Ok(())
}

/// Ticks the output can lag the library by with `Chip::bv_adverse`. Each pass
/// reissues a lost row, but a pass can be close to a whole number of PWM
/// cycles, so a real BOTTOM can catch the same few instructions for several
/// passes running. An eighth of a second is plenty, and a row that never
/// lands still fails.
const LOST_ROW_TICKS: u32 = 8;

/// docs/ui.md's examples and the random switching again on a chip that reads
/// a locked BOTTOM the other way (`Chip::locked_bottom`). Nothing may tear, and
/// the output has to catch up with the library, and it fails unless those
/// BOTTOMs did land.
fn adverse_bv(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    out: &mut Report,
) -> Result<(), String> {
    let mut report = Report::default();
    // Then again with interrupts mid-pass, since the two can meet.
    for landing in [false, true] {
        let adverse = |sim: &mut Sim| {
            sim.chip.bv_adverse = true;
            sim.landings = landing.then(Landings::new);
        };
        let at = |e: String| format!("landing {landing}: {e}");
        for example in spec::examples(spec::UI_MD)? {
            let samples = simulate::samples(&example.actions);
            let world = samples.first().map_or(World::NORMAL, |s| s.world);
            let (mut sim, boot) =
                power_on(&world).map_err(|e| format!("{}: startup: {e}", example.row))?;
            adverse(&mut sim);
            ui_example(sim, boot, samples, &example, &mut report)
                .map_err(|e| at(format!("{}: {e}", example.row)))?;
        }
        let (mut sim, _) = power_on(&World::NORMAL)?;
        adverse(&mut sim);
        against_the_library(sim, &mut report).map_err(|e| at(format!("random switching: {e}")))?;
    }
    out.worst_pass = out.worst_pass.max(report.loop_instructions);
    match report.bv_cleared {
        0 => Err("no BOTTOM ever landed under LUPD".into()),
        _ => Ok(()),
    }
}

/// Runs the same random switching on a chip that drops every write to set
/// LUPD, which is firmware with the lock taken out, and fails unless that's
/// caught. It's here so the check can't quietly stop checking anything.
fn lock_taken_out(power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>) -> Result<(), String> {
    let (mut sim, _) = power_on(&World::NORMAL).map_err(|e| format!("startup: {e}"))?;
    sim.chip.lupd_broken = true;
    let mut report = Report {
        stack: 0,
        loop_instructions: 0,
        loop_budget: TICK_BUDGET,
        off_instructions: 0,
        mid_access: 0,
        worst_pass: 0,
        ticks_landed: 0,
        deadlines_landed: 0,
        bv_cleared: 0,
    };
    match against_the_library(sim, &mut report) {
        Err(e) if e.contains("LUPD") => Ok(()),
        Err(e) => Err(format!("with LUPD taken out, it failed the wrong way: {e}")),
        Ok(()) => Err("with LUPD taken out, nothing noticed".into()),
    }
}

/// What src/bin/link_probe.rs's `main` leaves in `LINK_PROBE_COPY`: its
/// .rodata table, then its .data static's starting values, then a byte it
/// doesn't touch.
const PROBE_EXPECTED: [u8; 7] = [0x12, 0x34, 0x56, 0x9A, 0xBC, 0xDE, 0];

/// Runs the link probe from reset with SRAM full of junk, and checks it read
/// its const table through the mapped-flash window and its .data static
/// from where the startup code copied it. The firmware has no statics yet,
/// so this is the only end-to-end check of those parts of link.x.
pub fn probe(elf: &Elf) -> Result<(), String> {
    let symbol = |name| {
        elf.symbol(name)
            .ok_or_else(|| format!("no `{name}` symbol"))
    };
    let sram = |name| -> Result<u16, String> { Ok((symbol(name)? - DATA_SPACE) as u16) };
    let linked = Layout {
        data: sram("__data_start")?..sram("__data_end")?,
        bss: sram("__bss_start")?..sram("__bss_end")?,
        load: symbol("__data_load")? as u16,
    };
    let rodata = elf.section(".rodata").ok_or("no .rodata section")?;
    if rodata.is_empty() || linked.data.is_empty() || linked.bss.is_empty() {
        return Err(".rodata, .data or .bss is empty, so there's nothing to check".into());
    }

    let unhandled = symbol("__unhandled_interrupt")?;
    let mut sim = Sim::new(elf.flash_image(), linked.bss.end, unhandled)?
        .testing_until(symbol("__ram_tested")?);
    sim.run_to(symbol("main")? / 2, BOOT_BUDGET)
        .and_then(|()| check_startup(&sim.chip, &linked))
        .map_err(|e| format!("startup: {e}"))?;
    sim.run_to(symbol("link_probe_done")? / 2, BOOT_BUDGET)?;

    let copy = sram("LINK_PROBE_COPY")?;
    let got = sim.chip.sram(&(copy..copy + PROBE_EXPECTED.len() as u16));
    if got != PROBE_EXPECTED {
        return Err(format!(
            "LINK_PROBE_COPY holds {got:02x?}, expected {PROBE_EXPECTED:02x?}"
        ));
    }
    Ok(())
}

/// Runs src/bin/pwm_probe.rs, the real `hw::init` and `hw::set_output` going
/// through every level. At each stop in `pwm_probe_landed` it brings on a
/// BOTTOM, without moving time on so the watchdog doesn't need feeding, and
/// checks the pins show that level whole. The firmware's own runs only reach
/// the UI's level 50, so this is the only run where the FET's compare moves.
///
/// Then it checks what the real `hw::count_tick` makes of a good chip at the
/// longest and shortest cycles, around the jumps between them and at both
/// ends of the clocks' spread. Each count has to match the clocks that really
/// went by, to within the whole cycles at each end, and has to pass, or a good
/// chip would turn itself off for good.
pub fn pwm_probe(elf: &Elf) -> Result<(), String> {
    for (osc32k, main_clock) in [(100, 100), (130, 95), (70, 105)] {
        let world = World {
            osc32k,
            main_clock,
            ..World::NORMAL
        };
        for adverse in [false, true] {
            pwm_probe_in(elf, world, adverse).map_err(|e| {
                format!(
                    "with the clocks at {osc32k}% and {main_clock}%, \
                     BV cleared under LUPD {adverse}: {e}"
                )
            })?;
        }
    }
    Ok(())
}

/// Instructions `hw::count_tick` takes to start counting, which the count
/// misses as well as the pass before it.
const COUNT_SETUP: u64 = 64;
/// Instructions a run of the probe may take: a count polls for most of a tick.
const PROBE_BUDGET: u64 = 2_000_000;

fn pwm_probe_in(elf: &Elf, world: World, adverse: bool) -> Result<(), String> {
    let symbol = |name| {
        elf.symbol(name)
            .ok_or_else(|| format!("no `{name}` symbol"))
    };
    let sram = |name| -> Result<u16, String> { Ok((symbol(name)? - DATA_SPACE) as u16) };
    let (landed, done) = (
        symbol("pwm_probe_landed")? / 2,
        symbol("pwm_probe_done")? / 2,
    );
    let (counting, counted) = (
        symbol("pwm_probe_counting")? / 2,
        symbol("pwm_probe_counted")? / 2,
    );
    let level_at = sram("PWM_PROBE_LEVEL")?;
    let mut sim = Sim::new(
        elf.flash_image(),
        sram("__bss_end")?,
        symbol("__unhandled_interrupt")?,
    )?
    .testing_until(symbol("__ram_tested")?);
    sim.run_to(symbol("main")? / 2, BOOT_BUDGET)
        .map_err(|e| format!("startup: {e}"))?;
    sim.chip.set_world(world);
    sim.chip.bv_adverse = adverse;

    let mut seen = [false; ramp::LEVELS as usize + 1];
    let mut counts = 0;
    // The clocks to the next tick when a count starts, and the live cycle.
    let mut count_from = None;
    for _ in 0..PROBE_BUDGET {
        if sim.cpu.pc == done {
            if counts == 0 {
                return Err("never counted a tick".into());
            }
            if adverse && sim.chip.bv_cleared == 0 {
                return Err("no BOTTOM ever landed under LUPD".into());
            }
            return match seen.iter().position(|&seen| !seen) {
                Some(level) => Err(format!("never set level {level}")),
                None => Ok(()),
            };
        }
        if sim.cpu.pc == counting {
            count_from = Some((sim.chip.next_pit - sim.chip.now, sim.chip.tca_cycle()));
        }
        if sim.cpu.pc == counted {
            let r = &sim.cpu.r;
            let clocks = u32::from_le_bytes([r[22], r[23], r[24], r[25]]);
            let level = r[20];
            let top = ramp::output(level).top;
            let (window, was) = count_from
                .take()
                .ok_or("a count ended that never started")?;
            // A part-cycle lost at the end, and one gained at the start, of
            // whichever cycle was running by the time the count read PER.
            let least = window.saturating_sub(COUNT_SETUP + u64::from(top));
            let most = window + was.max(top.into());
            if !(least..=most).contains(&u64::from(clocks)) {
                return Err(format!(
                    "level {level} counted {clocks} clocks in a window of {window}, \
                     with cycles of {was} then {top}"
                ));
            }
            // In the window outright, so this can't lean on the second go.
            if !selftest::ClockCheck::in_window(clocks, top) {
                return Err(format!(
                    "a good chip failed the clock check at level {level}, on {clocks} clocks"
                ));
            }
            counts += 1;
        }
        if sim.cpu.pc == landed {
            sim.chip.next_bottom = sim.chip.now;
            sim.chip.tca_advance()?;
            let level = sim.chip.sram(&(level_at..level_at + 1))[0];
            let out = ramp::output(level);
            let leds = sim.chip.leds();
            let got = (leds.0, leds.1, sim.chip.tca_cycle());
            if got != (out.linear, out.fet, out.top.into()) {
                return Err(format!(
                    "level {level} is {out:?}, but (linear, FET, top) went live as {got:?}"
                ));
            }
            seen[usize::from(level)] = true;
        }
        sim.step()?;
    }
    Err(format!(
        "didn't reach pwm_probe_done within {PROBE_BUDGET} instructions"
    ))
}

/// Where src/rt.rs's startup copies and zeroes, as data-space addresses.
struct Layout {
    data: Range<u16>,
    bss: Range<u16>,
    /// .data's initial values, in the mapped-flash window.
    load: u16,
}

fn check_startup(chip: &Chip, layout: &Layout) -> Result<(), String> {
    let from = usize::from(layout.load.wrapping_sub(MAPPED));
    let len = usize::from(layout.data.end - layout.data.start);
    let initial = chip
        .flash
        .get(from..from + len)
        .ok_or(".data's initial values are outside flash")?;
    if chip.sram(&layout.data) != initial {
        return Err(".data in SRAM doesn't match its initial values in flash".into());
    }
    if let Some((i, b)) = chip
        .sram(&layout.bss)
        .iter()
        .enumerate()
        .find(|&(_, &b)| b != 0)
    {
        return Err(format!(
            ".bss isn't zeroed: {:#06x} holds {b:#04x}",
            usize::from(layout.bss.start) + i
        ));
    }
    Ok(())
}

/// Moves .data and .bss somewhere awkward, gives .data five bytes of
/// initial values, patches the addresses into the real startup code and
/// runs it. Then checks it wrote exactly those bytes and nothing either side.
/// `ram_tested` is where the self-tests end and the setup starts.
fn startup_with_statics(
    image: &[u8],
    ram_tested: u32,
    main: u32,
    linked: &Layout,
    unhandled: u32,
) -> Result<(), String> {
    let values = [0x11, 0x22, 0x33, 0x44, 0x55];
    let mut image = image.to_vec();
    image.resize(image.len().next_multiple_of(2), 0xFF);
    let load = MAPPED + image.len() as u16;
    image.extend(values);

    // An odd start, away from the edges of SRAM, so off-by-ones show.
    let start = SRAM.start + 0x11;
    let moved = Layout {
        data: start..start + 5,
        bss: start + 5..start + 12,
        load,
    };
    patch_startup(&mut image, ram_tested as usize, linked, &moved)?;

    let mut sim = Sim::new(image, moved.bss.end, unhandled)?.testing_until(ram_tested);
    sim.run_to(ram_tested / 2, BOOT_BUDGET)?;
    // The RAM test leaves its last background in every byte. Junk again, so
    // a stray write by the setup after it stands out.
    sim.chip.sram.fill(JUNK);
    sim.run_to(main, BOOT_BUDGET)?;
    check_startup(&sim.chip, &moved)?;
    for edge in [moved.data.start - 1, moved.bss.end] {
        if sim.chip.sram(&(edge..edge + 1)) != [JUNK] {
            return Err(format!("wrote to {edge:#06x}, outside .data and .bss"));
        }
    }
    Ok(())
}

/// Rewrites the `ldi`s in src/rt.rs's startup that load the .data and .bss
/// addresses, after checking they're the ones expected, in order. `reset`
/// is where to start looking: `__ram_tested`, after the self-tests' own
/// `ldi`s.
fn patch_startup(
    image: &mut [u8],
    reset: usize,
    linked: &Layout,
    moved: &Layout,
) -> Result<(), String> {
    // (register, byte) for each `ldi`, as rt.rs loads them: X = __data_start,
    // Z = __data_load, r25:r24 = __data_end, X = __bss_start, r25:r24 = __bss_end.
    let loads = |l: &Layout| -> Vec<(u8, u8)> {
        [
            (26, l.data.start),
            (30, l.load),
            (24, l.data.end),
            (26, l.bss.start),
            (24, l.bss.end),
        ]
        .into_iter()
        .flat_map(|(r, v)| {
            let [lo, hi] = v.to_le_bytes();
            [(r, lo), (r + 1, hi)]
        })
        .collect()
    };

    let mut found = Vec::new();
    let mut at = reset;
    loop {
        let w = word(image, at).ok_or("no call to main after __reset")?;
        if w & 0xFE0E == 0x940E {
            break; // call main
        }
        if w >> 12 == 0xE {
            let k = ((w >> 4) & 0xF0 | w & 0x0F) as u8;
            found.push((at, (16 + ((w >> 4) & 0x0F)) as u8, k));
        }
        at += if is_two_words(w) { 4 } else { 2 };
    }

    let actual: Vec<(u8, u8)> = found.iter().map(|&(_, r, k)| (r, k)).collect();
    if actual != loads(linked) {
        return Err(
            "src/rt.rs's startup doesn't load the registers sim.rs expects; update `patch_startup`"
                .into(),
        );
    }
    for (&(at, r, _), (_, k)) in found.iter().zip(loads(moved)) {
        image[at..at + 2].copy_from_slice(&ldi(r, k).to_le_bytes());
    }
    Ok(())
}

fn ldi(r: u8, k: u8) -> u16 {
    let (r, k) = (u16::from(r), u16::from(k));
    0xE000 | (k & 0xF0) << 4 | (r - 16) << 4 | k & 0x0F
}

fn word(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// Ticks of random switching to compare against the library: about five
/// minutes of a torch being clicked and held.
const RANDOM_TICKS: usize = 20_000;

/// Ticks `hw::init` can take with the 32 kHz oscillator dead. It polls each
/// sync flag it waits on until it gives up, and that's a few ticks' worth.
const DEAD_INIT_TICKS: u32 = 8;

/// Ticks the firmware can lag the library by when a tick comes too late to
/// count. It has to wait for TCB0, over a tick and a half after the last tick,
/// and the library gives up on the first.
const OVERDUE_GRACE_TICKS: u32 = 2;

/// Runs one of docs/ui.md's examples on the firmware and the library in
/// step, then checks where the library ended up against the example.
/// `sim` has just powered up in the example's first world and got as far
/// as `boot` says.
fn ui_example(
    mut sim: Sim,
    boot: Boot,
    samples: Vec<Sample>,
    example: &Example,
    report: &mut Report,
) -> Result<(), String> {
    let world = samples.first().map_or(World::NORMAL, |s| s.world);
    let level = match boot {
        Boot::Main if world.dead() => {
            return Err("the self-tests in src/rt.rs missed the broken RAM or register".into());
        }
        Boot::Failed { from } if !world.dead() => {
            return Err(format!(
                "the self-tests in src/rt.rs failed on a good chip, at {:#06x}",
                from * 2
            ));
        }
        Boot::Main => in_step(sim, samples, report)?,
        Boot::Failed { .. } => {
            // A dead rig ignores every tick, so its level is where it starts.
            stays_dark(&mut sim)?;
            Rig::new(&world).level()
        }
    };
    if example.expect.holds(level) {
        Ok(())
    } else {
        Err(format!(
            "expected {:?}, but it ends at level {level}",
            example.expect
        ))
    }
}

/// The `cli` instruction.
const CLI: u16 = 0x94F8;

/// Checks a chip that's stopped in `__selftest_failed` stays there, with
/// the FET and linear pins driven low and everything else off.
/// It presses and releases the switch a few times on the way, since
/// "nothing, ever" includes clicks.
fn stays_dark(sim: &mut Sim) -> Result<(), String> {
    // `__selftest_failed` starts with a `cli`, then spins.
    let cli = sim.cpu.pc;
    if word(&sim.chip.flash, cli as usize * 2) != Some(CLI) {
        return Err("__selftest_failed doesn't start with cli".into());
    }
    sim.step()?;
    let failed = sim.cpu.pc;
    if failed != cli + 1 {
        return Err(format!("went from cli to {:#06x}", failed * 2));
    }
    for step in 0..2_000 {
        if step % 250 == 0 {
            sim.chip.set_switch(step % 500 == 0)?;
        }
        sim.step()?;
        if sim.cpu.pc != failed {
            return Err(format!(
                "left __selftest_failed for {:#06x}",
                sim.cpu.pc * 2
            ));
        }
    }
    if sim.cpu.flag(I) {
        return Err("failed its self-tests with interrupts on".into());
    }
    // TCA0 is never on by this point, so this can't fail today; the pin
    // check below is the real one. PORTB.OUT isn't modelled, so any write
    // to it is an error, and the pins stay driven low.
    if sim.chip.leds() != (0, 0) {
        return Err(format!(
            "failed its self-tests with the LEDs at {:?}",
            sim.chip.leds()
        ));
    }
    if sim.chip.io[reg::VPORTB_DIR] & (FET | LINEAR) != FET | LINEAR {
        return Err("failed its self-tests with the FET and linear pins floating".into());
    }
    if sim.chip.button_led_lit() {
        return Err("failed its self-tests with the button LED lit".into());
    }
    Ok(())
}

/// Powers up with each kind of fault the self-tests in src/rt.rs are there
/// for, and checks every one stops in `__selftest_failed` and stays dark:
///
/// - every bit of r0 to r31, SREG but its I bit, and SP up to 0x3FFF,
///   stuck either way;
/// - stuck bits in the first, middle and last bytes of SRAM;
/// - addresses that reach another byte, either way round, next door and
///   far apart;
/// - every pair of bits in a byte shorted together, which needs backgrounds
///   that write the two differently: 0x00 and 0xFF never would.
///
/// It also checks the right test found each one: a register fault from
/// before `registers_tested`, a RAM fault from between there and
/// `ram_tested`, both word addresses. Otherwise a
/// register the register test skipped could pass because the RAM test
/// happens to trip over it.
fn startup_faults(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    registers_tested: u32,
    ram_tested: u32,
) -> Result<(), String> {
    let (first, last) = (SRAM.start, SRAM.end - 1);
    let middle = SRAM.start + (SRAM.end - SRAM.start) / 2;
    let mut ram = Vec::new();
    for addr in [first, middle, last] {
        for bit in [0, 7] {
            for high in [false, true] {
                ram.push(RamFault::Stuck { addr, bit, high });
            }
        }
    }
    for (addr, to) in [
        (first + 1, first),
        (first, first + 1),
        (last, first),
        (first, last),
        (middle, middle ^ 0x100),
    ] {
        ram.push(RamFault::Alias { addr, to });
    }
    for a in 0..8 {
        for b in a + 1..8 {
            ram.push(RamFault::Bridge {
                addr: middle,
                bits: [a, b],
            });
        }
    }
    // SREG.I isn't tested (nothing's enabled to fire, so a stuck one can't
    // do harm before `hw::init`), and SPH's top two bits might not exist.
    let tested = (0..32).map(|n| (Register::R(n), 8)).chain([
        (Register::Sreg, 7),
        (Register::Spl, 8),
        (Register::Sph, 6),
    ]);
    let registers = tested.flat_map(|(reg, bits)| {
        (0..bits)
            .flat_map(move |bit| [false, true].map(|high| RegisterFault::Stuck { reg, bit, high }))
    });

    let worlds = ram
        .into_iter()
        .map(|fault| World {
            ram: Some(fault),
            ..World::NORMAL
        })
        .chain(registers.map(|fault| World {
            register: Some(fault),
            ..World::NORMAL
        }));
    for world in worlds {
        let (what, found_in) = match (world.ram, world.register) {
            (Some(ram), _) => (ram.to_string(), registers_tested..ram_tested),
            (_, Some(register)) => (register.to_string(), 0..registers_tested),
            (None, None) => unreachable!("every world here has a fault"),
        };
        let fault = |e: String| format!("{what}: {e}");
        let (mut sim, boot) = power_on(&world).map_err(fault)?;
        let Boot::Failed { from } = boot else {
            return Err(fault("not found, and the firmware reached main".into()));
        };
        if !found_in.contains(&from) {
            return Err(fault(format!(
                "found by the wrong test, at {:#06x}",
                from * 2
            )));
        }
        stays_dark(&mut sim).map_err(fault)?;
    }
    Ok(())
}

/// Powers up with every pair of bits in every register src/rt.rs tests
/// shorted, both ways round, and checks each stops in `__selftest_failed`
/// and stays dark. 0x55 and 0xAA set two bits differently only if they're an
/// odd distance apart, so those shorts have to stop before
/// `__register_shorts`, and the rest have to get past it and stop after. The
/// exceptions can stop anywhere in the register test: SREG, where a short
/// can spoil the very flags the compares read, and r16 and r17, which also
/// carry SREG's and SPH's patterns, with bits masked off that 0x55 and 0xAA
/// would have set the same.
///
/// Then it jumps over `__register_shorts`, and checks a short it found gets
/// all the way to `main` without it.
fn register_shorts(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    power_on_image: &impl Fn(Vec<u8>, &World) -> Result<(Sim, Boot), String>,
    image: &[u8],
    marks: &Marks,
) -> Result<(), String> {
    let tested = (0..32).map(|n| (Register::R(n), 8)).chain([
        (Register::Spl, 8),
        (Register::Sph, 6),
        (Register::Sreg, 7),
    ]);
    for (reg, width) in tested {
        for a in 0..width {
            for b in a + 1..width {
                for high in [false, true] {
                    let register = RegisterFault::Short {
                        reg,
                        bits: [a, b],
                        high,
                    };
                    let fault = |e: String| format!("{register}: {e}");
                    let world = World {
                        register: Some(register),
                        ..World::NORMAL
                    };
                    let carries_patterns = matches!(reg, Register::Sreg | Register::R(16 | 17));
                    let found_in = if carries_patterns {
                        0..marks.registers_tested
                    } else if (b - a) % 2 == 1 {
                        0..marks.register_shorts
                    } else {
                        marks.register_shorts..marks.registers_tested
                    };
                    let (mut sim, boot) = power_on(&world).map_err(fault)?;
                    let Boot::Failed { from } = boot else {
                        return Err(fault("not found, and the firmware reached main".into()));
                    };
                    if !found_in.contains(&from) {
                        return Err(fault(format!(
                            "found by the wrong test, at {:#06x}",
                            from * 2
                        )));
                    }
                    stays_dark(&mut sim).map_err(fault)?;
                }
            }
        }
    }

    let mut skipping = image.to_vec();
    let at = marks.register_shorts as usize * 2;
    let jump = rjmp(marks.register_shorts, marks.register_shorts_end)?;
    skipping[at..at + 2].copy_from_slice(&jump.to_le_bytes());
    let world = World {
        register: Some(RegisterFault::Short {
            reg: Register::R(5),
            bits: [0, 2],
            high: false,
        }),
        ..World::NORMAL
    };
    match power_on_image(skipping, &world)? {
        (_, Boot::Main) => Ok(()),
        (_, Boot::Failed { from }) => Err(format!(
            "a short between bits two apart was found at {:#06x} with \
             `__register_shorts` skipped, so that isn't what finds it",
            from * 2
        )),
    }
}

/// Powers up with coupling faults between two bytes:
///
/// - Every one between two neighbours, either way round: any bit going
///   either way, flipping or forcing any bit of the other. All of them have
///   to stop in the RAM test.
/// - A bit 7 going high in the upper byte, flipping any bit of the lower,
///   next door and from one end of SRAM to the other. The ⇓ pass has to be
///   what finds it, and with that check patched out it has to reach `main`:
///   the ascending passes alone don't see it.
/// - The other way round, with the culprit in the lower byte, where a pass
///   going up finds it first, which is why the march needs both directions.
fn ram_coupling(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    power_on_image: &impl Fn(Vec<u8>, &World) -> Result<(Sim, Boot), String>,
    image: &[u8],
    marks: &Marks,
) -> Result<(), String> {
    let found = |fault: RamFault, found_in: &Range<u32>| -> Result<(), String> {
        let what = fault.to_string();
        let world = World {
            ram: Some(fault),
            ..World::NORMAL
        };
        let (mut sim, boot) = power_on(&world).map_err(|e| format!("{what}: {e}"))?;
        let Boot::Failed { from } = boot else {
            return Err(format!("{what}: not found, and the firmware reached main"));
        };
        if !found_in.contains(&from) {
            return Err(format!(
                "{what}: found by the wrong test, at {:#06x}",
                from * 2
            ));
        }
        stays_dark(&mut sim).map_err(|e| format!("{what}: {e}"))
    };
    let couple = |from, bit, rising, to, victim, force| RamFault::Coupling {
        from,
        bit,
        rising,
        to,
        victim,
        force,
    };

    let low = SRAM.start + (SRAM.end - SRAM.start) / 2;
    let high = low + 1;
    let ram_test = marks.registers_tested..marks.ram_tested;
    for (from, to) in [(high, low), (low, high)] {
        for (bit, victim) in (0..8).flat_map(|bit| (0..8).map(move |victim| (bit, victim))) {
            for rising in [false, true] {
                for force in [None, Some(false), Some(true)] {
                    found(couple(from, bit, rising, to, victim, force), &ram_test)?;
                }
            }
        }
    }

    let mut without_down = image.to_vec();
    without_check(&mut without_down, marks.march_down_check)?;

    let up = marks.registers_tested..marks.march_down_check;
    let down = marks.march_down_check..marks.march_last;
    let last = marks.march_last..marks.ram_tested;
    for (below, above) in [(low, high), (SRAM.start, SRAM.end - 1)] {
        for victim in 0..8 {
            let fault = couple(above, 7, true, below, victim, None);
            found(fault, &down)?;
            let world = World {
                ram: Some(fault),
                ..World::NORMAL
            };
            if let (_, Boot::Failed { from }) = power_on_image(without_down.clone(), &world)? {
                return Err(format!(
                    "{fault}: found at {:#06x} with the ⇓ pass's check gone, \
                     so that isn't the only thing that finds it",
                    from * 2
                ));
            }
            found(couple(below, 7, false, above, victim, None), &up)?;
            found(couple(below, 7, true, above, victim, None), &last)?;
        }
    }
    Ok(())
}

/// Instructions one pass of the periodic RAM test may take: `RAM_SECTION`
/// bytes at 24 instructions a background, four backgrounds, and the rest for
/// saving and putting back. It takes 1,656 today, about 0.46 ms of a 15.6 ms
/// tick by src/rt.rs's count of cycles.
const SECTION_BUDGET: u64 = 1_800;

/// Long enough for the periodic RAM test to get to any section, wherever
/// it's up to: a sweep, and a tick or two in hand.
const SWEEP_TICKS: u32 = selftest::RAM_SECTIONS as u32 + 2;

/// A torch that's powered up in a good world and been switched on.
fn switched_on(power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>) -> Result<Sim, String> {
    let (mut sim, _) = power_on(&World::NORMAL)?;
    sim.prove()?;
    sim.tick(false)?;
    for sample in simulate::samples(&simulate::parse(&["1C", "wait:1s"])?) {
        sim.tick(sample.pressed)?;
    }
    if sim.chip.leds() == (0, 0) {
        return Err("a click didn't turn the light on".into());
    }
    Ok(sim)
}

/// Runs for up to `ticks` ticks, stopping if the CPU reaches `failed`, a
/// word address. Returns where it jumped from if it did.
fn run_until(sim: &mut Sim, failed: u32, ticks: u32) -> Result<Option<u32>, String> {
    let stop = sim.chip.ticks + ticks;
    let mut from = sim.cpu.pc;
    while sim.chip.ticks < stop {
        if sim.cpu.pc == failed {
            return Ok(Some(from));
        }
        from = sim.cpu.pc;
        sim.step()?;
    }
    Ok(None)
}

/// Instructions `__ram_section_failed` may take to reach its spin, after the
/// two that hand the pins back: a `movw` and a store for each byte of the
/// section, and a few in hand.
const PUT_BACK_STEPS: u32 = selftest::RAM_SECTION as u32 + 4;

/// Checks a chip that's just reached `__ram_section_failed` turns the light
/// off straight away and stays there, with interrupts off and without
/// feeding the watchdog, until the watchdog resets it, on the tick the
/// last feed's windows run out (§19.3.3.2). A feed in the closed window would
/// reset it too, but that's a fault, and one in the open window pushes the
/// reset past that tick. It stops at the bite, so the reset itself is the
/// caller's to do.
fn held_low_until_reset(sim: &mut Sim) -> Result<(), String> {
    let timeout = (sim.chip.wdt_closed() + sim.chip.wdt_open()).div_ceil(WDT_CLOCKS_PER_TICK);
    let left = timeout.saturating_sub(sim.chip.wdt.ticks);
    let reset_on = sim.chip.ticks + left;
    sim.step()?; // ldi
    sim.step()?; // sts
    if sim.chip.leds() != (0, 0) {
        return Err(format!(
            "failed the periodic RAM test with the LEDs at {:?}",
            sim.chip.leds()
        ));
    }
    if sim.chip.io[reg::VPORTB_DIR] & (FET | LINEAR) != FET | LINEAR {
        return Err("failed the periodic RAM test with the pins floating".into());
    }
    if sim.cpu.flag(I) {
        return Err("failed the periodic RAM test with interrupts on".into());
    }
    // Putting the section back, up to the spin.
    let mut stopped = sim.cpu.pc;
    for _ in 0..PUT_BACK_STEPS {
        sim.step()?;
        if sim.cpu.pc == stopped {
            break;
        }
        stopped = sim.cpu.pc;
    }
    // A whole timeout past that, so a late reset gets called late.
    for _ in 0..u64::from(left + timeout + 1) * PIT_PERIOD {
        match sim.step() {
            Ok(()) if sim.cpu.pc == stopped => {}
            Ok(()) => {
                return Err(format!(
                    "left __ram_section_failed for {:#06x}",
                    sim.cpu.pc * 2
                ));
            }
            Err(e) if e.contains(WDT_TIMED_OUT) && sim.chip.ticks == reset_on => return Ok(()),
            Err(e) if e.contains(WDT_TIMED_OUT) => {
                return Err(format!(
                    "the watchdog reset it on tick {}, not {reset_on}: {e}",
                    sim.chip.ticks
                ));
            }
            Err(e) => return Err(e),
        }
    }
    Err("failed the periodic RAM test, and the watchdog never reset it".into())
}

/// Sweeps all of SRAM on a good chip with the light on, a section a tick,
/// with the sections in the order `RamSweep` gives. Each pass has to leave
/// its section as it found it, with interrupts off until then, leave r1,
/// SREG and the registers the calling convention keeps as they were, and
/// stay inside `SECTION_BUDGET`.
fn periodic_passes(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    section: u32,
) -> Result<(), String> {
    let mut sim = switched_on(power_on)?;
    // Where the firmware's sweep has got to is its own business, so line up
    // with its next section first.
    sim.run_to(section, PIT_PERIOD)?;
    let first = u16::from_le_bytes([sim.cpu.r[24], sim.cpu.r[25]]);
    let mut sweep = RamSweep::new();
    if !(0..selftest::RAM_SECTIONS).any(|_| sweep.next_section() == first) {
        return Err(format!(
            "tested {first:#06x}, which isn't a section's start"
        ));
    }
    let mut expected = first;
    for _ in 0..selftest::RAM_SECTIONS {
        sim.run_to(section, PIT_PERIOD)?;
        let start = u16::from_le_bytes([sim.cpu.r[24], sim.cpu.r[25]]);
        if start != expected {
            return Err(format!(
                "tested {start:#06x}, but `RamSweep` says {expected:#06x}"
            ));
        }
        expected = sweep.next_section();
        let bytes = start..start + selftest::RAM_SECTION;
        let (began, sp) = (sim.executed, sim.cpu.sp);
        // SP points at the next free byte. Below that nothing's live, and
        // the last march leaves its background everywhere, which a pass
        // that forgot to put anything back would match.
        if bytes.end - 1 <= sp {
            sim.chip.write_section(start);
        }
        // What the caller's registers hold could happen to match what a
        // pass leaves in them, so give them values that can't, and put the
        // real ones back after.
        let real = (sim.cpu.r, sim.cpu.sreg);
        for n in SAVED_REGISTERS {
            sim.cpu.r[n] = 0xC0 | n as u8;
        }
        sim.cpu.sreg = 1 << I | 0x55;
        let expected_registers = (sim.cpu.r, sim.cpu.sreg);
        let run_until = |sim: &mut Sim, done: &dyn Fn(&Sim) -> bool| -> Result<(), String> {
            while !done(sim) {
                if sim.executed - began > SECTION_BUDGET {
                    return Err(format!(
                        "{start:#06x}: past the {SECTION_BUDGET} instruction budget, \
                         or interrupts never went off and came back"
                    ));
                }
                sim.step()?;
            }
            Ok(())
        };
        run_until(&mut sim, &|s| !s.cpu.flag(I))?;
        let before = sim.chip.sram(&bytes).to_vec();
        run_until(&mut sim, &|s| s.cpu.flag(I))?;
        if sim.chip.sram(&bytes) != before {
            return Err(format!("didn't put {start:#06x} back as it was"));
        }
        run_until(&mut sim, &|s| s.cpu.sp == sp + 2)?;
        for n in iter::once(1).chain(SAVED_REGISTERS) {
            if sim.cpu.r[n] != expected_registers.0[n] {
                return Err(format!(
                    "{start:#06x}: r{n} went from {:#04x} to {:#04x}",
                    expected_registers.0[n], sim.cpu.r[n]
                ));
            }
        }
        if sim.cpu.sreg != expected_registers.1 {
            return Err(format!(
                "{start:#06x}: SREG went from {:#04x} to {:#04x}",
                expected_registers.1, sim.cpu.sreg
            ));
        }
        for n in SAVED_REGISTERS {
            sim.cpu.r[n] = real.0[n];
        }
        sim.cpu.sreg = real.1;
    }
    Ok(())
}

/// What `__ram_section` has to leave as it found them, r1 aside: the
/// registers the calling convention says a function keeps.
const SAVED_REGISTERS: [usize; 18] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 28, 29,
];

/// Each check in `__ram_section`'s march, with a fault between two bytes of
/// a section that only that check can find: it has to stop there, and with
/// the check patched out it has to get through the pass. The faults came
/// from a search through every coupling between two neighbours.
fn periodic_checks(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    marks: &SectionMarks,
) -> Result<(), String> {
    let couple = |from: u16, bit: u8, to: u16, force: Option<bool>| RamFault::Coupling {
        from,
        bit,
        rising: true,
        to,
        victim: if force.is_some() { 0 } else { 3 },
        force,
    };
    // Offsets into the section, the culprit first.
    let (low, high) = (8, 9);
    let checks = [
        (marks.up..marks.down, couple(low, 0, high, Some(true))),
        (marks.down..marks.last, couple(high, 0, low, Some(true))),
        // Rising from bit 7 high in the section's starting contents, so the
        // first ⇕(wD) doesn't set it off where ⇑ can see.
        (marks.down..marks.last, couple(high, 7, low, None)),
        (marks.last..marks.failed, couple(low, 0, high, Some(false))),
    ];
    for (found_in, fault) in checks {
        match one_pass(power_on, marks, fault, None)? {
            Some(from) if found_in.contains(&from) => {}
            Some(from) => {
                return Err(format!(
                    "{fault}, as offsets: found by the wrong check, at {:#06x}",
                    from * 2
                ));
            }
            None => return Err(format!("{fault}, as offsets: not found")),
        }
        if let Some(from) = one_pass(power_on, marks, fault, Some(found_in.start))? {
            return Err(format!(
                "{fault}, as offsets: found at {:#06x} with that check gone, \
                 so it isn't the only one that finds it",
                from * 2
            ));
        }
    }
    Ok(())
}

/// Runs one pass of `__ram_section` on a torch that's on, in free RAM, with
/// `fault`'s addresses as offsets into the section it's about to test, and
/// the check at `without` patched out. Returns where it failed, if it did.
fn one_pass(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    marks: &SectionMarks,
    fault: RamFault,
    without: Option<u32>,
) -> Result<Option<u32>, String> {
    let mut sim = switched_on(power_on)?;
    if let Some(check) = without {
        without_check(&mut sim.chip.flash, check)?;
    }
    sim.run_to(marks.section, PIT_PERIOD)?;
    let start = u16::from_le_bytes([sim.cpu.r[24], sim.cpu.r[25]]);
    if start + selftest::RAM_SECTION > sim.cpu.sp {
        return Err(format!("{start:#06x} isn't in free RAM"));
    }
    sim.chip.write_section(start);
    let RamFault::Coupling {
        from,
        bit,
        rising,
        to,
        victim,
        force,
    } = fault
    else {
        return Err(format!("{fault} isn't a coupling fault"));
    };
    sim.chip.world.ram = Some(RamFault::Coupling {
        from: start + from,
        bit,
        rising,
        to: start + to,
        victim,
        force,
    });
    let (sp, began) = (sim.cpu.sp, sim.executed);
    let mut last = sim.cpu.pc;
    while sim.cpu.sp != sp + 2 {
        if sim.cpu.pc == marks.failed {
            return Ok(Some(last));
        }
        if sim.executed - began > SECTION_BUDGET {
            return Err("a pass never finished".into());
        }
        last = sim.cpu.pc;
        sim.step()?;
    }
    Ok(None)
}

/// Turns a check off: `at` is its `cp`, and after that come its `breq` and
/// the `rjmp` to wherever failures go, which becomes a `nop`.
fn without_check(flash: &mut [u8], at: u32) -> Result<(), String> {
    let at = at as usize * 2 + 4;
    if word(flash, at).is_none_or(|w| w >> 12 != 0xC) {
        return Err(format!("expected an rjmp at {at:#06x}"));
    }
    flash[at..at + 2].fill(0);
    Ok(())
}

/// Word addresses of the labels in `__ram_section`.
struct SectionMarks {
    section: u32,
    /// The compares in the ⇑, ⇓ and last passes.
    up: u32,
    down: u32,
    last: u32,
    failed: u32,
}

/// RAM going bad with the cell in and the light on: a stuck bit, a short
/// and coupling, at the ends of SRAM and either side of where sections start,
/// in the live stack too. Each has to be found by `__ram_section` within a
/// sweep, hold the light off and let the watchdog reset the chip, and then
/// stop in `__selftest_failed` when the RAM test startup runs after a hang
/// finds it, rather than come back up and cycle.
///
/// A fault that's gone by the time of the reset has to come back off and
/// marked as a hang, which refuses the FET, even in the section holding the
/// marker. And with `__march_after_hang` patched out, a fault that's still
/// there has to reach `main`, so that's what stops it.
///
/// The coupling fault's two bytes straddle a multiple of `RAM_SECTION`,
/// so only the sections overlapping brings them together.
///
/// Then two that shouldn't be found. With `__ram_section` stubbed out, as if
/// the test were gone, the torch has to carry on with the light on for a
/// whole sweep: nothing else notices. And with the torch resting, where it
/// mustn't run.
fn periodic_faults(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    marks: &LateMarks,
) -> Result<(), String> {
    let section = marks.section;
    let (first, last) = (SRAM.start, SRAM.end - 1);
    let middle = SRAM.start + (SRAM.end - SRAM.start) / 2;
    let step = selftest::RAM_STEP;
    let mut faults = Vec::new();
    for addr in [first, first + step - 1, first + step, middle, last] {
        for (bit, high) in [(0, true), (0, false), (7, true), (7, false)] {
            faults.push(RamFault::Stuck { addr, bit, high });
        }
    }
    for bits in [[0, 1], [0, 2], [3, 7]] {
        faults.push(RamFault::Bridge { addr: middle, bits });
    }
    let straddle = middle + selftest::RAM_SECTION;
    faults.push(RamFault::Coupling {
        from: straddle,
        bit: 7,
        rising: true,
        to: straddle - 1,
        victim: 0,
        force: None,
    });
    for fault in faults {
        late_fault(power_on, fault, marks)?;
    }

    // A byte the firmware's using, stuck at what it holds now. That's only
    // invisible to the firmware while the byte doesn't change, and one that
    // does could send it wrong before the test gets there, which this would
    // report as an error rather than a find. So these are bytes of the live
    // stack that a sweep's worth of passes leaves alone.
    let sim = switched_on(power_on)?;
    let steady = steady_stack(power_on, section)?;
    let &[.., deepest] = steady.as_slice() else {
        return Err("every byte of the live stack changes".into());
    };
    for addr in [steady[0], steady[steady.len() / 2], deepest, last] {
        for bit in [0, 7] {
            let high = sim.chip.sram(&(addr..addr + 1))[0] & 1 << bit != 0;
            let fault = RamFault::Stuck { addr, bit, high };
            late_fault(power_on, fault, marks)?;
        }
    }

    let fault = RamFault::Stuck {
        addr: middle,
        bit: 0,
        high: true,
    };

    // Bit 0 of the marker's first byte reads high in `Step::Passed` anyway,
    // so the section's copy of it is good and only the test's patterns show
    // the fault.
    let in_marker = RamFault::Stuck {
        addr: sim.marker,
        bit: 0,
        high: true,
    };
    for gone in [fault, in_marker] {
        passing_fault(power_on, gone, marks)?;
    }
    without_the_march(power_on, fault, marks)?;

    let mut sim = switched_on(power_on)?;
    let at = section as usize * 2;
    sim.chip.flash[at..at + 2].copy_from_slice(&0x9508u16.to_le_bytes()); // ret
    sim.chip.world.ram = Some(fault);
    let leds = sim.chip.leds();
    for _ in 0..SWEEP_TICKS {
        sim.tick(false)?;
    }
    if sim.chip.leds() != leds {
        return Err("a fault changed the light with `__ram_section` stubbed out".into());
    }

    let (mut sim, _) = power_on(&World::NORMAL)?;
    sim.prove()?;
    for _ in 0..4 {
        sim.tick(false)?;
    }
    sim.chip.world.ram = Some(fault);
    let power_downs = sim.chip.power_downs;
    if run_until(&mut sim, section, SWEEP_TICKS)?.is_some() {
        return Err("ran the periodic RAM test while resting".into());
    }
    if sim.chip.power_downs < power_downs + u64::from(SWEEP_TICKS) - 1 {
        return Err("wasn't powering down while resting".into());
    }
    Ok(())
}

/// The bytes of the live stack, top of stack first, that hold still for a
/// whole sweep of a torch that's on. `__ram_section` is stubbed out, since it
/// writes to all of them and puts them back.
fn steady_stack(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    section: u32,
) -> Result<Vec<u16>, String> {
    let mut sim = switched_on(power_on)?;
    let at = section as usize * 2;
    sim.chip.flash[at..at + 2].copy_from_slice(&0x9508u16.to_le_bytes()); // ret
    let live = sim.cpu.sp + 1..SRAM.end;
    let held = sim.chip.sram(&live).to_vec();
    let mut steady = vec![true; held.len()];
    let stop = sim.chip.ticks + SWEEP_TICKS;
    while sim.chip.ticks < stop {
        sim.step()?;
        let now = sim.chip.sram(&live);
        for (still, (now, then)) in steady.iter_mut().zip(now.iter().zip(&held)) {
            *still &= now == then;
        }
    }
    Ok(live
        .zip(steady)
        .filter_map(|(addr, still)| still.then_some(addr))
        .collect())
}

/// Breaks `fault` into a torch that's on, and checks it goes as
/// `periodic_faults` says.
fn late_fault(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    fault: RamFault,
    marks: &LateMarks,
) -> Result<(), String> {
    let at = |e: String| format!("{fault}: {e}");
    let mut sim = found_late(power_on, fault, marks).map_err(at)?;
    sim.reset(regs::RSTCTRL_WDRF);
    let boot = sim.boot(marks.main, marks.selftest_failed).map_err(at)?;
    let Boot::Failed { from } = boot else {
        return Err(at(
            "came back up with the fault still there, so it would cycle".into(),
        ));
    };
    if !(marks.march_after_hang..marks.ram_tested).contains(&from) {
        return Err(at(format!(
            "stopped by the wrong test, at {:#06x}",
            from * 2
        )));
    }
    stays_dark(&mut sim).map_err(at)
}

/// Breaks `fault` into a torch that's on, and runs it until `__ram_section`
/// has found it and the watchdog is about to reset the chip.
fn found_late(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    fault: RamFault,
    marks: &LateMarks,
) -> Result<Sim, String> {
    let mut sim = switched_on(power_on)?;
    sim.chip.world.ram = Some(fault);
    let Some(from) = run_until(&mut sim, marks.section_failed, SWEEP_TICKS)? else {
        return Err(format!("not found within {SWEEP_TICKS} ticks"));
    };
    if !(marks.section..marks.section_failed).contains(&from) {
        return Err(format!("found from {:#06x}", from * 2));
    }
    held_low_until_reset(&mut sim)?;
    Ok(sim)
}

/// A fault that `__ram_section` finds and that's gone by the reset: the RAM
/// test after the hang passes, and the torch comes back off, with the marker
/// saying it hung, which is what refuses the FET.
fn passing_fault(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    fault: RamFault,
    marks: &LateMarks,
) -> Result<(), String> {
    let at = |e: String| format!("{fault}, gone by the reset: {e}");
    let mut sim = found_late(power_on, fault, marks).map_err(at)?;
    sim.chip.world.ram = None;
    sim.reset(regs::RSTCTRL_WDRF);
    let boot = sim.boot(marks.main, marks.selftest_failed).map_err(at)?;
    if boot != Boot::Main {
        return Err(at(format!("stopped: {boot:?}")));
    }
    sim.tick(false).map_err(at)?;
    if sim.proved() != Some(Step::Hung) {
        return Err(at(format!(
            "the marker says {:?}, not that it hung",
            sim.marker()
        )));
    }
    if sim.chip.leds() != (0, 0) {
        return Err(at(format!("came back on, at {:?}", sim.chip.leds())));
    }
    Ok(())
}

/// With startup jumping over the RAM test after a hang, a fault that's still
/// there reaches `main`, so it's that test that stops it, and nothing else
/// would. The patch breaks the flash's CRC, which only refuses the FET.
fn without_the_march(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    fault: RamFault,
    marks: &LateMarks,
) -> Result<(), String> {
    let mut sim = found_late(power_on, fault, marks)?;
    let at = marks.march_after_hang as usize * 2;
    let jump = rjmp(marks.march_after_hang, marks.ram_tested)?;
    sim.chip.flash[at..at + 2].copy_from_slice(&jump.to_le_bytes());
    sim.reset(regs::RSTCTRL_WDRF);
    match sim.boot(marks.main, marks.selftest_failed)? {
        Boot::Main => Ok(()),
        Boot::Failed { from } => Err(format!(
            "{fault}: stopped at {:#06x} with the RAM test after a hang gone, \
             so that isn't what stops it",
            from * 2
        )),
    }
}

/// Instructions from the reset vector to TCA0 letting go of the pins: `jmp`,
/// `sbi`s, the first of the register test, and a few in hand.
const PINS_OFF_INSTRUCTIONS: u32 = 300;

/// Something jumps to the reset vector with the light on. There's been no
/// reset, so TCA0's still driving the pins, and startup has to stop that
/// before it spends 85 ms on the RAM test, or longer. The interrupt flags are
/// still up as well, with the PIT's coming first, and `main` has to turn that
/// into a real reset, or they'd fail the clock check and leave the torch dark
/// for good. It has to come back off, with the FET refused, and work.
fn jump_to_reset(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    marks: &LateMarks,
) -> Result<(), String> {
    let mut sim = switched_on(power_on)?;
    sim.cpu = Cpu::new(&SRAM);
    sim.testing = sim.ram_tested;
    sim.chip.pit_flag = true;
    sim.chip.tcb.capt = true;
    let mut off = false;
    for _ in 0..PINS_OFF_INSTRUCTIONS {
        sim.step()?;
        off = sim.chip.leds() == (0, 0);
        if off {
            break;
        }
    }
    if !off {
        return Err(format!(
            "the light's still at {:?} after {PINS_OFF_INSTRUCTIONS} instructions",
            sim.chip.leds()
        ));
    }
    // A slow WDT, at the far end of its spread, lets the RAM test finish
    // before it bites, and a bite would be a real reset, which hides this.
    while sim.cpu.pc != marks.main {
        if sim.cpu.pc == marks.selftest_failed {
            return Err("the self-tests failed on a good chip".into());
        }
        sim.chip.wdt.ticks = 0;
        sim.step()?;
    }
    for _ in 0..4 {
        sim.tick(false)?;
    }
    if sim.proved() != Some(Step::Hung) {
        return Err(format!(
            "the marker says {:?}, not that it hung",
            sim.marker()
        ));
    }
    if sim.chip.leds() != (0, 0) {
        return Err(format!("came back on, at {:?}", sim.chip.leds()));
    }
    for sample in simulate::samples(&simulate::parse(&["1C", "wait:1s"])?) {
        sim.tick(sample.pressed)?;
    }
    match sim.chip.leds() {
        (0, _) => Err("came back dark: a click doesn't turn it on".into()),
        (_, 0) => Ok(()),
        leds => Err(format!("the FET's on, at {leds:?}")),
    }
}

/// Word addresses `periodic_faults` follows a fault through.
struct LateMarks {
    section: u32,
    section_failed: u32,
    main: u32,
    selftest_failed: u32,
    march_after_hang: u32,
    ram_tested: u32,
}

/// Word addresses of the labels in src/rt.rs's self-tests.
struct Marks {
    /// The compare in the March test's ⇓ pass, and the last pass after it.
    march_down_check: u32,
    march_last: u32,
    /// Where the RAM test ends.
    ram_tested: u32,
    /// Where the 0x33 and 0x0F register patterns start, and the end of them.
    register_shorts: u32,
    register_shorts_end: u32,
    /// Where the register test ends.
    registers_tested: u32,
}

/// An `rjmp` at word address `from`, forwards to `to`.
fn rjmp(from: u32, to: u32) -> Result<u16, String> {
    match to.checked_sub(from + 1) {
        Some(k) if k < 2048 => Ok(0xC000 | k as u16),
        _ => Err(format!(
            "an rjmp at {:#06x} can't reach {:#06x}",
            from * 2,
            to * 2
        )),
    }
}

/// Switches the firmware and the library the same random way. The host
/// tests trust the library; this checks the AVR build of it, and `hw.rs`'s
/// pins and channels, do what it says.
fn against_the_library(sim: Sim, report: &mut Report) -> Result<(), String> {
    let samples = RandomSwitching::new(0x5EED)
        .flat_map(|(pressed, ticks)| iter::repeat_n(pressed, ticks.into()))
        .take(RANDOM_TICKS)
        .map(|pressed| Sample {
            pressed,
            world: World::NORMAL,
        });
    in_step(sim, samples, report).map(drop)
}

/// Feeds the same switch samples and ADC readings to the firmware, from just
/// after `main`, and to `ts21::torch`, and checks the LEDs show the
/// library's level on every tick. Returns the level it ends at.
fn in_step(
    mut sim: Sim,
    samples: impl IntoIterator<Item = Sample>,
    report: &mut Report,
) -> Result<u8, String> {
    // The firmware reads the fuses before its first tick, so the world it
    // powers up in is the first sample's.
    let mut samples = samples.into_iter().peekable();
    let power_on = samples.peek().map_or(World::NORMAL, |s| s.world);
    sim.chip.world = World {
        hang: false,
        ..power_on
    };
    if power_on.corrupt_flash {
        // The checksum's high byte, which is never code, so the firmware
        // still runs however big it gets.
        let byte = sim
            .chip
            .flash
            .get_mut(crc::CHECKSUM_AT)
            .ok_or("can't corrupt flash: the image isn't padded to all of flash")?;
        *byte ^= 0x01;
    }
    let proved = sim.prove().map_err(|e| format!("proof: {e}"))?;
    // A dead 32 kHz oscillator stops the proof, which fails safe.
    let verdict = if power_on.wdt.is_some() || power_on.osc32k == 0 {
        Step::Failed
    } else {
        Step::Passed
    };
    if proved.step != verdict {
        return Err(format!(
            "the proof ended {:?}, not {verdict:?}",
            proved.step
        ));
    }
    let init_ticks = if power_on.osc32k == 0 {
        DEAD_INIT_TICKS
    } else {
        1
    };
    for _ in 0..init_ticks {
        sim.tick(false).map_err(|e| format!("hw::init: {e}"))?;
    }
    if verdict == Step::Passed && !sim.chip.watchdog_windowed() {
        return Err("hw::init never enabled the watchdog in window mode".into());
    }
    if !sim.chip.watchdog_running() {
        return Err("hw::init never enabled the watchdog".into());
    }
    if !sim.chip.button_led_lit() {
        return Err("hw::init left PB5 without its pull-up, so the button LED is out".into());
    }
    if !sim.chip.clocked_as_modelled() {
        return Err("hw::init didn't set up the 5 MHz clock and TCA0's PWM".into());
    }
    let mut rig = Rig::new(&power_on);
    let mut resting = 0;
    let mut hung = false;
    let mut overdue = 0;
    let mut behind = 0;
    for (tick, sample) in samples.enumerate() {
        sim.chip.set_world(sample.world);
        if sample.world.hang && !hung {
            hung = true;
            sim.hang(verdict).map_err(|e| format!("tick {tick}: {e}"))?;
        }
        let power_downs = sim.chip.power_downs;
        let took = sim
            .tick(sample.pressed)
            .map_err(|e| format!("tick {tick}: {e}"))?;
        if !sim.chip.button_led_lit() {
            return Err(format!(
                "tick {tick}: PB5 isn't an input with its pull-up on, \
                 so the button LED is out"
            ));
        }
        report.loop_instructions = report.loop_instructions.max(took);
        let powered_down = sim.chip.power_downs > power_downs;
        // Only a pass that started off resting, which is what a torch that's
        // off spends every tick. The pass that turns the light off started
        // on, and does a section of the periodic RAM test, but only once.
        if powered_down && resting > 0 {
            report.off_instructions = report.off_instructions.max(took);
        }
        rig.tick(sample.pressed, &sample.world);
        resting = if rig.torch.resting() { resting + 1 } else { 0 };
        overdue = if sample.world.overdue() {
            overdue + 1
        } else {
            0
        };
        let settling = (1..=OVERDUE_GRACE_TICKS).contains(&overdue);
        // A lost off row has to be reissued before the chip can power down.
        let lost_row_grace = if sim.chip.bv_adverse {
            LOST_ROW_TICKS
        } else {
            0
        };
        // With no tick to wake it, a chip that's powered down doesn't run
        // `sleep` again, so look for it asleep instead. Awake there means
        // it's hung, with no watchdog to rescue it.
        if sample.world.osc32k == 0 {
            if overdue > OVERDUE_GRACE_TICKS + lost_row_grace && !sim.chip_powered_down() {
                return Err(format!(
                    "tick {tick}: the 32 kHz oscillator's dead, but the firmware \
                     isn't powered down"
                ));
            }
        } else if resting > REST_GRACE_TICKS + lost_row_grace
            && !powered_down
            && !sim.counted
            && !settling
            && !sim.tick_landed()
        {
            return Err(format!(
                "tick {tick}: the library's been resting for {resting} ticks, \
                 but the firmware didn't power down"
            ));
        }
        let output = ramp::output(rig.level());
        let want = (output.linear, output.fet, output.top);
        let leds = sim.chip.leds();
        let leds = (leds.0, leds.1, sim.chip.wide(reg::TCA0_PER).wrapping_add(1));
        // A stopped TCA0 can't do what the library asks until the next count
        // turns the light off, so there's nothing to compare until then. Off
        // is still off.
        let frozen = sample.world.tca_stopped && rig.level() > 0;
        if leds == want || settling || frozen {
            behind = 0;
            continue;
        }
        behind += 1;
        // With `Chip::bv_adverse` a row can be lost, which is put right on
        // the next pass, and it goes live at the BOTTOM after that.
        if !sim.chip.bv_adverse || behind > LOST_ROW_TICKS {
            return Err(format!(
                "tick {tick}: the firmware's (linear, FET, top) is {leds:?}, \
                 but the library's level {} is {want:?}",
                rig.level()
            ));
        }
    }
    report.stack = report.stack.max(sim.stack());
    report.mid_access += sim.mid_access;
    report.ticks_landed += sim.ticks_landed;
    report.deadlines_landed += sim.deadlines_landed;
    report.bv_cleared += sim.chip.bv_cleared;
    Ok(rig.level())
}

/// Instructions a landed handler can take. The real ones take tens.
const HANDLER_BUDGET: u32 = 2_000;

struct Sim {
    cpu: Cpu,
    chip: Chip,
    /// SP below this means a push landed in .bss.
    stack_floor: u16,
    lowest_sp: u16,
    /// Instructions run, which unlike `Chip::now` stops while asleep.
    executed: u64,
    /// `__unhandled_interrupt`, as a word address.
    unhandled: u32,
    /// A bit per vector whose slot doesn't jump there.
    handled: u32,
    /// `__ram_tested`, as a word address, until the CPU gets there. The
    /// register test before it points SP all over SRAM without pushing
    /// anything, so the stack checks wait until it's done.
    testing: Option<u32>,
    /// What the running handler has to give back.
    landed: Option<Landed>,
    /// Handlers so far that landed with a TCA0 16-bit access half done.
    mid_access: u32,
    /// Real ticks so far that `Landings` put on `hw::wait_for_tick`'s loop.
    ticks_landed: u32,
    /// `tcb0_int`s so far that `Landings` set off, and that gave everything
    /// back.
    deadlines_landed: u32,
    /// Steps in a row with `landing_stops_time`, for `HANDLER_BUDGET`.
    stopped_steps: u32,
    /// Set to land interrupts on purpose, mid-pass.
    landings: Option<Landings>,
    /// What `testing` goes back to on a reset, since startup runs again.
    ram_tested: Option<u32>,
    /// The watchdog's marker in `.noinit`, as a data-space address.
    marker: u16,
    /// `executed` at the last reset.
    booted: u64,
    /// Whether the last pass ended in a counted wait for the tick.
    counted: bool,
}

/// Where startup ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boot {
    /// At `main`, with the self-tests passed and memory set up.
    Main,
    /// In `__selftest_failed`, for good, jumped to from `from`, a word
    /// address: which check failed.
    Failed { from: u32 },
}

/// A bit for each vector whose slot in `image`'s table is a `jmp` to
/// somewhere other than `unhandled` (a word address). layout.rs checks
/// every slot is a `jmp`.
fn handled(image: &[u8], unhandled: u32) -> u32 {
    (1..regs::VECTORS).fold(0, |mask, v| {
        let at = v * VECTOR_WORDS as usize * 2;
        let jmp = word(image, at).is_some_and(|w| w & 0xFE0E == 0x940C);
        let target = word(image, at + 2).map(u32::from);
        if jmp && target != Some(unhandled) {
            mask | 1 << v
        } else {
            mask
        }
    })
}

impl Sim {
    fn new(image: Vec<u8>, bss_end: u16, unhandled: u32) -> Result<Self, String> {
        if image.len() > FLASH_SIZE {
            return Err(format!("{} bytes won't fit in flash", image.len()));
        }
        let cpu = Cpu::new(&SRAM);
        let unhandled = unhandled / 2;
        Ok(Self {
            lowest_sp: cpu.sp,
            cpu,
            handled: handled(&image, unhandled),
            chip: Chip::new(image),
            stack_floor: bss_end - 1,
            executed: 0,
            unhandled,
            testing: None,
            landed: None,
            mid_access: 0,
            ticks_landed: 0,
            deadlines_landed: 0,
            stopped_steps: 0,
            landings: None,
            ram_tested: None,
            marker: SRAM.start,
            booted: 0,
            counted: false,
        })
    }

    /// Holds the stack checks off until the CPU reaches `ram_tested`, a
    /// byte address, as startup does with the real src/rt.rs.
    fn testing_until(mut self, ram_tested: u32) -> Self {
        self.testing = Some(ram_tested / 2);
        self.ram_tested = self.testing;
        self
    }

    /// Runs from reset until startup reaches `main` or gives up in
    /// `failed`, both word addresses.
    fn boot(&mut self, main: u32, failed: u32) -> Result<Boot, String> {
        let mut from = self.cpu.pc;
        for _ in 0..BOOT_BUDGET {
            if self.cpu.pc == main {
                if self.testing.is_some() {
                    return Err(
                        "reached main without passing __ram_tested, so the stack was never checked"
                            .into(),
                    );
                }
                return Ok(Boot::Main);
            }
            if self.cpu.pc == failed {
                return Ok(Boot::Failed { from });
            }
            from = self.cpu.pc;
            self.step()?;
        }
        Err(format!(
            "reached neither main nor __selftest_failed within {BOOT_BUDGET} instructions"
        ))
    }

    /// Whether the CPU's asleep in power-down.
    fn chip_powered_down(&self) -> bool {
        self.cpu.asleep && self.chip.powering_down()
    }

    /// The most stack used so far, in bytes.
    fn stack(&self) -> u16 {
        SRAM.end - 1 - self.lowest_sp
    }

    fn step(&mut self) -> Result<(), String> {
        // Before the instruction, so a stuck bit is stuck from reset and
        // every instruction reads it stuck.
        if let Some(fault) = self.chip.world.register {
            fault.apply(&mut self.cpu);
        }
        let pc = self.cpu.pc;
        let awake = !self.cpu.asleep;
        let handling = self.cpu.handling;
        if self.chip.stuck {
            self.chip.elapse()?;
        } else {
            self.cpu.step(&mut self.chip)?;
            self.watch_handler(handling)?;
            if self.landing_stops_time(handling) {
                // Neither time nor `executed` moves here, so nothing else
                // would stop a handler that spins.
                self.stopped_steps += 1;
                if self.stopped_steps > HANDLER_BUDGET {
                    return Err(format!(
                        "{pc:#06x}: a handler ran past {HANDLER_BUDGET} instructions"
                    ));
                }
            } else {
                self.stopped_steps = 0;
                if awake {
                    self.executed += 1;
                    self.chip.elapse()?;
                }
            }
        }
        if let Some(flag) = self.chip.pending_reset.take() {
            self.reset(flag);
            return Ok(());
        }
        if self.testing == Some(self.cpu.pc) {
            self.testing = None;
            if self.cpu.sp != SRAM.end - 1 {
                return Err(format!(
                    "the self-tests left SP at {:#06x}, not the top of SRAM",
                    self.cpu.sp
                ));
            }
        }
        if self.testing.is_none() {
            self.lowest_sp = self.lowest_sp.min(self.cpu.sp);
            if self.cpu.sp < self.stack_floor {
                return Err(format!("{:#06x}: the stack ran into .bss", pc * 2));
            }
        }
        // The real chip would `reti` straight back into the main loop with
        // the flag still up, and take the interrupt again, forever. Better
        // to catch it as soon as one's enabled, before it ever fires.
        if self.cpu.flag(I) {
            let orphans = self.chip.enabled() & !self.handled;
            if orphans != 0 {
                return Err(format!(
                    "{:#06x}: enables {} with SREG.I set, but its vector goes to \
                     __unhandled_interrupt",
                    pc * 2,
                    vector_name(orphans.trailing_zeros() as u8)
                ));
            }
        }
        // A backstop only: the chip only raises enabled interrupts, and the
        // check above catches an enabled one with no handler as soon as
        // SREG.I is set, so nothing reaches here today. It's kept in case an
        // interrupt source is ever modelled without going through `enabled`.
        if self.cpu.pc == self.unhandled
            && let Some(vector) = self.cpu.handling
        {
            return Err(format!(
                "takes {}, whose vector goes to __unhandled_interrupt",
                vector_name(vector)
            ));
        }
        self.land()
    }

    /// Runs until PC reaches `target`, a word address.
    fn run_to(&mut self, target: u32, budget: u64) -> Result<(), String> {
        for _ in 0..budget {
            if self.cpu.pc == target {
                return Ok(());
            }
            self.step()?;
        }
        Err(format!(
            "didn't reach {:#06x} within {budget} instructions",
            target * 2
        ))
    }

    /// One pass of the main loop with the switch as given: runs until the
    /// firmware picks up the next tick. Returns how many instructions it took,
    /// which stops at the start of a counted wait: that takes the rest of the
    /// tick on purpose, and it can't overrun, since the tick's what ends it.
    /// With the 32 kHz oscillator dead there are no ticks to overrun at all.
    fn tick(&mut self, pressed: bool) -> Result<u64, String> {
        self.chip.set_switch(pressed)?;
        let (start, ticks) = (self.executed, self.chip.ticks);
        let mut pass_end = None;
        self.chip.tca_polled = false;
        while self.chip.ticks == ticks {
            let ran = pass_end.unwrap_or(self.executed) - start;
            if ran > TICK_BUDGET && self.chip.world.osc32k != 0 {
                return Err(format!(
                    "a pass of the main loop ran past {TICK_BUDGET} instructions, \
                     so it hangs or misses 64 Hz ticks"
                ));
            }
            self.step()?;
            if self.chip.tca_polled {
                pass_end.get_or_insert(self.executed);
            }
        }
        // A counted wait polls right up to the tick, which can land between
        // the two bytes of a read of PER. Interrupts are off, so it finishes.
        if let (Some((low, _)), None) = (self.chip.temp, pass_end) {
            return Err(format!("left {} half done", name(low)));
        }
        self.counted = pass_end.is_some();
        Ok(pass_end.unwrap_or(self.executed) - start)
    }
}

struct Chip {
    flash: Vec<u8>,
    sram: Vec<u8>,
    /// Plain storage for the registers that are only storage.
    io: Vec<u8>,
    /// The time, counted an instruction at a time while the CPU's awake,
    /// and jumping on to the next PIT period while it waits for one.
    now: u64,
    /// The last instruction a protected register still takes a write on.
    unlocked_until: Option<u64>,
    /// Whether the CCP period's still holding interrupts off. The key's first
    /// store to the bus or `sleep` ends it, whatever the store was
    /// (§8.5.7.1). `take_protected_write` doesn't look at this, so a late
    /// protected write within the four still goes through here; layout.rs
    /// refuses anything between the key and its write.
    ccp_hold: bool,
    pressed: bool,
    pit_busy: u8,
    /// RTC.PITINTFLAGS.PI.
    pit_flag: bool,
    /// A landed tick that isn't one: the interrupt's pending, but a read of
    /// the flag doesn't show it, since nothing but a real tick can end a
    /// counted wait, and `Landings` mustn't be what fails the clock check.
    pit_probe: bool,
    /// PORTA.INTFLAGS.
    porta_flags: u8,
    /// When the PIT's current period ends.
    next_pit: u64,
    /// PIT periods so far with the PIT on.
    ticks: u32,
    /// TCA0's 16-bit registers, in `WIDE`'s order.
    wide: [u16; 5],
    /// TCA0's TEMP byte, and which register's access it's holding. There's
    /// one for all the 16-bit registers (§20.5.13): a write puts the low byte
    /// here for its high byte to meet, and a read of the low byte puts the
    /// high byte here for the read after it. So either overwrites the other.
    temp: Option<(usize, u8)>,
    /// TCA0.CMP0 and CMP1, the live values.
    compare: [u16; 2],
    /// When the counter next reaches BOTTOM.
    next_bottom: u64,
    /// TCA0.CTRLF's PERBV, CMP0BV and CMP1BV: which of `BUFFERED` hold a value
    /// the next UPDATE takes.
    buffer_valid: [bool; 3],
    /// TCA0.CTRLE.LUPD.
    lupd: bool,
    /// Drops writes that set LUPD, for `lock_taken_out`.
    lupd_broken: bool,
    /// In power-down, so TCA0's stopped.
    tca_frozen: bool,
    /// The low byte of the TCA0 register the running handler caught halfway
    /// through an access, which is what it mustn't disturb.
    interrupted: Option<usize>,
    /// Times `sleep` has run in power-down.
    power_downs: u64,
    /// What the ADC measures.
    world: World,
    adc: Adc,
    wdt: Wdt,
    crc: CrcScan,
    /// RSTCTRL.RSTFR.
    rstfr: u8,
    /// A reset that's happened, for `Sim::step` to carry out: which RSTFR
    /// flag it sets.
    pending_reset: Option<u8>,
    /// Resets so far, not counting power-on.
    resets: u32,
    /// Whether a bite resets the chip, as on the real thing, or fails the
    /// firmware, which is what it's for in normal use.
    bites: bool,
    /// The CPU's stopped dead, standing in for a hang. Time goes on, and a
    /// reset ends it.
    stuck: bool,
    tcb: Tcb,
    /// The waveform outputs WO0 and WO1: set at BOTTOM unless the compare's
    /// 0, cleared on a compare match (§20.3.3.4.3). Nothing moves them while
    /// the timer isn't counting.
    wo: [bool; 2],
    /// How far the counter had got when `wo` was last brought up to date.
    wo_counted: u64,
    /// TCA0.INTFLAGS.OVF.
    tca_ovf: bool,
    /// TCA0 ignores commands too, which a stopped one might, for all the
    /// datasheet says.
    tca_deaf: bool,
    /// A BOTTOM under LUPD clears BV, with nothing copied: see
    /// `Chip::locked_bottom`.
    bv_adverse: bool,
    /// Where in a lock the next `Chip::adverse_bottom` lands, 1 to 4.
    adverse_at: u8,
    /// Buffers written since LUPD went on.
    lock_writes: u8,
    /// Times a locked BOTTOM has cleared any buffer's BV.
    bv_cleared: u32,
    /// Whether anything's read TCA0.INTFLAGS, which only a counted wait does.
    tca_polled: bool,
}

/// TCB0, as the deadline for a tick, beyond the registers in `io`.
#[derive(Default)]
struct Tcb {
    /// TCB0's TEMP byte, and which register's low byte it holds.
    temp: Option<(usize, u8)>,
    /// TCB0.CCMP.
    top: u16,
    /// When CNT was last 0.
    zero_at: u64,
    /// TCB0.INTFLAGS.CAPT.
    capt: bool,
    /// A landed deadline that isn't one, like `Chip::pit_probe`.
    probe: bool,
}

/// The watchdog, as the firmware can see it. Its settings are in
/// `io[WDT_CTRLA]`.
///
/// It's timed in PIT periods, not instructions: `Chip::advance_to` counts
/// one at the end of each, however time got there (a period of
/// instructions, a poll of the PIT flag or asleep), and a period is always
/// 16 WDT clocks, since the PIT and the WDT share an oscillator. Counting
/// whole periods from a feed is exact when the feeds follow ticks, as the
/// firmware's do. Before the first feed it counts from the PIT's period,
/// not from the CTRLA write, which is microseconds after reset today,
/// against 16 clocks of slack.
#[derive(Default)]
struct Wdt {
    /// Ticks since the last `wdr`, or since CTRLA was written.
    ticks: u32,
    /// Whether a `wdr` has started window mode's closed windows. Until the
    /// first one the WDT times out after PERIOD alone (§19.3.3.2).
    windowed: bool,
    /// Reads of WDT.STATUS that still report SYNCBUSY after a CTRLA write.
    busy: u8,
    /// Whether a `wdr` has gone through, for `WdtFault::Stops`.
    fed: bool,
}

/// WDT clocks in a WDT.CTRLA PERIOD or WINDOW setting, 0 for off.
fn wdt_clocks(setting: u8) -> u32 {
    match setting {
        0 => 0,
        n => 8 << (n - 1),
    }
}

/// CRCSCAN's state beyond its plain registers.
struct CrcScan {
    /// The instruction that last wrote ENABLE, and what STATUS read just
    /// before it.
    started: Option<(u64, u8)>,
    /// Reads of STATUS still to show BUSY.
    busy_reads: u8,
    /// STATUS once the scan's done. OK from reset (DS §27.5.3), before any
    /// scan, which is why the firmware has to wait for its own.
    status: u8,
}

/// The scan starts three cycles after ENABLE, and the CPU keeps running
/// until then (DS §27.3.1). No instruction is quicker than a cycle, so at
/// most this many instructions see STATUS from before the scan.
const CRC_START_INSTRUCTIONS: u64 = 3;

/// Reads of STATUS that show BUSY once the scan's started. In priority mode
/// the scan stalls the CPU until it's done, so a real chip may never show
/// it; this makes sure the firmware waits if it does.
const CRC_BUSY_READS: u8 = 2;

/// Instructions after which a scan's over even if nothing polled it. The
/// real one stalls the CPU for 8,192 words at three clocks each (§27.3.2),
/// so this is generous.
const CRC_DONE_INSTRUCTIONS: u64 = 100;

/// ADC0's state beyond its plain registers.
#[derive(Default)]
struct Adc {
    /// The reference and input the last conversion used, to spot a switch.
    last_setup: Option<(u8, u8, u8)>,
    /// The result of the last conversion started.
    result: u16,
    /// When that conversion finishes, while its result's unread.
    ready_at: Option<u64>,
    /// The high byte of RES, latched into ADC0.TEMP by reading the low byte.
    temp: Option<u8>,
}

/// Instructions a conversion takes on the simulated chip. Four accumulated
/// conversions with the delays `hw::init` sets are about 165 ADC clocks, 1,320
/// CPU cycles, so this is roughly that, and well inside a tick, so a firmware
/// that reads the result straight after starting a conversion finds it isn't
/// ready.
const CONVERSION_INSTRUCTIONS: u64 = 165 * (CLK_PER / 625_000) as u64;

/// What the first conversion after a change of reference or input reads.
/// The datasheet doesn't promise it's wrong, but Anduril finds it unstable,
/// so the model makes it as wrong as it gets. The firmware throws it away;
/// if it ever stopped, the garbage would cap it at level 75, which the
/// in-step check only catches once the UI goes above that.
const UNSETTLED: u16 = 0;

/// CPU cycles a second, from `hw::init`'s clock setup.
const CLK_PER: u32 = 5_000_000;

impl Chip {
    fn new(flash: Vec<u8>) -> Self {
        Self {
            flash,
            sram: vec![JUNK; usize::from(SRAM.end - SRAM.start)],
            io: vec![0; 0x1000],
            now: 0,
            unlocked_until: None,
            ccp_hold: false,
            pressed: false,
            pit_busy: PIT_SYNC_READS,
            pit_flag: false,
            pit_probe: false,
            porta_flags: 0,
            next_pit: PIT_PERIOD,
            ticks: 0,
            // PER and PERBUF reset to 0xFFFF, the rest to 0.
            wide: [0, 0xFFFF, 0xFFFF, 0, 0],
            temp: None,
            compare: [0; 2],
            next_bottom: 0,
            buffer_valid: [false; 3],
            lupd: false,
            lupd_broken: false,
            tca_frozen: false,
            interrupted: None,
            power_downs: 0,
            world: World::NORMAL,
            adc: Adc::default(),
            wdt: Wdt::default(),
            crc: CrcScan {
                started: None,
                busy_reads: 0,
                status: regs::CRCSCAN_OK,
            },
            rstfr: regs::RSTCTRL_PORF,
            pending_reset: None,
            resets: 0,
            bites: false,
            stuck: false,
            tcb: Tcb::default(),
            wo: [false; 2],
            wo_counted: 0,
            tca_ovf: false,
            tca_deaf: false,
            bv_adverse: false,
            adverse_at: 1,
            lock_writes: 0,
            bv_cleared: 0,
            tca_polled: false,
        }
    }

    /// The open window in WDT clocks, which is the whole timeout in normal
    /// mode, or 0 with the WDT off.
    fn wdt_open(&self) -> u32 {
        wdt_clocks(self.io[reg::WDT_CTRLA] & WDT_PERIOD)
    }

    /// The closed window in WDT clocks, 0 in normal mode or before the first
    /// `wdr` starts window mode.
    fn wdt_closed(&self) -> u32 {
        if self.wdt.windowed {
            wdt_clocks((self.io[reg::WDT_CTRLA] & WDT_WINDOW) >> 4)
        } else {
            0
        }
    }

    /// Whether `hw::init` turned the watchdog on.
    fn watchdog_running(&self) -> bool {
        self.wdt_open() > 0
    }

    /// Whether it's set up for window mode, whether or not a `wdr` has
    /// started it yet.
    fn watchdog_windowed(&self) -> bool {
        self.watchdog_running() && self.io[reg::WDT_CTRLA] & WDT_WINDOW != 0
    }

    /// Whether the amber button LED on PB5 is lit by its pull-up: an input
    /// with PULLUPEN set, which is how docs/ui.md says it always is. Only
    /// VPORTB.DIR is stored; if PORTB.DIR and friends get modelled, they
    /// have to write through to it, or this and `leds` go stale.
    fn button_led_lit(&self) -> bool {
        const AUX: u8 = 1 << 5;
        self.io[reg::VPORTB_DIR] & AUX == 0
            && self.io[reg::PORTB_PIN0CTRL + 5] & regs::PORT_PULLUPEN != 0
    }

    /// Spends the CCP key on a write to protected register `a`, or refuses
    /// the write if there's no key to spend.
    fn take_protected_write(&mut self, a: usize) -> Result<(), String> {
        if !self.unlocked_until.is_some_and(|until| self.now <= until) {
            return Err(format!(
                "writes {} without the CCP key in the four instructions before, \
                 and the chip ignores that write",
                name(a)
            ));
        }
        self.unlocked_until = None;
        Ok(())
    }

    /// Moves time on to `to`, and runs whatever falls due on the way. This
    /// is the one place simulated time moves, awake, asleep or polling, so
    /// anything clocked off it (the PIT, and the watchdog off the same
    /// 32 kHz oscillator) is driven from here.
    fn advance_to(&mut self, to: u64) -> Result<(), String> {
        self.now = to;
        self.tca_advance()?;
        while self.now >= self.next_pit {
            self.pit_period()?;
        }
        while let Some(due) = self.tcb_due().filter(|&due| self.now >= due) {
            self.tcb.zero_at = due;
            self.tcb.capt = true;
        }
        Ok(())
    }

    /// One instruction's worth of time.
    fn elapse(&mut self) -> Result<(), String> {
        self.advance_to(self.now + 1)
    }

    /// INVEN on PA5 would flip both VPORTA.IN and which edge is which
    /// (DS40002204A §16.3.2.2), and the model does neither, so it refuses it.
    fn switch_not_inverted(&self) -> Result<(), String> {
        if self.io[reg::PORTA_PIN0CTRL + 5] & PORT_INVEN != 0 {
            return Err("sets INVEN on PA5, which the simulator doesn't model".into());
        }
        Ok(())
    }

    /// What has to be true before `sleep` in power-down. The main clock
    /// stops, which freezes TCA0 and the ADC where they are, and the chip
    /// stays there until something wakes it.
    fn ready_for_power_down(&self) -> Result<(), String> {
        if self.pit_busy > 0 && !self.osc32k_dead() {
            return Err(
                "powers down with RTC.PITSTATUS.CTRLBUSY still set, and the datasheet \
                        doesn't say the write finishes syncing with CLK_PER stopped"
                    .into(),
            );
        }
        if self.converting() {
            return Err(
                "powers down mid-conversion, which resumes on waking with an \
                        invalid result (section 30.3.5)"
                    .into(),
            );
        }
        if self.io[reg::ADC0_CTRLA] & regs::ADC_ENABLE != 0 {
            return Err("powers down with ADC0 still enabled".into());
        }
        if self.io[reg::VREF_CTRLB] & regs::VREF_ADC0REFEN != 0 {
            return Err(
                "powers down with VREF.CTRLB.ADC0REFEN set, forcing the reference on \
                        for as long as it sleeps"
                    .into(),
            );
        }
        if self.io[reg::TCA0_CTRLB] & (regs::TCA_CMP0EN | regs::TCA_CMP1EN) != 0 {
            return Err(
                "powers down with a TCA0 channel still driving its pin, which the \
                        datasheet doesn't promise a level for"
                    .into(),
            );
        }
        let bufs = [self.wide(reg::TCA0_CMP0BUF), self.wide(reg::TCA0_CMP1BUF)];
        if self.compare != [0; 2] || bufs != [0; 2] {
            return Err(format!(
                "powers down with TCA0's compares at {:?} and their BUFs at {bufs:?}, \
                 so the light's on or could come back on at that duty",
                self.compare
            ));
        }
        if self.buffer_valid != [false; 3] {
            return Err(format!(
                "powers down with TCA0's buffers {:?} still waiting for an UPDATE, \
                 and TCA0 stops, so they'd land on waking",
                self.buffer_valid
            ));
        }
        let per = self.wide(reg::TCA0_PER);
        if per != ramp::Output::OFF.top - 1 {
            return Err(format!(
                "powers down with TCA0.PER at {per}, not the off output's"
            ));
        }
        Ok(())
    }

    /// Presses or releases the switch on PA5. A change sets PA5's flag in
    /// PORTA.INTFLAGS if its ISC asks for that edge (DS40002204A §16.5.10).
    fn set_switch(&mut self, pressed: bool) -> Result<(), String> {
        if pressed == self.pressed {
            return Ok(());
        }
        self.pressed = pressed;
        self.switch_not_inverted()?;
        let sense = match self.io[reg::PORTA_PIN0CTRL + 5] & ISC {
            ISC_INTDISABLE | ISC_INPUT_DISABLE => false,
            ISC_BOTHEDGES => true,
            ISC_RISING => !pressed,
            ISC_FALLING => pressed,
            isc => {
                return Err(format!(
                    "PA5's ISC is {isc:#x}, which the simulator doesn't model"
                ));
            }
        };
        if sense {
            self.porta_flags |= SWITCH;
        }
        Ok(())
    }

    /// A bit per vector whose interrupt is enabled. A port's is enabled
    /// when any of its pins' ISC is.
    fn enabled(&self) -> u32 {
        self.interrupts(|_, isc| {
            matches!(isc, ISC_BOTHEDGES | ISC_RISING | ISC_FALLING | ISC_LEVEL)
        })
    }

    /// [`Chip::enabled`] for power-down, with the peripheral clock stopped.
    /// Only pins 2 and 6 are fully asynchronous. The rest wake it on
    /// BOTHEDGES or LEVEL and no other sense (table 16-3).
    fn enabled_in_power_down(&self) -> u32 {
        self.interrupts(|pin, isc| match isc {
            ISC_BOTHEDGES | ISC_LEVEL => true,
            ISC_RISING | ISC_FALLING => pin == 2 || pin == 6,
            _ => false,
        })
    }

    /// A bit per vector, a port's being set when `senses` is true of any of
    /// its pins' number and ISC, and the PIT's when its interrupt's on.
    fn interrupts(&self, senses: impl Fn(usize, u8) -> bool) -> u32 {
        let port = |base: usize, vector: usize| {
            let on = (0..8).any(|pin| senses(pin, self.io[base + pin] & ISC));
            u32::from(on) << vector
        };
        let pit = self.io[reg::RTC_PITINTCTRL] & regs::RTC_PI != 0;
        let tcb = self.io[reg::TCB0_INTCTRL] & regs::TCB_CAPT != 0;
        port(reg::PORTA_PIN0CTRL, VECTOR_PORTA_PORT)
            | port(reg::PORTB_PIN0CTRL, VECTOR_PORTB_PORT)
            | port(reg::PORTC_PIN0CTRL, VECTOR_PORTC_PORT)
            | u32::from(pit) << regs::VECTOR_RTC_PIT
            | u32::from(tcb) << regs::VECTOR_TCB0_INT
    }

    /// Jumps to the end of the PIT's period, as if the CPU had waited for
    /// it, or to TCB0's TOP if that's sooner. The PIT runs in every sleep
    /// mode (DS40002204A §23.8), and so does the ADC in idle, so a conversion
    /// that was running has finished. TCB0 stops in power-down (§11.3.2.1
    /// gives it no way to run there).
    fn wait_for_pit(&mut self) -> Result<(), String> {
        if self.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN == 0 && !self.osc32k_dead() {
            return Err("waits for the PIT with the PIT off, so it would wait forever".into());
        }
        if self.powering_down() {
            self.tcb.zero_at += self.next_pit - self.now;
            return self.advance_to(self.next_pit);
        }
        let due = self.tcb_due().unwrap_or(self.next_pit);
        self.advance_to(self.next_pit.min(due))
    }

    /// The PIT's period ends: its flag goes up whether or not the
    /// interrupt's enabled, and the watchdog counts another 16 clocks. With
    /// the 32 kHz oscillator dead all that happens is a tick's time passing.
    fn pit_period(&mut self) -> Result<(), String> {
        self.next_pit += self.pit_cycles();
        if self.osc32k_dead() {
            self.ticks += 1;
            return Ok(());
        }
        if self.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN != 0 {
            self.pit_flag = true;
            self.ticks += 1;
        }
        self.wdt_period()
    }

    /// A PIT period's worth of WDT clocks, while the watchdog's on. It runs
    /// in every sleep mode (DS40002204A §19.3.4), so this counts asleep too.
    fn wdt_period(&mut self) -> Result<(), String> {
        if !self.watchdog_running() {
            return Ok(());
        }
        let pit = self.io[reg::RTC_PITCTRLA] & !regs::RTC_PITEN;
        if pit != regs::RTC_PERIOD_CYC512 {
            return Err(format!(
                "ticks with RTC.PITCTRLA.PERIOD at {pit:#04x} and the watchdog \
                 on; the WDT model only knows a tick as 16 WDT clocks, at CYC512"
            ));
        }
        self.wdt.ticks += 1;
        let ticks = self.wdt.ticks;
        let clocks = ticks * WDT_CLOCKS_PER_TICK;
        let timeout = self.wdt_closed() + self.wdt_open();
        if self.wdt_times_out(clocks, timeout) {
            return self.bite(format!(
                "the watchdog went {ticks} ticks ({clocks} WDT clocks) {WDT_TIMED_OUT}, \
                 against a {timeout}-clock timeout, so the chip would have reset"
            ));
        }
        Ok(())
    }

    /// CRCSCAN.STATUS as a read sees it now.
    fn crc_status(&mut self) -> u8 {
        match self.crc.started {
            Some((at, before)) if self.now <= at + CRC_START_INSTRUCTIONS => before,
            Some(_) if self.crc_busy() => {
                self.crc.busy_reads -= 1;
                regs::CRCSCAN_BUSY
            }
            _ => self.crc.status,
        }
    }

    /// Whether a scan's still running. It ends after its stale window and
    /// BUSY reads, or after `CRC_DONE_INSTRUCTIONS` whether or not anything
    /// read STATUS, as the real scan finishes on its own.
    fn crc_busy(&self) -> bool {
        self.crc.started.is_some_and(|(at, _)| {
            self.now <= at + CRC_START_INSTRUCTIONS
                || (self.crc.busy_reads > 0 && self.now <= at + CRC_DONE_INSTRUCTIONS)
        })
    }

    /// Starts a whole-flash scan: CRC-16-CCITT over every byte of flash,
    /// erased bytes reading 0xFF, passing if it comes to 0.
    fn start_crc(&mut self) -> Result<(), String> {
        if self.crc_busy() {
            return Err(
                "starts a CRC scan while one's still running, which the datasheet \
                 doesn't say what it does"
                    .into(),
            );
        }
        let ctrlb = self.io[reg::CRCSCAN_CTRLB];
        if ctrlb != regs::CRCSCAN_SRC_FLASH | regs::CRCSCAN_MODE_PRIORITY {
            return Err(format!(
                "starts a CRC scan with CRCSCAN.CTRLB {ctrlb:#04x}; only the whole-flash \
                 priority scan works with BOOTEND and APPEND at 0"
            ));
        }
        let before = self.crc_status();
        let flash = self.flash.iter().copied().chain(iter::repeat(0xFF));
        let sum = crc::crc16(flash.take(FLASH_SIZE));
        self.crc = CrcScan {
            started: Some((self.now, before)),
            busy_reads: CRC_BUSY_READS,
            status: if sum == 0 { regs::CRCSCAN_OK } else { 0 },
        };
        Ok(())
    }

    fn converting(&self) -> bool {
        self.adc.ready_at.is_some_and(|at| self.now < at)
    }

    /// Starts a conversion, if the ADC's set up for one of the two
    /// measurements the firmware makes, and works out what it'll read.
    fn start_conversion(&mut self) -> Result<(), String> {
        if self.converting() {
            return Err("starts a conversion while one's still running".into());
        }
        let io = |r: usize| self.io[r];
        if io(reg::ADC0_CTRLA) != regs::ADC_ENABLE {
            return Err(format!(
                "starts a conversion with ADC0.CTRLA {:#04x}; it wants the ADC enabled, \
                 single conversions and 10-bit results",
                io(reg::ADC0_CTRLA)
            ));
        }
        if io(reg::VREF_CTRLB) & regs::VREF_ADC0REFEN == 0 {
            return Err(
                "starts a conversion with VREF.CTRLB.ADC0REFEN clear, so the reference \
                 may still be starting up"
                    .into(),
            );
        }
        if io(reg::ADC0_CALIB) != regs::ADC_DUTYCYC_DUTY25 {
            return Err("starts a conversion without ADC0.CALIB at 25% duty".into());
        }
        let ctrlc = io(reg::ADC0_CTRLC);
        let clk_adc = CLK_PER >> (1 + u32::from(ctrlc & regs::ADC_PRESC));
        if !(200_000..=1_500_000).contains(&clk_adc) {
            return Err(format!(
                "runs the ADC at {clk_adc} Hz, outside 200 kHz to 1.5 MHz (table 36-26)"
            ));
        }
        let vref = io(reg::VREF_CTRLA) & regs::VREF_ADC0REFSEL;
        let refsel = ctrlc & regs::ADC_REFSEL;
        let muxpos = io(reg::ADC0_MUXPOS);
        let sampnum = io(reg::ADC0_CTRLB) & regs::ADC_SAMPNUM;
        let sampcap = ctrlc & regs::ADC_SAMPCAP != 0;
        let channel = match (muxpos, refsel, vref, sampnum) {
            (
                regs::ADC_MUXPOS_INTREF,
                regs::ADC_REFSEL_VDDREF,
                regs::VREF_ADC0REFSEL_1V5,
                regs::ADC_SAMPNUM_ACC4,
            ) => Channel::Voltage,
            (
                regs::ADC_MUXPOS_TEMPSENSE,
                regs::ADC_REFSEL_INTREF,
                regs::VREF_ADC0REFSEL_1V1,
                regs::ADC_SAMPNUM_ACC4,
            ) => {
                // Section 30.3.2.6: SAMPCAP, and INITDLY and SAMPLEN each at
                // least 32 µs of ADC clock.
                let initdly = match io(reg::ADC0_CTRLD) >> 5 {
                    0 => 0,
                    n => 8u32 << n,
                };
                let samplen = u32::from(io(reg::ADC0_SAMPCTRL) & regs::ADC_SAMPLEN);
                let need = clk_adc.div_ceil(1_000_000 / 32);
                if !sampcap || initdly < need || samplen < need {
                    return Err(format!(
                        "reads the temperature sensor with SAMPCAP {sampcap}, INITDLY {initdly} \
                         and SAMPLEN {samplen}; it needs SAMPCAP and both at least {need} clocks"
                    ));
                }
                Channel::Temperature
            }
            _ => {
                return Err(format!(
                    "starts a conversion that's neither measurement: MUXPOS {muxpos:#04x}, \
                     REFSEL {refsel:#04x}, ADC0REFSEL {vref:#04x}, SAMPNUM {sampnum}"
                ));
            }
        };
        let setup = Some((vref, refsel, muxpos));
        self.adc.result = if self.adc.last_setup == setup {
            self.world.reading(channel)
        } else {
            UNSETTLED
        };
        self.adc.last_setup = setup;
        self.adc.ready_at = Some(self.now + CONVERSION_INSTRUCTIONS);
        Ok(())
    }

    /// Fills a section of the periodic RAM test with bytes that differ from
    /// each other and every background, with bit 7 high.
    fn write_section(&mut self, start: u16) {
        let from = usize::from(start - SRAM.start);
        for (i, byte) in self.sram[from..from + usize::from(selftest::RAM_SECTION)]
            .iter_mut()
            .enumerate()
        {
            *byte = 0x80 | i as u8;
        }
    }

    fn sram(&self, range: &Range<u16>) -> &[u8] {
        &self.sram[usize::from(range.start - SRAM.start)..usize::from(range.end - SRAM.start)]
    }

    fn wide(&self, addr: usize) -> u16 {
        WIDE.iter()
            .position(|&a| a == addr)
            .map_or(0, |i| self.wide[i])
    }

    /// Whether the clock and TCA0 are what `CLK_PER` and the off output say.
    /// Writes of anything else are refused, but this is what catches a write
    /// that never happened: the reset values are a 3.3 MHz clock and a PER
    /// of 0xFFFF, which leaves full duty nearly dark.
    fn clocked_as_modelled(&self) -> bool {
        self.io[reg::CLKCTRL_MCLKCTRLB] == regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN
            && self.io[reg::TCA0_CTRLA] == regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE
            && self.io[reg::TCA0_CTRLB] & TCA_WGMODE == regs::TCA_WGMODE_SINGLESLOPE
            && self.wide(reg::TCA0_PER) == ramp::Output::OFF.top - 1
    }

    /// (linear, FET) compares as the pins see them: TCA0 running, the compare
    /// channel enabled and the pin set as an output. These are the live
    /// values, so an update that never landed shows.
    ///
    /// A waveform output only overrides PORT, whose OUT is 0 here, with its
    /// channel enabled (§20.3.3.4.5). A timer that's counting makes the duty
    /// its compare says. A timer that isn't holds the pin at whatever its
    /// output was, which is fully on or fully off, whatever the compare's
    /// been set to since, and so is an output that hasn't reached BOTTOM
    /// after its compare went to 0.
    fn leds(&self) -> (u16, u16) {
        let top = self.wide(reg::TCA0_PER).wrapping_add(1);
        let channel = |enable: u8, pin: u8, i: usize| {
            let driving =
                self.io[reg::TCA0_CTRLB] & enable != 0 && self.io[reg::VPORTB_DIR] & pin != 0;
            let held = self.wo[i] && (self.compare[i] == 0 || !self.tca_counts());
            match driving {
                false => 0,
                true if held => top,
                true if self.tca_counts() => self.compare[i],
                true => 0,
            }
        };
        (
            channel(regs::TCA_CMP1EN, LINEAR, 1),
            channel(regs::TCA_CMP0EN, FET, 0),
        )
    }

    /// Whether the counter's moving, so the output's a PWM.
    fn tca_counts(&self) -> bool {
        self.tca_running() && !self.tca_frozen
    }
}

/// The two clocks and what's timed by them, for the clock check.
impl Chip {
    fn osc32k_dead(&self) -> bool {
        self.world.osc32k == 0
    }

    /// Whether `sleep` means power-down right now.
    fn powering_down(&self) -> bool {
        self.io[reg::SLPCTRL_CTRLA] == regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN
    }

    /// A new world, just after a tick. The period that's started runs at
    /// the new speeds, so the count the library makes from the same world
    /// is the one the firmware sees.
    fn set_world(&mut self, world: World) {
        let started = self.next_pit - self.pit_cycles();
        self.world = world;
        self.next_pit = started + self.pit_cycles();
    }

    /// Main clocks in a PIT period, with the clocks at the world's speeds. A
    /// dead oscillator has no period, so this stands in the nominal one.
    fn pit_cycles(&self) -> u64 {
        let osc32k = if self.osc32k_dead() {
            100
        } else {
            self.world.osc32k
        };
        PIT_PERIOD * u64::from(self.world.main_clock) / u64::from(osc32k)
    }

    /// When TCB0 next reaches TOP, if it's running. TOP + 1 of its clocks,
    /// at two main clocks each.
    fn tcb_due(&self) -> Option<u64> {
        let on = self.io[reg::TCB0_CTRLA] & regs::TCB_ENABLE != 0;
        on.then(|| self.tcb.zero_at + 2 * (u64::from(self.tcb.top) + 1))
    }

    /// A write to TCB0's registers, all of which only take what `hw::init`
    /// and `hw::wait_for_tick` write, since this models nothing else.
    fn write_tcb(&mut self, a: usize, value: u8) -> Result<(), String> {
        match a {
            reg::TCB0_CTRLA => {
                if value != regs::TCB_CLKSEL_CLKDIV2 | regs::TCB_ENABLE {
                    return Err(format!(
                        "writes {value:#04x} to TCB0.CTRLA; only CLK_PER / 2 is modelled"
                    ));
                }
                self.io[a] = value;
                self.tcb.zero_at = self.now;
            }
            reg::TCB0_INTCTRL => self.io[a] = value,
            reg::TCB0_INTFLAGS => {
                if value & regs::TCB_CAPT != 0 {
                    self.tcb.capt = false;
                    self.tcb.probe = false;
                }
            }
            reg::TCB0_CNT | reg::TCB0_CCMP => {
                if let Some((pending, _)) = self.tcb.temp {
                    return Err(format!(
                        "writes the low byte of {} while {}'s is still waiting for its high byte",
                        name(a),
                        name(pending)
                    ));
                }
                self.tcb.temp = Some((a, value));
            }
            _ => match self.tcb.temp.take() {
                Some((low, lo)) if low == a - 1 => {
                    let wide = u16::from_le_bytes([lo, value]);
                    if low == reg::TCB0_CCMP {
                        self.tcb.top = wide;
                    } else if wide == 0 {
                        self.tcb.zero_at = self.now;
                    } else {
                        return Err(format!("sets TCB0.CNT to {wide}, not 0"));
                    }
                }
                _ => {
                    return Err(format!(
                        "writes the high byte of {} without its low byte just before, \
                         so the chip pairs it with whatever TEMP held",
                        name(a - 1)
                    ));
                }
            },
        }
        Ok(())
    }

    /// A write straight to the high byte of TCA0.CMP0 or CMP1, which `a` is,
    /// with the low byte in TEMP. It takes effect now, not at BOTTOM, so with
    /// the timer running only 0 is safe: anything else could go over a PER
    /// that's waiting for its UPDATE, and hold the pin high for the cycle.
    fn write_live(&mut self, a: usize, value: u8) -> Result<(), String> {
        match self.temp.take() {
            Some((low, lo)) if low == a - 1 => {
                let wide = u16::from_le_bytes([lo, value]);
                if wide != 0 && self.tca_running() {
                    return Err(format!(
                        "writes {wide} straight to {} with the timer running, which \
                         lands at once against whatever PER is, so a compare over it \
                         holds the pin high for the cycle: use its BUF",
                        name(low)
                    ));
                }
                self.compare[usize::from(low == reg::TCA0_CMP1)] = wide;
                Ok(())
            }
            _ => Err(format!(
                "writes the high byte of {} without its low byte just before, \
                 so the chip pairs it with whatever TEMP held",
                name(a - 1)
            )),
        }
    }
}

/// TCB0's registers the firmware writes, high bytes included.
fn tcb0(addr: usize) -> bool {
    matches!(
        addr,
        reg::TCB0_CTRLA | reg::TCB0_INTCTRL | reg::TCB0_INTFLAGS
    ) || (reg::TCB0_CNT..reg::TCB0_CCMP + 2).contains(&addr)
}

/// TCA0's buffer registers, in `Chip::buffer_valid`'s order.
const BUFFERED: [usize; 3] = [reg::TCA0_PERBUF, reg::TCA0_CMP0BUF, reg::TCA0_CMP1BUF];

/// TCA0's counter and its UPDATE, in single-slope mode.
impl Chip {
    /// Whether TCA0's on and hasn't broken.
    fn tca_running(&self) -> bool {
        self.io[reg::TCA0_CTRLA] & regs::TCA_ENABLE != 0 && !self.world.tca_stopped
    }

    /// Clocks from one BOTTOM to the next (§20.3.3.4.3).
    fn tca_cycle(&self) -> u64 {
        u64::from(self.wide(reg::TCA0_PER)) + 1
    }

    /// Runs the counter up to `now`. A stopped timer stays a full cycle away
    /// from BOTTOM, which is where it starts. Each BOTTOM is an overflow, and
    /// PER's what it was for the cycle that ended, until the UPDATE there.
    fn tca_advance(&mut self) -> Result<(), String> {
        if self.tca_frozen {
            return Ok(());
        }
        if !self.tca_running() {
            self.next_bottom = self.now + self.tca_cycle();
            return Ok(());
        }
        while self.now >= self.next_bottom {
            self.tca_ovf = true;
            self.tca_update()?;
            self.next_bottom += self.tca_cycle();
            self.wo = self.compare.map(|compare| compare != 0);
            self.wo_counted = 0;
        }
        self.tca_compare_match();
        Ok(())
    }

    /// Clears each waveform output whose compare the counter has reached
    /// since this last looked. Writing a compare the counter's already past
    /// matches nothing, so the output stays as it is until BOTTOM.
    fn tca_compare_match(&mut self) {
        let left = self.next_bottom.saturating_sub(self.now);
        let count = self.tca_cycle().saturating_sub(left);
        for (wo, compare) in iter::zip(&mut self.wo, self.compare) {
            let compare = u64::from(compare);
            *wo &= !(self.wo_counted <= compare && compare <= count);
        }
        self.wo_counted = count;
    }

    /// (PER, [CMP0, CMP1]) as the next UPDATE leaves them.
    fn tca_after_update(&self) -> (u16, [u16; 2]) {
        let value = |i: usize, live: u16| {
            if self.buffer_valid[i] {
                self.wide(BUFFERED[i])
            } else {
                live
            }
        };
        (
            value(0, self.wide(reg::TCA0_PER)),
            [value(1, self.compare[0]), value(2, self.compare[1])],
        )
    }

    /// The UPDATE condition, at BOTTOM. LUPD holds it off, and the buffers
    /// stay valid for the next one.
    fn tca_update(&mut self) -> Result<(), String> {
        if self.lupd && self.bv_adverse {
            self.locked_bottom();
        }
        if self.lupd || self.buffer_valid == [false; 3] {
            return Ok(());
        }
        self.tca_check_update()?;
        let (per, compare) = self.tca_after_update();
        let i = WIDE.iter().position(|&a| a == reg::TCA0_PER);
        self.wide[i.unwrap_or_default()] = per;
        self.compare = compare;
        self.buffer_valid = [false; 3];
        Ok(())
    }

    /// The other way to read a BOTTOM under LUPD: BV clears on an UPDATE
    /// condition (§20.3.3.2), and one still happens, it just copies nothing
    /// (§20.5.6). `Chip::bv_adverse` is that reading. Any buffers written so
    /// far have to be written again.
    fn locked_bottom(&mut self) {
        if self.buffer_valid != [false; 3] {
            self.bv_cleared += 1;
        }
        self.buffer_valid = [false; 3];
    }

    /// Under `Chip::bv_adverse`, a BOTTOM lands at each place in the lock in
    /// turn, one lock at a time, since a real one would only be there now and
    /// then. `at` is how many buffers have been written in this lock, 4 with
    /// the lock about to clear, after the firmware's looked at the flags, which
    /// is the one that loses a whole row, or 5 just before the UPDATE command
    /// of a turn-off, which comes before that.
    fn adverse_bottom(&mut self, at: u8) {
        if self.bv_adverse && self.lupd && at == self.adverse_at {
            self.locked_bottom();
        }
    }

    /// Whether the next UPDATE leaves TCA0 running one level's whole output.
    fn tca_check_update(&self) -> Result<(), String> {
        let (per, [fet, linear]) = self.tca_after_update();
        let whole = |o: ramp::Output| (o.top - 1, o.fet, o.linear) == (per, fet, linear);
        if (0..=ramp::LEVELS).map(ramp::output).any(whole) {
            return Ok(());
        }
        let advice = "so PERBUF and both CMPnBUFs have to land together, under LUPD";
        if fet > per {
            return Err(format!(
                "an UPDATE here runs CMP0 at {fet} against a PER of {per}, which holds \
                 the FET fully on for the cycle, {advice}"
            ));
        }
        if linear > per {
            return Err(format!(
                "an UPDATE here runs CMP1 at {linear} against a PER of {per}, which holds \
                 the linear channel fully on for the cycle, {advice}"
            ));
        }
        Err(format!(
            "an UPDATE here runs (CMP0, CMP1) of ({fet}, {linear}) against a PER of {per}, \
             which is no level's output, {advice}"
        ))
    }

    /// The firmware has finished writing a 16-bit TCA0 register.
    fn tca_written(&mut self, low: usize) -> Result<(), String> {
        if low == reg::TCA0_CNT {
            let cnt = self.wide(reg::TCA0_CNT);
            if cnt != 0 && self.tca_running() {
                return Err(format!(
                    "writes {cnt} to TCA0.CNT with the timer running, and the \
                     simulator only models restarting the cycle"
                ));
            }
            self.next_bottom = self.now + self.tca_cycle();
            self.wo_counted = 0;
        } else if low == reg::TCA0_PER {
            if self.tca_running() {
                return Err(
                    "writes TCA0.PER with the timer running, which applies at once and \
                     can wrap the counter (section 20.3.3.3): use PERBUF"
                        .into(),
                );
            }
        } else if let Some(i) = BUFFERED.iter().position(|&a| a == low) {
            self.buffer_valid[i] = true;
            self.lock_writes = self.lock_writes.saturating_add(1);
            self.adverse_bottom(self.lock_writes);
            // A BOTTOM could land straight after this write.
            if self.tca_running() && !self.lupd {
                self.tca_check_update()?;
            }
        }
        Ok(())
    }

    /// A write to TCA0.CTRLESET or CTRLECLR. The UPDATE command does what
    /// BOTTOM would, whatever LUPD says (§20.3.3.5). That it works with the
    /// counter broken is likely, not stated, since the command doesn't come
    /// from the counter. `hw::set_output` doesn't lean on it to turn off.
    fn tca_lock(&mut self, a: usize, value: u8) -> Result<(), String> {
        if a == reg::TCA0_CTRLESET && value == regs::TCA_CMD_UPDATE {
            if !self.tca_deaf {
                self.adverse_bottom(5);
                let locked = mem::replace(&mut self.lupd, false);
                self.tca_update()?;
                self.lupd = locked;
            }
            return Ok(());
        }
        if value != regs::TCA_LUPD {
            return Err(format!(
                "writes {value:#04x} to {}, but the simulator only models LUPD and \
                 the UPDATE command",
                name(a)
            ));
        }
        if a == reg::TCA0_CTRLESET {
            self.lupd = !self.lupd_broken;
            self.lock_writes = 0;
        } else {
            self.adverse_bottom(4);
            self.adverse_at = self.adverse_at % 5 + 1;
            self.lupd = false;
            // Nothing lands with no BV set (table 20-1, section 20.3.3.2).
            if self.tca_running() && self.buffer_valid != [false; 3] {
                self.tca_check_update()?;
            }
        }
        Ok(())
    }
}

/// Registers that are plain storage as far as the firmware's concerned.
fn plain(addr: usize) -> bool {
    let pin_ctrl = [
        reg::PORTA_PIN0CTRL,
        reg::PORTB_PIN0CTRL,
        reg::PORTC_PIN0CTRL,
    ]
    .iter()
    .any(|&base| (base..base + 8).contains(&addr));
    pin_ctrl
        || matches!(
            addr,
            reg::VPORTB_DIR
                | reg::CLKCTRL_MCLKCTRLB
                | reg::WDT_CTRLA
                | reg::RTC_PITCTRLA
                | reg::TCA0_CTRLB
                | reg::VREF_CTRLA
                | reg::VREF_CTRLB
                | reg::ADC0_CALIB
                | reg::ADC0_CTRLA
                | reg::ADC0_CTRLB
                | reg::ADC0_CTRLC
                | reg::ADC0_CTRLD
                | reg::ADC0_SAMPCTRL
                | reg::ADC0_MUXPOS
                | reg::GPIO_GPIOR0
                | reg::GPIO_GPIOR1
                | reg::SLPCTRL_CTRLA
                | reg::RTC_PITINTCTRL
        )
}

/// The registers that decide what a conversion measures. Changing one
/// mid-conversion is a mistake the firmware shouldn't make.
const ADC_SETUP: [usize; 9] = [
    reg::VREF_CTRLA,
    reg::VREF_CTRLB,
    reg::ADC0_CALIB,
    reg::ADC0_CTRLA,
    reg::ADC0_CTRLB,
    reg::ADC0_CTRLC,
    reg::ADC0_CTRLD,
    reg::ADC0_SAMPCTRL,
    reg::ADC0_MUXPOS,
];

fn vector_name(vector: u8) -> String {
    match usize::from(vector) {
        VECTOR_PORTA_PORT => "PORTA_PORT (vector 3)".into(),
        VECTOR_PORTB_PORT => "PORTB_PORT (vector 4)".into(),
        VECTOR_PORTC_PORT => "PORTC_PORT (vector 5)".into(),
        regs::VECTOR_RTC_PIT => "RTC_PIT (vector 7)".into(),
        regs::VECTOR_TCB0_INT => "TCB0_INT (vector 13)".into(),
        v => format!("vector {v}"),
    }
}

/// The fuses and the lock byte after them.
fn fuse_or_lock(addr: usize) -> bool {
    let start = usize::from(map::FUSES);
    (start..start + map::FUSES_SIZE).contains(&addr)
}

fn name(addr: usize) -> String {
    if let Some(fuse) = addr
        .checked_sub(usize::from(map::FUSES))
        .and_then(|offset| world::FUSE_NAMES.get(offset))
        .filter(|name| !name.is_empty())
    {
        return format!("FUSE.{fuse}");
    }
    let known = [
        (reg::CPU_CCP, "CPU.CCP"),
        (reg::RSTCTRL_RSTFR, "RSTCTRL.RSTFR"),
        (reg::RSTCTRL_SWRR, "RSTCTRL.SWRR"),
        (reg::VPORTA_IN, "VPORTA.IN"),
        (reg::VPORTB_DIR, "VPORTB.DIR"),
        (reg::CLKCTRL_MCLKCTRLB, "CLKCTRL.MCLKCTRLB"),
        (reg::WDT_CTRLA, "WDT.CTRLA"),
        (reg::WDT_STATUS, "WDT.STATUS"),
        (reg::RTC_PITCTRLA, "RTC.PITCTRLA"),
        (reg::RTC_PITSTATUS, "RTC.PITSTATUS"),
        (reg::RTC_PITINTFLAGS, "RTC.PITINTFLAGS"),
        (reg::TCA0_CTRLA, "TCA0.CTRLA"),
        (reg::TCA0_CTRLB, "TCA0.CTRLB"),
        (reg::TCA0_CTRLECLR, "TCA0.CTRLECLR"),
        (reg::TCA0_CTRLESET, "TCA0.CTRLESET"),
        (reg::TCA0_CTRLFSET, "TCA0.CTRLFSET"),
        (reg::TCA0_CTRLFCLR, "TCA0.CTRLFCLR"),
        (reg::TCA0_CNT, "TCA0.CNT"),
        (reg::TCA0_PER, "TCA0.PER"),
        (reg::TCA0_CMP0, "TCA0.CMP0"),
        (reg::TCA0_CMP1, "TCA0.CMP1"),
        (reg::TCA0_PERBUF, "TCA0.PERBUF"),
        (reg::TCA0_CMP0BUF, "TCA0.CMP0BUF"),
        (reg::TCA0_CMP1BUF, "TCA0.CMP1BUF"),
        (reg::TCA0_PER, "TCA0.PER"),
        (reg::TCA0_INTFLAGS, "TCA0.INTFLAGS"),
        (reg::TCB0_CTRLA, "TCB0.CTRLA"),
        (reg::TCB0_INTCTRL, "TCB0.INTCTRL"),
        (reg::TCB0_INTFLAGS, "TCB0.INTFLAGS"),
        (reg::TCB0_CNT, "TCB0.CNT"),
        (reg::TCB0_CCMP, "TCB0.CCMP"),
        (reg::VREF_CTRLA, "VREF.CTRLA"),
        (reg::VREF_CTRLB, "VREF.CTRLB"),
        (reg::ADC0_CALIB, "ADC0.CALIB"),
        (reg::ADC0_CTRLA, "ADC0.CTRLA"),
        (reg::ADC0_CTRLB, "ADC0.CTRLB"),
        (reg::ADC0_CTRLC, "ADC0.CTRLC"),
        (reg::ADC0_CTRLD, "ADC0.CTRLD"),
        (reg::ADC0_SAMPCTRL, "ADC0.SAMPCTRL"),
        (reg::ADC0_MUXPOS, "ADC0.MUXPOS"),
        (reg::ADC0_COMMAND, "ADC0.COMMAND"),
        (reg::ADC0_INTFLAGS, "ADC0.INTFLAGS"),
        (reg::ADC0_RES, "ADC0.RESL"),
        (reg::ADC0_RES + 1, "ADC0.RESH"),
        (reg::SIGROW_TEMPSENSE0, "SIGROW.TEMPSENSE0"),
        (reg::SIGROW_TEMPSENSE1, "SIGROW.TEMPSENSE1"),
        (reg::GPIO_GPIOR0, "GPIO.GPIOR0"),
        (reg::GPIO_GPIOR1, "GPIO.GPIOR1"),
        (reg::SLPCTRL_CTRLA, "SLPCTRL.CTRLA"),
        (reg::RTC_PITINTCTRL, "RTC.PITINTCTRL"),
        (PORTA_INTFLAGS, "PORTA.INTFLAGS"),
        (reg::CRCSCAN_CTRLA, "CRCSCAN.CTRLA"),
        (reg::CRCSCAN_CTRLB, "CRCSCAN.CTRLB"),
        (reg::CRCSCAN_STATUS, "CRCSCAN.STATUS"),
    ];
    known
        .iter()
        .find(|&&(a, _)| a == addr)
        .map_or_else(|| format!("{addr:#06x}"), |(_, n)| (*n).to_string())
}

/// What a handler found when it landed, with its return address pushed.
struct Landed {
    r: [u8; 32],
    sreg: u8,
    sp: u16,
    gpior0: u8,
    gpior1: u8,
}

/// Lands interrupts where they'd otherwise never arrive, on a firmware that
/// doesn't know the difference, so a run still has to match the library tick
/// by tick.
///
/// Before every instruction a run reaches, the first time it does, a tick
/// that isn't one: the PIT's interrupt goes up, the handler runs, and what it
/// did to GPIOR0 is put back. The real tick can't land mid-pass unless a pass
/// overruns, which `Sim::tick` already fails, and a real one here would add a
/// pass the library doesn't have. This is there so `rtc_pit` gets looked at
/// wherever a stretched pass could put it. Then the second time, a deadline
/// that isn't one, for `tcb0_int`, the same way with GPIOR1.
///
/// Neither shows up in a read of its flag, since a counted wait polls those
/// with interrupts off, and a landing that ended a count would fail a good
/// chip. They just wait out the `cli`.
///
/// A landing with SREG.I clear, or held off, waits as the CPU says, and the
/// next instruction doesn't get one until it's been taken, so each
/// instruction of a `cli` section gets its own go over the passes that
/// follow.
///
/// Then for the tick that really can land near the loop. Nothing but the tick
/// wakes the CPU, so a pass can't be caught there by waking it early. Instead
/// `Landings` notes what `hw::wait_for_tick`'s loop runs, from its `cli` to its
/// `sleep`, and on each pass after that jumps time on to just before the PIT's
/// period ends, awake, `early` instructions before the `sleep`, and `early`
/// goes round the loop. That's the same as a pass that ran long, but one that's
/// already fed the watchdog. So the real tick lands on each instruction of the
/// loop, `sei; sleep` included, and a tick slept through would cost a pass.
///
/// Time stands still while a landed handler runs, so the handlers don't eat
/// into the pass's budget.
struct Landings {
    /// How many landings each instruction's had, by word address.
    visits: Vec<u8>,
    /// The tick count when a tick that isn't one went up, until its handler
    /// has been and gone.
    probe: Option<u32>,
    /// A deadline that isn't one is up, until its handler has been and gone.
    deadline: bool,
    /// The word addresses the CPU's run since the last `cli`, in order. Not
    /// a range, since the compiler can put the loop's `sleep` before its `cli`.
    trail: Vec<u32>,
    /// `hw::wait_for_tick`'s loop as it last ran, from its `cli` to its
    /// `sleep`.
    wait_loop: Vec<u32>,
    /// The tick count at the last jump. No other landing happens until that
    /// tick, so nothing gets in the way of it.
    jumped: Option<u32>,
    early: usize,
}

/// The `cli` instruction.
const CLI_WORD: u16 = 0x94F8;

impl Landings {
    fn new() -> Self {
        Self {
            visits: vec![0; FLASH_SIZE / 2],
            probe: None,
            deadline: false,
            trail: Vec::new(),
            wait_loop: Vec::new(),
            jumped: None,
            early: 1,
        }
    }
}

impl Sim {
    /// Whether the tick that just ended was one `Landings` jumped on to, so
    /// its pass reached it before getting as far as `sleep`.
    fn tick_landed(&self) -> bool {
        let landings = self.landings.as_ref();
        landings.is_some_and(|l| l.jumped.is_some_and(|tick| tick + 1 == self.chip.ticks))
    }

    /// `before` is what the CPU was handling before the step just run.
    fn landing_stops_time(&self, before: Option<u8>) -> bool {
        self.landings.is_some() && (before.is_some() || self.cpu.handling.is_some())
    }

    /// Lands whatever's due before the next instruction. Nothing lands on
    /// top of a request that's already pending, or inside a handler.
    fn land(&mut self) -> Result<(), String> {
        let Some(landings) = &mut self.landings else {
            return Ok(());
        };
        let chip = &mut self.chip;
        let pc = self.cpu.pc;
        if self.cpu.handling.is_none() && !self.cpu.asleep {
            if word(&chip.flash, pc as usize * 2) == Some(CLI_WORD) {
                landings.trail.clear();
            }
            landings.trail.push(pc);
        }
        if self.cpu.handling.is_some()
            || landings.jumped == Some(chip.ticks)
            || chip.interrupt().is_some()
        {
            return Ok(());
        }
        let pit_on = chip.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN != 0;
        let enabled = chip.enabled();
        if !pit_on || enabled & 1 << regs::VECTOR_RTC_PIT == 0 {
            return Ok(());
        }
        if self.cpu.asleep {
            if !landings.trail.is_empty() {
                landings.wait_loop = std::mem::take(&mut landings.trail);
            }
            return Ok(());
        }
        // The loop ends in the `sleep`, so `early` 1 is the instruction before.
        // A tick already waiting to be picked up means a pass is about to run,
        // and going round the loop to find that isn't waiting for the next.
        // With the oscillator dead there's no tick to land.
        let target = landings.wait_loop.len().checked_sub(landings.early + 1);
        if let Some(at) = target
            && landings.wait_loop[at] == pc
            && chip.io[reg::GPIO_GPIOR0] == 0
            && chip.world.osc32k != 0
        {
            landings.early = if at == 0 { 1 } else { landings.early + 1 };
            landings.jumped = Some(chip.ticks);
            self.ticks_landed += 1;
            let to = (chip.next_pit - 1).max(chip.now);
            // That time would've been asleep, and TCB0's stopped in
            // power-down, as in `wait_for_pit`.
            if chip.powering_down() {
                chip.tcb.zero_at += to - chip.now;
            }
            return chip.advance_to(to);
        }
        // A real deadline's coming in this world, and one landed on top of it
        // would have its flag put back.
        let deadlines = enabled & 1 << regs::VECTOR_TCB0_INT != 0 && !chip.world.overdue();
        match landings.visits.get_mut(pc as usize) {
            Some(visits @ 0) => {
                *visits = 1;
                chip.pit_probe = true;
                landings.probe = Some(chip.ticks);
            }
            Some(visits @ 1) if deadlines => {
                *visits = 2;
                chip.tcb.probe = true;
                landings.deadline = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// A handler's finished, having found `landed`. If that was a tick or a
    /// deadline that isn't one, puts back what it told the main loop, unless
    /// a real tick came in under the same flag while it was pending.
    fn end_probe(&mut self, vector: u8, landed: &Landed) {
        let Some(landings) = &mut self.landings else {
            return;
        };
        let vector = usize::from(vector);
        if vector == regs::VECTOR_RTC_PIT && landings.probe.take() == Some(self.chip.ticks) {
            self.chip.io[reg::GPIO_GPIOR0] = landed.gpior0;
        }
        if vector == regs::VECTOR_TCB0_INT && mem::take(&mut landings.deadline) {
            self.chip.io[reg::GPIO_GPIOR1] = landed.gpior1;
            self.deadlines_landed += 1;
        }
    }
}

/// Runs docs/ui.md's examples and the random switching again with `Landings`
/// on, for what `check` already compares against the library. A handler
/// that lands on the stack's deepest point counts towards the stack, but the
/// other figures are for a run with nothing extra in it, so they stay.
fn mid_pass(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    report: &mut Report,
) -> Result<(), String> {
    let mut landed = Report::default();
    for example in spec::examples(spec::UI_MD)? {
        let samples = simulate::samples(&example.actions);
        let world = samples.first().map_or(World::NORMAL, |s| s.world);
        let (mut sim, boot) =
            power_on(&world).map_err(|e| format!("{}: startup: {e}", example.row))?;
        sim.landings = Some(Landings::new());
        ui_example(sim, boot, samples, &example, &mut landed)
            .map_err(|e| format!("{}: {e}", example.row))?;
    }
    let (mut sim, _) = power_on(&World::NORMAL)?;
    sim.landings = Some(Landings::new());
    against_the_library(sim, &mut landed).map_err(|e| format!("random switching: {e}"))?;
    if landed.mid_access == 0 {
        return Err("no handler ever landed between a TCA0 low and high byte".into());
    }
    if landed.ticks_landed == 0 {
        return Err("the real tick never landed on `hw::wait_for_tick`'s loop".into());
    }
    if landed.deadlines_landed == 0 {
        return Err("`tcb0_int` never landed mid-pass".into());
    }
    report.stack = report.stack.max(landed.stack);
    report.worst_pass = report.worst_pass.max(landed.loop_instructions);
    report.mid_access = landed.mid_access;
    Ok(())
}

impl Sim {
    /// Takes note of a handler starting or finishing. `before` is what the
    /// CPU was handling before the step just run.
    fn watch_handler(&mut self, before: Option<u8>) -> Result<(), String> {
        match (before, self.cpu.handling) {
            (None, Some(_)) => {
                self.chip.interrupted = self.chip.temp_held();
                self.mid_access += u32::from(self.chip.interrupted.is_some());
                self.landed = Some(Landed {
                    r: self.cpu.r,
                    sreg: self.cpu.sreg,
                    sp: self.cpu.sp,
                    gpior0: self.chip.io[reg::GPIO_GPIOR0],
                    gpior1: self.chip.io[reg::GPIO_GPIOR1],
                });
            }
            (Some(vector), None) => {
                self.chip.interrupted = None;
                if let Some(landed) = self.landed.take() {
                    self.gave_back(&landed)
                        .map_err(|e| format!("the handler for {}: {e}", vector_name(vector)))?;
                    self.end_probe(vector, &landed);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A handler lands between any two instructions of the main loop, so
    /// anything it changes, the compiled code around it sees change at random.
    fn gave_back(&self, landed: &Landed) -> Result<(), String> {
        let changed = (0..32).find(|&n| self.cpu.r[n] != landed.r[n]);
        if let Some(n) = changed {
            return Err(format!(
                "returns with r{n} at {:#04x}, not the {:#04x} it found",
                self.cpu.r[n], landed.r[n]
            ));
        }
        if self.cpu.sreg != landed.sreg {
            return Err(format!(
                "returns with SREG at {:#04x}, not the {:#04x} it found",
                self.cpu.sreg, landed.sreg
            ));
        }
        if self.cpu.sp != landed.sp.wrapping_add(2) {
            return Err(format!(
                "returns with SP at {:#06x}, having found it at {:#06x}",
                self.cpu.sp,
                landed.sp.wrapping_add(2)
            ));
        }
        Ok(())
    }
}

impl Chip {
    /// The low byte of the TCA0 register that's halfway through an access,
    /// with TEMP holding the other half.
    fn temp_held(&self) -> Option<usize> {
        self.temp.map(|(low, _)| low)
    }

    /// Refuses a handler's access to `a`, if it's one of TCA0's 16-bit
    /// registers and the handler caught the main loop halfway through one.
    /// TCA0 has one TEMP for all of them (§20.5.13), so the handler overwrites
    /// the main loop's half and the main loop finishes with the wrong one.
    /// §8.5.6 also allows saving and restoring TCA0.TEMP in the handler, which
    /// isn't modelled: the firmware's handlers leave TCA0 alone.
    fn temp_undisturbed(&self, a: usize) -> Result<(), String> {
        let Some(low) = self.interrupted else {
            return Ok(());
        };
        // CMP0 and CMP1 aren't in `WIDE`, since they're not plain storage. A
        // read of CNT or PER goes through TEMP as much as a write does.
        let live = [reg::TCA0_CMP0, reg::TCA0_CMP1];
        if WIDE.iter().chain(&live).any(|&w| w == a & !1) {
            return Err(format!(
                "a handler touches {} with the main loop halfway through {}, and they \
                 share TCA0's TEMP byte, so one corrupts the other (section 8.5.6)",
                name(a & !1),
                name(low)
            ));
        }
        Ok(())
    }
}

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        word(&self.flash, pc as usize * 2)
    }

    /// PORTA's pin change, the PIT and TCB0, lowest vector first, the default
    /// static priority (DS40002204A §13.3.2.4). A port's flags only go up
    /// for pins whose ISC enables them, so any flag is a request.
    fn interrupt(&self) -> Option<u8> {
        let pending = u32::from(self.porta_flags != 0) << VECTOR_PORTA_PORT
            | u32::from(self.pit_flag || self.pit_probe) << regs::VECTOR_RTC_PIT
            | u32::from(self.tcb.capt || self.tcb.probe) << regs::VECTOR_TCB0_INT;
        let requests = pending & self.enabled();
        (requests != 0).then(|| requests.trailing_zeros() as u8)
    }

    /// Interrupts are ignored for the four instructions after either key,
    /// and left pending (§8.7.1, §8.5.7.2). That the store or `sleep` which
    /// ends the protected change (§8.5.7.1) ends this too is likely, not
    /// stated. So is a load from flash ending it, which isn't modelled:
    /// nothing runs one between the key and its write. Nor is a write to SPL,
    /// SPH or SREG, which the CPU keeps from the bus.
    fn holds_interrupts(&self) -> bool {
        self.ccp_hold && self.unlocked_until.is_some_and(|until| self.now <= until)
    }

    /// Idle and power-down are modelled. Every interrupt the firmware uses
    /// wakes the chip from either.
    fn sleep(&mut self) -> Result<(), String> {
        self.ccp_hold = false;
        let ctrla = self.io[reg::SLPCTRL_CTRLA];
        if ctrla & regs::SLPCTRL_SEN == 0 {
            return Err(
                "sleeps with SLPCTRL.SEN clear, which the chip treats as a nop, \
                 so the firmware would spin instead"
                    .into(),
            );
        }
        if self.lupd {
            return Err("sleeps with TCA0.CTRLE.LUPD set, so the output can't update".into());
        }
        if ctrla == regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN {
            self.ready_for_power_down()?;
            self.power_downs += 1;
            // The ADC and its reference are off, so the next conversion
            // catches them starting, whatever it's set up for.
            self.adc.last_setup = None;
        } else if ctrla != regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN {
            return Err(format!(
                "sleeps with SLPCTRL.CTRLA {ctrla:#04x}, but the simulator only models \
                 idle and power-down"
            ));
        }
        // The PIT's interrupt can only wake the chip if the PIT's running.
        let pit_off = self.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN == 0;
        let mut wakes = match ctrla == regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN {
            true => self.enabled_in_power_down(),
            false => self.enabled(),
        };
        if pit_off {
            wakes &= !(1 << regs::VECTOR_RTC_PIT);
        }
        // TCB0 stops in power-down.
        if self.powering_down() {
            wakes &= !(1 << regs::VECTOR_TCB0_INT);
        }
        // With its oscillator dead the torch is off for good, so powering down
        // with nothing to wake it is the right thing to do.
        if wakes == 0 && !self.osc32k_dead() {
            return Err("sleeps with no interrupt enabled that could wake it".into());
        }
        Ok(())
    }

    fn idle(&mut self) -> Result<(), String> {
        let from = self.now;
        self.tca_frozen =
            self.io[reg::SLPCTRL_CTRLA] == regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN;
        let waited = self.wait_for_pit();
        if self.tca_frozen {
            self.tca_frozen = false;
            self.next_bottom += self.now - from;
        }
        waited
    }

    fn read(&mut self, addr: u16) -> Result<u8, String> {
        let a = usize::from(addr);
        self.temp_undisturbed(a)?;
        if SRAM.contains(&addr) {
            let Some(fault) = self.world.ram else {
                return Ok(self.sram[usize::from(addr - SRAM.start)]);
            };
            let cell = fault.cell(addr);
            return Ok(fault.read(cell, self.sram[usize::from(cell - SRAM.start)]));
        }
        if (MAPPED..MAPPED + FLASH_SIZE as u16).contains(&addr) {
            // Erased flash past the end of the image reads as 0xFF.
            return Ok(*self.flash.get(usize::from(addr - MAPPED)).unwrap_or(&0xFF));
        }
        match a {
            reg::VPORTA_IN => {
                self.switch_not_inverted()?;
                if self.io[reg::PORTA_PIN0CTRL + 5] & ISC == ISC_INPUT_DISABLE {
                    return Err(
                        "reads PA5 with its input buffer off, so PORTA.IN never changes".into(),
                    );
                }
                Ok(if self.pressed { !SWITCH } else { 0xFF })
            }
            reg::RTC_PITSTATUS => {
                let busy = self.pit_busy > 0;
                if !self.osc32k_dead() {
                    self.pit_busy = self.pit_busy.saturating_sub(1);
                }
                Ok(if busy { regs::RTC_CTRLBUSY } else { 0 })
            }
            reg::WDT_STATUS => {
                let busy = self.wdt.busy > 0;
                if !self.osc32k_dead() {
                    self.wdt.busy = self.wdt.busy.saturating_sub(1);
                }
                Ok(if busy { regs::WDT_SYNCBUSY } else { 0 })
            }
            reg::TCB0_INTFLAGS => Ok(if self.tcb.capt { regs::TCB_CAPT } else { 0 }),
            reg::TCA0_INTFLAGS => {
                // With the PIT's flag up this is a counted wait that's
                // finishing, not one starting.
                self.tca_polled |= !self.pit_flag;
                Ok(if self.tca_ovf { regs::TCA_OVF } else { 0 })
            }
            // Reading the flag doesn't move time on: a firmware that went
            // back to busy-waiting on it would run out of instructions.
            reg::RTC_PITINTFLAGS => Ok(if self.pit_flag { regs::RTC_PI } else { 0 }),
            PORTA_INTFLAGS => Ok(self.porta_flags),
            reg::ADC0_INTFLAGS => {
                let ready = self.adc.ready_at.is_some_and(|at| self.now >= at);
                Ok(if ready { regs::ADC_RESRDY } else { 0 })
            }
            reg::ADC0_RES => {
                // Reading the low byte latches the high byte into ADC0.TEMP
                // and clears RESRDY.
                if !self.adc.ready_at.is_some_and(|at| self.now >= at) {
                    return Err("reads ADC0.RES with no result ready".into());
                }
                self.adc.ready_at = None;
                let [lo, hi] = self.adc.result.to_le_bytes();
                self.adc.temp = Some(hi);
                Ok(lo)
            }
            _ if a == reg::ADC0_RES + 1 => self.adc.temp.take().ok_or_else(|| {
                "reads ADC0.RES's high byte without its low byte just before, \
                 so it gets whatever ADC0.TEMP held"
                    .into()
            }),
            reg::TCA0_CTRLFSET => {
                let bits = [regs::TCA_PERBV, regs::TCA_CMP0BV, regs::TCA_CMP1BV];
                let valid = iter::zip(bits, self.buffer_valid).filter(|&(_, valid)| valid);
                Ok(valid.fold(0, |all, (bit, _)| all | bit))
            }
            reg::TCA0_CMP0 | reg::TCA0_CMP1 | reg::TCA0_PER => {
                let [lo, hi] = match a {
                    reg::TCA0_PER => self.wide(reg::TCA0_PER),
                    _ => self.compare[usize::from(a == reg::TCA0_CMP1)],
                }
                .to_le_bytes();
                self.temp = Some((a, hi));
                Ok(lo)
            }
            _ if [reg::TCA0_PER, reg::TCA0_CMP0, reg::TCA0_CMP1].contains(&a.wrapping_sub(1)) => {
                match self.temp.take() {
                    Some((low, hi)) if low == a - 1 => Ok(hi),
                    _ => Err(format!(
                        "reads the high byte of {} without its low byte just before, \
                         so it gets whatever TEMP held",
                        name(a - 1)
                    )),
                }
            }
            reg::SIGROW_TEMPSENSE0 => Ok(CALIBRATION.gain),
            reg::SIGROW_TEMPSENSE1 => Ok(CALIBRATION.offset as u8),
            reg::CRCSCAN_STATUS => Ok(self.crc_status()),
            reg::RSTCTRL_RSTFR => Ok(self.rstfr),
            _ if self.world.fuse(a).is_some() => Ok(self.world.fuse(a).unwrap_or(0xFF)),
            _ if plain(a) => Ok(self.io[a]),
            _ => Err(format!(
                "reads {}, which the simulator doesn't model",
                name(a)
            )),
        }
    }

    fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        let a = usize::from(addr);
        self.temp_undisturbed(a)?;
        self.ccp_hold = false;
        if SRAM.contains(&addr) {
            let cell = self.world.ram.map_or(addr, |fault| fault.cell(addr));
            let before = self.sram[usize::from(cell - SRAM.start)];
            self.sram[usize::from(cell - SRAM.start)] = value;
            if let Some(fault) = self.world.ram
                && let Some(victim) = fault.disturbs(cell, before, value)
            {
                let byte = &mut self.sram[usize::from(victim - SRAM.start)];
                *byte = fault.disturbed(*byte);
            }
            return Ok(());
        }
        match a {
            reg::CPU_CCP => {
                if value != regs::CPU_CCP_IOREG {
                    return Err(format!(
                        "writes {value:#04x} to CPU.CCP, which isn't the IOREG key"
                    ));
                }
                self.unlocked_until = Some(self.now + 4);
                self.ccp_hold = true;
            }
            reg::CLKCTRL_MCLKCTRLB => {
                self.take_protected_write(a)?;
                if value != regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN {
                    return Err(format!(
                        "writes {value:#04x} to CLKCTRL.MCLKCTRLB: the firmware runs at \
                         5 MHz, since 10 MHz is out of spec under 2.7 V (table 36-3)"
                    ));
                }
                self.io[a] = value;
            }
            reg::TCA0_CTRLA => {
                if value != regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE {
                    return Err(format!(
                        "writes {value:#04x} to TCA0.CTRLA: the period and the duty \
                         cycles assume the timer runs undivided, and it never stops"
                    ));
                }
                self.io[a] = value;
            }
            reg::TCA0_CTRLB => {
                // 0 is its reset value, which startup puts back.
                if value != 0 && value & TCA_WGMODE != regs::TCA_WGMODE_SINGLESLOPE {
                    return Err(format!(
                        "writes {value:#04x} to TCA0.CTRLB: the period and the duty \
                         cycles assume single-slope"
                    ));
                }
                if value & regs::TCA_ALUPD != 0 {
                    return Err(format!(
                        "writes {value:#04x} to TCA0.CTRLB: ALUPD would take LUPD out of \
                         the firmware's hands (section 20.5.2)"
                    ));
                }
                self.io[a] = value;
            }
            reg::WDT_CTRLA => {
                self.take_protected_write(a)?;
                if self.wdt.busy > 0 {
                    return Err("writes WDT.CTRLA while WDT.STATUS.SYNCBUSY is set, \
                                which the datasheet doesn't allow (section 19.3.6)"
                        .into());
                }
                if self.watchdog_running() {
                    // Whether a change restarts the windows isn't in the
                    // datasheet, and the firmware sets it up once.
                    return Err(if value & WDT_PERIOD == 0 {
                        "turns the watchdog off".into()
                    } else {
                        format!(
                            "writes {value:#04x} to WDT.CTRLA with the watchdog already on, \
                             which the simulator doesn't model"
                        )
                    });
                }
                if value & WDT_PERIOD > WDT_PERIOD_MAX || (value & WDT_WINDOW) >> 4 > WDT_PERIOD_MAX
                {
                    return Err(format!(
                        "writes {value:#04x} to WDT.CTRLA: a reserved WDT period or window"
                    ));
                }
                self.io[a] = value;
                self.wdt = Wdt {
                    busy: WDT_SYNC_READS,
                    ..Wdt::default()
                };
            }
            reg::WDT_STATUS => {
                return Err(if value & WDT_LOCK != 0 {
                    "sets WDT.STATUS.LOCK, which only debug mode can clear".into()
                } else {
                    "writes WDT.STATUS, whose only writable bit is LOCK".into()
                });
            }
            reg::RTC_PITCTRLA => {
                if self.pit_busy > 0 {
                    return Err(format!(
                        "writes {} while RTC.PITSTATUS.CTRLBUSY is set, and the chip drops that write",
                        name(a)
                    ));
                }
                self.io[a] = value;
                self.pit_busy = PIT_SYNC_READS;
            }
            reg::RTC_PITINTFLAGS => {
                // Writing a 1 clears the flag.
                if value & regs::RTC_PI != 0 {
                    self.pit_flag = false;
                    self.pit_probe = false;
                }
            }
            PORTA_INTFLAGS => self.porta_flags &= !value,
            reg::TCA0_CTRLESET | reg::TCA0_CTRLECLR => self.tca_lock(a, value)?,
            reg::TCA0_CTRLFCLR => {
                let bits = [regs::TCA_PERBV, regs::TCA_CMP0BV, regs::TCA_CMP1BV];
                for (valid, bit) in iter::zip(&mut self.buffer_valid, bits) {
                    *valid &= value & bit == 0;
                }
            }
            reg::RSTCTRL_RSTFR => self.rstfr &= !value,
            reg::RSTCTRL_SWRR => {
                self.take_protected_write(a)?;
                if value != regs::RSTCTRL_SWRE {
                    return Err(format!("writes {value:#04x} to RSTCTRL.SWRR"));
                }
                self.pending_reset = Some(regs::RSTCTRL_SWRF);
            }
            reg::TCA0_INTFLAGS => {
                if value & regs::TCA_OVF != 0 {
                    self.tca_ovf = false;
                }
            }
            _ if tcb0(a) => self.write_tcb(a, value)?,
            reg::TCA0_CMP0 | reg::TCA0_CMP1 => self.temp = Some((a, value)),
            _ if [reg::TCA0_CMP0, reg::TCA0_CMP1].contains(&a.wrapping_sub(1)) => {
                self.write_live(a, value)?;
            }
            _ if WIDE.contains(&a) => {
                if let Some((pending, _)) = self.temp {
                    return Err(format!(
                        "writes the low byte of {} while {}'s is still waiting for its high byte",
                        name(a),
                        name(pending)
                    ));
                }
                self.temp = Some((a, value));
            }
            _ if WIDE.contains(&a.wrapping_sub(1)) => match self.temp.take() {
                Some((low, lo)) if low == a - 1 => {
                    let i = WIDE.iter().position(|&w| w == low).unwrap_or_default();
                    self.wide[i] = u16::from_le_bytes([lo, value]);
                    self.tca_written(low)?;
                }
                _ => {
                    return Err(format!(
                        "writes the high byte of {} without its low byte just before, \
                         so the chip pairs it with whatever TEMP held",
                        name(a - 1)
                    ));
                }
            },
            reg::ADC0_COMMAND => {
                if value & regs::ADC_STCONV != 0 {
                    self.start_conversion()?;
                }
            }
            reg::ADC0_INTFLAGS => {
                // Writing a 1 clears RESRDY. It only clears the flag: a
                // conversion that's still running finishes and sets it again.
                if value & regs::ADC_RESRDY != 0 && !self.converting() {
                    self.adc.ready_at = None;
                }
            }
            _ if ADC_SETUP.contains(&a) && self.converting() => {
                return Err(format!("writes {} mid-conversion", name(a)));
            }
            reg::CRCSCAN_CTRLA => {
                if value != regs::CRCSCAN_ENABLE {
                    return Err(format!(
                        "writes {value:#04x} to CRCSCAN.CTRLA; the simulator only models \
                         ENABLE, with STATUS polled (NMIEN can't be undone without a reset)"
                    ));
                }
                self.start_crc()?;
                self.io[a] = value;
            }
            reg::CRCSCAN_CTRLB => {
                if self.crc_busy() {
                    return Err(
                        "writes CRCSCAN.CTRLB mid-scan, and the chip ignores that write".into(),
                    );
                }
                self.io[a] = value;
            }
            _ if fuse_or_lock(a) => {
                return Err(format!(
                    "writes {value:#04x} to {}: the firmware never writes the fuses",
                    name(a)
                ));
            }
            _ if plain(a) => self.io[a] = value,
            _ => {
                return Err(format!(
                    "writes {value:#04x} to {}, which the simulator doesn't model",
                    name(a)
                ));
            }
        }
        Ok(())
    }

    fn wdr(&mut self) -> Result<(), String> {
        // A dead oscillator takes the WDT's clock with it, so a `wdr` can't
        // sync, and nothing's counting anyway.
        if !self.watchdog_running() || self.osc32k_dead() {
            return Ok(());
        }
        if self.wdt.busy > 0 {
            return Err("wdr before the WDT.CTRLA write has synced".into());
        }
        if self.world.wdt == Some(WdtFault::Deaf) {
            return Ok(());
        }
        let ticks = self.wdt.ticks;
        let clocks = ticks * WDT_CLOCKS_PER_TICK;
        let closed = self.wdt_closed();
        if self.wdr_in_closed_window(clocks, closed) {
            return self.bite(format!(
                "wdr {ticks} ticks ({clocks} WDT clocks) after the last, in or on the edge \
                 of the {closed}-clock closed window, so the chip would have reset (or, \
                 within 2 to 3 clocks of the last, ignored it, section 19.3.6)"
            ));
        }
        self.wdt.ticks = 0;
        self.wdt.windowed = self.watchdog_windowed();
        self.wdt.fed = true;
        Ok(())
    }
}

// The reset controller, and the watchdog actually resetting the chip.

impl Chip {
    /// Whether `clocks` since the last `wdr` run out a `timeout`, allowing
    /// for a broken watchdog.
    fn wdt_times_out(&self, clocks: u32, timeout: u32) -> bool {
        match self.world.wdt {
            Some(WdtFault::Never) => false,
            Some(WdtFault::Stops) if self.wdt.fed => false,
            Some(WdtFault::Early) => clocks >= timeout / 2,
            _ => clocks >= timeout,
        }
    }

    /// Whether a `wdr` `clocks` after the last lands in a `closed` window,
    /// allowing for a broken watchdog.
    fn wdr_in_closed_window(&self, clocks: u32, closed: u32) -> bool {
        match self.world.wdt {
            Some(WdtFault::Open) => false,
            Some(WdtFault::Shut) => self.watchdog_windowed(),
            _ => closed > 0 && clocks <= closed,
        }
    }

    /// The watchdog resets the chip. In normal use nothing's expecting that,
    /// so it's the firmware failing, for the reason in `why`.
    fn bite(&mut self, why: String) -> Result<(), String> {
        if !self.bites {
            return Err(why);
        }
        self.pending_reset = Some(regs::RSTCTRL_WDRF);
        Ok(())
    }

    /// Everything back to its reset value but the things a reset doesn't
    /// reach: flash, the flags in RSTFR, the world outside and, most likely,
    /// SRAM. AN2632 §6 counts on that, the datasheet doesn't promise it, and
    /// `WdtFault::Wipes` is the chip where it's wrong. The PIT starts from
    /// nothing, and comes up to a period later, which is
    /// the slow end of "anytime between enabling and a full period"
    /// (DS40002204A §23.5.2.2).
    fn reset(&mut self, flag: u8) {
        let mut chip = Self::new(mem::take(&mut self.flash));
        if self.world.wdt != Some(WdtFault::Wipes) {
            chip.sram = mem::take(&mut self.sram);
        }
        chip.now = self.now;
        chip.next_pit = self.now + PIT_PERIOD;
        chip.pressed = self.pressed;
        chip.ticks = self.ticks;
        chip.power_downs = self.power_downs;
        chip.world = self.world;
        chip.rstfr = self.rstfr | flag;
        chip.resets = self.resets + 1;
        chip.bites = self.bites;
        chip.lupd_broken = self.lupd_broken;
        chip.bv_adverse = self.bv_adverse;
        chip.bv_cleared = self.bv_cleared;
        // It goes with `world.tca_stopped`, which survives too.
        chip.tca_deaf = self.tca_deaf;
        *self = chip;
    }
}

impl Sim {
    /// A reset that sets `flag` in RSTFR, which starts the CPU again from
    /// the reset vector. Startup's self-tests come round again, so the stack
    /// checks wait for them as they did at power-on.
    fn reset(&mut self, flag: u8) {
        self.cpu = Cpu::new(&SRAM);
        self.chip.reset(flag);
        self.testing = self.ram_tested;
        self.booted = self.executed;
        self.landed = None;
        // A reset takes any probe with it, and a stale one would swallow the
        // next real handler's flags.
        if let Some(landings) = &mut self.landings {
            landings.probe = None;
            landings.deadline = false;
        }
    }

    /// What's in the marker right now, if it's a marker at all.
    fn marker(&self) -> Option<Step> {
        let bytes = self.chip.sram(&(self.marker..self.marker + 2));
        watchdog::decode([bytes[0], bytes[1]]).map(|(step, _)| step)
    }

    /// Whether the firmware's got as far as the setup it means to run on:
    /// the marker has a verdict and `hw::init` has acted on it. The marker
    /// alone isn't enough, since a proof that gives up stores its verdict
    /// and then resets from the WDT setting it had for that step.
    fn proved(&self) -> Option<Step> {
        let step = self.marker()?;
        let want = match step {
            Step::Failed => regs::WDT_PERIOD_8KCLK,
            Step::Passed | Step::Hung => regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK,
            _ => return None,
        };
        (self.chip.io[reg::WDT_CTRLA] == want).then_some(step)
    }

    /// Runs the watchdog's proof, from `main` to the real setup, with bites
    /// resetting the chip. Checks the FET and linear pins on every
    /// instruction: both held low and both compares at 0, but for the
    /// instructions between a reset and src/rt.rs's `sbi`s, which is as
    /// soon as the CPU can do anything about it.
    fn prove(&mut self) -> Result<Proved, String> {
        let (ticks, resets) = (self.chip.ticks, self.chip.resets);
        let mut last = None;
        self.chip.bites = true;
        for _ in 0..BOOT_BUDGET {
            if let Some(step) = self.proved() {
                self.chip.bites = false;
                return Ok(Proved {
                    step,
                    last,
                    resets: self.chip.resets - resets,
                    ticks: self.chip.ticks - ticks,
                });
            }
            let press = self.chip.world.proof_press;
            let boot = self.chip.resets - resets;
            self.chip.set_switch(press == Some(boot))?;
            self.step()?;
            if let Some(step) = self.marker().filter(|step| step.proving()) {
                last = Some(step);
            }
            self.outputs_held_low()?;
        }
        Err(format!(
            "didn't reach the real setup within {BOOT_BUDGET} instructions"
        ))
    }

    fn outputs_held_low(&self) -> Result<(), String> {
        let chip = &self.chip;
        let buf = [chip.wide(reg::TCA0_CMP0BUF), chip.wide(reg::TCA0_CMP1BUF)];
        if chip.compare != [0; 2] || buf != [0; 2] {
            return Err(format!(
                "TCA0's compares are {:?} and their BUFs {buf:?} during the proof",
                chip.compare
            ));
        }
        if self.executed - self.booted > SBI_INSTRUCTIONS
            && chip.io[reg::VPORTB_DIR] & (FET | LINEAR) != FET | LINEAR
        {
            return Err("the FET and linear pins are floating during the proof".into());
        }
        Ok(())
    }

    /// Sticks the CPU where it is and lets the watchdog deal with it, then
    /// runs the boot that follows up to its first tick, as `in_step` does at
    /// power-on. `verdict` is how the proof came out.
    fn hang(&mut self, verdict: Step) -> Result<(), String> {
        let resets = self.chip.resets;
        self.chip.bites = true;
        self.chip.stuck = true;
        for _ in 0..HANG_TICKS {
            self.tick(false)?;
            if self.chip.resets > resets {
                break;
            }
        }
        if self.chip.resets == resets {
            return Err(format!(
                "the watchdog hadn't reset a hung chip after {HANG_TICKS} ticks"
            ));
        }
        self.chip.bites = false;
        // The RAM test that a hang gets is far longer than any pass, so
        // startup runs outside the pass's budget.
        for _ in 0..BOOT_BUDGET {
            if self.proved().is_some() {
                break;
            }
            self.step().map_err(|e| format!("after a hang: {e}"))?;
        }
        self.tick(false).map_err(|e| format!("after a hang: {e}"))?;
        let want = if verdict == Step::Passed {
            Step::Hung
        } else {
            verdict
        };
        if self.proved() != Some(want) {
            return Err(format!(
                "after a hang the marker says {:?} and WDT.CTRLA {:#04x}, not {want:?} \
                 and the watchdog back on",
                self.marker(),
                self.chip.io[reg::WDT_CTRLA]
            ));
        }
        if self.chip.leds() != (0, 0) {
            return Err(format!(
                "a hang's reset came back up at {:?}, not off",
                self.chip.leds()
            ));
        }
        Ok(())
    }
}

/// How `Sim::prove` went.
struct Proved {
    /// The verdict: `Step::Passed` or `Step::Failed`.
    step: Step,
    /// The last step it was on before that, which is what found a fault.
    last: Option<Step>,
    /// Resets it took, the software one after a give-up included.
    resets: u32,
    /// Ticks it took.
    ticks: u32,
}

/// `jmp __reset` and src/rt.rs's two `sbi`s: the instructions after a reset
/// that the pins can still be floating for.
const SBI_INSTRUCTIONS: u64 = 3;
/// Deliberate resets on a good chip: steps 1, 2 and 3 each end in one.
const PROOF_RESETS: u32 = 3;
/// Ticks the proof takes on a good chip, in simulated whole ticks: step 1
/// bites on its first, step 2 on its fourth and step 3 on its second.
const PROOF_TICKS: u32 = 7;
/// Ticks a hung chip gets. The 64-clock window is four, and a failed proof's
/// 8 s timeout is 512.
const HANG_TICKS: u32 = 600;

/// Every way the watchdog can be broken, and the step that has to find it.
/// A fault found anywhere else means the step meant to find it isn't doing
/// its job, even if the fault was caught.
const WDT_FAULTS: [(WdtFault, Step); WdtFault::ALL.len()] = [
    (WdtFault::Never, Step::Timeout),
    (WdtFault::Early, Step::Feed),
    (WdtFault::Deaf, Step::Feed),
    (WdtFault::Stops, Step::Fed),
    (WdtFault::Open, Step::Early),
    (WdtFault::Shut, Step::Window),
    (WdtFault::Wipes, Step::Timeout),
];

/// The proof on a chip that's fine, one that's just been flashed, one with
/// each fault in `WDT_FAULTS`, and with a press during each of its steps.
fn watchdog_proof(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
    main: u32,
    report: &mut Report,
) -> Result<(), String> {
    let (mut sim, _) = power_on(&World::NORMAL)?;
    let proved = sim.prove()?;
    if proved.step != Step::Passed {
        return Err(format!(
            "failed a good chip, at {:?}, after {} resets",
            proved.last, proved.resets
        ));
    }
    if (proved.resets, proved.ticks) != (PROOF_RESETS, PROOF_TICKS) {
        return Err(format!(
            "took {} resets and {} ticks, not {PROOF_RESETS} and {PROOF_TICKS}",
            proved.resets, proved.ticks
        ));
    }

    // A flash: the marker says the watchdog's fine, and the proof runs anyway.
    sim.tick(false)?;
    sim.reset(regs::RSTCTRL_UPDIRF);
    sim.run_to(main, BOOT_BUDGET)
        .map_err(|e| format!("a UPDI reset: {e}"))?;
    if sim.marker().is_some() {
        return Err("a UPDI reset left the marker, so it skipped the RAM test".into());
    }
    let proved = sim.prove().map_err(|e| format!("a UPDI reset: {e}"))?;
    if (proved.step, proved.resets) != (Step::Passed, PROOF_RESETS) {
        return Err(format!(
            "a UPDI reset ended {:?} after {} resets, so it didn't run the proof again",
            proved.step, proved.resets
        ));
    }

    dead_oscillator(power_on).map_err(|e| format!("a dead 32 kHz oscillator: {e}"))?;

    for (fault, found_by) in WDT_FAULTS {
        let world = World {
            wdt: Some(fault),
            ..World::NORMAL
        };
        let fault = |e: String| format!("{fault}: {e}");
        let (mut sim, _) = power_on(&world).map_err(fault)?;
        let proved = sim.prove().map_err(fault)?;
        if proved.step != Step::Failed {
            return Err(fault(format!("not found, it ended {:?}", proved.step)));
        }
        if proved.last != Some(found_by) {
            return Err(fault(format!(
                "found at {:?}, not {found_by:?}",
                proved.last
            )));
        }
        // And the light runs, as the rows in docs/ui.md say it does.
        let (sim, _) = power_on(&world).map_err(fault)?;
        let samples = iter::repeat_n(
            Sample {
                pressed: false,
                world,
            },
            64,
        );
        in_step(sim, samples, report).map_err(fault)?;
    }

    for boot in 0..PROOF_RESETS {
        let world = World {
            proof_press: Some(boot),
            ..World::NORMAL
        };
        let press = |e: String| format!("a press on boot {boot}: {e}");
        let (sim, _) = power_on(&world).map_err(press)?;
        let samples = iter::repeat_n(
            Sample {
                pressed: false,
                world,
            },
            usize::from(RELEASE_TICKS) + 2,
        );
        if in_step(sim, samples, report).map_err(press)? == 0 {
            return Err(press("the click never counted".into()));
        }
    }
    Ok(())
}

/// The 32 kHz oscillator dying at the start of each boot of the proof. No tick
/// and no bite ever comes, so only the proof's own bounded waits can end it:
/// it has to fail inside `Sim::prove`'s budget, and the boot after that has to
/// fail the clock check, powered down with the light off, not hang.
fn dead_oscillator(
    power_on: &impl Fn(&World) -> Result<(Sim, Boot), String>,
) -> Result<(), String> {
    let dead = World {
        osc32k: 0,
        ..World::NORMAL
    };
    for boot in 0..PROOF_RESETS {
        let at = |e: String| format!("from boot {boot} of the proof: {e}");
        let (mut sim, _) = power_on(&World::NORMAL).map_err(at)?;
        sim.chip.bites = true;
        for _ in 0..BOOT_BUDGET {
            if sim.chip.resets == boot {
                break;
            }
            sim.step().map_err(at)?;
        }
        sim.chip.set_world(dead);
        let proved = sim.prove().map_err(at)?;
        if proved.step != Step::Failed {
            return Err(at(format!("the proof ended {:?}", proved.step)));
        }
        for _ in 0..DEAD_INIT_TICKS {
            sim.tick(false).map_err(at)?;
        }
        if !sim.chip_powered_down() {
            return Err(at("it isn't powered down, so it hung".into()));
        }
        if sim.chip.leds() != (0, 0) {
            return Err(at(format!("the LEDs are at {:?}", sim.chip.leds())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip() -> Chip {
        Chip::new(vec![0; 64])
    }

    #[test]
    fn takes_a_protected_write_only_just_after_the_key() {
        let mut c = chip();
        let clk = reg::CLKCTRL_MCLKCTRLB as u16;
        let five_mhz = regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN;
        assert!(c.write(clk, five_mhz).is_err(), "no key");

        c.now = 10;
        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 14;
        assert_eq!(c.write(clk, five_mhz), Ok(()), "fourth instruction after");
        assert_eq!(c.io[reg::CLKCTRL_MCLKCTRLB], five_mhz);
        assert!(
            c.write(clk, five_mhz).is_err(),
            "the key only covers one write"
        );

        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 19;
        assert!(
            c.write(clk, five_mhz).unwrap_err().contains("ignores"),
            "too late"
        );
        assert!(c.write(reg::CPU_CCP as u16, 0x9D).is_err(), "SPM key");
    }

    #[test]
    fn refuses_any_clock_but_5_mhz() {
        let mut c = chip();
        // Undivided, 10 MHz, and the reset value's 3.3 MHz.
        for value in [0x00, regs::CLKCTRL_PEN, 0x11] {
            c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
            let err = c.write(reg::CLKCTRL_MCLKCTRLB as u16, value).unwrap_err();
            assert!(err.contains("5 MHz"), "{err}");
        }
    }

    #[test]
    fn the_clock_and_pwm_have_to_have_been_set_up() {
        let set_up = |c: &mut Chip| {
            c.io[reg::CLKCTRL_MCLKCTRLB] = regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN;
            c.io[reg::TCA0_CTRLA] = regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE;
            c.io[reg::TCA0_CTRLB] = regs::TCA_WGMODE_SINGLESLOPE;
            c.wide[1] = ramp::PWM_TOP - 1;
        };
        let mut c = chip();
        assert!(!c.clocked_as_modelled(), "out of reset");
        set_up(&mut c);
        assert!(c.clocked_as_modelled());

        for missed in [reg::CLKCTRL_MCLKCTRLB, reg::TCA0_CTRLA, reg::TCA0_CTRLB] {
            let mut c = chip();
            set_up(&mut c);
            c.io[missed] = 0;
            assert!(!c.clocked_as_modelled(), "{}", name(missed));
        }
        let mut c = chip();
        set_up(&mut c);
        c.wide[WIDE.iter().position(|&a| a == reg::TCA0_PER).unwrap()] = 0xFFFF;
        assert!(!c.clocked_as_modelled(), "TCA0.PER at its reset value");
    }

    #[test]
    fn refuses_pwm_that_isnt_single_slope() {
        let mut c = chip();
        let ctrlb = reg::TCA0_CTRLB as u16;
        let err = c.write(ctrlb, 0x07).unwrap_err();
        assert!(err.contains("single-slope"), "DSBOTTOM: {err}");
        assert_eq!(c.write(ctrlb, regs::TCA_WGMODE_SINGLESLOPE), Ok(()));
    }

    /// A chip with TCA0 running level `level`'s output from BOTTOM.
    fn pwm_at(level: u8) -> Chip {
        let mut c = chip();
        let out = ramp::output(level);
        c.wide[1] = out.top - 1;
        c.compare = [out.fet, out.linear];
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        c.next_bottom = c.now + u64::from(out.top);
        c
    }

    /// `hw::set_output`'s writes, up to clearing the lock.
    fn set_output(c: &mut Chip, out: ramp::Output) -> Result<(), String> {
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD)?;
        write_wide(c, reg::TCA0_PERBUF, out.top - 1);
        write_wide(c, reg::TCA0_CMP1BUF, out.linear);
        write_wide(c, reg::TCA0_CMP0BUF, out.fet);
        c.write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD)
    }

    /// Writes a 16-bit register as `write_wide` does, but says how it went.
    fn try_wide(c: &mut Chip, addr: usize, value: u16) -> Result<(), String> {
        let [lo, hi] = value.to_le_bytes();
        c.write(addr as u16, lo)?;
        c.write(addr as u16 + 1, hi)
    }

    /// (top, FET, linear) as they're running now.
    fn live(c: &Chip) -> (u16, u16, u16) {
        (c.wide(reg::TCA0_PER) + 1, c.compare[0], c.compare[1])
    }

    #[test]
    fn a_level_lands_whole_at_the_next_bottom() {
        let (from, to) = (ramp::output(1), ramp::output(ramp::LINEAR_MAX));
        let mut c = pwm_at(1);
        set_output(&mut c, to).unwrap();
        assert_eq!(
            live(&c),
            (from.top, from.fet, from.linear),
            "not until BOTTOM"
        );
        c.advance_to(c.next_bottom - 1).unwrap();
        assert_eq!(live(&c), (from.top, from.fet, from.linear));
        c.elapse().unwrap();
        assert_eq!(live(&c), (to.top, to.fet, to.linear));
    }

    #[test]
    fn the_cycle_is_the_live_period_not_the_buffered_one() {
        let to = ramp::output(1);
        let mut c = pwm_at(ramp::LINEAR_MAX);
        set_output(&mut c, to).unwrap();
        let bottom = c.next_bottom;
        c.advance_to(bottom).unwrap();
        assert_eq!(c.next_bottom, bottom + u64::from(to.top));
    }

    #[test]
    fn a_stopped_timer_counts_nothing_and_a_started_one_starts_a_cycle() {
        let mut c = chip();
        write_wide(&mut c, reg::TCA0_PER, ramp::PWM_TOP - 1);
        c.advance_to(c.now + 10_000).unwrap();
        assert_eq!(c.next_bottom, c.now + u64::from(ramp::PWM_TOP));
    }

    #[test]
    fn the_lock_holds_an_update_over_a_bottom_until_the_next_one() {
        let (from, to) = (ramp::output(1), ramp::output(ramp::LEVELS));
        let mut c = pwm_at(1);
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, to.top - 1);
        c.advance_to(c.next_bottom).unwrap();
        assert_eq!(live(&c), (from.top, from.fet, from.linear), "locked");
        write_wide(&mut c, reg::TCA0_CMP1BUF, to.linear);
        write_wide(&mut c, reg::TCA0_CMP0BUF, to.fet);
        c.write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD).unwrap();
        c.advance_to(c.next_bottom).unwrap();
        assert_eq!(live(&c), (to.top, to.fet, to.linear));
    }

    #[test]
    fn an_unlocked_write_is_caught_at_once() {
        // Any order, any level: the first write that tears the output fails.
        let to = ramp::output(ramp::LINEAR_MAX);
        let mut c = pwm_at(1);
        let err = try_wide(&mut c, reg::TCA0_PERBUF, to.top - 1).unwrap_err();
        assert!(err.contains("LUPD"), "{err}");

        let mut c = pwm_at(1);
        let err = try_wide(&mut c, reg::TCA0_CMP1BUF, to.linear).unwrap_err();
        assert!(err.contains("LUPD"), "{err}");
    }

    #[test]
    fn a_compare_left_over_a_shorter_period_holds_the_fet_on_and_fails() {
        // Level 149's FET compare against a period under it.
        let mut c = pwm_at(ramp::LEVELS - 1);
        let err = try_wide(&mut c, reg::TCA0_PERBUF, 100).unwrap_err();
        assert!(err.contains("holds the FET fully on"), "{err}");

        // Set up under the lock, the same goes for the moment it's cleared.
        let mut c = pwm_at(ramp::LEVELS - 1);
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, 100);
        let err = c
            .write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD)
            .unwrap_err();
        assert!(err.contains("holds the FET fully on"), "{err}");
    }

    #[test]
    fn a_compare_left_over_a_shorter_period_holds_the_linear_channel_on_and_fails() {
        let mut c = pwm_at(ramp::LINEAR_MAX - 1);
        let err = try_wide(&mut c, reg::TCA0_PERBUF, 100).unwrap_err();
        assert!(err.contains("holds the linear channel fully on"), "{err}");
    }

    #[test]
    fn a_period_that_keeps_full_duty_from_holding_the_pin_high_is_no_level() {
        let mut c = pwm_at(ramp::LINEAR_MAX);
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, ramp::PWM_TOP);
        let err = c
            .write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD)
            .unwrap_err();
        assert!(err.contains("no level's output"), "{err}");
    }

    #[test]
    fn a_torn_update_that_reaches_bottom_fails_too() {
        // With the lock broken and the write-time check out of the way,
        // BOTTOM itself still refuses it.
        let mut c = pwm_at(1);
        c.wide[2] = ramp::PWM_TOP - 1;
        c.buffer_valid[0] = true;
        let err = c.advance_to(c.next_bottom).unwrap_err();
        assert!(err.contains("no level's output"), "{err}");
    }

    #[test]
    fn a_broken_lock_lets_writes_through_as_if_it_was_never_set() {
        let mut c = pwm_at(1);
        c.lupd_broken = true;
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        let top = ramp::output(ramp::LINEAR_MAX).top;
        assert!(try_wide(&mut c, reg::TCA0_PERBUF, top - 1).is_err());
    }

    #[test]
    fn every_change_of_level_goes_through_under_the_lock() {
        for from in 0..=ramp::LEVELS {
            for to in [0, 1, ramp::LINEAR_MAX, ramp::LEVELS] {
                let mut c = pwm_at(from);
                let out = ramp::output(to);
                set_output(&mut c, out).unwrap();
                c.advance_to(c.next_bottom).unwrap();
                assert_eq!(live(&c), (out.top, out.fet, out.linear));
            }
        }
    }

    #[test]
    fn per_and_perbuf_reset_to_all_ones() {
        let c = chip();
        assert_eq!(c.wide(reg::TCA0_PER), 0xFFFF);
        assert_eq!(c.wide(reg::TCA0_PERBUF), 0xFFFF);
    }

    #[test]
    fn clearing_the_lock_on_a_stopped_timer_checks_nothing() {
        let mut c = chip();
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, 100);
        assert_eq!(c.write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD), Ok(()));
    }

    #[test]
    fn refuses_a_non_zero_cnt_with_the_timer_running() {
        let mut c = pwm_at(0);
        let err = try_wide(&mut c, reg::TCA0_CNT, 300).unwrap_err();
        assert!(err.contains("TCA0.CNT"), "{err}");
        assert_eq!(try_wide(&mut c, reg::TCA0_CNT, 0), Ok(()));
    }

    #[test]
    fn refuses_a_divided_or_stopped_timer() {
        let ctrla = reg::TCA0_CTRLA as u16;
        let mut c = chip();
        // CLKSEL = DIV2.
        let err = c.write(ctrla, 0x02 | regs::TCA_ENABLE).unwrap_err();
        assert!(err.contains("undivided"), "{err}");
        assert!(c.write(ctrla, 0).is_err());
        assert_eq!(
            c.write(ctrla, regs::TCA_CLKSEL_DIV1 | regs::TCA_ENABLE),
            Ok(())
        );
    }

    #[test]
    fn refuses_alupd() {
        let mut c = chip();
        let value = regs::TCA_ALUPD | regs::TCA_WGMODE_SINGLESLOPE;
        let err = c.write(reg::TCA0_CTRLB as u16, value).unwrap_err();
        assert!(err.contains("ALUPD"), "{err}");
    }

    #[test]
    fn ctrlf_reads_the_buffer_valid_flags() {
        let mut c = chip();
        assert_eq!(c.read(reg::TCA0_CTRLFSET as u16), Ok(0));
        write_wide(&mut c, reg::TCA0_PERBUF, 254);
        write_wide(&mut c, reg::TCA0_CMP1BUF, 0);
        assert_eq!(
            c.read(reg::TCA0_CTRLFSET as u16),
            Ok(regs::TCA_PERBV | regs::TCA_CMP1BV)
        );
    }

    #[test]
    fn wont_power_down_with_a_buffer_waiting() {
        let mut c = ready_to_power_down();
        c.buffer_valid[0] = true;
        assert!(c.sleep().unwrap_err().contains("UPDATE"));
    }

    #[test]
    fn wont_power_down_on_a_long_period() {
        let mut c = ready_to_power_down();
        c.wide[1] = ramp::MAX_TOP - 1;
        assert!(c.sleep().unwrap_err().contains("TCA0.PER"));
    }

    #[test]
    fn power_down_stops_the_counter_where_it_is() {
        let mut c = pwm_at(0);
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_PDOWN;
        let out = ramp::output(ramp::LEVELS);
        set_output(&mut c, out).unwrap();
        let left = c.next_bottom - c.now;
        c.idle().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0, "frozen");
        assert_eq!(c.next_bottom - c.now, left, "the same way from BOTTOM");

        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_IDLE;
        c.idle().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), out.fet, "idle runs it");
    }

    #[test]
    fn only_lupd_and_update_are_modelled_in_ctrle() {
        let mut c = chip();
        for addr in [reg::TCA0_CTRLESET, reg::TCA0_CTRLECLR] {
            let err = c.write(addr as u16, 0x08).unwrap_err();
            assert!(err.contains("only models LUPD"), "{err}");
        }
        let err = c
            .write(reg::TCA0_CTRLECLR as u16, regs::TCA_CMD_UPDATE)
            .unwrap_err();
        assert!(err.contains("only models LUPD"), "{err}");
    }

    #[test]
    fn the_update_command_lands_a_locked_row_now_and_clears_bv() {
        let mut c = pwm_at(1);
        let off = ramp::Output::OFF;
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, off.top - 1);
        write_wide(&mut c, reg::TCA0_CMP1BUF, off.linear);
        write_wide(&mut c, reg::TCA0_CMP0BUF, off.fet);
        assert_eq!(c.wide(reg::TCA0_PER), ramp::output(1).top - 1, "locked");
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_CMD_UPDATE)
            .unwrap();
        assert_eq!(c.wide(reg::TCA0_PER), off.top - 1);
        assert_eq!(c.compare, [0, 0]);
        assert_eq!(c.buffer_valid, [false; 3]);
        assert!(c.lupd, "the command leaves the lock as it was");
    }

    #[test]
    fn the_update_command_still_refuses_a_torn_row() {
        let mut c = pwm_at(1);
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_CMP0BUF, 1000);
        let err = c
            .write(reg::TCA0_CTRLESET as u16, regs::TCA_CMD_UPDATE)
            .unwrap_err();
        assert!(err.contains("UPDATE"), "{err}");
    }

    #[test]
    fn a_locked_bottom_keeps_or_clears_bv_depending_on_the_reading() {
        for (adverse, want) in [(false, [true, false, false]), (true, [false; 3])] {
            let mut c = pwm_at(1);
            c.bv_adverse = adverse;
            c.adverse_at = 3; // out of the way of the first write
            c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
            write_wide(&mut c, reg::TCA0_PERBUF, 254);
            let bottom = c.next_bottom;
            c.advance_to(bottom).unwrap();
            assert_eq!(c.buffer_valid, want, "adverse {adverse}");
            assert_eq!(c.wide(reg::TCA0_PER), ramp::output(1).top - 1, "not copied");
        }
    }

    #[test]
    fn a_row_that_loses_bv_twice_lands_nothing_at_the_end_of_the_lock() {
        let mut c = pwm_at(1);
        c.bv_adverse = true;
        c.adverse_at = 4;
        c.wide[1] = 257;
        c.compare = [0, 0];
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        for _ in 0..2 {
            write_wide(&mut c, reg::TCA0_PERBUF, 254);
            let bottom = c.next_bottom;
            c.advance_to(bottom).unwrap();
            assert_eq!(c.buffer_valid, [false; 3]);
        }
        // (257, 0, 0) is no level's output, but nothing's about to land.
        c.write(reg::TCA0_CTRLECLR as u16, regs::TCA_LUPD).unwrap();
        assert_eq!((c.wide[1], c.compare), (257, [0, 0]));
    }

    #[test]
    fn a_bottom_just_before_the_update_command_loses_the_row() {
        let mut c = pwm_at(1);
        c.bv_adverse = true;
        c.adverse_at = 5;
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        write_wide(&mut c, reg::TCA0_PERBUF, 254);
        let per = c.wide(reg::TCA0_PER);
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_CMD_UPDATE)
            .unwrap();
        assert_eq!(c.buffer_valid, [false; 3]);
        assert_eq!(c.wide(reg::TCA0_PER), per, "nothing landed");
    }

    #[test]
    fn the_buffer_valid_flags_clear_by_writing_1() {
        let mut c = chip();
        c.buffer_valid = [true; 3];
        let clear = regs::TCA_PERBV | regs::TCA_CMP1BV;
        c.write(reg::TCA0_CTRLFCLR as u16, clear).unwrap();
        assert_eq!(c.buffer_valid, [false, true, false]);
    }

    #[test]
    fn tca0s_16_bit_registers_share_one_temp() {
        let mut c = pwm_at(1);
        let [perbuf, cmp0] = [reg::TCA0_PERBUF as u16, reg::TCA0_CMP0 as u16];
        // A read's low byte overwrites a write's, and the other way round.
        c.write(perbuf, 0x12).unwrap();
        c.read(cmp0).unwrap();
        let err = c.write(perbuf + 1, 0x34).unwrap_err();
        assert!(err.contains("whatever TEMP held"), "{err}");

        c.read(cmp0).unwrap();
        c.temp = None;
        c.write(perbuf, 0x12).unwrap();
        let err = c.read(cmp0 + 1).unwrap_err();
        assert!(err.contains("whatever TEMP held"), "{err}");

        // One register's read doesn't finish another's.
        let per = reg::TCA0_PER as u16;
        c.temp = None;
        c.read(per).unwrap();
        assert!(c.read(cmp0 + 1).is_err());
    }

    #[test]
    fn a_direct_compare_write_with_the_timer_running_has_to_be_0() {
        let mut c = pwm_at(1);
        assert_eq!(try_wide(&mut c, reg::TCA0_CMP0, 0), Ok(()));
        assert_eq!(c.compare[0], 0);
        let err = try_wide(&mut c, reg::TCA0_CMP1, 4).unwrap_err();
        assert!(err.contains("use its BUF"), "{err}");
    }

    #[test]
    fn writes_per_only_with_the_timer_stopped() {
        let mut c = pwm_at(0);
        let err = try_wide(&mut c, reg::TCA0_PER, 254).unwrap_err();
        assert!(err.contains("PERBUF"), "{err}");
    }

    #[test]
    fn wont_sleep_with_the_lock_set() {
        let mut c = ready_to_power_down();
        c.write(reg::TCA0_CTRLESET as u16, regs::TCA_LUPD).unwrap();
        assert!(c.sleep().unwrap_err().contains("LUPD"));
    }

    #[test]
    fn writes_16_bit_registers_through_temp() {
        let mut c = chip();
        let cmp = reg::TCA0_CMP1BUF as u16;
        c.write(cmp, 0x34).unwrap();
        c.write(cmp + 1, 0x12).unwrap();
        assert_eq!(c.wide(reg::TCA0_CMP1BUF), 0x1234);

        assert!(c.write(cmp + 1, 0).unwrap_err().contains("high byte"));
        c.write(cmp, 1).unwrap();
        let err = c.write(reg::TCA0_CMP0BUF as u16, 1).unwrap_err();
        assert!(err.contains("still waiting"), "{err}");
        let err = c.write(reg::TCA0_CMP0BUF as u16 + 1, 1).unwrap_err();
        assert!(
            err.contains("TCA0.CMP0BUF"),
            "high byte of a different register: {err}"
        );
    }

    #[test]
    fn makes_the_firmware_wait_for_the_pit_to_sync() {
        let mut c = chip();
        let ctrla = reg::RTC_PITCTRLA as u16;
        let enable = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        assert!(c.write(ctrla, enable).unwrap_err().contains("CTRLBUSY"));
        let status = reg::RTC_PITSTATUS as u16;
        while c.read(status).unwrap() & regs::RTC_CTRLBUSY != 0 {}
        assert_eq!(c.write(ctrla, enable), Ok(()));
        assert!(c.write(ctrla, 0).is_err(), "busy again after a write");
    }

    #[test]
    fn reading_the_pit_flag_doesnt_move_time_on() {
        let mut c = chip();
        let flags = reg::RTC_PITINTFLAGS as u16;
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        assert_eq!(c.read(flags), Ok(0));
        assert_eq!((c.now, c.ticks), (0, 0), "no skipping ahead to the tick");
        c.idle().unwrap();
        assert_eq!(c.read(flags), Ok(regs::RTC_PI));
        assert_eq!(c.read(flags), Ok(regs::RTC_PI), "stays up until cleared");
        c.write(flags, regs::RTC_PI).unwrap();
        assert_eq!(c.read(flags), Ok(0));
    }

    /// Writes WDT.CTRLA with the CCP key just before, as `hw::init` does.
    fn wdt_write(c: &mut Chip, value: u8) -> Result<(), String> {
        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG)?;
        c.write(reg::WDT_CTRLA as u16, value)
    }

    /// `hw::init`'s setting: 32 clocks closed, then 32 open.
    const WDT_ON: u8 = regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK;

    /// A chip with the PIT running and the watchdog written with `value`
    /// and synced, as `hw::init` leaves it.
    fn watchdog(value: u8) -> Chip {
        let mut c = chip();
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        wdt_write(&mut c, value).unwrap();
        while c.read(reg::WDT_STATUS as u16).unwrap() != 0 {}
        c
    }

    #[test]
    fn the_watchdog_needs_the_64_hz_tick() {
        let mut c = watchdog(WDT_ON);
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        let err = next_tick(&mut c).unwrap_err();
        assert!(err.contains("CYC512"), "{err}");
    }

    #[test]
    fn in_step_wants_window_mode() {
        assert!(watchdog(WDT_ON).watchdog_windowed());
        assert!(!watchdog(regs::WDT_PERIOD_32CLK).watchdog_windowed());
        assert!(!chip().watchdog_windowed());
        let mut c = chip();
        wdt_write(&mut c, regs::WDT_WINDOW_32CLK).unwrap();
        assert!(!c.watchdog_windowed(), "a window with PERIOD off is off");
    }

    #[test]
    fn wdt_ctrla_needs_the_ccp_key() {
        let mut c = chip();
        let err = c.write(reg::WDT_CTRLA as u16, WDT_ON).unwrap_err();
        assert!(err.contains("CCP"), "{err}");
        assert!(!c.watchdog_running());
        assert_eq!(wdt_write(&mut c, WDT_ON), Ok(()));
        assert_eq!(c.io[reg::WDT_CTRLA], WDT_ON);
        assert!(c.watchdog_running());
    }

    #[test]
    fn wdt_ctrla_waits_for_syncbusy() {
        let mut c = chip();
        let status = reg::WDT_STATUS as u16;
        assert_eq!(c.read(status), Ok(0), "idle after reset");
        wdt_write(&mut c, WDT_ON).unwrap();
        assert_eq!(c.read(status), Ok(regs::WDT_SYNCBUSY));
        let err = wdt_write(&mut c, WDT_ON).unwrap_err();
        assert!(err.contains("SYNCBUSY"), "{err}");
        assert_eq!(c.read(status), Ok(regs::WDT_SYNCBUSY));
        assert_eq!(c.read(status), Ok(0));
    }

    #[test]
    fn refuses_to_lock_the_watchdog() {
        let mut c = chip();
        let status = reg::WDT_STATUS as u16;
        assert!(c.write(status, WDT_LOCK).unwrap_err().contains("LOCK"));
        assert!(c.write(status, 0).is_err());
    }

    #[test]
    fn refuses_a_reserved_watchdog_period_or_window() {
        for value in [0x0C, 0xC3, 0xFF] {
            let err = wdt_write(&mut chip(), value).unwrap_err();
            assert!(err.contains("reserved"), "{value:#04x}: {err}");
        }
        assert_eq!(wdt_write(&mut chip(), 0xBB), Ok(()), "8KCLK both");
    }

    /// Sleeps to the next tick and clears the PIT's flag, as
    /// `hw::wait_for_tick` and `hw::rtc_pit` do between them.
    fn next_tick(c: &mut Chip) -> Result<(), String> {
        c.idle()?;
        c.write(reg::RTC_PITINTFLAGS as u16, regs::RTC_PI)
    }

    #[test]
    fn refuses_to_change_the_watchdog_once_its_on() {
        let mut c = watchdog(WDT_ON);
        let err = wdt_write(&mut c, 0).unwrap_err();
        assert!(err.contains("off"), "{err}");
        let mut c = watchdog(WDT_ON);
        let err = wdt_write(&mut c, regs::WDT_PERIOD_32CLK).unwrap_err();
        assert!(err.contains("already on"), "{err}");
    }

    #[test]
    fn refuses_a_wdr_before_ctrla_syncs() {
        let mut c = chip();
        wdt_write(&mut c, WDT_ON).unwrap();
        let err = c.wdr().unwrap_err();
        assert!(err.contains("synced"), "{err}");
    }

    #[test]
    fn before_the_first_wdr_only_the_open_window_counts() {
        // No closed window yet (§19.3.3.2), so a wdr at once is fine...
        assert_eq!(watchdog(WDT_ON).wdr(), Ok(()));
        // ...and 32 clocks, two ticks, without one resets the chip.
        let mut c = watchdog(WDT_ON);
        assert_eq!(next_tick(&mut c), Ok(()));
        let err = next_tick(&mut c).unwrap_err();
        assert!(err.contains("2 ticks (32 WDT clocks)"), "{err}");
    }

    #[test]
    fn a_wdr_in_the_closed_window_resets_the_chip() {
        // One tick is 16 clocks, inside the closed window; two is 32, on its
        // edge, where the wdr's sync decides it.
        for ticks in [0, 1, 2] {
            let mut c = watchdog(WDT_ON);
            c.wdr().unwrap();
            for _ in 0..ticks {
                next_tick(&mut c).unwrap();
            }
            let err = c.wdr().unwrap_err();
            assert!(err.contains("closed window"), "{ticks} ticks: {err}");
        }
    }

    #[test]
    fn running_out_the_open_window_resets_the_chip() {
        let mut c = watchdog(WDT_ON);
        c.wdr().unwrap();
        for tick in 1..4 {
            assert_eq!(next_tick(&mut c), Ok(()), "tick {tick}");
        }
        let err = next_tick(&mut c).unwrap_err();
        assert!(err.contains("4 ticks (64 WDT clocks)"), "{err}");
    }

    #[test]
    fn feeding_as_the_library_says_keeps_the_watchdog_quiet() {
        let mut c = watchdog(WDT_ON);
        let mut feeder = ts21::watchdog::Feeder::new();
        for tick in 0..100 {
            assert_eq!(next_tick(&mut c), Ok(()), "tick {tick}");
            if feeder.tick() {
                assert_eq!(c.wdr(), Ok(()), "tick {tick}");
            }
        }
    }

    #[test]
    fn feeding_every_tick_lands_in_the_closed_window() {
        let mut c = watchdog(WDT_ON);
        next_tick(&mut c).unwrap();
        c.wdr().unwrap();
        next_tick(&mut c).unwrap();
        assert!(c.wdr().unwrap_err().contains("closed window"));
    }

    #[test]
    fn in_normal_mode_an_early_wdr_is_fine() {
        let mut c = watchdog(regs::WDT_PERIOD_32CLK);
        for tick in 0..20 {
            assert_eq!(next_tick(&mut c), Ok(()), "tick {tick}");
            assert_eq!(c.wdr(), Ok(()), "tick {tick}");
            assert_eq!(c.wdr(), Ok(()), "tick {tick}, twice");
        }
    }

    #[test]
    fn the_watchdog_period_runs_from_8_to_8k_cycles() {
        // 8CLK and 16CLK are a tick or less, so even a fed chip resets.
        for period in [0x01, 0x02] {
            let mut c = watchdog(period);
            assert!(next_tick(&mut c).is_err(), "PERIOD {period}");
        }
        // 8KCLK is 512 ticks.
        let mut c = watchdog(WDT_PERIOD_MAX);
        for tick in 1..512 {
            assert_eq!(next_tick(&mut c), Ok(()), "tick {tick}");
        }
        assert!(next_tick(&mut c).is_err());
    }

    #[test]
    fn a_watchdog_thats_off_never_fires() {
        let mut c = chip();
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        for tick in 0..20 {
            assert_eq!(next_tick(&mut c), Ok(()), "tick {tick}");
            assert_eq!(c.wdr(), Ok(()), "tick {tick}");
        }
    }

    /// A chip like `watchdog`'s with a `fault`, on which a bite resets it.
    fn biting(value: u8, fault: Option<WdtFault>) -> Chip {
        let mut c = chip();
        c.bites = true;
        c.world.wdt = fault;
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        wdt_write(&mut c, value).unwrap();
        while c.read(reg::WDT_STATUS as u16).unwrap() != 0 {}
        c
    }

    /// Ticks until `c` bites, up to `most`, feeding it after each of the
    /// ticks `feeds` says.
    fn ticks_to_bite(c: &mut Chip, most: u32, feeds: &[u32]) -> Option<u32> {
        for tick in 1..=most {
            next_tick(c).unwrap();
            if c.pending_reset.is_some() {
                return Some(tick);
            }
            if feeds.contains(&tick) {
                c.wdr().unwrap();
                if c.pending_reset.is_some() {
                    return Some(tick);
                }
            }
        }
        None
    }

    #[test]
    fn a_timeout_or_a_closed_window_resets_the_chip_once_bites_are_expected() {
        let mut c = biting(regs::WDT_PERIOD_32CLK, None);
        assert_eq!(ticks_to_bite(&mut c, 8, &[]), Some(2));
        assert_eq!(c.pending_reset, Some(regs::RSTCTRL_WDRF));
        let mut c = biting(WDT_ON, None);
        assert_eq!(ticks_to_bite(&mut c, 8, &[1, 2]), Some(2));
    }

    #[test]
    fn a_watchdog_that_never_bites() {
        let mut c = biting(regs::WDT_PERIOD_8CLK, Some(WdtFault::Never));
        assert_eq!(ticks_to_bite(&mut c, 100, &[]), None);
    }

    #[test]
    fn a_watchdog_that_bites_early() {
        let mut c = biting(regs::WDT_PERIOD_32CLK, Some(WdtFault::Early));
        assert_eq!(ticks_to_bite(&mut c, 8, &[]), Some(1));
    }

    #[test]
    fn a_watchdog_that_ignores_a_feed() {
        let mut c = biting(regs::WDT_PERIOD_32CLK, Some(WdtFault::Deaf));
        assert_eq!(ticks_to_bite(&mut c, 8, &[1]), Some(2));
    }

    #[test]
    fn a_watchdog_that_stops_once_fed() {
        let mut c = biting(regs::WDT_PERIOD_32CLK, Some(WdtFault::Stops));
        assert_eq!(ticks_to_bite(&mut c, 100, &[1]), None);
        let mut c = biting(regs::WDT_PERIOD_32CLK, Some(WdtFault::Stops));
        assert_eq!(ticks_to_bite(&mut c, 100, &[]), Some(2), "unfed");
    }

    #[test]
    fn a_window_that_lets_an_early_feed_through() {
        let mut c = biting(WDT_ON, Some(WdtFault::Open));
        // Fed in the closed window, and then it times out as usual.
        assert_eq!(ticks_to_bite(&mut c, 8, &[1, 2]), Some(6));
    }

    #[test]
    fn a_window_that_never_opens() {
        let mut c = biting(WDT_ON, Some(WdtFault::Shut));
        assert_eq!(ticks_to_bite(&mut c, 8, &[1]), Some(1));
        let mut c = biting(regs::WDT_PERIOD_32CLK, Some(WdtFault::Shut));
        assert_eq!(ticks_to_bite(&mut c, 8, &[1]), Some(3), "with no window");
    }

    #[test]
    fn rstfr_is_sticky_until_a_1_is_written() {
        let mut c = chip();
        let rstfr = reg::RSTCTRL_RSTFR as u16;
        assert_eq!(c.read(rstfr), Ok(regs::RSTCTRL_PORF), "power-on");
        c.reset(regs::RSTCTRL_WDRF);
        assert_eq!(
            c.read(rstfr),
            Ok(regs::RSTCTRL_PORF | regs::RSTCTRL_WDRF),
            "still there after another reset"
        );
        c.write(rstfr, regs::RSTCTRL_PORF).unwrap();
        assert_eq!(c.read(rstfr), Ok(regs::RSTCTRL_WDRF));
        c.write(rstfr, 0).unwrap();
        assert_eq!(c.read(rstfr), Ok(regs::RSTCTRL_WDRF), "0s do nothing");
    }

    #[test]
    fn a_software_reset_takes_the_ccp_key() {
        let swrr = reg::RSTCTRL_SWRR as u16;
        let mut c = chip();
        assert!(c.write(swrr, regs::RSTCTRL_SWRE).is_err());
        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.write(swrr, regs::RSTCTRL_SWRE).unwrap();
        assert_eq!(c.pending_reset, Some(regs::RSTCTRL_SWRF));
    }

    #[test]
    fn a_reset_leaves_sram_and_the_outside_world_and_nothing_else() {
        let mut c = biting(WDT_ON, Some(WdtFault::Open));
        c.write(SRAM.start, 0x42).unwrap();
        c.set_switch(true).unwrap();
        c.io[reg::VPORTB_DIR] = FET;
        c.pending_reset = Some(regs::RSTCTRL_WDRF);
        let (now, ticks) = (c.now, c.ticks);

        c.reset(regs::RSTCTRL_WDRF);

        assert_eq!(c.read(SRAM.start), Ok(0x42));
        assert!(c.pressed);
        assert_eq!(c.world.wdt, Some(WdtFault::Open));
        assert_eq!((c.now, c.ticks, c.resets), (now, ticks, 1));
        assert!(!c.watchdog_running(), "WDTCFG is 0");
        assert_eq!(c.io[reg::VPORTB_DIR], 0, "the pins float until startup");
        assert_eq!(c.pending_reset, None);
    }

    #[test]
    fn a_reset_can_lose_sram_if_the_world_says_so() {
        let mut c = chip();
        c.world.wdt = Some(WdtFault::Wipes);
        c.write(SRAM.start, 0x42).unwrap();
        c.reset(regs::RSTCTRL_WDRF);
        assert_eq!(c.read(SRAM.start), Ok(JUNK));
    }

    #[test]
    fn a_stuck_cpu_runs_nothing_until_the_watchdog_resets_it() {
        let mut s = sim(&[SPIN]);
        s.chip.bites = true;
        s.chip.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        s.chip.io[reg::WDT_CTRLA] = regs::WDT_PERIOD_32CLK;
        s.chip.stuck = true;
        while s.chip.resets == 0 {
            s.step().unwrap();
        }
        assert_eq!(s.executed, 0);
        assert_eq!(s.chip.ticks, 2);
        assert!(!s.chip.stuck, "the reset ends it");
    }

    #[test]
    fn a_reset_starts_the_cpu_and_the_self_test_wait_again() {
        let mut s = sim(&[SPIN]).testing_until(0x1000);
        s.testing = None;
        s.cpu.sp = SRAM.end - 0x10;
        s.chip.pending_reset = Some(regs::RSTCTRL_WDRF);
        s.step().unwrap();
        assert_eq!((s.cpu.pc, s.cpu.sp), (0, SRAM.end - 1));
        assert_eq!(s.chip.resets, 1);
        assert_eq!(s.testing, Some(0x1000 / 2));
    }

    /// The watchdog counts whenever time moves: polling for the tick, a
    /// period of instructions, or asleep.
    #[test]
    fn the_watchdog_counts_however_time_passes() {
        let mut c = watchdog(WDT_ON);
        c.wdr().unwrap();
        while c.now < c.next_pit - 1 {
            c.elapse().unwrap();
        }
        assert_eq!(c.wdt.ticks, 0);
        c.elapse().unwrap();
        assert_eq!(c.wdt.ticks, 1, "a period of instructions");
        c.idle().unwrap();
        assert_eq!(c.wdt.ticks, 2, "asleep");
        c.wdr().unwrap_err();
        let mut c = watchdog(WDT_ON);
        c.wdr().unwrap();
        for _ in 0..3 {
            c.idle().unwrap();
        }
        assert!(c.idle().unwrap_err().contains("without wdr"), "asleep");

        // The same timeout landing mid-pass, an instruction at a time.
        let mut c = watchdog(WDT_ON);
        c.wdr().unwrap();
        for _ in 0..3 {
            c.idle().unwrap();
        }
        let deadline = c.next_pit;
        while c.now < deadline - 1 {
            c.elapse().unwrap();
        }
        assert!(c.elapse().unwrap_err().contains("without wdr"), "awake");
    }

    #[test]
    fn the_pit_flag_goes_up_once_a_period_whoever_is_looking() {
        let mut c = chip();
        assert!(c.idle().unwrap_err().contains("PIT off"));
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        while c.now < PIT_PERIOD - 1 {
            c.elapse().unwrap();
        }
        assert!(!c.pit_flag);
        c.elapse().unwrap();
        assert!(c.pit_flag, "a period of instructions");
        assert_eq!(c.ticks, 1);

        // Asleep, time jumps to the end of the next period, and a
        // conversion that was running finishes.
        c.pit_flag = false;
        c.adc.ready_at = Some(c.now + CONVERSION_INSTRUCTIONS);
        c.idle().unwrap();
        assert_eq!((c.now, c.ticks, c.pit_flag), (2 * PIT_PERIOD, 2, true));
        assert!(!c.converting());
    }

    #[test]
    fn the_clocks_set_the_pit_period_in_main_clocks() {
        let mut c = chip();
        c.world.osc32k = 50;
        assert_eq!(c.pit_cycles(), 2 * PIT_PERIOD);
        c.world.osc32k = 100;
        c.world.main_clock = 400;
        assert_eq!(c.pit_cycles(), 4 * PIT_PERIOD);
        c.world.main_clock = 100;
        c.world.osc32k = 0;
        assert_eq!(c.pit_cycles(), PIT_PERIOD, "standing in for a tick's time");
    }

    #[test]
    fn a_dead_oscillator_stops_the_pit_the_wdt_and_their_sync_flags() {
        let mut c = chip();
        c.world.osc32k = 0;
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        c.io[reg::WDT_CTRLA] = regs::WDT_PERIOD_32CLK;
        c.wdt.busy = 1;
        for _ in 0..10 {
            c.idle().unwrap();
            assert_eq!(c.read(reg::RTC_PITSTATUS as u16), Ok(regs::RTC_CTRLBUSY));
            assert_eq!(c.read(reg::WDT_STATUS as u16), Ok(regs::WDT_SYNCBUSY));
        }
        assert!(!c.pit_flag, "no flag");
        assert_eq!(c.ticks, 10, "only the time");
        assert_eq!(c.wdt.ticks, 0, "and the watchdog never ran out");
        assert_eq!(c.wdr(), Ok(()));
    }

    #[test]
    fn tca0_flags_an_overflow_every_cycle_until_its_cleared() {
        let mut c = chip();
        let flags = reg::TCA0_INTFLAGS as u16;
        let cycle = u64::from(ramp::PWM_TOP);
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        c.wide[1] = ramp::PWM_TOP - 1;
        c.next_bottom = cycle;
        assert_eq!(c.read(flags), Ok(0));
        assert!(c.tca_polled);
        c.advance_to(cycle - 1).unwrap();
        assert_eq!(c.read(flags), Ok(0));
        c.advance_to(cycle).unwrap();
        assert_eq!(c.read(flags), Ok(regs::TCA_OVF));
        c.advance_to(3 * cycle).unwrap();
        assert_eq!(c.read(flags), Ok(regs::TCA_OVF), "one flag for any number");
        c.write(flags, regs::TCA_OVF).unwrap();
        assert_eq!(c.read(flags), Ok(0));
        c.advance_to(4 * cycle).unwrap();
        assert_eq!(c.read(flags), Ok(regs::TCA_OVF));
        c.write(flags, regs::TCA_OVF).unwrap();
        c.world.tca_stopped = true;
        c.advance_to(8 * cycle).unwrap();
        assert_eq!(c.read(flags), Ok(0), "stopped");
    }

    #[test]
    fn tca0s_overflows_follow_the_live_period() {
        let mut c = chip();
        let flags = reg::TCA0_INTFLAGS as u16;
        let long = u64::from(ramp::MAX_TOP);
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        c.wide[1] = ramp::MAX_TOP - 1;
        c.next_bottom = long;
        c.advance_to(long - 1).unwrap();
        assert_eq!(c.read(flags), Ok(0));
        c.advance_to(long).unwrap();
        assert_eq!(c.read(flags), Ok(regs::TCA_OVF));
    }

    /// TCB0 as `hw::init` sets it up.
    fn deadline(c: &mut Chip) {
        let [ccmp, cnt] = [reg::TCB0_CCMP as u16, reg::TCB0_CNT as u16];
        c.write(ccmp, 0xFF).unwrap();
        c.write(ccmp + 1, 0xFF).unwrap();
        c.write(reg::TCB0_INTCTRL as u16, regs::TCB_CAPT).unwrap();
        c.write(
            reg::TCB0_CTRLA as u16,
            regs::TCB_CLKSEL_CLKDIV2 | regs::TCB_ENABLE,
        )
        .unwrap();
        assert!(c.write(cnt + 1, 0).is_err(), "high byte first");
    }

    #[test]
    fn tcb0_fires_after_131072_main_clocks_and_wakes_idle() {
        let mut c = chip();
        c.world.osc32k = 0;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN;
        deadline(&mut c);
        while !c.tcb.capt {
            c.idle().unwrap();
        }
        assert_eq!(c.now, 131_072);
        assert_eq!(
            c.interrupt(),
            Some(regs::VECTOR_TCB0_INT as u8),
            "it goes off with no PIT to wake it"
        );
        c.write(reg::TCB0_INTFLAGS as u16, regs::TCB_CAPT).unwrap();
        assert_eq!(c.interrupt(), None);
    }

    #[test]
    fn writing_cnt_puts_tcb0s_deadline_back() {
        let mut c = chip();
        deadline(&mut c);
        c.now = 100_000;
        let cnt = reg::TCB0_CNT as u16;
        c.write(cnt, 0).unwrap();
        c.write(cnt + 1, 0).unwrap();
        c.advance_to(131_072).unwrap();
        assert!(!c.tcb.capt);
        c.advance_to(231_072).unwrap();
        assert!(c.tcb.capt);
    }

    #[test]
    fn tcb0_stops_in_power_down() {
        let mut c = chip();
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SMODE_PDOWN | regs::SLPCTRL_SEN;
        deadline(&mut c);
        for _ in 0..4 {
            c.idle().unwrap();
        }
        assert_eq!(c.now, 4 * PIT_PERIOD);
        assert!(!c.tcb.capt, "over a deadline's time, but it was stopped");
    }

    #[test]
    fn a_read_after_the_tick_doesnt_start_a_counted_wait() {
        let mut c = chip();
        c.pit_flag = true;
        c.read(reg::TCA0_INTFLAGS as u16).unwrap();
        assert!(!c.tca_polled);
        c.pit_flag = false;
        c.read(reg::TCA0_INTFLAGS as u16).unwrap();
        assert!(c.tca_polled);
    }

    #[test]
    fn a_new_world_changes_the_period_thats_started() {
        let mut c = chip();
        c.set_world(World {
            osc32k: 50,
            ..World::NORMAL
        });
        assert_eq!(c.next_pit, 2 * PIT_PERIOD);
        c.set_world(World::NORMAL);
        assert_eq!(c.next_pit, PIT_PERIOD);
    }

    #[test]
    fn tcb0_refuses_what_isnt_modelled() {
        let mut c = chip();
        let (ctrla, cnt) = (reg::TCB0_CTRLA as u16, reg::TCB0_CNT as u16);
        assert!(c.write(ctrla, regs::TCB_ENABLE).is_err(), "no prescaler");
        c.write(cnt, 1).unwrap();
        assert!(c.write(cnt + 1, 0).is_err(), "CNT only goes back to 0");
    }

    #[test]
    fn a_compare_written_directly_applies_at_once() {
        let mut c = chip();
        let cmp1 = reg::TCA0_CMP1 as u16;
        c.compare = [7, 9];
        c.write(cmp1, 0).unwrap();
        c.write(cmp1 + 1, 0).unwrap();
        assert_eq!(c.compare, [7, 0]);
        assert!(c.write(cmp1 + 1, 0).is_err(), "no low byte first");
    }

    #[test]
    fn the_pit_interrupts_only_once_enabled() {
        let mut c = chip();
        c.pit_flag = true;
        assert_eq!(c.interrupt(), None, "PITINTCTRL.PI clear");
        c.write(reg::RTC_PITINTCTRL as u16, regs::RTC_PI).unwrap();
        assert_eq!(c.interrupt(), Some(regs::VECTOR_RTC_PIT as u8));
        c.write(reg::RTC_PITINTFLAGS as u16, regs::RTC_PI).unwrap();
        assert_eq!(c.interrupt(), None, "cleared by writing 1");
    }

    #[test]
    fn the_switch_flags_porta_on_the_edges_its_isc_asks_for() {
        let pin5 = reg::PORTA_PIN0CTRL + 5;
        let mut c = chip();
        c.set_switch(true).unwrap();
        assert_eq!(c.porta_flags, 0, "INTDISABLE");
        assert_eq!(c.interrupt(), None);

        for (isc, press, release) in [
            (ISC_BOTHEDGES, true, true),
            (ISC_FALLING, true, false),
            (ISC_RISING, false, true),
        ] {
            let mut c = chip();
            c.io[pin5] = regs::PORT_PULLUPEN | isc;
            c.set_switch(true).unwrap();
            assert_eq!(c.porta_flags != 0, press, "ISC {isc} on press");
            c.porta_flags = 0;
            c.set_switch(true).unwrap();
            assert_eq!(c.porta_flags, 0, "no edge, no flag");
            c.set_switch(false).unwrap();
            assert_eq!(c.porta_flags != 0, release, "ISC {isc} on release");
        }

        // Both pending: PORTA's vector is the lower, so it goes first.
        let mut c = chip();
        c.io[pin5] = ISC_BOTHEDGES;
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        c.pit_flag = true;
        c.set_switch(true).unwrap();
        assert_eq!(c.interrupt(), Some(VECTOR_PORTA_PORT as u8));
        c.write(PORTA_INTFLAGS as u16, SWITCH).unwrap();
        assert_eq!(c.read(PORTA_INTFLAGS as u16), Ok(0));
        assert_eq!(c.interrupt(), Some(regs::VECTOR_RTC_PIT as u8));

        c.io[pin5] = ISC_LEVEL;
        assert!(c.set_switch(false).unwrap_err().contains("ISC"));
    }

    #[test]
    fn the_button_led_needs_pb5_an_input_with_its_pull_up() {
        let mut c = chip();
        assert!(!c.button_led_lit(), "reset: no pull-up");
        c.io[reg::PORTB_PIN0CTRL + 5] = regs::PORT_PULLUPEN;
        assert!(c.button_led_lit());
        c.io[reg::VPORTB_DIR] = 1 << 5;
        assert!(!c.button_led_lit(), "driven, not pulled up");
    }

    #[test]
    fn enables_follow_each_ports_isc_and_the_pit() {
        let mut c = chip();
        assert_eq!(c.enabled(), 0);
        c.io[reg::PORTB_PIN0CTRL + 2] = ISC_INPUT_DISABLE;
        assert_eq!(c.enabled(), 0, "INPUT_DISABLE doesn't interrupt");
        c.io[reg::PORTC_PIN0CTRL + 3] = ISC_LEVEL;
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        assert_eq!(
            c.enabled(),
            1 << VECTOR_PORTC_PORT | 1 << regs::VECTOR_RTC_PIT
        );
    }

    #[test]
    fn sleeps_only_in_idle_or_power_down_with_something_to_wake_it() {
        let mut c = chip();
        assert!(c.sleep().unwrap_err().contains("SEN clear"));
        let ctrla = reg::SLPCTRL_CTRLA as u16;
        c.write(ctrla, regs::SLPCTRL_SEN | 1 << 1).unwrap();
        assert!(c.sleep().unwrap_err().contains("only models"), "STDBY");
        c.write(ctrla, regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_IDLE)
            .unwrap();
        assert!(c.sleep().unwrap_err().contains("no interrupt enabled"));
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        assert!(c.sleep().is_err(), "the PIT's interrupt, but the PIT's off");
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        assert_eq!(c.sleep(), Ok(()));
        // The switch alone would do too.
        c.io[reg::RTC_PITINTCTRL] = 0;
        c.io[reg::PORTA_PIN0CTRL + 5] = ISC_BOTHEDGES;
        assert_eq!(c.sleep(), Ok(()));
    }

    /// A chip that's ready to power down, with the PIT to wake it.
    fn ready_to_power_down() -> Chip {
        let mut c = chip();
        c.pit_busy = 0;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_PDOWN;
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        c.wide[1] = ramp::Output::OFF.top - 1;
        c
    }

    #[test]
    fn powers_down_when_ready_and_counts_it() {
        let mut c = ready_to_power_down();
        assert_eq!(c.sleep(), Ok(()));
        assert_eq!(c.power_downs, 1);
    }

    #[test]
    fn idling_doesnt_count_as_powering_down() {
        let mut c = ready_to_power_down();
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_IDLE;
        assert_eq!(c.sleep(), Ok(()));
        assert_eq!(c.power_downs, 0);
    }

    #[test]
    fn wont_power_down_mid_conversion() {
        let mut c = ready_to_power_down();
        c.adc.ready_at = Some(c.now + 1);
        assert!(c.sleep().unwrap_err().contains("mid-conversion"));
    }

    #[test]
    fn wont_power_down_with_the_pit_still_syncing() {
        let mut c = ready_to_power_down();
        c.pit_busy = 1;
        assert!(c.sleep().unwrap_err().contains("CTRLBUSY"));
    }

    #[test]
    fn powering_down_leaves_the_next_conversion_unsettled() {
        let mut c = ready_to_power_down();
        c.adc.last_setup = Some((0, 0, 0));
        assert_eq!(c.sleep(), Ok(()));
        assert_eq!(c.adc.last_setup, None);
    }

    #[test]
    fn wont_power_down_with_the_adc_enabled() {
        let mut c = ready_to_power_down();
        c.io[reg::ADC0_CTRLA] = regs::ADC_ENABLE;
        assert!(c.sleep().unwrap_err().contains("ADC0 still enabled"));
    }

    #[test]
    fn wont_power_down_with_the_reference_forced_on() {
        let mut c = ready_to_power_down();
        c.io[reg::VREF_CTRLB] = regs::VREF_ADC0REFEN;
        assert!(c.sleep().unwrap_err().contains("ADC0REFEN"));
    }

    #[test]
    fn wont_power_down_with_a_channel_on_its_pin() {
        for channel in [regs::TCA_CMP0EN, regs::TCA_CMP1EN] {
            let mut c = ready_to_power_down();
            c.io[reg::TCA0_CTRLB] = channel;
            assert!(c.sleep().unwrap_err().contains("still driving"));
        }
    }

    #[test]
    fn wont_power_down_with_a_compare_or_buf_set() {
        let mut c = ready_to_power_down();
        c.compare[0] = 1;
        assert!(c.sleep().unwrap_err().contains("compares"), "live");
        let mut c = ready_to_power_down();
        c.wide[3] = 1;
        assert!(c.sleep().unwrap_err().contains("compares"), "BUF");
    }

    #[test]
    fn wont_power_down_with_the_pit_off_and_no_pin_to_wake_it() {
        let mut c = ready_to_power_down();
        c.io[reg::RTC_PITCTRLA] = 0;
        assert!(c.sleep().unwrap_err().contains("no interrupt enabled"));
    }

    #[test]
    fn only_some_pin_senses_wake_it_from_power_down() {
        let pin = |n: usize| reg::PORTA_PIN0CTRL + n;
        for (at, isc, wakes) in [
            (5, ISC_RISING, false),
            (5, ISC_FALLING, false),
            (5, ISC_BOTHEDGES, true),
            (5, ISC_LEVEL, true),
            (2, ISC_FALLING, true),
            (6, ISC_RISING, true),
        ] {
            let mut c = ready_to_power_down();
            c.io[reg::RTC_PITINTCTRL] = 0;
            c.io[pin(at)] = isc;
            assert_eq!(c.sleep().is_ok(), wakes, "PA{at} ISC {isc}");
        }
        // Idle has the clock, so any sense will do.
        let mut c = ready_to_power_down();
        c.io[reg::RTC_PITINTCTRL] = 0;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_IDLE;
        c.io[reg::RTC_PITCTRLA] = 0;
        c.io[pin(5)] = ISC_FALLING;
        assert_eq!(c.sleep(), Ok(()));
    }

    #[test]
    fn the_switch_cant_be_read_with_its_input_buffer_off() {
        let mut c = chip();
        c.io[reg::PORTA_PIN0CTRL + 5] = ISC_INPUT_DISABLE;
        let error = c.read(reg::VPORTA_IN as u16).unwrap_err();
        assert!(error.contains("input buffer"), "{error}");
    }

    /// Writes `value` to a 16-bit TCA0 register, low byte first.
    fn write_wide(c: &mut Chip, addr: usize, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        c.write(addr as u16, lo).unwrap();
        c.write(addr as u16 + 1, hi).unwrap();
    }

    fn read_wide(c: &mut Chip, addr: usize) -> u16 {
        let lo = c.read(addr as u16).unwrap();
        let hi = c.read(addr as u16 + 1).unwrap();
        u16::from_le_bytes([lo, hi])
    }

    #[test]
    fn a_compare_takes_its_buf_at_bottom() {
        let out = ramp::output(ramp::LEVELS);
        let mut c = pwm_at(0);
        set_output(&mut c, out).unwrap();
        c.advance_to(c.next_bottom - 1).unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP1), 0);
        c.elapse().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP1), out.linear);
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), out.fet);
    }

    #[test]
    fn writing_cnt_puts_the_update_off_rather_than_forcing_it() {
        let out = ramp::output(ramp::LEVELS);
        let mut c = pwm_at(0);
        set_output(&mut c, out).unwrap();
        c.advance_to(c.next_bottom - 1).unwrap();
        write_wide(&mut c, reg::TCA0_CNT, 0);
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0, "not forced");
        c.advance_to(c.now + c.tca_cycle() - 1).unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0, "a full cycle away");
        c.elapse().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), out.fet);
    }

    #[test]
    fn a_stopped_timer_never_updates_its_compares() {
        let mut c = chip();
        write_wide(&mut c, reg::TCA0_CMP0BUF, 7);
        c.advance_to(c.now + 0x20000).unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0);
    }

    #[test]
    fn a_compares_high_byte_needs_its_low_byte_first() {
        let mut c = chip();
        let high = reg::TCA0_CMP0 as u16 + 1;
        assert!(c.read(high).unwrap_err().contains("TCA0.CMP0"));
        c.read(reg::TCA0_CMP1 as u16).unwrap();
        assert!(c.read(high).is_err(), "the other register's low byte");
    }

    #[test]
    fn finds_the_vectors_with_handlers() {
        // Slots of `jmp 0x0040` (word 0x20) except 3 and 7, which go elsewhere.
        let mut image = Vec::new();
        for v in 0..regs::VECTORS {
            let target: u16 = match v {
                3 => 0x30,
                7 => 0x31,
                _ => 0x20,
            };
            image.extend(0x940Cu16.to_le_bytes());
            image.extend(target.to_le_bytes());
        }
        assert_eq!(handled(&image, 0x20), 1 << 3 | 1 << 7);
    }

    /// The word address of the `reti` `image` puts straight after the table.
    const UNHANDLED: u16 = (regs::VECTORS * 2) as u16;

    /// A vector table whose every interrupt slot goes to a `reti` at
    /// `UNHANDLED`, then `main` straight after it, which reset jumps to.
    fn image(main: &[u16]) -> Vec<u8> {
        let mut words = Vec::new();
        for v in 0..regs::VECTORS {
            words.extend([0x940C, if v == 0 { UNHANDLED + 1 } else { UNHANDLED }]);
        }
        words.push(0x9518); // reti
        words.extend(main);
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn sim(main: &[u16]) -> Sim {
        Sim::new(image(main), SRAM.start + 1, u32::from(UNHANDLED) * 2).unwrap()
    }

    /// `image`'s `main` as a word address, and the byte address of its
    /// `n`th word.
    const MAIN: u32 = UNHANDLED as u32 + 1;
    fn at(n: u32) -> u32 {
        (MAIN + n) * 2
    }

    // ldi r16, 0x38; out SPH, r16: SP to 0x38FF, as the register test does.
    const SP_LOW: [u16; 2] = [0xE308, 0xBF0E];
    // ldi r16, 0x3F; out SPH, r16: SP back to 0x3FFF.
    const SP_BACK: [u16; 2] = [0xE30F, 0xBF0E];
    const NOP: u16 = 0x0000;
    const SPIN: u16 = 0xCFFF; // rjmp .

    /// .bss up to 0x3A00, so SP at 0x38FF is in it.
    fn sim_with_bss(main: &[u16]) -> Sim {
        Sim::new(image(main), 0x3A00, u32::from(UNHANDLED) * 2).unwrap()
    }

    #[test]
    fn checks_the_stack_from_reset_without_self_tests() {
        let mut s = sim_with_bss(&[SP_LOW[0], SP_LOW[1], NOP, SPIN]);
        let err = (0..4).try_for_each(|_| s.step()).unwrap_err();
        assert!(err.contains("ran into .bss"), "{err}");
    }

    #[test]
    fn holds_the_stack_checks_until_the_self_tests_are_done() {
        let program = [SP_LOW[0], SP_LOW[1], SP_BACK[0], SP_BACK[1], NOP, SPIN];
        let mut s = sim_with_bss(&program).testing_until(at(4));
        for _ in 0..10 {
            s.step().unwrap();
        }
        assert_eq!(s.testing, None);
        assert_eq!(s.stack(), 0, "the self-tests' SP doesn't count as stack");
    }

    #[test]
    fn refuses_self_tests_that_leave_sp_elsewhere() {
        let mut s = sim_with_bss(&[SP_LOW[0], SP_LOW[1], NOP, SPIN]).testing_until(at(2));
        let err = (0..4).try_for_each(|_| s.step()).unwrap_err();
        assert!(err.contains("left SP at 0x38ff"), "{err}");
    }

    #[test]
    fn boot_refuses_main_before_the_self_tests_finish() {
        let mut s = sim(&[SPIN]).testing_until(0x1000);
        let err = s.boot(MAIN, MAIN + 100).unwrap_err();
        assert!(err.contains("without passing __ram_tested"), "{err}");
    }

    #[test]
    fn boot_says_which_check_failed() {
        // nop; nop; rjmp .+0 to the next word, which is `failed`.
        let mut s = sim(&[NOP, NOP, 0xC000, SPIN]);
        assert_eq!(
            s.boot(MAIN + 100, MAIN + 3),
            Ok(Boot::Failed { from: MAIN + 2 })
        );
        let mut s = sim(&[SPIN]);
        assert_eq!(s.boot(MAIN, MAIN + 100), Ok(Boot::Main));
    }

    #[test]
    fn a_stuck_register_is_stuck_from_the_first_instruction() {
        // mov r17, r16, with r16 stuck high at bit 0 and never written.
        let mut s = sim(&[0x2F10, SPIN]);
        s.chip.world.register = Some(RegisterFault::Stuck {
            reg: Register::R(16),
            bit: 0,
            high: true,
        });
        s.step().unwrap(); // jmp
        s.step().unwrap();
        assert_eq!(s.cpu.r[17], 0x01);
    }

    #[test]
    fn ram_faults_reach_loads_and_stores() {
        let mut c = chip();
        c.world.ram = Some(RamFault::Alias {
            addr: 0x3900,
            to: 0x3800,
        });
        c.write(0x3900, 0x12).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x12));
        c.write(0x3800, 0x34).unwrap();
        assert_eq!(c.read(0x3900), Ok(0x34));
        assert_eq!(c.sram(&(0x3900..0x3901)), [JUNK], "0x3900's own byte");

        c.world.ram = Some(RamFault::Stuck {
            addr: 0x3800,
            bit: 7,
            high: false,
        });
        c.write(0x3800, 0xFF).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x7F));

        c.world.ram = Some(RamFault::Bridge {
            addr: 0x3800,
            bits: [0, 1],
        });
        c.write(0x3800, 0x55).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x54));

        c.world.ram = Some(RamFault::Coupling {
            from: 0x3900,
            bit: 7,
            rising: true,
            to: 0x3800,
            victim: 0,
            force: None,
        });
        c.write(0x3800, 0x00).unwrap();
        c.write(0x3900, 0x00).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x00), "no edge");
        c.write(0x3900, 0x80).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x01), "the rising edge");
        c.write(0x3900, 0xFF).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x01), "no edge");
        c.read(0x3900).unwrap();
        assert_eq!(c.read(0x3800), Ok(0x01), "a read");
    }

    #[test]
    fn staying_dark_means_pins_low_interrupts_off_and_no_way_out() {
        // The pins driven low, then spin, as `__selftest_failed` does.
        let dark = |s: &mut Sim| {
            s.chip.io[reg::VPORTB_DIR] = FET | LINEAR;
            s.step().unwrap(); // jmp
        };
        let mut s = sim(&[CLI, SPIN]);
        dark(&mut s);
        assert_eq!(stays_dark(&mut s), Ok(()));

        let mut s = sim(&[CLI, SPIN]);
        s.step().unwrap();
        assert!(stays_dark(&mut s).unwrap_err().contains("floating"));

        // The `cli` can't clear an I bit that's stuck.
        let mut s = sim(&[CLI, SPIN]);
        dark(&mut s);
        s.chip.world.register = Some(RegisterFault::Stuck {
            reg: Register::Sreg,
            bit: I,
            high: true,
        });
        assert!(stays_dark(&mut s).unwrap_err().contains("interrupts on"));

        // nop, then back to the start: it leaves the spin.
        let mut s = sim(&[CLI, NOP, 0xCFFE]);
        dark(&mut s);
        assert!(stays_dark(&mut s).unwrap_err().contains("left"));

        let mut s = sim(&[NOP, SPIN]);
        dark(&mut s);
        assert!(stays_dark(&mut s).unwrap_err().contains("cli"));
    }

    /// `__ram_section_failed`'s shape: two instructions, then the spin.
    const STOP: [u16; 3] = [NOP, NOP, SPIN];

    /// A chip about to run `main` as if it were `__ram_section_failed`, with
    /// the windows `hw::init` sets, just fed.
    fn failing(main: &[u16]) -> Sim {
        let mut s = sim(main);
        s.step().unwrap(); // jmp
        s.chip.io[reg::VPORTB_DIR] = FET | LINEAR;
        s.chip.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        s.chip.io[reg::WDT_CTRLA] = regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK;
        s.chip.wdt.windowed = true;
        s
    }

    #[test]
    fn a_failed_section_holds_the_light_off_until_the_watchdog_runs_out() {
        assert_eq!(held_low_until_reset(&mut failing(&STOP)), Ok(()));

        let mut s = failing(&STOP);
        s.chip.wdt.ticks = 3;
        let tick = s.chip.ticks;
        assert_eq!(held_low_until_reset(&mut s), Ok(()));
        assert_eq!(s.chip.ticks, tick + 1, "a tick's left of the 64 clocks");
    }

    #[test]
    fn a_failed_section_mustnt_leave_the_light_on_or_the_pins_floating() {
        let mut s = failing(&STOP);
        s.chip.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        s.chip.io[reg::TCA0_CTRLB] = regs::TCA_CMP0EN | regs::TCA_CMP1EN;
        s.chip.wide = [0, 254, 254, 200, 100];
        s.chip.compare = [200, 100];
        assert!(held_low_until_reset(&mut s).unwrap_err().contains("LEDs"));

        let mut s = failing(&STOP);
        s.chip.io[reg::VPORTB_DIR] = FET;
        let err = held_low_until_reset(&mut s).unwrap_err();
        assert!(err.contains("floating"), "{err}");

        let mut s = failing(&STOP);
        s.cpu.sreg |= 1 << I;
        let err = held_low_until_reset(&mut s).unwrap_err();
        assert!(err.contains("interrupts on"), "{err}");
    }

    #[test]
    fn a_failed_section_mustnt_go_anywhere_or_feed_the_watchdog() {
        // A nop where the spin should be, then back to it.
        let err = held_low_until_reset(&mut failing(&[NOP, NOP, NOP, 0xCFFE])).unwrap_err();
        assert!(err.contains("left"), "{err}");

        // A wdr in the closed window.
        const WDR: u16 = 0x95A8;
        let err = held_low_until_reset(&mut failing(&[WDR, NOP, SPIN])).unwrap_err();
        assert!(err.contains("closed window"), "{err}");

        // A wdr in the open window, which puts the reset off.
        let mut s = failing(&[WDR, NOP, SPIN]);
        s.chip.wdt.ticks = 3;
        let err = held_low_until_reset(&mut s).unwrap_err();
        assert!(err.contains("the watchdog reset it on tick"), "{err}");
    }

    #[test]
    fn a_failed_section_needs_the_watchdog_to_reset_it() {
        let mut s = failing(&STOP);
        s.chip.io[reg::WDT_CTRLA] = 0;
        let err = held_low_until_reset(&mut s).unwrap_err();
        assert!(err.contains("never reset"), "{err}");
    }

    #[test]
    fn an_rjmp_only_goes_forwards_and_within_reach() {
        assert_eq!(rjmp(10, 11), Ok(0xC000));
        assert_eq!(rjmp(10, 2058), Ok(0xC7FF));
        assert!(rjmp(10, 2059).unwrap_err().contains("can't reach"));
        assert!(rjmp(10, 10).unwrap_err().contains("can't reach"));
    }

    #[test]
    fn refuses_an_interrupt_enabled_with_no_handler_once_sreg_i_is_set() {
        // sei; rjmp .
        let mut s = sim(&[0x9478, 0xCFFF]);
        s.chip.io[reg::PORTB_PIN0CTRL + 2] = ISC_BOTHEDGES;
        s.step().unwrap(); // jmp
        let err = s.step().unwrap_err();
        assert!(
            err.contains("enables PORTB_PORT (vector 4) with SREG.I set"),
            "{err}"
        );
    }

    // Hand-written programs for the interrupt tests. Encodings as in avr.rs.
    const SEI: u16 = 0x9478;
    const SLEEP: u16 = 0x9588;
    const RETI: u16 = 0x9518;
    const INC_R17: u16 = 0x9513;
    const OUT_CCP_R16: u16 = 0xBF04; // out CPU.CCP, r16
    const OUT_GPIOR0_R17: u16 = 0xBB1C; // out GPIO.GPIOR0, r17
    const STS_R17: u16 = 0x9310; // sts k, r17; k is the next word
    const STS_R18: u16 = 0x9320;
    const LDS_R19: u16 = 0x9130; // lds r19, k
    const LDS_R20: u16 = 0x9140;

    /// Where `counting_handler` writes down r17.
    const SEEN: u16 = 0x3900;
    /// Writes down r17 and clears PORTA's flags with r18, which `main` has
    /// to have set to 0xFF. It changes no register, as a handler mustn't.
    fn counting_handler() -> Vec<u16> {
        vec![STS_R17, SEEN, STS_R18, PORTA_INTFLAGS as u16, RETI]
    }

    /// ldi r18, 0xFF, then ldi r16, the CCP key.
    const SET_UP: [u16; 2] = [0xEF2F, 0xED08];

    /// `image` with the PORTA vector going to `handler`, after `main`, and
    /// PA5 sensing both edges.
    fn sim_with_handler(main: &[u16], handler: &[u16]) -> Sim {
        let mut bytes = image(main);
        let at = (bytes.len() / 2) as u16;
        let slot = VECTOR_PORTA_PORT * VECTOR_WORDS as usize * 2 + 2;
        bytes[slot..slot + 2].copy_from_slice(&at.to_le_bytes());
        bytes.extend(handler.iter().flat_map(|w| w.to_le_bytes()));
        let mut s = Sim::new(bytes, SRAM.start + 1, u32::from(UNHANDLED) * 2).unwrap();
        s.chip.io[reg::PORTA_PIN0CTRL + 5] = ISC_BOTHEDGES;
        s
    }

    /// `image` with the PIT's vector going to `handler`, after `main`, and
    /// the PIT running with its interrupt on, which is what `Landings` lands.
    fn sim_with_tick_handler(main: &[u16], handler: &[u16]) -> Sim {
        let mut bytes = image(main);
        let at = (bytes.len() / 2) as u16;
        let slot = regs::VECTOR_RTC_PIT * VECTOR_WORDS as usize * 2 + 2;
        bytes[slot..slot + 2].copy_from_slice(&at.to_le_bytes());
        bytes.extend(handler.iter().flat_map(|w| w.to_le_bytes()));
        let mut s = Sim::new(bytes, SRAM.start + 1, u32::from(UNHANDLED) * 2).unwrap();
        s.chip.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        s.chip.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        s
    }

    /// Clears the PIT's flag with r18, as `quiet_handler` does PORTA's.
    fn quiet_tick() -> Vec<u16> {
        vec![STS_R18, reg::RTC_PITINTFLAGS as u16, RETI]
    }

    /// `counting_handler` for the PIT.
    fn counting_tick() -> Vec<u16> {
        vec![STS_R17, SEEN, STS_R18, reg::RTC_PITINTFLAGS as u16, RETI]
    }

    /// Runs `main` with PA5's flag up from reset, and says how many
    /// `inc r17`s ran before `counting_handler` did.
    fn incs_before_the_handler(main: &[u16]) -> u8 {
        let main = [&SET_UP[..], main, &[SPIN]].concat();
        let mut s = sim_with_handler(&main, &counting_handler());
        s.chip.porta_flags = SWITCH;
        for _ in 0..40 {
            s.step().unwrap();
        }
        assert_eq!(s.chip.porta_flags, 0, "the handler ran");
        s.chip.sram(&(SEEN..SEEN + 1))[0]
    }

    #[test]
    fn interrupts_wait_out_the_ccp_window() {
        let incs = [INC_R17; 6];
        // sei's own hold covers the key, which starts the four.
        let main = [&[SEI, OUT_CCP_R16][..], &incs].concat();
        assert_eq!(incs_before_the_handler(&main), 4);
    }

    #[test]
    fn a_store_ends_the_ccp_window_early() {
        let main = [SEI, OUT_CCP_R16, INC_R17, OUT_GPIOR0_R17, INC_R17, INC_R17];
        assert_eq!(incs_before_the_handler(&main), 1, "an I/O register");
        let main = [SEI, OUT_CCP_R16, STS_R17, 0x3910, INC_R17, INC_R17];
        assert_eq!(incs_before_the_handler(&main), 0, "SRAM counts too");
    }

    #[test]
    fn the_protected_write_ends_the_ccp_window() {
        // ldi r17, PDIV 4X | PEN, then the key and its write.
        let value = regs::CLKCTRL_PDIV_4X | regs::CLKCTRL_PEN;
        let main = [
            ldi(17, value),
            SEI,
            OUT_CCP_R16,
            STS_R17,
            reg::CLKCTRL_MCLKCTRLB as u16,
            SPIN,
        ];
        let main = [&SET_UP[..], &main].concat();
        let mut s = sim_with_handler(&main, &counting_handler());
        s.chip.porta_flags = SWITCH;
        // jmp, the two set-ups, the ldi, sei, the key and the write.
        for _ in 0..7 {
            s.step().unwrap();
        }
        assert_eq!(s.chip.porta_flags, SWITCH);
        assert!(!s.chip.holds_interrupts());
        s.step().unwrap();
        assert_eq!(s.cpu.handling, Some(VECTOR_PORTA_PORT as u8));
    }

    #[test]
    fn sleep_ends_the_ccp_window() {
        let main = [&SET_UP[..], &[SEI, OUT_CCP_R16, SLEEP, SPIN]].concat();
        let mut s = sim_with_handler(&main, &counting_handler());
        s.chip.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN;
        s.chip.porta_flags = SWITCH;
        for _ in 0..6 {
            s.step().unwrap();
        }
        assert!(s.cpu.asleep);
        s.step().unwrap();
        assert!(!s.cpu.asleep, "the pending interrupt woke it at once");
        assert!(s.chip.now < PIT_PERIOD);
    }

    #[test]
    fn an_interrupt_during_the_ccp_window_keeps_its_flag_and_fires_after() {
        // The flag goes up partway through the window, not before it.
        let main = [&SET_UP[..], &[SEI, OUT_CCP_R16, INC_R17, INC_R17, SPIN]].concat();
        let mut s = sim_with_handler(&main, &counting_handler());
        for _ in 0..6 {
            s.step().unwrap();
        }
        s.chip.porta_flags = SWITCH;
        for _ in 0..3 {
            s.step().unwrap();
            assert_eq!(s.cpu.handling, None);
        }
        assert_eq!(s.chip.porta_flags, SWITCH, "pending");
        s.step().unwrap();
        assert_eq!(s.cpu.handling, Some(VECTOR_PORTA_PORT as u8));
    }

    /// `main`'s first access to a TCA0 register, halfway: r18 is 0xFF, then
    /// `first` runs, and the PORTA interrupt lands before `second`.
    fn landing_between(first: [u16; 2], second: [u16; 2], handler: &[u16]) -> Sim {
        let main = [SET_UP[0], first[0], first[1], second[0], second[1], SPIN];
        let mut s = sim_with_handler(&main, handler);
        s.cpu.sreg |= 1 << I;
        for _ in 0..3 {
            s.step().unwrap(); // jmp, ldi and `first`
        }
        s.chip.porta_flags = SWITCH;
        s.step().unwrap();
        assert!(s.cpu.handling.is_some());
        s
    }

    fn landing_mid_write(handler: &[u16]) -> Sim {
        let low = reg::TCA0_CMP0BUF as u16;
        landing_between([STS_R18, low], [STS_R18, low + 1], handler)
    }

    fn landing_mid_read(handler: &[u16]) -> Sim {
        let low = reg::TCA0_CMP0 as u16;
        landing_between([LDS_R19, low], [LDS_R20, low + 1], handler)
    }

    /// Clears PORTA's flags with r18, and that's all.
    fn quiet_handler() -> Vec<u16> {
        vec![STS_R18, PORTA_INTFLAGS as u16, RETI]
    }

    /// A handler landing between the two bytes is exercised, and one that
    /// leaves TCA0 alone doesn't stop the write arriving whole.
    #[test]
    fn a_handler_lands_between_a_low_and_a_high_byte_write() {
        let mut s = landing_mid_write(&quiet_handler());
        assert_eq!(s.chip.temp, Some((reg::TCA0_CMP0BUF, 0xFF)));
        assert_eq!(s.chip.wide(reg::TCA0_CMP0BUF), 0, "not written yet");
        assert_eq!(s.mid_access, 1);
        for _ in 0..4 {
            s.step().unwrap(); // the handler's sts and reti, then the high byte
        }
        assert_eq!(s.cpu.handling, None);
        assert_eq!(s.chip.temp, None);
        assert_eq!(s.chip.wide(reg::TCA0_CMP0BUF), 0xFFFF);
    }

    #[test]
    fn a_handler_lands_between_a_low_and_a_high_byte_read() {
        let mut s = landing_mid_read(&quiet_handler());
        assert_eq!(s.mid_access, 1);
        for _ in 0..4 {
            s.step().unwrap();
        }
        assert_eq!(s.chip.temp, None);
    }

    /// Runs on through the handler to the error it has to end in.
    fn refused(mut s: Sim) -> String {
        (0..4).try_for_each(|_| s.step()).unwrap_err()
    }

    #[test]
    fn a_handler_cant_touch_tca0s_temp_mid_write() {
        let write = |addr: usize| [STS_R18, addr as u16, RETI];
        let read = |addr: usize| [LDS_R19, addr as u16, RETI];
        // Another register's low byte, its high byte, the same register, and
        // a read, whose low byte would overwrite TEMP with its own high byte.
        for (handler, touched) in [
            (write(reg::TCA0_CMP1BUF), "TCA0.CMP1BUF"),
            (write(reg::TCA0_CMP1BUF + 1), "TCA0.CMP1BUF"),
            (write(reg::TCA0_CMP0BUF), "TCA0.CMP0BUF"),
            (write(reg::TCA0_CNT), "TCA0.CNT"),
            (write(reg::TCA0_PERBUF), "TCA0.PERBUF"),
            (write(reg::TCA0_PER), "TCA0.PER"),
            (read(reg::TCA0_CMP1), "TCA0.CMP1"),
            (read(reg::TCA0_CNT), "TCA0.CNT"),
        ] {
            let err = refused(landing_mid_write(&handler));
            let want = format!("touches {touched} with the main loop halfway through TCA0.CMP0BUF");
            assert!(err.contains(&want), "{err}");
            assert!(err.contains("section 8.5.6"), "{err}");
        }
    }

    #[test]
    fn a_handler_cant_touch_tca0s_temp_mid_read() {
        let handler = [STS_R18, reg::TCA0_CNT as u16, RETI];
        let err = refused(landing_mid_read(&handler));
        assert!(err.contains("halfway through TCA0.CMP0"), "{err}");
    }

    #[test]
    fn a_handler_is_free_with_tca0_when_nothing_was_halfway() {
        let low = reg::TCA0_CMP1BUF as u16;
        let handler = [
            STS_R18,
            low,
            STS_R18,
            low + 1,
            STS_R18,
            PORTA_INTFLAGS as u16,
            RETI,
        ];
        let mut s = sim_with_handler(&[SET_UP[0], SEI, NOP, SPIN], &handler);
        s.chip.porta_flags = SWITCH;
        for _ in 0..10 {
            s.step().unwrap();
        }
        assert_eq!(s.chip.wide(reg::TCA0_CMP1BUF), 0xFFFF);
        assert_eq!(s.mid_access, 0);
    }

    #[test]
    fn a_handler_gives_back_what_it_found() {
        let flags = PORTA_INTFLAGS as u16;
        // lds r19, SEEN changes r19; sec changes SREG; push r18 leaves SP down.
        for (body, want) in [
            (vec![LDS_R19, SEEN], "r19 at 0xa5"),
            (vec![0x9408], "SREG at 0x81"),
            (vec![0x932F], "SP at 0x3ffe"),
        ] {
            let handler = [&body[..], &[STS_R18, flags, RETI]].concat();
            let mut s = sim_with_handler(&[SET_UP[0], SEI, NOP, SPIN], &handler);
            s.chip.porta_flags = SWITCH;
            let err = (0..12).try_for_each(|_| s.step()).unwrap_err();
            assert!(err.contains("PORTA_PORT (vector 3)"), "{err}");
            assert!(err.contains(want), "{err}");
        }
    }

    /// ldi r18, 0xFF; sei; then a 16-bit write, which `Landings` has to find
    /// on its own.
    fn sixteen_bit_write(handler: &[u16]) -> Sim {
        let low = reg::TCA0_CMP0BUF as u16;
        let main = [SET_UP[0], SEI, STS_R18, low, STS_R18, low + 1, SPIN];
        let mut s = sim_with_tick_handler(&main, handler);
        s.landings = Some(Landings::new());
        s
    }

    #[test]
    fn landings_get_between_a_low_and_a_high_byte_on_their_own() {
        let mut s = sixteen_bit_write(&quiet_tick());
        for _ in 0..40 {
            s.step().unwrap();
        }
        // Once for the landing that waited for sei, once for its own.
        assert_eq!(s.mid_access, 2);
        assert_eq!(s.chip.wide(reg::TCA0_CMP0BUF), 0xFFFF);

        let handler = [STS_R18, reg::TCA0_CMP1BUF as u16, RETI];
        let mut s = sixteen_bit_write(&handler);
        let err = (0..40).try_for_each(|_| s.step()).unwrap_err();
        assert!(err.contains("section 8.5.6"), "{err}");
    }

    #[test]
    fn landings_visit_each_instruction_once_and_stop_there() {
        let main = [SET_UP[0], SEI, INC_R17, INC_R17, INC_R17, SPIN];
        let mut s = sim_with_tick_handler(&main, &counting_tick());
        s.landings = Some(Landings::new());
        let mut seen = Vec::new();
        for _ in 0..80 {
            let was = s.cpu.handling;
            s.step().unwrap();
            if was.is_none() && s.cpu.handling.is_some() {
                seen.push(s.cpu.r[17]);
            }
        }
        assert_eq!(s.cpu.r[17], 3);
        // The landing from before sei waits for the first `inc`. Then the
        // second `inc`, the third, and the spin, which is reached again and
        // again but only gets the one.
        assert_eq!(seen, [1, 1, 2, 3]);
    }

    #[test]
    fn a_landed_handler_takes_no_time() {
        let mut s = sixteen_bit_write(&quiet_tick());
        for _ in 0..40 {
            s.step().unwrap();
            if s.cpu.handling.is_some() {
                let (now, executed) = (s.chip.now, s.executed);
                s.step().unwrap();
                assert_eq!((s.chip.now, s.executed), (now, executed));
            }
        }
    }

    #[test]
    fn a_tick_that_isnt_one_leaves_gpior0_as_it_was() {
        // Clears its flag and tells the main loop there's been a tick, as
        // `rtc_pit` does.
        let mut s = sixteen_bit_write(&[
            STS_R18,
            reg::RTC_PITINTFLAGS as u16,
            STS_R18,
            reg::GPIO_GPIOR0 as u16,
            RETI,
        ]);
        let mut probes = 0;
        for _ in 0..60 {
            let was = s.cpu.handling;
            s.step().unwrap();
            if was.is_none() && s.cpu.handling == Some(regs::VECTOR_RTC_PIT as u8) {
                probes += 1;
            }
        }
        assert!(probes > 0);
        assert_eq!(s.chip.ticks, 0, "no real tick");
        assert!(!s.chip.pit_flag);
        assert_eq!(s.chip.io[reg::GPIO_GPIOR0], 0, "and no sign of one");
    }

    #[test]
    fn a_real_tick_under_a_probes_flag_stands() {
        let mut s = sim(&[SPIN]);
        s.landings = Some(Landings::new());
        s.landings.as_mut().unwrap().probe = Some(0);
        s.chip.ticks = 1;
        s.chip.io[reg::GPIO_GPIOR0] = 1;
        let landed = Landed {
            r: s.cpu.r,
            sreg: s.cpu.sreg,
            sp: s.cpu.sp,
            gpior0: 0,
            gpior1: 0,
        };
        s.end_probe(regs::VECTOR_RTC_PIT as u8, &landed);
        assert_eq!(s.chip.io[reg::GPIO_GPIOR0], 1);
    }

    #[test]
    fn the_real_tick_lands_just_before_the_sleep_and_further_back_each_time() {
        // Sleeps again whenever it wakes: a loop of cli, nop, sei and sleep,
        // straight and then laid out the way the compiler does it, with the
        // `sei; sleep` before the `cli` and a jump round.
        for (main, words) in [
            (&[SET_UP[0], CLI_WORD, NOP, SEI, SLEEP, 0xCFFB][..], 4),
            (
                &[SET_UP[0], 0xC002, SEI, SLEEP, CLI_WORD, NOP, 0xCFFB][..],
                5,
            ),
        ] {
            let mut s = sim_with_tick_handler(main, &quiet_tick());
            s.chip.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SMODE_IDLE | regs::SLPCTRL_SEN;
            s.landings = Some(Landings::new());
            let jumped = |s: &Sim| s.landings.as_ref().unwrap().jumped;
            // It can't jump until it's seen where the loop is.
            while s.landings.as_ref().unwrap().wait_loop.is_empty() {
                s.step().unwrap();
            }
            assert_eq!(jumped(&s), None);
            let wait_loop = s.landings.as_ref().unwrap().wait_loop.clone();
            assert_eq!(wait_loop.len(), words);
            // Further back each time, then round again, never out of the loop.
            for early in (1..words).chain([1]) {
                let last = jumped(&s);
                while jumped(&s) == last {
                    s.step().unwrap();
                }
                assert_eq!(s.cpu.pc, wait_loop[words - 1 - early]);
                assert!(!s.cpu.asleep);
                assert_eq!(s.chip.now, s.chip.next_pit - 1, "awake, just before");
            }
            assert_eq!(s.ticks_landed, words as u32);
        }
    }

    /// With the CCP window and landings together: nothing lands in the four.
    #[test]
    fn landings_wait_out_the_ccp_window_too() {
        let main = [&SET_UP[..], &[SEI, OUT_CCP_R16, INC_R17, INC_R17, SPIN]].concat();
        let mut s = sim_with_tick_handler(&main, &counting_tick());
        s.landings = Some(Landings::new());
        let mut landed_at = Vec::new();
        for _ in 0..40 {
            let was = s.cpu.handling;
            s.step().unwrap();
            if was.is_none() && s.cpu.handling.is_some() {
                landed_at.push(s.cpu.r[17]);
            }
        }
        assert!(!landed_at.is_empty());
        assert!(landed_at.iter().all(|&incs| incs == 2), "{landed_at:?}");
    }

    /// Polling the PIT flag doesn't move time on, and the loop budget
    /// counts instructions run, so a busy-wait fails long before a tick.
    #[test]
    fn a_busy_wait_on_the_pit_flag_runs_out_of_instructions() {
        // Words from main: 0: lds r16, RTC.PITINTFLAGS; 2: sbrs r16, 0;
        // 3: rjmp 0; 4: rjmp 0.
        let flags = reg::RTC_PITINTFLAGS as u16;
        let mut s = sim(&[0x9100, flags, 0xFF00, 0xCFFC, 0xCFFB]);
        s.chip.io[reg::RTC_PITCTRLA] = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        let err = s.tick(false).unwrap_err();
        assert!(err.contains("ran past"), "{err}");
        assert!(s.chip.now < PIT_PERIOD, "and never reached the tick");
    }

    #[test]
    fn an_input_disabled_or_inverted_switch_is_refused_or_ignored() {
        let pin5 = reg::PORTA_PIN0CTRL + 5;
        let mut c = chip();
        c.io[pin5] = ISC_INPUT_DISABLE;
        c.set_switch(true).unwrap();
        assert_eq!(c.porta_flags, 0, "INPUT_DISABLE raises no flag");

        let mut c = chip();
        c.io[pin5] = regs::PORT_PULLUPEN | PORT_INVEN;
        assert!(c.set_switch(true).unwrap_err().contains("INVEN"));
        assert!(c.read(reg::VPORTA_IN as u16).unwrap_err().contains("INVEN"));
    }

    /// Sets the ADC up the way `hw::init` and `hw::adc_start` do.
    fn adc_setup(c: &mut Chip, channel: Channel) {
        let w = |c: &mut Chip, r: usize, v: u8| c.write(r as u16, v).unwrap();
        w(c, reg::VREF_CTRLB, regs::VREF_ADC0REFEN);
        w(c, reg::ADC0_CALIB, regs::ADC_DUTYCYC_DUTY25);
        w(c, reg::ADC0_CTRLD, regs::ADC_INITDLY_DLY32);
        w(c, reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32US);
        w(c, reg::ADC0_CTRLA, regs::ADC_ENABLE);
        let (vref, refsel, muxpos, sampnum) = match channel {
            Channel::Voltage => (
                regs::VREF_ADC0REFSEL_1V5,
                regs::ADC_REFSEL_VDDREF,
                regs::ADC_MUXPOS_INTREF,
                regs::ADC_SAMPNUM_ACC4,
            ),
            Channel::Temperature => (
                regs::VREF_ADC0REFSEL_1V1,
                regs::ADC_REFSEL_INTREF,
                regs::ADC_MUXPOS_TEMPSENSE,
                regs::ADC_SAMPNUM_ACC4,
            ),
        };
        w(c, reg::VREF_CTRLA, vref);
        w(
            c,
            reg::ADC0_CTRLC,
            regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV8 | refsel,
        );
        w(c, reg::ADC0_MUXPOS, muxpos);
        w(c, reg::ADC0_CTRLB, sampnum);
    }

    /// Starts a conversion, waits it out and reads it, low byte first.
    fn convert(c: &mut Chip) -> Result<u16, String> {
        c.write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV)?;
        c.now += CONVERSION_INSTRUCTIONS;
        let flags = c.read(reg::ADC0_INTFLAGS as u16)?;
        assert_eq!(flags, regs::ADC_RESRDY);
        let lo = c.read(reg::ADC0_RES as u16)?;
        let hi = c.read(reg::ADC0_RES as u16 + 1)?;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    #[test]
    fn the_adc_reads_the_world_once_it_has_settled() {
        let mut c = chip();
        c.world.cell = 305;
        adc_setup(&mut c, Channel::Voltage);
        assert_eq!(convert(&mut c), Ok(UNSETTLED), "first after a change");
        let raw = convert(&mut c).unwrap();
        assert_eq!(ts21::battery::centivolts(raw), 305);

        adc_setup(&mut c, Channel::Temperature);
        assert_eq!(convert(&mut c), Ok(UNSETTLED));
        assert_eq!(
            CALIBRATION.sixteenths(convert(&mut c).unwrap()),
            298 * 16,
            "four conversions' worth, in sixteenths",
        );
    }

    #[test]
    fn the_adc_result_is_only_ready_once_the_conversion_is_done() {
        let mut c = chip();
        adc_setup(&mut c, Channel::Voltage);
        c.write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV).unwrap();
        assert_eq!(c.read(reg::ADC0_INTFLAGS as u16), Ok(0));
        assert!(c.read(reg::ADC0_RES as u16).is_err());
        let err = c
            .write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV)
            .unwrap_err();
        assert!(err.contains("still running"), "{err}");
        let err = c.write(reg::ADC0_MUXPOS as u16, 0).unwrap_err();
        assert!(err.contains("mid-conversion"), "{err}");
        c.now += CONVERSION_INSTRUCTIONS;
        assert_eq!(c.read(reg::ADC0_INTFLAGS as u16), Ok(regs::ADC_RESRDY));
        // Reading the result clears RESRDY.
        c.read(reg::ADC0_RES as u16).unwrap();
        c.read(reg::ADC0_RES as u16 + 1).unwrap();
        assert_eq!(c.read(reg::ADC0_INTFLAGS as u16), Ok(0));
    }

    #[test]
    fn clearing_resrdy_mid_conversion_leaves_the_conversion_running() {
        let mut c = chip();
        adc_setup(&mut c, Channel::Voltage);
        c.write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV).unwrap();
        c.write(reg::ADC0_INTFLAGS as u16, regs::ADC_RESRDY)
            .unwrap();
        c.now += CONVERSION_INSTRUCTIONS;
        assert_eq!(c.read(reg::ADC0_INTFLAGS as u16), Ok(regs::ADC_RESRDY));
        // Once it's done, clearing the flag throws the result away.
        c.write(reg::ADC0_INTFLAGS as u16, regs::ADC_RESRDY)
            .unwrap();
        assert_eq!(c.read(reg::ADC0_INTFLAGS as u16), Ok(0));
        assert!(c.read(reg::ADC0_RES as u16).is_err());
    }

    #[test]
    fn the_reference_and_duty_cycle_stay_put_mid_conversion() {
        for (register, value) in [(reg::VREF_CTRLB, 0), (reg::ADC0_CALIB, 0)] {
            let mut c = chip();
            adc_setup(&mut c, Channel::Voltage);
            c.write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV).unwrap();
            let err = c.write(register as u16, value).unwrap_err();
            assert!(
                err.contains(&format!("{} mid-conversion", name(register))),
                "{err}"
            );
        }
    }

    #[test]
    fn the_adc_result_is_read_low_byte_first() {
        let mut c = chip();
        adc_setup(&mut c, Channel::Voltage);
        convert(&mut c).unwrap();
        c.write(reg::ADC0_COMMAND as u16, regs::ADC_STCONV).unwrap();
        c.now += CONVERSION_INSTRUCTIONS;
        let err = c.read(reg::ADC0_RES as u16 + 1).unwrap_err();
        assert!(err.contains("low byte"), "{err}");
    }

    #[test]
    fn the_adc_refuses_setups_that_measure_neither() {
        let cases: [(&str, usize, u8); 7] = [
            (
                "wrong reference",
                reg::VREF_CTRLA,
                regs::VREF_ADC0REFSEL_1V1,
            ),
            ("one conversion", reg::ADC0_CTRLB, regs::ADC_SAMPNUM_ACC1),
            ("against INTREF", reg::ADC0_CTRLC, regs::ADC_PRESC_DIV8),
            ("ADC too fast", reg::ADC0_CTRLC, regs::ADC_REFSEL_VDDREF),
            ("ADC off", reg::ADC0_CTRLA, 0),
            ("reference left to start", reg::VREF_CTRLB, 0),
            ("50% duty", reg::ADC0_CALIB, 0),
        ];
        for (what, register, value) in cases {
            let mut c = chip();
            adc_setup(&mut c, Channel::Voltage);
            c.write(register as u16, value).unwrap();
            assert!(convert(&mut c).is_err(), "{what}");
        }
        // `temp::sixteenths` wants a sum of four, not one conversion.
        let mut c = chip();
        adc_setup(&mut c, Channel::Temperature);
        c.write(reg::ADC0_CTRLB as u16, regs::ADC_SAMPNUM_ACC1)
            .unwrap();
        assert!(convert(&mut c).is_err(), "one temperature conversion");
        // The temperature sensor needs its delays and SAMPCAP.
        for (register, value) in [
            (reg::ADC0_CTRLD, 0),
            (reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32US - 1),
            (
                reg::ADC0_CTRLC,
                regs::ADC_PRESC_DIV8 | regs::ADC_REFSEL_INTREF,
            ),
        ] {
            let mut c = chip();
            adc_setup(&mut c, Channel::Temperature);
            c.write(register as u16, value).unwrap();
            let err = convert(&mut c).unwrap_err();
            assert!(err.contains("temperature sensor"), "{err}");
        }
    }

    #[test]
    fn a_stuck_adc_reads_its_number_on_both_channels() {
        let mut c = chip();
        c.world.stuck = Some(7);
        for channel in [Channel::Voltage, Channel::Temperature] {
            adc_setup(&mut c, channel);
            convert(&mut c).unwrap();
            assert_eq!(convert(&mut c), Ok(7));
        }
    }

    #[test]
    fn the_signature_row_has_the_calibration() {
        let mut c = chip();
        let gain = c.read(reg::SIGROW_TEMPSENSE0 as u16).unwrap();
        let offset = c.read(reg::SIGROW_TEMPSENSE1 as u16).unwrap() as i8;
        assert_eq!(ts21::temp::Calibration { offset, gain }, CALIBRATION);
        assert!(c.write(reg::SIGROW_TEMPSENSE0 as u16, 0).is_err());
    }

    #[test]
    fn the_fuses_are_the_worlds_and_read_only() {
        let mut c = chip();
        let osccfg = reg::FUSE_OSCCFG as u16;
        assert_eq!(c.read(osccfg), Ok(0x02), "20 MHz from the factory");
        c.world.fuses[2] = 0x81;
        assert_eq!(c.read(osccfg), Ok(0x81));
        for addr in [reg::FUSE_WDTCFG, reg::FUSE_SYSCFG0, reg::FUSE_BOOTEND] {
            let err = c.write(addr as u16, 0).unwrap_err();
            assert!(err.contains("never writes the fuses"), "{err}");
            assert!(err.contains("FUSE."), "{err}");
        }
        // The lock byte too, which reads as unmodelled.
        let lockbit = map::FUSES + 10;
        assert!(c.write(lockbit, 0).unwrap_err().contains("never writes"));
        assert!(c.read(lockbit).unwrap_err().contains("doesn't model"));
    }

    /// Starts a scan the way `hw::flash_ok` does, with three instructions'
    /// wait, and polls it to the end. Returns STATUS.
    fn scan(c: &mut Chip) -> Result<u8, String> {
        c.write(reg::CRCSCAN_CTRLB as u16, regs::CRCSCAN_SRC_FLASH)?;
        c.write(reg::CRCSCAN_CTRLA as u16, regs::CRCSCAN_ENABLE)?;
        c.now += CRC_START_INSTRUCTIONS + 1;
        loop {
            let status = c.read(reg::CRCSCAN_STATUS as u16)?;
            if status & regs::CRCSCAN_BUSY == 0 {
                return Ok(status);
            }
            c.now += 1;
        }
    }

    #[test]
    fn a_signed_image_passes_the_crc_scan() {
        let mut c = Chip::new(crc::sign(&[0x0C, 0x94, 0x3E, 0x00]).unwrap());
        assert_eq!(scan(&mut c), Ok(regs::CRCSCAN_OK));
    }

    #[test]
    fn a_corrupted_flash_byte_fails_the_crc_scan() {
        let signed = crc::sign(&[0x0C, 0x94, 0x3E, 0x00]).unwrap();
        for at in [1, 3000, crc::CHECKSUM_AT - 1, FLASH_SIZE - 1] {
            let mut flash = signed.clone();
            flash[at] ^= 0x01;
            let mut c = Chip::new(flash);
            assert_eq!(scan(&mut c), Ok(0), "{at}");
        }
        // Unsigned, the end of flash is erased and the sum's wrong.
        assert_eq!(scan(&mut chip()), Ok(0));
    }

    #[test]
    fn the_crc_scan_reads_stale_until_it_starts_then_busy() {
        let mut flash = crc::sign(&[]).unwrap();
        flash[0] = 0;
        let mut c = Chip::new(flash);
        let status = reg::CRCSCAN_STATUS as u16;
        assert_eq!(c.read(status), Ok(regs::CRCSCAN_OK), "OK from reset");
        c.now = 100;
        c.write(reg::CRCSCAN_CTRLA as u16, regs::CRCSCAN_ENABLE)
            .unwrap();
        // A firmware that reads straight away sees the reset value, and
        // would take a failed scan for a pass.
        for now in 101..=103 {
            c.now = now;
            assert_eq!(c.read(status), Ok(regs::CRCSCAN_OK), "{now}");
        }
        c.now = 104;
        assert_eq!(c.read(status), Ok(regs::CRCSCAN_BUSY));
        assert_eq!(c.read(status), Ok(regs::CRCSCAN_BUSY));
        assert_eq!(c.read(status), Ok(0), "failed");
    }

    #[test]
    fn the_crc_scan_ends_on_its_own_without_a_poll() {
        let mut c = Chip::new(crc::sign(&[]).unwrap());
        c.write(reg::CRCSCAN_CTRLA as u16, regs::CRCSCAN_ENABLE)
            .unwrap();
        c.now += 1000;
        assert_eq!(c.write(reg::CRCSCAN_CTRLB as u16, 0), Ok(()));
        assert_eq!(c.read(reg::CRCSCAN_STATUS as u16), Ok(regs::CRCSCAN_OK));
    }

    #[test]
    fn the_crc_scan_refuses_what_it_doesnt_model() {
        let mut c = chip();
        let ctrla = reg::CRCSCAN_CTRLA as u16;
        assert!(c.write(ctrla, 0x02).unwrap_err().contains("NMIEN"));
        assert!(c.write(ctrla, 0x80).is_err(), "RESET");
        c.write(reg::CRCSCAN_CTRLB as u16, 0x01).unwrap();
        assert!(
            c.write(ctrla, regs::CRCSCAN_ENABLE)
                .unwrap_err()
                .contains("whole-flash")
        );
        c.write(reg::CRCSCAN_CTRLB as u16, 0).unwrap();
        c.write(ctrla, regs::CRCSCAN_ENABLE).unwrap();
        assert!(
            c.write(ctrla, regs::CRCSCAN_ENABLE)
                .unwrap_err()
                .contains("still running")
        );
        assert!(
            c.write(reg::CRCSCAN_CTRLB as u16, 0)
                .unwrap_err()
                .contains("mid-scan")
        );
    }

    #[test]
    fn reads_the_switch_active_low() {
        let mut c = chip();
        let pin = reg::VPORTA_IN as u16;
        assert_eq!(c.read(pin).unwrap() & SWITCH, SWITCH);
        c.pressed = true;
        assert_eq!(c.read(pin).unwrap() & SWITCH, 0);
    }

    #[test]
    fn refuses_registers_it_doesnt_model() {
        let mut c = chip();
        assert!(
            c.read(0x0800).unwrap_err().contains("doesn't model"),
            "USART0"
        );
        assert!(
            c.write(reg::VPORTA_IN as u16, 0).is_err(),
            "VPORTA.IN is read-only here"
        );
        assert!(c.write(MAPPED, 0).is_err(), "flash isn't writable");
        assert_eq!(c.read(MAPPED + 100), Ok(0xFF), "erased flash");
        assert_eq!(c.read(MAPPED + 2), Ok(0));
    }

    /// A chip at `level` with the pins on TCA0, just past a BOTTOM.
    fn pins_at(level: u8) -> Chip {
        let mut c = pwm_at(level);
        c.io[reg::TCA0_CTRLB] = regs::TCA_CMP0EN | regs::TCA_CMP1EN;
        c.io[reg::VPORTB_DIR] = FET | LINEAR;
        let bottom = c.next_bottom;
        c.advance_to(bottom).unwrap();
        c
    }

    #[test]
    fn a_waveform_output_is_set_at_bottom_and_cleared_on_a_match() {
        let mut c = pins_at(ramp::LINEAR_MAX - 1);
        let linear = u64::from(ramp::output(ramp::LINEAR_MAX - 1).linear);
        assert_eq!(c.wo, [false, true], "the FET's compare is 0");
        let start = c.now;
        c.advance_to(start + linear - 1).unwrap();
        assert!(c.wo[1]);
        c.advance_to(start + linear).unwrap();
        assert!(!c.wo[1], "on the match");
        let bottom = c.next_bottom;
        c.advance_to(bottom).unwrap();
        assert!(c.wo[1], "and again at BOTTOM");
    }

    #[test]
    fn a_compare_over_top_holds_a_waveform_output_high() {
        let mut c = pins_at(ramp::LEVELS);
        let top = u64::from(ramp::output(ramp::LEVELS).top);
        let start = c.now;
        for at in 1..2 * top {
            c.advance_to(start + at).unwrap();
            assert!(c.wo[1], "{at}");
        }
    }

    #[test]
    fn a_stopped_timer_holds_its_output_whatever_the_compare_says() {
        let mut c = pins_at(ramp::LINEAR_MAX - 1);
        c.world.tca_stopped = true;
        let top = ramp::output(ramp::LINEAR_MAX - 1).top;
        c.compare = [0, 0];
        c.advance_to(c.now + 10_000).unwrap();
        assert_eq!(c.leds(), (top, 0), "fully on, and the compare's 0");
        c.io[reg::TCA0_CTRLB] = regs::TCA_WGMODE_SINGLESLOPE;
        assert_eq!(c.leds(), (0, 0), "and PORT only gets it back with CMPnEN");
    }

    #[test]
    fn writing_a_compare_past_the_count_leaves_the_output_until_bottom() {
        let mut c = pins_at(ramp::LINEAR_MAX - 1);
        c.advance_to(c.now + 1).unwrap();
        c.compare[1] = 0;
        let top = ramp::output(ramp::LINEAR_MAX - 1).top;
        assert_eq!(c.leds(), (top, 0));
        let bottom = c.next_bottom;
        c.advance_to(bottom).unwrap();
        assert_eq!(c.leds(), (0, 0));
    }

    #[test]
    fn leds_need_the_timer_the_channel_and_the_pin() {
        let mut c = chip();
        c.compare = [200, 100];
        assert_eq!(c.leds(), (0, 0));
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        c.io[reg::TCA0_CTRLB] = regs::TCA_CMP0EN | regs::TCA_CMP1EN;
        assert_eq!(c.leds(), (0, 0), "pins still inputs");
        c.io[reg::VPORTB_DIR] = LINEAR;
        assert_eq!(c.leds(), (100, 0));
        c.io[reg::VPORTB_DIR] = LINEAR | FET;
        assert_eq!(c.leds(), (100, 200));
    }

    /// The `ldi`s rt.rs uses, then `call main`.
    fn startup_code(l: &Layout) -> Vec<u8> {
        let mut words = vec![0x2411, 0xBE1F]; // clr r1; out SREG, r1
        for (r, v) in [
            (26, l.data.start),
            (30, l.load),
            (24, l.data.end),
            (26, l.bss.start),
            (24, l.bss.end),
        ] {
            let [lo, hi] = v.to_le_bytes();
            words.extend([ldi(r, lo), ldi(r + 1, hi)]);
        }
        words.extend([0x940E, 0x0100]);
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn layout(start: u16, load: u16) -> Layout {
        Layout {
            data: start..start + 2,
            bss: start + 2..start + 4,
            load,
        }
    }

    #[test]
    fn patches_the_startup_addresses() {
        let (linked, moved) = (layout(0x3800, 0x8252), layout(0x3811, 0x8300));
        let mut image = startup_code(&linked);
        patch_startup(&mut image, 0, &linked, &moved).unwrap();
        assert_eq!(image, startup_code(&moved));
    }

    #[test]
    fn refuses_to_patch_startup_code_it_doesnt_recognise() {
        let linked = layout(0x3800, 0x8252);
        let mut image = startup_code(&linked);
        let other = layout(0x3802, 0x8252);
        assert!(patch_startup(&mut image, 0, &other, &linked).is_err());
        image.truncate(image.len() - 4);
        assert!(
            patch_startup(&mut image, 0, &linked, &linked).is_err(),
            "no call"
        );
    }
}
