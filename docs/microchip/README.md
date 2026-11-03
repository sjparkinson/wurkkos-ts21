# Microchip references

Microchip's datasheet, errata and app notes that matter for this firmware. The PDFs live in this directory on your machine but aren't in git: each one says "All Rights Reserved" and "No licenses are conveyed", so this public repo can't redistribute them. (The ATDF in `vendor/` is different; it's Apache-2.0.) `.gitignore` covers `*.pdf` here and the `txt/` cache the `microchip-docs` agent writes.

To get them, download each link below into this directory under the filename shown, then check you've got the same revision:

```sh
cd docs/microchip && shasum -a 256 -c SHA256SUMS
```

A newer revision will fail the check. That's fine if you want it, but update the table and `SHA256SUMS` in the same commit, and note what changed if it affects anything in `docs/hardware.md`.

## The part

| File | Document | What it's for here |
|---|---|---|
| [ATtiny1614-16-17-DataSheet-DS40002204A.pdf](https://ww1.microchip.com/downloads/en/DeviceDoc/ATtiny1614-16-17-DataSheet-DS40002204A.pdf) | DS40002204A, ATtiny1614/1616/1617 datasheet | The reference for every peripheral, timing and electrical limit |
| [ATtiny1614-16-17-SilConErrataClarif-DS80000886C.pdf](https://ww1.microchip.com/downloads/en/DeviceDoc/ATtiny1614-16-17-SilConErrataClarif-DS80000886C.pdf) | DS80000886C, silicon errata and datasheet clarifications | Overrides the datasheet. Check it for any peripheral you touch |
| [AVR-InstructionSet-Manual-DS40002198.pdf](https://ww1.microchip.com/downloads/aemDocuments/documents/MCU08/ProductDocuments/ReferenceManuals/AVR-InstructionSet-Manual-DS40002198.pdf) | DS40002198C, AVR instruction set manual | Instruction encodings and cycle counts (AVRxt column) for reading `cargo fw` output and the simulator in `xtask/src/avr.rs`. The older `/en/DeviceDoc/` URL serves revision B |

## App notes

| File | Document | What it's for here |
|---|---|---|
| [00002447A.pdf](https://ww1.microchip.com/downloads/en/AppNotes/00002447A.pdf) | AN2447A, measuring VCC without an I/O pin | The battery reading method `src/battery.rs` uses |
| [40001982B.pdf](https://ww1.microchip.com/downloads/en/AppNotes/40001982B.pdf) | AN1982B, the tinyAVR 0/1 interrupt system | The CPUINT controller, for the first real interrupt handler (sleep, roadmap stage 2) |
| [AN1983WritingtoFlashandEEPROMonthetinyAVR1-series40001983A.pdf](https://ww1.microchip.com/downloads/en/AppNotes/AN1983%20Writing%20to%20Flash%20and%20EEPROM%20on%20the%20tinyAVR%201-series%2040001983A.pdf) | AN1983A, writing flash and EEPROM on the tinyAVR 1-series | Remembering the level in EEPROM, and the NVM controller the UPDI programmer drives |
| [doc2540.pdf](https://ww1.microchip.com/downloads/en/Appnotes/doc2540.pdf) | AVR104 (Atmel doc2540), buffered EEPROM writes | Old (pre-1-series NVM), so take the ideas, not the register names |
| [AN2515-AVR-Low-Power-Techniques-00002515C.pdf](https://ww1.microchip.com/downloads/en/Appnotes/AN2515-AVR-Low-Power-Techniques-00002515C.pdf) | AN2515C, AVR low-power techniques | Sleeping when off |
| [ULPTim-32768kHz-EXT-XTAL-OSC-ULPIntOsc-DS00002633B.pdf](https://ww1.microchip.com/downloads/en/Appnotes/ULPTim-32768kHz-EXT-XTAL-OSC-ULPIntOsc-DS00002633B.pdf) | DS00002633B, low-power timekeeping with the 32 kHz oscillators | The RTC and PIT tick, and how accurate the internal ULP oscillator is |
| [AN2519-AVR-Microcontroller-Hardware-Design-Considerations-00002519B.pdf](https://ww1.microchip.com/downloads/en/Appnotes/AN2519-AVR-Microcontroller-Hardware-Design-Considerations-00002519B.pdf) | AN2519B, AVR hardware design considerations | Reading the driver board: decoupling, reset and UPDI pin |
| [Robustness-tinyAVR-0-1-megaAVR-0-series-DS00002747C.pdf](https://ww1.microchip.com/downloads/en/Appnotes/Robustness-tinyAVR-0-1-megaAVR-0-series-DS00002747C.pdf) | DS00002747C, robustness of tinyAVR 0/1 and megaAVR 0 | BOD, watchdog and how the chip behaves at its edges |
| [AN2632-IEC60730-ClassB-tinyAVR1-00002632A.pdf](https://ww1.microchip.com/downloads/en/AppNotes/00002632A.pdf) | AN2632A, IEC 60730 Class B with the tinyAVR 1-series | Self-tests (watchdog in window mode, clock, flash CRC, RAM) for making a hang or a fault fail safe |
| [TB3209-Getting-Started-with-ADC-DS90003209.pdf](https://ww1.microchip.com/downloads/en/Appnotes/TB3209-Getting-Started-with-ADC-DS90003209.pdf) | TB3209B, getting started with the ADC | ADC setup for the battery and temperature readings |
| [AN2572-ADC-Oversampling-with-tinyAVR-and-megaAVR-00002672C.pdf](https://ww1.microchip.com/downloads/en/Appnotes/AN2572-ADC-Oversampling-with-tinyAVR-and-megaAVR-00002672C.pdf) | AN2572C (file numbered 00002672C), ADC oversampling | The accumulated conversions in `src/sensors.rs` |
| [AN2551-Noise-Countermeasures-for-ADC-Applications-00002551C.pdf](https://ww1.microchip.com/downloads/en/Appnotes/AN2551-Noise-Countermeasures-for-ADC-Applications-00002551C.pdf) | AN2551C, noise countermeasures for ADC applications | Why a reading jumps while the FET is switching |

Download links checked against these hashes on 2026-11-03. Microchip's www site blocks scripted downloads, but these `ww1.microchip.com` links work with `curl -LO`.
