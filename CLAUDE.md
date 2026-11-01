# CLAUDE.md

Firmware for the Wurkkos TS21 v2 torch (ATtiny1616), in Rust, with zero crate dependencies. Read README.md for what it is, CONTRIBUTING.md for the checks and layout, and docs/ for the hardware, toolchain and UI spec.

## Commands

```sh
cargo test --workspace --locked -q                         # host tests, run these constantly
cargo clippy --workspace --all-targets --locked -- -D warnings   # host lints
cargo fw                                                   # firmware ELF
cargo fw-clippy                                            # lint hw.rs and rt.rs, which the host never compiles
cargo xtask size                                           # flash/RAM use, after any firmware change
```

`/check` runs the lot. Plain `cargo test` only tests the root package, so always pass `--workspace`.

`cargo fw`, `fw-clippy` and the xtask commands rebuild `core` with `-Zbuild-std`. The first run on a machine fetches crates from crates.io and writes to `~/.cargo` and `~/.rustup`, which the sandbox may block. If it does, rerun outside the sandbox; nothing else in this repo needs that.

## Rules that bite

- **No dependencies.** Not in the firmware, not in xtask. If something seems to need a crate, write the small part you need, or ask first.
- **Never write fuses, and never suggest a command that does.** A wrong `SYSCFG0` disables UPDI, and only a high-voltage programmer gets it back. `cargo xtask flash` writes flash only.
- **Never run `cargo xtask flash` or `backup`, or avrdude/pymcuprog, yourself.** They touch real hardware on a serial port. Give the user the command.
- **Every register address and bit mask comes from the ATDF**, with a comment naming the register. Use the `register-checker` agent after touching `src/hw.rs` or `src/rt.rs`.
- **16-bit peripheral registers: low byte first.** Use `write16` in `src/hw.rs`.
- **Logic goes in the library** (`src/lib.rs` and its modules), which is `no_std`, has no `unsafe`, and is tested on the host. `src/main.rs` stays a thin loop.
- **docs/ui.md is the UI spec.** Change it in the same commit as `src/ui.rs`, and add a test in `src/ui.rs` for every row you add.
- **Tick off docs/roadmap.md** in the commit that does the work.
- **Anduril is GPL-3.0.** Read it for facts about the hardware (pins, calibration, timings) and cite the file. Don't copy its code or its ramp tables into this MIT repo.
- **No runtime 32- or 64-bit multiplication.** There's no hardware multiplier and no libgcc, so it fails to link with `undefined symbol: __mulsi3`. Host tests won't catch it; `cargo fw` will. 16-bit multiplies, all divisions, and `const fn` maths are fine. docs/toolchain.md has the options.
- **Flash is 16 KB and RAM is 2 KB.** Check `cargo xtask size` after changes. No `core::fmt` in the firmware (it pulls in kilobytes), and no floats.
- **Mark uncertainty.** docs/hardware.md labels facts as confirmed or likely. Keep doing that; don't promote a guess to a fact.

## Things that look wrong but aren't

- `--cfg no_fp_fmt_parse` in `.cargo/config.toml` works around `core` failing to build for 16-bit targets on the pinned nightly. See docs/toolchain.md.
- `.rodata` is linked at 0x808000 and up: AVR ELF files put data space at 0x800000, plus the mapped-flash window at 0x8000. It's how const tables are read without `lpm`.
- `src/main.rs` has a host `fn main()` that exits with an error. It's there so the workspace builds and tests on the host.
- `hw::wait_for_tick` busy-waits. Sleep is roadmap stage 2.
