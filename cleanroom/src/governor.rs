//! One controller for both things that limit the light. Heat and a flat cell are
//! the same problem seen through a sensor: a raw reading that gets bigger as
//! things get worse (the temperature rises, or the supply falls against a fixed
//! reference). So a governor holds a ceiling on the level, pushes it down in
//! proportion to how far past `trip` the reading is, and lets it back up in
//! proportion to how far under `clear` it is. The UI never hears about any of this.
//!
//! Both directions have to be proportional, right down to fractions of a level.
//! The body takes minutes to answer, so a fixed step in either direction hunts:
//! by the time the reading says stop, the ceiling has already gone too far.

use crate::ramp::TOP;

/// Credit for one level. Each update earns credit towards a level, in steps of
/// `RISE_CREDIT` going up and `FALL_CREDIT` going down, plus another step for each
/// 2^shift counts beyond the limit. At four updates a second, a reading just past
/// a limit moves the ceiling a level every 16 s going up and every second going down.
const LEVEL_COST: u16 = 64;
const RISE_CREDIT: u16 = 1;
const FALL_CREDIT: u16 = 16;
const MOST_STEPS: u16 = 63;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub trip: u16,
    pub clear: u16,
    /// How many counts past a limit add another step of credit, as a power of two.
    pub shift: u8,
    /// The ceiling stops here.
    pub floor: u8,
}

#[derive(Debug)]
pub struct Governor {
    ceiling: u8,
    rise: u16,
    fall: u16,
}

impl Default for Governor {
    fn default() -> Self {
        Self {
            ceiling: TOP,
            rise: 0,
            fall: 0,
        }
    }
}

fn steps(counts: u16, shift: u8) -> u16 {
    (counts >> shift).min(MOST_STEPS) + 1
}

impl Governor {
    pub fn ceiling(&self) -> u8 {
        self.ceiling
    }

    pub fn update(&mut self, reading: u16, limits: &Limits) {
        if reading > limits.trip {
            self.rise = 0;
            self.fall += steps(reading - limits.trip, limits.shift) * FALL_CREDIT;
            let levels = (self.fall / LEVEL_COST) as u8;
            self.fall %= LEVEL_COST;
            self.ceiling = self.ceiling.saturating_sub(levels).max(limits.floor);
        } else if reading < limits.clear {
            self.fall = 0;
            self.rise += steps(limits.clear - reading, limits.shift) * RISE_CREDIT;
            let levels = (self.rise / LEVEL_COST) as u8;
            self.rise %= LEVEL_COST;
            self.ceiling = (self.ceiling + levels).min(TOP);
        }
    }

    /// The first reading after a sleep. Nothing counted time while the chip
    /// slept, so if things have recovered there's no reason to creep back.
    pub fn update_after_sleep(&mut self, reading: u16, limits: &Limits) {
        if reading < limits.clear {
            self.ceiling = TOP;
        }
        self.update(reading, limits);
    }
}

/// A one-pole filter: each reading moves the output 1/16 of the way to it.
#[derive(Debug, Default)]
pub struct Smoothed {
    sum: u16,
    primed: bool,
}

/// The ADC accumulates four 10-bit results, and 16 × this still fits a `u16`.
const MAX_READING: u16 = 4092;

impl Smoothed {
    pub fn push(&mut self, raw: u16) {
        let raw = raw.min(MAX_READING);
        if self.primed {
            self.sum = self.sum - (self.sum >> 4) + raw;
        } else {
            self.sum = raw << 4;
            self.primed = true;
        }
    }

    pub fn get(&self) -> Option<u16> {
        self.primed.then_some(self.sum >> 4)
    }

    pub fn forget(&mut self) {
        self.primed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        trip: 1000,
        clear: 900,
        shift: 3,
        floor: 10,
    };

    #[test]
    fn inside_the_deadband_nothing_moves() {
        let mut governor = Governor {
            ceiling: 50,
            ..Governor::default()
        };
        for reading in [900, 950, 1000] {
            governor.update(reading, &LIMITS);
        }
        assert_eq!(governor.ceiling(), 50);
    }

    #[test]
    fn the_step_down_grows_with_the_excess() {
        let fall = |reading, updates| {
            let mut governor = Governor::default();
            for _ in 0..updates {
                governor.update(reading, &LIMITS);
            }
            TOP - governor.ceiling()
        };
        assert_eq!(fall(1001, 3), 0, "a slight excess takes a second a level");
        assert_eq!(fall(1001, 4), 1);
        assert_eq!(fall(1080, 1), 2);
    }

    #[test]
    fn the_ceiling_stops_at_the_floor_however_bad_it_gets() {
        let mut governor = Governor::default();
        for _ in 0..200 {
            governor.update(u16::MAX, &LIMITS);
        }
        assert_eq!(governor.ceiling(), LIMITS.floor);
    }

    #[test]
    fn recovery_is_slow_and_stops_at_the_top() {
        let mut governor = Governor {
            ceiling: 50,
            ..Governor::default()
        };
        for _ in 0..10 {
            governor.update(800, &LIMITS);
        }
        assert_eq!(governor.ceiling(), 52);
        for _ in 0..1000 {
            governor.update(800, &LIMITS);
        }
        assert_eq!(governor.ceiling(), TOP);
    }

    #[test]
    fn recovery_is_slower_the_closer_the_reading_is_to_clear() {
        let climb = |reading| {
            let mut governor = Governor {
                ceiling: 50,
                ..Governor::default()
            };
            for _ in 0..64 {
                governor.update(reading, &LIMITS);
            }
            governor.ceiling() - 50
        };
        assert_eq!((climb(899), climb(800), climb(0)), (1, 13, 64));
    }

    #[test]
    fn a_trip_restarts_the_wait_to_recover() {
        let mut governor = Governor {
            ceiling: 50,
            rise: 60,
            fall: 0,
        };
        governor.update(1001, &LIMITS);
        governor.update(800, &LIMITS);
        assert_eq!(governor.ceiling(), 50);
    }

    #[test]
    fn sleep_clears_the_ceiling_only_if_the_reading_has_recovered() {
        let mut cooled = Governor {
            ceiling: 50,
            ..Governor::default()
        };
        cooled.update_after_sleep(800, &LIMITS);
        assert_eq!(cooled.ceiling(), TOP);
        let mut hot = Governor {
            ceiling: 50,
            ..Governor::default()
        };
        hot.update_after_sleep(950, &LIMITS);
        assert_eq!(hot.ceiling(), 50);
    }

    #[test]
    fn the_filter_starts_at_the_first_reading_and_converges() {
        let mut filter = Smoothed::default();
        assert_eq!(filter.get(), None);
        filter.push(1000);
        assert_eq!(filter.get(), Some(1000));
        for _ in 0..200 {
            filter.push(2000);
        }
        assert!(filter.get().unwrap().abs_diff(2000) <= 16);
    }

    #[test]
    fn the_filter_cannot_overflow() {
        let mut filter = Smoothed::default();
        for _ in 0..1000 {
            filter.push(u16::MAX);
        }
        assert_eq!(filter.get(), Some(MAX_READING));
    }
}
