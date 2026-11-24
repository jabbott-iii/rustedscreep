# rustedscreep-consts

The game constants and pure game types of rustedscreep, a bot for [Screeps: World](https://screeps.com), in one crate
that builds and tests natively. It re-exports what
[screeps-game-api](https://crates.io/crates/screeps-game-api) (the Rust bindings to the game's API) exports as plain
values, hides what carries JavaScript objects, and adds the values the API does not export.

It is part of [rustedscreep](https://github.com/jabbott-iii/rustedscreep). The bot's logic,
[rustedscreep-core](https://crates.io/crates/rustedscreep-core), reaches the game's numbers and types only through
this crate. Only this crate and the bot's wasm crate,
[rustedscreep-wasm](https://crates.io/crates/rustedscreep-wasm), name `screeps-game-api` at all, so one place decides
what the logic may use.

## Status

Version 0.1, built on screeps-game-api `=0.23.5` (an exact pin: a new version of the bindings is a deliberate change,
checked again before it lands). The API will change before 1.0.

What it holds:

- **The API's constants,** at the crate root: numbers such as `CREEP_LIFE_TIME`, enums such as `Part`,
  `StructureType`, and `ResourceType`, and grouped modules such as `creep` and `tower`.
- **The `Find` enum:** the numeric `FIND_*` codes, as plain values.
- **The pure local types,** which compute in Rust and never call into JavaScript: `RoomName`, `RoomXY`,
  `RoomCoordinate`, `Position`, `LocalRoomTerrain`, `LocalCostMatrix`, and their errors and index helpers.
- **`extra`:** values the API does not export, such as `extra::MEMORY_SEGMENT_COUNT` (the game's 100 RawMemory
  segments, the numbered strings it keeps between ticks). Each item cites its source and the date it was last checked.
  `extra` is never glob-re-exported, so nothing in it can shadow an API constant.

What it leaves out, because these carry JavaScript game objects and would call into JavaScript on wasm and panic
natively:

- the `find` and `look` modules, and the `FindConstant` and `LookConstant` traits;
- `ObjectId`, whose methods resolve objects through JavaScript, and `LodashFilter`.

## Usage

```toml
[dependencies]
rustedscreep-consts = "0.1"
```

```rust
use rustedscreep_consts::{CREEP_LIFE_TIME, Part, ROOM_SIZE, RoomName, RoomXY, extra};

// The API's constants, re-exported at the root.
assert_eq!(CREEP_LIFE_TIME, 1500);
assert_eq!(Part::Work.cost(), 100);

// Pure local types work in a native test as well as on wasm.
let room: RoomName = "W1N1".parse().expect("a room name");
assert_eq!(room.to_string(), "W1N1");
// A room is ROOM_SIZE (50) tiles on a side, so x and y run from 0 to 49.
assert!(RoomXY::checked_new(ROOM_SIZE - 1, 0).is_ok());
assert!(RoomXY::checked_new(ROOM_SIZE, 0).is_err());

// A value the API does not export.
assert_eq!(extra::MEMORY_SEGMENT_COUNT, 100);
```

## Using the re-exported API safely

The re-exports are the API's own items, so a few of them can still panic or call into JavaScript. The
[crate docs](https://docs.rs/rustedscreep-consts) list each case. In short:

- **Never call the game methods of `Position`** (`look`, `find_path_to`, `create_construction_site`, and the like)
  or the conversions to and from JavaScript values. They call into JavaScript on wasm and panic natively.
- **Use the checked forms of arithmetic:** `checked_add` on `RoomXY`, `Position`, and `RoomName`, and
  `Position::checked_from_world_coords`, instead of `+`, `-`, or the unchecked constructors, which panic or overflow
  on bad input.
- **Match the unknown case of string enums.** `Part`, `StructureType`, `ResourceType`, and the other string enums have
  a hidden variant for a string the bindings do not know. `Display` panics on it, and `Part::cost()` returns 0 for it.

## The constants check on the live server

The live server's constants are the ground truth for that server. Once per global (each time the game loads the bot's
code afresh), the bot compares this crate's values with the server's JavaScript globals and reports what differs, in
its log and in `rustedscreep.consts()` (the constants self-check, in rustedscreep-core's `consts_check`).

## Documentation

- API reference: https://docs.rs/rustedscreep-consts
- Where each value was checked:
  https://github.com/jabbott-iii/rustedscreep/blob/main/docs/VERIFICATION_LOG.md
- The bot: https://github.com/jabbott-iii/rustedscreep

## License

MIT: see [LICENSE](LICENSE).
