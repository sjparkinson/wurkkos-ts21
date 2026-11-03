//! A UPDI programmer for the ATtiny1616, spoken over a serial adapter.
//!
//! UPDI is Microchip's one-wire debug and programming interface. A "serial
//! UPDI" programmer is a USB-serial chip with TX and RX joined onto that one
//! wire, so everything we send comes straight back as an echo before the
//! chip's reply. [`Wire`] hides the echo; this module only sees replies.
//!
//! The protocol is in the tinyAVR 1-series datasheet (DS40002204, the UPDI
//! chapter). The sequences here (link set-up, NVM key, reset, page writes)
//! follow Microchip's pymcuprog, the reference serial UPDI implementation.
//! Register addresses and NVM commands are checked against the ATtiny1616
//! ATDF in `vendor/`.
//!
//! What this deliberately can't do: write fuses, write the user row, or
//! unlock a locked chip. None of those are needed to flash a TS21, and the
//! first can leave it unrecoverable without a high-voltage programmer.

use std::io;

/// Data-space layout of the ATtiny1616, as UPDI sees it.
pub mod map {
    /// Flash, seen through the data-space window. Writes land in the page
    /// buffer until an NVM command commits them.
    pub const FLASH: u16 = 0x8000;
    pub const FLASH_SIZE: usize = 16 * 1024;
    pub const FLASH_PAGE: usize = 64;

    pub const EEPROM: u16 = 0x1400;
    pub const EEPROM_SIZE: usize = 256;
    pub const EEPROM_PAGE: usize = 32;

    /// Device signature, the first three bytes of SIGROW.
    pub const SIGNATURE: u16 = 0x1100;
    /// From the ATDF's SIGNATURE0 to SIGNATURE2.
    pub const ATTINY1616: [u8; 3] = [0x1E, 0x94, 0x21];

    /// The fuses (0x1280..0x128A) and the lock byte (0x128A). Read only:
    /// nothing here writes them.
    pub const FUSES: u16 = 0x1280;
    pub const FUSES_SIZE: usize = 11;

    pub const NVMCTRL_CTRLA: u16 = 0x1000;
    pub const NVMCTRL_STATUS: u16 = 0x1002;
}

/// One serial UPDI connection. `send` writes bytes and swallows their echo;
/// `recv` reads the chip's reply.
pub trait Wire {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<()>;
    /// Holds the line low for two long frames, which resets UPDI's own
    /// state machine (not the chip) so it listens for a fresh SYNC.
    fn double_break(&mut self) -> io::Result<()>;
}

pub type Result<T> = std::result::Result<T, String>;

const SYNC: u8 = 0x55;
const ACK: u8 = 0x40;

// Instructions.
const LDS: u8 = 0x00;
const LD: u8 = 0x20;
const STS: u8 = 0x40;
const ST: u8 = 0x60;
const LDCS: u8 = 0x80;
const REPEAT: u8 = 0xA0;
const STCS: u8 = 0xC0;
const KEY: u8 = 0xE0;

// Instruction modifiers.
const PTR_INC: u8 = 0x04;
const PTR_ADDRESS: u8 = 0x08;
const ADDRESS_16: u8 = 0x04;
const DATA_8: u8 = 0x00;
const DATA_16: u8 = 0x01;
const KEY_SIB: u8 = 0x04;
const KEY_64: u8 = 0x00;
const SIB_16: u8 = 0x01;

// Control and status registers.
const CS_STATUSA: u8 = 0x00;
const CS_CTRLA: u8 = 0x02;
const CS_CTRLB: u8 = 0x03;
const ASI_KEY_STATUS: u8 = 0x07;
const ASI_RESET_REQ: u8 = 0x08;
const ASI_SYS_STATUS: u8 = 0x0B;

const CTRLA_IBDLY: u8 = 1 << 7; // inter-byte delay on replies
const CTRLA_RSD: u8 = 1 << 3; // response signature (ACK) disable
const CTRLB_CCDETDIS: u8 = 1 << 3; // collision detection off
const CTRLB_UPDIDIS: u8 = 1 << 2; // turn UPDI off
const KEY_STATUS_NVMPROG: u8 = 1 << 4;
const SYS_STATUS_NVMPROG: u8 = 1 << 3;
const SYS_STATUS_LOCKSTATUS: u8 = 1 << 0;
const RESET_REQ_APPLY: u8 = 0x59;

const KEY_NVMPROG: &[u8; 8] = b"NVMProg ";

