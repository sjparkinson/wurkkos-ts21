# Roadmap

Roughly in order. Each step should leave a torch that's no worse than the step before. Tick things off here in the commit that does them.

## 1. It lights up

- [x] Toolchain with nightly `avr-none`, `rust-lld`, own linker script and vector table, no crates
- [x] Pin map and registers for the TS21 (model 0614) driver
- [x] Switch sampling at 64 Hz and a click/hold decoder
- [x] Two-channel ramp (linear then FET), 150 levels
- [x] Click on, click off, at one level on the linear channel
- [x] A built-in UPDI programmer in `xtask` (ping, backup, flash, restore), tested against a fake chip
- [ ] Ping, back up and flash a real TS21, which is also the first real test of the programmer
- [ ] Confirm the batched UPDI frames work on a real chip. `write_page` now sends the RSD toggle, REPEAT, the page data and the RSD toggle back in one frame, and `read` sends REPEAT and LD together. The fake chip is happy with both; if a real flash fails partway through a page, split them back into separate sends first. Flash is committed with PAGEWRITE (0x01) and EEPROM with PAGEERASEWRITE (0x03), and only the fake chip has seen either, so if flash fails `verify` and EEPROM doesn't, suspect PAGEWRITE.
- [ ] See whether the `wait_nvm` after `NVM_PAGE_BUFFER_CLEAR` ever sees the controller busy. pymcuprog waits there, so keep it unless a real chip shows the first poll is always idle. It's one round trip per page.
- [ ] Check PB5 is the button LED and decide what it does
- [ ] Bring back the full UI from the `fancy-ui` branch, with ramping, moonlight, turbo and lockout

## 2. It's safe to leave a cell in

None of this is optional for a light with a direct-drive FET on a 21700.

