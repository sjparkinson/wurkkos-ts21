//! A level that follows a target one step at a time, so a step-down fades
//! rather than jumps.
//!
//! It moves at most one level per tick, and you pick how many ticks each step
//! waits for, separately going down and going up. Thermal regulation uses it
//! for its ceiling (`src/thermal.rs`), and low-voltage protection does the
//! same with its own (`src/limits.rs`). UI changes (on, off, ramping)
//! shouldn't go through it: they want to be instant.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gradual {
    level: u8,
    waited: u8,
    down_every: u8,
    up_every: u8,
}

impl Gradual {
    /// Starts at `level`, stepping down once every `down_every` ticks and up
    /// once every `up_every`. A rate of `0` counts as `1`, one level a tick.
    pub const fn new(level: u8, down_every: u8, up_every: u8) -> Self {
        Self {
            level,
            waited: 0,
            down_every,
            up_every,
        }
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    /// One tick towards `target`. Returns the new level, which is never more
    /// than one away from the last.
    pub fn tick(&mut self, target: u8) -> u8 {
        if target == self.level {
            self.waited = 0;
            return self.level;
        }
        let every = if target < self.level {
            self.down_every
        } else {
            self.up_every
        };
        self.waited = self.waited.saturating_add(1);
        if self.waited >= every {
            self.waited = 0;
            if target < self.level {
                self.level -= 1;
            } else {
                self.level += 1;
            }
        }
        self.level
    }

    /// Drops the level to `max` straight away if it's above it. Only do this
    /// when nothing can see the difference, such as pulling a ceiling down to
    /// the level the light is already at.
    pub fn cap(&mut self, max: u8) {
        if self.level > max {
            self.level = max;
            self.waited = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(gradual: &mut Gradual, target: u8, ticks: u16) -> u8 {
        for _ in 0..ticks {
            gradual.tick(target);
        }
        gradual.level()
    }

    #[test]
    fn stays_put_at_the_target() {
        let mut gradual = Gradual::new(80, 1, 1);
        assert_eq!(run(&mut gradual, 80, 1000), 80);
    }

    #[test]
    fn moves_one_level_a_tick_at_full_speed() {
        let mut gradual = Gradual::new(150, 1, 1);
        for expected in (50..150).rev() {
            assert_eq!(gradual.tick(50), expected);
        }
        assert_eq!(run(&mut gradual, 50, 10), 50);
        for expected in 51..=150 {
            assert_eq!(gradual.tick(150), expected);
        }
    }

    #[test]
    fn a_rate_of_zero_is_one_level_a_tick() {
        let mut gradual = Gradual::new(10, 0, 0);
        assert_eq!(gradual.tick(0), 9);
        assert_eq!(gradual.tick(20), 10);
    }

    #[test]
    fn waits_its_turn_in_each_direction() {
        let mut gradual = Gradual::new(100, 2, 8);
        assert_eq!(run(&mut gradual, 0, 20), 90);
        assert_eq!(run(&mut gradual, 200, 80), 100);
    }

    #[test]
    fn never_moves_more_than_one_level_a_tick() {
        let mut gradual = Gradual::new(0, 1, 3);
        let mut seed = 1u32;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let target = (seed >> 16) as u8;
            let before = gradual.level();
            let after = gradual.tick(target);
            assert!(before.abs_diff(after) <= 1, "{before} → {after}");
            assert!(
                after.abs_diff(target) <= before.abs_diff(target),
                "{before} → {after} moving to {target}"
            );
        }
    }

    #[test]
    fn a_new_target_mid_step_doesnt_skip_the_wait() {
        let mut gradual = Gradual::new(100, 4, 4);
        run(&mut gradual, 0, 3);
        assert_eq!(gradual.level(), 100);
        assert_eq!(gradual.tick(50), 99);
    }

    #[test]
    fn cap_only_ever_lowers() {
        let mut gradual = Gradual::new(150, 1, 1);
        gradual.cap(60);
        assert_eq!(gradual.level(), 60);
        gradual.cap(100);
        assert_eq!(gradual.level(), 60);
    }
}
