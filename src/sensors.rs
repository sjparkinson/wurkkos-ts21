//! Sharing the one ADC between the cell's voltage and the temperature
//! sensor.
//!
//! The ADC alternates every 8 ticks, so each gets a reading every 16 ticks
//! (four a second, what `battery::Protection` and `thermal::Thermal` expect),
//! the temperature 8 ticks after the voltage. Each turn takes three ticks:
//!
//! ```text
//! tick 0   set the ADC up for the voltage, start a conversion
//! tick 1   throw that result away, start another
//! tick 2   that result is the voltage
//! tick 8   set it up for the temperature, start a conversion
//! tick 9   throw that result away, start another
//! tick 10  that result is the temperature
//! ```
//!
//! The ADC's only set up when the channel changes. While a click's being
//! counted with the light off it reads nothing but the voltage, so after the
//! first round it skips the setup and goes straight to converting.
//!
//! A conversion takes a fraction of a millisecond, so it's always done by
//! the next tick, and nothing ever waits on the ADC.
//!
//! The datasheet doesn't ask for the first conversion after a switch to be
//! thrown away. It does say the ADC needs time to settle on a new channel,
//! and the internal reference about 25 µs to start (DS40002204A, sections
//! 30.3.2.4 and 36.15). `INITDLY` covers the reference when the ADC's
//! reference changes to it, but the voltage reading keeps VDD as the ADC's
//! reference and switches the internal one it measures from 1.1 to 1.5 V,
//! which `INITDLY` may not cover. Anduril throws the first one away too
//! ("first result is unstable", `fsm/adc.c`). It costs a tick, so it's cheap
//! insurance.
//!
//! The temperature is only read when asked for, which the caller does while
//! the light's on. The voltage is read until [`Schedule::rest`], since the
//! ADC can't run in power-down, and starts again from the top after it.

/// What the ADC is measuring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The internal 1.5 V reference against VDD, four conversions
    /// accumulated, for `battery::centivolts`.
    Voltage,
    /// The temperature sensor against the internal 1.1 V reference, four
    /// conversions accumulated, for `temp::sixteenths`.
    Temperature,
}

/// A conversion for the firmware to start this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Convert {
    pub channel: Channel,
    /// Set the ADC (and the reference) up for `channel` first, turning them
    /// on if they're off. Only the first conversion of a turn on a different
    /// channel from the last turn's needs it, or the first after a rest.
    pub setup: bool,
}

/// A reading that's come due, straight from the ADC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Voltage(u16),
    Temperature(u16),
}

/// Ticks in a full round of both channels.
pub const CYCLE_TICKS: u8 = 16;
/// Where the temperature's turn starts in the round.
const TEMPERATURE_AT: u8 = CYCLE_TICKS / 2;

const _: () = assert!(CYCLE_TICKS.is_power_of_two());
const _: () = assert!(CYCLE_TICKS as u16 == crate::battery::SAMPLE_TICKS);
const _: () = assert!(CYCLE_TICKS as u16 == crate::thermal::SAMPLE_TICKS);

#[derive(Clone, Debug, Default)]
pub struct Schedule {
    phase: u8,
    /// Whether this round reads the temperature.
    temperature: bool,
    /// The channel the ADC was last set up for, `None` before the first.
    /// Its registers keep their values until a reset, which starts this
    /// afresh too. [`Schedule::rest`] puts it back to `None`, since the
    /// firmware turns the reference off to power down.
    set_up: Option<Channel>,
}

impl Schedule {
    pub const fn new() -> Self {
        Self {
            phase: 0,
            temperature: false,
            set_up: None,
        }
    }

    /// Back to where it started, for after the firmware's powered down. The
    /// next tick sets the ADC up again, and throws that conversion away as
    /// usual, which gives the reference a tick to start.
    pub fn rest(&mut self) {
        *self = Self::new();
    }

    /// One tick. `result` is the ADC's result if it has one ready, and
    /// `temperature` whether to read the temperature this round, which only
    /// matters on the tick its turn starts.
    ///
    /// Returns the reading that's due this tick, if any, and the conversion
    /// to start. A reading that's due when the ADC has no result comes out as
    /// zero, which both channels treat as garbage.
    pub fn tick(
        &mut self,
        result: Option<u16>,
        temperature: bool,
    ) -> (Option<Reading>, Option<Convert>) {
        let phase = self.phase;
        self.phase = (phase + 1) & (CYCLE_TICKS - 1);
        let (channel, step) = match phase.checked_sub(TEMPERATURE_AT) {
            None => (Channel::Voltage, phase),
            Some(step) => (Channel::Temperature, step),
        };
        if step == 0 && channel == Channel::Temperature {
            self.temperature = temperature;
        }
        if channel == Channel::Temperature && !self.temperature {
            return (None, None);
        }
        let convert = |setup| Some(Convert { channel, setup });
        match step {
            0 => {
                let setup = self.set_up != Some(channel);
                self.set_up = Some(channel);
                (None, convert(setup))
            }
            1 => (None, convert(false)),
            2 => {
                let reading = result.unwrap_or(0);
                let reading = match channel {
                    Channel::Voltage => Reading::Voltage(reading),
                    Channel::Temperature => Reading::Temperature(reading),
                };
                (Some(reading), None)
            }
            _ => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// What `ticks` ticks of the schedule ask for and hand back, with the
    /// ADC's result always `result` and the temperature wanted or not.
    fn run(
        schedule: &mut Schedule,
        ticks: usize,
        result: Option<u16>,
        temperature: bool,
    ) -> Vec<(Option<Reading>, Option<Convert>)> {
        (0..ticks)
            .map(|_| schedule.tick(result, temperature))
            .collect()
    }

    const fn convert(channel: Channel, setup: bool) -> Option<Convert> {
        Some(Convert { channel, setup })
    }

    #[test]
    fn reads_the_voltage_then_the_temperature_8_ticks_later() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, Some(7), true);
        let mut expected = [(None, None); 16];
        expected[0].1 = convert(Channel::Voltage, true);
        expected[1].1 = convert(Channel::Voltage, false);
        expected[2].0 = Some(Reading::Voltage(7));
        expected[8].1 = convert(Channel::Temperature, true);
        expected[9].1 = convert(Channel::Temperature, false);
        expected[10].0 = Some(Reading::Temperature(7));
        assert_eq!(ticks, expected);
    }

