# Architecture (SPEC §6.1)

Claude Code fills each section in as its phase lands. Keep it factual: describe only what the code does.

## 1. Per-tick data flow: snapshot → decide → act
_P1._ A diagram plus a step list: loader → `loop` → tick context → snapshot → caches → scheduler tiers → decisions →
intent buffer (no-op filtering, pipeline check) → act → persistence flush → profiler flush. Include JS crossings per step.

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
rustedscreep-core must avoid. rustedscreep-core has no modules or dependencies yet; rustedscreep-wasm has only an empty `loop` export.

js/ (P1.3): `js/src/main.ts` is a build-pipeline placeholder until the loader lands (P1 module 4). `js/tools/` holds the build
(cargo → wasm-bindgen `--target web` → `wasm-opt -O4` → tsc → Rollup CJS into `dist/`), stage (into `deploy/` for the GitHub link),
size, check, smoke (the bundle in a Screeps-like sandbox, run at the end of every build) and setup-binaryen. Generated files go to the git-ignored `js/pkg/`,
`js/build/` and `dist/`.

## 3. Cache and invalidation table
_P1 onward._ | Cache | Key | Contents | Built in tier | Invalidated by | TTL | Approx. size | Toggle (naive mode) |

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
