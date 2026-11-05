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
//! So a reading goes up as the cell goes down, and the firmware never works
//! out a voltage. The numerator is 606,000, which needs 20 bits, and a 32-bit
//! divide costs about 1.3 KB of flash. The thresholds stay in centivolts,
//! since that's how docs/ui.md talks, and `highest_reading` turns them into
//! counts at compile time. Everything at runtime compares raw readings, so
//! "below a voltage" is "above a reading".
//!
//! `centivolts` is still here for the tests and the simulators, and only
//! exists for them, so the firmware can't call it by accident.

use crate::TICKS_PER_SECOND;
use crate::ramp::{LEVELS, LINEAR_MAX};

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
#[cfg(any(test, feature = "testing"))]
const MIN_READING: u16 = 10;

#[cfg(any(test, feature = "testing"))]
const _: () = assert!(NUMERATOR / MIN_READING as u32 + OFFSET as u32 <= u16::MAX as u32);

/// The cell's voltage in centivolts from one accumulated reading of the
/// internal reference against VCC, rounded to the nearest centivolt.
///
/// Readings too small for the answer to fit (zero included, which would mean
/// VCC at infinity, so a broken read) come out as `u16::MAX`.
///
/// This is the definition the thresholds are worked out from. It's a 32-bit
/// divide, so it's only built for the tests and the simulators.
#[cfg(any(test, feature = "testing"))]
pub fn centivolts(reading: u16) -> u16 {
    if reading < MIN_READING {
        return u16::MAX;
    }
    let reading = u32::from(reading);
    // Round half up.
    ((2 * NUMERATOR + reading) / (2 * reading)) as u16 + OFFSET
}

/// The highest reading `centivolts` calls `cv` or more, so a reading is
/// under `cv` exactly when it's above this. Compile time only.
///
/// `centivolts` rounds, so everything down to half a centivolt under `cv`
/// counts as `cv`, which is where the `2 × … - 1` comes from: the last reading
/// with `NUMERATOR / reading >= cv - OFFSET - ½`.
const fn highest_reading(cv: u16) -> u16 {
    (2 * NUMERATOR / (2 * (cv - OFFSET) as u32 - 1)) as u16
}

/// How many ticks apart the voltage samples come. The ADC alternates between
/// the voltage and the temperature sensor every 8 ticks, so each gets a fresh
/// reading every 16.
pub const SAMPLE_TICKS: u16 = 16;

/// Voltage samples a second: four.
pub const SAMPLES_PER_SECOND: u16 = TICKS_PER_SECOND / SAMPLE_TICKS;

const _: () = assert!(TICKS_PER_SECOND.is_multiple_of(SAMPLE_TICKS));

/// Below this many centivolts, held for [`STEP_SAMPLES`], the ceiling steps
/// down. It's 0.11 V over [`OFF_BELOW`], so there's room to step down before
/// cutting out.
pub const STEP_DOWN_BELOW: u16 = 310;

/// Below this many centivolts, held for [`HOLD_SAMPLES`], the torch turns off,
/// or drops to the linear channel if it's on the FET. Stock Anduril on this
/// light is reported to cut out at about 2.99 V, and the Wurkkos cell's
/// minimum is 2.50 V (docs/hardware.md).
pub const OFF_BELOW: u16 = 299;

/// At or above this many centivolts, held for [`RECOVER_SAMPLES`], the
/// ceiling steps back up. The 0.2 V gap to [`STEP_DOWN_BELOW`] is the
/// hysteresis: a cell that sags to 3.05 V on the FET and rests at 3.25 V on a
/// lower level stays put.
pub const RECOVER_AT: u16 = 330;

/// A filtered reading above this is under [`STEP_DOWN_BELOW`].
const STEP_DOWN_OVER: u16 = highest_reading(STEP_DOWN_BELOW);

/// A filtered reading above this is under [`OFF_BELOW`].
const OFF_OVER: u16 = highest_reading(OFF_BELOW);

