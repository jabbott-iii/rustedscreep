// Remote mining's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/remote.rs, snapshot.rs `push_remote` and `own_username`, and act.rs, over
// rustedscreep-core `economy::remotes`; P6.2, ADR-029: remote mining before P8's scouting, with each remote creep's
// remote room in its name). Two rooms:
// - W1N1, owned at RCL5: a spawn at (25, 25), the controller, and CONTROLLER_STRUCTURES.extension[5] (30) full
//   extensions, so energyCapacityAvailable is SPAWN_ENERGY_CAPACITY + 30 × EXTENSION_ENERGY_CAPACITY[5] (1800). It has
//   no source, so the miners and the haulers stay inert and the home has no hauling plan: the remote manager asks for no
//   creep, but it still reads its candidates off the home's grid, scouts, searches the ways, prices and chooses the room,
//   and places its sites (`plan_home`, `place_home`). No bot worker either, only an idle own creep whose name the bot did
//   not give (KEEPER): movement reads a room's terrain only while an own creep stands in it, and without that grid the
//   home has no candidates and no ways (a creep-free home never searches W2N1). The opening's interim placement may
//   place W1N1 sites (a controller link here). Walls on every edge but the west one, whose tiles 20 to 30 open onto W2N1;
// - W2N1, neutral (a controller of level 0 with no owner, `my` undefined, V101) with two sources, a mineral and one
//   worn road; its east edge's tiles 20 to 30 open onto W1N1. The controller, the sources and the mineral stand on
//   terrain walls (V228). Three bot creeps of W1N1 stand in it: a remote hauler `cW1N1W2N1-…` (one CARRY and one
//   MOVE part, so the snapshot reads W2N1's roads, ADR-029; it walks to a mining tile and waits, as nothing lies
//   there), a scout `sW1N1W2N1-…` (one MOVE part) and a reserver
//   `rW1N1W2N1-…` with CLAIM CLAIM parts and as many MOVE parts. CLAIM is the CLAIM count of the reserver the profit
//   model sizes at 1800 energy (`reserver_body`, economy/remote.rs), which the bot's check uses: three CLAIM parts and a
//   MOVE part cost 1850 (BODYPART_COST), and two CLAIM parts with two MOVE parts walk as fast as one with one, so they
//   cost the same energy a reservation tick and win the tie on intents. So the bot reserves only while
//   ticksToEnd + 2 × CONTROLLER_RESERVE stays within CONTROLLER_RESERVE_MAX.
// The harness plays the engine's part as far as the bot can see it:
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52, as in link-sim.mjs), with
//   fatigue (V54, V55); a creep that ends a tick on an edge tile goes to the next room (V151, as in route-sim.mjs);
// - `reserveController` makes the game side's checks in V263's order; the processor (V264) starts a reservation at
//   `{endTime: time + 1}`, adds CLAIM parts × CONTROLLER_RESERVE unless that passes time + CONTROLLER_RESERVE_MAX (then
//   it drops the reserve, which the sim counts as wasted), and the controller's tick clears it at endTime - 1 (V265);
//   `controller.reservation` is `{username, ticksToEnd: endTime - time}` or undefined, a new object on every read
//   (V268); `suicide` removes the creep at the end of the tick and leaves a tombstone for TOMBSTONE_DECAY_PER_PART a
//   part;
// - `room.createConstructionSite` makes the game side's checks (V200, V237, V238) and an accepted site appears on the
//   next tick; nothing builds it;
// - `PathFinder.search` and `Game.map.getRoomStatus` are recorded: every remote creep's goal is in its own room, so
//   neither may run; `spawnCreep` makes the runtime's checks and its `Memory.creeps` write, and the processor drops the
//   order (V171, V181, V173, as in planner-sim.mjs): no call may order other than a worker.
// It runs the bot until the reserver reserves, lets the reservation grow for GROW loops, holds ticksToEnd at PINNED
// (CONTROLLER_RESERVE_MAX - CLAIM × CONTROLLER_RESERVE + 1, 4999) for PIN loops, lets it run for RELEASE loops (it then
// alternates between PINNED - 1 and PINNED), halts once (a global reset) and runs AFTER more loops. It checks:
// 1. the snapshot's reads of W2N1, a remote room (a room it does not own that an own creep's name gives): on every loop
//    exactly one `reservation` read (undefined until the first reserve, then the bot's username and a positive
//    ticksToEnd), one FIND_HOSTILE_STRUCTURES find, one container-filtered FIND_STRUCTURES find (push_objects reads it
//    as an owned room's objects), and, only on the loops whose Game.time is divisible by remote.road_read_ticks (5),
//    one road-filtered FIND_STRUCTURES find with one `hits` and one `hitsMax` read a road; none of these in W1N1, and
//    `owner` read from an own creep exactly once a global: on the first loop whose reservation read gave an object;
// 2. `rustedscreep.remote()` on the first loop names W2N1 seen with 2 sources and the creeps s1 r1, not mined; before the
//    halt it shows W2N1 mined under a reserve plan, the reserver, the reservation the last read gave, and the sites
//    placed in W2N1;
// 3. the scout calls `suicide()` exactly once, on the loop after the Optional `remote placement` job searched W2N1's
//    ways (the loop the reserver takes its first step toward the controller), with no argument;
// 4. the reserver walks next to the controller with engine-valid moves and calls `reserveController` with that loop's
//    controller object, always from next to it, on exactly the loops of the first global it starts there with
//    ticksToEnd + CLAIM × CONTROLLER_RESERVE <= CONTROLLER_RESERVE_MAX: none while ticksToEnd is PINNED, one at
//    PINNED - 1; the processor drops none; and after the halt, once the placement has searched the ways again, it
//    reserves again within AFTER loops by the same rule;
// 5. `createConstructionSite(x, y, "container")` is called on that loop's W2N1 Room object once for a tile next to each
//    source, before any road site, never on a tile that holds a site, at most one W2N1 call a loop, never with
//    remote.max_sites (5) own sites in W2N1, and every call returns OK;
// and no fault line, no incident, no site refusal line, no remote spawn order or route search, every creep call OK, one
// halt, two globals. It prints the snapshot's W2N1 calls per loop. npm run check runs it (row 23); standalone:
// node js/tools/remote-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CREEP_METHODS, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { ENGINE_CONSTANTS, commandResults, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
const T0 = 50000000;
const HOME = 'W1N1';
const REMOTE = 'W2N1';
const USER = 'bot';
const LEVEL = 5;
const SPAWN = { x: 25, y: 25 };
const HOME_CONTROLLER = { x: 20, y: 5 };
const EXTENSION_CAPACITY = C.EXTENSION_ENERGY_CAPACITY[LEVEL];
/** Every extension RCL5 allows, in three rows east of the spawn (off its way to the west exits). */
const EXTENSIONS = Array.from({ length: C.CONTROLLER_STRUCTURES.extension[LEVEL] }, (_, i) => ({ x: 35 + (i % 10), y: 40 + Math.floor(i / 10) }));
const CAPACITY = C.SPAWN_ENERGY_CAPACITY + EXTENSIONS.length * EXTENSION_CAPACITY;
const REMOTE_CONTROLLER = { x: 10, y: 12 };
const SOURCES = [{ x: 12, y: 38 }, { x: 36, y: 10 }];
const MINERAL = { x: 42, y: 42, type: 'H' };
const ROADS = [{ x: 40, y: 25, hits: C.ROAD_HITS / 2 }];
const SCOUT_AT = { x: 30, y: 40 };
/** An own creep in W1N1 with a name the bot did not give: no job drives it, but movement reads a room's terrain only
 * while an own creep stands in it (crates/rustedscreep-wasm/src/movement.rs), and the remote manager needs the home's
 * grid for its candidates and ways. */
