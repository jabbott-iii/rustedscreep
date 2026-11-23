# Screeps: World bot (Rust → WebAssembly)

Build a top-tier Screeps: World bot in Rust. It compiles to WebAssembly (wasm) and loads through a thin TypeScript loader.
The goal is maximum colony performance at the lowest CPU per tick (one game turn). Performance means the growth of the
global control level (GCL) and room controller level (RCL), energy throughput, defense, expansion, and offense.
**Never trade real performance for CPU savings. Cut wasted work instead.**

- Authoritative requirements: `docs/SPEC.md`. Do not edit it. Record deviations in `docs/DECISIONS.md`, the architecture decision records (ADRs).
- Where we are: `docs/STATUS.md`, which is injected at session start and after compaction (when Claude Code summarizes the conversation). The phase plan and exit gates (the checks, with evidence, that close a phase) are in `docs/ROADMAP.md` (the core track for the MMO, the official persistent server: phases P0–P12). Two optional tracks build on it and never block it: `docs/ROADMAP_PRIVATE.md` (private-server phases PS#) and `docs/ROADMAP_SEASONAL.md` (Seasonal-server phases SE#, after P12, the last core phase) (ADR-010, which split the roadmap into these three tracks).
- Multi-step procedures live in skills: `/kickoff`, `/verify`, `/module`, `/phase-gate`, `/check`,
  `/cpu-audit`, `/combat-verify`, `/deploy`, `/commit-msg`. Area-specific rules live in `.claude/rules/` and load by path.

## Working agreement (always applies)
- Work as a senior engineer and pair programmer. Explain significant decisions in a sentence or two and name risks and assumptions. Leave the user's decisions to the user.
- Make the smallest complete change that meets the request. No unrelated refactors, renames, formatting sweeps,
  dependency upgrades, or new abstractions unless they are required or requested.
- Keep public APIs, persisted formats, config keys, and behavior compatible unless the user authorizes a breaking change.
- Never claim that a command, test, build, scan, or deploy succeeded unless its output appeared in this session.
  When it matters, label each statement as fact, assumption, recommendation, or unknown.
- Never commit, amend, push, tag, merge, rebase, reset, or otherwise alter git history or publish changes.
  Leave all changes uncommitted for review. At checkpoints, offer a commit message (`/commit-msg`) with no
  Co-Authored-By line or other trailer.
- Never read, print, or hard-code credentials (`.screeps.yaml`, tokens, passwords, connection strings).
- Never weaken lints, tests, type checks, validation, or security controls to get a green build.
- If instructions conflict, do not silently choose the path that weakens safety, breaks compatibility,
  or cannot be undone. State the conflict and propose the safest compliant option.
- Deploy to a private server only when asked. Deploy to an official server (mmo, season, or ptr, the public test realm) only
  when the user names that server in the current message. For the MMO, deploying means staging a checked build in `deploy/`;
  the user's push through the GitHub link (the Screeps account's GitHub integration) uploads it (ADR-012, which made that link
  the MMO deploy path). Never write to `deploy/` otherwise. Never write, copy, or link anything into the Steam
  client's script mirror (`~/.config/Screeps/scripts/`): any change there uploads at once (VERIFICATION_LOG V74, the client's
  two-way folder sync).

