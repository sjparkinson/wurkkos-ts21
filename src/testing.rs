//! Test doubles shared by the library's own tests and xtask's simulators, so
//! there's one fake ADC and one way of turning a voltage or a temperature
//! into a raw reading.
//!
//! Only built for `cargo test` and with the `testing` feature, which xtask
//! turns on. The firmware never sees it.

use crate::battery;
use crate::sensors::{Channel, Convert};
use crate::temp::Calibration;

/// A temperature sensor calibration for tests and the simulated chip's
/// signature row. Made up, but in the range real parts have: about 0.55 K a
/// count, so 25 °C reads 531.
pub const CALIBRATION: Calibration = Calibration {
    offset: -10,
    gain: 141,
};

/// The accumulated reading of the 1.5 V reference against VDD for a cell
/// `battery::centivolts` reports as `cell`: the closest, saturating at four
/// full-scale conversions.
pub fn voltage_reading(cell: u16) -> u16 {
    // Near 606,000 / (cell - offset), then whichever neighbour fits best.
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

/// An ADC that hands back a conversion's result a tick after it started, the
/// way the firmware's loop sees the real one. A conversion measures what
/// was there when it started.
///
/// `voltage` and `temperature` are the raw results [`Adc::start`] reads.
/// Anything else that knows what each channel reads can use
/// [`Adc::start_with`] instead.
#[derive(Clone, Debug)]
pub struct Adc {
    pub voltage: u16,
    pub temperature: u16,
    pending: Option<u16>,
}

impl Default for Adc {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc {
    /// 3.8 V and 25 °C with [`CALIBRATION`].
    pub const fn new() -> Self {
        Self {
            voltage: 1731,
            temperature: 531,
            pending: None,
        }
    }

    /// The result of the conversion started last tick, if there was one.
    pub fn result(&mut self) -> Option<u16> {
        self.pending.take()
    }

    /// Starts `convert`, if there is one, reading `voltage` or
    /// `temperature`.
    pub fn start(&mut self, convert: Option<Convert>) {
        let (voltage, temperature) = (self.voltage, self.temperature);
        self.start_with(convert, |channel| match channel {
            Channel::Voltage => voltage,
            Channel::Temperature => temperature,
        });
    }

    /// Starts `convert`, if there is one, reading whatever `read` says the
    /// channel reads.
    pub fn start_with(&mut self, convert: Option<Convert>, read: impl FnOnce(Channel) -> u16) {
        self.pending = convert.map(|convert| read(convert.channel));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(channel: Channel) -> Option<Convert> {
        Some(Convert {
            channel,
            setup: false,
        })
    }

    #[test]
    fn answers_for_the_channel_a_tick_later() {
        let mut adc = Adc::new();
        assert_eq!(adc.result(), None);
        adc.start(convert(Channel::Temperature));
        assert_eq!(adc.result(), Some(531));
        assert_eq!(adc.result(), None);
        adc.start(convert(Channel::Voltage));
        assert_eq!(adc.result(), Some(1731));
        adc.start(None);
        assert_eq!(adc.result(), None);
    }

    #[test]
    fn measures_what_was_there_when_it_started() {
        let mut adc = Adc::new();
        adc.start(convert(Channel::Voltage));
        adc.voltage = 7;
        assert_eq!(adc.result(), Some(1731));
    }

    #[test]
    fn the_defaults_are_a_good_cell_at_room_temperature() {
        assert_eq!(battery::centivolts(Adc::new().voltage), 380);
        assert_eq!(temperature_reading(crate::thermal::celsius(25)), 531);
        assert_eq!(CALIBRATION.kelvin(531), crate::thermal::celsius(25));
    }

    #[test]
    fn readings_come_back_as_what_was_asked_for() {
        for cell in [250, 299, 305, 380, 420] {
            assert_eq!(battery::centivolts(voltage_reading(cell)), cell, "{cell}");
        }
        for kelvin in [273, 298, 318, 343, 373] {
            let got = CALIBRATION.kelvin(temperature_reading(kelvin));
            assert!(got.abs_diff(kelvin) <= 1, "{kelvin} K reads as {got}");
        }
    }
}
