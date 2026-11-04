//! What the power-on self-tests found, and what the torch does about it.
//!
//! The firmware reads the fuses back before it turns anything on (AN2632
//! §11), and checks the fields it relies on, not whole bytes: the datasheet,
//! the errata and the ATDF disagree about the factory bytes, so a whole-byte
//! compare would fail on a chip that's fine. It never writes them.
//!
//! It also has the chip's CRCSCAN check the whole of flash against the
//! CRC-16 that `cargo xtask hex` stores in its last two bytes (AN2632 §9.1),
//! which catches a flash that a `cargo xtask flash` left half-written. The
//! hardware does the sums, so all that's here is what a failure means.
//!
//! `watchdog::Proof` adds one more: whether the watchdog passed its proof,
//! and whether it's since had to reset a chip that hung.
//!
//! A failure doesn't leave a dead torch, because the first flash has to be
//! easy to diagnose. It quietly refuses the FET instead: `limits::Limits`
//! caps the light at the top of the linear channel until the cell comes out.
//!
//! The clock check is the exception ([`ClockCheck`]): a wrong clock throws
//! off every timing the protections lean on, so that failure turns the light
//! off and leaves it off.

use crate::TICKS_PER_SECOND;
use crate::ramp::{MAX_TOP, PWM_TOP};
use crate::regs;

/// The fuse bytes the firmware checks, as read from data space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuses {
    pub wdtcfg: u8,
    pub osccfg: u8,
    pub syscfg0: u8,
    pub append: u8,
    pub bootend: u8,
}

impl Fuses {
    /// Whether every field the firmware relies on is what it expects:
    ///
    /// - `WDTCFG` is 0. Anything else starts the watchdog from the fuse and
    ///   locks its settings (DS §19.3.3.3), so the firmware can't set it up.
    /// - `OSCCFG.FREQSEL` is 20 MHz and `OSCLOCK` clear. Every timing assumes
    ///   a calibrated 20 MHz divided by four, and OSCLOCK stops the
    ///   calibration loading (errata 2.2.1).
    /// - `SYSCFG0.RSTPINCFG` is UPDI, so the light can still be reflashed.
    /// - `APPEND` and `BOOTEND` are 0: the vector table is linked at 0, and
    ///   only the whole-flash CRC scan expects the checksum where `xtask`
    ///   puts it.
    pub fn ok(self) -> bool {
        self.wdtcfg == 0
            && self.osccfg & regs::FUSE_FREQSEL == regs::FUSE_FREQSEL_20MHZ
            && self.osccfg & regs::FUSE_OSCLOCK == 0
            && self.syscfg0 & regs::FUSE_RSTPINCFG == regs::FUSE_RSTPINCFG_UPDI
            && self.append == 0
            && self.bootend == 0
    }
}

/// Which self-tests failed at power-on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// The fuses aren't what the firmware expects.
    pub fuses: bool,
    /// Flash doesn't match the CRC `cargo xtask hex` put at its end, by
    /// CRCSCAN's sums.
    pub flash: bool,
    /// The watchdog failed its proof, or had to reset a chip that hung.
    pub watchdog: bool,
}

impl Faults {
    pub const NONE: Self = Self {
        fuses: false,
        flash: false,
        watchdog: false,
    };

    /// Whether anything failed, which refuses the FET.
    pub const fn any(self) -> bool {
        self.fuses || self.flash || self.watchdog
    }
}

/// How many bytes one pass of the periodic RAM test covers. src/rt.rs keeps
/// a copy of them in CPU registers while it tests them, so this is as many
/// as it has registers to spare.
pub const RAM_SECTION: u16 = 16;
/// How far on the next section starts. It's less than [`RAM_SECTION`] so
/// neighbouring sections overlap, as AN2632 §3.5 wants: a coupling fault
/// between two bytes only shows if they're tested together.
pub const RAM_STEP: u16 = 8;
/// Sections in a sweep of all of SRAM, at one a tick: just under 4 seconds.
pub const RAM_SECTIONS: u8 = ((SRAM_BYTES - RAM_SECTION) / RAM_STEP + 1) as u8;

