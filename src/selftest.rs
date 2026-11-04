//! What the power-on self-tests found, and what the torch does about it.
//!
//! The firmware reads the fuses back before it turns anything on (AN2632
//! §11), and checks the fields it relies on, not whole bytes: the datasheet,
//! the errata and the ATDF disagree about the factory bytes, so a whole-byte
//! compare would fail on a chip that's fine. It never writes them.
//!
//! It also has the chip's CRCSCAN check the whole of flash against the
//! CRC-16 that `cargo xtask hex` stores in its last two bytes (AN2632 §9.1),
//! which catches a flash that a `cargo xtask flash` left half-written. The
//! hardware does the sums, so all that's here is what a failure means.
//!
//! `watchdog::Proof` adds one more: whether the watchdog passed its proof,
//! and whether it's since had to reset a chip that hung.
//!
//! A failure doesn't leave a dead torch, because the first flash has to be
//! easy to diagnose. It quietly refuses the FET instead: `limits::Limits`
//! caps the light at the top of the linear channel until the cell comes out.

use crate::regs;

/// The fuse bytes the firmware checks, as read from data space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuses {
    pub wdtcfg: u8,
    pub osccfg: u8,
    pub syscfg0: u8,
    pub append: u8,
    pub bootend: u8,
}

impl Fuses {
    /// Whether every field the firmware relies on is what it expects:
    ///
    /// - `WDTCFG` is 0. Anything else starts the watchdog from the fuse and
    ///   locks its settings (DS §19.3.3.3), so the firmware can't set it up.
    /// - `OSCCFG.FREQSEL` is 20 MHz and `OSCLOCK` clear. Every timing assumes
    ///   a calibrated 20 MHz divided by four, and OSCLOCK stops the
    ///   calibration loading (errata 2.2.1).
    /// - `SYSCFG0.RSTPINCFG` is UPDI, so the light can still be reflashed.
    /// - `APPEND` and `BOOTEND` are 0: the vector table is linked at 0, and
    ///   only the whole-flash CRC scan expects the checksum where `xtask`
    ///   puts it.
    pub fn ok(self) -> bool {
        self.wdtcfg == 0
            && self.osccfg & regs::FUSE_FREQSEL == regs::FUSE_FREQSEL_20MHZ
            && self.osccfg & regs::FUSE_OSCLOCK == 0
            && self.syscfg0 & regs::FUSE_RSTPINCFG == regs::FUSE_RSTPINCFG_UPDI
            && self.append == 0
            && self.bootend == 0
    }
}

/// Which self-tests failed at power-on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// The fuses aren't what the firmware expects.
    pub fuses: bool,
    /// Flash doesn't match the CRC `cargo xtask hex` put at its end, by
    /// CRCSCAN's sums.
    pub flash: bool,
    /// The watchdog failed its proof, or had to reset a chip that hung.
    pub watchdog: bool,
}

impl Faults {
    pub const NONE: Self = Self {
        fuses: false,
        flash: false,
        watchdog: false,
    };

    /// Whether anything failed, which refuses the FET.
    pub const fn any(self) -> bool {
        self.fuses || self.flash || self.watchdog
    }
}

/// How many bytes one pass of the periodic RAM test covers. src/rt.rs keeps
/// a copy of them in CPU registers while it tests them, so this is as many
/// as it has registers to spare.
pub const RAM_SECTION: u16 = 16;
/// How far on the next section starts. It's less than [`RAM_SECTION`] so
/// neighbouring sections overlap, as AN2632 §3.5 wants: a coupling fault
/// between two bytes only shows if they're tested together.
pub const RAM_STEP: u16 = 8;
/// Sections in a sweep of all of SRAM, at one a tick: just under 4 seconds.
pub const RAM_SECTIONS: u8 = ((SRAM_BYTES - RAM_SECTION) / RAM_STEP + 1) as u8;

const SRAM_BYTES: u16 = (regs::addr::SRAM_END - regs::addr::SRAM_START) as u16;

// The sections reach the end of SRAM exactly, and fit a `u8`.
const _: () = {
    assert!((SRAM_BYTES - RAM_SECTION).is_multiple_of(RAM_STEP));
    assert!((SRAM_BYTES - RAM_SECTION) / RAM_STEP < 255);
    assert!(RAM_STEP < RAM_SECTION);
};

/// Which section of SRAM the periodic test does next. It's kept as a count
/// rather than an address because it lives in the RAM it's testing: if it's
/// corrupted before the test gets to it, the worst that can happen is a
/// different section, never a pointer into the peripherals' registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct RamSweep {
    next: u8,
}

