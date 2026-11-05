# Hardware

What's inside a TS21 (model 0614), and how sure I am about each bit. "Confirmed" means a primary source (Microchip's files, Anduril's hardware definitions or someone who opened one up) says so. "Likely" means it follows from a sibling light or a convention and nobody's contradicted it.

## Spotting a model 0614

It has an ATtiny1616 and three pads labelled R, G and V on the driver. A version check from off (15 or more clicks in Anduril 2) blinks out the build date and then the model number, which is 0614. Confirmed.

Anduril's [`MODELS`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/MODELS) file maps 0614 to `sofirn-sp36-t1616`. The [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) lists the revised TS21 as ATtiny1616, "similar or same as SP10 Pro", and [post 6](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942/6) of that thread has a January 2023 TS21 with R-G-V pads reading "2022-02-08 0614".

## The driver

Anduril builds model 0614 from `hw/sofirn/sp36-t1616`, which includes `hw/sofirn/blf-q8-t1616`, which says "identical to TS10 hwdef" and includes `hw/wurkkos/ts10/hwdef.h`. So the TS21's pin map is the Wurkkos TS10's. Confirmed, though only in the sense that Wurkkos ships that build. Nobody's probed a board.

| Pin | Job | Notes |
|---|---|---|
| PA5 | Side switch | Active low with the internal pull-up, polled every tick, no interrupt |
| PB0 | FET channel | TCA0 WO0 (CMP0) |
| PB1 | Linear channel ("1×7135") | TCA0 WO1 (CMP1) |
| PB5 | Aux LED | Single channel, likely the button LED's amber element (see [the button LED](#the-button-led)) |
| VCC | Battery voltage | The MCU measures its own supply, probably about a quarter of a volt under the cell, though nobody's measured it (see below) |

Every other pin gets its pull-up turned on so it doesn't float and leak current.

Both LED channels drive the same emitters. The linear channel is a 7135-style regulator that tops out around 350 mA (about 126 lm on this light). It's dimmed by PWM like everything else, so the low end flickers at PWM speed. The FET connects the LEDs straight to the cell for everything above that. Anduril ramps the linear channel to full by level 75 of 150 and then brings the FET in, and I've kept that split.

Anduril runs TCA0 in dual-slope mode off CLK_PER, with a variable TOP ("dynamic PWM") for more resolution at the bottom of the ramp, written through `PERBUF` and `CMPnBUF` without `LUPD`. Confirmed from `hw/wurkkos/ts10/hwdef.h` and `hwdef.c`. The 0614 build takes its ramp from `hw/sofirn/blf-q8-t1616/anduril.h`, where level 1 is a compare of 1 and a TOP of 4095, confirmed. That file also sets `QUARTERSPEED_LEVEL 5` and `HALFSPEED_LEVEL 20`, and `auto_clock_speed()` in `fsm/misc.c` rewrites the main clock prescaler by level. TCA0 slows with it, so if that runs, level 1 is 2.5 MHz / (2 × 4095) ≈ 305 Hz with a 0.8 µs pulse, not the 1.2 kHz that 10 MHz would give. Likely: nobody's found where this build defines `USE_DYNAMIC_UNDERCLOCKING`, and those figures are worked out, not measured.

This firmware keeps the clock at 5 MHz and uses single-slope PWM, so the cycle is TOP clocks (PER + 1) and a compare of n is n clocks high. A compare of 0 holds the pin low and one over PER holds it high (DS40002204A, section 20.3.3.4.3), both confirmed, and full on is a compare equal to `Output::top`. The n/TOP duty in between is likely: the datasheet doesn't give the high time for a mid-range compare, but it's the only reading that fits those end points. So is `LUPD` holding the buffers valid over a BOTTOM, which `hw::set_output` relies on. Section 20.3.3.2 says BV clears on an UPDATE with no exception for the lock, but `ALUPD` (section 20.5.2) can't work unless it stays set. If it doesn't, a change of level that hits the lock gets lost. Turn-off doesn't lean on it: `hw::set_output` forces the off row live with an UPDATE command (section 20.3.3.5, confirmed from the datasheet, not seen on a real chip), then writes both compares to 0 directly in case a stopped TCA0 ignores commands, which the datasheet doesn't say. It's only run on the simulator so far.

