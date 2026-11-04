//! When to feed the watchdog, and how the firmware finds out it works.
//!
//! The firmware runs the WDT in window mode, as AN2632 §3.2 recommends: a
//! `wdr` in the closed window after the last one resets the chip, and so
//! does no `wdr` by the end of the open window that follows it. So a loop
//! that's stuck fails, and so does a jump into the wrong code that happens
//! to land on the feed.
//!
//! The WDT and the 64 Hz tick both run off the ultra-low-power oscillator,
//! so a tick is always 16 WDT clocks however far that oscillator drifts,
//! and the window can be sized in ticks. Feeding every tick would land on
//! a window edge whatever the setting, so the loop feeds every third tick:
//! 48 clocks, halfway through a 32-clock open window that starts 32 clocks
//! after the last feed.
//!
//! Window mode only starts with the first `wdr` (DS40002204A §19.3.3.2).
//! Until then the WDT most likely times out after the open window alone
//! (the datasheet implies it rather than saying so), so the first
//! pass feeds it straight away, which also lines the windows up with the
//! ticks.
//!
//! # The proof
//!
//! A watchdog nobody's seen bite is a guess, so after a power-on or UPDI
//! reset the firmware walks through AN2632 §6's four steps (page 17; §6.1 is
//! how to test the routine itself) before it starts the loop:
//!
//! 1. A timeout resets the chip.
//! 2. A feed in time doesn't. It feeds twice, half a period apart, stays up
//!    past where the timeout would have landed, then stops feeding and waits
//!    to be reset.
//! 3. An early feed in window mode resets the chip.
//! 4. The real setup.
//!
//! Each reset has to leave [`Step`] behind for the next boot, so a marker
//! goes in `.noinit` (`hw::marker`), and `RSTCTRL.RSTFR` says what kind of
//! reset it was. `src/rt.rs` skips its RAM test unless that was a power-on
//! or UPDI reset, or the test would wipe the marker. The datasheet doesn't
//! promise SRAM survives a watchdog reset. AN2632 §6 relies on it for its
//! own test state, so it's likely, but nobody's seen it on this light. If it
//! doesn't, the marker reads as junk and the proof fails safe.
//!
//! Every wait is counted in ticks. The PIT and the WDT share an oscillator,
//! so the ratio between them holds however far that oscillator is from
//! 1.024 kHz, and a chip at the edge of table 36-13's spread can't fail for
//! that alone. A wait on the CPU's clock could, or would need so much margin
//! it showed nothing. So a dead oscillator hangs the proof, as it would the
//! loop, with the outputs low. Spotting that is the clock check's job, and
//! so is §6's T/2 to 3T/2 check against TCA0. The nearest thing here is step
//! 2, which fails a watchdog that bites at half its period or sooner.
//!
//! The marker only says a bite is expected once a step has shown everything
//! it had to, so a bite before that is the watchdog biting early. A bite that
//! was expected and doesn't come ends in a software reset, which puts the WDT
//! back to its reset value for the setup that follows. Each boot writes
//! `WDT.CTRLA` once, since the datasheet doesn't say what rewriting it does
//! to a running count.
//!
//! Step 3 doesn't check a feed in the open window, which §6 does: that would
//! be 47 ms more on every power-on, and the loop's second feed is the same
//! check. A window that never opens resets the chip there, as a hang.

use crate::TICKS_PER_SECOND;
use crate::regs;

/// Cycles of the WDT's 1.024 kHz clock in one tick.
pub const CLOCKS_PER_TICK: u16 = 1024 / TICKS_PER_SECOND;
/// The closed window after each feed, in WDT clocks. `hw::init` sets
/// `WDT.CTRLA.WINDOW` to match.
pub const CLOSED_CLOCKS: u16 = 32;
/// The open window after the closed one, in WDT clocks. `hw::init` sets
/// `WDT.CTRLA.PERIOD` to match.
pub const OPEN_CLOCKS: u16 = 32;
/// How many ticks apart the feeds are.
pub const FEED_TICKS: u8 = 3;

