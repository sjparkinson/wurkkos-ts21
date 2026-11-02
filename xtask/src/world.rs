//! What the torch's ADC sees, for the simulators: the cell's voltage, the
//! MCU's temperature, or an ADC that's broken.
//!
//! The library-side runs (`simulate.rs`) and the firmware on the simulated
//! chip (`sim.rs`) both get their raw readings from here, so the two see
//! exactly the same numbers and can be compared tick by tick.

use ts21::battery;
use ts21::sensors::Channel;
use ts21::temp::Calibration;
use ts21::torch::Torch;

/// The temperature sensor calibration the simulated chip has in its
/// signature row. Made up, but in the range real parts have: about 0.55 K a
/// count, so 25 °C reads 531.
pub const CALIBRATION: Calibration = Calibration {
    offset: -10,
    gain: 141,
};

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

/// The accumulated reading of the 1.5 V reference against VDD for a cell
/// `battery::centivolts` reports as `cell`: the closest, saturating at four
/// full-scale conversions.
pub fn voltage_reading(cell: u16) -> u16 {
    // Near 600,000 / (cell - offset), then whichever neighbour fits best.
    let above = u32::from(cell.saturating_sub(battery::OFFSET).max(1));
    let guess = (606_000 / above).clamp(1, 4 * 1023) as u16;
    (guess.saturating_sub(3).max(1)..=(guess + 3).min(4 * 1023))
        .min_by_key(|&raw| (battery::centivolts(raw).abs_diff(cell), raw))
        .unwrap_or(guess)
}

/// The 10-bit temperature sensor reading closest to `kelvin` with
/// [`CALIBRATION`].
pub fn temperature_reading(kelvin: u16) -> u16 {
    (0..=1023)
        .min_by_key(|&raw| (CALIBRATION.kelvin(raw).abs_diff(kelvin), raw))
        .unwrap_or(0)
}

/// The library's torch with an ADC that answers from a [`World`], the way
/// the firmware's loop drives it: the result of last tick's conversion in,
/// this tick's conversion out. A conversion measures the world as it was
/// when it started, as on the simulated chip.
#[derive(Clone, Debug)]
pub struct Rig {
    pub torch: Torch,
    pending: Option<u16>,
}

impl Default for Rig {
    fn default() -> Self {
        Self {
            torch: Torch::new(CALIBRATION),
            pending: None,
        }
    }
}

impl Rig {
    pub fn tick(&mut self, pressed: bool, world: &World) -> Option<ts21::button::Event> {
        let event = self.torch.tick(pressed, self.pending.take());
        self.pending = self
            .torch
            .convert()
            .map(|convert| world.reading(convert.channel));
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
    fn readings_come_back_as_what_was_asked_for() {
        for cell in [250, 299, 305, 380, 420] {
            assert_eq!(battery::centivolts(voltage_reading(cell)), cell, "{cell}");
        }
        for kelvin in [273, 298, 318, 343, 373] {
            let got = CALIBRATION.kelvin(temperature_reading(kelvin));
            assert!(got.abs_diff(kelvin) <= 1, "{kelvin} K reads as {got}");
        }
        assert_eq!(temperature_reading(298), 531);
    }

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
