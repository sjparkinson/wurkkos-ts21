//! The MCU's internal temperature sensor, turned into sixteenths of a kelvin.
//!
//! The datasheet (DS40002204A, section 30.3.2.6) gives kelvin from one
//! conversion as `((reading - offset) * gain + 0x80) >> 8`, with the offset
//! and gain read from `SIGROW.TEMPSENSE1` (signed) and `SIGROW.TEMPSENSE0`
//! (unsigned). That's whole kelvin, and at about 0.55 K a count most of what
//! rounds away is real. `thermal` predicts from how the temperature's moving,
//! so a reading that sits still and then jumps a kelvin misleads it.
//!
//! So the ADC accumulates four conversions, and this works in the sum `s`.
//! Four conversions are four times the reading, and sixteenths are sixteen
//! times the kelvin, which leaves:
//!
//! ```text
//! sixteenths  ==  ((s - 4 * offset) * gain + 0x20) >> 6
//! ```
//!
//! The product runs to 21 bits, and there's no runtime 32-bit multiply on
//! this chip (see docs/toolchain.md). It doesn't need one. Split the
//! difference `d` into its high and low bytes:
//!
//! ```text
//! (d * g + 0x20) >> 6  ==  hi * g * 4 + ((lo * g + 0x20) >> 6)
//! ```
//!
//! `hi * g * 256` has nothing in its low six bits, so taking it out of the
//! shift loses nothing and the answer is exact. `lo * g + 0x20` is at most
//! 65,057. Four full-scale conversions and the lowest offset make `d` 4,604
//! at most, but capping it at `MAX_DIFFERENCE` is what keeps every step inside
//! a `u16` for any reading at all: `hi * g * 4` is then at most 64,260, and
//! the sum at most 65,276.

/// The factory calibration for this chip's temperature sensor, from
/// `SIGROW.TEMPSENSE1` (the offset) and `SIGROW.TEMPSENSE0` (the gain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub offset: i8,
    pub gain: u8,
}

impl Calibration {
    /// Sixteenths of a kelvin from four accumulated conversions, as
    /// [`sixteenths`] works it out.
    pub fn sixteenths(self, sum: u16) -> u16 {
        sixteenths(sum, self.offset, self.gain)
    }
}

/// The biggest difference [`sixteenths`] works with. It's more than three
/// times what the ADC can produce, and anything over it is garbage anyway.
const MAX_DIFFERENCE: u16 = 0x3FFF;

/// Sixteenths of a kelvin from four accumulated temperature sensor
/// conversions, using the factory calibration from the signature row. A sum
/// below the offset (which would mean a broken sensor or a bad read) comes
/// out as 0 rather than wrapping.
pub fn sixteenths(sum: u16, offset: i8, gain: u8) -> u16 {
    let d = sum
        .saturating_add_signed(-4 * i16::from(offset))
        .min(MAX_DIFFERENCE);
    let (hi, lo, g) = (d >> 8, d & 0xFF, u16::from(gain));
    hi * g * 4 + ((lo * g + 0x20) >> 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sum above, done the slow way with room to spare.
    fn reference(sum: u16, offset: i8, gain: u8) -> u16 {
        let d = (i32::from(sum) - 4 * i32::from(offset)).clamp(0, i32::from(MAX_DIFFERENCE));
        ((d * i32::from(gain) + 0x20) >> 6) as u16
    }

    #[test]
    fn matches_the_sum_for_every_accumulated_reading_and_calibration() {
        for sum in 0..=4 * 1023 {
            for offset in i8::MIN..=i8::MAX {
                for gain in (u8::MIN..=u8::MAX).step_by(5) {
                    assert_eq!(
                        sixteenths(sum, offset, gain),
                        reference(sum, offset, gain),
                        "sum {sum}, offset {offset}, gain {gain}",
                    );
                }
            }
        }
    }

    #[test]
    fn the_split_is_exact_for_every_16_bit_sum() {
        for sum in u16::MIN..=u16::MAX {
            for gain in u8::MIN..=u8::MAX {
                assert_eq!(
                    sixteenths(sum, 0, gain),
                    reference(sum, 0, gain),
                    "sum {sum}, gain {gain}"
                );
            }
        }
    }

    #[test]
    fn four_conversions_land_within_half_a_kelvin_of_the_datasheet() {
        // Its formula for one conversion rounds to the nearest kelvin, so
        // four of the same conversion can only differ from it by that.
        for reading in 0..1024_i32 {
            for offset in i8::MIN..=i8::MAX {
                for gain in u8::MIN..=u8::MAX {
                    let d = (reading - i32::from(offset)).max(0);
                    let datasheet = (d * i32::from(gain) + 0x80) >> 8;
                    let ours = i32::from(sixteenths(4 * reading as u16, offset, gain));
                    assert!(
                        (ours - 16 * datasheet).abs() <= 8,
                        "reading {reading}, offset {offset}, gain {gain}",
                    );
                }
            }
        }
    }

    #[test]
    fn the_calibration_does_the_same_sums() {
        let calibration = Calibration {
            offset: -10,
            gain: 141,
        };
        assert_eq!(calibration.sixteenths(2124), sixteenths(2124, -10, 141));
        assert_eq!(calibration.sixteenths(2124), 298 * 16);
    }

    #[test]
    fn one_count_of_the_sum_is_a_fraction_of_a_kelvin() {
        let at = |sum| sixteenths(sum, -10, 141);
        // 141 / 64 is a little over two sixteenths, about 0.14 K.
        assert_eq!(at(2125) - at(2124), 2);
        assert_eq!(at(2128) - at(2124), 8);
    }

    #[test]
    fn a_sum_below_the_offset_is_zero() {
        assert_eq!(sixteenths(10, 20, 200), 0);
    }

    #[test]
    fn a_sum_nothing_could_produce_stays_far_from_plausible() {
        assert!(sixteenths(u16::MAX, -128, 141) > 423 * 16);
    }
}
