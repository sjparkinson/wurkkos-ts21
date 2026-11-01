//! Just enough ELF32 to pull the flash image and RAM use out of the firmware.

/// AVR ELF files put data space at 0x800000.
const DATA_SPACE: u32 = 0x80_0000;
/// SRAM's data-space range on the ATtiny1616, as ELF addresses.
const RAM: std::ops::Range<u32> = 0x80_3800..0x80_4000;

const PT_LOAD: u32 = 1;
const SHF_ALLOC: u32 = 0x2;
const EM_AVR: u16 = 83;

pub struct Elf {
    segments: Vec<Segment>,
    sections: Vec<Section>,
}

struct Segment {
    paddr: u32,
    data: Vec<u8>,
}

struct Section {
    flags: u32,
    addr: u32,
    size: u32,
}

impl Elf {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let r = Reader(bytes);
        if bytes.get(..4) != Some(b"\x7fELF".as_slice()) {
            return Err("not an ELF file".into());
        }
        if r.u8(4)? != 1 || r.u8(5)? != 1 {
            return Err("not a 32-bit little-endian ELF file".into());
        }
        if r.u16(18)? != EM_AVR {
            return Err("not an AVR ELF file".into());
        }

        let (phoff, phentsize, phnum) = (r.u32(28)?, r.u16(42)?, r.u16(44)?);
        let mut segments = Vec::new();
        for i in 0..u32::from(phnum) {
            let at = (phoff + i * u32::from(phentsize)) as usize;
            let (kind, offset, paddr, filesz) =
                (r.u32(at)?, r.u32(at + 4)?, r.u32(at + 12)?, r.u32(at + 16)?);
            // Loadable bytes with a flash load address are what gets
            // programmed: .text, .rodata and .data's initial values.
            if kind == PT_LOAD && filesz > 0 && paddr < DATA_SPACE {
                let data = r.slice(offset as usize, filesz as usize)?.to_vec();
                segments.push(Segment { paddr, data });
            }
        }

        let (shoff, shentsize, shnum) = (r.u32(32)?, r.u16(46)?, r.u16(48)?);
        let mut sections = Vec::new();
        for i in 0..u32::from(shnum) {
            let at = (shoff + i * u32::from(shentsize)) as usize;
            sections.push(Section {
                flags: r.u32(at + 8)?,
                addr: r.u32(at + 12)?,
                size: r.u32(at + 20)?,
            });
        }

        Ok(Self { segments, sections })
    }

    /// Flash contents from address 0, with gaps filled with 0xFF (erased).
    pub fn flash_image(&self) -> Vec<u8> {
        let end = self
            .segments
            .iter()
            .map(|s| s.paddr as usize + s.data.len())
            .max()
            .unwrap_or(0);
        let mut image = vec![0xFF; end];
        for s in &self.segments {
            let start = s.paddr as usize;
            image[start..start + s.data.len()].copy_from_slice(&s.data);
        }
        image
    }

    /// Bytes of SRAM taken by .data and .bss.
    pub fn ram_bytes(&self) -> u32 {
        self.sections
            .iter()
            .filter(|s| s.flags & SHF_ALLOC != 0 && RAM.contains(&s.addr))
            .map(|s| s.size)
            .sum()
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn slice(&self, at: usize, len: usize) -> Result<&[u8], String> {
        self.0
            .get(at..at + len)
            .ok_or_else(|| format!("truncated at byte {at}"))
    }

    fn u8(&self, at: usize) -> Result<u8, String> {
        Ok(self.slice(at, 1)?[0])
    }

    fn u16(&self, at: usize) -> Result<u16, String> {
        let b = self.slice(at, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        let b = self.slice(at, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal AVR ELF: two loadable segments and two sections.
    fn sample() -> Vec<u8> {
        let mut f = vec![0u8; 52];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 1; // 32-bit
        f[5] = 1; // little-endian
        f[18..20].copy_from_slice(&EM_AVR.to_le_bytes());
        f[28..32].copy_from_slice(&52u32.to_le_bytes()); // phoff
        f[42..44].copy_from_slice(&32u16.to_le_bytes()); // phentsize
        f[44..46].copy_from_slice(&2u16.to_le_bytes()); // phnum
        let shoff = 52 + 2 * 32;
        f[32..36].copy_from_slice(&(shoff as u32).to_le_bytes());
        f[46..48].copy_from_slice(&40u16.to_le_bytes()); // shentsize
        f[48..50].copy_from_slice(&2u16.to_le_bytes()); // shnum

        let data_at = (shoff + 2 * 40) as u32;
        // .text at 0, then .rodata loaded at 6 but linked in mapped flash.
        for (paddr, vaddr, offset, len) in
            [(0u32, 0u32, data_at, 4u32), (6, 0x80_8006, data_at + 4, 2)]
        {
            let mut ph = vec![0u8; 32];
            ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
            ph[4..8].copy_from_slice(&offset.to_le_bytes());
            ph[8..12].copy_from_slice(&vaddr.to_le_bytes());
            ph[12..16].copy_from_slice(&paddr.to_le_bytes());
            ph[16..20].copy_from_slice(&len.to_le_bytes());
            f.extend(ph);
        }
        // .bss in SRAM (counts) and .debug_info (doesn't).
        for (flags, addr, size) in [(SHF_ALLOC | 0x1, 0x80_3800u32, 12u32), (0, 0, 999)] {
            let mut sh = vec![0u8; 40];
            sh[8..12].copy_from_slice(&flags.to_le_bytes());
            sh[12..16].copy_from_slice(&addr.to_le_bytes());
            sh[20..24].copy_from_slice(&size.to_le_bytes());
            f.extend(sh);
        }
        f.extend([0x0C, 0x94, 0x1F, 0x00, 0xAA, 0xBB]);
        f
    }

    #[test]
    fn lays_segments_out_by_load_address() {
        let elf = Elf::parse(&sample()).unwrap();
        assert_eq!(
            elf.flash_image(),
            [0x0C, 0x94, 0x1F, 0x00, 0xFF, 0xFF, 0xAA, 0xBB]
        );
    }

    #[test]
    fn counts_allocated_sram_sections() {
        assert_eq!(Elf::parse(&sample()).unwrap().ram_bytes(), 12);
    }

    #[test]
    fn rejects_other_files() {
        assert!(Elf::parse(b"#!/bin/sh\n").is_err());
        let mut arm = sample();
        arm[18] = 40;
        assert!(Elf::parse(&arm).is_err());
    }
}
