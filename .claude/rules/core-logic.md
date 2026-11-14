---
paths:
  - "crates/rustedscreep-core/**"
  - "crates/rustedscreep-consts/**"
---

# Pure logic crates (`rustedscreep-core`, `rustedscreep-consts`)

## Purity (a hook enforces this)
- No `wasm-bindgen`, `js-sys`, or `web-sys` anywhere in these crates.
- `rustedscreep-core` never names `screeps::`. Game constants and pure local types come in through `rustedscreep_consts`.
- `rustedscreep-consts` may re-export `screeps-game-api` constants and pure `local` types (positions, room names, terrain/cost-matrix
  value types). It must not touch JS-backed modules: `game`, `objects`, `memory`, `raw_memory`, `pathfinder`, `console`, and similar.
  ADR-005 records whether `screeps-game-api` builds and tests on the native host. If it does not, `rustedscreep-consts` mirrors the values
  in `extra.rs` with sources, and the in-game self-check (P1) catches drift.

## Determinism
- No wall clock and no ambient randomness. Pass in a seeded RNG and the tick number.
- Decision outputs must not depend on hash-map iteration order. Iterate sorted keys or indexed `Vec`s.
- A given snapshot and config must always produce the same intents. Tests rely on this.

## Data-oriented, allocation-free hot paths
- Use struct-of-arrays and numeric handles (`u16`/`u32` newtypes), not `String` IDs or `Rc` graphs.
- Store room grids as `[u8; 2500]` or `[u16; 2500]`, indexed by a `RoomXY`-style newtype whose constructor guarantees
  `x, y < 50` so indexing is provably in bounds.
- Reuse scratch buffers. Pass `&mut Scratch`, call `clear()`, and do not reallocate. Use `SmallVec`/`ArrayVec` for bounded small sets
  and an Fx hasher for maps (dependency choices go in ADR-006).
- Use enums and `match`, not trait objects, on hot paths. Keep generic code monomorphized and small.
- Do not format strings or allocate in per-creep or per-tile loops.

## No panics
- Non-test code uses `Result`/`Option`, checked or saturating arithmetic where overflow is possible, and `get()` or proven-in-bounds newtypes.
- Crate roots set `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::todo, clippy::unimplemented, clippy::unreachable))]`.
  `assert!`-family macros, division by zero and overflow are not covered by these lints, so avoid them in non-test code by hand.

## Formulas and tests
- Every non-trivial formula has a doc comment that lists the constants it uses (`/// Uses: rustedscreep_consts::X, rustedscreep_consts::Y`) and
  a source link for behavior the constants alone don't define.
- Tests derive expected values from those constants, never from bare literals. A balance change should show up as a failing test,
  not as silent drift. The only exception is a pinned example taken from a cited source; cite it in the test.
- Solvers and simulators (body solver, hauler sizing, tower/heal sim, breaker solver, formations) get property tests
  covering invariants such as body length ≤ max parts, cost ≤ budget, monotonicity, and no NaN.
- Criterion benches for the planner, flow field, and combat sim run on native targets only.

## Configuration
- Every threshold is a field in `config.rs` with a doc comment giving its default, unit, rationale, and safe range.
  Mirror it in `docs/TUNING.md`. A doc-sync test fails when they diverge.
- Runtime overrides (from a RawMemory segment) are validated and clamped to the safe range. An invalid override is
  logged and ignored, never trusted.
