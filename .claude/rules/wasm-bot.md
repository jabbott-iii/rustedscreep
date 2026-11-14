---
paths:
  - "crates/rustedscreep-wasm/**"
---

# WASM boundary crate (`rustedscreep-wasm`)

## Snapshot (JS → Rust), once per tick
- Make one pass over owned rooms, creeps, structures, and visible hostiles. Read each JS property once. Every getter, such as
  `creep.pos()` or `store()`, is a JS crossing, so cache the result in the snapshot struct.
- Convert to `rustedscreep-core` types immediately. Intern object IDs to `u32` handles. Game objects go stale next tick, so
  keep JS handles only for the current tick and re-resolve by ID when needed.
- Never call `find`/`look*` inside loops. Use one bulk call per room per tick, or cached static data (terrain, sources,
  minerals, controller position) that is invalidated by events.

## Act (Rust → intents), one batch
- Issue intents from the intent buffer produced by `rustedscreep-core`. Before issuing, drop no-ops: moving while fatigued,
  repairing full hits, transferring into a full store, or re-issuing an identical order to an idle structure.
- Enforce the engine intent-priority table encoded in `rustedscreep_core::intents`, and never emit a combination that blocks itself.
- One movement intent per creep. Prefer `move(direction)` computed from Rust paths and flow fields over `moveTo`,
  which pathfinds in JS. Use `PathFinder.search` only for inter-room routes, with Rust-built cost matrices and cached results.
- Count intents per tick in the profiler. The intent count is the main CPU cost to minimize.

## Memory and persistence
- Never touch `Memory` (`screeps::memory`) in the tick path. The wrapper JSON-parses on first access each tick.
- Persist only what must survive a reset, in RawMemory segments. Each segment carries a versioned header (magic, format version,
  length, checksum). Write only when dirty. Request segments ahead of use (verify how long activation takes).
- A decode failure or version mismatch triggers a rebuild from game state, logged once. Never panic.
- Encode role, home room, and serial in creep names so identity survives resets without Memory.

## Errors, logging, resets
- Process each room, creep, and task in its own `Result` boundary. A failure is logged with a rate limit and skipped. The tick continues.
- Same no-panic clippy denies as `rustedscreep-core`, at the crate root for non-test code.
- Logging goes through `log` + `fern`, with the level from config. Guard expensive formatting with `log::log_enabled!`.
- On a global reset, initialize in small steps across ticks: Critical tier first, caches rebuilt lazily by tier.
  The first tick after a reset must not run Optional work.

## Profiler and features
- The `profiler` feature compiles in sparse `Game.cpu.getUsed()` sampling per subsystem. Results go to a RawMemory stats segment
  that `npm run stats` can fetch. Without the feature, the profiler compiles out to nothing.
- Features: `mmo` and `sim` (map them to the `screeps-game-api` features of the same name, after verifying the names for the pinned
  version), `profiler`, `visuals`, and `trap-test` (a debug export that traps on demand, used only to test loader recovery).