// NVMCTRL.CTRLA commands and NVMCTRL.STATUS bits.
const NVM_PAGE_WRITE: u8 = 0x01;
const NVM_PAGE_ERASE_WRITE: u8 = 0x03;
const NVM_PAGE_BUFFER_CLEAR: u8 = 0x04;
const NVM_CHIP_ERASE: u8 = 0x05;
const NVM_FBUSY: u8 = 1 << 0;
const NVM_EEBUSY: u8 = 1 << 1;
const NVM_WRERROR: u8 = 1 << 2;

/// The most a single REPEAT can cover.
const MAX_BLOCK: usize = 256;

pub struct Programmer<W: Wire> {
    wire: W,
}

impl<W: Wire> Programmer<W> {
    /// Brings up the UPDI link, with a double break and a second try if the
    /// chip doesn't answer the first time.
    pub fn connect(wire: W) -> Result<Self> {
        let mut p = Self { wire };
        if p.init_link().is_err() {
            p.wire.double_break().map_err(io_error)?;
            p.init_link()?;
        }
        Ok(p)
    }

    fn init_link(&mut self) -> Result<()> {
        self.stcs(CS_CTRLB, CTRLB_CCDETDIS)?;
        self.stcs(CS_CTRLA, CTRLA_IBDLY)?;
        match self.ldcs(CS_STATUSA)? {
            0 => Err("the UPDI link came up but STATUSA reads 0".into()),
            _ => Ok(()),
        }
    }

    /// The System Information Block: 16 bytes of ASCII naming the chip
    /// family and UPDI version. Readable without any key.
    pub fn sib(&mut self) -> Result<[u8; 16]> {
        self.send(&[SYNC, KEY | KEY_SIB | SIB_16])?;
        let mut sib = [0; 16];
        self.recv(&mut sib)?;
        Ok(sib)
    }

    /// Sends the NVM programming key and resets the chip into programming
    /// mode. The firmware stops running from here until [`Self::leave`].
    pub fn enter_progmode(&mut self) -> Result<()> {
        if self.ldcs(ASI_SYS_STATUS)? & SYS_STATUS_NVMPROG != 0 {
            return Ok(());
        }

        let mut frame = vec![SYNC, KEY | KEY_64];
        frame.extend(KEY_NVMPROG.iter().rev());
        self.send(&frame)?;
        if self.ldcs(ASI_KEY_STATUS)? & KEY_STATUS_NVMPROG == 0 {
            return Err("the chip didn't accept the NVM programming key".into());
        }

        self.reset()?;
        for _ in 0..100 {
            let status = self.ldcs(ASI_SYS_STATUS)?;
            if status & SYS_STATUS_LOCKSTATUS == 0 && status & SYS_STATUS_NVMPROG != 0 {
                return Ok(());
            }
        }
        Err(
            "the chip didn't enter programming mode. If it's locked, unlocking means \
             erasing it, which this tool won't do"
                .into(),
        )
    }

    /// Fails unless the chip reports itself as an ATtiny1616.
    pub fn check_signature(&mut self) -> Result<()> {
        let signature = self.read(map::SIGNATURE, 3)?;
        if signature == map::ATTINY1616 {
            Ok(())
        } else {
            Err(format!(
                "expected an ATtiny1616 (signature {}) but found {}",
                hex(&map::ATTINY1616),
                hex(&signature)
            ))
        }
    }

