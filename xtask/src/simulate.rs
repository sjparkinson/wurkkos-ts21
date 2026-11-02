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
//! With no script it runs live in the terminal, in real time, with the keys
//! in `HELP`. Terminals don't say when a key goes up, which a hold needs, so
//! `h` holds the switch down until pressed again.

use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

use ts21::TICKS_PER_SECOND;
use ts21::button::{Event, HOLD_TICKS, RELEASE_TICKS};
use ts21::ramp::{self, LEVELS};
use ts21::torch::Torch;

use crate::terminal::{Key, Terminal};

/// How long a scripted click holds the switch down (about 60 ms).
const PRESS_TICKS: u16 = 4;
/// The pause between scripted clicks in a run, well inside `RELEASE_TICKS`.
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
}

/// Runs a script and prints what happened.
pub fn script(words: &[&str]) -> Result<(), String> {
    for line in run(&parse(words)?) {
        println!("{line}");
    }
    Ok(())
}

pub fn parse(words: &[&str]) -> Result<Vec<Action>, String> {
    words
        .iter()
        .map(|&word| parse_word(word).map_err(|e| format!("`{word}`: {e}")))
        .collect()
}

fn parse_word(word: &str) -> Result<Action, String> {
    if let Some(duration) = word.strip_prefix("wait:") {
        return Ok(Action::Wait(ticks(duration)?));
    }
    let (head, duration) = match word.split_once(':') {
        Some((head, duration)) => (head, Some(duration)),
        None => (word, None),
    };
    let split = head.len().saturating_sub(1);
    let (count, kind) = (&head[..split], &head[split..]);
    let count: u8 = match count.parse() {
        Ok(n) if n > 0 => n,
        _ => return Err("expected a count of 1 or more, like 1C, 2H or wait:1s".into()),
    };
    match (kind, duration) {
        ("C" | "c", None) => Ok(Action::Clicks(count)),
        ("C" | "c", Some(_)) => Err("clicks don't take a duration; holds do (1H:2s)".into()),
        ("H" | "h", duration) => {
            let ticks = duration.map_or(Ok(DEFAULT_HOLD_TICKS), ticks)?;
            if ticks <= HOLD_TICKS {
                return Err(format!(
                    "that's a click; a hold needs longer than {} ms",
                    ms(u32::from(HOLD_TICKS))
                ));
            }
            Ok(Action::Hold {
                clicks: count - 1,
                ticks,
            })
        }
        _ => Err("expected C (clicks), H (hold) or wait:".into()),
    }
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

/// The switch, one sample per tick, for a whole script.
fn samples(actions: &[Action]) -> Vec<bool> {
    let mut out = Vec::new();
    for &action in actions {
        match action {
            Action::Clicks(n) => {
                out.extend(clicks(n));
                out.extend(span(false, SETTLE_TICKS - GAP_TICKS));
            }
            Action::Hold { clicks: n, ticks } => {
                out.extend(clicks(n));
                out.extend(span(true, ticks));
                out.extend(span(false, SETTLE_TICKS));
            }
            Action::Wait(ticks) => out.extend(span(false, ticks)),
        }
    }
    out
}

/// `n` quick clicks, each a press then a short gap.
fn clicks(n: u8) -> impl Iterator<Item = bool> {
    (0..n).flat_map(|_| span(true, PRESS_TICKS).chain(span(false, GAP_TICKS)))
}

fn span(pressed: bool, ticks: u16) -> impl Iterator<Item = bool> + Clone {
    (0..ticks).map(move |_| pressed)
}

/// Runs a script on a fresh torch and returns the log, ending with where
/// the light was left.
pub fn run(actions: &[Action]) -> Vec<String> {
    let mut torch = Torch::new();
    let mut log = Log::new();
    let mut lines = Vec::new();
    let samples = samples(actions);
    for (now, &pressed) in samples.iter().enumerate() {
        let event = torch.tick(pressed);
        lines.extend(log.note(now as u32, event, torch.level()));
    }
    lines.push(format!(
        "{}  ends {}",
        seconds(samples.len() as u32),
        describe(torch.level())
    ));
    lines
}

/// Turns ticks into log lines: one per click, one per hold (when it ends,
/// not every tick it's held), and one for any change of level that no
/// click or hold explains.
pub struct Log {
    level: u8,
    /// The latest `Event::Hold` ticks and the level before the hold began.
    hold: Option<(u16, u8)>,
}

impl Log {
    pub fn new() -> Self {
        Self {
            level: 0,
            hold: None,
        }
    }

    pub fn note(&mut self, now: u32, event: Option<Event>, level: u8) -> Option<String> {
        let before = self.level;
        self.level = level;
        let (what, before) = match event {
            Some(Event::Click(n)) => (format!("{n}C"), before),
            Some(Event::Hold { ticks, .. }) => {
                let start = self.hold.map_or(before, |(_, start)| start);
                self.hold = Some((ticks, start));
                return None;
            }
            Some(Event::Release { clicks }) => {
                let (ticks, start) = self.hold.take().unwrap_or((0, before));
                let held = u32::from(HOLD_TICKS) + u32::from(ticks) + 1;
                (
                    format!("{}H, held {}", clicks + 1, seconds(held).trim_start()),
                    start,
                )
            }
            None if self.hold.is_none() && level != before => (String::new(), before),
            None => return None,
        };
        let unchanged = if level == before { " (no change)" } else { "" };
        Some(format!(
            "{}  {what:<16} → {}{unchanged}",
            seconds(now),
            describe(level)
        ))
    }
}

/// `"  1.50 s"` for 96 ticks.
pub fn seconds(ticks: u32) -> String {
    let per = u32::from(TICKS_PER_SECOND);
    format!("{:>3}.{:02} s", ticks / per, ticks % per * 100 / per)
}

pub fn describe(level: u8) -> String {
    match level {
        0 => "off".into(),
        level => format!("level {level}/{LEVELS}"),
    }
}

fn ms(ticks: u32) -> u32 {
    ticks * 1000 / u32::from(TICKS_PER_SECOND)
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
    let mut live = Live::new();
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
struct Live {
    torch: Torch,
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
    fn new() -> Self {
        Self {
            torch: Torch::new(),
            log: Log::new(),
            now: 0,
            queue: VecDeque::new(),
            held: false,
            pressed: false,
        }
    }

    fn key(&mut self, Key { key, ctrl }: Key) -> Command {
        match (key.to_ascii_lowercase(), ctrl) {
            ('c', true) | ('q', false) => return Command::Quit,
            (' ', false) => self.click(1),
            (digit @ '1'..='9', false) => self.click(digit as u8 - b'0'),
            ('h', false) => self.held = !self.held,
            ('?', false) => return Command::Help,
            ('r', false) => {
                *self = Self::new();
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
        let event = self.torch.tick(self.pressed);
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
        let level_text = match level {
            0 => "off".into(),
            level => format!("{level}/{LEVELS}"),
        };
        let switch = match (self.pressed, self.held) {
            (true, true) => "held",
            (true, _) => "down",
            (false, _) => "up",
        };
        let text = format!(
            " {level_text:>7}  linear {:>3}  FET {:>3}  switch {switch:<4}  {:?}",
            output.linear,
            output.fet,
            self.torch.ui()
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

    fn words(script: &str) -> Vec<&str> {
        script.split_whitespace().collect()
    }

    fn ends(script: &str) -> String {
        run(&parse(&words(script)).unwrap()).pop().unwrap()
    }

    #[test]
    fn parses_clicks_holds_and_waits() {
        assert_eq!(
            parse(&words("1C 3c 1H 2H:500ms wait:1.5s wait:2s")),
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
            assert!(parse(&[word]).is_err(), "{word:?} parsed");
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
        assert!(ends("1C").ends_with(&format!("ends level {LEVEL}/{LEVELS}")));
        assert!(ends("1C 1C").ends_with("ends off"));
    }

    #[test]
    fn everything_else_leaves_it_alone() {
        assert!(ends("2C 1H 3H:2s").ends_with("ends off"));
        assert!(ends("1C 2C 1H").ends_with(&format!("ends level {LEVEL}/{LEVELS}")));
    }

    #[test]
    fn logs_one_line_per_click_and_per_hold() {
        let lines = run(&parse(&words("1C 1H:1s 2C")).unwrap());
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(lines[0].contains("1C") && lines[0].ends_with(&describe(LEVEL)));
        assert!(lines[1].contains("1H, held 1.00 s") && lines[1].ends_with("(no change)"));
        assert!(lines[2].contains("2C") && lines[2].ends_with("(no change)"));
    }

    #[test]
    fn a_held_switch_counts_its_holds_ticks() {
        let mut log = Log::new();
        for ticks in 0..10 {
            let hold = Event::Hold { clicks: 1, ticks };
            assert_eq!(log.note(u32::from(ticks), Some(hold), 0), None);
        }
        let line = log.note(10, Some(Event::Release { clicks: 1 }), 0).unwrap();
        let held = seconds(u32::from(HOLD_TICKS) + 10);
        assert!(
            line.contains(&format!("2H, held {}", held.trim_start())),
            "{line}"
        );
    }

    #[test]
    fn logs_a_change_of_level_with_no_event() {
        let mut log = Log::new();
        assert!(log.note(0, None, 30).unwrap().ends_with("level 30/150"));
        assert_eq!(log.note(1, None, 30), None);
    }

    fn press(key: char) -> Key {
        Key { key, ctrl: false }
    }

    /// Ticks a live session `n` times and keeps the log.
    fn ticks_of(live: &mut Live, n: u16) -> Vec<String> {
        (0..n).filter_map(|_| live.tick()).collect()
    }

    #[test]
    fn space_clicks_and_digits_click_that_many_times() {
        let mut live = Live::new();
        live.key(press(' '));
        let lines = ticks_of(&mut live, 64);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("1C") && live.torch.level() == LEVEL);

        live.key(press('3'));
        let lines = ticks_of(&mut live, 64);
        assert!(lines[0].contains("3C") && live.torch.level() == LEVEL);
    }

    #[test]
    fn h_holds_the_switch_until_pressed_again() {
        let mut live = Live::new();
        live.key(press('h'));
        assert!(ticks_of(&mut live, 64).is_empty());
        assert!(live.pressed);
        live.key(press('h'));
        let lines = ticks_of(&mut live, 64);
        assert!(lines[0].contains("1H, held 1.00 s"), "{lines:?}");
        assert!(!live.pressed);
    }

    #[test]
    fn q_and_ctrl_c_quit_and_r_starts_again() {
        let mut live = Live::new();
        assert_eq!(live.key(press('q')), Command::Quit);
        let ctrl_c = Key {
            key: 'c',
            ctrl: true,
        };
        assert_eq!(live.key(ctrl_c), Command::Quit);
        assert_eq!(live.key(press('?')), Command::Help);

        live.key(press(' '));
        ticks_of(&mut live, 64);
        assert_eq!(live.key(press('r')), Command::Reset);
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
        let mut live = Live::new();
        live.key(press(' '));
        ticks_of(&mut live, 64);
        let status = live.status(60);
        assert!(status.contains(&format!(" {LEVEL}/{LEVELS} ")), "{status}");
        let visible = status.replace("\x1b[33m", "").replace("\x1b[2m", "");
        let visible = visible.replace("\x1b[0m", "");
        assert_eq!(visible.chars().count(), 59);
    }

    #[test]
    fn formats_time_in_seconds() {
        assert_eq!(seconds(0), "  0.00 s");
        assert_eq!(seconds(96), "  1.50 s");
        assert_eq!(seconds(64 * 125 + 1), "125.01 s");
    }
}
