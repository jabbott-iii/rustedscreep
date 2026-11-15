# Screeps: World bot (Rust → WebAssembly)

Build a top-tier Screeps: World bot in Rust. It compiles to WebAssembly and loads through a thin TypeScript loader.
The goal is maximum colony performance (GCL/RCL growth, energy throughput, defense, expansion, offense)
at the lowest CPU per tick. **Never trade real performance for CPU savings. Cut wasted work instead.**

- Authoritative requirements: `docs/SPEC.md`. Do not edit it. Record deviations in `docs/DECISIONS.md`.
- Where we are: `docs/STATUS.md`, which is injected at session start and after compaction. The phase plan and exit gates are in `docs/ROADMAP.md` (MMO core track, P0–P12). Two optional tracks build on it and never block it: `docs/ROADMAP_PRIVATE.md` (PS#) and `docs/ROADMAP_SEASONAL.md` (SE#, after P12) (ADR-010).
- Multi-step procedures live in skills: `/kickoff`, `/verify`, `/module`, `/phase-gate`, `/check`,
  `/cpu-audit`, `/combat-verify`, `/deploy`, `/commit-msg`. Area-specific rules live in `.claude/rules/` and load by path.

## Working agreement (always applies)
- Work as a senior engineer and pair programmer. Explain significant decisions in a sentence or two and name risks and assumptions. Leave the user's decisions to the user.
- Make the smallest complete change that meets the request. No unrelated refactors, renames, formatting sweeps,
  dependency upgrades or new abstractions unless they are required or requested.
- Keep public APIs, persisted formats, config keys and behavior compatible unless the user authorizes a breaking change.
- Never claim that a command, test, build, scan or deploy succeeded unless its output appeared in this session.
  When it matters, label each statement as fact, assumption, recommendation or unknown.
- Never commit, amend, push, tag, merge, rebase, reset, or otherwise alter git history or publish changes.
  Leave all changes uncommitted for review. At checkpoints, offer a commit message (`/commit-msg`) with no
  Co-Authored-By line or other trailer.
- Never read, print, or hard-code credentials (`.screeps.yaml`, tokens, passwords, connection strings).
- Never weaken lints, tests, type checks, validation, or security controls to get a green build.
- If instructions conflict, do not silently choose the path that weakens safety, breaks compatibility,
  or cannot be undone. State the conflict and propose the safest compliant option.
- Deploy to a private server only when asked. Deploy to an official server (mmo, season, ptr) only when the user names
  that server in the current message. For the MMO, deploying means staging a checked build in `deploy/`; the user's push through
  the GitHub link uploads it (ADR-012). Never write to `deploy/` otherwise. Never write, copy or link anything into the Steam
  client's script mirror (`~/.config/Screeps/scripts/`): any change there uploads at once (VERIFICATION_LOG V74).

## Source-of-truth rule (do not trust memory)
Before writing code that depends on a game rule, an API signature, or tool behavior, look it up in this order:
1. `screeps-game-api` docs and source for the **pinned** version (docs.rs, github.com/rustyscreeps)
2. docs.screeps.com (API reference, game mechanics pages, constants)
3. Screeps engine and common source (github.com/screeps/engine, github.com/screeps/common) for exact behavior
4. Official changelog and release notes
5. Community sources (wiki.screepspl.us, open-source bots): hints only, cross-checked against 1 to 3

Record every finding in `docs/VERIFICATION_LOG.md` with its source, value, version, date, and confidence (verified, inferred, or unverified).
- Code reads constants through `crates/rustedscreep-consts`.
- A value with no API constant goes in `crates/rustedscreep-consts/src/extra.rs`, with its source URL and a "last verified" date.
- If a source is unreachable, mark the item `VERIFY`, state the assumption, and choose the safest option.
- Never invent a function, field, constant, or crate feature.
- If a finding contradicts `docs/SPEC.md`, trust the source. Flag the discrepancy in the log and in your reply, then continue.

The live server's constants are the ground truth for that server. At init, the bot checks `rustedscreep-consts` against the server's JS globals (P1).

## Architecture invariants (change only through an ADR)
- `crates/rustedscreep-consts` is the constants facade. Besides `rustedscreep-wasm`, it is the only crate that may name `screeps::`, and only for constants and pure local types.
- `crates/rustedscreep-core` holds all decision logic in pure, deterministic Rust that is testable natively. It has no JS dependencies. It covers the scheduler, planner, economy, spawning, logistics, defense, offense (combat sim, formations, breaker solver), expansion, movement (flow fields, BFS, A*), market policy, profiling math, and util. Criterion benches live here.
- `crates/rustedscreep-wasm` is the `cdylib`. It handles the snapshot (JS → Rust), the act step (Rust → intents), persistence (RawMemory), the profiler, and the exported `loop`. It is the only crate that uses `wasm-bindgen`, `js-sys`, or game-object APIs.
- `js/` is the TypeScript loader and tools only. It handles bucket-gated compile/instantiate, calling `loop`, trap/panic recovery, and the deploy, console, and stats scripts. It contains no game logic.
- Each tick runs **snapshot → decide → act**:
  - The snapshot makes one pass over game objects into Rust-native SoA structs with numeric handles.
  - Decisions are pure Rust.
  - Intents are collected, de-duplicated, and checked against the engine's intent pipeline, then issued in one batch.
  - Skip no-op intents, because every state-changing intent has a fixed CPU cost (verify the value).
- CPU tiers:
  - **Critical** (spawning, defense, harvesting) always runs.
  - **Standard** (hauling, upgrading, building, offense ops) runs unless the bucket is critical.
  - **Optional** (planning, market, scouting, visuals, pixels, offense planning) runs only above bucket thresholds.
  - Heavy work is time-sliced.
- Every threshold lives in `crates/rustedscreep-core/src/config.rs` and is mirrored in `docs/TUNING.md`. A test keeps the two in sync.
- Error isolation:
  - `panic = "abort"` on wasm rules out `catch_unwind`, so isolation uses `Result` per room, creep, and task. A failing unit is logged with a rate limit and skipped.
  - Non-test code in `rustedscreep-core` and `rustedscreep-wasm` has no `unwrap`, `expect`, `panic!`, or unchecked indexing. Clippy enforces this.
  - Only the JS loader recovers from a trap.
- Persistence:
  - Rust statics are lost on a global reset and must rebuild cheaply.
  - Persist only what must survive a reset. Use a versioned compact binary format in RawMemory segments, written only when dirty.
  - Keep the tick path off `Memory`: never read it for state. The only writes undo `spawnCreep`'s `Memory.creeps[name]` entry and reset a `Memory` that does not parse (ADR-018, ADR-019).
  - Missing or corrupt data triggers a rebuild, never a crash.

## Commands (P1 created most of these; the API tools come in P2, ADR-017; update this table if they change)
| Purpose | Command |
|---|---|
| Native tests (pure logic) | `cargo test -p rustedscreep-core -p rustedscreep-consts` |
| Lints, native | `cargo clippy -p rustedscreep-core -p rustedscreep-consts --all-targets -- -D warnings` |
| Lints, wasm (`npm run check` also runs it with `--all-features`) | `cargo clippy -p rustedscreep-wasm --target wasm32-unknown-unknown -- -D warnings` |
| Benches (native only) | `cargo bench -p rustedscreep-core` |
| Install the pinned wasm-opt (binaryen version_133) into `.tools/` | `npm run setup:binaryen` |
| Release build (wasm + wasm-opt + Rollup to `dist/`); a trap-test or profiler variant | `npm run build` / `npm run build -- --features trap-test` (or `profiler`) |
| Trap recovery in the sandbox (10 traps, both ADR-003 modes; one trap after a repaired `Memory`) | `node js/tools/trap-sim.mjs` |
| Act layer in the sandbox (the `act-test` build's scripted intents) | `node js/tools/act-sim.mjs` |
| Profiler in the sandbox (the `profiler` build over six windows, stats segment and write failures) | `node js/tools/profiler-sim.mjs` |
| Persistence in the sandbox (the plain build's self-test: round trips, corrupt segments, console overrides, the save cap) | `node js/tools/persist-sim.mjs` |
| Runtime config in the sandbox (the plain build's overrides across four globals: refusals, clamps, the record, log level, naive toggle) | `node js/tools/config-sim.mjs` |
| Constants self-check in the sandbox (the plain build against the engine fixture: read path, drift, the line cap, no constants, low bucket) | `node js/tools/consts-sim.mjs` |
| Movement in the sandbox (the plain build: terrain and scan cadence, a console move order with engine-checked moves) | `node js/tools/move-sim.mjs` |
| Inter-room routes in the sandbox (the plain build with a PathFinder model: a cross-room order, room callback answers, a search that throws) | `node js/tools/route-sim.mjs` |
| Spawn path in the sandbox (the plain build with two spawns: a console request, energy waits, a refused call's hold, the body and name passed, a dropped order re-armed, the `Memory` entry delete and reset of ADR-018 and ADR-019) | `node js/tools/spawn-sim.mjs` |
| Regenerate the engine-constants fixture from an engine's `lib/constants.js` | `node js/tools/consts-fixture.mjs <path> <version>` |
| Tool tests (Node) / sizes and upload budget | `npm run test` / `npm run size` |
| Full gate (same as `/check`) | `npm run check` |
| Stage the MMO deploy folder for the GitHub link (ask first; the user pushes) | `npm run stage` (`-- --out <dir>` for a dry run, `-- --features trap-test` for the gate's recovery test, `-- --features profiler` for the live CPU measurements) |
| Deploy through the API (ask first; private, season, or the ADR-012 fallback; not written yet: P2, ADR-017; needs Node 22/24) | `npm run deploy -- --server <name> [--dryrun]` |
| Capture console / profiler stats to `logs/` (not written yet: P2, ADR-017; needs Node 22/24) | `npm run console -- --server <name>` / `npm run stats -- --server <name>` |

## Workflow
- Start with `/kickoff` (P0). Then run `/module` repeatedly. With no argument it takes the next item in STATUS and ROADMAP. Close each phase with `/phase-gate <P#|PS#|SE#>`.
- Before each module, state the CPU and performance trade-offs, the cache strategy and its invalidation, and the tier, in a few lines.
  After each module, summarize what is done, what is stubbed, and what to test next, then update STATUS.md.
- A phase is done only when its exit gate passes with evidence (command output, measured numbers) recorded in STATUS.md.
- Delegate lookups to the `screeps-verifier` subagent, running in parallel for independent items, so raw pages stay out of this context.
  Run `hot-path-reviewer` before closing any module that touches the tick path, and `combat-math-auditor` for defense or offense math.
  Use `wasm-build-doctor` for build, load, or instantiate failures.
- Measure before optimizing. Use the profiler (`profiler` feature), native benches and live MMO runs (or private-server runs on the optional track), not intuition.
- Definition of done for any change: `/check` passes, new logic has tests whose expectations derive from constants,
  formulas cite the constants they use, `docs/` is updated where the change touches it, and STATUS.md is updated.

## Known traps (seeded 2026-11-13; re-verify in P0, details in `docs/VERIFICATION_LOG.md`)
- `screeps-game-api` 0.22 and later is not compatible with `cargo-screeps`. Build with wasm-pack or the wasm-bindgen CLI, then deploy with a Node script.
- Rust 1.82 and later emit `reference-types` and `multivalue` by default on wasm32. Rust 1.87 and later add `bulk-memory`. An old server Node may reject these.
  The MMO (shard3) accepts all of them (P0 probe, 2026-11-13, VERIFICATION_LOG V72), so ADR-002 keeps the defaults. Re-probe any other server first (PS1, SE1).
- A wasm trap can leave wasm-bindgen glue state poisoned. The starter loader calls `Game.cpu.halt()` on the tick after a `loop` that did not finish.
  Choosing between that and a cheaper re-instantiate is ADR-003 and must be measured.
- Engine intent priorities: `heal` is never blocked, and it blocks `dismantle`, `attack`, `rangedHeal`, and others, even when the target is at full HP.
  A breaker cannot heal and dismantle in the same tick. Design every combat role around the verified table.
- Cargo profiles only apply from the workspace root `Cargo.toml`. `[package.metadata.wasm-pack...]` belongs in `rustedscreep-wasm`.
- Do not name a crate `core`, because it shadows `::core`. Keep `rustedscreep-wasm` out of `default-members` so native `cargo test` does not try to build it.