    /// Reads `len` bytes of data space from `addr`.
    pub fn read(&mut self, addr: u16, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0; len];
        for (i, chunk) in out.chunks_mut(MAX_BLOCK).enumerate() {
            self.st_ptr(offset(addr, i * MAX_BLOCK)?)?;
            let mut frame = repeat(chunk.len());
            frame.extend([SYNC, LD | PTR_INC | DATA_8]);
            self.send(&frame)?;
            self.recv(chunk)?;
        }
        Ok(out)
    }

    /// Erases all of flash, and EEPROM unless the EESAVE fuse is set.
    pub fn chip_erase(&mut self) -> Result<()> {
        self.wait_nvm()?;
        self.nvm_command(NVM_CHIP_ERASE)?;
        self.wait_nvm()
    }

    /// Chip-erases, then writes `data` from the start of flash with plain
    /// page writes, since every page is already 0xFF. That's one erase per
    /// page instead of two. Erases EEPROM too unless EESAVE is set.
    ///
    /// The erase and the write stay in one call because a page write can only
    /// clear bits: over flash that isn't blank, it ANDs the new data into the
    /// old. Follow with [`Self::verify`], which catches an erase that didn't
    /// take. Refuses an image bigger than flash before erasing anything.
    pub fn erase_and_write_flash(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > map::FLASH_SIZE {
            return Err(format!(
                "the image is {} bytes; the ATtiny1616 has {}",
                data.len(),
                map::FLASH_SIZE
            ));
        }
        self.chip_erase()?;
        // Blank pages can be skipped, because the chip erase left them blank.
        self.write_pages(map::FLASH, map::FLASH_PAGE, data, NVM_PAGE_WRITE, true)
    }

    /// Writes every page of `data` from `addr` with page erase-write. `addr`
    /// must be page aligned. For EEPROM, which EESAVE can keep through a chip
    /// erase, so a page that should end up blank still has to be written.
    pub fn write(&mut self, addr: u16, page: usize, data: &[u8]) -> Result<()> {
        self.write_pages(addr, page, data, NVM_PAGE_ERASE_WRITE, false)
    }

    fn write_pages(
        &mut self,
        addr: u16,
        page: usize,
        data: &[u8],
        command: u8,
        skip_blank: bool,
    ) -> Result<()> {
        // Each page write ends by waiting for the NVM controller, so this is
        // the only wait needed before the next one starts.
        self.wait_nvm()?;
        for (i, chunk) in data.chunks(page).enumerate() {
            if skip_blank && chunk.iter().all(|&b| b == 0xFF) {
                continue;
            }
            self.write_page(offset(addr, i * page)?, chunk, command)?;
        }
        Ok(())
    }

    /// Reads `data.len()` bytes from `addr` and compares them with `data`.
    /// Run it after [`Self::erase_and_write_flash`] and [`Self::write`].
    pub fn verify(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        let actual = self.read(addr, data.len())?;
        match actual.iter().zip(data).position(|(a, b)| a != b) {
            None => Ok(()),
            Some(i) => Err(format!(
                "verify failed at 0x{:04X}: wrote {:02X}, read {:02X}",
                usize::from(addr) + i,
                data[i],
                actual[i]
            )),
        }
    }

    /// Resets the chip out of programming mode and turns UPDI off, so the
    /// firmware starts.
    pub fn leave(mut self) -> Result<W> {
        self.reset()?;
        self.disconnect()
    }

    /// Turns UPDI off without resetting, for when nothing was changed.
    pub fn disconnect(mut self) -> Result<W> {
        self.stcs(CS_CTRLB, CTRLB_UPDIDIS | CTRLB_CCDETDIS)?;
        Ok(self.wire)
    }

    /// Expects the NVM controller to be idle, as [`Self::write_pages`] leaves
    /// it. `command` is the NVM command that commits the page buffer.
    fn write_page(&mut self, addr: u16, data: &[u8], command: u8) -> Result<()> {
        self.nvm_command(NVM_PAGE_BUFFER_CLEAR)?;
        self.wait_nvm()?;

        // Fill the page buffer in one burst, with the per-byte ACKs turned
        // off so it doesn't take a round trip per byte. Nothing in the
        // burst replies, so it goes out as a single frame.
        self.st_ptr(addr)?;
        let mut frame = vec![SYNC, STCS | CS_CTRLA, CTRLA_IBDLY | CTRLA_RSD];
        frame.extend(repeat(data.len()));
        frame.extend([SYNC, ST | PTR_INC | DATA_8]);
        frame.extend(data);
        frame.extend([SYNC, STCS | CS_CTRLA, CTRLA_IBDLY]);
        self.send(&frame)?;

        self.nvm_command(command)?;
        self.wait_nvm()
    }

    fn wait_nvm(&mut self) -> Result<()> {
        for _ in 0..10_000 {
            let status = self.lds(map::NVMCTRL_STATUS)?;
            if status & NVM_WRERROR != 0 {
                return Err("the NVM controller reported a write error".into());
            }
            if status & (NVM_FBUSY | NVM_EEBUSY) == 0 {
                return Ok(());
            }
        }
        Err("timed out waiting for the NVM controller".into())
    }

    fn nvm_command(&mut self, command: u8) -> Result<()> {
        self.sts(map::NVMCTRL_CTRLA, command)
    }

    fn reset(&mut self) -> Result<()> {
        self.stcs(ASI_RESET_REQ, RESET_REQ_APPLY)?;
        self.stcs(ASI_RESET_REQ, 0)
    }

    fn ldcs(&mut self, reg: u8) -> Result<u8> {
        self.send(&[SYNC, LDCS | reg])?;
        self.recv1()
    }

    fn stcs(&mut self, reg: u8, value: u8) -> Result<()> {
        self.send(&[SYNC, STCS | reg, value])
    }

    fn lds(&mut self, addr: u16) -> Result<u8> {
        let [lo, hi] = addr.to_le_bytes();
        self.send(&[SYNC, LDS | ADDRESS_16 | DATA_8, lo, hi])?;
        self.recv1()
    }

    fn sts(&mut self, addr: u16, value: u8) -> Result<()> {
        let [lo, hi] = addr.to_le_bytes();
        self.send(&[SYNC, STS | ADDRESS_16 | DATA_8, lo, hi])?;
        self.ack()?;
        self.send(&[value])?;
        self.ack()
    }

    fn st_ptr(&mut self, addr: u16) -> Result<()> {
        let [lo, hi] = addr.to_le_bytes();
        self.send(&[SYNC, ST | PTR_ADDRESS | DATA_16, lo, hi])?;
        self.ack()
    }

    fn ack(&mut self) -> Result<()> {
        match self.recv1()? {
            ACK => Ok(()),
            other => Err(format!(
                "expected an ACK (0x40) from the chip, got 0x{other:02X}"
            )),
        }
    }

    fn recv1(&mut self) -> Result<u8> {
        let mut byte = [0];
        self.recv(&mut byte)?;
        Ok(byte[0])
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.wire.send(bytes).map_err(io_error)
    }

    fn recv(&mut self, buf: &mut [u8]) -> Result<()> {
        self.wire.recv(buf).map_err(io_error)
    }
}

