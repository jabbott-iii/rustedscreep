# Roadmap and exit gates: MMO core track

There are three roadmaps (user decision, 2026-11-13; ADR-010):
- **MMO core track (this file, P0–P12):** the complete Screeps: World bot for the official MMO. It never depends on the other two.
- **Private-server track (`docs/ROADMAP_PRIVATE.md`, PS1–PS4):** optional. It builds on the core and adds a private test server,
  baseline comparisons and scripted scenarios.
- **Seasonal track (`docs/ROADMAP_SEASONAL.md`, SE1–SE2):** optional. It builds on the finished core and adapts the bot to a Seasonal server.

The feature order follows SPEC §3. Each phase lists its modules, which become STATUS checklist items, and an **exit gate**.
A gate passes only with evidence recorded in STATUS.md (`/phase-gate P#`). Every module follows the `/module` protocol.
No core gate needs a private server or a Seasonal server.

Evidence conventions:
- **Live run:** the bot running on the official MMO under the user's account, with the result captured through `npm run console` and
  `npm run stats` into `logs/`. A deploy to the MMO is a push of the staged `deploy/` folder through the GitHub link (ADR-012). Claude stages it
  only when the user names `mmo`, and the user pushes.
- **Native scenario test:** a pure-Rust test in `rustedscreep-core` that drives a decision module (bootstrap recovery, combat sim,
  formations, offense state machines, breaker solver) through a scripted situation, with expectations derived from constants.
  Scripted in-game scenarios belong to the private-server track.
- CPU numbers are `Game.cpu.getUsed()` deltas from the profiler, reported as mean and p95 over ≥ 1000 ticks unless stated otherwise.
- The **naive baseline** is the same bot with every cache and optimization toggled off through runtime config. This isolates the
  value of each optimization. Each cache gets a toggle. On the MMO it runs only in short, bucket-gated windows, because the free-tier
  limit is 20 CPU (VERIFICATION_LOG V35): optimized, naive and optimized windows alternate at a comparable colony state, and each window
  records its tick range, RCL, room count, creep count and intents. The window length and the bucket level that stops a window early
  are TUNING values set in P2. They are the stated exception to the ≥ 1000-tick rule. Same-seed comparisons and the unmodified
  screeps-starter-rust bot are on the private-server track (PS2).
- Never record an estimated number as measured.

---

## P0 Kickoff and verification (no bot code): `/kickoff`
- Clarifying questions answered and recorded in `docs/PROJECT_CONTEXT.md`.
- Seeded findings re-verified. The `tooling`, `cpu-memory` and `pipeline` groups verified.
- Feature probe run on the official MMO, with results in VERIFICATION_LOG. The private server is probed in PS1 and Seasonal in SE1.
- ADR-001..006 and ADR-008 proposed with evidence.

**Gate:**
- The user has answered the questions or accepted the stated assumptions.
- The probe results are recorded for the official MMO.
- The user has accepted ADR-001, 002, 003, 005, 006 and 008.
- No `tooling` row remains `unverified` without an assumption and a fallback.

## P1 Build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check
Modules:
1. Workspace: root `Cargo.toml` with members `crates/{rustedscreep-consts,rustedscreep-core,rustedscreep-wasm}`, `default-members` excluding `rustedscreep-wasm`, and the
   release profile at the root (`opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="debuginfo"` (VERIFICATION_LOG D7), per SPEC §1 and ADR-002).
   Toolchain pinned in `rust-toolchain.toml`. Clippy no-panic denies at the crate roots.
2. `rustedscreep-consts`: the facade over the API constants, `extra.rs` with sources and dates, and constant-derived tests.
3. Build: wasm build (ADR-001) + `wasm-opt` (flags from ADR-002) + a Rollup bundle of the TypeScript loader compiled by `tsc` to ES2022, no Babel (ADR-008), into `dist/`. npm scripts: `build`, `check`, `stage` (runs the release build and copies `main.js` and the `.wasm` into the tracked `deploy/` folder for the GitHub link, or into `--out <dir>` for tests and dry runs; reports sizes, a `.wasm` byte hash and a build ID; ADR-012),
   `size`, `deploy`, `console`, `stats`. The user creates `.screeps.yaml` (format: VERIFICATION_LOG V93); the repo has no example file (the user removed it on 2026-11-14).
4. Loader (`js/src/main.ts`):
   - Boot gated on bucket, with compile and instantiate spread across ticks.
   - Calls `loop`.
   - Trap and aborted-tick detection, with recovery per ADR-003 and backoff.
   - `console.error` routing and one notification per incident.
   - `trap-test` export wired to a console command.
   - At boot, logs the build ID, `Game.time`, the type of the value `require()` returned for the wasm and a hash over its bytes (V77).
