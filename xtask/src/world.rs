//! What the torch sees, for the simulators: the cell's voltage, the MCU's
//! temperature, an ADC that's broken, and the fuses it reads at power-on.
//!
//! The library-side runs (`simulate.rs`) and the firmware on the simulated
//! chip (`sim.rs`) both get their raw readings from here, so the two see
//! exactly the same numbers and can be compared tick by tick.

use ts21::selftest::{Faults, Fuses};
use ts21::sensors::Channel;
use ts21::testing::{Adc, temperature_reading, voltage_reading};
use ts21::torch::Torch;

use crate::sim::regs::addr as reg;
use crate::updi::map;

/// The temperature sensor calibration the simulated chip has in its
/// signature row, the same as the library tests'.
pub use ts21::testing::CALIBRATION;

/// The fuse bytes the simulated chip has, FUSE.WDTCFG to FUSE.BOOTEND.
pub const FUSE_BYTES: usize = 9;

/// Each fuse byte's name, by its offset from FUSE.WDTCFG, for scripts like
/// `fuse:OSCCFG=0x01`. Offset 3 is reserved.
pub const FUSE_NAMES: [&str; FUSE_BYTES] = [
    "WDTCFG", "BODCFG", "OSCCFG", "", "TCD0CFG", "SYSCFG0", "SYSCFG1", "APPEND", "BOOTEND",
];

/// The fuses the firmware expects, as the datasheet has a factory chip:
/// no watchdog from the fuse, BOD off, 20 MHz, UPDI on the reset pin, no
/// CRC at start-up and no boot or application section. The datasheet, the
/// errata and the ATDF disagree on OSCCFG and SYSCFG0's spare bits, which
/// is why the firmware only checks fields.
pub const FACTORY_FUSES: [u8; FUSE_BYTES] = [0x00, 0x00, 0x02, 0xFF, 0x00, 0xC4, 0x07, 0x00, 0x00];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
    /// The cell, in centivolts, as the 0614 calibration would report it.
    pub cell: u16,
    pub kelvin: u16,
    /// Every conversion reads this, whatever it's measuring.
    pub stuck: Option<u16>,
    /// The fuse bytes. The firmware reads them once, at power-on, so only
    /// the first tick's count.
    pub fuses: [u8; FUSE_BYTES],
}

impl Default for World {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl World {
    /// A cell at 3.8 V, a room at 25 °C and factory fuses.
    pub const NORMAL: Self = Self {
        cell: 380,
        kelvin: 298,
        stuck: None,
        fuses: FACTORY_FUSES,
    };

    /// The raw reading a conversion on `channel` gives.
    pub fn reading(&self, channel: Channel) -> u16 {
        if let Some(stuck) = self.stuck {
            return stuck;
        }
        match channel {
            Channel::Voltage => voltage_reading(self.cell),
            Channel::Temperature => temperature_reading(self.kelvin),
        }
    }

    /// The fuse at a data-space address, if it's one of `fuses`.
    pub fn fuse(&self, addr: usize) -> Option<u8> {
        let offset = addr.checked_sub(usize::from(map::FUSES))?;
        self.fuses.get(offset).copied()
    }

    /// What the power-on self-tests find in this world.
    pub fn faults(&self) -> Faults {
        let at = |addr| self.fuse(addr).unwrap_or(0xFF);
        let fuses = Fuses {
            wdtcfg: at(reg::FUSE_WDTCFG),
            osccfg: at(reg::FUSE_OSCCFG),
            syscfg0: at(reg::FUSE_SYSCFG0),
            append: at(reg::FUSE_APPEND),
            bootend: at(reg::FUSE_BOOTEND),
        };
        Faults { fuses: !fuses.ok() }
    }
}

/// The library's torch with an ADC that answers from a [`World`], the way
/// the firmware's loop drives it: the result of last tick's conversion in,
/// this tick's conversion out. A conversion measures the world as it was
/// when it started, as on the simulated chip.
#[derive(Clone, Debug)]
pub struct Rig {
    pub torch: Torch,
    adc: Adc,
}

impl Default for Rig {
    fn default() -> Self {
        Self::new(&World::NORMAL)
    }
}

impl Rig {
    /// A torch that's just had its cell put in, in `world`, which decides
    /// what its power-on self-tests find.
    pub fn new(world: &World) -> Self {
        Self {
            torch: Torch::new(CALIBRATION, world.faults()),
            adc: Adc::new(),
        }
    }

    pub fn tick(&mut self, pressed: bool, world: &World) -> Option<ts21::button::Event> {
        let event = self.torch.tick(pressed, self.adc.result());
        self.adc
            .start_with(self.torch.convert(), |channel| world.reading(channel));
        event
    }

    pub fn level(&self) -> u8 {
        self.torch.level()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_adc_reads_the_same_on_both_channels() {
        let world = World {
            stuck: Some(0),
            ..World::NORMAL
        };
        assert_eq!(world.reading(Channel::Voltage), 0);
        assert_eq!(world.reading(Channel::Temperature), 0);
    }

    #[test]
    fn the_rig_hands_results_back_a_tick_later() {
        let mut rig = Rig::default();
        for _ in 0..1000 {
            rig.tick(false, &World::NORMAL);
        }
        assert_eq!(rig.torch.limits().ceilings(), (150, 150));
    }

    #[test]
    fn factory_fuses_pass_the_self_test() {
        assert_eq!(World::NORMAL.faults(), Faults::NONE);
    }

    #[test]
    fn the_fuse_names_match_the_addresses() {
        for (addr, name) in [
            (reg::FUSE_WDTCFG, "WDTCFG"),
            (reg::FUSE_OSCCFG, "OSCCFG"),
            (reg::FUSE_SYSCFG0, "SYSCFG0"),
            (reg::FUSE_APPEND, "APPEND"),
            (reg::FUSE_BOOTEND, "BOOTEND"),
        ] {
            assert_eq!(FUSE_NAMES[addr - usize::from(map::FUSES)], name);
        }
    }

    #[test]
    fn a_wrong_fuse_is_a_fault() {
        let mut world = World::NORMAL;
        world.fuses[2] = 0x01; // OSCCFG: 16 MHz
        assert_eq!(world.faults(), Faults { fuses: true });
        assert_eq!(World::NORMAL.fuse(usize::from(map::FUSES) + 9), None);
    }
}
