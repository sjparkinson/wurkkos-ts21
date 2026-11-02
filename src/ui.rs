//! What each click and hold does. docs/ui.md is the spec; keep them in step.
//!
//! For now it's the smallest UI that proves the hardware works: one click on,
//! one click off, at a single level. The full UI (ramping, turbo, lockout)
//! lives on the `fancy-ui` branch until the basics are checked on a real torch.

use crate::button::Event;
use crate::ramp::LINEAR_MAX;

/// The one level the torch runs at. It's on the linear channel, so the FET
/// stays off until the switch, PWM and pin map have been checked on hardware.
pub const LEVEL: u8 = 50;

const _: () = assert!(LEVEL >= 1 && LEVEL <= LINEAR_MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    On,
}

#[derive(Clone, Debug)]
pub struct Ui {
    mode: Mode,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub const fn new() -> Self {
        Self { mode: Mode::Off }
    }

    /// The level the LEDs should be at right now, `0` meaning off.
    pub fn output_level(&self) -> u8 {
        match self.mode {
            Mode::On => LEVEL,
            Mode::Off => 0,
        }
    }

    pub fn handle(&mut self, event: Event) {
        self.mode = match (self.mode, event) {
            (Mode::Off, Event::Click(1)) => Mode::On,
            (Mode::On, Event::Click(1)) => Mode::Off,
            (mode, _) => mode,
        };
    }

    /// Off, whatever it was doing: the cell's flat. The next 1C turns it on
    /// as usual, and low-voltage protection turns it off again if the cell's
    /// still flat.
    pub fn off(&mut self) {
        self.mode = Mode::Off;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_turns_it_off_and_1c_still_turns_it_on() {
        let mut ui = Ui::new();
        ui.off();
        assert_eq!(ui.output_level(), 0);
        ui.handle(Event::Click(1));
        ui.off();
        assert_eq!(ui.mode, Mode::Off);
        ui.handle(Event::Click(1));
        assert_eq!(ui.output_level(), LEVEL);
    }

    #[test]
    fn starts_off() {
        let ui = Ui::new();
        assert_eq!(ui.mode, Mode::Off);
        assert_eq!(ui.output_level(), 0);
    }

    #[test]
    fn one_click_turns_on_and_off() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(1));
        assert_eq!(ui.mode, Mode::On);
        assert_eq!(ui.output_level(), LEVEL);
        ui.handle(Event::Click(1));
        assert_eq!(ui.mode, Mode::Off);
        assert_eq!(ui.output_level(), 0);
    }

    #[test]
    fn everything_else_does_nothing() {
        let others = [
            Event::Click(2),
            Event::Click(3),
            Event::Click(4),
            Event::Hold {
                clicks: 0,
                ticks: 0,
            },
            Event::Hold {
                clicks: 1,
                ticks: 40,
            },
            Event::Release { clicks: 0 },
        ];
        for start in [Mode::Off, Mode::On] {
            for event in others {
                let mut ui = Ui { mode: start };
                ui.handle(event);
                assert_eq!(ui.mode, start, "{event:?} from {start:?}");
            }
        }
    }
}
