//! The cell's voltage, and what to do as it runs flat.
//!
//! The MCU has no pin on the cell. It runs from it (through a drop of a few
//! tenths of a volt, see docs/hardware.md), so it measures its own supply:
//! the ADC uses VCC as its reference and converts the internal 1.5 V
//! reference, four conversions accumulated. The result goes down as the
//! voltage goes up.
//!
//! ```text
//! reading = 4 × 1024 × 1.5 V / VCC   so   VCC = 6144 / reading volts
//! ```
//!
//! That's the scaling Anduril uses (`mcu_vdd_raw2cooked16` in
//! `arch/attiny1616.c`), with 1024 counts to full scale where the datasheet
//! says 1023. The 0614 calibration (`VOLTAGE_SLOPE` 1010 and
//! `VOLTAGE_OFFSET` 30 in `hw/sofirn/blf-q8-t1616/hwdef.h`) was fitted against
//! Anduril's scaling, so this keeps it: the difference is 0.1%, about 0.4 cV
//! at 4.2 V, and the calibration is a guess for this light anyway. The slope
//! applies to the VCC term and the offset goes on after, as in Anduril.
//!
//! The numerator is 606,000, which needs 20 bits. A `u32` division links
//! fine, but `compiler_builtins`' 32-bit divide costs about 1.3 KB of flash,
//! and a 16-bit `/` and `%` together get the remainder with a multiply. So
//! this does schoolbook long division a bit at a time, with nothing but
//! compares, shifts and subtracts on `u16`s.

use crate::TICKS_PER_SECOND;
use crate::ramp::LEVELS;

/// The 0614 build's voltage slope, out of 1024. A starting point, not a
/// measurement of this light.
pub const SLOPE: u16 = 1010;

/// The 0614 build's voltage offset in centivolts, added after the slope. It's
/// most likely the drop between the cell and the MCU's supply.
pub const OFFSET: u16 = 30;

/// 1.5 V in centivolts, times four conversions, times 1024 counts to full
/// scale, times the slope out of 1024. The two 1024s cancel.
const NUMERATOR: u32 = 150 * 4 * SLOPE as u32;

const _: () = assert!(NUMERATOR < 1 << 20);

/// Readings below this give more than `u16::MAX` centivolts.
const MIN_READING: u16 = 10;

const _: () = assert!(NUMERATOR / MIN_READING as u32 + OFFSET as u32 <= u16::MAX as u32);

/// The cell's voltage in centivolts from one accumulated reading of the
/// internal reference against VCC, rounded to the nearest centivolt.
///
/// Readings too small for the answer to fit (zero included, which would mean
/// VCC at infinity, so a broken read) come out as `u16::MAX`, which is safe:
/// it never steps down or turns off.
pub fn centivolts(reading: u16) -> u16 {
    if reading < MIN_READING {
        return u16::MAX;
    }
    // Each step doubles the remainder and brings down a bit. The remainder is
    // always under the reading, so `reading - remainder - bit` can't wrap, and
    // comparing against it asks whether `2 × remainder + bit >= reading`
    // without ever forming a sum that could overflow.
    let (mut quotient, mut remainder) = (0u16, 0u16);
    // The numerator's 20 bits, top-aligned and shifted out one at a time.
    // This also stops the compiler unrolling the loop into 600 bytes.
    let mut bits = NUMERATOR << 12;
    for _ in 0..20 {
        let bit = (bits >> 31) as u16;
        bits <<= 1;
        let gap = reading - remainder - bit;
        quotient <<= 1;
        if remainder >= gap {
            remainder -= gap;
            quotient |= 1;
        } else {
            remainder = (remainder << 1) | bit;
        }
    }
    // Round half up: the fraction left over is remainder / reading.
    if remainder >= reading - remainder {
        quotient += 1;
    }
    quotient + OFFSET
}

/// Below this many centivolts, held for [`HOLD_TICKS`], the ceiling steps down.
pub const STEP_DOWN_BELOW: u16 = 300;

/// Below this many centivolts, when the hold runs out, the torch turns off.
pub const OFF_BELOW: u16 = 280;

/// At or above this many centivolts, held for [`RECOVER_TICKS`], the ceiling
/// steps back up. The 0.2 V gap to [`STEP_DOWN_BELOW`] is the hysteresis: a
/// cell that sags to 2.9 V on the FET and rests at 3.1 V on a lower level
/// stays put.
pub const RECOVER_AT: u16 = 320;

/// How long the filtered voltage has to stay under [`STEP_DOWN_BELOW`] before
/// anything happens: one second, at one reading a tick.
pub const HOLD_TICKS: u16 = TICKS_PER_SECOND;

/// How long it has to stay at [`RECOVER_AT`] or above for each step back up.
pub const RECOVER_TICKS: u16 = TICKS_PER_SECOND * 4;

