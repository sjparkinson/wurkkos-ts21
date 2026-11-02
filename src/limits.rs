//! What the cell and the temperature allow: the UI's level in, the level to
//! run at out.
//!
//! Each tick this runs the ADC's [`Schedule`], hands each reading to
//! low-voltage protection (`battery::Protection`) or thermal regulation
//! (`thermal::Thermal`), and runs the light at the lowest of the UI's level,
//! the thermal ceiling and the battery ceiling. Both ceilings fade through a
//! [`Gradual`], so a step-down never jumps. A flat cell turns the UI off.
//!
//! Garbage readings fail safe. Each side has its own idea of a reading that
//! can't be real (`battery::plausible`, `thermal::plausible`), lets a few
//! go, and then drops its ceiling to the top of the linear channel rather
//! than trusting it or turning the light off.
//!
//! As a diagnostic for the first flash, the first round of readings after
//! power-on takes the temperature too, light on or not. If any of them is
//! garbage, the light gives one short dim blink from off, so a bad ADC setup
//! shows without a debugger.

use crate::battery::{self, Protection, Verdict};
use crate::gradual::Gradual;
use crate::ramp::LEVELS;
use crate::sensors::{Convert, Reading, Schedule};
use crate::temp::Calibration;
use crate::thermal::{self, Thermal};

/// The level of the power-on diagnostic blink: dim, on the linear channel.
pub const BLINK_LEVEL: u8 = 20;
/// How long it lasts: a quarter of a second.
pub const BLINK_TICKS: u8 = 16;

/// Ticks per level as the battery ceiling fades down, and back up. Down is a
/// level a tick, quicker than thermal's, since the cell's already under its
/// threshold by the time it steps: 15 levels takes a quarter of a second,
/// and the FET's half of the ramp a little over a second.
const BATTERY_DOWN_EVERY: u8 = 1;
const BATTERY_UP_EVERY: u8 = 4;

/// One tick's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The level to run at, `0` meaning off.
    pub level: u8,
    /// The cell's flat: turn the UI off.
    pub off: bool,
}

#[derive(Clone, Debug)]
pub struct Limits {
    schedule: Schedule,
    calibration: Calibration,
    protection: Protection,
    /// The battery ceiling and where it's fading to.
    battery: Gradual,
    battery_target: u8,
    thermal: Thermal,
    /// Whether the UI was on last tick.
    on: bool,
    /// The level last tick.
    level: u8,
    /// True until the first temperature reading's in.
    first: bool,
    /// Whether anything in the first round was garbage.
    suspect: bool,
    /// Ticks of diagnostic blink left.
    blink: u8,
    convert: Option<Convert>,
}

impl Limits {
    /// `calibration` is the chip's own, from the signature row.
    pub const fn new(calibration: Calibration) -> Self {
        Self {
            schedule: Schedule::new(),
            calibration,
            protection: Protection::new(),
            battery: Gradual::new(LEVELS, BATTERY_DOWN_EVERY, BATTERY_UP_EVERY),
            battery_target: LEVELS,
            thermal: Thermal::new(thermal::Config::DEFAULT),
            on: false,
            level: 0,
            first: true,
            suspect: false,
            blink: 0,
            convert: None,
        }
    }

    /// The conversion the ADC should start this tick, if any.
    pub fn convert(&self) -> Option<Convert> {
        self.convert
    }

    /// The battery and thermal ceilings right now.
    pub fn ceilings(&self) -> (u8, u8) {
        (self.battery.level(), self.thermal.ceiling())
    }

    /// One tick: `ui` is the level the UI wants (`0` for off) and `result`
    /// the ADC's result, if it has one ready.
    pub fn tick(&mut self, ui: u8, result: Option<u16>) -> Step {
        let on = ui != 0;
        if on && !self.on {
            self.thermal.reset();
        }
        self.on = on;

        let (reading, convert) = self.schedule.tick(result, on || self.first);
        self.convert = convert;
        let mut off = false;
        match reading {
            Some(Reading::Voltage(raw)) => {
                let centivolts = battery::centivolts(raw);
                self.diagnose(battery::plausible(centivolts));
                // The blink isn't a load worth judging the cell by.
                let level = if on { self.level } else { 0 };
                match self.protection.sample(centivolts, level) {
                    Verdict::Off => off = true,
                    Verdict::Ceiling(ceiling) => self.battery_ceiling(ceiling),
                }
            }
            Some(Reading::Temperature(raw)) => {
                let kelvin = self.calibration.kelvin(raw);
                self.diagnose(thermal::plausible(kelvin));
                if self.first && self.suspect && !on {
                    self.blink = BLINK_TICKS;
                }
                self.first = false;
                if on {
                    self.thermal.sample(kelvin, self.level);
                }
            }
            None => {}
        }

        let thermal = self.thermal.tick();
        let battery = self.battery.tick(self.battery_target);
        self.level = if on && !off {
            self.blink = 0;
            ui.min(thermal).min(battery)
        } else if self.blink > 0 {
            self.blink -= 1;
            BLINK_LEVEL
        } else {
            0
        };
        Step {
            level: self.level,
            off,
        }
    }

