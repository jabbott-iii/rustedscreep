# Architecture (SPEC §6.1)

Claude Code fills each section in as its phase lands. Keep it factual: describe only what the code does.

In Screeps a tick is one game turn: the game runs each player's `loop` once, then applies the intents (the actions, such as a move or a harvest, that the code's calls recorded). Creeps are a player's units, and a spawn is the structure that builds them. References in parentheses point to the project's records: ADR-NNN is an architecture decision record (ADR) in `docs/DECISIONS.md`, Vnnn is a row of `docs/VERIFICATION_LOG.md`, and Pn is a phase of `docs/ROADMAP.md`, with Pn.m one of its modules as `docs/STATUS.md` numbers them (PSn and SEn are phases of the optional tracks for a private server and for the Seasonal server).

## 1. Per-tick data flow: snapshot → decide → act
_P1 (build pipeline, loader, snapshot, scheduler, profiler, persistence, and constants self-check)._ A diagram plus a step list: loader (the TypeScript code that starts the WebAssembly (wasm) module) → `loop` → tick context → snapshot (one pass that copies the game state into Rust tables) → caches → scheduler tiers (Critical, Standard, and Optional work, run in that order under a CPU budget) → decisions (the decide step, pure Rust) →
intent buffer (no-op filtering, which drops intents that would change nothing yet still be billed, and the check against the engine's intent pipeline, its rules on which of a creep's intents block others) → act (the act step, which issues the tick's intents in one batch) → persistence flush → profiler flush. Include JavaScript (JS) crossings per step.

Current state (through P5.3, the spawn queue's last module; P1 plus movement, in room and between rooms, the minimal spawn path, the RCL1–3 opening (the generalist workers that get a room through room controller level (RCL) 1 to 3), static miners (creeps that stay by a source, one of a room's energy deposits, and harvest it), the link network, haulers (creeps that carry the energy away), upgraders, builders and repairers, the base planner (P4), and the global spawn queue (P5)): loader → `loop` (rustedscreep-wasm `game_loop`) → a copy of the config in force (`config::Runtime`, the active profile (this server's config values) with the runtime overrides (changes made from the console); the whole tick runs on it, so a record (the overrides as saved in a RawMemory segment, one of the 100 numbered strings the game stores for each player; section 5) that loads during the decide step applies from the next tick, except the log level and the flush's segment limits) → snapshot → movement's tick start (step 5a) → the rate-limited skipped-object log (`SkipLog`, at most one line per `log.skip_window_ticks`) → the intent buffer and the spawner opened for it (the console's spawn request queued; the console is the game's JavaScript prompt, where the player types commands) → scheduler tick (the decide step: movement's room reads, a Critical job (P2.1, movement inside a room; a job is one subsystem's share of the tick's work, split into units the scheduler runs in turn), the opening's plan, a Critical job with one unit per owned room (P2.4, the RCL1–3 opening; below), the spawn plan, a Critical job with one unit per serving room that has a request it plans this tick and a ready spawn (not spawning, not held), none on a tick with nothing asked or with every spawn of each serving room busy or held (a room waiting for energy with a ready spawn is planned every tick) (P2.3, the minimal spawn path); a room serves its own requests, and from P5.1 (a priority queue across spawns and rooms) those of an owned room without a spawn within `spawn.help_range` whose body (the list of parts a creep is built from) it fits, after its own and only on a tick it starts full and the Standard tier runs; then the persistence loads, a Standard step job (P1.9, persistence; a step job runs resumable steps instead of units), the opening's building and upgrading (spending energy on the room's controller to raise its level), a Standard job with one unit per owned room (P2.4), movement's rescans and the console's move order, Standard (P2.1), then the constants self-check, an Optional step job that runs once per global (P1.11, which compares the compiled game constants with the server's; a global is one life of the bot's JavaScript environment and heap, which a global reset, a fresh start of that environment as after a deploy, ends), then movement's traffic resolution (which turns the requested moves into ones that do not collide); and, besides these, the economy jobs that section 2 describes: mining, hauling, and upgrading (Critical), the hauling and upgrading work jobs and the link sends (Standard), and the opening's site placement and the base planner (Optional); those jobs and the console's `say`, `moveTo`, and `spawn` orders queue the intents) → act step (the spawn orders first, step 7a, then the sites (construction sites, which creeps build into structures), the link sends (P3.2, the link network; a link is a structure that sends energy to another link in its room), then the creep intents, and the refused moves back to movement) → the naive guard (ADR-022: it ends a naive-baseline run, the bot with its caches switched off for comparison, after a set number of ticks or when the saved CPU runs low; every tier) → the persistence flush (Standard) → fault log → profiler record. A build with the `profiler` feature (step 9) reads the clock around these steps; without it the profiler compiles out. The snapshot
(`rustedscreep-wasm/src/snapshot.rs` filling `rustedscreep_core::snapshot::Snapshot`) makes one pass, in this order:
1. Tick context: `Game.time`, `Game.cpu.limit` (the CPU the player gets per tick), `tickLimit` (the most this tick may use), `bucket` (the CPU saved from earlier ticks that used less than the limit) (4 JS calls), sanitized by `TickContext::from_raw`.
2. `Game.creeps` (own creeps, spawning ones included; V101, on what the game's global collections hold): 3 calls for the collection (getter, `Object.values`, `length`),
   then per creep 7 (`Array.get`, `id`, the id string copy, `pos`, `__packedPos`, `spawning` (true while a spawn is still building the creep), and `fatigue` (the creep cannot move while it is above 0) unless it is spawning,
   P2.1, V160, on how `fatigue` reads), plus `name` and its copy on the tick the creep's handle (the snapshot's small numeric ID for a game object, valid while the object lives) is new: its identity (role, home, serial) parsed in core
   (`spawning::CreepName`, P2.3), kept as the handle's static data. An own creep that is not spawning adds 3 (P2.4): `store`,
   `getUsedCapacity("energy")`, and `getCapacity("energy")` (`CreepStore`; null reads 0, a creep with no active CARRY part, the body part that holds resources; V196, V197, on the store getters and the capacity CARRY parts give),
   and `ticksToLive` (the ticks left before the creep dies of age) on each tick until it reads a number (P5.2, pre-spawn replacement; V198, V261: it is undefined while the creep spawns and reaches Rust as `None`): `Game.time` plus that number is kept with the handle as
   the creep's death tick (`Creeps::set_death`, a side table by handle slot), so it is read once or twice in a life and once after a
   global reset. In a room that had an open threat on the tick before (P7.1, threat assessment), an own creep that is not spawning adds `hits` and `hitsMax` (P7.2: the towers heal damaged creeps there; the only damage an owned room sees comes from hostiles).
3. `Game.rooms`: 3 calls, then per room 8 with a controller (the structure a player owns a room through; its level, the RCL, unlocks structures) (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
   `length`) or 6 without, plus `energyAvailable` and `energyCapacityAvailable` for an owned room (P2.3, V175: the energy in the room's spawns and extensions, the structures that hold what spawning spends, and their capacity); per hostile 5 (`get`,
   `id`, the id copy, `pos`, `__packedPos`; hostile results never include spawning creeps, V101), and in an owned room `hits` every
   tick and, while no body is known for its handle, its `body` (3N + 2 calls for N parts, V289) (P7.1, threat assessment). An owned room adds (P2.4) its
   controller's `progress`, `ticksToDowngrade` (the ticks until the controller loses a level for lack of upgrading; undefined before the processor, the engine code that applies intents and updates objects after the players' code runs, sets the timer: read as the full timer), and `upgradeBlocked` (`ControllerState`, V189, on the controller's timers and getters) and its room objects
   (`RoomObjects`): 6 `find` calls with their `length`s (sources, own sites, dropped resources, tombstones (left where a creep died, holding what it carried), ruins (left where a structure was destroyed), and containers (small stores that stand on one tile)
   through the engine's `{filter: {structureType: "container"}}`, V199, on the `find` filters), then per object `get`, `id`, and the id copy, its position and
   type on the tick it is first seen, and its amount each tick (a source's `energy` and `energyCapacity`, a site's `progress` and
   `progressTotal`, a pile's (a dropped resource's) `amount`, a tombstone's, ruin's, or container's `store` and `getUsedCapacity("energy")`, and since
   P3.5 (builders and repairers) a container's `hits` (its remaining health), V223, on a container's `hits` and `hitsMax`).
   The name is parsed from its bytes (`snapshot::parse_room_name`, the crate's (screeps-game-api's) `RoomName::from_str` semantics without its str
   slicing, which costs 8139 bytes of release wasm; V114, on that parser and its size).
4. `Game.structures` (own structures; V101): 3 calls, then per structure 3 (`get`, `id`, the id copy), plus on the tick it is
   first seen `structureType`, and `pos` and `__packedPos` when the type is known (again every tick while that read fails). With
   `cache.structure_static` off (the naive baseline, P1.10, the config module with server profiles and runtime overrides) those three are read on every tick. Each spawn adds `spawning` (null when
   idle, V175); a busy spawn's `Spawning` comes back through an `Option` (below; its drop runs in wasm, V100, on how values cross the boundary that wasm-bindgen, the Rust-to-JavaScript binding generator, builds). Each spawn gets a row
   in `Snapshot::spawns` (P2.3). While a room's `energyAvailable` is below its capacity, each of its spawns and extensions adds
   `store` and `getFreeCapacity("energy")` (P2.4, V196); a full room's read as 0 with no call. Each tower adds `store`,
   `getUsedCapacity("energy")`, and `getFreeCapacity("energy")` on every tick (P7.2, the towers: its energy in `Snapshot::towers`,
   its room for more in the same free-energy column the fills read).
Calls back from JS into wasm: one `__wbindgen_malloc` per id or name copy, and one `__externref_table_alloc` (with a JS
`Table.set`) per object returned through an `Option` (every creep's `id`, a room's `controller` when present, a busy spawn's
`Spawning`, P2.3).
5. `finish`: handles of objects the pass did not push are released.
5a. Movement's tick (P2.1, `rustedscreep-wasm/src/movement.rs` over `rustedscreep_core::movement`): `Movement::begin` (no JS
   call) notes the rooms with own creeps and the owned ones, checks last tick's moves (a creep not on the tile it was moved toward
   counts as stuck; an engine failure into a tile no creep stands on counts as unexplained), drops suspect tiles (tiles where moves failed with no creep in the way) past their time,
   and drops rooms no own creep has been in for `movement.field_idle_ticks`. Since P2.2 (inter-room routes) it also sees crossings: a creep in another
   room than on the tick before crossed (its move counts as done), and its new room is marked for a read in this tick's first-read
   job when it had a request on the tick before, so it never stands on the edge (the room's border tiles, where x or y is 0 or 49) and goes back (V151: a creep that ends a tick on an edge tile moves to the next room); a creep still on an edge tile
   of the room it was in had its transfer refused (V152, V168: edge tiles, closed rooms, and room status), so the room beyond is held closed for `movement.route_ttl_ticks`
   and the routes from its room are dropped. Route exits (the edge tiles a route leaves its rooms by) and failures past their age are dropped. The rooms' reads are jobs in step 6. Own creeps'
   `fatigue` is read in step 2 (one call each unless spawning, V160).
6. Scheduler (P1.6, the tiers, the CPU budget, and time slicing; `rustedscreep-wasm/src/scheduler.rs` over `rustedscreep_core::scheduler`): `Scheduler::begin` turns the
   snapshot's tick context into the tick's budget (no JS call, §4). Its jobs so far, in tier order: `movement read` (Critical, first: per room that is owned or had a request within `movement.scan_interval_ticks` and has not been read, the terrain, 5 calls: `getTerrain`, `getRawBuffer`, `length`, js-sys `copy_to`'s `length` and copy, V156, V157, on the terrain API and its JS calls; sources and minerals (a room's deposit of a resource other than energy), 2 `find` calls with their `length`s and 3 calls each; and a scan, below; one unit per room, a heavy unit), the persistence loads (Standard, P1.9), `movement rescan` (Standard: the same rooms' scans every `movement.scan_interval_ticks`, or `rescan_min_ticks` after an early-rescan request; a scan is 3 `find` calls, structures, own sites, and deposits (the game's Deposit objects, read with `FIND_DEPOSITS`: the harvestable resource nodes of highway rooms, which block moves; not the sources or minerals read above), V158 and V162, on what `room.find` returns for them, with their `length`s, 4 calls per structure plus `my` and `isPublic` per rampart (a defensive structure that only its owner's creeps can pass, or anyone's when it is public), 4 per site, 3 per deposit, V159, on the structure properties the scan reads; a scan whose footprints (the tiles of the objects that block moves) equal the last one's rebuilds nothing), the console's move order (`rustedscreep.moveTo`, Standard, P2.1), and the constants self-check (Optional, P1.11). Jobs ask movement for steps with `Movement::request` (the jobs of P2, the bootstrap and movement phase, from the opening item on); a request builds its goal's flow field (every tile's distance to the goal, which a creep follows downhill) on first use and a stuck creep's detour (both heavy units, `Commit::heavy`, after which the slicer, the scheduler's part that runs units in batches between clock readings, reads the clock), and a request that plans a move commits one intent. A job whose creep may make way calls `Movement::give_way`; any other own creep stays put. After every job, `Movement::resolve` runs the traffic manager per room with movers and pushes the moves into the intent buffer; on a tick with a planned move it reads the clock once and caps each mover's shoves (moves of a standing creep out of a mover's way) by what its job's tier has left. After the act step (step 7), the moves the game refused are not counted as blocked, and one refused with `ERR_NO_BODYPART` (the creep has no working MOVE part, the body part that moves it) holds its creep as fixed for `movement.immobile_retry_ticks`. Ending the tick records it for
   `rustedscreep.scheduler()`, whose per-tick `faults` counts job faults only (act faults come later, step 7, and show in the
   fault totals and lines). A time-sliced job adds one `Game.cpu.getUsed()` per clock sample.
