# Verification groups (from SPEC §0, "Parameters to investigate")

Each item needs a source, a value or behavior, the version or commit it applies to, and a confidence level. Use the API constant where one exists.

## tooling
- Current compatible versions: `screeps-game-api` and its cargo features, `wasm-bindgen` (crate and CLI must match), `js-sys`,
  wasm-pack (where it is maintained now), binaryen/`wasm-opt` and its lowering passes, `cargo-screeps` (and its compatibility
  with the pinned `screeps-game-api`), Rollup, Babel, the `screeps-api` npm package, and the Rust toolchain.
- Supported build target and wasm feature set per target server (feature probe), and the loader pattern the bindings expect.
- Whether `panic = "abort"` is compatible with the recovery design, and what unwinding would require (nightly, build-std, engine
  exception-handling support).
- Code upload size limit, module/file count limits, and how binary modules are uploaded.
- Whether `screeps-game-api` builds and runs its constants and local types on the native host target (ADR-005).

## cpu-memory
- CPU limit formula per server, shard and subscription state. `tickLimit`, bucket maximum and refill rule.
- `generatePixel` cost and requirements, and the feature flag in the crate.
- CPU cost per state-changing intent. Measure the WASM compile/instantiate cost on a global reset on each server.
- `Memory` size limit and parse behavior. RawMemory segment count, segment size and active-segment limit, and when requested segments become
  available. Foreign segments. Heap limits and `getHeapStatistics` availability.

## creep
- Lifetime, including per-part exceptions such as CLAIM. Maximum body size. Part costs. Spawn time per part.
- Fatigue generation per terrain type (plain, swamp, road), fatigue reduction per MOVE, and how CARRY parts count when empty or full.
- Carry capacity, including boosted carry.

## economy
- Source capacity by room type (owned, reserved, unreserved, keeper) and regeneration time. Harvest per WORK.
- Controller downgrade timers, upgrade per WORK, any per-tick upgrade cap at max RCL, and how the safe-mode downgrade interacts.
- Structure counts per RCL (`CONTROLLER_STRUCTURES`). Build and repair power. Decay rates for roads, containers and ramparts.
- Link transfer loss and cooldown, terminal send cost formula and cooldown, factory production and cooldowns.

## combat
- Tower attack, heal and repair power, optimal range, falloff range and falloff formula.
- Heal and rangedHeal power. Attack, rangedAttack (and rangedMassAttack by range) and dismantle power per part.
- Damage application order across body parts. TOUGH boost reduction and how it is applied. How several damage and heal
  sources in one tick combine (engine source).
- Maximum rampart and wall hits per RCL.

## boosts
- The full compound list, with the part and action each affects and the exact multiplier per tier (dismantle, fatigue, carry, attack, ranged,
  heal, TOUGH, harvest, build/repair, upgrade). Boost cost per part (mineral and energy). Unboost rules.
- Lab reaction chains, reaction times and cooldowns, and lab range rules.

## pipeline
- The engine intent-priority table (file and commit). Which intents run together. Last-call-wins behavior for the same method.
- Movement semantics: swapping, pulling, fatigue, collision resolution, and when move intents fail silently.
- Consequences for duo, quad and siege-breaker intent design.

## safemode
- Safe-mode duration, cooldown, charges and how they are gained. Exactly what it blocks, including dismantle and attacks on structures.
- Novice and respawn area rules, and any other attack restrictions.

## vision
- Observer range and the tick on which `observeRoom` results become visible. The visibility needed to see structures, and what scouts reveal.

## expansion
- The GCL to room-count relationship. Claim and reserve mechanics and timers (reservation per CLAIM part, maximum reservation).
- Shard travel: portals, InterShardMemory limits and the CPU allocation across shards.

## market-power
- Order creation fee, price rules, deal energy cost, and terminal cooldown.
- Power processing rate and cost. Power-creep levels, abilities, cooldowns and operate ranges. Pixel and market rules for pixels.

## pathfinding
- PathFinder limits (`maxOps`, `maxRooms`, `maxCost`, heuristic weight), CostMatrix semantics (0 means terrain default, 255 means impassable),
  flee mode, `roomCallback` returning `false`, CPU characteristics, and the semantics of movement intents.
