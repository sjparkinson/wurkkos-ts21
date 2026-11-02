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

/// Endless random switching, as (pressed, ticks) spans that alternate,
/// starting released. Most lengths sit right at the edges [`Clicks`] cares
/// about. It's for tests: this crate's own, and xtask's check of the
/// firmware against the library. The firmware never uses it.
///
/// Xorshift underneath, so a seed always gives the same switching.
#[derive(Clone, Debug)]
pub struct RandomSwitching {
    state: u32,
    pressed: bool,
}

impl RandomSwitching {
    /// Span lengths worth hitting exactly.
    const EDGES: [u16; 10] = [
        1,
        2,
        4,
        HOLD_TICKS - 1,
        HOLD_TICKS,
        HOLD_TICKS + 1,
        HOLD_TICKS + 2,
        RELEASE_TICKS - 1,
        RELEASE_TICKS,
        RELEASE_TICKS + 1,
    ];

    pub const fn new(seed: u32) -> Self {
        // Xorshift never leaves zero.
        Self {
            state: if seed == 0 { 1 } else { seed },
            pressed: true,
        }
    }

    fn random(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }
}

impl Iterator for RandomSwitching {
    type Item = (bool, u16);

    fn next(&mut self) -> Option<Self::Item> {
        self.pressed = !self.pressed;
        let ticks = match self.random() % 3 {
            0 => 1 + (self.random() % 100) as u16,
            _ => Self::EDGES[self.random() as usize % Self::EDGES.len()],
        };
        Some((self.pressed, ticks))
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

    // The tests above pick their timings by hand. These compare `Clicks`
    // against a second description of the same rules, written in terms of
    // whole presses and gaps rather than one tick at a time, over thousands
    // of random switchings and a few very long ones.

    extern crate std;
    use std::vec::Vec;

    /// Spans of the switch, as (pressed, ticks).
    type Runs = [(bool, u32)];

    /// What `Clicks` should send, and on which tick, from the presses and
    /// gaps alone. Runs must alternate between pressed and released.
    fn model(runs: &Runs) -> Vec<(u32, Event)> {
        let saturate = |ticks: u32| ticks.min(u32::from(u16::MAX)) as u16;
        let mut out = Vec::new();
        let (mut t, mut count) = (0, 0u8);
        // The length of the press that just ended, if one did.
        let mut press = None;
        for &(pressed, len) in runs {
            if pressed {
                // A press counts from its first tick, and is a hold from
                // HOLD_TICKS in, sending a Hold every tick after that.
                for i in u32::from(HOLD_TICKS)..len {
                    let ticks = saturate(i) - HOLD_TICKS;
                    out.push((
                        t + i,
                        Event::Hold {
                            clicks: count,
                            ticks,
                        },
                    ));
                }
                press = Some(len);
            } else {
                // The tick a hold ends sends Release; a shorter press adds
                // a click.
                match press.take() {
                    Some(len) if saturate(len) > HOLD_TICKS => {
                        out.push((t, Event::Release { clicks: count }));
                        count = 0;
                    }
                    Some(_) => count = count.saturating_add(1),
                    None => {}
                }
                // Clicks count once the switch has been up for
                // RELEASE_TICKS after the tick it came up.
                if count > 0 && len > u32::from(RELEASE_TICKS) {
                    out.push((t + u32::from(RELEASE_TICKS), Event::Click(count)));
                    count = 0;
                }
            }
            t += len;
        }
        out
    }

    fn actual(runs: &Runs) -> Vec<(u32, Event)> {
        let mut clicks = Clicks::new();
        let mut out = Vec::new();
        let mut t = 0;
        for &(pressed, len) in runs {
            for _ in 0..len {
                if let Some(event) = clicks.tick(pressed) {
                    out.push((t, event));
                }
                t += 1;
            }
        }
        out
    }

    #[test]
    fn matches_the_model_on_random_switching() {
        for seed in 1..=500 {
            let runs: Vec<_> = RandomSwitching::new(seed)
                .take(60)
                .map(|(pressed, ticks)| (pressed, u32::from(ticks)))
                .collect();
            assert_eq!(actual(&runs), model(&runs), "seed {seed}: {runs:?}");
        }
    }

    #[test]
    fn random_switching_alternates_from_released() {
        let spans: Vec<_> = RandomSwitching::new(0).take(1000).collect();
        assert!(spans.iter().step_by(2).all(|&(pressed, _)| !pressed));
        assert!(spans.iter().skip(1).step_by(2).all(|&(pressed, _)| pressed));
        assert!(spans.iter().all(|&(_, ticks)| (1..=100).contains(&ticks)));
    }

    #[test]
    fn a_click_still_counts_after_a_long_wait() {
        // Longer than `ticks` can count to, so it has saturated.
        let runs = [(false, 70_000), (true, 4), (false, 30)];
        assert_eq!(actual(&runs), model(&runs));
        assert_eq!(
            actual(&runs).last(),
            Some(&(70_004 + u32::from(RELEASE_TICKS), Event::Click(1)))
        );
    }

    #[test]
    fn a_very_long_hold_saturates_and_still_releases() {
        let runs = [(true, 70_000), (false, 2)];
        let events = actual(&runs);
        assert_eq!(events, model(&runs));
        let n = events.len();
        let top = Event::Hold {
            clicks: 0,
            ticks: u16::MAX - HOLD_TICKS,
        };
        assert_eq!(
            (events[n - 2].1, events[n - 1].1),
            (top, Event::Release { clicks: 0 })
        );
    }

    #[test]
    fn hundreds_of_clicks_count_up_to_255() {
        let mut runs: Vec<_> = (0..300).flat_map(|_| [(true, 2), (false, 2)]).collect();
        // The last gap is long enough for the clicks to count.
        *runs.last_mut().unwrap() = (false, 30);
        assert_eq!(actual(&runs), model(&runs));
        assert_eq!(
            actual(&runs),
            [(299 * 4 + 2 + u32::from(RELEASE_TICKS), Event::Click(255))]
        );
    }
}
