# Architecture (SPEC §6.1)

Claude Code fills each section in as its phase lands. Keep it factual: describe only what the code does.

## 1. Per-tick data flow: snapshot → decide → act
_P1._ A diagram plus a step list: loader → `loop` → tick context → snapshot → caches → scheduler tiers → decisions →
intent buffer (no-op filtering, pipeline check) → act → persistence flush → profiler flush. Include JS crossings per step.

Current state (P1.7): loader → `loop` (rustedscreep-wasm `game_loop`) → snapshot → the intent buffer opened for it → scheduler tick (the decide step: no job until P2; until then only the console's `say` queues intents) → act step → fault log. The snapshot
(`rustedscreep-wasm/src/snapshot.rs` filling `rustedscreep_core::snapshot::Snapshot`) makes one pass, in this order:
1. Tick context: `Game.time`, `Game.cpu.limit`, `tickLimit`, `bucket` (4 JS calls), sanitized by `TickContext::from_raw`.
2. `Game.creeps` (own creeps, spawning ones included; V101): 3 calls for the collection (getter, `Object.values`, `length`),
   then per creep 6 (`Array.get`, `id`, the id string copy, `pos`, `__packedPos`, `spawning`).
3. `Game.rooms`: 3 calls, then per room 8 with a controller (`get`, `name`, the name copy, `controller`, `my`, `level`, `find`,
   `length`) or 6 without; per hostile 5 (`get`, `id`, the id copy, `pos`, `__packedPos`; hostile results never include spawning
   creeps, V101).
4. `Game.structures` (own structures; V101): 3 calls, then per structure 3 (`get`, `id`, the id copy), plus on the tick it is
   first seen `structureType`, and `pos` and `__packedPos` when the type is known (again every tick while that read fails).
Calls back from JS into wasm: one `__wbindgen_malloc` per id or name copy, and one `__externref_table_alloc` (with a JS
`Table.set`) per object returned through an `Option` (every creep's `id`, a room's `controller` when present).
5. `finish`: handles of objects the pass did not push are released.
6. Scheduler (P1.6, `rustedscreep-wasm/src/scheduler.rs` over `rustedscreep_core::scheduler`): `Scheduler::begin` turns the
   snapshot's tick context into the tick's budget (no JS call, §4). No job runs under it until P2. Ending the tick records it for
   `rustedscreep.scheduler()`, whose per-tick `faults` counts job faults only (act faults come later, step 7, and show in the
   fault totals and lines). A time-sliced job adds one `Game.cpu.getUsed()` per clock sample.
7. Act (P1.7, `rustedscreep-wasm/src/act.rs` over `rustedscreep_core::intents`): during the decide step, jobs push intents by creep
   handle into the `IntentBuffer`, which commits 0.2 CPU per new intent that is not free (V34) and refuses, so that nothing is queued
   and nothing earlier replaced: calls the game API refuses (an actor that is gone, hostile or spawning; a target that is gone; a
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
8. The fault reports queued this tick (jobs' and the act step's) go to `console.log`.
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

Act layer (P1.7): `rustedscreep_core::intents` holds the engine's creep intent-priority table as data (`CreepAction`, `ActionSet`,
`ActionSet::executed` as the engine model, `PRIORITY_ORDER` and `ActionSet::kept` for what is issued, with the engine commit), the
per-tick `IntentBuffer` with its `CreepIntent` variants (move by direction, move toward a creep, say, drop, transfer, withdraw, heal,
rangedHeal, repair, upgradeController, pull; harvest, build and pickup arrive with their target tables in P2, attacks with the ally
check in P7 and P10, structure actors such as `spawnCreep` in P2) and its gated `issue` walk, the return-code classifier `Outcome`,
and the counters `ActLog`. rustedscreep-wasm's `act.rs` binds the eleven methods and makes the calls; `act_test.rs` (feature
`act-test`, refused by `npm run stage` for deploy/) is a scripted producer for the sandbox test.

js/ (P1.3): `js/src/main.ts` is a build-pipeline placeholder until the loader lands (P1 module 4). `js/tools/` holds the build
(cargo → wasm-bindgen `--target web` → `wasm-opt -O4` → tsc → Rollup CJS into `dist/`), stage (into `deploy/` for the GitHub link),
size, check, smoke (the bundle in a Screeps-like sandbox, run at the end of every build) and setup-binaryen. Generated files go to the git-ignored `js/pkg/`,
`js/build/` and `dist/`.

## 3. Cache and invalidation table
_P1 onward._