    /// A new battery ceiling. Like thermal's, a ceiling above the light's
    /// level does nothing, so it's pulled down to the level first, out of
    /// sight, and the fade shows from its first step.
    fn battery_ceiling(&mut self, ceiling: u8) {
        if ceiling < self.battery.level() {
            self.battery.cap(match self.on {
                true => self.level.max(ceiling),
                false => ceiling,
            });
        }
        self.battery_target = ceiling;
    }

    /// Notes whether a reading in the first round made sense, for the
    /// power-on blink.
    fn diagnose(&mut self, plausible: bool) {
        self.suspect |= self.first && !plausible;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::TICKS_PER_SECOND;
    use crate::ramp::LINEAR_MAX;
    use crate::sensors::FakeAdc;
    use std::vec::Vec;

    /// Limits and a fake ADC, a tick at a time, checking the output never
    /// jumps by more than a level while the UI holds steady.
    struct Run {
        limits: Limits,
        adc: FakeAdc,
        ui: u8,
        uis: Vec<u8>,
        levels: Vec<u8>,
        offs: usize,
    }

    impl Run {
        fn new() -> Self {
            Self {
                limits: Limits::new(FakeAdc::CALIBRATION),
                adc: FakeAdc::new(),
                ui: 0,
                uis: Vec::new(),
                levels: Vec::new(),
                offs: 0,
            }
        }

        fn tick(&mut self) -> u8 {
            let ui = self.ui;
            let step = self.limits.tick(ui, self.adc.result());
            self.adc.start(self.limits.convert());
            if step.off {
                self.offs += 1;
                self.ui = 0;
            }
            // Only a change of UI level (or the cell going flat) may jump.
            let steady = self.uis.last() == Some(&ui) && ui != 0 && !step.off;
            if let (Some(&last), true) = (self.levels.last(), steady) {
                assert!(
                    last.abs_diff(step.level) <= 1,
                    "jumped {last} → {}",
                    step.level
                );
            }
            self.uis.push(ui);
            self.levels.push(step.level);
            step.level
        }

        fn ticks(&mut self, ticks: u32) -> u8 {
            for _ in 0..ticks {
                self.tick();
            }
            *self.levels.last().unwrap()
        }

        fn secs(&mut self, seconds: u32) -> u8 {
            self.ticks(seconds * u32::from(TICKS_PER_SECOND))
        }
    }

    /// The raw voltage reading for a calibrated `cv`, as near as it gets.
    fn volts(cv: u16) -> u16 {
        (1..4 * 1023)
            .min_by_key(|&raw| battery::centivolts(raw).abs_diff(cv))
            .unwrap()
    }

    /// The raw temperature reading for `degrees` Celsius.
    fn celsius(degrees: u8) -> u16 {
        let kelvin = thermal::celsius(degrees);
        (0..1024)
            .min_by_key(|&raw| FakeAdc::CALIBRATION.kelvin(raw).abs_diff(kelvin))
            .unwrap()
    }

    #[test]
    fn a_good_cell_and_a_cool_head_leave_the_ui_alone() {
        let mut run = Run::new();
        assert_eq!(run.secs(1), 0, "no blink with good readings");
        for ui in [1, 50, LINEAR_MAX, LEVELS] {
            run.ui = ui;
            assert_eq!(run.secs(60), ui);
            assert!(run.levels.iter().rev().take(60 * 64).all(|&l| l == ui));
        }
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn a_hot_head_fades_down_to_the_linear_channel() {
        let mut run = Run::new();
        run.ui = LEVELS;
        run.secs(2);
        run.adc.temperature = celsius(70);
        let level = run.secs(2);
        assert!(level < LEVELS && level > LINEAR_MAX, "{level}");
        assert_eq!(run.secs(120), LINEAR_MAX);
        assert_eq!(run.limits.ceilings().1, LINEAR_MAX);
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn heat_leaves_a_level_under_the_floor_alone() {
        let mut run = Run::new();
        run.ui = 50;
        run.adc.temperature = celsius(80);
        assert_eq!(run.secs(120), 50);
    }

    #[test]
    fn a_low_cell_steps_down_gradually_then_turns_off() {
        let mut run = Run::new();
        run.adc.voltage = volts(305);
        run.ui = 50;
        assert_eq!(run.secs(4), 50);
        assert_eq!(run.secs(2), 35);
        assert_eq!(run.secs(5), 20);
        // From 50 to 35 took a level a tick, not one jump.
        for level in 35..50 {
            assert!(run.levels.contains(&level), "{level}");
        }
        run.secs(30);
        assert_eq!(run.offs, 1);
        assert_eq!(*run.levels.last().unwrap(), 0);
    }

    #[test]
    fn a_flat_cell_on_the_fet_drops_to_the_linear_channel_first() {
        let mut run = Run::new();
        run.ui = LEVELS;
        run.secs(1);
        run.adc.voltage = volts(290);
        // The filter takes four seconds or so to fall under 2.99 V, then the
        // hold a second, so it's still at the top.
        assert_eq!(run.secs(5), LEVELS);
        assert_eq!(run.offs, 0);
        // Then it fades towards the linear channel, and with the cell still
        // flat a second later it turns off, without ever jumping on the way.
        run.secs(5);
        assert_eq!(run.offs, 1);
        let lit = run.levels.iter().rev().skip_while(|&&l| l == 0);
        assert!(lit.take(10).all(|&l| l < LEVELS - 50));
    }

    #[test]
    fn a_flat_cell_turns_off_again_after_turning_back_on() {
        let mut run = Run::new();
        run.adc.voltage = volts(290);
        run.ui = 50;
        run.secs(3);
        assert_eq!(run.offs, 1);
        run.ui = 50;
        assert_eq!(run.ticks(16), 50);
        run.secs(2);
        assert_eq!(run.offs, 2);
    }

    #[test]
    fn garbage_voltages_cap_it_at_the_linear_channel_and_never_turn_it_off() {
        for garbage in [0, 4 * 1023, 1] {
            let mut run = Run::new();
            run.ui = LEVELS;
            run.secs(2);
            run.adc.voltage = garbage;
            run.secs(1);
            assert_eq!(run.secs(10), LINEAR_MAX, "{garbage}");
            assert_eq!(run.secs(600), LINEAR_MAX, "{garbage}");
            assert_eq!(run.offs, 0);
        }
    }

    #[test]
    fn a_dead_adc_floors_both_ceilings() {
        let mut run = Run::new();
        run.ui = LEVELS;
        run.secs(1);
        run.adc.voltage = 0;
        run.adc.temperature = 0;
        assert_eq!(run.secs(30), LINEAR_MAX);
        assert_eq!(run.limits.ceilings(), (LINEAR_MAX, LINEAR_MAX));
        assert_eq!(run.offs, 0);
    }

    #[test]
    fn no_result_at_all_is_garbage_too() {
        let mut limits = Limits::new(FakeAdc::CALIBRATION);
        for _ in 0..30 * TICKS_PER_SECOND {
            limits.tick(LEVELS, None);
        }
        assert_eq!(limits.ceilings(), (LINEAR_MAX, LINEAR_MAX));
    }

    #[test]
    fn the_temperature_is_only_read_on_or_at_power_on() {
        let mut run = Run::new();
        let mut temperatures = 0;
        for tick in 0..60 * TICKS_PER_SECOND {
            run.tick();
            let reads = run.limits.convert().map(|c| c.channel)
                == Some(crate::sensors::Channel::Temperature);
            if reads {
                assert!(tick < 16, "tick {tick}");
                temperatures += 1;
            }
        }
        assert_eq!(temperatures, 2, "a setup and the real one");
    }

    #[test]
    fn turning_on_resets_thermal_regulation() {
        let mut run = Run::new();
        run.ui = LEVELS;
        run.adc.temperature = celsius(70);
        run.secs(60);
        assert_eq!(run.limits.ceilings().1, LINEAR_MAX);
        run.ui = 0;
        run.adc.temperature = celsius(25);
        run.secs(1);
        run.ui = LEVELS;
        // Straight back to the top: last time's heat doesn't carry over.
        assert_eq!(run.ticks(1), LEVELS);
    }

    #[test]
    fn garbage_at_power_on_blinks_once_dimly() {
        for (voltage, temperature) in [(0, 531), (1731, 0), (0, 0), (4 * 1023, 1023)] {
            let mut run = Run::new();
            run.adc.voltage = voltage;
            run.adc.temperature = temperature;
            run.secs(10);
            let lit: Vec<usize> = (0..run.levels.len())
                .filter(|&t| run.levels[t] != 0)
                .collect();
            assert_eq!(
                lit.len(),
                usize::from(BLINK_TICKS),
                "{voltage}, {temperature}"
            );
            assert!(lit.windows(2).all(|w| w[1] == w[0] + 1), "one blink");
            assert!(lit.iter().all(|&t| run.levels[t] == BLINK_LEVEL));
            assert!(lit[0] < 16, "within the first round");
        }
    }

    #[test]
    fn garbage_later_on_doesnt_blink() {
        let mut run = Run::new();
        run.secs(1);
        run.adc.voltage = 0;
        run.adc.temperature = 0;
        assert!(run.levels.iter().all(|&l| l == 0));
        run.secs(10);
        assert!(run.levels.iter().all(|&l| l == 0));
    }

    #[test]
    fn turning_on_mid_blink_stops_it() {
        let mut run = Run::new();
        run.adc.voltage = 0;
        // The blink starts once the first temperature reading's in.
        assert_eq!(run.ticks(10), 0);
        assert_eq!(run.ticks(1), BLINK_LEVEL);
        run.ui = 50;
        run.ticks(1);
        run.ui = 0;
        assert_eq!(run.ticks(1), 0);
        run.secs(2);
        assert!(run.levels.iter().rev().take(128).all(|&l| l == 0));
    }
}
