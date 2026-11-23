// Remote mining's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/remote.rs, snapshot.rs `push_remote` and `own_username`, and act.rs, over
// rustedscreep-core `economy::remotes`; P6.2, ADR-029: remote mining before P8's scouting, with each remote creep's
// remote room in its name; P6.3: the threats that send a remote room's creeps home). Two rooms:
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
// - a room is visible (in Game.rooms) while an own creep stands in it, the owned one always (V101); hostile creeps are
//   instances of the engine's global `Creep` class (screeps-game-api's `get_active_bodyparts` is a `final` binding:
//   `Creep.prototype.getActiveBodyparts.call(creep, type)`), whose method counts like the engine's (V269), and their
//   `owner` is `{username}` (V268); FIND_HOSTILE_CREEPS gives those in the room;
// - `PathFinder.search` is route-sim.mjs's model of the native search and `Game.map.getRoomStatus` says 'normal'; both
//   are recorded, and before the threat every remote creep's goal is in its own room, so neither may run then;
//   `spawnCreep` makes the runtime's checks and its `Memory.creeps` write, and the processor drops the order (V171,
//   V181, V173, as in planner-sim.mjs): no call may order other than a worker.
// It runs the bot until the reserver reserves, lets the reservation grow for GROW loops, holds ticksToEnd at PINNED
// (CONTROLLER_RESERVE_MAX - CLAIM × CONTROLLER_RESERVE + 1, 4999) for PIN loops, lets it run for RELEASE loops (it then
// alternates between PINNED - 1 and PINNED) and halts once (a global reset). In the second global, once the reserver
// reserves again (within AFTER ticks) it reserves RESUMED times; then remote.evacuate_ticks is set to EVACUATE_TICKS
// through the runtime config and an unarmed hostile (two MOVE parts) stands in W2N1 for UNARMED loops, with another in
// W1N1; then an armed one (a dead ATTACK part, then ATTACK, RANGED_ATTACK and two MOVE parts) stands beside it for ARMED
// loops, and all leave. The run ends RETURN_RESERVES reserves after the wait is over. It checks:
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
// 6. the threat (P6.3): `getActiveBodyparts` exactly twice a loop for each hostile W2N1 shows (ATTACK, then
//    RANGED_ATTACK), never for the one in W1N1; with only the unarmed one, reserves go on and nothing is logged; on the
//    first loop W2N1 shows the armed one, one warn line naming its owner and its live parts (1 ATTACK, 1 RANGED_ATTACK)
//    and the wait's end, and that hostile's `owner` read once, on that loop only; while threatened the reserver and the
//    hauler only move, and both reach W1N1 and wait within EVACUATE_RANGE (5) of its spawn, each crossing the edge once
//    each way; `rustedscreep.remote()` then shows ` threatened (1 armed) until T`, T being EVACUATE_TICKS after the last
//    loop W2N1 showed the armed hostile; on T one info line says the wait is over, and the reserver goes back and
//    reserves again by check 4's rule; no room is abandoned (one threat);
// and no fault line, no incident, no site refusal line, no remote spawn order, no route search before the threat and
// none incomplete, every creep call OK, one halt, two globals. It prints the snapshot's W2N1 calls per loop. npm run
// check runs it (row 23); standalone: node js/tools/remote-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CREEP_METHODS, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { CONFIG_SEGMENT, ENGINE_CONSTANTS, commandResults, configRecord, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
const T0 = 50000000;
const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
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
/** Ticks after the halt by which the reserver must reserve again: the boot, the warm-up loop (no Optional work), then the
 * Optional placement searches the ways again (a build of 12:13 today reserved again 4 ticks after the halt). */
const AFTER = 10;
/** P6.3's threat path, in the second global: RESUMED reserves after the reset, then UNARMED loops with an unarmed
 * hostile in W2N1 (and one in W1N1), then ARMED loops with an armed one beside it, then none. */
const RESUMED = 3;
const UNARMED = 5;
const ARMED = 3;
/** remote.evacuate_ticks for the run (REMOTE_MMO: 1500), set through the runtime config before the hostiles come: long
 * enough for the reserver's walk home (about 60 ticks here) and a wait there. */
