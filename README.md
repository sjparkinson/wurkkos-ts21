# wurkkos-ts21

My own firmware for the Wurkkos TS21 torch, in Rust, with no crate dependencies.

The TS21 ships with [Anduril](https://github.com/ToyKeeper/anduril), which is excellent and does far more than I need. This is a smaller replacement: a UI I can hold in my head, code I can read in an evening, and an excuse to get Rust onto an 8-bit AVR without a C toolchain anywhere in the build.

> [!WARNING]
> This isn't ready to carry yet. It doesn't sleep when off (it'll flatten a cell in days), and it has no thermal regulation or low-voltage protection. On a light with a direct-drive FET and a 21700, that matters. See the [roadmap](docs/roadmap.md).

## Which TS21

The v2, from about 2023 on: ATtiny1616 inside, three pads labelled **R G V** on the driver, and a version check (15 or more clicks from off in Anduril) that ends in **0614**. TS21s made before about early 2023 have an ATtiny85 and aren't supported. [docs/hardware.md](docs/hardware.md) has the details.

## Using it

For now it's one click on, one click off, at a single level on the linear channel. That's enough to prove the switch, the PWM and the pin map on a real torch. The full UI (ramping, turbo, lockout) is waiting on the `fancy-ui` branch.

[docs/ui.md](docs/ui.md) is the full spec, including what's still to come.

## Building

You need [rustup](https://rustup.rs). The pinned nightly, `rust-src` and everything else come down on the first build.

```sh
cargo fw            # target/avr-none/release/ts21.elf
cargo xtask hex     # target/ts21.hex
cargo xtask size    # flash and RAM use
```

There's no avr-gcc involved. The firmware links with the `rust-lld` that ships with Rust, against a linker script and vector table in this repo. The one price is no runtime 32-bit multiplication, since that comes from libgcc. [docs/toolchain.md](docs/toolchain.md) explains how and why.

## Flashing

You need a serial UPDI programmer on the R, G and V pads, and that's all: the programmer software is built into `cargo xtask`, so there's no avrdude. Check it can see the chip, back up the factory firmware, then flash:

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
cargo xtask backup /dev/cu.usbserial-XXXX
cargo xtask flash /dev/cu.usbserial-XXXX
```

[docs/flashing.md](docs/flashing.md) covers programmers, pad order, restoring the backup and getting back to Anduril.

## Licence

MIT. The pin map comes from reading Anduril's hardware definitions (GPL-3.0), but no Anduril code is in here.
