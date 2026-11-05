//! One pass of the firmware's loop, minus the hardware: a switch sample and
//! an ADC result in, a level and the next conversion out. `src/main.rs` and
//! `cargo xtask simulate` both run this, so the simulator can't drift from
//! what the torch does.

use crate::button::{Clicks, Event};
use crate::limits::Limits;
use crate::ramp::output;
use crate::selftest::{ClockCheck, Faults};
use crate::sensors::Convert;
use crate::temp::Calibration;
use crate::ui::Ui;

#[derive(Clone, Debug)]
pub struct Torch {
    clicks: Clicks,
    ui: Ui,
    limits: Limits,
    clock: ClockCheck,
}

impl Torch {
    /// `calibration` is the temperature sensor's, from the signature row,
    /// and `faults` what the power-on self-tests found.
    pub const fn new(calibration: Calibration, faults: Faults) -> Self {
        Self {
            clicks: Clicks::new(),
            ui: Ui::new(),
            limits: Limits::new(calibration, faults),
            clock: ClockCheck::new(),
        }
    }

    /// One 64 Hz tick, `pressed` meaning the switch is down and `adc` the
    /// ADC's result, if it has one ready. Returns the click or hold this
    /// sample finished, if any. After a failed clock check it does nothing
    /// but keep the light off.
    pub fn tick(&mut self, pressed: bool, adc: Option<u16>) -> Option<Event> {
        if self.clock.failed() {
            self.ui.off();
            self.limits.rest();
            return None;
        }
        let event = self.clicks.tick(pressed);
        if let Some(event) = event {
            self.ui.handle(event);
        }
        if !self.resting() && self.limits.tick(self.ui.output_level(), adc).off {
            self.ui.off();
        }
        // Checked again, so a pass that turns the light off has already
        // rested if the firmware powers down straight after it.
        let resting = self.resting();
        if resting {
            self.limits.rest();
        }
        self.clock.tick(resting);
        event
    }

    /// Whether there's nothing to do until the switch moves: the light's
    /// off and no click is being counted. The firmware powers down between
    /// these ticks, and a press ends it on the tick that sees the press, in
    /// time to read the cell before a click can turn the light on. A torch
    /// that's failed its clock check rests for good.
    pub fn resting(&self) -> bool {
        self.clock.failed()
            || self.clicks.idle() && self.ui.output_level() == 0 && self.limits.level() == 0
    }

    /// Whether the wait for the next tick should count TCA0's clocks and hand
    /// them to [`Torch::clock`].
    pub fn clock_due(&self) -> bool {
        self.clock.due()
    }

    /// What the wait for a tick counted, or `selftest::NO_TICK` if the tick
    /// never came.
    pub fn clock(&mut self, clocks: u32) {
        self.clock.result(clocks, output(self.level()).top);
    }

    /// Whether the clock check has failed, so the light's off for good.
    pub fn clock_failed(&self) -> bool {
        self.clock.failed()
    }

