# Contributing

## Checks

CI runs these on every pull request, so run them before you push.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo clippy --package ts21 --release --target avr-none -Zbuild-std=core -- -D warnings
cargo xtask hex
```

The first three run on your machine's own target. The second clippy lints the firmware as the AVR sees it, which is the only place `src/hw.rs` and `src/rt.rs` get linted. `cargo xtask hex` builds the real firmware, checks the linked image against what those two files assume (`xtask/src/layout.rs`: the vector table, where startup copies and zeroes memory, and that each protected-register unlock is followed straight away by its write), runs it on a simulated ATtiny1616 (`xtask/src/avr.rs` and `sim.rs`: startup, the power-on register and RAM tests against every stuck register bit, every short between two bits and a spread of broken and coupled RAM, the periodic RAM test on a good sweep and with RAM going bad mid-run, register rules like low-byte-first and the CCP window, that the watchdog is on in window mode and every feed lands in the open window, the watchdog's proof on a good chip, on every broken watchdog and with a press on each of its boots, what each kind of reset leads to, the clock check against dead, stopped and wrong clocks, interrupts and idle sleep, the fuse row and a CRCSCAN that sums the same signed image it writes, every example in docs/ui.md, and twenty thousand ticks of random clicks and holds checked against the library tick by tick, then the examples and the ticks again with interrupts landing mid-pass, on every instruction, which has to catch a handler between a TCA0 low and high byte and hold each handler to giving back r0 to r31, SREG and SP), runs `src/bin/pwm_probe.rs` to check the real `hw::set_output` lands every level whole and the real `hw::count_tick` passes a good chip at the longest and shortest cycles, then that and the examples again with a BOTTOM under LUPD read the other way, does the same layout checks and a simulator run on `src/bin/link_probe.rs` (a throwaway program with a const table, a `.data` static and a `.bss` static, since the firmware has none of those yet), writes the file you'd flash, padded to 16 KB with the CRC the firmware checks at power-on, and prints how much of the 16 KB of flash and 2 KB of RAM it uses. CI keeps that hex and the ELF as a build artefact.

## Layout

```
src/lib.rs        the no_std library: everything testable, nothing hardware
src/button.rs     switch samples in, clicks and holds out
src/gradual.rs    a level that fades towards a target, at most a level a tick
src/ramp.rs       brightness levels to PWM duty cycles
src/temp.rs       temperature sensor readings to sixteenths of a kelvin
src/battery.rs    battery readings to centivolts, and low-voltage protection
src/thermal.rs    temperatures in, a ceiling on the level out
src/sensors.rs    which ADC channel to read each tick, and when a reading's due
src/limits.rs     ADC readings in, the UI's level capped by the cell and the heat out
src/selftest.rs   the self-tests' rules, the clock check and the RAM sweep, and what a failure means
src/ui.rs         the state machine docs/ui.md describes
src/torch.rs      one pass of the firmware loop: switch sample and ADC result in, level out
src/watchdog.rs   the watchdog's window, which ticks feed it, and the proof that it works
src/main.rs       the firmware loop (a stub on the host)
src/regs.rs       register addresses and bit values, generated from the ATDF by build.rs
src/hw.rs         pins and register access (AVR only)
src/rt.rs         vector table and startup (AVR only)
src/bin/link_probe.rs  a program with one static of each kind, so xtask can check link.x (never flashed)
src/bin/pwm_probe.rs   a program that runs hw::set_output through every level and counts the clock, for the simulator (never flashed)
link.x            memory layout for the linker, checked against the ATDF in xtask/src/atdf.rs
xtask/            hex, size, the UPDI programmer (ping, backup, flash, restore) and the UI simulator
docs/             hardware, flashing, toolchain, UI spec, roadmap
docs/microchip/   a list of Microchip's datasheet and app notes; the PDFs go here locally, gitignored
vendor/           Microchip's ATDF for the part (Apache-2.0)
```

Keep logic in the library, where `cargo test` can reach it. `src/main.rs` should stay a loop that moves values between `hw` and the library.

## Registers

There's no PAC, and nothing to add when you need a register. `build.rs` generates a constant for every register, bitfield, named value, memory segment and vector in `vendor/ATtiny1616.atdf`, and `src/regs.rs` includes them, so use the one you want: `regs::addr::RTC_PITCTRLA`, `regs::RTC_PITEN`, `regs::RTC_PERIOD_CYC512`, `regs::mem::EEPROM_START`, `regs::VECTOR_RTC_PIT`. The names are the ATDF's, and the comment at the top of `src/regs.rs` has the rules. `src/hw.rs` uses the constant, not the number. xtask gets the same constants through `ts21::regs`. The firmware pays nothing for the ones it doesn't use.

The generator is `generate` in `xtask/src/atdf.rs`, which `build.rs` loads by path, and its tests are in that file. Those also check what generating can't: what `src/hw.rs` assumes about the ATDF's layout, and `link.x`. If you need a number that isn't in the ATDF, like `ADC_SAMPLEN_32US`, it goes in `src/regs.rs` by hand, with a comment saying why and a check against whatever the ATDF does say about it. Reset values stay out on purpose, because the ATDF's disagree with the datasheet on `ADC0.CALIB` and the factory `OSCCFG` and `SYSCFG0`.

The ATDF only says so much: addresses, widths, masks and named values. Picking the wrong constant still compiles, and write order, timing and which flags set without their interrupt enabled are in the datasheet, so have someone (or the `register-checker` agent in `.claude/agents/`) read those against it. A wrong register does something to the hardware you didn't ask for.

The simulator in `xtask/src/sim.rs` refuses any register it doesn't model, so the first `cargo xtask hex` after you touch a new one fails until you teach the fake chip about it. Model what the datasheet says the register does, including any rule the real chip enforces by quietly ignoring a write, and add a test for it in `sim.rs`.

Never write fuses. Changing `SYSCFG0` can disable UPDI, and then the light needs a high-voltage programmer to recover.

## Unsafe

`unsafe_code` is denied across the workspace. Only `src/hw.rs`, `src/rt.rs`, the `#[unsafe(no_mangle)]` on `main`, and the `no_mangle` items in `src/bin/link_probe.rs` and `src/bin/pwm_probe.rs` opt out, and every `unsafe` block gets a `// SAFETY:` comment.

## Bumping the nightly

AVR is a tier 3 target. Dependabot opens a PR with the latest nightly every week, and CI runs every check above on it. To bump by hand, pick a nightly, change `rust-toolchain.toml` and run those checks. Either way, compare `cargo xtask hex` before and after, then see whether the workarounds in [docs/toolchain.md](docs/toolchain.md) are still needed.

## Commits

Single-line [Conventional Commits](https://www.conventionalcommits.org), such as `feat(ui): add battery check on 3C`. PRs merge by rebase only, so every commit on a branch lands on main.
