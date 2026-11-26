// Scouting's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/scout.rs, snapshot.rs `push_intel` and persist.rs's segment 94, over rustedscreep-core
// `expansion`; ROADMAP P8.1, ADR-039: an atlas of the rooms near the homes, read whenever a room is visible, toured by
// one cheap explorer creep and looked at by the observers, and kept across a global reset). The world is sign-sim.mjs's:
// four rooms, each pair of neighbors sharing an exit of the tiles 20 to 30 of their common edge (corners and every
// other edge tile are walls), every controller on a terrain wall (V228):
// - W1N1, owned (run A at RCL3, `scout.min_level`; run B at RCL8, where CONTROLLER_STRUCTURES.observer allows one
//   observer): the controller at (10, 10), one spawn at (25, 25), no source, so mining and hauling stay inert. As in
//   sign-sim.mjs, an idle own creep whose name the bot did not give (KEEPER) stands in it from the start: movement
//   reads a room's terrain only while an own creep stands in it, and without that grid the opening keeps
//   `opening.min_workers` and orders a worker every tick; with the grid, a room with no source wants no worker. Its
//   `owner` also gives the bot its username (read once a global), which a reservation's read compares;
// - W2N1 (west): a neutral controller (level 0, no owner, `my` undefined, V101) at (25, 25), two sources at (40, 40) and
//   (10, 40) and a hydrogen mineral (H) of density DENSITY_MODERATE at (10, 10);
// - W2N2 (north-west): a neutral controller at (25, 25) reserved by an invader core (V365: a lesser core, level 0, at
//   (10, 40), owner `Invader`), its reservation ending at RESERVATION_END (`ticksToEnd` counts down to it, V263), one
//   source at (40, 10) and an oxygen mineral (O) of density DENSITY_HIGH at (10, 10);
// - W1N2 (north): `Game.map.getRoomStatus` gives `closed` (V168; every other room `normal`), so no creep may enter
//   it; a neutral controller at (25, 25), no source, no mineral.
// With `scout.max_distance` 1 these three are the candidates: the other rooms within one of W1N1 are highways (W0x,
// xN0), which cannot have a controller (economy::is_remote_room).
// Runs A and B start with the overrides `sign.enabled=false` (no signer, whose creep would give vision too),
// `scout.max_distance=1`, `scout.save_ticks=5` and `claim.enabled=false` (claim scoring would read W2N1's terrain;
// run C checks it) in segment 97 (the runtime config's record; scouting stays on), which the bot loads on its second
// loop; it then requests segment 94 (the atlas), which loads on the loop after. Runs C and D start with the same
// overrides but the last, and with `claim.cpu_mean_loops=3` (P8.4: the claim's CPU mean is full from the global's fifth
// loop, so the claim is not held for the MMO default's 1500 loops). The snapshot reads a due room only
// once the atlas has loaded and the tick before ran the Optional tier.
// Run A (explorer and record): the spawn holds SPAWN_ENERGY_CAPACITY. The explorer's tour this gives (expansion/mod.rs
// `nearest_due`, by world tiles from where it stands to each room's center, then by packed name): out of the spawn on
// (25, 24), W1N2's center is 49 tiles away and the other two 50, so W1N2 first, whose route the glue refuses before
// PathFinder (route.rs: a target room of another status), so it is marked unreachable; then W2N2 (the tie with W2N1
// goes to the lower packed name), whose route passes through W2N1: W2N1 is read on the explorer's first loop there, W2N2
// on its first loop there, and with nothing left the explorer ends. The run lets SETTLE loops pass (the record's save),
// asks `rustedscreep.scout()`, halts once (a global reset), runs AFTER more loops and asks it again.
// Run B (observer): an own observer at OBSERVER_AT in W1N1, and the spawn starts with no energy (it regains 1 a tick,
// V174), so no explorer can be paid for (BODYPART_COST.move) before the observer has looked at the three rooms, and
// once nothing is due none is requested. Each look goes to the stalest due room in range (never read first, then the
// oldest read, then the nearest, then the packed name): W2N2, W2N1, W1N2. It asks `rustedscreep.scout()` on the first
// look's loop, and again once B_SETTLE loops have passed since the last read.
// The harness plays the engine's part as far as the bot can see it:
// - `spawnCreep(body, name)` makes the runtime's checks in V171's order and its `Memory.creeps[name]` write (V181); the
//   processor (V173) turns an order for an explorer's name (`x…`, `Role::Explorer`) into a creep that is spawning for
//   CREEP_SPAWN_TIME × parts loops and then stands on the first free tile around the spawn in direction order (V174),
//   with ticksToLive CREEP_LIFE_TIME on its first loop out (V198 infers undefined there; the bot reads it as an Option
//   either way), and drops any other order; the spawn's energy pays the body and regains 1 a tick (V174);
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52), with fatigue (V54, V55); a
//   creep that ends a tick on an edge tile goes to the next room (V151), except into a closed room, where it stays on
//   its edge (V168); `suicide` removes the creep in the intents stage (before any room transfer) and leaves a tombstone;
// - a room is visible (in Game.rooms) while an own creep stands in it, spawning or not, the owned one always (V101),
//   and on the tick after an observer's accepted `observeRoom(name)` (V361, V362: that one tick only, with every
//   object in it, V363);
// - `observeRoom(name)` makes the game side's checks in V358's order (ERR_INVALID_ARGS for a non-string or a name not
//   matching `^(W|E)\d+(S|N)\d+$`, ERR_RCL_NOT_ENOUGH at a level CONTROLLER_STRUCTURES.observer does not allow,
//   ERR_NOT_IN_RANGE beyond OBSERVER_RANGE on either world axis, V359), and its intent takes one slot per observer
//   (V360: a second call in a tick replaces the first);
// - the atlas's reads (snapshot.rs `push_intel`) see the engine's getters: `Game.map.getRoomStatus(name).status`
//   (V168), the controller's `pos` and `reservation` (V263), `room.find` of FIND_SOURCES, FIND_MINERALS (each
//   mineral's `mineralType` and `density`, V364) and FIND_HOSTILE_STRUCTURES with a `{structureType}` filter (V199,
//   V267), an invader core's `level` (V365); each status call, filtered find, `reservation`, `mineralType`, `density`
//   and `level` read is counted by room and loop, split by whether it falls in the snapshot's pass over Game.rooms
//   (from the enumeration of Game.rooms to that of Game.structures; a status call there is counted for the room whose
//   FIND_HOSTILE_CREEPS find, the first of every room's reads, came last);
// - `PathFinder.search` is route-sim.mjs's model of the native search (V163 to V166, copied from sign-sim.mjs); every
//   search and `Game.map.getRoomTerrain` call is recorded;
// - `room.createConstructionSite` makes the game side's checks (V200, V237, V238, as in sign-sim.mjs) for the opening's
//   and the planner's sites in W1N1; an accepted site appears on the next tick and nothing builds it.
// It checks, in run A:
// 1. segment 94 is in no `setActiveSegments` call of a global up to the loop on which that global's config record
//    (segment 97) loaded, and in one after it; the bot writes segment 94 (a frame starting `rs1b`) at least once in
//    the first global, never on two loops less than `scout.save_ticks` (5) apart, and not at all in the second global;
//    the first global's last write decodes (persist/frame.rs, expansion/atlas.rs) to kind 5, schema 1, written at that
//    loop's Game.time, a good CRC-32, and exactly the three rooms' entries in packed-name order, each as the world and
//    the loops of its read (or W1N2's mark) give it; the second global logs exactly one line `rustedscreep scout: atlas
//    loaded from segment 94 (3 rooms, written at T)`, T being that write's Game.time, and the first global none (its
//    segment was empty);
// 2. exactly one spawnCreep call in the run, for the explorer: two arguments, a name matching /^xW1N1-[0-9a-z]+$/ and the
//    body ['move'] (BODYPART_COST.move, 50 energy), OK; so no second explorer while one lives and none in the second
//    global; the processor drops no order, and the explorer spawns for CREEP_SPAWN_TIME loops;
// 3. W2N1 and W2N2 each log exactly one line `rustedscreep scout: <room> read: <text>`, <text> being scout.rs
//    `describe` of the room's contents, on the first loop the room is visible, with the explorer standing there; that
//    loop's pass makes exactly one status call for the room, one FIND_SOURCES and one FIND_MINERALS find, one find
//    through each of the tower, spawn, keeperLair and invaderCore filters, one `reservation` read, one `mineralType`
//    and one `density` read, and (W2N2) one core `level` read; no loop of either global makes any of these in the pass
//    for a room it does not read (W1N1's FIND_SOURCES, its own room objects' find, aside), none falls in the pass for
//    another room, and no filtered FIND_HOSTILE_STRUCTURES find, `reservation`, `mineralType`, `density` or `level`
//    read happens outside the pass; W1N2 is never visible;
// 4. one line `rustedscreep scout: no route to W1N2; it waits 50000 ticks`, on a loop the explorer stands in W1N1 out of
//    the spawn, before both reads; no PathFinder.search has its target in W1N2 (the glue refuses it before PathFinder),
//    and W1N2's status was read outside the pass; no search stops short; none runs in the second global;
//    `Game.map.getRoomTerrain` is never called;
// 5. the explorer ends: one `suicide` call with no argument, on the loop of the one line `rustedscreep scout: nothing
//    left in reach from W2N2; the explorer ends`, the room it stands in, after both reads; it calls nothing after it and
//    is gone on the next loop; it stood in W1N1, W2N1 and W2N2 and never in W1N2;
// 6. `rustedscreep.scout()` before the halt is exactly `... atlas 3 rooms (2 read, 1 unreachable, 0 towered), loaded |
//    homes [W1N1] | 0 due, next at <W1N2's mark + scout.refresh_ticks> | no explorer | since the global: reads 2,
//    searches 2, unreachable 1, lost 0, retreats 0, explorers 1, looks 0 (refused 0)` (the two searches: W1N2's, refused,
//    and W2N2's, the one PathFinder.search of the run), and at the end the same atlas with nothing done in the second
//    global (reads 0, searches 0, unreachable 0, explorers 0);
// 7. no log line starting `rustedscreep fault` or `rustedscreep incident`, no other `rustedscreep scout:` line, no
//    `rustedscreep persist:` warning, every creep call OK and the explorer's move or suicide, no move the engine would
//    refuse and no creep left on an exit to W1N2, every site call in W1N1 and OK, one halt, two globals.
// And in run B:
// 8. segment 94 is requested only after the config record loaded, and the atlas is written within the save cap;
// 9. exactly three `observeRoom` calls, by the observer, each with one string argument and OK, at W2N2, W2N1 and W1N2
//    in that order, one on each loop from the first to the last and never two in a loop, none at a room visible on
//    that loop, the first after segment 94 was requested; each room's read line (W1N2's with `closed`) comes on the
//    loop right after its call, the only loop that room is visible, with that loop's pass making the reads of check 3
//    once (so no look is left without vision, which scout.rs would log as `gave no vision`); no call on any of the
//    B_SETTLE loops after the last read;
// 10. `rustedscreep.scout()` on the first look's loop is exactly `... atlas 0 rooms (0 read, 0 unreachable, 0 towered),
//    loaded | homes [W1N1] | 3 due | no explorer | looks [W2N2] | since the global: ... looks 1 (refused 0)`, and at the
//    end `... atlas 3 rooms (3 read, 0 unreachable, 0 towered), loaded | homes [W1N1] | 0 due, next at <W2N2's read +
//    scout.refresh_ticks> | no explorer | since the global: reads 3, ... looks 3 (refused 0)`; the last write decodes
//    to the three rooms' entries;
// 11. no spawnCreep call, no PathFinder search, no atlas load line (the segment was empty), and check 7's log and call
//    rules (the read lines the only scout lines), with the observer's calls the only structure calls.
// And in run C (claim scoring, ROADMAP P8.2, ADR-041: run B's world with claim scoring on), checks 12 to 15 at
// `claimsRun`: W2N1's one terrain read, its best line with terms from the constants, and `rustedscreep.claims()`.
// And in run D (claimers, ROADMAP P8.3, ADR-042: run C's world at GCL 2 with W1N1's capacity at RCL4's), checks 16 to 21
// at `claimerRun`: the GCL read, the claimer's order, walk, claim and end, the first spawn site at the plan's tile, and
// a pioneer's order. For it the harness also plays `Game.gcl` (V379), `claimController` (V374's checks; the claimed
// room is the bot's at level 1 from the next tick, V377), and W1N1 refilled to its capacity after each loop.
// npm run check runs it (row 26); standalone: node js/tools/scout-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { crc32 } from './lib/crc32.mjs';
import { CREEP_METHODS, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { ATLAS_SEGMENT, CONFIG_SEGMENT, ENGINE_CONSTANTS, commandResults, configRecord, driverRawMemory, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
const T0 = 50000000;
const id = (n) => `5bbcaa9099f9b0a0f7f4c${n.toString(16).padStart(3, '0')}`;
const HOME = 'W1N1';
const WEST = 'W2N1';
const NORTH = 'W1N2';
const CORNER = 'W2N2';
const ROOMS = [HOME, WEST, NORTH, CORNER];
const USER = 'bot';
const INVADER = 'Invader';
const SPAWN = { x: 25, y: 25 };
/** The idle own creep in W1N1 (see the header), off the explorer's ways; its ticksToLive outlasts the run. */
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
/** Run B's observer in W1N1. */
const OBSERVER_AT = { x: 40, y: 10 };
const CONTROLLER = { [HOME]: { x: 10, y: 10 }, [WEST]: { x: 25, y: 25 }, [NORTH]: { x: 25, y: 25 }, [CORNER]: { x: 25, y: 25 } };
/** Each room's open sides (toward the other rooms of the world). */
const OPEN = { [HOME]: ['W', 'N'], [WEST]: ['E', 'N'], [NORTH]: ['S', 'W'], [CORNER]: ['S', 'E'] };
/** Game.map.getRoomStatus's `status` (V168): every room not listed is 'normal'. */
const STATUS = { [NORTH]: 'closed' };
/** The sources, in the order FIND_SOURCES gives them. */
const SOURCES = { [WEST]: [{ x: 40, y: 40 }, { x: 10, y: 40 }], [CORNER]: [{ x: 40, y: 10 }] };
const MINERAL = {
  [WEST]: { type: C.RESOURCE_HYDROGEN, x: 10, y: 10, density: C.DENSITY_MODERATE },
  [CORNER]: { type: C.RESOURCE_OXYGEN, x: 10, y: 10, density: C.DENSITY_HIGH },
};
/** W2N2's lesser invader core (level 0) and the end of its reservation of the controller. */
const CORE = { [CORNER]: { x: 10, y: 40, level: 0 } };
const RESERVATION_END = T0 + 4000;
/** The filters push_intel finds FIND_HOSTILE_STRUCTURES through, in its order. */
const HOSTILE_TYPES = [C.STRUCTURE_TOWER, C.STRUCTURE_SPAWN, C.STRUCTURE_KEEPER_LAIR, C.STRUCTURE_INVADER_CORE];
const CANDIDATES = [WEST, NORTH, CORNER];
const SAVE_TICKS = 5;
/** ScoutConfig's MMO default (crates/rustedscreep-core/src/config.rs SCOUT_MMO). */
const REFRESH_TICKS = 50000;
const CONFIG_LINES = ['sign.enabled=false', 'scout.max_distance=1', `scout.save_ticks=${SAVE_TICKS}`, 'claim.enabled=false'];
/** Runs C and D: the claim's CPU mean (P8.4) over C_CPU_LOOPS loops, so it is full before the best line. The claim
 * allows at most ClaimConfig's MMO `cpu_pct` (80) of the sandbox's Game.cpu.limit (20); the sandbox's clock is real
 * elapsed time, about 0.3 to 0.5 ms a loop here (2026-11-25), so the cap of 16 leaves a margin of about 30 for 1 owned
 * room, and 3 loops average out one slow loop. */
const C_CPU_LOOPS = 3;
const C_CPU_CAP = (20 * 80 / 100).toFixed(2);
/** Run C's overrides: claim scoring stays on. */
const C_CONFIG_LINES = [...CONFIG_LINES.filter((l) => !l.startsWith('claim.')), `claim.cpu_mean_loops=${C_CPU_LOOPS}`];
/** The claims summary's CPU fragment (crates/rustedscreep-wasm/src/claim.rs `claimer_summary`): the mean, the load with
 * one more room, and the cap. */
const CPU_PART = new RegExp(`; CPU (\\d+\\.\\d\\d) a tick, (\\d+\\.\\d\\d) with another room \\(at most ${C_CPU_CAP.replace('.', '\\.')}\\)`);
/** The runtime config's log line for the record above (crates/rustedscreep-wasm/src/config.rs `loaded`). */
const CONFIG_LINE = `rustedscreep config: ${CONFIG_LINES.length} override(s) from segment ${CONFIG_SEGMENT} (written at tick 0): ${CONFIG_LINES.join(' ')}`;
/** The explorer's body (expansion `request`) and its cost. */
const EXPLORER_BODY = ['move'];
const EXPLORER_COST = EXPLORER_BODY.reduce((s, p) => s + C.BODYPART_COST[p], 0);
/** Scouting's log lines (crates/rustedscreep-wasm/src/scout.rs `log_events` and `loaded`). */
const READ_LINE = /^rustedscreep scout: ([WE]\d+[NS]\d+) read: (.*)$/;
const UNREACHABLE_LINE = `rustedscreep scout: no route to ${NORTH}; it waits ${REFRESH_TICKS} ticks`;
const DONE_LINE = /^rustedscreep scout: nothing left in reach from ([WE]\d+[NS]\d+); the explorer ends$/;
const LOADED_LINE = new RegExp(`^rustedscreep scout: atlas loaded from segment ${ATLAS_SEGMENT} \\((\\d+) rooms, written at (\\d+)\\)$`);
const SUMMARY = 'rustedscreep.scout()';
const HALT = 'Game.cpu.halt()';
/** Run A: loops after the explorer's end before the first summary (the record's save, at most SAVE_TICKS after the
 * one before, with room to spare), loops after the halt before the final summary, and the tour's limit. */
const SETTLE = 12;
const AFTER = 100;
const TOUR_MAX = 500;
/** Run B: loops after the last read before the final summary, and the run's limit. */
const B_SETTLE = 20;
const B_MAX = 200;
/** Run C: loops after the best line before the summary, and the run's limit. */
const C_SETTLE = 5;
const C_MAX = 300;
const CLAIMS = 'rustedscreep.claims()';
/** Run D (P8.3, ADR-042): GCL 2, and W1N1's capacity as at RCL4 (a spawn and CONTROLLER_STRUCTURES.extension[4] full
 * extensions), enough for the claimer (850) as `claim.min_home_level` (4) means; loops after the pioneer's first order
 * before the summary, and the run's limit. */
const D_GCL = 2;
const D_CAPACITY = C.SPAWN_ENERGY_CAPACITY + C.CONTROLLER_STRUCTURES.extension[4] * C.EXTENSION_ENERGY_CAPACITY[4];
const D_SETTLE = 5;
const D_MAX = 400;
/** The claimer's body (expansion::claimer_body): one CLAIM part and the MOVE parts that take a swamp tile's fatigue
 * for it within a tick (10 a part that makes fatigue, 2 removed a MOVE part, V54, V55, as `applyMoves` models). */
const CLAIMER_BODY = ['claim', ...Array(Math.ceil(10 / 2)).fill('move')];
const CLAIMER_COST = CLAIMER_BODY.reduce((sum, p) => sum + C.BODYPART_COST[p], 0);
const CLAIMER_NAME = new RegExp(`^k${HOME}${WEST}-[0-9a-z]+$`);
const PIONEER_NAME = new RegExp(`^w${WEST}-[0-9a-z]+$`);
/** ClaimConfig's MMO default `retry_ticks` (crates/rustedscreep-core/src/config.rs CLAIM_MMO). */
const RETRY_TICKS = 5000;
/** The claimer's log lines (crates/rustedscreep-wasm/src/claim.rs `log_events`). */
const SENT_LINE = `rustedscreep claims: a claimer from ${HOME} goes to claim ${WEST}`;
const CLAIMED_LINE = `rustedscreep claims: claimed ${WEST}; its pioneers come from the nearest spawn room, and its first spawn `
  + 'site follows from its plan';
const SITE_LINE = new RegExp(`^rustedscreep claims: ${WEST}'s first spawn site placed at \\((\\d+), (\\d+)\\)$`);
const NO_BEST_LINE = 'rustedscreep claims: no planned candidate';
/** ClaimConfig's MMO default `distance_cost` (crates/rustedscreep-core/src/config.rs CLAIM_MMO). Unit: me/t a room. */
const DISTANCE_COST = 1000;
/** Claim scoring's best line (crates/rustedscreep-wasm/src/claim.rs `log_events` and `describe`). */
const E = '(-?\\d+\\.\\d{3})';
const BEST_LINE = new RegExp(`^rustedscreep claims: best ${WEST} from ${HOME}, 1 room, 2 sources: ${E} e/t \\(sources ${E}, `
  + `remotes ${E}, distance ${E}, foreign ${E}, walks ${E}, ramparts ${E}\\), first spawn \\((\\d+), (\\d+)\\), (\\d+) ramparts$`);

const count = (body, part) => body.filter((p) => p === part).length;
const at = (p) => `(${p.x}, ${p.y})`;

/** Room coordinates as the engine counts them: W<n> is -n - 1, E<n> is n; N<n> is -n - 1, S<n> is n (V103). */
function roomXY(name) {
  const m = /^([WE])(\d+)([NS])(\d+)$/.exec(name);
  if (!m) throw new Error(`not a room name: ${name}`);
  return [m[1] === 'W' ? -Number(m[2]) - 1 : Number(m[2]), m[3] === 'N' ? -Number(m[4]) - 1 : Number(m[4])];
}
const roomAt = (rx, ry) => `${rx < 0 ? `W${-rx - 1}` : `E${rx}`}${ry < 0 ? `N${-ry - 1}` : `S${ry}`}`;
/** RoomName::packed_repr (V255): (x + 128) << 8 | (y + 128), the atlas's room key and order. */
const packedRoom = (name) => { const [rx, ry] = roomXY(name); return ((rx + 128) << 8) | (ry + 128); };
const unpack = (p) => {
  const v = p >>> 0;
  return { x: (v >>> 8) & 0xff, y: v & 0xff, roomName: roomAt(((v >>> 24) & 0xff) - 128, ((v >>> 16) & 0xff) - 128) };
};
const pos = (room, x, y) => ({ x, y, roomName: room, __packedPos: packPos(room, x, y) });
/** movement::Tile's index: column-major, x × 50 + y. */
const tile = (p) => p.x * 50 + p.y;

/**
 * scout.rs `describe` of a room's read: what the read line says, word for word as the bot logs it (`mineral X`, or `no
 * mineral` for a room without one).
 */
function describe(room) {
  const reserved = room === CORNER ? ', reserved by another' : '';
  const sources = (SOURCES[room] ?? []).length;
  const mineral = MINERAL[room] ? `mineral ${MINERAL[room].type}` : 'no mineral';
  const core = CORE[room] ? `, an invader core at level ${CORE[room].level}` : '';
  const status = STATUS[room] === 'closed' ? ', closed' : '';
  return `neutral${reserved}, ${sources} source(s), ${mineral}${core}${status}`;
}

/** Row-major terrain (V156): wall edges but the open sides' tiles 20..30 (corners always walls), and wall tiles. */
function terrain(open, walls) {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) {
      const gap = (v) => v >= 20 && v <= 30;
      const side = (x === 0 && open.includes('W') && gap(y)) || (x === 49 && open.includes('E') && gap(y))
        || (y === 0 && open.includes('N') && gap(x)) || (y === 49 && open.includes('S') && gap(x));
      const edge = x === 0 || y === 0 || x === 49 || y === 49;
      raw[y * 50 + x] = edge && !side ? C.TERRAIN_MASK_WALL : 0;
    }
  }
  for (const w of walls) raw[w.y * 50 + w.x] = C.TERRAIN_MASK_WALL;
  return raw;
}

