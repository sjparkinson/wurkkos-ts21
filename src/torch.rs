//! One pass of the firmware's loop, minus the hardware: a switch sample and
//! an ADC result in, a level and the next conversion out. `src/main.rs` and
//! `cargo xtask simulate` both run this, so the simulator can't drift from
//! what the torch does.

use crate::button::{Clicks, Event};
use crate::limits::Limits;
use crate::sensors::Convert;
use crate::temp::Calibration;
use crate::ui::Ui;

#[derive(Clone, Debug)]
pub struct Torch {
    clicks: Clicks,
    ui: Ui,
    limits: Limits,
    level: u8,
}

impl Torch {
    /// `calibration` is the temperature sensor's, from the signature row.
    pub const fn new(calibration: Calibration) -> Self {
        Self {
            clicks: Clicks::new(),
            ui: Ui::new(),
            limits: Limits::new(calibration),
            level: 0,
        }
    }

    /// One 64 Hz tick, `pressed` meaning the switch is down and `adc` the
    /// ADC's result, if it has one ready. Returns the click or hold this
    /// sample finished, if any.
    pub fn tick(&mut self, pressed: bool, adc: Option<u16>) -> Option<Event> {
        let event = self.clicks.tick(pressed);
        if let Some(event) = event {
            self.ui.handle(event);
        }
        let step = self.limits.tick(self.ui.output_level(), adc);
        if step.off {
            self.ui.off();
        }
        self.level = step.level;
        event
    }

    /// The level the LEDs should be at right now, `0` meaning off.
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The conversion to start on the ADC after this tick, if any.
    pub fn convert(&self) -> Option<Convert> {
        self.limits.convert()
    }

    pub fn ui(&self) -> &Ui {
        &self.ui
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }
}

/// The last value sent to the hardware, so the loop only rewrites the
/// timer when the output moves.
#[derive(Clone, Copy, Debug, Default)]
pub struct Changes<T> {
    last: Option<T>,
}

impl<T: Copy + PartialEq> Changes<T> {
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// `Some(value)` the first time and whenever `value` differs from the
    /// last one seen, `None` otherwise.
    pub fn update(&mut self, value: T) -> Option<T> {
        if self.last == Some(value) {
            return None;
        }
        self.last = Some(value);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::button::RELEASE_TICKS;
    use crate::limits::{BLINK_LEVEL, BLINK_TICKS};
    use crate::ramp::{LEVELS, LINEAR_MAX, Output, output};
    use crate::sensors::FakeAdc;
    use crate::ui::LEVEL;

    /// A torch and the ADC it reads.
    struct Rig {
        torch: Torch,
        adc: FakeAdc,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                torch: Torch::new(FakeAdc::CALIBRATION),
                adc: FakeAdc::new(),
            }
        }

        fn tick(&mut self, pressed: bool) -> Option<Event> {
            let event = self.torch.tick(pressed, self.adc.result());
            self.adc.start(self.torch.convert());
            event
        }

        fn run(&mut self, pressed: bool, ticks: u16) {
            for _ in 0..ticks {
                self.tick(pressed);
            }
        }

        fn click(&mut self) {
            self.run(true, 4);
            self.run(false, RELEASE_TICKS + 1);
        }

