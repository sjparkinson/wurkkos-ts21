//! ATtiny1616 register addresses and bitfield values, typed in by hand from
//! the ATtiny1617 ATDF in `vendor/` (the 1616 is the same die in a 20-pin
//! package).
//!
//! Only plain constants live here, so the file compiles anywhere. The
//! firmware uses it from `src/hw.rs` and `src/rt.rs`, and xtask's tests pull
//! it in by path and check every constant against the ATDF
//! (`xtask/src/atdf.rs`). Add a check there for anything you add here; an
//! unchecked constant shows up as dead code in the xtask build.
//!
//! Field values are already shifted into place, so they can be OR'd
//! straight into the register.

/// Data-space addresses.
pub mod addr {
    /// CPU.CCP. Below 0x40, so its I/O address is the same number and
    /// `out` can reach it.
    pub const CPU_CCP: usize = 0x0034;
    /// CPU.SREG, also within `out` range.
    pub const CPU_SREG: usize = 0x003F;

    pub const VPORTA_IN: usize = 0x0002;
    pub const VPORTB_DIR: usize = 0x0004;

    pub const CLKCTRL_MCLKCTRLB: usize = 0x0061;

    pub const RTC_PITCTRLA: usize = 0x0150;
    pub const RTC_PITSTATUS: usize = 0x0151;
    pub const RTC_PITINTFLAGS: usize = 0x0153;

    pub const PORTA_PIN0CTRL: usize = 0x0410;
    pub const PORTB_PIN0CTRL: usize = 0x0430;
    pub const PORTC_PIN0CTRL: usize = 0x0450;

    pub const TCA0_CTRLA: usize = 0x0A00;
    pub const TCA0_CTRLB: usize = 0x0A01;
    /// 16-bit: write through `write16`, low byte first.
    pub const TCA0_CNT: usize = 0x0A20;
    /// 16-bit.
    pub const TCA0_PERBUF: usize = 0x0A36;
    /// 16-bit.
    pub const TCA0_CMP0BUF: usize = 0x0A38;
    /// 16-bit.
    pub const TCA0_CMP1BUF: usize = 0x0A3A;
}

/// CPU.CCP: the key that unlocks I/O registers behind configuration change
/// protection.
pub const CPU_CCP_IOREG: u8 = 0xD8;

/// CLKCTRL.MCLKCTRLB.PEN, prescaler enable.
pub const CLKCTRL_PEN: u8 = 0x01;
/// CLKCTRL.MCLKCTRLB.PDIV = 2X.
pub const CLKCTRL_PDIV_2X: u8 = 0x00;

/// PORTx.PINnCTRL.PULLUPEN.
pub const PORT_PULLUPEN: u8 = 0x08;
/// PORTx.PINnCTRL.ISC = BOTHEDGES.
pub const PORT_ISC_BOTHEDGES: u8 = 0x01;

/// TCA0.CTRLA.ENABLE (single mode).
pub const TCA_ENABLE: u8 = 0x01;
/// TCA0.CTRLA.CLKSEL = DIV1 (single mode).
pub const TCA_CLKSEL_DIV1: u8 = 0x00;
/// TCA0.CTRLB.CMP0EN (single mode).
pub const TCA_CMP0EN: u8 = 0x10;
/// TCA0.CTRLB.CMP1EN (single mode).
pub const TCA_CMP1EN: u8 = 0x20;
/// TCA0.CTRLB.WGMODE = DSBOTTOM (single mode).
pub const TCA_WGMODE_DSBOTTOM: u8 = 0x07;

/// RTC.PITCTRLA.PITEN.
pub const RTC_PITEN: u8 = 0x01;
/// RTC.PITCTRLA.PERIOD = CYC512.
pub const RTC_PERIOD_CYC512: u8 = 0x08 << 3;
/// RTC.PITSTATUS.CTRLBUSY.
pub const RTC_CTRLBUSY: u8 = 0x01;
/// RTC.PITINTFLAGS.PI.
pub const RTC_PI: u8 = 0x01;

/// Entries in the vector table: reset plus 30 interrupts.
pub const VECTORS: usize = 31;