const TERRAIN = Object.fromEntries(ROOMS.map((room) => [room, terrain(OPEN[room], [CONTROLLER[room]])]));
const isWall = (room, x, y) => (TERRAIN[room][y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

/** The engine's global `Creep` class, as far as the bindings use it (copied from sign-sim.mjs, V269). */
class EngineCreep {
  getActiveBodyparts(type) {
    let n = 0;
    for (let i = this.body.length - 1; i >= 0; i--) {
      if (this.body[i].hits <= 0) break;
      if (this.body[i].type === type) n += 1;
    }
    return n;
  }
}

/** The engine's RoomPosition, as far as the crate and a PathFinder call would use it (V103; copied from sign-sim.mjs). */
function definePositions(context) {
  class RoomPosition {
    constructor(x, y, roomName) { this.__packedPos = packPos(roomName, x, y); }
    get x() { return unpack(this.__packedPos).x; }
    get y() { return unpack(this.__packedPos).y; }
    get roomName() { return unpack(this.__packedPos).roomName; }
  }
  context.RoomPosition = RoomPosition;
  return RoomPosition;
}

/**
 * A model of the native PathFinder, copied from sign-sim.mjs's makePathFinder (route-sim.mjs's model; driver
 * lib/path-finder.js and native/src/pf.cc; V163 to V166). Each search is recorded in `state.searches`.
 */
function makePathFinder(state, RoomPosition) {
  class CostMatrix {
    constructor() { this._bits = new Uint8Array(2500); }
    set(x, y, v) { this._bits[x * 50 + y] = Math.min(Math.max(0, v), 255); }
    get(x, y) { return this._bits[x * 50 + y]; }
  }
  function search(origin, goal, options = {}) {
    const plainCost = Math.min(254, Math.max(1, (options.plainCost | 0) || 1));
    const swampCost = Math.min(254, Math.max(1, (options.swampCost | 0) || 5));
    const maxOps = Math.max(1, (options.maxOps | 0) || 2000);
    const maxRooms = Math.min(64, Math.max(1, (options.maxRooms | 0) || 16));
    const from = unpack(origin.__packedPos);
    const to = unpack(goal.pos.__packedPos);
    const range = Math.max(0, goal.range | 0);
    const record = { offset: state.reads.length - 1, from, to, range, incomplete: null };
    state.searches.push(record);
    const table = new Map();
    const blockedRooms = new Set();
    const room = (name) => {
      if (table.has(name)) return table.get(name);
      if (blockedRooms.has(name) || table.size >= maxRooms) return null;
      if (!TERRAIN[name]) throw new Error('Could not load terrain data');
      const ret = typeof options.roomCallback === 'function' ? options.roomCallback(name) : undefined;
      if (ret === false) {
        blockedRooms.add(name);
        return null;
      }
      const info = { raw: TERRAIN[name], bits: ret && ret._bits && ret._bits.length === 2500 ? ret._bits : null };
      table.set(name, info);
      return info;
    };
    const world = (p) => { const [rx, ry] = roomXY(p.roomName); return [rx * 50 + p.x, ry * 50 + p.y]; };
    const local = (wx, wy) => ({ x: ((wx % 50) + 50) % 50, y: ((wy % 50) + 50) % 50, roomName: roomAt(Math.floor(wx / 50), Math.floor(wy / 50)) });
    const look = (wx, wy) => {
      const p = local(wx, wy);
      const info = room(p.roomName);
      if (!info) return Infinity;
      const v = info.bits ? info.bits[p.x * 50 + p.y] : 0;
      if (v === 255) return Infinity;
      if (v !== 0) return v;
      const t = info.raw[p.y * 50 + p.x];
      if (t & C.TERRAIN_MASK_WALL) return Infinity;
      return t & C.TERRAIN_MASK_SWAMP ? swampCost : plainCost;
    };
    const [gx, gy] = world(to);
    const h = (wx, wy) => Math.max(0, Math.max(Math.abs(wx - gx), Math.abs(wy - gy)) - range);
    const weight = Math.min(9, Math.max(1, options.heuristicWeight || 1.2));
    const [ox, oy] = world(from);
    if (h(ox, oy) === 0) {
      record.incomplete = false;
      return { path: [], ops: 0, cost: 0, incomplete: false };
    }
    room(from.roomName);
    const key = (x, y) => `${x},${y}`;
    const g = new Map([[key(ox, oy), 0]]);
    const parent = new Map();
    const open = [[weight * h(ox, oy), 0, ox, oy]];
    let ops = 0;
    let best = [h(ox, oy), ox, oy];
    while (open.length && ops < maxOps) {
      open.sort((a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2] || a[3] - b[3]);
      const [, cost, x, y] = open.shift();
      if (cost > g.get(key(x, y))) continue;
      ops += 1;
      if (h(x, y) < best[0]) best = [h(x, y), x, y];
      if (h(x, y) === 0) break;
      const lx = ((x % 50) + 50) % 50;
      const ly = ((y % 50) + 50) % 50;
      for (let d = 1; d <= 8; d++) {
        const [dx, dy] = OFFSETS[d];
        if (lx === 0 || lx === 49) {
          const across = (lx === 0 && dx === -1) || (lx === 49 && dx === 1);
          if ((across && dy !== 0) || dx === 0) continue;
        } else if (ly === 0 || ly === 49) {
          const across = (ly === 0 && dy === -1) || (ly === 49 && dy === 1);
          if ((across && dx !== 0) || dy === 0) continue;
        }
        const nx = x + dx;
        const ny = y + dy;
        const step = look(nx, ny);
        if (step === Infinity) continue;
        const next = cost + step;
        if (next < (g.get(key(nx, ny)) ?? Infinity)) {
          g.set(key(nx, ny), next);
          parent.set(key(nx, ny), [x, y]);
          open.push([next + weight * h(nx, ny), next, nx, ny]);
        }
      }
    }
    const path = [];
    let [, x, y] = best;
    while (x !== ox || y !== oy) {
      const p = local(x, y);
      path.push(new RoomPosition(p.x, p.y, p.roomName));
      [x, y] = parent.get(key(x, y));
    }
    path.reverse();
    record.incomplete = best[0] !== 0;
    return { path, ops, cost: g.get(key(best[1], best[2])), incomplete: best[0] !== 0 };
  }
  return { CostMatrix, search };
}

/** A run's world: `level` is W1N1's RCL, `energy` the spawn's at the start, `observer` whether one stands in W1N1. */
function makeWorld({ level, energy, observer, gcl = 1, capacity = C.SPAWN_ENERGY_CAPACITY }) {
  return {
    level,
    observer,
    /** Game.gcl.level (V379), and its reads; W1N1's energyCapacityAvailable; whether the spawns and extensions are
     * refilled to it at once after each loop (run D: `refill`); the rooms the bot claimed (run D). */
    gcl,
    gclReads: [],
    capacity,
    refill: false,
    claimed: new Set(),
    /** Own creeps: { id, name, room, x, y, body, fatigue, spawning, death (the Game.time it is gone), rooms }. */
    creeps: [],
    /** The spawn's accepted order this tick ({ name, body }), and its busy state ({ name, needTime, until }). */
    order: null,
    spawning: null,
    energy,
    sites: [],
    siteOrders: [],
    tombstones: [],
    nextId: 100,
    intents: new Map(),
    /** The observer's look this tick (its one intent slot, V360), and the room it gives on the next tick (V362). */
    observeIntent: null,
    observedNext: null,
    /** Where the tick is: 'before' the snapshot's pass over Game.rooms, in it ('pass'), or 'after' it. */
    phase: 'before',
    passRoom: null,
    calls: [],
    structureCalls: [],
    looks: [],
    spawnCalls: [],
    dropped: [],
    siteCalls: [],
    searches: [],
    statusCalls: [],
    terrainCalls: [],
    /** The bot's writes of segment 94 ({ offset, value }), seen through RawMemory.segments. */
    writes: [],
    reads: [],
    logMarks: [],
    errors: [],
    context: null,
  };
}

/** Why the game side refuses a site of `type` on (x, y) in `room` (V200, V237, V238; sign-sim.mjs's checks), or OK. */
function refuses(state, room, x, y, type) {
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x > 49 || y > 49) return C.ERR_INVALID_ARGS;
  if (!C.CONSTRUCTION_COST[type]) return C.ERR_INVALID_ARGS;
  // W1N1's controller is the bot's, and a room it claimed (run D) is at level 1 (V377); a neutral one has no level,
  // so only level-0 counts are allowed there.
  const level = room === HOME ? state.level : state.claimed.has(room) ? 1 : 0;
  const here = (o) => o.room === room && o.x === x && o.y === y;
  const structures = room === HOME
    ? [{ ...SPAWN, type: 'spawn', room }, ...(state.observer ? [{ ...OBSERVER_AT, type: 'observer', room }] : [])]
    : [];
  const sites = [...state.sites, ...state.siteOrders];
  const have = [...structures, ...sites].filter((s) => s.room === room && s.type === type).length;
  if (have >= (C.CONTROLLER_STRUCTURES[type]?.[level] ?? 0)) return C.ERR_RCL_NOT_ENOUGH;
  if (x <= 0 || y <= 0 || x >= 49 || y >= 49) return C.ERR_INVALID_TARGET;
  if (type !== 'road' && type !== 'container') {
    const border = x === 1 ? [[0, y - 1], [0, y], [0, y + 1]] : x === 48 ? [[49, y - 1], [49, y], [49, y + 1]] : null;
    const rows = y === 1 ? [[x - 1, 0], [x, 0], [x + 1, 0]] : y === 48 ? [[x - 1, 49], [x, 49], [x + 1, 49]] : border;
    if (rows && rows.some(([bx, by]) => !isWall(room, bx, by))) return C.ERR_INVALID_TARGET;
  }
  if (type !== 'road' && isWall(room, x, y)) return C.ERR_INVALID_TARGET;
  if (sites.some(here)) return C.ERR_INVALID_TARGET;
  const costed = structures.filter(here);
  if (costed.some((s) => s.type === type)) return C.ERR_INVALID_TARGET;
  if (type !== 'road' && type !== 'rampart' && costed.some((s) => s.type !== 'road' && s.type !== 'rampart')) return C.ERR_INVALID_TARGET;
  if (sites.length >= C.MAX_CONSTRUCTION_SITES) return C.ERR_FULL;
  return C.OK;
}

function sceneFor(state, offset) {
  const time = T0 + offset;
  state.intents.clear();
  state.order = null;
  state.siteOrders = [];
  state.observeIntent = null;
  state.phase = 'before';
  state.passRoom = null;
  const observed = state.observedNext;
  state.observedNext = null;
  // A room is visible while an own creep stands in it, spawning or not, the owned one always (V101), and on the tick
  // after an observer's look (V362).
  const visible = new Set([HOME, ...state.claimed, ...state.creeps.map((c) => c.room), ...(observed ? [observed] : [])]);
  const explorer = state.creeps.find((c) => c.name.startsWith('x'));
  const r = {
    offset, snapshots: 0, structurePasses: 0, visible: ROOMS.filter((n) => visible.has(n)), observed,
    pass: {}, other: {}, misplaced: [],
    explorer: explorer ? { name: explorer.name, room: explorer.room, x: explorer.x, y: explorer.y, spawning: explorer.spawning } : null,
  };
  state.reads[offset] = r;
  // A read counted by room, in the snapshot's pass over the rooms or not; one in the pass for a room other than the
  // one whose reads came last is misplaced.
  const note = (room, key) => {
    const where = state.phase === 'pass' ? r.pass : r.other;
    const m = (where[room] ??= {});
    m[key] = (m[key] ?? 0) + 1;
    if (state.phase === 'pass' && state.passRoom !== room) r.misplaced.push(`${key} of ${room} while reading ${state.passRoom}`);
  };
  const intentsOf = (cid) => {
    if (!state.intents.has(cid)) state.intents.set(cid, {});
    return state.intents.get(cid);
  };

  // The visible rooms' controllers: W1N1's with an owned controller's getters (V189), the others neutral (`my`
  // undefined, V101); W2N2's reserved by its invader core.
  const controllers = {};
  for (const room of ROOMS.filter((n) => visible.has(n))) {
    const c = CONTROLLER[room];
    const owned = room === HOME || state.claimed.has(room);
    const level = room === HOME ? state.level : owned ? 1 : 0;
    controllers[room] = {
      id: id(1 + ROOMS.indexOf(room)), structureType: 'controller', pos: pos(room, c.x, c.y), level,
      ...(owned ? {
        my: true, owner: { username: USER }, progress: 0, progressTotal: C.CONTROLLER_LEVELS[level],
        ticksToDowngrade: C.CONTROLLER_DOWNGRADE[level],
      } : {}),
      get reservation() {
        note(room, 'reservation');
        return room === CORNER ? { username: INVADER, ticksToEnd: RESERVATION_END - time } : undefined;
      },
      sign: undefined,
    };
  }
  const record = (c, method, args, result, extra = {}) => {
    const call = {
      offset, actor: c.name, method, argCount: args.length,
      args: args.map((a) => (a && typeof a === 'object' ? a.name ?? a.structureType ?? a.id : a)), result,
      from: { room: c.room, x: c.x, y: c.y }, ...extra,
    };
    state.calls.push(call);
    return call;
  };
  const taken = new Set();
  const spawn = {
    id: id(10), name: 'Spawn1', structureType: 'spawn', my: true, owner: { username: USER }, pos: pos(HOME, SPAWN.x, SPAWN.y),
    store: makeStore({ energy: Math.min(state.energy, C.SPAWN_ENERGY_CAPACITY) }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    spawning: state.spawning ? { name: state.spawning.name, needTime: state.spawning.needTime, remainingTime: state.spawning.until - offset + 1 } : null,
    // The runtime's checks in V171's order and its `Memory.creeps[name]` write on OK (V181).
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      const call = { offset, args: args.length, name, body: Array.isArray(body) ? [...body] : body, result: C.OK };
      state.spawnCalls.push(call);
      const parts = new Set(C.BODYPARTS_ALL);
      const cost = Array.isArray(body) ? body.reduce((s, p) => s + (C.BODYPART_COST[p] ?? 0), 0) : 0;
      if (!name || opts === null || typeof opts !== 'object' || `${name}`.length > 100) call.result = C.ERR_INVALID_ARGS;
      else if (state.creeps.some((c) => c.name === `${name}`) || taken.has(`${name}`)) call.result = C.ERR_NAME_EXISTS;
      else if (state.spawning) call.result = C.ERR_BUSY;
      else if (!Array.isArray(body) || !body.length || body.length > C.MAX_CREEP_SIZE || body.some((p) => !parts.has(p))) call.result = C.ERR_INVALID_ARGS;
      else if (state.energy < cost) call.result = C.ERR_NOT_ENOUGH_ENERGY;
      else if (!opts.dryRun) {
        taken.add(`${name}`);
        state.order = { name: `${name}`, body: [...body] };
        const mem = state.context.Memory;
        if (mem && typeof mem === 'object') {
          if (mem.creeps === undefined) mem.creeps = {};
          if (mem.creeps && typeof mem.creeps === 'object') mem.creeps[name] = opts.memory || mem.creeps[name] || {};
        }
      }
      return call.result;
    },
  };
  // The act layer's other bindings on structures (a destroy, ADR-026): nothing here calls for them.
  const recordStructure = (s, method, args, result, extra = {}) => {
    const call = { offset, structure: s.structureType, method, argCount: args.length, args: [...args], result, ...extra };
    state.structureCalls.push(call);
    return call.result;
  };
  spawn.destroy = (...args) => recordStructure(spawn, 'destroy', args, C.OK);
  if (controllers[HOME]) controllers[HOME].destroy = (...args) => recordStructure(controllers[HOME], 'destroy', args, C.OK);
  const owned = [spawn, ...(controllers[HOME] ? [controllers[HOME]] : [])];
  // A claimed room's controller is an own structure too (run D).
  const claimedControllers = [...state.claimed].map((n) => controllers[n]).filter(Boolean);
  if (state.observer) {
    const observer = {
      id: id(11), structureType: 'observer', my: true, owner: { username: USER }, pos: pos(HOME, OBSERVER_AT.x, OBSERVER_AT.y),
      hits: C.OBSERVER_HITS, hitsMax: C.OBSERVER_HITS,
      // V358's checks in order (the observer is the bot's), V359's range, then the intent's one slot (V360).
      observeRoom(...args) {
        const [name] = args;
        const level = state.level;
        let result = C.OK;
        if (typeof name !== 'string' || !/^(W|E)\d+(S|N)\d+$/.test(name)) result = C.ERR_INVALID_ARGS;
        else if (!C.CONTROLLER_STRUCTURES.observer[level]) result = C.ERR_RCL_NOT_ENOUGH;
        else {
          const [tx, ty] = roomXY(name);
          const [x, y] = roomXY(HOME);
          if (Math.abs(tx - x) > C.OBSERVER_RANGE || Math.abs(ty - y) > C.OBSERVER_RANGE) result = C.ERR_NOT_IN_RANGE;
        }
        if (result === C.OK) state.observeIntent = name;
        state.looks.push({ offset, room: name, argCount: args.length, type: typeof name, result, visible: visible.has(name) });
        return recordStructure(observer, 'observeRoom', args, result);
      },
    };
    observer.destroy = (...args) => recordStructure(observer, 'destroy', args, C.OK);
    owned.push(observer);
  }

  const creeps = state.creeps.map((c) => {
    const parts = c.body.length;
    const self = {
      id: c.id, name: c.name, my: true, spawning: c.spawning, fatigue: c.fatigue, pos: pos(c.room, c.x, c.y),
      body: c.body.map((type) => ({ type, hits: 100 })), hits: 100 * parts, hitsMax: 100 * parts,
      ticksToLive: c.spawning ? undefined : c.death - time,
      store: makeStore({}, { capacity: C.CARRY_CAPACITY * count(c.body, 'carry') }),
      owner: { username: USER },
    };
    // The act layer's other bindings: nothing here calls for them; recorded (OK) so they show up as unexpected.
    for (const m of CREEP_METHODS) self[m] = (...args) => record(c, m, args, C.OK).result;
    // V108's shared first checks (the creep is the bot's), then each method's own.
    self.move = (...args) => {
      const d = +args[0];
      const result = c.spawning ? C.ERR_BUSY : c.fatigue > 0 ? C.ERR_TIRED : !count(c.body, 'move') ? C.ERR_NO_BODYPART
        : !Number.isInteger(d) || d < 1 || d > 8 ? C.ERR_INVALID_ARGS : C.OK;
      if (result === C.OK) intentsOf(c.id).move = d;
      return record(c, 'move', args, result).result;
    };
    self.suicide = (...args) => {
      const result = c.spawning ? C.ERR_BUSY : C.OK;
      if (result === C.OK) intentsOf(c.id).suicide = true;
      return record(c, 'suicide', args, result).result;
    };
    // V374's checks in order (the creep is the bot's; no novice area, shard access, or safe mode in this world): the
    // GCL against the owned rooms and this tick's claims (GCL N allows N rooms, V378), the target, a CLAIM part, range 1,
    // a controller at level 0 with no other user's reservation; then the intent (V376, V377 in `advance`).
    self.claimController = (...args) => {
      const [target] = args;
      const owned = 1 + state.claimed.size + [...state.intents.values()].filter((i) => i.claim).length;
      const near = target?.pos && target.pos.roomName === c.room && Math.max(Math.abs(target.pos.x - c.x), Math.abs(target.pos.y - c.y)) <= 1;
      const reservation = target?.structureType === 'controller' ? target.reservation : undefined;
      const result = c.spawning ? C.ERR_BUSY
        : owned >= state.gcl ? C.ERR_GCL_NOT_ENOUGH
          : !target?.structureType ? C.ERR_INVALID_TARGET
            : !count(c.body, 'claim') ? C.ERR_NO_BODYPART
              : !near ? C.ERR_NOT_IN_RANGE
                : target.structureType !== 'controller' || target.level > 0 ? C.ERR_INVALID_TARGET
                  : reservation && reservation.username !== USER ? C.ERR_INVALID_TARGET
                    : C.OK;
      if (result === C.OK) intentsOf(c.id).claim = target.pos.roomName;
      return record(c, 'claimController', args, result).result;
    };
    return self;
  });

  const siteObject = (s) => ({
    id: s.id, my: true, owner: { username: USER }, structureType: s.type, progress: 0, progressTotal: C.CONSTRUCTION_COST[s.type],
    pos: pos(s.room, s.x, s.y),
  });
  const tombstones = state.tombstones.map((t) => ({
    id: t.id, pos: pos(t.room, t.x, t.y), deathTime: t.deathTime, ticksToDecay: t.decayTime - time, store: makeStore({}),
    creep: { id: t.creepId, name: t.name, my: true },
  }));
  const rooms = {};
  for (const name of ROOMS.filter((n) => visible.has(n))) {
    const inRoom = (o) => o.pos.roomName === name;
    const sites = state.sites.filter((s) => s.room === name).map(siteObject);
    // The engine's find cache leaves spawning creeps out of its creep lists (V101 for hostiles; assumed here for own).
    const mine = creeps.filter((c) => inRoom(c) && !c.spawning);
    const sources = (SOURCES[name] ?? []).map((s, i) => ({
      id: id(20 + 4 * ROOMS.indexOf(name) + i), pos: pos(name, s.x, s.y), energy: C.SOURCE_ENERGY_CAPACITY,
      energyCapacity: name === CORNER ? C.SOURCE_ENERGY_CAPACITY : C.SOURCE_ENERGY_NEUTRAL_CAPACITY, ticksToRegeneration: undefined,
    }));
    const m = MINERAL[name];
    const minerals = m ? [{
      id: id(40 + ROOMS.indexOf(name)), pos: pos(name, m.x, m.y), mineralAmount: C.MINERAL_DENSITY[m.density], ticksToRegeneration: undefined,
      get mineralType() { note(name, 'mineralType'); return m.type; },
      get density() { note(name, 'density'); return m.density; },
    }] : [];
    const core = CORE[name];
    const hostileStructures = core ? [{
      id: id(50 + ROOMS.indexOf(name)), structureType: C.STRUCTURE_INVADER_CORE, my: false, owner: { username: INVADER },
      pos: pos(name, core.x, core.y), hits: C.INVADER_CORE_HITS, hitsMax: C.INVADER_CORE_HITS, ticksToDeploy: undefined, effects: [],
      get level() { note(name, 'level'); return core.level; },
    }] : [];
    const structures = name === HOME ? owned : [controllers[name], ...hostileStructures];
    const mineOwned = name === HOME ? owned : state.claimed.has(name) ? [controllers[name]] : [];
    const lists = {
      [C.FIND_STRUCTURES]: structures, [C.FIND_MY_STRUCTURES]: mineOwned, [C.FIND_MY_SPAWNS]: name === HOME ? [spawn] : [],
      [C.FIND_HOSTILE_STRUCTURES]: hostileStructures, [C.FIND_SOURCES]: sources, [C.FIND_SOURCES_ACTIVE]: sources,
      [C.FIND_MINERALS]: minerals, [C.FIND_CONSTRUCTION_SITES]: sites, [C.FIND_MY_CONSTRUCTION_SITES]: sites,
      [C.FIND_CREEPS]: mine, [C.FIND_MY_CREEPS]: mine, [C.FIND_TOMBSTONES]: tombstones.filter(inRoom),
    };
    const room = {
      name,
      controller: controllers[name],
      energyAvailable: name === HOME ? state.energy : 0,
      energyCapacityAvailable: name === HOME ? state.capacity : 0,
      find(type, opts) {
        const filter = opts?.filter;
        const key = filter ? `${type}:${filter.structureType ?? 'other'}` : `${type}`;
        // Every room's reads in the pass start with its hostile creeps (snapshot.rs `push_room`).
        if (type === C.FIND_HOSTILE_CREEPS && state.phase === 'pass') state.passRoom = name;
        note(name, key);
        return findFiltered(lists[type] ?? [], opts);
      },
      getTerrain() { return { getRawBuffer: () => TERRAIN[name].slice() }; },
      createConstructionSite(...args) {
        const [x, y, type] = args;
        const call = { offset, room: name, x, y, type, args: args.length, sameRoom: this === room, result: refuses(state, name, x, y, type) };
        state.siteCalls.push(call);
        if (call.result === C.OK) state.siteOrders.push({ room: name, x, y, type });
        return call.result;
      },
    };
    rooms[name] = room;
  }
  // The snapshot's pass over Object.values(Game.rooms) starts the pass; its pass over Game.structures, next, ends it.
  const roomsSeen = new Proxy(rooms, {
    ownKeys(target) {
      r.snapshots += 1;
      state.phase = 'pass';
      return Reflect.ownKeys(target);
    },
  });
  const structuresSeen = new Proxy(Object.fromEntries([...owned, ...claimedControllers].map((s) => [s.id, s])), {
    ownKeys(target) {
      r.structurePasses += 1;
      state.phase = 'after';
      return Reflect.ownKeys(target);
    },
  });
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: roomsSeen,
    structures: structuresSeen,
    gcl: {
      get level() { state.gclReads.push(offset); return state.gcl; },
      progress: 0,
      progressTotal: C.GCL_MULTIPLY,
    },
    map: {
      getRoomStatus(name) {
        state.statusCalls.push({ offset, room: name, phase: state.phase });
        note(name, 'status');
        return { status: STATUS[name] ?? 'normal', timestamp: null };
      },
      // V156: no vision needed. Nothing in this world asks for it (no signer).
      getRoomTerrain(name) {
        state.terrainCalls.push([offset, name]);
        return { getRawBuffer: () => TERRAIN[name]?.slice() ?? new Uint8Array(2500) };
      },
    },
  };
}

