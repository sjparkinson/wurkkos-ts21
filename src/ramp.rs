//! Brightness levels and the PWM duty cycles that produce them.
//!
//! The driver stacks two channels on the same LEDs: a 7135-style linear
//! regulator (about 350 mA, flat output, good for the low end) and a direct
//! drive FET (everything the cell can give). Levels `1..=LINEAR_MAX` ramp the
//! linear channel alone; above that it stays fully on and the FET ramps in on
//! top. Both channels share TCA0, so they share its period.
//!
//! The curve is cubic in each half, which looks roughly even to the eye.
//! It's generated here rather than copied from Anduril (GPL-3.0).
//!
//! The linear half uses dynamic PWM: each level has its own period as well as
//! its own compare. The duty is `compare / top`, so a longer period gets under
//! the 1/255 that a fixed one stops at, and picking the period to suit the
//! compare hits the curve to within a fraction of a percent, even when the
//! compare's only 1 or 2. The FET half stays at [`PWM_TOP`], since the linear
//! channel shares its period and that's where the current is.

use crate::regs;

/// The number of brightness levels, not counting off.
pub const LEVELS: u8 = 150;

/// The highest level that runs on the linear channel alone.
pub const LINEAR_MAX: u8 = 75;

/// The shortest cycle, in clocks, so the fastest PWM: single-slope at 5 MHz
/// with a period of 254 runs at 5 MHz / 255 ≈ 19.6 kHz. It's the cycle from
/// level [`LINEAR_MAX`] up, and while off. What the cycle is at any other
/// level is `output(level).top`.
pub const PWM_TOP: u16 = 255;

/// The longest cycle, in clocks: 5 MHz / 4095 ≈ 1.22 kHz, at level 1, a period
/// of 819 µs. docs/hardware.md has why it stops there.
pub const MAX_TOP: u16 = 4095;

/// The shortest pulse the linear channel gets, in clocks: 0.8 µs. A 7135 takes
/// about a microsecond to turn on, so one clock (200 ns) might not light it at
/// all. Likely, from forum measurements; docs/hardware.md has them. With
/// [`MAX_TOP`] this sets level 1, at 4 / 4095 of full. It's the number to
/// change if a real light shows level 1 is brighter or darker than it needs
/// to be.
pub const MIN_PULSE: u16 = 4;

/// Clocks in one ADC conversion, from the setup in `hw::init`: 13 ADC clocks
/// plus SAMPLEN (DS40002204A §30.3.2.3), at the prescaler's clocks each.
const CONVERSION_CLOCKS: u16 = (13 + regs::ADC_SAMPLEN_32US as u16) * (2 << regs::ADC_PRESC_DIV8);

/// TCA0's settings for one level. A pin's duty is its compare over `top`, so 0
/// holds it low (`CMPn` = BOTTOM) and `top` holds it high, since `top` is one
/// over PER (`CMPn` > TOP, DS40002204A §20.3.3.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub linear: u16,
    pub fet: u16,
    /// The cycle in clocks, which is PER + 1.
    pub top: u16,
}

impl Output {
    pub const OFF: Self = Self {
        linear: 0,
        fet: 0,
        top: PWM_TOP,
    };
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
            dynamic(level)
        } else {
            Output {
                linear: PWM_TOP,
                fet: cubic(level - LINEAR_MAX, LEVELS - LINEAR_MAX) as u16,
                top: PWM_TOP,
            }
        };
        i += 1;
    }
    table
}

