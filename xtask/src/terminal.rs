//! Just enough terminal for `cargo xtask simulate`: keys one at a time,
//! without echo.
//!
//! Raw mode properly means termios, and termios means libc or `unsafe`, so
//! this does what `serial.rs` does and asks `stty`, which acts on the
//! terminal it inherits as stdin. `Drop` puts the settings back, panic or
//! not. `-isig` means Ctrl-C arrives as a key rather than killing the
//! process with the terminal still raw.

use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

/// Ctrl-C, as it arrives with `-isig`.
pub const CTRL_C: char = '\x03';

pub struct Terminal {
    saved: String,
    input: Receiver<u8>,
    decoder: Decoder,
    /// Columns, for keeping the status line to one row.
    pub width: usize,
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
        let terminal = Self {
            saved,
            input,
            decoder: Decoder::default(),
            width,
        };
        stty(&["-icanon", "-echo", "-isig", "min", "1"])?;
        terminal.write("\x1b[?25l");
        Ok(terminal)
    }

    /// Every key typed since the last call, or `None` once stdin closes.
    pub fn poll(&mut self) -> Option<Vec<char>> {
        let mut keys = Vec::new();
        loop {
            match self.input.try_recv() {
                Ok(byte) => keys.extend(self.decoder.feed(byte)),
                Err(TryRecvError::Empty) => return Some(keys),
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

/// Bytes from the terminal in, keys out. Arrow keys and the like arrive as
/// escape sequences (`ESC [ 1 ; 5 D`), which it swallows whole so their
/// digits don't come out as clicks. A sequence can arrive split across
/// reads, so it remembers where it's up to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Decoder {
    #[default]
    Ground,
    /// After `ESC`.
    Escape,
    /// Inside `ESC [ ...`, waiting for its final byte.
    Sequence,
}

impl Decoder {
    fn feed(&mut self, byte: u8) -> Option<char> {
        let (next, key) = match (*self, byte) {
            (Self::Ground, 0x1B) => (Self::Escape, None),
            (Self::Escape, b'[') => (Self::Sequence, None),
            // Escape then anything but `[` is Alt and a key, or Esc alone.
            (Self::Ground | Self::Escape, _) => (Self::Ground, key(byte)),
            (Self::Sequence, 0x40..=0x7E) => (Self::Ground, None),
            (Self::Sequence, _) => (Self::Sequence, None),
        };
        *self = next;
        key
    }
}

/// Printable ASCII, and Ctrl-C.
fn key(byte: u8) -> Option<char> {
    let key = char::from(byte);
    (key.is_ascii_graphic() || key == ' ' || key == CTRL_C).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> String {
        let mut decoder = Decoder::default();
        bytes.iter().filter_map(|&b| decoder.feed(b)).collect()
    }

    #[test]
    fn reads_printable_keys_and_ctrl_c() {
        assert_eq!(decode(b"q 3\x03\x04\t"), "q 3\x03");
    }

    #[test]
    fn swallows_escape_sequences_whole() {
        assert_eq!(decode(b"\x1b[A\x1b[1;5D\x1bxq"), "xq");
    }

    #[test]
    fn swallows_a_sequence_split_across_reads() {
        let mut decoder = Decoder::default();
        let keys: String = [b"\x1b[1".as_slice(), b";5", b"Dq"]
            .concat()
            .into_iter()
            .filter_map(|b| decoder.feed(b))
            .collect();
        assert_eq!(keys, "q");
    }
}
