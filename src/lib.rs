//! Everything that decides what the torch does, with no hardware in sight.
//!
//! This crate is `no_std` so the same code runs on the ATtiny1616 and in
//! `cargo test` on your laptop. The firmware binary (`src/main.rs`) is a thin
//! loop that feeds switch samples in and writes PWM values out.

#![no_std]

pub mod battery;
pub mod button;
pub mod ramp;
pub mod temp;
pub mod torch;
pub mod ui;

/// How often the firmware samples the switch and steps the UI. The RTC's
/// periodic interrupt timer divides its 32.768 kHz clock by 512.
pub const TICKS_PER_SECOND: u16 = 64;
