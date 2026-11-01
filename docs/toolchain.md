# Toolchain

How Rust gets onto an ATtiny1616 here, and why it's put together this way.

## The short version

- rustc's built-in `avr-none` target with `-C target-cpu=attiny1616`.
- A pinned nightly (`rust-toolchain.toml`), because `core` has to be rebuilt for AVR with `-Zbuild-std=core`, and inline assembly on AVR still needs `#![feature(asm_experimental_arch)]`.
- Linked by the `rust-lld` that rustup ships, against this repo's own linker script (`link.x`) and startup code (`src/rt.rs`).
- No crate dependencies. Not avr-device, not avr-hal, not ravedude. (Rebuilding `core` pulls a few crates of its own, see [First build](#first-build).)
- No avr-gcc and no avr-libc. The only thing to install outside rustup is avrdude, and only to flash.

`cargo fw` builds `target/avr-none/release/ts21.elf`. `cargo xtask hex` turns it into Intel HEX.

## Why no avr-gcc

`avr-none` defaults to linking with `avr-gcc`, which brings avr-libc's startup files and linker scripts along. That's three more things to install (on macOS, via a Homebrew tap) for maybe 60 bytes of startup code. Writing that code here means:

- the vector table is in `src/rt.rs`, where you can read it;
- `.data` and `.bss` set-up is twenty lines of assembly in the same file;
- the memory layout is in `link.x`, including the bit avr-gcc's default scripts handle awkwardly: read-only data stays in flash and is read through the mapped-flash window at 0x8000.

`rust-lld` handles AVR relocations fine. I checked the output by having rustc emit assembly (`cargo rustc --release --target avr-none -Zbuild-std=core -- --emit asm=fw.s`).

### The catch: 32-bit multiplication

The rustc book says `avr-none` needs avr-gcc "because a couple of intrinsics (like 32-bit multiplication) rely on libgcc". That's true. The ATtiny1616 has no hardware multiplier, and LLVM lowers a runtime `u32` or `u64` multiply to a call to `__mulsi3`, which `compiler_builtins` doesn't provide for AVR. Without libgcc the link fails:

```
rust-lld: error: undefined symbol: __mulsi3
```

I tested what does work without it: 8- and 16-bit multiplication, and division and remainder at every width up to 32 bits, all come from `compiler_builtins`. Multiplying constants at compile time (as `src/ramp.rs` does in a `const fn`) is fine too.

So the rule for now: keep runtime multiplication to 16 bits. If something genuinely needs a 32-bit product (the temperature sensor calibration will), the options are, in order of preference:

1. Do it as 16 × 16 → 32 in a small shift-and-add helper in the library, tested on the host.
2. Write `__mulsi3` in `src/rt.rs`. libgcc's version uses a non-standard calling convention (fewer clobbered registers), so a Rust function with the C ABI isn't a safe drop-in. It would need to be assembly that matches libgcc's contract.
3. Give in and link libgcc from avr-gcc.

If the link ever fails with an undefined `__*` symbol, this is why.

## Why no PAC or HAL

[avr-device](https://github.com/Rahix/avr-device) has no `attiny1616` feature (it has the 1614 and 1617), and [avr-hal](https://github.com/Rahix/avr-hal) has no tinyAVR 1-series support. Using the 1617's PAC would probably work, since it's the same die, but this firmware touches about fifteen registers. Writing them out by hand with a comment pointing at the ATDF is less code than the dependency, and the `register-checker` agent in `.claude/agents/` can verify them.

## Workarounds in use

Each has a comment where it lives. Remove it when a nightly makes it unnecessary.

| Where | What | Why |
|---|---|---|
| `.cargo/config.toml` | `--cfg no_fp_fmt_parse` | `core`'s float formatting needs a bignum type that only exists for 32- and 64-bit pointers on the pinned nightly, so it fails to compile for AVR. The firmware never formats floats, so cutting it out costs nothing. |
| `src/main.rs` | `#![feature(asm_experimental_arch)]` | `asm!` and `global_asm!` on AVR aren't stable. |
| `Cargo.toml` | `panic = "abort"` in both profiles | AVR can't unwind. |
| Library code | No runtime `u32`/`u64` multiplication | There's no `__mulsi3` without libgcc. See above. |
| `xtask` | Its own ELF-to-HEX conversion | The `rust-objcopy` in the toolchain directory is rustc's private copy and won't run on its own, and `cargo-binutils` would be a dependency. |

## Host builds

`cargo test`, `cargo clippy` and `cargo fmt` run on your machine as normal. The library (`src/lib.rs` and below) is `no_std` and has no hardware access, so it builds for both. `src/main.rs` compiles to a stub on anything but AVR, and `src/hw.rs` and `src/rt.rs` only exist on AVR. That's why there's a separate `cargo fw-clippy`.

## First build

The first `cargo fw` downloads the pinned nightly with `rust-src`, and then the handful of crates that `core`'s build needs from crates.io. After that it's offline.