function isObstacle(state, room, x, y) {
  if (isWall(room, x, y)) return 'a wall';
  if (room === HOME && x === SPAWN.x && y === SPAWN.y) return 'the spawn';
  if (room === HOME && state.observer && x === OBSERVER_AT.x && y === OBSERVER_AT.y) return 'the observer';
  const statics = [...(SOURCES[room] ?? []), ...(MINERAL[room] ? [MINERAL[room]] : []), ...(CORE[room] ? [CORE[room]] : [])];
  if (statics.some((s) => s.x === x && s.y === y)) return 'a source, mineral or core';
  return null;
}

/** The engine's move resolution (V51, V52), fatigue for the creeps that moved (V54), then the room transfers (V151,
 * V168); from sign-sim.mjs's applyMoves. */
function applyMoves(state, offset) {
  const moves = new Map();
  for (const c of state.creeps) {
    const d = state.intents.get(c.id)?.move;
    if (d === undefined) continue;
    const [dx, dy] = OFFSETS[d];
    moves.set(c, { x: c.x + dx, y: c.y + dy });
  }
  let changed = true;
  while (changed) {
    changed = false;
    for (const [c, d] of moves) {
      const others = [...moves].filter(([o, e]) => o !== c && o.room === c.room && e.x === d.x && e.y === d.y);
      const stays = state.creeps.some((o) => o !== c && o.room === c.room && o.x === d.x && o.y === d.y && !moves.has(o));
      const off = d.x < 0 || d.y < 0 || d.x > 49 || d.y > 49 ? 'off the room' : null;
      const why = off ?? isObstacle(state, c.room, d.x, d.y) ?? (others.length ? `${others[0][0].name} moves there too` : stays ? 'a creep stays there' : null);
      if (why) {
        state.errors.push(`offset ${offset}: ${c.name}'s move to ${c.room} ${at(d)} fails (${why})`);
        moves.delete(c);
        changed = true;
      }
    }
  }
  const edge = (p) => p.x === 0 || p.y === 0 || p.x === 49 || p.y === 49;
  for (const [c, d] of moves) {
    const wasEdge = edge(c);
    c.x = d.x;
    c.y = d.y;
    if (edge(c) && !wasEdge) {
      c.fatigue = 0;
      continue;
    }
    const swamp = (TERRAIN[c.room][c.y * 50 + c.x] & C.TERRAIN_MASK_SWAMP) !== 0;
    const weight = c.body.filter((p) => p !== 'move' && p !== 'carry').length;
    c.fatigue += weight * (swamp ? 10 : 2);
  }
  for (const c of state.creeps) {
    if (c.spawning || !edge(c)) continue;
    const [rx, ry] = roomXY(c.room);
    const next = c.x === 0 ? roomAt(rx - 1, ry) : c.x === 49 ? roomAt(rx + 1, ry) : c.y === 0 ? roomAt(rx, ry - 1) : roomAt(rx, ry + 1);
    if (!TERRAIN[next]) {
      state.errors.push(`offset ${offset}: ${c.name} ended the tick on ${c.room} ${at(c)}, an exit to ${next}, which this world has not`);
      continue;
    }
    if (STATUS[next] === 'closed') {
      // V168: the transfer into a room not in accessibleRooms is skipped, and the creep stays on its edge tile.
      state.errors.push(`offset ${offset}: ${c.name} ended the tick on ${c.room} ${at(c)}, an exit to the closed ${next}`);
      continue;
    }
    if (c.x === 0) c.x = 49; else if (c.x === 49) c.x = 0; else if (c.y === 0) c.y = 49; else c.y = 0;
    c.room = next;
    c.rooms.push(next);
  }
}

