//! `cargo xtask simulate`: the UI in `src/ui.rs`, run on your machine.
//!
//! It drives `ts21::torch::Torch`, the same loop body the firmware runs, one
//! switch sample per 64 Hz tick. Give it a script in Anduril's notation and
//! it prints what each click and hold did:
//!
//! ```text
//! cargo xtask simulate 1C 2H:1.5s wait:1s 3C
//! ```
//!
//! `NC` is N quick clicks. `NH` is N - 1 clicks then a hold (so `2H` is
//! click, hold), for a second unless it says otherwise (`1H:500ms`).
//! `wait:2s` leaves the switch alone. Every click or hold is followed by
//! enough of a pause for it to count, so `1C 1C` is two single clicks.
//!
//! A few words set what the ADC sees from then on, taking no time: `cell:3.05V`
//! for the cell (as the 0614 calibration reports it, 3.8 V to start), `temp:60C`
//! for the MCU (25 °C to start), and `adc:0` for a broken ADC whose every
//! conversion reads that number. So `cell:3.05V 1C wait:6s` is a low cell.
//! `fuse:OSCCFG=0x01` sets a fuse byte and `flash:corrupt` breaks a byte of
//! flash. `ram:stuck:0x3800.3=1`, `ram:alias:0x3900=0x3800`,
//! `ram:bridge:0x3800.0.1` and `ram:couple:0x3900.7=1:0x3800.0` break a byte
//! of SRAM, `reg:r24.0=1` and `reg:short:r24.0.2=0` a CPU register, `SREG`,
//! `SPL` and `SPH` included (`world::RamFault` and `RegisterFault` say how),
//! and `wdt:never` the watchdog (`world::WdtFault` has the rest). `proof:press`
//! is a press that comes and goes during the watchdog's proof. A script's
//! are all there from power-on, so they have to come before anything that
//! takes time. `hang` can go anywhere: the firmware stops there until the
//! watchdog resets it, once.
//!
//! With no script it runs live in the terminal, in real time, with the keys
//! in `HELP`. Terminals don't say when a key goes up, which a hold needs, so
//! `h` holds the switch down until pressed again.

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use crate::world::{FUSE_NAMES, RamFault, RegisterFault, Rig, WdtFault, World, unmodelled_fuse};
use ts21::TICKS_PER_SECOND;
use ts21::button::{Event, HOLD_TICKS, RELEASE_TICKS};
use ts21::ramp::{self, LEVELS};

use crate::terminal::{CTRL_C, Terminal};

/// How long a scripted click holds the switch down (about 60 ms).
const PRESS_TICKS: u16 = 4;
/// The pause after each scripted click, well inside `RELEASE_TICKS`.
const GAP_TICKS: u16 = 4;
/// Released samples after a click or hold before the next action: enough
/// for `button::Clicks` to send the click (see its `RELEASE_TICKS` check).
const SETTLE_TICKS: u16 = RELEASE_TICKS + 1;
/// A hold with no duration lasts a second.
const DEFAULT_HOLD_TICKS: u16 = TICKS_PER_SECOND;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// This many quick clicks.
    Clicks(u8),
    /// `clicks` quick clicks, then the switch held down for `ticks`.
    Hold { clicks: u8, ticks: u16 },
    /// The switch left alone for this many ticks.
    Wait(u16),
    /// The cell from now on, in centivolts.
    Cell(u16),
    /// The MCU's temperature from now on, in kelvin.
    Temp(u16),
    /// Every ADC conversion reads this from now on.
    Adc(u16),
    /// A fuse byte, by its offset from FUSE.WDTCFG. Power-on only.
    Fuse { offset: u8, value: u8 },
    /// A flash byte that doesn't match the CRC. Power-on only.
    CorruptFlash,
    /// A broken byte of SRAM, from reset. Power-on only.
    Ram(RamFault),
    /// A stuck bit in a CPU register, from reset. Power-on only.
    Register(RegisterFault),
    /// A broken watchdog, from reset. Power-on only.
    Wdt(WdtFault),
    /// A press that comes and goes during the watchdog's proof. Power-on only.
    ProofPress,
    /// The firmware hangs, until the watchdog resets it.
    Hang,
}

