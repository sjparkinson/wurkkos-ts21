//! Random switching for tests, kept out of the library because none of it
//! goes onto the torch. `sim.rs` uses it to check the firmware against the
//! library, and the tests below use it to check `ts21::button::Clicks`
//! against a second description of its rules.

use ts21::button::{HOLD_TICKS, RELEASE_TICKS};

/// Endless random switching, as (pressed, ticks) spans that alternate,
/// starting released. Most lengths sit right at the edges `Clicks` cares
/// about.
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

/// src/button.rs's own tests pick their timings by hand. These compare
/// `Clicks` against a second description of the same rules, written in
/// terms of whole presses and gaps rather than one tick at a time, over
/// thousands of random switchings and a few very long ones.
#[cfg(test)]
mod tests {
    use super::*;
    use ts21::button::{Clicks, Event};

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
        let samples = runs
            .iter()
            .flat_map(|&(pressed, len)| std::iter::repeat_n(pressed, len as usize));
        (0..)
            .zip(samples)
            .filter_map(|(t, pressed)| clicks.tick(pressed).map(|event| (t, event)))
            .collect()
    }

    #[test]
    fn clicks_matches_the_model_on_random_switching() {
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
        // Longer than `Clicks` can count to, so its count has saturated.
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
        let top = Event::Hold {
            clicks: 0,
            ticks: u16::MAX - HOLD_TICKS,
        };
        let last: Vec<_> = events.iter().rev().take(2).map(|&(_, e)| e).collect();
        assert_eq!(last, [Event::Release { clicks: 0 }, top]);
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
