---
name: fix-review
description: Fix the findings from a code review with a small team of agents split by area, run every check, and open a PR. Use after /code-review when asked to "fix these" or "work on the fixes".
argument-hint: "[number of agents]"
disable-model-invocation: true
---

Take the findings from the review in this conversation (or the PR comments, if pointed at a PR) and fix them all.

1. **Group the findings by the files they touch**, so no two agents edit the same file. The split that has worked here:
   - library: `src/` (not `hw.rs` or `rt.rs` unless the finding is there)
   - simulator: `xtask/src/sim.rs`, `avr.rs`, `world.rs`, `simulate.rs`
   - firmware glue: `src/hw.rs`, `src/rt.rs`, `src/regs.rs`, `link.x`
   - programmer and tooling: `xtask/src/updi.rs`, `serial.rs`, `ihex.rs`, `elf.rs`
   Use `$ARGUMENTS` agents if given, otherwise one per group that has findings, at most three.
2. **Start each agent in its own worktree** (`isolation: "worktree"`). Agents sharing a checkout trip over each other's half-finished edits and fight over `target/`. Brief each with its findings verbatim, the files it owns, and these rules:
   - edit `.rs` files with Edit or Write, never `sed -i` or `cp`, so the lint hook runs;
   - never `git stash`; to compare before and after, use a WIP commit or a second worktree;
   - add a test that fails without the fix;
   - run the `check` skill before reporting, and report flash and RAM before and after;
   - use the `register-checker` agent after touching register code, `sim-checker` after touching the simulator, and `ui-spec-checker` after touching the UI or docs/ui.md.
3. **Bring the work together** on one branch off `origin/main` (cherry-pick each agent's commits), then run the `check` skill on the combined tree. Fix anything that only breaks when combined.
4. **Commit** one single-line Conventional Commit per area, push with `git push origin <branch>` (the sandbox can't record an upstream, so skip `-u`), and open a ready PR against `main` with `gh pr create --head <branch>`. The PR body lists each finding and how it was fixed. PRs merge by rebase, so every commit lands on main as written.

Don't fix things the review didn't raise. If a finding turns out to be wrong, say so in the PR body instead of changing code to satisfy it.