        fn level(&self) -> u8 {
            self.torch.level()
        }
    }

    #[test]
    fn starts_off() {
        let mut rig = Rig::new();
        assert_eq!(rig.tick(false), None);
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn a_click_turns_it_on_once_the_click_counts() {
        let mut rig = Rig::new();
        rig.run(true, 4);
        rig.run(false, RELEASE_TICKS);
        assert_eq!(rig.level(), 0);
        assert_eq!(rig.tick(false), Some(Event::Click(1)));
        assert_eq!(rig.level(), LEVEL);
    }

    #[test]
    fn still_turns_off_after_hours_on() {
        let mut rig = Rig::new();
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        // Past where button::Clicks' tick count saturates.
        for _ in 0..4 {
            rig.run(false, u16::MAX);
        }
        assert_eq!(rig.level(), LEVEL);
        rig.click();
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn matches_clicks_and_ui_run_by_hand_with_good_readings() {
        let samples = [(true, 4), (false, 30), (true, 40), (false, 30), (true, 3)];
        let (mut rig, mut clicks, mut ui) = (Rig::new(), Clicks::new(), Ui::new());
        for (pressed, ticks) in samples {
            for _ in 0..ticks {
                let event = clicks.tick(pressed);
                if let Some(event) = event {
                    ui.handle(event);
                }
                assert_eq!(rig.tick(pressed), event);
                assert_eq!(rig.level(), ui.output_level());
            }
        }
    }

    #[test]
    fn a_flat_cell_turns_the_ui_off() {
        let mut rig = Rig::new();
        rig.adc.voltage = 2331; // 2.90 V
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        rig.run(false, 2 * 64);
        assert_eq!(rig.level(), 0);
        assert_eq!(rig.torch.ui().output_level(), 0);
        // It comes back on with a click, and goes off again.
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        rig.run(false, 2 * 64);
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn a_low_cell_dims_it_a_level_at_a_time() {
        let mut rig = Rig::new();
        rig.adc.voltage = 2204; // 3.05 V
        rig.click();
        let mut last = rig.level();
        for _ in 0..6 * 64 {
            rig.tick(false);
            assert!(last.abs_diff(rig.level()) <= 1);
            last = rig.level();
        }
        assert_eq!(last, LEVEL - crate::battery::STEP);
    }

    #[test]
    fn a_recovered_cell_fades_it_back_up() {
        let mut rig = Rig::new();
        rig.adc.voltage = 2204; // 3.05 V
        rig.click();
        rig.run(false, 6 * 64);
        assert_eq!(rig.level(), LEVEL - crate::battery::STEP);
        rig.adc.voltage = 1810; // 3.65 V
        let mut last = rig.level();
        for _ in 0..10 * 64 {
            rig.tick(false);
            assert!(rig.level() >= last && rig.level() - last <= 1);
            last = rig.level();
        }
        assert_eq!(last, LEVEL);
    }

    #[test]
    fn a_dead_adc_blinks_at_power_on_and_still_works() {
        let mut rig = Rig::new();
        (rig.adc.voltage, rig.adc.temperature) = (0, 0);
        let mut lit = 0;
        for _ in 0..64 {
            rig.tick(false);
            if rig.level() != 0 {
                assert_eq!(rig.level(), BLINK_LEVEL);
                lit += 1;
            }
        }
        assert_eq!(lit, BLINK_TICKS);
        // At level 50 the floor's above it, so on and off work as usual.
        rig.click();
        rig.run(false, 60 * 64);
        assert_eq!(rig.level(), LEVEL);
        rig.click();
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn the_first_output_always_counts_as_a_change() {
        assert_eq!(Changes::new().update(Output::OFF), Some(Output::OFF));
        let on = output(LEVEL);
        assert_eq!(Changes::new().update(on), Some(on));
    }

    #[test]
    fn an_unchanged_output_is_not_a_change() {
        let mut changes = Changes::new();
        changes.update(output(LEVEL));
        assert_eq!(changes.update(output(LEVEL)), None);
        assert_eq!(changes.update(output(LEVEL)), None);
    }

    #[test]
    fn turning_on_and_off_are_changes() {
        let (off, on) = (output(0), output(LEVEL));
        let mut changes = Changes::new();
        assert_eq!(changes.update(off), Some(off));
        assert_eq!(changes.update(off), None);
        assert_eq!(changes.update(on), Some(on));
        assert_eq!(changes.update(off), Some(off));
        assert_eq!(changes.update(off), None);
    }

    #[test]
    fn moving_between_lit_levels_is_a_change() {
        let mut changes = Changes::new();
        let (linear, fet) = (output(LINEAR_MAX), output(LEVELS));
        changes.update(linear);
        assert_eq!(changes.update(fet), Some(fet));
        assert_eq!(changes.update(linear), Some(linear));
    }
}
