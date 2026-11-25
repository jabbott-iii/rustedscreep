// The controller signer's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in
// dist/ (crates/rustedscreep-wasm/src/sign.rs, snapshot.rs `push_sign`, act.rs `sign_controller` and persist.rs's
// segment 95, over rustedscreep-core `signing`; ADR-030: one cheap creep signs every controller it can reach, and its
// record survives a global reset). Four rooms, each pair of neighbors sharing an exit of the tiles 20 to 30 of their
// common edge (corners and every other edge tile are walls), every controller on a terrain wall (V228):
// - W1N1, owned at RCL3 (the signer waits for `sign.min_level`, 3): the controller at (10, 10), one spawn at (25, 25),
//   energyAvailable and energyCapacityAvailable SPAWN_ENERGY_CAPACITY (300), no source, so mining and hauling stay
//   inert. As in remote-sim.mjs's home, an idle own creep whose name the bot did not give (KEEPER) stands in it from
//   the start: movement reads a room's terrain only while an own creep stands in it, and without that grid the
//   opening has no plan, so it keeps `opening.min_workers` and its zero-creep recovery orders a worker every tick,
//   which outranks the signer; with the grid, a room with no source wants no worker;
// - W2N1 (west): a neutral controller (level 0, no owner, `my` undefined, V101) at (25, 25) with no sign;
// - W2N2 (north-west): a neutral controller at (25, 25) showing the server's sign (V275: username `Screeps`), which the
//   bot must count as unsignable and leave alone;
// - W1N2 (north): `Game.map.getRoomStatus` gives `closed` (V168; every other room `normal`), so no creep may enter it.
// With `sign.max_distance` 1 these four are the candidates: the other rooms within one of W1N1 are highways (W0x, xN0).
// The tour this gives: W1N1 first (the nearest home's own room), then the due rooms one room away in packed-name order
// (signing/mod.rs `nearest_due`): W2N2 (a look through W2N1, which finds the server's sign), W2N1 (signed), and W1N2, whose
// refused route ends the signer; the next plan's search from the home spawn then marks W1N2 unreachable.
// The run starts with the overrides `sign.max_distance=1`, `sign.save_ticks=5` and `scout.enabled=false` (scouting,
// ADR-039, is scout-sim.mjs's) in segment 97 (the runtime config's
// record), which the bot loads on its second loop.
// The harness plays the engine's part as far as the bot can see it:
// - `spawnCreep(body, name)` makes the runtime's checks in V171's order and its `Memory.creeps[name]` write (V181); the
//   processor (V173) turns an order for a signer's name (`g…`, `Role::Signer`) into a creep that is spawning for
//   CREEP_SPAWN_TIME × parts loops and then stands on the first free tile around the spawn in direction order (V174),
//   with ticksToLive CREEP_LIFE_TIME on its first loop out (V198 infers undefined there; the bot reads it as an Option
//   either way), and drops any other order; the spawn's energy pays the body and regains 1 a tick (V174);
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52, as in remote-sim.mjs), with
//   fatigue (V54, V55); a creep that ends a tick on an edge tile goes to the next room (V151), except into a closed
//   room, where it stays on its edge (V168); `suicide` removes the creep at the end of the tick and leaves a tombstone;
// - `signController(target, text)` makes the game side's checks (V275: ERR_BUSY while spawning, ERR_INVALID_TARGET for a
//   target not of this tick, ERR_NOT_IN_RANGE unless within 1 on both axes in the same room, ERR_INVALID_TARGET for
//   one that is not a controller), and the processor (V274) stores `{user, text, time, datetime}` on that controller;
//   `controller.sign` (V275) is the server's sign where one is set, else the player's as `{username, text, time,
//   datetime}`, else undefined, a new object on every read, so a sign shows from the next tick;
// - a room is visible (in Game.rooms) while an own creep stands in it, spawning or not, the owned one always (V101);
// - `PathFinder.search` is route-sim.mjs's model of the native search (V163 to V166, copied from remote-sim.mjs);
//   every search, `Game.map.getRoomStatus` call and `Game.map.getRoomTerrain` call is recorded, and so are each
//   `sign` read (room and loop) and each `find` call by room, type and filter;
// - `room.createConstructionSite` makes the game side's checks (V200, V237, V238, as in remote-sim.mjs) for the
//   opening's and the planner's sites in W1N1; an accepted site appears on the next tick and nothing builds it.
// It runs until the signer has ended, lets SETTLE loops pass (the next plan's search from the home spawn and the
// record's save), asks `rustedscreep.sign()`, halts once (a global reset), runs AFTER more loops and asks it again.
// It checks:
// 1. segment 95 is in no `setActiveSegments` call of a global up to the loop on which that global's config record
//    (segment 97) loaded, and in one after it; the bot writes segment 95 (a frame starting `rs1b`) at least once in the
//    first global, never on two loops less than `sign.save_ticks` (5) apart, and not at all in the second global; the
//    second global logs exactly one line `rustedscreep sign: record loaded from segment 95 (4 rooms, written at T)`, T
//    being the Game.time of the first global's last write;
// 2. exactly one spawnCreep call in the run, for the signer: two arguments, a name matching /^gW1N1-/ and the body
//    ['move'] (BODYPART_COST.move, 50 energy), after one line `rustedscreep sign: next signer for W1N1: 0 TOUGH, 1 MOVE
//    (50 energy)`; so no second signer while one lives and no spawnCreep in the second global; the processor drops no
//    order, and the signer spawns for CREEP_SPAWN_TIME loops;
// 3. exactly one `signController` call on W1N1's controller and one on W2N1's, each with that loop's controller object,
//    the text `jabbott.dev rustedscreep bot - https://github.com/jabbott-iii/rustedscreep` and from a tile within 1 of
//    the controller, each OK and followed on the next loop by one line `rustedscreep sign: signed <room>`; none on
//    W2N2's controller (the server's sign) and none in W1N2;
// 4. on every loop the signer stands in a room (spawning included), exactly one `sign` read and one tower-filtered
//    FIND_HOSTILE_STRUCTURES find in that room, and none in any room it does not stand in (W1N1's controller is not
//    read while the signer is elsewhere); it stands in W2N2 at least once;
// 5. no PathFinder.search has its target in W1N2 (the glue refuses a target room of another status before PathFinder,
//    V168), and W1N2's status was read; no search stops short; none runs in the second global (nothing is due);
//    `Game.map.getRoomTerrain` is never called (no towered room, so no body sizing);
// 6. the signer ends: one `suicide` call with no argument, on the loop of the one line `rustedscreep sign: nothing left
//    in reach from <room>; the signer ends`, <room> being where it stands, after both signs; it calls nothing after it
//    and is gone on the next loop;
// 7. `rustedscreep.sign()` before the halt shows the record with `signed 2` and `server sign 1`, no signer and `since
//    the global: signed 2`; at the end it shows `record 4 rooms (signed 2, unsigned 0, server sign 1, unreachable 1,
//    deadly 0, no controller 0; 0 due), loaded | no signer | plan none` (W1N2 unreachable once the next plan's search
//    from the home spawn failed) and nothing done in the second global;
// 8. no log line starting `rustedscreep fault` or `rustedscreep incident`, no other `rustedscreep sign:` line, no
//    `rustedscreep persist:` warning, every creep call OK and one of move, signController and suicide, no move the
//    engine would refuse and no creep left on an exit to W1N2, every site call in W1N1 and OK, one halt, two globals.
// npm run check runs it (row 24); standalone: node js/tools/sign-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CREEP_METHODS, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { CONFIG_SEGMENT, ENGINE_CONSTANTS, SIGN_SEGMENT, commandResults, configRecord, driverRawMemory, simulate } from './smoke.mjs';

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
const LEVEL = 3;
const SPAWN = { x: 25, y: 25 };
/** The idle own creep in W1N1 (see the header), off the signer's ways; its ticksToLive outlasts the run. */
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
const CONTROLLER = { [HOME]: { x: 10, y: 10 }, [WEST]: { x: 25, y: 25 }, [NORTH]: { x: 25, y: 25 }, [CORNER]: { x: 25, y: 25 } };
/** Each room's open sides (toward the other rooms of the world). */
const OPEN = { [HOME]: ['W', 'N'], [WEST]: ['E', 'N'], [NORTH]: ['S', 'W'], [CORNER]: ['S', 'E'] };
/** Game.map.getRoomStatus's `status` (V168): every room not listed is 'normal'. */
const STATUS = { [NORTH]: 'closed' };
/** rustedscreep-core signing::SIGN_TEXT. */
const SIGN_TEXT = 'jabbott.dev rustedscreep bot - https://github.com/jabbott-iii/rustedscreep';
/** The server's sign in W2N2 (V275: the getter shows it under SYSTEM_USERNAME). */
const SYSTEM_USERNAME = 'Screeps';
const SERVER_SIGN = {
  text: 'A new Novice or Respawn Area is being planned somewhere in this sector. Please make sure all important rooms are reserved.',
  time: T0 - 5000,
  datetime: Date.UTC(2026, 10, 1),
};
const SAVE_TICKS = 5;
// Scouting (ADR-039) is off: its explorer, room reads and route searches are not the signer's, and scout-sim.mjs covers
// them.
const CONFIG_LINES = ['sign.max_distance=1', `sign.save_ticks=${SAVE_TICKS}`, 'scout.enabled=false'];
/** The runtime config's log line for the record above (crates/rustedscreep-wasm/src/config.rs `loaded`). */
const CONFIG_LINE = `rustedscreep config: ${CONFIG_LINES.length} override(s) from segment ${CONFIG_SEGMENT} (written at tick 0): ${CONFIG_LINES.join(' ')}`;
/** The signer's body (signing::plain_signer) and its cost. */
const SIGNER_BODY = ['move'];
const SIGNER_COST = SIGNER_BODY.reduce((s, p) => s + C.BODYPART_COST[p], 0);
/** The signer's log lines (crates/rustedscreep-wasm/src/sign.rs `log_events` and `loaded`). */
const PLANNED_LINE = `rustedscreep sign: next signer for ${HOME}: 0 TOUGH, ${SIGNER_BODY.length} MOVE (${SIGNER_COST} energy)`;
const signedLine = (room) => `rustedscreep sign: signed ${room}`;
const DONE_LINE = /^rustedscreep sign: nothing left in reach from ([WE]\d+[NS]\d+); the signer ends$/;
const LOADED_LINE = new RegExp(`^rustedscreep sign: record loaded from segment ${SIGN_SEGMENT} \\((\\d+) rooms, written at (\\d+)\\)$`);
/** The record's counts in `rustedscreep.sign()` (sign.rs `summary`, SignState::ALL's order). */
const COUNTS = /\| record (\d+) rooms \(signed (\d+), unsigned (\d+), server sign (\d+), unreachable (\d+), deadly (\d+), no controller (\d+); (\d+) due\), (loaded|loading) \|/;
const SUMMARY = 'rustedscreep.sign()';
const HALT = 'Game.cpu.halt()';
/** Loops after the signer's end before the first summary: the next plan's search (the loop after the end) and the
 * record's save (at most SAVE_TICKS after the one before), with room to spare. */