/// How many levels each step down (or up) moves the ceiling, a tenth of the
/// ramp.
pub const STEP: u8 = 15;

/// The low-voltage verdict for one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run no higher than this level. [`LEVELS`] means no limit.
    Ceiling(u8),
    /// Turn off now. The cell's flat.
    Off,
}

/// Low-voltage protection: feed it one voltage a tick and it says how bright
/// the torch may be.
///
/// Readings go through a moving average (each one moves the filtered value a
/// sixteenth of the way, a time constant of a quarter of a second at 64 Hz),
/// and the filtered value has to stay low for a whole [`HOLD_TICKS`] before it
/// acts, so one bad reading, or the sag as the FET comes on, does nothing.
///
/// Once the hold runs out, it turns the torch off if the filtered voltage is
/// under [`OFF_BELOW`] or the ceiling has nowhere left to go, and otherwise
/// drops the ceiling to [`STEP`] levels under whichever is lower of the
/// ceiling and the level the torch is at. Then the hold starts again. It
/// never steps down or turns off while the torch is off, since there's no
/// load to sag under, but it does recover then.
#[derive(Clone, Debug)]
pub struct Protection {
    /// The filtered voltage in sixteenths of a centivolt. Zero until the
    /// first reading, which seeds it.
    filtered: u16,
    low_ticks: u16,
    high_ticks: u16,
    ceiling: u8,
}

impl Default for Protection {
    fn default() -> Self {
        Self::new()
    }
}

impl Protection {
    pub const fn new() -> Self {
        Self {
            filtered: 0,
            low_ticks: 0,
            high_ticks: 0,
            ceiling: LEVELS,
        }
    }

    /// The filtered voltage in centivolts, or `None` before the first reading.
    pub fn centivolts(&self) -> Option<u16> {
        match self.filtered {
            0 => None,
            filtered => Some(filtered >> 4),
        }
    }

    /// One tick: `centivolts` from [`centivolts`] and the level the torch is
    /// at (`0` for off), before any ceiling.
    pub fn tick(&mut self, centivolts: u16, level: u8) -> Verdict {
        let voltage = self.filter(centivolts);

        if voltage >= RECOVER_AT {
            self.high_ticks += 1;
            if self.high_ticks >= RECOVER_TICKS {
                self.high_ticks = 0;
                self.ceiling = self.ceiling.saturating_add(STEP).min(LEVELS);
            }
        } else {
            self.high_ticks = 0;
        }

        if level == 0 || voltage >= STEP_DOWN_BELOW {
            self.low_ticks = 0;
            return Verdict::Ceiling(self.ceiling);
        }

        self.low_ticks += 1;
        if self.low_ticks < HOLD_TICKS {
            return Verdict::Ceiling(self.ceiling);
        }

        self.low_ticks = 0;
        let from = self.ceiling.min(level);
        if voltage < OFF_BELOW || from == 1 {
            return Verdict::Off;
        }
        self.ceiling = from.saturating_sub(STEP).max(1);
        Verdict::Ceiling(self.ceiling)
    }

