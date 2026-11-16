# Architecture (SPEC §6.1)

Claude Code fills each section in as its phase lands. Keep it factual: describe only what the code does.

## 1. Per-tick data flow: snapshot → decide → act
_P1._ A diagram plus a step list: loader → `loop` → tick context → snapshot → caches → scheduler tiers → decisions →
intent buffer (no-op filtering, pipeline check) → act → persistence flush → profiler flush. Include JS crossings per step.

Current state (P2.4; P1 plus movement, in room and between rooms, the minimal spawn path and the RCL1–3 opening): loader → `loop` (rustedscreep-wasm `game_loop`) → a copy of the config in force (`config::Runtime`, the active profile with the runtime overrides; the whole tick runs on it, so a record that loads during the decide step applies from the next tick, except the log level and the flush's segment limits) → snapshot → movement's tick start (step 5a) → the rate-limited skipped-object log (`SkipLog`, at most one line per `log.skip_window_ticks`) → the intent buffer and the spawner opened for it (the console's spawn request queued) → scheduler tick (the decide step: movement's room reads, a Critical job (P2.1), the opening's plan, a Critical job with one unit per owned room (P2.4, below), the spawn plan, a Critical job with one unit per room that has a request and a ready spawn (not spawning, not held), none on a tick with nothing asked or with every spawn of each asking room busy or held (a room waiting for energy with a ready spawn is planned every tick) (P2.3), the persistence loads, a Standard step job (P1.9), the opening's building and upgrading, a Standard job with one unit per owned room (P2.4), movement's rescans and the console's move order, Standard (P2.1), then the constants self-check, an Optional step job that runs once per global (P1.11), then movement's traffic resolution; no game job yet, so only the console's `say`, `moveTo` and `spawn` queue intents) → act step (the spawn orders first, step 7a, then the creep intents, and the refused moves back to movement) → the naive guard (ADR-022; every tier) → the persistence flush (Standard) → fault log → profiler record. A build with the `profiler` feature (step 9) reads the clock around these steps; without it the profiler compiles out. The snapshot
(`rustedscreep-wasm/src/snapshot.rs` filling `rustedscreep_core::snapshot::Snapshot`) makes one pass, in this order:
1. Tick context: `Game.time`, `Game.cpu.limit`, `tickLimit`, `bucket` (4 JS calls), sanitized by `TickContext::from_raw`.
2. `Game.creeps` (own creeps, spawning ones included; V101): 3 calls for the collection (getter, `Object.values`, `length`),
   then per creep 7 (`Array.get`, `id`, the id string copy, `pos`, `__packedPos`, `spawning`, and `fatigue` unless it is spawning,
   P2.1, V160), plus `name` and its copy on the tick the creep's handle is new: its identity (role, home, serial) parsed in core
   (`spawning::CreepName`, P2.3), kept as the handle's static data. An own creep that is not spawning adds 3 (P2.4): `store`,
   `getUsedCapacity("energy")` and `getCapacity("energy")` (`CreepStore`; null reads 0, a creep with no active CARRY part, V196, V197).
3. `Game.rooms`: 3 calls, then per room 8 with a controller (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
   `length`) or 6 without, plus `energyAvailable` and `energyCapacityAvailable` for an owned room (P2.3, V175); per hostile 5 (`get`,
   `id`, the id copy, `pos`, `__packedPos`; hostile results never include spawning creeps, V101). An owned room adds (P2.4) its
   controller's `progress`, `ticksToDowngrade` (undefined before the processor sets the timer: read as the full timer) and `upgradeBlocked` (`ControllerState`, V189) and its room objects
   (`RoomObjects`): 6 `find` calls with their `length`s (sources, own sites, dropped resources, tombstones, ruins, and containers
   through the engine's `{filter: {structureType: "container"}}`, V199), then per object `get`, `id` and the id copy, its position and
   type on the tick it is first seen, and its amount each tick (a source's `energy` and `energyCapacity`, a site's `progress` and
   `progressTotal`, a pile's `amount`, a tombstone's, ruin's or container's `store` and `getUsedCapacity("energy")`).
   The name is parsed from its bytes (`snapshot::parse_room_name`, the crate's `RoomName::from_str` semantics without its str
   slicing, which costs 8139 bytes of release wasm; V114).
4. `Game.structures` (own structures; V101): 3 calls, then per structure 3 (`get`, `id`, the id copy), plus on the tick it is
   first seen `structureType`, and `pos` and `__packedPos` when the type is known (again every tick while that read fails). With
   `cache.structure_static` off (the naive baseline, P1.10) those three are read on every tick. Each spawn adds `spawning` (null when
   idle, V175); a busy spawn's `Spawning` comes back through an `Option` (below; its drop runs in wasm, V100). Each spawn gets a row
   in `Snapshot::spawns` (P2.3). While a room's `energyAvailable` is below its capacity, each of its spawns and extensions adds
   `store` and `getFreeCapacity("energy")` (P2.4, V196); a full room's read as 0 with no call.
Calls back from JS into wasm: one `__wbindgen_malloc` per id or name copy, and one `__externref_table_alloc` (with a JS
`Table.set`) per object returned through an `Option` (every creep's `id`, a room's `controller` when present, a busy spawn's
`Spawning`, P2.3).
5. `finish`: handles of objects the pass did not push are released.
5a. Movement's tick (P2.1, `rustedscreep-wasm/src/movement.rs` over `rustedscreep_core::movement`): `Movement::begin` (no JS
   call) notes the rooms with own creeps and the owned ones, checks last tick's moves (a creep not on the tile it was moved toward
   counts as stuck; an engine failure into a tile no creep stands on counts as unexplained), drops suspect tiles past their time,
   and drops rooms no own creep has been in for `movement.field_idle_ticks`. Since P2.2 it also sees crossings: a creep in another
   room than on the tick before crossed (its move counts as done), and its new room is marked for a read in this tick's first-read
   job when it had a request on the tick before, so it never stands on the edge and goes back (V151); a creep still on an edge tile
   of the room it was in had its transfer refused (V152, V168), so the room beyond is held closed for `movement.route_ttl_ticks`
   and the routes from its room are dropped. Route exits and failures past their age are dropped. The rooms' reads are jobs in step 6. Own creeps'
   `fatigue` is read in step 2 (one call each unless spawning, V160).
