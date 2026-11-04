//! Turns one button sample per tick into gestures. The 64 Hz sample is the
//! debounce: a press counts on the first sample, so it feels instant, and a
//! release needs two, which outlasts the bounce.
//!
//! The decoder reports every edge as it happens rather than waiting to see how
//! many clicks there'll be. That's what lets the UI answer at once.

/// 0.375 s.
pub const HOLD_TICKS: u8 = 24;
/// 0.31 s between clicks before a burst is over.
pub const GAP_TICKS: u8 = 20;
const RELEASE_SAMPLES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// The nth press of a burst went down.
    Press(u8),
    /// The nth press came up before it became a hold.
    Release(u8),
    /// The nth press has been down long enough to be a hold.
    HoldStart(u8),
    /// Once a tick while a hold lasts.
    Holding,
    /// A hold came up, which also ends the burst.
    HoldEnd,
    /// Nothing happened for `GAP_TICKS` after n clicks.
    BurstEnd(u8),
}

#[derive(Debug, Default)]
pub struct Decoder {
    down: bool,
    holding: bool,
    count: u8,
    held: u8,
    up_samples: u8,
    gap: u8,
}

impl Decoder {
    pub fn step(&mut self, pressed: bool) -> Option<Gesture> {
        if self.down {
            self.step_down(pressed)
        } else if pressed {
            self.down = true;
            self.held = 0;
            self.up_samples = 0;
            self.count = self.count.saturating_add(1);
            Some(Gesture::Press(self.count))
        } else if self.count > 0 {
            self.gap += 1;
            (self.gap >= GAP_TICKS).then(|| {
                let clicks = self.count;
                self.count = 0;
                Gesture::BurstEnd(clicks)
            })
        } else {
            None
        }
    }

    fn step_down(&mut self, pressed: bool) -> Option<Gesture> {
        if !pressed {
            self.up_samples += 1;
            if self.up_samples < RELEASE_SAMPLES {
                return self.holding.then_some(Gesture::Holding);
            }
            self.down = false;
            self.gap = 0;
            if self.holding {
                self.holding = false;
                self.count = 0;
                return Some(Gesture::HoldEnd);
            }
            return Some(Gesture::Release(self.count));
        }
        self.up_samples = 0;
        if self.holding {
            return Some(Gesture::Holding);
        }
        self.held += 1;
        if self.held >= HOLD_TICKS {
            self.holding = true;
            return Some(Gesture::HoldStart(self.count));
        }
        None
    }

    pub fn is_down(&self) -> bool {
        self.down
    }

    /// No press in progress and no burst waiting to finish.
    pub fn is_idle(&self) -> bool {
        !self.down && self.count == 0
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::Gesture::*;
    use super::*;
    use std::vec::Vec;

    /// Feeds (pressed, ticks) spans and collects the gestures, minus `Holding`.
    fn run(spans: &[(bool, u8)]) -> Vec<Gesture> {
        let mut decoder = Decoder::default();
        let mut seen = Vec::new();
        for &(pressed, ticks) in spans {
            for _ in 0..ticks {
                seen.extend(decoder.step(pressed).filter(|g| *g != Holding));
            }
        }
        seen
    }

    #[test]
    fn a_click_reports_both_edges_then_the_end_of_the_burst() {
        let seen = run(&[(true, 5), (false, 30)]);
        assert_eq!(seen, [Press(1), Release(1), BurstEnd(1)]);
    }

    #[test]
    fn clicks_inside_the_gap_count_up() {
        let seen = run(&[
            (true, 4),
            (false, 8),
            (true, 4),
            (false, 8),
            (true, 4),
            (false, 30),
        ]);
        let want = [
            Press(1),
            Release(1),
            Press(2),
            Release(2),
            Press(3),
            Release(3),
            BurstEnd(3),
        ];
        assert_eq!(seen, want);
    }

    #[test]
    fn clicks_outside_the_gap_are_separate_bursts() {
        let seen = run(&[(true, 4), (false, 30), (true, 4), (false, 30)]);
        let want = [
            Press(1),
            Release(1),
            BurstEnd(1),
            Press(1),
            Release(1),
            BurstEnd(1),
        ];
        assert_eq!(seen, want);
    }

    #[test]
    fn a_hold_ends_the_burst_with_no_burst_end() {
        let seen = run(&[(true, 40), (false, 40)]);
        assert_eq!(seen, [Press(1), HoldStart(1), HoldEnd]);
    }

    #[test]
    fn a_click_then_a_hold_is_a_second_hold() {
        let seen = run(&[(true, 4), (false, 8), (true, 40), (false, 4)]);
        assert_eq!(
            seen,
            [Press(1), Release(1), Press(2), HoldStart(2), HoldEnd]
        );
    }

    #[test]
    fn a_single_sample_of_bounce_on_release_is_ignored() {
        let seen = run(&[(true, 4), (false, 1), (true, 1), (false, 30)]);
        assert_eq!(seen, [Press(1), Release(1), BurstEnd(1)]);
    }

    #[test]
    fn the_press_is_reported_on_its_first_sample() {
        let mut decoder = Decoder::default();
        assert_eq!(decoder.step(true), Some(Press(1)));
    }

    #[test]
    fn idle_only_once_the_burst_is_over() {
        let mut decoder = Decoder::default();
        assert!(decoder.is_idle());
        decoder.step(true);
        assert!(!decoder.is_idle());
        for _ in 0..3 {
            decoder.step(false);
        }
        assert!(!decoder.is_idle());
        for _ in 0..GAP_TICKS {
            decoder.step(false);
        }
        assert!(decoder.is_idle());
    }
}