/** The processor after loop `offset`: the spawn's order (V173), the suicides, the moves, the creep ticks (V55), the
 * spawn's tick (V174), age (V170), the spawn's energy, the tombstones' decay, the new sites and the look (V362). */
function advance(state, offset) {
  const time = T0 + offset;
  if (state.order) {
    const { name, body } = state.order;
    const cost = body.reduce((s, p) => s + C.BODYPART_COST[p], 0);
    if ((name.startsWith('x') || name.startsWith('k')) && !state.spawning && state.energy >= cost) {
      state.energy -= cost;
      const needTime = C.CREEP_SPAWN_TIME * body.length;
      state.spawning = { name, needTime, until: offset + needTime };
      state.creeps.push({ id: id(state.nextId++), name, room: HOME, ...SPAWN, body, fatigue: 0, spawning: true, death: null, rooms: [HOME] });
    } else {
      state.dropped.push({ offset, name });
    }
  }
  // A claim (V376, V377): the room is the bot's at level 1 from the next tick.
  for (const i of state.intents.values()) if (i.claim) state.claimed.add(i.claim);
  for (const c of state.creeps.filter((x) => state.intents.get(x.id)?.suicide)) {
    state.creeps = state.creeps.filter((x) => x !== c);
    state.tombstones.push({
      id: id(state.nextId++), room: c.room, x: c.x, y: c.y, name: c.name, creepId: c.id, deathTime: time,
      decayTime: time + C.TOMBSTONE_DECAY_PER_PART * c.body.length,
    });
  }
  applyMoves(state, offset);
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  if (state.spawning && offset >= state.spawning.until) {
    const c = state.creeps.find((x) => x.name === state.spawning.name);
    const free = OFFSETS.slice(1).map(([dx, dy]) => ({ x: SPAWN.x + dx, y: SPAWN.y + dy }))
      .find((t) => !isObstacle(state, HOME, t.x, t.y) && !state.creeps.some((o) => o !== c && o.room === HOME && o.x === t.x && o.y === t.y));
    if (c && free) {
      c.x = free.x;
      c.y = free.y;
      c.spawning = false;
      // ticksToLive is CREEP_LIFE_TIME on the first loop out (see the header).
      c.death = time + 1 + C.CREEP_LIFE_TIME;
      state.spawning = null;
    }
  }
  state.creeps = state.creeps.filter((c) => c.spawning || c.death > time + 1);
  if (state.refill) state.energy = state.capacity;
  else if (state.energy < C.SPAWN_ENERGY_CAPACITY) state.energy += 1;
  state.tombstones = state.tombstones.filter((t) => time < t.decayTime - 1);
  for (const s of state.siteOrders) state.sites.push({ id: id(state.nextId++), ...s, since: offset });
  state.observedNext = state.observeIntent;
}

