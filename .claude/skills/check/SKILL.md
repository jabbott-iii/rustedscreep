---
name: check
description: Run the full validation gate for the Screeps Rust bot (fmt, clippy native and wasm, native tests, doc-sync, purity, release wasm build, size, TypeScript typecheck, bundle) and report each result honestly. Use after any code change and before closing a module or phase.
allowed-tools: Bash(cargo fmt *), Bash(cargo clippy *), Bash(cargo test *), Bash(cargo build *), Bash(cargo tree *), Bash(npm run build *), Bash(npm run check *), Bash(npm run size *), Bash(npx tsc *), Bash(git status *), Bash(git diff *), Bash(ls *), Bash(wc *)
---

# /check

Run these in order. Do not stop at the first failure, because the user wants the full picture. If a command doesn't exist yet
(before P1 scaffolding), report it as `n/a (not created yet)`, never as a pass.

| # | Check | Command |
|---|---|---|
| 1 | Format | `cargo fmt --all -- --check` |
| 2 | Clippy, native | `cargo clippy -p rustedscreep-core -p rustedscreep-consts --all-targets -- -D warnings` |
| 3 | Clippy, wasm | `cargo clippy -p rustedscreep-wasm --target wasm32-unknown-unknown -- -D warnings` |
| 4 | Native tests (includes the TUNING doc-sync test and the consts tests) | `cargo test -p rustedscreep-core -p rustedscreep-consts` |
| 5 | Purity: no JS crates in pure crates | `cargo tree -p rustedscreep-core -e normal` must not contain `wasm-bindgen`, `js-sys`, or `web-sys`. If ADR-005 allows `screeps-game-api` in `rustedscreep-consts`, check only `rustedscreep-core`'s direct dependencies. |
| 6 | TypeScript | `npx tsc --noEmit -p js` |
| 7 | Release build + bundle | `npm run build` |
| 8 | Sizes | Report the sizes of the `.wasm` and the JS bundle in `dist/` and the delta against the last value in STATUS.md |

If `npm run check` exists, it may run all of these at once. Still report each row separately.

## Report
A table: # | check | result (pass / FAIL / n/a) | key output (first relevant error lines, or the counts).
Then one line: "All checks passed" **only** if every row is a pass. Otherwise "N checks failed / M not run", followed by the root-cause
hypothesis for each failure. Never weaken a lint, a test, or `-D warnings` to make this pass.
