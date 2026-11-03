# Microchip references

Microchip's datasheet, errata and app notes that matter for this firmware. The PDFs live in this directory on your machine but aren't in git: each one says "All Rights Reserved" and "No licenses are conveyed", so this public repo can't redistribute them. (The ATDF in `vendor/` is different; it's Apache-2.0.) `.gitignore` covers `*.pdf` here and the `txt/` cache the `microchip-docs` agent writes.

Each file is named `<document number><revision>-<short title>.pdf`, using the number printed on the document: the AN or TB number for app notes, the DS number for the rest. Microchip's download names are all over the place, so save each one under the name in the table, for example:

```sh
cd docs/microchip
curl -Lo AN2447A-measure-vcc-without-io-pin.pdf https://ww1.microchip.com/downloads/en/AppNotes/00002447A.pdf
```

Then check you've got the same revisions:

```sh
shasum -a 256 -c SHA256SUMS
```

A newer revision will fail the check. That's fine if you want it, but rename the file to its new revision letter, update the table and `SHA256SUMS` in the same commit, and note what changed if it affects anything in `docs/hardware.md`.

## The part

| File | Document | What it's for here |
|---|---|---|
| `DS40002204A-attiny1614-16-17-datasheet.pdf` | [ATtiny1614/1616/1617 datasheet](https://ww1.microchip.com/downloads/en/DeviceDoc/ATtiny1614-16-17-DataSheet-DS40002204A.pdf) | The reference for every peripheral, timing and electrical limit |
| `DS80000886C-attiny1614-16-17-errata.pdf` | [Silicon errata and datasheet clarifications](https://ww1.microchip.com/downloads/en/DeviceDoc/ATtiny1614-16-17-SilConErrataClarif-DS80000886C.pdf) | Overrides the datasheet. Check it for any peripheral you touch |
| `DS40002198C-avr-instruction-set.pdf` | [AVR instruction set manual](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/ReferenceManuals/AVR-InstructionSet-Manual-DS40002198.pdf) | Instruction encodings and cycle counts (AVRxt column) for reading `cargo fw` output and the simulator in `xtask/src/avr.rs`. The older `/en/DeviceDoc/` URL serves revision B |

## App notes

| File | Document | What it's for here |
|---|---|---|
| `AN1982B-interrupt-system.pdf` | [The tinyAVR 0/1 interrupt system](https://ww1.microchip.com/downloads/en/AppNotes/40001982B.pdf) | The CPUINT controller, for the first real interrupt handler (sleep, roadmap stage 2) |
| `AN1983A-writing-flash-and-eeprom.pdf` | [Writing flash and EEPROM on the tinyAVR 1-series](https://ww1.microchip.com/downloads/en/AppNotes/AN1983%20Writing%20to%20Flash%20and%20EEPROM%20on%20the%20tinyAVR%201-series%2040001983A.pdf) | Remembering the level in EEPROM, and the NVM controller the UPDI programmer drives |
| `AN2447A-measure-vcc-without-io-pin.pdf` | [Measuring VCC without an I/O pin](https://ww1.microchip.com/downloads/en/AppNotes/00002447A.pdf) | The battery reading method `src/battery.rs` uses |
| `AN2515C-low-power-techniques.pdf` | [AVR low-power techniques](https://ww1.microchip.com/downloads/en/Appnotes/AN2515-AVR-Low-Power-Techniques-00002515C.pdf) | Sleeping when off |
| `AN2519B-hardware-design-considerations.pdf` | [AVR hardware design considerations](https://ww1.microchip.com/downloads/en/Appnotes/AN2519-AVR-Microcontroller-Hardware-Design-Considerations-00002519B.pdf) | Reading the driver board: decoupling, reset and UPDI pin |
| `AN2551C-adc-noise-countermeasures.pdf` | [Noise countermeasures for ADC applications](https://ww1.microchip.com/downloads/en/Appnotes/AN2551-Noise-Countermeasures-for-ADC-Applications-00002551C.pdf) | Why a reading jumps while the FET is switching |
| `AN2572C-adc-oversampling.pdf` | [ADC oversampling with tinyAVR and megaAVR](https://ww1.microchip.com/downloads/en/Appnotes/AN2572-ADC-Oversampling-with-tinyAVR-and-megaAVR-00002672C.pdf) | The accumulated conversions in `src/sensors.rs`. Microchip's file is numbered 00002672C, but the document says AN2572 |
| `AN2632A-iec-60730-class-b.pdf` | [IEC 60730 Class B with the tinyAVR 1-series](https://ww1.microchip.com/downloads/en/AppNotes/00002632A.pdf) | Self-tests (watchdog in window mode, clock, flash CRC, RAM) for making a hang or a fault fail safe |
| `AN2633B-ulp-timing-32khz-oscillators.pdf` | [Low-power timing with the 32 kHz oscillators](https://ww1.microchip.com/downloads/en/Appnotes/ULPTim-32768kHz-EXT-XTAL-OSC-ULPIntOsc-DS00002633B.pdf) | The RTC and PIT tick, and how accurate the internal ULP oscillator is |
| `AN2747C-robustness.pdf` | [Robustness of tinyAVR 0/1 and megaAVR 0](https://ww1.microchip.com/downloads/en/Appnotes/Robustness-tinyAVR-0-1-megaAVR-0-series-DS00002747C.pdf) | BOD, watchdog and how the chip behaves at its edges |
| `AVR104A-buffered-eeprom-writes.pdf` | [Buffered interrupt-controlled EEPROM writes](https://ww1.microchip.com/downloads/en/Appnotes/doc2540.pdf) (Atmel doc2540) | Old (pre-1-series NVM), so take the ideas, not the register names |
| `TB3209B-getting-started-with-adc.pdf` | [Getting started with the ADC](https://ww1.microchip.com/downloads/en/Appnotes/TB3209-Getting-Started-with-ADC-DS90003209.pdf) | ADC setup for the battery and temperature readings |

Download links checked against these hashes on 2026-11-03. Microchip's www site blocks scripted downloads, but these `ww1.microchip.com` links work with `curl`.