/**
 * P8.4's CPU fragment of a claims summary: present once, the load with one more room `factor` times the mean
 * (`Claiming::cpu_check`: (owned + 1) over the rooms at `claim.min_home_level` or more), each shown to two decimals.
 */
function checkCpu(problems, summary, factor) {
  const m = CPU_PART.exec(summary ?? '');
  if (!m || (summary.match(/; CPU /g) ?? []).length !== 1) {
    problems.push(`the claims summary ${JSON.stringify(summary)} has no single CPU fragment matching ${CPU_PART}`);
    return;
  }
  const [mean, projected] = [Number(m[1]), Number(m[2])];
  // Both are rounded to 0.005: the projection's error is its own plus `factor` times the mean's.
  if (Math.abs(projected - mean * factor) > 0.005 * (1 + factor) + 1e-9) {
    problems.push(`the CPU fragment ${JSON.stringify(m[0])}: ${projected} with another room, expected ${mean} × ${factor}`);
  }
}

/** A sim of `state`'s world, with the console's commands from `command(offset, lines)`. */
function runWorld(state, dir, ticks, command) {
  state.creeps.push({
    id: id(state.nextId++), name: KEEPER.name, room: HOME, x: KEEPER.x, y: KEEPER.y, body: ['move'], fatigue: 0, spawning: false,
    death: T0 + C.CREEP_LIFE_TIME - 10, rooms: [HOME],
  });
  let finished = false;
  const world = simulate(dir, {
    ticks,
    setup: (context, w) => {
      // The overrides in segment CONFIG_SEGMENT before the first global, in the config's field order.
      w.segments[CONFIG_SEGMENT] ??= configRecord(state.configLines ?? CONFIG_LINES);
      state.context = context;
      context.Creep = EngineCreep;
      context.PathFinder = makePathFinder(state, definePositions(context));
    },
    // The driver's RawMemory, with the bot's assignments to segment ATLAS_SEGMENT recorded (the persist glue writes
    // with Reflect.set; the end-of-tick save reads the same object).
    rawMemory: (offset, w) => {
      const raw = driverRawMemory(offset, w);
      raw.segments = new Proxy(raw.segments, {
        set(target, key, value) {
          if (String(key) === String(ATLAS_SEGMENT)) state.writes.push({ offset, value });
          target[key] = value;
          return true;
        },
      });
      return raw;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      state.logMarks[offset] = w.logs.length;
      const lines = w.logs.slice(state.logMarks[offset - 1] ?? 0);
      advance(state, offset);
      const out = command(offset, lines, w);
      if (out === 'finish') {
        finished = true;
        return null;
      }
      if (out && out.finish) {
        finished = true;
        return out.cmd;
      }
      return out;
    },
    until: () => finished,
  });
  return world;
}

/** Decodes a segment frame (persist/frame.rs; see smoke.mjs configRecord): its header, CRC check and payload. */
function decodeFrame(text) {
  if (typeof text !== 'string' || !text.startsWith('rs1b')) return null;
  const bytes = Buffer.from(text.slice(4), 'base64');
  if (bytes.length < 16) return null;
  const head = bytes.subarray(0, 12);
  const length = bytes.readUInt32LE(8);
  const payload = bytes.subarray(16);
  return {
    kind: bytes.readUInt16LE(0), schema: bytes.readUInt16LE(2), written: bytes.readUInt32LE(4), length,
    crcOk: payload.length === length && crc32(Buffer.concat([head, payload])) === bytes.readUInt32LE(12), payload,
  };
}

/** The atlas's payload (expansion/atlas.rs): its entries, each field by its name there. */
function decodeAtlas(payload) {
  let i = 0;
  const u8 = () => payload.readUInt8(i++);
  const u16 = () => { const v = payload.readUInt16LE(i); i += 2; return v; };
  const u32 = () => { const v = payload.readUInt32LE(i); i += 4; return v; };
  const entries = [];
  const n = u16();
  for (let k = 0; k < n; k++) {
    const e = {
      room: u16(), seen: u32(), flags: u8(), status: u8(), owner: u8(), level: u8(), controller: u16(), reserve: u8(),
      reserveEnd: u32(), safeEnd: u32(),
    };
    const sources = u8();
    e.sources = [];
    for (let s = 0; s < sources; s++) e.sources.push(u16());
    Object.assign(e, { mineral: u8(), mineralAt: u16(), density: u8(), towers: u8(), spawns: u8(), lairs: u8(), core: u8(), hostiles: u8() });
    entries.push(e);
  }
  return { entries, rest: payload.length - i };
}

/** A room's atlas entry after its read at `seen` (expansion/atlas.rs's codes: owner neutral 1, reserve other 2, the
 * minerals' codes H 1 and O 2, status normal 0 and closed 1, no tile 65535, no core 255). */
function readEntry(room, seen) {
  const m = MINERAL[room];
  return {
    room: packedRoom(room), seen, flags: 1, status: STATUS[room] === 'closed' ? 1 : 0, owner: 1, level: 0, controller: tile(CONTROLLER[room]),
    reserve: room === CORNER ? 2 : 0, reserveEnd: room === CORNER ? RESERVATION_END : 0, safeEnd: 0,
    sources: (SOURCES[room] ?? []).map(tile),
    mineral: m ? { [C.RESOURCE_HYDROGEN]: 1, [C.RESOURCE_OXYGEN]: 2 }[m.type] : 0, mineralAt: m ? tile(m) : 65535, density: m ? m.density : 0,
    towers: 0, spawns: 0, lairs: 0, core: CORE[room] ? CORE[room].level : 255, hostiles: 0,
  };
}

/** An entry the explorer marked unreachable at `seen` with no read before (RoomIntel::new, then the mark). */
function unreachableEntry(room, seen) {
  return {
    room: packedRoom(room), seen, flags: 2, status: 0, owner: 0, level: 0, controller: 65535, reserve: 0, reserveEnd: 0, safeEnd: 0,
    sources: [], mineral: 0, mineralAt: 65535, density: 0, towers: 0, spawns: 0, lairs: 0, core: 255, hostiles: 0,
  };
}

/** The reads a room's atlas read makes in the snapshot's pass (snapshot.rs `push_intel`), each once. */
function intelKeys(room) {
  return [
    'status', `${C.FIND_MINERALS}`, ...HOSTILE_TYPES.map((t) => `${C.FIND_HOSTILE_STRUCTURES}:${t}`),
    ...(room === HOME ? [] : [`${C.FIND_SOURCES}`, 'reservation']),
    ...(MINERAL[room] ? ['mineralType', 'density'] : []),
    ...(CORE[room] ? ['level'] : []),
  ];
}
/** Reads only the atlas makes: never outside the pass. */
const INTEL_ONLY = [...HOSTILE_TYPES.map((t) => `${C.FIND_HOSTILE_STRUCTURES}:${t}`), 'reservation', 'mineralType', 'density', 'level'];

/** Shared checks of the atlas reads: `readAt` maps each room read to the loop of its read line. */
function checkReads(problems, loops, readAt) {
  for (const r of loops) {
    if (r.snapshots !== 1 || r.structurePasses !== 1) problems.push(`offset ${r.offset}: Game.rooms enumerated ${r.snapshots} and Game.structures ${r.structurePasses} time(s), expected once each`);
    problems.push(...r.misplaced.slice(0, 3).map((m) => `offset ${r.offset}: ${m}`));
    for (const room of ROOMS) {
      const reading = readAt[room] === r.offset;
      for (const key of intelKeys(room)) {
        const n = r.pass[room]?.[key] ?? 0;
        if (n !== (reading ? 1 : 0)) problems.push(`offset ${r.offset}: ${n} ${key} read(s) of ${room} in the snapshot's pass, expected ${reading ? 1 : 0}`);
      }
      for (const key of INTEL_ONLY) {
        const n = r.other[room]?.[key] ?? 0;
        if (n) problems.push(`offset ${r.offset}: ${n} ${key} read(s) of ${room} outside the snapshot's pass`);
      }
    }
  }
}

/** The log and call rules both runs share. */
function checkLog(problems, world, state, allowed, terrainOk = false, more = {}) {
  const bad = world.logs.filter((l) => /^rustedscreep (fault|incident)/.test(l) || l.startsWith('rustedscreep persist:')
    || (l.startsWith('rustedscreep scout: ') && !allowed(l)));
  problems.push(...bad.slice(0, 10).map((l) => `log: ${l}`));
  for (const c of state.calls) {
    if (more.creep?.(c)) continue;
    if ((c.method !== 'move' && c.method !== 'suicide') || c.result !== C.OK || !c.actor.startsWith('x')) {
      problems.push(`offset ${c.offset}: ${c.actor} called ${c.method}(${c.args.join(', ')}) from ${c.from.room} ${at(c.from)}: ${c.result}`);
    }
  }
  for (const c of state.structureCalls) {
    if (c.method !== 'observeRoom') problems.push(`offset ${c.offset}: ${c.structure} ${c.method}(${c.args.join(', ')}): ${c.result}`);
  }
  for (const c of state.siteCalls) {
    if (more.site?.(c)) continue;
    if (c.room !== HOME || c.result !== C.OK || c.args !== 3 || !c.sameRoom) problems.push(`offset ${c.offset}: ${c.room} createConstructionSite(${c.x}, ${c.y}, ${c.type}) with ${c.args} arguments: ${c.result}`);
  }
  if (state.terrainCalls.length && !terrainOk) problems.push(`Game.map.getRoomTerrain calls ${JSON.stringify(state.terrainCalls.slice(0, 4))}`);
  if (world.segmentErrors.length) problems.push(`segment saves the driver would refuse: ${world.segmentErrors.join(' / ')}`);
}

/** Segment 94's requests and writes within one global (`from` to `to`), after its config record loaded at `loadedAt`. */
function checkSegment(problems, world, state, g, [from, to], loadedAt) {
  const calls = (world.activeCalls ?? []).filter(([o]) => o >= from && o <= to);
  const early = calls.filter(([o, ids]) => o <= loadedAt && ids.includes(ATLAS_SEGMENT));
  const later = calls.filter(([o, ids]) => o > loadedAt && ids.includes(ATLAS_SEGMENT));
  if (early.length) problems.push(`global ${g}: setActiveSegments with ${ATLAS_SEGMENT} at ${JSON.stringify(early)}, before or on the config load at ${loadedAt}`);
  if (!later.length) problems.push(`global ${g}: no setActiveSegments call with ${ATLAS_SEGMENT} after the config load at ${loadedAt} (calls ${JSON.stringify(calls)})`);
  const writes = state.writes.filter((w) => w.offset >= from && w.offset <= to);
  for (const w of writes) if (typeof w.value !== 'string' || !w.value.startsWith('rs1b')) problems.push(`offset ${w.offset}: segment ${ATLAS_SEGMENT} written as ${JSON.stringify(w.value).slice(0, 40)}`);
  writes.forEach((w, i) => {
    const before = writes[i - 1];
    if (before && w.offset - before.offset < SAVE_TICKS) problems.push(`segment ${ATLAS_SEGMENT} written at offsets ${before.offset} and ${w.offset}, less than scout.save_ticks (${SAVE_TICKS}) apart`);
  });
  return writes;
}

/** Checks a write's frame and its atlas against `want` (entries in packed-name order). */
function checkRecord(problems, write, want, label) {
  const f = write ? decodeFrame(write.value) : null;
  if (!f || f.kind !== 5 || f.schema !== 1 || f.written !== T0 + write.offset || !f.crcOk) {
    problems.push(`${label}: the frame ${JSON.stringify(f && { ...f, payload: undefined })}, expected kind 5 (persist::kind::ATLAS), schema 1, written at ${write ? T0 + write.offset : '?'}, a good CRC-32`);
    return;
  }
  const atlas = decodeAtlas(f.payload);
  const sorted = [...want].sort((a, b) => a.room - b.room);
  if (atlas.rest !== 0 || JSON.stringify(atlas.entries) !== JSON.stringify(sorted)) {
    problems.push(`${label}: the atlas ${JSON.stringify(atlas)}, expected ${JSON.stringify(sorted)}`);
  }
}

