//! Just enough ELF32 to pull the flash image, RAM use and the symbols
//! `layout.rs` checks out of the firmware.

use std::ops::Range;

use ts21::regs::mem;

/// AVR ELF files put data space at 0x800000.
pub const DATA_SPACE: u32 = 0x80_0000;
/// SRAM's data-space range on the ATtiny1616, as ELF addresses.
pub const RAM: Range<u32> =
    DATA_SPACE + mem::INTERNAL_SRAM_START as u32..DATA_SPACE + mem::INTERNAL_SRAM_END as u32;

const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHF_ALLOC: u32 = 0x2;
const EM_AVR: u16 = 83;

pub struct Elf {
    entry: u32,
    segments: Vec<Segment>,
    sections: Vec<Section>,
    symbols: Vec<(String, u32)>,
}

/// A loadable piece of the image. `vaddr` is where the code expects to find
/// it, `paddr` where it sits in flash. They differ for .rodata (read through
/// the mapped-flash window) and .data (copied into SRAM by src/rt.rs).
pub struct Segment {
    pub vaddr: u32,
    pub paddr: u32,
    pub data: Vec<u8>,
}

struct Section {
    name: String,
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
        let entry = r.u32(24)?;

        let (phoff, phentsize, phnum) = (r.u32(28)?, r.u16(42)?, r.u16(44)?);
        let mut segments = Vec::new();
        for i in 0..u32::from(phnum) {
            let at = (phoff + i * u32::from(phentsize)) as usize;
            let (kind, offset, vaddr, paddr, filesz) = (
                r.u32(at)?,
                r.u32(at + 4)?,
                r.u32(at + 8)?,
                r.u32(at + 12)?,
                r.u32(at + 16)?,
            );
            // Loadable bytes with a flash load address are what gets
            // programmed: .text, .rodata and .data's initial values.
            if kind == PT_LOAD && filesz > 0 && paddr < DATA_SPACE {
                let data = r.slice(offset as usize, filesz as usize)?.to_vec();
                segments.push(Segment { vaddr, paddr, data });
            }
        }

        let (shoff, shentsize, shnum, shstrndx) = (r.u32(32)?, r.u16(46)?, r.u16(48)?, r.u16(50)?);
        let header = |i: u32| (shoff + i * u32::from(shentsize)) as usize;
        let names = r.u32(header(u32::from(shstrndx)) + 16)? as usize;
        let mut sections = Vec::new();
        let mut symbols = Vec::new();
        for i in 0..u32::from(shnum) {
            let at = header(i);
            sections.push(Section {
                name: r.cstr(names + r.u32(at)? as usize)?,
                flags: r.u32(at + 8)?,
                addr: r.u32(at + 12)?,
                size: r.u32(at + 20)?,
            });
            if r.u32(at + 4)? == SHT_SYMTAB {
                // sh_link names the string table the symbols' names are in.
                let (offset, size) = (r.u32(at + 16)? as usize, r.u32(at + 20)? as usize);
                let strings = r.u32(header(r.u32(at + 24)?) + 16)? as usize;
                // Entry 0 is the reserved null symbol.
                for sym in (offset..offset + size).step_by(16).skip(1) {
                    let name = r.cstr(strings + r.u32(sym)? as usize)?;
                    symbols.push((name, r.u32(sym + 4)?));
                }
            }
        }

        Ok(Self {
            entry,
            segments,
            sections,
            symbols,
        })
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

    /// The address execution starts at, from the ELF header.
    pub fn entry(&self) -> u32 {
        self.entry
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The address range of the first section called `name`.
    pub fn section(&self, name: &str) -> Option<Range<u32>> {
        self.sections
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.addr..s.addr + s.size)
    }

