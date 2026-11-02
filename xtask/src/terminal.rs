//! Just enough terminal for `cargo xtask simulate`: keys one at a time,
//! without echo, and key releases where the terminal can report them.
//!
//! Raw mode properly means termios, and termios means libc or `unsafe`, so
//! this does what `serial.rs` does and asks `stty`, which acts on the
//! terminal it inherits as stdin. `Drop` puts the settings back, panic or
//! not. `-isig` means Ctrl-C arrives as a key rather than killing the
//! process with the terminal still raw.
//!
//! Terminals don't normally say when a key goes up. Those that speak the
//! kitty keyboard protocol (kitty, Ghostty, WezTerm, foot, recent iTerm2) do
//! once asked, which lets the space bar behave like the switch. `open` asks
//! whether the protocol's there by sending its query followed by a primary
//! device attributes request, which every terminal answers: if the kitty
//! reply comes first, it's supported. See
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.

use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// The kitty protocol's progressive enhancement flags: disambiguate escape
/// codes (1), report event types (2), report alternate keys (4), and report
/// all keys as escape codes (8), without which ordinary keys have no release.
const KITTY_FLAGS: u8 = 1 | 2 | 4 | 8;

/// How long to wait for the terminal to answer the kitty query.
const QUERY_TIMEOUT: Duration = Duration::from_millis(300);

/// The longest control sequence worth reading, `ESC [` included.
const MAX_SEQUENCE: usize = 32;

/// The kitty keyboard protocol's modifier bits, once 1 is taken off.
const SHIFT: u32 = 1;
const CTRL: u32 = 4;

pub struct Terminal {
    saved: String,
    input: Receiver<u8>,
    decoder: Decoder,
    /// Whether the terminal reports key releases.
    pub kitty: bool,
    /// Columns, for keeping the status line to one row.
    pub width: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key {
        key: char,
        ctrl: bool,
        kind: Kind,
    },
    /// The terminal answered the kitty protocol query.
    KittyFlags,
    /// The terminal answered the device attributes request.
    Attributes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Press,
    Repeat,
    Release,
}

