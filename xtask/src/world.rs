//! What the torch sees, for the simulators: the cell's voltage, the MCU's
//! temperature, an ADC that's broken, the fuses, flash, RAM and CPU registers
//! it checks at power-on, and the clocks it checks against each other.
//!
//! The library-side runs (`simulate.rs`) and the firmware on the simulated
//! chip (`sim.rs`) both get their raw readings from here, so the two see
//! exactly the same numbers and can be compared tick by tick.

use std::fmt;
use std::mem;
use std::str::FromStr;

use ts21::ramp::PWM_TOP;
use ts21::selftest::{Faults, Fuses, NO_TICK, OVERFLOWS_MAX};
use ts21::sensors::Channel;
use ts21::testing::{Adc, temperature_reading, voltage_reading};
use ts21::torch::Torch;

use crate::avr::Cpu;
use crate::sim::regs::addr as reg;
use crate::updi::map;

/// The temperature sensor calibration the simulated chip has in its
/// signature row, the same as the library tests'.
pub use ts21::testing::CALIBRATION;

/// The fuse bytes the simulated chip has, FUSE.WDTCFG to FUSE.BOOTEND.
pub const FUSE_BYTES: usize = 9;

/// Each fuse byte's name, by its offset from FUSE.WDTCFG, for scripts like
/// `fuse:OSCCFG=0x01`. Offset 3 is reserved.
pub const FUSE_NAMES: [&str; FUSE_BYTES] = [
    "WDTCFG", "BODCFG", "OSCCFG", "", "TCD0CFG", "SYSCFG0", "SYSCFG1", "APPEND", "BOOTEND",
];

/// Why a fuse value can't go in a script, if it can't. The simulated chip
/// only lets the firmware read the fuses; it doesn't do what they'd do to a
/// real one. Most wrong values change something the firmware then reports
/// (a clock, a section), but these stop it running at all, so a script
/// with them would show behaviour no real light has:
///
/// - `WDTCFG.PERIOD` (mask 0x0F) starts the watchdog at reset, and nothing
///   feeds it before `hw::init`, so the chip resets in a loop (DS §19.3.3.3).
/// - `SYSCFG0.CRCSRC` (mask 0xC0) other than NOCRC (0x3) scans flash before
///   the CPU starts and hangs on a mismatch (DS §27.3.1).
pub fn unmodelled_fuse(offset: usize, value: u8) -> Option<&'static str> {
    match FUSE_NAMES.get(offset).copied() {
        Some("WDTCFG") if value & 0x0F != 0 => {
            Some("a WDTCFG.PERIOD starts the watchdog at reset, which the simulator doesn't model")
        }
        Some("SYSCFG0") if value & 0xC0 != 0xC0 => Some(
            "a SYSCFG0.CRCSRC scans flash before reset ends, which the simulator doesn't model",
        ),
        _ => None,
    }
}

/// The fuses the firmware expects, as the datasheet has a factory chip:
/// no watchdog from the fuse, BOD off, 20 MHz, UPDI on the reset pin, no
/// CRC at start-up and no boot or application section. The datasheet, the
/// errata and the ATDF disagree on OSCCFG and SYSCFG0's spare bits, which
/// is why the firmware only checks fields.
pub const FACTORY_FUSES: [u8; FUSE_BYTES] = [0x00, 0x00, 0x02, 0xFF, 0x00, 0xC4, 0x07, 0x00, 0x00];

/// A fault in SRAM for src/rt.rs's March tests to find, on every load and
/// store. A script's is there from reset; sim.rs's `late_fault` also breaks
/// a chip that's already running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RamFault {
    /// One bit of one byte always reads `high`, whatever was written.
    Stuck { addr: u16, bit: u8, high: bool },
    /// A broken address decoder: `addr` reaches `to`'s byte instead of its
    /// own, so the two read and write as one.
    Alias { addr: u16, to: u16 },
    /// Two bits of one byte shorted together. Both read as the AND of what
    /// was written to them, so they only show up when written different
    /// values, which a march with only 0x00 and 0xFF never does.
    Bridge { addr: u16, bits: [u8; 2] },
    /// A coupling fault: a store that takes bit `bit` of `from` to `rising`
    /// disturbs bit `victim` of `to`, flipping it, or forcing it to `force`
    /// if that's set. A store that doesn't change the bit does nothing, and
    /// reading never does.
    Coupling {
        from: u16,
        bit: u8,
        rising: bool,
        to: u16,
        victim: u8,
        force: Option<bool>,
    },
}