impl RamSweep {
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    /// The first address of the section to test now, moving on to the next.
    pub fn next_section(&mut self) -> u16 {
        let section = if self.next < RAM_SECTIONS {
            self.next
        } else {
            0
        };
        self.next = section + 1;
        regs::addr::SRAM_START as u16 + u16::from(section) * RAM_STEP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes a factory ATtiny1616 should hold, near enough: the fields the
    /// check reads are right, and the rest is as the datasheet has it.
    const GOOD: Fuses = Fuses {
        wdtcfg: 0x00,
        osccfg: 0x02,
        syscfg0: 0xC4,
        append: 0x00,
        bootend: 0x00,
    };

    #[test]
    fn a_sweep_covers_all_of_sram_with_each_section_overlapping_the_last() {
        let mut sweep = RamSweep::new();
        let mut tested = [false; regs::addr::SRAM_END - regs::addr::SRAM_START];
        let mut last = None;
        for _ in 0..RAM_SECTIONS {
            let start = sweep.next_section();
            let from = usize::from(start) - regs::addr::SRAM_START;
            tested[from..from + usize::from(RAM_SECTION)].fill(true);
            if let Some(last) = last {
                assert_eq!(start - last, RAM_STEP);
            }
            last = Some(start);
        }
        assert!(tested.iter().all(|&t| t));
        assert_eq!(
            last.map(|l| usize::from(l + RAM_SECTION)),
            Some(regs::addr::SRAM_END)
        );
    }

    #[test]
    fn a_sweep_starts_again_at_the_bottom() {
        let mut sweep = RamSweep::new();
        let first = sweep.next_section();
        for _ in 1..RAM_SECTIONS {
            sweep.next_section();
        }
        assert_eq!(sweep.next_section(), first);
    }

    #[test]
    fn any_corrupted_count_still_lands_in_sram() {
        for next in 0..=u8::MAX {
            let start = RamSweep { next }.next_section();
            assert!(usize::from(start) >= regs::addr::SRAM_START);
            assert!(usize::from(start + RAM_SECTION) <= regs::addr::SRAM_END);
        }
    }

    #[test]
    fn factory_fuses_pass() {
        assert!(GOOD.ok());
    }

    #[test]
    fn checks_fields_not_whole_bytes() {
        // OSCCFG's middle bits are reserved, and SYSCFG0's CRCSRC and EESAVE
        // are none of the firmware's business. The ATDF's 0x7E and 0xF6 are
        // its idea of the bytes, and should pass too.
        for (osccfg, syscfg0) in [(0x7E, 0xF6), (0x02, 0x05), (0x7E, 0xC4), (0x02, 0x04)] {
            let fuses = Fuses {
                osccfg,
                syscfg0,
                ..GOOD
            };
            assert!(fuses.ok(), "{osccfg:#04x} {syscfg0:#04x}");
        }
    }

    #[test]
    fn each_wrong_field_fails() {
        // (what, which byte, its value), the bytes in `Fuses`' order.
        let bad = [
            ("watchdog on from the fuse", 0, 0x0B),
            ("watchdog window only", 0, 0x10),
            ("16 MHz", 1, 0x01),
            ("FREQSEL reserved", 1, 0x03),
            ("FREQSEL 0", 1, 0x00),
            ("OSCLOCK", 1, 0x82),
            ("reset pin as GPIO", 2, 0xC0),
            ("reset pin as reset", 2, 0xC8),
            ("application section", 3, 0x20),
            ("boot section", 4, 0x02),
        ];
        for (what, byte, value) in bad {
            let mut fuses = GOOD;
            let field = match byte {
                0 => &mut fuses.wdtcfg,
                1 => &mut fuses.osccfg,
                2 => &mut fuses.syscfg0,
                3 => &mut fuses.append,
                _ => &mut fuses.bootend,
            };
            *field = value;
            assert!(!fuses.ok(), "{what}");
        }
        let erased = Fuses {
            wdtcfg: 0xFF,
            osccfg: 0xFF,
            syscfg0: 0xFF,
            append: 0xFF,
            bootend: 0xFF,
        };
        assert!(!erased.ok());
    }

    #[test]
    fn only_a_fault_refuses_the_fet() {
        assert!(!Faults::NONE.any());
        let fuses = Faults {
            fuses: true,
            ..Faults::NONE
        };
        let flash = Faults {
            flash: true,
            ..Faults::NONE
        };
        let watchdog = Faults {
            watchdog: true,
            ..Faults::NONE
        };
        assert!(fuses.any() && flash.any() && watchdog.any());
    }
}