impl Terminal {
    pub fn open() -> Result<Self, String> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("simulate needs a terminal to read keys from; \
                        pass a script instead, like `cargo xtask simulate 1C 1H 2C`"
                .into());
        }
        let saved = stty(&["-g"])?.trim().to_string();
        // Some pseudo-terminals report 0 columns.
        let width = stty(&["size"])
            .ok()
            .and_then(|size| size.split_whitespace().nth(1)?.parse().ok())
            .filter(|&columns| columns > 0)
            .unwrap_or(80);

        let (tx, input) = mpsc::channel();
        thread::spawn(move || {
            for byte in io::stdin().lock().bytes() {
                let Ok(byte) = byte else { break };
                if tx.send(byte).is_err() {
                    break;
                }
            }
        });

        // From here on, `Drop` puts the terminal back.
        let mut terminal = Self {
            saved,
            input,
            decoder: Decoder::default(),
            kitty: false,
            width,
        };
        stty(&["-icanon", "-echo", "-isig", "min", "1"])?;
        terminal.write("\x1b[?25l");
        terminal.kitty = terminal.detect_kitty();
        if terminal.kitty {
            terminal.write(&format!("\x1b[>{KITTY_FLAGS}u"));
        }
        Ok(terminal)
    }

    fn detect_kitty(&mut self) -> bool {
        self.write("\x1b[?u\x1b[c");
        let deadline = Instant::now() + QUERY_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.input.recv_timeout(left) {
                Ok(byte) => match self.decoder.feed(byte) {
                    Some(Input::KittyFlags) => return true,
                    Some(Input::Attributes) => return false,
                    _ => {}
                },
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Everything typed since the last call, or `None` once stdin closes.
    pub fn poll(&mut self) -> Option<Vec<Input>> {
        let mut inputs = Vec::new();
        loop {
            match self.input.try_recv() {
                Ok(byte) => inputs.extend(self.decoder.feed(byte)),
                Err(TryRecvError::Empty) => return Some(inputs),
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }

    pub fn write(&self, text: &str) {
        let mut out = io::stdout().lock();
        // Nothing useful to do if the terminal's gone.
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.kitty {
            self.write("\x1b[<u");
        }
        self.write("\r\x1b[2K\x1b[?25h");
        if let Err(e) = stty(&[&self.saved]) {
            eprintln!("couldn't put the terminal back ({e}); run `stty sane`");
        }
    }
}

fn stty(args: &[&str]) -> Result<String, String> {
    let output = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("couldn't run stty: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "stty {} failed ({})",
            args.join(" "),
            output.status
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("stty: {e}"))
}

/// Bytes from the terminal in, keys out. Escape sequences can arrive split
/// across reads, so it holds on to a partial one.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub fn feed(&mut self, byte: u8) -> Option<Input> {
        match (self.pending.as_slice(), byte) {
            ([], 0x1B) | ([0x1B], b'[') => {
                self.pending.push(byte);
                None
            }
            ([], _) => plain(byte),
            // Escape then anything but `[`: Alt and a key, or Esc on its own.
            ([0x1B], _) => {
                self.pending.clear();
                plain(byte)
            }
            // A final byte ends the sequence. One too long to be anything
            // this reads is dropped whole, rather than its tail coming out
            // as keys.
            _ if (0x40..=0x7E).contains(&byte) => {
                let mut sequence = std::mem::take(&mut self.pending);
                if sequence.len() >= MAX_SEQUENCE {
                    return None;
                }
                sequence.push(byte);
                csi(&sequence[2..])
            }
            _ => {
                if self.pending.len() < MAX_SEQUENCE {
                    self.pending.push(byte);
                }
                None
            }
        }
    }
}

/// A key as a terminal sends it without the kitty protocol: no releases,
/// and Ctrl with a letter as a control code.
fn plain(byte: u8) -> Option<Input> {
    let (key, ctrl) = match byte {
        b'\t' | b'\n' | b'\r' => (char::from(byte), false),
        1..=26 => (char::from(byte - 1 + b'a'), true),
        b' '..=b'~' => (char::from(byte), false),
        _ => return None,
    };
    Some(Input::Key {
        key,
        ctrl,
        kind: Kind::Press,
    })
}

/// A control sequence, after `ESC [` and up to and including its final byte.
fn csi(body: &[u8]) -> Option<Input> {
    let (&last, params) = body.split_last()?;
    let params = std::str::from_utf8(params).ok()?;
    match (last, params.strip_prefix('?')) {
        (b'u', Some(_)) => Some(Input::KittyFlags),
        (b'c', Some(_)) => Some(Input::Attributes),
        (b'u', None) => kitty_key(params),
        _ => None,
    }
}

/// `code[:shifted[:base]][;modifiers[:event]]`, from the kitty protocol.
fn kitty_key(params: &str) -> Option<Input> {
    let (codes, rest) = params.split_once(';').unwrap_or((params, ""));
    let (mods, event) = rest.split_once(':').unwrap_or((rest, ""));
    let number = |s: &str| -> Option<u32> {
        if s.is_empty() {
            Some(1)
        } else {
            s.parse().ok()
        }
    };
    let mods = number(mods)?.saturating_sub(1);
    let mut codes = codes.split(':');
    let code: u32 = codes.next()?.parse().ok()?;
    let shifted = codes.next().filter(|s| !s.is_empty());
    let code = match shifted {
        Some(shifted) if mods & SHIFT != 0 => shifted.parse().ok()?,
        _ => code,
    };
    let kind = match number(event)? {
        1 => Kind::Press,
        2 => Kind::Repeat,
        3 => Kind::Release,
        _ => return None,
    };
    Some(Input::Key {
        key: char::from_u32(code)?,
        ctrl: mods & CTRL != 0,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<Input> {
        let mut decoder = Decoder::default();
        bytes.iter().filter_map(|&b| decoder.feed(b)).collect()
    }

    fn key(key: char, ctrl: bool, kind: Kind) -> Input {
        Input::Key { key, ctrl, kind }
    }

    #[test]
    fn reads_plain_keys_as_presses() {
        assert_eq!(
            decode(b"q 3\x03"),
            [
                key('q', false, Kind::Press),
                key(' ', false, Kind::Press),
                key('3', false, Kind::Press),
                key('c', true, Kind::Press),
            ]
        );
    }

    #[test]
    fn reads_kitty_presses_repeats_and_releases() {
        assert_eq!(
            decode(b"\x1b[32u\x1b[32;1:2u\x1b[32;1:3u"),
            [
                key(' ', false, Kind::Press),
                key(' ', false, Kind::Repeat),
                key(' ', false, Kind::Release),
            ]
        );
    }

    #[test]
    fn reads_kitty_modifiers_and_shifted_keys() {
        assert_eq!(
            decode(b"\x1b[99;5u\x1b[47:63;2u\x1b[104;2u"),
            [
                key('c', true, Kind::Press),
                key('?', false, Kind::Press),
                key('h', false, Kind::Press),
            ]
        );
    }

    #[test]
    fn reads_the_answers_to_its_queries() {
        assert_eq!(
            decode(b"\x1b[?15u\x1b[?62;22c"),
            [Input::KittyFlags, Input::Attributes]
        );
    }

    #[test]
    fn skips_sequences_it_doesnt_know() {
        assert_eq!(
            decode(b"\x1b[A\x1b[1;5D\x1bxq"),
            [key('x', false, Kind::Press), key('q', false, Kind::Press)]
        );
    }

    #[test]
    fn drops_a_runaway_sequence_whole() {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend([b'1'; 40]);
        bytes.extend(b"uq");
        assert_eq!(decode(&bytes), [key('q', false, Kind::Press)]);
    }
}