/// A filtered reading at or below this is at [`RECOVER_AT`] or above.
const RECOVER_UP_TO: u16 = highest_reading(RECOVER_AT);

const _: () = assert!(RECOVER_UP_TO < STEP_DOWN_OVER && STEP_DOWN_OVER < OFF_OVER);

/// How long the filtered voltage has to stay under [`OFF_BELOW`] before it
/// acts: one second.
pub const HOLD_SAMPLES: u16 = SAMPLES_PER_SECOND;

/// How long it has to stay under [`STEP_DOWN_BELOW`] for each step down: five
/// seconds. Long enough for the cell to recover a little after the last step,
/// the way Anduril waits about six seconds between its low-voltage
/// step-downs.
pub const STEP_SAMPLES: u16 = SAMPLES_PER_SECOND * 5;

/// How long it has to stay at [`RECOVER_AT`] or above for each step back up:
/// four seconds.
pub const RECOVER_SAMPLES: u16 = SAMPLES_PER_SECOND * 4;

/// How many levels each step down (or up) moves the ceiling, a tenth of the
/// ramp.
pub const STEP: u8 = 15;

/// Ticks of rest before the filter forgets the cell, five minutes. Any
/// sooner and clicking off and on would restart it from the unloaded voltage
/// each time, which keeps a nearly flat cell running. Any later and a cell
/// that's sat for days gets judged on what it read before.
pub const FORGET_TICKS: u16 = TICKS_PER_SECOND * 60 * 5;

/// Voltages a real cell can give, in centivolts. A lithium-ion cell sits
/// between about 2.5 and 4.2 V, and this allows for the calibration being
/// off by a few tenths either way. Outside it, the ADC's set up wrong or the
/// read failed: a reading of zero would be VCC at infinity, and four
/// saturated conversions (the internal reference at or above VCC) are 1.78 V.
pub const PLAUSIBLE: core::ops::RangeInclusive<u16> = 200..=460;

/// [`PLAUSIBLE`] as readings, the ends swapped since a reading falls as the
/// voltage rises.
const PLAUSIBLE_READINGS: core::ops::RangeInclusive<u16> =
    highest_reading(*PLAUSIBLE.end() + 1) + 1..=highest_reading(*PLAUSIBLE.start());

// Zero means unseeded, so no sample may be zero, and the highest sample has to
// fit in sixteenths, plus the 7 the filter's rounding can leave on top.
const _: () = assert!(*PLAUSIBLE_READINGS.start() > 0);
const _: () = assert!(*PLAUSIBLE_READINGS.end() as u32 * 16 + 7 <= u16::MAX as u32);

/// Whether a raw reading could be a real cell.
pub fn plausible(reading: u16) -> bool {
    PLAUSIBLE_READINGS.contains(&reading)
}

/// Implausible samples in a row that [`Protection`] lets go before it assumes
/// the worst. The fourth in a row, a second's worth, acts.
const GLITCHES: u8 = 3;

/// The low-voltage verdict for one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run no higher than this level. [`LEVELS`] means no limit.
    Ceiling(u8),
    /// Turn off now. The cell's flat.
    Off,
}

