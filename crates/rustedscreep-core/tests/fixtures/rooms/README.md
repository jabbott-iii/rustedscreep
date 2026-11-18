# Terrain corpus (ROADMAP P4.1)

Rooms from the official MMO for the base planner's tests and benches (P4.2, the P4 gate). One file per room,
`<shard>/<room>.txt`, read by `tests/common/corpus.rs` and checked by `tests/rooms_corpus.rs`.

## Where the data came from
- **Rooms:** every room with a controller in the sector E1–E9 × N51–N59 of `shard3` (the bot's home sector). The
  sector's 81 rooms were requested and the 9 without a controller (its centre, E4–E6 × N54–N56) were left out:
  72 rooms. They were chosen by whether a controller exists (VERIFICATION_LOG V231), not by coordinates.
- **Requests:** the official server's public endpoints, with no token or account, on 2026-11-17, one request a second:
  - `GET https://screeps.com/api/game/room-objects?room=<room>&shard=shard3` for each of the 81 rooms;
  - `GET https://screeps.com/api/game/room-terrain?room=<room>&shard=shard3&encoded=1` for each room with a controller.
- **Conversion:** `node js/tools/rooms-fixture.mjs <directory of saved responses> shard3 2026-11-17`. The directory
  holds `<room>.terrain.json` and `<room>.objects.json` per room. The tool reads local files only (no network,
  ADR-021), refuses any response of the wrong shape, and writes each room's file.
- **Kept and dropped:** a file keeps the terrain and the positions of the controller, the sources and the mineral
  (with its type). Everything else in the responses is dropped: owners, usernames, reservations, signs, levels,
  structures, creeps, ruins and sites. The corpus is map data, not anyone's base.

## Format
```
# <comment lines>
room E3N53
shard shard3
controller 33 15
source 7 9            (one line per source, sorted by x then y)
mineral H 12 40       (type, x, y)
terrain
<50 rows of 50 digits: row y = 0 first, column x = 0 first>
```
Each digit is the tile's raw terrain mask (0 plain, 1 wall, 2 swamp, 3 wall and swamp), byte for byte what
`Room.Terrain.getRawBuffer()` returns at index `y * 50 + x` (V156, V224 to V226). `TerrainGrid::from_row_major`
takes the bytes as they are.

## What the corpus holds (counted from the files on 2026-11-17)
- 72 rooms: 39 with two sources and 33 with one. Every room has one mineral (H 20, O 17, L 10, U 9, K 8, Z 7, X 1).
  No tile is both wall and swamp (no digit 3).
- Terrain share per room (lowest, median, highest): wall 12.9%, 32.8%, 82.2%; swamp 0.0%, 4.5%, 38.5%;
  plain 17.5%, 54.8%, 81.4%. The rooms range from open plains to cramped ones, which suits a planner benchmark.
- 120 pairs of neighbouring rooms share a border, so the border check compares 11520 edge tiles (each border from
  both sides, 48 tiles each).

`tests/rooms_corpus.rs` checks the properties the open-source map generator gives (V227 to V229). The official
server's map code is closed and may differ, so the test asserts each property on the data instead of assuming it:
- every byte is a combination of the wall and swamp masks, and the grid reads it with wall precedence;
- the controller, each source and the mineral stand on a wall tile off the edge, with an open inner tile next to it;
- one or two sources and at most one mineral per room;
- the open inner tiles form one area, which reaches every object and at least one exit;
- every edge tile matches the tile it leads to in a neighbouring room of the corpus, which pins the digit order
  (read transposed, the same edges disagree).

## Adding rooms
Fetch the two responses per room as above into an empty directory, run the tool with that directory, the shard and
the fetch date, and update this README's room list and numbers. A new shard gets its own directory.