/// Level `1..=LINEAR_MAX` on the linear channel. The duty runs along x³ from
/// [`MIN_PULSE`] / [`MAX_TOP`] at level 1 to fully on at [`LINEAR_MAX`]. It
/// takes the smallest compare that's at least [`MIN_PULSE`] and leaves the
/// cycle at [`PWM_TOP`] or more, which is the fastest PWM that can reach this
/// duty, then the cycle nearest to the duty for that compare. So the bottom
/// levels are one length of pulse coming more and more often, and their
/// brightness follows the frequency whatever the regulator does to the pulse.
const fn dynamic(level: u8) -> Output {
    let x = (level - 1) as u64;
    let n = (LINEAR_MAX - 1) as u64;
    let max = MAX_TOP as u64;
    let min = MIN_PULSE as u64;
    // The duty is `num / den`.
    let num = min * n * n * n + (max - min) * x * x * x;
    let den = max * n * n * n;
    let fastest = (PWM_TOP as u64 * num).div_ceil(den);
    let compare = if fastest < min { min } else { fastest };
    let top = (2 * compare * den + num) / (2 * num);
    Output {
        linear: compare as u16,
        fet: 0,
        // A conversion as long as the cycle would sample one phase every
        // time, so step off it. That's 0.4% of the duty.
        top: if top as u16 == CONVERSION_CLOCKS {
            top as u16 + 1
        } else {
            top as u16
        },
    }
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
        assert_eq!(
            output(1),
            Output {
                linear: MIN_PULSE,
                fet: 0,
                top: MAX_TOP
            }
        );
    }

    #[test]
    fn the_top_is_everything() {
        assert_eq!(
            output(LEVELS),
            Output {
                linear: PWM_TOP,
                fet: PWM_TOP,
                top: PWM_TOP
            }
        );
        assert_eq!(output(u8::MAX), output(LEVELS));
    }

    #[test]
    fn the_fet_joins_above_the_linear_channel() {
        assert_eq!(
            output(LINEAR_MAX),
            Output {
                linear: PWM_TOP,
                fet: 0,
                top: PWM_TOP
            }
        );
        assert_eq!(
            output(LINEAR_MAX + 1),
            Output {
                linear: PWM_TOP,
                fet: 1,
                top: PWM_TOP
            }
        );
    }

    /// `a / a_top` against `b / b_top`, without fractions.
    fn duty(a: u16, a_top: u16, b: u16, b_top: u16) -> (u32, u32) {
        (
            u32::from(a) * u32::from(b_top),
            u32::from(b) * u32::from(a_top),
        )
    }

    #[test]
    fn never_gets_dimmer_going_up() {
        for level in 1..LEVELS {
            let (a, b) = (output(level), output(level + 1));
            let (linear_a, linear_b) = duty(a.linear, a.top, b.linear, b.top);
            let (fet_a, fet_b) = duty(a.fet, a.top, b.fet, b.top);
            assert!(
                linear_b >= linear_a && fet_b >= fet_a,
                "level {level} → {}: {a:?} → {b:?}",
                level + 1
            );
        }
    }

    #[test]
    fn every_step_of_the_linear_channel_gets_brighter() {
        for level in 1..LINEAR_MAX {
            let (a, b) = (output(level), output(level + 1));
            let (a_duty, b_duty) = duty(a.linear, a.top, b.linear, b.top);
            assert!(b_duty > a_duty, "level {level}: {a:?} → {b:?}");
        }
    }

    #[test]
    fn full_on_is_exactly_the_top_and_nothing_else_is() {
        for level in 1..=LEVELS {
            let out = output(level);
            assert_eq!(
                out.linear == out.top,
                level >= LINEAR_MAX,
                "level {level}: {out:?}"
            );
            assert_eq!(
                out.fet == out.top,
                level == LEVELS,
                "level {level}: {out:?}"
            );
            assert!(out.linear <= out.top && out.fet <= out.top, "{out:?}");
        }
    }

    #[test]
    fn off_is_really_off_and_only_off_is() {
        assert_eq!(output(0), Output::OFF);
        assert_eq!((Output::OFF.linear, Output::OFF.fet), (0, 0));
        for level in 1..=LEVELS {
            assert_ne!(output(level).linear, 0, "level {level}");
        }
    }

    #[test]
    fn the_fet_stays_out_of_the_linear_half() {
        for level in 1..=LINEAR_MAX {
            assert_eq!(output(level).fet, 0, "level {level}");
        }
        for level in LINEAR_MAX + 1..=LEVELS {
            assert_ne!(output(level).fet, 0, "level {level}");
        }
    }

    #[test]
    fn the_period_stays_in_range_and_fast_from_the_top_of_the_linear_channel() {
        for level in 1..=LEVELS {
            let top = output(level).top;
            assert!((PWM_TOP..=MAX_TOP).contains(&top), "level {level}: {top}");
        }
        for level in LINEAR_MAX..=LEVELS {
            assert_eq!(output(level).top, PWM_TOP, "level {level}");
        }
    }

    #[test]
    fn only_the_bottom_few_levels_run_under_10_khz() {
        // 5 MHz / 500 is 10 kHz, and level 16 is under 1% duty.
        for level in 16..=LEVELS {
            let top = output(level).top;
            assert!(top <= 500, "level {level}: {top}");
        }
    }

    #[test]
    fn a_burst_never_samples_one_phase_over_and_over() {
        // Four conversions start three gaps apart, so no gap can be a whole
        // number of cycles.
        assert_eq!(CONVERSION_CLOCKS, 264);
        for level in 1..=LEVELS {
            let top = output(level).top;
            for gaps in 1..=3 {
                assert_ne!(gaps * CONVERSION_CLOCKS % top, 0, "level {level}: {top}");
            }
        }
    }

    #[test]
    fn the_period_outlasts_a_burst_only_on_the_shortest_pulse() {
        // Past this the four samples can all miss the pulse, or some catch
        // it, so the reading depends on the phase. docs/hardware.md has why
        // that's fine: the pulse is 0.8 µs against a 35 µs sample.
        for level in 1..=LEVELS {
            let out = output(level);
            if out.top > 4 * CONVERSION_CLOCKS {
                assert_eq!(out.linear, MIN_PULSE, "level {level}: {out:?}");
            }
        }
    }

    #[test]
    fn no_pulse_on_the_linear_channel_is_under_the_minimum() {
        for level in 1..=LEVELS {
            let linear = output(level).linear;
            assert!(linear >= MIN_PULSE, "level {level}: {linear}");
        }
    }
}