// A feed lands strictly inside the open window, and the first one comes
// before the open window alone runs out: the first tick arrives within a
// tick of `hw::init` turning the WDT on. `wdr` takes two to three WDT clocks to sync
// (§19.3.6), the same for every feed, so it shifts the windows without
// changing the gap between feeds.
const _: () = {
    let gap = FEED_TICKS as u16 * CLOCKS_PER_TICK;
    assert!(gap > CLOSED_CLOCKS && gap < CLOSED_CLOCKS + OPEN_CLOCKS);
    assert!(CLOCKS_PER_TICK < OPEN_CLOCKS);
};

/// Counts ticks between feeds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Feeder {
    /// Ticks until the next feed.
    wait: u8,
}

impl Feeder {
    pub const fn new() -> Self {
        Self { wait: 0 }
    }

    /// Call once a tick, at the end of the loop. True when this pass feeds
    /// the watchdog: the first one, then every [`FEED_TICKS`]th.
    pub fn tick(&mut self) -> bool {
        if self.wait == 0 {
            self.wait = FEED_TICKS - 1;
            true
        } else {
            self.wait -= 1;
            false
        }
    }
}

/// `WDT.CTRLA` for the light in normal use: the window this file counts on.
const WINDOWED: u8 = regs::WDT_WINDOW_32CLK | regs::WDT_PERIOD_32CLK;

/// `WDT.CTRLA` after a failed proof. The light still has to run, and a
/// watchdog that bites early would keep resetting it on [`WINDOWED`] with
/// every feed on time. No window and the longest timeout keeps a hang
/// covered on any watchdog that bites at all, after about 8 s.
const FAILED: u8 = regs::WDT_PERIOD_8KCLK;

/// The tick step 1 gives up on. The timeout's 8 clocks after 2 to 3 for the
/// write to sync (§19.3.6), which is under a tick, however the ticks fall.
const TIMEOUT_GIVES_UP: u8 = 3;
/// The ticks step 2 feeds on, half of the 32-clock period apart. The first
/// comes up to a tick after the PIT starts (§23.5.2.2).
const FIRST_FEED: u8 = 1;
const LAST_FEED: u8 = 2;
/// The tick that shows the feeds worked, when step 2 starts expecting the
/// bite. The PIT only starts after the WDT write has synced, so this is past
/// the 32 clocks an unfed watchdog would have had, and the last feed gave it
/// 32 more.
const OUTLIVED: u8 = 3;
/// The tick step 2 gives up on. The bite's due just after the fourth.
const FED_GIVES_UP: u8 = 6;
/// The tick step 3 starts window mode on, with its first `wdr`.
const WINDOW_STARTED: u8 = 1;
/// The tick step 3 feeds early on, 16 clocks into the closed window.
const FED_EARLY: u8 = 2;
/// The tick step 3 gives up on. The reset's due 2 to 3 clocks after the feed
/// (§19.3.6). This has to come before any timeout could, or a window that
/// lets the feed through would pass for one that doesn't.
const EARLY_GIVES_UP: u8 = 3;

