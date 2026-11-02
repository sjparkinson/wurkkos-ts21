---
name: check
description: Run every check CI runs, host and AVR, fix what fails, and report flash and RAM. Use before committing or pushing, or when asked to "run the checks".
---

Run these from the root of the checkout you're working in, in order. Stop at the first failure and fix it before moving on.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -q
cargo fw-clippy
cargo xtask size
cargo xtask hex
```

`cargo fw-clippy` and `cargo xtask size` rebuild `core` for AVR with `-Zbuild-std`, which needs crates.io on the first run. If the sandbox blocks the download, rerun those two outside it.

If `cargo fw-clippy` or `cargo xtask size` fails with an undefined `__mulsi3` (or any `__*si3` symbol), that's the no-32-bit-multiply rule in CLAUDE.md. Hand it to the `avr-codegen` agent if the cause isn't obvious.

Report the flash and RAM figures from `cargo xtask size`, and how they changed against `origin/main` if you know them. A jump of more than about 100 bytes deserves a sentence on why.