const SRAM_BYTES: u16 = (regs::addr::SRAM_END - regs::addr::SRAM_START) as u16;

// The sections reach the end of SRAM exactly, and fit a `u8`.
const _: () = {
    assert!((SRAM_BYTES - RAM_SECTION).is_multiple_of(RAM_STEP));
    assert!((SRAM_BYTES - RAM_SECTION) / RAM_STEP < 255);
    assert!(RAM_STEP < RAM_SECTION);
};

/// Which section of SRAM the periodic test does next. It's kept as a count
/// rather than an address because it lives in the RAM it's testing: if it's
/// corrupted before the test gets to it, the worst that can happen is a
/// different section, never a pointer into the peripherals' registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RamSweep {
    next: u8,
}

impl RamSweep {
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    /// The first address of the section to test now, moving on to the next.
    pub fn next_section(&mut self) -> u16 {
        let section = if self.next < RAM_SECTIONS {
            self.next
        } else {
            0
        };
        self.next = section + 1;
        regs::addr::SRAM_START as u16 + u16::from(section) * RAM_STEP
    }
}

/// `hw::init` runs the core at this. It's the 20 MHz oscillator divided by 4.
const CLK_PER_HZ: u32 = 5_000_000;
/// Main clocks in a tick with both oscillators spot on: 5 MHz / 64.
const NOMINAL_CLOCKS: u32 = CLK_PER_HZ / TICKS_PER_SECOND as u32;
/// The most a tick can take on a good chip, with the 32 kHz oscillator 30%
/// slow (DS table 36-13, full range) and the 20 MHz one 5% fast (table 36-12):
/// 78,125 x 1.05 / 0.70 = 117,187.
const MOST_CLOCKS: u32 = NOMINAL_CLOCKS * 105 / 70;
/// The least, the other way round: 78,125 x 0.95 / 1.30 = 57,091.
const LEAST_CLOCKS: u32 = NOMINAL_CLOCKS * 95 / 130;
/// The longest pass of the main loop the sums here allow for. `cargo xtask hex`
/// fails if it measures one longer, so raising this means rechecking
/// [`CLOCKS_MIN`], which the assert below does.
pub const PASS_INSTRUCTIONS: u32 = 3_500;
/// Clocks the count can miss. It starts when the pass before it ends, not on
/// the tick, and a pass is at most this if every instruction took four. Most
/// take one or two, so that's pessimistic by about a factor of three. It
/// leaves 43,091 as the least a good chip can count, whole cycles aside.
const PASS_CLOCKS: u32 = PASS_INSTRUCTIONS * 4;

/// The fewest clocks that pass, give or take the cycle in use: see
/// [`ClockCheck::result`]. It's 3,566 under that 43,091, or 8%. A count at the
/// shortest cycle has to reach 154 of them, which is one over T/2 below, so
/// a tick at twice its rate fails at power-on, when the cycle's short.
pub const CLOCKS_MIN: u32 = 155 * PWM_TOP as u32;
/// The most that pass: 13,373 over the 117,187 above, or 11%, and still 7%
/// with a whole cycle of the longest PWM on top. Both margins come on top of
/// every worst case at once, because the table figures are characterised, not
/// production tested, and a false trip turns a working torch off for good.
///
/// So the window is 0.50 to 1.67 of nominal, and only a clock out by about a
/// half or more fails. It can't tell 16 MHz from 20 MHz (62,500 clocks), which
/// is what the fuse check is for, or spot a prescaler that never got set,
/// which leaves 3.3 MHz and 52,083.
///
/// The WDT runs off the same oscillator as the tick, so this bounds the
/// WDT's clock too. AN2632 §6 wants its period within T/2 to 3T/2 by TCA0,
/// which is 39,062 to 117,187 clocks here. The floor sits on T/2. The ceiling
/// doesn't, since 3T/2 falls right on what a good chip can reach.
pub const CLOCKS_MAX: u32 = 512 * PWM_TOP as u32;
/// What a count comes to when no tick ended it. Over [`CLOCKS_MAX`], so it
/// fails like any other count.
pub const NO_TICK: u32 = u32::MAX;
/// How often the count runs while the light's on: once a second. Each one
/// keeps the CPU awake for a tick, so this is 1.6% of the time.
pub const CLOCK_CHECK_TICKS: u8 = TICKS_PER_SECOND as u8;

