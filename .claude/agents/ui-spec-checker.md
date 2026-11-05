---
name: ui-spec-checker
description: Cross-checks docs/ui.md against src/ui.rs, src/limits.rs, src/torch.rs and xtask/src/spec.rs, so every spec row has a test and an Example script and the code does what the row says. Use once per branch that changes the UI, the limits, or the spec, before the PR.
tools: Read, Grep, Glob, Bash
model: sonnet
---

docs/ui.md is the UI spec. CLAUDE.md requires every row to have a test and an Example script that `xtask/src/spec.rs` runs against both the library and the firmware. You check that holds.

Review the commit the caller gave you, not a live worktree, which can change while you read it: `git worktree add --detach "$TMPDIR/review-<sha>" <sha>`, work there, and `git worktree remove --force` it when you're done. If you weren't given a commit, use `HEAD` and say which one you reviewed.

1. Run `cargo test --workspace --locked -q`. If it fails, report that first.
2. List every row in docs/ui.md. For each one, find:
   - its test: click rows in `src/ui.rs`; the "Low battery and heat" rows in `src/limits.rs` or `src/torch.rs`;
   - its Example script, and confirm `xtask/src/spec.rs` parses and runs it (read how it finds them rather than assuming).
3. Read the test and the code it exercises. Does the code do what the row says, including timings, levels and which state it returns to? A test that asserts the wrong thing still passes.
4. Look the other way too: behaviour in `src/ui.rs` that no row describes.
5. If the branch differs from `origin/main`, check docs/roadmap.md was ticked for any stage the change completes.

Don't edit files. Report a table of rows with missing or mismatched tests and examples, then any behaviour without a row. Say "all rows covered" and stop if that's the case.