| Cache | Key | Contents | Built in tier | Invalidated by | TTL | Approx. size | Toggle (naive mode) |
|---|---|---|---|---|---|---|---|
| Object handles, creeps and structures (P1.5) | packed object id (`ObjectKey`, u128) | per slot: generation, last pass seen, row, live flag; FxHashMap id → slot | Critical (snapshot, every tick) | the first pass that does not push the object: the slot is released and its generation advances, so old handles stop resolving; free slots are reused oldest first; a slot whose generation would wrap is retired | none (pass-based) | about 48 B per slot plus a map entry | none: handles are the identity scheme, not an optimization |
| Structure static data (P1.5) | structure handle | `structureType` and position | Critical (snapshot), read only when a structure is first seen | the structure's handle being released (destroyed, or missing from one pass) | none | 8 B per slot | none yet; the toggle to re-read every tick arrives with runtime config (P1.10) |
| Job state (P1.6) | one per job, owned by its subsystem | round-robin cursor, measured CPU per unit | the job's tier | nothing during a global; a global reset restarts it at unit 0 with no estimate (only costs extra clock samples on that tick) | none | 16 B per job | none |
| Sleep wheel (P1.6) | item (a handle or a job) | wake tick and an optional event | the owner's tier | the wake tick passing (gaps included), its event firing, a new sleep or a cancel; time going back wakes everyone; a global reset empties it, so every item starts awake | none | about 28 B per entry, 256 slot vectors, a heap for wakes beyond 256 ticks, one list per event being waited for (emptied lists are reused, at most 64 kept) | none |
| Intent buffer (P1.7) | creep row in this tick's snapshot | every accepted push of the tick (actor and target rows, the intent), the methods queued per row, the latest push per row and method, and the planned issues | the decide step (any tier) and the act step | `IntentBuffer::begin` each tick, after the snapshot; a push before `begin`, after the plan, or with a snapshot whose `Game.time` or creep count differs from the one `begin` saw is refused (`closed`) | one tick | 44 B per push and per issue (`Issue`, measured natively; it holds no pointers), 4 B per row, plus 92 B per row for the latest-push slots, which only grow (the rows' method sets say which are this tick's) | none: it holds one tick's intents |
| Fault log (P1.6) | one per global | fault totals, the current window's counts, up to `fault_reports_per_window` queued reports, up to 4 names of jobs whose faults were held back, the last fault | every tick (scheduler) | its window closing `fault_window_ticks` ticks after it opened, or `Game.time` falling before its start; a global reset clears it | the window | under 1 KB | none |

## 4. CPU tiers and budget
_P1._ Tier membership, the budget formula (with its sources), time slicing, and bucket policy (target, spend surplus, safety margin, degradation).

Current state (P1.6, `rustedscreep_core::scheduler`; values from `SCHEDULER_MMO`, rationale in TUNING.md):
- Tiers: Critical (spawning, defense, harvesting), Standard (hauling, upgrading, building, offense operations), Optional (planning,
  market, scouting, visuals, pixels, offense planning). A job declares its tier as a constant `Job { name, tier }`. A tick runs every
  subsystem's Critical jobs before any Standard job, and those before any Optional job: the tiers share one ceiling, so this order
  across subsystems is what gives Critical its priority, and `TickStats::out_of_order` counts breaches. No job exists yet; P2 adds
  the first ones.
