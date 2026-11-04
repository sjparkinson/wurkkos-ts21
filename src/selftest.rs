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
use crate::ramp::PWM_TOP;
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
/// TCA0 overflows in a tick with both oscillators spot on: 5 MHz / 255 / 64
/// is 306.4, which this rounds down.
const NOMINAL_OVERFLOWS: u32 = CLK_PER_HZ / PWM_TOP as u32 / TICKS_PER_SECOND as u32;
/// The most a good chip can count, with the 32 kHz oscillator 30% slow (DS
/// table 36-13, full range) and the 20 MHz one 5% fast (table 36-12): 306.4 x
/// 1.05 / 0.70 = 459.6.
const MOST_OVERFLOWS: u32 = NOMINAL_OVERFLOWS * 105 / 70;
/// The least, the other way round: 306.4 x 0.95 / 1.30 = 223.9.
const LEAST_OVERFLOWS: u32 = NOMINAL_OVERFLOWS * 95 / 130;
/// The longest pass of the main loop the sums here allow for. `cargo xtask hex`
/// fails if it measures one longer, so raising this means rechecking
/// [`OVERFLOWS_MIN`], which the assert below does.
pub const PASS_INSTRUCTIONS: u32 = 3_500;
/// Overflows the count can miss. It starts when the pass before it ends, not
/// on the tick. A pass is at most 14,000 cycles if every instruction took
/// four, so 55 overflows. Most take one or two, so that's pessimistic by about
/// a factor of three. It leaves 168.9 as the least a good chip can count.
const PASS_OVERFLOWS: u32 = (PASS_INSTRUCTIONS * 4).div_ceil(PWM_TOP as u32);

/// The fewest overflows that pass: 14 under that 168.9, or 9%, and one over
/// T/2 below, so a tick at twice its rate still fails.
pub const OVERFLOWS_MIN: u16 = 154;
/// The most that pass: 52 over the 459.6 above, or 11%. Both margins are on
/// top of every worst case at once, because the table figures are
/// characterised, not production tested, and a false trip turns a working
/// torch off for good.
///
/// So the window is 0.50 to 1.67 of nominal, and only a clock out by about a
/// half or more fails. It can't tell 16 MHz from 20 MHz (245 overflows),
/// which is what the fuse check is for, or spot a prescaler that never got
/// set, which leaves 3.3 MHz and 204.
///
/// The WDT runs off the same oscillator as the tick, so this bounds the
/// WDT's clock too. AN2632 §6 wants its period within T/2 to 3T/2 by TCA0,
/// which is 153 to 460 overflows here. The floor sits on T/2. The ceiling
/// doesn't, since 3T/2 falls right on the 459.6 a good chip can reach.
pub const OVERFLOWS_MAX: u16 = 512;
/// What a count comes to when no tick ended it. Over [`OVERFLOWS_MAX`], so it
/// fails like any other count.
pub const NO_TICK: u16 = u16::MAX;
/// How often the count runs while the light's on: once a second. Each one
/// keeps the CPU awake for a tick, so this is 1.6% of the time.
pub const CLOCK_CHECK_TICKS: u8 = TICKS_PER_SECOND as u8;

// `src/hw.rs` counts overflows of a fixed PER, and the sums above need it.
const _: () = {
    assert!(PWM_TOP == 255);
    assert!(NOMINAL_OVERFLOWS == 306);
    assert!((OVERFLOWS_MIN as u32) < LEAST_OVERFLOWS - PASS_OVERFLOWS);
    assert!(OVERFLOWS_MAX as u32 > MOST_OVERFLOWS);
    assert!(TICKS_PER_SECOND <= u8::MAX as u16);
};

/// The main clock against the RTC, after AN2632 §8: TCA0 runs off the main
/// clock and the tick off the 32 kHz oscillator, so the number of TCA0
/// overflows between two ticks is the ratio of the two. A count outside
/// [`OVERFLOWS_MIN`] to [`OVERFLOWS_MAX`] means one of them, the PIT or TCA0
/// is broken, and the torch turns off and stays off until the cell comes out.
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

    /// The overflows counted between two ticks, or [`NO_TICK`].
    pub fn result(&mut self, overflows: u16) {
        self.counted = true;
        self.due = false;
        self.wait = CLOCK_CHECK_TICKS;
        self.failed |= !(OVERFLOWS_MIN..=OVERFLOWS_MAX).contains(&overflows);
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

    /// Whether a fresh check passes this count.
    fn passes(overflows: u16) -> bool {
        let mut check = ClockCheck::new();
        check.result(overflows);
        !check.failed()
    }

    #[test]
    fn the_clock_window_passes_on_each_edge_and_fails_just_past_it() {
        assert!(passes(OVERFLOWS_MIN));
        assert!(!passes(OVERFLOWS_MIN - 1));
        assert!(passes(OVERFLOWS_MAX));
        assert!(!passes(OVERFLOWS_MAX + 1));
    }

    #[test]
    fn every_clock_a_good_chip_can_have_passes() {
        // Nominal, then the two worst corners of tables 36-12 and 36-13,
        // and the low one again with the longest pass before the count taken
        // off.
        for overflows in [306, 459, 460, 223, 224, 169, 168] {
            assert!(passes(overflows), "{overflows}");
        }
    }

    #[test]
    fn a_clock_thats_badly_wrong_fails() {
        // What's left of 5 MHz with TCA0 stopped, with the prescaler at 2
        // and at 1, the PIT at twice its rate, and no tick at all.
        for overflows in [0, 613, 1225, 153, NO_TICK] {
            assert!(!passes(overflows), "{overflows}");
        }
    }

    #[test]
    fn counts_after_the_first_tick_even_at_rest_then_only_while_on() {
        let mut check = ClockCheck::new();
        assert!(!check.due(), "the first PIT period can be short");
        check.tick(true);
        assert!(check.due());
        check.result(306);
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
        check.result(0);
        for _ in 0..2 * u16::from(CLOCK_CHECK_TICKS) {
            check.tick(false);
            assert!(!check.due());
        }
        check.result(306);
        assert!(check.failed());
    }
}