impl RamFault {
    /// The byte a store of `after` over `before` at `addr` disturbs, if any.
    pub fn disturbs(self, addr: u16, before: u8, after: u8) -> Option<u16> {
        match self {
            Self::Coupling {
                from,
                bit,
                rising,
                to,
                ..
            } if from == addr => {
                let mask = 1 << bit;
                let edge = before & mask != after & mask && (after & mask != 0) == rising;
                edge.then_some(to)
            }
            _ => None,
        }
    }

    /// What a byte holding `stored` holds once `disturbs` has picked it.
    pub fn disturbed(self, stored: u8) -> u8 {
        match self {
            Self::Coupling { victim, force, .. } => match force {
                None => stored ^ 1 << victim,
                Some(true) => stored | 1 << victim,
                Some(false) => stored & !(1 << victim),
            },
            _ => stored,
        }
    }

    /// The byte an access to `addr` really reaches.
    pub fn cell(self, addr: u16) -> u16 {
        match self {
            Self::Alias { addr: from, to } if addr == from => to,
            _ => addr,
        }
    }

    /// What the byte at `addr` reads as, holding `stored`.
    pub fn read(self, addr: u16, stored: u8) -> u8 {
        match self {
            Self::Stuck {
                addr: at,
                bit,
                high,
            } if at == addr => {
                if high {
                    stored | 1 << bit
                } else {
                    stored & !(1 << bit)
                }
            }
            Self::Bridge { addr: at, bits } if at == addr => {
                let both = 1 << bits[0] | 1 << bits[1];
                if stored & both == both {
                    stored
                } else {
                    stored & !both
                }
            }
            _ => stored,
        }
    }
}

/// A CPU register the startup register test covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Register {
    /// r0 to r31.
    R(u8),
    Sreg,
    Spl,
    Sph,
}

/// A broken CPU register. The simulated chip forces it before every
/// instruction, from reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterFault {
    /// A bit that's stuck.
    Stuck { reg: Register, bit: u8, high: bool },
    /// Two bits shorted together. Unless they hold the same value, both read
    /// `high`, the winner of the short.
    Short {
        reg: Register,
        bits: [u8; 2],
        high: bool,
    },
}

impl RegisterFault {
    pub fn apply(self, cpu: &mut Cpu) {
        let reg = match self {
            Self::Stuck { reg, .. } | Self::Short { reg, .. } => reg,
        };
        let force = |v: &mut u8| match self {
            Self::Stuck { bit, high, .. } => set_bits(v, 1 << bit, high),
            Self::Short {
                bits: [a, b], high, ..
            } => {
                let both = 1 << a | 1 << b;
                if *v & both != 0 && *v & both != both {
                    set_bits(v, both, high);
                }
            }
        };
        match reg {
            Register::R(n) => force(&mut cpu.r[usize::from(n)]),
            Register::Sreg => force(&mut cpu.sreg),
            Register::Spl | Register::Sph => {
                let mut bytes = cpu.sp.to_le_bytes();
                force(&mut bytes[usize::from(reg == Register::Sph)]);
                cpu.sp = u16::from_le_bytes(bytes);
            }
        }
    }
}

fn set_bits(v: &mut u8, mask: u8, high: bool) {
    if high {
        *v |= mask;
    } else {
        *v &= !mask;
    }
}

/// Scripts write the faults as `ram:stuck:0x3800.3=1` (bit 3 of 0x3800
/// stuck high), `ram:alias:0x3900=0x3800` (0x3900 reaches 0x3800's byte),
/// `ram:bridge:0x3800.0.1` (bits 0 and 1 of 0x3800 shorted),
/// `ram:couple:0x3900.7=1:0x3800.0` (bit 7 of 0x3900 going high flips bit 0
/// of 0x3800; a trailing `=0` or `=1` forces it instead), `reg:r24.0=1` and
/// `reg:short:r24.0.2=0` (bits 0 and 2 of r24 shorted, with 0 winning).
/// These parse the part after `ram:` or `reg:`, and print the whole word.
impl FromStr for RamFault {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "expected ram:stuck:0x3800.3=1, ram:alias:0x3900=0x3800, \
                 ram:bridge:0x3800.0.1 or ram:couple:0x3900.7=1:0x3800.0, not `ram:{s}`"
            )
        };
        let (kind, rest) = s.split_once(':').ok_or_else(bad)?;
        let fault = match kind {
            "stuck" => {
                let (at, value) = rest.split_once('=').ok_or_else(bad)?;
                let (addr, bit) = at.split_once('.').ok_or_else(bad)?;
                Self::Stuck {
                    addr: sram_address(addr)?,
                    bit: bit_number(bit)?,
                    high: level(value)?,
                }
            }
            "alias" => {
                let (addr, to) = rest.split_once('=').ok_or_else(bad)?;
                let (addr, to) = (sram_address(addr)?, sram_address(to)?);
                if addr == to {
                    return Err(format!("{addr:#06x} reaching its own byte is no fault"));
                }
                Self::Alias { addr, to }
            }
            "bridge" => {
                let mut parts = rest.split('.');
                let (Some(addr), Some(a), Some(b), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return Err(bad());
                };
                let bits = [bit_number(a)?, bit_number(b)?];
                if bits[0] == bits[1] {
                    return Err(format!("bit {} can't short to itself", bits[0]));
                }
                Self::Bridge {
                    addr: sram_address(addr)?,
                    bits,
                }
            }
            "couple" => {
                let (culprit, target) = rest.split_once(':').ok_or_else(bad)?;
                let (at, edge) = culprit.split_once('=').ok_or_else(bad)?;
                let (from, bit) = at.split_once('.').ok_or_else(bad)?;
                let (at, force) = match target.split_once('=') {
                    Some((at, force)) => (at, Some(level(force)?)),
                    None => (target, None),
                };
                let (to, victim) = at.split_once('.').ok_or_else(bad)?;
                let (from, bit) = (sram_address(from)?, bit_number(bit)?);
                let (to, victim) = (sram_address(to)?, bit_number(victim)?);
                if from == to && bit == victim {
                    return Err(format!("bit {bit} of {from:#06x} can't couple to itself"));
                }
                Self::Coupling {
                    from,
                    bit,
                    rising: level(edge)?,
                    to,
                    victim,
                    force,
                }
            }
            _ => return Err(bad()),
        };
        Ok(fault)
    }
}