    /// The value of the first symbol called `name`. For code that's its
    /// byte address in flash.
    pub fn symbol(&self, name: &str) -> Option<u32> {
        self.symbols
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, value)| value)
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

    /// A NUL-terminated string starting at `at`.
    fn cstr(&self, at: usize) -> Result<String, String> {
        let rest = self
            .0
            .get(at..)
            .ok_or_else(|| format!("truncated at byte {at}"))?;
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| format!("unterminated string at byte {at}"))?;
        String::from_utf8(rest[..len].to_vec()).map_err(|_| format!("bad string at byte {at}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal AVR ELF: two loadable segments, a .bss section, a
    /// debug section and a symbol table.
    fn sample() -> Vec<u8> {
        const PHNUM: usize = 2;
        const SHNUM: usize = 5;
        let shoff = 52 + PHNUM * 32;
        let data_at = shoff + SHNUM * 40;

        let mut f = vec![0u8; 52];
        f[..4].copy_from_slice(b"\x7fELF");
        f[4] = 1; // 32-bit
        f[5] = 1; // little-endian
        f[18..20].copy_from_slice(&EM_AVR.to_le_bytes());
        f[24..28].copy_from_slice(&0x7Eu32.to_le_bytes()); // entry
        f[28..32].copy_from_slice(&52u32.to_le_bytes()); // phoff
        f[32..36].copy_from_slice(&(shoff as u32).to_le_bytes());
        f[42..44].copy_from_slice(&32u16.to_le_bytes()); // phentsize
        f[44..46].copy_from_slice(&(PHNUM as u16).to_le_bytes());
        f[46..48].copy_from_slice(&40u16.to_le_bytes()); // shentsize
        f[48..50].copy_from_slice(&(SHNUM as u16).to_le_bytes());
        f[50..52].copy_from_slice(&2u16.to_le_bytes()); // shstrndx

        // The bytes the headers point at, in file order.
        let image = [0x0C, 0x94, 0x1F, 0x00, 0xAA, 0xBB];
        let shstrtab = b"\0.bss\0.shstrtab\0.symtab\0.strtab\0.debug_info\0";
        let strtab = b"\0__reset\0main\0";
        let mut symtab = vec![0u8; 16]; // null symbol
        for (name, value) in [(1u32, 0x7Eu32), (9, 0xB2)] {
            let mut sym = vec![0u8; 16];
            sym[0..4].copy_from_slice(&name.to_le_bytes());
            sym[4..8].copy_from_slice(&value.to_le_bytes());
            symtab.extend(sym);
        }
        let image_at = data_at as u32;
        let shstrtab_at = image_at + image.len() as u32;
        let strtab_at = shstrtab_at + shstrtab.len() as u32;
        let symtab_at = strtab_at + strtab.len() as u32;

        // .text at 0, then .rodata loaded at 6 but linked in mapped flash.
        for (paddr, vaddr, offset, len) in [
            (0u32, 0u32, image_at, 4u32),
            (6, 0x80_8006, image_at + 4, 2),
        ] {
            let mut ph = vec![0u8; 32];
            ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
            ph[4..8].copy_from_slice(&offset.to_le_bytes());
            ph[8..12].copy_from_slice(&vaddr.to_le_bytes());
            ph[12..16].copy_from_slice(&paddr.to_le_bytes());
            ph[16..20].copy_from_slice(&len.to_le_bytes());
            f.extend(ph);
        }
        // (name, type, flags, addr, offset, size, link)
        let sections = [
            (1u32, 8u32, SHF_ALLOC | 0x1, 0x80_3800u32, 0u32, 12u32, 0u32), // .bss counts
            (34, 1, 0, 0, 0, 999, 0),                                       // .debug_info doesn't
            (6, 3, 0, 0, shstrtab_at, shstrtab.len() as u32, 0),
            (16, SHT_SYMTAB, 0, 0, symtab_at, symtab.len() as u32, 4),
            (24, 3, 0, 0, strtab_at, strtab.len() as u32, 0),
        ];
        for (name, kind, flags, addr, offset, size, link) in sections {
            let mut sh = vec![0u8; 40];
            sh[0..4].copy_from_slice(&name.to_le_bytes());
            sh[4..8].copy_from_slice(&kind.to_le_bytes());
            sh[8..12].copy_from_slice(&flags.to_le_bytes());
            sh[12..16].copy_from_slice(&addr.to_le_bytes());
            sh[16..20].copy_from_slice(&offset.to_le_bytes());
            sh[20..24].copy_from_slice(&size.to_le_bytes());
            sh[24..28].copy_from_slice(&link.to_le_bytes());
            f.extend(sh);
        }
        f.extend(image);
        f.extend(shstrtab);
        f.extend(strtab);
        f.extend(symtab);
        f
    }

    #[test]
    fn lays_segments_out_by_load_address() {
        let elf = Elf::parse(&sample()).unwrap();
        assert_eq!(
            elf.flash_image(),
            [0x0C, 0x94, 0x1F, 0x00, 0xFF, 0xFF, 0xAA, 0xBB]
        );
        let rodata = &elf.segments()[1];
        assert_eq!((rodata.vaddr, rodata.paddr), (0x80_8006, 6));
    }

    #[test]
    fn counts_allocated_sram_sections() {
        assert_eq!(Elf::parse(&sample()).unwrap().ram_bytes(), 12);
    }

    #[test]
    fn reads_section_names_symbols_and_the_entry_point() {
        let elf = Elf::parse(&sample()).unwrap();
        assert_eq!(elf.section(".bss"), Some(0x80_3800..0x80_380C));
        assert_eq!(elf.section(".text"), None);
        assert_eq!(elf.symbol("__reset"), Some(0x7E));
        assert_eq!(elf.symbol("main"), Some(0xB2));
        assert_eq!(elf.symbol(""), None, "the null symbol isn't listed");
        assert_eq!(elf.entry(), 0x7E);
    }

    #[test]
    fn rejects_other_files() {
        assert!(Elf::parse(b"#!/bin/sh\n").is_err());
        let mut arm = sample();
        arm[18] = 40;
        assert!(Elf::parse(&arm).is_err());
    }
}
