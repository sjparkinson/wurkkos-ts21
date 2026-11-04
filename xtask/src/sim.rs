//! A fake ATtiny1616 around `avr.rs`'s CPU, and the checks that run the
//! real firmware on it. `build()` in main.rs runs them after `layout.rs`'s,
//! so `size`, `hex` and `flash` all refuse firmware that fails, and CI runs
//! them through `size`.
//!
//! The fake chip models only the registers the firmware touches, and turns
//! the rules the real chip enforces quietly into errors:
//!
//! - A protected register only takes a write within four instructions of
//!   the CCP key. The real chip ignores a late write.
//! - TCA0's 16-bit registers are written low byte, then high byte, through
//!   the shared TEMP byte. The other way round, the real chip pairs the high
//!   byte with a stale low one.
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
//!   and nothing may change it after that, off included.
//! - ADC0 only converts when it's set up for one of the two measurements the
//!   firmware makes (the 1.5 V reference against VDD, four accumulated, or
//!   the temperature sensor against 1.1 V with the datasheet's delays and
//!   clock), and then reads the scripted world (`world.rs`). Starting a
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
//!   could wake it. `avr.rs` adds that SREG.I is set and no handler's
//!   running. Power-down also wants nothing left that it would freeze or
//!   keep burning: no conversion running (§30.3.5 resumes it on waking, with
//!   a result that's invalid), the ADC off, the reference not forced on,
//!   TCA0's channels off the pins with both compares at 0, and PA5 able to
//!   wake it.
//! - TCA0's CMP0 and CMP1 pick up their BUF registers a PWM cycle after a
//!   write, standing in for the next BOTTOM. The datasheet doesn't say
//!   whether writing CNT counts as one, so this guesses that it doesn't and
//!   pushes BOTTOM a full cycle away. The real wait can be far shorter, so
//!   the firmware mustn't lean on a pass that writes the output staying in
//!   idle: `Torch::tick` rests in that same pass.
//! - CRCSCAN runs CRC-16-CCITT over all of flash and sets STATUS.OK if it
//!   comes to 0, which is what a correct checksum in the last two bytes
//!   does (`crc.rs`). The firmware runs on the image `cargo xtask hex`
//!   writes, so this checks xtask's sum too. STATUS reads as it was for the
//!   three instructions after ENABLE, since the scan only starts three
//!   cycles later, then BUSY for a couple of reads. Only the whole-flash
//!   scan with STATUS polled is modelled; NMIEN is an error.
//!
//! Interrupts only ever arrive while the CPU's asleep, since that's where
//! the PIT's period ends and between ticks is where the switch changes. So
//! nothing here tests a handler landing mid-instruction-sequence, such as
//! halfway through a 16-bit TCA0 write.
//!
//! Touching a register it doesn't model is an error too, so new peripheral
//! code fails here until the model learns about it.
//!
//! It can also break the chip for src/rt.rs's register and RAM tests to
//! find: a stuck bit in a CPU register, forced after every instruction, or
//! a byte of SRAM with a stuck bit, two shorted bits, or an address that
//! reaches another byte (`world::RamFault`). The stack checks wait until
//! those tests are done, since they point SP all over SRAM.
//!
//! With that in place it runs five things:
//!
//! 1. Reset to `main`, with SRAM full of junk, checking the self-tests pass
//!    and .data and .bss come out right.
//! 2. The same startup code with .data and .bss moved and given contents,
//!    because the firmware has no statics yet, so as linked both loops in
//!    src/rt.rs run zero times.
//! 3. Power-on with every stuck register bit and a spread of RAM faults
//!    (`startup_faults`), checking each one stops in `__selftest_failed`
//!    with the light dark.
//! 4. Every example in docs/ui.md (see `spec.rs`), each on a fresh chip,
//!    with the cell, temperature, broken ADC, fuses, corrupt flash or
//!    broken RAM or register the example scripts.
//! 5. Twenty thousand ticks of random clicks and holds, with a good cell at
//!    room temperature.
//!
//! `probe` separately runs src/bin/link_probe.rs, which has real statics,
//! and checks it reads them from where link.x put them.
//!
//! Both drive `ts21::torch` alongside the firmware and check the LEDs show
//! the library's level on every tick; the examples then check where the
//! library ended up.
//!
//! Throughout, it measures how deep the stack goes and checks every pass of
//! the main loop fits in a 64 Hz tick.

