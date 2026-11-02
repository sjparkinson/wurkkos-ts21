# Hardware

What's inside a TS21 (model 0614), and how sure I am about each bit. "Confirmed" means a primary source (Microchip's files, Anduril's hardware definitions or someone who opened one up) says so. "Likely" means it follows from a sibling light or a convention and nobody's contradicted it.

## Spotting a model 0614

It has an ATtiny1616 and three pads labelled R, G and V on the driver. A version check from off (15 or more clicks in Anduril 2) blinks out the build date and then the model number, which is 0614. Confirmed.

Anduril's [`MODELS`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/MODELS) file maps 0614 to `sofirn-sp36-t1616`. The [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) lists the revised TS21 as ATtiny1616, "similar or same as SP10 Pro", and [post 6](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942/6) of that thread has a January 2023 TS21 with R-G-V pads reading "2022-02-08 0614".

## The driver

Anduril builds model 0614 from `hw/sofirn/sp36-t1616`, which includes `hw/sofirn/blf-q8-t1616`, which says "identical to TS10 hwdef" and includes `hw/wurkkos/ts10/hwdef.h`. So the TS21's pin map is the Wurkkos TS10's. Confirmed, though only in the sense that Wurkkos ships that build. Nobody's probed a board.

| Pin | Job | Notes |
|---|---|---|
| PA5 | Side switch | Active low with the internal pull-up, interrupt on both edges |
| PB0 | FET channel | TCA0 WO0 (CMP0) |
| PB1 | Linear channel ("1×7135") | TCA0 WO1 (CMP1) |
| PB5 | Aux LED | Single channel, likely the button LED's amber element |
| VCC | Battery voltage | The MCU measures its own supply, probably about 0.3 V under the cell (see below) |

Every other pin gets its pull-up turned on so it doesn't float and leak current.

Both LED channels drive the same emitters. The linear channel is a 7135-style regulator that tops out around 350 mA (about 126 lm on this light). It's dimmed by PWM like everything else, so the low end flickers at PWM speed. The FET connects the LEDs straight to the cell for everything above that. Anduril ramps the linear channel to full by level 75 of 150 and then brings the FET in, and I've kept that split.

Anduril runs TCA0 in dual-slope mode off the undivided CPU clock, with a variable TOP ("dynamic PWM") for more resolution at the bottom of the ramp. TOP runs from 4095 at level 1 down to 255 at about level 69, so the PWM frequency, 10 MHz / (2 × TOP), goes from about 1.2 kHz at the bottom to about 19.6 kHz from the middle up. This firmware sits at 19.6 kHz throughout for now, which costs resolution at the bottom.

Anduril sets the CPU clock to 10 MHz (the 20 MHz oscillator divided by 2) in `hwdef_setup()`. It measures the battery by reading the internal 1.5 V reference with VCC as the ADC's reference, accumulated 4×, so VCC ≈ 1.5 × 1023 × 4 / result. Then it applies this board's calibration, a slope of 1010/1024 and +0.30 V. That offset suggests a drop between the cell and the MCU (my inference), and any low-voltage threshold has to allow for it.