const _: () = {
    assert!(NOMINAL_CLOCKS == 78_125);
    assert!(CLOCKS_MIN < LEAST_CLOCKS - PASS_CLOCKS);
    assert!(CLOCKS_MIN - PWM_TOP as u32 > NOMINAL_CLOCKS / 2);
    assert!(CLOCKS_MAX > MOST_CLOCKS + MAX_TOP as u32);
    assert!(TICKS_PER_SECOND <= u8::MAX as u16);
};

/// The main clock against the RTC, after AN2632 §8: TCA0 runs off the main
/// clock and the tick off the 32 kHz oscillator, so how many clocks of TCA0's
/// go by between two ticks is the ratio of the two. `hw::count_tick` adds up
/// the cycles that end in that time, so it's addition only, whatever the
/// cycle's length. A count outside [`CLOCKS_MIN`] to [`CLOCKS_MAX`] means one
/// of them, the PIT or TCA0 is broken, and the torch turns off and stays off
/// until the cell comes out.
///
/// The firmware counts once at power-on and then every
/// [`CLOCK_CHECK_TICKS`] while the light's on. It doesn't count at rest, since
/// TCA0 stops in power-down, and a torch that's off isn't doing anything a
/// bad clock can upset. It waits for the first tick before the power-on count,
/// because the first PIT period can be anything up to a full one (DS
/// §23.5.2.2).
#[derive(Clone, Copy, Debug)]
pub struct ClockCheck {
    /// Ticks until the next count.
    wait: u8,
    /// Whether the power-on count has happened.
    counted: bool,
    due: bool,
    failed: bool,
}

impl ClockCheck {
    pub const fn new() -> Self {
        Self {
            wait: 1,
            counted: false,
            due: false,
            failed: false,
        }
    }

    /// Whether the next wait for a tick should be a counted one.
    pub const fn due(&self) -> bool {
        self.due
    }

    /// Call once a tick, with the torch `resting` or not.
    pub fn tick(&mut self, resting: bool) {
        let running = !resting || !self.counted;
        if running {
            self.wait = self.wait.saturating_sub(1);
        }
        self.due = running && self.wait == 0 && !self.failed;
    }

    /// The clocks counted between two ticks, or [`NO_TICK`], with `top` the
    /// cycle in use, in clocks.
    ///
    /// The count is whole cycles, so it can be a part-cycle out at each end:
    /// the last cycle hadn't finished when the tick came, and the first had
    /// already started when the count did. That's a cycle low at most, so the
    /// floor comes down by `top`, which keeps the margin on [`CLOCKS_MIN`] the
    /// same at any level. It's 16 of the shortest cycles at the longest PWM.
    /// The ceiling doesn't move, since it already has a longest cycle to
    /// spare, and a good chip is nowhere near it.
    pub fn result(&mut self, clocks: u32, top: u16) {
        self.counted = true;
        self.due = false;
        self.wait = CLOCK_CHECK_TICKS;
        self.failed |= clocks > CLOCKS_MAX || clocks + u32::from(top) < CLOCKS_MIN;
    }

    /// Whether any count has failed. It never clears.
    pub const fn failed(&self) -> bool {
        self.failed
    }
}