## Source-of-truth rule (do not trust memory)
Before writing code that depends on a game rule, an API signature, or tool behavior, look it up in this order:
1. `screeps-game-api` (the Rust bindings to the game's API) docs and source for the **pinned** version (docs.rs, github.com/rustyscreeps)
2. docs.screeps.com (API reference, game mechanics pages, and constants)
3. Screeps engine and common source (github.com/screeps/engine, github.com/screeps/common) for exact behavior
4. Official changelog and release notes
5. Community sources (wiki.screepspl.us, open-source bots): hints only, cross-checked against 1 to 3

Record every finding in `docs/VERIFICATION_LOG.md` with its source, value, version, date, and confidence (verified, inferred, or unverified).
- Code reads constants through `crates/rustedscreep-consts`.
- A value with no API constant goes in `crates/rustedscreep-consts/src/extra.rs`, with its source URL and a "last verified" date.
- If a source is unreachable, mark the item `VERIFY`, state the assumption, and choose the safest option.
- Never invent a function, field, constant, or crate feature.
- If a finding contradicts `docs/SPEC.md`, trust the source. Flag the discrepancy in the log and in your reply, then continue.

The live server's constants are the ground truth for that server. At init, the bot checks `rustedscreep-consts` against the server's JS globals (P1, the phase that built the loader and this self-check).

## Architecture invariants (change only through an ADR)
- `crates/rustedscreep-consts` is the constants facade. Besides `rustedscreep-wasm`, it is the only crate that may name `screeps::`, and only for constants and pure local types.
- `crates/rustedscreep-core` holds all decision logic in pure, deterministic Rust that is testable natively. It has no JS dependencies. It covers the scheduler, planner, economy, spawning, logistics, defense, offense (combat sim, squad formations, and the breaker solver, which builds the body of the siege breaker, a creep that dismantles structures), expansion, movement (flow fields, which hold every tile's cost to a goal for all creeps heading there; breadth-first search; A*), market policy, profiling math, and util. Criterion benches live here.
- `crates/rustedscreep-wasm` is the `cdylib` (the crate built into the wasm module). It handles the snapshot (JS → Rust, one pass over the game state each tick), the act step (Rust → intents: commands, such as a move, that the engine applies once the tick's code has run), persistence (RawMemory, the game's raw string storage), the profiler, and the exported `loop`. It is the only crate that uses `wasm-bindgen`, `js-sys`, or game-object APIs.
- `js/` is the TypeScript loader and tools only. It handles bucket-gated compile/instantiate (the bucket is the CPU the game banks from earlier ticks' unused share), calling `loop`, trap/panic recovery (a trap is a wasm runtime fault that aborts the call), and the build, stage, size, check, and sandbox tools (the sandbox tools run a build in Node against a model of the game; there are no API tools, ADR-021 dropped them). It contains no game logic.
- Each tick runs **snapshot → decide → act**:
  - The snapshot makes one pass over game objects into Rust-native struct-of-arrays (SoA) structs with numeric handles.
  - Decisions are pure Rust.
  - Intents are collected, de-duplicated, and checked against the engine's intent pipeline (its rules for which intents run and which block others), then issued in one batch.
  - Skip no-op intents, because every state-changing intent has a fixed CPU cost (verify the value).
- CPU tiers (what runs at each bucket level):
  - **Critical** (spawning, defense, and harvesting) always runs.
  - **Standard** (hauling, upgrading, building, and offense operations) runs unless the bucket is critical.
  - **Optional** (planning, market, scouting, visuals, pixel generation from a full bucket, and offense planning) runs only above bucket thresholds.
  - Heavy work is time-sliced (spread over several ticks).
- Every threshold lives in `crates/rustedscreep-core/src/config.rs` and is mirrored in `docs/TUNING.md`. A test keeps the two in sync.
- Error isolation:
  - `panic = "abort"` on wasm rules out `catch_unwind`, so isolation uses `Result` per room, creep (one of the player's units), and task. A failing unit is logged with a rate limit and skipped.
  - Non-test code in `rustedscreep-core` and `rustedscreep-wasm` has no `unwrap`, `expect`, `panic!`, or unchecked indexing. Clippy enforces this.
  - Only the JS loader recovers from a trap.
- Persistence:
  - Rust statics are lost on a global reset (the game discards the JS heap and reloads the code) and must rebuild cheaply.
  - Persist only what must survive a reset. Use a versioned compact binary format in RawMemory segments (numbered strings the game keeps between ticks), written only when dirty.
  - Keep the tick path off `Memory` (the game's JSON store, parsed on first use in a tick): never read it for state. The only writes undo `spawnCreep`'s `Memory.creeps[name]` entry and reset a `Memory` that does not parse (ADR-018 and ADR-019 decided these two writes).
  - Missing or corrupt data triggers a rebuild, never a crash.

## Commands (P1 to P4 created these; there are no API tools, ADR-021; update this table if they change)
| Purpose | Command |
|---|---|
| Native tests (pure logic) | `cargo test -p rustedscreep-core -p rustedscreep-consts` |
| Lints, native | `cargo clippy -p rustedscreep-core -p rustedscreep-consts --all-targets -- -D warnings` |
| Lints, wasm (`npm run check` also runs it with `--all-features`) | `cargo clippy -p rustedscreep-wasm --target wasm32-unknown-unknown -- -D warnings` |
| Benches (native only) | `cargo bench -p rustedscreep-core` |
| Install the pinned wasm-opt (binaryen version_133) into `.tools/` | `npm run setup:binaryen` |
| Release build (wasm + wasm-opt + Rollup to `dist/`); a trap-test or profiler variant | `npm run build` / `npm run build -- --features trap-test` (or `profiler`) |
| Trap recovery in the sandbox (10 traps, both ADR-003 recovery modes: halt and re-instantiate; one trap after a repaired `Memory`) | `node js/tools/trap-sim.mjs` |
| Act layer in the sandbox (the `act-test` build's scripted intents) | `node js/tools/act-sim.mjs` |
| Profiler in the sandbox (the `profiler` build over six windows, stats segment, and write failures) | `node js/tools/profiler-sim.mjs` |
| Persistence in the sandbox (the plain build's self-test: round trips, corrupt segments, console overrides, the save cap) | `node js/tools/persist-sim.mjs` |
| Runtime config in the sandbox (the plain build's overrides across four globals: refusals, clamps, the record, log level, naive toggle, the naive guard's two stops) | `node js/tools/config-sim.mjs` |
| Constants self-check in the sandbox (the plain build against the engine fixture: read path, drift, the line cap, no constants, low bucket) | `node js/tools/consts-sim.mjs` |
| Movement in the sandbox (the plain build: terrain and scan cadence, the owned room's road read every `upkeep.read_ticks` (P7.3), a console move order with engine-checked moves) | `node js/tools/move-sim.mjs` |
| Inter-room routes in the sandbox (the plain build with a PathFinder model: a cross-room order, room callback answers, a search that throws) | `node js/tools/route-sim.mjs` |
| Spawn path in the sandbox (the plain build with two spawns: a console request, energy waits, a refused call's hold, the body and name passed, a dropped order re-armed, the `Memory` entry delete and reset of ADR-018 and ADR-019) | `node js/tools/spawn-sim.mjs` |
| Opening in the sandbox (the plain build from zero creeps at RCL1 to level 2 and a first extension: the recovery and its log lines, the worker calls and their targets, the `Memory` entries, the extension sites ADR-020 places before the base planner exists, a dropped energy pile and a dead creep's tombstone, `rustedscreep.opening()`) | `node js/tools/opening-sim.mjs` |
| Links (structures that send energy within a room) in the sandbox (the plain build at RCL5 with a source link and a controller link: the snapshot's link reads, the miner's transfers, the sends with their targets, amounts, and cooldowns, the worker's withdraws, the summary's link fragments, a worn container under repair that no worker walks to, from P3.5's minimal builders and repairers) | `node js/tools/link-sim.mjs` |
| Base planner in the sandbox (the plain build in E7N57 as it stands at RCL4: the plan in the Optional tier, road sites only while storage, towers and ramparts are off, then with `planner.storage` and `planner.towers` on the destroy of the ADR-020 extension on each one's tile and its site, and with `planner.ramparts` on the line's sites (P7.3, ADR-032), the CPU per planner step; then two more globals: ADR-027's record of each room's plan anchor and adopted role tiles loaded and used, a corrupted one refused; P4.3 and ADR-026: placing the plan's sites and destroying off-plan structures in their way) | `node js/tools/planner-sim.mjs` |
| Regenerate the engine-constants fixture from an engine's `lib/constants.js` | `node js/tools/consts-fixture.mjs <path> <version>` |
| Write the terrain corpus fixtures from saved room-terrain and room-objects responses (local files only; the fixtures' README lists the requests) | `node js/tools/rooms-fixture.mjs <dir> <shard> <YYYY-MM-DD>` |
| Remote mining in the sandbox (the plain build with an owned home and a neutral neighbor its creeps name: the snapshot's remote-room reads (the controller's reservation, the bot's username once a global, hostile structures, the roads every `remote.road_read_ticks`, and the home's roads every `upkeep.read_ticks`, P7.3), the scout's `suicide` once the room is searched, the reserver's `reserveController` calls up to `CONTROLLER_RESERVE_MAX`, a container site in the remote room, `rustedscreep.remote()`; P6.2, ADR-029; P6.3: an unarmed hostile ignored, an armed one's `getActiveBodyparts` reads, the threat's log line and the evacuation home, the wait's end and the return) | `node js/tools/remote-sim.mjs` |
| Controller signer in the sandbox (the plain build with an owned RCL3 home and three neighbors, one with the server's sign and one of another status: the record's segment requested only once the config overrides load, the one signer spawned with one MOVE part, `signController` with the exact text next to each signable controller once, the `sign` and tower reads only in the signer's room, no route search into the closed room, the signer's `suicide` once nothing is left in reach, `rustedscreep.sign()`, the record loaded after a reset with no new signer; ADR-030) | `node js/tools/sign-sim.mjs` |
| Threat assessment in the sandbox (the plain build in the smoke world with scripted hostiles in the owned room: `hits` each loop, the body once and again only after another player's lab room, an unknown boost string read as none, the start, change, and end lines with figures from the constants, owners read only on those loops; P7.1; and a tower's focus fire, its fire lines and its heals after the fight, P7.2) | `node js/tools/defense-sim.mjs` |
| Split wasm in the sandbox (the plain build in three text parts: the boot from them, a stale part never required, each part empty, one missing, two cut, one padded; ADR-028: the wasm's base64 split into modules of at most 905709 bytes) | `node js/tools/split-sim.mjs` |
| Tool tests (Node) / sizes, upload budget, and the per-module cap (ADR-028) | `npm run test` / `npm run size` |
| Full gate (same as `/check`) | `npm run check` |
| Stage the MMO deploy folder for the GitHub link (ask first; the user pushes) | `npm run stage` (`-- --out <dir>` for a dry run, `-- --features trap-test` for the gate's recovery test, `-- --features profiler` for the live CPU measurements) |

## Workflow
- Start with `/kickoff` (P0, the kickoff and verification phase). Then run `/module` repeatedly. With no argument it takes the next item in STATUS and ROADMAP. Close each phase with `/phase-gate <P#|PS#|SE#>`.
- Before each module, state the CPU and performance trade-offs, the cache strategy and its invalidation, and the tier, in a few lines.
  After each module, summarize what is done, what is stubbed, and what to test next, then update STATUS.md.
- A phase is done only when its exit gate passes with evidence (command output, measured numbers) recorded in STATUS.md.
- Delegate lookups to the `screeps-verifier` subagent, running in parallel for independent items, so raw pages stay out of this context.
  Run `hot-path-reviewer` before closing any module that touches the tick path, and `combat-math-auditor` for defense or offense math.
  Use `wasm-build-doctor` for build, load, or instantiate failures.
- Measure before optimizing. Use the profiler (`profiler` feature), native benches, and live MMO runs (or private-server runs on the optional track), not intuition.
- Definition of done for any change: `/check` passes, new logic has tests whose expectations derive from constants,
  formulas cite the constants they use, `docs/` is updated where the change touches it, and STATUS.md is updated.

## Known traps (seeded 2026-11-13; re-verify in P0, details in `docs/VERIFICATION_LOG.md`)
- `screeps-game-api` 0.22 and later is not compatible with `cargo-screeps`. Build with wasm-pack or the wasm-bindgen CLI, then deploy with a Node script.
- Rust 1.82 and later emit `reference-types` and `multivalue` by default on wasm32. Rust 1.87 and later add `bulk-memory`. An old server Node may reject these.
  The MMO (shard3, one of its separate worlds, called shards) accepts all of them (P0 probe, 2026-11-13, VERIFICATION_LOG V72:
  all 10 probed wasm features load), so ADR-002 (the pinned stable Rust toolchain and its wasm feature target) keeps the defaults.
  Re-probe any other server first (PS1, SE1: the private and Seasonal tracks' first phases, which probe their servers).
- A wasm trap can leave wasm-bindgen glue state (in the generated JS bindings) poisoned. The starter loader (from screeps-starter-rust) calls `Game.cpu.halt()` (which forces a global reset) on the tick after a `loop` that did not finish.
  ADR-003 (trap recovery) chose a hybrid: this halt is the default, measured on the MMO over 10 traps in a row (V148),
  and re-instantiating (2 ticks per trap instead of 3, with no halt, in the sandbox, V99) stays behind a config flag, off by
  default, until its own 10-trap MMO run shows it correct and cheaper than a halt plus recompile.
- Engine intent priorities: `heal` is never blocked, and it blocks `dismantle`, `attack`, `rangedHeal`, and others, even when the target is at full HP.
  A breaker cannot heal and dismantle in the same tick. Design every combat role around the verified table.
- Cargo profiles only apply from the workspace root `Cargo.toml`. `[package.metadata.wasm-pack...]` belongs in `rustedscreep-wasm`.
- Do not name a crate `core`, because it shadows `::core`. Keep `rustedscreep-wasm` out of `default-members` so native `cargo test` does not try to build it.