impl fmt::Display for RamFault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Self::Stuck { addr, bit, high } => {
                write!(f, "ram:stuck:{addr:#06x}.{bit}={}", u8::from(high))
            }
            Self::Alias { addr, to } => write!(f, "ram:alias:{addr:#06x}={to:#06x}"),
            Self::Bridge { addr, bits: [a, b] } => write!(f, "ram:bridge:{addr:#06x}.{a}.{b}"),
            Self::Coupling {
                from,
                bit,
                rising,
                to,
                victim,
                force,
            } => {
                let edge = u8::from(rising);
                write!(f, "ram:couple:{from:#06x}.{bit}={edge}:{to:#06x}.{victim}")?;
                match force {
                    Some(force) => write!(f, "={}", u8::from(force)),
                    None => Ok(()),
                }
            }
        }
    }
}

impl FromStr for RegisterFault {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "expected a broken register like reg:r24.0=1 or reg:short:r24.0.2=0, \
                 not `reg:{s}`"
            )
        };
        if let Some(short) = s.strip_prefix("short:") {
            let (at, value) = short.split_once('=').ok_or_else(bad)?;
            let mut parts = at.split('.');
            let (Some(reg), Some(a), Some(b), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(bad());
            };
            let bits = [bit_number(a)?, bit_number(b)?];
            if bits[0] == bits[1] {
                return Err(format!("bit {} can't short to itself", bits[0]));
            }
            return Ok(Self::Short {
                reg: reg.parse()?,
                bits,
                high: level(value)?,
            });
        }
        let (at, value) = s.split_once('=').ok_or_else(bad)?;
        let (reg, bit) = at.split_once('.').ok_or_else(bad)?;
        Ok(Self::Stuck {
            reg: reg.parse()?,
            bit: bit_number(bit)?,
            high: level(value)?,
        })
    }
}

impl FromStr for Register {
    type Err = String;

    fn from_str(reg: &str) -> Result<Self, String> {
        match reg {
            "SREG" => Ok(Self::Sreg),
            "SPL" => Ok(Self::Spl),
            "SPH" => Ok(Self::Sph),
            _ => reg
                .strip_prefix('r')
                .and_then(|n| n.parse().ok())
                .filter(|&n| n < 32)
                .map(Self::R)
                .ok_or_else(|| format!("no register `{reg}`; try r0 to r31, SREG, SPL or SPH")),
        }
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::R(n) => write!(f, "r{n}"),
            Self::Sreg => f.write_str("SREG"),
            Self::Spl => f.write_str("SPL"),
            Self::Sph => f.write_str("SPH"),
        }
    }
}

impl fmt::Display for RegisterFault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Self::Stuck { reg, bit, high } => write!(f, "reg:{reg}.{bit}={}", u8::from(high)),
            Self::Short {
                reg,
                bits: [a, b],
                high,
            } => write!(f, "reg:short:{reg}.{a}.{b}={}", u8::from(high)),
        }
    }
}