const KEEPER = 'Keeper';
const KEEPER_AT = { x: 40, y: 20 };
const RESERVER_AT = { x: 38, y: 28 };
/** The CLAIM parts of the reserver the profit model sizes at CAPACITY (see the header): as many as fit beside a MOVE part. */
const CLAIM = Math.floor((CAPACITY - C.BODYPART_COST.move) / C.BODYPART_COST.claim);
const RESERVER_BODY = [...Array(CLAIM).fill('claim'), ...Array(CLAIM).fill('move')];
const SCOUT_BODY = ['move'];
/** The least ticksToEnd at which a reserve of CLAIM parts would pass the cap (V264): 4999. */
const PINNED = C.CONTROLLER_RESERVE_MAX - CLAIM * C.CONTROLLER_RESERVE + 1;
/** REMOTE_MMO (crates/rustedscreep-core/src/config.rs). */
const REMOTE_CFG = { maxRooms: 2, spawnPermille: 400, maxSites: 5, roadReadTicks: 5 };
const GROW = 4;
const PIN = 4;
const RELEASE = 6;
/** Ticks after the halt: the boot, the warm-up loop (no Optional work), then the Optional placement searches the ways
 * again and the reserver resumes (a build of 12:13 today reserved again 4 ticks after the halt). */
const AFTER = 10;
const RUN = 400;
const SEEN_AT = 1;
const SUMMARY = 'rustedscreep.remote()';
const HALT = 'Game.cpu.halt()';

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const botName = (role, time, index) => `${role}${HOME}${REMOTE}-${((BigInt(time) << 8n) | BigInt(index)).toString(36)}`;
const SCOUT = botName('s', T0 - 300, 0);
const RESERVER = botName('r', T0 - 100, 0);
/** A remote hauler of W1N1 for W2N1 (one CARRY and one MOVE part): roads are read only in a room one stands in. With
 * no container or pile at a mining tile it only walks there and waits. */
const HAULER = botName('c', T0 - 50, 0);
const HAULER_AT = { x: 40, y: 40 };
const HAULER_BODY = ['carry', 'move'];
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

