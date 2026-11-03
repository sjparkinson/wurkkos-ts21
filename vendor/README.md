# vendor

Third-party files kept in the repo so register checks work offline and against a fixed copy. Nothing here is compiled into the firmware or xtask.

## ATtiny1616.atdf

Microchip's device description for the ATtiny1616. It's the source for every register address, bit mask, interrupt vector and fuse default in `src/hw.rs`, `src/rt.rs` and `xtask/src/updi.rs`.

- From Microchip's ATtiny device pack, [`Microchip.ATtiny_DFP.3.4.278.atpack`](https://packs.download.microchip.com/Microchip.ATtiny_DFP.3.4.278.atpack) (released 2026-04-11, SHA-256 `41f09d5825abaa764c9000f4488f8ba3fab152afe6229b359de4a5efe9a21eda`), file `atdf/ATtiny1616.atdf`, unchanged.
- SHA-256 `dda9db31c3a2942d0ecc0e7ed53254597c8f9eaa15c5225b8417dbaddca56f29`.
- Copyright Microchip Technology Inc., Apache License 2.0. `LICENSE` is the pack's `LICENSE.txt` and `LICENSE-APACHE-2.0.txt` is the full licence text. This file isn't covered by the repo's MIT licence.

The rest of the pack is C and assembler support (headers, startup objects, linker specs) and MPLAB device files, all generated from the same data, so it isn't vendored. `include/avr/iotn1616.h` is handy if you want the avr-libc names for something.

To update: the latest version is in the `Microchip.ATtiny_DFP.pdsc` entry of [the pack index](https://packs.download.microchip.com/index.idx). An `.atpack` is a zip. Replace the file with `atdf/ATtiny1616.atdf` from the new pack, update the version and both hashes above, and run `cargo test --workspace`, which checks every constant in the repo against it.
