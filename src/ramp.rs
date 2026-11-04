//! Brightness levels and the PWM duty cycles that produce them.
//!
//! The driver stacks two channels on the same LEDs: a 7135-style linear
//! regulator (about 350 mA, flat output, good for the low end) and a direct
//! drive FET (everything the cell can give). Levels `1..=LINEAR_MAX` ramp the
//! linear channel alone; above that it stays fully on and the FET ramps in on
//! top. Both channels share TCA0, so they share its period.
//!
//! The curve is cubic in each half, which looks roughly even to the eye.
//! It's generated here rather than copied from Anduril (GPL-3.0), and it's a
//! first pass: no dynamic PWM yet, so the bottom of the ramp is coarse.

/// The number of brightness levels, not counting off.
pub const LEVELS: u8 = 150;

/// The highest level that runs on the linear channel alone.
pub const LINEAR_MAX: u8 = 75;

/// Full duty, and TCA0's cycle in clocks. Single-slope PWM at 5 MHz with a
/// period of 254 runs at 5 MHz / 255 ≈ 19.6 kHz, well above anything you'd
/// hear or see, and a compare of 255 is over the period, so fully on.
pub const PWM_TOP: u16 = 255;

/// Duty cycles for one level, each out of [`PWM_TOP`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub linear: u8,
    pub fet: u8,
}

impl Output {
    pub const OFF: Self = Self { linear: 0, fet: 0 };
}

/// Duty cycles for `level`, where `0` is off and anything over [`LEVELS`]
/// clamps to the top.
pub fn output(level: u8) -> Output {
    match level {
        0 => Output::OFF,
        _ => TABLE[usize::from(level.min(LEVELS) - 1)],
    }
}

static TABLE: [Output; LEVELS as usize] = table();

const fn table() -> [Output; LEVELS as usize] {
    let mut table = [Output::OFF; LEVELS as usize];
    let mut i = 0;
    while i < LEVELS as usize {
        let level = i as u8 + 1;
        table[i] = if level <= LINEAR_MAX {
            Output {
                linear: cubic(level, LINEAR_MAX),
                fet: 0,
            }
        } else {
            Output {
                linear: 255,
                fet: cubic(level - LINEAR_MAX, LEVELS - LINEAR_MAX),
            }
        };
        i += 1;
    }
    table
}

/// Maps step `1..=steps` onto `1..=255` along x³.
const fn cubic(step: u8, steps: u8) -> u8 {
    let x = (step - 1) as u32;
    let n = (steps - 1) as u32;
    (1 + 254 * x * x * x / (n * n * n)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_off() {
        assert_eq!(output(0), Output::OFF);
    }

    #[test]
    fn the_lowest_level_still_lights() {
        assert_eq!(output(1), Output { linear: 1, fet: 0 });
    }

    #[test]
    fn the_top_is_everything() {
        assert_eq!(
            output(LEVELS),
            Output {
                linear: 255,
                fet: 255
            }
        );
        assert_eq!(output(u8::MAX), output(LEVELS));
    }

    #[test]
    fn the_fet_joins_above_the_linear_channel() {
        assert_eq!(
            output(LINEAR_MAX),
            Output {
                linear: 255,
                fet: 0
            }
        );
        assert_eq!(
            output(LINEAR_MAX + 1),
            Output {
                linear: 255,
                fet: 1
            }
        );
    }

    #[test]
    fn never_gets_dimmer_going_up() {
        for level in 1..LEVELS {
            let (a, b) = (output(level), output(level + 1));
            assert!(
                (b.linear, b.fet) >= (a.linear, a.fet),
                "level {level} → {}: {a:?} → {b:?}",
                level + 1
            );
        }
    }
}
