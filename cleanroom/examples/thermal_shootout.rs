//! Main's `thermal::Thermal` against this crate's `Governor`, on the same plants
//! at the same limit, each fed the way its firmware would feed it: main gets one
//! 10-bit reading every 16 ticks as whole kelvin, this crate gets four accumulated
//! every 2 ticks. Both start at turbo with a floor at the top of the linear channel.
//!
//! The plants are guesses, so what matters is how each controller copes across
//! them, not any one number. Run with `cargo run --example thermal_shootout`.

use ts21::ramp as main_ramp;
use ts21::temp;
use ts21::thermal::{Config, Thermal, celsius};
use ts21_clean::governor::{Governor, Limits, Smoothed};
use ts21_clean::ramp;
use ts21_clean::sensors::heat_counts;

const GAIN: u8 = 160;
const OFFSET: i8 = -10;
const LIMIT_C: u8 = 45;
const TICK: f64 = 1.0 / 64.0;
const RUN_SECONDS: u32 = 3600;
const LAST_SECONDS: u32 = 900;
const SEEDS: u32 = 5;
const LINEAR_LM: f64 = 126.0;
const FET_LM: f64 = 2874.0;

#[derive(Clone, Copy)]
struct PlantSpec {
    name: &'static str,
    /// Where the LED's heat lands, and the rest of the light, in J/K.
    head: f64,
    body: f64,
    /// Head to body, and body to air, in K/W.
    head_to_body: f64,
    body_to_air: f64,
    /// How far the MCU's sensor lags the head, in seconds.
    sensor_lag: f64,
    ambient: f64,
    /// Body to air after ten minutes, for a change of conditions.
    later_body_to_air: f64,
}

const PLANTS: [PlantSpec; 6] = [
    PlantSpec {
        name: "one lump, 1 s lag",
        head: 10.0,
        body: 60.0,
        head_to_body: 0.2,
        body_to_air: 8.0,
        sensor_lag: 1.0,
        ambient: 22.0,
        later_body_to_air: 8.0,
    },
    PlantSpec {
        name: "two masses, 10 s lag",
        head: 20.0,
        body: 60.0,
        head_to_body: 1.0,
        body_to_air: 8.0,
        sensor_lag: 10.0,
        ambient: 22.0,
        later_body_to_air: 8.0,
    },
    PlantSpec {
        name: "two masses, 45 s lag",
        head: 20.0,
        body: 60.0,
        head_to_body: 1.0,
        body_to_air: 8.0,
        sensor_lag: 45.0,
        ambient: 22.0,
        later_body_to_air: 8.0,
    },
    PlantSpec {
        name: "two masses, 120 s lag",
        head: 20.0,
        body: 60.0,
        head_to_body: 1.0,
        body_to_air: 8.0,
        sensor_lag: 120.0,
        ambient: 22.0,
        later_body_to_air: 8.0,
    },
    PlantSpec {
        name: "35 C day, 45 s lag",
        head: 20.0,
        body: 60.0,
        head_to_body: 1.0,
        body_to_air: 8.0,
        sensor_lag: 45.0,
        ambient: 35.0,
        later_body_to_air: 8.0,
    },
    PlantSpec {
        name: "wind at 10 min, 45 s lag",
        head: 20.0,
        body: 60.0,
        head_to_body: 1.0,
        body_to_air: 8.0,
        sensor_lag: 45.0,
        ambient: 22.0,
        later_body_to_air: 4.0,
    },
];

struct Plant {
    spec: PlantSpec,
    head: f64,
    body: f64,
    sensor: f64,
    seconds: f64,
}

impl Plant {
    fn new(spec: PlantSpec) -> Self {
        Self {
            spec,
            head: spec.ambient,
            body: spec.ambient,
            sensor: spec.ambient,
            seconds: 0.0,
        }
    }

    fn step(&mut self, lumens: f64) {
        let spec = self.spec;
        let to_air = match self.seconds >= 600.0 {
            true => spec.later_body_to_air,
            false => spec.body_to_air,
        };
        let across = (self.head - self.body) / spec.head_to_body;
        let out = (self.body - spec.ambient) / to_air;
        self.head += (lumens / 120.0 - across) / spec.head * TICK;
        self.body += (across - out) / spec.body * TICK;
        self.sensor += (self.head - self.sensor) / spec.sensor_lag * TICK;
        self.seconds += TICK;
    }
}

struct Noise(u32);

impl Noise {
    /// A 10-bit sensor reading with a count of noise either way.
    fn raw(&mut self, celsius: f64) -> u16 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        let jitter = i32::try_from(self.0 % 3).unwrap() - 1;
        let exact = (celsius + 273.0) * 256.0 / f64::from(GAIN) + f64::from(OFFSET);
        (exact.round() as i32 + jitter) as u16
    }
}

trait Controller {
    fn name(&self) -> &'static str;
    /// One tick, returning the lumens to run at.
    fn tick(&mut self, ticks: u32, sensor: f64, noise: &mut Noise) -> f64;
}

struct OnMain {
    thermal: Thermal,
    level: u8,
}

impl OnMain {
    fn new() -> Self {
        Self {
            thermal: Thermal::new(Config {
                limit: celsius(LIMIT_C),
                floor: main_ramp::LINEAR_MAX,
            }),
            level: main_ramp::LEVELS,
        }
    }
}