7. Act (P1.7, the act layer: the intent buffer, its no-op filtering, and the priority table; `rustedscreep-wasm/src/act.rs` over `rustedscreep_core::intents`): during the decide step, jobs push intents by creep
   handle into the `IntentBuffer`, which commits 0.2 CPU per new intent that is not free (V34, the engine's CPU cost per intent) and refuses, so that nothing is queued
   and nothing earlier replaced: calls the game API refuses (an actor that is gone, hostile, or spawning; a `move(direction)` while the actor is fatigued, P2.1; a target that is gone; a
   spawning creep target for transfer, pull, or a move toward it; a target out of range or in another room; a controller or a type
   built without a store for transfer and withdraw, also a nuker or power bank for withdraw; anything but a controller for
   upgradeController; a self-pull), billed no-ops that the processor skips (heal or rangedHeal of a spawning creep, repair of a type
   without a `CONSTRUCTION_COST`, a transfer to or move toward itself), and an amount of 0, which the API reads as "all" (V108, V109,
   V113: the engine's checks on the game side and the intents the processor bills but skips). Every other refusal reaches JS and comes back as a known code, counted as failed and not billed. After the scheduler's tick
   ends, `issue` plans each creep's last push per method, blockers first (`PRIORITY_ORDER`), and walks the plan: it skips an
   intent once a call on the same creep that blocks it was recorded (OK or unknown, V44, V45: the engine's table of which intents on a creep block which others, and how the processor applies it), so a blocker the game refused blocks
   nothing and, when every call is accepted, it calls `ActionSet::kept`. Each call is one call of a `Creep` method on this tick's JS
   object (`JsWorld`, row for row): one JS call per issued intent, none when the buffer is empty, plus a string decode
   (`getStringFromWasm0`) per `say`. The methods are bound locally with `catch` and an `f64` return (V111, V112: the crate's typed methods panic on a return code they do not list, and a `catch` binding turns a JS throw into an `Err`): the return value is
   classified (OK, a known `ErrorCode`, or unknown), and a throw is a fault under job `act` (unit: the actor's handle slot),
   rate-limited with the scheduler's, after which the next issue runs. `rustedscreep.act()` prints the last tick's counts and the
   totals since boot. The buffer issues only a creep's last push of a method, with no fallback to an earlier push if that call fails.
7a. Spawn orders (P2.3, `rustedscreep-wasm/src/spawn.rs` over `rustedscreep_core::spawning`), in the act step before the creep
   intents: the processor takes a user's objects in the order of each one's first recorded intent (V179, read in the driver and the processor), so the spawn is charged
   before any of the bot's own creeps acts (another user's intents may still come first). One `spawnCreep(body, name)` per order on
   this tick's JS spawn, the body one slice of part indices that the glue (the JavaScript wasm-bindgen generates around the wasm) maps to strings and frees with a `__wbindgen_free` callback
   (V178, on how a `Part` slice crosses), bound locally with `catch` and an `f64` return (the crate's binding unwraps the code and converts each part with its own
   call, V177, on the crate's spawn bindings). Each outcome goes to the spawner (anything but OK holds the spawn for the next `spawn.hold_ticks` ticks, then
   probation, in which it takes orders only after the room's other ready spawns until a call of its own is accepted) and to the act counters (`ActLog::record_structure`, billed on OK, V34); a throw is a fault under job `spawn act` and a
   warning with the JS error's message; an OK logs one info line ("accepted": the processor may still drop the order, V173; the
   console's request stands again when no creep of the order's identity is in the next snapshot). A successful call makes the
   engine read `Memory` (the player's JSON store, which the game parses on first access in a tick and saves at its end) and write `Memory.creeps[name]` (V181, on what a successful `spawnCreep` does); the step then deletes that entry, 8 JS calls, so the end-of-tick
   save never stores it (ADR-018, the decision to delete these entries; V180, on `RawMemory.set` and that save). With a raw `Memory` that does not parse every such call throws instead (V181): the step
   resets the raw string to `{}` with `RawMemory.set` (kept by the end-of-tick save, V180), warns, notifies once per global,
   releases the spawn so it tries again next tick, and calls none of the tick's other orders (ADR-019, the decision to repair an unusable `Memory` this way). These are the tick path's
   only `Memory` writes; it never reads `Memory` for state. No orders, no call.
8. The persistence output step (P1.9; while the tick runs the Standard tier): dirty records are written (the config record after a
   console change, P1.10) and the active set (the segments to load on the next tick) is issued when the manager asks; see section 5. Then the fault reports queued this tick
   (jobs' and the act step's) go out through `log::warn!` (`logging.rs`: `log` macros and a minimal logger that prints with
   `console.log`; ADR-014, which dropped fern for that logger; V127, V128, on the `log` crate's macros and the logger).
9. Profiler (P1.8, sparse CPU spans per tick stage; `rustedscreep-wasm/src/profiler.rs` over `rustedscreep_core::profiler`), only with the `profiler` feature: two
   `Game.cpu.getUsed()` readings back to back at the start of `loop`, then one each after the snapshot, the decide step, the act step,
   and the fault log (6 per loop). `Profiler::record` adds the spans to a 1000-loop window (no JS call). On a global's first loop, one
   `console.log` (the cold loop). On a window's last loop, the statistics, the stats segment text, and its write: the
   `RawMemory.segments` getter, an `is_object` check, two glue casts, and one `Reflect.set`, all behind `catch`, plus one reading and,
   if the write failed, a `console.log` (V115, on the profiler's bindings and the segment write). Without the feature: no call.
Each id or name copy is its own call (`__wbindgen_string_get`) because that call is the type check (V100). No Memory access, and intents only
from the act step; on a steady tick the only Rust allocations are the copied id and name strings (the tables keep their capacity). This tick's JS objects stay in `JsWorld`, row
for row, for the act layer (module 7). A failed read skips that object and counts it; `rustedscreep.snapshot()` prints the tick
context, each room with its controller and creep and structure counts, table sizes, handle churn, and skips.

## 2. Crates and modules
_P1._ rustedscreep-consts, rustedscreep-core (scheduler, planner, economy, spawning, logistics, defense, offense, expansion, movement,
market, profiling, util), rustedscreep-wasm (snapshot, act, persistence, profiler, glue), and js/ (loader and tools).

Current state (P1.1, the Cargo workspace and toolchain pin): a virtual Cargo workspace with `crates/rustedscreep-consts`, `crates/rustedscreep-core`, and `crates/rustedscreep-wasm`
(a `cdylib`). `default-members` excludes rustedscreep-wasm, so a bare `cargo test` or `cargo clippy` in the root covers only the two
native crates. The release profile and the toolchain pin live only in the root (`Cargo.toml`, `rust-toolchain.toml`). Each crate root
denies the panicking clippy lints outside tests.

rustedscreep-consts (P1.2, the constants facade) re-exports the `screeps::constants` items at its root, minus the JS-carrying `find` and `look` modules and
`FindConstant`/`LookConstant` traits (hidden by private shadows), plus the pure `screeps::local` types by name, and holds values the
API does not export in `extra` (never glob-re-exported). Its crate docs list the JS-backed, panicking, and overflowing APIs that
rustedscreep-core must avoid.

Snapshot (P1.5, the one-pass read into Rust tables): `rustedscreep_core::snapshot` holds `TickContext`, the generational `Interner` (packed object id → `Handle`),
`ObjectKey::from_hex`, and the struct-of-arrays tables `Rooms`, `Creeps`, and `Structures` behind `Snapshot`, all pure and tested
natively. rustedscreep-wasm's `snapshot.rs` reads the game into it and keeps the state in a `thread_local` that a global reset
or a re-instantiation starts empty. rustedscreep-core now depends on rustc-hash (ADR-006, the accepted dependency list); rustedscreep-wasm on screeps-game-api,
js-sys, and rustedscreep-core.

Config (P1.10): `rustedscreep_core::config` holds every threshold struct, `Config` (one of each), the profile table `PROFILES`
(`mmo` only; PS1, the private-server setup, and SE2, the Seasonal profile and modules, add rows), the field table `FIELDS` (runtime key, safe range, whether a runtime override may change it,
getter, and setter) and the cross-field `RULES`; `config::overrides` parses, clamps, and checks overrides, encodes and decodes their
record, and `Store` holds the profile's values, the overrides, and the config in force. rustedscreep-wasm's `config.rs` selects the
active profile (`PROFILE`) and answers the `rustedscreep.config*` console commands; `logging.rs` installs the logger once per
instance, from the start function, and sets the level. rustedscreep-wasm depends on log 0.4.35 (ADR-006, no features); fern was dropped for its own minimal logger (ADR-014).

Act layer (P1.7): `rustedscreep_core::intents` holds the engine's creep intent-priority table as data (`CreepAction`, `ActionSet`,
`ActionSet::executed` as the engine model, `PRIORITY_ORDER` and `ActionSet::kept` for what is issued, with the engine commit), the
per-tick `IntentBuffer` with its `CreepIntent` variants (move by direction, move toward a creep, say, drop, transfer, withdraw, heal,
rangedHeal, repair, upgradeController, pull; harvest, build, pickup, and withdraw from a tombstone, ruin, or container came with the
room-object table in P2.4, attacks come with the ally check in P7 (defense) and P10 (the offense core); `spawnCreep` is a structure call, P2.3) and its gated
`issue` walk, the return-code classifier `Outcome`, and the counters `ActLog`. rustedscreep-wasm's `act.rs` binds the fourteen
methods and makes the calls; `act_test.rs` (feature
`act-test`, refused by `npm run stage` for deploy/) is a scripted producer for the sandbox test.

Movement (P2.1): `rustedscreep_core::movement` is pure. `Tile` (column-major, the CostMatrix layout, the 2500-byte cost table that the game's path search, PathFinder, reads; V86, on the pure `screeps::local` types and their layouts, among them `LocalCostMatrix`'s column-major order, `x * 50 + y`), `TerrainGrid` (each tile plain, swamp, or wall; from
the row-major raw buffer, V156), `CostGrid` (terrain costs from `MovementConfig`'s profile, roads, blocking footprints, edge tiles
blocked because a creep that ends a tick there changes rooms, V151, V152; portals, which teleport a creep that steps on them, blocked, V153), `FlowField` (Dijkstra from the
goal tiles with Dial's 256-bucket queue) and `FieldCache` (by room, goal, and grid version; least-recently-used (LRU) and idle eviction), `PathSearch`
(A* detours around creeps), `Traffic` (an augmenting-path matching: movers in priority order claim their best free step or push a
chain of creeps on, at most `shove_depth` deep; holding creeps are shoved to a free tile beside the mover, swapped, or pushed on,
within their anchor, the range of their goal they must stay in; shoves capped per mover, a claim budget per mover search), and `Movement`, which ties them together per tick:
requests (the field's steps, or a detour after `stuck_ticks`, with backoff, skipped when no goal tile is free, dropped once the
tile that blocked it is free), creeps with no request pinned unless their job calls `give_way`, suspect tiles learned from failed
moves into tiles no creep stands on (what the snapshot cannot see, such as power creeps, a late-game kind of unit, V52, on the engine's obstacle and swap rules; they are not read until P7, the defense phase, or P9, the late-game phase),
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
and back into the room from (PathFinder allows it, V166, on its rules at room edges; no creep can, V151) are learned when they cost a creep its way and blocked in every later search's
matrix for `route_ttl_ticks` (only those of the creep's own room, from the search just made). Open exits go to PathFinder at their own terrain cost. rustedscreep-wasm's `route.rs` is the `RouteSearch`: `PathFinder.search` bound with `catch`
(native PathFinder throws on a room without terrain, V165), the cost profile's plain and swamp costs, `route_max_ops` and `route_max_rooms`, and one `'static`
room callback per global over a shared state: movement's grids as PathFinder bytes (copied when a grid's version changes; the JS CostMatrix made on first use),
`false` for closed rooms and rooms whose `getRoomStatus` differs from the origin's (V168), and a shared matrix that leaves the terrain to PathFinder but blocks the
corners for any other room. Without a `RouteSearch` (native tests) a target in another room is refused, as in P2.1. rustedscreep-wasm's `movement.rs` runs the read and rescan jobs, the console order, and `resolve`, and feeds the act
step's refused moves back (`act.rs` collects them); the intent buffer refuses a `move(direction)` while the creep is fatigued
(`Rejected::Tired`, V108, V160). The scheduler's `Commit::heavy` lets a unit end its batch so the slicer reads the clock right
after it.

Spawn path (P2.3; the P5 global queue replaces the planner): `rustedscreep_core::spawning` is pure. `Body` is 1 to `MAX_CREEP_SIZE`
known parts with its cost (`BODYPART_COST`) and spawn time (`CREEP_SPAWN_TIME` per part, V170, on body size, part costs, spawn time, and lifetime); `Body::scaled` is the largest body of
whole repeats of a pattern within an energy amount, a repeat cap, and the size cap (no job asks for one since P3.6, the body solver: every body is solved
from its targets in `economy::sizing` and requested exactly). `CreepName`
encodes and parses a creep's identity (`Role`, home room, serial from `Game.time` and the order's index in its tick, so names never
repeat, V171, V176: the game refuses a spawn under a name in use and kills a second creep that has one): the bot's only creep state, read once per creep (§3). `Spawner` takes the tick's `SpawnRequest`s (home, role,
a body (`BodySpec::Exact`, or a pattern and a repeat cap), `EnergyBudget::Capacity` to wait for the room's capacity, `Available` to spawn what the room holds now, or `UpTo(cap)`
to wait for the largest body within a cap (P2.4's wait fallback), and a priority); `close` keeps the rooms with a request and a ready spawn as the Critical `spawn` job's units; `plan_room` gives the room's
ready spawns, in id order with those on probation last, its requests in priority order, keeping the energy left over the orders because the runtime checks
each call against the same tick-start `energyAvailable` and the processor silently drops a second order the first one starved
(V181, V173); a request that must wait for energy stops the room, so nothing below it spends the energy; one with no body that
fits the room's capacity (a repeat above it, or no repeat allowed) is skipped whatever its budget, so it never blocks the room. `record` holds a spawn after a refused call, then keeps it on probation until a call of its own is accepted. The snapshot gained `RoomEnergy` per room, `Spawns` (handle,
structure row, busy), and the creep identity column; `ActLog::record_structure` counts a structure's call. rustedscreep-wasm's
`spawn.rs` binds `spawnCreep`, runs the job, issues the orders (§1 step 7a), and answers `rustedscreep.spawner()` and
`rustedscreep.spawn(room, role)` (a request repeated each tick until a spawn's call for it returns OK, and again if its creep does
not appear on the next tick).

Pre-spawn replacement (P5.2): `spawning::replace` gives a replacement's lead (`lead_ticks`: the successor body's spawn time, its walk
to the post (the tile the creep works from), and `spawn.replace_margin_ticks`, at most half the body's life, so a slot never holds more than two creeps) and `is_due`
(a creep whose `ticksToLive`, from the snapshot's death tick, is at most the lead). Each job whose creeps hold a post leaves a due
creep out of its count, so it asks for the successor while the creep still works: mining per source (a second slot: the successor walks next to
the source, off the mining tile, and takes the source on the tick the miner is gone; `SourceHarvest` counts the handovers and their
gaps), hauling per source (`loads` counts the haulers not due, for the request and for a new hauler's source), upgrading (a successor
with every post held waits near the best reachable post). The opening's workers are not replaced early (a pool: measured, TUNING).
The miners', haulers', and upgraders' walks are upper bounds on their plans' ways (`sizing::walk_ticks_max`: the way's cost times the
ticks a rate unit takes, whatever its tile count; an empty hauler's is up to twice its tiles on plain), an upgrader's plus an estimated
two plain tiles round the store to its post, so the lead falls short only by where the spawn puts the creep, a swamp tile by the post,
and an order's wait, which the margin covers. Nothing is persisted: a reset reads each death tick again and
gives each due miner its successor back in the same pass.

No wasted energy (P5.3): `Spawner` counts, per tick and since the global started, the spawn-ticks a spawn not spawning took no order
while its room had a request it plans, by cause (`idle_energy`: the room could not pay its next request; `idle_held`: held after a
refused call; `idle_help`: only other rooms' requests on a tick it does not help), and keeps the rooms that waited for energy one tick
(`waits_for_energy`, read by the jobs, which run before the plan). While a room waits and `spawn.refill_from_stores` is on, its
workers fill the spawns and extensions even while haulers fill, and, while the spawns have more room than the haulers hold, an empty
hauler with too little at its own mining tile takes a whole load from a mining tile no hauler takes from or the controller container
instead of waiting (never storing a load from the controller container back; the upgraders see those takes). `rustedscreep.spawner()` prints the
idle spawn-ticks by cause.

RCL1–3 opening (P2.4; the P3 economy replaces it): `rustedscreep_core::economy` is pure. `sizing` gives a worker's round between a
source and the controller (harvest a load at `HARVEST_POWER` per WORK part, the body part that harvests, builds, and upgrades, walk it loaded and back with the fatigue rules, V54, V55, on how moves add fatigue and MOVE parts remove it, upgrade it
at `UPGRADE_CONTROLLER_POWER` per WORK) and the workers a source keeps busy: enough to carry away what it yields over a round
(`SOURCE_ENERGY_CAPACITY` per `ENERGY_REGEN_TIME`), no more than its open tiles can harvest at once. The workers' body (P3.6,
`sizing::worker_plan`) is solved from those targets: of the WORK, CARRY, and MOVE bodies within the energy, the ones whose workers, counted
as fractions, put the most energy to use over a creep life net of their bodies (`worker_value`: each source's yield carried on its rounds;
when the workers' energy is by the controller, a linked source's or the builders' while upgraders take, the time of their duties too:
the build work on the spawn-to-controller trip, the mining containers' repairs, and the fills while no hauler fills; and in any room
the decay of its roads and own ramparts over a life, `sizing::road_decay` and `sizing::rampart_decay`, on repair rounds to them, ADR-032's
amendment), and within `opening.body_slack_pct` of the best the fewest workers. The room keeps at least the whole workers its sources'
fractions and all its repairs take together (P7.3), so the repairs never wait for the time the builds leave; without upgraders a linked source's workers walk their duties empty at a
tile a tick. A request below the plan's capacity (recovery, the wait fallback) is solved at its energy, cached per room. `Opening` keeps one plan per owned
room (the sources' open tiles, trips, and counts, from a flow field from the controller on movement's grid, built again when the grid
version or the room's capacity changes), and per worker (own creeps whose name says `worker` and the room) a mode, a source, and a
task, in the heap. Each tick, in id order with reservations: gatherers (workers collecting energy) take the nearest pile, tombstone, ruin, or container with
`opening.scavenge_min` energy that is no farther than their source, else harvest; workers with energy fill spawns and extensions until
the free space is covered, then the downgrade guard's upgrader (the guard keeps one upgrading while the controller's downgrade timer is below a threshold), then `opening.min_upgraders`, then repair the containers on the mining plan's tiles
(P3.5, `Mining::mine_tiles`; containers elsewhere are never repaired: one worker per container below `repair.start_pct` of
`CONTAINER_HITS`, kept until it is full; last tick's repairer keeps its container while it may still repair it, then each container
left, fewest hits first, takes the nearest worker within 3 tiles, or below `repair.walk_pct` the nearest anywhere;
`CreepIntent::RepairObject`, which the buffer drops at full hits; with them, P7.3 (ADR-032), the roads below `upkeep.road_urgent_pct`
of their `hitsMax` or repaired on the tick before, and the own ramparts below `upkeep.rampart_floor` (or their target, if lower), one
worker each, any worker walking to them, lowest share of its goal first), then build the room's best site, then, in a room with no site, the other repairs of P7.3 (a
road below `upkeep.road_start_pct`, up to full; a rampart below `upkeep.rampart_start_pct` of its target, `upkeep.rampart_permille` of
`rampart_hits_max` at the level, up to it; `economy::Fix` says which, a road by its tile, a rampart by its handle; a rampart started below the floor stays before the sites up to `upkeep.rampart_refill`, ADR-032's amendment; the plan's constructed walls, by tile, are held as ramparts, and a rampart a wall replaces is left to decay, ADR-033), then upgrade (the guard's upgrader keeps upgrading until it is empty). A target movement finds no way to goes on the room's unreachable
list for `movement.route_retry_ticks` (at most 16): skipped by the worker that failed, and by every worker once a second one fails on
it, so one walled-in worker cannot close the room; the worker's task is dropped at once. A harvest or a build is pushed only while
this tick's earlier pushes (a lower bound each: `HARVEST_POWER`, or `BUILD_POWER` capped by the builder's energy) leave the target
something. Spawn requests
keep the room at the plan's count within `opening.min_workers..=max_workers`, at the energy the room holds below `min_workers`, capped
at the most energy it held after `opening.wait_ticks` without a new worker (`EnergyBudget::UpTo`, until the room is seen full), else at
its capacity. `Watchdog` per room reports a recovery (no worker can work while the plan wants some), the downgrade guard (with
hysteresis), the wait fallback, and a stall (no controller progress, harvest, or construction for `opening.stall_ticks`; the room is
planned again). New intents: `harvest`, `build`, `pickup`, and `withdraw` from a room object
(`CreepIntent::{Harvest, Build, Pickup, WithdrawObject}`, `Target::Object`). rustedscreep-wasm's `opening.rs` runs the two jobs, asks
movement for an early rescan when a room's own sites change in number, logs the watchdogs' events (a recovery and a stall also notify,
once per kind per global), and answers `rustedscreep.opening()`. Interim extension placement (ADR-020, which places extension sites until the base planner exists; `economy::placement`): while an
owned room with a spawn has fewer extensions and extension sites than its level allows, the Optional `opening placement` job picks one
tile (the first spawn's checkerboard parity at range 2 or more, nearest by path, never next to a source or a mineral, within 3 of the
controller or within 3 of the edge, and only where the free neighbors stay connected and one can fill it) and the act step calls
`Room.createConstructionSite(x, y, "extension")` after the spawn orders (billed, V203; one per room per tick, V204, on several such calls in one tick). A tile the game
refuses (`ERR_INVALID_TARGET`) is skipped for the global; a pick not seen on its tile within `APPEAR_TICKS`, or refused for the room
(`ERR_RCL_NOT_ENOUGH`, `ERR_FULL`), counts toward `LOST_MAX` (3 in a row stop the room's placement for the global); a pick that finds
no tile waits until the grid, the count, the level, or the refused tiles change. `opening.place_extensions` turns it off.

Static mining (P3.1, ADR-023, which places a container site on each mining tile until the base planner exists): `rustedscreep_core::economy::Mining` is pure. Per owned room with a spawn it keeps a plan: each source's
mining tile (its walkable neighbor off the room's edge and not another source's tile, nearest the first spawn by path on movement's grid;
a tile holding a container or a container site wins), the miner's walk from the spawn (`Trip`), and its body at the room's capacity
(`sizing::miner_body`: `ceil(capacity / (ENERGY_REGEN_TIME × HARVEST_POWER))` WORK parts and the MOVE count that loses the least energy
over the walk, no CARRY; none when the room cannot pay for it). The Critical `mining` job runs before `opening` and the `spawn` job:
it gives each miner (own creeps named `m<room>-<serial>`, `Role::Miner`) a source (kept from the last tick, else the tile it stands on,
else the nearest free source), asks for one miner per source with none (`BodySpec::Exact`, `PRIORITY_MINER` 125, held back while it costs
more than the opening watchdog's cap), pushes a harvest while the miner is next to its source and the source holds energy, and moves it
onto its tile (range 0; next to the source while the tile cannot be reached). With no CARRY the harvest lands in a container on the
miner's tile or on a pile there (V212: a miner with no CARRY part drops its harvest on its tile). Per source it records the harvest of each regeneration cycle for the P3 gate (the economy phase's exit check; `SourceHarvest`,
heap only): a rise of the source's energy between two ticks' snapshots is a refill (V184, V215: a source refills all at once, after that tick's harvests), and the cycle left the earlier
snapshot's energy less what that tick's miner harvest could take; the last `CREEP_LIFE_TIME / ENERGY_REGEN_TIME` cycles and the
totals go to `rustedscreep.opening()`'s `mining:` part, with the cycles no miner harvested in and those after a skipped tick apart. `Mining::mined` lists the sources whose miner is next to them and where it stands: the opening
does not harvest them, sizes their workers with `carry_round` (one tick at the source), and its gatherers take energy there only as a
whole load (else `Task::Wait` next to the tile), so one take replaces several. `Mining::container_needs` lists the mining tiles with no
container and no container site once the room pays for the miner; the Optional `opening placement` job places them before any extension
(`economy::placement`, the same one call per room per tick and refusal rules). A miner's successor is ordered before the miner dies, timed to the successor body's spawn time, its walk out, and `spawn.replace_margin_ticks` (P5.2, pre-spawn replacement). `mining.enabled` and `mining.place_containers` turn the requests and the container sites off. The `rustedscreep.opening()`
summary has a `mining:` part (miners, mined sources, harvests, requests, and per source its tile, trip, and body).

The link network (P3.2, ADR-024, which places link sites from RCL5 until the base planner exists) lives in the same job, since it hangs off the mining plan and its flow field. The plan also holds the
controller link's tile (a link or link site within `RECEIVER_RANGE` 3 of the controller, else a free tile at `CONTROLLER_LINK_RANGE` 2,
nearest the spawn by path) and each source link's tile (a link or link site next to the mining tile, else a free neighbor of it, off the
source's other tiles first, then farthest from the spawn); a link tile is inside `LINK_MARGIN`, off roads and mining tiles, and keeps its
free neighbors connected (`placement::keeps_neighbours_connected`). From RCL5 `Mining::link_needs` lists the controller link's tile,
then, once it stands or is placed, the `links.source_links` (1) sources farthest from the spawn; `economy::placement` places them after the
containers and before the extensions, one per room per tick (V219, on several link sites in one tick), never on a tile the placement gave up on (`Placement::refused_tiles`
feeds the plan, whose key counts them), and only while the room holds fewer links and link sites than its level allows. Each tick the job
reads the room's links from the snapshot (`Links`: energy, and the cooldown, the ticks before a link can send again, while a link holds energy; V216, on the game's checks of `transferEnergy`), marks the ones the level keeps
active (the nearest the controller, V218, on which links a level keeps active; a tie at the last allowed range counts as inactive), and a miner with a CARRY part
(`sizing::miner_body_with`, one CARRY while a built link that can pass energy on stands next to its tile) transfers its store into its source
link on the tick the harvest would not fit (transfer runs before harvest, V45) when the link's room fits it and the link is a controller link
or an active one. The sends are hauling, so the Standard `links` job makes them (`Mining::send_room`, one unit per room with a link that may
send): each active source link that is not a controller link sends at most once a tick, once it holds `links.send_min` (200) and the
controller link with the most room left this tick has that much, an explicit amount (V216) through `Mining::link_orders`, which the act step
issues after the sites (`StructureLink.transferEnergy`, billed); a refused send holds its link for `links.hold_ticks`. The receiver gets the amount less `ceil(3%)` and the sender's cooldown is `LINK_COOLDOWN`
per tile (V217, on the link processor). `Mining::receivers` gives the opening the controller links: a gatherer takes a whole load there (`Task::Withdraw`), or
any `scavenge_min` once no send fits (`Receiver::full`), so the pipe never stalls; `Mined::link` points a linked source's gatherers at the
receiver, and the opening sizes their workers with no walk (`SourcePlan::linked`). When the receiver backs up, the source link fills, the
miner stops transferring, and the container takes the harvest again. `links.enabled` and `links.place_links` turn the network and the sites
off.

Haulers (P3.3, ADR-025, which places a controller container for haulers to deliver to until the base planner exists): the mining plan also holds the controller container's tile (a container or container site within `RECEIVER_RANGE`
of the controller that is not next to a source, else a free tile at `CONTROLLER_LINK_RANGE`, nearest the spawn by path, under the link
tiles' rules but with no connectivity test, since a container blocks no move; off the link tiles), which `Mining::container_needs` lists
after the mining tiles from `hauling.container_level` (3) once a source with no link pays for its miner; `Mining::mine_tiles` adds it to
the containers the workers repair. The Critical `hauling` job (`economy::Hauling`, after `mining`, before `opening` and the `spawn` job)
runs per owned room: its plan (built again when the grid version, the capacity, the level's extension capacity, the container's place,
`hauling.margin_pct`, `hauling.max_per_source`, or the mining plan's sources change; a flow field to the container per build, heavy)
gives each mined source with no link a body of CARRY then MOVE parts and a count (`sizing::hauler_plan`: the fewest haulers that carry
the yield over the longer of the trip from the spawn and the trip to the container, the least energy for them, the fewest MOVE; the
round counts a transfer a tick into fills of `EXTENSION_ENERGY_CAPACITY` with a step between two; at most `CONTAINER_CAPACITY` of CARRY),
only while the container stands; a room with no hauler and no store on the container's tile skips the job's scan.
Haulers are own creeps named `h<room>-<serial>` (`Role::Hauler`); one request per source short of haulers while its miner is next to it
(`BodySpec::Exact`, `PRIORITY_HAULER` 115, held back above the opening watchdog's cap). A hauler gathers until full (a whole load in one take
from the pile on its mining tile or the container there, or the pile when the container makes up the rest; else it waits next to the
tile, so it never picks up a full container's overflow pile a tick at a time) and delivers until empty: the spawns
and extensions with room (Critical; last tick's fillers, then the nearest; free space reserved per tick), then the controller container
(Standard, the `hauling work` job, one unit per room with a hauler to store this tick). `Hauling::view` gives the opening the sources
whose haulers work this tick (`HaulView::redirect` points their `Mined::link` at the container, so the workers take there and are sized
with no walk), whether a hauler hauls a source mined this tick (the workers then fill nothing and the `min_workers` rush is off, except
in a recovery), the energy the haulers take this tick (left out of the workers' loose energy), and the room they fill (left out of the
workers' fills). Workers waiting at a controller link or the container stand
`WAIT_RANGE` (2) off it, so the tiles next to it stay free for takers and haulers, and treat the two receivers alike; the sources whose
workers take next to the controller are sized as one pool. `hauling.enabled` and `hauling.place_container` turn the requests and the site
off. The summary has a `hauling:` part (haulers and tasks, requests, the container, per source the trip and body).

Upgraders (P3.4): `economy::Upgrading` runs per owned room with a controller store (a container or site on the mining plan's controller
container tile, or a controller link) or an upgrader (`Upgrading::select`, after `hauling`; other rooms get no unit). The Critical
`upgrading` job (after `hauling`, before `opening` and the `spawn` job) builds its plan again when the grid version, the capacity, the
level, the controller, the stores, the budget's two sums, the counted sources, or `upgrading.free_tiles` change (a flow field to the first
store per build, heavy). The budget is per creep life: the yield of each source whose energy reaches the controller (hauled: the hauling
plan wants haulers for it, the container stands, and the room has a hauler; sent: the mining plan says `linked` and a controller link
stands, less `link_loss`; either only while the room has a miner; a miner or hauler, spawning ones included, counts for
`upgrading.crew_grace_ticks` after the room last had one), less every other creep's body (the mining
plan's miners, the hauling plan's haulers, the opening's planned workers in the body solved for them while upgraders take, at most
`upgrading.worker_repeats` WORK parts: the plan's `capped` body, P3.6). `sizing::upgrader_plan` gives
one body (WORK, `upgrader_carry` CARRY, MOVE) and a count that upgrade the most of it, at most one per post and, at level 8,
`CONTROLLER_MAX_UPGRADE_PER_TICK` WORK in all. Posts are the walkable tiles within 3 of the controller next to a fed store (the
container while a counted source is hauled, the links while one is sent; the container's own tile too), off the mining and link tiles,
but the `free_tiles` next to the first fed store nearest the spawn (room for the haulers and the workers); best first (next to the
most stores, then the farthest from the spawn). After a global reset each upgrader takes the post it stands on. Upgraders are own creeps named `u<room>-<serial>` (`Role::Upgrader`); one request while the room is short (`BodySpec::Exact`,
`PRIORITY_UPGRADER` 110, held back above the opening watchdog's cap). Each holds a post (range 0). The Standard `upgrading work` job (after
`hauling work`, one unit per room with an upgrader out of the spawn, by room index) withdraws from the first store next to it whose
energy left covers its free space (links first), else the fullest, an explicit amount (its free space, at most
what the store holds less this tick's claims, the workers' included through `Opening::taken`) on the tick its store holds no more than
one tick's upgrade by its own body (the plan's body when it was first seen), and upgrades every tick it holds energy in range. `Upgrading::view` gives the opening whether the
upgraders take the controller's energy (from the room's first upgrader until the plan wants none) and which sources they count: the
opening then wants no worker for those sources, keeps `opening.min_workers`, caps new workers at `upgrading.worker_repeats`, sets
`min_upgraders` to 0, and a worker with nothing else to do holds its energy unless its source is not counted. A worker also leaves
alone a spawn or extension a hauler's transfer goes into this tick (and the room a walking hauler claimed). The view stays last tick's through a run that faults. `upgrading.enabled` turns the requests off. The summary has an `upgrading:` part (upgraders, tasks, budget, posts, body).

Remote profit model (P6.1; P6.2's remote manager prices each candidate with it in its Standard `remote` job, again only when a way's grid versions or the home's capacity change): `rustedscreep_core::economy::remote_profit` is pure. For a remote room (one the bot mines without owning it), given each source's way from the home's first spawn (`RemoteWay`: measured bare and with its planned road tiles at the road's cost), the road tiles each way keeps and still needs (`RoadTiles`, by terrain), whether its container stands, and the way to the controller, it counts one creep life (`CREEP_LIFE_TIME`) for a `RemotePlan` (a reserver or not, roads or not): the sources' yield at the plan's capacity (`SOURCE_ENERGY_CAPACITY` reserved, `SOURCE_ENERGY_NEUTRAL_CAPACITY` not, V184); the bodies the economy's solvers size (a remote miner of `remote_miner_work` WORK parts and one CARRY that also repairs its container, `miner_body_work`; haulers from `hauler_plan_with`, one WORK part each with roads to repair them as they walk; a reserver from `reserver_body`, CLAIM then MOVE parts), each post (a mining tile, the controller's side) held without a gap by pre-spawn replacement, so `need / (life − walk)` lives; the containers' and roads' repairs (road wear `ROAD_WEAROUT` a part a step, V54, V242); the one-time builds over `remote.horizon_ticks`; and the CPU, every billed intent at `INTENT_CPU_COST` plus `remote.creep_ucpu` a creep a tick. `RemoteProfit` holds the net energy, the CPU, the spawn ticks and their ratios (`per_cpu`, `per_energy_permille`, `spawn_permille`). `best_plan` takes the plan with the least CPU among those that net more than 0 and within `remote.slack_pct` of the best net. `remote_crew` gives the bodies and counts the model priced, so the manager spawns exactly them.

Remote manager (P6.2, ADR-029; `rustedscreep_core::economy::Remotes`, glue in `rustedscreep-wasm` `remote.rs`). It decides which remote rooms each owned home mines and drives their creeps.

- **Candidates.** A home's candidates are the rooms through its open exits (its cost grid knows them), less highway rooms and sector centers (`is_remote_room`, V231).
- **Intel.** The snapshot reads a "remote room" fully: a visible room the bot does not own that an own creep's name gives as its remote room. It reads the same room objects as an owned room's, the controller as an object with its reservation's `ticksToEnd`, who holds the reservation (the bot's username, read once a global from an own creep's `owner`), `FIND_HOSTILE_STRUCTURES`, and every `remote.road_read_ticks` the roads with their `hits` and `hitsMax`. `Intel` keeps what was seen in the heap.
- **Scouts.** A scout (one MOVE part) is ordered while a home wants another room and has a candidate it never saw, or saw more than `remote.rescout_ticks` ago. It stays in a minable room until the room's ways are searched, then calls `suicide` once nothing is left to see.
- **Ways.** `WaySearch` is a Dijkstra search by cost, then steps, then tile: over the home's grid from next to its first spawn to the exits on the candidate's side, then over the candidate's grid from the tiles across those exits (V151). It gives each source's mining tile (a container or container site first), its trip bare and paved, its road tiles by terrain, and the tile next to the controller. It runs in the Optional `remote placement` job, one search a step (a heavy unit), again when either grid's version changes.
- **Choice.** The manager mines the priced candidates that net the most, at most `remote.max_rooms`, while the model's spawn time stays within `remote.spawn_permille` of each home spawn. A room whose intel stops allowing it requests nothing more; abandoning and evacuating are P6.3's.
- **Creeps.** Names carry the remote room (`Role::is_remote`), so after a reset only each creep's source is chosen again. The requests run below every home request, with pre-spawn leads (P5.2).
  - A reserver reserves while `ticksToEnd` plus its CLAIM parts stays within `CONTROLLER_RESERVE_MAX` (V264).
  - A remote miner builds its container site, repairs the container below `remote.container_repair_pct`, and harvests otherwise.
  - A remote hauler takes whole loads at the mining tile and walks home. In its home room with energy it is a guest of the home's `Hauling`, which delivers its load like the room's own haulers. On a roads plan it carries a WORK part, builds road sites within range 3 on the way home, and repairs the road that lacks the most hits when the roads are read.
- **Placement.** One site a tick per mined, visible room, at most `remote.max_sites`: the containers first, then the roads along each source's path.
- **Upgraders.** `Upgrading::set_remote` adds each home's mined rooms' net energy (while a remote hauler lives) to its upgraders' budget.
- **Tiers.** The choice and the requests run in the Critical `remote` job, before the spawn job, as every job that requests creeps does, but they request and search only on ticks the Standard tier runs. The creeps' work runs in Standard, and the ways and sites in Optional. Remote mining is spare work under SPEC §2's bucket policy.
- **Threats, contests and audits (P6.3).** For hostile creeps in a remote room the snapshot reads their active ATTACK and RANGED_ATTACK parts (`snapshot::Combat`, V269).
  - *Threatened.* Armed hostiles in a mined room start a `Threat` for `remote.evacuate_ticks` after the last sight. The room's creeps wait near the home's first spawn, its haulers' loads are delivered there, and nothing is requested for it until the wait runs out.
  - *Contested.* Another user's ownership or reservation (an invader core's among them, V273), or a hostile structure, abandons the room. Its miners and reserver `suicide`, since a harvest or a reserve fails there, and its haulers bring their loads home first. The `remote.contest_threats`-th threat within `remote.abandon_ticks` does the same.
  - *Unprofitable.* An `Audit` window of `remote.audit_ticks` that delivered less energy home than its new creeps' bodies cost abandons the room, though never its first window in a global. Its creeps work on until they die.
  - *Abandoned.* An abandoned room is not chosen for `remote.abandon_ticks`. Each event is logged by `remote.rs` (the threat's line names the hostiles' owners, read only on that tick).
  - *P7's hooks.* `Remotes::threats`, `evacuate` and `clear_threat`.

Controller signer (ADR-030; `rustedscreep_core::signing::Signing`, glue in `rustedscreep-wasm` `sign.rs`). One cheap creep for the whole bot signs every controller it can reach with `signing::SIGN_TEXT`, whoever owns the room. Signing checks no ownership (V274, V275), and another player's safe mode does not stop it (V280).

- **What it knows.** `signing::Book` keeps, per room, the state last seen (signed, unsigned, the server's own sign, unreachable, deadly, or no controller), the tick, the controller's tile, another owner's towers, and the signers lost and signs replaced there. It is persisted in segment 95 (§5), so a reset does not send a signer back to rooms already signed.
- **Rooms.** The candidates are the rooms whose names say they can have a controller (`is_remote_room`) within `sign.max_distance` (31, one creep life's straight walk) of a home. A home is an owned room with a spawn at `sign.min_level` or more. A room is due with nothing known of it, when it was seen without the text, or once its recheck (`sign.recheck_ticks`) or retry (`sign.retry_ticks`) comes due.
- **Reads.** The snapshot reads the room an own signer stands in (`snapshot::SignRead`, `push_sign`): the controller as an object, its `sign` (the server's username `Screeps` counts as the server's sign), and the towers of other users.
- **The way first.** A signer gets a room only after `Movement::search_route` finds a route there. The route runs to a tile next to the controller, or two tiles into a room no creep has seen (a look, which then becomes a sign). The search uses its own PathFinder limits and avoids every room with seen towers or `sign.max_deaths` lost signers, but never the target or the start. It keeps the route it found for the walk, and a planned trip is searched again from the new signer's first tile, so movement walks the signer's own route. Only the home spawn's search judges a room: a failed search there, a route longer than a creep life, or a controller no tile next to which can be reached makes it unreachable. A search that fails from where the live signer stands only passes the room over for that signer, and so does a second miss of movement's own route.
- **One signer.** With none alive, the Optional work plans the next: the due room nearest a home, a route from its spawn, and a body. A safe route gets one MOVE part. A towered target gets `signing::signer_body`'s cheapest TOUGH and MOVE body for the worst case of its towers, after the path's rooms' terrain has been fed in one room a tick. The walk is modeled a tick at a time with MOVE parts lost front first (V278, V283, V284), and a target no body survives is deadly. The Critical `sign` job requests the plan at `PRIORITY_SIGNER` (70, below every other role), only on ticks that run the Optional tier.
- **The live signer.** It signs next to a controller that does not show the text, in a room that is due. It then takes the nearest due room it can reach whose way no seen tower reaches, with no search when even the shortest walk (the Chebyshev distance in world tiles) would outlast its life. In a towered room it was not sized for, it stands on the entrance edge, and the engine carries it back out (V151); on an edge tile with no trip otherwise, it steps inside. A death before old age counts against its room (`sign.max_deaths`). So does a sign another player replaced, and a sign that does not show on the next read (an owner whose code re-signs in the same tick), against `sign.max_overwrites`. Either makes the room wait `sign.retry_ticks`, and a room given up on keeps its retry clock while a signer passes through. With nothing left in reach it calls `suicide` (no death then).
- **Tiers and resets.** Neither job runs with nothing to do, so an idle signer leaves no trace in the scheduler. The record's segment is requested only once a home reaches `sign.min_level` and the config overrides have loaded (so `sign.enabled=false` in them holds from a global's start). Until the record loads, nothing goes into the book, so a reset with a signer alive cannot save over the record. Without the game's `PathFinder` and `RoomPosition` (a sandbox), movement installs no route search, and the signer plans nothing.

Threat assessment (P7.1; `rustedscreep_core::defense::Threats`, glue in `rustedscreep-wasm` `defense.rs`). It says what the hostile creeps in the bot's rooms can do, for the rest of P7 (towers, safe mode, defenders) to act on.

- **Reads.** In an owned room the snapshot reads each hostile creep's `hits` every tick (`Creeps::set_assessed`), and its body (each part's type and boost compound) once (`Creeps::set_body`). A body is kept by the creep's handle, dropped when the handle is released, and forgotten when the creep is seen in another player's room at a level with labs, the only place a lab can boost it (`Controller::boosts_possible`, V290, V291). A neutral room keeps it, so a hostile stepping in and out over the edge of a visible room costs no new read.
- **Power.** Each part's hits follow from the creep's total (V283: the front parts empty first), so the live parts are the last ones. An action's power is the live parts of its type, each counting its boost's multiplier for that action (`Power`, V286, V288; the multipliers are the `BOOSTS` table, V131). Its methods turn it into melee, ranged, mass-attack, heal, and dismantle figures per tick. Effective hit points are the least damage in one tick that kills the creep, with its boosted TOUGH parts' saving (`effective_hits`, V287).
- **Rooms and events.** `Threats::assess` sums the hostiles per owned room. A hostile that can attack, ranged attack, dismantle, or attack the controller is dangerous. A room with one is under threat until none has been seen for `defense.threat_end_ticks`. A start, a change in its counts of dangerous or boosted hostiles (at most once per `defense.threat_log_ticks`), and an end are logged, the start and changes with the dangerous hostiles' owners.
- **Tier.** Critical, but outside the scheduler's jobs: it runs right after the snapshot, as the other `begin` steps do, on every tick a hostile stands in an owned room or a threat is open. Its work is bounded by the few hostiles in the bot's rooms, so it needs no CPU gate, and on a quiet tick nothing runs. A global reset forgets every open threat and every body.

Towers (P7.2; `rustedscreep_core::defense::TowerControl`, glue in `rustedscreep-wasm` `defense.rs`). The towers' answer to a threat, the heals after it, and getting energy into them.

- **A tower's action.** One a tick, heal before repair before attack (V295), `TOWER_ENERGY_COST` each and none with less (V281, V296). Its power falls off from `TOWER_OPTIMAL_RANGE` to `TOWER_FALLOFF_RANGE`, floored once (`tower_amount`, V239, V296). Power effects are not modeled (V299).
- **The combat sim.** `kill_ticks` plays a hostile's fight forward a tick at a time: every tower with a shot left fires from where it stands, every hostile healer within range 3 heals the target (`heal_on`: at range 1 or on itself with `heal`, at 2 or 3 with `rangedHeal`; all on this target, the worst case for the bot), the damage goes through its boosted TOUGH parts at its hits then (`damage_taken`, V287), the heal lands capped at `hitsMax`, and it dies at 0. A tick that leaves it no lower means it never dies.
- **Focus fire.** The towers shoot one target: the one the sim kills within `defense.max_kill_ticks`, dangerous before harmless, then the fewest ticks, the most heal, the lowest id; a harmless healer within range 3 of it goes first when its kill plus the target's without its heal is quicker. Last tick's target is kept while it still dies in time (unless it is harmless and a dangerous one can be killed), so heal never undoes a half-done kill. Only the fewest towers that kill it in the same ticks fire, strongest at it first; on a one-tick kill the spare towers take other one-tick kills, and the rest heal. When the sim kills nothing in time, the towers hold fire: shooting into heal that matches them only drains them.
- **The shot check.** After each shot the bot keeps the hits the sim expects the target to have next tick (with the worst-case heal). A target above that on `defense.miss_ticks` ticks in a row is not hit as modeled (a rampart on its tile, a power effect, an inactive tower): the towers stop firing at it while the room's state lives, with one log line.
- **Heal priority.** With no target, towers holding at least `defense.heal_reserve` heal damaged own creeps, each tower the creep with the most hits still missing.
- **Tier and calls.** The Critical `towers` job (one unit per owned room with a tower holding energy and a hostile, or a damaged own creep and a tower holding the heal reserve; none, and no clock sample, on other ticks) runs after movement's first reads. The calls go out in the act step right after the spawn orders, `attack` or `heal` through a `catch` binding, billed when OK (V297, V298). A new target and a hold are logged at most once per `defense.threat_log_ticks` per room, except the first line about danger after one that was not; a room's state lives while it plans or its threat is open. A tower's store is read only on a tick its room is full, has a hostile, or had an open threat; own creeps' `hitsMax` once in their life.
- **Supply.** Towers join a room's fills (the haulers', and the opening's workers' where they fill) only once its spawns and extensions are full, and only with at least `hauling.tower_refill_min` room. `planner.towers` is on since P7.2 (ADR-026: the phase that uses a kind turns its switch on), so each owned room's plan places its towers from RCL3.
- **Not yet.** No ally list (P10's whitelist): towers shoot every hostile creep in an owned room. In the bot's own safe mode hostiles cannot heal (V300), which the sim does not know yet (P7.4 reads safe mode), so it may hold fire on a target it could kill then. A hostile stepping in and out over an exit edge, healed outside, is shot whenever the sim kills it in time; whether that drains the towers is not modeled. A tower's shot at a creep on a rampart hits the rampart (V239, V281); the sim does not model that. In an owned room it should not happen (inferred: only the room's owner builds ramparts there, and a hostile can stand only on a public one, which the bot does not make).

Defenders (P7.5, ADR-036; `rustedscreep_core::defense::Defenders`, glue in `rustedscreep-wasm` `defense.rs`). Ranged creeps an owned room spawns only when its towers cannot win, as many as the combat sim says.

- **When.** In each owned room with a dangerous hostile, every dangerous hostile the defenders can reach is judged by P7.2's sim with the towers alone. One the towers kill within `defense.max_kill_ticks` needs no defender. One they do not, one they distrust (counted with no tower), and any in a room with no tower holding energy for a shot, needs defenders.
- **Reach.** Defenders never leave the rampart line. Its inside is a flood from the room's spawns over the movement grid's walkable tiles in all 8 directions, taking in own ramparts but not passing them, so no inside tile touches a walkable outside one (a move is checked only against its destination, V244). A hostile is in reach when an inside tile lies within range 3 of it. With a gap in the line, no spawn, or no grid yet, every hostile counts as in reach.
- **How many.** `least_power` finds, per hostile, the fewest RANGED_ATTACK parts that kill it with the towers within `defense.max_kill_ticks` (a binary search over the sim). The room keeps the most it needed in its threat and requests full bodies (`body_for`: MOVE then RANGED_ATTACK pairs, as many as the serving room's capacity pays for) until its living defenders hold that many parts, up to `defense.max_defenders` bodies, at `PRIORITY_DEFENDER` (175, above every economy role but the recovery worker). A hostile beyond the cap gets no request and one line a threat.
- **Fire and posts.** The room's target is the towers' focus when the defenders reach it, else last tick's target while still killable with the team, else the reachable dangerous hostile the sim kills soonest with the towers and the whole team. A creep with a CLAIM part gets as many towers as without the defenders, since P7.4's claim trigger counts only the towers. The towers' sim counts the defenders in their home within range 3 of each candidate (`TowerControl::set_allies`), so towers and defenders fire together; to keep that count true, a defender in its home shoots the towers' focus first whenever it is in range of it (even a harmless one), else the room's target, else the weakest dangerous hostile in its reach. Each takes a post inside, within range 3 of the target, free of other defenders, out of melee reach first, then on an own rampart, then the nearest, and keeps it while it stays valid. The intent buffer refuses a `rangedAttack` on an own creep (V339).
- **Tier and reads.** Both jobs are Critical: `defenders` (one unit per owned room with a dangerous hostile) after the safe-mode triggers and before the spawn plan, and `defender work` (one unit per defender: a shot, a move, or a give-way). With no hostile in an owned room and every defender home, neither runs: each defender gives way outside the jobs, with no clock reading. The snapshot reads a defender's `body` once in its life and skips its energy store; own creeps' `hits` stay read only in rooms with an open threat and a tower. Defenders are not recycled.


Base planner (P4.2, the two candidate planners and their benchmark; ADR-007 accepted 2026-11-18: `flood` is the planner, `bunker` the comparison baseline; wired in P4.3, the placement of the planned structures, below): `rustedscreep_core::planner` is pure.
A `PlanInput` (terrain, controller, sources in (x, y) order, mineral, a standing spawn) gives a `Layout`: every structure, road, and
rampart as an `Item` with the controller level it belongs from, sorted (level, type, tile), plus the `Roles` the economy reads by
position (hub, the tile where the base's filler creep stands, next to the storage, the hub link, and a spawn; storage, hub link, mining tiles, source links, the controller's container and link, the mineral container). A `Board`
holds a run's working state and checks the site rules (V237, `checkConstructionSite`'s tile rules) and the level counts (V234, `CONTROLLER_STRUCTURES` per level) on every placement. Two candidates, `bunker`
and `flood`, each a `Run` that does one phase per `step` (`scheduler::Step`), share `sites` (the economy's tiles), `roads` (A* at the
fatigue rates, reusing planned roads), `score` (the hub's anchor cost), `defence` and `mincut` (the minimum cut on a vertex-split graph,
by Dinic's max-flow algorithm; since ADR-033 the cut is constructed walls, which never decay, but for its gates: ramparts where a
planned road crosses it, where no wall may stand, and one per outside region own creeps must still reach from the hub, nearest the
towers, `defence::gates`), `transform` (the Chebyshev distance transform), and `validate` (the one referee, which also measures). A `Scratch` holds the
buffers a run reuses; a run's own state is in its `Run`, so a run stepped with a new scratch each step gives the same plan. Since
P4.3 a `Board` holds its own copy of the terrain and `flood::Run` borrows nothing: each `step` is given the input the run was made
from, so a job keeps a run across ticks.

Base planner in the tick path (P4.3, ADR-026: destroy only the off-plan structures in a due planned one's way, and build only what the bot uses). `planner::job::Planner` reads each owned room's `Facts` (the terrain from movement,
the controller, sources, and spawns from the snapshot, the mineral from movement's statics, the standing containers and links and their
own sites, which `flood::Run::adopt` hands the run) and plans it once a global, one phase a step, then validates it; with several
spawns or a spawn site each is tried as the anchor (the spawn tile the plan is built around; from the highest tile down, so the room's first spawn usually goes first), and the
valid plan that holds every standing spawn and the most of what stands is kept. The plan lives in the heap; a small record (ADR-027,
`planner::record`, segment `persist.planner_segment`) keeps each room's anchor and adopted container and link tiles across global resets,
and no room is planned until it has loaded: the record's anchor is tried first and its plan kept once it holds every standing spawn; a plan is planned again only when a spawn it did not know
stands off its spawn tiles. Each planned road records what it serves (`layout::RoadNeed`), so `planner::build` places it only from the
earliest level a structure it serves is placed under the config's switches. rustedscreep-wasm's
`planner.rs` runs it as the Optional `planner` job (each step heavy, timed with two `getUsed` calls for `rustedscreep.planner()`).
`economy::placement` reads the room's `PlanView`: while the plan is built it places nothing; with no plan it runs the interim
placement (ADR-020, ADR-023 to ADR-025); with a plan, `by_plan` maps `economy::mining`'s container and link requests (each with
its role, `ContainerNeed` and `LinkNeed`) to the plan's roles (`build::role_tile`), then asks `planner::build::next` over a `Ground` built from the snapshot and movement's last scan (roads
have no owner, so they show only there; a look waits for a scan newer than the room's last built, destroyed, or removed object).
One action a room a tick: a site, or a `Structure.destroy()` of an own off-plan structure in a due item's way (one open replacement
a room, never a spawn, never with a hostile creep; `DestroyOrder`, issued by the opening glue before the sites). A look that finds
nothing waits until what it watched changes or `planner.verify_ticks` pass.

Constants self-check (P1.11): `rustedscreep_core::consts_check` is pure. A `Globals` source gives the server's names, then one
value's JSON text per step; `Check::step` reads them all, checks that at least one anchor constant (`OK`, `WORK`, `CREEP_LIFE_TIME`) was seen (otherwise the global holds no game constants and the check fails),
then compares 15 groups (scalars, enum families by prefix, sets, tables entry by entry) with `rustedscreep_consts`, through a strict
JSON reader with exact numbers on Clinger's fast path (an exact decimal-to-float conversion when the digits and the power of ten are small; no float parser in the wasm). `KNOWN` names each known difference with its
reason; findings are capped (`MAX_FINDINGS`). rustedscreep-wasm's `consts_check.rs` calls `Object.getOwnPropertyNames(globalThis)` once
and reads each upper-case name's value through its property descriptor and `JSON.stringify` (local `catch` bindings; no getter
runs). A value with no JSON text is a finding (unreadable); a throw from the descriptor read fails the check. It is time-sliced as an Optional step job under that tier's cap, once per global. It logs one completion line
(info, or a warning with up to `log.consts_findings` finding lines); `rustedscreep.consts()` shows the phase, findings, and known
differences. The native test runs it against engine common 2.16.0 (`tests/fixtures/constants-common-2.16.0.tsv`).

js/ (P1.3, the build pipeline; P1.4, the loader): `js/src/main.ts` wires the loader (`js/src/loader.ts`) to the game's `loop` and the `rustedscreep.*` console
commands. The loader boots once the bucket reaches `LoaderConfig::boot_bucket_min` (480, set at the P1 gate from the MMO (the official persistent server) cold start, the cost of loading the wasm in a new global,
VERIFICATION_LOG V150). On the first boot tick it requires the wasm, which arrives through the GitHub link (the game's sync of the code from the user's GitHub repository) as a base64 text module
(`rustedscreep_wasm.js`, decoded in plain JS by `js/src/base64.ts`, ADR-015; split across `rustedscreep_wasm.js`, `rustedscreep_wasm_1.js`, … when one
module would pass 905709 bytes, each part checked against the length the build records and decoded into one buffer, ADR-028) or as a binary module through the API, checks its
cyclic redundancy check (CRC-32) against the build's, logs the boot line, and compiles it; on the next tick (`boot_split_ticks`) it instantiates and runs the
first loop. Recovery is in section 6. `js/tools/` holds the build
(cargo → wasm-bindgen `--target web` → `wasm-opt -O4` → tsc → Rollup CommonJS (CJS) into `dist/`), stage (into `deploy/` for the GitHub link),
size, check, smoke (the bundle in a Screeps-like sandbox, run at the end of every build), and setup-binaryen. Generated files go to the git-ignored `js/pkg/`,
`js/build/`, and `dist/`.

## 3. Cache and invalidation table
_P1 onward._

| Cache | Key | Contents | Built in tier | Invalidated by | TTL (time to live) | Approx. size | Toggle (naive mode) |
|---|---|---|---|---|---|---|---|
| Object handles, creeps and structures (P1.5) | packed object id (`ObjectKey`, u128) | per slot: generation, last pass seen, row, live flag; FxHashMap id → slot | Critical (snapshot, every tick) | the first pass that does not push the object: the slot is released and its generation advances, so old handles stop resolving; free slots are reused oldest first; a slot whose generation would wrap is retired | none (pass-based) | about 48 B per slot plus a map entry | none: handles are the identity scheme, not an optimization |
| Structure static data (P1.5) | structure handle | `structureType` and position | Critical (snapshot), read only when a structure is first seen | the structure's handle being released (destroyed, or missing from one pass) | none | 8 B per slot | `cache.structure_static` (P1.10): off, every push reads both again (`Interner::mark_fresh`) and keeps the handle; a failed re-read skips the structure for that tick and releases its handle |
| Config in force (P1.10) | one per global | the profile's values, the overrides, the config they give, the record's state | at the global's start (profile values), then on a console change or when the record loads (Standard, a tick or two after the start) | `configSet`, `configUnset`, `configReset`, the record loading, and the naive guard (ADR-022: once per loop, it removes the overrides that turn a cache off after `cache.naive_max_ticks` or below `cache.naive_stop_bucket`); a global reset starts again from the profile | none | under 1 KB | none: it is the switchboard for the others |
| Job state (P1.6) | one per job, owned by its subsystem | round-robin cursor, measured CPU per unit | the job's tier | nothing during a global; a global reset restarts it at unit 0 with no estimate (only costs extra clock samples on that tick) | none | 16 B per job | none |
| Sleep wheel (P1.6) | item (a handle or a job) | wake tick and an optional event | the owner's tier | the wake tick passing (gaps included), its event firing, a new sleep, or a cancel; time going back wakes everyone; a global reset empties it, so every item starts awake | none | about 28 B per entry, 256 slot vectors, a heap for wakes beyond 256 ticks, one list per event being waited for (emptied lists are reused, at most 64 kept) | none |
| Intent buffer (P1.7) | creep row in this tick's snapshot | every accepted push of the tick (actor and target rows, the intent), the methods queued per row, the latest push per row and method, and the planned issues | the decide step (any tier) and the act step | `IntentBuffer::begin` each tick, after the snapshot; a push before `begin`, after the plan, or with a snapshot whose `Game.time` or creep count differs from the one `begin` saw is refused (`closed`) | one tick | 44 B per push and per issue (`Issue`, measured natively; it holds no pointers), 4 B per row, plus 92 B per row for the latest-push slots, which only grow (the rows' method sets say which are this tick's) | none: it holds one tick's intents |
| Room terrain, sources, and minerals (P2.1) | room name | the terrain masks, column-major (2500 B), and the tiles of sources and minerals | Critical (`movement read`), the first tick an owned room has own creeps or a request finds a room not read | the room evicted after `movement.field_idle_ticks` with no own creep in it; a global reset | none | about 2.5 KB per room | `cache.movement_grid`: off, read every tick for every room it would read |
| Room objects and cost grid (P2.1) | room name | the last scan's footprints (structures, own sites, deposits; sorted), the grid (2500 B, allocated on the first build) and its version | Critical (the first scan) and Standard (`movement rescan`); the grid on the first request after a change | a rescan every `movement.scan_interval_ticks` while the room has own creeps and is owned or had a request within that interval, sooner (`rescan_min_ticks`) after repeated failed moves into a tile with no creep on it or `Movement::invalidate`; the grid is rebuilt only when the footprints, the terrain, the statics, or the cost profile changed, which bumps its version; the room evicted as above | the scan interval | about 2.5 KB plus 4 B per object per room | `cache.movement_grid`: off, scanned and rebuilt every tick |
| Suspect tiles (P2.1) | room and tile | tiles moves kept failing into with no creep on them (something the snapshot cannot see) | the requesting job's tier | `movement.suspect_ticks` after they were learned; the room evicted | `suspect_ticks` | 8 B per tile | none: it is learned state, not a cache |
| Flow fields (P2.1) | room, goal tile, and range | distances to the goal (2500 `u16`) and the grid version they were built on | the requesting job's tier, on first use | a newer grid version (rebuilt in place on next use), `movement.field_idle_ticks` without use, or LRU eviction beyond `movement.field_cache_size` | `field_idle_ticks` | 5 KB per field | `cache.flow_fields`: off, every request builds its field again |
| Creep movement state (P2.1) | creep handle slot (with the handle) | goal (a goal near a target, or an exit on a route; P2.2), the room it was in at the last `begin` and the tick of its last request (P2.2: crossings, refused transfers, the same-tick read), the room it came from and when, the route it follows and its leg, a failed search of its own (kept for `route_retry_ticks`), the tile it was moved toward, stuck and unexplained counts, the blocked tile, failed detours in a row (the backoff), an A* detour and the tile it went round, the tick it stays fixed until (a move refused with `ERR_NO_BODYPART`) | each request, resolve, and the act step's refusals | a new goal (detour dropped), a detour step off its path or its blocker freed while the creep is within 2 tiles of it, the slot reused by another creep; a global reset | none | about 72 B plus the detour per slot | none: it is the creep's progress, not a cache |
| Routes (P2.2) | a slot with a generation (`RouteId`); found by room, target, and range | a search's room visits in order, each with the exit it is left by and the grid version the search used; the tick it was made and last used | the requesting job's tier (a search is a heavy unit in the request) | `movement.route_ttl_ticks` after it was made; a creep that cannot reach its exit in a room whose grid changed since the search; a refused transfer into a room on it; a request that found no way just after following it; LRU eviction beyond `movement.route_cache_size` of a route no creep used this tick or the one before (the book grows past the size while they travel); a global reset | `route_ttl_ticks` | under 200 B per route | none: a route search costs up to `route_max_ops` PathFinder ops, so it is never repeated per tick |
| Avoided edge tiles (P2.2) | room and tile | edge tiles a path stepped onto and back into the room from, learned when that cost a creep its way | the requesting job's tier | `movement.route_ttl_ticks` after they were learned; a global reset | `route_ttl_ticks` | 12 B per tile | none: it is learned state |
| Closed rooms (P2.2) | room name | rooms an own creep could not cross into (a refused transfer), and until when | the snapshot step (`Movement::begin`) | `movement.route_ttl_ticks` after the last refusal; a global reset | `route_ttl_ticks` | 8 B per room | none: it is learned state |
| PathFinder matrices and room statuses (P2.2, rustedscreep-wasm route.rs) | room name | each grid as PathFinder bytes (2500 B, avoided edge tiles at 255; a room with avoided tiles and no grid gets the unknown-room matrix with them) with its version and a fingerprint of its avoided tiles and the JS CostMatrix made from it; `Game.map.getRoomStatus` per room and the tick read; one shared CostMatrix for rooms without a grid | in a route search (the requesting job's tier; a target room that is closed or of another status fails before PathFinder runs) | a grid's new version or a change in its avoided tiles (copied again, the CostMatrix remade on use), the room leaving movement's rooms; a status `route_ttl_ticks` old (read again); at most `route_cache_size` statuses (the oldest out); a global reset | statuses: `route_ttl_ticks` | about 2.5 KB of wasm memory and 2.5 KB of JS memory per room with a grid | none |
| Creep identity (P2.3) | creep handle (the interner's static data) | role, home room, and serial parsed from an own creep's name (`spawning::CreepName`), or none | Critical (snapshot), read only when the handle is new | the handle being released (the creep died or left `Game.creeps`); a global reset reads every name once | none | 24 B per slot | none |
| Hostile bodies (P7.1) | creep handle (with the handle) | each part's type and boost compound, the first part first (up to `MAX_CREEP_SIZE`) | Critical (snapshot), read for a hostile creep in an owned room while none is known | the handle being released (the creep left the snapshot), or the creep seen in another player's room at a level with labs (it may be boosted there, V290, V291); a global reset reads each body again | none | about 400 B per hostile in an owned room | none: a body's boosts change only next to an active lab (V240, V290), which stands only in its owner's room at a level with labs (V291), so the cache has no stale case to measure |
| Tower focus (P7.2) | owned room name | last tick's target (a creep handle), whether the towers held fire, the tick of the last plan | Critical (the `towers` job) | the target no longer killable within `defense.max_kill_ticks`, or gone (a new target is taken); the room not planned on a tick (no hostile, no damaged creep, or no tower with energy) drops its entry; a global reset | none | about 24 B per owned room with a fight | none: it only keeps the towers on one target across ticks; dropping it costs a switch, never a missed shot |
| Defender bodies (P7.5) | creep handle (with the handle), in the same table as the hostile bodies | each part's type and boost compound of an own defender (role `d`) | Critical (snapshot), read once per creep life on its first tick out of the spawn | the handle being released (the creep died); a global reset reads each body again | none | about 400 B per defender | none: the bot boosts no defender, and an own creep's body changes only by boosts |
| Defender inside tiles (P7.5) | owned room name | one byte per tile (2500 B): outside, inside the rampart line, or an own rampart, from a flood from the room's spawns | Critical (the `defenders` job), on a tick the room has a dangerous hostile | a newer movement grid version, or a change in the room's own rampart or spawn tiles (a key over their sorted tiles); dropped when the room's threat ends; a global reset | none | about 2.5 KB per owned room with an open threat | none: the flood is rebuilt from what stands whenever either input changes |
| Defender need (P7.5) | owned room name | the most RANGED_ATTACK parts the room needed since its threat started, the parts its last line gave, whether its `Outmatched` line went out, and this tick's target | Critical (the `defenders` job) | a new threat in the room (its start tick differs) starts afresh; dropped when the threat ends; a global reset | none | about 48 B per owned room with an open threat | none: it keeps the team's size steady through a threat; dropping it re-sizes the team from the next tick |
| Opening plans (P2.4) | owned room name | per source its place, open tiles, trip to the controller, worker count, and whether it was mined (P3.1); the room's count | Critical (the `opening` job), a flow field per build (heavy) | a new grid version (movement rebuilt the room's grid) or a new room capacity; a change in which sources have a miner next to them (P3.1); a stall; a global reset | none | about 64 B per room plus 40 B per source | the plan's count only sets how many workers are asked for; the watchdogs catch a room that stops |
| Roads and ramparts for upkeep (P7.3, ADR-032) | owned room name | its roads by tile (id, hits, `hitsMax`), from the snapshot's full read every `upkeep.read_ticks` and the roads read by id for their repairers between reads; its own ramparts' last read hits, by handle; the road tiles asked for this tick | Critical (the `opening` job's `gather_upkeep`), from the snapshot | a full read replaces the roads; a road asked for and not read is dropped; a rampart not in this tick's structures is dropped; `upkeep.enabled` off clears both; a global reset (the next full read refills the roads, and each rampart is read when first seen) | none | about 40 B per road, 12 B per rampart | a road gone between reads costs at most its repairer's walk and one tick |
| Containers under repair (P3.5) | owned room name | the handles of its mining tiles' containers below `repair.start_pct` of `CONTAINER_HITS` once and not full since (since P7.3 the list also holds the roads and ramparts under repair, as `economy::Fix`) | Critical (the `opening` job's assignment), from the snapshot's container hits | a container at full hits or gone (it drops out of the list on the next run); a global reset | none | 8 B per container under repair (an object handle; at most `CONTROLLER_STRUCTURES[container]`, 5, per room) | none (`repair.enabled` off repairs nothing); a reset only delays a repair until the container is below the share again, which keeps about 40 decay steps of margin (TUNING) |
| Mining plans (P3.1) | owned room name | per source its place, mining tile, the miner's walk from the spawn, and its body at the room's capacity; the plan's key | Critical (the `mining` job), a flow field from the first spawn per build (heavy) | a new grid version, room capacity, or first spawn, a change in the room's sources or in the containers and container sites next to them; a global reset | none | about 48 B per room plus about 120 B per source | the body and tile only decide what is asked for and where a miner stands; the scenario tests cover the drain (`miners_drain_their_sources_from_their_containers`) |
| Link tiles (P3.2) | owned room name, in the mining plan | the controller link's tile and each source link's tile; the plan's key adds the controller, the room's links and link sites, the built links, and `links.enabled` | Critical (the `mining` job), with the mining plan's flow field (no extra field) | a link or link site appearing or going, the controller, and every key of the mining plan; a global reset | none | 8 B per tile | the tile only decides where a site goes; the scenario tests check the rules (`links_are_planned_and_placed_by_the_rules`) |
| Link roles (P3.2) | owned room name, for the tick | each link's role (controller link within `RECEIVER_RANGE` of the controller, source link next to a mining tile), whether it is active, this tick's promised energy | Critical (the `mining` job), from the snapshot's `Links`; read by the Standard `links` job | every tick | none | about 40 B per link | the engine's own checks (V216, V217); a refused send is logged, counted, and held |
| Held links (P3.2) | link handle | a source link whose send the game refused, and until when | the act step | `links.hold_ticks` after the refusal; a global reset | `links.hold_ticks` | 12 B per held link | none: it is learned state |
| Hauling plans (P3.3) | owned room name | per hauled source its mining tile, the trip its haulers are sized for, their body and count; the plan's key (grid version, capacity, the level's extension capacity, the container's place, `hauling.margin_pct` and `max_per_source`, the mining plan's sources with their tiles, trips, links, and paid miners) | Critical (the `hauling` job), a flow field to the controller container per build (heavy) | any key change; the container gone (the plan is dropped); a global reset | none | about 48 B per room plus about 120 B per source | the count and body only decide what is asked for; the scenario tests cover the carrying (`haulers_carry_a_sources_energy_to_the_controller_container`) |
| Controller container tile (P3.3) | owned room name, in the mining plan | the container's tile | Critical (the `mining` job), with the mining plan's flow field | every key of the mining plan, whose container count now also counts the stores within `RECEIVER_RANGE` of the controller; a global reset | none | 8 B | the tile only decides where a site goes |
| Hauler state (P3.3) | creep handle slot (with the handle) | mode (gather or deliver), source, this tick's task; per room, targets found unreachable and until when | Critical (the `hauling` job) | the handle changing (the creep died); a source leaving the plan; a global reset (rebuilt from the stores: a hauler with energy delivers, and the most wanted source) | `movement.route_retry_ticks` for an unreachable target | about 48 B per hauler | tasks are chosen again every tick from the snapshot |
| Upgrading plans (P3.4) | owned room name | the stores, the budget's two sums (what reaches the controller, the other creeps' bodies) and the sources counted, the trip from the spawn, the posts, the upgraders' body and count | Critical (the `upgrading` job), a flow field to the first store per build (heavy) | any key change (grid version, capacity, level, controller, stores, the two sums, the counted sources, `upgrading.free_tiles`); no store and no upgrader (the room is not selected and the plan dropped); a global reset | none | about 150 B per room (the body inline) plus 4 B per post, store, and counted source | the count and body only decide what is asked for; the scenario tests cover the upgrading (`upgraders_take_the_controllers_energy_from_their_posts`) |
| Upgrader state (P3.4) | creep handle slot (with the handle) | its post and last task; per room, posts found unreachable and until when, whether the room's upgraders took the controller's energy (`engaged`), and the last tick it had a miner and a hauler (the grace) | Critical (the `upgrading` job's posts), Standard (the tasks) | the handle changing (the creep died); a post leaving the plan; a global reset (posts taken again in id order; `engaged` again once an upgrader is seen) | `movement.route_retry_ticks` for an unreachable post | about 24 B per upgrader | tasks are chosen again every tick from the snapshot |
| Miner state (P3.1) | creep handle slot (with the handle) | the source it mines; per room, mining tiles found unreachable and until when | Critical (the `mining` job) | the handle changing (the creep died); a source leaving the plan; a global reset (a miner on a mining tile takes it again, the rest the nearest free source) | `movement.route_retry_ticks` for an unreachable tile | 24 B per miner | assignments are checked every tick against the plan |
| Worker state (P2.4) | creep handle slot (with the handle) | mode (gather or work), source, this tick's task, a target found unreachable and until when | Critical (the `opening` job) | the handle changing (the creep died); a stall drops the room's; a global reset (rebuilt from the stores: a worker with energy works) | `movement.route_retry_ticks` for an unreachable target | about 64 B per worker | tasks are chosen again every tick from the snapshot |
| Spawn holds (P2.3) | spawn handle | the tick from which a spawn whose call the game refused may take an order again; after it, the spawn is on probation (it takes orders after the room's other ready spawns) | the act step | an accepted call of its own (ends the probation), the spawn's handle being released, or a global reset (fails open: one refused call) | held for `hold_ticks`, then probation until an accepted call | 12 B per refused spawn | none: it is learned state |
| Fault log (P1.6) | one per global | fault totals, the current window's counts, up to `fault_reports_per_window` queued reports, up to 4 names of jobs whose faults were held back, the last fault | every tick (scheduler) | its window closing `fault_window_ticks` ticks after it opened, or `Game.time` falling before its start; a global reset clears it | the window | under 1 KB | none |

## 4. CPU tiers and budget
_P1._ Tier membership, the budget formula (with its sources), time slicing, and bucket policy (target, spend surplus, safety margin, degradation).

Current state (P1.6, `rustedscreep_core::scheduler`; values from `SCHEDULER_MMO`, rationale in TUNING.md):
- Tiers: Critical (spawning, defense, harvesting), Standard (hauling, upgrading, building, offense operations), Optional (planning,
  market, scouting, visuals (drawings in the game's room view), pixels (a resource made from bucket CPU), offense planning). A job declares its tier as a constant `Job { name, tier }`. A tick runs every
  subsystem's Critical jobs before any Standard job, and those before any Optional job: the tiers share one ceiling, so this order
  across subsystems is what gives Critical its priority, and `TickStats::out_of_order` counts breaches. The jobs are movement's room reads
  (Critical) and rescans (Standard), the persistence loads (Standard), the console's move order (Standard), and the constants
  self-check (Optional), plus the game jobs: the towers' plan (P7.2), the opening, mining, hauling, upgrading, and spawn plans (Critical), the opening's,
  hauling's, and upgrading's work and the link sends (Standard), and the opening's site placement and the base planner (Optional). A unit that does heavy work (a flow field, a detour, a room
  read) marks itself (`Commit::heavy`): the slicer reads the clock right after it, leaves that batch out of the cost estimate and
  makes the next batch one unit, so one heavy unit cannot carry a batch of cheap ones past the cap.
- Budget (`Budget::compute`, once per tick from the snapshot's tick context, no JS call). Every cap is a level of the whole tick's CPU
  (the latest `getUsed` reading plus committed CPU), and a tier runs while the tick is below its cap:
  - ceiling = tickLimit × (100 − `tick_reserve_pct`) / 100, 450 at tickLimit 500. Every tier stops there, Critical included
    (ADR-013): the engine kills a tick by wall time at tickLimit + 5 ms (V105) while `getUsed` counts CPU time and intents
    (V106). A killed tick is an aborted tick that the loader recovers from with a global reset (ADR-003, on how the loader recovers from a trap, a fatal wasm error, or an aborted tick), and while the bucket is
    below 500 − limit, tickLimit is bucket + limit, so a tick that runs past it without being killed is billed into a negative
    bucket and the next tick is skipped (V37).
  - Standard: on while bucket ≥ `standard_bucket_min` (2000; at least the loader's 480 boot gate plus `CPU_TICK_LIMIT_MAX`), capped at the ceiling.
  - Optional: on while bucket ≥ `optional_bucket_min` (3000) and the global has run `optional_warmup_loops` (1) loops, so the first
    tick after a reset never runs it. Cap = min(ceiling, limit + (bucket − `bucket_target`) / `surplus_spread_ticks`), off when not
    above 0.
- Bucket policy, from the refill rule bucket' = min(10000, bucket + limit − billed) (V37): at the target (9000) the Optional cap is
  the limit, so Optional work uses only what the tick would otherwise leave unused; below the target the cap shrinks and the bucket
  refills; above it the surplus is spent at a rate that falls with it (about `surplus_spread_ticks` ticks). The cap is checked
  during the decide step, so the work after it, the slicer's overshoot, and ceil billing (V106) come on top, and under heavy
  Optional demand the bucket settles slightly below the target. At the free tier's limit of 20: below 2000 only Critical runs; up
  to 7000 Critical and Standard; above that Optional too, with a cap of 20 at 9000 and 30 at 10000. Standard's gate is at least the
  loader's boot gate plus `CPU_TICK_LIMIT_MAX`, so one Standard tick cannot take the bucket below the level a reset needs to boot.
- Time slicing (`TickCpu`): `run_units` runs a job's units round robin from a saved cursor (units must come in a stable order,
  by handle or id, V102: the game's collections iterate in an order that may change between ticks); `run_steps` runs resumable steps of heavy work. The clock is read when a job starts (unless the tick is
  already at the job's cap, which needs no read) and after each batch. The first batch of every call is one unit, so a cost that
  changed since the last tick is measured first. After that a batch is at most `max_units_per_sample` (16) units, at most half of
  what is left before the cap, and at most a quarter of the reserve at the measured cost. So a cost that jumps up to 4 times passes
  the cap by at most half the reserve plus one unit (tested at a full and an empty bucket). Intents a unit queues for the act
  layer are committed at 0.2 CPU each (V34), so they count before `getUsed` sees them; the act layer issues them after the tick's
  `finish`, and the scheduler reads the clock no more (a profiler build reads it twice more, after the act step and the fault log,
  outside the budget).
- Clock readings are sanitized: monotone within the tick (a `cancelOrder` refunds 0.2, V106); a NaN, infinite, or negative reading
  keeps the last good one, and after `bad_samples_to_break` (2) such readings every tier stops for the rest of that tick
  (ADR-013).
- Sleep wheel (`SleepWheel<T, E>`): items sleep until a tick or an event, whichever comes first; owned by the subsystem that
  uses it. Wakes are correct across skipped ticks and never fire twice (V107: `Game.time` is the global tick counter, so it jumps over the ticks a player skips).
- Isolation: each unit returns `Result<(), Fault>`; a fault is counted, queued for the log within a rate limit shared by all
  jobs (`fault_reports_per_window` per `fault_window_ticks`), and the next unit runs. The note that closes a window gives the
  count it held back and names up to 4 of those jobs, so one stuck unit cannot hide a new failure. A failing step stops its step
  job for the tick. rustedscreep-wasm prints the reports with `log::warn!` (P1.10).
- All of it lives in the heap. A global reset fails open: cursors restart, every item is awake, Optional is in warm-up.
- Not yet: per-tier and per-job CPU accounting (planned for P2, still not built although the jobs exist: from `TickCpu`'s samples at job start and after each batch, with no
  extra `getUsed` calls, likely as stats segment format v2; P1.8's profiler has one `decide` span over every tier).
- Every threshold above can be overridden at runtime under its key (`scheduler.<field>`, P1.10; TUNING.md's runtime-key column):
  the value is clamped to its safe range, and a change that breaks a rule (Standard gate ≥ boot gate + `CPU_TICK_LIMIT_MAX`, Standard
  ≤ Optional ≤ target) is refused. A change applies from the next tick.

## 5. Persistence
_P1._ Segment map, format versions, dirty policy, rebuild path.

Current state (P1.10 and ADR-027; the manager and the frame are P1.9, ADR-004, the encoding: base64 with a CRC-32 checksum):

| Segment | Owner | Format | Written | Read |
|---|---|---|---|---|
| 95 (`PersistConfig::sign_segment`) | the controller signer's record (`signing::Book`, ADR-030) | frame kind 4, schema 1; payload `count:u16`, then per room `room:u16 state:u8 seen:u32 controller:u16 deaths:u8 overwrites:u8 n:u8 tower:u16*n` (12 bytes a room, 2 per tower) | when an entry changes, at most once per `sign.save_ticks`, on a tick that runs the Standard tier; never in a global whose load did not get the segment's text | requested once a home reaches `sign.min_level`; no signer is planned before the load ends; an empty segment means no record, a refused one is logged and written again |
| 96 (`PersistConfig::planner_segment`) | the planner's record (`planner::record`, ADR-027) | frame kind 3, schema 1; payload `count:u16`, then per owned room `room:u16 anchor:u16 n:u8 (kind:u8 tile:u16)*n` (the packed room name, V255; the plan's anchor and adopted container and link tiles) | when a room's entry changes (a plan ready with another anchor or adopted tiles, a room no longer owned), or at once after a refused record, on the next tick that runs the Standard tier; never in a global whose load did not get the segment's text | once per global, as 97; no room is planned until it has loaded |
| 97 (`PersistConfig::config_segment`) | runtime config overrides (`config::overrides`) | frame kind 2, schema 1; payload ASCII `key=value\n` lines in field order | after a console change, on the next tick that runs the Standard tier | once per global: requested at the start, looked up on the first loop, read on the next after the activation |
| 98 (`PersistConfig::self_test_segment`) | the self-test (`rustedscreep.persistTest`) | frame kind 1, a test pattern | on the tick after the command | on the tick after the write |
| 99 (`ProfilerConfig::stats_segment`) | the profiler (`profiler` builds) | JSON, format v1 (`profiler/text.rs`) | once per 1000-loop window, without activation | never by the bot (read in the console, TEST_PLAN §8); the manager reserves it |

- Frame (ADR-004): `rs1b` + base64 of a 16-byte header (kind, schema, write tick, length, CRC-32) and the payload. Every decode
  failure is an error the owner answers by rebuilding: for the config record, the profile's values apply (an empty segment means no
  overrides; anything else that is not a valid record is logged once and its overrides are not used); for the planner's record, rooms
  are planned from what stands (an empty segment means no record; a refused one is logged once and written again with what this
  global plans; a segment never read is logged once and left as it is). The record's lines are
  checked again on load like a console change: unknown keys, fields only a profile sets, and bad values are dropped with a note,
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
_P1._ Error isolation boundaries, loader recovery (ADR-003), reset behavior, respawn (the player starting over with a new first spawn), shard travel (moving between shards, the game's separate worlds), missing Memory.

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
_P10–P11 (the offense core and the siege breaker)._ State machines, formations (squads that move as one), the breaker solver (which sizes the body of a creep that dismantles enemy structures), budgets, and safety gates.