5. Snapshot: one pass building SoA structs with interned handles. The tick context includes time, CPU limit, tickLimit and bucket.
6. Scheduler:
   - Critical, Standard and Optional tiers.
   - A CPU budget derived from limit, bucket and tickLimit, with formulas from verified rows.
   - Time-sliced jobs.
   - A sleep wheel for "wake at tick N or on event".
   - Per-unit `Result` isolation.
7. Act layer: intent buffer, no-op filtering, the intent-priority table (from P0 `pipeline`) enforced, and a per-tick intent counter.
8. Profiler (`profiler` feature): sparse spans per subsystem, written to a stats RawMemory segment.
9. Persistence:
   - Segment manager with activation scheduling and dirty tracking.
   - Versioned header with checksum.
   - Encoding per ADR-004.
   - Rebuild on any decode failure.
10. Config: `config.rs` with the `mmo` server profile and runtime overrides from a segment (validated and clamped). Profiles are a table,
    so the optional tracks can add `private` (PS1) and `season` (SE2) as new rows without changing `mmo` behavior or any core gate. `docs/TUNING.md` plus the doc-sync test.
11. Constants self-check: at init (Optional tier, once per global), compare `rustedscreep-consts` against the server's JS globals and log mismatches.

The MMO room has no economy code until P2 lands, so these P1 live checks cost no colony progress. But a controller that is never
upgraded downgrades: an RCL1 room is lost about 20000 ticks after it is claimed (CONTROLLER_DOWNGRADE[1], VERIFICATION_LOG V71), roughly
a day on the MMO. Run the P1 live checks shortly before P2 is ready, or respawn before the P2 deploy. When to respawn is the user's call.

**Gate:**
- `/check` is all green.
- The wasm loads on the official MMO.
- `trap-test` on the MMO shows recovery within the ADR-003 bound, and the bot never bricks: 10 forced traps in a row recover.
- Cold-start cost (compile + instantiate + first tick) is measured on the MMO, and the boot gate is set from it.
- Empty-bot CPU per tick is measured on the MMO. This is the floor.
- The persistence round-trip test passes natively, including the corrupt-segment test, and one live round trip on the MMO succeeds.
- Sizes are recorded.
- The GitHub-link deploy path is verified with the first checked build, staged by `/deploy mmo` and pushed by the user (V77): the
  wasm hash the loader logs in game equals the one `npm run stage` reported, the sentinel module shows the deletion behavior, the
  push-to-first-run delay is recorded, and a docs-only push shows whether an identical re-sync reloads the code. If the hashes differ,
  ADR-012's fallback is in place.

## P2 Bootstrap (SPEC §3.1) and movement core
Modules:
- Movement: in-room cost grids from terrain plus structures, BFS/flow fields to common targets, cached creep paths that re-path only
  when blocked, and a traffic manager (swap, shove, priority). `move(direction)` is issued from Rust.
  Inter-room routes use PathFinder with cached results.
- Minimal spawn path, which the P5 queue replaces.
- RCL1–3 opening, zero-creep recovery, and deadlock watchdogs (for example no harvester, or energy stuck in containers).

**Gate:**
- The user's fresh MMO room reaches RCL3 with no manual action. Record the tick count.
- Zero-creep recovery: native scenario tests (no creeps and an empty spawn, at RCL2 and at RCL3) recover. Any live recovery event is
  logged. Deliberately killing the colony's creeps is a private-track scenario (PS3).
- No deadlock over 20k live ticks.
- CPU per tick is recorded against the naive baseline.

## P3 Economy (SPEC §3.2)
Modules:
- Static miners on containers or links.
- Link network.
- Haulers sized from distance and throughput math.
- Upgraders sized to the energy budget.
- Minimal builders and repairers.
- Body solver from targets, with no hardcoded bodies.

**Gate:**
- Tests derive bodies and hauler sizing from constants.
- Measured live harvest efficiency (energy harvested per source per regen cycle against capacity) meets the target set in TUNING.
- CPU per room per tick is recorded.

## P4 Base planner (SPEC §3.3): ADR-007 decided here
Modules:
- Terrain corpus fixtures (≥ 30 rooms exported from MMO maps) in `crates/rustedscreep-core/tests/fixtures/`.
- Candidates: a stamp/bunker planner, and distance transform with min-cut ramparts. Benchmark both, then decide ADR-007.
- Placement of extensions, towers, labs, storage, terminal, links, roads, and ramparts. Incremental site placement per RCL, cached, with periodic verification.

**Gate:**
- The planner is deterministic and valid on the whole corpus: counts per RCL come from constants, and every structure is reachable.
- Criterion bench numbers are recorded.
- The planner runs time-sliced in the Optional tier.

## P5 Global spawn queue (SPEC §3.4)
Modules:
- A priority queue across spawns and rooms, with energy-aware bodies.
- Pre-spawn replacement timed to spawn time plus travel time.
- No wasted energy (no body or energy left idle while the queue is non-empty).

