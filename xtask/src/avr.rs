//! An AVR CPU, the AVRxt flavour in the tinyAVR 1-series, so the real
//! firmware can run on the host. `sim.rs` puts a fake ATtiny1616 around it.
//!
//! It counts instructions, not cycles. Anything it doesn't decode is an
//! error naming the opcode, so a new instruction from a compiler upgrade
//! shows up as a clear failure rather than a wrong answer. Encodings and
//! flag rules come from Microchip's AVR Instruction Set Manual (DS40002198).
//!
//! Interrupts work the AVRxt way (DS40002204A §13, and §8.7.3 for SREG.I;
//! the instruction set manual's `reti` says it sets I, and its AVRxt note
//! only covers CPUINT.STATUS): with SREG.I set, the CPU takes the lowest-numbered pending vector
//! the bus offers, pushes PC and jumps to the vector's slot, two words each
//! on a 16 KB part. Taking one doesn't clear I and `reti` doesn't set it;
//! what stops a second level-0 interrupt nesting is CPUINT.STATUS.LVL0EX,
//! which is set on the way in and cleared by `reti`. The instruction after
//! `sei` always runs before a pending interrupt, which is what lets
//! `sei; sleep` close the race AN2515C §4.5 describes. Only level 0 is
//! modelled: no NMI, no level 1 vector and no round robin.
//!
//! Between instructions, then, an interrupt waits while SREG.I is clear,
//! while LVL0EX is set, for the instruction after `sei`, for four
//! instructions after a write to SPL or until the next I/O write if that's
//! sooner (§8.5.4, §8.7.2), and for as long as the bus says
//! (`holds_interrupts`, which is the CCP window, §8.5.7). Each of those
//! leaves the request pending. `out SREG` setting I gets no hold, since the
//! instruction set manual only promises one for `sei`, though §8.7.3 gives
//! that write a one-cycle wait state and doesn't say whether it delays an
//! interrupt, so that's likely, not known. None of this has been seen on a
//! real chip.
//!
//! `sleep` is an error with interrupts off or inside a handler, since
//! nothing could wake the CPU, and otherwise asks the bus whether it would
//! sleep and what could wake it. Asleep, each step either takes a pending
//! interrupt or lets the bus move time on.

use std::ops::Range;

use ts21::regs::addr;

/// CPU.SPL, CPU.SPH and CPU.SREG. The CPU keeps these itself rather than
/// passing them to the bus.
pub const SPL: u16 = addr::CPU_SPL as u16;
pub const SPH: u16 = addr::CPU_SPH as u16;
pub const SREG: u16 = addr::CPU_SREG as u16;

// SREG bits.
const C: u8 = 0;
const Z: u8 = 1;
const N: u8 = 2;
const V: u8 = 3;
const S: u8 = 4;
const H: u8 = 5;
const T: u8 = 6;
/// SREG.I, global interrupt enable.
pub const I: u8 = 7;

/// Words in each vector slot. Parts with more than 8 KB of flash use a
/// two-word `jmp` per slot: vector n is at word 2n (DS40002204A table 7-2).
pub const VECTOR_WORDS: u32 = 2;

/// Instructions a write to SPL holds interrupts off for (DS40002204A §8.5.4).
const SPL_HOLD: u8 = 4;
/// Where I/O memory ends, the 64 I/O registers and the extended ones (§6.2,
/// the memory map). A write below this ends SPL's hold early.
const IO_END: u16 = 0x1000;

/// Everything outside the CPU: program memory, data space and the
/// interrupt lines.
pub trait Bus {
    /// The program word at word address `pc`, if there's flash there.
    fn fetch(&self, pc: u32) -> Option<u16>;
    fn read(&mut self, addr: u16) -> Result<u8, String>;
    fn write(&mut self, addr: u16, value: u8) -> Result<(), String>;
    /// The `wdr` instruction: the program fed the watchdog. An error means
    /// the real chip would have reset.
    fn wdr(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// The highest-priority interrupt that's both enabled and flagged, as a
    /// vector number. The CPU checks SREG.I and LVL0EX itself.
    fn interrupt(&self) -> Option<u8>;
    /// Whether something outside the CPU is holding interrupts off for now.
    /// Requests stay pending until it stops.
    fn holds_interrupts(&self) -> bool {
        false
    }
    /// The CPU has run `sleep` with interrupts on. Errors if the chip
    /// wouldn't sleep, or would sleep with nothing enabled to wake it.
    fn sleep(&mut self) -> Result<(), String>;
    /// The CPU's asleep and nothing's pending: move time on to the next
    /// thing that happens.
    fn idle(&mut self) -> Result<(), String>;
}

pub struct Cpu {
    pub r: [u8; 32],
    /// A word address, as the hardware counts it.
    pub pc: u32,
    pub sp: u16,
    pub sreg: u8,
    /// Stopped by `sleep`, until an interrupt.
    pub asleep: bool,
    /// The vector whose handler is running, which is CPUINT.STATUS.LVL0EX:
    /// no other level-0 interrupt is taken until its `reti`.
    pub handling: Option<u8>,
    /// Set by `sei`: the next instruction runs before any interrupt.
    hold: bool,
    /// Set by a write to SPL: instructions still to run before any interrupt.
    spl_hold: u8,
    /// The SP bits that exist. The datasheet says unused ones read 0
    /// without saying which (DS40002204A §8.7.2), so this keeps only the
    /// ones SRAM needs, the fewest it could be.
    sp_bits: u16,
}

impl Cpu {
    /// The state after reset: PC at 0, SP at the top of SRAM, SREG clear.
    pub fn new(sram: &Range<u16>) -> Self {
        Self {
            r: [0; 32],
            pc: 0,
            sp: sram.end - 1,
            sreg: 0,
            asleep: false,
            handling: None,
            hold: false,
            spl_hold: 0,
            sp_bits: sram.end.next_power_of_two() - 1,
        }
    }

    /// Takes a pending interrupt, or runs one instruction, or sleeps on.
    /// Errors carry the instruction's byte address.
    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), String> {
        let hold = std::mem::take(&mut self.hold);
        let spl_hold = self.spl_hold;
        self.spl_hold = spl_hold.saturating_sub(1);
        if self.flag(I)
            && self.handling.is_none()
            && !hold
            && spl_hold == 0
            && !bus.holds_interrupts()
            && let Some(vector) = bus.interrupt()
        {
            // Asleep, PC already points past the `sleep`, which is where
            // the handler returns to.
            self.asleep = false;
            self.push_pc(bus)
                .map_err(|e| format!("taking interrupt {vector}: {e}"))?;
            self.pc = u32::from(vector) * VECTOR_WORDS;
            self.handling = Some(vector);
            return Ok(());
        }
        if self.asleep {
            return bus.idle().map_err(|e| format!("asleep: {e}"));
        }
        let at = self.pc;
        let w = bus
            .fetch(at)
            .ok_or_else(|| format!("{:#06x}: jumped outside flash", at * 2))?;
        self.pc += 1;
        self.execute(bus, w)
            .map_err(|e| format!("{:#06x}: {e}", at * 2))
    }

