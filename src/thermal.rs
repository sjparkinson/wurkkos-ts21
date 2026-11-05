//! Thermal regulation: temperature readings in, a ceiling on the level out.
//!
//! Feed it a reading from `temp::sixteenths` every [`SAMPLE_TICKS`] ticks with
//! [`Thermal::sample`], call [`Thermal::tick`] every tick, and run the light
//! at whichever is lower, the UI's level or the ceiling. The ceiling moves
//! through a [`Gradual`], so a step-down is a fade of at most one level every
//! other tick, never a jump.
//!
//! The approach follows the one Anduril describes in `fsm/adc.c` and
//! `ui/anduril/ramp-mode.c` (facts only, it's GPL-3.0):
//!
//! - Smooth the readings, then predict where the temperature is heading from
//!   how far it's moved over the last eight samples, times a lookahead of
//!   four. Anduril uses the same history length and lookahead.
//! - If the prediction is over the limit and it isn't already cooling, step
//!   down. The error adds up over readings and each step spends some of it,
//!   so a little over steps down now and then and a lot over steps down
//!   every reading. Anduril's warning threshold works much the same way.
//! - If the prediction is more than 2 °C under the limit (Anduril's window)
//!   and it isn't warming up, step back up the same way, but slower and never
//!   more than a level a reading. In between, hold. That gap, the slow climb
//!   and the lookahead are what stop it hunting.
//! - Never step down below a floor, by default the top of the linear
//!   channel. The linear regulator tops out around 350 mA, which can't
//!   overheat the head, so that's the brightest level that's always safe.
//!   Anduril's build for this light (model 0614) floors at 60, with the
//!   linear channel full at 75 (`MIN_THERM_STEPDOWN` and `MAX_1x7135` in
//!   `hw/sofirn/blf-q8-t1616/anduril.h`), so it's on the linear channel too.
//!
//! Only sample while the light's on. Anduril leaves the temperature alone
//! in standby because the ADC costs about 250 µA against about 20 µA asleep
//! (`fsm/adc.c`), and the torch will sleep when off. Call [`Thermal::reset`]
//! when it turns on, so the readings and ceiling from last time don't carry
//! over. A light that turns on hot then steps down from the first readings.
//! - Ignore a reading that's impossible or far from the last one, unless a
//!   few in a row agree. If the sensor keeps reading impossible values, step
//!   down to the floor, since a broken sensor can't say it's cool.
//!
//! The numbers here are a first guess. Nobody's tuned them on a real light
//! yet, so expect them to change once one's been run up to temperature.

use crate::gradual::Gradual;
use crate::ramp::{LEVELS, LINEAR_MAX};

/// How many ticks apart to take readings: four a second.
pub const SAMPLE_TICKS: u16 = 16;

/// Kelvin, from whole degrees Celsius.
pub const fn celsius(degrees: u8) -> u16 {
    degrees as u16 + 273
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The temperature to hold the MCU at, in kelvin.
    pub limit: u16,
    /// The lowest level thermal regulation will step down to.
    pub floor: u8,
}