impl Default for ClockCheck {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes a factory ATtiny1616 should hold, near enough: the fields the
    /// check reads are right, and the rest is as the datasheet has it.
    const GOOD: Fuses = Fuses {
        wdtcfg: 0x00,
        osccfg: 0x02,
        syscfg0: 0xC4,
        append: 0x00,
        bootend: 0x00,
    };

    #[test]
    fn a_sweep_covers_all_of_sram_with_each_section_overlapping_the_last() {
        let mut sweep = RamSweep::new();
        let mut tested = [false; regs::addr::SRAM_END - regs::addr::SRAM_START];
        let mut last = None;
        for _ in 0..RAM_SECTIONS {
            let start = sweep.next_section();
            let from = usize::from(start) - regs::addr::SRAM_START;
            tested[from..from + usize::from(RAM_SECTION)].fill(true);
            if let Some(last) = last {
                assert_eq!(start - last, RAM_STEP);
            }
            last = Some(start);
        }
        assert!(tested.iter().all(|&t| t));
        assert_eq!(
            last.map(|l| usize::from(l + RAM_SECTION)),
            Some(regs::addr::SRAM_END)
        );
    }

    #[test]
    fn a_sweep_starts_again_at_the_bottom() {
        let mut sweep = RamSweep::new();
        let first = sweep.next_section();
        for _ in 1..RAM_SECTIONS {
            sweep.next_section();
        }
        assert_eq!(sweep.next_section(), first);
    }

    #[test]
    fn any_corrupted_count_still_lands_in_sram() {
        for next in 0..=u8::MAX {
            let start = RamSweep { next }.next_section();
            assert!(usize::from(start) >= regs::addr::SRAM_START);
            assert!(usize::from(start + RAM_SECTION) <= regs::addr::SRAM_END);
        }
    }

    #[test]
    fn factory_fuses_pass() {
        assert!(GOOD.ok());
    }

    #[test]
    fn checks_fields_not_whole_bytes() {
        // OSCCFG's middle bits are reserved, and SYSCFG0's CRCSRC and EESAVE
        // are none of the firmware's business. The ATDF's 0x7E and 0xF6 are
        // its idea of the bytes, and should pass too.
        for (osccfg, syscfg0) in [(0x7E, 0xF6), (0x02, 0x05), (0x7E, 0xC4), (0x02, 0x04)] {
            let fuses = Fuses {
                osccfg,
                syscfg0,
                ..GOOD
            };
            assert!(fuses.ok(), "{osccfg:#04x} {syscfg0:#04x}");
        }
    }

    #[test]
    fn each_wrong_field_fails() {
        // (what, which byte, its value), the bytes in `Fuses`' order.
        let bad = [
            ("watchdog on from the fuse", 0, 0x0B),
            ("watchdog window only", 0, 0x10),
            ("16 MHz", 1, 0x01),
            ("FREQSEL reserved", 1, 0x03),
            ("FREQSEL 0", 1, 0x00),
            ("OSCLOCK", 1, 0x82),
            ("reset pin as GPIO", 2, 0xC0),
            ("reset pin as reset", 2, 0xC8),
            ("application section", 3, 0x20),
            ("boot section", 4, 0x02),
        ];
        for (what, byte, value) in bad {
            let mut fuses = GOOD;
            let field = match byte {
                0 => &mut fuses.wdtcfg,
                1 => &mut fuses.osccfg,
                2 => &mut fuses.syscfg0,
                3 => &mut fuses.append,
                _ => &mut fuses.bootend,
            };
            *field = value;
            assert!(!fuses.ok(), "{what}");
        }
        let erased = Fuses {
            wdtcfg: 0xFF,
            osccfg: 0xFF,
            syscfg0: 0xFF,
            append: 0xFF,
            bootend: 0xFF,
        };
        assert!(!erased.ok());
    }

    #[test]
    fn only_a_fault_refuses_the_fet() {
        assert!(!Faults::NONE.any());
        let fuses = Faults {
            fuses: true,
            ..Faults::NONE
        };
        let flash = Faults {
            flash: true,
            ..Faults::NONE
        };
        let watchdog = Faults {
            watchdog: true,
            ..Faults::NONE
        };
        assert!(fuses.any() && flash.any() && watchdog.any());
    }