/// A watchdog that doesn't do what the datasheet says, for the proof in
/// `ts21::watchdog` to find. The chip has it from reset. Scripts write them
/// as `wdt:never`, and so on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WdtFault {
    /// It never resets the chip, however long it goes without a `wdr`.
    Never,
    /// Its timeouts run out in half the time.
    Early,
    /// `wdr` does nothing, so a timeout runs out however often it's fed.
    Deaf,
    /// After a `wdr` it never resets the chip again.
    Stops,
    /// A `wdr` in the closed window goes through as an ordinary feed.
    Open,
    /// The window never opens: any `wdr` with a window set resets the chip.
    Shut,
    /// The watchdog's fine, but SRAM doesn't survive its reset, so the marker
    /// comes back as junk. The datasheet doesn't rule that out.
    Wipes,
}

impl WdtFault {
    pub const ALL: [Self; 7] = [
        Self::Never,
        Self::Early,
        Self::Deaf,
        Self::Stops,
        Self::Open,
        Self::Shut,
        Self::Wipes,
    ];

    fn word(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Early => "early",
            Self::Deaf => "deaf",
            Self::Stops => "stops",
            Self::Open => "open",
            Self::Shut => "shut",
            Self::Wipes => "wipes",
        }
    }
}

impl FromStr for WdtFault {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|fault| fault.word() == s)
            .ok_or_else(|| {
                let words: Vec<&str> = Self::ALL.iter().map(|f| f.word()).collect();
                format!("no watchdog fault `{s}`; try {}", words.join(", "))
            })
    }
}

impl fmt::Display for WdtFault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "wdt:{}", self.word())
    }
}

/// A data-space address in SRAM, like `0x3800`.
fn sram_address(s: &str) -> Result<u16, String> {
    let addr = s
        .strip_prefix("0x")
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("expected an address like 0x3800, not `{s}`"))?;
    if !(reg::SRAM_START..reg::SRAM_END).contains(&usize::from(addr)) {
        return Err(format!(
            "{addr:#06x} isn't in SRAM, which is {:#06x} to {:#06x}",
            reg::SRAM_START,
            reg::SRAM_END - 1
        ));
    }
    Ok(addr)
}

fn bit_number(s: &str) -> Result<u8, String> {
    s.parse()
        .ok()
        .filter(|&b| b < 8)
        .ok_or_else(|| format!("expected a bit from 0 to 7, not `{s}`"))
}

/// A clock's speed in a script: `40%` of what it should be. The range keeps
/// a tick longer than `sim.rs`'s budget for a pass.
pub fn clock_percent(s: &str) -> Result<u16, String> {
    s.strip_suffix('%')
        .and_then(|n| n.parse().ok())
        .filter(|n| (25..=400).contains(n))
        .ok_or_else(|| format!("expected a speed from 25% to 400%, not `{s}`"))
}

