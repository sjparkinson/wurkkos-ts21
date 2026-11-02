# Roadmap

Roughly in order. Each step should leave a torch that's no worse than the step before. Tick things off here in the commit that does them.

## 1. It lights up

- [x] Toolchain with nightly `avr-none`, `rust-lld`, own linker script and vector table, no crates
- [x] Pin map and registers for the TS21 v2 driver
- [x] Switch sampling at 64 Hz and a click/hold decoder
- [x] Two-channel ramp (linear then FET), 150 levels
- [x] Click on, click off, at one level on the linear channel
- [x] A built-in UPDI programmer in `xtask` (ping, backup, flash, restore), tested against a fake chip
- [ ] Ping, back up and flash a real TS21, which is also the first real test of the programmer
- [ ] Confirm the batched UPDI frames work on a real chip. `write_page` now sends the RSD toggle, REPEAT, the page data and the RSD toggle back in one frame, and `read` sends REPEAT and LD together. The fake chip is happy with both; if a real flash fails partway through a page, split them back into separate sends first.
- [ ] See whether the `wait_nvm` after `NVM_PAGE_BUFFER_CLEAR` ever sees the controller busy. pymcuprog waits there, so keep it unless a real chip shows the first poll is always idle. It's one round trip per page.
- [ ] Check PB5 is the button LED and decide what it does
- [ ] Bring back the full UI from the `fancy-ui` branch, with ramping, moonlight, turbo and lockout

## 2. It's safe to leave a cell in

None of this is optional for a light with a direct-drive FET on a 21700.

- [ ] Sleep when off, using standby sleep with the switch's pin-change interrupt to wake (PA5 is already set to interrupt on both edges). PA5 isn't one of the fully asynchronous pins (only pins 2 and 6 are), but the datasheet (DS40002204A, table 16-3) says a partially asynchronous pin still wakes the chip with the peripheral clock stopped when its sense is BOTHEDGES or LEVEL. The catch is that the new level has to last until the clock restarts, or the chip wakes with no interrupt. A press lasts far longer than that, but the safe pattern is to read PA5 on every wake whether or not the interrupt fired. This means the first real interrupt handler, so `src/rt.rs` grows named vectors and Rust needs `extern "avr-interrupt"` (another nightly feature). Anduril also slows the PIT in standby.
- [ ] Sleep between ticks when on, using the RTC PIT interrupt in place of the busy-wait. `hw::set_output` already only runs when the output changes (`torch::Changes`), so the loop no longer rewrites the TCA0 compare buffers every tick or resets `TCA0_CNT` every tick while off.
- [ ] Battery voltage, with the ADC measuring the internal reference with VCC as its reference (Anduril uses the 1.5 V reference and 4× accumulation), then the 0614 calibration (slope 1010/1024, +0.30 V) as a starting point. The maths is done: `battery::centivolts` matches the formula exactly for every 16-bit reading, with no multiply and no 32-bit divide. Still to do: the ADC setup and a thin read in `src/hw.rs` (VREF's ADC0REFSEL at 1.5 V, MUXPOS INTREF, REFSEL VDDREF, SAMPNUM ACC4), the registers in `src/regs.rs` with ATDF checks, an ADC model in `xtask/src/sim.rs`, and sharing the ADC with the temperature sensor.
- [ ] Low-voltage protection, stepping down below 3.10 V and turning off below 2.99 V. The Wurkkos cell is unprotected and rated down to 2.50 V, and [hardware.md](hardware.md#the-driver) has the evidence. The logic is done: `battery::Protection` takes one voltage sample every 16 ticks (4 Hz, since the ADC alternates with the temperature sensor every 8 ticks) and filters them. After five seconds below 3.10 V it steps the ceiling down 15 levels. After a second below 2.99 V it turns off, or, if the light is on the FET, where sag can fake a low reading, drops to the linear channel first. It also turns off when a step-down would go below level 1, and steps the ceiling back up at 3.30 V. Still to do: feed it from the ADC and apply its verdict in `src/torch.rs` (cap the level, or turn the UI off), with rows in docs/ui.md.
- [ ] Thermal regulation, using the internal temperature sensor (calibrated from `SIGROW.TEMPSENSE0` and `TEMPSENSE1`), a target temperature (Anduril defaults to 45 °C), and a controller that steps down without visible jumps. The calibration maths is done: `temp::kelvin` gets the datasheet's answer exactly with 16-bit multiplies, tested against every 10-bit reading and calibration. [hardware.md](hardware.md#the-driver) has the datasheet's formula and ADC setup. The controller is done in the library too: `thermal::Thermal` takes a reading every 16 ticks and gives a ceiling that fades down a level every other tick at most, with prediction, a 2 °C hold window and a floor. It's tested against made-up traces and a rough model of the head, and it isn't wired up yet. Still to do: the ADC read in `src/hw.rs` (registers in `src/regs.rs`, checked in `xtask/src/atdf.rs`), taking the lower of the UI's level and the ceiling in `src/torch.rs`, teaching the simulator in `xtask/src/sim.rs` the ADC, and tuning the constants on a real light.
- [ ] A watchdog, so a hang can't leave the FET on. Anduril has no precedent here: it only uses the WDT to reboot, with an 8 ms timeout. The WDT runs off the 1.024 kHz ultra-low-power oscillator and keeps counting in every sleep mode, so the standby tick has to feed it too. Enable it from software through `CPU.CCP`. Leave `WDT.STATUS` LOCK alone, since only debug mode can clear it, and never use the `WDTCFG` fuse.

## 3. It's nice to use

- [ ] Battery check on 3C
- [ ] Remember the level in EEPROM, with wear levelling
- [ ] Dynamic PWM (variable TCA0 TOP) for a smoother, dimmer bottom end
- [ ] Gradual level changes, so the step-downs in stage 2 aren't noticeable. `gradual::Gradual` does the fading, a level at a time with separate rates down and up, and thermal regulation uses it for its ceiling. Low-voltage step-down should use it the same way. It's done when both step-downs run through it on the torch.
- [ ] A version check that blinks out the build

## Maybe

- Configurable floor, ceiling and thermal limit
- [x] A UI simulator that runs `src/ui.rs` in a terminal: `cargo xtask simulate`
- Support for the TS10 and other lights that share this driver layout
- The write-page NVM command (0x01) for flash in place of page erase-write (0x03), since `program` chip-erases first. The datasheet (table 36-38) gives 2 ms for a page write against 4 ms for erase-write, so even a full 256-page flash only saves about half a second. Keep erase-write for EEPROM, since EESAVE can keep it through a chip erase.