6. Scheduler (P1.6, `rustedscreep-wasm/src/scheduler.rs` over `rustedscreep_core::scheduler`): `Scheduler::begin` turns the
   snapshot's tick context into the tick's budget (no JS call, §4). Its jobs so far, in tier order: `movement read` (Critical, first: per room that is owned or had a request within `movement.scan_interval_ticks` and has not been read, the terrain, 5 calls: `getTerrain`, `getRawBuffer`, `length`, js-sys `copy_to`'s `length` and copy, V156, V157; sources and minerals, 2 `find` calls with their `length`s and 3 calls each; and a scan, below; one unit per room, a heavy unit), the persistence loads (Standard, P1.9), `movement rescan` (Standard: the same rooms' scans every `movement.scan_interval_ticks`, or `rescan_min_ticks` after an early-rescan request; a scan is 3 `find` calls, structures, own sites and deposits, V158, with their `length`s, 4 calls per structure plus `my` and `isPublic` per rampart, 4 per site, 3 per deposit, V159; a scan whose footprints equal the last one's rebuilds nothing), the console's move order (`rustedscreep.moveTo`, Standard, P2.1) and the constants self-check (Optional, P1.11). Jobs ask movement for steps with `Movement::request` (P2 jobs from the opening item on); a request builds its goal's flow field on first use and a stuck creep's detour (both heavy units, `Commit::heavy`, after which the slicer reads the clock), and a request that plans a move commits one intent. A job whose creep may make way calls `Movement::give_way`; any other own creep stays put. After every job, `Movement::resolve` runs the traffic manager per room with movers and pushes the moves into the intent buffer; on a tick with a planned move it reads the clock once and caps each mover's shoves by what its job's tier has left. After the act step (step 7), the moves the game refused are not counted as blocked, and one refused with `ERR_NO_BODYPART` holds its creep as fixed for `movement.immobile_retry_ticks`. Ending the tick records it for
   `rustedscreep.scheduler()`, whose per-tick `faults` counts job faults only (act faults come later, step 7, and show in the
   fault totals and lines). A time-sliced job adds one `Game.cpu.getUsed()` per clock sample.
7. Act (P1.7, `rustedscreep-wasm/src/act.rs` over `rustedscreep_core::intents`): during the decide step, jobs push intents by creep
   handle into the `IntentBuffer`, which commits 0.2 CPU per new intent that is not free (V34) and refuses, so that nothing is queued
   and nothing earlier replaced: calls the game API refuses (an actor that is gone, hostile or spawning; a `move(direction)` while the actor is fatigued, P2.1; a target that is gone; a
   spawning creep target for transfer, pull or a move toward it; a target out of range or in another room; a controller or a type
   built without a store for transfer and withdraw, also a nuker or power bank for withdraw; anything but a controller for
   upgradeController; a self-pull), billed no-ops that the processor skips (heal or rangedHeal of a spawning creep, repair of a type
   without a `CONSTRUCTION_COST`, a transfer to or move toward itself), and an amount of 0, which the API reads as "all" (V108, V109,
   V113). Every other refusal reaches JS and comes back as a known code, counted as failed and not billed. After the scheduler's tick
   ends, `issue` plans each creep's last push per method, blockers first (`PRIORITY_ORDER`), and walks the plan: it skips an
   intent once a call on the same creep that blocks it was recorded (OK or unknown, V44, V45), so a blocker the game refused blocks
   nothing and, when every call is accepted, it calls `ActionSet::kept`. Each call is one call of a `Creep` method on this tick's JS
   object (`JsWorld`, row for row): one JS call per issued intent, none when the buffer is empty, plus a string decode
   (`getStringFromWasm0`) per `say`. The methods are bound locally with `catch` and an `f64` return (V111, V112): the return value is
   classified (OK, a known `ErrorCode`, or unknown), and a throw is a fault under job `act` (unit: the actor's handle slot),
   rate-limited with the scheduler's, after which the next issue runs. `rustedscreep.act()` prints the last tick's counts and the
   totals since boot. The buffer issues only a creep's last push of a method, with no fallback to an earlier push if that call fails.
7a. Spawn orders (P2.3, `rustedscreep-wasm/src/spawn.rs` over `rustedscreep_core::spawning`), in the act step before the creep
   intents: the processor takes a user's objects in the order of each one's first recorded intent (V179), so the spawn is charged
   before any of the bot's own creeps acts (another user's intents may still come first). One `spawnCreep(body, name)` per order on
   this tick's JS spawn, the body one slice of part indices that the glue maps to strings and frees with a `__wbindgen_free` callback
   (V178), bound locally with `catch` and an `f64` return (the crate's binding unwraps the code and converts each part with its own
   call, V177). Each outcome goes to the spawner (anything but OK holds the spawn for the next `spawn.hold_ticks` ticks, then
   probation) and to the act counters (`ActLog::record_structure`, billed on OK, V34); a throw is a fault under job `spawn act` and a
   warning with the JS error's message; an OK logs one info line ("accepted": the processor may still drop the order, V173; the
   console's request stands again when no creep of the order's identity is in the next snapshot). A successful call makes the
   engine read `Memory` and write `Memory.creeps[name]` (V181); the step then deletes that entry, 8 JS calls, so the end-of-tick
   save never stores it (ADR-018, V180). With a raw `Memory` that does not parse every such call throws instead (V181): the step
   resets the raw string to `{}` with `RawMemory.set` (kept by the end-of-tick save, V180), warns, notifies once per global,
   releases the spawn so it tries again next tick, and calls none of the tick's other orders (ADR-019). These are the tick path's
   only `Memory` writes; it never reads `Memory` for state. No orders, no call.
8. The persistence output step (P1.9; while the tick runs the Standard tier): dirty records are written (the config record after a
   console change, P1.10) and the active set is issued when the manager asks; see section 5. Then the fault reports queued this tick
   (jobs' and the act step's) go out through `log::warn!` (`logging.rs`: `log` macros and a minimal logger that prints with
   `console.log`; ADR-014, V127, V128).
