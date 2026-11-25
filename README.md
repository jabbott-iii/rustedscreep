# rustedscreep

rustedscreep is a bot for [Screeps: World](https://screeps.com), the MMO strategy game in which you write the code
that runs your units, called creeps, instead of steering them by hand. The bot is written in Rust and compiled to
WebAssembly (wasm), and a small TypeScript loader runs it on the game's servers.

Its goal is to grow its colony as fast as the game allows, at the lowest CPU per tick. A tick is one game turn, and
the game rations each player's CPU time per tick, so CPU spent on wasted work is CPU the colony cannot use.

## Status

Version 0.1. The bot is built one phase of the [roadmap](docs/ROADMAP.md) at a time, P0 to P12, and tested live on
shard3 of the official MMO server, with 20 CPU per tick (the limit of an account without a subscription). Phases P0
to P6 have passed, and P7, defense, is under way. [`docs/STATUS.md`](docs/STATUS.md) has the details.

What it does today:

- **Runs a room from its first spawn.** With no creeps, it spawns workers that harvest, fill the spawn, upgrade the
  room controller, and build. As the room controller level (RCL) rises, static miners take over the sources, and
  haulers, links, and upgraders take over moving the energy. Each creep's body is sized to its job.
- **Plans the whole base:** spawns, extensions, roads, towers, and the rampart line. It places construction sites
  from the plan and keeps the ramparts and walls repaired.
- **Mines neighboring rooms it does not own** (remote mining): it scouts them, picks the ones that pay, and sends
  reservers, miners, and haulers. Their creeps come home when armed hostiles show up.
- **Defends:** it assesses each hostile creep from its body and boosts, and its towers fire, focused by a combat
  simulation, and heal.
- **Signs controllers** in reach with a link to this repository.
- **Spends CPU by tiers.** Critical work (spawning, defense, and harvesting) always runs. Standard work (hauling,
  upgrading, and building) stops when the bucket (the CPU the game banks from earlier ticks) is critically low.
  Optional work (planning, scouting, and the signer) runs only when the bucket has CPU to spare.

Not built yet:

- claiming a second room (P8);
- the storage, the terminal, labs, and the market (P9). The planner can place the storage, but `planner.storage` is
  off by default;
- attacking other players (P10 and P11), and room visuals (P12).

## Run it on the MMO

You need a Screeps: World account and a room. The bot does not choose its first room: spawn in the game (pick a room
and place your first spawn) before or after you load the code. The bot is tested only on shard3. Its wasm uses
features (such as reference types and bulk memory) that only shard3's runtime has been checked for.

There are two ways to load it. Either way, the bot takes over a whole code branch.

### With the GitHub link (how this repository deploys)

The game can sync a code branch from a folder of a GitHub repository. Every push to the repository's `main` then
uploads that folder's files, so a push is a deploy.

1. **Fork this repository.** Its `deploy/` folder holds the last checked build, as the MMO runs it.
2. **Link the fork.** In the game client, open Account → GitHub. Under "Sync from repository", enter your fork as the
   repository and `deploy` as the folder, and press Sync. The client warns: "All your scripts in Screeps account will
   be overwritten with this action!" It means it: the sync replaces your `default` branch with exactly the files in
   `deploy/`, and deletes every other module, including ones you added in the in-game editor.
3. **Make `default` the active branch,** since that is the branch the sync writes.

The code runs within seconds of a push. Every push to `main` reloads the code, even one that leaves `deploy/` as it
was: the game discards the bot's JavaScript environment (a global reset), and the next tick compiles the wasm again,
for about 17 CPU from the bucket.

### With the files of a release

Each [release](https://github.com/jabbott-iii/rustedscreep/releases) holds the same files as `deploy/` at its tag:
`main.js`, `rustedscreep_wasm.js`, and, when the wasm needs more than one module, `rustedscreep_wasm_1.js` and so on.
The release notes give their SHA-256 sums.

Upload them together as one branch's modules. Each file becomes the module named after it without `.js` (`main`,
`rustedscreep_wasm`, `rustedscreep_wasm_1`), and `main` is the entry module. The modules are text: `main.js` decodes the
wasm from the others' base64 when the bot boots.

The Steam client's local folder is one way to upload them. The client uploads every top-level `.js` file in
`<app data>/scripts/screeps.com/<branch>/` (on Linux, `~/.config/Screeps/scripts/screeps.com/<branch>/`) as soon as
the folder changes, and deletes the modules of files you remove. So far only the GitHub link has delivered these files
to the MMO.

The files together take up most of the MMO's code limit, about 2 MB for all of a branch's modules, so the branch has
no room for another large module.

### Check that it booted

The first tick with the new code prints a boot line in the game's console, like:

```text
rustedscreep boot: build f24faa96f537-dirty-8ea89287 | Game.time 84448704 | wasm String(base64, 1721084 chars in 2 parts) 1290811 B crc32 8ea89287 (stage 8ea89287: match) | ...
```

The build ID starts with the commit the build came from. `match` says the wasm decoded from the modules has the
checksum (CRC-32) that the build recorded, so it arrived intact. The loader compiles the wasm only once the bucket
holds at least 480 CPU, so with a low bucket the boot waits until it refills. Then type `rustedscreep.status()` in
the console to see the build, the loader's phase, and any incidents.

## Commands in the game's console

| Command | What it shows or does |
| :- | :- |
| `rustedscreep.status()` | The build ID, the loader's phase, its recovery mode, and recent incidents |
| `rustedscreep.scheduler()` | The last tick's CPU: what each tier was allowed and what ran |
| `rustedscreep.opening()` | Each owned room's workers and their tasks, and its next spawn request |
| `rustedscreep.spawner()` | The spawn queue: requests, orders, and creeps by home and role |
| `rustedscreep.planner()` | Each room's base plan and its placement |
| `rustedscreep.remote()` | The rooms it mines remotely, with their figures and creeps |
| `rustedscreep.sign()` | The controller signer's record and its current trip |
| `rustedscreep.scout()` | What the bot knows of the rooms near its homes, the explorer, and the observers' looks |
| `rustedscreep.claims()` | The rooms the bot could claim next, best first, with each one's score and base plan |
| `rustedscreep.movement()` | Movement's rooms, grids, flow fields, and traffic |
| `rustedscreep.consts()` | The constants self-check: this build's game constants against the server's |
| `rustedscreep.persist()` | The RawMemory segments it uses |
| `rustedscreep.snapshot()`, `rustedscreep.act()` | The tick's snapshot tables, and its intents (the commands the bot gave) |
| `rustedscreep.config()` | Every setting, with the value in force |
| `rustedscreep.configSet(key, value)` | Changes a setting, from the next tick |
| `rustedscreep.configUnset(key)`, `rustedscreep.configReset()` | Removes one change, or all of them |
| `rustedscreep.spawn(room, role)`, `rustedscreep.moveTo(room, x, y, range)` | A one-off spawn or move order, for testing; called with nothing, it cancels |

### Settings

`rustedscreep.config()` lists every key. For example, `rustedscreep.configSet("log.level", "debug")` prints more of
the log, and `rustedscreep.configSet("sign.enabled", false)` stops sending controller signers. A value outside a key's safe
range is clamped to it, and anything else invalid is refused. Changes are kept in a RawMemory segment, so they survive
a global reset, and come back a tick or two after one. [`docs/TUNING.md`](docs/TUNING.md) gives each setting's
default, unit, reason, and safe range.

The signer's text is `SIGN_TEXT` in
[`crates/rustedscreep-core/src/signing/mod.rs`](crates/rustedscreep-core/src/signing/mod.rs). To sign with your own,
change it and build the bot yourself.

## What it uses in your account

- **A whole code branch.** The GitHub link deletes any module that is not in `deploy/`.
- **RawMemory segments 94 to 99** (the game's numbered strings that survive between ticks): the atlas of rooms near
  its homes (94), the signer's record (95),
  the base planner's record (96), the settings (97), a self-test (98), and the profiler's statistics (99, in a
  profiler build only).
- **Almost none of `Memory`.** The bot never reads `Memory` (the game's JSON store, which costs CPU to parse) for its
  state. It removes the `Memory.creeps` entry that the game adds for each creep it spawns, and resets a `Memory` that
  does not parse. The loader keeps its record of incidents in `Memory.rustedscreepLoader`.
- **Your email,** through `Game.notify`, when the bot crashes, at most one batch an hour.

## When it crashes

A room, creep, or task that fails is logged and skipped, and the rest of the tick goes on. A panic or another runtime
fault in the wasm is worse: it traps, which aborts the whole wasm call. The loader catches the trap, logs it, sends a
notification, and halts (`Game.cpu.halt()`), so that the next tick starts with a fresh JavaScript environment and a
fresh wasm.

Three incidents in a row make the loader wait before it boots again: 100 ticks, doubling with each further incident,
up to 3200. The streak is kept in `Memory`, so it carries over to a new build. Once `rustedscreep.status()` shows the
fixed build, clear it in the console:

```js
delete Memory.rustedscreepLoader
```

## On a private server

Untested. The private-server track ([`docs/ROADMAP_PRIVATE.md`](docs/ROADMAP_PRIVATE.md)) starts by checking that a
server's runtime accepts the wasm. Releases also go to the Steam Workshop of Screeps: World, whose "Server Mods & Bots"
section is for private servers. Whether a private server runs the bot from there is not checked yet.

## Build it from source

The build runs on Linux x86_64. You need:

- [rustup](https://rustup.rs). `rust-toolchain.toml` pins Rust 1.100.0 with the `wasm32-unknown-unknown` target, and
  rustup installs it the first time you run `cargo` here.
- Node 18.19 or later, then the build's packages:

  ```bash
  npm ci
  ```

- The wasm-bindgen command-line tool at exactly the version in `Cargo.lock`:

  ```bash
  cargo install wasm-bindgen-cli --version 0.2.130 --locked
  ```

- wasm-opt from binaryen version_133, downloaded into `.tools/` and checked against its pinned SHA-256:

  ```bash
  npm run setup:binaryen
  ```

Then:

| Command | What it does |
| :- | :- |
| `npm run build` | Builds the wasm and the loader into `dist/`, and runs them once in a model of the game |
| `npm run check` | The full gate: formatting, both lints, the tests, the build, the sizes, and every sandbox simulation |
| `npm run stage` | Builds and writes the deployable files to `deploy/` (`-- --out <dir>` writes them elsewhere) |
| `cargo test -p rustedscreep-core -p rustedscreep-consts` | The native tests of the bot's logic |
| `cargo bench -p rustedscreep-core` | The benchmarks of the planner and the combat simulation |

With the GitHub link, commit `deploy/` and push to `main` to deploy. Otherwise upload `deploy/`'s files as above.

## Repository layout

| Path | Contents |
| :- | :- |
| `crates/rustedscreep-consts` | The game's constants and pure types, re-exported from `screeps-game-api`, with the values it lacks ([README](crates/rustedscreep-consts/README.md)) |
| `crates/rustedscreep-core` | All of the bot's decisions, in pure Rust that tests natively ([README](crates/rustedscreep-core/README.md)) |
| `crates/rustedscreep-wasm` | The wasm module: the snapshot of the game, the intents, persistence, and the exported `loop` ([README](crates/rustedscreep-wasm/README.md)) |
| `js/src` | The TypeScript loader: it boots the wasm, calls `loop` each tick, and recovers from traps |
| `js/tools` | The build, stage, size, and check tools, and the sandbox simulations that run a build against a model of the game |
| `deploy/` | The staged build that the GitHub link uploads |
| `docs/` | The design, the roadmap, the decisions, and the evidence |
| `.claude/` | The Claude Code setup this project is built with ([`KIT_README.md`](KIT_README.md)) |

## Documentation

| Document | Contents |
| :- | :- |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | The tick's data flow, the crates and modules, caches, CPU tiers, persistence, and failure handling |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | The phases, each with the checks that close it. [`ROADMAP_PRIVATE.md`](docs/ROADMAP_PRIVATE.md) and [`ROADMAP_SEASONAL.md`](docs/ROADMAP_SEASONAL.md) are optional tracks for private and Seasonal servers |
| [`docs/STATUS.md`](docs/STATUS.md) | Where the work stands, with the evidence for each step |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | Architecture decision records: why things are the way they are |
| [`docs/VERIFICATION_LOG.md`](docs/VERIFICATION_LOG.md) | Every game rule and number the code relies on, with its source |
| [`docs/TUNING.md`](docs/TUNING.md) | Every setting, with its default, unit, reason, and safe range |
| [`docs/TEST_PLAN.md`](docs/TEST_PLAN.md) | The tests, the sandbox simulations, and the live checks |
| [`docs/RELEASE.md`](docs/RELEASE.md) | Making a release |
| [`docs/SPEC.md`](docs/SPEC.md) | The original requirements |

## Releases

A version tag, `v<version>`, publishes a release ([`docs/RELEASE.md`](docs/RELEASE.md)):

- the three crates on crates.io: [rustedscreep-consts](https://crates.io/crates/rustedscreep-consts),
  [rustedscreep-core](https://crates.io/crates/rustedscreep-core), and
  [rustedscreep-wasm](https://crates.io/crates/rustedscreep-wasm);
- `deploy/`'s files on a [GitHub release](https://github.com/jabbott-iii/rustedscreep/releases);
- the same files as an item in the Steam Workshop of Screeps: World.

## License

MIT: see [`LICENSE`](LICENSE).