impl Action {
    /// Whether it sets something the firmware only looks at once, at
    /// power-on, so it has to come before anything that takes time.
    fn power_on(self) -> bool {
        matches!(
            self,
            Self::Fuse { .. }
                | Self::CorruptFlash
                | Self::Ram(_)
                | Self::Register(_)
                | Self::Wdt(_)
                | Self::ProofPress
        )
    }

    /// Whether it takes time, rather than setting the scene.
    fn takes_time(self) -> bool {
        matches!(self, Self::Clicks(_) | Self::Hold { .. } | Self::Wait(_))
    }
}

impl FromStr for Action {
    type Err = String;

    /// `3C`, `2H`, `1H:500ms`, `wait:2s`, `cell:3.05V`, `temp:60C` or `adc:0`.
    fn from_str(word: &str) -> Result<Self, String> {
        if let Some(volts) = word.strip_prefix("cell:") {
            return centivolts(volts).map(Self::Cell);
        }
        if let Some(celsius) = word.strip_prefix("temp:") {
            let bad = || format!("expected a temperature like 60C, not `{celsius}`");
            let degrees: u8 = celsius
                .strip_suffix('C')
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            return Ok(Self::Temp(ts21::thermal::celsius(degrees)));
        }
        if let Some(setting) = word.strip_prefix("fuse:") {
            return fuse(setting);
        }
        if word == "flash:corrupt" {
            return Ok(Self::CorruptFlash);
        }
        if let Some(fault) = word.strip_prefix("ram:") {
            return fault.parse().map(Self::Ram);
        }
        if let Some(fault) = word.strip_prefix("reg:") {
            return fault.parse().map(Self::Register);
        }
        if let Some(fault) = word.strip_prefix("wdt:") {
            return fault.parse().map(Self::Wdt);
        }
        if word == "proof:press" {
            return Ok(Self::ProofPress);
        }
        if word == "hang" {
            return Ok(Self::Hang);
        }
        if let Some(raw) = word.strip_prefix("adc:") {
            let raw = raw
                .parse()
                .map_err(|_| format!("expected a raw reading like adc:0, not `{raw}`"))?;
            return Ok(Self::Adc(raw));
        }
        if let Some(duration) = word.strip_prefix("wait:") {
            return Ok(Self::Wait(ticks(duration)?));
        }
        let (head, duration) = match word.split_once(':') {
            Some((head, duration)) => (head, Some(duration)),
            None => (word, None),
        };
        let split = head.len().saturating_sub(1);
        let (count, kind) = head.split_at(split);
        let count: u8 = match count.parse() {
            Ok(n) if n > 0 => n,
            _ => return Err("expected a count of 1 or more, like 1C, 2H or wait:1s".into()),
        };
        match (kind, duration) {
            ("C" | "c", None) => Ok(Self::Clicks(count)),
            ("C" | "c", Some(_)) => Err("clicks don't take a duration; holds do (1H:2s)".into()),
            ("H" | "h", duration) => {
                let ticks = duration.map_or(Ok(DEFAULT_HOLD_TICKS), ticks)?;
                if ticks <= HOLD_TICKS {
                    return Err(format!(
                        "that's a click; a hold needs longer than {} ms",
                        u32::from(HOLD_TICKS) * 1000 / u32::from(TICKS_PER_SECOND)
                    ));
                }
                Ok(Self::Hold {
                    clicks: count - 1,
                    ticks,
                })
            }
            _ => Err("expected C (clicks), H (hold) or wait:".into()),
        }
    }
}