9. Profiler (P1.8, `rustedscreep-wasm/src/profiler.rs` over `rustedscreep_core::profiler`), only with the `profiler` feature: two
   `Game.cpu.getUsed()` readings back to back at the start of `loop`, then one each after the snapshot, the decide step, the act step
   and the fault log (6 per loop). `Profiler::record` adds the spans to a 1000-loop window (no JS call). On a global's first loop, one
   `console.log` (the cold loop). On a window's last loop, the statistics, the stats segment text and its write: the
   `RawMemory.segments` getter, an `is_object` check, two glue casts and one `Reflect.set`, all behind `catch`, plus one reading and,
   if the write failed, a `console.log` (V115). Without the feature: no call.
Each id or name copy is its own call (`__wbindgen_string_get`) because that call is the type check (V100). No Memory access, and intents only
from the act step; on a steady tick the only Rust allocations are the copied id and name strings (the tables keep their capacity). This tick's JS objects stay in `JsWorld`, row
for row, for the act layer (module 7). A failed read skips that object and counts it; `rustedscreep.snapshot()` prints the tick
context, each room with its controller and creep and structure counts, table sizes, handle churn and skips.

## 2. Crates and modules
_P1._ rustedscreep-consts, rustedscreep-core (scheduler, planner, economy, spawning, logistics, defense, offense, expansion, movement,
market, profiling, util), rustedscreep-wasm (snapshot, act, persistence, profiler, glue), and js/ (loader and tools).

Current state (P1.1): a virtual Cargo workspace with `crates/rustedscreep-consts`, `crates/rustedscreep-core` and `crates/rustedscreep-wasm`
(a `cdylib`). `default-members` excludes rustedscreep-wasm, so a bare `cargo test` or `cargo clippy` in the root covers only the two
native crates. The release profile and the toolchain pin live only in the root (`Cargo.toml`, `rust-toolchain.toml`). Each crate root
denies the panicking clippy lints outside tests.

rustedscreep-consts (P1.2) re-exports the `screeps::constants` items at its root, minus the JS-carrying `find` and `look` modules and
`FindConstant`/`LookConstant` traits (hidden by private shadows), plus the pure `screeps::local` types by name, and holds values the
API does not export in `extra` (never glob-re-exported). Its crate docs list the JS-backed, panicking and overflowing APIs that
rustedscreep-core must avoid.

Snapshot (P1.5): `rustedscreep_core::snapshot` holds `TickContext`, the generational `Interner` (packed object id → `Handle`),
`ObjectKey::from_hex`, and the struct-of-arrays tables `Rooms`, `Creeps` and `Structures` behind `Snapshot`, all pure and tested
natively. rustedscreep-wasm's `snapshot.rs` reads the game into it and keeps the state in a `thread_local` that a global reset
or a re-instantiation starts empty. rustedscreep-core now depends on rustc-hash (ADR-006); rustedscreep-wasm on screeps-game-api,
js-sys and rustedscreep-core.

Config (P1.10): `rustedscreep_core::config` holds every threshold struct, `Config` (one of each), the profile table `PROFILES`
(`mmo` only; PS1 and SE2 add rows), the field table `FIELDS` (runtime key, safe range, whether a runtime override may change it,
getter and setter) and the cross-field `RULES`; `config::overrides` parses, clamps and checks overrides, encodes and decodes their
record, and `Store` holds the profile's values, the overrides and the config in force. rustedscreep-wasm's `config.rs` selects the
active profile (`PROFILE`) and answers the `rustedscreep.config*` console commands; `logging.rs` installs the logger once per
instance, from the start function, and sets the level. rustedscreep-wasm depends on log 0.4.35 (ADR-006, no features); fern was dropped for its own minimal logger (ADR-014).

Act layer (P1.7): `rustedscreep_core::intents` holds the engine's creep intent-priority table as data (`CreepAction`, `ActionSet`,
`ActionSet::executed` as the engine model, `PRIORITY_ORDER` and `ActionSet::kept` for what is issued, with the engine commit), the
per-tick `IntentBuffer` with its `CreepIntent` variants (move by direction, move toward a creep, say, drop, transfer, withdraw, heal,
rangedHeal, repair, upgradeController, pull; harvest, build, pickup and withdraw from a tombstone, ruin or container came with the
room-object table in P2.4, attacks come with the ally check in P7 and P10; `spawnCreep` is a structure call, P2.3) and its gated
`issue` walk, the return-code classifier `Outcome`, and the counters `ActLog`. rustedscreep-wasm's `act.rs` binds the fourteen
methods and makes the calls; `act_test.rs` (feature
`act-test`, refused by `npm run stage` for deploy/) is a scripted producer for the sandbox test.

