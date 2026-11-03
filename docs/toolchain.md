# Toolchain

How Rust gets onto an ATtiny1616 here, and why it's put together this way.

## The short version

- rustc's built-in `avr-none` target with `-C target-cpu=attiny1616`.
- A pinned nightly (`rust-toolchain.toml`), because `core` has to be rebuilt for AVR with `-Zbuild-std=core`, and inline assembly on AVR still needs `#![feature(asm_experimental_arch)]`.
- Linked by the `rust-lld` that rustup ships, against this repo's own linker script (`link.x`) and startup code (`src/rt.rs`).
- Registers written out by hand in `src/hw.rs`, with zero crate dependencies. (Rebuilding `core` pulls a few crates of its own, see [First build](#first-build).)
- The UPDI programmer lives in `xtask`, so rustup is the only thing to install.

`cargo fw` builds `target/avr-none/release/wurkkos-ts21-0614.elf`. `cargo xtask hex` runs `cargo fw` and turns the ELF into Intel HEX. Cargo has no post-build step, so the hex can't come out of `cargo build` itself, and making `avr-none` the default target would break `cargo test` for the library.

## Linking with rust-lld

`avr-none` defaults to linking with `avr-gcc`, which brings avr-libc's startup files and linker scripts along. That's three more things to install (on macOS, via a Homebrew tap) for maybe 60 bytes of startup code. Writing that code here means

- the vector table is in `src/rt.rs`, where you can read it
- `.data` and `.bss` set-up is twenty lines of assembly in the same file
- the memory layout is in `link.x`, including the bit avr-gcc's default scripts handle awkwardly, where read-only data stays in flash and is read through the mapped-flash window at 0x8000

`rust-lld` handles AVR relocations fine. I checked the output by having rustc emit assembly (`cargo rustc --release --target avr-none -Zbuild-std=core -- --emit asm=fw.s`).

### 32-bit multiplication

The rustc book says `avr-none` needs avr-gcc "because a couple of intrinsics (like 32-bit multiplication) rely on libgcc". That's true. The ATtiny1616 does have a hardware multiplier (see [hardware.md](hardware.md)), but it's 8×8, and LLVM lowers a runtime `u32` or `u64` multiply to a call to `__mulsi3`, which `compiler_builtins` doesn't provide for AVR. Without libgcc the link fails with this.

```
rust-lld: error: undefined symbol: __mulsi3
```

The one that catches you out is dividing a `u16` by a constant. LLVM swaps `x / 10` and `x % 10` for a multiply by a magic number, done in 32 bits, so it calls `__mulsi3` too. I checked this on the nightly-2026-09-23 build: `x % 7` on a `u16` fails to link, and `x % core::hint::black_box(7)` links. Powers of two are shifts, so they're fine.

I tested what does work. 8- and 16-bit multiplication compile to `mul` and `muls` inline. So does dividing a `u8` by a constant (`x % 10` is a `mul`, a shift and a `muls`). Division and remainder by a runtime value, at every width up to 32 bits, come from `compiler_builtins`. Multiplying constants at compile time (as `src/ramp.rs` does in a `const fn`) is fine too.

So for now, keep runtime multiplication to 16 bits, and divide a `u16` only by a power of two or a runtime value. If something genuinely needs a 32-bit product, these are the options, in order of preference.

1. Split one operand into bytes so each partial product fits 16 bits. When the result is shifted right by 8 anyway, as with the temperature calibration in `src/temp.rs`, the low byte's product only needs its top half and the whole thing stays in 16 bits.
1. Do it as 16 × 16 → 32 in a small shift-and-add helper in the library, tested on the host.
2. Write `__mulsi3` in `src/rt.rs`. libgcc's version uses a non-standard calling convention (fewer clobbered registers), so a Rust function with the C ABI isn't a safe drop-in. It would need to be assembly that matches libgcc's contract.
3. Give in and link libgcc from avr-gcc.

If the link ever fails with an undefined `__*` symbol, this is why.

## Registers by hand

[avr-device](https://github.com/Rahix/avr-device) has the ATtiny1614 and 1617 but not the 1616, and [avr-hal](https://github.com/Rahix/avr-hal) has no tinyAVR 1-series support. Using the 1617's PAC would probably work, since it's the same die, but this firmware touches about fifteen registers. Writing them out by hand with a comment pointing at the ATDF is less code than the dependency, and the `register-checker` agent in `.claude/agents/` can verify them.

## Workarounds in use

Each has a comment where it lives. Remove it when a nightly makes it unnecessary.

| Where | What | Why |
|---|---|---|
| `src/main.rs` | `#![feature(asm_experimental_arch)]` | `asm!` and `global_asm!` on AVR aren't stable. |
| `Cargo.toml` | `panic = "abort"` in both profiles | AVR can't unwind. |
| Library code | No runtime `u32`/`u64` multiplication, and no `u16` division by a constant that isn't a power of two | There's no `__mulsi3` without libgcc. See above. |
| `xtask` | Its own ELF-to-HEX conversion | The `rust-objcopy` in the toolchain directory is rustc's private copy and won't run on its own. `llvm-tools` has a working `llvm-objcopy`, but that's one more thing to install for every flash, and `xtask` needs to parse the ELF to program it anyway. |

Up to nightly-2026-09-23 there was also a `--cfg no_fp_fmt_parse` in `.cargo/config.toml`, because `core`'s float formatting needed a bignum type that only existed for 32- and 64-bit pointers and wouldn't compile for AVR. `core` builds without it on nightly-2026-11-02, so it's gone. The firmware is the same size either way.

## Looking inside the ELF

For reading the generated code, rustup's `llvm-tools` component has an `llvm-objdump`, `llvm-nm` and `llvm-size` built from rustc's own LLVM, AVR backend included. It's optional, since nothing in the build or CI needs it, so it isn't in `rust-toolchain.toml`. Add it once to the pinned nightly:

```sh
rustup component add llvm-tools --toolchain nightly-2026-11-02
```

The binaries aren't on your `PATH`. They live in the toolchain's sysroot, under the host triple. Run this from the repo so rustup picks the pinned nightly:

```sh
LLVM="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
ELF=target/avr-none/release/wurkkos-ts21-0614.elf
```

Then, after `cargo fw`:

```sh
"$LLVM/llvm-objdump" -d --no-show-raw-insn "$ELF"                          # everything, vector table first
"$LLVM/llvm-objdump" -d -S --no-show-raw-insn --disassemble-symbols=main "$ELF"   # one function, with source
"$LLVM/llvm-nm" -S -C --size-sort "$ELF"                                   # what's using the flash
"$LLVM/llvm-objdump" -h "$ELF"                                             # section addresses
```

`-S` works because the release profile keeps debug info. I checked all four on the nightly-2026-09-23 build: `llvm-size -A` adds `.text` and `.rodata` up to the same 1250 bytes `cargo xtask size` reports, so that command stays the one to use for budgets. These are for when you want to know *why*, such as which function grew, or whether an interrupt handler saves more registers than it should.

## Host builds

`cargo test`, `cargo clippy` and `cargo fmt` run on your machine as normal. The library (`src/lib.rs` and below) is `no_std` and has no hardware access, so it builds for both. `src/main.rs` compiles to a stub on anything but AVR, and `src/hw.rs` and `src/rt.rs` only exist on AVR. That's why there's a separate `cargo fw-clippy`.

## First build

The first `cargo fw` downloads the pinned nightly with `rust-src`, and then the handful of crates that `core`'s build needs from crates.io. After that it's offline.
