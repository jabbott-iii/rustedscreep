<!-- Original requirements from the user, verbatim. Do not edit. Record deviations in DECISIONS.md and discrepancies in VERIFICATION_LOG.md. -->

You are an expert Screeps engineer and Rust systems programmer. Design and implement a top-tier Screeps: World bot in Rust, compiled to WebAssembly, loaded by a thin TypeScript glue layer. The goal is maximum colony performance (GCL/RCL growth, energy throughput, defense, expansion, offense) at the lowest CPU cost per tick. Never trade real performance for CPU savings. Trade away wasted work.

## 0. Accuracy rules: investigate sources of truth, do not trust memory

Do not rely on remembered numbers. Before writing any code that depends on a game rule, API signature, or tool behavior, look up the authoritative source, then reference it in code via constants from the API rather than hardcoded values. If a source cannot be reached, mark the item `VERIFY`, state your assumption, and use the safest alternative.

### Sources of truth (consult in this order)
1. `screeps-game-api` crate docs (docs.rs, for the pinned version) and its source/examples. This is the authority for Rust signatures and exported constants.
2. Official Screeps docs: API reference (docs.screeps.com/api/), game mechanics pages, and the constants listing.
3. Screeps engine/common source (github.com/screeps) for exact behavior the docs leave ambiguous (intent processing, damage order, action pipeline).
4. Official release notes/changelog for balance changes.
5. Community material (wiki, open-source bots like Overmind), used only as a hint and to be cross-checked against 1-3.

### Parameters to investigate (never hardcode, never assume)
For each item: find the source, record the value/behavior and the version it applies to, and use the API constant in code where one exists.

- **Tooling and versions:** current compatible versions of `screeps-game-api`, `wasm-bindgen`, `wasm-pack`, `wasm-opt`, `cargo-screeps`, Rollup, and the Rust toolchain. Supported build target and loader pattern. Whether `panic="abort"` is compatible with the panic-recovery design.
- **CPU and memory:** the CPU limit formula for the target server/shard and subscription state, the bucket maximum, the cost and requirements of `generatePixel`, the cost of WASM compile/instantiate on a global reset, the `Memory` size limit, and the RawMemory segment count/size/active limit.
- **Creep rules:** lifetime (including per-part exceptions such as CLAIM), the maximum body size, part costs, spawn time per part, fatigue rules per terrain/road, carry/capacity behavior.
- **Economy:** source capacity and regeneration time, controller downgrade timers, structure counts per RCL, link/terminal/factory costs and cooldowns, and the energy/throughput numbers your haulers and upgraders are sized from.
- **Combat:** tower damage, optimal range and falloff, heal and ranged-heal power, attack/ranged-attack/dismantle power per part, damage application order across body parts, TOUGH-boost damage reduction, and how multiple damage sources combine within a tick.
- **Boosts:** the full list of compounds, which part and action each affects, and the exact multiplier per tier (including dismantle, fatigue, carry, attack, heal, and TOUGH). Lab production chains and reaction times.
- **Action pipeline:** which creep intents can run in the same tick, which override which, and the resulting constraints for the duo, quad and siege-breaker roles. Do not design intents until this is confirmed in the engine source.
- **Safe mode and protection:** duration, cooldown, charges, what it blocks (including dismantle and attacks on structures), and the rules for novice/respawn areas or any other attack restrictions.
- **Vision and scouting:** observer range and when `observeRoom` results become available, and what visibility is needed to see structures in a room.
- **Expansion:** the GCL/room-count relationship, claim/reserve mechanics and timers, and shard-travel rules.
- **Market and power:** order, fee and price rules, terminal cooldown and transfer cost, power processing rates, power creep abilities and levels.
- **Pathfinding:** `PathFinder`/`CostMatrix` behavior (room limits, ops limits, `maxRooms`, flee mode), and the movement-intent semantics.

### How to apply this
- Create a `constants` module that re-exports `screeps-game-api` constants wherever they exist. Put any value not available as an API constant in one clearly labeled place with a source URL/commit and a "last verified" note, so it is easy to update.
- Every non-trivial formula (body solver, tower-damage simulation, fatigue/MOVE math, throughput sizing) must cite which constants it uses. Where practical, add a test that derives expected values from those constants, so a balance change surfaces as a failing test rather than silent drift.
- Never invent a function, method, field, constant, or crate feature. If it is not found in the docs/source you consulted, say so.
- Pin versions you actually looked up. If you could not verify a version, say that rather than guessing.
- Open the deliverables with a "verification log": for each parameter above, the source consulted, the finding, and the confidence (verified / inferred / unverified). Close with a list of remaining unverified items and what I should check manually.
- When your findings conflict with something stated in this prompt, trust the source of truth, flag the discrepancy, and continue.

