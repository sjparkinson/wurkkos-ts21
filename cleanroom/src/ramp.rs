//! Brightness levels. A level is a step on a geometric scale of lumens, so each
//! one looks the same amount brighter than the last, and the table is worked out
//! from that at compile time rather than tuned by hand.
//!
//! Below the linear channel's limit the PWM period stretches, buying duty
//! resolution at the dim end, where the eye is fussiest and a slow PWM can't be
//! seen anyway. Above it the linear channel stays full and the FET adds the rest.

pub const OFF: u8 = 0;
pub const FLOOR: u8 = 1;
pub const TOP: u8 = 127;

/// Timer settings for one level. `linear` and `fet` are compare values: 0 holds
/// the pin low and `period + 1` holds it high.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pwm {
    pub period: u16,
    pub linear: u16,
    pub fet: u16,
}

const FLOOR_MLM: u64 = 200;
const LINEAR_MLM: u64 = 126_000;
const FULL_MLM: u64 = 3_000_000;
/// Per-level brightness ratio, in ten-thousandths.
const RATIO: u64 = 10_792;
/// Shortest period: 5 MHz / 255 is 19.6 kHz, above hearing.
const MIN_COUNTS: u64 = 255;
/// Longest period: 5 MHz / 4096 is 1.2 kHz, too fast to see.
const MAX_COUNTS: u64 = 4096;
/// Stretch the period until the on-time is at least this many counts.
const WANTED_ON_COUNTS: u64 = 32;

const fn pwm_for(mlm: u64) -> Pwm {
    if mlm <= LINEAR_MLM {
        let mut counts = WANTED_ON_COUNTS * LINEAR_MLM / mlm;
        if counts < MIN_COUNTS {
            counts = MIN_COUNTS;
        }
        if counts > MAX_COUNTS {
            counts = MAX_COUNTS;
        }
        let on = (mlm * counts + LINEAR_MLM / 2) / LINEAR_MLM;
        Pwm {
            period: (counts - 1) as u16,
            linear: on as u16,
            fet: 0,
        }
    } else {
        let fet = (mlm - LINEAR_MLM) * MIN_COUNTS / (FULL_MLM - LINEAR_MLM);
        Pwm {
            period: (MIN_COUNTS - 1) as u16,
            linear: MIN_COUNTS as u16,
            fet: fet as u16,
        }
    }
}

const fn build() -> [Pwm; TOP as usize + 1] {
    let mut table = [Pwm {
        period: (MIN_COUNTS - 1) as u16,
        linear: 0,
        fet: 0,
    }; TOP as usize + 1];
    let mut mlm = FLOOR_MLM;
    let mut level = FLOOR as usize;
    while level < TOP as usize {
        table[level] = pwm_for(mlm);
        mlm = mlm * RATIO / 10_000;
        level += 1;
    }
    table[TOP as usize] = pwm_for(FULL_MLM);
    table
}

pub static TABLE: [Pwm; TOP as usize + 1] = build();

/// The brightest level with the FET fully off, which is as far as heat can
/// reasonably ask the light to come down.
pub const LINEAR_TOP: u8 = {
    let table = build();
    let mut level = TOP as usize;
    while table[level].fet != 0 {
        level -= 1;
    }
    level as u8
};

pub fn pwm(level: u8) -> Pwm {
    TABLE[(level & TOP) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Duty as a fraction of full output, in millionths, on the lumen scale.
    fn output(pwm: Pwm) -> u64 {
        let counts = u64::from(pwm.period) + 1;
        let linear = u64::from(pwm.linear) * LINEAR_MLM / counts;
        let fet = u64::from(pwm.fet) * (FULL_MLM - LINEAR_MLM) / counts;
        linear + fet
    }

    #[test]
    fn off_is_dark_and_top_is_both_pins_held_high() {
        assert_eq!((pwm(OFF).linear, pwm(OFF).fet), (0, 0));
        let top = pwm(TOP);
        assert_eq!((top.linear, top.fet), (top.period + 1, top.period + 1));
    }

    #[test]
    fn every_level_is_lit_and_none_is_dimmer_than_the_last() {
        for level in FLOOR..TOP {
            let (this, next) = (output(pwm(level)), output(pwm(level + 1)));
            assert!(this > 0, "level {level} is dark");
            assert!(next >= this, "level {level} is brighter than the next");
        }
    }

    #[test]
    fn no_step_is_a_visible_jump() {
        for level in FLOOR..TOP - 1 {
            let (this, next) = (output(pwm(level)), output(pwm(level + 1)));
            assert!(
                next * 100 <= this * 125,
                "level {level} to {} jumps",
                level + 1
            );
        }
    }

    #[test]
    fn compares_never_pass_full_on_and_periods_stay_in_range() {
        for entry in &TABLE {
            assert!(entry.linear <= entry.period + 1 && entry.fet <= entry.period + 1);
            assert!((254..=4095).contains(&entry.period));
        }
    }

    #[test]
    fn the_fet_only_comes_in_once_the_linear_channel_is_full() {
        for entry in &TABLE {
            assert!(entry.fet == 0 || entry.linear == entry.period + 1);
        }
        const { assert!(LINEAR_TOP > 60 && LINEAR_TOP < 100) };
    }
}
