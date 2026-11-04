//! The firmware never converts a reading to volts or degrees. It converts the
//! thresholds to raw ADC counts instead, the voltage ones at compile time and the
//! temperature ones once at boot, so the running code only compares integers.
//!
//! Both sensors share the 1.1 V reference, which means the reference never has to
//! be switched and settle between readings. Voltage reads the 1.1 V reference
//! against VCC, so the count goes up as the cell goes down. Temperature reads the
//! internal sensor against the 1.1 V reference. All counts are four 10-bit
//! results accumulated, so 0 to 4092.

use crate::governor::Limits;
use crate::ramp::{FLOOR, LINEAR_TOP};

const REFERENCE_MV: u32 = 1100;
const FULL_SCALE: u32 = 4092;
/// Cell to VCC, across the reverse-polarity diode. Likely, see docs/hardware.md.
const DIODE_MV: u32 = 250;

/// The count that a cell at this voltage reads.
pub const fn battery_counts(cell_mv: u32) -> u16 {
    (REFERENCE_MV * FULL_SCALE / (cell_mv - DIODE_MV)) as u16
}

pub const BATTERY: Limits = Limits {
    trip: battery_counts(3000),
    clear: battery_counts(3200),
    // 8 counts is about 13 mV, so 0.1 V under takes about 8 levels a step.
    shift: 3,
    floor: FLOOR,
};

const LOWEST_TENTHS: u8 = 20;
const HIGHEST_TENTHS: u8 = 45;
const TENTHS_ENTRIES: usize = (HIGHEST_TENTHS - LOWEST_TENTHS + 1) as usize;

/// The count at the bottom of each tenth of a volt, rounding to nearest.
static TENTHS: [u16; TENTHS_ENTRIES] = {
    let mut table = [0; TENTHS_ENTRIES];
    let mut entry = 0;
    while entry < TENTHS_ENTRIES {
        table[entry] = battery_counts((LOWEST_TENTHS as u32 + entry as u32) * 100 - 50);
        entry += 1;
    }
    table
};

/// A smoothed battery count as tenths of a volt, for the readout. A table search
/// rather than a division, because this is the only place that needs volts.
pub fn tenths_of_a_volt(counts: u16) -> u8 {
    let mut tenths = HIGHEST_TENTHS;
    for &bottom in TENTHS.iter().rev() {
        if counts <= bottom {
            return tenths;
        }
        tenths -= 1;
    }
    LOWEST_TENTHS
}

const fn celsius(degrees: u16) -> u16 {
    degrees + 273
}

/// Turns the datasheet's kelvin = ((raw - offset) × gain + 0x80) >> 8 around, using
/// this chip's factory calibration from SIGROW.
///
/// Divides by repeated subtraction, because this runs twice at boot and a real
/// 32-bit division would pull in over 1 KB of `compiler_builtins`.
pub fn heat_counts(kelvin: u16, gain: u8, offset: i8) -> u16 {
    // Past this, the offset can't bring the result back into 10 bits.
    const MOST: i16 = 1023 + 128;
    let mut scaled = (u32::from(kelvin) << 8).saturating_sub(0x80);
    let gain = u32::from(gain.max(1));
    let mut raw = 0;
    while scaled >= gain && raw < MOST {
        scaled -= gain;
        raw += 1;
    }
    ((raw + i16::from(offset)).clamp(0, 1023) as u16) << 2
}

/// Limits for this chip, built once at boot.
pub fn heat_limits(gain: u8, offset: i8) -> Limits {
    Limits {
        trip: heat_counts(celsius(50), gain, offset),
        clear: heat_counts(celsius(47), gain, offset),
        // About 4.5 °C over adds a level to the step.
        shift: 5,
        floor: LINEAR_TOP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datasheet's formula, the way round it's written, on a 10-bit result.
    fn kelvin(counts: u16, gain: u8, offset: i8) -> i32 {
        let raw = i32::from(counts >> 2);
        ((raw - i32::from(offset)) * i32::from(gain) + 0x80) >> 8
    }

    #[test]
    fn a_flatter_cell_reads_higher() {
        assert!(battery_counts(3000) > battery_counts(3200));
        assert_eq!(battery_counts(3000), 1636);
    }

    #[test]
    fn counts_stay_in_range_down_to_a_flat_cell() {
        assert!(u32::from(battery_counts(1400)) < FULL_SCALE);
    }

    #[test]
    fn the_readout_lands_on_the_nearest_tenth() {
        for tenths in 25..=42 {
            let counts = battery_counts(u32::from(tenths) * 100);
            assert_eq!(tenths_of_a_volt(counts), tenths);
        }
    }

    #[test]
    fn the_readout_clamps_at_both_ends() {
        assert_eq!(tenths_of_a_volt(0), HIGHEST_TENTHS);
        assert_eq!(tenths_of_a_volt(4092), LOWEST_TENTHS);
    }

    #[test]
    fn heat_counts_invert_the_datasheet_formula() {
        for (gain, offset) in [(160, -10), (140, 20), (200, 0), (255, -128)] {
            for target in [300, 320, 323, 340] {
                let counts = heat_counts(target, gain, offset);
                let back = kelvin(counts, gain, offset);
                assert!(
                    (back - i32::from(target)).abs() <= 1,
                    "{gain} {offset} {target}: {back}"
                );
            }
        }
    }

    #[test]
    fn a_blank_calibration_row_does_not_divide_by_zero() {
        assert_eq!(heat_counts(323, 0, 0), 1023 << 2);
    }

    #[test]
    fn clear_sits_under_trip_for_both() {
        const { assert!(BATTERY.clear < BATTERY.trip) };
        let heat = heat_limits(160, 0);
        assert!(heat.clear < heat.trip);
    }
}