## 1. Tech stack and build
- Rust + `screeps-game-api` (wasm-bindgen), `wasm-bindgen`, `js-sys`, `log`/`fern`, `console_error_panic_hook`.
- Build with `wasm-pack build --target web` (or `cargo-screeps`), then `wasm-opt`. Release profile: `opt-level=3`, `lto="fat"`, `codegen-units=1`, `panic="abort"` (confirm it is compatible with the panic-recovery design), strip.
- TypeScript glue (no game logic), bundled with Rollup and deployed via `screeps-api`/`cargo-screeps`:
  1. Instantiate WASM only when `Game.cpu.bucket` is high enough (compilation on a global reset is expensive). Spread init across ticks if needed.
  2. Call the exported Rust `game_loop()` each tick.
  3. Catch traps and panics, log them, and re-instantiate on the next tick. A panic must never permanently brick the bot.

## 2. CPU-efficiency architecture
- Minimize JS<->WASM crossings: snapshot reads at tick start into Rust-native structs, compute in pure Rust, issue commands in a batch.
- No serde/JSON in hot paths. Keep state in Rust statics (lost on global reset, so rebuild cheaply). Persist only what must survive resets, in a compact binary format in RawMemory segments or Memory, written only when dirty.
- Explicit caches with invalidation: terrain, static room objects, layouts, CostMatrices, stored paths (re-path only when blocked).
- Own in-room pathfinding in Rust (flow fields, BFS, A* on `[u8; 2500]`). Use `PathFinder` only for inter-room routes, with cached results.
- Priority scheduler with CPU tiers: Critical (spawning, defense, harvesting) always runs. Standard (hauling, upgrading, building) runs unless the bucket is critically low. Optional (planning, market, scouting, visuals, pixels, offense planning) runs only above bucket thresholds. Time-slice heavy work.
- Bucket policy: aim for a full bucket, spend surplus (extra planning, remotes, `generatePixel`), keep a safety margin, and degrade features on low bucket.
- Sleep idle creeps until tick N or an event. No `find` in loops. No allocations in hot loops (reuse buffers, SmallVec, FxHash, numeric IDs).
- Built-in per-subsystem CPU profiler (sparse `getUsed()` sampling), compiled out via a feature flag.

## 3. Features (implement in this order)
1. Bootstrap: RCL1-3 opening, zero-creep recovery, no deadlocks.
2. Economy: static miners, links, haulers sized from distance/throughput math, upgraders sized to budget, minimal builders. Compute bodies from targets, not hardcoded.
3. Automatic base planner (stamp/bunker or distance-transform): extensions, towers, labs, storage, terminal, links, roads, ramparts. Incremental site placement, cached, periodically verified.
4. Global spawn queue: energy-aware bodies, replacement timed to travel time, no wasted energy.
5. Remote mining: profit per CPU and per energy, reservers, haulers, roads, auto-abandon contested or unprofitable rooms.
6. Defense: tower focus fire and heal priority, threat assessment, rampart upkeep, safe-mode triggers, scaled defenders, remote evacuation.
7. Expansion: scouting/observers, claim scoring, claimers and pioneers, respect GCL and CPU limits.
8. Late game: minerals, lab reactions and boosts, factory, terminal balancing, market with sanity limits, power processing and power creeps, pixels.
9. Offense (required, not optional): section 4.

## 4. Offense module (mandatory)
### 4a. Core offense
- Target selection: rank hostile rooms and structures by value, defense strength (towers, ramparts, creeps, safe-mode status) and distance, using scouting/observer intel. Configurable ally/whitelist, and never attack listed players.
- Formations: duo (attacker + healer), quad (2x2 with coordinated movement and rotation), larger squads. Movement must be fatigue-synchronized and healing distributed correctly. Keep the formation logic pure Rust so it can be unit tested.
- Boosting: compute required boosts from the target's tower damage and heal output. Labs prepare boosts. Verify stock before launch.
- Combat math: model tower damage falloff by range, heal output and TOUGH damage reduction (applied in body order). Simulate expected outcomes before committing, and abort/retreat when expected loss exceeds a threshold.
- Phases: stage -> boost -> approach -> breach -> clear -> optionally claim or leave. Each is a state machine with explicit abort conditions.
- Optional force multipliers: nukers and power creeps with a launch policy.
- Do not target rooms where attacks are impossible (safe mode, novice/respawn-protected areas). Handle this gracefully.