impl Config {
    /// 45 °C, Anduril's default, and a floor at the top of the linear
    /// channel.
    pub const DEFAULT: Self = Self {
        limit: celsius(45),
        floor: LINEAR_MAX,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Readings and the smoothed temperature are in sixteenths of a kelvin, which
/// is what `temp::sixteenths` hands over.
const FRACTION: i16 = 16;
/// Samples of history to measure the trend over.
const HISTORY: usize = 8;
/// How many trend-lengths ahead to predict.
const LOOKAHEAD: i16 = 4;
/// How far under the limit the prediction has to be before stepping up.
const WINDOW: i16 = 2 * FRACTION;
/// A trend at least this steep counts as warming up, and blocks stepping up.
const WARMING: i16 = FRACTION;
/// The steepest trend the prediction believes, 64 °C over the history's two
/// seconds.
const MAX_TREND: i16 = 64 * FRACTION;
/// How much accumulated error, in sixteenths of a kelvin over the limit
/// summed over readings, buys one level down.
const DOWN_PRESSURE: i16 = 4 * FRACTION;
/// The same for one level up, counted from the bottom of the window.
const UP_PRESSURE: i16 = 16 * FRACTION;
/// The most levels to step down per sample. With a sample every 16 ticks
/// and the ceiling moving a level every other tick, that's as fast as the
/// ceiling can follow.
const MAX_STEP_DOWN: u8 = 8;
/// Ticks per level as the ceiling fades down, and up.
const DOWN_EVERY: u8 = 2;
const UP_EVERY: u8 = 4;
/// Readings outside -40 °C to 150 °C can't be real. The part is rated to
/// 125 °C at most, so the top leaves some slack. In kelvin.
const PLAUSIBLE: core::ops::RangeInclusive<u16> = 233..=423;

/// Whether a reading in sixteenths of a kelvin could be real. Anything else
/// is a broken sensor or a bad ADC setup.
pub fn plausible(sixteenths: u16) -> bool {
    let unit = FRACTION as u16;
    (*PLAUSIBLE.start() * unit..=*PLAUSIBLE.end() * unit).contains(&sixteenths)
}
/// A reading further than this from the last good one, in sixteenths of a
/// kelvin, is a glitch, unless more than `GLITCHES` in a row agree, and then
/// it's believed.
const JUMP: u16 = 6 * FRACTION as u16;
const GLITCHES: u8 = 3;

const _: () = assert!(MAX_STEP_DOWN as u16 * DOWN_EVERY as u16 <= SAMPLE_TICKS);

#[derive(Clone, Debug)]
pub struct Thermal {
    config: Config,
    /// Where the ceiling is heading.
    target: u8,
    ceiling: Gradual,
    /// Smoothed temperature in sixteenths of a kelvin, or `None` before the
    /// first good reading.
    smooth: Option<i16>,
    /// The last reading that wasn't a glitch.
    last: u16,
    history: [i16; HISTORY],
    next: u8,
    /// Glitches in a row.
    rejected: u8,
    /// Error summed over readings: positive pushes the ceiling down,
    /// negative pushes it up.
    pressure: i16,
}

impl Default for Thermal {
    fn default() -> Self {
        Self::new(Config::DEFAULT)
    }
}

impl Thermal {
    pub const fn new(config: Config) -> Self {
        Self {
            config,
            target: LEVELS,
            ceiling: Gradual::new(LEVELS, DOWN_EVERY, UP_EVERY),
            smooth: None,
            last: 0,
            history: [0; HISTORY],
            next: 0,
            rejected: 0,
            pressure: 0,
        }
    }

    /// The ceiling right now.
    pub fn ceiling(&self) -> u8 {
        self.ceiling.level()
    }

    /// Forgets everything but the config: no ceiling, and the next reading
    /// starts the smoothing and the trend afresh. Call it when the light
    /// turns on, since nothing's sampled while it's off.
    pub fn reset(&mut self) {
        *self = Self::new(self.config);
    }

    /// One tick: moves the ceiling a step towards where it's heading and
    /// returns it.
    pub fn tick(&mut self) -> u8 {
        self.ceiling.tick(self.target)
    }

    /// One temperature reading in sixteenths of a kelvin, taken every
    /// [`SAMPLE_TICKS`] ticks while the light's on, and the level it's at
    /// right now.
    pub fn sample(&mut self, reading: u16, level: u8) {
        let smooth = match self.smooth {
            _ if !plausible(reading) => {
                self.rejected = self.rejected.saturating_add(1);
                if self.rejected > GLITCHES {
                    // The sensor's broken. Assume the worst.
                    self.step_down(level, MAX_STEP_DOWN);
                }
                return;
            }
            Some(smooth) if reading.abs_diff(self.last) <= JUMP => {
                self.rejected = 0;
                self.last = reading;
                // An eighth of the way there, but always at least a
                // sixteenth, so a steady reading is reached exactly.
                let difference = reading as i16 - smooth;
                let step = match difference / 8 {
                    0 => difference.signum(),
                    step => step,
                };
                smooth + step
            }
            Some(_) if self.rejected < GLITCHES => {
                self.rejected += 1;
                return;
            }
            // The first reading, or a jump that's lasted long enough to be
            // real.
            _ => self.start(reading),
        };
        self.smooth = Some(smooth);

        let oldest = core::mem::replace(&mut self.history[usize::from(self.next)], smooth);
        self.next = match self.next + 1 {
            next if usize::from(next) == HISTORY => 0,
            next => next,
        };
        let trend = (smooth - oldest).clamp(-MAX_TREND, MAX_TREND);
        let predicted = smooth + trend * LOOKAHEAD;

        let limit = self.config.limit.min(*PLAUSIBLE.end()) as i16 * FRACTION;
        let over = predicted - limit;
        if over > 0 && (trend >= 0 || smooth > limit + WINDOW) {
            // Each step down costs `DOWN_PRESSURE`, so the rate follows how
            // far over the prediction is: a kelvin over steps once a second,
            // four over steps every reading, and a sixteenth over takes
            // sixteen seconds.
            let pressure = self.pressure.max(0).saturating_add(over);
            let steps = (pressure / DOWN_PRESSURE).min(i16::from(MAX_STEP_DOWN));
            self.pressure = (pressure - steps * DOWN_PRESSURE).min(DOWN_PRESSURE - 1);
            if steps > 0 {
                self.step_down(level, steps as u8);
            }
        } else if over < -WINDOW && trend < WARMING {
            // The same going up, counted from the bottom of the window, but
            // at most a level a reading and for four times the pressure. The
            // head takes a minute or so to show what a step up did, so
            // coming back up too fast is what makes it hunt.
            let pressure = self.pressure.min(0).saturating_add(over + WINDOW);
            if pressure <= -UP_PRESSURE {
                self.pressure = (pressure + UP_PRESSURE).max(1 - UP_PRESSURE);
                self.target = (self.target + 1).min(LEVELS);
            } else {
                self.pressure = pressure;
            }
        } else if over <= 0 {
            // Inside the window: hold, and forget the push either way.
            self.pressure = 0;
        }
        // Over the limit but already cooling: hold, and keep the pressure
        // for if it turns.
    }

    /// Starts smoothing over from `reading`, as if it had always been there.
    /// Only for plausible readings, which keep it well inside an `i16`.
    fn start(&mut self, reading: u16) -> i16 {
        let smooth = reading as i16;
        self.history = [smooth; HISTORY];
        self.last = reading;
        self.rejected = 0;
        smooth
    }

    fn step_down(&mut self, level: u8, steps: u8) {
        let floor = self.config.floor;
        if level != 0 {
            // A ceiling above the light's level does nothing, so pull it
            // down to there first. Nothing visible changes, and the next
            // step down shows straight away.
            let level = level.max(floor);
            self.ceiling.cap(level);
            self.target = self.target.min(level);
        }
        self.target = self.target.saturating_sub(steps).max(floor);
    }
}

#[cfg(test)]
mod tests {
    // Only for `TRACE=1 cargo test -q thermal -- --nocapture`, which prints
    // what each closed-loop stretch did. Handy when retuning.
    extern crate std;
    use super::*;
    use crate::ramp::{self, LINEAR_MAX};

    const LIMIT: u16 = Config::DEFAULT.limit;
    const FLOOR: u8 = Config::DEFAULT.floor;

    /// The light at whatever level the UI asks for, capped by thermal
    /// regulation, checking every tick that the cap never moves the output
    /// more than one level.
    struct Run {
        thermal: Thermal,
        output: u8,
    }

    impl Run {
        fn new() -> Self {
            Self {
                thermal: Thermal::default(),
                output: 0,
            }
        }

        /// One reading in whole kelvin and the ticks up to the next. Returns
        /// the output.
        fn sample(&mut self, kelvin: u16, ui: u8) -> u8 {
            self.sample_fraction(fraction(kelvin), ui)
        }

        /// The same for a reading in sixteenths of a kelvin.
        fn sample_fraction(&mut self, reading: u16, ui: u8) -> u8 {
            self.thermal.sample(reading, self.output);
            for _ in 0..SAMPLE_TICKS {
                self.tick(ui);
            }
            self.output
        }

        fn tick(&mut self, ui: u8) {
            let output = ui.min(self.thermal.tick());
            // Turning on is the UI's jump to make. Everything else is the
            // ceiling's doing, and that can't jump.
            if self.output != 0 {
                assert!(
                    output.abs_diff(self.output) <= 1,
                    "jumped {} → {output}",
                    self.output
                );
            }
            self.output = output;
        }

        fn hold(&mut self, kelvin: u16, ui: u8, samples: u16) -> u8 {
            for _ in 0..samples {
                self.sample(kelvin, ui);
            }
            self.output
        }
    }

    /// A made-up pseudo-random sequence, so the tests can add noise.
    struct Noise(u32);

    impl Noise {
        /// Something in `-spread..=spread`.
        fn next(&mut self, spread: i16) -> i16 {
            self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((self.0 >> 16) % (2 * spread as u32 + 1)) as i16 - spread
        }
    }

    fn noisy(kelvin: u16, noise: &mut Noise, spread: i16) -> u16 {
        kelvin.saturating_add_signed(noise.next(spread))
    }

    /// A whole number of kelvin as a reading, keeping junk well out of range.
    fn fraction(kelvin: u16) -> u16 {
        kelvin.saturating_mul(FRACTION as u16)
    }

    /// A rough head: heat in from the LEDs, out to the air, and the MCU a
    /// few seconds behind the head. Nothing about it is measured; it's
    /// there to give the controller something with lag to fight against.
    /// Temperatures are in thousandths of a kelvin.
    struct Head {
        head: i32,
        mcu: i32,
        ambient: i32,
    }

    impl Head {
        /// About how far over ambient turbo would take the head if nothing
        /// stopped it.
        const TURBO_RISE: i32 = 80_000;

        fn new(ambient: u16) -> Self {
            let ambient = i32::from(ambient) * 1000;
            Self {
                head: ambient,
                mcu: ambient,
                ambient,
            }
        }

        /// Roughly the power at `level`: the linear channel's 350 mA is
        /// about a twentieth of what the FET lets through.
        fn power(level: u8) -> i32 {
            let output = ramp::output(level);
            i32::from(output.linear) + 20 * i32::from(output.fet)
        }

        /// One tick at `level`. The head settles with a time constant of a
        /// minute, and the MCU follows it over five seconds.
        fn tick(&mut self, level: u8) {
            let settle = self.ambient + Self::TURBO_RISE * Self::power(level) / Self::power(LEVELS);
            self.head += (settle - self.head) / (60 * 64);
            self.mcu += (self.head - self.mcu) / (5 * 64);
        }

        fn kelvin(&self) -> u16 {
            ((self.mcu + 500) / 1000) as u16
        }

        /// What the sensor reads, to the sixteenth.
        fn reading(&self) -> u16 {
            ((self.mcu * i32::from(FRACTION) + 500) / 1000) as u16
        }
    }

    /// What a stretch of [`closed_loop`] did.
    #[derive(Debug)]
    struct Stretch {
        /// The hottest the MCU got, in kelvin.
        hottest: u16,
        /// The output's range.
        lowest: u8,
        highest: u8,
        /// How often the output changed direction.
        turns: u16,
    }

    /// Runs the light at `ui` against `head` for `samples` readings, each
    /// off by up to `spread` kelvin.
    fn closed_loop(
        run: &mut Run,
        head: &mut Head,
        ui: u8,
        samples: u16,
        noise: &mut Noise,
        spread: i16,
    ) -> Stretch {
        let mut stretch = Stretch {
            hottest: 0,
            lowest: u8::MAX,
            highest: 0,
            turns: 0,
        };
        let (mut last, mut falling) = (run.output, None);
        for _ in 0..samples {
            let off = noise.next(spread) * FRACTION;
            let reading = head.reading().saturating_add_signed(off);
            run.thermal.sample(reading, run.output);
            for _ in 0..SAMPLE_TICKS {
                run.tick(ui);
                head.tick(run.output);
            }
            stretch.hottest = stretch.hottest.max(head.kelvin());
            stretch.lowest = stretch.lowest.min(run.output);
            stretch.highest = stretch.highest.max(run.output);
            if run.output != last {
                let now = run.output < last;
                if falling.is_some_and(|was| was != now) {
                    stretch.turns += 1;
                }
                falling = Some(now);
                last = run.output;
            }
        }
        if std::env::var_os("TRACE").is_some() {
            std::println!("{stretch:?}, ending at {}", run.output);
        }
        stretch
    }

    #[test]
    fn starts_with_no_ceiling() {
        assert_eq!(Thermal::default().ceiling(), LEVELS);
        assert_eq!(Config::DEFAULT.limit, 318);
    }

    #[test]
    fn leaves_it_alone_below_the_limit() {
        let mut run = Run::new();
        for kelvin in [celsius(20), celsius(35), LIMIT - 3] {
            assert_eq!(run.hold(kelvin, LEVELS, 4 * 60 * 10), LEVELS, "{kelvin} K");
            assert_eq!(run.thermal.ceiling(), LEVELS);
        }
    }

    #[test]
    fn steps_down_smoothly_as_it_heats_up() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        let mut last = LEVELS;
        // Warming a degree every two seconds, to 15 °C over the limit.
        for step in 0..8 * 30 {
            let output = run.sample(celsius(30) + step / 8, LEVELS);
            assert!(output <= last, "stepped up while heating");
            last = output;
        }
        assert!(last < LEVELS, "never stepped down");
        assert!(last >= FLOOR);
    }

    #[test]
    fn starts_stepping_down_before_it_reaches_the_limit() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        let mut kelvin = celsius(30);
        while run.output == LEVELS {
            kelvin += 1;
            run.hold(kelvin, LEVELS, 4);
        }
        assert!(kelvin < LIMIT, "waited until {kelvin} K");
    }