    fn execute(&mut self, bus: &mut impl Bus, w: u16) -> Result<(), String> {
        // Operand fields, most instructions use one or two of these.
        let d = usize::from((w >> 4) & 0x1F);
        let r = usize::from(w & 0x0F | (w >> 5) & 0x10);
        let hi = 16 + usize::from((w >> 4) & 0x0F);
        let k = (((w >> 4) & 0xF0) | (w & 0x0F)) as u8;
        let (rd, rr) = (self.r[d], self.r[r]);
        let carry = self.flag(C);

        match w >> 12 {
            0x0 => match (w >> 10) & 3 {
                0 => match (w >> 8) & 3 {
                    0 if w == 0 => {} // nop
                    1 => {
                        // movw
                        let (d, r) = (usize::from((w >> 4) & 0xF) * 2, usize::from(w & 0xF) * 2);
                        self.r[d] = self.r[r];
                        self.r[d + 1] = self.r[r + 1];
                    }
                    2 => {
                        // muls, r16 to r31 only.
                        let (a, b) = (self.r[hi] as i8, self.r[16 + usize::from(w & 0xF)] as i8);
                        self.product((i16::from(a) * i16::from(b)) as u16, false);
                    }
                    3 => {
                        // mulsu, fmul, fmuls and fmulsu, r16 to r23 only.
                        let (a, b) = (
                            self.r[16 + usize::from((w >> 4) & 7)],
                            self.r[16 + usize::from(w & 7)],
                        );
                        // Which operands are signed, and whether it's fractional.
                        let (sa, sb, fractional) = match w & 0x88 {
                            0x00 => (true, false, false), // mulsu
                            0x08 => (false, false, true), // fmul
                            0x80 => (true, true, true),   // fmuls
                            _ => (true, false, true),     // fmulsu
                        };
                        let a = if sa { i16::from(a as i8) } else { i16::from(a) };
                        let b = if sb { i16::from(b as i8) } else { i16::from(b) };
                        // 255 * 255 doesn't fit an i16, but its bits are right.
                        self.product(a.wrapping_mul(b) as u16, fractional);
                    }
                    _ => return Err(unknown(w)),
                },
                1 => {
                    self.sub(rd, rr, carry, true); // cpc
                }
                2 => self.r[d] = self.sub(rd, rr, carry, true), // sbc
                _ => self.r[d] = self.add(rd, rr, false),       // add, lsl
            },
            0x1 => match (w >> 10) & 3 {
                0 => {
                    if rd == rr {
                        self.skip(bus)?; // cpse
                    }
                }
                1 => {
                    self.sub(rd, rr, false, false); // cp
                }
                2 => self.r[d] = self.sub(rd, rr, false, false), // sub
                _ => self.r[d] = self.add(rd, rr, carry),        // adc, rol
            },
            0x2 => match (w >> 10) & 3 {
                0 => self.r[d] = self.logic(rd & rr), // and, tst
                1 => self.r[d] = self.logic(rd ^ rr), // eor, clr
                2 => self.r[d] = self.logic(rd | rr), // or
                _ => self.r[d] = rr,                  // mov
            },
            0x3 => {
                self.sub(self.r[hi], k, false, false); // cpi
            }
            0x4 => self.r[hi] = self.sub(self.r[hi], k, carry, true), // sbci
            0x5 => self.r[hi] = self.sub(self.r[hi], k, false, false), // subi
            0x6 => self.r[hi] = self.logic(self.r[hi] | k),           // ori
            0x7 => self.r[hi] = self.logic(self.r[hi] & k),           // andi
            0x8 | 0xA => {
                // ldd/std Y+q or Z+q, which with q = 0 are plain ld/st Y or Z.
                let q = ((w >> 8) & 0x20) | ((w >> 7) & 0x18) | (w & 7);
                let base = if w & 8 != 0 {
                    self.pair(28)
                } else {
                    self.pair(30)
                };
                let addr = base.wrapping_add(q);
                if w & 0x0200 != 0 {
                    self.store(bus, addr, rd)?;
                } else {
                    self.r[d] = self.load(bus, addr)?;
                }
            }
            0x9 => self.execute_9(bus, w, d)?,
            0xB => {
                // in/out: I/O address A is the same as its data address.
                let a = ((w >> 5) & 0x30) | (w & 0x0F);
                if w & 0x0800 != 0 {
                    self.store(bus, a, rd)?;
                } else {
                    self.r[d] = self.load(bus, a)?;
                }
            }
            0xC => self.jump_relative(w), // rjmp
            0xD => {
                // rcall
                self.push_pc(bus)?;
                self.jump_relative(w);
            }
            0xE => self.r[hi] = k, // ldi
            _ => {
                let bit = (w & 7) as u8;
                match (w >> 9) & 7 {
                    0..=3 => {
                        // brbs/brbc: bit 10 picks "branch if clear".
                        if self.flag(bit) != (w & 0x0400 != 0) {
                            let k = (((w >> 3) & 0x7F) as i8) << 1 >> 1;
                            self.pc = self.pc.wrapping_add_signed(i32::from(k));
                        }
                    }
                    4 if w & 8 == 0 => {
                        // bld
                        let mask = 1 << bit;
                        self.r[d] = if self.flag(T) { rd | mask } else { rd & !mask };
                    }
                    5 if w & 8 == 0 => self.set(T, rd & 1 << bit != 0), // bst
                    6 | 7 if w & 8 == 0 => {
                        // sbrc/sbrs: bit 9 picks "skip if set".
                        if (rd & 1 << bit != 0) == (w & 0x0200 != 0) {
                            self.skip(bus)?;
                        }
                    }
                    _ => return Err(unknown(w)),
                }
            }
        }
        Ok(())
    }

