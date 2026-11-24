# rustedscreep-core

The decision logic of rustedscreep, a bot for [Screeps: World](https://screeps.com). Screeps is an MMO strategy game in
which players write code that runs their units, called creeps, instead of steering them by hand. Every tick (one game
turn) this crate decides what each creep, spawn, and tower does. It is plain Rust: it never calls the game.

It is part of [rustedscreep](https://github.com/jabbott-iii/rustedscreep), which compiles to WebAssembly (wasm) and
runs on the game's official server. Each tick runs in three steps:

1. **Snapshot.** [rustedscreep-wasm](https://crates.io/crates/rustedscreep-wasm) makes one pass over the game's
   objects and copies what the bot needs into this crate's tables. Each kind of object gets a table with one array per
   field (a struct of arrays), and the objects are named by numeric handles.
2. **Decide.** This crate reads the snapshot and decides.
3. **Act.** rustedscreep-wasm issues the decisions as intents: the commands, such as a move or a harvest, that the game
   applies once the tick's code has run.

Because this crate never touches a game object, it builds and tests natively with `cargo test`. Its results are
deterministic: the same snapshot and state always give the same decisions. Game constants and the game's pure types,
such as `RoomName` and `Position`, come in through
[rustedscreep-consts](https://crates.io/crates/rustedscreep-consts).

## Status

Version 0.1. This is the bot's own logic, published so that each release's source is on crates.io. It is not a
stable library, and its API changes from one version to the next.

The bot is built in phases (the
[roadmap](https://github.com/jabbott-iii/rustedscreep/blob/main/docs/ROADMAP.md) has P0 to P12). What is built, from
P1 to P7:

- **The scheduler:** three CPU tiers chosen by the bucket (the CPU that earlier ticks left unused, which the game banks
  for later), a per-tick budget, and heavy jobs time-sliced (spread over several ticks).
  - Critical work (spawning, defense, and harvesting) always runs.
  - Standard work (hauling, upgrading, and building) runs unless the bucket is critically low.
  - Optional work (planning, scouting, and the controller signer) runs only above bucket thresholds.
- **The act layer's pure half:** the engine's table of which intents block which others, de-duplication, and skipping
  intents that would change nothing. Every intent that changes the game's state costs a fixed amount of CPU.
- **Movement:** cost grids for each room, flow fields (every tile's cost to a goal, shared by all the creeps going
  there), cached detours, traffic between creeps, and routes from room to room.
- **Spawning:** one spawn queue for all the bot's rooms, creep bodies and names, and pre-spawn replacement (ordering a
  creep's successor early enough that it is at its post when the old creep dies of age).
- **The economy:** the opening from room controller level (RCL) 1 to 3, static miners, links, haulers, upgraders,
  repairs, and the solver that sizes each body to its job.
- **The base planner:** where a room's structures, roads, and ramparts go, laid out by a flood fill outward from a hub
  with the rampart line found by a minimum cut. It checks the plan and places construction sites from it.
- **Remote mining:** a profit model per room, the choice of neighboring rooms to mine without owning them, reservers,
  remote miners and haulers, roads, and evacuating creeps from armed hostiles.
- **Defense:** an assessment of hostile creeps from their bodies and boosts, tower fire and healing chosen by a combat
  simulation, and upkeep of the rampart line.
- **The rest:** the controller signer, the binary formats the bot keeps in RawMemory segments (strings the game stores
  between ticks), the runtime config and its server profiles, the constants self-check, and the profiler's math.

Not yet: claiming more rooms (P8), the late-game structures and the market (P9), offense (P10 and P11), and visuals
(P12).

## Usage

```toml
[dependencies]
rustedscreep-core = "0.1"
rustedscreep-consts = "0.1"
```

The functions take the game's constants and types from rustedscreep-consts, so add it too.

A creep body, what it costs, and when to order its successor:

```rust
use rustedscreep_consts::{CREEP_LIFE_TIME, CREEP_SPAWN_TIME, Part};
use rustedscreep_core::spawning::{Body, lead_ticks};

// The parts in the order the spawn makes them. The engine damages a body front first.
let body = Body::new(&[Part::Work, Part::Carry, Part::Move, Part::Move]).expect("a valid body");

// The spawn charges each part's BODYPART_COST: 100 + 50 + 50 + 50 energy.
assert_eq!(body.cost(), Part::Work.cost() + Part::Carry.cost() + 2 * Part::Move.cost());
// It takes CREEP_SPAWN_TIME (3) ticks a part, and the creep lives CREEP_LIFE_TIME (1500) ticks.
assert_eq!(body.spawn_time(), 4 * CREEP_SPAWN_TIME);
assert_eq!(body.life(), CREEP_LIFE_TIME);

// Its successor must be ordered this many ticks before it dies: the spawn time, a walk of 20
// ticks from the spawn to the post, and a margin of 10.
assert_eq!(lead_ticks(&body, 20, 10), body.spawn_time() + 20 + 10);
```

A tower's damage by range, as the engine computes it:

```rust
use rustedscreep_consts::{TOWER_FALLOFF, TOWER_FALLOFF_RANGE, TOWER_OPTIMAL_RANGE, TOWER_POWER_ATTACK};
use rustedscreep_core::defense::tower_amount;

// Up to TOWER_OPTIMAL_RANGE (5) tiles away, a tower hits for its full TOWER_POWER_ATTACK (600).
let near = u32::from(TOWER_OPTIMAL_RANGE);
assert_eq!(tower_amount(TOWER_POWER_ATTACK, near), TOWER_POWER_ATTACK);

// From TOWER_FALLOFF_RANGE (20) on, it hits for TOWER_FALLOFF (75%) less: 150.
let far = u32::from(TOWER_FALLOFF_RANGE);
let weakest = TOWER_POWER_ATTACK - (f64::from(TOWER_POWER_ATTACK) * TOWER_FALLOFF) as u32;
assert_eq!(tower_amount(TOWER_POWER_ATTACK, far), weakest);
```

The benchmarks of the planner and the combat simulation run natively, with Criterion:

```bash
cargo bench -p rustedscreep-core
```

## Rules the code keeps

- **No JavaScript.** Nothing here depends on `wasm-bindgen` or `js-sys`, or calls a function of `screeps-game-api`
  that calls into JavaScript. The repository's purity check enforces it.
- **No panics outside tests.** On wasm a panic aborts the whole tick (the release profile sets `panic = "abort"`), so
  non-test code uses no `unwrap`, `expect`, `panic!`, or unchecked indexing, and Clippy denies them. A room, creep,
  or task that fails returns an error. The caller logs it, with a rate limit, and goes on with the rest.
- **Thresholds live in [`config`](https://docs.rs/rustedscreep-core/latest/rustedscreep_core/config/).** Each one is
  documented in
  [`docs/TUNING.md`](https://github.com/jabbott-iii/rustedscreep/blob/main/docs/TUNING.md), with its default, unit,
  reason, and safe range, and a test fails when the two differ.

## Documentation

- API reference: https://docs.rs/rustedscreep-core
- Design: https://github.com/jabbott-iii/rustedscreep/blob/main/docs/ARCHITECTURE.md
- The bot: https://github.com/jabbott-iii/rustedscreep

## License

MIT: see [LICENSE](LICENSE).
