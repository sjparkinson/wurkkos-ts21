//! What each click and hold does. docs/ui.md is the spec; keep them in step.

use crate::button::Event;
use crate::ramp::LEVELS;

/// The dimmest level, used for moonlight.
pub const FLOOR: u8 = 1;

/// The brightest level, used for turbo.
pub const CEIL: u8 = LEVELS;

/// Where a fresh torch starts when you click it on.
pub const DEFAULT_LEVEL: u8 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    On,
    Lockout,
}

#[derive(Clone, Debug)]
pub struct Ui {
    mode: Mode,
    level: u8,
    /// The level to come back on at, from the last time it was switched off.
    memory: u8,
    /// The level to drop back to when leaving turbo.
    before_turbo: u8,
    /// Set when a hold switched the light on, so that the same hold doesn't
    /// go on to ramp. Cleared on release.
    swallow_hold: bool,
    /// Holding the switch in lockout for a momentary moonlight.
    momentary: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub const fn new() -> Self {
        Self {
            mode: Mode::Off,
            level: DEFAULT_LEVEL,
            memory: DEFAULT_LEVEL,
            before_turbo: DEFAULT_LEVEL,
            swallow_hold: false,
            momentary: false,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The level the LEDs should be at right now, `0` meaning off.
    pub fn output_level(&self) -> u8 {
        match self.mode {
            Mode::On => self.level,
            Mode::Lockout if self.momentary => FLOOR,
            Mode::Off | Mode::Lockout => 0,
        }
    }

    pub fn handle(&mut self, event: Event) {
        match (self.mode, event) {
            (_, Event::Release { .. }) => {
                self.swallow_hold = false;
                self.momentary = false;
            }

            (Mode::Off, Event::Click(1)) => self.turn_on(self.memory),
            (Mode::Off, Event::Click(2)) => {
                self.before_turbo = self.memory;
                self.turn_on(CEIL);
            }
            (Mode::Off, Event::Click(4)) => self.mode = Mode::Lockout,
            (Mode::Off, Event::Hold { clicks: 0, .. }) => {
                self.turn_on(FLOOR);
                self.swallow_hold = true;
            }

            (Mode::On, Event::Click(1)) => {
                self.memory = self.level;
                self.mode = Mode::Off;
            }
            (Mode::On, Event::Click(2)) if self.level == CEIL => self.level = self.before_turbo,
            (Mode::On, Event::Click(2)) => {
                self.before_turbo = self.level;
                self.level = CEIL;
            }
            (Mode::On, Event::Hold { .. }) if self.swallow_hold => {}
            (Mode::On, Event::Hold { clicks: 0, .. }) => {
                self.level = self.level.saturating_add(1).min(CEIL);
            }
            (Mode::On, Event::Hold { clicks: 1, .. }) => {
                self.level = self.level.saturating_sub(1).max(FLOOR);
            }

            (Mode::Lockout, Event::Click(4)) => self.mode = Mode::Off,
            (Mode::Lockout, Event::Hold { clicks: 0, .. }) => self.momentary = true,

            _ => {}
        }
    }

    fn turn_on(&mut self, level: u8) {
        self.mode = Mode::On;
        self.level = level;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hold(clicks: u8, ticks: u16) -> Event {
        Event::Hold { clicks, ticks }
    }

    fn hold_for(ui: &mut Ui, clicks: u8, ticks: u16) {
        for t in 0..ticks {
            ui.handle(hold(clicks, t));
        }
        ui.handle(Event::Release { clicks });
    }

    #[test]
    fn starts_off() {
        let ui = Ui::new();
        assert_eq!(ui.mode(), Mode::Off);
        assert_eq!(ui.output_level(), 0);
    }

    #[test]
    fn click_on_and_off_remembers_the_level() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(1));
        assert_eq!(ui.output_level(), DEFAULT_LEVEL);
        hold_for(&mut ui, 0, 10);
        ui.handle(Event::Click(1));
        assert_eq!(ui.output_level(), 0);
        ui.handle(Event::Click(1));
        assert_eq!(ui.output_level(), DEFAULT_LEVEL + 10);
    }

    #[test]
    fn hold_from_off_is_moonlight_and_does_not_ramp() {
        let mut ui = Ui::new();
        hold_for(&mut ui, 0, 30);
        assert_eq!(ui.mode(), Mode::On);
        assert_eq!(ui.output_level(), FLOOR);
        hold_for(&mut ui, 0, 3);
        assert_eq!(ui.output_level(), FLOOR + 3);
    }

    #[test]
    fn ramps_stop_at_the_ends() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(1));
        hold_for(&mut ui, 0, 500);
        assert_eq!(ui.output_level(), CEIL);
        hold_for(&mut ui, 1, 500);
        assert_eq!(ui.output_level(), FLOOR);
    }

    #[test]
    fn double_click_toggles_turbo() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(1));
        ui.handle(Event::Click(2));
        assert_eq!(ui.output_level(), CEIL);
        ui.handle(Event::Click(2));
        assert_eq!(ui.output_level(), DEFAULT_LEVEL);
    }

    #[test]
    fn double_click_from_off_is_turbo() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(2));
        assert_eq!(ui.output_level(), CEIL);
        ui.handle(Event::Click(2));
        assert_eq!(ui.output_level(), DEFAULT_LEVEL);
    }

    #[test]
    fn lockout_ignores_clicks_and_allows_momentary_moonlight() {
        let mut ui = Ui::new();
        ui.handle(Event::Click(4));
        assert_eq!(ui.mode(), Mode::Lockout);
        ui.handle(Event::Click(1));
        ui.handle(Event::Click(2));
        assert_eq!(ui.output_level(), 0);
        ui.handle(hold(0, 0));
        assert_eq!(ui.output_level(), FLOOR);
        ui.handle(Event::Release { clicks: 0 });
        assert_eq!(ui.output_level(), 0);
        ui.handle(Event::Click(4));
        assert_eq!(ui.mode(), Mode::Off);
    }
}
