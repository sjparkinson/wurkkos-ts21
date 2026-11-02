//! What the torch's ADC sees, for the simulators: the cell's voltage, the
//! MCU's temperature, or an ADC that's broken.
//!
//! The library-side runs (`simulate.rs`) and the firmware on the simulated
//! chip (`sim.rs`) both get their raw readings from here, so the two see
//! exactly the same numbers and can be compared tick by tick.

use ts21::sensors::Channel;
use ts21::testing::{Adc, temperature_reading, voltage_reading};
use ts21::torch::Torch;

/// The temperature sensor calibration the simulated chip has in its
/// signature row, the same as the library tests'.
pub use ts21::testing::CALIBRATION;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
    /// The cell, in centivolts, as the 0614 calibration would report it.
    pub cell: u16,
    pub kelvin: u16,
    /// Every conversion reads this, whatever it's measuring.
    pub stuck: Option<u16>,
}

impl Default for World {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl World {
    /// A cell at 3.8 V and a room at 25 °C.
    pub const NORMAL: Self = Self {
        cell: 380,
        kelvin: 298,
        stuck: None,
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
        Self {
            torch: Torch::new(CALIBRATION),
            adc: Adc::new(),
        }
    }
}

impl Rig {
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
}