    /// Folds a reading into the moving average and returns the result in
    /// centivolts. Readings over 4095 cV (which a real cell can't give)
    /// clamp, so the sum stays in a `u16`.
    fn filter(&mut self, centivolts: u16) -> u16 {
        let sample = centivolts.min(u16::MAX >> 4);
        self.filtered = match self.filtered {
            0 => (sample << 4).max(1),
            filtered => filtered - (filtered >> 4) + sample,
        };
        self.filtered >> 4
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// The conversion from first principles, as an exact fraction with room
    /// to spare: VCC is 1.5 V × 4 × 1024 / reading, in centivolts that's
    /// 15 × 4 × 1024 × 10 / reading, then the slope out of 1024, rounded
    /// half up, then the offset.
    fn reference(reading: u16) -> u16 {
        let p = 15 * 4 * 1024 * 10 * u64::from(SLOPE);
        let q = 1024 * u64::from(reading);
        let rounded = (2 * p + q) / (2 * q);
        (rounded + u64::from(OFFSET)).min(u64::from(u16::MAX)) as u16
    }

    /// The calibrated voltage in centivolts for a supply in volts.
    fn calibrated(supply: f64) -> f64 {
        supply * f64::from(SLOPE) / 1024.0 * 100.0 + f64::from(OFFSET)
    }

    /// The reading a supply of `volts` gives, before calibration.
    fn reading_at(volts: f64) -> u16 {
        (1.5 * 4.0 * 1024.0 / volts).round() as u16
    }

    #[test]
    fn matches_the_formula_for_every_reading() {
        for reading in 1..=u16::MAX {
            assert_eq!(centivolts(reading), reference(reading), "{reading}");
        }
    }

    #[test]
    fn falls_as_the_reading_rises() {
        for reading in 1..u16::MAX {
            assert!(centivolts(reading) >= centivolts(reading + 1), "{reading}");
        }
    }

    #[test]
    fn covers_a_cell_from_full_to_flat_within_a_centivolt() {
        // 4.2 V at the cell to 2.5 V, at the MCU 0.3 V lower and before the
        // slope. Anything 1.8 V and up is in range of four 10-bit conversions.
        for supply_cv in 180..=500 {
            let supply = f64::from(supply_cv) / 100.0;
            let reading = reading_at(supply);
            assert!(reading <= 4 * 1023, "{supply_cv} cV reads {reading}");
            let got = f64::from(centivolts(reading));
            // Half a centivolt of rounding, plus half a count from the
            // reading's own rounding, which is under a quarter of a
            // centivolt up to 5 V.
            assert!(
                (got - calibrated(supply)).abs() <= 0.75,
                "{supply_cv} cV gives {got}"
            );
        }
    }

    #[test]
    fn known_points() {
        // 3 V at the MCU reads 2048 and calibrates to 2.96 + 0.30 V.
        assert_eq!(reading_at(3.0), 2048);
        assert_eq!(centivolts(2048), 326);
        // The largest reading four 10-bit conversions can give is 1.5 V.
        assert_eq!(centivolts(4 * 1023), 178);
        assert_eq!(centivolts(1), u16::MAX);
    }

    #[test]
    fn a_zero_reading_is_safe_rather_than_a_panic() {
        assert_eq!(centivolts(0), u16::MAX);
    }

    /// Runs `ticks` ticks at a steady voltage and level, and returns every
    /// verdict.
    fn run(lvp: &mut Protection, cv: u16, level: u8, ticks: u16) -> Vec<Verdict> {
        (0..ticks).map(|_| lvp.tick(cv, level)).collect()
    }

    fn last(lvp: &mut Protection, cv: u16, level: u8, ticks: u16) -> Verdict {
        *run(lvp, cv, level, ticks).last().unwrap()
    }

    const NONE: Verdict = Verdict::Ceiling(LEVELS);

    #[test]
    fn a_full_cell_has_no_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(lvp.centivolts(), None);
        assert!(
            run(&mut lvp, 410, LEVELS, 10_000)
                .iter()
                .all(|&v| v == NONE)
        );
        assert_eq!(lvp.centivolts(), Some(410));
    }

    #[test]
    fn the_first_reading_seeds_the_filter() {
        let mut lvp = Protection::new();
        lvp.tick(290, 100);
        assert_eq!(lvp.centivolts(), Some(290));
    }

    #[test]
    fn steps_down_after_a_second_below_3_volts() {
        let mut lvp = Protection::new();
        let verdicts = run(&mut lvp, 295, 100, HOLD_TICKS);
        let (before, after) = verdicts.split_at(usize::from(HOLD_TICKS) - 1);
        assert!(before.iter().all(|&v| v == NONE));
        assert_eq!(after, [Verdict::Ceiling(100 - STEP)]);
    }