    /// The 1001 xxxx block: loads and stores, one-operand maths, jumps and
    /// calls, word maths and single-bit I/O.
    fn execute_9(&mut self, bus: &mut impl Bus, w: u16, d: usize) -> Result<(), String> {
        let rd = self.r[d];
        match (w >> 9) & 7 {
            0 => {
                self.r[d] = match w & 0xF {
                    0 => {
                        let addr = self.next_word(bus)?; // lds
                        self.load(bus, addr)?
                    }
                    0x1 | 0x2 => self.indirect_load(bus, 30, w)?, // ld Z+, -Z
                    0x9 | 0xA => self.indirect_load(bus, 28, w)?, // ld Y+, -Y
                    0xC..=0xE => self.indirect_load(bus, 26, w)?, // ld X, X+, -X
                    0x4 | 0x5 => {
                        // lpm Rd, Z(+)
                        let z = self.pair(30);
                        if w & 1 != 0 {
                            self.set_pair(30, z.wrapping_add(1));
                        }
                        self.program_byte(bus, z)?
                    }
                    0xF => self.pop(bus)?,
                    _ => return Err(unknown(w)),
                };
            }
            1 => match w & 0xF {
                0 => {
                    let addr = self.next_word(bus)?; // sts
                    self.store(bus, addr, rd)?;
                }
                0x1 | 0x2 => self.indirect_store(bus, 30, w, rd)?,
                0x9 | 0xA => self.indirect_store(bus, 28, w, rd)?,
                0xC..=0xE => self.indirect_store(bus, 26, w, rd)?,
                0xF => self.push(bus, rd)?,
                _ => return Err(unknown(w)),
            },
            2 => match w & 0xF {
                0x0 => {
                    // com
                    self.set(C, true);
                    self.r[d] = self.logic(!rd);
                }
                0x1 => self.r[d] = self.sub(0, rd, false, false), // neg
                0x2 => self.r[d] = rd.rotate_left(4),             // swap
                0x3 => {
                    // inc
                    let v = rd.wrapping_add(1);
                    self.set(V, v == 0x80);
                    self.r[d] = self.nzs(v);
                }
                0x5 => self.r[d] = self.shift(((rd as i8) >> 1) as u8, rd & 1 != 0), // asr
                0x6 => self.r[d] = self.shift(rd >> 1, rd & 1 != 0),                 // lsr
                0x7 => {
                    // ror
                    let v = rd >> 1 | u8::from(self.flag(C)) << 7;
                    self.r[d] = self.shift(v, rd & 1 != 0);
                }
                0xA => {
                    // dec
                    let v = rd.wrapping_sub(1);
                    self.set(V, v == 0x7F);
                    self.r[d] = self.nzs(v);
                }
                0x8 if w & 0x0100 == 0 => {
                    // bset/bclr, which covers sei, cli, sec, clc and friends.
                    let (bit, on) = (((w >> 4) & 7) as u8, w & 0x80 == 0);
                    self.set(bit, on);
                    if bit == I && on {
                        self.hold = true; // sei
                    }
                }
                0x8 => match w {
                    0x9508 => self.pc = self.pop_pc(bus)?, // ret
                    0x9518 => {
                        // reti. It leaves SREG.I alone on AVRxt.
                        if self.handling.take().is_none() {
                            return Err("reti outside an interrupt handler".into());
                        }
                        self.pc = self.pop_pc(bus)?;
                    }
                    0x95A8 => bus.wdr()?,
                    0x95C8 => self.r[0] = self.program_byte(bus, self.pair(30))?, // lpm
                    0x9588 => self.sleep(bus)?,
                    _ => return Err(unknown(w)),
                },
                0x9 => match w {
                    0x9409 => self.pc = u32::from(self.pair(30)), // ijmp
                    0x9509 => {
                        // icall
                        self.push_pc(bus)?;
                        self.pc = u32::from(self.pair(30));
                    }
                    _ => return Err(unknown(w)),
                },
                0xC..=0xF => {
                    // jmp (110k) and call (111k): a 22-bit word address.
                    let high = u32::from(w & 0x01F0) << 13 | u32::from(w & 1) << 16;
                    let target = high | u32::from(self.next_word(bus)?);
                    if w & 2 != 0 {
                        self.push_pc(bus)?;
                    }
                    self.pc = target;
                }
                _ => return Err(unknown(w)),
            },
            3 => {
                // adiw/sbiw on r25:r24, X, Y or Z.
                let reg = 24 + 2 * usize::from((w >> 4) & 3);
                let k = ((w >> 2) & 0x30) | (w & 0x0F);
                let a = self.pair(reg);
                let (v, overflow, borrow) = if w & 0x0100 == 0 {
                    let v = a.wrapping_add(k);
                    (v, !a & v & 0x8000 != 0, !v & a & 0x8000 != 0)
                } else {
                    let v = a.wrapping_sub(k);
                    (v, a & !v & 0x8000 != 0, v & !a & 0x8000 != 0)
                };
                self.set_pair(reg, v);
                self.set(V, overflow);
                self.set(C, borrow);
                self.set(N, v & 0x8000 != 0);
                self.set(Z, v == 0);
                self.set(S, self.flag(N) != self.flag(V));
            }
            4 | 5 => {
                // cbi, sbic, sbi, sbis on the first 32 I/O addresses.
                let a = (w >> 3) & 0x1F;
                let mask = 1u8 << (w & 7);
                let value = self.load(bus, a)?;
                match (w >> 8) & 3 {
                    0 => self.store(bus, a, value & !mask)?,
                    2 => self.store(bus, a, value | mask)?,
                    // sbic (1) and sbis (3)
                    op => {
                        if (value & mask != 0) == (op == 3) {
                            self.skip(bus)?;
                        }
                    }
                }
            }
            _ => {
                // mul: 1001 11rd dddd rrrr, any registers.
                let r = usize::from(w & 0x0F | (w >> 5) & 0x10);
                self.product(u16::from(rd) * u16::from(self.r[r]), false);
            }
        }
        Ok(())
    }

    /// `sleep`. With interrupts off, or inside a handler where LVL0EX holds
    /// every other interrupt off, nothing could wake the CPU.
    fn sleep(&mut self, bus: &mut impl Bus) -> Result<(), String> {
        if !self.flag(I) {
            return Err("sleeps with SREG.I clear, so no interrupt can wake it".into());
        }
        if let Some(vector) = self.handling {
            return Err(format!(
                "sleeps inside the handler for interrupt {vector}, so no other can wake it"
            ));
        }
        bus.sleep()?;
        self.asleep = true;
        Ok(())
    }

    pub fn flag(&self, bit: u8) -> bool {
        self.sreg & 1 << bit != 0
    }

    fn set(&mut self, bit: u8, on: bool) {
        if on {
            self.sreg |= 1 << bit;
        } else {
            self.sreg &= !(1 << bit);
        }
    }

    fn pair(&self, lo: usize) -> u16 {
        u16::from_le_bytes([self.r[lo], self.r[lo + 1]])
    }

    fn set_pair(&mut self, lo: usize, value: u16) {
        [self.r[lo], self.r[lo + 1]] = value.to_le_bytes();
    }

    /// Sets N, Z and S from a result. V must already be set.
    fn nzs(&mut self, v: u8) -> u8 {
        self.set(N, v & 0x80 != 0);
        self.set(Z, v == 0);
        self.set(S, self.flag(N) != self.flag(V));
        v
    }

    fn logic(&mut self, v: u8) -> u8 {
        self.set(V, false);
        self.nzs(v)
    }

    fn shift(&mut self, v: u8, carry: bool) -> u8 {
        self.set(C, carry);
        self.set(N, v & 0x80 != 0);
        self.set(V, self.flag(N) != carry);
        self.nzs(v)
    }

    fn add(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let c = u8::from(carry);
        let v = a.wrapping_add(b).wrapping_add(c);
        self.set(H, (a & 0xF) + (b & 0xF) + c > 0xF);
        self.set(V, !(a ^ b) & (a ^ v) & 0x80 != 0);
        self.set(C, u16::from(a) + u16::from(b) + u16::from(c) > 0xFF);
        self.nzs(v)
    }

    /// `a - b - carry`. With `chain` set (sbc, sbci, cpc), Z only stays set
    /// if it already was, so multi-byte compares work.
    fn sub(&mut self, a: u8, b: u8, carry: bool, chain: bool) -> u8 {
        let c = u8::from(carry);
        let v = a.wrapping_sub(b).wrapping_sub(c);
        let zero = v == 0 && (!chain || self.flag(Z));
        self.set(H, (b & 0xF) + c > a & 0xF);
        self.set(V, (a ^ b) & (a ^ v) & 0x80 != 0);
        self.set(C, u16::from(b) + u16::from(c) > u16::from(a));
        self.nzs(v);
        self.set(Z, zero);
        v
    }