const EVACUATE_TICKS = 90;
const EVACUATE_CMD = `rustedscreep.configSet("remote.evacuate_ticks", "${EVACUATE_TICKS}")`;
/** economy::remotes EVACUATE_RANGE: an evacuated creep waits within this range of the home's first spawn. */
const EVACUATE_RANGE = 5;
/** Reserves after the creeps came back that end the run, or the ticks after the wait that end it without them. */
const RETURN_RESERVES = 3;
const RETURN_MAX = 120;
const RUN = 600;
/** The hostiles' owner: user '2''s name is the server's (V272), so any name not the bot's; this one is the usual. */
const INVADER = 'Invader';
/** Hostile creeps, by role: [part, hits] bodies. The armed one's first ATTACK part is dead, so getActiveBodyparts (V269:
 * from the last part back to the first dead one) gives 1 ATTACK and 1 RANGED_ATTACK; the others have neither. */
const HOSTILES = {
  unarmed: { id: id(70), name: 'Invader70', room: REMOTE, x: 20, y: 44, body: [['move', 100], ['move', 100]] },
  armed: { id: id(71), name: 'Invader71', room: REMOTE, x: 24, y: 44, body: [['attack', 0], ['attack', 100], ['ranged_attack', 100], ['move', 100], ['move', 100]] },
  home: { id: id(72), name: 'Invader72', room: HOME, x: 45, y: 5, body: [['move', 100]] },
};
const ARMED_PARTS = { attack: 1, ranged_attack: 1 };
/** The threat's log lines (crates/rustedscreep-wasm/src/remote.rs `log_events`). */
const THREAT_LINE = `rustedscreep remote: ${REMOTE} (of ${HOME}) threatened by `;
const RESUMED_LINE = `rustedscreep remote: ${REMOTE} (of ${HOME}): the wait is over, its creeps go back`;
const SEEN_AT = 1;
const SUMMARY = 'rustedscreep.remote()';
const HALT = 'Game.cpu.halt()';

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

/**
 * The engine's global `Creep` class, as far as the bindings use it: screeps-game-api's `get_active_bodyparts` is a
 * `final` binding, so the glue calls `Creep.prototype.getActiveBodyparts.call(creep, type)` (the engine registers the
 * class as a global; game/creeps.js). Its method is the engine's (V269): from the last body part back, stopping at the
 * first one with no hits, the parts of the type. `__counted(type)` lets the sim count the calls per creep.
 */
class EngineCreep {
  getActiveBodyparts(type) {
    this.__counted?.(type);
    let n = 0;
    for (let i = this.body.length - 1; i >= 0; i--) {
      if (this.body[i].hits <= 0) break;
      if (this.body[i].type === type) n += 1;
    }
    return n;
  }
}