    #[test]
    fn exactly_3_volts_is_not_low() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, STEP_DOWN_BELOW, 100, 10_000), NONE);
    }

    #[test]
    fn keeps_stepping_down_a_second_at_a_time() {
        let mut lvp = Protection::new();
        let mut ceiling = 100;
        while ceiling > STEP {
            ceiling -= STEP;
            assert_eq!(
                last(&mut lvp, 290, 100, HOLD_TICKS),
                Verdict::Ceiling(ceiling)
            );
        }
        // 100 is 6 × 15 + 10, so the last step lands on the bottom level.
        assert_eq!(last(&mut lvp, 290, 100, HOLD_TICKS), Verdict::Ceiling(1));
        assert_eq!(last(&mut lvp, 290, 100, HOLD_TICKS), Verdict::Off);
    }

    #[test]
    fn steps_from_the_level_when_it_is_under_the_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 295, 40, HOLD_TICKS), Verdict::Ceiling(25));
    }

    #[test]
    fn turns_off_after_a_second_below_2_8_volts() {
        let mut lvp = Protection::new();
        let verdicts = run(&mut lvp, 275, LEVELS, HOLD_TICKS);
        let (before, after) = verdicts.split_at(usize::from(HOLD_TICKS) - 1);
        assert!(before.iter().all(|&v| v == NONE));
        assert_eq!(after, [Verdict::Off]);
    }

    #[test]
    fn exactly_2_8_volts_steps_down_rather_than_off() {
        let mut lvp = Protection::new();
        assert_eq!(
            last(&mut lvp, OFF_BELOW, LEVELS, HOLD_TICKS),
            Verdict::Ceiling(LEVELS - STEP)
        );
    }

    #[test]
    fn one_bad_reading_does_nothing() {
        let mut lvp = Protection::new();
        run(&mut lvp, 380, LEVELS, 100);
        for glitch in [0, 1, 100, 279] {
            assert_eq!(lvp.tick(glitch, LEVELS), NONE);
            assert!(run(&mut lvp, 380, LEVELS, 1000).iter().all(|&v| v == NONE));
        }
    }

    #[test]
    fn a_burst_of_bad_readings_shorter_than_the_hold_does_nothing() {
        let mut lvp = Protection::new();
        run(&mut lvp, 380, LEVELS, 100);
        assert!(
            run(&mut lvp, 0, LEVELS, HOLD_TICKS / 2)
                .iter()
                .all(|&v| v == NONE)
        );
        assert!(run(&mut lvp, 380, LEVELS, 1000).iter().all(|&v| v == NONE));
    }

    #[test]
    fn the_filter_takes_a_while_to_follow_a_drop() {
        let mut lvp = Protection::new();
        run(&mut lvp, 360, LEVELS, 100);
        let verdicts = run(&mut lvp, 250, LEVELS, 4 * HOLD_TICKS);
        // The filtered voltage passes 3 V a few ticks in, then the hold has
        // to run out before the light goes off.
        let off = verdicts.iter().position(|&v| v == Verdict::Off).unwrap();
        assert!(
            off > usize::from(HOLD_TICKS) && off < usize::from(HOLD_TICKS) + 32,
            "{off}"
        );
    }

    #[test]
    fn a_reading_back_above_3_volts_restarts_the_hold() {
        let mut lvp = Protection::new();
        run(&mut lvp, 295, 100, HOLD_TICKS / 2);
        // Enough good readings to lift the filtered value well over 3 V.
        run(&mut lvp, 320, 100, 32);
        // Without the restart this would be well past the hold.
        assert_eq!(last(&mut lvp, 295, 100, HOLD_TICKS - 2), NONE);
        assert_eq!(last(&mut lvp, 295, 100, HOLD_TICKS), Verdict::Ceiling(85));
    }

    #[test]
    fn does_nothing_while_the_torch_is_off() {
        let mut lvp = Protection::new();
        assert!(run(&mut lvp, 250, 0, 10_000).iter().all(|&v| v == NONE));
        // And the hold starts from scratch when it comes on.
        assert_eq!(last(&mut lvp, 250, 50, HOLD_TICKS - 1), NONE);
        assert_eq!(lvp.tick(250, 50), Verdict::Off);
    }

    #[test]
    fn turning_off_keeps_the_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 295, 60, HOLD_TICKS), Verdict::Ceiling(45));
        assert_eq!(last(&mut lvp, 250, 45, 4 * HOLD_TICKS), Verdict::Off);
        assert_eq!(last(&mut lvp, 290, 0, 100), Verdict::Ceiling(45));
    }

    #[test]
    fn hysteresis_holds_the_ceiling_between_3_and_3_2_volts() {
        let mut lvp = Protection::new();
        assert_eq!(
            last(&mut lvp, 295, LEVELS, HOLD_TICKS),
            Verdict::Ceiling(135)
        );
        let resting = Verdict::Ceiling(135);
        assert!(
            run(&mut lvp, 319, 135, 10_000)
                .iter()
                .all(|&v| v == resting)
        );
        assert!(
            run(&mut lvp, 300, 135, 10_000)
                .iter()
                .all(|&v| v == resting)
        );
    }

    #[test]
    fn recovers_a_step_every_four_seconds_at_3_2_volts() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 295, 50, HOLD_TICKS), Verdict::Ceiling(35));
        // The filter needs a few ticks to climb from 2.95 to 3.2 V.
        let verdicts = run(&mut lvp, 330, 35, RECOVER_TICKS + 64);
        let up = verdicts
            .iter()
            .position(|&v| v == Verdict::Ceiling(50))
            .unwrap();
        let first = usize::from(RECOVER_TICKS);
        assert!(up >= first && up < first + 64, "{up}");
        assert_eq!(last(&mut lvp, 330, 0, RECOVER_TICKS), Verdict::Ceiling(65));
        // Back to no limit, and no further.
        assert_eq!(last(&mut lvp, 330, 0, 20 * RECOVER_TICKS), NONE);
    }

    #[test]
    fn readings_too_big_for_the_filter_clamp() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, u16::MAX, LEVELS, 1000), NONE);
        assert_eq!(lvp.centivolts(), Some(4095));
    }

    #[test]
    fn a_seed_of_zero_still_counts_as_seeded() {
        let mut lvp = Protection::new();
        lvp.tick(0, 100);
        assert_eq!(lvp.centivolts(), Some(0));
        // And the next reading averages in rather than seeding it again.
        lvp.tick(400, 100);
        assert_eq!(lvp.centivolts(), Some(25));
    }
}
