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

    /// VREF.CTRLA: ADC0REFSEL picks the internal reference ADC0 uses or
    /// measures.
    pub const VREF_CTRLA: usize = 0x00A0;
    /// VREF.CTRLB: ADC0REFEN keeps ADC0's reference running.
    pub const VREF_CTRLB: usize = 0x00A1;

    pub const RTC_PITCTRLA: usize = 0x0150;
    pub const RTC_PITSTATUS: usize = 0x0151;
    pub const RTC_PITINTFLAGS: usize = 0x0153;

    pub const PORTA_PIN0CTRL: usize = 0x0410;
    pub const PORTB_PIN0CTRL: usize = 0x0430;
    pub const PORTC_PIN0CTRL: usize = 0x0450;

    /// ADC0.CTRLA.
    pub const ADC0_CTRLA: usize = 0x0600;
    /// ADC0.CTRLB: SAMPNUM.
    pub const ADC0_CTRLB: usize = 0x0601;
    /// ADC0.CTRLC: PRESC, REFSEL and SAMPCAP.
    pub const ADC0_CTRLC: usize = 0x0602;
    /// ADC0.CTRLD: SAMPDLY, ASDV and INITDLY.
    pub const ADC0_CTRLD: usize = 0x0603;
    /// ADC0.SAMPCTRL: SAMPLEN.
    pub const ADC0_SAMPCTRL: usize = 0x0605;
    /// ADC0.MUXPOS.
    pub const ADC0_MUXPOS: usize = 0x0606;
    /// ADC0.COMMAND: STCONV.
    pub const ADC0_COMMAND: usize = 0x0608;
    /// ADC0.INTFLAGS: RESRDY.
    pub const ADC0_INTFLAGS: usize = 0x060B;
    /// ADC0.RES, 16-bit: read the low byte first, which latches the high
    /// byte into ADC0.TEMP.
    pub const ADC0_RES: usize = 0x0610;
    /// ADC0.CALIB: DUTYCYC.
    pub const ADC0_CALIB: usize = 0x0616;

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

    /// SIGROW.TEMPSENSE0: the temperature sensor's gain, unsigned.
    pub const SIGROW_TEMPSENSE0: usize = 0x1120;
    /// SIGROW.TEMPSENSE1: the temperature sensor's offset, signed.
    pub const SIGROW_TEMPSENSE1: usize = 0x1121;
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

/// VREF.CTRLA.ADC0REFSEL = 1V1, for the temperature sensor.
pub const VREF_ADC0REFSEL_1V1: u8 = 0x1 << 4;
/// VREF.CTRLA.ADC0REFSEL = 1V5, the reference the voltage reading measures.
pub const VREF_ADC0REFSEL_1V5: u8 = 0x4 << 4;

/// VREF.CTRLB.ADC0REFEN: keep ADC0's reference on between conversions, so
/// a single conversion never catches it starting up (section 18.5.2). A
/// cold 1.5 V reference reads low, which reads as a fuller cell.
pub const VREF_ADC0REFEN: u8 = 0x02;

/// ADC0.CALIB.DUTYCYC = DUTY25, which the datasheet wants at ADC clocks of
/// 1.5 MHz and under (section 30.5.17).
pub const ADC_DUTYCYC_DUTY25: u8 = 0x01;

/// ADC0.CTRLA.ENABLE.
pub const ADC_ENABLE: u8 = 0x01;
/// ADC0.CTRLB.SAMPNUM = ACC1, one conversion per result.
pub const ADC_SAMPNUM_ACC1: u8 = 0x00;
/// ADC0.CTRLB.SAMPNUM = ACC4, four conversions accumulated.
pub const ADC_SAMPNUM_ACC4: u8 = 0x02;
/// ADC0.CTRLC.PRESC = DIV16: 10 MHz / 16 = 625 kHz, inside the 200 kHz to
/// 1.5 MHz the datasheet wants for 10 bits (table 36-26).
pub const ADC_PRESC_DIV16: u8 = 0x03;
/// ADC0.CTRLC.REFSEL = INTREF, the internal reference from VREF.
pub const ADC_REFSEL_INTREF: u8 = 0x00 << 4;
/// ADC0.CTRLC.REFSEL = VDDREF.
pub const ADC_REFSEL_VDDREF: u8 = 0x01 << 4;
/// ADC0.CTRLC.SAMPCAP, the smaller sample capacitor for references of 1 V
/// and up.
pub const ADC_SAMPCAP: u8 = 0x40;
/// ADC0.CTRLD.INITDLY = DLY32: 32 ADC clocks, 51 µs at 625 kHz, over the
/// 32 µs the temperature sensor needs (section 30.3.2.6).
pub const ADC_INITDLY_DLY32: u8 = 0x02 << 5;
/// ADC0.SAMPCTRL.SAMPLEN: 20 more ADC clocks of sampling, 32 µs at 625 kHz,
/// the least the temperature sensor needs (section 30.3.2.6). A count, not a
/// named value, so the ATDF check is only that it fits the field.
pub const ADC_SAMPLEN_32US: u8 = 20;
/// ADC0.MUXPOS = INTREF, the internal reference.
pub const ADC_MUXPOS_INTREF: u8 = 0x1D;
/// ADC0.MUXPOS = TEMPSENSE, the temperature sensor.
pub const ADC_MUXPOS_TEMPSENSE: u8 = 0x1E;
/// ADC0.COMMAND.STCONV.
pub const ADC_STCONV: u8 = 0x01;
/// ADC0.INTFLAGS.RESRDY.
pub const ADC_RESRDY: u8 = 0x01;

/// Entries in the vector table: reset plus 30 interrupts.
pub const VECTORS: usize = 31;
