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

/// Decodes Intel HEX into an image from address 0, with gaps as 0xFF.
/// Understands data, end-of-file and both extended address records, which
/// covers what avr-objcopy, avrdude and [`encode`] write.
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let mut image = Vec::new();
    let mut base = 0usize;

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let at = |msg: &str| format!("line {}: {msg}", n + 1);

        let hex = line
            .strip_prefix(':')
            .ok_or_else(|| at("doesn't start with ':'"))?;
        if hex.len() % 2 != 0 || hex.len() < 10 {
            return Err(at("malformed record"));
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| at("not hex"))?;

        let len = usize::from(bytes[0]);
        if bytes.len() != len + 5 {
            return Err(at("length doesn't match"));
        }
        if sum(&bytes) != 0 {
            return Err(at("bad checksum"));
        }
        let address = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
        let data = &bytes[4..4 + len];

        match bytes[3] {
            0x00 => {
                let start = base + address;
                if image.len() < start + len {
                    image.resize(start + len, 0xFF);
                }
                image[start..start + len].copy_from_slice(data);
            }
            0x01 => return Ok(image),
            0x02 if len == 2 => base = usize::from(u16::from_be_bytes([data[0], data[1]])) << 4,
            0x04 if len == 2 => base = usize::from(u16::from_be_bytes([data[0], data[1]])) << 16,
            0x03 | 0x05 => {} // start addresses: meaningless for AVR flash
            kind => return Err(at(&format!("unsupported record type {kind:02X}"))),
        }
    }
    Err("no end-of-file record".into())
}

fn record(out: &mut String, address: u16, kind: u8, data: &[u8]) {
    let [hi, lo] = address.to_be_bytes();
    let header = [data.len() as u8, hi, lo, kind];
    let checksum = sum(&header).wrapping_add(sum(data)).wrapping_neg();

    out.push(':');
    for byte in header.iter().chain(data) {
        let _ = write!(out, "{byte:02X}");
    }
    let _ = writeln!(out, "{checksum:02X}");
}

/// Byte sum, mod 256. A valid record, checksum included, sums to zero.
fn sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, b| sum.wrapping_add(*b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_it_encodes() {
        let image: Vec<u8> = (0..100u8).collect();
        assert_eq!(decode(&encode(&image)).unwrap(), image);
    }

    #[test]
    fn decodes_gaps_and_extended_addresses() {
        // Two bytes at 0x0002, then one at 0x10000 via an extended linear
        // address record.
        let text = ":020002000102F9\n:020000040001F9\n:01000000AA55\n:00000001FF\n";
        let image = decode(text).unwrap();
        assert_eq!(&image[..4], &[0xFF, 0xFF, 0x01, 0x02]);
        assert_eq!(image.len(), 0x10001);
        assert_eq!(image[0x10000], 0xAA);
    }

    #[test]
    fn rejects_bad_checksums_and_truncated_files() {
        assert!(
            decode(":0100000000FE\n:00000001FF\n")
                .unwrap_err()
                .contains("checksum")
        );
        assert!(
            decode(":0100000000FF\n")
                .unwrap_err()
                .contains("end-of-file")
        );
    }

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
