# wurkkos-ts21

Rust firmware for the Wurkkos TS21 v2 torch, with a smaller UI than [Anduril](https://github.com/ToyKeeper/anduril).

> [!WARNING]
> This isn't ready to carry yet. It doesn't sleep when off (it'll flatten a cell in days), and it has no thermal regulation or low-voltage protection. On a light with a direct-drive FET and a 21700, that matters. See the [roadmap](docs/roadmap.md).

## Using it

For now it's one click on, one click off, at a single level on the linear channel. That's enough to prove the switch, the PWM and the pin map on a real torch. The full UI (ramping, turbo, lockout) is waiting on the `fancy-ui` branch.

[docs/ui.md](docs/ui.md) is the full spec, including what's still to come.

To try it without a torch, `cargo xtask simulate` runs the UI live in your terminal, and `cargo xtask simulate 1C 2H:1.5s 3C` runs a script of clicks and holds.

## Building

You need [rustup](https://rustup.rs). The pinned nightly, `rust-src` and everything else come down on the first build.

```sh
cargo fw            # target/avr-none/release/ts21.elf
cargo xtask hex     # target/ts21.hex
cargo xtask size    # flash and RAM use
```

[docs/toolchain.md](docs/toolchain.md) explains how it builds and links.

## Flashing

Put a serial UPDI programmer on the R, G and V pads and `cargo xtask` does the rest. Check it can see the chip, back up the factory firmware, then flash.

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX
cargo xtask flash /dev/cu.usbserial-XXXX
```

[docs/flashing.md](docs/flashing.md) covers programmers, pad order, restoring the backup and getting back to Anduril.