`ramp::output` gives each level of the linear channel its own TOP, from 4095 (1.22 kHz, 819 µs) at level 1 down to 255 (19.6 kHz). Everything from level 75 up, and off, stays at 255, so the FET never runs slower than it did. TOP stops at 4095 to stay well clear of visible flicker: Anduril's probable 305 Hz is the kind of rate you can see in a moving beam. Going down to that here would buy a level 1 four times dimmer. The audible band can't be avoided, since anything under 20 kHz is in it and 5 MHz / 20 kHz leaves no more resolution than a fixed TOP. So it's kept to where there's little current: levels 1 to 15 are under 10 kHz, at under 1% duty and under 3 mA on average, and levels 16 and up are 11.6 kHz or more (19.4 kHz at level 50). No report turned up of audible PWM at low levels on this driver, and Anduril's hwdef has no inductor, but nobody's looked at a board for one.

The shortest pulse is `ramp::MIN_PULSE`, 4 clocks, 0.8 µs, which is the shortest Anduril sends this regulator if its underclock runs. Forum measurements of the 7135 ([BLF](https://budgetlightforum.com/comment/828212)) disagree on the detail, but have it reaching 350 mA after about 1 to 2 µs and settling by about 6 µs, so a pulse this short doesn't reach full current. Likely, and there's no datasheet figure. One clock (200 ns) might not light it at all, so the ramp doesn't use it, which makes level 1 a nominal 4/4095 of the linear channel, four times dimmer than a fixed TOP's 1/255 was. Levels 1 to 19 all use that one pulse length and vary only how often it comes, so their brightness follows the frequency whatever the regulator does to the pulse. Past that the compare climbs and each step in it nudges TOP back up a little, between 255 and about 295. The steps get small quickly, since it's 5 clocks against 4, not 2 against 1. If a pulse loses a fixed time to turn-on, that still only ever adds brightness at those steps.

A long cycle changes what the ADC sees (AN2551C section 3.2). A conversion is 33 ADC clocks (13 + `SAMPLEN`, section 30.3.2.3), 264 CPU clocks or 52.8 µs, and a four-conversion burst is 1056 clocks, so TOP is longer than a burst on levels 1 to 11. There the four samples can all miss the pulse or one can catch it. I've accepted that rather than start conversions from a TCA0 event. The pulse there is 0.8 µs in every 211 µs or more against a 35 µs sample, with about 1 mA of average draw at most, so the most it can do to a reading is small (likely, not measured). An event start would need EVSYS, it would pin every reading to the same place in the cycle, which is a fixed bias rather than a spread, and errata DS80000886C 2.4.2 leaves a pending event stuck if the ADC's disabled mid-conversion, which is what `hw::prepare_sleep` does on its way to power-down. The one TOP to keep away from is 264, a conversion exactly one cycle long, so `ramp::dynamic` steps off it.

Anduril sets the CPU clock to 10 MHz (the 20 MHz oscillator divided by 2) in `hwdef_setup()`. It measures the battery by reading the internal 1.5 V reference with VCC as the ADC's reference, accumulated 4×, so VCC ≈ 1.5 × 1023 × 4 / result. Then it applies this board's calibration, a slope of 1010/1024 and +0.30 V. Anduril's docs put the offset down to the drop across the reverse-polarity diode (`fsm/spaghetti-monster.txt`, commit `9dfa2b7`), and any low-voltage threshold has to allow for it. Confirmed that those are the 0614's constants, but they were tuned against a meter on some light, not measured on a TS21: before August 2026 the same build used about +0.25 V ("voltage readings were a little high with the Q8 value", `cbdcc1b^`), and the TS10's own hwdef has 1006 and +0.25 V. With the slope, the net drop Anduril assumes is about 0.24 to 0.26 V rather than 0.30 V (derived). It's likely a Schottky, going by sibling drivers (`hw/gchart/fet1-t1616/hwdef.h` has "diode with ~0.4v drop"), but nobody's named the part, and the drop will vary with current.

This firmware runs at 5 MHz, the 20 MHz oscillator divided by 4, where Anduril runs at 10 MHz. The datasheet specifies 10 MHz from 2.7 V and 5 MHz from 1.8 V, linear between them (table 36-3 and figure 36-1), and AN2747C §3 lists what can go wrong under that: wrong execution, corrupted registers or flash, and pins toggling. The 2.99 V cut-off is an ADC reading of 2253, which is 2.73 V of VCC, or 2.65 to 2.81 V with the 1.5 V reference's ±3% (table 36-23, characterised). Sag on the FET can go lower than that, and quicker than the low-voltage filter's couple of seconds can see, so neither a higher cut-off nor a clock change triggered by voltage would cover it. Nothing here needs 10 MHz: the longest pass of the main loop is about 8% of a tick at 5 MHz, the ADC stays at 625 kHz, and the PWM gets the same frequencies from a single-slope timer (above). Stock Anduril is less careful and seems to get away with it. Its 2.90 V warning is about 2.64 V of VCC (derived), it only drops the clock at low levels (5 MHz under level 20, 2.5 MHz under level 5) for efficiency, never on voltage, and I found no reports of trouble on 1616 lights. So this is staying in spec, not fixing a fault anyone's seen.

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

- **Both:** single conversions, 10-bit, `PRESC` DIV8 (625 kHz at 5 MHz, inside the 200 kHz to 1.5 MHz of table 36-26), `SAMPCAP` set, `INITDLY` DLY32 (51 µs) and `SAMPLEN` 20 (32 µs), which the temperature sensor needs and the voltage doesn't mind. `ADC0.CALIB` is written to 25% duty, since the ATDF (50%) and the datasheet (25%, section 30.5.17) disagree on its reset value. `VREF.CTRLB.ADC0REFEN` keeps the reference running: the datasheet doesn't say whether a single conversion keeps it requested, and a 1.5 V reference caught starting up reads low, which looks like a fuller cell. That costs some current, so `hw::prepare_sleep` clears it before power-down, and `hw::adc_start` sets it again when the ADC's next set up.
- **Voltage:** `VREF.CTRLA` ADC0REFSEL 1.5 V, `MUXPOS` INTREF, `REFSEL` VDDREF, `SAMPNUM` ACC4. That's Anduril's (`arch/attiny1616.c`), except Anduril free-runs.
- **Temperature:** ADC0REFSEL 1.1 V, `MUXPOS` TEMPSENSE, `REFSEL` INTREF, `SAMPNUM` ACC1, so `temp::kelvin` gets the plain 10-bit result the datasheet's formula wants (section 30.3.2.6). Anduril accumulates 4×.
- **Settling:** the first conversion after each switch is thrown away. The datasheet doesn't ask for that: it says `INITDLY` covers the reference "when enabling the ADC or changing to an internal reference voltage", that a new channel needs settling time (section 30.3.2.4), and that VREF takes about 25 µs to start (table 36-22). The voltage reading keeps VDD as the ADC's reference while the reference it measures moves from 1.1 to 1.5 V, which `INITDLY` may not cover, and Anduril throws the first result away too ("first result is unstable", `fsm/adc.c`). A tick is cheap.
- **Reading:** poll `INTFLAGS.RESRDY` a tick after starting, then read `RES` low byte first. No interrupts.

Anything that can't be real counts as garbage and fails safe (a ceiling at level 75, never off). See [ui.md](ui.md#low-battery-and-heat).

The sensor and its factory calibration read sensibly on a real TS21 (model 0614). Likely, not confirmed. After the light had sat off at room temperature, stock Anduril's temperature check blinked 20 °C against 20.6 °C on the user's thermometer. Anduril shows whole degrees, so that's within 1 °C. It isn't confirmation of this firmware's sums, though: Anduril uses its own formula (`(measurement>>6) + THERM_CAL_OFFSET + TH_CAL - 275` in [`fsm/adc.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/fsm/adc.c)), not the datasheet's that `temp::kelvin` follows, and it has a user-adjustable calibration that may not be at its default on this light. This firmware's own reading still needs checking against a thermometer, and nothing's changed on this evidence.

This firmware also reads the temperature only while the light's on. The voltage it reads while the light's on or a click's being counted, since the ADC can't run in power-down (DS40002204A section 30.3.5).

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
- **The watchdog runs in sleep.** It's clocked from the 1.024 kHz ultra-low-power oscillator and keeps counting in any sleep mode. Writing 1 to `WDT.STATUS` LOCK can only be undone in debug mode, and the `WDTCFG` fuse can enable it at boot (section 19.3). Confirmed. `WDT.CTRLA` is behind configuration change protection with the IOREG key, and writing it while `WDT.STATUS` SYNCBUSY is set isn't allowed (sections 19.3.6 and 19.3.7). PERIOD and WINDOW count WDT clocks, 8 << (n - 1), not milliseconds. In window mode WINDOW is the closed window after each `wdr` and PERIOD the open window after that, so the timeout is their sum, and the first closed window only starts at the first `wdr` (section 19.3.3.2). A `wdr` takes two to three WDT clock cycles to sync, and another `wdr` meanwhile is ignored (section 19.3.6). Both the WDT and the PIT run off OSCULP32K, so a 64 Hz tick is 16 WDT clocks however far the oscillator drifts (±30% over the full operating range, table 36-13). Confirmed. The firmware runs it 32 clocks closed and 32 open, see `hw::init` and `ts21::watchdog`.
- **There's a two-cycle 8×8 hardware multiplier** (DS40002204A, page 1, under CPU features), with `mul`, `muls`, `mulsu`, `fmul`, `fmuls` and `fmulsu`. `rustc --print cfg --target avr-none -C target-cpu=attiny1616` lists `target_feature="mul"` too, and LLVM uses it for 8- and 16-bit products. Confirmed. It doesn't help with 32-bit maths, see [toolchain.md](toolchain.md#32-bit-multiplication).
- **The factory fuses are fine.** Anduril never writes fuses on the t1616, so the defaults (20 MHz oscillator, UPDI enabled on the reset pin, EESAVE off) are what's on a stock TS21. Don't change `SYSCFG0` to turn the UPDI pin into a GPIO or reset pin unless you own a high-voltage programmer, because that's the only way back.

The register addresses, bit values and vector count in `src/regs.rs`, and the memory map and NVM constants in `xtask/src/updi.rs`, are checked by `cargo test` (`xtask/src/atdf.rs`) against Microchip's ATtiny1616 ATDF in [`vendor/`](../vendor/README.md), from their ATtiny device pack. None of it has met a real board yet.

## The rest of the light

Three Luminus SST-20 emitters (4000 K, 5000 K or 6500 K) behind a TIR optic, a 21700 cell and USB-C charging. Confirmed.

The driver is a plain FET+1, with one 7135-style regulator and a direct-drive FET (a HYG013N03LS1C2). The teardown calls it "pretty much a standard FET+1 with the switch LED, same configuration as a Q8 or SP36". So above level 75 output follows the cell voltage down as it sags.

Wurkkos claims 3,500 lumens. Reviewers measured between 1,878 and 3,316 lm at turn-on, about 1,100 lm after 30 seconds (with 7.75 A at the tail), and 200 to 280 lm sustained at Anduril's default 45 °C limit. Thermal regulation is a must on this light.

Reviewers saw stock Anduril cut out at about 2.99 V, most likely the cell at rest afterwards (see [the driver](#the-driver)). Parasitic drain when off is 30 to 116 µA, and several mA with the button LED on high.

A separate Injoinic IP5310 chip handles charging (at about 1.6 A), so the firmware leaves it alone. The teardown found it on a 2021 light, and it's likely the same now, since no Anduril source gives it a pin. The button LED is an amber and blue pair, and only the amber half has an MCU pin, so the blue half probably belongs to the charger.

These come from [the 2021 teardown](https://budgetlightforum.com/t/wurkkos-ts21-tear-down/68750), [1Lumen](https://1lumen.com/review/wurkkos-ts21/), [ZeroAir](https://zeroair.org/2021/10/12/wurkkos-ts21-anduril-flashlight-review/), [rockenrooster](https://reviews.rockenrooster.dedyn.io/wurkkos-ts21-review/) and [the BLF review](https://budgetlightforum.com/t/review-wurkkos-ts21-sst20-4000k-anduril-1-an-excellent-budget-enthusiast-light/68189). They all predate the switch to the ATtiny1616, and nobody's said the rest of the light changed with it.

## The button LED

There's an amber LED and a blue one under the switch. The blue one lights while charging. Confirmed, by an owner. Only the amber one seems to have an MCU pin, PB5, so the blue one is likely the charger's.

There are no aux LEDs on the face. [1Lumen](https://1lumen.com/review/wurkkos-ts21/) says the light has none beyond the switch, and [LiquidRetro](https://liquidretro.net/2022/02/06/wurkkos-ts21-review-3200-lumens-sst20-90-cri-21700-usb-c/) says Anduril's aux channel "on the TS21 is on the button". Confirmed. Some of the switch's light leaks out through the optic, and there's a foil disc that's meant to stop it ([BLF modding thread](https://budgetlightforum.com/t/wurkkos-ts21-modding-advice/69374)). Anduril's TS10 config calls PB5 a "front-facing aux LED" because the TS10 V2 has real front aux; the TS21 shares the pin map, not the LEDs.

Anduril drives it at three levels in `mcu_set_aux1_power()` ([`arch/attiny1616.c`](https://github.com/ToyKeeper/anduril/blob/trunk/arch/attiny1616.c), read on trunk, not at the pinned hash). Confirmed from the code, likely for the TS21 since it comes from the TS10 config:

| Level | Pin | Current through the LED |
|---|---|---|
| Off | Output, driven low | None |
| Low | Input, `PINnCTRL.PULLUPEN` set | Whatever the pull-up lets through, tens of µA |
| High | Output, driven high | Set by the resistor on the board, a few mA |

Low works because the pull-up is a weak resistor from the pin to VCC (20 to 50 kΩ, 35 kΩ typical, DS40002204A Electrical Characteristics). With the pin an input, the only path for that current is through the LED to ground, and LEDs are visible at tens of µA. Roughly (VCC − the LED's forward voltage) / 35 kΩ, about 50 µA with a full cell and an amber LED at about 2 V (my estimate). It dims as the cell runs down. There's nothing between low and high: the pull-up's strength isn't adjustable, and PB5 isn't on a timer channel this firmware uses, so anything in between would mean software PWM and waking the chip all the time. The pull-up is disconnected whenever the pin is an output, and pin settings don't change in any sleep mode (DS40002204A §16.3.5), so low survives power-down. Confirmed.

The two measurements in [the teardown thread](https://budgetlightforum.com/t/wurkkos-ts21-tear-down/68750) put low at 50 to 64 µA over off (116 against 180 µA, and 30 against 80 µA), and high at anywhere from 3 to 13.5 mA. Likely; neither says which firmware.

This firmware leaves it on low all the time. PB5 isn't in `PORTB_OUTPUTS` in `src/hw.rs`, so `hw::init` gives it a pull-up like every other pin it doesn't drive, and nothing touches it after that.

## Open questions

- Is PB5 really the button LED's amber element on the TS21? It's the only aux pin in the config Wurkkos ships, the light has no other aux, and [LiquidRetro's review](https://liquidretro.net/tag/wurkkos-ts21-user-manual/) has amber as the locator, so it's very likely. Seeing it glow after a flash of this firmware settles it.
- How close is the 0614 voltage calibration on this light to better than 0.1 V (it's within that at 3.57 to 4.08 V, see [the driver](#the-driver)), what is it near 3 V, and how much does the cell sag on the FET at turbo? That decides whether 3.10 V and 2.99 V under load are in the right place.
- Does the TS21 driver match the TS10 exactly (same 7135 current, same FET), or only in pinout? The calibration numbers differ slightly between the TS10 and Q8 builds.
