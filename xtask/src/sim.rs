//! A fake ATtiny1616 around `avr.rs`'s CPU, and the checks that run the
//! real firmware on it. `build()` in main.rs runs them after `layout.rs`'s,
//! so `size`, `hex` and `flash` all refuse firmware that fails, and CI runs
//! them through `size`.
//!
//! The fake chip models only the registers the firmware touches, and turns
//! the rules the real chip enforces quietly into errors:
//!
//! - A protected register only takes a write within four instructions of
//!   the CCP key. The real chip ignores a late write.
//! - TCA0's 16-bit registers are written low byte, then high byte, through
//!   the shared TEMP byte. The other way round, the real chip pairs the high
//!   byte with a stale low one.
//! - RTC.PITCTRLA isn't written while PITSTATUS.CTRLBUSY is set. The real
//!   chip drops that write.
//!
//! Touching a register it doesn't model is an error too, so new peripheral
//! code fails here until the model learns about it.
//!
//! With that in place it runs three things:
//!
//! 1. Reset to `main`, with SRAM full of junk, checking .data and .bss come
//!    out right.
//! 2. The same startup code with .data and .bss moved and given contents,
//!    because the firmware has no statics yet, so as linked both loops in
//!    src/rt.rs run zero times.
//! 3. A script of clicks and holds from docs/ui.md, checking the LEDs after
//!    each, how deep the stack goes, and that every pass of the main loop
//!    fits in a 64 Hz tick.

use std::ops::Range;

use crate::avr::{Bus, Cpu, is_two_words};
use crate::elf::{DATA_SPACE, Elf, RAM};

// The fake chip uses the addresses and the fields it models, not all of them.
// atdf.rs loads this file again for its tests on purpose: there, an unused
// constant is a warning, which is how an unchecked one gets caught.
#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../../src/regs.rs"]
mod regs;

use regs::addr as reg;

/// SRAM as data-space addresses.
const SRAM: Range<u16> = (RAM.start - DATA_SPACE) as u16..(RAM.end - DATA_SPACE) as u16;
/// Where flash appears in data space, and how much of it there is.
const MAPPED: u16 = 0x8000;
const FLASH_SIZE: usize = 16 * 1024;

/// SRAM's contents before startup. Real SRAM powers up holding anything; a
/// fixed pattern makes a byte that should have been written stand out.
const JUNK: u8 = 0xA5;

// The pins, from docs/hardware.md, as in src/hw.rs.
const SWITCH: u8 = 1 << 5; // PA5, active low
const FET: u8 = 1 << 0; // PB0, TCA0 WO0
const LINEAR: u8 = 1 << 1; // PB1, TCA0 WO1

/// Instructions startup may take before reaching `main`.
const BOOT_BUDGET: u64 = 100_000;
/// CPU cycles in a 64 Hz tick at 10 MHz.
const TICK_CYCLES: u64 = 10_000_000 / 64;
/// Instructions one pass of the main loop may take. No AVRxt instruction
/// takes more than 4 cycles, so this many always fit in a tick.
const TICK_BUDGET: u64 = TICK_CYCLES / 4;

/// How many reads of RTC.PITSTATUS report busy after reset or a write to
/// PITCTRLA. The real chip is busy for a couple of 32 kHz clocks while it
/// syncs; any non-zero number makes the firmware wait.
const PIT_SYNC_READS: u8 = 2;

/// TCA0's 16-bit registers the firmware writes, by the low byte's address.
const WIDE: [usize; 4] = [
    reg::TCA0_CNT,
    reg::TCA0_PERBUF,
    reg::TCA0_CMP0BUF,
    reg::TCA0_CMP1BUF,
];

pub struct Report {
    /// The most stack the firmware used, in bytes.
    pub stack: u16,
    /// The most instructions one pass of the main loop took.
    pub loop_instructions: u64,
    /// What that's measured against.
    pub loop_budget: u64,
}