- [ ] Sleep when off, using standby sleep with the switch's pin-change interrupt to wake (PA5 is already set to interrupt on both edges). PA5 isn't one of the fully asynchronous pins (only pins 2 and 6 are), but the datasheet (DS40002204A, table 16-3) says a partially asynchronous pin still wakes the chip with the peripheral clock stopped when its sense is BOTHEDGES or LEVEL. The catch is that the new level has to last until the clock restarts, or the chip wakes with no interrupt. A press lasts far longer than that, but the safe pattern is to read PA5 on every wake whether or not the interrupt fired. This means the first real interrupt handler, so `src/rt.rs` grows named vectors and Rust needs `extern "avr-interrupt"` (another nightly feature). Anduril also slows the PIT in standby. The ADC also needs turning off in standby: `hw::init` leaves it enabled with `VREF.CTRLB.ADC0REFEN` set, and the voltage gets read on or off.
- [ ] Sleep between ticks when on, using the RTC PIT interrupt in place of the busy-wait. `hw::set_output` already only runs when the output changes (`torch::Changes`), so the loop no longer rewrites the TCA0 compare buffers every tick or resets `TCA0_CNT` every tick while off.
- [x] Battery voltage, with the ADC measuring the internal 1.5 V reference against VCC, four conversions accumulated, then the 0614 calibration (slope 1010/1024, +0.30 V) through `battery::centivolts`. The ADC alternates with the temperature sensor every 8 ticks (`src/sensors.rs`), throwing away the first conversion after each switch, and `src/hw.rs` has the setup ([hardware.md](hardware.md#the-driver) lists it). It runs on the simulated chip, which checks the setup and feeds scripted readings. Still to check on a real light: that the firmware's reading matches a multimeter at a few charge levels and under load. Stock Anduril on this light reads within 0.1 V at 3.57 to 4.08 V with the same method, which is encouraging, not proof.
- [x] Low-voltage protection, stepping down below 3.10 V and turning off below 2.99 V. The Wurkkos cell is unprotected and rated down to 2.50 V, and [hardware.md](hardware.md#the-driver) has the evidence. `battery::Protection` filters a sample every 16 ticks; after five seconds below 3.10 V the ceiling steps down 15 levels, after a second below 2.99 V the light turns off (or, on the FET, drops to the linear channel first), and it steps back up at 3.30 V. `src/limits.rs` fades the ceiling through a `Gradual` and turns the UI off on a flat cell. A voltage that can't be real (outside 2.00 to 4.60 V, a zero or saturated reading) never counts as safe: a second of them caps the light at level 75, and never turns it off. docs/ui.md has the rows. Still to check on a real light: the step-down and cut-off with a cell run down on a bench supply or for real, and how far the cell sags on the FET at turbo.
- [ ] Thermal regulation, using the internal temperature sensor (calibrated from `SIGROW.TEMPSENSE0` and `TEMPSENSE1`), a 45 °C limit (Anduril's default), and `thermal::Thermal`'s controller with prediction, a 2 °C hold window and a floor at the top of the linear channel. It's wired up: the ADC reads the temperature every 16 ticks while the light's on (and once at power-on, for the diagnostic blink), `Thermal::reset` runs on turn-on, and the light runs at the lower of the UI's level and the ceiling. A broken sensor takes the ceiling to the floor. Not ticked because it can't do its job yet: the UI's one level (50) is under the floor, so it never visibly steps down, and its constants are tuned against a made-up model of the head. Still to do: the full UI with levels above 75, and a run up to temperature on a real light with a thermometer on the head (turbo until it steps down) to check the reading and retune. Stock Anduril read 20 °C against a 20.6 °C room on this light, with its own formula.
- [ ] A watchdog, so a hang can't leave the FET on. Anduril has no precedent here: it only uses the WDT to reboot, with an 8 ms timeout. The WDT runs off the 1.024 kHz ultra-low-power oscillator and keeps counting in every sleep mode, so the standby tick has to feed it too. Enable it from software through `CPU.CCP`. Leave `WDT.STATUS` LOCK alone, since only debug mode can clear it, and never use the `WDTCFG` fuse. Run it in window mode, as [AN2632](microchip/README.md) §3.2 recommends, so a jump into the wrong code fails whether it feeds the watchdog too late or too early, with the closed window at least half the period. AN2632 also says to prove the watchdog resets the chip before trusting it: let it expire once at power-on, then tell that reset apart from a real hang using `RSTCTRL.RSTFR`.
- [ ] Check the main clock against the RTC, using AN2632 §8's method. The RTC runs off the separate 32 kHz oscillator, so count TCA0 overflows between RTC ticks and fail if the count is outside a tolerance. That catches a wrong main clock, a dead 32 kHz oscillator or a broken timer, any of which throws off PWM, the tick and every filter timed by it. On a failure, turn the outputs off and stay off.
- [ ] Check flash against a CRC at power-on (AN2632 §3.6 and §9.1). `cargo xtask hex` computes a CRC-16 CCITT over the image and appends it, and the firmware checks it before turning anything on. The 1-series has a hardware `CRCSCAN` peripheral that does the sum without the CPU; the datasheet has its start-up and software-triggered modes, and only the software trigger is on the table, since the other one is a fuse. A half-written flash from an interrupted `cargo xtask flash` is the realistic fault here, not a bit flip.
- [ ] Read the fuses back at power-on and refuse the FET if they're not what the firmware expects (AN2632 §11). Reading is safe; this never writes them. Find out from a `cargo xtask backup` of a stock light what `BODCFG` it ships with. Brown-out detection is only set by that fuse, so if it's off, the firmware has to assume the chip can run below a safe voltage and lean harder on low-voltage protection. Nobody's read one off a TS21 yet.

## 3. It's nice to use

- [ ] Battery check on 3C
- [ ] Remember the level in EEPROM, with wear levelling
- [ ] Dynamic PWM (variable TCA0 TOP) for a smoother, dimmer bottom end
- [x] Gradual level changes, so the step-downs in stage 2 aren't noticeable. `gradual::Gradual` does the fading, a level at a time with separate rates down and up, and both the thermal and the battery ceilings run through it on the torch (`src/limits.rs`).
- [ ] A version check that blinks out the build

## Maybe

- Configurable floor, ceiling and thermal limit
- [x] A UI simulator that runs `src/ui.rs` in a terminal: `cargo xtask simulate`
- Support for the TS10 and other lights that share this driver layout
- A RAM test at power-on, the March test AN2632 §9.2 describes, plus a stuck-bit check on the CPU registers (§3.1). Class B wants both, but they guard against silicon faults, which are rarer than the software and flashing faults the stage 2 checks cover
- [x] The write-page NVM command (0x01) for flash in place of page erase-write (0x03), since `program` chip-erases first. That's one erase per page per flash instead of two, and saves up to half a second (the datasheet's table 36-38 gives 2 ms for a page write against 4 ms for erase-write). `Programmer::erase_and_write_flash` does the chip erase and the page writes in one call, so nothing can page-write over flash that isn't blank, and `verify` catches an erase that didn't take. EEPROM keeps erase-write, since EESAVE can keep it through a chip erase.
