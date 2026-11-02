//! One pass of the firmware's loop, minus the hardware: a switch sample in,
//! a level out. `src/main.rs` and `cargo xtask simulate` both run this, so
//! the simulator can't drift from what the torch does.

use crate::button::{Clicks, Event};
use crate::ui::Ui;

#[derive(Clone, Debug, Default)]
pub struct Torch {
    clicks: Clicks,
    ui: Ui,
}

impl Torch {
    pub const fn new() -> Self {
        Self {
            clicks: Clicks::new(),
            ui: Ui::new(),
        }
    }

    /// One 64 Hz tick, `pressed` meaning the switch is down. Returns the
    /// click or hold this sample finished, if any.
    pub fn tick(&mut self, pressed: bool) -> Option<Event> {
        let event = self.clicks.tick(pressed);
        if let Some(event) = event {
            self.ui.handle(event);
        }
        event
    }

    /// The level the LEDs should be at right now, `0` meaning off.
    pub fn level(&self) -> u8 {
        self.ui.output_level()
    }

    pub fn ui(&self) -> &Ui {
        &self.ui
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
    use crate::ui::LEVEL;

    fn run(torch: &mut Torch, pressed: bool, ticks: u16) {
        for _ in 0..ticks {
            torch.tick(pressed);
        }
    }

    #[test]
    fn starts_off() {
        let mut torch = Torch::new();
        assert_eq!(torch.tick(false), None);
        assert_eq!(torch.level(), 0);
    }

    #[test]
    fn a_click_turns_it_on_once_the_click_counts() {
        let mut torch = Torch::new();
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS);
        assert_eq!(torch.level(), 0);
        assert_eq!(torch.tick(false), Some(Event::Click(1)));
        assert_eq!(torch.level(), LEVEL);
    }

    #[test]
    fn still_turns_off_after_hours_on() {
        let mut torch = Torch::new();
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS + 1);
        assert_eq!(torch.level(), LEVEL);
        // Past where button::Clicks' tick count saturates.
        for _ in 0..4 {
            run(&mut torch, false, u16::MAX);
        }
        assert_eq!(torch.level(), LEVEL);
        run(&mut torch, true, 4);
        run(&mut torch, false, RELEASE_TICKS + 1);
        assert_eq!(torch.level(), 0);
    }

    #[test]
    fn matches_clicks_and_ui_run_by_hand() {
        let samples = [(true, 4), (false, 30), (true, 40), (false, 30), (true, 3)];
        let (mut torch, mut clicks, mut ui) = (Torch::new(), Clicks::new(), Ui::new());
        for (pressed, ticks) in samples {
            for _ in 0..ticks {
                let event = clicks.tick(pressed);
                if let Some(event) = event {
                    ui.handle(event);
                }
                assert_eq!(torch.tick(pressed), event);
                assert_eq!(torch.level(), ui.output_level());
            }
        }
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