/** The engine's RoomPosition, as far as the crate and a PathFinder call would use it (V103). */
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
 * A model of the native PathFinder, as in route-sim.mjs (driver lib/path-finder.js and native/src/pf.cc; V163 to V166):
 * the driver's option defaults and clamps; a room is indexed once per search, its terrain checked first (a room without
 * terrain throws 'Could not load terrain data') and then roomCallback asked (false blocks it, an object with a 2500-byte
 * `_bits` is its matrix, anything else leaves the terrain); a matrix byte other than 0 is the cost (255 an obstacle), 0
 * the terrain (walls obstacles); moves from an edge tile go straight across or back in, never along it; A* by world
 * Chebyshev range, one op per expansion; the path leaves the origin out. Each search is recorded in `state.searches`.
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
    /** The hostile creeps standing this tick ({ id, name, room, x, y, body }), and the log length after each loop. */
    hostiles: [],
    logMarks: [],
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
  const hauler = state.creeps.find((c) => c.name === HAULER);
  // A room is visible while an own creep stands in it, and the owned one always (V101).
  const visible = new Set([HOME, ...state.creeps.map((c) => c.room)]);
  const shown = state.hostiles.filter((h) => visible.has(h.room));
  const r = {
    offset, snapshots: 0, reservation: [], homeReservation: 0, owner: 0, roadHits: 0, roadHitsMax: 0, finds: { [HOME]: {}, [REMOTE]: {} },
    ticksToEnd: reservationOf(state, time)?.ticksToEnd ?? null,
    reserver: reserver ? { room: reserver.room, x: reserver.x, y: reserver.y } : null,
    hauler: hauler ? { room: hauler.room, x: hauler.x, y: hauler.y } : null,
    remoteSites: state.sites.filter((s) => s.room === REMOTE).length,
    visible: visible.has(REMOTE),
    haulerIn: hauler?.room === REMOTE,
    hostiles: shown.map((h) => ({ id: h.id, room: h.room })),
    bodyCalls: {},
    hostileOwner: {},
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

  // Hostile creeps (V101, V268, V269): `owner` and `getActiveBodyparts` are counted per creep.
  const hostiles = shown.map((h) => Object.create(EngineCreep.prototype, Object.getOwnPropertyDescriptors({
    id: h.id, name: h.name, my: false, spawning: false, fatigue: 0, pos: pos(h.room, h.x, h.y),
    body: h.body.map(([type, hits]) => ({ type, hits })), hits: h.body.reduce((n, [, hits]) => n + hits, 0), hitsMax: 100 * h.body.length,
    get owner() {
      r.hostileOwner[h.id] = (r.hostileOwner[h.id] ?? 0) + 1;
      return { username: INVADER };
    },
    __counted(type) {
      (r.bodyCalls[h.id] ??= []).push(type);
    },
  })));
  const siteObject = (s) => ({
    id: s.id, my: true, owner: { username: USER }, structureType: s.type, progress: 0, progressTotal: C.CONSTRUCTION_COST[s.type],
    pos: pos(s.room, s.x, s.y),
  });
  const tombstones = state.tombstones.map((t) => ({
    id: t.id, pos: pos(t.room, t.x, t.y), deathTime: t.deathTime, ticksToDecay: t.decayTime - time, store: makeStore({}),
    creep: { id: t.creepId, name: t.name, my: true },
  }));
  const rooms = {};
  for (const name of [HOME, REMOTE].filter((n) => visible.has(n))) {
    const inRoom = (o) => o.pos.roomName === name;
    const sites = state.sites.filter((s) => s.room === name).map(siteObject);
    const mine = creeps.filter(inRoom);
    const foes = hostiles.filter(inRoom);
    const lists = name === HOME
      ? {
        [C.FIND_STRUCTURES]: homeStructures, [C.FIND_MY_STRUCTURES]: homeStructures, [C.FIND_MY_SPAWNS]: [spawn],
      }
      : {
        [C.FIND_SOURCES]: sources, [C.FIND_SOURCES_ACTIVE]: sources, [C.FIND_MINERALS]: [mineral], [C.FIND_STRUCTURES]: [remoteController, ...roads],
        [C.FIND_TOMBSTONES]: tombstones.filter(inRoom),
      };
    Object.assign(lists, {
      [C.FIND_CONSTRUCTION_SITES]: sites, [C.FIND_MY_CONSTRUCTION_SITES]: sites, [C.FIND_CREEPS]: [...mine, ...foes], [C.FIND_MY_CREEPS]: mine,
      [C.FIND_HOSTILE_CREEPS]: foes,
    });
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
      const stays = state.creeps.some((o) => o !== c && o.room === c.room && o.x === d.x && o.y === d.y && !moves.has(o))
        || state.hostiles.some((o) => o.room === c.room && o.x === d.x && o.y === d.y);
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
    if (!TERRAIN[next]) {
      state.errors.push(`offset ${offset}: ${c.name} ended the tick on ${c.room} ${at(c)}, an exit to ${next}, which this world has not`);
      continue;
    }
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
    setup: (context, w) => {
      // The controller signer (ADR-030) is off for this run: its spawn requests and route searches are not remote
      // mining's, and sign-sim.mjs covers them. The record is in segment CONFIG_SEGMENT before the first global.
      w.segments[CONFIG_SEGMENT] ??= configRecord(['sign.enabled=false']);
      state.context = context;
      context.Creep = EngineCreep;
      context.PathFinder = makePathFinder(state, definePositions(context));
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      // This loop's log lines are those after the previous loop's mark (and that loop's command and its reply).
      state.logMarks[offset] = w.logs.length;
      const lines = w.logs.slice(state.logMarks[offset - 1] ?? 0);
      advance(state, offset);
      let cmd = null;
      const home = (c) => c && c.room === HOME && range(c, SPAWN) <= EVACUATE_RANGE;
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
      } else if (phase === 'halted') {
        // The second global: once the reserver reserves again, the hostiles come (first the unarmed ones).
        const back = state.calls.find((c) => c.method === 'reserveController' && c.offset > marks.halt);
        if (back && offset >= back.offset + RESUMED - 1) {
          phase = 'unarmed';
          marks.unarmedFrom = offset + 1;
          state.hostiles = [{ ...HOSTILES.unarmed }, { ...HOSTILES.home }];
          cmd = EVACUATE_CMD;
        } else if (offset >= marks.halt + AFTER) {
          phase = 'stuck';
        }
      } else if (phase === 'unarmed' && offset >= marks.unarmedFrom + UNARMED - 1) {
        phase = 'armed';
        marks.armedFrom = offset + 1;
        state.hostiles.push({ ...HOSTILES.armed });
      } else if (phase === 'armed' && offset >= marks.armedFrom + ARMED - 1) {
        phase = 'evacuating';
        marks.armedTo = offset;
        state.hostiles = [];
      } else if (phase === 'evacuating') {
        // The summary once both wait at the home's spawn, or (so a check can say why) just before the wait would end.
        const both = home(state.creeps.find((c) => c.name === RESERVER)) && home(state.creeps.find((c) => c.name === HAULER));
        if (both || offset >= marks.armedTo + EVACUATE_TICKS - 2) {
          phase = 'waiting';
          marks.waitSummary = offset;
          cmd = SUMMARY;
        }
      } else if (phase === 'waiting' && (lines.some((l) => l.includes(RESUMED_LINE)) || offset >= marks.armedTo + EVACUATE_TICKS + 5)) {
        phase = 'returning';
        marks.resumed = offset;
      } else if (phase === 'returning'
        && (state.calls.filter((c) => c.method === 'reserveController' && c.offset > marks.resumed).length >= RETURN_RESERVES
          || offset >= marks.resumed + RETURN_MAX)) {
        finished = true;
      }
      return offset === SEEN_AT ? SUMMARY : cmd;
    },
    until: () => finished,
  });

  const problems = [...state.errors.slice(0, 10)];
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|rustedscreep remote: no /.test(l));
  problems.push(...bad.slice(0, 10).map((l) => `log: ${l}`));
  if (!finished || phase !== 'returning') problems.push(`not finished in ${world.ticksRun} ticks (phase ${phase}; marks ${JSON.stringify(marks)})`);
  if (world.halts !== 1 || world.globals !== 2) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 1 and 2`);
  // With no hauling plan the remote manager orders no creep; the opening's worker orders are its own.
  const remoteOrders = state.spawnCalls.filter((c) => !/^w[WE]\d+[NS]\d+-/.test(c.name ?? ''));
  if (remoteOrders.length) problems.push(`spawnCreep calls for other than a worker: ${JSON.stringify(remoteOrders.slice(0, 4))}`);
  for (const c of state.spawnCalls) if (c.result !== C.OK || c.args !== 2) problems.push(`offset ${c.offset}: spawnCreep(${c.name}) with ${c.args} arguments: ${c.result}`);
  // This loop's log lines, and the threat's start and end.
  const linesOf = (o) => world.logs.slice(state.logMarks[o - 1] ?? 0, state.logMarks[o] ?? world.logs.length);
  const linesAt = (text) => state.logMarks.map((_, o) => o).filter((o) => linesOf(o).some((l) => l.includes(text)));
  const threatAt = linesAt(THREAT_LINE);
  const resumedAt = linesAt(RESUMED_LINE);
  const threatFrom = threatAt[0] ?? Infinity;
  const threatTo = resumedAt[0] ?? Infinity;
  const threatened = (o) => o >= threatFrom && o < threatTo;
  // Route searches only for the walks home and back: before the threat every remote creep's goal is in its own room.
  const early = [...state.searches.filter((x) => x.offset < threatFrom), ...state.statusCalls.filter(([o]) => o < threatFrom)];
  if (early.length) problems.push(`route searches or room-status reads before the threat: ${JSON.stringify(early.slice(0, 4))}`);
  const bad1 = state.searches.filter((x) => x.incomplete !== false);
  if (bad1.length) problems.push(`incomplete route searches: ${JSON.stringify(bad1.slice(0, 3))}`);
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
    // W2N1 is read as a remote room on a loop it is visible (an own creep stands in it); its roads only where an own
    // remote hauler stands, on the road ticks.
    const loop = r.snapshots > 0 && r.visible;
    const roadTick = loop && r.haulerIn && (T0 + r.offset) % REMOTE_CFG.roadReadTicks === 0;
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
  if (summaries.length !== 3 || !minedWant.test(summaries[1] ?? '')) problems.push(`summaries ${JSON.stringify(summaries.slice(0, 2))}; the second should match ${minedWant}`);

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
    // The same rule after the halt, outside the threat (check 6 covers it).
    for (const r of second.filter((x) => x.offset >= resumed.offset && !threatened(x.offset))) {
      const called = reserves.some((c) => c.offset === r.offset);
      const fits = (r.ticksToEnd ?? 0) + CLAIM * C.CONTROLLER_RESERVE <= C.CONTROLLER_RESERVE_MAX;
      if (adjacent(r) && called !== fits) problems.push(`offset ${r.offset} (after the halt): ticksToEnd ${r.ticksToEnd}, reserve ${called ? 'called' : 'not called'}`);
    }
  }

  // 6. The threat (P6.3): unarmed hostiles change nothing; an armed one sends the room's creeps home until the wait ends.
  const configReply = commandResults(world, EVACUATE_CMD);
  const configWant = `rustedscreep config: remote.evacuate_ticks = ${EVACUATE_TICKS} from the next tick (profile mmo: 1500); stored on the next tick that runs the Standard tier`;
  if (configReply.length !== 1 || configReply[0] !== configWant) problems.push(`configSet reply ${JSON.stringify(configReply)}, expected ${JSON.stringify(configWant)}`);
  const { unarmedFrom, armedFrom, armedTo } = marks;
  // getActiveBodyparts: ATTACK then RANGED_ATTACK once each for every hostile W2N1 shows, on every loop; never in W1N1.
  for (const r of loops) {
    for (const h of r.hostiles) {
      const got = JSON.stringify(r.bodyCalls[h.id] ?? []);
      const want = JSON.stringify(h.room === REMOTE ? ['attack', 'ranged_attack'] : []);
      if (got !== want) problems.push(`offset ${r.offset}: getActiveBodyparts calls ${got} on ${h.id} in ${h.room}, expected ${want}`);
    }
  }
  const unarmedLoops = loops.filter((r) => r.offset >= unarmedFrom && r.offset < armedFrom && r.hostiles.length);
  if (unarmedLoops.length !== UNARMED) problems.push(`${unarmedLoops.length} loops with only unarmed hostiles, expected ${UNARMED}`);
  const unarmedReserves = reserves.filter((c) => c.offset >= unarmedFrom && c.offset < armedFrom);
  if (!unarmedReserves.length) problems.push('no reserve while only unarmed hostiles stood in W2N1');
  // One warn line, on the first loop W2N1 shows the armed hostile, with its owner and its live parts.
  const threatLines = world.logs.filter((l) => l.includes(THREAT_LINE));
  const threatWant = `${THREAT_LINE}1 armed hostile creep(s) of ${INVADER} (${ARMED_PARTS.attack} ATTACK, ${ARMED_PARTS.ranged_attack} RANGED_ATTACK parts); `
    + `its creeps wait in ${HOME} until tick ${T0 + armedFrom + EVACUATE_TICKS}`;
  if (threatLines.length !== 1 || !threatLines[0].endsWith(threatWant) || threatFrom !== armedFrom) {
    problems.push(`threat lines ${JSON.stringify(threatLines)} at offsets ${JSON.stringify(threatAt)}, expected one ending ${JSON.stringify(threatWant)} at ${armedFrom}`);
  }
  // `owner` of a hostile: the armed one's, once, on that loop.
  const hostileOwner = loops.flatMap((r) => Object.entries(r.hostileOwner).map(([hid, n]) => [r.offset, hid, n]));
  const ownerWant = JSON.stringify([[armedFrom, HOSTILES.armed.id, 1]]);
  if (JSON.stringify(hostileOwner) !== ownerWant) problems.push(`hostile owner reads ${JSON.stringify(hostileOwner)}, expected ${ownerWant}`);
  // While threatened the reserver and the hauler only move, and both reach the home's spawn; the wait ends EVACUATE_TICKS
  // after the last sight of the armed hostile, with one info line.
  const working = state.calls.filter((c) => threatened(c.offset) && (c.actor === RESERVER || c.actor === HAULER) && c.method !== 'move');
  if (working.length) problems.push(`calls while threatened: ${JSON.stringify(working.slice(0, 4).map((c) => `${c.offset}:${c.actor}:${c.method}`))}`);
  const waited = loops.find((r) => threatened(r.offset) && [r.reserver, r.hauler].every((c) => c && c.room === HOME && range(c, SPAWN) <= EVACUATE_RANGE));
  if (!waited) problems.push(`the reserver and the hauler never both waited within ${EVACUATE_RANGE} of ${HOME}'s spawn while threatened`);
  // Each crosses twice: home, and back once the wait is over. A creep left on an edge tile is moved to the next room
  // at the end of the tick (V151), so one that holds on the home's edge goes back into the threatened room.
  for (const name of [RESERVER, HAULER]) {
    const rooms = state.creeps.find((c) => c.name === name)?.rooms ?? [];
    if (JSON.stringify(rooms) !== JSON.stringify([REMOTE, HOME, REMOTE])) {
      const shown = rooms.length > 8 ? `${rooms.slice(0, 6).join(' → ')} → … (${rooms.length - 1} crossings)` : rooms.join(' → ');
      problems.push(`${name} went ${shown}, expected ${REMOTE} → ${HOME} → ${REMOTE}`);
    }
  }
  const resumedLines = world.logs.filter((l) => l.includes(RESUMED_LINE));
  if (resumedLines.length !== 1 || threatTo !== armedTo + EVACUATE_TICKS) {
    problems.push(`resumed lines ${resumedLines.length} at offsets ${JSON.stringify(resumedAt)}, expected one at ${armedTo + EVACUATE_TICKS} (the last sight ${armedTo} + ${EVACUATE_TICKS})`);
  }
  // The summary's site counts are this global's.
  const placedSecond = remoteCalls.filter((c) => c.offset > haltAt && c.offset <= (marks.waitSummary ?? Infinity) && c.result === C.OK).length;
  const waitWant = new RegExp(`^${esc(head(T0 + (marks.waitSummary ?? 0), placedSecond))} \\| ${HOME}: ${REMOTE} seen \\d+ ago \\(2 sources\\) mined `
    + `threatened \\(1 armed\\) until ${T0 + armedTo + EVACUATE_TICKS} \\[reserve(, roads)?, net -?\\d+/tick, cpu \\d+\\.\\d\\d, spawn \\d+ per mille\\] `
    + 'creeps s0 r1 m0 h1 \\(asked 0\\) reserved \\d+;$');
  if (!waitWant.test(summaries[2] ?? '')) problems.push(`summary while waiting ${JSON.stringify(summaries[2])}, expected to match ${waitWant}`);
  const after = reserves.filter((c) => c.offset >= threatTo);
  if (after.length < RETURN_RESERVES) problems.push(`${after.length} reserves after the wait, expected at least ${RETURN_RESERVES}`);
  if (world.logs.some((l) => l.includes(' abandoned ('))) problems.push('a room was abandoned after one threat');

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
    threat: { from: threatFrom, waited: waited.offset, to: threatTo, back: after[0].offset, searches: state.searches.length },
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
      + `W1N1 site calls ${r.homeSites}; threat from offset ${r.threat.from} (both home by ${r.threat.waited}), over at ${r.threat.to}, `
      + `reserving again at ${r.threat.back}, ${r.threat.searches} route searches; snapshot: ${r.calls} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