    #[test]
    fn each_channel_comes_round_every_16_ticks() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 160, Some(1), true);
        for (channel, at) in [(Channel::Voltage, 2), (Channel::Temperature, 10)] {
            let due: Vec<usize> = ticks
                .iter()
                .enumerate()
                .filter(|(_, (reading, _))| match reading {
                    Some(Reading::Voltage(_)) => channel == Channel::Voltage,
                    Some(Reading::Temperature(_)) => channel == Channel::Temperature,
                    None => false,
                })
                .map(|(tick, _)| tick)
                .collect();
            let expected: Vec<usize> = (0..10).map(|n| at + 16 * n).collect();
            assert_eq!(due, expected, "{channel:?}");
        }
    }

    #[test]
    fn the_first_conversion_after_a_setup_is_never_a_reading() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 64, Some(1), true);
        for (tick, (reading, _)) in ticks.iter().enumerate().skip(1) {
            if ticks[tick - 1].1.is_some_and(|c| c.setup) {
                assert_eq!(*reading, None, "tick {tick}");
            }
        }
    }

    #[test]
    fn leaves_the_temperature_alone_when_not_asked() {
        let mut schedule = Schedule::new();
        for (reading, convert) in run(&mut schedule, 160, Some(1), false) {
            assert!(!matches!(reading, Some(Reading::Temperature(_))));
            assert!(convert.is_none_or(|c| c.channel == Channel::Voltage));
        }
    }

    #[test]
    fn decides_on_the_temperature_at_the_start_of_its_turn() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 8, Some(1), false);
        // Asked at the start of its turn, it finishes even if the ask goes.
        assert_eq!(
            schedule.tick(Some(1), true),
            (None, convert(Channel::Temperature, true))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (None, convert(Channel::Temperature, false))
        );
        assert_eq!(
            schedule.tick(Some(1), false),
            (Some(Reading::Temperature(1)), None)
        );
        // Not asked at the start, it doesn't start halfway through.
        run(&mut schedule, 13, Some(1), false);
        assert_eq!(schedule.tick(Some(1), false), (None, None));
        assert_eq!(schedule.tick(Some(1), true), (None, None));
    }

    #[test]
    fn only_sets_the_adc_up_when_the_channel_changes() {
        let mut schedule = Schedule::new();
        let setups = |ticks: &[(Option<Reading>, Option<Convert>)]| -> Vec<Channel> {
            ticks
                .iter()
                .filter_map(|(_, convert)| convert.filter(|c| c.setup).map(|c| c.channel))
                .collect()
        };
        // Off: the first voltage turn sets up, the rest don't.
        let off = run(&mut schedule, 64, Some(1), false);
        assert_eq!(off[0].1, convert(Channel::Voltage, true));
        assert_eq!(off[16].1, convert(Channel::Voltage, false));
        assert_eq!(setups(&off), [Channel::Voltage]);
        // On: every turn switches channel, so every turn sets up.
        let on = run(&mut schedule, 64, Some(1), true);
        let alternating = [Channel::Temperature, Channel::Voltage].repeat(4);
        assert_eq!(setups(&on)[..], alternating[..7]);
        // Off again: the voltage turn after the last temperature one sets up,
        // and that's all.
        let off = run(&mut schedule, 64, Some(1), false);
        assert_eq!(setups(&off), [Channel::Voltage]);
    }

    #[test]
    fn a_rest_starts_the_round_again_and_sets_the_adc_up() {
        let mut schedule = Schedule::new();
        run(&mut schedule, 21, Some(1), true);
        schedule.rest();
        let ticks = run(&mut schedule, 3, Some(7), false);
        assert_eq!(ticks[0], (None, convert(Channel::Voltage, true)));
        assert_eq!(ticks[1], (None, convert(Channel::Voltage, false)));
        assert_eq!(ticks[2], (Some(Reading::Voltage(7)), None));
    }

    #[test]
    fn no_result_when_one_is_due_reads_as_zero() {
        let mut schedule = Schedule::new();
        let ticks = run(&mut schedule, 16, None, true);
        assert_eq!(ticks[2].0, Some(Reading::Voltage(0)));
        assert_eq!(ticks[10].0, Some(Reading::Temperature(0)));
    }
}
