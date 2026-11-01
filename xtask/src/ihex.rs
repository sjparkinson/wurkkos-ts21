//! Intel HEX, the format avrdude and pymcuprog both read.

use std::fmt::Write;

/// Encodes `image` (flash from address 0) as Intel HEX, 16 bytes a line.
/// Images over 64 KB would need extended address records, which a 16 KB
/// part never will.
pub fn encode(image: &[u8]) -> String {
    assert!(
        image.len() <= 0x1_0000,
        "image too big for 16-bit addresses"
    );

    let mut out = String::new();
    for (i, chunk) in image.chunks(16).enumerate() {
        record(&mut out, (i * 16) as u16, 0x00, chunk);
    }
    record(&mut out, 0, 0x01, &[]);
    out
}

fn record(out: &mut String, address: u16, kind: u8, data: &[u8]) {
    let [hi, lo] = address.to_be_bytes();
    let header = [data.len() as u8, hi, lo, kind];
    let sum = header
        .iter()
        .chain(data)
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));

    out.push(':');
    for byte in header.iter().chain(data) {
        let _ = write!(out, "{byte:02X}");
    }
    let _ = writeln!(out, "{:02X}", sum.wrapping_neg());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_just_the_end_record() {
        assert_eq!(encode(&[]), ":00000001FF\n");
    }

    #[test]
    fn splits_into_sixteen_byte_lines_with_checksums() {
        // A jmp to 0x3E (0C 94 1F 00), as at the start of a vector table.
        let mut image = vec![0x0C, 0x94, 0x1F, 0x00];
        image.extend([0xFF; 14]);
        assert_eq!(
            encode(&image),
            ":100000000C941F00FFFFFFFFFFFFFFFFFFFFFFFF3D\n\
             :02001000FFFFF0\n\
             :00000001FF\n"
        );
    }
}
