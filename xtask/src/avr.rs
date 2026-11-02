//! An AVR CPU, the AVRxt flavour in the tinyAVR 1-series, so the real
//! firmware can run on the host. `sim.rs` puts a fake ATtiny1616 around it.
//!
//! It counts instructions, not cycles, and never takes an interrupt: the
//! firmware doesn't enable any yet. Anything it doesn't decode is an error
//! naming the opcode, so a new instruction from a compiler upgrade shows up
//! as a clear failure rather than a wrong answer. Encodings and flag rules
//! come from Microchip's AVR Instruction Set Manual (DS40002198).

use std::ops::Range;

/// CPU.SPL, CPU.SPH and CPU.SREG. The CPU keeps these itself rather than
/// passing them to the bus. atdf.rs checks the addresses.
pub const SPL: u16 = 0x3D;
pub const SPH: u16 = 0x3E;
pub const SREG: u16 = 0x3F;

// SREG bits.
const C: u8 = 0;
const Z: u8 = 1;
const N: u8 = 2;
const V: u8 = 3;
const S: u8 = 4;
const H: u8 = 5;
const T: u8 = 6;

/// Everything outside the CPU: program memory and data space.
pub trait Bus {
    /// The program word at word address `pc`, if there's flash there.
    fn fetch(&self, pc: u32) -> Option<u16>;
    fn read(&mut self, addr: u16) -> Result<u8, String>;
    fn write(&mut self, addr: u16, value: u8) -> Result<(), String>;
}

pub struct Cpu {
    pub r: [u8; 32],
    /// A word address, as the hardware counts it.
    pub pc: u32,
    pub sp: u16,
    pub sreg: u8,
}

impl Cpu {
    /// The state after reset: PC at 0, SP at the top of SRAM, SREG clear.
    pub fn new(sram: &Range<u16>) -> Self {
        Self {
            r: [0; 32],
            pc: 0,
            sp: sram.end - 1,
            sreg: 0,
        }
    }

    /// Runs one instruction. Errors carry the instruction's byte address.
    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), String> {
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
                    2 | 3 => return Err(no_multiplier(w)),
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
                    self.set(((w >> 4) & 7) as u8, w & 0x80 == 0);
                }
                0x8 => match w {
                    0x9508 => self.pc = self.pop_pc(bus)?, // ret
                    // reti. With no interrupts to return from, the firmware
                    // only gets here by jumping somewhere it shouldn't.
                    0x9518 => return Err("reti outside an interrupt".into()),
                    0x95A8 => {} // wdr: there's no watchdog to reset
                    0x95C8 => self.r[0] = self.program_byte(bus, self.pair(30))?, // lpm
                    // sleep needs interrupts to wake from, which aren't modelled yet.
                    0x9588 => return Err("sleep, which the simulator doesn't model".into()),
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
            _ => return Err(no_multiplier(w)),
        }
        Ok(())
    }

    fn flag(&self, bit: u8) -> bool {
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
        match addr {
            SPL => self.sp = self.sp & 0xFF00 | u16::from(value),
            SPH => self.sp = self.sp & 0x00FF | u16::from(value) << 8,
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

fn no_multiplier(w: u16) -> String {
    format!(
        "multiply opcode {w:#06x}, but the ATtiny1616 has no hardware multiplier (docs/hardware.md)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAM: Range<u16> = 0x3800..0x4000;

    struct TestBus {
        flash: Vec<u16>,
        data: Vec<u8>,
    }

    impl Bus for TestBus {
        fn fetch(&self, pc: u32) -> Option<u16> {
            self.flash.get(pc as usize).copied()
        }
        fn read(&mut self, addr: u16) -> Result<u8, String> {
            Ok(self.data[usize::from(addr)])
        }
        fn write(&mut self, addr: u16, value: u8) -> Result<(), String> {
            self.data[usize::from(addr)] = value;
            Ok(())
        }
    }

    /// Runs `program` until PC reaches its end.
    fn run(program: &[u16], setup: impl FnOnce(&mut Cpu, &mut TestBus)) -> (Cpu, TestBus) {
        let mut bus = TestBus {
            flash: program.to_vec(),
            data: vec![0; 0x10000],
        };
        let mut cpu = Cpu::new(&RAM);
        setup(&mut cpu, &mut bus);
        for _ in 0..1000 {
            if cpu.pc as usize >= program.len() {
                return (cpu, bus);
            }
            cpu.step(&mut bus).unwrap();
        }
        panic!("program didn't finish");
    }

    fn ldi(d: u16, k: u16) -> u16 {
        0xE000 | (k & 0xF0) << 4 | (d - 16) << 4 | k & 0x0F
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
    fn refuses_multiplies_and_unknown_opcodes() {
        let mut bus = TestBus {
            flash: vec![0x9F01, 0xFFFF],
            data: vec![0; 0x10000],
        };
        let mut cpu = Cpu::new(&RAM);
        assert!(
            cpu.step(&mut bus)
                .unwrap_err()
                .contains("no hardware multiplier")
        );
        assert!(
            cpu.step(&mut bus)
                .unwrap_err()
                .contains("0x0002: opcode 0xffff")
        );
        assert!(cpu.step(&mut bus).unwrap_err().contains("outside flash"));
    }
}