/// Parses a script, word by word.
pub fn parse(words: &[&str]) -> Result<Vec<Action>, String> {
    let mut started = false;
    let actions = words
        .iter()
        .map(|word| {
            let action: Action = word.parse().map_err(|e| format!("`{word}`: {e}"))?;
            if action.power_on() && started {
                return Err(format!(
                    "`{word}`: the firmware only reads that at power-on, so it goes \
                     before anything that takes time"
                ));
            }
            started |= action.takes_time();
            Ok(action)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !started && actions.iter().any(|a| a.power_on()) {
        return Err(
            "a fuse, flash, RAM or register setting needs something after it that takes time, \
                    or nothing runs to check it"
                .into(),
        );
    }
    Ok(actions)
}

/// A fuse byte from `OSCCFG=0x01`.
fn fuse(setting: &str) -> Result<Action, String> {
    let bad = || format!("expected a fuse like fuse:OSCCFG=0x01, not `{setting}`");
    let (name, value) = setting.split_once('=').ok_or_else(bad)?;
    let offset = FUSE_NAMES
        .iter()
        .position(|&n| !n.is_empty() && n == name)
        .ok_or_else(|| {
            let names: Vec<&str> = FUSE_NAMES.into_iter().filter(|n| !n.is_empty()).collect();
            format!("no fuse called `{name}`; try {}", names.join(", "))
        })?;
    let value = value.strip_prefix("0x").ok_or_else(bad)?;
    let value = u8::from_str_radix(value, 16).map_err(|_| bad())?;
    if let Some(why) = unmodelled_fuse(offset, value) {
        return Err(format!("{name}={value:#04x}: {why}"));
    }
    Ok(Action::Fuse {
        offset: offset as u8,
        value,
    })
}

/// Ticks from `500ms`, `2s` or `1.5s`, rounded to the nearest tick.
fn ticks(duration: &str) -> Result<u16, String> {
    let bad = || format!("expected a duration like 500ms, 2s or 1.5s, not `{duration}`");
    let ms: u32 = if let Some(ms) = duration.strip_suffix("ms") {
        ms.parse().map_err(|_| bad())?
    } else if let Some(s) = duration.strip_suffix('s') {
        let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
        if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let whole: u32 = whole.parse().map_err(|_| bad())?;
        let fraction: u32 = format!("{fraction:0<3}").parse().map_err(|_| bad())?;
        whole.checked_mul(1000).ok_or_else(bad)? + fraction
    } else {
        return Err(bad());
    };
    let ticks = (u64::from(ms) * u64::from(TICKS_PER_SECOND) + 500) / 1000;
    match u16::try_from(ticks) {
        Ok(0) => Err(format!("{duration} is shorter than a tick")),
        Ok(ticks) => Ok(ticks),
        Err(_) => Err(format!("{duration} is too long")),
    }
}

/// Centivolts from `3.05V`.
fn centivolts(volts: &str) -> Result<u16, String> {
    let bad = || format!("expected a voltage like 3.05V, not `{volts}`");
    let number = volts.strip_suffix('V').ok_or_else(bad)?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if fraction.len() > 2 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let whole: u16 = whole.parse().map_err(|_| bad())?;
    let fraction: u16 = format!("{fraction:0<2}").parse().map_err(|_| bad())?;
    whole
        .checked_mul(100)
        .and_then(|cv| cv.checked_add(fraction))
        .ok_or_else(bad)
}

/// One tick of a script: the switch, and what the ADC sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub pressed: bool,
    pub world: World,
}

/// The switch and the world, one sample per tick, for a whole script.
pub fn samples(actions: &[Action]) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut world = World::NORMAL;
    for &action in actions {
        let pressed: Vec<bool> = match action {
            Action::Clicks(n) => clicks(n).chain(span(false, SETTLE_TICKS)).collect(),
            Action::Hold { clicks: n, ticks } => clicks(n)
                .chain(span(true, ticks))
                .chain(span(false, SETTLE_TICKS))
                .collect(),
            Action::Wait(ticks) => span(false, ticks).collect(),
            Action::Cell(cell) => {
                world.cell = cell;
                continue;
            }
            Action::Temp(kelvin) => {
                world.kelvin = kelvin;
                continue;
            }
            Action::Adc(raw) => {
                world.stuck = Some(raw);
                continue;
            }
            Action::Fuse { offset, value } => {
                world.fuses[usize::from(offset)] = value;
                continue;
            }
            Action::CorruptFlash => {
                world.corrupt_flash = true;
                continue;
            }
            Action::Ram(fault) => {
                world.ram = Some(fault);
                continue;
            }
            Action::Register(fault) => {
                world.register = Some(fault);
                continue;
            }
            Action::Wdt(fault) => {
                world.wdt = Some(fault);
                continue;
            }
            Action::ProofPress => {
                world.proof_press = Some(1);
                continue;
            }
            Action::Hang => {
                world.hang = true;
                continue;
            }
        };
        out.extend(pressed.into_iter().map(|pressed| Sample { pressed, world }));
    }
    out
}