pub fn check(elf: &Elf) -> Result<Report, String> {
    let symbol = |name| {
        elf.symbol(name)
            .ok_or_else(|| format!("no `{name}` symbol"))
    };
    let sram = |name| -> Result<u16, String> { Ok((symbol(name)? - DATA_SPACE) as u16) };
    let main = symbol("main")? / 2;
    let image = elf.flash_image();
    let linked = Layout {
        data: sram("__data_start")?..sram("__data_end")?,
        bss: sram("__bss_start")?..sram("__bss_end")?,
        load: symbol("__data_load")? as u16,
    };

    let mut sim = Sim::new(image.clone(), linked.bss.end)?;
    sim.run_to(main, BOOT_BUDGET)
        .and_then(|()| check_startup(&sim.chip, &linked))
        .map_err(|e| format!("startup: {e}"))?;

    startup_with_statics(&image, symbol("__reset")?, main, &linked)
        .map_err(|e| format!("startup with statics: {e}"))?;

    clicks(sim)
}

/// Where src/rt.rs's startup copies and zeroes, as data-space addresses.
struct Layout {
    data: Range<u16>,
    bss: Range<u16>,
    /// .data's initial values, in the mapped-flash window.
    load: u16,
}

fn check_startup(chip: &Chip, layout: &Layout) -> Result<(), String> {
    let from = usize::from(layout.load.wrapping_sub(MAPPED));
    let len = usize::from(layout.data.end - layout.data.start);
    let initial = chip
        .flash
        .get(from..from + len)
        .ok_or(".data's initial values are outside flash")?;
    if chip.sram(&layout.data) != initial {
        return Err(".data in SRAM doesn't match its initial values in flash".into());
    }
    if let Some((i, b)) = chip
        .sram(&layout.bss)
        .iter()
        .enumerate()
        .find(|&(_, &b)| b != 0)
    {
        return Err(format!(
            ".bss isn't zeroed: {:#06x} holds {b:#04x}",
            usize::from(layout.bss.start) + i
        ));
    }
    Ok(())
}

/// Moves .data and .bss somewhere awkward, gives .data five bytes of
/// initial values, patches the addresses into the real startup code and
/// runs it. Then checks it wrote exactly those bytes and nothing either side.
fn startup_with_statics(
    image: &[u8],
    reset: u32,
    main: u32,
    linked: &Layout,
) -> Result<(), String> {
    let values = [0x11, 0x22, 0x33, 0x44, 0x55];
    let mut image = image.to_vec();
    image.resize(image.len().next_multiple_of(2), 0xFF);
    let load = MAPPED + image.len() as u16;
    image.extend(values);

    // An odd start, away from the edges of SRAM, so off-by-ones show.
    let start = SRAM.start + 0x11;
    let moved = Layout {
        data: start..start + 5,
        bss: start + 5..start + 12,
        load,
    };
    patch_startup(&mut image, reset as usize, linked, &moved)?;

    let mut sim = Sim::new(image, moved.bss.end)?;
    sim.run_to(main, BOOT_BUDGET)?;
    check_startup(&sim.chip, &moved)?;
    for edge in [moved.data.start - 1, moved.bss.end] {
        if sim.chip.sram(&(edge..edge + 1)) != [JUNK] {
            return Err(format!("wrote to {edge:#06x}, outside .data and .bss"));
        }
    }
    Ok(())
}