const _: () = {
    /// Ticks in the 32-clock period steps 2 and 3 run on.
    const PERIOD: u16 = OPEN_CLOCKS / CLOCKS_PER_TICK;
    /// WDT clocks a `wdr` can take to sync, when a second one is ignored.
    const SYNC: u16 = 3;
    const fn ticks(from: u8, to: u8) -> u16 {
        (to - from) as u16
    }

    // Step 2's feeds are in time, clear of each other's sync, and the tick
    // that says they worked is past the first deadline and short of the
    // last one.
    assert!((FIRST_FEED as u16) < PERIOD);
    assert!(ticks(FIRST_FEED, LAST_FEED) < PERIOD);
    assert!(ticks(FIRST_FEED, LAST_FEED) * CLOCKS_PER_TICK > SYNC);
    assert!(OUTLIVED as u16 > PERIOD);
    assert!(ticks(LAST_FEED, OUTLIVED) < PERIOD);
    assert!(ticks(LAST_FEED, FED_GIVES_UP) > PERIOD);

    // Step 3's early feed is clear of the first one's sync and inside the
    // closed window, and it gives up before the period after that feed
    // could run out.
    assert!(ticks(WINDOW_STARTED, FED_EARLY) * CLOCKS_PER_TICK > SYNC);
    assert!(ticks(WINDOW_STARTED, FED_EARLY) * CLOCKS_PER_TICK < CLOSED_CLOCKS);
    assert!(ticks(FED_EARLY, EARLY_GIVES_UP) < PERIOD);
    assert!(ticks(FED_EARLY, EARLY_GIVES_UP) * CLOCKS_PER_TICK > SYNC);
};

/// How far the proof has got, or how it came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Step 1. A bite is all it's waiting for.
    Timeout,
    /// Step 2, feeding. A bite now is early.
    Feed,
    /// Step 2, fed and past the deadline. Now a bite is the pass.
    Fed,
    /// Step 3, before the early feed. A bite now is early.
    Window,
    /// Step 3, feeding early. Now a bite is the pass.
    Early,
    /// The proof passed, so a bite from here on means something hung.
    Passed,
    /// The proof failed, so the watchdog can't be trusted.
    Failed,
    /// The watchdog passed its proof, then reset a chip that hung.
    Hung,
}

impl Step {
    const ALL: [Self; 8] = [
        Self::Timeout,
        Self::Feed,
        Self::Fed,
        Self::Window,
        Self::Early,
        Self::Passed,
        Self::Failed,
        Self::Hung,
    ];

    /// Whether the proof's still going.
    const fn proving(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Feed | Self::Fed | Self::Window | Self::Early
        )
    }
}

/// The marker's bit for a press during the proof.
const PRESSED: u8 = 0x80;

/// What the marker bytes in `.noinit` say: a step, and whether the switch
/// was seen down. `None` for anything else, which is what SRAM holds after
/// the RAM test, or if it doesn't survive a reset after all: the second
/// byte is the first's complement, so junk doesn't pass for a step.
pub fn decode(marker: [u8; 2]) -> Option<(Step, bool)> {
    let [value, check] = marker;
    if value != !check {
        return None;
    }
    let step = *Step::ALL.get(usize::from(value & !PRESSED))?;
    Some((step, value & PRESSED != 0))
}

const fn encode(step: Step, press: bool) -> [u8; 2] {
    let value = step as u8 | if press { PRESSED } else { 0 };
    [value, !value]
}

/// What a tick of the proof wants done, once the marker's been stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// `wdr`. Step 3's second one is meant to reset the chip.
    Feed,
    /// The bite it was waiting for didn't come, so the proof's failed. Reset
    /// the chip from software into the setup for that.
    GiveUp,
}

/// What this boot's doing about the watchdog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proof {
    step: Step,
    /// Ticks so far on this boot.
    ticks: u8,
    /// The switch has been seen down during this proof, for the next boot
    /// to find.
    press: bool,
    /// The switch was seen down during a proof that's now over, so the loop
    /// owes it a press.
    owed: bool,
}

