# Roadmap and exit gates: MMO core track

There are three roadmaps (user decision, 2026-11-13; ADR-010, where an ADR is an architecture decision record in `docs/DECISIONS.md`):
- **MMO core track (this file, P0–P12):** the complete Screeps: World bot for the official MMO (the massively multiplayer online server, the game's persistent shared world). It never depends on the other two.
- **Private-server track (`docs/ROADMAP_PRIVATE.md`, PS1–PS4):** optional. It builds on the core and adds a private test server,
  baseline comparisons, and scripted scenarios.
- **Seasonal track (`docs/ROADMAP_SEASONAL.md`, SE1–SE2):** optional. It builds on the finished core and adapts the bot to a Seasonal server (an official server that runs the game in seasons, each with its own rules).

The feature order follows SPEC §3. Each phase lists its modules, which become STATUS checklist items, and an **exit gate** (the criteria the phase must meet, with evidence, before it counts as done).
A gate passes only with evidence recorded in STATUS.md (`/phase-gate P#`). Every module follows the `/module` protocol (the project skill's steps for building one module end to end).
No core gate needs a private server or a Seasonal server.
Checkboxes show progress: `/module` ticks a module when its STATUS log line records it done, and `/phase-gate` ticks a gate's
criteria and dates its **Gate:** line when the gate passes. STATUS.md holds the evidence and wins if the two disagree.

Evidence conventions:
- **Live run:** the bot running on the official MMO under the user's account, with the result taken from the user's console output
  (the bot's log lines, the `rustedscreep.*` summaries, and segment 99 read in the console; segment 99 is the profiler's stats segment, one of
  the 100 numbered RawMemory segments, persistent strings the game stores for each player), recorded in STATUS.md and VERIFICATION_LOG.md
  (ADR-017 for P1, ADR-021 from P2 on: the decisions that make the user's console output the live evidence; there are no API tools). A
  deploy to the MMO is a push of the staged `deploy/` folder through the GitHub link (the game's repository sync, which on every push to `main` replaces the bot's code in the game with exactly the linked
  folder's files, deleting any other module; ADR-012, V145). Claude stages it only when the user names `mmo`, and the user pushes.
- **Native scenario test:** a pure-Rust test in `rustedscreep-core` that drives a decision module (bootstrap recovery, combat sim,
  formations, offense state machines, breaker solver; P11's siege breaker is one heavily armored unit, or creep, that dismantles enemy structures)
  through a scripted situation, with expectations derived from constants.
  Scripted in-game scenarios belong to the private-server track.
- CPU numbers are `Game.cpu.getUsed()` deltas from the profiler, reported as mean and p95 (95th percentile) over ≥ 1000 ticks (a tick is
  one game turn, in which every player's code runs once) unless stated otherwise.
- The **naive baseline** is the same bot with every cache and optimization toggled off through runtime config. This isolates the
  value of each optimization. Each cache gets a toggle. On the MMO it runs only in short, bucket-gated windows (a window ends early when the
  bucket, the game's reserve of the player's unused CPU, runs low), because the free-tier limit (an account without a subscription or CPU
  unlock) is 20 CPU (VERIFICATION_LOG V35): optimized, naive, and optimized windows alternate at a comparable colony state, and each window
  records its tick range, room controller level (RCL; the controller is the room object whose level sets which structures the room may
  build), room count, creep count, and intents (the actions the bot issues to the game). The window length
  and the bucket level that stops a window early are TUNING values (settings in `config.rs`, listed in `docs/TUNING.md`) set in P2
  (`CacheConfig::naive_max_ticks` and `naive_stop_bucket`, which the bot enforces; ADR-022). They are the stated exception to the ≥ 1000-tick rule. Same-seed comparisons and the unmodified
  screeps-starter-rust bot are on the private-server track (PS2, its baselines phase).
- Never record an estimated number as measured.

---

## P0 Kickoff and verification (no bot code): `/kickoff`
- [x] Clarifying questions answered and recorded in `docs/PROJECT_CONTEXT.md`.
- [x] Seeded findings (the log's pre-filled rows) re-verified. The `tooling`, `cpu-memory`, and `pipeline` groups verified.
- [x] Feature probe (console checks of the WebAssembly, or wasm, features and JavaScript syntax the server accepts, the API globals it defines, and its CPU, shard, and heap values) run on the official MMO, with results in VERIFICATION_LOG. The private server is probed in PS1 (private server setup) and Seasonal in SE1 (season verification).
- [x] ADR-001..006 and ADR-008 (build toolchain, wasm target, trap recovery, persistence encoding, test constants, dependencies, and loader language) proposed with evidence.

**Gate:** passed 2026-11-13.
- [x] The user has answered the questions or accepted the stated assumptions.
- [x] The probe results are recorded for the official MMO.
- [x] The user has accepted ADR-001, 002, 003, 005, 006, and 008.
- [x] No `tooling` row remains `unverified` without an assumption and a fallback.

## P1 Build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check
Modules:
1. [x] Workspace: root `Cargo.toml` with members `crates/{rustedscreep-consts,rustedscreep-core,rustedscreep-wasm}`, `default-members` excluding `rustedscreep-wasm`, and the
   release profile at the root (`opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip="debuginfo"` (VERIFICATION_LOG D7: `strip = true` would drop the section wasm-bindgen reads the wasm's features from), per SPEC §1 and ADR-002).
   Toolchain pinned in `rust-toolchain.toml`. Clippy no-panic denies at the crate roots.
2. [x] `rustedscreep-consts`: the facade over the API constants, `extra.rs` with sources and dates, and constant-derived tests.
3. [x] Build: wasm build (ADR-001) + `wasm-opt` (flags from ADR-002) + a Rollup bundle of the TypeScript loader compiled by `tsc` to ES2022, no Babel (ADR-008), into `dist/`. npm scripts: `build`, `check`, `stage` (runs the release build and copies `main.js` and the wasm into the tracked `deploy/` folder for the GitHub link, the wasm as a base64 text module `rustedscreep_wasm.js` since the link uploads files as text (ADR-015), split into `rustedscreep_wasm_1.js`, … when one module would pass the largest size the link has delivered intact (ADR-028), or into `--out <dir>` for tests and dry runs; reports sizes, a byte hash of the wasm, and a build ID; ADR-012),
   `size`. The API tools `deploy`, `console`, and `stats` moved to P2 (ADR-017) and were then dropped (ADR-021), so no tool reads `.screeps.yaml` (format: VERIFICATION_LOG V93, the screepers unified credentials format); the repo has no example file (the user removed it on 2026-11-14).
4. [x] Loader (`js/src/main.ts`):
   - Boot gated on bucket, with compile and instantiate spread across ticks.
   - Calls `loop`.
   - Trap (a wasm abort, such as the one a Rust panic causes) and aborted-tick (a tick the engine killed for running too long) detection, with recovery per ADR-003 and backoff.
   - `console.error` routing and one notification per incident.
   - `trap-test` export wired to a console command.
   - At boot, logs the build ID, `Game.time`, the type of the value `require()` returned for the wasm, and a hash over its bytes (V77, the question whether the GitHub link delivers the wasm intact; V136 and V137 found that a binary `.wasm` cannot travel through it, so ADR-015 ships it as base64 text).
5. [x] Snapshot (the per-tick copy of game state into Rust): one pass building SoA (structure-of-arrays) structs with interned handles. The tick context includes time, CPU limit, tickLimit (the most CPU this tick may use), and bucket.
6. [x] Scheduler:
   - Critical, Standard, and Optional tiers (priority classes that decide which work runs at each bucket level).
   - A CPU budget derived from limit, bucket, and tickLimit, with formulas from verified rows.
   - Time-sliced jobs.
   - A sleep wheel for "wake at tick N or on event".
   - Per-unit `Result` isolation.
7. [x] Act layer (the step that issues the tick's intents to the game): intent buffer, no-op filtering, the intent-priority table (from P0 `pipeline`) enforced, and a per-tick intent counter.
8. [x] Profiler (`profiler` feature): sparse spans per subsystem, written to a stats RawMemory segment. Ticked by the user's
   decision (2026-11-16): the spans cover tick stages (snapshot, decide, act), and per-tier and per-job accounting is carried in
   STATUS (ARCHITECTURE §4).
9. [x] Persistence:
   - Segment manager with activation scheduling and dirty tracking.
   - Versioned header with checksum.
   - Encoding per ADR-004.
   - Rebuild on any decode failure.
10. [x] Config: `config.rs` with the `mmo` server profile and runtime overrides from a segment (validated and clamped). Profiles are a table,
    so the optional tracks can add `private` (PS1) and `season` (SE2) as new rows without changing `mmo` behavior or any core gate. `docs/TUNING.md` plus the doc-sync test (the test that keeps it in step with `config.rs`).
11. [x] Constants self-check: at init (Optional tier, once per global, meaning each fresh JavaScript environment the server starts for the bot), compare `rustedscreep-consts` against the server's JS globals and log mismatches.

The MMO room has no economy code until P2 lands, so these P1 live checks cost no colony progress. But a controller that is never
upgraded downgrades: an RCL1 room is lost about 20000 ticks after it is claimed (CONTROLLER_DOWNGRADE[1], VERIFICATION_LOG V71), roughly
a day on the MMO. Run the P1 live checks shortly before P2 is ready, or respawn (start over in a fresh room) before the P2 deploy. When to respawn is the user's call.

**Gate:** passed 2026-11-14.
- [x] `/check` is all green.
- [x] The wasm loads on the official MMO.
- [x] `trap-test` on the MMO shows recovery within the ADR-003 bound (in halt mode, the loop runs again on the third tick after a trap),
  and the bot never bricks: 10 forced traps in a row recover.
- [x] Cold-start cost (compile + instantiate + first tick) is measured on the MMO, and the boot gate (the bucket level the loader waits for before it loads the wasm) is set from it.
- [x] Empty-bot CPU per tick: moved to the P2 gate (ADR-016), where the profiler build reports the fixed per-tick cost beside P2's jobs.
  Met there (the CPU floor).
- [x] The persistence round-trip test passes natively, including the corrupt-segment test, and one live round trip on the MMO succeeds.
- [x] Sizes are recorded.
- [x] The GitHub-link deploy path is verified with the first checked build, staged by `/deploy mmo` and pushed by the user (V77): the
  wasm hash the loader logs in game equals the one `npm run stage` reported, the sentinel module (a module added by hand in the game's
  editor) shows the deletion behavior, the push-to-first-run delay is recorded, and a docs-only push shows whether an identical re-sync
  reloads the code. If the hashes differ, ADR-012's fallback (an upload through the game's API, with the link off) is in place. (ADR-015 amends this: the first checked build's `.wasm` could not travel through the link, V136, V137;
  the path was verified with the first build in the base64 text-module form, whose hash matched, V140, V145.)

## P2 Bootstrap (SPEC §3.1) and movement core
Modules:
- [x] Movement: in-room cost grids from terrain plus structures, BFS (breadth-first search)/flow fields (tables of each tile's cost to a goal,
  which creeps share to pick their next step) to common targets, cached creep paths that re-path only
  when blocked, and a traffic manager (swap, shove, priority). `move(direction)` is issued from Rust.
  Inter-room routes use PathFinder (the game's built-in path search across rooms) with cached results.
- [x] Minimal spawn path (the code that orders creeps from a spawn, the structure that makes them), which the P5 queue replaces.
- [x] RCL1–3 opening, zero-creep recovery, and deadlock watchdogs (for example no harvester, or energy stuck in containers), with interim extension placement (extensions are structures that hold extra energy for spawning) from RCL2 until P4 (ADR-020).

**Gate:** passed 2026-11-16.
- [x] The user's fresh MMO room reaches RCL3 with no manual action. Record the tick count.
- [x] Zero-creep recovery: native scenario tests (no creeps and an empty spawn, at RCL2 and at RCL3) recover. Any live recovery event is
  logged. Deliberately killing the colony's creeps is a private-track scenario (PS3).
- [x] No deadlock over 20k live ticks.
- [x] CPU per tick is recorded against the naive baseline.
- [x] The profiler build's live check (P1.8, moved here from the P1 gate by ADR-016): a `profiler` build on the MMO logs its cold loop once, and at least
  two closed 1000-loop windows are read from segment 99 in the console (matched to the boot's `Game.time`; TEST_PLAN §8). The floor, the fixed per-tick cost outside the jobs
  (`start`, the snapshot, and the act step's per-tick overhead, each less its own reading), is taken from a window after the first and recorded
  beside the CPU per tick.

## P3 Economy (SPEC §3.2)
Modules:
- [x] Static miners (creeps that stay at one source, a regenerating energy deposit, and harvest it) on containers or links (structures that send energy across the room).
- [x] Link network.
- [x] Haulers (creeps that carry energy) sized from distance and throughput math.
- [x] Upgraders (creeps that spend energy on the controller's progress) sized to the energy budget.
- [x] Minimal builders and repairers.
- [x] Body solver (which picks a creep's body parts) from targets, with no hardcoded bodies.

**Gate:** passed 2026-11-17.
- [x] Tests derive bodies and hauler sizing from constants.
- [x] Measured live harvest efficiency (energy harvested per source per regen cycle against capacity) meets the target set in TUNING.
- [x] CPU per room per tick is recorded.

## P4 Base planner (SPEC §3.3): ADR-007 decided here
Modules:
- [x] Terrain corpus fixtures (≥ 30 rooms exported from MMO maps) in `crates/rustedscreep-core/tests/fixtures/`.
- [x] Candidates: a stamp/bunker planner (fixed layout pieces stamped onto the room), and distance transform with min-cut ramparts. Benchmark both, then decide ADR-007 (decided 2026-11-18: (b), the flood fill with min-cut ramparts).
- [x] Placement of extensions, towers, labs, storage, terminal, links, roads, and ramparts. Incremental site placement (construction sites, which builders turn into structures) per RCL, cached, with periodic verification. It keeps or replaces the interim extensions P2 placed (ADR-020) and takes over from `opening.place_extensions`.

**Gate:** passed 2026-11-19.
- [x] The planner is deterministic and valid on the whole corpus: counts per RCL come from constants, and every structure is reachable.
- [x] Criterion bench numbers are recorded.
- [x] The planner runs time-sliced in the Optional tier.

## P5 Global spawn queue (SPEC §3.4)
Modules:
- [x] A priority queue across spawns and rooms, with energy-aware bodies.
- [x] Pre-spawn replacement (ordering a creep's successor early enough that it is in place when the old creep dies of age) timed to spawn time plus travel time.
- [x] No wasted energy (no body or energy left idle while the queue is non-empty).

**Gate:** passed 2026-11-21.
- [x] Replacement-timing tests pass.
- [x] A live run shows no idle spawn time with a non-empty queue, beyond the energy wait.
- [x] Creep handover gaps are measured.

## P6 Remote mining (SPEC §3.5)
- [x] Profit model per CPU and per energy, with tests.
- [ ] Reservers (creeps that reserve a remote room's controller, which raises its sources' capacity; a remote room is one the bot mines without owning it), haulers, and roads.
- [ ] Auto-abandon of contested or unprofitable rooms, and threat-driven evacuation hooks for P7.

**Gate:**
- [ ] Model tests pass.
- [ ] Native scenario tests feed threat and contest inputs to the remote manager and show abandon and evacuation.
- [ ] Net energy per CPU is recorded from a live run. Any natural threat event in a remote room (for example NPC invaders, the game's non-player-character raiders) is logged with the
  bot's response. Scripted invaders are a private-track scenario (PS3).

## P7 Defense (SPEC §3.6)
- [ ] Threat assessment from hostile bodies and boosts (mineral compounds that labs apply to body parts to strengthen them).
- [ ] Tower focus fire and heal priority, driven by the combat sim.
- [ ] Rampart upkeep.
- [ ] Safe-mode triggers (when to activate safe mode, which for a limited time stops every creep but the owner's in the room from using any method that affects the owner's creeps or objects; exactly what it blocks is VERIFICATION_LOG R25, still unverified).
- [ ] Scaled defenders.
- [ ] Remote evacuation.

**Gate:**
- [ ] `/combat-verify` reports no high-severity findings.
- [ ] Native combat scenario tests (melee, ranged, and a healer duo against the room's tower and defender configuration) pass, with expected
  outcomes derived from constants.
- [ ] Safe-mode trigger tests pass.
- [ ] Any live defense event is logged with the bot's response. Scripted attacks are a private-track scenario (PS3).

## P8 Expansion (SPEC §3.7)
- [ ] Scouting and observers (structures that give vision of a distant room).
- [ ] Claim scoring.
- [ ] Claimers and pioneers (creeps sent to build up a newly claimed room).
- [ ] GCL (global control level, which caps how many rooms the bot may own) and CPU limits respected.

**Gate:**
- [ ] Scoring tests pass.
- [ ] Once GCL allows a second room (VERIFICATION_LOG R28, the GCL-to-room-count rule, still unverified), a live run claims and bootstraps it without lowering the home room's RCL
  progress below the threshold in TUNING.

## P9 Late game (SPEC §3.8)
- [ ] Minerals.
- [ ] Lab reactions and boost production.
- [ ] Factory.
- [ ] Terminal balancing (moving resources between the rooms' terminals).
- [ ] Market with sanity limits (price bands, volume caps, credit floor).
- [ ] Power processing (registering the power resource to the account in a power spawn, to develop power creeps).
- [ ] Power creeps (hero units tied to the player's account).
- [ ] Pixels (a resource generated from the bucket, 10000 CPU each). These run only when the bucket is full, the scope allows them, and config enables them.

**Gate:**
- [ ] Per-feature tests pass.
- [ ] Market sanity-limit tests pass.
- [ ] Live runs never generate pixels below the configured bucket.

## P10 Offense core (SPEC §4a, §4c budgets)
- [ ] Offense energy and CPU budgets enforced first.
- [ ] Target selection from intel, with the ally whitelist and protection rules.
- [ ] Formations (duo, quad, larger squads) as pure Rust, with fatigue-synchronized movement (fatigue is the cost a creep's weight adds to each move; it cannot move again until its MOVE parts clear it), quad rotation, and heal distribution.
- [ ] Boost planning from target tower damage and heal output, with lab preparation and a stock check before launch.
- [ ] Combat simulation before commitment, with an abort threshold.
- [ ] The stage → boost → approach → breach → clear → claim|leave state machine.
- [ ] Optional: a nuker (the structure that launches nukes) and power-creep launch policy.

**Gate:**
- [ ] Formation unit tests pass (rotation, fatigue sync, heal distribution).
- [ ] Whitelist and protection tests pass.
- [ ] Native scenario tests drive the state machine through every phase against simulated targets, including at least one abort.
- [ ] Live evidence: one sortie on the MMO against a target the user approves, with every phase logged. If the user decides not to run live
  offense, that decision is recorded in STATUS instead. A scripted dummy base is a private-track scenario (PS3).

## P11 Siege breaker (SPEC §4b)
- [ ] Body solver (TOUGH, WORK, MOVE; up to 50 parts; boosts; MOVE from route terrain; TOUGH from measured towers) with ETK (expected time-to-kill) and survival outputs.
- [ ] Nearest-economy-structure targeting by path cost, with ramparts and walls as dismantle obstacles and hysteresis.
- [ ] Breach logic.
- [ ] Survival rules and the "no regrets" threshold.
- [ ] Launch controller: cooldown, at most one per target room, a global cap, and gates on energy, boosts, and bucket.
- [ ] Sortie telemetry.

**Gate:**
- [ ] Unit tests pass for the solver, targeting, and survival sim.
- [ ] A native siege scenario against simulated towers reports predicted ETK and survival, and the survival rules retreat when they should.
- [ ] Telemetry records are present.
- [ ] Live evidence: one breaker sortie on the MMO against a target the user approves, with predicted ETK and survival logged next to what
  happened. If the user decides not to run it, that decision is recorded in STATUS instead. A dummy base with towers is a private-track
  scenario (PS3).

## P12 Debug, visuals and final deliverables (SPEC §4c, §6)
- [ ] RoomVisual (the game's API for drawing in a room's view) overlays for squads, breaker targets, and paths (`visuals` feature, Optional tier). Combat logs.
- [ ] `docs/ARCHITECTURE.md`, `docs/TEST_PLAN.md`, and `docs/TUNING.md` complete. TEST_PLAN covers the core evidence (unit, native scenario, and
  live MMO checks) and links to the optional tracks' sections.
- [ ] VERIFICATION_LOG closing list of remaining unverified items and manual checks.

**Gate:**
- [ ] The CPU and bucket comparison against the naive baseline is documented with its method.
- [ ] Every threshold in `config.rs` is documented.
- [ ] `/check` is green.
