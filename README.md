# wurkkos-ts21

Rust firmware for the Wurkkos TS21 torch (model 0614).

> [!WARNING]
> This isn't ready to carry yet. Its power-down when off, thermal regulation and low-voltage protection have only run on a simulator, not a real light. On a light with a direct-drive FET and a 21700, that matters. See the [roadmap](docs/roadmap.md).

## Using it

See [docs/ui.md](docs/ui.md) for the full specification.

Try the behaviour out without a torch, `cargo xtask simulate` runs the UI live in your terminal, and `cargo xtask simulate 1C 2H:1.5s 3C` runs a script of clicks and holds.

## Building

You need [rustup](https://rustup.rs). The pinned nightly, `rust-src` and everything else come down on the first build.

```sh
cargo xtask hex     # build the firmware, write target/wurkkos-ts21-0614.hex and print flash and RAM use
cargo test --workspace
```

See [docs/toolchain.md](docs/toolchain.md) for an explanation of how it builds and links.

## Flashing

Put a serial UPDI programmer on the R, G and V pads and `cargo xtask` does the rest. Check it can see the chip, back up the factory firmware, then flash.

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX
cargo xtask flash /dev/cu.usbserial-XXXX    # builds first
```

See [docs/flashing.md](docs/flashing.md) for programmers, pad order, restoring the backup and getting back to Anduril.

## Acknowledgements

- [Anduril](https://github.com/ToyKeeper/anduril), by ToyKeeper, for the click notation and most of what's known about this hardware.
- [pymcuprog](https://github.com/microchip-pic-avr-tools/pymcuprog), which the UPDI programmer's sequences follow.
- [AVRDUDE](https://github.com/avrdudes/avrdude), whose flashing logic we reviewed when writing the UPDI programmer.
- Microchip, for the ATDF in `vendor/` and the datasheets.
