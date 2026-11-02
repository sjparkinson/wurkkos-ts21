# Hardware

What's inside a TS21 v2, and how sure I am about each bit. "Confirmed" means a primary source (Microchip's files, Anduril's hardware definitions or someone who opened one up) says so. "Likely" means it follows from a sibling light or a convention and nobody's contradicted it.

## Spotting a v2

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

Temperature comes from the MCU's internal sensor, using the factory calibration in `SIGROW.TEMPSENSE0` and `TEMPSENSE1`. The datasheet (DS40002204A, section 30.3.2.6) gives kelvin as `((result - TEMPSENSE1) × TEMPSENSE0 + 0x80) >> 8`, where TEMPSENSE1 is a signed offset and TEMPSENSE0 an unsigned gain. That product runs to 19 bits, but splitting the difference into high and low bytes gets the exact answer from 16-bit multiplies (`src/temp.rs` has the working). The measurement wants the 1.1 V internal reference, `SAMPCAP` set, and `INITDLY` and `SAMPLEN` each at least 32 µs of ADC clock. Typical accuracy is ±3 °C at 25 °C, at 0.55 °C a step (section 36.17). Anduril accumulates 4× and keeps the result as kelvin × 64.

All of this comes from [`hw/wurkkos/ts10/hwdef.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/wurkkos/ts10/hwdef.h), [`hw/sofirn/blf-q8-t1616/hwdef.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/sofirn/blf-q8-t1616/hwdef.h), [`hw/sofirn/blf-q8-t1616/anduril.h`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/hw/sofirn/blf-q8-t1616/anduril.h) for the ramp tables, and [`arch/attiny1616.c`](https://github.com/ToyKeeper/anduril/blob/71fe3012202e6a31ccb4aaf0a569b324a3c8826c/arch/attiny1616.c) for voltage and temperature. Anduril is GPL-3.0, so this repo takes facts about the board from it and no code.

## The R, G, V pads

Unscrew the head and they're on the driver. They're UPDI.

| Pad | Job |
|---|---|
| R | UPDI data (the ATtiny1616's reset pin doubles as UPDI) |
| G | Ground |
| V | VCC, 3.3 to 5 V from the programmer depending on which one |

People who've opened one confirm the labels. What each one does is the standard three-pad layout gchart's t1616 drivers use, which other lights label `+ - R` or, read from the other end, `R - +` ([pingle.org](https://www.pingle.org/2022/08/07/flashing-a-3-pin-t1616-flashlight-with-new-firmware)). Likely, bordering on confirmed. The pads are at 1.27 mm pitch, the same as gchart's layout, so a standard 1.27 mm probe fits. Confirmed by measuring a TS21 v2 (it's nowhere near 2.54 mm). Pad order varies between lights, so before you press a programmer onto them, check with a multimeter that G is continuous with the battery negative.

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

Stock Anduril cuts out at about 2.99 V. Parasitic drain when off is 30 to 116 µA, and several mA with the button LED on high.

A separate Injoinic IP5310 chip handles charging (at about 1.6 A), so the firmware leaves it alone. The teardown found it on a 2021 light, and it's likely the same now, since no Anduril source gives it a pin. The button LED is an amber and blue pair, and only the amber half has an MCU pin, so the blue half probably belongs to the charger.

These come from [the 2021 teardown](https://budgetlightforum.com/t/wurkkos-ts21-tear-down/68750), [1Lumen](https://1lumen.com/review/wurkkos-ts21/), [ZeroAir](https://zeroair.org/2021/10/12/wurkkos-ts21-anduril-flashlight-review/), [rockenrooster](https://reviews.rockenrooster.dedyn.io/wurkkos-ts21-review/) and [the BLF review](https://budgetlightforum.com/t/review-wurkkos-ts21-sst20-4000k-anduril-1-an-excellent-budget-enthusiast-light/68189). They all predate the switch to the ATtiny1616, and nobody's said the rest of the light changed with it.

## Open questions

- Is PB5 really the button LED's amber element on the TS21? Anduril's TS10 config calls it front-facing aux, and the SP36 config talks about a button LED. A multimeter on the driver would settle it, and so would flashing a build that blinks it. [LiquidRetro's review](https://liquidretro.net/tag/wurkkos-ts21-user-manual/) of the Anduril 2 light describes red, blue and amber under the switch, with amber as the locator, which fits PB5 being amber and the charger owning the rest.
- What lights the button LED's blue element? The charger, or something else?
- Does the TS21 driver match the TS10 exactly (same 7135 current, same FET), or only in pinout? The calibration numbers differ slightly between the TS10 and Q8 builds.
