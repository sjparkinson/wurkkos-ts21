# Contributing

## Checks

CI runs these on every pull request, so run them before you push.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo fw-clippy
cargo xtask size
cargo xtask hex
```

The first three run on your machine's own target. `cargo fw-clippy` lints the firmware as the AVR sees it, which is the only place `src/hw.rs` and `src/rt.rs` get linted. `cargo xtask size` builds the real firmware, checks the linked image against what those two files assume (`xtask/src/layout.rs`: the vector table, where startup copies and zeroes memory, and that each protected-register unlock is followed straight away by its write), and prints how much of the 16 KB of flash and 2 KB of RAM it uses, and `cargo xtask hex` writes the file you'd flash. CI keeps that hex and the ELF as a build artefact.

## Layout

```
src/lib.rs        the no_std library: everything testable, nothing hardware
src/button.rs     switch samples in, clicks and holds out
src/ramp.rs       brightness levels to PWM duty cycles
src/temp.rs       temperature sensor readings to kelvin
src/ui.rs         the state machine docs/ui.md describes
src/main.rs       the firmware loop (a stub on the host)
src/regs.rs       register addresses and bit values, checked against the ATDF
src/hw.rs         pins and register access (AVR only)
src/rt.rs         vector table and startup (AVR only)
link.x            memory layout for the linker
xtask/            hex, size and the UPDI programmer (ping, backup, flash, restore)
docs/             hardware, flashing, toolchain, UI spec, roadmap
vendor/           Microchip's ATDF for the part (Apache-2.0)
```

Keep logic in the library, where `cargo test` can reach it. `src/main.rs` should stay a loop that moves values between `hw` and the library.

## Registers

There's no PAC. When you need a register, add its address or bitfield value to `src/regs.rs`, taken from the ATtiny1617 ATDF in `vendor/attiny1617.atdf` (same register map as the 1616), with a comment naming the register and bitfield. `src/hw.rs` uses the constant, not the number.

Then add a row for it to the tests in `xtask/src/atdf.rs`, which parse the ATDF and compare every constant in `src/regs.rs` against it. You can't forget: a constant no test reads is dead code in the xtask build, and clippy fails on it. The NVM and memory-map constants in `xtask/src/updi.rs` get the same check in that file's tests.

The tests only cover what the ATDF can say: addresses, widths, masks and named values. Write order, timing and which flags set without their interrupt enabled are in the datasheet, so have someone (or the `register-checker` agent in `.claude/agents/`) read those against it. A wrong address compiles fine and does something to the hardware you didn't ask for.

Never write fuses. Changing `SYSCFG0` can disable UPDI, and then the light needs a high-voltage programmer to recover.

## Unsafe

`unsafe_code` is denied across the workspace. Only `src/hw.rs`, `src/rt.rs` and the `#[unsafe(no_mangle)]` on `main` opt out, and every `unsafe` block gets a `// SAFETY:` comment.

## Bumping the nightly

AVR is a tier 3 target. Pick a nightly at least a week old, change `rust-toolchain.toml`, run every check above, and compare `cargo xtask size` before and after. Then see whether the workarounds in [docs/toolchain.md](docs/toolchain.md) are still needed.

## Commits

Single-line [Conventional Commits](https://www.conventionalcommits.org), such as `feat(ui): add battery check on 3C`. PRs merge by rebase only, so every commit on a branch lands on main.

## Workflows

Actions are pinned in `.github/workflows/actions.lock`, a [workflow lockfile](https://docs.github.com/actions/concepts/security/workflow-dependency-lockfiles) that records the commit each `uses:` resolves to. Rerun `gh actions-lock` (`gh extension install github/gh-actions-lock`) after adding a workflow or changing any `uses:`, and commit the lockfile with it. Dependabot updates the lockfile in its own PRs.

## Claude Code hooks

`.claude/settings.json` wires up two Ruby hooks (standard library only).

- `.claude/hooks/post-tool-use.rb` runs after every Edit or Write. It runs `rustfmt --check` and clippy for `.rs` files (`cargo fw-clippy` for `src/hw.rs` and `src/rt.rs`), `ruby -wc` for `.rb` files, and `gh actions-lock --verify-local` for workflows.
- `.claude/hooks/pre-tool-use.rb` runs before any `git commit`. It rejects a message that isn't a Conventional Commit, then runs `cargo test --workspace --locked -q` and blocks the commit if it fails.