use std::iter;
use std::ops::Range;

use ts21::ramp;
use ts21::sensors::Channel;

use crate::avr::{Bus, Cpu, I, VECTOR_WORDS, is_two_words};
use crate::crc;
use crate::elf::{DATA_SPACE, Elf, RAM};
use crate::simulate::{self, Sample};
use crate::spec::{self, Example};
use crate::switching::RandomSwitching;
use crate::updi::map;
use crate::world::{self, CALIBRATION, RamFault, Register, RegisterFault, Rig, World};

// The fake chip uses the addresses and the fields it models, not all of them.
// atdf.rs loads this file again for its tests on purpose: there, an unused
// constant is a warning, which is how an unchecked one gets caught.
// world.rs reads the fuse addresses from here too.
#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../../src/regs.rs"]
pub(crate) mod regs;

use regs::addr as reg;

/// SRAM as data-space addresses.
const SRAM: Range<u16> = (RAM.start - DATA_SPACE) as u16..(RAM.end - DATA_SPACE) as u16;
/// Where flash appears in data space, and how much of it there is. updi.rs
/// checks both against the ATDF.
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
/// CPU cycles in a 64 Hz tick at 10 MHz.
const TICK_CYCLES: u64 = 10_000_000 / 64;
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
/// WDT.STATUS.LOCK. Only the simulator needs it (to refuse it), so it isn't
/// in regs.rs, where an unused constant fails the firmware clippy. atdf.rs
/// checks it.
pub(crate) const WDT_LOCK: u8 = 0x80;
/// WDT.CTRLA.PERIOD's mask, and its largest value, 8KCLK. atdf.rs checks both.
pub(crate) const WDT_PERIOD: u8 = 0x0F;
pub(crate) const WDT_PERIOD_MAX: u8 = 11;
/// WDT.CTRLA.WINDOW's mask. It takes the same values as PERIOD, shifted up
/// four, which atdf.rs checks too.
pub(crate) const WDT_WINDOW: u8 = 0xF0;
/// WDT clocks in a tick: the PIT divides the same oscillator by 512, and
/// the WDT by 32. Only at CYC512, so a tick refuses any other PIT period
/// while the WDT's on.
const WDT_CLOCKS_PER_TICK: u32 = 16;
// The WDT model counts whole ticks, which only works while a pass can't
// overrun one and drop a tick unseen.
const _: () = assert!(TICK_BUDGET < TICK_CYCLES / 2);

/// PORTx.PINnCTRL.ISC, the mask and its values (DS40002204A §16.5.11),
/// beyond the BOTHEDGES the firmware uses. atdf.rs checks them.
pub const ISC: u8 = 0x07;
pub const ISC_INTDISABLE: u8 = 0x0;
pub const ISC_BOTHEDGES: u8 = regs::PORT_ISC_BOTHEDGES;
pub const ISC_RISING: u8 = 0x2;
pub const ISC_FALLING: u8 = 0x3;
pub const ISC_INPUT_DISABLE: u8 = 0x4;
pub const ISC_LEVEL: u8 = 0x5;
/// Instructions from a write to a CMPnBUF to the BOTTOM that applies it: a
/// dual-slope cycle is two PERs of CPU clocks, and no instruction is quicker
/// than a clock, so it's never less than this on the real chip's time.
const PWM_CYCLE: u64 = 2 * ramp::PWM_TOP as u64;
/// PORTx.PINnCTRL.INVEN, which the simulator refuses on PA5.
pub const PORT_INVEN: u8 = 0x80;
/// PORTB's and PORTC's pin-change vectors, which the firmware has no
/// handlers for. atdf.rs checks them.
pub const VECTOR_PORTB_PORT: usize = 4;
pub const VECTOR_PORTC_PORT: usize = 5;

