---
name: avr-codegen
description: Builds the firmware and reads the generated AVR code for things host tests can't see, such as __mulsi3 link failures, core::fmt or float code sneaking in, flash or RAM growth against main, and fat interrupt handlers. Use when `cargo xtask hex` fails to link, or to answer "why did flash grow?".
---

Audit what the ATtiny1616 actually runs. The host tests compile the library for the host's target, so they can't see AVR codegen problems.

Build with `cargo xtask hex`. If only its layout or simulator checks fail, the ELF is still there to look at. If the sandbox blocks the first `-Zbuild-std` build from fetching crates, rerun it outside the sandbox.

Then use the `llvm-tools` binaries as docs/toolchain.md ("Looking inside the ELF") describes. They aren't on `PATH`:

```sh
LLVM="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
ELF=target/avr-none/release/wurkkos-ts21-0614.elf
```

`rust-toolchain.toml` pins `llvm-tools`, so rustup installs them with the toolchain. If they're still missing, say so and stop rather than working around it.

Check, in this order, and stop early if the question's already answered:

1. **Link failures.** `undefined symbol: __mulsi3` (or `__udivmodsi4`, `__mulhi3` and friends) means a 32-bit multiply. Find the source: `u32`/`u64` multiplication, or a `u16` divided or modded by a constant that isn't a power of two. Point at the line and suggest a fix from docs/toolchain.md (a `const fn` table, shifts, a runtime divisor, or 16-bit maths).
2. **Code that shouldn't be there.** `llvm-nm -S -C --size-sort` for symbols from `core::fmt`, float routines, panic formatting, or anything with `fmt` or `f32`/`f64` in the name.
3. **Size against main.** If the branch differs from `origin/main`, build main in a throwaway `git worktree add "$TMPDIR/base" origin/main` (remove it after), run `cargo xtask hex` there, and report flash and RAM before and after, plus the symbols that grew most (`llvm-nm -S --size-sort` on both).
4. **Interrupt handlers.** Disassemble each vector's handler and say whether its register save and restore looks larger than the body needs.

Keep raw disassembly out of the conversation: filter it with `rg` or `head` down to the few lines that matter. Never run `cargo xtask ping`, `backup`, `flash` or `restore`: they drive real hardware. Report findings with the symbol or source line, the evidence (a disassembly excerpt of a few lines, or the size numbers), and a suggested fix. Keep it short when everything's fine.
