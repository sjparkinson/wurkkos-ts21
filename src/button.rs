//! Turns one switch sample per tick into clicks and holds.
//!
//! Sampling at 64 Hz debounces the switch for free: contact bounce is over
//! well within the 15.6 ms between samples.

use crate::TICKS_PER_SECOND;

/// Press for this long and it's a hold, not a click (about 375 ms).
/// Counted from the tick the press is first seen, which is tick 0.
pub const HOLD_TICKS: u16 = TICKS_PER_SECOND * 3 / 8;

/// Leave the switch alone this long and the clicks so far count (about 280 ms).
pub const RELEASE_TICKS: u16 = TICKS_PER_SECOND * 9 / 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `n` quick clicks, then nothing for [`RELEASE_TICKS`].
    Click(u8),
    /// The switch is being held after `clicks` quick clicks (so `2` is
    /// click, click, hold). Sent every tick while held; `ticks` counts from
    /// the moment the press became a hold.
    Hold { clicks: u8, ticks: u16 },
    /// A hold ended.
    Release { clicks: u8 },
}

#[derive(Clone, Debug, Default)]
pub struct Clicks {
    pressed: bool,
    count: u8,
    ticks: u16,
}

impl Clicks {
    pub const fn new() -> Self {
        Self {
            pressed: false,
            count: 0,
            ticks: 0,
        }
    }

    /// Feeds in one sample, `true` meaning the switch is down.
    pub fn tick(&mut self, pressed: bool) -> Option<Event> {
        self.ticks = self.ticks.saturating_add(1);

        match (self.pressed, pressed) {
            (false, true) => {
                self.pressed = true;
                self.ticks = 0;
                None
            }
            (true, true) if self.ticks >= HOLD_TICKS => Some(Event::Hold {
                clicks: self.count,
                ticks: self.ticks - HOLD_TICKS,
            }),
            (true, true) => None,
            (true, false) => {
                // `ticks` has gone up once since the last pressed tick, so
                // a press that sent Holds is now past HOLD_TICKS.
                let held = self.ticks > HOLD_TICKS;
                self.pressed = false;
                self.ticks = 0;
                if held {
                    Some(Event::Release {
                        clicks: core::mem::take(&mut self.count),
                    })
                } else {
                    self.count = self.count.saturating_add(1);
                    None
                }
            }
            (false, false) if self.count > 0 && self.ticks >= RELEASE_TICKS => {
                Some(Event::Click(core::mem::take(&mut self.count)))
            }
            (false, false) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a script of (pressed, ticks) spans and collects the events.
    fn run(script: &[(bool, u16)]) -> ([Option<Event>; 64], usize) {
        let mut clicks = Clicks::new();
        let mut events = [None; 64];
        let mut n = 0;
        for &(pressed, ticks) in script {
            for _ in 0..ticks {
                if let Some(event) = clicks.tick(pressed) {
                    if n < events.len() {
                        events[n] = Some(event);
                    }
                    n += 1;
                }
            }
        }
        (events, n)
    }

    #[test]
    fn single_click() {
        let (events, n) = run(&[(true, 5), (false, RELEASE_TICKS + 5)]);
        assert_eq!(n, 1);
        assert_eq!(events[0], Some(Event::Click(1)));
    }

    #[test]
    fn three_clicks() {
        let (events, n) = run(&[
            (true, 4),
            (false, 6),
            (true, 4),
            (false, 6),
            (true, 4),
            (false, RELEASE_TICKS + 1),
        ]);
        assert_eq!(n, 1);
        assert_eq!(events[0], Some(Event::Click(3)));
    }

    #[test]
    fn nothing_until_the_release_timeout() {
        let (_, n) = run(&[(true, 4), (false, RELEASE_TICKS - 1)]);
        assert_eq!(n, 0);
    }

    #[test]
    fn hold_then_release() {
        let (events, n) = run(&[(true, HOLD_TICKS + 4), (false, RELEASE_TICKS + 1)]);
        assert_eq!(n, 4 + 1);
        assert_eq!(
            events[0],
            Some(Event::Hold {
                clicks: 0,
                ticks: 0
            })
        );
        assert_eq!(
            events[3],
            Some(Event::Hold {
                clicks: 0,
                ticks: 3
            })
        );
        assert_eq!(events[4], Some(Event::Release { clicks: 0 }));
    }

    #[test]
    fn click_then_hold() {
        let (events, n) = run(&[(true, 4), (false, 6), (true, HOLD_TICKS + 2), (false, 1)]);
        assert_eq!(n, 3);
        assert_eq!(
            events[0],
            Some(Event::Hold {
                clicks: 1,
                ticks: 0
            })
        );
        assert_eq!(events[2], Some(Event::Release { clicks: 1 }));
    }

    #[test]
    fn a_click_after_a_hold_starts_from_zero() {
        let (events, n) = run(&[
            (true, HOLD_TICKS + 1),
            (false, 6),
            (true, 4),
            (false, RELEASE_TICKS + 1),
        ]);
        assert_eq!(events[n - 1], Some(Event::Click(1)));
    }
}
