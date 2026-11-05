//! ATtiny1616 register addresses and bitfield values, generated at build time
//! from Microchip's ATDF in `vendor/` by `build.rs`, with `generate` in
//! `xtask/src/atdf.rs`. Nothing in here that the ATDF can give us is typed by
//! hand. The generated file is `regs.rs` in the package's `OUT_DIR`.
//!
//! Only plain constants live here, so the file compiles anywhere. It's a
//! module of the library, where `selftest` checks the fuse fields with it.
//! The firmware uses it from `src/hw.rs` and `src/rt.rs`, and xtask from its
//! simulator and UPDI programmer. A constant nothing uses costs nothing.
//!
//! The names follow Microchip's C headers, so each is the ATDF's own:
//!
//! - `addr::RTC_PITCTRLA`, an instance and a register: a data-space address.
//! - `mem::EEPROM_START`, `_SIZE`, `_END` and `_PAGE`: a memory segment.
//! - `RTC_PITEN`, a module and a bitfield: the field's mask.
//! - `RTC_PERIOD_CYC512`, a module, a bitfield and a named value: the value,
//!   already shifted into place, so it can be OR'd straight into the register.
//! - `VECTOR_RTC_PIT`, an instance and an interrupt, and `VECTORS`, how many
//!   entries the vector table has.
//!
//! Masks and values are `u8` unless they don't fit. The ATDF's reset values
//! aren't here: it disagrees with the datasheet on some of them.

include!(concat!(env!("OUT_DIR"), "/regs.rs"));

/// ADC0.SAMPCTRL.SAMPLEN: 20 more ADC clocks of sampling, 32 µs at 625 kHz,
/// the least the temperature sensor needs (section 30.3.2.6). Hand-written,
/// since it's a count we chose rather than a value the ATDF names.
pub const ADC_SAMPLEN_32US: u8 = 20;
const _: () = assert!(ADC_SAMPLEN_32US & !ADC_SAMPLEN == 0);
