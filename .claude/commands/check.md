---
description: Run every check CI runs, host and AVR, and fix what fails
---

Run these from the repo root, in order. Stop at the first failure and fix it before moving on.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -q
cargo fw-clippy
cargo xtask size
cargo xtask hex
```

`cargo fw-clippy` and `cargo xtask size` rebuild `core` for AVR with `-Zbuild-std`, which needs crates.io on the first run. If the sandbox blocks the download, rerun those two outside it.

Report the flash and RAM figures from `cargo xtask size`, and how they changed if you know the previous numbers.