/** Run A: the explorer's tour, the record, a halt and the record's load. */
export function explorerRun(dir) {
  const state = makeWorld({ level: 3, energy: C.SPAWN_ENERGY_CAPACITY, observer: false });
  let phase = 'tour';
  const marks = {};
  const world = runWorld(state, dir, TOUR_MAX + SETTLE + AFTER + 20, (offset, lines) => {
    if (phase === 'tour' && lines.some((l) => DONE_LINE.test(l))) {
      phase = 'settle';
      marks.done = offset;
    } else if (phase === 'tour' && offset >= TOUR_MAX) {
      phase = 'stuck';
      return 'finish';
    } else if (phase === 'settle' && offset >= marks.done + SETTLE) {
      phase = 'summary';
      marks.summary = offset;
      return SUMMARY;
    } else if (phase === 'summary') {
      phase = 'halted';
      marks.halt = offset;
      return HALT;
    } else if (phase === 'halted' && offset >= marks.halt + AFTER) {
      phase = 'done';
      marks.final = offset;
      return { finish: true, cmd: SUMMARY };
    }
    return null;
  });

  const problems = [...state.errors.slice(0, 10)];
  const fmt = (o) => (o === undefined ? 'none' : `offset ${o}`);
  // This loop's log lines (after the previous loop's mark, with that loop's command and its reply).
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (test) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some(test));
  if (phase !== 'done') problems.push(`not finished in ${world.ticksRun} ticks (phase ${phase}; marks ${JSON.stringify(marks)})`);
  if (world.halts !== 1 || world.globals !== 2) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 1 and 2`);
  const haltAt = marks.halt ?? Infinity;
  const loops = state.reads.filter((r) => r && r.snapshots > 0);

  // 7. The log: no fault, no incident, no other scout line, no persistence warning; the calls.
  const allowed = (l) => READ_LINE.test(l) || l === UNREACHABLE_LINE || DONE_LINE.test(l) || LOADED_LINE.test(l);
  checkLog(problems, world, state, allowed);
  if (state.looks.length) problems.push(`observeRoom calls with no observer: ${JSON.stringify(state.looks)}`);

  // 2. The one spawnCreep call: the explorer's.
  const [order] = state.spawnCalls;
  if (state.spawnCalls.length !== 1) problems.push(`spawnCreep calls ${JSON.stringify(state.spawnCalls.map((c) => [c.offset, c.name, c.result]))}, expected one (the explorer's)`);
  if (!order || !/^xW1N1-[0-9a-z]+$/.test(order.name ?? '') || JSON.stringify(order.body) !== JSON.stringify(EXPLORER_BODY) || order.args !== 2 || order.result !== C.OK) {
    problems.push(`the explorer's spawnCreep ${JSON.stringify(order)}, expected (${JSON.stringify(EXPLORER_BODY)}, "xW1N1-…"; ${EXPLORER_COST} energy) and OK`);
  }
  if (state.dropped.length) problems.push(`spawn orders the processor dropped: ${JSON.stringify(state.dropped)}`);
  const explorerName = order?.name;
  const lived = loops.filter((r) => r.explorer);
  const outAt = lived.find((r) => !r.explorer.spawning)?.offset;
  const spawningLoops = lived.filter((r) => r.explorer.spawning).length;
  if (spawningLoops !== C.CREEP_SPAWN_TIME * EXPLORER_BODY.length) problems.push(`the explorer was spawning on ${spawningLoops} loops, expected CREEP_SPAWN_TIME × ${EXPLORER_BODY.length}`);

  // 3. The reads: one line per room the explorer saw, on its first loop there, with the push_intel reads once.
  const readAt = {};
  for (const room of [WEST, CORNER]) {
    const want = `rustedscreep scout: ${room} read: ${describe(room)}`;
    const at = linesAt((l) => l === want);
    const got = world.logs.filter((l) => READ_LINE.exec(l)?.[1] === room);
    const n = got.length;
    const first = loops.find((r) => r.visible.includes(room));
    readAt[room] = at[0];
    if (n !== 1 || at.length !== 1 || at[0] !== first?.offset || first?.explorer?.room !== room) {
      problems.push(`read lines of ${room} ${JSON.stringify(got)} at ${JSON.stringify(at)}, expected one ${JSON.stringify(want)} on the room's first visible loop (${fmt(first?.offset)}) with the explorer there (${first?.explorer?.room})`);
    }
  }
  if (world.logs.some((l) => READ_LINE.exec(l)?.[1] === NORTH || READ_LINE.exec(l)?.[1] === HOME)) problems.push(`a read line for ${NORTH} or ${HOME}`);
  if (loops.some((r) => r.visible.includes(NORTH))) problems.push(`${NORTH} was visible`);
  checkReads(problems, loops, readAt);

  // 4. W1N2: unreachable, with no search into it.
  const unreachableAt = linesAt((l) => l === UNREACHABLE_LINE);
  const unreachableLoop = state.reads[unreachableAt[0]];
  if (world.logs.filter((l) => l === UNREACHABLE_LINE).length !== 1 || unreachableAt.length !== 1 || unreachableLoop?.explorer?.room !== HOME
    || unreachableLoop.explorer.spawning || !(unreachableAt[0] < Math.min(readAt[WEST] ?? -1, readAt[CORNER] ?? -1))) {
    problems.push(`unreachable lines at ${JSON.stringify(unreachableAt)} (explorer ${JSON.stringify(unreachableLoop?.explorer)}), expected one ${JSON.stringify(UNREACHABLE_LINE)} with the explorer out in ${HOME}, before both reads`);
  }
  const north = state.searches.filter((s) => s.to.roomName === NORTH);
  if (north.length) problems.push(`PathFinder.search with a target in ${NORTH}: ${JSON.stringify(north.slice(0, 3))}`);
  const northStatus = state.statusCalls.filter((s) => s.room === NORTH && s.phase !== 'pass' && s.offset < haltAt).length;
  if (!northStatus) problems.push(`Game.map.getRoomStatus("${NORTH}") was never called outside the snapshot's pass`);
  const short = state.searches.filter((s) => s.incomplete !== false);
  if (short.length) problems.push(`incomplete route searches: ${JSON.stringify(short.slice(0, 3))}`);
  const late = state.searches.filter((s) => s.offset > haltAt);
  if (late.length) problems.push(`route searches in the second global: ${JSON.stringify(late.slice(0, 3))}`);

  // 5. The end: one suicide on the loop of the one end line, after both reads, then nothing.
  const suicides = state.calls.filter((c) => c.method === 'suicide');
  const doneLines = world.logs.filter((l) => DONE_LINE.test(l));
  const doneAt = linesAt((l) => DONE_LINE.test(l));
  const end = suicides[0];
  const doneRoom = doneLines.length === 1 ? DONE_LINE.exec(doneLines[0])[1] : null;
  if (suicides.length !== 1 || end.actor !== explorerName || end.argCount !== 0) {
    problems.push(`suicide calls ${JSON.stringify(suicides.map((c) => [c.offset, c.actor, c.argCount]))}: expected one by ${explorerName} with no argument`);
  } else if (doneLines.length !== 1 || doneAt[0] !== end.offset || doneRoom !== end.from.room || doneRoom !== CORNER) {
    problems.push(`end lines ${JSON.stringify(doneLines)} at ${JSON.stringify(doneAt)}, expected one on the suicide's loop (${end.offset}) naming ${end.from.room} (${CORNER})`);
  } else if (!(end.offset >= Math.max(readAt[WEST] ?? Infinity, readAt[CORNER] ?? Infinity))) {
    problems.push(`the explorer ended at offset ${end.offset}, before both reads (${JSON.stringify(readAt)})`);
  }
  if (end && state.calls.some((c) => c.actor === explorerName && c.offset > end.offset)) problems.push('the explorer called something after its suicide');
  if (end && loops.some((r) => r.offset > end.offset && r.explorer)) problems.push('the explorer still stood after its suicide');
  const stood = [...new Set(lived.map((r) => r.explorer.room))];
  if (JSON.stringify([...stood].sort()) !== JSON.stringify([HOME, WEST, CORNER].sort())) problems.push(`the explorer stood in ${JSON.stringify(stood)}, expected ${HOME}, ${WEST} and ${CORNER}`);

  // 1. Segment 94: requested only after the config record loaded, written within the save cap, loaded after the halt.
  const configAt = linesAt((l) => l === CONFIG_LINE);
  if (configAt.length !== 2 || !(configAt[0] < haltAt && configAt[1] > haltAt)) {
    problems.push(`config record lines at ${JSON.stringify(configAt)}, expected one in each global (halt at ${haltAt}): ${JSON.stringify(CONFIG_LINE)}`);
  }
  const firstWrites = checkSegment(problems, world, state, 1, [0, haltAt], configAt[0] ?? Infinity);
  const secondWrites = checkSegment(problems, world, state, 2, [haltAt + 1, Infinity], configAt[1] ?? Infinity);
  if (!firstWrites.length) problems.push(`segment ${ATLAS_SEGMENT} was never written in the first global`);
  if (secondWrites.length) problems.push(`segment ${ATLAS_SEGMENT} written in the second global, with nothing changed, at ${JSON.stringify(secondWrites.map((w) => w.offset))}`);
  const lastWrite = firstWrites.at(-1);
  if (lastWrite && !(lastWrite.offset >= Math.max(readAt[WEST] ?? Infinity, readAt[CORNER] ?? Infinity, unreachableAt[0] ?? Infinity))) {
    problems.push(`the first global's last write at offset ${lastWrite.offset}, before the last change`);
  }
  checkRecord(problems, lastWrite, [
    readEntry(WEST, T0 + (readAt[WEST] ?? 0)), readEntry(CORNER, T0 + (readAt[CORNER] ?? 0)), unreachableEntry(NORTH, T0 + (unreachableAt[0] ?? 0)),
  ], 'the first global\'s last write');
  const loadedLines = world.logs.filter((l) => LOADED_LINE.test(l));
  const loadedAt = linesAt((l) => LOADED_LINE.test(l));
  const loaded = loadedLines.length === 1 ? LOADED_LINE.exec(loadedLines[0]) : null;
  const wantWritten = lastWrite ? T0 + lastWrite.offset : null;
  if (!loaded || loadedAt[0] <= haltAt || Number(loaded[1]) !== 3 || Number(loaded[2]) !== wantWritten) {
    problems.push(`atlas load lines ${JSON.stringify(loadedLines)} at ${JSON.stringify(loadedAt)}, expected one after the halt (${haltAt}) with 3 rooms, written at ${wantWritten}`);
  }

  // 6. The console summaries.
  const summaries = commandResults(world, SUMMARY);
  const [before, final] = summaries;
  const next = T0 + (unreachableAt[0] ?? 0) + REFRESH_TICKS;
  const atlas = `on (observers on) | atlas 3 rooms (2 read, 1 unreachable, 0 towered), loaded | homes [${HOME}] | 0 due, next at ${next} | no explorer`;
  const beforeWant = `rustedscreep scout at tick ${T0 + (marks.summary ?? 0)}: ${atlas} | since the global: reads 2, searches 2, `
    + 'unreachable 1, lost 0, retreats 0, explorers 1, looks 0 (refused 0)';
  const finalWant = `rustedscreep scout at tick ${T0 + (marks.final ?? 0)}: ${atlas} | since the global: reads 0, searches 0, `
    + 'unreachable 0, lost 0, retreats 0, explorers 0, looks 0 (refused 0)';
  if (summaries.length !== 2) problems.push(`${summaries.length} scout summaries, expected 2`);
  if (before !== beforeWant) problems.push(`the summary before the halt ${JSON.stringify(before)}, expected ${JSON.stringify(beforeWant)}`);
  if (final !== finalWant) problems.push(`the final summary ${JSON.stringify(final)}, expected ${JSON.stringify(finalWant)}`);
  if (state.searches.length !== 1) problems.push(`${state.searches.length} PathFinder searches, expected 1 (the trip to ${CORNER})`);
  if (state.spawnCalls.some((c) => c.offset > haltAt)) problems.push('spawnCreep called in the second global');
  if (world.logs.slice(state.logMarks[haltAt] ?? 0).some((l) => READ_LINE.test(l))) problems.push('a read line in the second global');

  if (problems.length) throw new Error(`scout-sim (run A, explorer):\n  ${problems.slice(0, 25).join('\n  ')}`);
  return {
    ticks: world.ticksRun,
    orderAt: order.offset,
    outAt,
    unreachableAt: unreachableAt[0],
    reads: `${WEST} at offset ${readAt[WEST]}, ${CORNER} at ${readAt[CORNER]}`,
    end: `${end.from.room} at offset ${end.offset}`,
    searches: state.searches.length,
    writes: state.writes.map((w) => w.offset).join(' '),
    loaded: loadedLines[0],
    before,
    final,
  };
}