/// TCA0's 16-bit registers the firmware writes, by the low byte's address.
const WIDE: [usize; 4] = [
    reg::TCA0_CNT,
    reg::TCA0_PERBUF,
    reg::TCA0_CMP0BUF,
    reg::TCA0_CMP1BUF,
];

pub struct Report {
    /// The most stack the firmware used, in bytes.
    pub stack: u16,
    /// The most instructions one pass of the main loop took.
    pub loop_instructions: u64,
    /// What that's measured against.
    pub loop_budget: u64,
}

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

    // A chip that's just had its cell put in, in `world`, run to the end of
    // startup.
    let power_on = |world: &World| -> Result<(Sim, Boot), String> {
        let mut sim =
            Sim::new(signed.clone(), linked.bss.end, unhandled)?.testing_until(ram_tested);
        sim.chip.world = *world;
        let boot = sim.boot(main, failed)?;
        Ok((sim, boot))
    };

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

    let mut report = Report {
        stack: 0,
        loop_instructions: 0,
        loop_budget: TICK_BUDGET,
    };
    for example in spec::examples(spec::UI_MD)? {
        let samples = simulate::samples(&example.actions);
        let world = samples.first().map_or(World::NORMAL, |s| s.world);
        let (sim, boot) = power_on(&world).map_err(|e| format!("{}: startup: {e}", example.row))?;
        ui_example(sim, boot, samples, &example, &mut report)
            .map_err(|e| format!("{}: {e}", example.row))?;
    }
    against_the_library(sim, &mut report)
        .map_err(|e| format!("random switching, against the library: {e}"))?;
    Ok(report)
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