/// `n` quick clicks, each a press then a short gap.
fn clicks(n: u8) -> impl Iterator<Item = bool> {
    (0..n).flat_map(|_| span(true, PRESS_TICKS).chain(span(false, GAP_TICKS)))
}

fn span(pressed: bool, ticks: u16) -> impl Iterator<Item = bool> {
    std::iter::repeat_n(pressed, ticks.into())
}

/// What a script did to a fresh torch.
pub struct Run {
    /// One line per click and hold.
    pub lines: Vec<String>,
    pub ticks: u32,
    /// Where the light was left.
    pub level: u8,
}

/// Runs a script on a fresh torch.
pub fn run(actions: &[Action]) -> Run {
    let samples = samples(actions);
    let mut rig = Rig::new(&samples.first().map_or(World::NORMAL, |s| s.world));
    let mut log = Log::default();
    let mut lines = Vec::new();
    for (now, sample) in (0..).zip(&samples) {
        let event = rig.tick(sample.pressed, &sample.world);
        lines.extend(log.note(now, event, rig.level()));
    }
    lines.extend(log.settle());
    Run {
        lines,
        ticks: samples.len() as u32,
        level: rig.level(),
    }
}

/// Runs a script and prints what happened.
pub fn script(words: &[&str]) -> Result<(), String> {
    let run = run(&parse(words)?);
    for line in &run.lines {
        println!("{line}");
    }
    println!("{}  ends {}", Seconds(run.ticks), Level(run.level));
    Ok(())
}

/// Turns ticks into log lines: one per click, one per hold (when it ends,
/// not every tick it's held), and one for any change of level that no
/// click or hold explains. A run of one-level steps a tick apart, all the
/// same way, is a fade, and gets one line when it stops.
#[derive(Default)]
struct Log {
    level: u8,
    /// The tick the press that's being held started on, and the level
    /// before it.
    hold: Option<(u32, u8)>,
    fade: Option<Fade>,
}

/// A fade in progress: the tick of its first and latest steps, and the
/// level before it and now.
struct Fade {
    start: u32,
    last: u32,
    from: u8,
    to: u8,
}

impl Fade {
    /// Whether a step from `before` to `level` on tick `now` carries on.
    fn continues(&self, now: u32, before: u8, level: u8) -> bool {
        self.last + 1 == now && self.to == before && (level > before) == (self.to > self.from)
    }
}

impl Log {
    /// The lines this tick ends up with: usually none, sometimes a fade that
    /// just stopped and the thing that stopped it.
    fn note(&mut self, now: u32, event: Option<Event>, level: u8) -> Vec<String> {
        let before = std::mem::replace(&mut self.level, level);
        let quiet = event.is_none() && self.hold.is_none();
        if quiet && level.abs_diff(before) == 1 {
            if let Some(fade) = &mut self.fade
                && fade.continues(now, before, level)
            {
                fade.last = now;
                fade.to = level;
                return Vec::new();
            }
            let stopped = self.settle();
            self.fade = Some(Fade {
                start: now,
                last: now,
                from: before,
                to: level,
            });
            return stopped.into_iter().collect();
        }
        let mut lines: Vec<String> = self.settle().into_iter().collect();
        lines.extend(self.line(now, event, before, level));
        lines
    }

    /// The line for a fade that's stopped or been interrupted, if there was
    /// one. A single step shows as a plain change of level.
    fn settle(&mut self) -> Option<String> {
        let Fade {
            start,
            last,
            from,
            to,
        } = self.fade.take()?;
        if from.abs_diff(to) == 1 {
            return Some(format!("{}  {:<16} → {}", Seconds(start), "", Level(to)));
        }
        let over = Seconds(last + 1 - start).to_string();
        Some(
            format!(
                "{}  {:<16} → {}",
                Seconds(start),
                format!("fade from {:#}", Level(from)),
                Level(to)
            ) + &format!(" over {}", over.trim_start()),
        )
    }