Movement (P2.1): `rustedscreep_core::movement` is pure. `Tile` (column-major, the CostMatrix layout, V86), `TerrainGrid` (from
the row-major raw buffer, V156), `CostGrid` (terrain costs from `MovementConfig`'s profile, roads, blocking footprints, edge tiles
blocked because a creep that ends a tick there changes rooms, V151, V152; portals blocked, V153), `FlowField` (Dijkstra from the
goal tiles with Dial's 256-bucket queue) and `FieldCache` (by room, goal and grid version; LRU and idle eviction), `PathSearch`
(A* detours around creeps), `Traffic` (an augmenting-path matching: movers in priority order claim their best free step or push a
chain of creeps on, at most `shove_depth` deep; holding creeps are shoved to a free tile beside the mover, swapped, or pushed on,
within their anchor; shoves capped per mover, a claim budget per mover search) and `Movement`, which ties them together per tick:
requests (the field's steps, or a detour after `stuck_ticks`, with backoff, skipped when no goal tile is free, dropped once the
tile that blocked it is free), creeps with no request pinned unless their job calls `give_way`, suspect tiles learned from failed
moves into tiles no creep stands on (what the snapshot cannot see, such as power creeps, V52; they are not read yet, P7 and P9),
and creeps whose `move(direction)` the game refused with `ERR_NO_BODYPART` held as fixed. Inter-room routes (P2.2, `movement::route`): a request whose target
is in another room follows a route kept whole: the path's room visits in order, each with the edge tile it is left by, and the grid version each search used
(versions are unique within a global). The creep follows its own route leg by leg (`Movement::begin` moves it on to the next leg when it lands in that leg's
room), else joins a kept route at the leg in its room with the shortest way to its exit in the room's grid, else the installed `RouteSearch` searches once, inside the
request (a heavy unit). Exits found by different searches never mix (kept per room, they sent creeps round between two rooms), and a room a path passes through
twice is told apart by the leg. Following a leg is an `Aim::Exit` flow field onto its exit tile (the grid keeps which edge tiles are open exits; it never makes
them walkable), and the traffic manager lets a mover onto an exit only when that is its own step, never shoves a creep onto one; a creep on an edge tile (it goes
back over the edge if it stays) is offered every way in, best first. A creep that cannot reach its leg's exit searches from where it stands at once, dropping the
route for every creep when the room's grid changed since the search; one that cannot reach its target inside the target's room takes or searches a route out and
back in, unless the target's field reaches no exit (then no route can help and no search runs); a target in a closed room is not searched for. A refused transfer drops every route into the closed room; a request that finds no way just after a crossing drops the route that led there, and a creep
left on an edge tile steps into the room. A search that found nothing is kept by that creep for `route_retry_ticks`, never shared. Edge tiles a path stepped onto
and back into the room from (PathFinder allows it, V166; no creep can, V151) are learned when they cost a creep its way and blocked in every later search's
matrix for `route_ttl_ticks` (only those of the creep's own room, from the search just made). Open exits go to PathFinder at their own terrain cost. rustedscreep-wasm's `route.rs` is the `RouteSearch`: `PathFinder.search` bound with `catch`
(native PathFinder throws on a room without terrain, V165), the cost profile's plain and swamp costs, `route_max_ops` and `route_max_rooms`, and one `'static`
room callback per global over a shared state: movement's grids as PathFinder bytes (copied when a grid's version changes; the JS CostMatrix made on first use),
`false` for closed rooms and rooms whose `getRoomStatus` differs from the origin's (V168), and a shared matrix that leaves the terrain to PathFinder but blocks the
corners for any other room. Without a `RouteSearch` (native tests) a target in another room is refused, as in P2.1. rustedscreep-wasm's `movement.rs` runs the read and rescan jobs, the console order and `resolve`, and feeds the act
step's refused moves back (`act.rs` collects them); the intent buffer refuses a `move(direction)` while the creep is fatigued
(`Rejected::Tired`, V108, V160). The scheduler's `Commit::heavy` lets a unit end its batch so the slicer reads the clock right
after it.

Spawn path (P2.3; the P5 global queue replaces the planner): `rustedscreep_core::spawning` is pure. `Body` is 1 to `MAX_CREEP_SIZE`
known parts with its cost (`BODYPART_COST`) and spawn time (`CREEP_SPAWN_TIME` per part, V170); `Body::scaled` is the largest body of
whole repeats of a pattern within an energy amount, a repeat cap and the size cap (the P3 body solver replaces it). `CreepName`
encodes and parses a creep's identity (`Role`, home room, serial from `Game.time` and the order's index in its tick, so names never
repeat, V171, V176): the bot's only creep state, read once per creep (§3). `Spawner` takes the tick's `SpawnRequest`s (home, role,
pattern, repeat cap, `EnergyBudget::Capacity` to wait for the room's capacity, `Available` to spawn what the room holds now, or `UpTo(cap)`
to wait for the largest body within a cap (P2.4's wait fallback), and a priority); `close` keeps the rooms with a request and a ready spawn as the Critical `spawn` job's units; `plan_room` gives the room's
ready spawns, in id order with those on probation last, its requests in priority order, keeping the energy left over the orders because the runtime checks
each call against the same tick-start `energyAvailable` and the processor silently drops a second order the first one starved
(V181, V173); a request that must wait for energy stops the room, so nothing below it spends the energy; one with no body that
fits the room's capacity (a repeat above it, or no repeat allowed) is skipped whatever its budget, so it never blocks the room. `record` holds a spawn after a refused call, then keeps it on probation until a call of its own is accepted. The snapshot gained `RoomEnergy` per room, `Spawns` (handle,
structure row, busy) and the creep identity column; `ActLog::record_structure` counts a structure's call. rustedscreep-wasm's
`spawn.rs` binds `spawnCreep`, runs the job, issues the orders (§1 step 7a) and answers `rustedscreep.spawner()` and
`rustedscreep.spawn(room, role)` (a request repeated each tick until a spawn's call for it returns OK, and again if its creep does
not appear on the next tick).

RCL1–3 opening (P2.4; the P3 economy replaces it): `rustedscreep_core::economy` is pure. `sizing` gives a worker's round between a
source and the controller (harvest a load at `HARVEST_POWER` per WORK, walk it loaded and back with the fatigue rules, V54, V55, upgrade it
at `UPGRADE_CONTROLLER_POWER` per WORK) and the workers a source keeps busy: enough to carry away what it yields over a round
(`SOURCE_ENERGY_CAPACITY` per `ENERGY_REGEN_TIME`), no more than its open tiles can harvest at once. `Opening` keeps one plan per owned
room (the sources' open tiles, trips and counts, from a flow field from the controller on movement's grid, built again when the grid
version or the room's capacity changes), and per worker (own creeps whose name says `worker` and the room) a mode, a source and a
task, in the heap. Each tick, in id order with reservations: gatherers take the nearest pile, tombstone, ruin or container with
`opening.scavenge_min` energy that is no farther than their source, else harvest; workers with energy fill spawns and extensions until
the free space is covered, then the downgrade guard's upgrader, then `opening.min_upgraders`, then build the room's best site, then
upgrade (the guard's upgrader keeps upgrading until it is empty). A target movement finds no way to goes on the room's unreachable
list for `movement.route_retry_ticks` (at most 16): skipped by the worker that failed, and by every worker once a second one fails on
it, so one walled-in worker cannot close the room; the worker's task is dropped at once. A harvest or a build is pushed only while
this tick's earlier pushes (a lower bound each: `HARVEST_POWER`, or `BUILD_POWER` capped by the builder's energy) leave the target
something. Spawn requests
keep the room at the plan's count within `opening.min_workers..=max_workers`, at the energy the room holds below `min_workers`, capped
at the most energy it held after `opening.wait_ticks` without a new worker (`EnergyBudget::UpTo`, until the room is seen full), else at
its capacity. `Watchdog` per room reports a recovery (no worker can work while the plan wants some), the downgrade guard (with
hysteresis), the wait fallback and a stall (no controller progress, harvest or construction for `opening.stall_ticks`; the room is
planned again). New intents: `harvest`, `build`, `pickup`, and `withdraw` from a room object
(`CreepIntent::{Harvest, Build, Pickup, WithdrawObject}`, `Target::Object`). rustedscreep-wasm's `opening.rs` runs the two jobs, asks
movement for an early rescan when a room's own sites change in number, logs the watchdogs' events (a recovery and a stall also notify,
once per kind per global) and answers `rustedscreep.opening()`. Interim extension placement (ADR-020, `economy::placement`): while an
owned room with a spawn has fewer extensions and extension sites than its level allows, the Optional `opening placement` job picks one
tile (the first spawn's checkerboard parity at range 2 or more, nearest by path, never next to a source or a mineral, within 3 of the
controller or within 3 of the edge, and only where the free neighbours stay connected and one can fill it) and the act step calls
`Room.createConstructionSite(x, y, "extension")` after the spawn orders (billed, V203; one per room per tick, V204). A tile the game
refuses (`ERR_INVALID_TARGET`) is skipped for the global; a pick not seen on its tile within `APPEAR_TICKS`, or refused for the room
(`ERR_RCL_NOT_ENOUGH`, `ERR_FULL`), counts toward `LOST_MAX` (3 in a row stop the room's placement for the global); a pick that finds
no tile waits until the grid, the count, the level or the refused tiles change. `opening.place_extensions` turns it off.

Constants self-check (P1.11): `rustedscreep_core::consts_check` is pure. A `Globals` source gives the server's names, then one
value's JSON text per step; `Check::step` reads them all, checks that at least one anchor constant (`OK`, `WORK`, `CREEP_LIFE_TIME`) was seen (otherwise the global holds no game constants and the check fails),
then compares 15 groups (scalars, enum families by prefix, sets, tables entry by entry) with `rustedscreep_consts`, through a strict
JSON reader with exact numbers on Clinger's fast path (no float parser in the wasm). `KNOWN` names each known difference with its
reason; findings are capped (`MAX_FINDINGS`). rustedscreep-wasm's `consts_check.rs` calls `Object.getOwnPropertyNames(globalThis)` once
and reads each upper-case name's value through its property descriptor and `JSON.stringify` (local `catch` bindings; no getter
runs). A value with no JSON text is a finding (unreadable); a throw from the descriptor read fails the check. It is time-sliced as an Optional step job under that tier's cap, once per global. It logs one completion line
(info, or a warning with up to `log.consts_findings` finding lines); `rustedscreep.consts()` shows the phase, findings and known
differences. The native test runs it against engine common 2.16.0 (`tests/fixtures/constants-common-2.16.0.tsv`).

js/ (P1.3, P1.4): `js/src/main.ts` wires the loader (`js/src/loader.ts`) to the game's `loop` and the `rustedscreep.*` console
commands. The loader boots once the bucket reaches `LoaderConfig::boot_bucket_min` (480, set at the P1 gate from the MMO cold start,
VERIFICATION_LOG V150). On the first boot tick it requires the wasm, which arrives through the GitHub link as a base64 text module
(`rustedscreep_wasm.js`, decoded in plain JS by `js/src/base64.ts`, ADR-015) or as a binary module through the API, checks its
CRC-32 against the build's, logs the boot line and compiles it; on the next tick (`boot_split_ticks`) it instantiates and runs the
first loop. Recovery is in section 6. `js/tools/` holds the build
(cargo → wasm-bindgen `--target web` → `wasm-opt -O4` → tsc → Rollup CJS into `dist/`), stage (into `deploy/` for the GitHub link),
size, check, smoke (the bundle in a Screeps-like sandbox, run at the end of every build) and setup-binaryen. Generated files go to the git-ignored `js/pkg/`,
`js/build/` and `dist/`.

## 3. Cache and invalidation table
_P1 onward._

| Cache | Key | Contents | Built in tier | Invalidated by | TTL | Approx. size | Toggle (naive mode) |
|---|---|---|---|---|---|---|---|
| Object handles, creeps and structures (P1.5) | packed object id (`ObjectKey`, u128) | per slot: generation, last pass seen, row, live flag; FxHashMap id → slot | Critical (snapshot, every tick) | the first pass that does not push the object: the slot is released and its generation advances, so old handles stop resolving; free slots are reused oldest first; a slot whose generation would wrap is retired | none (pass-based) | about 48 B per slot plus a map entry | none: handles are the identity scheme, not an optimization |
| Structure static data (P1.5) | structure handle | `structureType` and position | Critical (snapshot), read only when a structure is first seen | the structure's handle being released (destroyed, or missing from one pass) | none | 8 B per slot | `cache.structure_static` (P1.10): off, every push reads both again (`Interner::mark_fresh`) and keeps the handle; a failed re-read skips the structure for that tick and releases its handle |
| Config in force (P1.10) | one per global | the profile's values, the overrides, the config they give, the record's state | at the global's start (profile values), then on a console change or when the record loads (Standard, a tick or two after the start) | `configSet`, `configUnset`, `configReset`, the record loading, and the naive guard (ADR-022: once per loop, it removes the overrides that turn a cache off after `cache.naive_max_ticks` or below `cache.naive_stop_bucket`); a global reset starts again from the profile | none | under 1 KB | none: it is the switchboard for the others |
| Job state (P1.6) | one per job, owned by its subsystem | round-robin cursor, measured CPU per unit | the job's tier | nothing during a global; a global reset restarts it at unit 0 with no estimate (only costs extra clock samples on that tick) | none | 16 B per job | none |
| Sleep wheel (P1.6) | item (a handle or a job) | wake tick and an optional event | the owner's tier | the wake tick passing (gaps included), its event firing, a new sleep or a cancel; time going back wakes everyone; a global reset empties it, so every item starts awake | none | about 28 B per entry, 256 slot vectors, a heap for wakes beyond 256 ticks, one list per event being waited for (emptied lists are reused, at most 64 kept) | none |
| Intent buffer (P1.7) | creep row in this tick's snapshot | every accepted push of the tick (actor and target rows, the intent), the methods queued per row, the latest push per row and method, and the planned issues | the decide step (any tier) and the act step | `IntentBuffer::begin` each tick, after the snapshot; a push before `begin`, after the plan, or with a snapshot whose `Game.time` or creep count differs from the one `begin` saw is refused (`closed`) | one tick | 44 B per push and per issue (`Issue`, measured natively; it holds no pointers), 4 B per row, plus 92 B per row for the latest-push slots, which only grow (the rows' method sets say which are this tick's) | none: it holds one tick's intents |
| Room terrain, sources and minerals (P2.1) | room name | the terrain masks, column-major (2500 B), and the tiles of sources and minerals | Critical (`movement read`), the first tick an owned room has own creeps or a request finds a room not read | the room evicted after `movement.field_idle_ticks` with no own creep in it; a global reset | none | about 2.5 KB per room | `cache.movement_grid`: off, read every tick for every room it would read |
| Room objects and cost grid (P2.1) | room name | the last scan's footprints (structures, own sites, deposits; sorted), the grid (2500 B, allocated on the first build) and its version | Critical (the first scan) and Standard (`movement rescan`); the grid on the first request after a change | a rescan every `movement.scan_interval_ticks` while the room has own creeps and is owned or had a request within that interval, sooner (`rescan_min_ticks`) after repeated failed moves into a tile with no creep on it or `Movement::invalidate`; the grid is rebuilt only when the footprints, the terrain, the statics or the cost profile changed, which bumps its version; the room evicted as above | the scan interval | about 2.5 KB plus 4 B per object per room | `cache.movement_grid`: off, scanned and rebuilt every tick |
| Suspect tiles (P2.1) | room and tile | tiles moves kept failing into with no creep on them (something the snapshot cannot see) | the requesting job's tier | `movement.suspect_ticks` after they were learned; the room evicted | `suspect_ticks` | 8 B per tile | none: it is learned state, not a cache |
| Flow fields (P2.1) | room, goal tile and range | distances to the goal (2500 `u16`) and the grid version they were built on | the requesting job's tier, on first use | a newer grid version (rebuilt in place on next use), `movement.field_idle_ticks` without use, or LRU eviction beyond `movement.field_cache_size` | `field_idle_ticks` | 5 KB per field | `cache.flow_fields`: off, every request builds its field again |
| Creep movement state (P2.1) | creep handle slot (with the handle) | goal (a goal near a target, or an exit on a route; P2.2), the room it was in at the last `begin` and the tick of its last request (P2.2: crossings, refused transfers, the same-tick read), the room it came from and when, the route it follows and its leg, a failed search of its own (kept for `route_retry_ticks`), the tile it was moved toward, stuck and unexplained counts, the blocked tile, failed detours in a row (the backoff), an A* detour and the tile it went round, the tick it stays fixed until (a move refused with `ERR_NO_BODYPART`) | each request, resolve and the act step's refusals | a new goal (detour dropped), a detour step off its path or its blocker freed while the creep is within 2 tiles of it, the slot reused by another creep; a global reset | none | about 72 B plus the detour per slot | none: it is the creep's progress, not a cache |
| Routes (P2.2) | a slot with a generation (`RouteId`); found by room, target and range | a search's room visits in order, each with the exit it is left by and the grid version the search used; the tick it was made and last used | the requesting job's tier (a search is a heavy unit in the request) | `movement.route_ttl_ticks` after it was made; a creep that cannot reach its exit in a room whose grid changed since the search; a refused transfer into a room on it; a request that found no way just after following it; LRU eviction beyond `movement.route_cache_size` of a route no creep used this tick or the one before (the book grows past the size while they travel); a global reset | `route_ttl_ticks` | under 200 B per route | none: a route search costs up to `route_max_ops` PathFinder ops, so it is never repeated per tick |
| Avoided edge tiles (P2.2) | room and tile | edge tiles a path stepped onto and back into the room from, learned when that cost a creep its way | the requesting job's tier | `movement.route_ttl_ticks` after they were learned; a global reset | `route_ttl_ticks` | 12 B per tile | none: it is learned state |
| Closed rooms (P2.2) | room name | rooms an own creep could not cross into (a refused transfer), and until when | the snapshot step (`Movement::begin`) | `movement.route_ttl_ticks` after the last refusal; a global reset | `route_ttl_ticks` | 8 B per room | none: it is learned state |
| PathFinder matrices and room statuses (P2.2, rustedscreep-wasm route.rs) | room name | each grid as PathFinder bytes (2500 B, avoided edge tiles at 255; a room with avoided tiles and no grid gets the unknown-room matrix with them) with its version and a fingerprint of its avoided tiles and the JS CostMatrix made from it; `Game.map.getRoomStatus` per room and the tick read; one shared CostMatrix for rooms without a grid | in a route search (the requesting job's tier; a target room that is closed or of another status fails before PathFinder runs) | a grid's new version or a change in its avoided tiles (copied again, the CostMatrix remade on use), the room leaving movement's rooms; a status `route_ttl_ticks` old (read again); at most `route_cache_size` statuses (the oldest out); a global reset | statuses: `route_ttl_ticks` | about 2.5 KB of wasm memory and 2.5 KB of JS memory per room with a grid | none |
| Creep identity (P2.3) | creep handle (the interner's static data) | role, home room and serial parsed from an own creep's name (`spawning::CreepName`), or none | Critical (snapshot), read only when the handle is new | the handle being released (the creep died or left `Game.creeps`); a global reset reads every name once | none | 24 B per slot | none |
| Opening plans (P2.4) | owned room name | per source its place, open tiles, trip to the controller and worker count; the room's count | Critical (the `opening` job), a flow field per build (heavy) | a new grid version (movement rebuilt the room's grid) or a new room capacity; a stall; a global reset | none | about 64 B per room plus 40 B per source | the plan's count only sets how many workers are asked for; the watchdogs catch a room that stops |
| Worker state (P2.4) | creep handle slot (with the handle) | mode (gather or work), source, this tick's task, a target found unreachable and until when | Critical (the `opening` job) | the handle changing (the creep died); a stall drops the room's; a global reset (rebuilt from the stores: a worker with energy works) | `movement.route_retry_ticks` for an unreachable target | about 64 B per worker | tasks are chosen again every tick from the snapshot |
| Spawn holds (P2.3) | spawn handle | the tick from which a spawn whose call the game refused may take an order again; after it, the spawn is on probation (it takes orders after the room's other ready spawns) | the act step | an accepted call of its own (ends the probation), the spawn's handle being released, or a global reset (fails open: one refused call) | held for `hold_ticks`, then probation until an accepted call | 12 B per refused spawn | none: it is learned state |
| Fault log (P1.6) | one per global | fault totals, the current window's counts, up to `fault_reports_per_window` queued reports, up to 4 names of jobs whose faults were held back, the last fault | every tick (scheduler) | its window closing `fault_window_ticks` ticks after it opened, or `Game.time` falling before its start; a global reset clears it | the window | under 1 KB | none |

## 4. CPU tiers and budget
_P1._ Tier membership, the budget formula (with its sources), time slicing, and bucket policy (target, spend surplus, safety margin, degradation).

Current state (P1.6, `rustedscreep_core::scheduler`; values from `SCHEDULER_MMO`, rationale in TUNING.md):
- Tiers: Critical (spawning, defense, harvesting), Standard (hauling, upgrading, building, offense operations), Optional (planning,
  market, scouting, visuals, pixels, offense planning). A job declares its tier as a constant `Job { name, tier }`. A tick runs every
  subsystem's Critical jobs before any Standard job, and those before any Optional job: the tiers share one ceiling, so this order
  across subsystems is what gives Critical its priority, and `TickStats::out_of_order` counts breaches. The jobs so far are movement's room reads
  (Critical) and rescans (Standard), the persistence loads (Standard), the console's move order (Standard) and the constants
  self-check (Optional); the rest of P2 adds the first game jobs. A unit that does heavy work (a flow field, a detour, a room
  read) marks itself (`Commit::heavy`): the slicer reads the clock right after it, leaves that batch out of the cost estimate and
  makes the next batch one unit, so one heavy unit cannot carry a batch of cheap ones past the cap.
- Budget (`Budget::compute`, once per tick from the snapshot's tick context, no JS call). Every cap is a level of the whole tick's CPU
  (the latest `getUsed` reading plus committed CPU), and a tier runs while the tick is below its cap:
  - ceiling = tickLimit × (100 − `tick_reserve_pct`) / 100, 450 at tickLimit 500. Every tier stops there, Critical included
    (ADR-013): the engine kills a tick by wall time at tickLimit + 5 ms (V105) while `getUsed` counts CPU time and intents
    (V106). A killed tick is an aborted tick that the loader recovers from with a global reset (ADR-003), and while the bucket is
    below 500 − limit, tickLimit is bucket + limit, so a tick that runs past it without being killed is billed into a negative
    bucket and the next tick is skipped (V37).
  - Standard: on while bucket ≥ `standard_bucket_min` (2000; at least the loader's 480 boot gate plus `CPU_TICK_LIMIT_MAX`), capped at the ceiling.
  - Optional: on while bucket ≥ `optional_bucket_min` (3000) and the global has run `optional_warmup_loops` (1) loops, so the first
    tick after a reset never runs it. Cap = min(ceiling, limit + (bucket − `bucket_target`) / `surplus_spread_ticks`), off when not
    above 0.
- Bucket policy, from the refill rule bucket' = min(10000, bucket + limit − billed) (V37): at the target (9000) the Optional cap is
  the limit, so Optional work uses only what the tick would otherwise leave unused; below the target the cap shrinks and the bucket
  refills; above it the surplus is spent at a rate that falls with it (about `surplus_spread_ticks` ticks). The cap is checked
  during the decide step, so the work after it, the slicer's overshoot and ceil billing (V106) come on top, and under heavy
  Optional demand the bucket settles slightly below the target. At the free tier's limit of 20: below 2000 only Critical runs; up
  to 7000 Critical and Standard; above that Optional too, with a cap of 20 at 9000 and 30 at 10000. Standard's gate is at least the
  loader's boot gate plus `CPU_TICK_LIMIT_MAX`, so one Standard tick cannot take the bucket below the level a reset needs to boot.
- Time slicing (`TickCpu`): `run_units` runs a job's units round robin from a saved cursor (units must come in a stable order,
  by handle or id, V102); `run_steps` runs resumable steps of heavy work. The clock is read when a job starts (unless the tick is
  already at the job's cap, which needs no read) and after each batch. The first batch of every call is one unit, so a cost that
  changed since the last tick is measured first. After that a batch is at most `max_units_per_sample` (16) units, at most half of
  what is left before the cap and at most a quarter of the reserve at the measured cost. So a cost that jumps up to 4 times passes
  the cap by at most half the reserve plus one unit (tested at a full and an empty bucket). Intents a unit queues for the act
  layer are committed at 0.2 CPU each (V34), so they count before `getUsed` sees them; the act layer issues them after the tick's
  `finish`, and the scheduler reads the clock no more (a profiler build reads it twice more, after the act step and the fault log,
  outside the budget).
- Clock readings are sanitized: monotone within the tick (a `cancelOrder` refunds 0.2, V106); a NaN, infinite or negative reading
  keeps the last good one, and after `bad_samples_to_break` (2) such readings every tier stops for the rest of that tick
  (ADR-013).
- Sleep wheel (`SleepWheel<T, E>`): items sleep until a tick or an event, whichever comes first; owned by the subsystem that
  uses it. Wakes are correct across skipped ticks and never fire twice (V107).
- Isolation: each unit returns `Result<(), Fault>`; a fault is counted, queued for the log within a rate limit shared by all
  jobs (`fault_reports_per_window` per `fault_window_ticks`), and the next unit runs. The note that closes a window gives the
  count it held back and names up to 4 of those jobs, so one stuck unit cannot hide a new failure. A failing step stops its step
  job for the tick. rustedscreep-wasm prints the reports with `log::warn!` (P1.10).
- All of it lives in the heap. A global reset fails open: cursors restart, every item is awake, Optional is in warm-up.
- Not yet: per-tier and per-job CPU accounting (P2, once jobs exist: from `TickCpu`'s samples at job start and after each batch, with no
  extra `getUsed` calls, likely as stats segment format v2; P1.8's profiler has one `decide` span over every tier), the first game jobs (P2).
- Every threshold above can be overridden at runtime under its key (`scheduler.<field>`, P1.10; TUNING.md's runtime-key column):
  the value is clamped to its safe range, and a change that breaks a rule (Standard gate ≥ boot gate + `CPU_TICK_LIMIT_MAX`, Standard
  ≤ Optional ≤ target) is refused. A change applies from the next tick.

## 5. Persistence
_P1._ Segment map, format versions, dirty policy, rebuild path.

Current state (P1.10; the manager and the frame are P1.9, ADR-004):

| Segment | Owner | Format | Written | Read |
|---|---|---|---|---|
| 97 (`PersistConfig::config_segment`) | runtime config overrides (`config::overrides`) | frame kind 2, schema 1; payload ASCII `key=value\n` lines in field order | after a console change, on the next tick that runs the Standard tier | once per global: requested at the start, looked up on the first loop, read on the next after the activation |
| 98 (`PersistConfig::self_test_segment`) | the self-test (`rustedscreep.persistTest`) | frame kind 1, a test pattern | on the tick after the command | on the tick after the write |
| 99 (`ProfilerConfig::stats_segment`) | the profiler (`profiler` builds) | JSON, format v1 (`profiler/text.rs`) | once per 1000-loop window, without activation | never by the bot (read in the console, TEST_PLAN §8); the manager reserves it |

- Frame (ADR-004): `rs1b` + base64 of a 16-byte header (kind, schema, write tick, length, CRC-32) and the payload. Every decode
  failure is an error the owner answers by rebuilding: for the config record, the profile's values apply (an empty segment means no
  overrides; anything else that is not a valid record is logged once and its overrides are not used). The record's lines are
  checked again on load like a console change: unknown keys, fields only a profile sets and bad values are dropped with a note,
  numbers are clamped, and overrides that break a rule together are dropped.
- Console changes wait until the record has loaded, so they cannot overwrite a record not yet read. Clearing segment 97 from the
  console (`RawMemory.segments[97] = ""`) leaves the next global with no overrides, without the bot.
- The driver re-saves every segment loaded on a tick (V119), so each load costs a save key on its tick; the manager keeps one key
  free for segment 99 in a `profiler` build.
- Creep names (P2.3) are persisted state outside RawMemory: a creep keeps the name a build gave it for its whole life, across
  global resets and deploys, and it is the bot's only creep state (`rustedscreep_core::spawning` name docs: the role's letter, the
  home room, `-`, the serial in base 36). A role's letter is never changed or reused; a format change keeps parsing the old one
  until no own creep carries it: at least `CREEP_LIFE_TIME` + `MAX_CREEP_SIZE` × `CREEP_SPAWN_TIME` ticks after the last old-form
  order (the life clock starts when the creep leaves the spawn, V170), longer while a spawn's exits are blocked, unbounded once
  creeps are renewed. A name that does not parse gives no identity, never an error. `spawnCreep` itself writes
  `Memory.creeps[name]` (V181), and the act step deletes each such entry in the same tick (ADR-018), so `Memory` does not grow
  with the creeps spawned. A raw `Memory` that does not parse is reset to `{}` when it makes a spawn order throw (ADR-019, V180):
  its content, the loader's record included, is lost (it was unreadable).

## 6. Failure handling
_P1._ Error isolation boundaries, loader recovery (ADR-003), reset behavior, respawn, shard travel, missing Memory.

Config (P1.10): a console change the store cannot use (an unknown key, a field only a profile sets, a bad value, a broken rule, or
a record still loading) is refused with its reason and changes nothing; a number outside its safe range is clamped. A record that
fails to load or decode leaves the profile's values in force and is replaced by the next change. Logging cannot fail the tick: the
logger is installed once per instance (a second install keeps the first), prints with `console.log` only, and never logs itself.

Act step (P1.7): a push that the game API would refuse, or that would be a billed no-op the snapshot can already see, is refused in
Rust and counted; other refusals come back from JS as known codes and are counted as failed. An intent call that throws in JS is
caught by its binding (`catch`), logged as a rate-limited fault under job `act`, and the next issue runs; a throw counts as not
recorded, like the engine's own throw paths (V108). A return value that is not a known code is counted as unknown and treated as
recorded. An issue whose JS object is missing is a fault, counted as missing, and makes no call. None of these traps.

Loader (P1.4, js/src/loader.ts): any throw from the wasm, or a tick that ended inside it (the `running` flag still set), poisons
the instance. The boot tick (require, decode, CRC-32, compile) runs outside that flag, so a boot tick the server kills is not an
incident and is retried whenever the bucket reaches the boot gate; the gate (480) gives that tick the full tickLimit (V150). The loader records the incident in `Memory.rustedscreepLoader` on that tick and recovers on the next: `Game.cpu.halt()`
by default (a global reset and a recompile), or `__wbg_reset_state` (a new instance from the cached module) in reinstantiate mode or
when the server has no halt. Compile and load failures do not halt; they back off. From the third consecutive incident the next
boot waits, doubling up to a cap (the streak is kept in Memory), so a crash loop cannot drain the bucket, and the bot retries
without human action; a capped re-instantiation without halt waits for its window. The loader itself never throws out of `loop`.
Every read of the loader record repairs it (`sanitizeLoaderMemory`), and a `Memory` that is not an object (null after invalid JSON)
falls back to a heap record and one notification.

## 7. Offense subsystem
_P10–P11._ State machines, formations, the breaker solver, budgets, and safety gates.
