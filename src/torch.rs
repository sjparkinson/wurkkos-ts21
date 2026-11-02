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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::button::RELEASE_TICKS;
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
}