    fn line(&mut self, now: u32, event: Option<Event>, before: u8, level: u8) -> Option<String> {
        let (what, before) = match event {
            Some(Event::Click(n)) => (format!("{n}C"), before),
            Some(Event::Hold { ticks, .. }) => {
                // The first Hold comes HOLD_TICKS into the press.
                let start = now - u32::from(HOLD_TICKS) - u32::from(ticks);
                self.hold.get_or_insert((start, before));
                return None;
            }
            Some(Event::Release { clicks }) => {
                let (start, before) = self.hold.take().unwrap_or((now, before));
                let held = Seconds(now - start).to_string();
                (
                    format!("{}H, held {}", clicks + 1, held.trim_start()),
                    before,
                )
            }
            None if self.hold.is_none() && level != before => (String::new(), before),
            None => return None,
        };
        let unchanged = if level == before { " (no change)" } else { "" };
        Some(format!(
            "{}  {what:<16} → {}{unchanged}",
            Seconds(now),
            Level(level)
        ))
    }
}

/// Ticks shown as seconds: `  1.50 s` for 96 ticks.
struct Seconds(u32);

impl fmt::Display for Seconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let per = u32::from(TICKS_PER_SECOND);
        write!(f, "{:>3}.{:02} s", self.0 / per, self.0 % per * 100 / per)
    }
}

/// A level as people read it: `off` or `level 50/150`, or with `{:#}`, the
/// shorter `50/150`.
struct Level(u8);

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => f.pad("off"),
            n if f.alternate() => f.pad(&format!("{n}/{LEVELS}")),
            n => f.pad(&format!("level {n}/{LEVELS}")),
        }
    }
}

