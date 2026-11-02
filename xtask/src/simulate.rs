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
//!
//! With no script it runs live in the terminal, in real time, with the keys
//! in `HELP`. Terminals don't say when a key goes up, which a hold needs, so
//! `h` holds the switch down until pressed again.

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use crate::world::{Rig, World};
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
    words
        .iter()
        .map(|word| word.parse().map_err(|e| format!("`{word}`: {e}")))
        .collect()
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
    let mut rig = Rig::default();
    let mut log = Log::default();
    let mut lines = Vec::new();
    let samples = samples(actions);
    for (now, sample) in (0..).zip(&samples) {
        let event = rig.tick(sample.pressed, &sample.world);
        lines.extend(log.note(now, event, rig.level()));
    }
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
/// click or hold explains.
#[derive(Default)]
struct Log {
    level: u8,
    /// The tick the press that's being held started on, and the level
    /// before it.
    hold: Option<(u32, u8)>,
}

impl Log {
    fn note(&mut self, now: u32, event: Option<Event>, level: u8) -> Option<String> {
        let before = std::mem::replace(&mut self.level, level);
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
        let line = self.log.note(self.now, event, self.torch.level());
        self.now += 1;
        line
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
        ] {
            assert!(word.parse::<Action>().is_err(), "{word:?} parsed");
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
            assert_eq!(log.note(now, Some(hold), 0), None);
        }
        let now = u32::from(HOLD_TICKS) + 10;
        let line = log
            .note(now, Some(Event::Release { clicks: 1 }), 0)
            .unwrap();
        let held = Seconds(now).to_string();
        assert!(
            line.contains(&format!("2H, held {}", held.trim_start())),
            "{line}"
        );
    }

    #[test]
    fn logs_a_change_of_level_with_no_event() {
        let mut log = Log::default();
        assert!(log.note(0, None, 30).unwrap().ends_with("level 30/150"));
        assert_eq!(log.note(1, None, 30), None);
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
