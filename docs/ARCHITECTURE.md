# Architecture (SPEC §6.1)

Claude Code fills each section in as its phase lands. Keep it factual: describe only what the code does.

## 1. Per-tick data flow: snapshot → decide → act
_P1._ A diagram plus a step list: loader → `loop` → tick context → snapshot → caches → scheduler tiers → decisions →
intent buffer (no-op filtering, pipeline check) → act → persistence flush → profiler flush. Include JS crossings per step.

Current state (P1.5): loader → `loop` (rustedscreep-wasm `game_loop`) → snapshot, and nothing after it yet. The snapshot
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
Each id or name copy is its own call (`__wbindgen_string_get`) because that call is the type check (V100). No intents, no Memory
access; on a steady tick the only Rust allocations are the copied id and name strings (the tables keep their capacity). This tick's JS objects stay in `JsWorld`, row
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

## 4. CPU tiers and budget
_P1._ Tier membership, the budget formula (with its sources), time slicing, and bucket policy (target, spend surplus, safety margin, degradation).

## 5. Persistence
_P1._ Segment map, format versions, dirty policy, rebuild path.

## 6. Failure handling
_P1._ Error isolation boundaries, loader recovery (ADR-003), reset behavior, respawn, shard travel, missing Memory.

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