    /// Every multiply leaves its result in r1:r0, with C from bit 15 and Z
    /// set if it's zero. The fractional ones (fmul, fmuls, fmulsu) shift the
    /// result left one place, and C comes from bit 15 before the shift.
    fn product(&mut self, p: u16, fractional: bool) {
        let v = if fractional { p << 1 } else { p };
        self.set_pair(0, v);
        self.set(C, p & 0x8000 != 0);
        self.set(Z, v == 0);
    }

    fn jump_relative(&mut self, w: u16) {
        let k = ((w & 0x0FFF) as i16) << 4 >> 4;
        self.pc = self.pc.wrapping_add_signed(i32::from(k));
    }

    fn next_word(&mut self, bus: &impl Bus) -> Result<u16, String> {
        let w = bus
            .fetch(self.pc)
            .ok_or("instruction runs off the end of flash")?;
        self.pc += 1;
        Ok(w)
    }

    /// Steps over the next instruction, which may be two words long.
    fn skip(&mut self, bus: &impl Bus) -> Result<(), String> {
        let next = self.next_word(bus)?;
        if is_two_words(next) {
            self.pc += 1;
        }
        Ok(())
    }

    fn program_byte(&self, bus: &impl Bus, addr: u16) -> Result<u8, String> {
        let w = bus
            .fetch(u32::from(addr >> 1))
            .ok_or_else(|| format!("lpm from {addr:#06x}, outside flash"))?;
        Ok(w.to_le_bytes()[usize::from(addr & 1)])
    }

    /// Data-space loads and stores, with SP and SREG kept in the CPU.
    fn load(&mut self, bus: &mut impl Bus, addr: u16) -> Result<u8, String> {
        match addr {
            SPL => Ok(self.sp.to_le_bytes()[0]),
            SPH => Ok(self.sp.to_le_bytes()[1]),
            SREG => Ok(self.sreg),
            _ => bus.read(addr),
        }
    }

    fn store(&mut self, bus: &mut impl Bus, addr: u16, value: u8) -> Result<(), String> {
        if addr < IO_END {
            self.spl_hold = 0;
        }
        match addr {
            SPL => {
                self.sp = (self.sp & 0xFF00 | u16::from(value)) & self.sp_bits;
                self.spl_hold = SPL_HOLD;
            }
            SPH => self.sp = (self.sp & 0x00FF | u16::from(value) << 8) & self.sp_bits,
            SREG => self.sreg = value,
            _ => bus.write(addr, value)?,
        }
        Ok(())
    }

    /// The pointer register's post-increment (low bit 1) and pre-decrement
    /// (low bit 2) forms. X's plain form (0xC) leaves it alone.
    fn step_pointer(&mut self, reg: usize, w: u16) -> u16 {
        let p = self.pair(reg);
        match w & 3 {
            1 => {
                self.set_pair(reg, p.wrapping_add(1));
                p
            }
            2 => {
                let p = p.wrapping_sub(1);
                self.set_pair(reg, p);
                p
            }
            _ => p,
        }
    }

    fn indirect_load(&mut self, bus: &mut impl Bus, reg: usize, w: u16) -> Result<u8, String> {
        let addr = self.step_pointer(reg, w);
        self.load(bus, addr)
    }

    fn indirect_store(
        &mut self,
        bus: &mut impl Bus,
        reg: usize,
        w: u16,
        value: u8,
    ) -> Result<(), String> {
        let addr = self.step_pointer(reg, w);
        self.store(bus, addr, value)
    }

    fn push(&mut self, bus: &mut impl Bus, value: u8) -> Result<(), String> {
        self.store(bus, self.sp, value)?;
        self.sp = self.sp.wrapping_sub(1);
        Ok(())
    }

    fn pop(&mut self, bus: &mut impl Bus) -> Result<u8, String> {
        self.sp = self.sp.wrapping_add(1);
        self.load(bus, self.sp)
    }

    /// Return addresses are two bytes on a 16 KB part, low byte pushed first.
    fn push_pc(&mut self, bus: &mut impl Bus) -> Result<(), String> {
        let [lo, hi] = (self.pc as u16).to_le_bytes();
        self.push(bus, lo)?;
        self.push(bus, hi)
    }

    fn pop_pc(&mut self, bus: &mut impl Bus) -> Result<u32, String> {
        let hi = self.pop(bus)?;
        let lo = self.pop(bus)?;
        Ok(u32::from(u16::from_le_bytes([lo, hi])))
    }
}

/// `lds`, `sts`, `jmp` and `call` take a second word.
pub fn is_two_words(w: u16) -> bool {
    w & 0xFC0F == 0x9000 || w & 0xFE0C == 0x940C
}

fn unknown(w: u16) -> String {
    format!("opcode {w:#06x}, which the simulator doesn't decode")
}

#[cfg(test)]
mod tests {
    use ts21::regs::mem;

    use super::*;

    const RAM: Range<u16> = mem::INTERNAL_SRAM_START as u16..mem::INTERNAL_SRAM_END as u16;

    /// A data address the test programs' handlers write to: each write is
    /// logged, and clears those bits of `pending`, like an INTFLAGS.
    const FLAGS: u16 = 0x00FF;
    /// A data address that adds what's written to it to `pending`, so a
    /// test program can flag an interrupt partway through.
    const RAISE: u16 = 0x00FE;

    struct TestBus {
        flash: Vec<u16>,
        data: Vec<u8>,
        /// How many `wdr`s the program ran.
        wdrs: u32,
        /// Interrupts flagged, a bit per vector. All count as enabled.
        pending: u32,
        /// `holds_interrupts`, as the CCP window would.
        held: bool,
        /// What the handlers wrote to `FLAGS`.
        log: Vec<u8>,
        /// Bits of `pending` that time flags each `idle`.
        wake: u32,
        sleeps: u32,
        idles: u32,
    }

    impl TestBus {
        fn new(flash: &[u16]) -> Self {
            Self {
                flash: flash.to_vec(),
                data: vec![0; 0x10000],
                wdrs: 0,
                pending: 0,
                held: false,
                log: Vec::new(),
                wake: 0,
                sleeps: 0,
                idles: 0,
            }
        }
    }