**Gate:**
- Replacement-timing tests pass.
- A live run shows no idle spawn time with a non-empty queue, beyond the energy wait.
- Creep handover gaps are measured.

## P6 Remote mining (SPEC §3.5)
- Profit model per CPU and per energy, with tests.
- Reservers, haulers and roads.
- Auto-abandon of contested or unprofitable rooms, and threat-driven evacuation hooks for P7.

**Gate:**
- Model tests pass.
- Native scenario tests feed threat and contest inputs to the remote manager and show abandon and evacuation.
- Net energy per CPU is recorded from a live run. Any natural threat event in a remote room (for example NPC invaders) is logged with the
  bot's response. Scripted invaders are a private-track scenario (PS3).

## P7 Defense (SPEC §3.6)
- Threat assessment from hostile bodies and boosts.
- Tower focus fire and heal priority, driven by the combat sim.
- Rampart upkeep.
- Safe-mode triggers.
- Scaled defenders.
- Remote evacuation.

**Gate:**
- `/combat-verify` reports no high-severity findings.
- Native combat scenario tests (melee, ranged and a healer duo against the room's tower and defender configuration) pass, with expected
  outcomes derived from constants.
- Safe-mode trigger tests pass.
- Any live defense event is logged with the bot's response. Scripted attacks are a private-track scenario (PS3).

## P8 Expansion (SPEC §3.7)
- Scouting and observers.
- Claim scoring.
- Claimers and pioneers.
- GCL and CPU limits respected.

**Gate:**
- Scoring tests pass.
- Once GCL allows a second room (VERIFICATION_LOG R28), a live run claims and bootstraps it without lowering the home room's RCL
  progress below the threshold in TUNING.

## P9 Late game (SPEC §3.8)
- Minerals.
- Lab reactions and boost production.
- Factory.
- Terminal balancing.
- Market with sanity limits (price bands, volume caps, credit floor).
- Power processing.
- Power creeps.
- Pixels. These run only when the bucket is full, the scope allows them, and config enables them.

**Gate:**
- Per-feature tests pass.
- Market sanity-limit tests pass.
- Live runs never generate pixels below the configured bucket.

## P10 Offense core (SPEC §4a, §4c budgets)
- Offense energy and CPU budgets enforced first.
- Target selection from intel, with the ally whitelist and protection rules.
- Formations (duo, quad, larger squads) as pure Rust, with fatigue-synchronized movement, quad rotation and heal distribution.
- Boost planning from target tower damage and heal output, with lab preparation and a stock check before launch.
- Combat simulation before commitment, with an abort threshold.
- The stage → boost → approach → breach → clear → claim|leave state machine.
- Optional: a nuker and power-creep launch policy.

**Gate:**
- Formation unit tests pass (rotation, fatigue sync, heal distribution).
- Whitelist and protection tests pass.
- Native scenario tests drive the state machine through every phase against simulated targets, including at least one abort.
- Live evidence: one sortie on the MMO against a target the user approves, with every phase logged. If the user decides not to run live
  offense, that decision is recorded in STATUS instead. A scripted dummy base is a private-track scenario (PS3).

## P11 Siege breaker (SPEC §4b)
- Body solver (TOUGH, WORK, MOVE; up to 50 parts; boosts; MOVE from route terrain; TOUGH from measured towers) with ETK and survival outputs.
- Nearest-economy-structure targeting by path cost, with ramparts and walls as dismantle obstacles and hysteresis.
- Breach logic.
- Survival rules and the "no regrets" threshold.
- Launch controller: cooldown, at most one per target room, a global cap, and gates on energy, boosts and bucket.
- Sortie telemetry.

**Gate:**
- Unit tests pass for the solver, targeting and survival sim.
- A native siege scenario against simulated towers reports predicted ETK and survival, and the survival rules retreat when they should.
- Telemetry records are present.
- Live evidence: one breaker sortie on the MMO against a target the user approves, with predicted ETK and survival logged next to what
  happened. If the user decides not to run it, that decision is recorded in STATUS instead. A dummy base with towers is a private-track
  scenario (PS3).

## P12 Debug, visuals and final deliverables (SPEC §4c, §6)
- RoomVisual overlays for squads, breaker targets and paths (`visuals` feature, Optional tier). Combat logs.
- `docs/ARCHITECTURE.md`, `docs/TEST_PLAN.md` and `docs/TUNING.md` complete. TEST_PLAN covers the core evidence (unit, native scenario and
  live MMO checks) and links to the optional tracks' sections.
- VERIFICATION_LOG closing list of remaining unverified items and manual checks.

**Gate:**
- The CPU and bucket comparison against the naive baseline is documented with its method.
- Every threshold in `config.rs` is documented.
- `/check` is green.
