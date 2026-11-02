---
name: bump-nightly
description: Move rust-toolchain.toml to a newer nightly and prove the AVR build still works.
argument-hint: "[YYYY-MM-DD]"
disable-model-invocation: true
---

AVR is a tier 3 target, so nightlies break it now and then. Bump deliberately:

1. Pick the date: `$ARGUMENTS` if given, otherwise a nightly at least a week old.
2. Run `cargo xtask size` on the current nightly and note flash and RAM.
3. Change `channel` in `rust-toolchain.toml` to `nightly-<date>`, and update the pinned date in docs/toolchain.md (the `rustup component add llvm-tools` line and anything else that names the old one).
4. Run the `check` skill. Compare `cargo xtask size` before and after; a big jump in flash is worth a sentence in the PR.
5. Look at the workarounds in `.cargo/config.toml` (`--cfg no_fp_fmt_parse`) and `src/main.rs` (`asm_experimental_arch`). If the new nightly no longer needs one, remove it and say so in docs/toolchain.md.
6. If the build breaks, find the smallest failing piece in `$TMPDIR`, note the rustc version (`rustc -vV`), and report back rather than piling on workarounds.

Commit as `build: bump nightly to <date>`.