    impl Bus for TestBus {
        fn wdr(&mut self) -> Result<(), String> {
            self.wdrs += 1;
            Ok(())
        }
        fn fetch(&self, pc: u32) -> Option<u16> {
            self.flash.get(pc as usize).copied()
        }
        fn read(&mut self, addr: u16) -> Result<u8, String> {
            Ok(self.data[usize::from(addr)])
        }
        fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
            if addr == FLAGS {
                self.log.push(value);
                self.pending &= !u32::from(value);
            }
            if addr == RAISE {
                self.pending |= u32::from(value);
            }
            self.data[usize::from(addr)] = value;
            Ok(())
        }
        fn interrupt(&self) -> Option<u8> {
            (self.pending != 0).then(|| self.pending.trailing_zeros() as u8)
        }
        fn holds_interrupts(&self) -> bool {
            self.held
        }
        fn sleep(&mut self) -> Result<(), String> {
            self.sleeps += 1;
            Ok(())
        }
        fn idle(&mut self) -> Result<(), String> {
            if self.wake == 0 {
                return Err("nothing to wake on".into());
            }
            self.idles += 1;
            self.pending |= self.wake;
            Ok(())
        }
    }

    /// Runs `program` until PC reaches its end.
    fn run(program: &[u16], setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> (Cpu, TestBus) {
        try_run(program, setup).unwrap()
    }

    fn try_run(
        program: &[u16],
        setup: impl FnOnce(&mut Cpu, &mut TestBus),
    ) -> Result<(Cpu, TestBus), String> {
        let mut bus = TestBus::new(program);
        let mut cpu = Cpu::new(&RAM);
        setup(&mut cpu, &mut bus);
        for _ in 0..1000 {
            if cpu.pc as usize >= program.len() {
                return Ok((cpu, bus));
            }
            cpu.step(&mut bus)?;
        }
        panic!("program didn't finish");
    }

    fn ldi(d: u16, k: u16) -> u16 {
        0xE000 | (k & 0xF0) << 4 | (d - 16) << 4 | k & 0x0F
    }

    #[test]
    fn wdr_tells_the_bus() {
        let (cpu, bus) = run(&[0x95A8], |_, _| {});
        assert_eq!(bus.wdrs, 1);
        assert_eq!(cpu.pc, 1);
    }

    #[test]
    fn adds_with_carry_across_bytes() {
        // r25:r24 = 0x01FF; r23:r22 = 0x0001; add r24,r22; adc r25,r23
        let (cpu, _) = run(
            &[
                ldi(24, 0xFF),
                ldi(25, 0x01),
                ldi(22, 1),
                ldi(23, 0),
                0x0F86,
                0x1F97,
            ],
            |_, _| {},
        );
        assert_eq!(cpu.pair(24), 0x0200);
        assert!(!cpu.flag(C) && !cpu.flag(Z));
    }

    #[test]
    fn sets_overflow_and_half_carry_on_add() {
        // 0x7F + 0x01 = 0x80: signed overflow, half carry, negative.
        let (cpu, _) = run(&[ldi(16, 0x7F), ldi(17, 1), 0x0F01], |_, _| {});
        assert_eq!(cpu.r[16], 0x80);
        assert!(cpu.flag(V) && cpu.flag(H) && cpu.flag(N) && !cpu.flag(S) && !cpu.flag(C));
    }

    #[test]
    fn compares_16_bit_values_with_cp_and_cpc() {
        // cp r24,r22; cpc r25,r23 with equal high bytes but different low.
        let program = [0x1786, 0x0797];
        let (cpu, _) = run(&program, |cpu, _| {
            cpu.set_pair(24, 0x3801);
            cpu.set_pair(22, 0x3800);
        });
        assert!(!cpu.flag(Z), "cpc keeps Z clear once cp clears it");
        let (cpu, _) = run(&program, |cpu, _| {
            cpu.set_pair(24, 0x3800);
            cpu.set_pair(22, 0x3800);
        });
        assert!(cpu.flag(Z));
        let (cpu, _) = run(&program, |cpu, _| {
            cpu.set_pair(24, 0x37FF);
            cpu.set_pair(22, 0x3800);
        });
        assert!(cpu.flag(C), "borrow when smaller");
    }

    #[test]
    fn subtracts_immediates_and_borrows() {
        // subi r16,1 on 0 gives 0xFF with carry; sbci r17,0 carries it on.
        let (cpu, _) = run(&[ldi(16, 0), ldi(17, 1), 0x5001, 0x4010], |_, _| {});
        assert_eq!((cpu.r[16], cpu.r[17]), (0xFF, 0x00));
        assert!(!cpu.flag(Z), "sbci only keeps Z if it was already set");
    }

    #[test]
    fn adiw_and_sbiw_work_on_word_pairs() {
        // adiw r24,1 then sbiw r30,2
        let (cpu, _) = run(&[0x9601, 0x9732], |cpu, _| {
            cpu.set_pair(24, 0x00FF);
            cpu.set_pair(30, 0x0001);
        });
        assert_eq!(cpu.pair(24), 0x0100);
        assert_eq!(cpu.pair(30), 0xFFFF);
        assert!(cpu.flag(C) && cpu.flag(N));
    }

    #[test]
    fn shifts_and_rotates_through_carry() {
        // lsr r24; ror r25 moves bit 0 of r24 into bit 7 of r25.
        let (cpu, _) = run(&[ldi(24, 0x03), ldi(25, 0), 0x9586, 0x9597], |_, _| {});
        assert_eq!((cpu.r[24], cpu.r[25]), (0x01, 0x80));
        // asr keeps the sign bit.
        let (cpu, _) = run(&[ldi(24, 0x80), 0x9585], |_, _| {});
        assert_eq!(cpu.r[24], 0xC0);
    }

    #[test]
    fn loads_and_stores_through_pointers() {
        // X = 0x3800: st X+, r16; st X+, r17; then Z = 0x3802: ld r18, -Z
        let program = [
            ldi(26, 0x00),
            ldi(27, 0x38),
            ldi(16, 0xAA),
            ldi(17, 0xBB),
            0x930D,
            0x931D,
            ldi(30, 0x02),
            ldi(31, 0x38),
            0x9122,
        ];
        let (cpu, bus) = run(&program, |_, _| {});
        assert_eq!(&bus.data[0x3800..0x3802], &[0xAA, 0xBB]);
        assert_eq!(cpu.pair(26), 0x3802);
        assert_eq!((cpu.r[18], cpu.pair(30)), (0xBB, 0x3801));
    }

    #[test]
    fn uses_displacement_from_y_and_z() {
        // std Y+5, r16; ldd r17, Z+5 with Y = Z.
        let (cpu, bus) = run(&[0x830D, 0x8115], |cpu, _| {
            cpu.r[16] = 0x42;
            cpu.set_pair(28, 0x3810);
            cpu.set_pair(30, 0x3810);
        });
        assert_eq!(bus.data[0x3815], 0x42);
        assert_eq!(cpu.r[17], 0x42);
    }

    #[test]
    fn calls_and_returns_through_the_stack() {
        // 0: rcall +2; 1: ldi r16,1; 2: rjmp +2; 3: ldi r17,2; 4: ret
        let program = [0xD002, ldi(16, 1), 0xC002, ldi(17, 2), 0x9508];
        let (cpu, bus) = run(&program, |_, _| {});
        assert_eq!((cpu.r[16], cpu.r[17]), (1, 2));
        assert_eq!(cpu.sp, RAM.end - 1, "the stack is balanced");
        assert_eq!(&bus.data[0x3FFE..0x4000], &[0x00, 0x01], "return address 1");
    }

    #[test]
    fn calls_and_jumps_with_two_word_instructions() {
        // 0: call 4; 2: jmp 6; 4: ldi r16,7; 5: ret; 6: (end)
        let (cpu, _) = run(&[0x940E, 4, 0x940C, 6, ldi(16, 7), 0x9508], |_, _| {});
        assert_eq!(cpu.r[16], 7);
    }

    #[test]
    fn skips_over_a_whole_two_word_instruction() {
        // cpse r16,r16 skips the sts; then ldi r17,1 runs.
        let (cpu, bus) = run(&[0x1300, 0x9300, 0x3800, ldi(17, 1)], |cpu, _| {
            cpu.r[16] = 9
        });
        assert_eq!(bus.data[0x3800], 0, "sts was skipped");
        assert_eq!(cpu.r[17], 1);
    }

    #[test]
    fn branches_backwards_on_flags() {
        // r16 = 3; loop: dec r16; brne loop
        let (cpu, _) = run(&[ldi(16, 3), 0x950A, 0xF7F1], |_, _| {});
        assert_eq!(cpu.r[16], 0);
        assert!(cpu.flag(Z));
    }

    #[test]
    fn keeps_sp_and_sreg_in_the_cpu() {
        // in r28,SPL; in r29,SPH; out SREG,r1 (r1 = 0); out SPL,r16
        let (cpu, bus) = run(&[0xB7CD, 0xB7DE, 0xBE1F, 0xBF0D], |cpu, _| {
            cpu.sreg = 0xFF;
            cpu.r[16] = 0x80;
        });
        assert_eq!(cpu.pair(28), 0x3FFF);
        assert_eq!(cpu.sreg, 0);
        assert_eq!(cpu.sp, 0x3F80);
        assert!(
            bus.data[0x3D..0x40].iter().all(|&b| b == 0),
            "never reaches the bus"
        );
    }

    #[test]
    fn reads_program_memory() {
        // lpm r16, Z+; lpm r17, Z+; rjmp over the data word at byte 6.
        let (cpu, _) = run(&[0x9105, 0x9115, 0xC001, 0x1234], |cpu, _| {
            cpu.set_pair(30, 6)
        });
        assert_eq!((cpu.r[16], cpu.r[17]), (0x34, 0x12));
    }

    #[test]
    fn bit_instructions() {
        // bst r16,3; bld r17,7; sbrs r17,7; ldi r18,1 (skipped); sbi 0x05,2
        let (cpu, bus) = run(&[0xFB03, 0xF917, 0xFF17, ldi(18, 1), 0x9A2A], |cpu, _| {
            cpu.r[16] = 0x08;
        });
        assert_eq!((cpu.r[17], cpu.r[18]), (0x80, 0));
        assert_eq!(bus.data[0x05], 0x04);
    }

    #[test]
    fn sp_only_has_the_bits_sram_needs() {
        // ldi r16, 0xFF; out SPH, r16; out SPL, r16
        let (cpu, _) = run(&[ldi(16, 0xFF), 0xBF0E, 0xBF0D], |_, _| {});
        assert_eq!(cpu.sp, 0x3FFF);
    }

    #[test]
    fn refuses_unknown_opcodes() {
        // 0x0001 sits next to nop, movw and the multiplies.
        let mut bus = TestBus::new(&[0x0001, 0xFFFF]);
        let mut cpu = Cpu::new(&RAM);
        assert!(
            cpu.step(&mut bus)
                .unwrap_err()
                .contains("0x0000: opcode 0x0001")
        );
        assert!(
            cpu.step(&mut bus)
                .unwrap_err()
                .contains("0x0002: opcode 0xffff")
        );
        assert!(cpu.step(&mut bus).unwrap_err().contains("outside flash"));
    }

    /// Runs one multiply with `a` in its Rd and `b` in its Rr, and returns
    /// r1:r0, C and Z. r1:r0 start non-zero, so Z has to be earned.
    fn multiply(w: u16, (d, a): (usize, u8), (r, b): (usize, u8)) -> (u16, bool, bool) {
        let (cpu, _) = run(&[w], |cpu, _| {
            cpu.set_pair(0, 0xA5A5);
            cpu.r[d] = a;
            cpu.r[r] = b;
        });
        (cpu.pair(0), cpu.flag(C), cpu.flag(Z))
    }

    // The multiply words in these tests come from LLVM's AVR assembler (llc
    // -mcpu=attiny1616, then llvm-objdump), as a check on the decoding.

    #[test]
    fn mul_is_unsigned_on_any_registers() {
        // mul r22, r24
        assert_eq!(
            multiply(0x9F68, (22, 200), (24, 100)),
            (20_000, false, false)
        );
        assert_eq!(
            multiply(0x9F68, (22, 0xFF), (24, 0xFF)),
            (0xFE01, true, false)
        );
        assert_eq!(multiply(0x9F68, (22, 0), (24, 0x80)), (0, false, true));
        // mul r1, r31 reads r1 before overwriting it.
        assert_eq!(multiply(0x9E1F, (1, 3), (31, 5)), (15, false, false));
        // mul r24, r24
        assert_eq!(multiply(0x9F88, (24, 16), (24, 16)), (256, false, false));
    }

    #[test]
    fn muls_is_signed_on_r16_to_r31() {
        // muls r16, r31
        let muls = |a: i8, b: i8| multiply(0x020F, (16, a as u8), (31, b as u8));
        assert_eq!(muls(-128, -128), (0x4000, false, false));
        assert_eq!(muls(-1, -1), (1, false, false));
        assert_eq!(muls(-1, 1), (0xFFFF, true, false));
        assert_eq!(muls(127, -128), (-16_256i16 as u16, true, false));
        assert_eq!(muls(0, -5), (0, false, true));
        // muls r22, r25
        assert_eq!(
            multiply(0x0269, (22, 0xFE), (25, 3)),
            (-6i16 as u16, true, false)
        );
    }

    #[test]
    fn mulsu_is_signed_by_unsigned_on_r16_to_r23() {
        // mulsu r17, r22
        let mulsu = |a: i8, b: u8| multiply(0x0316, (17, a as u8), (22, b));
        assert_eq!(mulsu(-1, 255), (-255i16 as u16, true, false));
        assert_eq!(mulsu(-128, 255), (-32_640i16 as u16, true, false));
        assert_eq!(mulsu(127, 255), (32_385, false, false));
        assert_eq!(mulsu(2, 0x80), (256, false, false), "Rr isn't signed");
        assert_eq!(mulsu(-128, 0), (0, false, true));
    }

    #[test]
    fn fmul_shifts_left_and_takes_c_from_before_the_shift() {
        // fmul r18, r21. 0.5 * 0.5 in 1.7 format is 0.25, 0x2000 in 1.15.
        let fmul = |a: u8, b: u8| multiply(0x032D, (18, a), (21, b));
        assert_eq!(fmul(0x40, 0x40), (0x2000, false, false));
        // 1.5 * 1.5 doesn't fit: the product 0x9000 loses bit 15 to C.
        assert_eq!(fmul(0xC0, 0xC0), (0x2000, true, false));
        assert_eq!(fmul(0xFF, 0xFF), (0xFC02, true, false));
        // The product 0x4000 shifts into bit 15, but C comes from before.
        assert_eq!(fmul(0x80, 0x80), (0x8000, false, false));
        assert_eq!(fmul(0x80, 0), (0, false, true));
    }

    #[test]
    fn fmuls_and_fmulsu_are_signed() {
        // fmuls r23, r16. -1 * -1 in 1.7 format overflows to -1 in 1.15.
        let fmuls = |a: i8, b: i8| multiply(0x03F0, (23, a as u8), (16, b as u8));
        assert_eq!(fmuls(-128, -128), (0x8000, false, false));
        assert_eq!(fmuls(-1, -1), (2, false, false));
        assert_eq!(fmuls(-1, 1), (0xFFFE, true, false));
        assert_eq!(fmuls(0, -1), (0, false, true));
        // fmulsu r20, r19
        let fmulsu = |a: i8, b: u8| multiply(0x03CB, (20, a as u8), (19, b));
        assert_eq!(fmulsu(-1, 255), (-510i16 as u16, true, false));
        assert_eq!(fmulsu(64, 0x80), (0x4000, false, false), "Rr isn't signed");
        // -128 * 128 is 0xC000, which shifts to 0x8000.
        assert_eq!(fmulsu(-128, 0x80), (0x8000, true, false));
        assert_eq!(fmulsu(0, 0x80), (0, false, true));
    }

    /// What the pinned nightly compiles `a.wrapping_mul(b)` on two `u16`s,
    /// and `a % 10` on a `u8`, to for the ATtiny1616, with the `ret` left off.
    #[test]
    fn runs_what_llvm_emits_for_u16_multiply_and_u8_remainder() {
        let mul_u16 = [
            0x0269, // muls r22, r25
            0x2D90, // mov r25, r0
            0x2411, // clr r1
            0x9F68, // mul r22, r24
            0x2D20, // mov r18, r0
            0x2D31, // mov r19, r1
            0x2411, // clr r1
            0x0F39, // add r19, r25
            0x0278, // muls r23, r24
            0x2411, // clr r1
            0x2D80, // mov r24, r0
            0x0F83, // add r24, r19
            0x2F48, // mov r20, r24
            0x2755, // clr r21
            0x2F54, // mov r21, r20
            0x2744, // clr r20
            0x2F82, // mov r24, r18
            0x2799, // clr r25
            0x2B84, // or r24, r20
            0x2B95, // or r25, r21
        ];
        let rem_u8 = [
            0xEC9D, // ldi r25, 0xcd
            0x9F89, // mul r24, r25
            0x2D91, // mov r25, r1
            0x2411, // clr r1
            0x9596, // lsr r25
            0x9596, // lsr r25
            0x9596, // lsr r25
            0xE02A, // ldi r18, 0xa
            0x0292, // muls r25, r18
            0x2411, // clr r1
            0x1980, // sub r24, r0
        ];
        let values = [
            0u16, 1, 2, 9, 10, 99, 0x7F, 0x80, 0xFF, 0x100, 0x1234, 0x7FFF, 0x8000, 0xFFFF,
        ];
        for a in values {
            for b in values {
                let (cpu, _) = run(&mul_u16, |cpu, _| {
                    cpu.set_pair(24, a);
                    cpu.set_pair(22, b);
                });
                assert_eq!(cpu.pair(24), a.wrapping_mul(b), "{a} * {b}");
                assert_eq!(cpu.r[1], 0, "r1 is back to zero");
            }
        }
        for a in 0..=u8::MAX {
            let (cpu, _) = run(&rem_u8, |cpu, _| cpu.r[24] = a);
            assert_eq!(cpu.r[24], a % 10, "{a} % 10");
        }
    }

    const NOP: u16 = 0x0000;
    const SEI: u16 = 0x9478;
    const CLI: u16 = 0x94F8;
    const SLEEP: u16 = 0x9588;
    const RETI: u16 = 0x9518;
    /// sts FLAGS, r16
    const LOG_R16: [u16; 2] = [0x9300, FLAGS];

    fn rjmp(from: usize, to: usize) -> u16 {
        0xC000 | (to as i32 - from as i32 - 1) as u16 & 0x0FFF
    }

    /// A vector table for vectors 0 to 7, two words a slot, with `rjmp`s to
    /// `handlers` placed after it and `main` last, so `run` stops when
    /// `main` finishes. Reset jumps to `main`.
    fn program(handlers: &[(u8, &[u16])], main: &[u16]) -> Vec<u16> {
        let mut words = vec![NOP; 16];
        for (vector, body) in handlers {
            let slot = usize::from(*vector) * VECTOR_WORDS as usize;
            words[slot] = rjmp(slot, words.len());
            words.extend_from_slice(body);
        }
        words[0] = rjmp(0, words.len());
        words.extend_from_slice(main);
        words
    }

    /// A handler that logs `bits` to FLAGS, clearing those interrupts.
    fn clears(bits: u8) -> Vec<u16> {
        let mut body = vec![ldi(16, u16::from(bits))];
        body.extend(LOG_R16);
        body.push(RETI);
        body
    }

    #[test]
    fn takes_the_lowest_vector_first_without_nesting() {
        // Vector 3 copies r17 to r19, logs, then sets r18. Vector 7 copies
        // r18 to r20. Both are pending from the start.
        let mut three = vec![0x2F31]; // mov r19, r17
        three.extend(clears(1 << 3));
        three.insert(4, ldi(18, 1));
        let mut seven = vec![0x2F42]; // mov r20, r18
        seven.extend(clears(1 << 7));
        let image = program(&[(3, &three), (7, &seven)], &[SEI, ldi(17, 1), NOP]);
        let (cpu, bus) = run(&image, |_, bus| bus.pending = 1 << 3 | 1 << 7);

        assert_eq!(bus.log, [1 << 3, 1 << 7], "3 first, then 7");
        assert_eq!(cpu.r[19], 1, "the instruction after sei ran first");
        assert_eq!(cpu.r[20], 1, "7 waited for 3's reti (LVL0EX)");
        assert!(cpu.flag(I), "taking an interrupt leaves I set");
        assert_eq!((cpu.sp, cpu.handling), (RAM.end - 1, None));
    }

    #[test]
    fn takes_nothing_with_sreg_i_clear() {
        let image = program(&[(3, &clears(1 << 3))], &[ldi(17, 1), NOP]);
        let (_, bus) = run(&image, |_, bus| bus.pending = 1 << 3);
        assert!(bus.log.is_empty());
    }

    const INC_R17: u16 = 0x9513;
    /// out SPL, r16
    const OUT_SPL: u16 = 0xBF0D;
    /// out SPH, r18
    const OUT_SPH: u16 = 0xBF2E;

    /// Runs `main` with vector 3 pending from reset, under a handler that
    /// copies r17 to r19, and says how many `inc r17`s ran before it. r16 and
    /// r18 hold SP where it is.
    fn incs_before_the_handler(main: &[u16]) -> u8 {
        let mut handler = vec![0x2F31]; // mov r19, r17
        handler.extend(clears(1 << 3));
        let image = program(&[(3, &handler)], main);
        let (cpu, bus) = run(&image, |cpu, bus| {
            bus.pending = 1 << 3;
            [cpu.r[16], cpu.r[18]] = cpu.sp.to_le_bytes();
        });
        assert_eq!(bus.log, [1 << 3], "the handler ran");
        cpu.r[19]
    }

    #[test]
    fn a_write_to_spl_holds_interrupts_off_for_four_instructions() {
        // sei's own hold covers the `out`, which starts the four.
        assert_eq!(
            incs_before_the_handler(&[SEI, OUT_SPL, INC_R17, INC_R17, INC_R17, INC_R17, INC_R17]),
            4
        );
    }

    #[test]
    fn the_next_io_write_ends_spls_hold() {
        assert_eq!(
            incs_before_the_handler(&[SEI, OUT_SPL, INC_R17, OUT_SPH, INC_R17, INC_R17]),
            1,
            "SPH"
        );
        // sts 0x001C, r18
        let main = [SEI, OUT_SPL, 0x9320, 0x001C, INC_R17, INC_R17];
        assert_eq!(incs_before_the_handler(&main), 0, "any I/O register");
        // sts 0x3800, r18 is data memory, so it only uses up one of the four.
        let main = [
            SEI, OUT_SPL, 0x9320, 0x3800, INC_R17, INC_R17, INC_R17, INC_R17,
        ];
        assert_eq!(incs_before_the_handler(&main), 3, "SRAM isn't I/O");
    }

    #[test]
    fn writing_spl_again_starts_the_hold_again() {
        let main = [
            SEI, OUT_SPL, INC_R17, INC_R17, OUT_SPL, INC_R17, INC_R17, INC_R17, INC_R17, INC_R17,
        ];
        assert_eq!(incs_before_the_handler(&main), 6);
    }

    /// `out SREG` isn't `sei`: only `sei` promises the next instruction.
    #[test]
    fn setting_i_through_sreg_takes_a_pending_interrupt_straight_away() {
        // ldi r20, 0x80; out SREG, r20
        let main = [ldi(20, 0x80), 0xBF4F, INC_R17, INC_R17];
        assert_eq!(incs_before_the_handler(&main), 0);
    }

    /// Flags vector 3 through `RAISE`, then runs `body`.
    fn raises(body: &[u16]) -> Vec<u16> {
        // sts RAISE, r20
        let mut words = vec![ldi(20, 1 << 3), 0x9340, RAISE];
        words.extend_from_slice(body);
        words
    }

    /// Mid-program, with the CPU awake, the handler lands between two
    /// instructions and the program carries on from where it was.
    #[test]
    fn an_interrupt_lands_between_two_instructions_awake() {
        let main = [&[SEI, INC_R17][..], &raises(&[INC_R17, INC_R17])].concat();
        assert_eq!(incs_before_the_handler_raised(&main), (1, 3));
    }

    #[test]
    fn an_interrupt_flagged_with_sreg_i_clear_waits_for_sei_and_one_more() {
        let main = [
            &[SEI, CLI][..],
            &raises(&[INC_R17, INC_R17, SEI, INC_R17, INC_R17, INC_R17]),
        ]
        .concat();
        assert_eq!(incs_before_the_handler_raised(&main), (3, 5));
    }

    /// Like `incs_before_the_handler`, with nothing pending until `main`
    /// raises it. Also says how many `inc`s ran in all.
    fn incs_before_the_handler_raised(main: &[u16]) -> (u8, u8) {
        let mut handler = vec![0x2F31]; // mov r19, r17
        handler.extend(clears(1 << 3));
        let (cpu, bus) = run(&program(&[(3, &handler)], main), |_, _| {});
        assert_eq!(bus.log, [1 << 3], "the handler ran");
        (cpu.r[19], cpu.r[17])
    }

    #[test]
    fn the_bus_can_hold_interrupts_off() {
        let mut handler = vec![0x2F31]; // mov r19, r17
        handler.extend(clears(1 << 3));
        // Words: sei, nop, inc, inc, then the bus lets go.
        let image = program(&[(3, &handler)], &[SEI, NOP, INC_R17, INC_R17, NOP, NOP]);
        let mut bus = TestBus::new(&image);
        let mut cpu = Cpu::new(&RAM);
        bus.pending = 1 << 3;
        bus.held = true;
        for _ in 0..5 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.handling, None, "still pending");
        assert_eq!(cpu.r[17], 2);
        bus.held = false;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.handling, Some(3));
    }

    #[test]
    fn reti_leaves_sreg_i_alone_and_needs_a_handler_to_return_from() {
        let mut handler = vec![CLI];
        handler.extend(clears(1 << 3));
        let image = program(&[(3, &handler)], &[SEI, NOP, NOP]);
        let (cpu, bus) = run(&image, |_, bus| bus.pending = 1 << 3);
        assert_eq!(bus.log, [1 << 3]);
        assert!(!cpu.flag(I), "the handler's cli sticks");

        let err = try_run(&[RETI], |_, _| {}).err().unwrap();
        assert!(err.contains("outside an interrupt handler"), "{err}");
    }

    #[test]
    fn sleeps_until_an_interrupt_then_carries_on_after_the_sleep() {
        let image = program(&[(7, &clears(1 << 7))], &[SEI, SLEEP, ldi(17, 1)]);
        let (cpu, bus) = run(&image, |_, bus| bus.wake = 1 << 7);
        assert_eq!((bus.sleeps, bus.idles), (1, 1));
        assert_eq!(bus.log, [1 << 7]);
        assert_eq!(cpu.r[17], 1);
        assert!(!cpu.asleep);
    }

    /// AN2515C §4.5: with interrupts off, check the flag, then `sei; sleep`.
    /// An interrupt that's arrived since the check is taken straight after
    /// the `sleep`, so it wakes the CPU at once instead of being slept
    /// through.
    #[test]
    fn sei_then_sleep_wakes_at_once_for_an_interrupt_already_pending() {
        let image = program(&[(7, &clears(1 << 7))], &[SEI, SLEEP, ldi(17, 1)]);
        let (cpu, bus) = run(&image, |_, bus| bus.pending = 1 << 7);
        assert_eq!(bus.sleeps, 1, "sleep ran before the interrupt");
        assert_eq!(bus.idles, 0, "and didn't wait for anything else");
        assert_eq!((bus.log.as_slice(), cpu.r[17]), ([1 << 7].as_slice(), 1));
    }

    #[test]
    fn refuses_to_sleep_with_nothing_able_to_wake_it() {
        let err = try_run(&[SLEEP], |_, _| {}).err().unwrap();
        assert!(err.contains("SREG.I clear"), "{err}");

        let image = program(&[(3, &[SLEEP])], &[SEI, NOP, NOP]);
        let err = try_run(&image, |_, bus| bus.pending = 1 << 3)
            .err()
            .unwrap();
        assert!(err.contains("inside the handler for interrupt 3"), "{err}");

        // The bus has the last word: here, nothing will ever flag.
        let image = program(&[], &[SEI, SLEEP, NOP]);
        let err = try_run(&image, |_, _| {}).err().unwrap();
        assert!(err.contains("asleep: nothing to wake on"), "{err}");
    }
}