    /// The shortest and longest cycles, in clocks.
    const SHORT: u16 = 255;
    const LONG: u16 = 4095;
    /// What a good chip's count comes to with nothing wrong.
    const GOOD_COUNT: u32 = 306 * SHORT as u32;

    /// Whether a fresh check passes this count, with cycles of `top` clocks.
    fn passes(clocks: u32, top: u16) -> bool {
        let mut check = ClockCheck::new();
        check.result(clocks, top);
        !check.failed()
    }

    #[test]
    fn the_clock_window_passes_on_each_edge_and_fails_just_past_it() {
        for top in [SHORT, LONG] {
            let floor = CLOCKS_MIN - u32::from(top);
            assert!(passes(floor, top), "{top}");
            assert!(!passes(floor - 1, top), "{top}");
            assert!(passes(CLOCKS_MAX, top), "{top}");
            assert!(!passes(CLOCKS_MAX + 1, top), "{top}");
        }
    }

    #[test]
    fn a_long_cycle_is_counted_in_a_few_big_steps() {
        let long = u32::from(LONG);
        // 8.65 cycles is the floor, and 31.88 the ceiling.
        assert!(!passes(8 * long, LONG));
        assert!(passes(9 * long, LONG));
        assert!(passes(31 * long, LONG));
        assert!(!passes(32 * long, LONG));
    }

    #[test]
    fn every_clock_a_good_chip_can_have_passes_whatever_the_cycle() {
        // Nominal, then the two worst corners of tables 36-12 and 36-13, and
        // the low one again with the longest pass before the count taken off.
        for tick in [78_125, 117_187, 57_091, 57_091 - PASS_CLOCKS] {
            for top in [SHORT, 1000, LONG] {
                let cycle = u32::from(top);
                // A part-cycle's lost at the end, or gained at the start.
                let (least, most) = (tick / cycle * cycle, (tick / cycle + 1) * cycle);
                assert!(passes(least, top), "{tick} at {top}");
                assert!(passes(most, top), "{tick} at {top}");
            }
        }
    }

    #[test]
    fn a_clock_thats_badly_wrong_fails() {
        // What's left of 5 MHz with TCA0 stopped, with the prescaler at 2
        // and at 1, the PIT at twice its rate, and no tick at all.
        for clocks in [0, 613, 1225, 153].map(|c| c * u32::from(SHORT)) {
            assert!(!passes(clocks, SHORT), "{clocks}");
        }
        assert!(!passes(NO_TICK, SHORT));
        assert!(!passes(NO_TICK, LONG));
        assert!(!passes(0, LONG));
    }

    #[test]
    fn counts_after_the_first_tick_even_at_rest_then_only_while_on() {
        let mut check = ClockCheck::new();
        assert!(!check.due(), "the first PIT period can be short");
        check.tick(true);
        assert!(check.due());
        check.result(GOOD_COUNT, SHORT);
        assert!(!check.due());
        for _ in 0..3 * u16::from(CLOCK_CHECK_TICKS) {
            check.tick(true);
            assert!(!check.due());
        }
        for _ in 0..CLOCK_CHECK_TICKS - 1 {
            check.tick(false);
            assert!(!check.due());
        }
        check.tick(false);
        assert!(check.due());
        check.tick(true);
        assert!(!check.due(), "not if the light's just gone off");
    }

    #[test]
    fn a_failed_clock_stays_failed_and_stops_counting() {
        let mut check = ClockCheck::new();
        check.tick(true);
        check.result(0, SHORT);
        for _ in 0..2 * u16::from(CLOCK_CHECK_TICKS) {
            check.tick(false);
            assert!(!check.due());
        }
        check.result(GOOD_COUNT, SHORT);
        assert!(check.failed());
    }
}
