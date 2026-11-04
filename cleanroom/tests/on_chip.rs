//! Runs the built hex on an AVR CPU core with a fake chip around it, so the
//! start-up code, the handler, the sleep paths and the register sequences run for
//! real. The fake is deliberately strict: anything the datasheet rules out is an
//! error, and registers it has never heard of are too.
//!
//! The CPU core is the one in xtask, which knows nothing about torches. The chip
//! around it is written here, from the ATDF names, and models only what this
//! firmware touches. `./hex.sh` builds the hex first and runs this.

#[allow(dead_code, clippy::all)]
#[path = "../../xtask/src/avr.rs"]
mod avr;

include!(concat!(env!("OUT_DIR"), "/regs.rs"));
use addr::*;
use avr::{Bus, Cpu};
use bits::*;
use ts21_clean::ramp::{self, FLOOR, TOP};
use ts21_clean::sensors::{battery_counts, heat_counts};

const FLASH_SIZE: usize = 16 * 1024;
const SRAM: std::ops::Range<u16> = 0x3800..0x4000;
const MAPPED: u16 = 0x8000;
const GAIN: u8 = 160;
const OFFSET: i8 = -10;
const FIRST_MEMORY: u8 = 50;
/// Enough to run a tick several times over, so a hang fails rather than spins.
const STEPS_PER_TICK: u32 = 100_000;

fn load_hex() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/ts21-clean.hex");
    let text = std::fs::read_to_string(path).expect("run ./hex.sh first");
    let mut flash = vec![0xFF; FLASH_SIZE];
    for line in text.lines() {
        let bytes: Vec<u8> = (1..line.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&line[at..at + 2], 16).unwrap())
            .collect();
        if bytes[3] == 0 {
            let address = usize::from(bytes[1]) << 8 | usize::from(bytes[2]);
            let data = &bytes[4..4 + usize::from(bytes[0])];
            flash[address..address + data.len()].copy_from_slice(data);
        }
    }
    flash
}

struct Chip {
    flash: Vec<u8>,
    sram: Vec<u8>,
    io: Vec<u8>,
    pressed: bool,
    cell_mv: u32,
    kelvin: u16,
    adc_result: Option<u16>,
    pit_flag: bool,
    porta_flag: bool,
    ccp_armed: bool,
    low_byte_at: Option<u16>,
    /// TCA0's LUPD: buffers don't load while it's set.
    locked: bool,
    active: ramp::Pwm,
    ticks: u32,
    ticks_since_wdr: u32,
    /// In power-down with nothing to wake it but the thumb.
    parked: bool,
    wakes: u32,
}

impl Chip {
    fn new() -> Self {
        Self {
            flash: load_hex(),
            sram: vec![0; SRAM.len()],
            io: vec![0; 0x1400],
            pressed: false,
            cell_mv: 3700,
            kelvin: 295,
            adc_result: None,
            pit_flag: false,
            porta_flag: false,
            ccp_armed: false,
            low_byte_at: None,
            locked: false,
            active: ramp::pwm(0),
            ticks: 0,
            ticks_since_wdr: 0,
            parked: false,
            wakes: 0,
        }
    }

    fn io(&self, address: u16) -> u8 {
        self.io[usize::from(address)]
    }

    fn word(&self, address: u16) -> u16 {
        u16::from(self.io(address)) | u16::from(self.io(address + 1)) << 8
    }

    fn set_button(&mut self, pressed: bool) {
        let changed = pressed != self.pressed;
        self.pressed = pressed;
        let sense = self.io(PORTA_PIN5CTRL) & 0x07;
        if changed && sense == PORT_PIN5CTRL_ISC_BOTHEDGES {
            self.porta_flag = true;
            self.parked = false;
            self.wakes += 1;
        }
    }

    fn convert(&mut self) -> Result<(), String> {
        if self.io(ADC0_CTRLA) & ADC_CTRLA_ENABLE == 0 {
            return Err("conversion with the ADC off".into());
        }
        if self.io(VREF_CTRLA) != VREF_CTRLA_ADC0REFSEL_1V1 {
            return Err("the reference isn't 1.1 V".into());
        }
        let reference = self.io(ADC0_CTRLC) & 0x30;
        let result = match (self.io(ADC0_MUXPOS), reference) {
            (ADC_MUXPOS_MUXPOS_INTREF, ADC_CTRLC_REFSEL_VDDREF) => battery_counts(self.cell_mv),
            (ADC_MUXPOS_MUXPOS_TEMPSENSE, ADC_CTRLC_REFSEL_INTREF) => {
                heat_counts(self.kelvin, GAIN, OFFSET)
            }
            pair => return Err(format!("no such measurement: {pair:?}")),
        };
        self.adc_result = Some(result);
        Ok(())
    }