/** Run B: an observer's looks. */
export function observerRun(dir) {
  const state = makeWorld({ level: 8, energy: 0, observer: true });
  const marks = {};
  const world = runWorld(state, dir, B_MAX, (offset, lines, w) => {
    const reads = w.logs.filter((l) => READ_LINE.test(l)).length;
    if (marks.lookSummary === undefined && state.looks.some((l) => l.offset === offset)) {
      marks.lookSummary = offset;
      return SUMMARY;
    }
    if (marks.allRead === undefined && reads >= CANDIDATES.length) marks.allRead = offset;
    if (marks.allRead !== undefined && offset >= marks.allRead + B_SETTLE) {
      marks.final = offset;
      return { finish: true, cmd: SUMMARY };
    }
    if (offset >= B_MAX - 1) return 'finish';
    return null;
  });

  const problems = [...state.errors.slice(0, 10)];
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (test) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some(test));
  if (marks.final === undefined) problems.push(`not finished in ${world.ticksRun} ticks (marks ${JSON.stringify(marks)})`);
  if (world.halts !== 0 || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 0 and 1`);
  const loops = state.reads.filter((r) => r && r.snapshots > 0);

  // 11. The log and the calls; no spawn, no search.
  const allowed = (l) => READ_LINE.test(l);
  checkLog(problems, world, state, allowed);
  if (state.spawnCalls.length) problems.push(`spawnCreep calls ${JSON.stringify(state.spawnCalls.map((c) => [c.offset, c.name, c.result]))}, expected none`);
  if (state.searches.length) problems.push(`${state.searches.length} PathFinder searches, expected none`);

  // 9. The looks: W2N2, W2N1, W1N2, one a loop, each at a room not visible, each read on the next loop.
  const order = [CORNER, WEST, NORTH];
  const looks = state.looks;
  if (JSON.stringify(looks.map((l) => l.room)) !== JSON.stringify(order)) problems.push(`observeRoom calls at ${JSON.stringify(looks.map((l) => [l.offset, l.room]))}, expected ${order.join(', ')} in that order`);
  const perLoop = new Map();
  for (const l of looks) perLoop.set(l.offset, (perLoop.get(l.offset) ?? 0) + 1);
  for (const [o, n] of perLoop) if (n > 1) problems.push(`offset ${o}: ${n} observeRoom calls`);
  // One a loop while a room is due: every loop from the first look to the last has one.
  for (let o = looks[0]?.offset ?? 0; o <= (looks.at(-1)?.offset ?? -1); o++) {
    if (!perLoop.get(o)) problems.push(`offset ${o}: no observeRoom call, with a room due`);
  }
  for (const l of looks) {
    if (l.argCount !== 1 || l.type !== 'string' || l.result !== C.OK || l.visible) problems.push(`offset ${l.offset}: observeRoom with ${l.argCount} argument(s) of type ${l.type} at ${l.room} (visible: ${l.visible}): ${l.result}`);
  }
  const readAt = {};
  for (const room of order) {
    const look = looks.find((l) => l.room === room);
    const want = `rustedscreep scout: ${room} read: ${describe(room)}`;
    const at = linesAt((l) => l === want);
    const got = world.logs.filter((l) => READ_LINE.exec(l)?.[1] === room);
    const n = got.length;
    const seen = loops.filter((r) => r.visible.includes(room)).map((r) => r.offset);
    readAt[room] = at[0];
    if (!look || n !== 1 || at.length !== 1 || at[0] !== look.offset + 1 || JSON.stringify(seen) !== JSON.stringify([look.offset + 1])) {
      problems.push(`read lines of ${room} ${JSON.stringify(got)} at ${JSON.stringify(at)}, the room visible at ${JSON.stringify(seen)}, expected one ${JSON.stringify(want)} on the loop after its look (${look?.offset}), the only loop it is visible`);
    }
  }
  const lastRead = Math.max(...order.map((room) => readAt[room] ?? Infinity));
  if (looks.some((l) => l.offset >= lastRead)) problems.push(`observeRoom calls on or after the last read (${lastRead}): ${JSON.stringify(looks.filter((l) => l.offset >= lastRead))}`);
  if (!(marks.final >= lastRead + B_SETTLE)) problems.push(`the run ended at ${marks.final}, less than ${B_SETTLE} loops after the last read (${lastRead})`);
  checkReads(problems, loops, readAt);

  // 8. Segment 94: requested after the config record loaded, written within the save cap, the last write the atlas.
  const configAt = linesAt((l) => l === CONFIG_LINE);
  if (configAt.length !== 1) problems.push(`config record lines at ${JSON.stringify(configAt)}, expected one: ${JSON.stringify(CONFIG_LINE)}`);
  const writes = checkSegment(problems, world, state, 1, [0, Infinity], configAt[0] ?? Infinity);
  const firstCall = (world.activeCalls ?? []).find(([, ids]) => ids.includes(ATLAS_SEGMENT));
  if (looks[0] && firstCall && !(looks[0].offset > firstCall[0])) problems.push(`the first look at offset ${looks[0].offset}, before the atlas's segment was active (requested at ${firstCall[0]})`);
  checkRecord(problems, writes.at(-1), order.map((room) => readEntry(room, T0 + (readAt[room] ?? 0))), 'the last write');
  if (world.logs.some((l) => LOADED_LINE.test(l))) problems.push('an atlas load line, with an empty segment');

  // 10. The console summaries.
  const summaries = commandResults(world, SUMMARY);
  const [during, final] = summaries;
  const duringWant = `rustedscreep scout at tick ${T0 + (marks.lookSummary ?? 0)}: on (observers on) | atlas 0 rooms (0 read, 0 unreachable, `
    + `0 towered), loaded | homes [${HOME}] | ${CANDIDATES.length} due | no explorer | looks [${CORNER}] | since the global: reads 0, `
    + 'searches 0, unreachable 0, lost 0, retreats 0, explorers 0, looks 1 (refused 0)';
  const next = T0 + (readAt[CORNER] ?? 0) + REFRESH_TICKS;
  const finalWant = `rustedscreep scout at tick ${T0 + (marks.final ?? 0)}: on (observers on) | atlas 3 rooms (3 read, 0 unreachable, `
    + `0 towered), loaded | homes [${HOME}] | 0 due, next at ${next} | no explorer | since the global: reads 3, searches 0, `
    + 'unreachable 0, lost 0, retreats 0, explorers 0, looks 3 (refused 0)';
  if (summaries.length !== 2) problems.push(`${summaries.length} scout summaries, expected 2`);
  if (during !== duringWant) problems.push(`the summary on the first look's loop ${JSON.stringify(during)}, expected ${JSON.stringify(duringWant)}`);
  if (final !== finalWant) problems.push(`the final summary ${JSON.stringify(final)}, expected ${JSON.stringify(finalWant)}`);

  if (problems.length) throw new Error(`scout-sim (run B, observer):\n  ${problems.slice(0, 25).join('\n  ')}`);
  return {
    ticks: world.ticksRun,
    looks: looks.map((l) => `${l.room} at ${l.offset}`).join(', '),
    reads: order.map((room) => `${room} at ${readAt[room]}`).join(', '),
    writes: writes.map((w) => w.offset).join(' '),
    energy: state.energy,
    during,
    final,
  };
}

/** `v` me/t as claim.rs `energy` prints it. */
const energyText = (v) => `${v < 0 ? '-' : ''}${Math.floor(Math.abs(v) / 1000)}.${String(Math.abs(v) % 1000).padStart(3, '0')}`;
/** Parses claim.rs `energy`'s text back to me/t. */
const milli = (text) => Math.round(Number(text) * 1000);

/**
 * Run C (claim scoring, P8.2, ADR-041): run B's world with claim scoring on. The observer reads W2N2, W2N1 and W1N2;
 * each read changes the atlas, so the ranking is built again on that loop. W2N2 (reserved by its invader core, V375)
 * and W1N2 (no source) are left out, and W2N1 is the one candidate, planned with its terrain from one
 * `Game.map.getRoomTerrain` call (no creep stands there, so movement holds none). It checks:
 * 12. exactly one `Game.map.getRoomTerrain` call, for W2N1, on the loop of W2N1's read line;
 * 13. exactly one claims line, `rustedscreep claims: best W2N1 from W1N1, 1 room, 2 sources: V e/t (sources S, remotes
 *     0.000, distance D, foreign 0.000, walks W, ramparts R), first spawn (X, Y), N ramparts`, after that call: S is two
 *     sources at SOURCE_ENERGY_CAPACITY / ENERGY_REGEN_TIME, D one room at `claim.distance_cost`, W at most 0, R the
 *     repair of N ramparts' decay a tick (`RAMPART_DECAY_AMOUNT` every `RAMPART_DECAY_TIME` - 1 ticks, V304, at
 *     `REPAIR_COST`, over `CREEP_LIFE_TIME`), V their sum, and (X, Y) a tile of W2N1 that is neither a wall nor an edge;
 * 14. `rustedscreep.claims()` C_SETTLE loops later is exactly `rustedscreep claims: 1 candidates (1 planned); left out:
 *     reserved 1, sources 1; best W2N1 at V e/t; scores 3, plans 1 (1 fit), steps K, terrain reads 1 (0 failed); GCL 1
 *     for 1 owned; CPU M a tick, P with another room (at most 16.00); no claimer; claimers 0, claims 0, failed 0, spawn
 *     sites 0 (0 refused), route searches 0, GCL reads 1 | 1. <the best line's text after "best ">`, K above 0 (a score
 *     on each of the three reads' loops, none before), P twice M to the rounding (P8.4's CPU check, `checkCpu`), and
 *     one `Game.gcl.level` read;
 * 15. check 11's rules, with the claims line the only other line allowed and the terrain call above.
 */
export function claimsRun(dir) {
  const state = makeWorld({ level: 8, energy: 0, observer: true });
  state.configLines = C_CONFIG_LINES;
  const marks = {};
  const world = runWorld(state, dir, C_MAX, (offset, lines, w) => {
    if (marks.best === undefined && w.logs.some((l) => BEST_LINE.test(l))) marks.best = offset;
    if (marks.best !== undefined && offset >= marks.best + C_SETTLE) {
      marks.final = offset;
      return { finish: true, cmd: CLAIMS };
    }
    if (offset >= C_MAX - 1) return 'finish';
    return null;
  });

  const problems = [...state.errors.slice(0, 10)];
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (test) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some(test));
  if (marks.final === undefined) problems.push(`not finished in ${world.ticksRun} ticks (marks ${JSON.stringify(marks)})`);
  if (world.halts !== 0 || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 0 and 1`);

  // 15. The log and the calls.
  // The console's reply is in the log too; it is check 14's.
  const claimLines = world.logs.filter((l) => l.startsWith('rustedscreep claims: ') && !/^rustedscreep claims: \d+ candidates /.test(l));
  checkLog(problems, world, state, (l) => READ_LINE.test(l), true);
  if (state.spawnCalls.length) problems.push(`spawnCreep calls ${JSON.stringify(state.spawnCalls.map((c) => [c.offset, c.name, c.result]))}, expected none`);

  // 12. One terrain read, W2N1's, on the loop of its read.
  const westRead = linesAt((l) => READ_LINE.exec(l)?.[1] === WEST);
  if (westRead.length !== 1) problems.push(`${WEST}'s read lines at ${JSON.stringify(westRead)}, expected one`);
  if (JSON.stringify(state.terrainCalls) !== JSON.stringify([[westRead[0], WEST]])) {
    problems.push(`Game.map.getRoomTerrain calls ${JSON.stringify(state.terrainCalls)}, expected one for ${WEST} at offset ${westRead[0]}`);
  }

  // 13. The best line and its terms.
  const best = claimLines.filter((l) => BEST_LINE.test(l));
  if (best.length !== 1 || claimLines.length !== 1) problems.push(`claims lines ${JSON.stringify(claimLines)}, expected one best line for ${WEST}`);
  const bestAt = linesAt((l) => BEST_LINE.test(l));
  if (!(bestAt[0] >= (state.terrainCalls[0]?.[0] ?? Infinity))) problems.push(`the best line at ${bestAt[0]}, before the terrain call`);
  const m = BEST_LINE.exec(best[0] ?? '');
  if (m) {
    const [, value, sources, remotes, distance, foreign, walks, ramparts, x, y, n] = m;
    const sourceYield = Math.floor(C.SOURCE_ENERGY_CAPACITY * 1000 / C.ENERGY_REGEN_TIME);
    const decay = Math.floor(C.CREEP_LIFE_TIME * C.RAMPART_DECAY_AMOUNT / (C.RAMPART_DECAY_TIME - 1));
    const repair = Math.ceil(decay * Number(n) / Math.round(1 / C.REPAIR_COST));
    const rampartWant = -Math.floor(repair * 1000 / C.CREEP_LIFE_TIME);
    const want = { sources: 2 * sourceYield, remotes: 0, distance: -DISTANCE_COST, foreign: 0 };
    const got = { sources: milli(sources), remotes: milli(remotes), distance: milli(distance), foreign: milli(foreign) };
    if (JSON.stringify(got) !== JSON.stringify(want)) problems.push(`the best line's terms ${JSON.stringify(got)}, expected ${JSON.stringify(want)}`);
    if (milli(ramparts) !== rampartWant) problems.push(`ramparts ${ramparts} for ${n} ramparts, expected ${energyText(rampartWant)}`);
    if (milli(walks) > 0) problems.push(`walks ${walks} above 0`);
    const sum = got.sources + got.remotes + got.distance + got.foreign + milli(walks) + milli(ramparts);
    if (milli(value) !== sum) problems.push(`value ${value}, expected the terms' sum ${energyText(sum)}`);
    const [sx, sy] = [Number(x), Number(y)];
    if (isWall(WEST, sx, sy) || sx <= 0 || sy <= 0 || sx >= 49 || sy >= 49) problems.push(`the first spawn at (${sx}, ${sy}): a wall or an edge`);
  } else {
    problems.push(`no best line matches ${BEST_LINE}`);
  }

  // 14. The console summary.
  const [summary] = commandResults(world, CLAIMS);
  const value = m?.[1];
  const shown = (best[0] ?? '').replace('rustedscreep claims: best ', '');
  const head = `rustedscreep claims: 1 candidates (1 planned); left out: reserved 1, sources 1; best ${WEST} at ${value} e/t; `
    + 'scores 3, plans 1 (1 fit), steps ';
  // The claimer's part (P8.3): W1N1 is at RCL8, so the GCL is read once, and level 1 allows no second room. P8.4's
  // CPU fragment (CPU_PART) stands between the GCL and the claimer, with one more room twice the mean for 1 owned.
  const tail = `, terrain reads 1 (0 failed); GCL 1 for 1 owned; CPU M; no claimer; claimers 0, claims 0, failed 0, `
    + `spawn sites 0 (0 refused), route searches 0, GCL reads 1 | 1. ${shown}`;
  checkCpu(problems, summary, 2);
  const plain = summary?.replace(CPU_PART, '; CPU M');
  const steps = plain?.startsWith(head) && plain.endsWith(tail) ? Number(plain.slice(head.length, plain.length - tail.length)) : NaN;
  if (!(steps > 0)) problems.push(`the claims summary ${JSON.stringify(summary)}, expected ${JSON.stringify(`${head}K${tail}`)} with K above 0 and M as ${CPU_PART}`);
  if (state.gclReads.length !== 1) problems.push(`Game.gcl.level read at ${JSON.stringify(state.gclReads)}, expected once`);

  if (problems.length) throw new Error(`scout-sim (run C, claims):\n  ${problems.slice(0, 25).join('\n  ')}`);
  return { ticks: world.ticksRun, terrainAt: state.terrainCalls[0]?.[0], bestAt: bestAt[0], steps, summary };
}