    /// The level the LEDs should be at right now, `0` meaning off.
    pub fn level(&self) -> u8 {
        self.limits.level()
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
    use crate::ramp::{LEVELS, LINEAR_MAX, Output, output};
    use crate::selftest::{CLOCKS_MAX, CLOCKS_MIN, NO_TICK};
    use crate::sensors::Channel;
    use crate::testing::{Adc, CALIBRATION, voltage_reading};
    use crate::ui::LEVEL;

    /// A torch and the ADC it reads.
    struct Rig {
        torch: Torch,
        adc: Adc,
    }

    impl Rig {
        fn new() -> Self {
            Self::with(Faults::NONE)
        }

        fn with(faults: Faults) -> Self {
            Self {
                torch: Torch::new(CALIBRATION, faults),
                adc: Adc::new(),
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
    fn a_dead_adc_stays_dark_from_off_and_still_works() {
        let mut rig = Rig::new();
        (rig.adc.voltage, rig.adc.temperature) = (0, 0);
        // Resting reads nothing, so hold the switch to read the garbage.
        rig.run(true, 3 * 64);
        assert_eq!(rig.level(), 0);
        rig.run(false, RELEASE_TICKS + 1);
        // At level 50 the floor's above it, so on and off work as usual.
        rig.click();
        rig.run(false, 60 * 64);
        assert_eq!(rig.level(), LEVEL);
        rig.click();
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn bad_fuses_leave_the_ui_working_on_the_linear_channel() {
        let mut rig = Rig::with(Faults {
            fuses: true,
            ..Faults::NONE
        });
        rig.run(false, 3 * 64);
        assert_eq!(rig.level(), 0);
        // Level 50's on the linear channel, so on and off work as usual.
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        assert_eq!(output(rig.level()).fet, 0);
        rig.click();
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn a_refused_watchdog_leaves_the_ui_working_on_the_linear_channel() {
        let mut rig = Rig::with(Faults {
            watchdog: true,
            ..Faults::NONE
        });
        rig.run(false, 3 * 64);
        assert_eq!(rig.level(), 0);
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        assert_eq!(output(rig.level()).fet, 0);
        rig.click();
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn one_pressed_tick_is_a_click() {
        // `src/main.rs` counts its first tick as pressed if the watchdog's
        // proof owes it a press, and a tick is all the press that's left.
        let mut rig = Rig::new();
        rig.run(true, 1);
        rig.run(false, RELEASE_TICKS + 1);
        assert_eq!(rig.level(), LEVEL);
    }

    #[test]
    fn a_bad_clock_turns_the_light_off_on_the_next_tick_and_clicks_do_nothing() {
        for clocks in [0, CLOCKS_MIN / 2, CLOCKS_MAX + 1, NO_TICK] {
            let mut rig = Rig::new();
            rig.click();
            assert_eq!(rig.level(), LEVEL);
            rig.torch.clock(clocks);
            rig.tick(false);
            // A low count gets a second go; the rest have already failed.
            if rig.torch.clock_due() {
                rig.torch.clock(clocks);
                rig.tick(false);
            }
            assert_eq!(rig.level(), 0, "{clocks}");
            assert!(rig.torch.resting(), "{clocks}");
            assert_eq!(rig.torch.convert(), None, "{clocks}");
            for _ in 0..3 {
                rig.click();
                assert_eq!(rig.level(), 0, "{clocks}");
                assert!(rig.torch.resting(), "{clocks}");
            }
            rig.run(true, 3 * 64);
            assert_eq!(rig.level(), 0, "{clocks}");
        }
    }

    #[test]
    fn a_failed_power_on_count_beats_the_first_click() {
        for clocks in [0, CLOCKS_MIN / 2, CLOCKS_MAX + 1, NO_TICK] {
            let mut rig = Rig::new();
            rig.tick(false);
            assert!(rig.torch.clock_due());
            rig.torch.clock(clocks);
            rig.tick(false);
            if rig.torch.clock_due() {
                rig.torch.clock(clocks);
            }
            rig.click();
            assert_eq!(rig.level(), 0, "{clocks}");
            assert!(rig.torch.resting(), "{clocks}");
        }
    }

    #[test]
    fn a_low_count_just_before_turning_off_is_settled_at_rest() {
        let mut rig = Rig::new();
        rig.click();
        rig.torch.clock(CLOCKS_MIN / 2);
        rig.click();
        rig.run(false, 64);
        assert!(rig.torch.resting());
        assert!(rig.torch.clock_due());
        rig.torch.clock(CLOCKS_MIN / 2);
        assert!(rig.torch.clock_failed());
    }

    #[test]
    fn one_low_count_leaves_the_light_on_if_the_next_is_good() {
        let mut rig = Rig::new();
        rig.click();
        rig.torch.clock(CLOCKS_MIN / 2);
        rig.tick(false);
        assert_eq!(rig.level(), LEVEL);
        assert!(rig.torch.clock_due());
        rig.torch.clock(306 * 255);
        rig.tick(false);
        assert_eq!(rig.level(), LEVEL);
        assert!(!rig.torch.clock_failed());
    }

    #[test]
    fn a_clock_on_the_edge_of_its_window_leaves_the_light_alone() {
        for clocks in [CLOCKS_MIN, CLOCKS_MAX] {
            let mut rig = Rig::new();
            rig.click();
            rig.torch.clock(clocks);
            rig.tick(false);
            assert_eq!(rig.level(), LEVEL);
            assert!(!rig.torch.clock_failed());
        }
    }

    #[test]
    fn a_stopped_timer_fails_at_the_next_count_while_its_on_and_for_good() {
        let mut rig = Rig::new();
        rig.tick(false);
        rig.torch.clock(306 * 255);
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        // TCA0 stops here. Off is as usual, and no count starts at rest.
        rig.click();
        assert_eq!(rig.level(), 0);
        rig.run(false, 3 * 64);
        assert!(!rig.torch.clock_due());
        assert!(!rig.torch.clock_failed());
        // The next time it's on, the count comes to nothing.
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        while !rig.torch.clock_due() {
            rig.tick(false);
        }
        rig.torch.clock(0);
        rig.tick(false);
        rig.torch.clock(0);
        rig.tick(false);
        assert_eq!(rig.level(), 0);
        rig.click();
        assert_eq!(rig.level(), 0, "for good");
    }

    #[test]
    fn counts_the_clock_after_the_first_tick_then_once_a_second_while_on() {
        let mut rig = Rig::new();
        assert!(!rig.torch.clock_due());
        rig.tick(false);
        assert!(rig.torch.clock_due());
        rig.torch.clock(306 * 255);
        rig.run(false, 3 * 64);
        assert!(!rig.torch.clock_due(), "at rest");
        rig.click();
        rig.run(false, u16::from(crate::selftest::CLOCK_CHECK_TICKS));
        assert!(rig.torch.clock_due());
    }

    #[test]
    fn rests_once_off_and_idle() {
        let mut rig = Rig::new();
        assert!(rig.torch.resting(), "from power-on");
        rig.run(false, 64);
        assert!(rig.torch.resting());
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        assert!(!rig.torch.resting(), "on");
        rig.click();
        assert_eq!(rig.level(), 0);
        assert!(rig.torch.resting(), "back off");
    }

    #[test]
    fn never_rests_mid_click() {
        let mut rig = Rig::new();
        rig.run(true, 4);
        assert!(!rig.torch.resting(), "pressed");
        for _ in 0..RELEASE_TICKS {
            rig.tick(false);
            assert!(!rig.torch.resting(), "a click waiting to count");
        }
    }

    #[test]
    fn the_pass_that_turns_it_off_has_already_rested() {
        // At every point in the ADC's round, some of which start a
        // conversion that power-down would cut off.
        for phase in 0..u16::from(crate::sensors::CYCLE_TICKS) {
            let mut rig = Rig::new();
            rig.click();
            rig.run(false, phase);
            rig.run(true, 4);
            rig.run(false, RELEASE_TICKS);
            assert_eq!(rig.level(), LEVEL, "{phase}");
            rig.tick(false);
            assert_eq!(rig.level(), 0, "{phase}");
            assert!(rig.torch.resting(), "{phase}");
            assert_eq!(rig.torch.convert(), None, "{phase}");
            // So a press on the very next tick sets the ADC up again.
            rig.tick(true);
            assert!(rig.torch.convert().is_some_and(|c| c.setup), "{phase}");
        }
    }

    #[test]
    fn clicks_that_do_nothing_rest_on_the_pass_they_count() {
        // The one way in that skips `Limits::tick`: the clicks finish with
        // the light off, so the torch is resting before it gets there. Each
        // press length lands that pass on a different point of the ADC's
        // round, some of which leave a conversion asked for.
        for press in 1..=u16::from(crate::sensors::CYCLE_TICKS) {
            let mut rig = Rig::new();
            rig.run(true, press);
            rig.run(false, 1);
            rig.run(true, press);
            rig.run(false, RELEASE_TICKS);
            assert!(!rig.torch.resting(), "{press}");
            assert_eq!(rig.tick(false), Some(Event::Click(2)), "{press}");
            assert!(rig.torch.resting(), "{press}");
            assert_eq!(rig.torch.convert(), None, "{press}");
            rig.tick(true);
            assert!(rig.torch.convert().is_some_and(|c| c.setup), "{press}");
        }
    }

    #[test]
    fn a_resting_torch_starts_no_conversions() {
        let mut rig = Rig::new();
        for _ in 0..64 {
            rig.tick(false);
            assert_eq!(rig.torch.convert(), None);
        }
    }

    #[test]
    fn a_press_ends_the_rest_and_reads_the_cell_before_the_light_comes_on() {
        let mut rig = Rig::new();
        rig.run(false, 64);
        rig.tick(true);
        let setup = Convert {
            channel: Channel::Voltage,
            setup: true,
        };
        assert_eq!(rig.torch.convert(), Some(setup), "on the tick it's seen");
        // The conversion after the one that's thrown away, a tick later, is
        // the reading, long before the click can count.
        rig.tick(true);
        let reading = Convert {
            channel: Channel::Voltage,
            setup: false,
        };
        assert_eq!(rig.torch.convert(), Some(reading));
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn clicking_off_and_on_doesnt_buy_a_sagging_cell_time() {
        let mut rig = Rig::new();
        // 2.90 V under load, off a second, and it reads 3.20 V at rest.
        rig.adc.voltage = 2331;
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        rig.run(false, 32);
        rig.click();
        rig.adc.voltage = voltage_reading(320);
        rig.run(false, 64);
        // Still unloaded for the click, then it sags as the light comes on.
        // A filter that started again from 3.20 V would take about 3.4 s.
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        rig.adc.voltage = 2331;
        rig.run(false, 2 * 64);
        assert_eq!(rig.level(), 0);
    }

    #[test]
    fn a_stale_voltage_never_outlives_a_long_rest() {
        let mut rig = Rig::new();
        rig.click();
        rig.run(false, 5 * 64);
        rig.click();
        rig.run(false, crate::battery::FORGET_TICKS);
        assert!(rig.torch.resting());
        // It runs flat in the drawer. Filtering down from 3.8 V would take
        // over four seconds, so this only passes on a fresh reading.
        rig.adc.voltage = 2331; // 2.90 V
        rig.click();
        assert_eq!(rig.level(), LEVEL);
        rig.run(false, 2 * 64);
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