fn offset(addr: u16, by: usize) -> Result<u16> {
    u16::try_from(usize::from(addr) + by).map_err(|_| "address out of range".to_string())
}

/// A REPEAT instruction, to be sent ahead of the one it repeats.
fn repeat(count: usize) -> Vec<u8> {
    assert!((1..=MAX_BLOCK).contains(&count), "REPEAT covers 1 to 256");
    vec![SYNC, REPEAT, (count - 1) as u8]
}

/// `[0x1E, 0x94]` as `1E 94`.
fn hex(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    hex.join(" ")
}

fn io_error(error: io::Error) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut => "no reply from the chip. Check the probe's contact, \
                                    the pad order and that the cell is out"
            .into(),
        _ => format!("serial: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::Tiny;

    /// The memory map and NVM constants against the vendored ATDF. The
    /// UPDI instruction set isn't in the ATDF, so those stay checked by
    /// review against the datasheet and pymcuprog.
    #[test]
    fn map_and_nvm_constants_match_the_atdf() {
        let atdf = crate::atdf::Atdf::load();

        let flash = atdf.segment("MAPPED_PROGMEM");
        assert_eq!(u32::from(map::FLASH), flash.start, "flash window");
        assert_eq!(map::FLASH_SIZE as u32, flash.size, "flash size");
        assert_eq!(
            map::FLASH_SIZE as u32,
            atdf.segment("PROGMEM").size,
            "PROGMEM size"
        );
        assert_eq!(Some(map::FLASH_PAGE as u32), flash.page, "flash page");

        let eeprom = atdf.segment("EEPROM");
        assert_eq!(u32::from(map::EEPROM), eeprom.start, "EEPROM");
        assert_eq!(map::EEPROM_SIZE as u32, eeprom.size, "EEPROM size");
        assert_eq!(Some(map::EEPROM_PAGE as u32), eeprom.page, "EEPROM page");

        let signatures = atdf.segment("SIGNATURES");
        assert_eq!(u32::from(map::SIGNATURE), signatures.start, "signature");
        assert_eq!(
            map::ATTINY1616.len() as u32,
            signatures.size,
            "signature size"
        );
        assert_eq!(u32::from(map::ATTINY1616[0]), atdf.property("SIGNATURE0"));
        assert_eq!(u32::from(map::ATTINY1616[1]), atdf.property("SIGNATURE1"));
        assert_eq!(u32::from(map::ATTINY1616[2]), atdf.property("SIGNATURE2"));

        // FUSES_SIZE covers the fuses and the lock byte straight after them.
        let fuses = atdf.segment("FUSES");
        let lock = atdf.segment("LOCKBITS");
        assert_eq!(u32::from(map::FUSES), fuses.start, "fuses");
        assert_eq!(
            fuses.start + fuses.size,
            lock.start,
            "lock byte follows the fuses"
        );
        assert_eq!(
            map::FUSES_SIZE as u32,
            fuses.size + lock.size,
            "fuses and lock size"
        );

        let ctrla = atdf.register("NVMCTRL", None, "CTRLA");
        let status = atdf.register("NVMCTRL", None, "STATUS");
        assert_eq!(
            u32::from(map::NVMCTRL_CTRLA),
            ctrla.address,
            "NVMCTRL.CTRLA"
        );
        assert_eq!(
            u32::from(map::NVMCTRL_STATUS),
            status.address,
            "NVMCTRL.STATUS"
        );

        for (ours, theirs, label) in [
            (
                NVM_PAGE_WRITE,
                ctrla.value("CMD", "PAGEWRITE"),
                "CMD=PAGEWRITE",
            ),
            (
                NVM_PAGE_ERASE_WRITE,
                ctrla.value("CMD", "PAGEERASEWRITE"),
                "CMD=PAGEERASEWRITE",
            ),
            (
                NVM_PAGE_BUFFER_CLEAR,
                ctrla.value("CMD", "PAGEBUFCLR"),
                "CMD=PAGEBUFCLR",
            ),
            (
                NVM_CHIP_ERASE,
                ctrla.value("CMD", "CHIPERASE"),
                "CMD=CHIPERASE",
            ),
            (NVM_FBUSY, status.mask("FBUSY"), "STATUS.FBUSY"),
            (NVM_EEBUSY, status.mask("EEBUSY"), "STATUS.EEBUSY"),
            (NVM_WRERROR, status.mask("WRERROR"), "STATUS.WRERROR"),
        ] {
            assert_eq!(u32::from(ours), theirs, "NVMCTRL.{label}");
        }
    }

    #[test]
    fn reads_the_sib() {
        let mut p = Programmer::connect(Tiny::new()).unwrap();
        assert_eq!(&p.sib().unwrap(), b"tinyAVR P:0D:1-3");
    }

    #[test]
    fn retries_with_a_double_break() {
        let mut tiny = Tiny::new();
        tiny.asleep = true;
        let tiny = Programmer::connect(tiny).unwrap().disconnect().unwrap();
        assert_eq!(tiny.breaks, 1);
        assert!(tiny.updi_disabled);
    }

    #[test]
    fn enters_progmode_and_checks_the_signature() {
        let mut p = Programmer::connect(Tiny::new()).unwrap();
        p.enter_progmode().unwrap();
        p.check_signature().unwrap();
    }

    #[test]
    fn refuses_another_chip() {
        let mut tiny = Tiny::new();
        tiny.signature = [0x1E, 0x94, 0x20];
        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        assert!(p.check_signature().unwrap_err().contains("1E 94 20"));
    }

    #[test]
    fn refuses_a_locked_chip() {
        let mut tiny = Tiny::new();
        tiny.locked = true;
        let mut p = Programmer::connect(tiny).unwrap();
        assert!(p.enter_progmode().unwrap_err().contains("locked"));
    }

    #[test]
    fn writes_and_verifies_flash_then_starts_the_firmware() {
        let image: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut tiny = Tiny::new();
        tiny.flash.fill(0x42);

        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&image).unwrap();
        p.verify(map::FLASH, &image).unwrap();
        let tiny = p.leave().unwrap();

        assert_eq!(&tiny.flash[..image.len()], &image[..]);
        assert!(tiny.flash[image.len()..].iter().all(|&b| b == 0xFF));
        // One erase per page, the chip erase's: 1000 bytes is 16 pages.
        assert_eq!(tiny.page_writes, 16);
        assert_eq!(tiny.page_erase_writes, 0);
        assert!(tiny.updi_disabled);
        assert!(!tiny.in_reset);
    }

    #[test]
    fn verify_catches_flash_that_did_not_erase() {
        let image: Vec<u8> = (0..200u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut tiny = Tiny::new();
        tiny.flash.fill(0x42);
        tiny.ignores_chip_erase = true;

        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&image).unwrap();
        let err = p.verify(map::FLASH, &image).unwrap_err();
        assert!(err.contains("verify failed"), "{err}");
    }

    #[test]
    fn refuses_an_image_bigger_than_flash_before_erasing() {
        let mut tiny = Tiny::new();
        tiny.flash.fill(0x42);
        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        let err = p
            .erase_and_write_flash(&vec![0; map::FLASH_SIZE + 1])
            .unwrap_err();
        assert!(err.contains("16384"), "{err}");
        let tiny = p.leave().unwrap();
        assert!(tiny.flash.iter().all(|&b| b == 0x42), "flash was erased");
    }

    #[test]
    fn skips_blank_flash_pages() {
        let mut image = vec![0xFF; 3 * map::FLASH_PAGE];
        image[2 * map::FLASH_PAGE] = 0x12;
        let mut p = Programmer::connect(Tiny::new()).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&image).unwrap();
        let tiny = p.leave().unwrap();
        assert_eq!(tiny.page_writes, 1);
        assert_eq!(tiny.page_erase_writes, 0);
    }

    #[test]
    fn writes_blank_eeprom_pages_over_eesave_contents() {
        // A backup whose fourth page is blank, over EEPROM that EESAVE kept.
        let mut eeprom: Vec<u8> = (0..=255).collect();
        eeprom[3 * map::EEPROM_PAGE..4 * map::EEPROM_PAGE].fill(0xFF);
        let mut tiny = Tiny::new();
        tiny.eesave = true;
        tiny.eeprom.fill(0x42);

        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&[0x12]).unwrap();
        p.write(map::EEPROM, map::EEPROM_PAGE, &eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        let tiny = p.leave().unwrap();
        assert_eq!(tiny.page_erase_writes, 8);
    }

    #[test]
    fn eeprom_erase_write_leaves_bytes_it_was_not_given() {
        // A 40-byte image ends 8 bytes into the second page; the rest of that
        // page keeps its old contents, as on the chip.
        let eeprom = vec![0x11; 40];
        let mut tiny = Tiny::new();
        tiny.eeprom.fill(0x42);

        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        p.write(map::EEPROM, map::EEPROM_PAGE, &eeprom).unwrap();
        let tiny = p.leave().unwrap();
        assert_eq!(&tiny.eeprom[..40], &eeprom[..]);
        assert!(tiny.eeprom[40..].iter().all(|&b| b == 0x42));
    }

    #[test]
    fn writes_eeprom_over_eesave_contents_and_reads_fuses() {
        let eeprom: Vec<u8> = (0..=255).collect();
        let mut tiny = Tiny::new();
        tiny.eesave = true;
        tiny.eeprom.fill(0x42);

        let mut p = Programmer::connect(tiny).unwrap();
        p.enter_progmode().unwrap();
        p.erase_and_write_flash(&[0x12]).unwrap();
        p.write(map::EEPROM, map::EEPROM_PAGE, &eeprom).unwrap();
        p.verify(map::EEPROM, &eeprom).unwrap();
        let fuses = p.read(map::FUSES, map::FUSES_SIZE).unwrap();
        assert_eq!(fuses[2], 0x02); // OSCCFG: 20 MHz

        // EEPROM kept its old contents through the chip erase, so it needs
        // erase-write: 256 bytes is 8 pages. The flash page is a plain write.
        let tiny = p.leave().unwrap();
        assert_eq!(tiny.page_erase_writes, 8);
        assert_eq!(tiny.page_writes, 1);
    }

    #[test]
    fn reports_a_verify_mismatch() {
        let mut p = Programmer::connect(Tiny::new()).unwrap();
        p.enter_progmode().unwrap();
        let err = p.verify(map::FLASH, &[0xFF, 0x00]).unwrap_err();
        assert!(err.contains("0x8001"), "{err}");
    }

    /// A pretend ATtiny1616 on the far end of the wire, enough to exercise
    /// the instruction framing, the ACK rules and the NVM controller. It's
    /// written from the same datasheet as the programmer, so it catches
    /// slips, not misreadings. Only a real chip catches those.
    mod fake {
        use super::super::*;
        use std::collections::VecDeque;

        const FLASH_END: u16 = map::FLASH + (map::FLASH_SIZE - 1) as u16;
        const EEPROM_END: u16 = map::EEPROM + (map::EEPROM_SIZE - 1) as u16;
        const SIGNATURE_END: u16 = map::SIGNATURE + (map::ATTINY1616.len() - 1) as u16;
        /// FUSE.OSCCFG, the third fuse byte.
        const OSCCFG: u16 = map::FUSES + 2;

        /// The base address and page size of whichever NVM `addr` is in.
        fn region(addr: u16) -> (u16, usize) {
            if addr >= map::FLASH {
                (map::FLASH, map::FLASH_PAGE)
            } else {
                (map::EEPROM, map::EEPROM_PAGE)
            }
        }

        enum Expect {
            Sync,
            Opcode,
            CsValue(u8),
            Key(Vec<u8>),
            LdsAddress(Vec<u8>),
            StsAddress(Vec<u8>),
            StsData(u16),
            PtrAddress(Vec<u8>),
            RepeatCount,
            StIncData(usize),
        }

        pub struct Tiny {
            pub flash: Vec<u8>,
            pub eeprom: Vec<u8>,
            pub signature: [u8; 3],
            pub locked: bool,
            pub asleep: bool,
            pub breaks: usize,
            pub updi_disabled: bool,
            pub in_reset: bool,
            /// The EESAVE fuse: EEPROM survives a chip erase.
            pub eesave: bool,
            /// A chip erase that silently does nothing.
            pub ignores_chip_erase: bool,
            /// Plain page writes (PAGEWRITE).
            pub page_writes: usize,
            /// Page erase-writes (PAGEERASEWRITE).
            pub page_erase_writes: usize,
            expect: Expect,
            replies: VecDeque<u8>,
            ctrla: u8,
            key_status: u8,
            progmode: bool,
            ptr: u16,
            repeat: usize,
            page_buffer: Vec<u8>,
            /// Which page buffer bytes have been written since it was cleared.
            page_loaded: Vec<bool>,
            page_addr: Option<u16>,
        }

        impl Tiny {
            pub fn new() -> Self {
                Self {
                    flash: vec![0xFF; map::FLASH_SIZE],
                    eeprom: vec![0xFF; map::EEPROM_SIZE],
                    signature: map::ATTINY1616,
                    locked: false,
                    asleep: false,
                    breaks: 0,
                    updi_disabled: false,
                    in_reset: false,
                    eesave: false,
                    ignores_chip_erase: false,
                    page_writes: 0,
                    page_erase_writes: 0,
                    expect: Expect::Sync,
                    replies: VecDeque::new(),
                    ctrla: 0,
                    key_status: 0,
                    progmode: false,
                    ptr: 0,
                    repeat: 1,
                    page_buffer: vec![0xFF; map::FLASH_PAGE],
                    page_loaded: vec![false; map::FLASH_PAGE],
                    page_addr: None,
                }
            }

            fn byte(&mut self, b: u8) {
                self.expect = match std::mem::replace(&mut self.expect, Expect::Sync) {
                    Expect::Sync => {
                        assert_eq!(b, SYNC, "every instruction starts with SYNC");
                        Expect::Opcode
                    }
                    Expect::Opcode => self.opcode(b),
                    Expect::CsValue(reg) => {
                        self.stcs(reg, b);
                        Expect::Sync
                    }
                    Expect::Key(mut key) => {
                        key.push(b);
                        if key.len() < 8 {
                            Expect::Key(key)
                        } else {
                            key.reverse();
                            if key == KEY_NVMPROG {
                                self.key_status |= KEY_STATUS_NVMPROG;
                            }
                            Expect::Sync
                        }
                    }
                    Expect::LdsAddress(mut a) => {
                        a.push(b);
                        if a.len() < 2 {
                            Expect::LdsAddress(a)
                        } else {
                            let v = self.load(u16::from_le_bytes([a[0], a[1]]));
                            self.replies.push_back(v);
                            Expect::Sync
                        }
                    }
                    Expect::StsAddress(mut a) => {
                        a.push(b);
                        if a.len() < 2 {
                            Expect::StsAddress(a)
                        } else {
                            self.ack();
                            Expect::StsData(u16::from_le_bytes([a[0], a[1]]))
                        }
                    }
                    Expect::StsData(addr) => {
                        self.store(addr, b);
                        self.ack();
                        Expect::Sync
                    }
                    Expect::PtrAddress(mut a) => {
                        a.push(b);
                        if a.len() < 2 {
                            Expect::PtrAddress(a)
                        } else {
                            self.ptr = u16::from_le_bytes([a[0], a[1]]);
                            self.ack();
                            Expect::Sync
                        }
                    }
                    Expect::RepeatCount => {
                        self.repeat = usize::from(b) + 1;
                        Expect::Sync
                    }
                    Expect::StIncData(left) => {
                        self.store(self.ptr, b);
                        self.ptr += 1;
                        self.ack();
                        if left > 1 {
                            Expect::StIncData(left - 1)
                        } else {
                            Expect::Sync
                        }
                    }
                };
            }

            fn opcode(&mut self, op: u8) -> Expect {
                let repeat = std::mem::replace(&mut self.repeat, 1);
                match op {
                    _ if op & 0xF0 == LDCS => {
                        let v = self.ldcs(op & 0x0F);
                        self.replies.push_back(v);
                        Expect::Sync
                    }
                    _ if op & 0xF0 == STCS => Expect::CsValue(op & 0x0F),
                    0xE0 => Expect::Key(Vec::new()),
                    0xE5 => {
                        self.replies.extend(b"tinyAVR P:0D:1-3");
                        Expect::Sync
                    }
                    0x04 => Expect::LdsAddress(Vec::new()),
                    0x44 => Expect::StsAddress(Vec::new()),
                    0x69 => Expect::PtrAddress(Vec::new()),
                    0xA0 => Expect::RepeatCount,
                    0x24 => {
                        for _ in 0..repeat {
                            let v = self.load(self.ptr);
                            self.replies.push_back(v);
                            self.ptr += 1;
                        }
                        Expect::Sync
                    }
                    0x64 => Expect::StIncData(repeat),
                    _ => panic!("unexpected opcode 0x{op:02X}"),
                }
            }

            fn ack(&mut self) {
                if self.ctrla & CTRLA_RSD == 0 {
                    self.replies.push_back(ACK);
                }
            }

            fn ldcs(&self, reg: u8) -> u8 {
                match reg {
                    CS_STATUSA => 0x30, // UPDI revision 3
                    ASI_KEY_STATUS => self.key_status,
                    ASI_SYS_STATUS => {
                        let lock = if self.locked {
                            SYS_STATUS_LOCKSTATUS
                        } else {
                            0
                        };
                        let prog = if self.progmode { SYS_STATUS_NVMPROG } else { 0 };
                        lock | prog
                    }
                    _ => 0,
                }
            }

            fn stcs(&mut self, reg: u8, value: u8) {
                match reg {
                    CS_CTRLA => self.ctrla = value,
                    CS_CTRLB => self.updi_disabled = value & CTRLB_UPDIDIS != 0,
                    ASI_RESET_REQ if value == RESET_REQ_APPLY => self.in_reset = true,
                    ASI_RESET_REQ => {
                        self.in_reset = false;
                        self.progmode = self.key_status & KEY_STATUS_NVMPROG != 0 && !self.locked;
                        self.key_status = 0;
                    }
                    _ => {}
                }
            }

            fn load(&self, addr: u16) -> u8 {
                match addr {
                    map::FLASH..=FLASH_END => self.flash[usize::from(addr - map::FLASH)],
                    map::EEPROM..=EEPROM_END => self.eeprom[usize::from(addr - map::EEPROM)],
                    map::SIGNATURE..=SIGNATURE_END => {
                        self.signature[usize::from(addr - map::SIGNATURE)]
                    }
                    OSCCFG => 0x02,
                    _ => 0,
                }
            }

            fn store(&mut self, addr: u16, value: u8) {
                assert!(
                    self.progmode,
                    "store to 0x{addr:04X} outside programming mode"
                );
                match addr {
                    map::NVMCTRL_CTRLA => self.nvm(value),
                    map::FLASH..=FLASH_END | map::EEPROM..=EEPROM_END => {
                        let (_, page) = region(addr);
                        self.page_addr = Some(addr - addr % page as u16);
                        self.page_buffer[usize::from(addr) % page] = value;
                        self.page_loaded[usize::from(addr) % page] = true;
                    }
                    _ => panic!("store to 0x{addr:04X}"),
                }
            }

            fn nvm(&mut self, command: u8) {
                match command {
                    NVM_PAGE_BUFFER_CLEAR => {
                        self.page_buffer.fill(0xFF);
                        self.page_loaded.fill(false);
                        self.page_addr = None;
                    }
                    NVM_PAGE_WRITE | NVM_PAGE_ERASE_WRITE => {
                        let addr = self.page_addr.take().expect("a page to write");
                        let (base, page) = region(addr);
                        let mem = if base == map::FLASH {
                            &mut self.flash
                        } else {
                            &mut self.eeprom
                        };
                        let start = usize::from(addr - base);
                        let cells = mem[start..start + page].iter_mut();
                        let bytes = self.page_buffer.iter().zip(&self.page_loaded);
                        if command == NVM_PAGE_WRITE {
                            // A write can only clear bits; only an erase sets them.
                            for (cell, (&b, _)) in cells.zip(bytes) {
                                *cell &= b;
                            }
                            self.page_writes += 1;
                        } else {
                            // Flash erases the whole page. EEPROM erases only
                            // the bytes loaded into the page buffer (DS40001893A,
                            // 9.3.1.2: byte granularity on erase-write).
                            for (cell, (&b, &loaded)) in cells.zip(bytes) {
                                if base == map::FLASH || loaded {
                                    *cell = b;
                                }
                            }
                            self.page_erase_writes += 1;
                        }
                        self.page_buffer.fill(0xFF);
                        self.page_loaded.fill(false);
                    }
                    NVM_CHIP_ERASE if self.ignores_chip_erase => {}
                    NVM_CHIP_ERASE => {
                        self.flash.fill(0xFF);
                        if !self.eesave {
                            self.eeprom.fill(0xFF);
                        }
                    }
                    _ => panic!("unexpected NVM command 0x{command:02X}"),
                }
            }
        }

        impl Wire for Tiny {
            fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
                for &b in bytes {
                    self.byte(b);
                }
                Ok(())
            }

            fn recv(&mut self, buf: &mut [u8]) -> io::Result<()> {
                if self.asleep {
                    self.replies.clear();
                    self.expect = Expect::Sync;
                    return Err(io::ErrorKind::TimedOut.into());
                }
                for slot in buf {
                    *slot = self
                        .replies
                        .pop_front()
                        .ok_or(io::Error::from(io::ErrorKind::TimedOut))?;
                }
                Ok(())
            }

            fn double_break(&mut self) -> io::Result<()> {
                self.breaks += 1;
                self.asleep = false;
                self.expect = Expect::Sync;
                self.replies.clear();
                Ok(())
            }
        }
    }
}