`src/battery.rs` does the same sums: a reading of 4 × 1024 × 1.5 V / VCC (Anduril's scaling, which uses 1024 counts to full scale where the datasheet says 1023), then the slope and offset, rounded to the nearest centivolt. How Anduril measures and scales is confirmed, from its source.

The 0614 calibration is confirmed to within 0.1 V at 3.57 to 4.08 V on a real TS21 (model 0614), and no better than that yet. Stock Anduril's battery check against a multimeter on the cell, at rest:

| Multimeter | Anduril battcheck | What Anduril read |
|---|---|---|
| 4.084 V | 4.1 | 4.10 V or more, so at least 0.016 V high |
| 3.953 V | 3.9 | 3.90 to 3.99 V |
| 3.709 V | 3.7 | 3.70 to 3.79 V |
| 3.565 V | 3.5 | 3.50 to 3.59 V |

Anduril's battcheck truncates to tenths rather than rounding (`blink_num(voltage16 / dV16)` in [`fsm/adc.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/fsm/adc.c)), hence the ranges. All are within 0.1 V, and the top one reads a little high, the unsafe direction for a cut-off, though by under a tenth. Readings to the hundredths from the stock firmware are still to come, and nothing's been measured under load or near the 2.99 V cut-off, so the constants stay as they are until then. This firmware will need checking too: its own reading against a multimeter, once it can report one.

Anduril's low-voltage protection has one threshold, `VOLTAGE_LOW`, which defaults to 2.90 V in [`fsm/adc.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/fsm/adc.h), and nothing in the TS21's include chain (`hw/sofirn/sp36-t1616`, `hw/sofirn/blf-q8-t1616`, `hw/wurkkos/ts10`, `hw/wurkkos/anduril.h`) overrides it. Confirmed. It compares that against its smoothed, calibrated voltage, measured with the light on, and while it's low it raises a low-voltage event about every six seconds (`VOLTAGE_WARNING_SECONDS` is 5, and with thermal regulation the ADC does one voltage check a second, in [`fsm/adc.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/fsm/adc.c)). Each event in steady mode takes the level to about three quarters of what it was, and an event at level 1 turns the light off ([`ui/anduril/anduril.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/ui/anduril/anduril.c), `low_voltage()`). Confirmed. So the "about 2.99 V" reviewers saw at cut-out (below) is most likely the cell after it rested, a little above the 2.90 V Anduril acted on under load. That's my inference, so likely.

Anduril also skips its voltage checks while the button is held (`NO_LVP_WHILE_BUTTON_PRESSED` in `fsm/adc.c`), because on some lights the button reads as zero volts. That's likely irrelevant here, since the TS21's switch is on PA5 and the MCU measures its own supply rather than a divider the switch could pull down. Likely, not confirmed.

The cell Wurkkos sells for it is its own-brand 21700, 5000 mAh. Wurkkos's shop lists it as "Unprotected, 5000mAh 3C 15A" (confirmed, from [wurkkos.com](https://wurkkos.com/collections/battery)), so the cell has no protection circuit of its own and the firmware is the only thing standing between it and over-discharge. A distributor's spec sheet ([Wurkkos SA](https://wurkkos.co.za/product/wurkkos-21700-5000mah-button-top-rechargeable-li-ion-battery-3-6v-high-capacity-pointed-cell-1-battery-per-pack/)) gives 3.6 V nominal, 2.50 V minimum, 4.20 V maximum, 10 A continuous (15 A peak) and at most 30 mΩ. Likely, since it isn't Wurkkos's own page. Nobody says who makes the cell. The numbers match generic 5000 mAh 21700 datasheets, so it's probably a rebadged cell (likely).

So `src/battery.rs` turns off at 2.98 V or under (below 2.99 V), held for a second, as asked for and close to what reviewers saw from stock. That's 0.5 V above the cell's minimum. Measured under load, it's more cautious than Anduril's 2.90 V. It steps down below 3.10 V, a step every five seconds, so there's room to step down before cutting out, and steps back up at 3.30 V. On the FET, a reading under 2.99 V drops the light to the top of the linear channel rather than straight off. At turbo the tail current is about 7.75 A (see below), and 30 mΩ of cell alone sags 0.23 V at that, before the springs and FET, so a low reading on the FET can come from a cell with plenty left. The thresholds are guesses until a real cell's been measured.

Temperature comes from the MCU's internal sensor, using the factory calibration in `SIGROW.TEMPSENSE0` and `TEMPSENSE1`. The datasheet (DS40002204A, section 30.3.2.6) gives kelvin as `((result - TEMPSENSE1) × TEMPSENSE0 + 0x80) >> 8`, where TEMPSENSE1 is a signed offset and TEMPSENSE0 an unsigned gain. That product runs to 19 bits, but splitting the difference into high and low bytes gets the exact answer from 16-bit multiplies (`src/temp.rs` has the working). The measurement wants the 1.1 V internal reference, `SAMPCAP` set, and `INITDLY` and `SAMPLEN` each at least 32 µs of ADC clock. Typical accuracy is ±3 °C at 25 °C, at 0.55 °C a step (section 36.17). Anduril accumulates 4× and keeps the result as kelvin × 64.

This firmware shares ADC0 between the two, switching every 8 ticks so each is read four times a second (`src/sensors.rs`). The setup, all in `src/hw.rs`, is likely, not confirmed: it follows the datasheet and Anduril, and has only ever run on the simulator.

- **Both:** single conversions, 10-bit, `PRESC` DIV16 (625 kHz at 10 MHz, inside the 200 kHz to 1.5 MHz of table 36-26), `SAMPCAP` set, `INITDLY` DLY32 (51 µs) and `SAMPLEN` 20 (32 µs), which the temperature sensor needs and the voltage doesn't mind. `ADC0.CALIB` is written to 25% duty, since the ATDF (50%) and the datasheet (25%, section 30.5.17) disagree on its reset value. `VREF.CTRLB.ADC0REFEN` keeps the reference running: the datasheet doesn't say whether a single conversion keeps it requested, and a 1.5 V reference caught starting up reads low, which looks like a fuller cell. That costs some current, to revisit with sleep.
- **Voltage:** `VREF.CTRLA` ADC0REFSEL 1.5 V, `MUXPOS` INTREF, `REFSEL` VDDREF, `SAMPNUM` ACC4. That's Anduril's (`arch/attiny1616.c`), except Anduril free-runs.
- **Temperature:** ADC0REFSEL 1.1 V, `MUXPOS` TEMPSENSE, `REFSEL` INTREF, `SAMPNUM` ACC1, so `temp::kelvin` gets the plain 10-bit result the datasheet's formula wants (section 30.3.2.6). Anduril accumulates 4×.
- **Settling:** the first conversion after each switch is thrown away. The datasheet doesn't ask for that: it says `INITDLY` covers the reference "when enabling the ADC or changing to an internal reference voltage", that a new channel needs settling time (section 30.3.2.4), and that VREF takes about 25 µs to start (table 36-22). The voltage reading keeps VDD as the ADC's reference while the reference it measures moves from 1.1 to 1.5 V, which `INITDLY` may not cover, and Anduril throws the first result away too ("first result is unstable", `fsm/adc.c`). A tick is cheap.
- **Reading:** poll `INTFLAGS.RESRDY` a tick after starting, then read `RES` low byte first. No interrupts.

Anything that can't be real counts as garbage and fails safe (a ceiling at level 75, never off), and garbage in the first round after power-on gives one dim blink, so a wrong setup shows on the first flash. See [ui.md](ui.md#low-battery-and-heat).

The sensor and its factory calibration read sensibly on a real TS21 (model 0614). Likely, not confirmed. After the light had sat off at room temperature, stock Anduril's temperature check blinked 20 °C against 20.6 °C on the user's thermometer. Anduril shows whole degrees, so that's within 1 °C. It isn't confirmation of this firmware's sums, though: Anduril uses its own formula (`(measurement>>6) + THERM_CAL_OFFSET + TH_CAL - 275` in [`fsm/adc.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/fsm/adc.c)), not the datasheet's that `temp::kelvin` follows, and it has a user-adjustable calibration that may not be at its default on this light. This firmware's own reading still needs checking against a thermometer, and nothing's changed on this evidence.

This firmware also reads the temperature only while the light's on, plus once at power-on for the diagnostic. The voltage it reads all the time, on or off, until sleep arrives and that gets revisited.

That sensor is in the MCU on the driver, so it measures the driver, not the emitters, and it lags behind them. Likely, since that's where the chip sits, but nobody's measured how far behind or how much cooler. Anduril's 45 °C default is a limit on that reading, and Anduril has a per-light calibration offset on top (`THERM_CAL_OFFSET` in `fsm/adc.c`), so "45 °C" here may not be 45 °C anywhere you'd touch. The controller in `src/thermal.rs` is tuned against a made-up model of the head (a one-minute time constant, with the MCU five seconds behind), not a real TS21, so its constants are a starting point. Expect to retune them once a light has been run up to temperature with a thermocouple on the head.

Anduril's build for this light never steps down below level 60, and its linear channel is full at 75 (`MIN_THERM_STEPDOWN` and `MAX_1x7135` in [`hw/sofirn/blf-q8-t1616/anduril.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/sofirn/blf-q8-t1616/anduril.h), which the 0614 build includes). Confirmed. So its thermal floor is on the linear channel, and this firmware floors at 75, the top of it. The 0614 build also throttles down faster above level 130 (`THERM_FASTER_LEVEL` in `hw/sofirn/sp36-t1616/anduril.h`). `src/thermal.rs` has no equivalent, because its step-down rate already scales with how far over the limit it is. Anduril only reads the temperature while the light's on: `fsm/adc.c` says a running ADC adds about 250 µA in standby, against about 20 µA without it. Confirmed as what Anduril says, not measured on a TS21.

All of this comes from [`hw/wurkkos/ts10/hwdef.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/wurkkos/ts10/hwdef.h), [`hw/sofirn/blf-q8-t1616/hwdef.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/sofirn/blf-q8-t1616/hwdef.h), [`hw/sofirn/blf-q8-t1616/anduril.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/sofirn/blf-q8-t1616/anduril.h) for the ramp tables, and [`arch/attiny1616.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/arch/attiny1616.c) for voltage and temperature. Anduril is GPL-3.0, so this repo takes facts about the board from it and no code.

## The R, G, V pads

Unscrew the head and they're on the driver. They're UPDI.

| Pad | Job |
|---|---|
| R | UPDI data (the ATtiny1616's reset pin doubles as UPDI) |
| G | Ground |
| V | VCC, 3.3 to 5 V from the programmer depending on which one |

People who've opened one confirm the labels. What each one does is the standard three-pad layout gchart's t1616 drivers use, which other lights label `+ - R` or, read from the other end, `R - +` ([pingle.org](https://www.pingle.org/2022/08/07/flashing-a-3-pin-t1616-flashlight-with-new-firmware)). Likely, bordering on confirmed. The pads are at 1.27 mm pitch, the same as gchart's layout, so a standard 1.27 mm probe fits. Confirmed by measuring a TS21 (model 0614) (it's nowhere near 2.54 mm). Pad order varies between lights, so before you press a programmer onto them, check with a multimeter that G is continuous with the battery negative.

How to flash through them is in [flashing.md](flashing.md).

## The MCU

The ATtiny1616 is an AVR (avrxmega3) at up to 20 MHz, with 16 KB flash, 2 KB SRAM and 256 B EEPROM. It shares a datasheet (DS40002204), register map and vector table with the 14-pin ATtiny1614 and the 24-pin ATtiny1617. The 1617 has four pins (PB6, PB7, PC4, PC5) that the 1616 doesn't, so leave those alone.

These are the bits that matter for this firmware.

- **Flash is mapped into data space at 0x8000.** Const tables live in flash and are read with ordinary loads. `link.x` puts `.rodata` there.
- **16 KB of flash means two-word `jmp` vectors**, 31 of them (reset plus 30 interrupts).
- **16-bit timer registers go through a shared TEMP byte.** Write the low byte, then the high byte.
- **Clock changes are behind configuration change protection.** Write 0xD8 to `CPU.CCP`, then the register, within four instructions.
- **Any pin can wake standby, with the right sense setting.** Only pins 2 and 6 of each port are fully asynchronous. The rest, PA5 included, wake the chip with the peripheral clock stopped only on BOTHEDGES or LEVEL, and the new level has to last until the clock restarts or the chip wakes with no interrupt (section 16.3.3.1, table 16-3). Confirmed.
- **The watchdog runs in sleep.** It's clocked from the 1.024 kHz ultra-low-power oscillator and keeps counting in any sleep mode. Writing 1 to `WDT.STATUS` LOCK can only be undone in debug mode, and the `WDTCFG` fuse can enable it at boot (section 19.3). Confirmed.
- **There's a two-cycle 8×8 hardware multiplier** (DS40002204A, page 1, under CPU features), with `mul`, `muls`, `mulsu`, `fmul`, `fmuls` and `fmulsu`. `rustc --print cfg --target avr-none -C target-cpu=attiny1616` lists `target_feature="mul"` too, and LLVM uses it for 8- and 16-bit products. Confirmed. It doesn't help with 32-bit maths, see [toolchain.md](toolchain.md#32-bit-multiplication).
- **The factory fuses are fine.** Anduril never writes fuses on the t1616, so the defaults (20 MHz oscillator, UPDI enabled on the reset pin, EESAVE off) are what's on a stock TS21. Don't change `SYSCFG0` to turn the UPDI pin into a GPIO or reset pin unless you own a high-voltage programmer, because that's the only way back.

The register addresses, bit values and vector count in `src/regs.rs`, and the memory map and NVM constants in `xtask/src/updi.rs`, are checked by `cargo test` (`xtask/src/atdf.rs`) against the ATtiny1617 ATDF in [`vendor/`](../vendor/README.md), copied from [avr-device](https://github.com/Rahix/avr-device/tree/0bdf99ce7fa92a128de2bbb5d0303b8f0e633c7f/vendor). Microchip's own `ATtiny1616.atdf` is in their ATtiny device pack at packs.download.microchip.com if you want to check against the exact part. None of it has met a real board yet.

## The rest of the light

Three Luminus SST-20 emitters (4000 K, 5000 K or 6500 K) behind a TIR optic, a 21700 cell and USB-C charging. Confirmed.

The driver is a plain FET+1, with one 7135-style regulator and a direct-drive FET (a HYG013N03LS1C2). The teardown calls it "pretty much a standard FET+1 with the switch LED, same configuration as a Q8 or SP36". So above level 75 output follows the cell voltage down as it sags.

Wurkkos claims 3,500 lumens. Reviewers measured between 1,878 and 3,316 lm at turn-on, about 1,100 lm after 30 seconds (with 7.75 A at the tail), and 200 to 280 lm sustained at Anduril's default 45 °C limit. Thermal regulation is a must on this light.

Reviewers saw stock Anduril cut out at about 2.99 V, most likely the cell at rest afterwards (see [the driver](#the-driver)). Parasitic drain when off is 30 to 116 µA, and several mA with the button LED on high.

A separate Injoinic IP5310 chip handles charging (at about 1.6 A), so the firmware leaves it alone. The teardown found it on a 2021 light, and it's likely the same now, since no Anduril source gives it a pin. The button LED is an amber and blue pair, and only the amber half has an MCU pin, so the blue half probably belongs to the charger.

These come from [the 2021 teardown](https://budgetlightforum.com/t/wurkkos-ts21-tear-down/68750), [1Lumen](https://1lumen.com/review/wurkkos-ts21/), [ZeroAir](https://zeroair.org/2021/10/12/wurkkos-ts21-anduril-flashlight-review/), [rockenrooster](https://reviews.rockenrooster.dedyn.io/wurkkos-ts21-review/) and [the BLF review](https://budgetlightforum.com/t/review-wurkkos-ts21-sst20-4000k-anduril-1-an-excellent-budget-enthusiast-light/68189). They all predate the switch to the ATtiny1616, and nobody's said the rest of the light changed with it.

## Open questions

- Is PB5 really the button LED's amber element on the TS21? Anduril's TS10 config calls it front-facing aux, and the SP36 config talks about a button LED. A multimeter on the driver would settle it, and so would flashing a build that blinks it. [LiquidRetro's review](https://liquidretro.net/tag/wurkkos-ts21-user-manual/) of the Anduril 2 light describes red, blue and amber under the switch, with amber as the locator, which fits PB5 being amber and the charger owning the rest.
- What lights the button LED's blue element? The charger, or something else?
- How close is the 0614 voltage calibration on this light to better than 0.1 V (it's within that at 3.57 to 4.08 V, see [the driver](#the-driver)), what is it near 3 V, and how much does the cell sag on the FET at turbo? That decides whether 3.10 V and 2.99 V under load are in the right place.
- Does the TS21 driver match the TS10 exactly (same 7135 current, same FET), or only in pinout? The calibration numbers differ slightly between the TS10 and Q8 builds.