- Budget (`Budget::compute`, once per tick from the snapshot's tick context, no JS call). Every cap is a level of the whole tick's CPU
  (the latest `getUsed` reading plus committed CPU), and a tier runs while the tick is below its cap:
  - ceiling = tickLimit × (100 − `tick_reserve_pct`) / 100, 450 at tickLimit 500. Every tier stops there, Critical included
    (ADR-013): the engine kills a tick by wall time at tickLimit + 5 ms (V105) while `getUsed` counts CPU time and intents
    (V106). A killed tick is an aborted tick that the loader recovers from with a global reset (ADR-003), and while the bucket is
    below 500 − limit, tickLimit is bucket + limit, so a tick that runs past it without being killed is billed into a negative
    bucket and the next tick is skipped (V37).
  - Standard: on while bucket ≥ `standard_bucket_min` (2000, above the loader's 1500 boot gate), capped at the ceiling.
  - Optional: on while bucket ≥ `optional_bucket_min` (3000) and the global has run `optional_warmup_loops` (1) loops, so the first
    tick after a reset never runs it. Cap = min(ceiling, limit + (bucket − `bucket_target`) / `surplus_spread_ticks`), off when not
    above 0.
- Bucket policy, from the refill rule bucket' = min(10000, bucket + limit − billed) (V37): at the target (9000) the Optional cap is
  the limit, so Optional work uses only what the tick would otherwise leave unused; below the target the cap shrinks and the bucket
  refills; above it the surplus is spent at a rate that falls with it (about `surplus_spread_ticks` ticks). The cap is checked
  during the decide step, so the work after it, the slicer's overshoot and ceil billing (V106) come on top, and under heavy
  Optional demand the bucket settles slightly below the target. At the free tier's limit of 20: below 2000 only Critical runs; up
  to 7000 Critical and Standard; above that Optional too, with a cap of 20 at 9000 and 30 at 10000. Standard's gate is the
  loader's boot gate plus `CPU_TICK_LIMIT_MAX`, so one Standard tick cannot take the bucket below the level a reset needs to boot.
- Time slicing (`TickCpu`): `run_units` runs a job's units round robin from a saved cursor (units must come in a stable order,
  by handle or id, V102); `run_steps` runs resumable steps of heavy work. The clock is read when a job starts (unless the tick is
  already at the job's cap, which needs no read) and after each batch. The first batch of every call is one unit, so a cost that
  changed since the last tick is measured first. After that a batch is at most `max_units_per_sample` (16) units, at most half of
  what is left before the cap and at most a quarter of the reserve at the measured cost. So a cost that jumps up to 4 times passes
  the cap by at most half the reserve plus one unit (tested at a full and an empty bucket). Intents a unit queues for the act
  layer are committed at 0.2 CPU each (V34), so they count before `getUsed` sees them; the act layer issues them after the tick's
  `finish`, and nothing reads the clock after that.
- Clock readings are sanitized: monotone within the tick (a `cancelOrder` refunds 0.2, V106); a NaN, infinite or negative reading
  keeps the last good one, and after `bad_samples_to_break` (2) such readings every tier stops for the rest of that tick
  (ADR-013).
- Sleep wheel (`SleepWheel<T, E>`): items sleep until a tick or an event, whichever comes first; owned by the subsystem that
  uses it. Wakes are correct across skipped ticks and never fire twice (V107).
- Isolation: each unit returns `Result<(), Fault>`; a fault is counted, queued for the log within a rate limit shared by all
  jobs (`fault_reports_per_window` per `fault_window_ticks`), and the next unit runs. The note that closes a window gives the
  count it held back and names up to 4 of those jobs, so one stuck unit cannot hide a new failure. A failing step stops its step
  job for the tick. rustedscreep-wasm prints the reports with `console.log` until `log` + `fern` arrive (P1.10).
- All of it lives in the heap. A global reset fails open: cursors restart, every item is awake, Optional is in warm-up.
- Not yet: per-tier CPU accounting (profiler, P1.8), runtime overrides of the thresholds (P1.10), the first jobs (P2).

## 5. Persistence
_P1._ Segment map, format versions, dirty policy, rebuild path.

## 6. Failure handling
_P1._ Error isolation boundaries, loader recovery (ADR-003), reset behavior, respawn, shard travel, missing Memory.

Act step (P1.7): a push that the game API would refuse, or that would be a billed no-op the snapshot can already see, is refused in
Rust and counted; other refusals come back from JS as known codes and are counted as failed. An intent call that throws in JS is
caught by its binding (`catch`), logged as a rate-limited fault under job `act`, and the next issue runs; a throw counts as not
recorded, like the engine's own throw paths (V108). A return value that is not a known code is counted as unknown and treated as
recorded. An issue whose JS object is missing is a fault, counted as missing, and makes no call. None of these traps.

Loader (P1.4, js/src/loader.ts): any throw from the wasm, or a tick that ended inside it (the `running` flag still set), poisons
the instance. The loader records the incident in `Memory.rustedscreepLoader` on that tick and recovers on the next: `Game.cpu.halt()`
by default (a global reset and a recompile), or `__wbg_reset_state` (a new instance from the cached module) in reinstantiate mode or
when the server has no halt. Compile and load failures do not halt; they back off. From the third consecutive incident the next
boot waits, doubling up to a cap (the streak is kept in Memory), so a crash loop cannot drain the bucket, and the bot retries
without human action; a capped re-instantiation without halt waits for its window. The loader itself never throws out of `loop`.
Every read of the loader record repairs it (`sanitizeLoaderMemory`), and a `Memory` that is not an object (null after invalid JSON)
falls back to a heap record and one notification.

## 7. Offense subsystem
_P10–P11._ State machines, formations, the breaker solver, budgets, and safety gates.
