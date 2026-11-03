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

/// How long a read waits for silence before giving up, in the tenths of a
/// second that `stty time` counts in. Long enough for any reply.
const READ_TIMEOUT: u8 = 10;

/// The same while draining: 100 ms of quiet means nothing else is coming.
const DRAIN_TIMEOUT: u8 = 1;

/// Reads of junk before deciding the line will never go quiet.
const DRAIN_READS: usize = 64;

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
        let mut port = Self { file };
        port.drain().map_err(|e| format!("{path}: {e}"))?;
        Ok(port)
    }

    fn configure(&self, baud: u32, timeout: u8) -> io::Result<()> {
        let status = Command::new("stty")
            // `raw` first: on macOS it's cfmakeraw, which also resets the
            // character size and parity that the later flags set.
            .args(["raw", "-echo", "clocal", "-crtscts", "-ixon", "-ixoff"])
            // UPDI frames are 8 data bits, even parity and two stop bits.
            .args(["cs8", "parenb", "-parodd", "cstopb"])
            // Reads return after `timeout` of silence rather than blocking
            // forever.
            .args(["min", "0", "time", &timeout.to_string()])
            .arg(baud.to_string())
            .stdin(Stdio::from(self.file.try_clone()?))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("stty failed ({status})")))
        }
    }

    /// Sets the port up at [`BAUD`] and throws away anything already waiting
    /// in the adapter, so the next echo starts clean. `send` checks every
    /// echo byte for byte, so one stale byte (noise from the probe landing,
    /// or a run stopped part way) would otherwise fail the first instruction
    /// with a misleading "echo didn't match". avrdude drains on open and after
    /// a double break for the same reason: an FT232B was seen echoing a break
    /// late enough to shift every reply after it by a byte (avrdude issue
    /// #874).
    fn drain(&mut self) -> io::Result<()> {
        self.configure(BAUD, DRAIN_TIMEOUT)?;
        let mut junk = [0; 64];
        for _ in 0..DRAIN_READS {
            if self.file.read(&mut junk)? == 0 {
                return self.configure(BAUD, READ_TIMEOUT);
            }
        }
        Err(io::Error::other(
            "the line never went quiet, so something is driving it",
        ))
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
        self.configure(BREAK_BAUD, READ_TIMEOUT)?;
        self.file.write_all(&[0, 0])?;
        // The break mangles its own echo, so it may come back as two bytes,
        // fewer, or late. Wait for what arrives in time, then drain the rest.
        let mut echo = [0; 2];
        let _ = self.read_full(&mut echo);
        self.drain()
    }
}