/**
 * Run D (the claimer and the first spawn site, P8.3, ADR-042): run C's world with Game.gcl.level D_GCL and W1N1's
 * capacity at D_CAPACITY. The spawn starts empty and regains 1 a tick until the best line (so no explorer is paid for
 * before the observer has read every room, as in run B); from then on W1N1 is full after every loop. It checks:
 * 16. one `Game.gcl.level` read, on the loop of the claimer's order, which is the run's first spawnCreep call: two
 *     arguments, a name matching CLAIMER_NAME, the body CLAIMER_BODY (CLAIMER_COST energy), OK; the line SENT_LINE on
 *     a loop its creep exists; no other claimer order in the run;
 * 17. one `claimController` call, by the claimer, with one argument (W2N1's controller), from W2N1 within one tile of
 *     it, OK; on the next loop the line CLAIMED_LINE and the claimer's `suicide` (no argument, OK), and the claimer is
 *     gone on the loop after; it never stood in W1N2; NO_BEST_LINE once, after the claim;
 * 18. one `createConstructionSite` call in W2N1, on or after the claimed line's loop: (X, Y, 'spawn') with three
 *     arguments, OK, (X, Y) the best line's first spawn; SITE_LINE with (X, Y) on that loop; the site stands from the
 *     next loop and no other site call in W2N1;
 * 19. after the claim, a spawnCreep call for a pioneer (a name matching PIONEER_NAME, two arguments, OK) from W1N1's
 *     spawn, its body WORK, CARRY and MOVE parts costing at most D_CAPACITY (the processor drops it: nothing builds);
 * 20. `rustedscreep.claims()` D_SETTLE loops after it ends with `; GCL 2 for 2 owned; CPU M a tick, P with another room
 *     (at most 16.00); no claimer; claimers 1, claims 1, failed 0, spawn sites 1 (0 refused), route searches 1, GCL
 *     reads 1`, P three times M to the rounding (W2N1 at level 1 counts at W1N1's cost);
 * 21. check 11's rules, with the claimer's move, claimController and suicide calls and the one W2N1 site allowed, the
 *     claimer's and pioneers' orders the only spawnCreep calls, and the claim lines above the only other lines.
 */
export function claimerRun(dir) {
  const state = makeWorld({ level: 8, energy: 0, observer: true, gcl: D_GCL, capacity: D_CAPACITY });
  state.configLines = C_CONFIG_LINES;
  const marks = {};
  const world = runWorld(state, dir, D_MAX, (offset, lines, w) => {
    if (marks.best === undefined && w.logs.some((l) => BEST_LINE.test(l))) {
      marks.best = offset;
      state.refill = true;
      state.energy = state.capacity;
    }
    if (marks.pioneer === undefined && state.spawnCalls.some((c) => PIONEER_NAME.test(c.name ?? ''))) marks.pioneer = offset;
    if (marks.pioneer !== undefined && offset >= marks.pioneer + D_SETTLE) {
      marks.final = offset;
      return { finish: true, cmd: CLAIMS };
    }
    if (offset >= D_MAX - 1) return 'finish';
    return null;
  });

  const problems = [...state.errors.slice(0, 10)];
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (test) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some(test));
  if (marks.final === undefined) problems.push(`not finished in ${world.ticksRun} ticks (marks ${JSON.stringify(marks)})`);
  if (world.halts !== 0 || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 0 and 1`);
  const best = BEST_LINE.exec(world.logs.find((l) => BEST_LINE.test(l)) ?? '');
  const spawnAt = best ? { x: Number(best[8]), y: Number(best[9]) } : null;

  // 21. The log and the calls.
  const claimer = (c) => CLAIMER_NAME.test(c.actor) && ['move', 'claimController', 'suicide'].includes(c.method) && c.result === C.OK;
  const westSite = (c) => c.room === WEST && c.type === 'spawn';
  checkLog(problems, world, state, (l) => READ_LINE.test(l), true, { creep: claimer, site: westSite });
  const allowedClaims = (l) => BEST_LINE.test(l) || SITE_LINE.test(l) || [SENT_LINE, CLAIMED_LINE, NO_BEST_LINE].includes(l)
    || /^rustedscreep claims: \d+ candidates /.test(l);
  const otherClaims = world.logs.filter((l) => l.startsWith('rustedscreep claims: ') && !allowedClaims(l));
  if (otherClaims.length) problems.push(`other claims lines ${JSON.stringify(otherClaims.slice(0, 4))}`);
  const orders = state.spawnCalls.filter((c) => !PIONEER_NAME.test(c.name ?? ''));

  // 16. The GCL read and the claimer's order.
  const [order] = orders;
  if (orders.length !== 1 || !order || order.args !== 2 || !CLAIMER_NAME.test(order.name) || JSON.stringify(order.body) !== JSON.stringify(CLAIMER_BODY)
    || order.result !== C.OK) {
    problems.push(`spawnCreep calls but the pioneers' ${JSON.stringify(orders.map((c) => [c.offset, c.name, c.body, c.result]))}, expected one `
      + `for the claimer (${JSON.stringify(CLAIMER_BODY)}, ${CLAIMER_COST} energy) and OK`);
  }
  if (order && state.spawnCalls[0] !== order) problems.push('the claimer\'s order is not the first spawnCreep call');
  if (JSON.stringify(state.gclReads) !== JSON.stringify(order ? [order.offset] : [])) {
    problems.push(`Game.gcl.level read at ${JSON.stringify(state.gclReads)}, expected once, at the claimer's order (${order?.offset})`);
  }
  const sentAt = linesAt((l) => l === SENT_LINE);
  if (sentAt.length !== 1 || !(sentAt[0] > (order?.offset ?? Infinity))) problems.push(`the sent line at ${JSON.stringify(sentAt)}, expected once after the order at ${order?.offset}`);

  // 17. The claim and the claimer's end.
  const claims = state.calls.filter((c) => c.method === 'claimController');
  const [claim] = claims;
  const ctrl = CONTROLLER[WEST];
  // The call record names an object argument by its structure type.
  if (claims.length !== 1 || claim.argCount !== 1 || claim.args[0] !== 'controller' || claim.from.room !== WEST
    || Math.max(Math.abs(claim.from.x - ctrl.x), Math.abs(claim.from.y - ctrl.y)) > 1 || claim.result !== C.OK) {
    problems.push(`claimController calls ${JSON.stringify(claims.map((c) => [c.offset, c.args, c.from, c.result]))}, expected one at ${WEST}'s `
      + `controller from within one tile, OK`);
  }
  const claimedAt = linesAt((l) => l === CLAIMED_LINE);
  if (!claim || JSON.stringify(claimedAt) !== JSON.stringify([claim.offset + 1])) problems.push(`the claimed line at ${JSON.stringify(claimedAt)}, expected once at ${claim ? claim.offset + 1 : '?'}`);
  const suicides = state.calls.filter((c) => CLAIMER_NAME.test(c.actor) && c.method === 'suicide');
  if (suicides.length !== 1 || suicides[0].offset !== claimedAt[0] || suicides[0].argCount !== 0) {
    problems.push(`the claimer's suicide calls ${JSON.stringify(suicides.map((c) => [c.offset, c.argCount]))}, expected one with no argument at ${claimedAt[0]}`);
  }
  const lastCall = Math.max(...state.calls.filter((c) => CLAIMER_NAME.test(c.actor)).map((c) => c.offset));
  if (suicides[0] && lastCall !== suicides[0].offset) problems.push(`the claimer called at ${lastCall}, after its suicide`);
  if (state.creeps.some((c) => CLAIMER_NAME.test(c.name))) problems.push('the claimer still lives at the end');
  const noBest = linesAt((l) => l === NO_BEST_LINE);
  if (noBest.length !== 1 || !(noBest[0] > (claim?.offset ?? Infinity))) problems.push(`the no-best line at ${JSON.stringify(noBest)}, expected once after the claim`);

  // 18. The first spawn site.
  const sites = state.siteCalls.filter((c) => c.room === WEST);
  const [site] = sites;
  if (sites.length !== 1 || site.type !== 'spawn' || site.args !== 3 || !site.sameRoom || site.result !== C.OK || !spawnAt
    || site.x !== spawnAt.x || site.y !== spawnAt.y || !(site.offset >= (claimedAt[0] ?? Infinity))) {
    problems.push(`${WEST} site calls ${JSON.stringify(sites.map((c) => [c.offset, c.x, c.y, c.type, c.args, c.result]))}, expected one spawn `
      + `site at the best line's first spawn ${spawnAt ? at(spawnAt) : '?'} on or after ${claimedAt[0]}`);
  }
  const siteLines = linesAt((l) => SITE_LINE.test(l));
  const siteText = world.logs.find((l) => SITE_LINE.test(l));
  if (!site || JSON.stringify(siteLines) !== JSON.stringify([site.offset]) || !spawnAt || siteText !== `rustedscreep claims: ${WEST}'s first spawn site placed at ${at(spawnAt)}`) {
    problems.push(`the site line ${JSON.stringify(siteText)} at ${JSON.stringify(siteLines)}, expected once at ${site?.offset}`);
  }
  if (!state.sites.some((x) => x.room === WEST && x.type === 'spawn')) problems.push(`no spawn site stands in ${WEST}`);

  // 19. A pioneer's order.
  const pioneers = state.spawnCalls.filter((c) => PIONEER_NAME.test(c.name ?? ''));
  const [pioneer] = pioneers;
  const cost = pioneer && Array.isArray(pioneer.body) ? pioneer.body.reduce((sum, p) => sum + C.BODYPART_COST[p], 0) : Infinity;
  if (!pioneer || pioneer.args !== 2 || pioneer.result !== C.OK || !(pioneer.offset > (claim?.offset ?? Infinity)) || cost > D_CAPACITY
    || !['work', 'carry', 'move'].every((p) => pioneer.body.includes(p))) {
    problems.push(`the first pioneer order ${JSON.stringify(pioneer && [pioneer.offset, pioneer.name, pioneer.body, pioneer.result])}, expected `
      + `one after the claim with WORK, CARRY and MOVE parts at most ${D_CAPACITY} energy, OK`);
  }

  // 20. The console summary, with P8.4's CPU fragment (CPU_PART) for 2 owned rooms, W2N1 at level 1 below
  // `claim.min_home_level` (4), so the load is W1N1's alone and one more room makes three times it.
  const [summary] = commandResults(world, CLAIMS);
  const tail = `; GCL ${D_GCL} for 2 owned; CPU M; no claimer; claimers 1, claims 1, failed 0, spawn sites 1 (0 refused), `
    + 'route searches 1, GCL reads 1';
  checkCpu(problems, summary, 3);
  if (!summary?.replace(CPU_PART, '; CPU M').endsWith(tail)) {
    problems.push(`the claims summary ${JSON.stringify(summary)}, expected it to end ${JSON.stringify(tail)} with M as ${CPU_PART}`);
  }

  if (problems.length) throw new Error(`scout-sim (run D, claimer):\n  ${problems.slice(0, 25).join('\n  ')}`);
  return {
    ticks: world.ticksRun, bestAt: marks.best, orderAt: order?.offset, claimAt: claim?.offset, siteAt: site?.offset,
    site: spawnAt ? at(spawnAt) : '?', pioneerAt: pioneer?.offset, pioneerCost: cost, summary,
  };
}

if (isMain(import.meta.url)) {
  try {
    const dir = process.argv[2] ?? join(ROOT, 'dist');
    const a = explorerRun(dir);
    const b = observerRun(dir);
    const c = claimsRun(dir);
    const d = claimerRun(dir);
    console.log(`scout-sim ok: run A ${a.ticks} ticks, 2 globals; explorer ordered at offset ${a.orderAt}, out at ${a.outAt}; `
      + `${NORTH} unreachable at ${a.unreachableAt}; read ${a.reads}; ended in ${a.end}; ${a.searches} PathFinder search(es); `
      + `segment ${ATLAS_SEGMENT} written at offsets ${a.writes}; after the halt: ${a.loaded} | ${a.final}`);
    console.log(`scout-sim ok: run B ${b.ticks} ticks; looks ${b.looks}; read ${b.reads}; segment ${ATLAS_SEGMENT} written at offsets `
      + `${b.writes}; spawn energy ${b.energy} at the end, no spawn | ${b.during} | ${b.final}`);
    console.log(`scout-sim ok: run C ${c.ticks} ticks; ${WEST}'s terrain read at offset ${c.terrainAt}, best line at ${c.bestAt} `
      + `after ${c.steps} plan steps | ${c.summary}`);
    console.log(`scout-sim ok: run D ${d.ticks} ticks; best at ${d.bestAt}, claimer ordered at ${d.orderAt}, claim at ${d.claimAt}, `
      + `spawn site at ${d.site} on ${d.siteAt}, first pioneer order at ${d.pioneerAt} (${d.pioneerCost} energy) | ${d.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