impl Controller for OnMain {
    fn name(&self) -> &'static str {
        "main"
    }

    fn tick(&mut self, ticks: u32, sensor: f64, noise: &mut Noise) -> f64 {
        if ticks.is_multiple_of(16) {
            let kelvin = temp::kelvin(noise.raw(sensor), OFFSET, GAIN);
            self.thermal.sample(kelvin, self.level);
        }
        self.level = main_ramp::LEVELS.min(self.thermal.tick());
        let output = main_ramp::output(self.level);
        (f64::from(output.linear) * LINEAR_LM + f64::from(output.fet) * FET_LM) / 255.0
    }
}

struct OnClean {
    filter: Smoothed,
    governor: Governor,
    limits: Limits,
}

impl OnClean {
    fn new() -> Self {
        let kelvin = u16::from(LIMIT_C) + 273;
        Self {
            filter: Smoothed::default(),
            governor: Governor::default(),
            limits: Limits {
                trip: heat_counts(kelvin, GAIN, OFFSET),
                clear: heat_counts(kelvin - 3, GAIN, OFFSET),
                shift: 5,
                floor: ramp::LINEAR_TOP,
            },
        }
    }
}

impl Controller for OnClean {
    fn name(&self) -> &'static str {
        "clean"
    }

    fn tick(&mut self, ticks: u32, sensor: f64, noise: &mut Noise) -> f64 {
        if ticks.is_multiple_of(2) {
            self.filter.push((0..4).map(|_| noise.raw(sensor)).sum());
        }
        if ticks.is_multiple_of(16)
            && let Some(reading) = self.filter.get()
        {
            self.governor.update(reading, &self.limits);
        }
        let pwm = ramp::pwm(self.governor.ceiling());
        let counts = f64::from(pwm.period) + 1.0;
        (f64::from(pwm.linear) * LINEAR_LM + f64::from(pwm.fet) * FET_LM) / counts
    }
}

#[derive(Default)]
struct Score {
    peak_sensor: f64,
    peak_head: f64,
    over_seconds: f64,
    mean_lumens: f64,
    swing: f64,
    reversals: f64,
    worst_second: f64,
}

fn run(spec: PlantSpec, mut controller: impl Controller, seed: u32) -> Score {
    let mut plant = Plant::new(spec);
    let mut noise = Noise(seed.wrapping_mul(2_654_435_761) | 1);
    let mut score = Score::default();
    let (mut lowest, mut highest, mut total, mut samples) = (f64::MAX, 0.0_f64, 0.0, 0.0);
    let (mut direction, mut second_ago) = (0.0_f64, 0.0_f64);
    let mut last = 0.0;
    let ticks = RUN_SECONDS * 64;
    for tick in 0..ticks {
        let lumens = controller.tick(tick, plant.sensor, &mut noise);
        plant.step(lumens);
        score.peak_sensor = score.peak_sensor.max(plant.sensor);
        score.peak_head = score.peak_head.max(plant.head);
        if plant.sensor > f64::from(LIMIT_C) + 1.0 {
            score.over_seconds += TICK;
        }
        if tick.is_multiple_of(64) {
            if second_ago > 0.0 {
                let ratio = (lumens / second_ago).max(second_ago / lumens);
                score.worst_second = score.worst_second.max(ratio);
            }
            second_ago = lumens;
        }
        if tick >= (RUN_SECONDS - LAST_SECONDS) * 64 {
            lowest = lowest.min(lumens);
            highest = highest.max(lumens);
            total += lumens;
            samples += 1.0;
            let moved = lumens - last;
            if moved != 0.0 {
                if moved.signum() == -direction {
                    score.reversals += 1.0;
                }
                direction = moved.signum();
            }
        }
        last = lumens;
    }
    score.mean_lumens = total / samples;
    score.swing = highest / lowest;
    score
}

fn report(spec: PlantSpec, make: impl Fn() -> Box<dyn Controller>) {
    let mut sum = Score::default();
    let mut name = "";
    for seed in 1..=SEEDS {
        let controller = make();
        name = controller.name();
        let score = run(spec, Boxed(controller), seed);
        sum.peak_sensor = sum.peak_sensor.max(score.peak_sensor);
        sum.peak_head = sum.peak_head.max(score.peak_head);
        sum.over_seconds += score.over_seconds / f64::from(SEEDS);
        sum.mean_lumens += score.mean_lumens / f64::from(SEEDS);
        sum.swing = sum.swing.max(score.swing);
        sum.reversals += score.reversals / f64::from(SEEDS);
        sum.worst_second = sum.worst_second.max(score.worst_second);
    }
    println!(
        "{:<26}{:<7}{:>7.1}{:>7.1}{:>7.0}{:>7.0}{:>7.2}{:>7.0}{:>7.2}",
        spec.name,
        name,
        sum.peak_sensor,
        sum.peak_head,
        sum.over_seconds,
        sum.mean_lumens,
        sum.swing,
        sum.reversals,
        sum.worst_second,
    );
}

struct Boxed(Box<dyn Controller>);

impl Controller for Boxed {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn tick(&mut self, ticks: u32, sensor: f64, noise: &mut Noise) -> f64 {
        self.0.tick(ticks, sensor, noise)
    }
}

fn main() {
    println!(
        "limit {LIMIT_C} C, {RUN_SECONDS} s from turbo, {SEEDS} seeds; last {LAST_SECONDS} s for lm, swing and rev"
    );
    println!(
        "{:<26}{:<7}{:>7}{:>7}{:>7}{:>7}{:>7}{:>7}{:>7}",
        "plant", "who", "sensC", "headC", "over_s", "lm", "swing", "rev", "1s_x"
    );
    for spec in PLANTS {
        report(spec, || Box::new(OnMain::new()));
        report(spec, || Box::new(OnClean::new()));
    }
}
