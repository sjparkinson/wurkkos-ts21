# Roadmap

Roughly in order. Each step should leave a torch that's no worse than the step before. Tick things off here in the commit that does them.

## 1. It lights up

- [x] Toolchain: nightly `avr-none`, `rust-lld`, own linker script and vector table, no crates
- [x] Pin map and registers for the TS21 v2 driver
- [x] Switch sampling at 64 Hz and a click/hold decoder
- [x] Two-channel ramp (linear then FET), 150 levels
- [x] Click on, click off, at one level on the linear channel
- [x] A built-in UPDI programmer in `xtask` (ping, backup, flash, restore), tested against a fake chip
- [ ] Ping, back up and flash a real TS21, which is also the first real test of the programmer
- [ ] Check PB5 is the button LED and decide what it does
- [ ] Bring back the full UI from the `fancy-ui` branch: ramp up and down, moonlight, turbo, lockout

## 2. It's safe to leave a cell in

None of this is optional for a light with a direct-drive FET on a 21700.

- [ ] Sleep when off: standby sleep with the switch's pin-change interrupt to wake (PA5 is already set to interrupt on both edges). PA5 isn't one of the fully asynchronous pins (only pins 2 and 6 are), so check the datasheet's port chapter on what wakes standby before relying on it. This means the first real interrupt handler, so `src/rt.rs` grows named vectors and Rust needs `extern "avr-interrupt"` (another nightly feature). Anduril also slows the PIT in standby.
- [ ] Sleep between ticks when on, using the RTC PIT interrupt in place of the busy-wait.
- [ ] Battery voltage: the ADC measuring the internal reference with VCC as its reference (Anduril uses the 1.5 V reference and 4× accumulation), then the 0614 calibration (slope 1010/1024, +0.30 V) as a starting point.
- [ ] Low-voltage protection: step down below about 3.0 V, turn off below about 2.8 V.
- [ ] Thermal regulation: the internal temperature sensor (calibrated from `SIGROW.TEMPSENSE0` and `TEMPSENSE1`), a target temperature (Anduril defaults to 45 °C), and a controller that steps down without visible jumps. The calibration maths wants a 32-bit product, so it needs the 16 × 16 → 32 helper from [toolchain.md](toolchain.md#the-catch-32-bit-multiplication) first.
- [ ] A watchdog, so a hang can't leave the FET on.

## 3. It's nice to use

- [ ] Battery check on 3C
- [ ] Remember the level in EEPROM, with wear levelling
- [ ] Dynamic PWM (variable TCA0 TOP) for a smoother, dimmer bottom end
- [ ] Gradual level changes, so the step-downs in stage 2 aren't noticeable
- [ ] A version check that blinks out the build

## Maybe

- Configurable floor, ceiling and thermal limit
- A UI simulator that runs `src/ui.rs` in a terminal
- Support for the TS10 and other lights that share this driver layout