/// Rewrites the `ldi`s in src/rt.rs's startup that load the .data and .bss
/// addresses, after checking they're the ones expected, in order.
fn patch_startup(
    image: &mut [u8],
    reset: usize,
    linked: &Layout,
    moved: &Layout,
) -> Result<(), String> {
    // (register, byte) for each `ldi`, as rt.rs loads them: X = __data_start,
    // Z = __data_load, r25:r24 = __data_end, X = __bss_start, r25:r24 = __bss_end.
    let loads = |l: &Layout| -> Vec<(u8, u8)> {
        [
            (26, l.data.start),
            (30, l.load),
            (24, l.data.end),
            (26, l.bss.start),
            (24, l.bss.end),
        ]
        .into_iter()
        .flat_map(|(r, v)| {
            let [lo, hi] = v.to_le_bytes();
            [(r, lo), (r + 1, hi)]
        })
        .collect()
    };

    let mut found = Vec::new();
    let mut at = reset;
    loop {
        let w = word(image, at).ok_or("no call to main after __reset")?;
        if w & 0xFE0E == 0x940E {
            break; // call main
        }
        if w >> 12 == 0xE {
            let k = ((w >> 4) & 0xF0 | w & 0x0F) as u8;
            found.push((at, (16 + ((w >> 4) & 0x0F)) as u8, k));
        }
        at += if is_two_words(w) { 4 } else { 2 };
    }

    let actual: Vec<(u8, u8)> = found.iter().map(|&(_, r, k)| (r, k)).collect();
    if actual != loads(linked) {
        return Err(
            "src/rt.rs's startup doesn't load the registers sim.rs expects; update `patch_startup`"
                .into(),
        );
    }
    for (&(at, r, _), (_, k)) in found.iter().zip(loads(moved)) {
        image[at..at + 2].copy_from_slice(&ldi(r, k).to_le_bytes());
    }
    Ok(())
}

fn ldi(r: u8, k: u8) -> u16 {
    let (r, k) = (u16::from(r), u16::from(k));
    0xE000 | (k & 0xF0) << 4 | (r - 16) << 4 | k & 0x0F
}

