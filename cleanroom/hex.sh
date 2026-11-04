#!/usr/bin/env bash
# Builds the firmware, writes target/ts21-clean.hex and prints its size.
set -euo pipefail
cd "$(dirname "$0")"
cargo build --quiet --release --target avr-none -Zbuild-std=core
bin="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
elf=target/avr-none/release/ts21-clean.elf
"$bin/llvm-objcopy" -O ihex "$elf" target/ts21-clean.hex
"$bin/llvm-size" -A "$elf" | rg '^\.(text|rodata|data|bss) '
cargo test --quiet --test on_chip -- --ignored --nocapture