const SETTLE = 12;
/** Loops after the halt before the final summary. */
const AFTER = 300;
/** The signer's tour must end by this offset. */
const TOUR_MAX = 500;
const RUN = TOUR_MAX + SETTLE + AFTER + 20;

const count = (body, part) => body.filter((p) => p === part).length;
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
const at = (p) => `(${p.x}, ${p.y})`;

/** Room coordinates as the engine counts them: W<n> is -n - 1, E<n> is n; N<n> is -n - 1, S<n> is n (V103). */
function roomXY(name) {
  const m = /^([WE])(\d+)([NS])(\d+)$/.exec(name);
  if (!m) throw new Error(`not a room name: ${name}`);
  return [m[1] === 'W' ? -Number(m[2]) - 1 : Number(m[2]), m[3] === 'N' ? -Number(m[4]) - 1 : Number(m[4])];
}
const roomAt = (rx, ry) => `${rx < 0 ? `W${-rx - 1}` : `E${rx}`}${ry < 0 ? `N${-ry - 1}` : `S${ry}`}`;
const unpack = (p) => {
  const v = p >>> 0;
  return { x: (v >>> 8) & 0xff, y: v & 0xff, roomName: roomAt(((v >>> 24) & 0xff) - 128, ((v >>> 16) & 0xff) - 128) };
};
const pos = (room, x, y) => ({ x, y, roomName: room, __packedPos: packPos(room, x, y) });

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