    /// The pins as the timer drives them, as (linear, fet) duty in counts.
    fn lit(&self) -> Option<ramp::Pwm> {
        let channels = self.io(TCA0_SINGLE_CTRLB);
        let enabled = TCA_SINGLE_CTRLB_CMP0EN | TCA_SINGLE_CTRLB_CMP1EN;
        (channels & enabled == enabled).then_some(self.active)
    }

    fn level(&self) -> Option<u8> {
        let Some(active) = self.lit() else {
            return Some(0);
        };
        (0..=TOP).find(|&level| ramp::pwm(level) == active)
    }
}

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        let at = pc as usize * 2;
        let bytes = self.flash.get(at..at + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read(&mut self, address: u16) -> Result<u8, String> {
        if SRAM.contains(&address) {
            return Ok(self.sram[usize::from(address - SRAM.start)]);
        }
        if address >= MAPPED {
            let at = usize::from(address - MAPPED);
            return self.flash.get(at).copied().ok_or("past flash".into());
        }
        Ok(match address {
            PORTA_IN => {
                if self.pressed {
                    0
                } else {
                    1 << 5
                }
            }
            ADC0_INTFLAGS if self.adc_result.is_some() => ADC_INTFLAGS_RESRDY,
            ADC0_INTFLAGS | RTC_PITSTATUS | WDT_STATUS => 0,
            ADC0_RES => self.adc_result.ok_or("reading RES with no result")? as u8,
            a if a == ADC0_RES + 1 => {
                let high = self.adc_result.ok_or("high byte without low")? >> 8;
                self.adc_result = None;
                high as u8
            }
            ADC0_MUXPOS => self.io(address),
            SIGROW_TEMPSENSE0 => GAIN,
            SIGROW_TEMPSENSE1 => OFFSET as u8,
            _ => return Err(format!("read of unmodelled {address:#06x}")),
        })
    }

    fn write(&mut self, address: u16, value: u8) -> Result<(), String> {
        if SRAM.contains(&address) {
            self.sram[usize::from(address - SRAM.start)] = value;
            return Ok(());
        }
        let armed = std::mem::take(&mut self.ccp_armed);
        let protected = [CLKCTRL_MCLKCTRLB, WDT_CTRLA, RSTCTRL_SWRR];
        if protected.contains(&address) && !armed {
            return Err(format!("{address:#06x} written without CCP"));
        }
        let wide = [
            TCA0_SINGLE_PER,
            TCA0_SINGLE_PERBUF,
            TCA0_SINGLE_CMP0BUF,
            TCA0_SINGLE_CMP1BUF,
        ];
        let low = self.low_byte_at.take();
        if wide.contains(&address) {
            self.low_byte_at = Some(address);
        } else if address > 0 && wide.contains(&(address - 1)) && low != Some(address - 1) {
            return Err(format!("{address:#06x}: high byte first"));
        }
        match address {
            CPU_CCP if value == CPU_CCP_CCP_IOREG => self.ccp_armed = true,
            PORTA_INTFLAGS => self.porta_flag = false,
            RTC_PITINTFLAGS => self.pit_flag = false,
            PORTB_DIRSET => self.io[usize::from(PORTB_DIR)] |= value,
            PORTB_DIRCLR => self.io[usize::from(PORTB_DIR)] &= !value,
            PORTB_OUTCLR => self.io[usize::from(PORTB_OUT)] &= !value,
            ADC0_COMMAND => self.convert()?,
            ADC0_INTFLAGS => self.adc_result = None,
            TCA0_SINGLE_CTRLESET if value == TCA_SINGLE_CTRLESET_LUPD => self.locked = true,
            TCA0_SINGLE_CTRLECLR if value == TCA_SINGLE_CTRLECLR_LUPD => self.locked = false,
            RSTCTRL_SWRR => return Err("software reset: something panicked".into()),
            CLKCTRL_MCLKCTRLB | WDT_CTRLA | RTC_PITCTRLA | RTC_PITINTCTRL | SLPCTRL_CTRLA
            | VREF_CTRLA | VREF_CTRLB | ADC0_CTRLA | ADC0_CTRLB | ADC0_CTRLC | ADC0_CTRLD
            | ADC0_SAMPCTRL | ADC0_MUXPOS | TCA0_SINGLE_CTRLA | TCA0_SINGLE_CTRLB => {
                self.io[usize::from(address)] = value
            }
            a if (PORTA_PIN0CTRL..PORTA_PIN0CTRL + 8).contains(&a)
                || (PORTB_PIN0CTRL..PORTB_PIN0CTRL + 8).contains(&a)
                || (PORTC_PIN0CTRL..PORTC_PIN0CTRL + 4).contains(&a) =>
            {
                self.io[usize::from(a)] = value;
            }
            a if wide.contains(&a) || (a > 0 && wide.contains(&(a - 1))) => {
                self.io[usize::from(a)] = value;
            }
            _ => return Err(format!("write of unmodelled {address:#06x}")),
        }
        Ok(())
    }

    fn wdr(&mut self) -> Result<(), String> {
        self.ticks_since_wdr = 0;
        Ok(())
    }

    fn interrupt(&self) -> Option<u8> {
        if self.porta_flag {
            Some(3)
        } else if self.pit_flag && self.io(RTC_PITINTCTRL) & RTC_PITINTCTRL_PI != 0 {
            Some(7)
        } else {
            None
        }
    }

    fn sleep(&mut self) -> Result<(), String> {
        let control = self.io(SLPCTRL_CTRLA);
        if control & SLPCTRL_CTRLA_SEN == 0 {
            return Err("`sleep` without SEN".into());
        }
        if control & 0x06 == SLPCTRL_CTRLA_SMODE_PDOWN {
            if self.io(PORTA_PIN5CTRL) & 0x07 != PORT_PIN5CTRL_ISC_BOTHEDGES {
                return Err("power-down with no way to wake".into());
            }
            if self.io(PORTA_PIN5CTRL) & PORT_PIN5CTRL_PULLUPEN == 0 {
                return Err("power-down with the button floating".into());
            }
            for (name, address) in [
                ("timer", TCA0_SINGLE_CTRLA),
                ("ADC", ADC0_CTRLA),
                ("reference", VREF_CTRLB),
                ("PIT", RTC_PITCTRLA),
                ("watchdog", WDT_CTRLA),
            ] {
                if self.io(address) != 0 {
                    return Err(format!("power-down with the {name} on"));
                }
            }
            if self.lit().is_some() {
                return Err("power-down with the LED pins on the timer".into());
            }
        } else if self.io(RTC_PITCTRLA) & RTC_PITCTRLA_PITEN == 0 {
            return Err("idle with no tick".into());
        }
        Ok(())
    }

    fn idle(&mut self) -> Result<(), String> {
        if self.io(RTC_PITCTRLA) & RTC_PITCTRLA_PITEN == 0 {
            self.parked = true;
            return Ok(());
        }
        self.ticks += 1;
        self.ticks_since_wdr += 1;
        if self.ticks_since_wdr > 60 {
            return Err("the watchdog bit".into());
        }
        self.pit_flag = true;
        if self.locked {
            return Err("asleep with the timer's buffers locked".into());
        }
        self.active = ramp::Pwm {
            period: self.word(TCA0_SINGLE_PERBUF),
            linear: self.word(TCA0_SINGLE_CMP1BUF),
            fet: self.word(TCA0_SINGLE_CMP0BUF),
        };
        Ok(())
    }
}

