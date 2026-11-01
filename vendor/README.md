# vendor

Third-party files kept in the repo so register checks work offline and against a fixed copy. Nothing here is compiled into the firmware or xtask.

## attiny1617.atdf

Microchip's device description for the ATtiny1617, which shares its die and register map with the ATtiny1616 (the 1616 is the 20-pin package). It's the source for every register address, bit mask and interrupt vector in `src/hw.rs`, `src/rt.rs` and `xtask/src/updi.rs`.

- Copied from [avr-device](https://github.com/Rahix/avr-device/blob/0bdf99ce7fa92a128de2bbb5d0303b8f0e633c7f/vendor/attiny1617.atdf) at commit `0bdf99ce7fa92a128de2bbb5d0303b8f0e633c7f`, which took it from Microchip's ATtiny device pack.
- SHA-256 `f1be2ffb227bd84cc40ffde250eff00a33b0007b55bacbf9d1c5ee932928f603`.
- Copyright Microchip Technology Inc., Apache License 2.0. `LICENSE` is the notice avr-device ships with it and `LICENSE-APACHE-2.0.txt` is the full licence text. This file isn't covered by the repo's MIT licence.

Microchip's own `ATtiny1616.atdf` is in their ATtiny device pack at packs.download.microchip.com if you want the exact part. To update this copy, fetch it from a newer avr-device commit, then update the commit and hash above.