impl Proof {
    /// What a reset means, from `RSTCTRL.RSTFR` and the marker it left. Every
    /// boot clears the flags, so they're this reset's.
    ///
    /// | Reset | Marker | Then |
    /// |---|---|---|
    /// | Power-on or UPDI | Any | The proof starts again |
    /// | Watchdog alone | Expecting a bite | The next step, or [`Step::Passed`] |
    /// | Watchdog, or no flag at all | Passed or hung | [`Step::Hung`] |
    /// | Brown-out, external or software | Passed or hung | As it was |
    /// | Any of these | Anything else | [`Step::Failed`] |
    ///
    /// No flag at all means something jumped to the reset vector, which is
    /// as much a hang as a bite. A UPDI flash goes first because it means new
    /// firmware, and a bite just before it can leave WDRF up as well.
    pub fn after_reset(flags: u8, marker: [u8; 2]) -> Self {
        const FLAGS: u8 = regs::RSTCTRL_PORF
            | regs::RSTCTRL_BORF
            | regs::RSTCTRL_EXTRF
            | regs::RSTCTRL_WDRF
            | regs::RSTCTRL_SWRF
            | regs::RSTCTRL_UPDIRF;
        let flags = flags & FLAGS;
        if flags & (regs::RSTCTRL_PORF | regs::RSTCTRL_UPDIRF) != 0 {
            return Self::at(Step::Timeout, false, false);
        }
        let bite = flags == regs::RSTCTRL_WDRF;
        let hang = flags & regs::RSTCTRL_WDRF != 0 || flags == 0;
        let (left, press) = match decode(marker) {
            Some((step, press)) => (Some(step), press),
            None => (None, false),
        };
        let step = match left {
            Some(Step::Timeout) if bite => Step::Feed,
            Some(Step::Fed) if bite => Step::Window,
            Some(Step::Early) if bite => Step::Passed,
            Some(Step::Passed) if hang => Step::Hung,
            Some(step @ (Step::Passed | Step::Hung)) => step,
            _ => Step::Failed,
        };
        if step.proving() {
            Self::at(step, press, false)
        } else {
            Self::at(step, false, press)
        }
    }

    const fn at(step: Step, press: bool, owed: bool) -> Self {
        Self {
            step,
            ticks: 0,
            press,
            owed,
        }
    }

    /// Whether this boot's a step of the proof, rather than the real thing.
    pub const fn proving(&self) -> bool {
        self.step.proving()
    }

    /// `WDT.CTRLA` for this boot.
    pub const fn watchdog(&self) -> u8 {
        match self.step {
            Step::Timeout => regs::WDT_PERIOD_8CLK,
            Step::Feed | Step::Fed => regs::WDT_PERIOD_32CLK,
            Step::Window | Step::Early | Step::Passed | Step::Hung => WINDOWED,
            Step::Failed => FAILED,
        }
    }

    /// What to store in `.noinit`, which has to happen before anything that
    /// could reset the chip.
    pub const fn marker(&self) -> [u8; 2] {
        encode(self.step, self.press)
    }

    /// Whether the watchdog failed or had to step in, which refuses the FET.
    pub const fn refused(&self) -> bool {
        matches!(self.step, Step::Failed | Step::Hung)
    }

    /// Whether the switch was seen down during a proof that's now over. The
    /// loop counts its first tick as pressed, so a press that came and went
    /// during the proof isn't lost.
    pub const fn owed_press(&self) -> bool {
        self.owed
    }

    /// Notes the switch, `true` meaning it's down.
    pub const fn sample(&mut self, pressed: bool) {
        self.press = self.press || pressed;
    }

