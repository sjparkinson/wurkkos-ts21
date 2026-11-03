//! The checksum the chip's CRCSCAN checks flash against at power-on.
//!
//! DS40002204A §27.2: CRC-16-CCITT (x¹⁶ + x¹² + x⁵ + 1, so 0x1021), the
//! checksum register starting at 0xFFFF, each byte fed in most significant
//! bit first. That's the parameter set usually called CRC-16/CCITT-FALSE
//! (or CRC-16/IBM-3740): no reflection and no final XOR. §27.3.2.1 and
//! table 27-1 put the checksum for a whole-flash scan in the last two bytes
//! of flash, high byte at FLASHEND - 1.
//!
//! With no final XOR, running the same CRC over the data and then the
//! checksum high byte first always ends at 0, which is how the scan can
//! "pass if the last bytes contain the correct checksum". `sim.rs` models
//! the scan that way, over all 16 KB, so it checks [`sign`] rather than
//! repeating it.
//!
//! None of this has been checked against a real chip yet.

use crate::updi::map::FLASH_SIZE;

/// CRC-16-CCITT as CRCSCAN computes it: seed 0xFFFF, MSB first, no final
/// XOR.
pub fn crc16(bytes: impl IntoIterator<Item = u8>) -> u16 {
    bytes.into_iter().fold(0xFFFF, |crc, byte| {
        (0..8).fold(crc ^ u16::from(byte) << 8, |crc, _| {
            if crc & 0x8000 != 0 {
                crc << 1 ^ 0x1021
            } else {
                crc << 1
            }
        })
    })
}

/// Where the checksum goes: the last two bytes of flash.
pub const CHECKSUM_AT: usize = FLASH_SIZE - 2;

/// The firmware as it's flashed: padded to all of flash with 0xFF, which is
/// what erased flash reads as, and the checksum of everything before it in
/// the last two bytes, high byte first.
pub fn sign(image: &[u8]) -> Result<Vec<u8>, String> {
    if image.len() > CHECKSUM_AT {
        return Err(format!(
            "the image is {} bytes, which leaves no room for the CRC at {CHECKSUM_AT:#06x}",
            image.len()
        ));
    }
    let mut flash = image.to_vec();
    flash.resize(CHECKSUM_AT, 0xFF);
    let checksum = crc16(flash.iter().copied());
    flash.extend(checksum.to_be_bytes());
    Ok(flash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_crc_16_ccitt_false_check_value() {
        // The standard check: the ASCII digits 1 to 9.
        assert_eq!(crc16(*b"123456789"), 0x29B1);
        assert_eq!(crc16([]), 0xFFFF, "the seed");
    }

    #[test]
    fn is_the_unaugmented_crc_not_the_augmented_one() {
        // Published values for "A" with the 0xFFFF seed: 0xB915 for this
        // CRC, 0x9479 for the "augmented" CCITT that feeds 16 zero bits
        // after the message. The datasheet's figure 27-1 XORs data in at
        // the top of the register, which is this one.
        assert_eq!(crc16(*b"A"), 0xB915);
    }

    #[test]
    fn feeds_bytes_most_significant_bit_first() {
        // 0x80 goes into the top of the register first; worked by hand,
        // three shifts with the polynomial and five without. A reflected
        // CRC would treat it like 0x01.
        assert_eq!(crc16([0x80]), 0x7078);
        assert_ne!(crc16([0x80]), crc16([0x01]));
    }

    #[test]
    fn a_signed_image_fills_flash_and_ends_in_its_checksum() {
        let image = [0x0C, 0x94, 0x3E, 0x00];
        let flash = sign(&image).unwrap();
        assert_eq!(flash.len(), FLASH_SIZE);
        assert_eq!(flash[..4], image);
        assert!(flash[4..CHECKSUM_AT].iter().all(|&b| b == 0xFF));
        let checksum = crc16(flash[..CHECKSUM_AT].iter().copied());
        assert_eq!(
            flash[CHECKSUM_AT..],
            checksum.to_be_bytes(),
            "high byte first"
        );
    }

    #[test]
    fn the_whole_of_a_signed_flash_checks_to_zero() {
        for image in [&[][..], &[0x12, 0x34], &[0xAA; 4000]] {
            let flash = sign(image).unwrap();
            assert_eq!(crc16(flash.iter().copied()), 0);
            // A flipped bit anywhere, the checksum included, doesn't.
            for at in [0, 1000, CHECKSUM_AT - 1, CHECKSUM_AT, FLASH_SIZE - 1] {
                let mut bad = flash.clone();
                bad[at] ^= 0x10;
                assert_ne!(crc16(bad.iter().copied()), 0, "{at}");
            }
        }
    }

    #[test]
    fn refuses_an_image_that_reaches_the_checksum() {
        assert!(sign(&vec![0; CHECKSUM_AT]).is_ok());
        let err = sign(&vec![0; CHECKSUM_AT + 1]).unwrap_err();
        assert!(err.contains("no room for the CRC"), "{err}");
    }
}