/// Checks a chip that's stopped in `__selftest_failed` stays there, with
/// the FET and linear pins driven low and everything else off.
/// It presses and releases the switch a few times on the way, since
/// "nothing, ever" includes clicks.
fn stays_dark(sim: &mut Sim) -> Result<(), String> {
    let failed = sim.cpu.pc;
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
/// - every pair of bits in a byte shorted together, which only shows up
///   with the backgrounds after 0x00, since 0x00 and 0xFF write both bits
///   the same.
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
        (0..bits).flat_map(move |bit| [false, true].map(|high| RegisterFault { reg, bit, high }))
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
    sim.chip.world = power_on;
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
    sim.tick(false).map_err(|e| format!("hw::init: {e}"))?;
    if !sim.chip.watchdog_windowed() {
        return Err("hw::init never enabled the watchdog in window mode".into());
    }
    if !sim.chip.button_led_lit() {
        return Err("hw::init left PB5 without its pull-up, so the button LED is out".into());
    }
    let mut rig = Rig::new(&power_on);
    for (tick, sample) in samples.enumerate() {
        sim.chip.world = sample.world;
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
        rig.tick(sample.pressed, &sample.world);
        let output = ramp::output(rig.level());
        let want = (output.linear.into(), output.fet.into());
        let leds = sim.chip.leds();
        if leds != want {
            return Err(format!(
                "tick {tick}: the firmware's (linear, FET) duty is {leds:?}, \
                 but the library's level {} is {want:?}",
                rig.level()
            ));
        }
    }
    report.stack = report.stack.max(sim.stack());
    Ok(rig.level())
}

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
        })
    }

    /// Holds the stack checks off until the CPU reaches `ram_tested`, a
    /// byte address, as startup does with the real src/rt.rs.
    fn testing_until(mut self, ram_tested: u32) -> Self {
        self.testing = Some(ram_tested / 2);
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
        self.cpu.step(&mut self.chip)?;
        if awake {
            self.executed += 1;
            self.chip.elapse()?;
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
        Ok(())
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
    /// firmware picks up the next tick. Returns how many instructions it took.
    fn tick(&mut self, pressed: bool) -> Result<u64, String> {
        self.chip.set_switch(pressed)?;
        let (start, ticks) = (self.executed, self.chip.ticks);
        while self.chip.ticks == ticks {
            if self.executed - start > TICK_BUDGET {
                return Err(format!(
                    "a pass of the main loop ran past {TICK_BUDGET} instructions, \
                     so it hangs or misses 64 Hz ticks"
                ));
            }
            self.step()?;
        }
        if let Some((low, _)) = self.chip.temp {
            return Err(format!("left {} half-written", name(low)));
        }
        Ok(self.executed - start)
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
    pressed: bool,
    pit_busy: u8,
    /// RTC.PITINTFLAGS.PI.
    pit_flag: bool,
    /// PORTA.INTFLAGS.
    porta_flags: u8,
    /// When the PIT's current period ends.
    next_pit: u64,
    /// PIT periods so far with the PIT on.
    ticks: u32,
    /// TCA0's 16-bit registers, in `WIDE`'s order.
    wide: [u16; 4],
    /// TCA0's TEMP byte, and which register's low byte it holds.
    temp: Option<(usize, u8)>,
    /// TCA0.CMP0 and CMP1, the live values.
    compare: [u16; 2],
    /// When the BUF registers next reach `compare`, if they've changed.
    compare_update: Option<u64>,
    /// The high byte of a CMPn, latched into TEMP by reading its low byte,
    /// and which register that was.
    compare_temp: Option<(usize, u8)>,
    /// Times `sleep` has run in power-down.
    power_downs: u64,
    /// What the ADC measures.
    world: World,
    adc: Adc,
    wdt: Wdt,
    crc: CrcScan,
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
/// conversions with the delays `hw::init` sets are about 2,600 CPU cycles,
/// so this is roughly that, and well inside a tick, so a firmware that reads
/// the result straight after starting a conversion finds it isn't ready.
const CONVERSION_INSTRUCTIONS: u64 = 2_640;

/// What the first conversion after a change of reference or input reads.
/// The datasheet doesn't promise it's wrong, but Anduril finds it unstable,
/// so the model makes it as wrong as it gets. The firmware throws it away;
/// if it ever stopped, the garbage would cap it at level 75, which the
/// in-step check only catches once the UI goes above that.
const UNSETTLED: u16 = 0;

/// CPU cycles a second, from `hw::init`'s clock setup.
const CLK_PER: u32 = 10_000_000;

impl Chip {
    fn new(flash: Vec<u8>) -> Self {
        Self {
            flash,
            sram: vec![JUNK; usize::from(SRAM.end - SRAM.start)],
            io: vec![0; 0x1000],
            now: 0,
            unlocked_until: None,
            pressed: false,
            pit_busy: PIT_SYNC_READS,
            pit_flag: false,
            porta_flags: 0,
            next_pit: PIT_PERIOD,
            ticks: 0,
            wide: [0; 4],
            temp: None,
            compare: [0; 2],
            compare_update: None,
            compare_temp: None,
            power_downs: 0,
            world: World::NORMAL,
            adc: Adc::default(),
            wdt: Wdt::default(),
            crc: CrcScan {
                started: None,
                busy_reads: 0,
                status: regs::CRCSCAN_OK,
            },
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
        let running = self.io[reg::TCA0_CTRLA] & regs::TCA_ENABLE != 0;
        if running && self.compare_update.is_some_and(|at| self.now >= at) {
            self.compare_update = None;
            self.compare = [self.wide(reg::TCA0_CMP0BUF), self.wide(reg::TCA0_CMP1BUF)];
        }
        while self.now >= self.next_pit {
            self.pit_period()?;
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
        if self.pit_busy > 0 {
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
        let isc = self.io[reg::PORTA_PIN0CTRL + 5] & ISC;
        if isc != ISC_BOTHEDGES && isc != ISC_LEVEL {
            return Err(format!(
                "powers down with PA5's ISC at {isc:#x}: it's a partially asynchronous pin, \
                 so only BOTHEDGES or LEVEL wakes the chip (table 16-3)"
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
        let port = |base: usize, vector: usize| {
            let on = (base..base + 8).any(|a| {
                matches!(
                    self.io[a] & ISC,
                    ISC_BOTHEDGES | ISC_RISING | ISC_FALLING | ISC_LEVEL
                )
            });
            u32::from(on) << vector
        };
        let pit = self.io[reg::RTC_PITINTCTRL] & regs::RTC_PI != 0;
        port(reg::PORTA_PIN0CTRL, regs::VECTOR_PORTA_PORT)
            | port(reg::PORTB_PIN0CTRL, VECTOR_PORTB_PORT)
            | port(reg::PORTC_PIN0CTRL, VECTOR_PORTC_PORT)
            | u32::from(pit) << regs::VECTOR_RTC_PIT
    }

    /// Jumps to the end of the PIT's period, as if the CPU had waited for
    /// it. The PIT runs in every sleep mode (DS40002204A §23.8), and so does
    /// the ADC in idle, so a conversion that was running has finished.
    fn wait_for_pit(&mut self) -> Result<(), String> {
        if self.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN == 0 {
            return Err("waits for the PIT with the PIT off, so it would wait forever".into());
        }
        self.advance_to(self.next_pit)
    }

    /// The PIT's period ends: its flag goes up whether or not the
    /// interrupt's enabled, and the watchdog counts another 16 clocks.
    fn pit_period(&mut self) -> Result<(), String> {
        self.next_pit += PIT_PERIOD;
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
        if clocks >= timeout {
            return Err(format!(
                "the watchdog went {ticks} ticks ({clocks} WDT clocks) without \
                 wdr, against a {timeout}-clock timeout, so the chip would \
                 have reset"
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
        let clk_adc = CLK_PER >> (1 + u32::from(ctrlc & 0x07));
        if !(200_000..=1_500_000).contains(&clk_adc) {
            return Err(format!(
                "runs the ADC at {clk_adc} Hz, outside 200 kHz to 1.5 MHz (table 36-26)"
            ));
        }
        let vref = io(reg::VREF_CTRLA) & 0x70;
        let refsel = ctrlc & 0x30;
        let muxpos = io(reg::ADC0_MUXPOS);
        let sampnum = io(reg::ADC0_CTRLB) & 0x07;
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
                regs::ADC_SAMPNUM_ACC1,
            ) => {
                // Section 30.3.2.6: SAMPCAP, and INITDLY and SAMPLEN each at
                // least 32 µs of ADC clock.
                let initdly = match io(reg::ADC0_CTRLD) >> 5 {
                    0 => 0,
                    n => 8u32 << n,
                };
                let samplen = u32::from(io(reg::ADC0_SAMPCTRL) & 0x1F);
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

    fn sram(&self, range: &Range<u16>) -> &[u8] {
        &self.sram[usize::from(range.start - SRAM.start)..usize::from(range.end - SRAM.start)]
    }

    fn wide(&self, addr: usize) -> u16 {
        WIDE.iter()
            .position(|&a| a == addr)
            .map_or(0, |i| self.wide[i])
    }

    /// (linear, FET) duty as the pins see it: TCA0 running, the compare
    /// channel enabled and the pin set as an output.
    fn leds(&self) -> (u16, u16) {
        let running = self.io[reg::TCA0_CTRLA] & regs::TCA_ENABLE != 0;
        let channel = |enable: u8, pin: u8, cmp: usize| {
            let on = running
                && self.io[reg::TCA0_CTRLB] & enable != 0
                && self.io[reg::VPORTB_DIR] & pin != 0;
            if on { self.wide(cmp) } else { 0 }
        };
        (
            channel(regs::TCA_CMP1EN, LINEAR, reg::TCA0_CMP1BUF),
            channel(regs::TCA_CMP0EN, FET, reg::TCA0_CMP0BUF),
        )
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
                | reg::TCA0_CTRLA
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
        regs::VECTOR_PORTA_PORT => "PORTA_PORT (vector 3)".into(),
        VECTOR_PORTB_PORT => "PORTB_PORT (vector 4)".into(),
        VECTOR_PORTC_PORT => "PORTC_PORT (vector 5)".into(),
        regs::VECTOR_RTC_PIT => "RTC_PIT (vector 7)".into(),
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
        (reg::TCA0_CNT, "TCA0.CNT"),
        (reg::TCA0_CMP0, "TCA0.CMP0"),
        (reg::TCA0_CMP1, "TCA0.CMP1"),
        (reg::TCA0_PERBUF, "TCA0.PERBUF"),
        (reg::TCA0_CMP0BUF, "TCA0.CMP0BUF"),
        (reg::TCA0_CMP1BUF, "TCA0.CMP1BUF"),
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
        (reg::SLPCTRL_CTRLA, "SLPCTRL.CTRLA"),
        (reg::RTC_PITINTCTRL, "RTC.PITINTCTRL"),
        (reg::PORTA_INTFLAGS, "PORTA.INTFLAGS"),
        (reg::CRCSCAN_CTRLA, "CRCSCAN.CTRLA"),
        (reg::CRCSCAN_CTRLB, "CRCSCAN.CTRLB"),
        (reg::CRCSCAN_STATUS, "CRCSCAN.STATUS"),
    ];
    known
        .iter()
        .find(|&&(a, _)| a == addr)
        .map_or_else(|| format!("{addr:#06x}"), |(_, n)| (*n).to_string())
}

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        word(&self.flash, pc as usize * 2)
    }

    /// PORTA's pin change and the PIT, lowest vector first, the default
    /// static priority (DS40002204A §13.3.2.4). A port's flags only go up
    /// for pins whose ISC enables them, so any flag is a request.
    fn interrupt(&self) -> Option<u8> {
        let pending = u32::from(self.porta_flags != 0) << regs::VECTOR_PORTA_PORT
            | u32::from(self.pit_flag) << regs::VECTOR_RTC_PIT;
        let requests = pending & self.enabled();
        (requests != 0).then(|| requests.trailing_zeros() as u8)
    }

    /// Idle and power-down are modelled. Every interrupt the firmware uses
    /// wakes the chip from either.
    fn sleep(&mut self) -> Result<(), String> {
        let ctrla = self.io[reg::SLPCTRL_CTRLA];
        if ctrla & regs::SLPCTRL_SEN == 0 {
            return Err(
                "sleeps with SLPCTRL.SEN clear, which the chip treats as a nop, \
                 so the firmware would spin instead"
                    .into(),
            );
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
        let mut wakes = self.enabled();
        if pit_off {
            wakes &= !(1 << regs::VECTOR_RTC_PIT);
        }
        if wakes == 0 {
            return Err("sleeps with no interrupt enabled that could wake it".into());
        }
        Ok(())
    }

    fn idle(&mut self) -> Result<(), String> {
        self.wait_for_pit()
    }

    fn read(&mut self, addr: u16) -> Result<u8, String> {
        let a = usize::from(addr);
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
                Ok(if self.pressed { !SWITCH } else { 0xFF })
            }
            reg::RTC_PITSTATUS => {
                let busy = self.pit_busy > 0;
                self.pit_busy = self.pit_busy.saturating_sub(1);
                Ok(if busy { regs::RTC_CTRLBUSY } else { 0 })
            }
            reg::WDT_STATUS => {
                let busy = self.wdt.busy > 0;
                self.wdt.busy = self.wdt.busy.saturating_sub(1);
                Ok(if busy { regs::WDT_SYNCBUSY } else { 0 })
            }
            // Reading the flag doesn't move time on: a firmware that went
            // back to busy-waiting on it would run out of instructions.
            reg::RTC_PITINTFLAGS => Ok(if self.pit_flag { regs::RTC_PI } else { 0 }),
            reg::PORTA_INTFLAGS => Ok(self.porta_flags),
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
            reg::TCA0_CMP0 | reg::TCA0_CMP1 => {
                let i = usize::from(a == reg::TCA0_CMP1);
                let [lo, hi] = self.compare[i].to_le_bytes();
                self.compare_temp = Some((a, hi));
                Ok(lo)
            }
            _ if a == reg::TCA0_CMP0 + 1 || a == reg::TCA0_CMP1 + 1 => {
                match self.compare_temp.take() {
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
        if SRAM.contains(&addr) {
            let cell = self.world.ram.map_or(addr, |fault| fault.cell(addr));
            self.sram[usize::from(cell - SRAM.start)] = value;
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
            }
            reg::CLKCTRL_MCLKCTRLB => {
                self.take_protected_write(a)?;
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
                }
            }
            reg::PORTA_INTFLAGS => self.porta_flags &= !value,
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
                    let bottom = self.now + PWM_CYCLE;
                    match low {
                        reg::TCA0_CMP0BUF | reg::TCA0_CMP1BUF => {
                            self.compare_update.get_or_insert(bottom);
                        }
                        reg::TCA0_CNT if self.compare_update.is_some() => {
                            self.compare_update = Some(bottom);
                        }
                        _ => {}
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
        if !self.watchdog_running() {
            return Ok(());
        }
        if self.wdt.busy > 0 {
            return Err("wdr before the WDT.CTRLA write has synced".into());
        }
        let ticks = self.wdt.ticks;
        let clocks = ticks * WDT_CLOCKS_PER_TICK;
        let closed = self.wdt_closed();
        if closed > 0 && clocks <= closed {
            return Err(format!(
                "wdr {ticks} ticks ({clocks} WDT clocks) after the last, in or on the edge \
                 of the {closed}-clock closed window, so the chip would have reset (or, \
                 within 2 to 3 clocks of the last, ignored it, section 19.3.6)"
            ));
        }
        self.wdt.ticks = 0;
        self.wdt.windowed = self.watchdog_windowed();
        Ok(())
    }
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
        assert!(c.write(clk, 1).is_err(), "no key");

        c.now = 10;
        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 14;
        assert_eq!(c.write(clk, 1), Ok(()), "fourth instruction after");
        assert_eq!(c.io[reg::CLKCTRL_MCLKCTRLB], 1);
        assert!(c.write(clk, 2).is_err(), "the key only covers one write");

        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 19;
        assert!(c.write(clk, 3).unwrap_err().contains("ignores"), "too late");
        assert!(c.write(reg::CPU_CCP as u16, 0x9D).is_err(), "SPM key");
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
        c.io[pin5] = regs::PORT_ISC_BOTHEDGES;
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        c.pit_flag = true;
        c.set_switch(true).unwrap();
        assert_eq!(c.interrupt(), Some(regs::VECTOR_PORTA_PORT as u8));
        c.write(reg::PORTA_INTFLAGS as u16, SWITCH).unwrap();
        assert_eq!(c.read(reg::PORTA_INTFLAGS as u16), Ok(0));
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
        c.io[reg::PORTA_PIN0CTRL + 5] = regs::PORT_ISC_BOTHEDGES;
        assert_eq!(c.sleep(), Ok(()));
    }

    /// A chip that's ready to power down, with the PIT to wake it.
    fn ready_to_power_down() -> Chip {
        let mut c = chip();
        c.pit_busy = 0;
        c.io[reg::SLPCTRL_CTRLA] = regs::SLPCTRL_SEN | regs::SLPCTRL_SMODE_PDOWN;
        c.io[reg::RTC_PITINTCTRL] = regs::RTC_PI;
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        c.io[reg::PORTA_PIN0CTRL + 5] = regs::PORT_ISC_BOTHEDGES;
        c
    }

    #[test]
    fn powers_down_when_ready_and_counts_it() {
        let mut c = ready_to_power_down();
        assert_eq!(c.sleep(), Ok(()));
        assert_eq!(c.power_downs, 1);
        c.io[reg::PORTA_PIN0CTRL + 5] = ISC_LEVEL;
        assert_eq!(c.sleep(), Ok(()), "LEVEL wakes it too");
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
    fn wont_power_down_unless_pa5_can_wake_it() {
        for isc in [ISC_INTDISABLE, ISC_RISING, ISC_FALLING, ISC_INPUT_DISABLE] {
            let mut c = ready_to_power_down();
            c.io[reg::PORTA_PIN0CTRL + 5] = isc;
            assert!(c.sleep().unwrap_err().contains("PA5"), "{isc}");
        }
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
    fn a_compare_takes_its_buf_a_pwm_cycle_after_the_write() {
        let mut c = chip();
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        write_wide(&mut c, reg::TCA0_CMP1BUF, 300);
        c.advance_to(c.now + PWM_CYCLE - 1).unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP1), 0);
        c.elapse().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP1), 300);
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0);
    }

    #[test]
    fn writing_cnt_puts_the_update_off_rather_than_forcing_it() {
        let mut c = chip();
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        write_wide(&mut c, reg::TCA0_CMP0BUF, 7);
        c.advance_to(c.now + PWM_CYCLE - 1).unwrap();
        write_wide(&mut c, reg::TCA0_CNT, 0);
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0, "not forced");
        c.advance_to(c.now + PWM_CYCLE - 1).unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 0, "a full cycle away");
        c.elapse().unwrap();
        assert_eq!(read_wide(&mut c, reg::TCA0_CMP0), 7);
    }

    #[test]
    fn a_stopped_timer_never_updates_its_compares() {
        let mut c = chip();
        write_wide(&mut c, reg::TCA0_CMP0BUF, 7);
        c.advance_to(c.now + 2 * PWM_CYCLE).unwrap();
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
        s.chip.world.register = Some(RegisterFault {
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
    }

    #[test]
    fn staying_dark_means_pins_low_interrupts_off_and_no_way_out() {
        // The pins driven low, then spin, as `__selftest_failed` does.
        let dark = |s: &mut Sim| {
            s.chip.io[reg::VPORTB_DIR] = FET | LINEAR;
            s.step().unwrap(); // jmp
        };
        let mut s = sim(&[SPIN]);
        dark(&mut s);
        assert_eq!(stays_dark(&mut s), Ok(()));

        let mut s = sim(&[SPIN]);
        s.step().unwrap();
        assert!(stays_dark(&mut s).unwrap_err().contains("floating"));

        let mut s = sim(&[SPIN]);
        dark(&mut s);
        s.cpu.sreg |= 1 << I;
        assert!(stays_dark(&mut s).unwrap_err().contains("interrupts on"));

        // nop, then back to the start: it leaves the spin.
        let mut s = sim(&[NOP, 0xCFFE]);
        dark(&mut s);
        assert!(stays_dark(&mut s).unwrap_err().contains("left"));
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
                regs::ADC_SAMPNUM_ACC1,
            ),
        };
        w(c, reg::VREF_CTRLA, vref);
        w(
            c,
            reg::ADC0_CTRLC,
            regs::ADC_SAMPCAP | regs::ADC_PRESC_DIV16 | refsel,
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
        assert_eq!(CALIBRATION.kelvin(convert(&mut c).unwrap()), 298);
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
            ("against INTREF", reg::ADC0_CTRLC, regs::ADC_PRESC_DIV16),
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
        // The temperature sensor needs its delays and SAMPCAP.
        for (register, value) in [
            (reg::ADC0_CTRLD, 0),
            (reg::ADC0_SAMPCTRL, regs::ADC_SAMPLEN_32US - 1),
            (
                reg::ADC0_CTRLC,
                regs::ADC_PRESC_DIV16 | regs::ADC_REFSEL_INTREF,
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

    #[test]
    fn leds_need_the_timer_the_channel_and_the_pin() {
        let mut c = chip();
        c.wide = [0, 255, 200, 100];
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