struct Rig {
    cpu: Cpu,
    chip: Chip,
    lowest_sp: u16,
}

impl Rig {
    fn booted() -> Self {
        let mut rig = Self {
            cpu: Cpu::new(&SRAM),
            chip: Chip::new(),
            lowest_sp: SRAM.end,
        };
        rig.run(1);
        assert!(rig.chip.parked, "it should boot straight to power-down");
        rig
    }

    /// Runs until `ticks` more have passed or the chip parks in power-down.
    fn run(&mut self, ticks: u32) {
        let target = self.chip.ticks + ticks;
        let mut steps = 0;
        while self.chip.ticks < target && !self.chip.parked {
            self.cpu.step(&mut self.chip).unwrap();
            self.lowest_sp = self.lowest_sp.min(self.cpu.sp);
            steps += 1;
            assert!(
                steps < STEPS_PER_TICK * (ticks + 1),
                "hung at {:#x}",
                self.cpu.pc * 2
            );
        }
    }

    fn press(&mut self, ticks: u32) {
        self.chip.set_button(true);
        self.run(ticks);
    }

    fn release(&mut self, ticks: u32) {
        self.chip.set_button(false);
        self.run(ticks);
    }

    fn click(&mut self) {
        self.press(4);
        self.release(8);
    }

    fn level(&self) -> u8 {
        self.chip.level().expect("the timer isn't on a table entry")
    }
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn boots_dark_with_the_button_armed_and_nothing_running() {
    let rig = Rig::booted();
    assert_eq!(rig.level(), 0);
    assert_eq!(rig.chip.io(PORTB_DIR) & 0b11, 0b11);
    assert_eq!(rig.chip.io(PORTB_OUT) & 0b11, 0);
    assert_eq!(rig.chip.io(PORTB_DIR) & (1 << 5), 0, "amber on its pull-up");
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn a_press_wakes_it_and_a_click_lands_on_the_memory() {
    let mut rig = Rig::booted();
    rig.press(3);
    assert_eq!(rig.chip.wakes, 1);
    assert_eq!(rig.level(), FLOOR);
    rig.release(40);
    assert_eq!(rig.level(), FIRST_MEMORY);
    assert!(!rig.chip.parked);
    assert_ne!(rig.chip.io(PORTB_DIR) & (1 << 5), 0, "amber off while lit");
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn a_click_when_on_goes_dark_then_powers_down_and_wakes_again() {
    let mut rig = Rig::booted();
    rig.click();
    rig.run(40);
    rig.click();
    assert_eq!(rig.level(), 0);
    rig.run(40);
    assert!(rig.chip.parked);
    rig.click();
    rig.run(40);
    assert_eq!(rig.level(), FIRST_MEMORY, "memory survives power-down");
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn turbo_holds_both_pins_high_through_the_table_in_mapped_flash() {
    let mut rig = Rig::booted();
    rig.click();
    rig.click();
    rig.run(40);
    assert_eq!(rig.level(), TOP);
    let active = rig.chip.lit().unwrap();
    assert_eq!(active.period, 254);
    assert_eq!((active.linear, active.fet), (255, 255));
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn three_clicks_blink_the_voltage_read_through_the_adc() {
    let mut rig = Rig::booted();
    rig.chip.cell_mv = 3700;
    for _ in 0..3 {
        rig.click();
    }
    let mut blinks = 0;
    let mut was_lit = false;
    rig.run(30);
    for _ in 0..1500 {
        rig.run(1);
        let lit = rig.level() > 0;
        blinks += u32::from(lit && !was_lit);
        was_lit = lit;
    }
    assert_eq!(blinks, 3 + 7);
    assert!(rig.chip.parked);
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn heat_read_through_the_adc_with_real_calibration_pulls_turbo_down() {
    let mut rig = Rig::booted();
    rig.click();
    rig.click();
    rig.run(64);
    assert_eq!(rig.level(), TOP);
    rig.chip.kelvin = 273 + 60;
    rig.run(64 * 60);
    assert_eq!(rig.level(), ramp::LINEAR_TOP);
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn a_flat_cell_turns_it_off_and_it_powers_down() {
    let mut rig = Rig::booted();
    rig.click();
    rig.run(64);
    rig.chip.cell_mv = 2800;
    rig.run(64 * 12);
    assert_eq!(rig.level(), 0);
    assert!(rig.chip.parked);
}

#[test]
#[ignore = "needs the hex from ./hex.sh, which runs this"]
fn the_stack_is_all_the_ram_it_needs() {
    let mut rig = Rig::booted();
    rig.click();
    rig.click();
    rig.run(200);
    for _ in 0..3 {
        rig.click();
    }
    rig.run(400);
    let used = SRAM.end - 1 - rig.lowest_sp;
    println!("deepest stack seen: {used} bytes of {}", SRAM.len());
    assert!(used < 512, "{used}");
}
