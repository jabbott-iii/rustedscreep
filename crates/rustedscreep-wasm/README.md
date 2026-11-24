# rustedscreep-wasm

The WebAssembly (wasm) module of rustedscreep, a bot for [Screeps: World](https://screeps.com). Screeps is an MMO
strategy game in which players write code that runs their units, called creeps. This crate is the bot's boundary with
the game: it reads the game's objects, hands them to the bot's logic, and issues the logic's decisions back to the
game.

It is part of [rustedscreep](https://github.com/jabbott-iii/rustedscreep). The decisions are made in
[rustedscreep-core](https://crates.io/crates/rustedscreep-core), which never calls the game, and the game's constants
come from [rustedscreep-consts](https://crates.io/crates/rustedscreep-consts). This crate is the only one that uses
`wasm-bindgen`, `js-sys`, or the game objects of
[screeps-game-api](https://crates.io/crates/screeps-game-api) (the Rust bindings to the game's API).

## Status

Version 0.1. The crate builds a `cdylib` (a library compiled into a standalone wasm module) for
`wasm32-unknown-unknown`, so no Rust crate depends on it. It is published so that each release's source is on
crates.io. To run the bot, use the files of a
[GitHub release](https://github.com/jabbott-iii/rustedscreep/releases), or build them from the repository.

## What it does each tick

The game calls the exported `loop` once per tick (one game turn), through the repository's TypeScript loader. The tick
runs in three steps:

1. **Snapshot.** One pass over the game's objects copies what the bot needs into rustedscreep-core's tables: one array
   per field, with the objects named by numeric handles. Each JavaScript call costs CPU, so the snapshot reads each
   value once and later steps read the tables.
2. **Decide.** rustedscreep-core's scheduler runs the bot's work in three CPU tiers, chosen by the bucket (the CPU that
   earlier ticks left unused, which the game banks for later). Critical work (spawning, defense, and harvesting)
   always runs. Standard work (hauling, upgrading, and building) runs unless the bucket is critically low. Optional
   work (planning, scouting, and the controller signer) runs only above bucket thresholds.
3. **Act.** The decisions become intents (the commands, such as a move or a harvest, that the game applies once the
   tick's code has run). Intents are de-duplicated and checked against the engine's rules for which intents block
   others, and intents that would change nothing are skipped, since each one costs a fixed amount of CPU. The rest are
   issued in one batch.

Between ticks it keeps its state in Rust memory. A global reset (the game discarding the bot's JavaScript environment
and loading the code again) loses that memory, and the next ticks rebuild it. What must survive a reset is kept in
RawMemory segments, the numbered strings the game stores between ticks, in a versioned binary format that is written
only when it changed:

| Segment | What it holds |
| :- | :- |
| 95 | The controller signer's record: the rooms it signed or gave up on, and when each is due again |
| 96 | The base planner's record: each room's plan anchor and the structures it adopted |
| 97 | The runtime config overrides set from the console |
| 98 | The test pattern of `rustedscreep.persistTest()` |
| 99 | The profiler's statistics, in a `profiler` build |

It never reads `Memory` (the game's JSON store, which costs CPU to parse) for its state. It writes there only to undo
the `Memory.creeps` entry that `spawnCreep` adds, and to reset a `Memory` that does not parse.

A room, creep, or task that fails is logged, with a rate limit, and skipped. Nothing in the crate's non-test code
unwraps or panics, because the release profile sets `panic = "abort"`: a panic would trap (abort the wasm call). The
loader recovers from a trap, by default by halting so that the next tick starts a new global.

## Exports

Besides `loop`, the module exports one function per console command. The loader wraps them as `rustedscreep.*` in the
game's console, for example `rustedscreep.scheduler()` for `schedulerSummary`:

| Export | Console command | What it gives |
| :- | :- | :- |
| `snapshotSummary` | `rustedscreep.snapshot()` | The tick's table sizes, handle churn, and skipped objects |
| `schedulerSummary` | `rustedscreep.scheduler()` | The last tick's tier caps, what ran, and the fault counts |
| `actSummary` | `rustedscreep.act()` | The last tick's intents: queued, refused, blocked, and issued, with their results |
| `movementSummary`, `moveTo` | `rustedscreep.movement()`, `rustedscreep.moveTo(room, x, y, range, from)` | Rooms, grids, fields, and traffic; a console order that walks creeps to a tile |
| `spawnerSummary`, `spawn` | `rustedscreep.spawner()`, `rustedscreep.spawn(room, role)` | The spawn queue; a console spawn request |
| `openingSummary` | `rustedscreep.opening()` | Each owned room's workers and tasks, its spawn request, and its watchdog |
| `plannerSummary` | `rustedscreep.planner()` | Each room's plan, its placement, and the CPU per planner step |
| `remoteSummary` | `rustedscreep.remote()` | The remote rooms, their figures, their creeps, and the sites placed |
| `signSummary` | `rustedscreep.sign()` | The signer's record, its trip, and the next signer's plan |
| `persistSummary`, `persistTest` | `rustedscreep.persist()`, `rustedscreep.persistTest(bytes)` | The segments in use; a write and read-back test |
| `configSummary`, `configSet`, `configUnset`, `configReset` | `rustedscreep.config()` and the same names | The config in force; set, remove, or clear overrides |
| `constsSummary` | `rustedscreep.consts()` | The constants self-check: this build's constants against the server's |
| `profilerSummary` | `rustedscreep.profiler()` | CPU per subsystem, in a `profiler` build |
| `queueSay` | `rustedscreep.say(text)` | A creep says the text on the next tick, which checks the act path |

## Building

The repository's `npm run build` builds it. Building the crate alone gives the wasm, but not the loader that runs it:

1. `cargo build --release --target wasm32-unknown-unknown -p rustedscreep-wasm`, with the toolchain that
   `rust-toolchain.toml` pins;
2. the wasm-bindgen CLI at exactly the version in `Cargo.lock` (0.2.130), with `--target web`;
3. `wasm-opt -O4` from binaryen version_133;
4. the TypeScript loader, bundled by Rollup into `dist/main.js`, and a smoke test in a model of the game's runtime.

`npm run stage` runs that build and writes `deploy/`: `main.js`, and the wasm as base64 text in modules of at most 905709 bytes
each (`rustedscreep_wasm.js`, `rustedscreep_wasm_1.js`, and so on). The game's GitHub sync delivers each file as a text
module, so binary files do not survive it, and 905709 bytes is the largest module it has been seen to deliver intact.

## Features

All are off by default. Each one adds `+<feature>` to the build ID that the boot line prints.

- `profiler`: CPU spans per subsystem (six `Game.cpu.getUsed()` calls per call of `loop`) and the statistics in
  segment 99, written once every 1000 calls. Off, it compiles out.
- `trap-test`: `rustedscreep.trapTest(n)`, which forces traps, to test the loader's recovery. In this build the loader
  also reads `Memory` every tick.
- `act-test`: a scripted intent producer for the act layer's sandbox test. The stage step refuses it, so it never
  reaches a server.

## Documentation

- Design: https://github.com/jabbott-iii/rustedscreep/blob/main/docs/ARCHITECTURE.md
- Setting up the bot: https://github.com/jabbott-iii/rustedscreep#readme
- API reference: https://docs.rs/rustedscreep-wasm

## License

MIT: see [LICENSE](LICENSE).
