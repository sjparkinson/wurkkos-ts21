//! The MCU's internal temperature sensor, turned into kelvin.
//!
//! The datasheet (DS40002204A, section 30.3.2.6) gives the conversion as
//! `((reading - offset) * gain + 0x80) >> 8`, with the offset and gain read
//! from `SIGROW.TEMPSENSE1` (signed) and `SIGROW.TEMPSENSE0` (unsigned). The
//! product runs to 19 bits for a 10-bit reading, and there's no runtime 32-bit
//! multiply on this chip (see docs/toolchain.md).
//!
//! It doesn't need one. Split the difference `d` into its high and low bytes:
//!
//! ```text
//! (d * g + 0x80) >> 8  ==  hi * g + ((lo * g + 0x80) >> 8)
//! ```
//!
//! `hi * g * 256` has nothing in its low byte, so taking it out of the shift
//! loses nothing and the answer is exact. `lo * g + 0x80` is at most 65,153
//! and `hi * g` at most 65,025, and the sum is at most 65,279, so every step
//! fits in a `u16` for any `d` at all.

/// The factory calibration for this chip's temperature sensor, from
/// `SIGROW.TEMPSENSE1` (the offset) and `SIGROW.TEMPSENSE0` (the gain).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub offset: i8,
    pub gain: u8,
}

impl Calibration {
    /// Kelvin from one conversion, as [`kelvin`] works it out.
    pub fn kelvin(self, reading: u16) -> u16 {
        kelvin(reading, self.offset, self.gain)
    }
}

/// Kelvin from one temperature sensor conversion, using the factory
/// calibration from the signature row. A reading below the offset (which
/// would mean a broken sensor or a bad read) comes out as 0 K rather than
/// wrapping.
pub fn kelvin(reading: u16, offset: i8, gain: u8) -> u16 {
    let d = reading.saturating_add_signed(-i16::from(offset));
    let (hi, lo, g) = (d >> 8, d & 0xFF, u16::from(gain));
    hi * g + ((lo * g + 0x80) >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datasheet's formula, done the slow way with room to spare.
    fn reference(reading: u16, offset: i8, gain: u8) -> u16 {
        let d = (i32::from(reading) - i32::from(offset)).clamp(0, i32::from(u16::MAX));
        ((d * i32::from(gain) + 0x80) >> 8) as u16
    }

    #[test]
    fn matches_the_datasheet_for_every_10_bit_reading_and_calibration() {
        for reading in 0..1024 {
            for offset in i8::MIN..=i8::MAX {
                for gain in u8::MIN..=u8::MAX {
                    assert_eq!(
                        kelvin(reading, offset, gain),
                        reference(reading, offset, gain),
                        "reading {reading}, offset {offset}, gain {gain}",
                    );
                }
            }
        }
    }

    #[test]
    fn the_split_is_exact_for_every_16_bit_difference() {
        for d in u16::MIN..=u16::MAX {
            for gain in u8::MIN..=u8::MAX {
                assert_eq!(
                    kelvin(d, 0, gain),
                    reference(d, 0, gain),
                    "d {d}, gain {gain}"
                );
            }
        }
    }

    #[test]
    fn the_calibration_does_the_same_sums() {
        let calibration = Calibration {
            offset: -10,
            gain: 141,
        };
        assert_eq!(calibration.kelvin(531), kelvin(531, -10, 141));
        assert_eq!(calibration.kelvin(531), 298);
    }

    #[test]
    fn a_reading_below_the_offset_is_zero_kelvin() {
        assert_eq!(kelvin(10, 20, 200), 0);
    }
}