/**
 * The engine's global `Creep` class, as far as the bindings use it (copied from remote-sim.mjs): screeps-game-api's
 * `get_active_bodyparts` is a `final` binding, `Creep.prototype.getActiveBodyparts.call(creep, type)`, whose method is
 * the engine's (V269). Only a hostile in a mined room is asked, and this world has none.
 */
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

/** The engine's RoomPosition, as far as the crate and a PathFinder call would use it (V103; copied from remote-sim.mjs). */
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
 * A model of the native PathFinder, copied from remote-sim.mjs's makePathFinder (itself route-sim.mjs's model; driver
 * lib/path-finder.js and native/src/pf.cc; V163 to V166): the driver's option defaults and clamps; a room is indexed
 * once per search, its terrain checked first (a room without terrain throws 'Could not load terrain data') and then
 * roomCallback asked (false blocks it, an object with a 2500-byte `_bits` is its matrix, anything else leaves the
 * terrain); a matrix byte other than 0 is the cost (255 an obstacle), 0 the terrain (walls obstacles); moves from an
 * edge tile go straight across or back in, never along it; A* by world Chebyshev range, one op per expansion; the path
 * leaves the origin out. Each search is recorded in `state.searches`.
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

function makeWorld() {
  return {
    /** Own creeps: { id, name, room, x, y, body, fatigue, spawning, death (the Game.time it is gone), rooms }. */
    creeps: [],
    /** A player's sign per room, as the processor stores it (V274): { user, text, time, datetime }. */
    signs: {},
    /** The spawn's accepted order this tick ({ name, body }), and its busy state ({ name, needTime, until }). */
    order: null,
    spawning: null,
    energy: C.SPAWN_ENERGY_CAPACITY,
    sites: [],
    siteOrders: [],
    tombstones: [],
    nextId: 100,
    intents: new Map(),
    calls: [],
    spawnCalls: [],
    dropped: [],
    siteCalls: [],
    searches: [],
    statusCalls: [],
    terrainCalls: [],
    /** The bot's writes of segment 95 ({ offset, value }), seen through RawMemory.segments. */
    writes: [],
    reads: [],
    logMarks: [],
    errors: [],
    context: null,
  };
}

/** `controller.sign` (V275): the server's sign wins, then a player's (its user's name), else undefined. */
function signOf(state, room) {
  if (room === CORNER) return { username: SYSTEM_USERNAME, text: SERVER_SIGN.text, time: SERVER_SIGN.time, datetime: new Date(SERVER_SIGN.datetime) };
  const s = state.signs[room];
  return s ? { username: USER, text: s.text, time: s.time, datetime: new Date(s.datetime) } : undefined;
}