### 4b. Siege breaker (new)
An occasional, single-unit, heavily armored deconstruction creep that attacks the nearest enemy economy structure.
- Role: one creep at a time. It is spawned only occasionally, controlled by a configurable cooldown (ticks between launches), a max of one active breaker per target room, and a global cap. It is gated on energy reserves, boost stock, and a healthy bucket. Spawn priority is below economy and defense.
- Body (computed by a solver, not hardcoded): up to 50 parts, ordered TOUGH first, then WORK, MOVE last. Typical ratio: heavy TOUGH (boosted XGHO2, -70% damage), many WORK (boosted XZH2O for +300% dismantle), and enough MOVE (boosted XZHO2 if available) for the terrain along the planned route. Solve for MOVE count from the fatigue rules per terrain type (plain/swamp/road). The solver takes the target room's measured tower count and damage, and picks the TOUGH count that lets the creep survive long enough to destroy its objective. Output: body, boost list, expected time-to-kill and expected survival time.
- Targeting: "nearest enemy economy structure" is measured by real path cost from the creep (using the PathFinder cost model, with enemy ramparts/walls counted as obstacles to dismantle), not by straight-line range. Define economy structures explicitly: spawn, extension, link, storage, terminal, container, lab, factory, power spawn. Exclude the controller and defensive structures unless they block the route. Re-evaluate the nearest target when the current one dies, with hysteresis to avoid flip-flopping.
- Breaching: if a rampart or wall blocks access to the chosen structure, dismantle the blocking rampart first, then the structure. Recompute when the situation changes.
- Survival rules: continuously evaluate incoming tower damage against remaining TOUGH hits. Retreat when survival time drops below time-to-objective, and when boosts are gone or the creep is not paying off. Add a "no regrets" retreat threshold to configuration.
- Verify (`VERIFY`) in the docs whether dismantle can be combined with other actions in the same tick, and design the creep's intents around the real pipeline.
- Telemetry: log the cost (energy and boosts) versus damage dealt for every sortie, and use it to tune the spawn cooldown and body solver.
- Unit-test the body solver, nearest-target selection, and the survival simulator in pure Rust.

### 4c. Offense budgets and debug
- Offense has its own energy and CPU budget and runs in the Standard/Optional tiers. It must never starve the economy or defense.
- RoomVisual overlays for squads, breaker targets and paths, plus combat logs.

## 5. Code quality
- Cargo workspace with modules: snapshot, scheduler, planner, economy, spawning, logistics, defense, offense, expansion, movement, market, profiling, util.
- Data-oriented design, no trait-object-heavy hot paths. Per-room and per-creep error isolation.
- Pure logic (planner, bodies, scheduler, flow fields, combat sim, formations, breaker solver) is testable with `cargo test` without the game. `criterion` benchmarks for the planner, flow field and combat sim (native targets only).
- Robust to global resets, respawns, shard travel and missing Memory.

## 6. Deliverables
1. Architecture doc: per-tick data flow (snapshot -> decide -> act), cache and invalidation table, CPU tiers.
2. Project tree, `Cargo.toml`, `package.json`, Rollup/TS config, build and deploy scripts, `.cargo` config.
3. Complete compilable Rust and TS source, module by module, starting with the build pipeline, loader, snapshot and scheduler.
4. Test plan: unit tests, `screeps-launcher`/private-server instructions, CPU/bucket comparison against a naive baseline, and offense tests against a scripted dummy base (including a siege-breaker scenario with towers).
5. Tuning guide listing every threshold (bucket tiers, cache TTLs, spawn lead time, offense abort thresholds, breaker cooldown) with its rationale.

## 7. Method
- Before each module, state CPU/performance tradeoffs and the cache strategy in a few lines.
- After each module, summarize what is done, what is stubbed and what to test next.
- Ask clarifying questions first: MMO or private server, CPU limit and subscription, current GCL/RCL, pixels/market scope, ally list, breaker launch frequency.