/// `0` or `1`.
fn level(s: &str) -> Result<bool, String> {
    match s {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("a stuck bit reads 0 or 1, not `{s}`")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
    /// The cell, in centivolts, as the 0614 calibration would report it.
    pub cell: u16,
    pub kelvin: u16,
    /// Every conversion reads this, whatever it's measuring.
    pub stuck: Option<u16>,
    /// The fuse bytes. The firmware reads them once, at power-on, so only
    /// the first tick's count.
    pub fuses: [u8; FUSE_BYTES],
    /// Flash has a byte that doesn't match its CRC, as a half-finished
    /// flash would leave it. Like the fuses, only the first tick's counts.
    pub corrupt_flash: bool,
    /// A broken byte of SRAM. The chip has it from reset, so only the first
    /// tick's counts.
    pub ram: Option<RamFault>,
    /// A broken CPU register, from reset too.
    pub register: Option<RegisterFault>,
    /// A broken watchdog, from reset too.
    pub wdt: Option<WdtFault>,
    /// The switch is down for one boot of the watchdog's proof, counted from
    /// 0, and up again before it ends. Only the first tick's counts.
    pub proof_press: Option<u32>,
    /// The firmware's stuck, and stays stuck until the watchdog resets it.
    /// It's up to whoever's running the firmware to see that happens once.
    pub hang: bool,
    /// The 32 kHz oscillator, as a percentage of 32.768 kHz, 0 being dead.
    /// The PIT and the WDT run off it, so they go with it.
    pub osc32k: u16,
    /// The main clock, as a percentage of 5 MHz.
    pub main_clock: u16,
    /// TCA0's stopped counting, with the rest of the chip fine.
    pub tca_stopped: bool,
}

impl Default for World {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl World {
    /// A cell at 3.8 V, a room at 25 °C and factory fuses.
    pub const NORMAL: Self = Self {
        cell: 380,
        kelvin: 298,
        stuck: None,
        fuses: FACTORY_FUSES,
        corrupt_flash: false,
        ram: None,
        register: None,
        wdt: None,
        proof_press: None,
        hang: false,
        osc32k: 100,
        main_clock: 100,
        tca_stopped: false,
    };

    /// TCA0 overflows between two ticks in this world, to the nearest below,
    /// or `NO_TICK` with no ticks.
    pub fn overflows(&self) -> u16 {
        if self.osc32k == 0 {
            return NO_TICK;
        }
        if self.tca_stopped {
            return 0;
        }
        let cycles = 5_000_000 * u64::from(self.main_clock) / u64::from(self.osc32k);
        (cycles / u64::from(PWM_TOP) / u64::from(ts21::TICKS_PER_SECOND)) as u16
    }

    /// Whether the tick's so late that the firmware gives up on it, rather
    /// than waiting to count it.
    pub fn overdue(&self) -> bool {
        self.overflows() > OVERFLOWS_MAX
    }

    /// Whether src/rt.rs's startup tests find a broken register or byte of
    /// RAM, and stop there with the light off for good. The library never
    /// runs on a chip like that, so the rig plays dead instead.
    pub fn dead(&self) -> bool {
        self.ram.is_some() || self.register.is_some()
    }

    /// The raw reading a conversion on `channel` gives.
    pub fn reading(&self, channel: Channel) -> u16 {
        if let Some(stuck) = self.stuck {
            return stuck;
        }
        match channel {
            Channel::Voltage => voltage_reading(self.cell),
            Channel::Temperature => temperature_reading(self.kelvin),
        }
    }

    /// The fuse at a data-space address, if it's one of `fuses` and not the
    /// reserved byte, which the datasheet doesn't say how reads.
    pub fn fuse(&self, addr: usize) -> Option<u8> {
        let offset = addr.checked_sub(usize::from(map::FUSES))?;
        if FUSE_NAMES.get(offset)?.is_empty() {
            return None;
        }
        self.fuses.get(offset).copied()
    }

    /// What the power-on self-tests find in this world.
    pub fn faults(&self) -> Faults {
        let at = |addr| self.fuse(addr).unwrap_or(0xFF);
        let fuses = Fuses {
            wdtcfg: at(reg::FUSE_WDTCFG),
            osccfg: at(reg::FUSE_OSCCFG),
            syscfg0: at(reg::FUSE_SYSCFG0),
            append: at(reg::FUSE_APPEND),
            bootend: at(reg::FUSE_BOOTEND),
        };
        Faults {
            fuses: !fuses.ok(),
            flash: self.corrupt_flash,
            watchdog: self.wdt.is_some(),
        }
    }
}

/// The library's torch with an ADC that answers from a [`World`], the way
/// the firmware's loop drives it: the result of last tick's conversion in,
/// this tick's conversion out. A conversion measures the world as it was
/// when it started, as on the simulated chip.
#[derive(Clone, Debug)]
pub struct Rig {
    pub torch: Torch,
    adc: Adc,
    /// The startup tests failed, so nothing runs (`World::dead`).
    dead: bool,
    /// A press from the watchdog's proof, for the first tick to count.
    owed_press: bool,
    /// It's hung and been reset, which only happens the once.
    hung: bool,
    /// What the wait after the last tick counted, if it was a counted one.
    counted: Option<u16>,
}

impl Default for Rig {
    fn default() -> Self {
        Self::new(&World::NORMAL)
    }
}

impl Rig {
    /// A torch that's just had its cell put in, in `world`, which decides
    /// what its power-on self-tests find.
    pub fn new(world: &World) -> Self {
        let mut torch = Torch::new(CALIBRATION, world.faults());
        // `hw::init` gives up on a dead oscillator's sync flags.
        if world.osc32k == 0 {
            torch.clock(NO_TICK);
        }
        Self {
            torch,
            adc: Adc::new(),
            dead: world.dead(),
            owed_press: world.proof_press.is_some(),
            hung: false,
            counted: None,
        }
    }

    pub fn tick(&mut self, pressed: bool, world: &World) -> Option<ts21::button::Event> {
        if self.dead {
            return None;
        }
        if world.hang && !self.hung {
            // The watchdog's reset, which takes the torch back to off with
            // the FET refused. The library has no ticks to lose, so this is
            // at once; `sim.rs` waits for the bite and starts here from it.
            self.hung = true;
            self.adc = Adc::new();
            self.counted = None;
            let faults = Faults {
                watchdog: true,
                ..world.faults()
            };
            self.torch = Torch::new(CALIBRATION, faults);
        }
        let pressed = pressed || mem::take(&mut self.owed_press);
        if let Some(overflows) = self.counted.take() {
            self.torch.clock(overflows);
        } else if world.osc32k == 0 || world.overdue() && !self.torch.resting() {
            // At rest TCB0 is stopped, so a late tick goes unseen. A dead
            // oscillator stays off that way anyway, since no tick ever comes
            // to see a press.
            self.torch.clock(NO_TICK);
        }
        let event = self.torch.tick(pressed, self.adc.result());
        self.adc
            .start_with(self.torch.convert(), |channel| world.reading(channel));
        self.counted = self.torch.clock_due().then(|| world.overflows());
        event
    }

    pub fn level(&self) -> u8 {
        if self.dead { 0 } else { self.torch.level() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_adc_reads_the_same_on_both_channels() {
        let world = World {
            stuck: Some(0),
            ..World::NORMAL
        };
        assert_eq!(world.reading(Channel::Voltage), 0);
        assert_eq!(world.reading(Channel::Temperature), 0);
    }

    #[test]
    fn the_rig_hands_results_back_a_tick_later() {
        let mut rig = Rig::default();
        for _ in 0..1000 {
            rig.tick(false, &World::NORMAL);
        }
        assert_eq!(rig.torch.limits().ceilings(), (150, 150));
    }

    #[test]
    fn factory_fuses_pass_the_self_test() {
        assert_eq!(World::NORMAL.faults(), Faults::NONE);
    }

    #[test]
    fn the_fuse_names_match_the_addresses() {
        for (addr, name) in [
            (reg::FUSE_WDTCFG, "WDTCFG"),
            (reg::FUSE_OSCCFG, "OSCCFG"),
            (reg::FUSE_SYSCFG0, "SYSCFG0"),
            (reg::FUSE_APPEND, "APPEND"),
            (reg::FUSE_BOOTEND, "BOOTEND"),
        ] {
            assert_eq!(FUSE_NAMES[addr - usize::from(map::FUSES)], name);
        }
    }

    #[test]
    fn a_wrong_fuse_is_a_fault() {
        let mut world = World::NORMAL;
        world.fuses[2] = 0x01; // OSCCFG: 16 MHz
        assert_eq!(
            world.faults(),
            Faults {
                fuses: true,
                ..Faults::NONE
            }
        );
        assert_eq!(World::NORMAL.fuse(usize::from(map::FUSES) + 9), None);
        assert_eq!(
            World::NORMAL.fuse(usize::from(map::FUSES) + 3),
            None,
            "reserved"
        );
    }

    #[test]
    fn a_broken_watchdog_is_a_fault() {
        let world = World {
            wdt: Some(WdtFault::Early),
            ..World::NORMAL
        };
        assert!(world.faults().watchdog && world.faults().any());
    }

    #[test]
    fn a_hang_restarts_the_rig_off_and_only_once() {
        let mut rig = Rig::default();
        for pressed in [true, true, false] {
            rig.tick(pressed, &World::NORMAL);
        }
        for _ in 0..64 {
            rig.tick(false, &World::NORMAL);
        }
        assert!(rig.level() > 0);
        let hung = World {
            hang: true,
            ..World::NORMAL
        };
        rig.tick(false, &hung);
        assert_eq!(rig.level(), 0);
        for pressed in [true, true, false] {
            rig.tick(pressed, &hung);
        }
        for _ in 0..64 {
            rig.tick(false, &hung);
        }
        assert!(rig.level() > 0, "one hang, not one every tick");
    }

    #[test]
    fn a_press_from_the_proof_counts_on_the_first_tick() {
        let world = World {
            proof_press: Some(1),
            ..World::NORMAL
        };
        let mut rig = Rig::new(&world);
        for _ in 0..64 {
            rig.tick(false, &world);
        }
        assert!(rig.level() > 0);
    }

    #[test]
    fn a_stuck_ram_bit_reads_stuck_only_at_its_byte() {
        let stuck = RamFault::Stuck {
            addr: 0x3800,
            bit: 3,
            high: true,
        };
        assert_eq!(stuck.read(0x3800, 0x00), 0x08);
        assert_eq!(stuck.read(0x3801, 0x00), 0x00);
        let low = RamFault::Stuck {
            addr: 0x3800,
            bit: 0,
            high: false,
        };
        assert_eq!(low.read(0x3800, 0xFF), 0xFE);
    }

    #[test]
    fn an_alias_sends_one_address_to_another_byte() {
        let alias = RamFault::Alias {
            addr: 0x3900,
            to: 0x3800,
        };
        assert_eq!(alias.cell(0x3900), 0x3800);
        assert_eq!(alias.cell(0x3800), 0x3800);
        assert_eq!(alias.cell(0x3901), 0x3901);
    }

    #[test]
    fn a_bridge_only_shows_when_its_bits_differ() {
        let bridge = RamFault::Bridge {
            addr: 0x3800,
            bits: [0, 1],
        };
        for same in [0x00, 0xFF, 0x03, 0xFC] {
            assert_eq!(bridge.read(0x3800, same), same, "{same:#04x}");
        }
        assert_eq!(bridge.read(0x3800, 0x55), 0x54);
        assert_eq!(bridge.read(0x3800, 0xAA), 0xA8);
        assert_eq!(bridge.read(0x3801, 0x55), 0x55);
    }

    #[test]
    fn a_coupling_only_fires_on_its_own_edge() {
        let fault = |rising, force| RamFault::Coupling {
            from: 0x3900,
            bit: 7,
            rising,
            to: 0x3800,
            victim: 0,
            force,
        };
        let rising = fault(true, None);
        assert_eq!(rising.disturbs(0x3900, 0x00, 0x80), Some(0x3800));
        assert_eq!(rising.disturbs(0x3900, 0x80, 0x00), None, "falling");
        assert_eq!(rising.disturbs(0x3900, 0x80, 0xFF), None, "already high");
        assert_eq!(rising.disturbs(0x3901, 0x00, 0x80), None, "another byte");
        let falling = fault(false, None);
        assert_eq!(falling.disturbs(0x3900, 0x80, 0x00), Some(0x3800));
        assert_eq!(falling.disturbs(0x3900, 0x00, 0x80), None, "rising");
    }

    #[test]
    fn a_coupling_flips_or_forces_its_victim() {
        let fault = |force| RamFault::Coupling {
            from: 0x3900,
            bit: 7,
            rising: true,
            to: 0x3800,
            victim: 0,
            force,
        };
        assert_eq!(fault(None).disturbed(0x10), 0x11);
        assert_eq!(fault(None).disturbed(0x11), 0x10);
        assert_eq!(fault(Some(true)).disturbed(0x10), 0x11);
        assert_eq!(fault(Some(true)).disturbed(0x11), 0x11);
        assert_eq!(fault(Some(false)).disturbed(0x11), 0x10);
        assert_eq!(fault(Some(false)).disturbed(0x10), 0x10);
    }

    #[test]
    fn faults_print_the_way_scripts_write_them() {
        for word in [
            "ram:stuck:0x3800.3=1",
            "ram:stuck:0x3fff.0=0",
            "ram:alias:0x3900=0x3800",
            "ram:bridge:0x3800.0.1",
            "ram:couple:0x3900.7=1:0x3800.0",
            "ram:couple:0x3800.0=0:0x3800.1=1",
        ] {
            let fault: RamFault = word["ram:".len()..].parse().unwrap();
            assert_eq!(fault.to_string(), word);
        }
        for word in [
            "reg:r0.0=1",
            "reg:r31.7=0",
            "reg:SREG.1=1",
            "reg:SPL.0=0",
            "reg:SPH.5=1",
            "reg:short:r24.0.2=0",
            "reg:short:SREG.1.3=1",
        ] {
            let fault: RegisterFault = word["reg:".len()..].parse().unwrap();
            assert_eq!(fault.to_string(), word);
        }
    }

    #[test]
    fn refuses_faults_it_cant_place() {
        for ram in [
            "stuck:0x3800.3",
            "stuck:0x3800.8=1",
            "stuck:0x3800.3=2",
            "stuck:0x37ff.0=1",
            "stuck:0x4000.0=1",
            "stuck:3800.0=1",
            "alias:0x3800=0x3800",
            "alias:0x3800",
            "bridge:0x3800.1.1",
            "bridge:0x3800.1",
            "bridge:0x3800.0.1.2",
            "couple:0x3900.7=1",
            "couple:0x3900.7:0x3800.0",
            "couple:0x3900.7=2:0x3800.0",
            "couple:0x3900.7=1:0x3800",
            "couple:0x3900.7=1:0x3800.0=2",
            "couple:0x3800.0=1:0x3800.0",
            "flip:0x3800.0",
            "",
        ] {
            assert!(ram.parse::<RamFault>().is_err(), "ram:{ram} parsed");
        }
        for register in [
            "r32.0=1",
            "r24.0",
            "x24.0=1",
            "r24.9=1",
            "r24.0=x",
            "short:r24.0.2",
            "short:r24.0=1",
            "short:r24.2.2=1",
            "short:r32.0.2=1",
        ] {
            assert!(
                register.parse::<RegisterFault>().is_err(),
                "reg:{register} parsed"
            );
        }
        assert!("sometimes".parse::<WdtFault>().is_err());
    }

    #[test]
    fn watchdog_faults_print_the_way_scripts_write_them() {
        for fault in WdtFault::ALL {
            let word = fault.to_string();
            assert_eq!(word["wdt:".len()..].parse(), Ok(fault));
        }
    }

    #[test]
    fn a_stuck_register_bit_is_forced() {
        let stuck = |reg, bit, high| RegisterFault::Stuck { reg, bit, high };
        let mut cpu = Cpu::new(&(0x3800..0x4000));
        stuck(Register::R(24), 7, true).apply(&mut cpu);
        assert_eq!(cpu.r[24], 0x80);
        assert!(cpu.r.iter().enumerate().all(|(i, &v)| i == 24 || v == 0));
        stuck(Register::Sreg, 1, true).apply(&mut cpu);
        assert_eq!(cpu.sreg, 0x02);
        stuck(Register::Sph, 0, false).apply(&mut cpu);
        assert_eq!(cpu.sp, 0x3EFF);
        stuck(Register::Spl, 7, false).apply(&mut cpu);
        assert_eq!(cpu.sp, 0x3E7F);
    }

    #[test]
    fn a_register_short_only_shows_when_its_bits_differ() {
        let after = |high, value| {
            let mut cpu = Cpu::new(&(0x3800..0x4000));
            cpu.r[24] = value;
            RegisterFault::Short {
                reg: Register::R(24),
                bits: [0, 2],
                high,
            }
            .apply(&mut cpu);
            cpu.r[24]
        };
        for same in [0x00, 0x05, 0x55, 0xAA, 0xFF] {
            assert_eq!(after(false, same), same, "{same:#04x}");
            assert_eq!(after(true, same), same, "{same:#04x}");
        }
        assert_eq!(after(false, 0x33), 0x32);
        assert_eq!(after(true, 0x33), 0x37);
        assert_eq!(after(false, 0xCC), 0xC8);
        assert_eq!(after(true, 0xCC), 0xCD);
    }

    #[test]
    fn a_broken_ram_or_register_leaves_the_rig_dead() {
        for world in [
            World {
                ram: Some(RamFault::Alias {
                    addr: 0x3900,
                    to: 0x3800,
                }),
                ..World::NORMAL
            },
            World {
                register: Some(RegisterFault::Stuck {
                    reg: Register::R(0),
                    bit: 0,
                    high: true,
                }),
                ..World::NORMAL
            },
        ] {
            assert!(world.dead());
            let mut rig = Rig::new(&world);
            for pressed in [true, true, true, true, false, false] {
                assert_eq!(rig.tick(pressed, &world), None);
            }
            assert_eq!(rig.level(), 0);
        }
        assert!(!World::NORMAL.dead());
    }

    #[test]
    fn overflows_follow_the_two_clocks() {
        let world = |osc32k, main_clock| World {
            osc32k,
            main_clock,
            ..World::NORMAL
        };
        assert_eq!(World::NORMAL.overflows(), 306);
        assert_eq!(world(50, 100).overflows(), 612);
        assert_eq!(world(100, 200).overflows(), 612);
        assert_eq!(world(200, 100).overflows(), 153);
        assert_eq!(world(0, 100).overflows(), NO_TICK);
        let stopped = World {
            tca_stopped: true,
            ..World::NORMAL
        };
        assert_eq!(stopped.overflows(), 0);
        assert!(world(0, 100).overdue() && world(50, 100).overdue());
        assert!(!World::NORMAL.overdue() && !stopped.overdue());
    }

    #[test]
    fn the_rig_turns_off_for_a_clock_that_goes_wrong() {
        for world in [
            World {
                osc32k: 0,
                ..World::NORMAL
            },
            World {
                main_clock: 400,
                ..World::NORMAL
            },
            World {
                osc32k: 200,
                ..World::NORMAL
            },
            World {
                tca_stopped: true,
                ..World::NORMAL
            },
        ] {
            let mut rig = Rig::default();
            for pressed in [true, true, true, true, false]
                .into_iter()
                .chain([false; 80])
            {
                rig.tick(pressed, &World::NORMAL);
            }
            assert_eq!(rig.level(), 50);
            for _ in 0..80 {
                rig.tick(false, &world);
            }
            assert_eq!(rig.level(), 0);
            assert!(rig.torch.clock_failed());
        }
    }

    #[test]
    fn reads_clock_speeds() {
        assert_eq!(clock_percent("40%"), Ok(40));
        for bad in ["40", "24%", "401%", "%", "x%"] {
            assert!(clock_percent(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fuses_that_stop_a_real_chip_are_refused() {
        assert!(unmodelled_fuse(0, 0x0B).is_some(), "WDTCFG.PERIOD");
        assert!(unmodelled_fuse(0, 0x10).is_none(), "WDTCFG.WINDOW alone");
        assert!(unmodelled_fuse(5, 0x04).is_some(), "CRCSRC FLASH");
        assert!(unmodelled_fuse(5, 0xC4).is_none(), "CRCSRC NOCRC");
        assert!(unmodelled_fuse(2, 0x01).is_none(), "OSCCFG");
    }
}