    /// One tick of the proof, with the switch as it is. Store the marker
    /// before doing what this says, since it changes when a reset's next.
    pub const fn tick(&mut self, pressed: bool) -> Action {
        self.sample(pressed);
        self.ticks = self.ticks.saturating_add(1);
        match (self.step, self.ticks) {
            (Step::Timeout, TIMEOUT_GIVES_UP..)
            | (Step::Fed, FED_GIVES_UP..)
            | (Step::Early, EARLY_GIVES_UP..) => {
                self.step = Step::Failed;
                Action::GiveUp
            }
            (Step::Feed, FIRST_FEED | LAST_FEED) | (Step::Window, WINDOW_STARTED) => Action::Feed,
            (Step::Feed, OUTLIVED) => {
                self.step = Step::Fed;
                Action::Wait
            }
            (Step::Window, FED_EARLY) => {
                self.step = Step::Early;
                Action::Feed
            }
            _ => Action::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORF: u8 = regs::RSTCTRL_PORF;
    const WDRF: u8 = regs::RSTCTRL_WDRF;
    /// The resets that are neither a bite nor a new start.
    const OTHERS: [u8; 3] = [regs::RSTCTRL_BORF, regs::RSTCTRL_SWRF, regs::RSTCTRL_EXTRF];
    /// What SRAM holds after the RAM test: its last background, twice.
    const JUNK: [u8; 2] = [0x0F, 0x0F];

    fn marked(step: Step) -> [u8; 2] {
        encode(step, false)
    }

    fn after(flags: u8, step: Step) -> Step {
        Proof::after_reset(flags, marked(step)).step
    }

    /// A proof at `step`, with nothing owed.
    fn at(step: Step) -> Proof {
        Proof::at(step, false, false)
    }

    #[test]
    fn a_tick_is_16_wdt_clocks() {
        assert_eq!(CLOCKS_PER_TICK, 16);
    }

    #[test]
    fn feeds_on_the_first_tick() {
        assert!(Feeder::new().tick());
    }

    #[test]
    fn feeds_every_third_tick_after_that() {
        let mut feeder = Feeder::new();
        let feeds: [bool; 10] = core::array::from_fn(|_| feeder.tick());
        let (t, f) = (true, false);
        assert_eq!(feeds, [t, f, f, t, f, f, t, f, f, t]);
    }

    #[test]
    fn every_feed_lands_mid_window() {
        let middle = CLOSED_CLOCKS + OPEN_CLOCKS / 2;
        let mut feeder = Feeder::new();
        let mut last = None;
        for tick in 0..1000u16 {
            if feeder.tick() {
                if let Some(last) = last {
                    assert_eq!((tick - last) * CLOCKS_PER_TICK, middle, "tick {tick}");
                }
                last = Some(tick);
            }
        }
        assert!(last.is_some());
    }

    #[test]
    fn a_power_on_or_updi_reset_starts_the_proof_whatever_the_marker_says() {
        let updi = regs::RSTCTRL_UPDIRF;
        for flags in [PORF, updi, updi | WDRF, PORF | regs::RSTCTRL_BORF] {
            for step in Step::ALL {
                assert_eq!(after(flags, step), Step::Timeout, "{flags:#04x} {step:?}");
            }
            let proof = Proof::after_reset(flags, JUNK);
            assert_eq!(proof.step, Step::Timeout, "{flags:#04x}");
        }
    }

    #[test]
    fn expected_bites_walk_the_steps_to_a_pass() {
        assert_eq!(after(WDRF, Step::Timeout), Step::Feed);
        assert_eq!(after(WDRF, Step::Fed), Step::Window);
        assert_eq!(after(WDRF, Step::Early), Step::Passed);
    }

    #[test]
    fn a_bite_before_the_step_expects_one_fails_the_proof() {
        assert_eq!(after(WDRF, Step::Feed), Step::Failed);
        assert_eq!(after(WDRF, Step::Window), Step::Failed);
    }

    #[test]
    fn a_bite_after_the_proof_is_a_hang_and_a_refusal_lasts() {
        assert_eq!(after(WDRF, Step::Passed), Step::Hung);
        assert_eq!(after(WDRF, Step::Hung), Step::Hung);
        assert_eq!(after(WDRF, Step::Failed), Step::Failed);
    }

    #[test]
    fn no_flag_at_all_is_a_jump_to_the_reset_vector() {
        assert_eq!(after(0, Step::Passed), Step::Hung);
        assert_eq!(after(0, Step::Hung), Step::Hung);
        assert_eq!(after(0, Step::Failed), Step::Failed);
        assert_eq!(after(0, Step::Early), Step::Failed, "no bite, so no pass");
    }

    #[test]
    fn only_a_bite_on_its_own_counts_for_the_proof() {
        for other in OTHERS {
            for step in [Step::Timeout, Step::Fed, Step::Early] {
                assert_eq!(after(WDRF | other, step), Step::Failed, "{other:#04x}");
                assert_eq!(after(other, step), Step::Failed, "{other:#04x}");
            }
            assert_eq!(after(WDRF | other, Step::Passed), Step::Hung);
        }
    }

    #[test]
    fn other_resets_leave_the_verdict_alone() {
        for other in OTHERS {
            assert_eq!(after(other, Step::Passed), Step::Passed);
            assert_eq!(after(other, Step::Hung), Step::Hung);
            assert_eq!(after(other, Step::Failed), Step::Failed);
        }
    }

    #[test]
    fn a_marker_that_didnt_survive_fails_safe() {
        let junk = [JUNK, [0x00, 0x00], [0xFF, 0xFF], [0xA5, 0xA5], [0x7F, 0x80]];
        for flags in [WDRF, 0, OTHERS[0], OTHERS[1], OTHERS[2]] {
            for marker in junk {
                let proof = Proof::after_reset(flags, marker);
                assert_eq!(proof.step, Step::Failed, "{flags:#04x} {marker:02x?}");
                assert!(proof.refused());
            }
        }
    }

    #[test]
    fn the_flags_the_datasheet_doesnt_define_are_ignored() {
        assert_eq!(after(WDRF | 0xC0, Step::Early), Step::Passed);
    }

    #[test]
    fn every_step_survives_the_marker() {
        for step in Step::ALL {
            for press in [false, true] {
                assert_eq!(decode(encode(step, press)), Some((step, press)));
            }
        }
    }

    #[test]
    fn only_a_marker_that_checks_out_decodes() {
        assert_eq!(decode(JUNK), None);
        let [value, check] = marked(Step::Passed);
        assert_eq!(decode([value ^ 1, check]), None);
        assert_eq!(decode([value, check ^ 0x80]), None);
    }

    #[test]
    fn each_step_has_its_setting_and_only_a_failure_loses_the_window() {
        assert_eq!(at(Step::Timeout).watchdog(), regs::WDT_PERIOD_8CLK);
        assert_eq!(at(Step::Feed).watchdog(), regs::WDT_PERIOD_32CLK);
        assert_eq!(at(Step::Fed).watchdog(), regs::WDT_PERIOD_32CLK);
        for step in [Step::Window, Step::Early, Step::Passed, Step::Hung] {
            assert_eq!(at(step).watchdog(), WINDOWED, "{step:?}");
        }
        assert_eq!(at(Step::Failed).watchdog(), regs::WDT_PERIOD_8KCLK);
    }

    #[test]
    fn only_a_failure_or_a_hang_refuses_the_fet() {
        for step in Step::ALL {
            assert_eq!(
                at(step).refused(),
                matches!(step, Step::Failed | Step::Hung),
                "{step:?}"
            );
        }
    }

    #[test]
    fn the_proof_is_over_once_theres_a_verdict() {
        for step in Step::ALL {
            let over = matches!(step, Step::Passed | Step::Failed | Step::Hung);
            assert_eq!(at(step).proving(), !over, "{step:?}");
        }
    }

    #[test]
    fn step_1_waits_for_the_bite_then_gives_up() {
        let mut proof = Proof::after_reset(PORF, JUNK);
        assert_eq!(proof.tick(false), Action::Wait);
        assert_eq!(proof.tick(false), Action::Wait);
        assert_eq!(proof.step, Step::Timeout);
        assert_eq!(proof.tick(false), Action::GiveUp);
        assert_eq!(proof.step, Step::Failed);
    }

    #[test]
    fn step_2_feeds_twice_and_only_then_expects_a_bite() {
        let mut proof = Proof::after_reset(WDRF, marked(Step::Timeout));
        assert_eq!(proof.tick(false), Action::Feed);
        assert_eq!(proof.tick(false), Action::Feed);
        assert_eq!(proof.step, Step::Feed, "not past the first deadline yet");
        assert_eq!(proof.tick(false), Action::Wait);
        assert_eq!(proof.step, Step::Fed);
        assert_eq!(proof.tick(false), Action::Wait);
        assert_eq!(proof.tick(false), Action::Wait);
        assert_eq!(proof.tick(false), Action::GiveUp);
        assert_eq!(proof.step, Step::Failed);
    }

    #[test]
    fn step_3_expects_the_bite_before_it_feeds_early() {
        let mut proof = Proof::after_reset(WDRF, marked(Step::Fed));
        assert_eq!(proof.tick(false), Action::Feed);
        assert_eq!(proof.step, Step::Window);
        assert_eq!(proof.tick(false), Action::Feed);
        assert_eq!(proof.step, Step::Early);
        assert_eq!(proof.tick(false), Action::GiveUp);
        assert_eq!(proof.step, Step::Failed);
    }

    /// Runs the proof with `press` on the steps it says, ticking each for as
    /// long as a good chip lets it: the bite lands within one tick of step 1
    /// and just after the fourth of step 2, and the early feed of step 3 is
    /// the second.
    fn good_proof(press: [bool; 3]) -> Proof {
        let mut proof = Proof::after_reset(PORF, JUNK);
        for (ticks, press) in [1, 4, 2].into_iter().zip(press) {
            proof.sample(press);
            for _ in 0..ticks {
                proof.tick(false);
            }
            proof = Proof::after_reset(WDRF, proof.marker());
        }
        proof
    }

    #[test]
    fn a_good_chip_passes_and_runs_the_window() {
        let proof = good_proof([false; 3]);
        assert_eq!(proof.step, Step::Passed);
        assert!(!proof.proving() && !proof.refused() && !proof.owed_press());
        assert_eq!(proof.watchdog(), WINDOWED);
    }

    #[test]
    fn a_press_during_any_step_is_still_owed_when_the_proof_ends() {
        for press in [
            [true, false, false],
            [false, true, false],
            [false, false, true],
        ] {
            let proof = good_proof(press);
            assert_eq!(proof.step, Step::Passed);
            assert!(proof.owed_press(), "{press:?}");
        }
    }

    #[test]
    fn a_press_seen_on_a_tick_counts_too() {
        let mut proof = Proof::after_reset(WDRF, marked(Step::Fed));
        proof.tick(true);
        proof.tick(false);
        assert!(Proof::after_reset(WDRF, proof.marker()).owed_press());
    }

    #[test]
    fn a_failed_proof_still_owes_its_press() {
        // By a bite too early, and by a bite that never came.
        let mut proof = Proof::after_reset(WDRF, marked(Step::Timeout));
        proof.tick(true);
        let proof = Proof::after_reset(WDRF, proof.marker());
        assert!(proof.refused() && proof.owed_press());

        let mut proof = Proof::after_reset(PORF, JUNK);
        proof.sample(true);
        while proof.tick(false) != Action::GiveUp {}
        let proof = Proof::after_reset(regs::RSTCTRL_SWRF, proof.marker());
        assert!(proof.refused() && proof.owed_press());
    }

    #[test]
    fn a_press_is_only_owed_once() {
        let proof = Proof::after_reset(WDRF, encode(Step::Early, true));
        assert!(proof.owed_press());
        let proof = Proof::after_reset(WDRF, proof.marker());
        assert_eq!(proof.step, Step::Hung);
        assert!(!proof.owed_press(), "a hang's reset isn't a click");
    }

    #[test]
    fn a_new_proof_forgets_an_old_press() {
        let proof = Proof::after_reset(regs::RSTCTRL_UPDIRF, encode(Step::Feed, true));
        assert_eq!(decode(proof.marker()), Some((Step::Timeout, false)));
    }
}
