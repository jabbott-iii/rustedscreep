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
| 5 | Purity: no JS crates in pure crates | `cargo tree -p rustedscreep-core -e normal --prefix depth` must not contain `wasm-bindgen`, `js-sys`, or `web-sys` outside the `rustedscreep-consts` subtree, which ADR-005 allows to pull in `screeps-game-api` (guard-purity.mjs covers the facade's own boundary). |
| 6 | Tool tests | `npm run test` |
| 7 | Release build + bundle + smoke | `npm run build`; it ends by running `dist/main.js` in a Screeps-like sandbox for a few ticks (`js/tools/smoke.mjs`) |
| 8 | TypeScript | `npx tsc --noEmit -p js` (after the build, which generates the glue it type-checks against) |
| 9 | Sizes | `npm run size` prints the build ID, the `.wasm` and `main.js` sizes in `dist/`, and the upload estimate against the V76 budget. Report those plus the delta against the last values in STATUS.md, computed by hand. |
| 10 | Trap recovery (sandbox) | run by `npm run check` (`js/tools/trap-sim.mjs`): builds two trap-test variants and runs 10 forced traps through each ADR-003 recovery mode |

`npm run check` runs all of these at once (`js/tools/check.mjs`). Still report each row separately.

## Report
A table: # | check | result (pass / FAIL / n/a) | key output (first relevant error lines, or the counts).
Then one line: "All checks passed" **only** if every row is a pass. Otherwise "N checks failed / M not run", followed by the root-cause
hypothesis for each failure. Never weaken a lint, a test, or `-D warnings` to make this pass.