fn word(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

// Press and release timings, in ticks. src/button.rs counts a press under
// 24 ticks as a click and a gap over 18 as the end of a run of clicks.
const PRESS: (bool, u32) = (true, 4);
const GAP: (bool, u32) = (false, 4);
const HOLD: (bool, u32) = (true, 64);
const SETTLE: (bool, u32) = (false, 40);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Light {
    Off,
    On,
}

/// A docs/ui.md row: its name, the switch as (pressed, ticks) steps, and
/// what the light should be after.
type Row = (&'static str, &'static [(bool, u32)], Light);

/// Runs docs/ui.md's rows against the firmware, from just after `main`.
fn clicks(mut sim: Sim) -> Result<Report, String> {
    use Light::{Off, On};
    let script: [Row; 7] = [
        ("starts off", &[SETTLE], Off),
        ("1C from off turns on", &[PRESS, SETTLE], On),
        ("1H while on does nothing", &[HOLD, SETTLE], On),
        ("2C while on does nothing", &[PRESS, GAP, PRESS, SETTLE], On),
        ("1C from on turns off", &[PRESS, SETTLE], Off),
        (
            "2C while off does nothing",
            &[PRESS, GAP, PRESS, SETTLE],
            Off,
        ),
        ("1H while off does nothing", &[HOLD, SETTLE], Off),
    ];

    // The first pass runs hw::init too, so it doesn't count towards the loop.
    sim.tick(false).map_err(|e| format!("hw::init: {e}"))?;
    let mut busiest = 0;
    for (row, steps, want) in script {
        for &(pressed, ticks) in steps {
            for _ in 0..ticks {
                let took = sim.tick(pressed).map_err(|e| format!("{row}: {e}"))?;
                busiest = busiest.max(took);
            }
        }
        let leds = sim.chip.leds();
        let light = if leds == (0, 0) { Off } else { On };
        if light != want {
            return Err(format!(
                "{row}: expected the light {want:?}, but (linear, FET) duty is {leds:?}"
            ));
        }
    }

    Ok(Report {
        stack: SRAM.end - 1 - sim.lowest_sp,
        loop_instructions: busiest,
        loop_budget: TICK_BUDGET,
    })
}

struct Sim {
    cpu: Cpu,
    chip: Chip,
    /// SP below this means a push landed in .bss.
    stack_floor: u16,
    lowest_sp: u16,
}

impl Sim {
    fn new(image: Vec<u8>, bss_end: u16) -> Result<Self, String> {
        if image.len() > FLASH_SIZE {
            return Err(format!("{} bytes won't fit in flash", image.len()));
        }
        let cpu = Cpu::new(&SRAM);
        Ok(Self {
            lowest_sp: cpu.sp,
            cpu,
            chip: Chip::new(image),
            stack_floor: bss_end - 1,
        })
    }

    fn step(&mut self) -> Result<(), String> {
        let pc = self.cpu.pc;
        self.cpu.step(&mut self.chip)?;
        self.chip.now += 1;
        self.lowest_sp = self.lowest_sp.min(self.cpu.sp);
        if self.cpu.sp < self.stack_floor {
            return Err(format!("{:#06x}: the stack ran into .bss", pc * 2));
        }
        Ok(())
    }

    /// Runs until PC reaches `target`, a word address.
    fn run_to(&mut self, target: u32, budget: u64) -> Result<(), String> {
        for _ in 0..budget {
            if self.cpu.pc == target {
                return Ok(());
            }
            self.step()?;
        }
        Err(format!(
            "didn't reach {:#06x} within {budget} instructions",
            target * 2
        ))
    }

    /// One pass of the main loop with the switch as given: runs until the
    /// firmware picks up the next tick. Returns how many instructions it took.
    fn tick(&mut self, pressed: bool) -> Result<u64, String> {
        self.chip.pressed = pressed;
        let (start, ticks) = (self.chip.now, self.chip.ticks);
        while self.chip.ticks == ticks {
            if self.chip.now - start > TICK_BUDGET {
                return Err(format!(
                    "a pass of the main loop ran past {TICK_BUDGET} instructions, \
                     so it hangs or misses 64 Hz ticks"
                ));
            }
            self.step()?;
        }
        if let Some((low, _)) = self.chip.temp {
            return Err(format!("left {} half-written", name(low)));
        }
        Ok(self.chip.now - start)
    }
}

struct Chip {
    flash: Vec<u8>,
    sram: Vec<u8>,
    /// Plain storage for the registers that are only storage.
    io: Vec<u8>,
    /// Instructions run so far.
    now: u64,
    /// The last instruction a protected register still takes a write on.
    unlocked_until: Option<u64>,
    pressed: bool,
    pit_busy: u8,
    pit_flag: bool,
    ticks: u32,
    /// TCA0's 16-bit registers, in `WIDE`'s order.
    wide: [u16; 4],
    /// TCA0's TEMP byte, and which register's low byte it holds.
    temp: Option<(usize, u8)>,
}

impl Chip {
    fn new(flash: Vec<u8>) -> Self {
        Self {
            flash,
            sram: vec![JUNK; usize::from(SRAM.end - SRAM.start)],
            io: vec![0; 0x1000],
            now: 0,
            unlocked_until: None,
            pressed: false,
            pit_busy: PIT_SYNC_READS,
            pit_flag: false,
            ticks: 0,
            wide: [0; 4],
            temp: None,
        }
    }

    fn sram(&self, range: &Range<u16>) -> &[u8] {
        &self.sram[usize::from(range.start - SRAM.start)..usize::from(range.end - SRAM.start)]
    }

    fn wide(&self, addr: usize) -> u16 {
        WIDE.iter()
            .position(|&a| a == addr)
            .map_or(0, |i| self.wide[i])
    }

    /// (linear, FET) duty as the pins see it: TCA0 running, the compare
    /// channel enabled and the pin set as an output.
    fn leds(&self) -> (u16, u16) {
        let running = self.io[reg::TCA0_CTRLA] & regs::TCA_ENABLE != 0;
        let channel = |enable: u8, pin: u8, cmp: usize| {
            let on = running
                && self.io[reg::TCA0_CTRLB] & enable != 0
                && self.io[reg::VPORTB_DIR] & pin != 0;
            if on { self.wide(cmp) } else { 0 }
        };
        (
            channel(regs::TCA_CMP1EN, LINEAR, reg::TCA0_CMP1BUF),
            channel(regs::TCA_CMP0EN, FET, reg::TCA0_CMP0BUF),
        )
    }
}

/// Registers that are plain storage as far as the firmware's concerned.
fn plain(addr: usize) -> bool {
    let pin_ctrl = [
        reg::PORTA_PIN0CTRL,
        reg::PORTB_PIN0CTRL,
        reg::PORTC_PIN0CTRL,
    ]
    .iter()
    .any(|&base| (base..base + 8).contains(&addr));
    pin_ctrl
        || matches!(
            addr,
            reg::VPORTB_DIR
                | reg::CLKCTRL_MCLKCTRLB
                | reg::RTC_PITCTRLA
                | reg::TCA0_CTRLA
                | reg::TCA0_CTRLB
        )
}

fn name(addr: usize) -> String {
    let known = [
        (reg::CPU_CCP, "CPU.CCP"),
        (reg::VPORTA_IN, "VPORTA.IN"),
        (reg::VPORTB_DIR, "VPORTB.DIR"),
        (reg::CLKCTRL_MCLKCTRLB, "CLKCTRL.MCLKCTRLB"),
        (reg::RTC_PITCTRLA, "RTC.PITCTRLA"),
        (reg::RTC_PITSTATUS, "RTC.PITSTATUS"),
        (reg::RTC_PITINTFLAGS, "RTC.PITINTFLAGS"),
        (reg::TCA0_CTRLA, "TCA0.CTRLA"),
        (reg::TCA0_CTRLB, "TCA0.CTRLB"),
        (reg::TCA0_CNT, "TCA0.CNT"),
        (reg::TCA0_PERBUF, "TCA0.PERBUF"),
        (reg::TCA0_CMP0BUF, "TCA0.CMP0BUF"),
        (reg::TCA0_CMP1BUF, "TCA0.CMP1BUF"),
    ];
    known
        .iter()
        .find(|&&(a, _)| a == addr)
        .map_or_else(|| format!("{addr:#06x}"), |(_, n)| (*n).to_string())
}

impl Bus for Chip {
    fn fetch(&self, pc: u32) -> Option<u16> {
        word(&self.flash, pc as usize * 2)
    }

    fn read(&mut self, addr: u16) -> Result<u8, String> {
        let a = usize::from(addr);
        if SRAM.contains(&addr) {
            return Ok(self.sram[usize::from(addr - SRAM.start)]);
        }
        if (MAPPED..MAPPED + FLASH_SIZE as u16).contains(&addr) {
            // Erased flash past the end of the image reads as 0xFF.
            return Ok(*self.flash.get(usize::from(addr - MAPPED)).unwrap_or(&0xFF));
        }
        match a {
            reg::VPORTA_IN => Ok(if self.pressed { !SWITCH } else { 0xFF }),
            reg::RTC_PITSTATUS => {
                let busy = self.pit_busy > 0;
                self.pit_busy = self.pit_busy.saturating_sub(1);
                Ok(if busy { regs::RTC_CTRLBUSY } else { 0 })
            }
            reg::RTC_PITINTFLAGS => {
                // The firmware busy-waits on this, so the next tick arrives
                // the moment it looks.
                if !self.pit_flag {
                    if self.io[reg::RTC_PITCTRLA] & regs::RTC_PITEN == 0 {
                        return Err(
                            "waits on RTC.PITINTFLAGS with the PIT off, so it would wait forever"
                                .into(),
                        );
                    }
                    self.pit_flag = true;
                    self.ticks += 1;
                }
                Ok(regs::RTC_PI)
            }
            _ if plain(a) => Ok(self.io[a]),
            _ => Err(format!(
                "reads {}, which the simulator doesn't model",
                name(a)
            )),
        }
    }

    fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        let a = usize::from(addr);
        if SRAM.contains(&addr) {
            self.sram[usize::from(addr - SRAM.start)] = value;
            return Ok(());
        }
        match a {
            reg::CPU_CCP => {
                if value != regs::CPU_CCP_IOREG {
                    return Err(format!(
                        "writes {value:#04x} to CPU.CCP, which isn't the IOREG key"
                    ));
                }
                self.unlocked_until = Some(self.now + 4);
            }
            reg::CLKCTRL_MCLKCTRLB => {
                if !self.unlocked_until.is_some_and(|until| self.now <= until) {
                    return Err(format!(
                        "writes {} without the CCP key in the four instructions before, \
                         and the chip ignores that write",
                        name(a)
                    ));
                }
                self.unlocked_until = None;
                self.io[a] = value;
            }
            reg::RTC_PITCTRLA => {
                if self.pit_busy > 0 {
                    return Err(format!(
                        "writes {} while RTC.PITSTATUS.CTRLBUSY is set, and the chip drops that write",
                        name(a)
                    ));
                }
                self.io[a] = value;
                self.pit_busy = PIT_SYNC_READS;
            }
            reg::RTC_PITINTFLAGS => {
                // Writing a 1 clears the flag.
                if value & regs::RTC_PI != 0 {
                    self.pit_flag = false;
                }
            }
            _ if WIDE.contains(&a) => {
                if let Some((pending, _)) = self.temp {
                    return Err(format!(
                        "writes the low byte of {} while {}'s is still waiting for its high byte",
                        name(a),
                        name(pending)
                    ));
                }
                self.temp = Some((a, value));
            }
            _ if WIDE.contains(&a.wrapping_sub(1)) => match self.temp.take() {
                Some((low, lo)) if low == a - 1 => {
                    let i = WIDE.iter().position(|&w| w == low).unwrap_or_default();
                    self.wide[i] = u16::from_le_bytes([lo, value]);
                }
                _ => {
                    return Err(format!(
                        "writes the high byte of {} without its low byte just before, \
                         so the chip pairs it with whatever TEMP held",
                        name(a - 1)
                    ));
                }
            },
            _ if plain(a) => self.io[a] = value,
            _ => {
                return Err(format!(
                    "writes {value:#04x} to {}, which the simulator doesn't model",
                    name(a)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip() -> Chip {
        Chip::new(vec![0; 64])
    }

    #[test]
    fn takes_a_protected_write_only_just_after_the_key() {
        let mut c = chip();
        let clk = reg::CLKCTRL_MCLKCTRLB as u16;
        assert!(c.write(clk, 1).is_err(), "no key");

        c.now = 10;
        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 14;
        assert_eq!(c.write(clk, 1), Ok(()), "fourth instruction after");
        assert_eq!(c.io[reg::CLKCTRL_MCLKCTRLB], 1);
        assert!(c.write(clk, 2).is_err(), "the key only covers one write");

        c.write(reg::CPU_CCP as u16, regs::CPU_CCP_IOREG).unwrap();
        c.now = 19;
        assert!(c.write(clk, 3).unwrap_err().contains("ignores"), "too late");
        assert!(c.write(reg::CPU_CCP as u16, 0x9D).is_err(), "SPM key");
    }

    #[test]
    fn writes_16_bit_registers_through_temp() {
        let mut c = chip();
        let cmp = reg::TCA0_CMP1BUF as u16;
        c.write(cmp, 0x34).unwrap();
        c.write(cmp + 1, 0x12).unwrap();
        assert_eq!(c.wide(reg::TCA0_CMP1BUF), 0x1234);

        assert!(c.write(cmp + 1, 0).unwrap_err().contains("high byte"));
        c.write(cmp, 1).unwrap();
        let err = c.write(reg::TCA0_CMP0BUF as u16, 1).unwrap_err();
        assert!(err.contains("still waiting"), "{err}");
        let err = c.write(reg::TCA0_CMP0BUF as u16 + 1, 1).unwrap_err();
        assert!(
            err.contains("TCA0.CMP0BUF"),
            "high byte of a different register: {err}"
        );
    }

    #[test]
    fn makes_the_firmware_wait_for_the_pit_to_sync() {
        let mut c = chip();
        let ctrla = reg::RTC_PITCTRLA as u16;
        let enable = regs::RTC_PERIOD_CYC512 | regs::RTC_PITEN;
        assert!(c.write(ctrla, enable).unwrap_err().contains("CTRLBUSY"));
        let status = reg::RTC_PITSTATUS as u16;
        while c.read(status).unwrap() & regs::RTC_CTRLBUSY != 0 {}
        assert_eq!(c.write(ctrla, enable), Ok(()));
        assert!(c.write(ctrla, 0).is_err(), "busy again after a write");
    }

    #[test]
    fn ticks_when_the_firmware_polls_the_pit_flag() {
        let mut c = chip();
        let flags = reg::RTC_PITINTFLAGS as u16;
        assert!(c.read(flags).unwrap_err().contains("PIT off"));
        c.io[reg::RTC_PITCTRLA] = regs::RTC_PITEN;
        assert_eq!(c.read(flags), Ok(regs::RTC_PI));
        assert_eq!(c.read(flags), Ok(regs::RTC_PI), "stays up until cleared");
        assert_eq!(c.ticks, 1);
        c.write(flags, regs::RTC_PI).unwrap();
        c.read(flags).unwrap();
        assert_eq!(c.ticks, 2);
    }

    #[test]
    fn reads_the_switch_active_low() {
        let mut c = chip();
        let pin = reg::VPORTA_IN as u16;
        assert_eq!(c.read(pin).unwrap() & SWITCH, SWITCH);
        c.pressed = true;
        assert_eq!(c.read(pin).unwrap() & SWITCH, 0);
    }

    #[test]
    fn refuses_registers_it_doesnt_model() {
        let mut c = chip();
        assert!(
            c.read(0x0600).unwrap_err().contains("doesn't model"),
            "ADC0"
        );
        assert!(
            c.write(reg::VPORTA_IN as u16, 0).is_err(),
            "VPORTA.IN is read-only here"
        );
        assert!(c.write(MAPPED, 0).is_err(), "flash isn't writable");
        assert_eq!(c.read(MAPPED + 100), Ok(0xFF), "erased flash");
        assert_eq!(c.read(MAPPED + 2), Ok(0));
    }

    #[test]
    fn leds_need_the_timer_the_channel_and_the_pin() {
        let mut c = chip();
        c.wide = [0, 255, 200, 100];
        assert_eq!(c.leds(), (0, 0));
        c.io[reg::TCA0_CTRLA] = regs::TCA_ENABLE;
        c.io[reg::TCA0_CTRLB] = regs::TCA_CMP0EN | regs::TCA_CMP1EN;
        assert_eq!(c.leds(), (0, 0), "pins still inputs");
        c.io[reg::VPORTB_DIR] = LINEAR;
        assert_eq!(c.leds(), (100, 0));
        c.io[reg::VPORTB_DIR] = LINEAR | FET;
        assert_eq!(c.leds(), (100, 200));
    }

    /// The `ldi`s rt.rs uses, then `call main`.
    fn startup_code(l: &Layout) -> Vec<u8> {
        let mut words = vec![0x2411, 0xBE1F]; // clr r1; out SREG, r1
        for (r, v) in [
            (26, l.data.start),
            (30, l.load),
            (24, l.data.end),
            (26, l.bss.start),
            (24, l.bss.end),
        ] {
            let [lo, hi] = v.to_le_bytes();
            words.extend([ldi(r, lo), ldi(r + 1, hi)]);
        }
        words.extend([0x940E, 0x0100]);
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn layout(start: u16, load: u16) -> Layout {
        Layout {
            data: start..start + 2,
            bss: start + 2..start + 4,
            load,
        }
    }

    #[test]
    fn patches_the_startup_addresses() {
        let (linked, moved) = (layout(0x3800, 0x8252), layout(0x3811, 0x8300));
        let mut image = startup_code(&linked);
        patch_startup(&mut image, 0, &linked, &moved).unwrap();
        assert_eq!(image, startup_code(&moved));
    }

    #[test]
    fn refuses_to_patch_startup_code_it_doesnt_recognise() {
        let linked = layout(0x3800, 0x8252);
        let mut image = startup_code(&linked);
        let other = layout(0x3802, 0x8252);
        assert!(patch_startup(&mut image, 0, &other, &linked).is_err());
        image.truncate(image.len() - 4);
        assert!(
            patch_startup(&mut image, 0, &linked, &linked).is_err(),
            "no call"
        );
    }
}
