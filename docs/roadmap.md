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
- [ ] Try the write-page NVM command (0x01) for flash in place of page erase-write (0x03). `program` always chip-erases first, so erasing each page again is wasted time. Keep erase-write for EEPROM, since EESAVE can keep it through a chip erase. Check the datasheet's NVMCTRL timings to see if the saving is worth it, and verify a full flash afterwards.
- [ ] See whether the `wait_nvm` after `NVM_PAGE_BUFFER_CLEAR` ever sees the controller busy. pymcuprog waits there, so keep it unless a real chip shows the first poll is always idle. It's one round trip per page.
- [ ] Check PB5 is the button LED and decide what it does
- [ ] Bring back the full UI from the `fancy-ui` branch, with ramping, moonlight, turbo and lockout

## 2. It's safe to leave a cell in

None of this is optional for a light with a direct-drive FET on a 21700.

- [ ] Sleep when off, using standby sleep with the switch's pin-change interrupt to wake (PA5 is already set to interrupt on both edges). PA5 isn't one of the fully asynchronous pins (only pins 2 and 6 are), so check the datasheet's port chapter on what wakes standby before relying on it. This means the first real interrupt handler, so `src/rt.rs` grows named vectors and Rust needs `extern "avr-interrupt"` (another nightly feature). Anduril also slows the PIT in standby.
- [ ] Sleep between ticks when on, using the RTC PIT interrupt in place of the busy-wait. While doing it, only call `hw::set_output` when the level changes, since today it rewrites both TCA0 compare buffers every tick and resets `TCA0_CNT` every tick while off.
- [ ] Battery voltage, with the ADC measuring the internal reference with VCC as its reference (Anduril uses the 1.5 V reference and 4× accumulation), then the 0614 calibration (slope 1010/1024, +0.30 V) as a starting point.
- [ ] Low-voltage protection, stepping down below about 3.0 V and turning off below about 2.8 V.
- [ ] Thermal regulation, using the internal temperature sensor (calibrated from `SIGROW.TEMPSENSE0` and `TEMPSENSE1`), a target temperature (Anduril defaults to 45 °C), and a controller that steps down without visible jumps. The calibration maths wants a 32-bit product, so it needs the 16 × 16 → 32 helper from [toolchain.md](toolchain.md#32-bit-multiplication) first.
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