/** Why the game side refuses a site of `type` on (x, y) in `room` (V200, V237, V238; remote-sim.mjs's checks), or OK. */
function refuses(state, room, x, y, type) {
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x > 49 || y > 49) return C.ERR_INVALID_ARGS;
  if (!C.CONSTRUCTION_COST[type]) return C.ERR_INVALID_ARGS;
  // Only W1N1's controller is the bot's; a neutral one has no level, so only level-0 counts are allowed there.
  const level = room === HOME ? LEVEL : 0;
  const here = (o) => o.room === room && o.x === x && o.y === y;
  const structures = room === HOME ? [{ ...SPAWN, type: 'spawn', room }] : [];
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
  // A room is visible while an own creep stands in it, spawning or not, and the owned one always (V101).
  const visible = new Set([HOME, ...state.creeps.map((c) => c.room)]);
  const signer = state.creeps.find((c) => c.name.startsWith('g'));
  const r = {
    offset, snapshots: 0, signReads: {}, finds: {},
    signer: signer ? { name: signer.name, room: signer.room, x: signer.x, y: signer.y, spawning: signer.spawning } : null,
  };
  state.reads[offset] = r;
  const registered = new Set();
  const intentsOf = (cid) => {
    if (!state.intents.has(cid)) state.intents.set(cid, {});
    return state.intents.get(cid);
  };

  // The visible rooms' controllers: W1N1's with an owned controller's getters (V189), the others neutral (`my`
  // undefined, V101); each `sign` read is counted.
  const controllers = {};
  for (const room of ROOMS.filter((n) => visible.has(n))) {
    const c = CONTROLLER[room];
    const owned = room === HOME;
    controllers[room] = {
      id: id(1 + ROOMS.indexOf(room)), structureType: 'controller', pos: pos(room, c.x, c.y), level: owned ? LEVEL : 0,
      ...(owned ? {
        my: true, owner: { username: USER }, progress: 0, progressTotal: C.CONTROLLER_LEVELS[LEVEL], ticksToDowngrade: C.CONTROLLER_DOWNGRADE[LEVEL],
      } : {}),
      reservation: undefined,
      get sign() {
        r.signReads[room] = (r.signReads[room] ?? 0) + 1;
        return signOf(state, room);
      },
    };
    registered.add(controllers[room]);
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
    store: makeStore({ energy: state.energy }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    spawning: state.spawning ? { name: state.spawning.name, needTime: state.spawning.needTime, remainingTime: state.spawning.until - offset + 1 } : null,
    // The runtime's checks in V171's order and its `Memory.creeps[name]` write on OK (V181); a later call replaces
    // the tick's order (V181's intents.set).
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
  registered.add(spawn);
  // The act layer's other bindings on structures (a destroy, ADR-026): nothing here calls for them.
  spawn.destroy = (...args) => record({ name: spawn.name, room: HOME, ...SPAWN }, 'destroy', args, C.OK).result;
  if (controllers[HOME]) controllers[HOME].destroy = (...args) => record({ name: 'controller', room: HOME, ...CONTROLLER[HOME] }, 'destroy', args, C.OK).result;

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
    self.signController = (...args) => {
      // V275, in its order.
      const [target, text] = args;
      const result = (() => {
        if (c.spawning) return C.ERR_BUSY;
        if (!target?.id || !registered.has(target)) return C.ERR_INVALID_TARGET;
        if (target.pos.roomName !== c.room || range(c, target.pos) > 1) return C.ERR_NOT_IN_RANGE;
        if (target.structureType !== 'controller') return C.ERR_INVALID_TARGET;
        return C.OK;
      })();
      const room = target?.pos?.roomName;
      if (result === C.OK) intentsOf(c.id).sign = { room, text: `${text}` };
      return record(c, 'signController', args, result, { target: room, text, sameObject: !!room && target === controllers[room] }).result;
    };
    registered.add(self);
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
    for (const s of sites) registered.add(s);
    // The engine's find cache leaves spawning creeps out of its creep lists (V101 for hostiles; assumed here for own).
    const mine = creeps.filter((c) => inRoom(c) && !c.spawning);
    const structures = name === HOME ? [spawn, controllers[name]] : [controllers[name]];
    const lists = {
      [C.FIND_STRUCTURES]: structures, [C.FIND_MY_STRUCTURES]: name === HOME ? structures : [], [C.FIND_MY_SPAWNS]: name === HOME ? [spawn] : [],
      [C.FIND_CONSTRUCTION_SITES]: sites, [C.FIND_MY_CONSTRUCTION_SITES]: sites, [C.FIND_CREEPS]: mine, [C.FIND_MY_CREEPS]: mine,
      [C.FIND_TOMBSTONES]: tombstones.filter(inRoom),
    };
    const room = {
      name,
      controller: controllers[name],
      energyAvailable: name === HOME ? state.energy : 0,
      energyCapacityAvailable: name === HOME ? C.SPAWN_ENERGY_CAPACITY : 0,
      find(type, opts) {
        const filter = opts?.filter;
        const key = filter ? `${type}:${filter.structureType ?? 'other'}` : `${type}`;
        const finds = (r.finds[name] ??= {});
        finds[key] = (finds[key] ?? 0) + 1;
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
  // The snapshot's pass starts with Object.values(Game.rooms): a loop that ran it counts here.
  const roomsSeen = new Proxy(rooms, {
    ownKeys(target) {
      r.snapshots += 1;
      return Reflect.ownKeys(target);
    },
  });
  const owned = [spawn, ...(controllers[HOME] ? [controllers[HOME]] : [])];
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: roomsSeen,
    structures: Object.fromEntries(owned.map((s) => [s.id, s])),
    map: {
      getRoomStatus(name) {
        state.statusCalls.push([offset, name]);
        return { status: STATUS[name] ?? 'normal', timestamp: null };
      },
      // V156: no vision needed. The signer reads it only to size a body for a towered room, and this world has none.
      getRoomTerrain(name) {
        state.terrainCalls.push([offset, name]);
        return { getRawBuffer: () => TERRAIN[name]?.slice() ?? new Uint8Array(2500) };
      },
    },
  };
}

function isObstacle(room, x, y) {
  if (isWall(room, x, y)) return 'a wall';
  if (room === HOME && x === SPAWN.x && y === SPAWN.y) return 'the spawn';
  return null;
}

/** The engine's move resolution (V51, V52), fatigue for the creeps that moved (V54), then the room transfers (V151,
 * V168); from remote-sim.mjs's applyMoves. */
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
      const why = off ?? isObstacle(c.room, d.x, d.y) ?? (others.length ? `${others[0][0].name} moves there too` : stays ? 'a creep stays there' : null);
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

/** The processor after loop `offset`: the spawn's order (V173), the signs (V274), the moves, the creep ticks (V55),
 * the suicides, the spawn's tick (V174), age (V170), the spawn's energy, the tombstones' decay and the new sites. */
function advance(state, offset) {
  const time = T0 + offset;
  if (state.order) {
    const { name, body } = state.order;
    const cost = body.reduce((s, p) => s + C.BODYPART_COST[p], 0);
    if (name.startsWith('g') && !state.spawning && state.energy >= cost) {
      state.energy -= cost;
      const needTime = C.CREEP_SPAWN_TIME * body.length;
      state.spawning = { name, needTime, until: offset + needTime };
      state.creeps.push({ id: id(state.nextId++), name, room: HOME, ...SPAWN, body, fatigue: 0, spawning: true, death: null, rooms: [HOME] });
    } else {
      state.dropped.push({ offset, name });
    }
  }
  for (const c of state.creeps) {
    const s = state.intents.get(c.id)?.sign;
    // The processor's own checks (V274): not spawning, a controller, within 1 on both axes.
    if (!s || c.spawning || s.room !== c.room || range(c, CONTROLLER[s.room]) > 1) continue;
    state.signs[s.room] = { user: USER, text: s.text, time, datetime: Date.UTC(2026, 10, 22) };
  }
  applyMoves(state, offset);
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  for (const c of state.creeps.filter((x) => state.intents.get(x.id)?.suicide)) {
    state.creeps = state.creeps.filter((x) => x !== c);
    state.tombstones.push({
      id: id(state.nextId++), room: c.room, x: c.x, y: c.y, name: c.name, creepId: c.id, deathTime: time,
      decayTime: time + C.TOMBSTONE_DECAY_PER_PART * c.body.length,
    });
  }
  if (state.spawning && offset >= state.spawning.until) {
    const c = state.creeps.find((x) => x.name === state.spawning.name);
    const free = OFFSETS.slice(1).map(([dx, dy]) => ({ x: SPAWN.x + dx, y: SPAWN.y + dy }))
      .find((t) => !isObstacle(HOME, t.x, t.y) && !state.creeps.some((o) => o !== c && o.room === HOME && o.x === t.x && o.y === t.y));
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
  if (state.energy < C.SPAWN_ENERGY_CAPACITY) state.energy += 1;
  state.tombstones = state.tombstones.filter((t) => time < t.decayTime - 1);
  for (const s of state.siteOrders) state.sites.push({ id: id(state.nextId++), ...s, since: offset });
}

export function signSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  state.creeps.push({
    id: id(state.nextId++), name: KEEPER.name, room: HOME, x: KEEPER.x, y: KEEPER.y, body: ['move'], fatigue: 0, spawning: false,
    death: T0 + C.CREEP_LIFE_TIME - 10, rooms: [HOME],
  });
  let phase = 'tour';
  const marks = {};
  let finished = false;
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context, w) => {
      // The overrides in segment CONFIG_SEGMENT before the first global, in the config's field order.
      w.segments[CONFIG_SEGMENT] ??= configRecord(CONFIG_LINES);
      state.context = context;
      context.Creep = EngineCreep;
      context.PathFinder = makePathFinder(state, definePositions(context));
    },
    // The driver's RawMemory, with the bot's assignments to segment SIGN_SEGMENT recorded (the persist glue writes
    // with Reflect.set; the end-of-tick save reads the same object).
    rawMemory: (offset, w) => {
      const raw = driverRawMemory(offset, w);
      raw.segments = new Proxy(raw.segments, {
        set(target, key, value) {
          if (String(key) === String(SIGN_SEGMENT)) state.writes.push({ offset, value });
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
      if (phase === 'tour' && lines.some((l) => DONE_LINE.test(l))) {
        phase = 'settle';
        marks.done = offset;
      } else if (phase === 'tour' && offset >= TOUR_MAX) {
        phase = 'stuck';
        finished = true;
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
        finished = true;
        return SUMMARY;
      }
      return null;
    },
    until: () => finished,
  });

  const problems = [...state.errors.slice(0, 10)];
  const fmt = (o) => (o === undefined ? 'none' : `offset ${o}`);
  // This loop's log lines (after the previous loop's mark, with that loop's command and its reply).
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (test) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some(test));
  if (phase !== 'done') problems.push(`not finished in ${world.ticksRun} ticks (phase ${phase}; marks ${JSON.stringify(marks)})`);
  if (world.halts !== 1 || world.globals !== 2) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 1 and 2`);
  const haltAt = marks.halt ?? Infinity;

  // 8. The log: no fault, no incident, no other signer line, no persistence warning.
  const allowed = (l) => l === PLANNED_LINE || l === signedLine(HOME) || l === signedLine(WEST) || DONE_LINE.test(l) || LOADED_LINE.test(l);
  const bad = world.logs.filter((l) => /^rustedscreep (fault|incident)/.test(l) || l.startsWith('rustedscreep persist:')
    || (l.startsWith('rustedscreep sign: ') && !allowed(l)));
  problems.push(...bad.slice(0, 10).map((l) => `log: ${l}`));
  const expected = new Set(['move', 'signController', 'suicide']);
  for (const c of state.calls) {
    if (!expected.has(c.method) || c.result !== C.OK) {
      problems.push(`offset ${c.offset}: ${c.actor} called ${c.method}(${c.args.join(', ')}) from ${c.from.room} ${at(c.from)}: ${c.result}`);
    }
  }
  for (const c of state.siteCalls) {
    if (c.room !== HOME || c.result !== C.OK || c.args !== 3 || !c.sameRoom) problems.push(`offset ${c.offset}: ${c.room} createConstructionSite(${c.x}, ${c.y}, ${c.type}) with ${c.args} arguments: ${c.result}`);
  }

  // 1. Segment 95: requested only after the config record loaded, written within the save cap, loaded after the halt.
  const configAt = linesAt((l) => l === CONFIG_LINE);
  if (configAt.length !== 2 || !(configAt[0] < haltAt && configAt[1] > haltAt)) {
    problems.push(`config record lines at ${JSON.stringify(configAt)}, expected one in each global (halt at ${haltAt}): ${JSON.stringify(CONFIG_LINE)}`);
  }
  const globals = [[0, haltAt], [haltAt + 1, Infinity]];
  globals.forEach(([from, to], g) => {
    const calls = (world.activeCalls ?? []).filter(([o]) => o >= from && o <= to);
    const loadedAt = configAt[g] ?? Infinity;
    const early = calls.filter(([o, ids]) => o <= loadedAt && ids.includes(SIGN_SEGMENT));
    const later = calls.filter(([o, ids]) => o > loadedAt && ids.includes(SIGN_SEGMENT));
    if (early.length) problems.push(`global ${g + 1}: setActiveSegments with ${SIGN_SEGMENT} at ${JSON.stringify(early)}, before or on the config load at ${loadedAt}`);
    if (!later.length) problems.push(`global ${g + 1}: no setActiveSegments call with ${SIGN_SEGMENT} after the config load at ${loadedAt} (calls ${JSON.stringify(calls)})`);
  });
  const firstWrites = state.writes.filter((w) => w.offset < haltAt);
  const secondWrites = state.writes.filter((w) => w.offset > haltAt);
  if (!firstWrites.length) problems.push(`segment ${SIGN_SEGMENT} was never written in the first global`);
  if (secondWrites.length) problems.push(`segment ${SIGN_SEGMENT} written in the second global, with nothing changed, at ${JSON.stringify(secondWrites.map((w) => w.offset))}`);
  for (const w of state.writes) if (typeof w.value !== 'string' || !w.value.startsWith('rs1b')) problems.push(`offset ${w.offset}: segment ${SIGN_SEGMENT} written as ${JSON.stringify(w.value).slice(0, 40)}`);
  state.writes.forEach((w, i) => {
    const before = state.writes[i - 1];
    if (before && w.offset - before.offset < SAVE_TICKS) problems.push(`segment ${SIGN_SEGMENT} written at offsets ${before.offset} and ${w.offset}, less than sign.save_ticks (${SAVE_TICKS}) apart`);
  });
  const lastWrite = firstWrites.at(-1);
  const loadedLines = world.logs.map((l, i) => [l, i]).filter(([l]) => LOADED_LINE.test(l));
  const loadedAt = linesAt((l) => LOADED_LINE.test(l));
  const loaded = loadedLines.length === 1 ? LOADED_LINE.exec(loadedLines[0][0]) : null;
  const wantWritten = lastWrite ? T0 + lastWrite.offset : null;
  if (!loaded || loadedAt[0] <= haltAt || Number(loaded[1]) !== 4 || Number(loaded[2]) !== wantWritten) {
    problems.push(`record load lines ${JSON.stringify(loadedLines.map(([l]) => l))} at ${JSON.stringify(loadedAt)}, expected one after the halt (${haltAt}) with 4 rooms, written at ${wantWritten}`);
  }

  // 2. The one spawnCreep call: the signer's, after its plan.
  const plannedAt = linesAt((l) => l === PLANNED_LINE);
  const planned = world.logs.filter((l) => l === PLANNED_LINE).length;
  const [order] = state.spawnCalls;
  if (state.spawnCalls.length !== 1) {
    problems.push(`spawnCreep calls ${JSON.stringify(state.spawnCalls.map((c) => [c.offset, c.name, c.result]))}, expected one (the signer's)`);
  }
  if (!order || !/^gW1N1-/.test(order.name ?? '') || JSON.stringify(order.body) !== JSON.stringify(SIGNER_BODY) || order.args !== 2 || order.result !== C.OK) {
    problems.push(`the signer's spawnCreep ${JSON.stringify(order)}, expected (${JSON.stringify(SIGNER_BODY)}, "gW1N1-…") and OK`);
  }
  if (planned !== 1 || !(plannedAt[0] < (order?.offset ?? -1))) problems.push(`${planned} plan line(s) at ${JSON.stringify(plannedAt)}, expected one before the order at ${fmt(order?.offset)}: ${PLANNED_LINE}`);
  if (state.spawnCalls.some((c) => c.offset > haltAt)) problems.push('spawnCreep called in the second global');
  if (state.dropped.length) problems.push(`spawn orders the processor dropped: ${JSON.stringify(state.dropped)}`);
  const signerName = order?.name;
  const loops = state.reads.filter((r) => r && r.snapshots > 0);
  const lived = loops.filter((r) => r.signer);
  const outAt = lived.find((r) => !r.signer.spawning)?.offset;
  const spawningLoops = lived.filter((r) => r.signer.spawning).length;
  if (spawningLoops !== C.CREEP_SPAWN_TIME * SIGNER_BODY.length) problems.push(`the signer was spawning on ${spawningLoops} loops, expected CREEP_SPAWN_TIME × ${SIGNER_BODY.length}`);

  // 3. The signs: one on W1N1's controller and one on W2N1's, from next to it; none on W2N2's or in W1N2.
  const signs = state.calls.filter((c) => c.method === 'signController');
  const signAt = {};
  for (const room of ROOMS) {
    const on = signs.filter((c) => c.target === room || c.from.room === room);
    const want = room === HOME || room === WEST ? 1 : 0;
    if (on.length !== want) problems.push(`${on.length} signController call(s) on or in ${room} (${JSON.stringify(on.map((c) => c.offset))}), expected ${want}`);
    for (const c of on) {
      if (c.actor !== signerName || c.argCount !== 2 || c.text !== SIGN_TEXT || !c.sameObject || c.from.room !== room || range(c.from, CONTROLLER[room]) > 1) {
        problems.push(`offset ${c.offset}: signController by ${c.actor} with ${c.argCount} argument(s), text ${JSON.stringify(c.text)} (this loop's controller: ${c.sameObject}) from ${c.from.room} ${at(c.from)}`);
      }
    }
    if (want) {
      signAt[room] = on[0]?.offset;
      const lineAt = linesAt((l) => l === signedLine(room));
      const n = world.logs.filter((l) => l === signedLine(room)).length;
      if (n !== 1 || lineAt[0] !== (on[0]?.offset ?? NaN) + 1) problems.push(`${n} line(s) ${JSON.stringify(signedLine(room))} at ${JSON.stringify(lineAt)}, expected one on the loop after the sign at ${fmt(on[0]?.offset)}`);
    }
  }
  if (state.signs[HOME]?.text !== SIGN_TEXT || state.signs[WEST]?.text !== SIGN_TEXT || state.signs[CORNER] || state.signs[NORTH]) {
    problems.push(`the controllers' signs at the end: ${JSON.stringify(state.signs)}`);
  }

  // 4. The reads: on every loop the signer stands in a room, one `sign` read and one tower find there, none elsewhere.
  const towerKey = `${C.FIND_HOSTILE_STRUCTURES}:tower`;
  const standing = {};
  for (const r of loops) {
    const here = r.signer?.room;
    if (here) standing[here] = (standing[here] ?? 0) + 1;
    for (const room of ROOMS) {
      const want = room === here ? 1 : 0;
      const reads = r.signReads[room] ?? 0;
      const towers = r.finds[room]?.[towerKey] ?? 0;
      if (reads !== want || towers !== want) {
        problems.push(`offset ${r.offset}: ${reads} sign read(s) and ${towers} tower find(s) in ${room} with the signer ${here ? `in ${here}` : 'gone'}, expected ${want} and ${want}`);
      }
    }
  }
  if (!standing[CORNER]) problems.push(`the signer never stood in ${CORNER}, whose server sign it should have seen`);

  // 5. The route searches: none into W1N2 (its status was read), none incomplete, none in the second global; no terrain read.
  const north = state.searches.filter((s) => s.to.roomName === NORTH);
  if (north.length) problems.push(`PathFinder.search with a target in ${NORTH}: ${JSON.stringify(north.slice(0, 3))}`);
  const northStatus = state.statusCalls.filter(([, n]) => n === NORTH).length;
  if (!northStatus) problems.push(`Game.map.getRoomStatus("${NORTH}") was never called`);
  const short = state.searches.filter((s) => s.incomplete !== false);
  if (short.length) problems.push(`incomplete route searches: ${JSON.stringify(short.slice(0, 3))}`);
  const late = state.searches.filter((s) => s.offset > haltAt);
  if (late.length) problems.push(`route searches in the second global: ${JSON.stringify(late.slice(0, 3))}`);
  if (state.terrainCalls.length) problems.push(`Game.map.getRoomTerrain calls ${JSON.stringify(state.terrainCalls.slice(0, 4))}`);

  // 6. The end: one suicide on the loop of the one end line, after both signs, then nothing.
  const suicides = state.calls.filter((c) => c.method === 'suicide');
  const doneLines = world.logs.filter((l) => DONE_LINE.test(l));
  const doneAt = linesAt((l) => DONE_LINE.test(l));
  const end = suicides[0];
  const doneRoom = doneLines.length === 1 ? DONE_LINE.exec(doneLines[0])[1] : null;
  if (suicides.length !== 1 || end.actor !== signerName || end.argCount !== 0) {
    problems.push(`suicide calls ${JSON.stringify(suicides.map((c) => [c.offset, c.actor, c.argCount]))}: expected one by ${signerName} with no argument`);
  } else if (doneLines.length !== 1 || doneAt[0] !== end.offset || doneRoom !== end.from.room) {
    problems.push(`end lines ${JSON.stringify(doneLines)} at ${JSON.stringify(doneAt)}, expected one on the suicide's loop (${end.offset}) naming ${end.from.room}`);
  } else if (!(end.offset > Math.max(signAt[HOME] ?? Infinity, signAt[WEST] ?? Infinity) + 1)) {
    problems.push(`the signer ended at offset ${end.offset}, before both signs showed (${JSON.stringify(signAt)})`);
  }
  if (end && state.calls.some((c) => c.actor === signerName && c.offset > end.offset)) problems.push('the signer called something after its suicide');
  if (end && loops.some((r) => r.offset > end.offset && r.signer)) problems.push('the signer still stood after its suicide');

  // 7. The console summaries.
  const summaries = commandResults(world, SUMMARY);
  const [before, final] = summaries;
  const countsOf = (s) => {
    const m = COUNTS.exec(s ?? '');
    return m && { rooms: +m[1], signed: +m[2], unsigned: +m[3], server: +m[4], unreachable: +m[5], deadly: +m[6], none: +m[7], due: +m[8], loaded: m[9] };
  };
  const b = countsOf(before);
  if (summaries.length !== 2) problems.push(`${summaries.length} sign summaries, expected 2`);
  if (!b || b.signed !== 2 || b.server !== 1 || b.loaded !== 'loaded' || !before.includes(' | no signer | ') || !before.includes(' | since the global: signed 2, ')) {
    problems.push(`the summary before the halt ${JSON.stringify(before)}: expected the record's counts with signed 2 and server sign 1, loaded, no signer, and signed 2 since the global`);
  }
  const finalWant = `rustedscreep sign at tick ${T0 + (marks.final ?? 0)}: on | record 4 rooms (signed 2, unsigned 0, server sign 1, unreachable 1, deadly 0, `
    + 'no controller 0; 0 due), loaded | no signer | plan none | since the global: signed 0, searches 0, lost 0, retreats 0, planned 0, '
    + 'terrain reads 0 (failed 0)';
  if (final !== finalWant) problems.push(`the final summary ${JSON.stringify(final)}, expected ${JSON.stringify(finalWant)}`);

  if (problems.length) throw new Error(`sign-sim:\n  ${problems.slice(0, 25).join('\n  ')}`);

  const fromOf = (room) => at(signs.find((c) => c.target === room).from);
  return {
    ticks: world.ticksRun,
    orderAt: order.offset,
    outAt,
    signs: `${HOME} at offset ${signAt[HOME]} from ${fromOf(HOME)}, ${WEST} at ${signAt[WEST]} from ${fromOf(WEST)}`,
    standing: ROOMS.filter((n) => standing[n]).map((n) => `${n} ${standing[n]}`).join(', '),
    end: `${end.from.room} at offset ${end.offset}`,
    searches: state.searches.length,
    status: northStatus,
    writes: state.writes.map((w) => w.offset).join(' '),
    loaded: loadedLines[0][0],
    before,
    final,
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = signSim(process.argv[2]);
    console.log(`sign-sim ok: ${r.ticks} ticks, 2 globals; signer ordered at offset ${r.orderAt}, out at ${r.outAt}; signed ${r.signs}; `
      + `loops standing in ${r.standing}; ended in ${r.end}; ${r.searches} PathFinder searches, none into ${NORTH} `
      + `(its status read ${r.status} time(s)); segment ${SIGN_SEGMENT} written at offsets ${r.writes}; after the halt: ${r.loaded} | ${r.final}`);
  } catch (e) {
    fail(e.message);
  }
}
