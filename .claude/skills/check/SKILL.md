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
| 3 | Clippy, wasm | `cargo clippy -p rustedscreep-wasm --target wasm32-unknown-unknown -- -D warnings`, then the same with `--all-features`, so the feature-gated `trap-test`, `act-test` and `profiler` code is linted too |
| 4 | Native tests (includes the TUNING doc-sync test and the consts tests) | `cargo test -p rustedscreep-core -p rustedscreep-consts` |
| 5 | Purity: no JS crates in pure crates | `cargo tree -p rustedscreep-core -e normal --prefix depth` must not contain `wasm-bindgen`, `js-sys`, or `web-sys` outside the `rustedscreep-consts` subtree, which ADR-005 allows to pull in `screeps-game-api` (guard-purity.mjs covers the facade's own boundary). |
| 6 | Tool tests | `npm run test` |
| 7 | Release build + bundle + smoke | `npm run build`; it ends by running `dist/main.js` in a Screeps-like sandbox for a few ticks (`js/tools/smoke.mjs`) |
| 8 | TypeScript | `npx tsc --noEmit -p js` (after the build, which generates the glue it type-checks against) |
| 9 | Sizes | `npm run size` prints the build ID, the `.wasm` and `main.js` sizes in `dist/`, and the upload estimate against the V76 budget. Report those plus the delta against the last values in STATUS.md, computed by hand. |
| 10 | Trap recovery (sandbox) | run by `npm run check` (`js/tools/trap-sim.mjs`): builds two trap-test variants and runs 10 forced traps through each ADR-003 recovery mode, then one trap after a `Memory` that did not parse at boot was reset to `{}`, which must halt (ADR-019) |
| 11 | Act layer (sandbox) | run by `npm run check` (`js/tools/act-sim.mjs`): builds the act-test variant and checks every intent call, its arguments, the return classification and `rustedscreep.act()` against the scripted producer |
| 12 | Profiler (sandbox) | run by `npm run check` (`js/tools/profiler-sim.mjs`): builds the profiler variant and runs it through six 1000-loop windows, checking the cold loop, the stats segment's writes and layout under the driver's save rules, four guarded write failures, and `rustedscreep.profiler()` |
| 13 | Persistence (sandbox) | run by `npm run check` (`js/tools/persist-sim.mjs`): the plain build's self-test round trips, corrupt segments, console overrides of the active set, the save cap and `rustedscreep.persist()` |
| 14 | Runtime config (sandbox) | run by `npm run check` (`js/tools/config-sim.mjs`): the plain build's overrides across four globals: refusals, clamps, the stored record, the log level, the naive structure toggle, a corrupt record, the console clear and the naive guard's two stops (ADR-022) |
| 15 | Constants self-check (sandbox) | run by `npm run check` (`js/tools/consts-sim.mjs`): the plain build against the engine fixture: the read path, drift, the line cap, no constants, a low bucket |
| 16 | Movement (sandbox) | run by `npm run check` (`js/tools/move-sim.mjs`): the plain build in a room whose wall gaps hold walkable and blocking ramparts, a wall and a site: terrain read once, scans at the interval, a console move order walked with engine-valid moves only and through the walkable gaps only, the order ending, a second one cancelled |
| 17 | Routes (sandbox) | run by `npm run check` (`js/tools/route-sim.mjs`): the plain build in four rooms with a PathFinder model: a console order from another room walked across two room edges on one search, the novice room blocked by the room callback, the bot's grid as the CostMatrix, a search that throws caught and cached |
| 18 | Spawn path (sandbox) | run by `npm run check` (`js/tools/spawn-sim.mjs`): the plain build in a room with two spawns: no call without a request or energy, the lower id first, the body as one part array and the encoded name, a refused call holding its spawn while the other takes the request, no clock read while no request can be served, a cleared request after OK, a dropped order's request standing again, the console's replies and the spawner summaries, each spawned creep's `Memory.creeps` entry deleted (ADR-018), a `Memory` that does not parse reset to `{}` with one notification (ADR-019) |
| 19 | Opening (sandbox) | run by `npm run check` (`js/tools/opening-sim.mjs`): the plain build from zero creeps in an RCL1 room with one spawn and one source, the harness playing the engine (spawning, harvest, transfer, upgrade, build, pickup, withdraw, moves and fatigue, the source's regeneration, the level-up, site creation): the first spawnCreep on the first loop with one worker pattern and the encoded name, worker calls only on the right targets and all OK, no `Memory.creeps` entry left, a recovery start and end and nothing else in the log, level 2, the extension sites of ADR-020 on legal tiles and the first one built, a pile and a tombstone emptied, `rustedscreep.opening()` matching the harness's counts |
| 20 | Links (sandbox) | run by `npm run check` (`js/tools/link-sim.mjs`): the plain build in an RCL5 room with a source link next to a forced mining tile and a controller link, a miner with one CARRY and an empty worker, the harness playing the engine (V216 to V220: transferEnergy's game-side checks at the tick's start, the link processor's clamp, loss and cooldown, creep intents in creepActions order): the snapshot's link reads (cooldown only while a link holds energy), the miner's transfer exactly when its store would overflow, the source link's sends exactly when the network allows with the right target and an explicit amount (none during the cooldown), the worker's withdraws, `rustedscreep.opening()`'s link fragments matching the harness's counts, no fault or refusal line |

`npm run check` runs all of these at once (`js/tools/check.mjs`). Still report each row separately.

## Report
A table: # | check | result (pass / FAIL / n/a) | key output (first relevant error lines, or the counts).
Then one line: "All checks passed" **only** if every row is a pass. Otherwise "N checks failed / M not run", followed by the root-cause
hypothesis for each failure. Never weaken a lint, a test, or `-D warnings` to make this pass.
