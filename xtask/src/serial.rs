//! A serial UPDI adapter on a Unix serial port, set up with `stty`.
//!
//! Configuring a port properly means termios, which means libc or a serial
//! crate. `stty` is on every Mac and Linux box already and does the same
//! job: we open the port, hand the open file to `stty` as its stdin, and the
//! settings stick for as long as we hold it open. Windows isn't supported.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use crate::updi::Wire;

/// What pymcuprog and avrdude both default to, and comfortably inside what
/// UPDI manages on the chip's 4 MHz reset clock.
const BAUD: u32 = 115_200;

/// Slow enough that one 0x00 frame holds the line low for longer than a
/// UPDI break needs.
const BREAK_BAUD: u32 = 300;

pub struct Port {
    file: File,
}

impl Port {
    pub fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))?;
        let port = Self { file };
        port.configure(BAUD).map_err(|e| format!("{path}: {e}"))?;
        Ok(port)
    }

    fn configure(&self, baud: u32) -> io::Result<()> {
        let status = Command::new("stty")
            // `raw` first: on macOS it's cfmakeraw, which also resets the
            // character size and parity that the later flags set.
            .args(["raw", "-echo", "clocal", "-crtscts", "-ixon", "-ixoff"])
            // UPDI frames are 8 data bits, even parity and two stop bits.
            .args(["cs8", "parenb", "-parodd", "cstopb"])
            // Reads return after 1 s of silence rather than blocking forever.
            .args(["min", "0", "time", "10"])
            .arg(baud.to_string())
            .stdin(Stdio::from(self.file.try_clone()?))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("stty failed ({status})")))
        }
    }

    fn read_full(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.file.read(&mut buf[filled..])? {
                0 => return Err(io::ErrorKind::TimedOut.into()),
                n => filled += n,
            }
        }
        Ok(())
    }
}

impl Wire for Port {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        let mut echo = vec![0; bytes.len()];
        self.read_full(&mut echo).map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut => io::Error::other(
                "nothing echoed back. A serial UPDI adapter joins TX and RX, so \
                 every byte sent should come straight back; is this one?",
            ),
            _ => e,
        })?;
        if echo != bytes {
            return Err(io::Error::other(
                "the echo didn't match what was sent, so something else is on the line",
            ));
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.read_full(buf)
    }

    fn double_break(&mut self) -> io::Result<()> {
        self.configure(BREAK_BAUD)?;
        self.file.write_all(&[0, 0])?;
        // Whatever comes back is mangled by the break; drain it and move on.
        let mut echo = [0; 2];
        let _ = self.read_full(&mut echo);
        self.configure(BAUD)
    }
}