const HELP: &str = "\
space  click
1-9    that many quick clicks
h      hold the switch down, or let it go (terminals can't report a key
       being let go, so holding a key down won't do)
r      start again from off
?      these keys
q      quit";

/// Width of the brightness bar on the status line, in characters.
const BAR: usize = 20;

/// Runs the UI live in the terminal until `q` or Ctrl-C.
pub fn interactive() -> Result<(), String> {
    let mut terminal = Terminal::open()?;
    let mut live = Live::default();
    let print = |terminal: &Terminal, text: &str| {
        for line in text.lines() {
            terminal.write(&format!("\r\x1b[2K{line}\n"));
        }
    };
    print(&terminal, &format!("{HELP}\n"));

    let period = Duration::from_micros(1_000_000 / u64::from(TICKS_PER_SECOND));
    let mut next = Instant::now();
    let mut shown = String::new();
    loop {
        let Some(keys) = terminal.poll() else {
            return Ok(());
        };
        for key in keys {
            match live.key(key) {
                Command::Continue => {}
                Command::Help => print(&terminal, HELP),
                Command::Reset => print(&terminal, "reset"),
                Command::Quit => return Ok(()),
            }
        }
        if let Some(line) = live.tick() {
            print(&terminal, &line);
            shown.clear();
        }
        let status = live.status(terminal.width);
        if status != shown {
            terminal.write(&format!("\r\x1b[2K{status}"));
            shown = status;
        }
        // Keep to the clock, not to how long each pass took.
        next += period;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Continue,
    Help,
    Reset,
    Quit,
}

/// A live session, minus the terminal: keys in, ticks, a status line out.
#[derive(Default)]
struct Live {
    /// The torch, with a 3.8 V cell at 25 °C.
    torch: Rig,
    log: Log,
    now: u32,
    /// Scripted samples from clicks, played before `held` counts.
    queue: VecDeque<bool>,
    /// Where the switch rests once `queue` runs out.
    held: bool,
    /// The last sample the torch saw.
    pressed: bool,
}

impl Live {
    fn key(&mut self, key: char) -> Command {
        match key.to_ascii_lowercase() {
            'q' | CTRL_C => return Command::Quit,
            ' ' => self.click(1),
            digit @ '1'..='9' => self.click(digit as u8 - b'0'),
            'h' => self.held = !self.held,
            '?' => return Command::Help,
            'r' => {
                *self = Self::default();
                return Command::Reset;
            }
            _ => {}
        }
        Command::Continue
    }

    fn click(&mut self, n: u8) {
        self.held = false;
        self.queue.extend(clicks(n));
    }

    /// One 64 Hz tick, and a log line if it did something.
    fn tick(&mut self) -> Option<String> {
        self.pressed = self.queue.pop_front().unwrap_or(self.held);
        let event = self.torch.tick(self.pressed, &World::NORMAL);
        let lines = self.log.note(self.now, event, self.torch.level());
        self.now += 1;
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// A brightness bar, the level and duty cycles, the switch and the UI's
    /// state as `Debug` shows it, cut to fit in `width` columns.
    fn status(&self, width: usize) -> String {
        let level = self.torch.level();
        let output = ramp::output(level);
        let lit = lit(level);
        let switch = match (self.pressed, self.held) {
            (true, true) => "held",
            (true, false) => "down",
            (false, _) => "up",
        };
        let text = format!(
            " {:>#7}  linear {:>3}  FET {:>3}  switch {switch:<4}  {:?}",
            Level(level),
            output.linear,
            output.fet,
            self.torch.torch.ui()
        );
        let text: String = text.chars().take(width.saturating_sub(BAR + 1)).collect();
        format!(
            "\x1b[33m{}\x1b[2m{}\x1b[0m{text}",
            "█".repeat(lit),
            "░".repeat(BAR - lit)
        )
    }
}

/// How much of the bar a level fills: none when off, at least one block
/// when on, and all of it at the top.
fn lit(level: u8) -> usize {
    (usize::from(level) * BAR).div_ceil(usize::from(LEVELS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Register;
    use ts21::ui::LEVEL;

    fn script(script: &str) -> Run {
        let words: Vec<_> = script.split_whitespace().collect();
        run(&parse(&words).unwrap())
    }

    #[test]
    fn parses_clicks_holds_and_waits() {
        let words: Vec<_> = "1C 3c 1H 2H:500ms wait:1.5s wait:2s"
            .split_whitespace()
            .collect();
        assert_eq!(
            parse(&words),
            Ok(vec![
                Action::Clicks(1),
                Action::Clicks(3),
                Action::Hold {
                    clicks: 0,
                    ticks: 64
                },
                Action::Hold {
                    clicks: 1,
                    ticks: 32
                },
                Action::Wait(96),
                Action::Wait(128),
            ])
        );
    }

    #[test]
    fn rejects_what_it_cant_read() {
        for word in [
            "0C",
            "C",
            "1X",
            "",
            "1C:1s",
            "1H:fast",
            "1H:1.2345s",
            "1H:100ms",
            "wait:0ms",
            "wait:2",
            "wait:1.-5s",
            "wait:9999s",
            "256C",
            "fuse:OSCCFG",
            "fuse:OSCCFG=1",
            "fuse:OSCCFG=0x100",
            "fuse:FOO=0x01",
            "fuse:=0x01",
            "fuse:LOCKBIT=0x00",
        ] {
            assert!(word.parse::<Action>().is_err(), "{word:?} parsed");
        }
    }

    #[test]
    fn sets_fuses_by_name() {
        assert_eq!(
            "fuse:OSCCFG=0x01".parse::<Action>(),
            Ok(Action::Fuse {
                offset: 2,
                value: 0x01
            })
        );
        let samples = samples(&parse(&["fuse:BOOTEND=0x02", "wait:1s"]).unwrap());
        assert_eq!(samples[0].world.fuses[8], 0x02);
    }

    #[test]
    fn fuses_and_flash_only_go_before_anything_that_takes_time() {
        assert!(parse(&["cell:3.1V", "fuse:WDTCFG=0x10", "flash:corrupt", "1C"]).is_ok());
        for late in ["fuse:WDTCFG=0x10", "flash:corrupt"] {
            let err = parse(&["wait:1s", late]).unwrap_err();
            assert!(err.contains("power-on"), "{err}");
        }
        assert!("flash:fine".parse::<Action>().is_err());
        let err = parse(&["flash:corrupt"]).unwrap_err();
        assert!(err.contains("takes time"), "nothing would check it: {err}");
    }

    #[test]
    fn refuses_fuses_that_would_stop_a_real_chip() {
        for word in ["fuse:WDTCFG=0x0B", "fuse:SYSCFG0=0x04"] {
            let err = word.parse::<Action>().unwrap_err();
            assert!(err.contains("doesn't model"), "{word}: {err}");
        }
    }

    #[test]
    fn corrupt_flash_shows_nothing_and_keeps_the_ui() {
        let run = script("flash:corrupt wait:3s");
        assert_eq!(run.level, 0);
        assert!(run.lines.is_empty(), "{:?}", run.lines);
        assert_eq!(script("flash:corrupt wait:3s 1C").level, 50);
    }

    #[test]
    fn a_bad_fuse_shows_nothing_and_keeps_the_ui() {
        let run = script("fuse:OSCCFG=0x01 wait:3s");
        assert_eq!(run.level, 0);
        assert!(run.lines.is_empty(), "{:?}", run.lines);
        assert_eq!(script("fuse:OSCCFG=0x01 wait:3s 1C").level, 50);
    }

    #[test]
    fn broken_ram_or_registers_go_before_anything_that_takes_time() {
        let actions = parse(&["ram:alias:0x3900=0x3800", "reg:r5.1=0", "1C"]).unwrap();
        let world = samples(&actions)[0].world;
        assert_eq!(
            world.ram,
            Some(RamFault::Alias {
                addr: 0x3900,
                to: 0x3800
            })
        );
        assert_eq!(
            world.register,
            Some(RegisterFault::Stuck {
                reg: Register::R(5),
                bit: 1,
                high: false
            })
        );
        for late in ["ram:stuck:0x3800.0=1", "reg:r0.0=1"] {
            let err = parse(&["wait:1s", late]).unwrap_err();
            assert!(err.contains("power-on"), "{err}");
        }
    }

    #[test]
    fn broken_ram_or_registers_leave_it_dark() {
        for script_text in [
            "ram:stuck:0x3800.3=1 wait:3s 1C wait:1s",
            "ram:alias:0x3900=0x3800 1C wait:2s",
            "ram:bridge:0x3C00.0.1 1C wait:2s",
            "ram:couple:0x3900.7=1:0x3800.0 1C wait:2s",
            "reg:r24.0=1 1C 2H wait:1s",
            "reg:short:r24.0.2=0 1C wait:1s",
        ] {
            let run = script(script_text);
            assert_eq!(run.level, 0, "{script_text}");
            assert!(run.lines.is_empty(), "{script_text}: {:?}", run.lines);
        }
    }

    #[test]
    fn rounds_durations_to_the_nearest_tick() {
        assert_eq!(ticks("8ms"), Ok(1));
        assert_eq!(ticks("15ms"), Ok(1));
        assert_eq!(ticks("24ms"), Ok(2));
        assert_eq!(ticks("1s"), Ok(64));
        assert_eq!(ticks("0.25s"), Ok(16));
    }

    #[test]
    fn a_click_turns_it_on_and_another_turns_it_off() {
        assert_eq!(script("1C").level, LEVEL);
        assert_eq!(script("1C 1C").level, 0);
    }

    #[test]
    fn everything_else_leaves_it_alone() {
        assert_eq!(script("2C 1H 3H:2s").level, 0);
        assert_eq!(script("1C 2C 1H").level, LEVEL);
    }

    #[test]
    fn logs_one_line_per_click_and_per_hold() {
        let lines = script("1C 1H:1s 2C").lines;
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[0].contains("1C") && lines[0].ends_with(&Level(LEVEL).to_string()));
        assert!(lines[1].contains("1H, held 1.00 s") && lines[1].ends_with("(no change)"));
        assert!(lines[2].contains("2C") && lines[2].ends_with("(no change)"));
    }

    #[test]
    fn times_a_hold_from_the_start_of_the_press() {
        let mut log = Log::default();
        for ticks in 0..10 {
            let hold = Event::Hold { clicks: 1, ticks };
            let now = u32::from(HOLD_TICKS + ticks);
            assert!(log.note(now, Some(hold), 0).is_empty());
        }
        let now = u32::from(HOLD_TICKS) + 10;
        let line = log
            .note(now, Some(Event::Release { clicks: 1 }), 0)
            .remove(0);
        let held = Seconds(now).to_string();
        assert!(
            line.contains(&format!("2H, held {}", held.trim_start())),
            "{line}"
        );
    }

    #[test]
    fn logs_a_change_of_level_with_no_event() {
        let mut log = Log::default();
        assert!(log.note(0, None, 30)[0].ends_with("level 30/150"));
        assert!(log.note(1, None, 30).is_empty());
        // A lone one-level step shows once the next tick says it's alone.
        assert!(log.note(2, None, 31).is_empty());
        let lines = log.note(3, None, 31);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with(&Seconds(2).to_string()), "{lines:?}");
        assert!(lines[0].ends_with("level 31/150"), "{lines:?}");
    }

    #[test]
    fn logs_a_fade_as_one_line_when_it_stops() {
        let mut log = Log::default();
        log.note(0, None, 150);
        for (now, level) in (1..).zip((140..150).rev()) {
            assert!(log.note(now, None, level).is_empty(), "tick {now}");
        }
        let lines = log.note(11, None, 140);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(
            lines[0],
            format!(
                "{}  fade from 150/150 → level 140/150 over 0.15 s",
                Seconds(1)
            )
        );
    }

    #[test]
    fn a_click_or_a_turn_stops_a_fade() {
        let mut log = Log::default();
        log.note(0, None, 10);
        log.note(1, None, 9);
        log.note(2, None, 8);
        // Turning round starts a new fade.
        let lines = log.note(3, None, 9);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("fade from 10/150 → level 8/150 "),
            "{lines:?}"
        );
        log.note(4, None, 10);
        let lines = log.note(5, Some(Event::Click(1)), 0);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("fade from 8/150  → level 10/150 "),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("1C") && lines[1].ends_with("off"),
            "{lines:?}"
        );
        assert_eq!(log.settle(), None);
    }

    #[test]
    fn a_low_cell_logs_a_line_per_step_down() {
        let lines = script("cell:3.05V 1C wait:11s").lines;
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[1].contains("fade from 50/150 → level 35/150 over"));
        assert!(lines[2].contains("fade from 35/150 → level 20/150 over"));
    }

    /// Ticks a live session `n` times and keeps the log.
    fn ticks_of(live: &mut Live, n: u16) -> Vec<String> {
        (0..n).filter_map(|_| live.tick()).collect()
    }

    #[test]
    fn space_clicks_and_digits_click_that_many_times() {
        let mut live = Live::default();
        live.key(' ');
        let lines = ticks_of(&mut live, 64);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("1C") && live.torch.level() == LEVEL);

        live.key('3');
        let lines = ticks_of(&mut live, 64);
        assert!(lines[0].contains("3C") && live.torch.level() == LEVEL);
    }

    #[test]
    fn h_holds_the_switch_until_pressed_again() {
        let mut live = Live::default();
        live.key('h');
        assert!(ticks_of(&mut live, 64).is_empty());
        assert!(live.pressed);
        live.key('h');
        let lines = ticks_of(&mut live, 64);
        assert!(lines[0].contains("1H, held 1.00 s"), "{lines:?}");
        assert!(!live.pressed);
    }

    #[test]
    fn q_and_ctrl_c_quit_and_r_starts_again() {
        let mut live = Live::default();
        assert_eq!(live.key('q'), Command::Quit);
        assert_eq!(live.key(CTRL_C), Command::Quit);
        assert_eq!(live.key('?'), Command::Help);

        live.key(' ');
        ticks_of(&mut live, 64);
        assert_eq!(live.key('r'), Command::Reset);
        assert_eq!((live.torch.level(), live.now), (0, 0));
    }

    #[test]
    fn the_bar_is_empty_only_when_off_and_full_only_at_the_top() {
        assert_eq!(lit(0), 0);
        assert_eq!(lit(1), 1);
        assert_eq!(lit(LEVELS - 1), BAR);
        assert_eq!(lit(LEVELS), BAR);
        assert_eq!(lit(LEVELS / 2), BAR / 2);
    }

    #[test]
    fn the_status_line_fits_the_terminal() {
        let mut live = Live::default();
        live.key(' ');
        ticks_of(&mut live, 64);
        let status = live.status(60);
        assert!(status.contains(&format!(" {LEVEL}/{LEVELS} ")), "{status}");
        let visible = status.replace("\x1b[33m", "").replace("\x1b[2m", "");
        let visible = visible.replace("\x1b[0m", "");
        assert_eq!(visible.chars().count(), 59);
    }

    #[test]
    fn shows_levels_and_seconds() {
        assert_eq!(Level(0).to_string(), "off");
        assert_eq!(Level(50).to_string(), "level 50/150");
        assert_eq!(format!("{:>#7}", Level(50)), " 50/150");
        assert_eq!(Seconds(0).to_string(), "  0.00 s");
        assert_eq!(Seconds(96).to_string(), "  1.50 s");
        assert_eq!(Seconds(64 * 125 + 1).to_string(), "125.01 s");
    }
}