/// Low-voltage protection: feed it each raw voltage reading (every
/// [`SAMPLE_TICKS`] ticks) and it says how bright the torch may be.
///
/// Samples go through a moving average (each one moves the filtered value an
/// eighth of the way, a time constant of about two seconds at four samples a
/// second), and the filtered value has to stay low for a whole second or more
/// before it acts, so one bad reading, or the sag as the FET comes on, does
/// nothing.
///
/// The average is of readings, which isn't quite the average of voltages,
/// since one's the reciprocal of the other. A steady cell gives the same
/// answer either way. On the way between two voltages this one reads lower:
/// about 1 cV partway from 3.40 to 3.05 V, but about 7 cV partway from 3.80 to
/// 2.90 V, which is two samples, half a second, sooner to act.
///
/// - Under [`STEP_DOWN_BELOW`] for [`STEP_SAMPLES`], the ceiling drops to
///   [`STEP`] levels under whichever is lower of the ceiling and the level the
///   torch is at, or the torch turns off if that's already level 1.
/// - Under [`OFF_BELOW`] for [`HOLD_SAMPLES`], the torch turns off if it's on
///   the linear channel. On the FET, several amps make the cell sag a lot, so
///   a low reading there may be a cell with plenty left: the ceiling drops to
///   [`LINEAR_MAX`] instead, where the 350 mA regulator sags the cell much
///   less. The hold starts again once the light's faded down to it, so it's
///   a second of samples on the linear channel that decide.
/// - At [`RECOVER_AT`] or above for [`RECOVER_SAMPLES`], the ceiling steps
///   back up by [`STEP`].
///
/// It never steps down or turns off while the torch is off, since there's no
/// load to sag under. It can recover then, but only while the switch keeps
/// the readings coming, since nothing's read during a [`Protection::rest`].
///
/// A sample that isn't [`plausible`] never reaches the filter. It can't
/// count towards stepping back up, and it doesn't restart the holds, so a
/// flat cell can't hide behind the odd bad reading. More than [`GLITCHES`]
/// in a row and the ADC can't be trusted to say the cell's fine, so the
/// ceiling drops to [`LINEAR_MAX`], on or off. It never turns the torch
/// off: a broken reading isn't evidence of a flat cell, and the linear
/// channel's 350 mA is gentle on one that is. Good samples at
/// [`RECOVER_AT`] or above step it back up as usual.
#[derive(Clone, Debug)]
pub struct Protection {
    /// The filtered reading in sixteenths of a count. Zero until the first
    /// plausible sample, which seeds it.
    filtered: u16,
    low_samples: u16,
    flat_samples: u16,
    high_samples: u16,
    /// Implausible samples in a row.
    implausible: u8,
    ceiling: u8,
    /// Ticks of rest since the last sample, up to [`FORGET_TICKS`].
    rested: u16,
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
            low_samples: 0,
            flat_samples: 0,
            high_samples: 0,
            implausible: 0,
            ceiling: LEVELS,
            rested: 0,
        }
    }

    /// One tick with nothing measuring the cell. The holds start again, as
    /// they do for any time off, and the ceiling stays, since only good
    /// readings raise it. The filter stays too, until [`FORGET_TICKS`] have
    /// gone by.
    pub fn rest(&mut self) {
        (self.low_samples, self.flat_samples, self.high_samples) = (0, 0, 0);
        self.implausible = 0;
        self.rested = self.rested.saturating_add(1);
        if self.rested >= FORGET_TICKS {
            self.filtered = 0;
        }
    }

    /// The filtered voltage in centivolts, or `None` before the first sample.
    #[cfg(any(test, feature = "testing"))]
    pub fn centivolts(&self) -> Option<u16> {
        match self.filtered {
            0 => None,
            filtered => Some(centivolts(filtered >> 4)),
        }
    }

    /// One voltage sample: the raw `reading` and the level the torch is at
    /// (`0` for off).
    pub fn sample(&mut self, reading: u16, level: u8) -> Verdict {
        self.rested = 0;
        if !plausible(reading) {
            self.high_samples = 0;
            self.implausible = self.implausible.saturating_add(1);
            if self.implausible > GLITCHES {
                self.ceiling = self.ceiling.min(LINEAR_MAX);
            }
            return Verdict::Ceiling(self.ceiling);
        }
        self.implausible = 0;
        let filtered = self.filter(reading);

        if filtered <= RECOVER_UP_TO {
            self.high_samples += 1;
            if self.high_samples >= RECOVER_SAMPLES {
                self.high_samples = 0;
                self.ceiling = self.ceiling.saturating_add(STEP).min(LEVELS);
            }
        } else {
            self.high_samples = 0;
        }

        if level == 0 {
            (self.low_samples, self.flat_samples) = (0, 0);
            return Verdict::Ceiling(self.ceiling);
        }
        self.low_samples = match filtered > STEP_DOWN_OVER {
            true => self.low_samples + 1,
            false => 0,
        };
        self.flat_samples = match filtered > OFF_OVER {
            // Dropped to the linear channel but still fading down to it:
            // the cell's still under the FET's load, so wait till it's
            // there and judge it on the linear channel's.
            true if level > LINEAR_MAX && self.ceiling <= LINEAR_MAX => 0,
            true => self.flat_samples + 1,
            false => 0,
        };

        let from = self.ceiling.min(level);
        if self.flat_samples >= HOLD_SAMPLES {
            (self.low_samples, self.flat_samples) = (0, 0);
            if from <= LINEAR_MAX {
                return Verdict::Off;
            }
            self.ceiling = LINEAR_MAX;
        } else if self.low_samples >= STEP_SAMPLES {
            self.low_samples = 0;
            if from == 1 {
                return Verdict::Off;
            }
            self.ceiling = from.saturating_sub(STEP).max(1);
        }
        Verdict::Ceiling(self.ceiling)
    }

    /// Folds a sample into the moving average and returns the result in
    /// counts. In sixteenths, moving an eighth of the way is taking off an
    /// eighth and adding the sample times two. Only plausible samples get
    /// here, so the sum stays inside a `u16` and the seed isn't zero.
    fn filter(&mut self, reading: u16) -> u16 {
        self.filtered = match self.filtered {
            0 => reading << 4,
            filtered => filtered - (filtered >> 3) + (reading << 1),
        };
        self.filtered >> 4
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::testing::voltage_reading;
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

    #[test]
    fn samples_come_four_times_a_second() {
        assert_eq!(SAMPLES_PER_SECOND, 4);
    }

    /// Samples in `seconds` of real time.
    const fn secs(seconds: u16) -> u16 {
        seconds * SAMPLES_PER_SECOND
    }

    /// The raw reading for a cell at `cv`, so the tests can go on talking in
    /// centivolts. `0` and `u16::MAX` stand for a reading of zero, which is
    /// what used to come out of `centivolts` as `u16::MAX`.
    fn raw(cv: u16) -> u16 {
        match cv {
            0 | u16::MAX => 0,
            cv => voltage_reading(cv),
        }
    }

    /// One sample of a cell at `cv`.
    fn sample(lvp: &mut Protection, cv: u16, level: u8) -> Verdict {
        lvp.sample(raw(cv), level)
    }

    /// Feeds `samples` samples at a steady voltage and level, and returns
    /// every verdict.
    fn run(lvp: &mut Protection, cv: u16, level: u8, samples: u16) -> Vec<Verdict> {
        (0..samples).map(|_| sample(lvp, cv, level)).collect()
    }

    fn last(lvp: &mut Protection, cv: u16, level: u8, samples: u16) -> Verdict {
        *run(lvp, cv, level, samples).last().unwrap()
    }

    fn all(verdicts: &[Verdict], expected: Verdict) -> bool {
        verdicts.iter().all(|&v| v == expected)
    }

    const NONE: Verdict = Verdict::Ceiling(LEVELS);

    #[test]
    fn a_full_cell_has_no_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(lvp.centivolts(), None);
        assert!(all(&run(&mut lvp, 410, LEVELS, secs(600)), NONE));
        assert_eq!(lvp.centivolts(), Some(410));
    }

    #[test]
    fn the_first_sample_seeds_the_filter() {
        let mut lvp = Protection::new();
        sample(&mut lvp, 290, 100);
        assert_eq!(lvp.centivolts(), Some(290));
    }

    #[test]
    fn steps_down_after_5_seconds_below_3_1_volts() {
        let mut lvp = Protection::new();
        let verdicts = run(&mut lvp, 305, 100, secs(5));
        let (before, after) = verdicts.split_at(usize::from(secs(5)) - 1);
        assert!(all(before, NONE));
        assert_eq!(after, [Verdict::Ceiling(100 - STEP)]);
    }

    #[test]
    fn exactly_3_1_volts_is_not_low() {
        let mut lvp = Protection::new();
        assert!(all(&run(&mut lvp, STEP_DOWN_BELOW, 100, secs(600)), NONE));
    }

    #[test]
    fn keeps_stepping_down_every_5_seconds_then_turns_off() {
        let mut lvp = Protection::new();
        for ceiling in [85, 70, 55, 40, 25, 10, 1] {
            assert_eq!(last(&mut lvp, 305, 100, secs(5)), Verdict::Ceiling(ceiling));
        }
        assert_eq!(last(&mut lvp, 305, 100, secs(5)), Verdict::Off);
    }

    #[test]
    fn steps_from_the_level_when_it_is_under_the_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 40, secs(5)), Verdict::Ceiling(25));
    }

    #[test]
    fn turns_off_after_a_second_below_2_99_volts_on_the_linear_channel() {
        let mut lvp = Protection::new();
        let verdicts = run(&mut lvp, 298, LINEAR_MAX, secs(1));
        let (before, after) = verdicts.split_at(usize::from(secs(1)) - 1);
        assert!(all(before, NONE));
        assert_eq!(after, [Verdict::Off]);
    }

    #[test]
    fn drops_to_the_linear_channel_first_when_on_the_fet() {
        let mut lvp = Protection::new();
        let fet = LINEAR_MAX + 1;
        assert_eq!(
            last(&mut lvp, 290, fet, secs(1)),
            Verdict::Ceiling(LINEAR_MAX)
        );
        // If the cell recovers there, it stays on.
        assert!(all(
            &run(&mut lvp, 315, LINEAR_MAX, secs(60)),
            Verdict::Ceiling(LINEAR_MAX)
        ));
        // If it doesn't, it turns off a second later.
        let verdicts = run(&mut lvp, 250, LINEAR_MAX, secs(3));
        assert!(verdicts.contains(&Verdict::Off));
    }

    #[test]
    fn waits_for_the_light_to_reach_the_linear_channel_before_turning_off() {
        let mut lvp = Protection::new();
        let fet = LINEAR_MAX + 1;
        assert_eq!(
            last(&mut lvp, 250, LEVELS, secs(1)),
            Verdict::Ceiling(LINEAR_MAX)
        );
        // Still fading down to it. The fade takes a second or so; this is four.
        assert!(all(
            &run(&mut lvp, 250, fet, secs(4)),
            Verdict::Ceiling(LINEAR_MAX)
        ));
        // There, a second of flat samples turns it off.
        let verdicts = run(&mut lvp, 250, LINEAR_MAX, secs(1));
        let (before, after) = verdicts.split_at(usize::from(secs(1)) - 1);
        assert!(all(before, Verdict::Ceiling(LINEAR_MAX)));
        assert_eq!(after, [Verdict::Off]);
    }

    #[test]
    fn exactly_2_99_volts_steps_down_rather_than_off() {
        let mut lvp = Protection::new();
        let verdicts = run(&mut lvp, OFF_BELOW, 50, secs(5));
        assert!(!verdicts.contains(&Verdict::Off));
        assert_eq!(verdicts.last(), Some(&Verdict::Ceiling(35)));
    }

    #[test]
    fn one_bad_reading_does_nothing() {
        let mut lvp = Protection::new();
        run(&mut lvp, 380, 50, secs(10));
        for glitch in [0, 1, 100, 298] {
            assert_eq!(sample(&mut lvp, glitch, 50), NONE);
            assert!(all(&run(&mut lvp, 380, 50, secs(60)), NONE));
        }
    }

    #[test]
    fn half_a_second_of_zero_readings_does_nothing() {
        let mut lvp = Protection::new();
        run(&mut lvp, 380, 50, secs(10));
        assert!(all(&run(&mut lvp, 0, 50, secs(1) / 2), NONE));
        assert!(all(&run(&mut lvp, 380, 50, secs(60)), NONE));
    }

    #[test]
    fn the_filter_takes_a_couple_of_seconds_to_follow_a_drop() {
        let mut lvp = Protection::new();
        run(&mut lvp, 360, 50, secs(10));
        let verdicts = run(&mut lvp, 250, 50, secs(10));
        // The filtered voltage passes 2.99 V a second or two in, then the
        // hold has to run out before the light goes off. Averaging readings
        // rather than volts gets there two samples sooner on a drop this big,
        // hence the lower bound.
        let off = verdicts.iter().position(|&v| v == Verdict::Off).unwrap();
        let off = u16::try_from(off).unwrap();
        assert!(off >= secs(2) - 1 && off < secs(3), "{off}");
    }

    #[test]
    fn a_voltage_back_above_3_1_volts_restarts_the_hold() {
        let mut lvp = Protection::new();
        run(&mut lvp, 305, 100, secs(3));
        // Enough good samples to lift the filtered value over 3.1 V.
        run(&mut lvp, 320, 100, secs(2));
        // Without the restart this would be well past the hold.
        assert!(all(&run(&mut lvp, 305, 100, secs(5) - 1), NONE));
        assert!(run(&mut lvp, 305, 100, secs(5)).contains(&Verdict::Ceiling(85)));
    }

    #[test]
    fn does_nothing_while_the_torch_is_off() {
        let mut lvp = Protection::new();
        assert!(all(&run(&mut lvp, 250, 0, secs(600)), NONE));
        // And the hold starts from scratch when it comes on.
        assert_eq!(last(&mut lvp, 250, 50, secs(1) - 1), NONE);
        assert_eq!(sample(&mut lvp, 250, 50), Verdict::Off);
    }

    #[test]
    fn turning_off_keeps_the_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 60, secs(5)), Verdict::Ceiling(45));
        assert!(run(&mut lvp, 250, 45, secs(5)).contains(&Verdict::Off));
        assert_eq!(last(&mut lvp, 290, 0, secs(1)), Verdict::Ceiling(45));
    }

    #[test]
    fn a_long_rest_keeps_the_ceiling_and_forgets_the_filter() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 60, secs(5)), Verdict::Ceiling(45));
        for _ in 0..FORGET_TICKS {
            lvp.rest();
        }
        assert_eq!(lvp.centivolts(), None);
        // The next sample stands alone, rather than filtering into 3.05 V.
        assert_eq!(sample(&mut lvp, 380, 0), Verdict::Ceiling(45));
        assert_eq!(lvp.centivolts(), Some(380));
    }

    #[test]
    fn a_short_rest_keeps_the_filter() {
        let mut lvp = Protection::new();
        run(&mut lvp, 290, 50, secs(1) - 1);
        for _ in 0..FORGET_TICKS - 1 {
            lvp.rest();
        }
        assert_eq!(lvp.centivolts(), Some(290));
    }

    #[test]
    fn a_sample_starts_the_wait_to_forget_again() {
        let mut lvp = Protection::new();
        for _ in 0..3 {
            for _ in 0..FORGET_TICKS - 1 {
                lvp.rest();
            }
            sample(&mut lvp, 380, 0);
        }
        assert_eq!(lvp.centivolts(), Some(380));
    }

    #[test]
    fn a_rest_starts_the_hold_afresh() {
        let mut lvp = Protection::new();
        run(&mut lvp, 250, 50, secs(1) - 1);
        lvp.rest();
        assert_eq!(last(&mut lvp, 250, 50, secs(1) - 1), NONE);
        assert_eq!(sample(&mut lvp, 250, 50), Verdict::Off);
    }

    #[test]
    fn hysteresis_holds_the_ceiling_between_3_1_and_3_3_volts() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, LEVELS, secs(5)), Verdict::Ceiling(135));
        let resting = Verdict::Ceiling(135);
        assert!(all(&run(&mut lvp, 329, 135, secs(600)), resting));
        assert!(all(&run(&mut lvp, 310, 135, secs(600)), resting));
    }

    #[test]
    fn recovers_a_step_every_4_seconds_at_3_3_volts() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 50, secs(5)), Verdict::Ceiling(35));
        // The filter needs a couple of seconds to climb from 3.05 to 3.3 V.
        let verdicts = run(&mut lvp, 340, 35, secs(10));
        let up = verdicts
            .iter()
            .position(|&v| v == Verdict::Ceiling(50))
            .unwrap();
        let up = u16::try_from(up).unwrap();
        assert!(up >= secs(4) && up < secs(7), "{up}");
        assert_eq!(last(&mut lvp, 340, 0, secs(4)), Verdict::Ceiling(65));
        // Back to no limit, and no further.
        assert_eq!(last(&mut lvp, 340, 0, secs(120)), NONE);
    }

    #[test]
    fn the_filter_has_room_for_the_most_plausible_reading() {
        let mut lvp = Protection::new();
        // The highest reading is the lowest voltage, now.
        assert!(u32::from(*PLAUSIBLE_READINGS.end()) << 4 <= u32::from(u16::MAX));
        assert_eq!(lvp.sample(*PLAUSIBLE_READINGS.end(), 0), NONE);
        assert_eq!(last(&mut lvp, *PLAUSIBLE.start(), 0, secs(60)), NONE);
        assert_eq!(lvp.centivolts(), Some(*PLAUSIBLE.start()));
        // And from there all the way up to the top.
        assert_eq!(last(&mut lvp, *PLAUSIBLE.end(), 0, secs(60)), NONE);
        assert_eq!(lvp.centivolts(), Some(*PLAUSIBLE.end()));
    }

    #[test]
    fn plausible_covers_a_real_cell_and_nothing_broken() {
        for cv in [250, 299, 310, 420, 435] {
            assert!(plausible(raw(cv)), "{cv}");
        }
        // A zero reading, four saturated conversions, and either side.
        for reading in [0, 4 * 1023, raw(199), raw(461)] {
            assert!(!plausible(reading), "{reading}");
        }
    }

    #[test]
    fn plausible_rejects_what_it_rejected_in_centivolts() {
        for reading in 0..=u16::MAX {
            let cv = centivolts(reading);
            assert_eq!(plausible(reading), PLAUSIBLE.contains(&cv), "{reading}");
        }
        // Too small for centivolts to answer, and 1.78 V saturated.
        for reading in 0..MIN_READING {
            assert!(!plausible(reading), "{reading}");
        }
        assert_eq!(centivolts(4 * 1023), 178);
        assert!(!plausible(4 * 1023));
    }

    #[test]
    fn the_thresholds_land_where_centivolts_says_for_every_reading() {
        for reading in 1..=u16::MAX {
            let cv = centivolts(reading);
            assert_eq!(reading > STEP_DOWN_OVER, cv < STEP_DOWN_BELOW, "{reading}");
            assert_eq!(reading > OFF_OVER, cv < OFF_BELOW, "{reading}");
            assert_eq!(reading <= RECOVER_UP_TO, cv >= RECOVER_AT, "{reading}");
        }
    }

    #[test]
    fn the_thresholds_in_counts() {
        assert_eq!(RECOVER_UP_TO, 2023);
        assert_eq!(STEP_DOWN_OVER, 2168);
        assert_eq!(OFF_OVER, 2256);
        assert_eq!(PLAUSIBLE_READINGS, 1408..=3575);
    }

    #[test]
    fn a_steady_reading_on_a_boundary_goes_the_way_centivolts_says() {
        /// The last verdict of `samples` samples of a steady raw `reading`.
        fn steady(lvp: &mut Protection, reading: u16, level: u8, samples: u16) -> Verdict {
            (0..samples).fold(NONE, |_, _| lvp.sample(reading, level))
        }

        let mut lvp = Protection::new();
        assert_eq!(steady(&mut lvp, STEP_DOWN_OVER, 100, secs(600)), NONE);
        let mut lvp = Protection::new();
        assert_eq!(
            steady(&mut lvp, STEP_DOWN_OVER + 1, 100, secs(5)),
            Verdict::Ceiling(100 - STEP)
        );

        let mut lvp = Protection::new();
        assert_ne!(steady(&mut lvp, OFF_OVER, 50, secs(1)), Verdict::Off);
        let mut lvp = Protection::new();
        assert_eq!(steady(&mut lvp, OFF_OVER + 1, 50, secs(1)), Verdict::Off);

        // Off, so nothing steps down, and a minute for the filter to settle.
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 50, secs(5)), Verdict::Ceiling(35));
        assert_eq!(steady(&mut lvp, RECOVER_UP_TO, 0, secs(60)), NONE);
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 50, secs(5)), Verdict::Ceiling(35));
        assert_eq!(
            steady(&mut lvp, RECOVER_UP_TO + 1, 0, secs(60)),
            Verdict::Ceiling(35)
        );
    }

    #[test]
    fn implausible_samples_never_reach_the_filter() {
        let mut lvp = Protection::new();
        sample(&mut lvp, u16::MAX, 100);
        sample(&mut lvp, 0, 100);
        assert_eq!(lvp.centivolts(), None);
        sample(&mut lvp, 400, 100);
        sample(&mut lvp, u16::MAX, 100);
        assert_eq!(lvp.centivolts(), Some(400));
    }

    #[test]
    fn a_second_of_garbage_drops_to_the_linear_channel_and_never_off() {
        for level in [0, 50, LINEAR_MAX, LEVELS] {
            let mut lvp = Protection::new();
            let verdicts = run(&mut lvp, u16::MAX, level, u16::from(GLITCHES));
            assert!(all(&verdicts, NONE), "level {level}");
            assert_eq!(
                sample(&mut lvp, u16::MAX, level),
                Verdict::Ceiling(LINEAR_MAX)
            );
            assert!(all(
                &run(&mut lvp, 0, level, secs(600)),
                Verdict::Ceiling(LINEAR_MAX)
            ));
        }
    }

    #[test]
    fn garbage_keeps_a_lower_ceiling() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 50, secs(5)), Verdict::Ceiling(35));
        assert_eq!(last(&mut lvp, 0, 35, secs(10)), Verdict::Ceiling(35));
    }

    #[test]
    fn good_samples_bring_it_back_up_after_garbage() {
        let mut lvp = Protection::new();
        run(&mut lvp, 0, LEVELS, secs(2));
        let verdicts = run(&mut lvp, 400, LEVELS, secs(30));
        assert!(verdicts.contains(&Verdict::Ceiling(LINEAR_MAX + STEP)));
        assert_eq!(verdicts.last(), Some(&NONE));
    }

    #[test]
    fn garbage_never_counts_towards_stepping_up() {
        let mut lvp = Protection::new();
        assert_eq!(last(&mut lvp, 305, 50, secs(5)), Verdict::Ceiling(35));
        run(&mut lvp, 340, 35, secs(3));
        // Every third sample bad: never four good seconds in a row.
        for _ in 0..200 {
            sample(&mut lvp, 340, 35);
            sample(&mut lvp, 340, 35);
            assert_eq!(sample(&mut lvp, 0, 35), Verdict::Ceiling(35));
        }
    }

    #[test]
    fn garbage_doesnt_restart_the_hold_on_a_flat_cell() {
        let mut lvp = Protection::new();
        let mut verdicts = Vec::new();
        // Every other sample bad: the flat ones still add up.
        for _ in 0..secs(2) {
            verdicts.push(sample(&mut lvp, 290, 50));
            verdicts.push(sample(&mut lvp, u16::MAX, 50));
        }
        assert!(verdicts.contains(&Verdict::Off));
    }
}
