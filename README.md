# wurkkos-ts21

Rust firmware for the Wurkkos TS21 torch (model 0614), with a smaller UI than [Anduril](https://github.com/ToyKeeper/anduril).

> [!WARNING]
> This isn't ready to carry yet. It doesn't sleep when off (it'll flatten a cell in days), and its thermal regulation and low-voltage protection have only run on a simulator, not a real light. On a light with a direct-drive FET and a 21700, that matters. See the [roadmap](docs/roadmap.md).

## Using it

See [docs/ui.md](docs/ui.md) for the full specification.

Try the behaviour out without a torch, `cargo xtask simulate` runs the UI live in your terminal, and `cargo xtask simulate 1C 2H:1.5s 3C` runs a script of clicks and holds.

## Building

You need [rustup](https://rustup.rs). The pinned nightly, `rust-src` and everything else come down on the first build.

```sh
cargo xtask hex     # build the firmware and write target/wurkkos-ts21-0614.hex
cargo xtask size    # build it and print flash and RAM use
cargo test --workspace
```

Plain `cargo build` builds for your machine, where the firmware is a stub, because the library and its tests live there. `cargo xtask hex` runs `cargo fw` (the AVR build, `target/avr-none/release/wurkkos-ts21-0614.elf`) before it writes the hex. `cargo xtask` lists everything else.

See [docs/toolchain.md](docs/toolchain.md) for an explanation of how it builds and links.

## Flashing

Put a serial UPDI programmer on the R, G and V pads and `cargo xtask` does the rest. Check it can see the chip, back up the factory firmware, then flash.

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX
cargo xtask flash /dev/cu.usbserial-XXXX    # builds first
```

Every merge to main publishes a [release](https://github.com/sjparkinson/wurkkos-ts21/releases) with the firmware attached as `wurkkos-ts21-0614-<version>.hex`. To flash one of those rather than your own build, pass it to `cargo xtask flash /dev/cu.usbserial-XXXX wurkkos-ts21-0614-<version>.hex`. Each file has a build provenance attestation, so `gh attestation verify wurkkos-ts21-0614-<version>.hex -R sjparkinson/wurkkos-ts21` checks it came from this repo's CI.

See [docs/flashing.md](docs/flashing.md) for programmers, pad order, restoring the backup and getting back to Anduril.