    #[test]
    fn sustained_heat_takes_it_to_the_floor_and_no_further() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        assert_eq!(run.hold(LIMIT + 10, LEVELS, 4 * 60), FLOOR);
        assert_eq!(run.hold(LIMIT + 30, LEVELS, 4 * 60 * 10), FLOOR);
    }

    #[test]
    fn a_level_below_the_floor_is_never_touched() {
        let mut run = Run::new();
        run.hold(celsius(30), FLOOR - 10, 20);
        assert_eq!(run.hold(LIMIT + 30, FLOOR - 10, 4 * 60), FLOOR - 10);
    }

    #[test]
    fn steps_down_straight_away_from_a_level_under_the_ceiling() {
        let mut run = Run::new();
        let ui = LINEAR_MAX + 20;
        run.hold(celsius(30), ui, 20);
        // A believable jump, so it takes a few readings to count.
        run.hold(LIMIT + 5, ui, u16::from(GLITCHES) + 2);
        assert!(
            run.output < ui,
            "waited for the ceiling to come down from the top"
        );
    }

    #[test]
    fn comes_back_up_gently_once_it_cools() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        assert_eq!(run.hold(LIMIT + 10, LEVELS, 4 * 60), FLOOR);
        let (mut last, mut samples) = (FLOOR, 0);
        while run.output < LEVELS {
            let output = run.sample(celsius(30), LEVELS);
            assert!(output >= last, "stepped down while cool");
            last = output;
            samples += 1;
            assert!(samples < 4 * 60, "took over a minute to recover");
        }
        // No faster than a level a reading.
        assert!(samples >= u16::from(LEVELS - FLOOR));
    }

    #[test]
    fn holds_inside_the_window() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        run.hold(LIMIT + 3, LEVELS, 20);
        assert!(run.output < LEVELS);
        // Neither hot enough to step down nor cool enough to step up.
        for kelvin in [LIMIT, LIMIT - 1] {
            let held = run.hold(kelvin, LEVELS, 20);
            assert_eq!(
                run.hold(kelvin, LEVELS, 4 * 60),
                held,
                "moved at {kelvin} K"
            );
        }
    }

    #[test]
    fn noise_below_the_limit_changes_nothing() {
        let mut run = Run::new();
        let mut noise = Noise(7);
        for _ in 0..4 * 60 * 30 {
            let output = run.sample(noisy(LIMIT - 8, &mut noise, 2), LEVELS);
            assert_eq!(output, LEVELS);
        }
    }

    #[test]
    fn noise_over_the_limit_still_steps_down() {
        let mut run = Run::new();
        let mut noise = Noise(11);
        for _ in 0..4 * 60 {
            run.sample(noisy(LIMIT + 8, &mut noise, 2), LEVELS);
        }
        assert_eq!(run.output, FLOOR);
    }

    #[test]
    fn a_steady_reading_between_two_kelvin_is_reached_exactly() {
        let mut thermal = Thermal::default();
        thermal.sample(fraction(celsius(30)), LEVELS);
        let reading = fraction(celsius(30)) + 5;
        for _ in 0..8 {
            thermal.sample(reading, LEVELS);
        }
        assert_eq!(thermal.smooth, Some(reading as i16));
    }

    #[test]
    fn a_fraction_of_a_kelvin_over_the_limit_steps_down() {
        // Rounded to whole kelvin this would be the limit exactly, and hold.
        let (mut at, mut over) = (Run::new(), Run::new());
        for _ in 0..4 * 60 {
            at.sample_fraction(fraction(LIMIT), LEVELS);
            over.sample_fraction(fraction(LIMIT) + 7, LEVELS);
        }
        assert_eq!(at.output, LEVELS);
        assert!(over.output < LEVELS);
    }

    #[test]
    fn a_slow_rise_moves_the_prediction_steadily() {
        // Warming a sixteenth a reading, the trend should stay the same from
        // one reading to the next once it's filled. Whole kelvin would show
        // a kelvin's step every sixteen readings, and predict from that.
        let mut thermal = Thermal::default();
        let start = fraction(celsius(30));
        let mut trends = std::vec::Vec::new();
        for step in 0..64 {
            thermal.sample(start + step, LEVELS);
            let smooth = thermal.smooth.unwrap();
            let oldest = thermal.history[usize::from(thermal.next)];
            trends.push(smooth - oldest);
        }
        let settled = &trends[32..];
        assert!(
            settled.iter().all(|&trend| trend == settled[0]),
            "{trends:?}"
        );
        assert!(settled[0] > 0, "{trends:?}");
    }

    #[test]
    fn the_plausible_range_and_the_jump_are_the_ones_they_always_were() {
        assert!(!plausible(fraction(233) - 1));
        assert!(plausible(fraction(233)));
        assert!(plausible(fraction(423)));
        assert!(!plausible(fraction(423) + 1));

        let mut thermal = Thermal::default();
        let start = fraction(celsius(30));
        thermal.sample(start, LEVELS);
        // Six kelvin away is believed straight off, and a sixteenth more
        // waits for the readings after it.
        thermal.sample(start + JUMP, LEVELS);
        assert_eq!(thermal.rejected, 0);
        thermal.sample(start + 2 * JUMP + 1, LEVELS);
        assert_eq!(thermal.rejected, 1);
    }

    #[test]
    fn single_glitches_are_ignored() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        for glitch in [0, 1, 100, LIMIT + 20, 500, 1023, u16::MAX] {
            for _ in 0..GLITCHES {
                assert_eq!(run.sample(glitch, LEVELS), LEVELS, "{glitch}");
            }
            assert_eq!(run.hold(celsius(30), LEVELS, 20), LEVELS, "{glitch}");
        }
    }

    #[test]
    fn a_jump_that_lasts_is_believed() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        assert_eq!(run.hold(LIMIT + 20, LEVELS, u16::from(GLITCHES)), LEVELS);
        run.hold(LIMIT + 20, LEVELS, 4);
        assert!(run.output < LEVELS);
    }

    #[test]
    fn a_broken_sensor_steps_down_to_the_floor() {
        for broken in [0, u16::MAX] {
            let mut run = Run::new();
            run.hold(celsius(30), LEVELS, 20);
            assert_eq!(run.hold(broken, LEVELS, 4 * 10), FLOOR, "{broken}");
            // And one that never worked from the start.
            let mut run = Run::new();
            assert_eq!(run.hold(broken, LEVELS, 4 * 10), FLOOR, "{broken}");
        }
    }

    #[test]
    fn anything_at_all_moves_a_level_a_tick_at_most_and_stays_off_the_floor() {
        let mut run = Run::new();
        let mut noise = Noise(13);
        let mut kelvin = celsius(30);
        // An hour of a temperature wandering about, with junk mixed in.
        for _ in 0..4 * 60 * 60 {
            kelvin = kelvin
                .saturating_add_signed(noise.next(1))
                .clamp(celsius(10), LIMIT + 20);
            let reading = match noise.next(50) {
                50 => 0,
                -50 => u16::MAX,
                _ => kelvin,
            };
            // `Run` checks the step every tick.
            assert!(run.sample(reading, LEVELS) >= FLOOR);
        }
    }

    #[test]
    fn the_floor_is_the_top_of_the_linear_channel() {
        assert_eq!(FLOOR, LINEAR_MAX);
    }

    /// Turns the light off (no readings while it is) and on again.
    fn off_and_on(run: &mut Run) {
        run.output = 0;
        run.thermal.reset();
    }

    #[test]
    fn a_reset_forgets_the_ceiling_and_the_old_readings() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        assert_eq!(run.hold(LIMIT + 10, LEVELS, 4 * 60), FLOOR);
        off_and_on(&mut run);
        assert_eq!(run.thermal.ceiling(), LEVELS);
        // Cooled while off. It's a fresh start, not a jump to ignore, and
        // there's no trend from the hot readings to predict with.
        assert_eq!(run.hold(celsius(30), LEVELS, 4 * 60), LEVELS);
    }

    #[test]
    fn turning_on_hot_steps_down_from_the_first_readings() {
        let mut run = Run::new();
        run.hold(celsius(30), LEVELS, 20);
        off_and_on(&mut run);
        // Turned on at turbo while still 10 °C over: the first reading has
        // no history, and it still steps down straight away.
        assert!(run.sample(LIMIT + 10, LEVELS) < LEVELS);
        let mut samples = 1;
        while run.output > FLOOR {
            run.sample(LIMIT + 10, LEVELS);
            samples += 1;
        }
        assert!(samples <= 4 * 10, "took {samples} readings");
    }

    #[test]
    fn turning_on_hot_at_a_low_level_steps_down_from_there() {
        let mut run = Run::new();
        let ui = LINEAR_MAX + 30;
        off_and_on(&mut run);
        run.sample(LIMIT + 10, ui);
        // The first step down shows, rather than spending a second or two
        // bringing the ceiling down from the top.
        run.hold(LIMIT + 10, ui, 2);
        assert!(run.output < ui);
    }

    #[test]
    fn a_reset_keeps_the_config() {
        let config = Config {
            limit: celsius(50),
            floor: 100,
        };
        let mut thermal = Thermal::new(config);
        thermal.reset();
        assert_eq!(thermal.config, config);
    }

    #[test]
    fn turbo_settles_near_the_limit_without_hunting() {
        let mut run = Run::new();
        let mut head = Head::new(celsius(25));
        let mut noise = Noise(3);
        // Ten minutes to get hot and settle, then ten to watch.
        let warming = closed_loop(&mut run, &mut head, LEVELS, 4 * 60 * 10, &mut noise, 0);
        assert!(warming.hottest <= LIMIT + 2, "{warming:?}");
        let settled = closed_loop(&mut run, &mut head, LEVELS, 4 * 60 * 10, &mut noise, 0);
        assert!(settled.hottest <= LIMIT, "{settled:?}");
        assert!(settled.lowest > FLOOR, "{settled:?}");
        assert!(settled.highest - settled.lowest <= 4, "{settled:?}");
    }

    #[test]
    fn turbo_with_a_noisy_sensor_still_settles() {
        let mut run = Run::new();
        let mut head = Head::new(celsius(25));
        let mut noise = Noise(5);
        let warming = closed_loop(&mut run, &mut head, LEVELS, 4 * 60 * 10, &mut noise, 1);
        assert!(warming.hottest <= LIMIT + 2, "{warming:?}");
        let settled = closed_loop(&mut run, &mut head, LEVELS, 4 * 60 * 10, &mut noise, 1);
        assert!(settled.hottest <= LIMIT + 1, "{settled:?}");
        assert!(settled.lowest > FLOOR, "{settled:?}");
        assert!(settled.highest - settled.lowest <= 20, "{settled:?}");
    }

    #[test]
    fn a_cool_day_leaves_a_low_fet_level_alone() {
        let mut run = Run::new();
        let mut head = Head::new(celsius(20));
        let mut noise = Noise(9);
        let ui = LINEAR_MAX + 20;
        let stretch = closed_loop(&mut run, &mut head, ui, 4 * 60 * 20, &mut noise, 1);
        assert_eq!((stretch.lowest, stretch.highest), (ui, ui), "{stretch:?}");
    }
}
