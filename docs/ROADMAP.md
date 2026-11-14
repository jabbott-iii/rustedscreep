# Roadmap and exit gates

The feature order follows SPEC §3. Each phase lists its modules, which become STATUS checklist items, and an **exit gate**.
A gate passes only with evidence recorded in STATUS.md (`/phase-gate P#`). Every module follows the `/module` protocol.
"Private-server run" means screeps-launcher or equivalent, with the result captured through `npm run console` and `npm run stats` into `logs/`.

Measurement conventions:
- CPU numbers are `Game.cpu.getUsed()` deltas from the profiler, reported as mean and p95 over ≥ 1000 ticks unless stated otherwise.
- The **naive baseline** is the same bot with every cache and optimization toggled off through runtime config. This isolates the
  value of each optimization. Each cache gets a toggle. A second reference is the unmodified screeps-starter-rust bot, used for P1–P2 only.
- Never record an estimated number as measured.

---

## P0 Kickoff and verification (no bot code): `/kickoff`
- Clarifying questions answered and recorded in `docs/PROJECT_CONTEXT.md`.
- Seeded findings re-verified. The `tooling`, `cpu-memory` and `pipeline` groups verified.
- Feature probe run on every target server, with results in VERIFICATION_LOG.
- ADR-001..006 and ADR-008 proposed with evidence.

**Gate:**
- The user has answered the questions or accepted the stated assumptions.
- The probe results are recorded for each target server.
- The user has accepted ADR-001, 002, 003, 005, 006 and 008.
- No `tooling` row remains `unverified` without an assumption and a fallback.

## P1 Build pipeline, loader, snapshot, scheduler, profiler, persistence, consts self-check
Modules:
1. Workspace: root `Cargo.toml` with members `crates/{game-consts,bot-core,bot-wasm}`, `default-members` excluding `bot-wasm`, and the
   release profile at the root (`opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip`, per SPEC §1 and ADR-002).
   Toolchain pinned in `rust-toolchain.toml`. Clippy no-panic denies at the crate roots.
2. `game-consts`: the facade over the API constants, `extra.rs` with sources and dates, and constant-derived tests.
3. Build: wasm build (ADR-001) + `wasm-opt` (flags from ADR-002) + Rollup/Babel bundle (ADR-008) into `dist/`. npm scripts: `build`, `check`,
   `size`, `deploy`, `console`, `stats`. Add `.example-screeps.yaml` to the repo with no secrets. The user creates `.screeps.yaml`.
4. Loader (`js/src/main.ts`):
   - Boot gated on bucket, with compile and instantiate spread across ticks.
   - Calls `loop`.
   - Trap and aborted-tick detection, with recovery per ADR-003 and backoff.
   - `console.error` routing and one notification per incident.
   - `trap-test` export wired to a console command.
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
10. Config: `config.rs` with server profiles (mmo, season, private) and runtime overrides from a segment (validated and clamped).
    `docs/TUNING.md` plus the doc-sync test.
11. Constants self-check: at init (Optional tier, once per global), compare `game-consts` against the server's JS globals and log mismatches.

**Gate:**
- `/check` is all green.
- The wasm loads on the private server.
- `trap-test` shows recovery within the ADR-003 bound, and the bot never bricks: 10 forced traps in a row recover.
- Cold-start cost (compile + instantiate + first tick) is measured on each reachable server, and the boot gate is set from it.
- Empty-bot CPU per tick is measured. This is the floor.
- The persistence round-trip test passes, including the corrupt-segment test.
- Sizes are recorded.

## P2 Bootstrap (SPEC §3.1) and movement core
Modules:
- Movement: in-room cost grids from terrain plus structures, BFS/flow fields to common targets, cached creep paths that re-path only
  when blocked, and a traffic manager (swap, shove, priority). `move(direction)` is issued from Rust.
  Inter-room routes use PathFinder with cached results.
- Minimal spawn path, which the P5 queue replaces.
- RCL1–3 opening, zero-creep recovery, and deadlock watchdogs (for example no harvester, or energy stuck in containers).

**Gate:**
- A fresh spawn on the private server reaches RCL3 with no manual action. Record the tick count.
- Killing every creep at RCL2 and RCL3 recovers automatically.
- No deadlock over 20k ticks.
- CPU per tick is recorded against the naive baseline and the starter bot.

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
- Measured harvest efficiency (energy harvested per source per regen cycle against capacity) meets the target set in TUNING.
- CPU per room per tick is recorded.

## P4 Base planner (SPEC §3.3): ADR-007 decided here
Modules:
- Terrain corpus fixtures (≥ 30 rooms exported from the private server and MMO maps) in `crates/bot-core/tests/fixtures/`.
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
- A private-server run shows no idle spawn time with a non-empty queue, beyond the energy wait.
- Creep handover gaps are measured.

## P6 Remote mining (SPEC §3.5)
- Profit model per CPU and per energy, with tests.
- Reservers, haulers and roads.
- Auto-abandon of contested or unprofitable rooms, and threat-driven evacuation hooks for P7.

**Gate:**
- Model tests pass.
- A private-server run with a scripted invader shows abandon and evacuation.
- Net energy per CPU is recorded.

## P7 Defense (SPEC §3.6)
- Threat assessment from hostile bodies and boosts.
- Tower focus fire and heal priority, driven by the combat sim.
- Rampart upkeep.
- Safe-mode triggers.
- Scaled defenders.
- Remote evacuation.

**Gate:**
- `/combat-verify` reports no high-severity findings.
- Scripted attack scenarios on the private server (melee, ranged and a healer duo) are logged.
- Safe-mode trigger tests pass.

## P8 Expansion (SPEC §3.7)
- Scouting and observers.
- Claim scoring.
- Claimers and pioneers.
- GCL and CPU limits respected.

**Gate:**
- Scoring tests pass.
- A private-server run claims and bootstraps a second room without lowering the home room's RCL progress below the threshold in TUNING.

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
- Pixels are never generated below the configured bucket.

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
- A private-server run against a scripted dummy base logs every phase, including at least one tested abort.

## P11 Siege breaker (SPEC §4b)
- Body solver (TOUGH, WORK, MOVE; up to 50 parts; boosts; MOVE from route terrain; TOUGH from measured towers) with ETK and survival outputs.
- Nearest-economy-structure targeting by path cost, with ramparts and walls as dismantle obstacles and hysteresis.
- Breach logic.
- Survival rules and the "no regrets" threshold.
- Launch controller: cooldown, at most one per target room, a global cap, and gates on energy, boosts and bucket.
- Sortie telemetry.

**Gate:**
- Unit tests pass for the solver, targeting and survival sim.
- The private-server scenario against a dummy base with towers completes. The log shows predicted ETK and survival against what actually happened.
- Telemetry records are present.

## P12 Debug, visuals and final deliverables (SPEC §4c, §6)
- RoomVisual overlays for squads, breaker targets and paths (`visuals` feature, Optional tier). Combat logs.
- `docs/ARCHITECTURE.md`, `docs/TEST_PLAN.md` and `docs/TUNING.md` complete.
- VERIFICATION_LOG closing list of remaining unverified items and manual checks.

**Gate:**
- The CPU and bucket comparison against the naive baseline is documented with its method.
- Every threshold in `config.rs` is documented.
- `/check` is green.