const TERRAIN = {
  [HOME]: terrain(['W'], []),
  [REMOTE]: terrain(['E'], [REMOTE_CONTROLLER, ...SOURCES, MINERAL]),
};
const isWall = (room, x, y) => (TERRAIN[room][y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

/** The engine's RoomPosition, as far as the crate and a PathFinder call would use it (V103). */
function definePositions(context) {
  class RoomPosition {
    constructor(x, y, roomName) { this.__packedPos = packPos(roomName, x, y); }
    get x() { return unpack(this.__packedPos).x; }
    get y() { return unpack(this.__packedPos).y; }
    get roomName() { return unpack(this.__packedPos).roomName; }
  }
  context.RoomPosition = RoomPosition;
}

function makeWorld() {
  return {
    creeps: [
      { id: id(100), name: SCOUT, room: REMOTE, ...SCOUT_AT, body: SCOUT_BODY, fatigue: 0, death: T0 + 1200, rooms: [REMOTE] },
      { id: id(101), name: RESERVER, room: REMOTE, ...RESERVER_AT, body: RESERVER_BODY, fatigue: 0, death: T0 + 500, rooms: [REMOTE] },
      { id: id(103), name: HAULER, room: REMOTE, ...HAULER_AT, body: HAULER_BODY, fatigue: 0, death: T0 + 1400, rooms: [REMOTE] },
      { id: id(102), name: KEEPER, room: HOME, ...KEEPER_AT, body: ['move'], fatigue: 0, death: T0 + 1400, rooms: [HOME] },
    ],
    /** The controller's reservation (V264, V265): { endTime } or null. */
    reservation: null,
    /** Offsets at whose start the harness sets ticksToEnd to PINNED. */
    pin: null,
    /** Own construction sites: { id, room, x, y, type, since }. */
    sites: [],
    tombstones: [],
    nextId: 200,
    intents: new Map(),
    siteOrders: [],
    calls: [],
    siteCalls: [],
    spawnCalls: [],
    spawned: new Set(),
    context: null,
    searches: [],
    statusCalls: [],
    dropped: [],
    reads: [],
    errors: [],
  };
}

const reservationOf = (state, time) => (state.reservation ? { username: USER, ticksToEnd: state.reservation.endTime - time } : undefined);

/** Why the game side refuses a site of `type` on (x, y) in `room` (V200, V237, V238), or OK. */
function refuses(state, room, x, y, type) {
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x > 49 || y > 49) return C.ERR_INVALID_ARGS;
  if (!C.CONSTRUCTION_COST[type]) return C.ERR_INVALID_ARGS;
  // (3) to (5) pass: no spawn site, W1N1's controller is the bot's, W2N1's has no level and the only reservation is the bot's.
  const level = room === HOME ? LEVEL : 0;
  const here = (o) => o.room === room && o.x === x && o.y === y;
  const structures = room === HOME
    ? [{ ...SPAWN, type: 'spawn' }, ...EXTENSIONS.map((e) => ({ ...e, type: 'extension' }))].map((s) => ({ ...s, room }))
    : ROADS.map((r) => ({ ...r, type: 'road', room }));
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
  state.siteOrders = [];
  if (state.pin && offset >= state.pin.from && offset <= state.pin.to) state.reservation = { endTime: time + PINNED };
  const reserver = state.creeps.find((c) => c.name === RESERVER);
  const r = {
    offset, snapshots: 0, reservation: [], homeReservation: 0, owner: 0, roadHits: 0, roadHitsMax: 0, finds: { [HOME]: {}, [REMOTE]: {} },
    ticksToEnd: reservationOf(state, time)?.ticksToEnd ?? null,
    reserver: reserver ? { room: reserver.room, x: reserver.x, y: reserver.y } : null,
    remoteSites: state.sites.filter((s) => s.room === REMOTE).length,
  };
  state.reads[offset] = r;
  const registered = new Set();
  const intentsOf = (cid) => {
    if (!state.intents.has(cid)) state.intents.set(cid, {});
    return state.intents.get(cid);
  };

  // W1N1: the spawn, the extensions and the controller (an owned controller's getters, V189).
  const homeController = {
    id: id(1), structureType: 'controller', my: true, owner: { username: USER }, level: LEVEL, pos: pos(HOME, HOME_CONTROLLER.x, HOME_CONTROLLER.y),
    progress: 0, progressTotal: C.CONTROLLER_LEVELS[LEVEL], ticksToDowngrade: C.CONTROLLER_DOWNGRADE[LEVEL],
    get reservation() {
      r.homeReservation += 1;
      return undefined;
    },
  };
  const spawn = {
    id: id(2), name: 'Spawn1', structureType: 'spawn', my: true, owner: { username: USER }, pos: pos(HOME, SPAWN.x, SPAWN.y),
    store: makeStore({ energy: C.SPAWN_ENERGY_CAPACITY }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    spawning: null,
    // The runtime's checks (V171) and its `Memory.creeps[name]` write on OK (V181); the processor drops every accepted
    // order (V173), so no creep appears (as in planner-sim.mjs).
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      const call = { offset, args: args.length, name, result: C.OK };
      state.spawnCalls.push(call);
      if (!name || !Array.isArray(body) || state.spawned.has(name)) call.result = C.ERR_INVALID_ARGS;
      else if (!opts.dryRun) {
        state.spawned.add(name);
        const mem = state.context.Memory;
        if (mem && typeof mem === 'object') {
          if (mem.creeps === undefined) mem.creeps = {};
          if (mem.creeps && typeof mem.creeps === 'object') mem.creeps[name] = opts.memory || {};
        }
      }
      return call.result;
    },
  };
  const extensions = EXTENSIONS.map((e, i) => ({
    id: id(10 + i), structureType: 'extension', my: true, owner: { username: USER }, pos: pos(HOME, e.x, e.y),
    store: makeStore({ energy: EXTENSION_CAPACITY }, { resourceCapacity: { energy: EXTENSION_CAPACITY } }),
  }));
  const homeStructures = [spawn, ...extensions, homeController];

  // W2N1: the neutral controller, whose `reservation` is counted, its sources, mineral and road.
  const remoteController = {
    id: id(50), structureType: 'controller', level: 0, pos: pos(REMOTE, REMOTE_CONTROLLER.x, REMOTE_CONTROLLER.y),
    get reservation() {
      const v = reservationOf(state, time);
      r.reservation.push(v === undefined ? undefined : { ...v });
      return v;
    },
  };
  const sources = SOURCES.map((s, i) => ({
    id: id(51 + i), energy: C.SOURCE_ENERGY_NEUTRAL_CAPACITY, energyCapacity: C.SOURCE_ENERGY_NEUTRAL_CAPACITY, pos: pos(REMOTE, s.x, s.y),
    ticksToRegeneration: undefined,
  }));
  const mineral = { id: id(55), mineralType: MINERAL.type, mineralAmount: 0, density: 1, pos: pos(REMOTE, MINERAL.x, MINERAL.y), ticksToRegeneration: undefined };
  const roads = ROADS.map((rd, i) => ({
    id: id(60 + i), structureType: 'road', pos: pos(REMOTE, rd.x, rd.y), ticksToDecay: 500,
    get hits() { r.roadHits += 1; return rd.hits; },
    get hitsMax() { r.roadHitsMax += 1; return C.ROAD_HITS; },
  }));
  for (const s of [...homeStructures, remoteController, ...roads]) registered.add(s);

  const record = (c, method, args, result, extra = {}) => {
    const call = {
      offset, actor: c.name, method, argCount: args.length,
      args: args.map((a) => (a && typeof a === 'object' ? a.name ?? a.structureType ?? a.id : a)), result,
      from: { room: c.room, x: c.x, y: c.y }, ...extra,
    };
    state.calls.push(call);
    return call;
  };
  const creeps = state.creeps.map((c) => {
    const parts = c.body.length;
    const self = {
      id: c.id, name: c.name, my: true, spawning: false, fatigue: c.fatigue, pos: pos(c.room, c.x, c.y),
      body: c.body.map((type) => ({ type, hits: 100 })), hits: 100 * parts, hitsMax: 100 * parts, ticksToLive: c.death - time,
      store: makeStore({}, { capacity: C.CARRY_CAPACITY * count(c.body, 'carry') }),
      get owner() {
        r.owner += 1;
        return { username: USER };
      },
    };
    // The act layer's other bindings: nothing here calls for them; recorded (OK) so they show up as unexpected.
    for (const m of CREEP_METHODS) self[m] = (...args) => record(c, m, args, C.OK).result;
    self.move = (...args) => {
      const d = +args[0];
      const result = c.fatigue > 0 ? C.ERR_TIRED : !count(c.body, 'move') ? C.ERR_NO_BODYPART
        : !Number.isInteger(d) || d < 1 || d > 8 ? C.ERR_INVALID_ARGS : C.OK;
      if (result === C.OK) intentsOf(c.id).move = d;
      return record(c, 'move', args, result).result;
    };
    self.reserveController = (...args) => {
      // V263, in its order (the shared first checks pass: the creep is the bot's and not spawning).
      const [target] = args;
      const result = (() => {
        if (!target?.id || !registered.has(target)) return C.ERR_INVALID_TARGET;
        if (target.pos.roomName !== c.room || range(c, target.pos) > 1) return C.ERR_NOT_IN_RANGE;
        if (target.structureType !== 'controller') return C.ERR_INVALID_TARGET;
        if (target.owner) return C.ERR_INVALID_TARGET;
        if (!count(c.body, 'claim')) return C.ERR_NO_BODYPART;
        return C.OK;
      })();
      if (result === C.OK) intentsOf(c.id).reserve = true;
      return record(c, 'reserveController', args, result, { sameObject: target === remoteController }).result;
    };
    self.suicide = (...args) => {
      intentsOf(c.id).suicide = true;
      return record(c, 'suicide', args, C.OK).result;
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
  for (const name of [HOME, REMOTE]) {
    const inRoom = (o) => o.pos.roomName === name;
    const sites = state.sites.filter((s) => s.room === name).map(siteObject);
    const mine = creeps.filter(inRoom);
    const lists = name === HOME
      ? {
        [C.FIND_STRUCTURES]: homeStructures, [C.FIND_MY_STRUCTURES]: homeStructures, [C.FIND_MY_SPAWNS]: [spawn],
      }
      : {
        [C.FIND_SOURCES]: sources, [C.FIND_SOURCES_ACTIVE]: sources, [C.FIND_MINERALS]: [mineral], [C.FIND_STRUCTURES]: [remoteController, ...roads],
        [C.FIND_TOMBSTONES]: tombstones.filter(inRoom),
      };
    Object.assign(lists, { [C.FIND_CONSTRUCTION_SITES]: sites, [C.FIND_MY_CONSTRUCTION_SITES]: sites, [C.FIND_CREEPS]: mine, [C.FIND_MY_CREEPS]: mine });
    const room = {
      name,
      controller: name === HOME ? homeController : remoteController,
      energyAvailable: name === HOME ? CAPACITY : 0,
      energyCapacityAvailable: name === HOME ? CAPACITY : 0,
      find(type, opts) {
        const filter = opts?.filter;
        const key = type === C.FIND_STRUCTURES && filter ? `${type}:${filter.structureType ?? 'other'}` : `${type}`;
        r.finds[name][key] = (r.finds[name][key] ?? 0) + 1;
        return findFiltered(lists[type] ?? [], opts);
      },
      getTerrain() { return { getRawBuffer: () => TERRAIN[name].slice() }; },
      createConstructionSite(...args) {
        const [x, y, type] = args;
        const call = {
          offset, room: name, x, y, type, args: args.length, sameRoom: this === room,
          open: state.sites.filter((s) => s.room === name).length + state.siteOrders.filter((s) => s.room === name).length,
          onSite: state.sites.some((s) => s.room === name && s.x === x && s.y === y), result: refuses(state, name, x, y, type),
        };
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
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: roomsSeen,
    structures: Object.fromEntries(homeStructures.map((s) => [s.id, s])),
    map: {
      getRoomStatus(name) {
        state.statusCalls.push([offset, name]);
        return { status: 'normal', timestamp: null };
      },
    },
  };
}

function isObstacle(room, x, y) {
  if (isWall(room, x, y)) return 'a wall';
  if (room === HOME && [SPAWN, HOME_CONTROLLER, ...EXTENSIONS].some((s) => s.x === x && s.y === y)) return 'a structure';
  return null;
}

/** The engine's move resolution (V51, V52), fatigue for the creeps that moved (V54), then the room transfers (V151). */
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
    const road = c.room === REMOTE && ROADS.some((rd) => rd.x === c.x && rd.y === c.y);
    const swamp = (TERRAIN[c.room][c.y * 50 + c.x] & C.TERRAIN_MASK_SWAMP) !== 0;
    const weight = c.body.filter((p) => p !== 'move' && p !== 'carry').length;
    c.fatigue += weight * (road ? 1 : swamp ? 10 : 2);
  }
  for (const c of state.creeps) {
    if (!edge(c)) continue;
    const [rx, ry] = roomXY(c.room);
    const next = c.x === 0 ? roomAt(rx - 1, ry) : c.x === 49 ? roomAt(rx + 1, ry) : c.y === 0 ? roomAt(rx, ry - 1) : roomAt(rx, ry + 1);
    state.errors.push(`offset ${offset}: ${c.name} ended the tick on ${c.room} ${at(c)} and goes to ${next}`);
    if (c.x === 0) c.x = 49; else if (c.x === 49) c.x = 0; else if (c.y === 0) c.y = 49; else c.y = 0;
    c.room = next;
    c.rooms.push(next);
  }
}

/** The processor after loop `offset`: the reserves (V264), the moves, the creep ticks (V55), the suicides, the
 * controller's tick (V265), the tombstones' decay and the new sites. */
function advance(state, offset) {
  const time = T0 + offset;
  for (const c of state.creeps) {
    if (!state.intents.get(c.id)?.reserve) continue;
    const effect = count(c.body, 'claim') * C.CONTROLLER_RESERVE;
    state.reservation ??= { endTime: time + 1 };
    if (state.reservation.endTime + effect > time + C.CONTROLLER_RESERVE_MAX) state.dropped.push({ offset, ticksToEnd: state.reservation.endTime - time });
    else state.reservation.endTime += effect;
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
  if (state.reservation && time >= state.reservation.endTime - 1) state.reservation = null;
  state.tombstones = state.tombstones.filter((t) => time < t.decayTime - 1);
  for (const s of state.siteOrders) state.sites.push({ id: id(state.nextId++), ...s, since: offset });
}

export function remoteSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let phase = 'walk';
  const marks = {};
  let finished = false;
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context) => {
      state.context = context;
      definePositions(context);
      context.PathFinder = {
        CostMatrix: class CostMatrix {
          constructor() { this._bits = new Uint8Array(2500); }
          set(x, y, v) { this._bits[x * 50 + y] = v; }
          get(x, y) { return this._bits[x * 50 + y]; }
        },
        search: (...args) => {
          state.searches.push([state.reads.length - 1, args.length]);
          return { path: [], ops: 0, cost: 0, incomplete: true };
        },
      };
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      let cmd = null;
      if (phase === 'walk' && state.calls.some((c) => c.method === 'reserveController')) {
        phase = 'grow';
        marks.firstReserve = offset;
      } else if (phase === 'grow' && offset >= marks.firstReserve + GROW) {
        phase = 'pin';
        state.pin = { from: offset + 1, to: offset + PIN };
      } else if (phase === 'pin' && offset >= state.pin.to) {
        phase = 'release';
        marks.released = offset;
      } else if (phase === 'release' && offset >= marks.released + RELEASE) {
        phase = 'summary';
        marks.summary = offset;
        cmd = SUMMARY;
      } else if (phase === 'summary') {
        phase = 'halted';
        marks.halt = offset;
        cmd = HALT;
      } else if (phase === 'halted' && offset >= marks.halt + AFTER) {
        finished = true;
      }
      return offset === SEEN_AT ? SUMMARY : cmd;
    },
    until: () => finished,
  });

  const problems = [...state.errors.slice(0, 10)];
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|rustedscreep remote: no /.test(l));
  problems.push(...bad.slice(0, 10).map((l) => `log: ${l}`));
  if (!finished) problems.push(`not finished in ${world.ticksRun} ticks (phase ${phase}; first reserve at ${marks.firstReserve ?? 'none'})`);
  if (world.halts !== 1 || world.globals !== 2) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 1 and 2`);
  // With no hauling plan the remote manager orders no creep; the opening's worker orders are its own.
  const remoteOrders = state.spawnCalls.filter((c) => !/^w[WE]\d+[NS]\d+-/.test(c.name ?? ''));
  if (remoteOrders.length) problems.push(`spawnCreep calls for other than a worker: ${JSON.stringify(remoteOrders.slice(0, 4))}`);
  for (const c of state.spawnCalls) if (c.result !== C.OK || c.args !== 2) problems.push(`offset ${c.offset}: spawnCreep(${c.name}) with ${c.args} arguments: ${c.result}`);
  if (state.searches.length || state.statusCalls.length) {
    problems.push(`PathFinder searches ${JSON.stringify(state.searches.slice(0, 4))}, getRoomStatus calls ${JSON.stringify(state.statusCalls.slice(0, 4))}: no remote creep leaves its room`);
  }
  const expected = new Set(['move', 'reserveController', 'suicide']);
  for (const c of state.calls) {
    if (!expected.has(c.method) || c.result !== C.OK) {
      problems.push(`offset ${c.offset}: ${c.actor} called ${c.method}(${c.args.join(', ')}) from ${c.from.room} ${at(c.from)}: ${c.result}`);
    }
  }
  const haltAt = marks.halt ?? Infinity;
  const loops = state.reads.filter((r) => r && r.snapshots > 0);
  const first = loops.filter((r) => r.offset <= haltAt);
  const second = loops.filter((r) => r.offset > haltAt);
  if (first.length !== Math.min(haltAt, world.ticksRun - 1) || first[0]?.offset !== 1) {
    problems.push(`snapshot loops in the first global at offsets ${first[0]?.offset}..${first.at(-1)?.offset} (${first.length}), expected 1..${haltAt}`);
  }
  if (second.length < AFTER - 1) problems.push(`${second.length} snapshot loops after the halt, expected at least ${AFTER - 1}`);

  // 1. The snapshot's reads of the remote room.
  for (const r of state.reads) {
    if (!r) continue;
    const loop = r.snapshots > 0;
    const roadTick = loop && (T0 + r.offset) % REMOTE_CFG.roadReadTicks === 0;
    const f = r.finds[REMOTE];
    const want = {
      reservation: [r.reservation.length, loop ? 1 : 0],
      'hostile-structures finds': [f[C.FIND_HOSTILE_STRUCTURES] ?? 0, loop ? 1 : 0],
      'container finds': [f[`${C.FIND_STRUCTURES}:container`] ?? 0, loop ? 1 : 0],
      'road finds': [f[`${C.FIND_STRUCTURES}:road`] ?? 0, roadTick ? 1 : 0],
      'road hits reads': [r.roadHits, roadTick ? ROADS.length : 0],
      'road hitsMax reads': [r.roadHitsMax, roadTick ? ROADS.length : 0],
      'W1N1 reservation reads': [r.homeReservation, 0],
      'W1N1 hostile-structures finds': [r.finds[HOME][C.FIND_HOSTILE_STRUCTURES] ?? 0, 0],
      'W1N1 road finds': [r.finds[HOME][`${C.FIND_STRUCTURES}:road`] ?? 0, 0],
    };
    for (const [what, [got, exp]] of Object.entries(want)) {
      if (got !== exp) problems.push(`offset ${r.offset} (Game.time ${T0 + r.offset}): ${what} ${got}, expected ${exp}`);
    }
    for (const v of r.reservation) {
      const model = r.ticksToEnd;
      const ok = v === undefined ? model === null : v.username === USER && v.ticksToEnd === model && v.ticksToEnd > 0;
      if (!ok) problems.push(`offset ${r.offset}: reservation read ${JSON.stringify(v)}, the model's ticksToEnd ${model}`);
    }
  }
  const firstObject = first.find((r) => r.reservation[0] !== undefined);
  if (first[0]?.reservation[0] !== undefined) problems.push(`the first reservation read was ${JSON.stringify(first[0]?.reservation[0])}, expected undefined`);
  if (!firstObject) problems.push('no reservation read gave an object');
  const ownerReads = (list) => list.filter((r) => r.owner).map((r) => [r.offset, r.owner]);
  const wantOwner1 = JSON.stringify(firstObject ? [[firstObject.offset, 1]] : []);
  if (JSON.stringify(ownerReads(first)) !== wantOwner1) problems.push(`owner reads in the first global ${JSON.stringify(ownerReads(first))}, expected ${wantOwner1}`);
  const wantOwner2 = JSON.stringify(second[0] ? [[second[0].offset, 1]] : []);
  if (JSON.stringify(ownerReads(second)) !== wantOwner2) problems.push(`owner reads after the halt ${JSON.stringify(ownerReads(second))}, expected ${wantOwner2}`);

  // 2. The console summary.
  const summaries = commandResults(world, SUMMARY);
  const head = (tick, placed) => `rustedscreep remote at tick ${tick}: on (max rooms ${REMOTE_CFG.maxRooms}, spawn ${REMOTE_CFG.spawnPermille} per mille) | sites placed ${placed}, refused 0`;
  const seenWant = `${head(T0 + SEEN_AT, 0)} | ${HOME}: ${REMOTE} seen 0 ago (2 sources) creeps s1 r1 m0 h1 (asked 0);`;
  if (summaries[0] !== seenWant) problems.push(`first summary ${JSON.stringify(summaries[0])}, expected ${JSON.stringify(seenWant)}`);
  const remoteCalls = state.siteCalls.filter((c) => c.room === REMOTE);
  const placedBefore = remoteCalls.filter((c) => c.offset <= (marks.summary ?? Infinity) && c.result === C.OK).length;
  const lastRead = state.reads[marks.summary]?.reservation[0];
  const minedWant = new RegExp(`^${esc(head(T0 + (marks.summary ?? 0), placedBefore))} \\| ${HOME}: ${REMOTE} seen 0 ago \\(2 sources\\) mined `
    + `\\[reserve(, roads)?, net -?\\d+/tick, cpu \\d+\\.\\d\\d, spawn \\d+ per mille\\] creeps s0 r1 m0 h1 \\(asked 0\\) reserved ${lastRead?.ticksToEnd};$`);
  if (summaries.length !== 2 || !minedWant.test(summaries[1] ?? '')) problems.push(`summaries ${JSON.stringify(summaries)}; the second should match ${minedWant}`);

  // 3. The scout's suicide, on the loop after the ways search.
  const suicides = state.calls.filter((c) => c.method === 'suicide');
  const reserverMoves = state.calls.filter((c) => c.actor === RESERVER && c.method === 'move');
  const waysAt = reserverMoves[0]?.offset;
  if (suicides.length !== 1 || suicides[0].actor !== SCOUT || suicides[0].argCount !== 0) {
    problems.push(`suicide calls ${JSON.stringify(suicides)}: expected one by ${SCOUT} with no argument`);
  } else if (suicides[0].offset !== waysAt) {
    problems.push(`the scout's suicide at offset ${suicides[0].offset}, the reserver's first step toward the controller at ${waysAt}: expected the same loop`);
  }
  if (state.calls.some((c) => c.actor === SCOUT && c.offset > (suicides[0]?.offset ?? Infinity))) problems.push('the scout called something after its suicide');

  // 4. The reserver's reserves.
  const reserves = state.calls.filter((c) => c.method === 'reserveController');
  for (const c of reserves) {
    const r = state.reads[c.offset];
    const fits = (r.ticksToEnd ?? 0) + CLAIM * C.CONTROLLER_RESERVE <= C.CONTROLLER_RESERVE_MAX;
    if (c.actor !== RESERVER || c.argCount !== 1 || !c.sameObject || range(c.from, REMOTE_CONTROLLER) > 1 || c.from.room !== REMOTE || !fits) {
      problems.push(`offset ${c.offset}: reserveController by ${c.actor} with ${c.argCount} argument(s) (this loop's controller: ${c.sameObject}) from ${at(c.from)}, ticksToEnd ${r.ticksToEnd}`);
    }
  }
  const adjacent = (r) => r.reserver && r.reserver.room === REMOTE && range(r.reserver, REMOTE_CONTROLLER) <= 1;
  const firstNext = first.find(adjacent);
  if (!firstNext || reserves[0]?.offset !== firstNext.offset) problems.push(`first reserve at offset ${reserves[0]?.offset}, first loop next to the controller ${firstNext?.offset}`);
  const window = first.filter((r) => r.offset >= (reserves[0]?.offset ?? Infinity));
  let atBoundary = 0;
  let atPinned = 0;
  for (const r of window) {
    const called = reserves.some((c) => c.offset === r.offset);
    const fits = (r.ticksToEnd ?? 0) + CLAIM * C.CONTROLLER_RESERVE <= C.CONTROLLER_RESERVE_MAX;
    if (!adjacent(r)) problems.push(`offset ${r.offset}: the reserver left the controller for ${JSON.stringify(r.reserver)}`);
    else if (called !== fits) problems.push(`offset ${r.offset}: ticksToEnd ${r.ticksToEnd}, reserve ${called ? 'called' : 'not called'}`);
    if (r.ticksToEnd === PINNED - 1 && called) atBoundary += 1;
    if (r.ticksToEnd === PINNED && !called) atPinned += 1;
  }
  if (atPinned < PIN || !atBoundary) problems.push(`loops held at ${PINNED} without a reserve ${atPinned} (expected at least ${PIN}), reserves at ${PINNED - 1} ${atBoundary} (expected at least 1)`);
  if (state.dropped.length) problems.push(`reserves the processor dropped (V264): ${JSON.stringify(state.dropped)}`);
  // After the reset the bot knows no ways until the placement searches them again, which needs movement's read of W2N1.
  const resumed = reserves.find((c) => c.offset > haltAt);
  if (!resumed) {
    const after = state.calls.filter((c) => c.offset > haltAt).map((c) => `${c.offset}:${c.method}`);
    problems.push(`no reserve in the ${second.length} loops after the halt (ticksToEnd ${second[0]?.ticksToEnd} to ${second.at(-1)?.ticksToEnd}; creep calls ${JSON.stringify(after)})`);
  } else {
    for (const r of second.filter((x) => x.offset >= resumed.offset)) {
      const called = reserves.some((c) => c.offset === r.offset);
      const fits = (r.ticksToEnd ?? 0) + CLAIM * C.CONTROLLER_RESERVE <= C.CONTROLLER_RESERVE_MAX;
      if (adjacent(r) && called !== fits) problems.push(`offset ${r.offset} (after the halt): ticksToEnd ${r.ticksToEnd}, reserve ${called ? 'called' : 'not called'}`);
    }
  }

  // 5. The remote room's sites.
  const containers = remoteCalls.filter((c) => c.type === 'container');
  const roadsPlaced = remoteCalls.filter((c) => c.type === 'road');
  const sourceOf = (c) => SOURCES.findIndex((s) => range(s, c) === 1);
  if (containers.length !== SOURCES.length || new Set(containers.map(sourceOf)).size !== SOURCES.length || containers.some((c) => sourceOf(c) < 0)) {
    problems.push(`container site calls ${JSON.stringify(containers.map((c) => `${c.offset}:${at(c)}`))}: expected one next to each source ${JSON.stringify(SOURCES.map(at))}`);
  }
  if (roadsPlaced.some((c) => c.offset < Math.max(...containers.map((x) => x.offset)))) problems.push('a road site came before the container sites');
  const tiles = new Set();
  const perLoop = new Map();
  for (const c of remoteCalls) {
    const key = `${c.x},${c.y}`;
    if (c.args !== 3 || !c.sameRoom || c.result !== C.OK || c.onSite || tiles.has(key) || c.open >= REMOTE_CFG.maxSites || !['container', 'road'].includes(c.type)) {
      problems.push(`offset ${c.offset}: createConstructionSite(${c.x}, ${c.y}, ${c.type}) with ${c.args} arguments on ${c.sameRoom ? "this loop's" : 'another'} room object, ${c.open} sites open${c.onSite ? ', on a site' : ''}: ${c.result}`);
    }
    tiles.add(key);
    perLoop.set(c.offset, (perLoop.get(c.offset) ?? 0) + 1);
  }
  for (const [o, n] of perLoop) if (n > 1) problems.push(`offset ${o}: ${n} W2N1 site calls in one loop`);
  const homeCalls = state.siteCalls.filter((c) => c.room === HOME);
  for (const c of homeCalls) if (c.result !== C.OK) problems.push(`offset ${c.offset}: W1N1 createConstructionSite(${c.x}, ${c.y}, ${c.type}): ${c.result}`);

  if (problems.length) throw new Error(`remote-sim:\n  ${problems.slice(0, 25).join('\n  ')}`);

  // The snapshot's W2N1 calls on a loop of the first global after the ways, by find type, on road ticks and others.
  const steady = first.filter((r) => r.offset > waysAt);
  const findsOf = (r) => Object.values(r.finds[REMOTE]).reduce((s, n) => s + n, 0);
  const spread = (list) => {
    const ns = list.map(findsOf);
    if (!ns.length) return 'none';
    const [lo, hi] = [Math.min(...ns), Math.max(...ns)];
    return lo === hi ? `${lo}` : `${lo} to ${hi}`;
  };
  const roadLoops = steady.filter((r) => (T0 + r.offset) % REMOTE_CFG.roadReadTicks === 0);
  const plain = steady.filter((r) => (T0 + r.offset) % REMOTE_CFG.roadReadTicks !== 0);
  return {
    ticks: world.ticksRun,
    suicideAt: suicides[0].offset,
    reserveAt: reserves[0].offset,
    standing: at(reserves[0].from),
    reserves: reserves.length,
    resumedAt: resumed.offset - haltAt,
    containers: containers.map((c) => at(c)),
    roads: roadsPlaced.map((c) => at(c)),
    homeSites: homeCalls.length,
    calls: `W2N1 finds per loop ${spread(plain)} (${spread(roadLoops)} on road ticks, with ${ROADS.length * 2} road reads), reservation 1, owner 1 a global`,
    summary: summaries[1],
  };
}

function esc(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

if (isMain(import.meta.url)) {
  try {
    const r = remoteSim(process.argv[2]);
    console.log(`remote-sim ok: ${r.ticks} ticks, 2 globals; scout suicide at offset ${r.suicideAt}; reserver at ${r.standing} from offset ${r.reserveAt}, `
      + `${r.reserves} reserves, none at ticksToEnd ${PINNED}, again ${r.resumedAt} ticks after the halt; W2N1 sites: containers ${r.containers.join(' ')}, roads ${r.roads.join(' ') || 'none'}; `
      + `W1N1 site calls ${r.homeSites}; snapshot: ${r.calls} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
