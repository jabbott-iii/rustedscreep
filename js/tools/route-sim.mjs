// Inter-room routes end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/route.rs and movement.rs over rustedscreep-core `movement::route`). Four rooms:
// W1N1 (owned) with W2N1 to its west and W1N2 to its north, and W2N2 north of W2N1 and west of W1N2. Each pair of
// neighbours shares an exit of 11 tiles; the corners are walls. W2N1 is all swamp, so the cheap way to W2N2 runs
// through W1N2, whose Game.map.getRoomStatus is 'novice' and which the engine model refuses creeps into (novice
// areas are walled off; V168). W2N2 also has an open west edge onto a room the world has no terrain for. It checks:
// - `rustedscreep.moveTo("W2N2", 40, 25, 1, "W1N1")` takes both creeps of W1N1 to W2N2 through W2N1, with
//   `move(1..8)` calls only, which the harness resolves like the engine (V51, V52) and which all succeed; a creep
//   that ends a tick on an edge tile is on the tile across in the next room on the next tick (V151), and none is ever
//   refused (the route never heads for W1N2) or left on an edge;
// - one PathFinder.search serves both creeps and both rooms, with the bot's costs and limits (plainCost 2,
//   swampCost 10, maxOps 4000, maxRooms 16, range 1); its roomCallback answers W1N1 with a CostMatrix of the bot's
//   grid (open exits 0, their terrain cost; plain 2; walls and the controller 255), W2N1 with the shared matrix for rooms without a grid
//   (0 but the corners), and W1N2 with false; getRoomStatus is read once per room;
// - the order ends (one log line) once both creeps are in range;
// - a second order, to W3N2, makes the next search reach the room without terrain, which throws in the native
//   PathFinder (V165): the bot catches it (no trap, no fault), the creeps hold, each creep searches once and keeps its
//   failure (a failure is never shared: it says nothing about another creep's tile), and `rustedscreep.movement()`
//   counts the searches that threw.
// The P2.4 opening reads the owned W1N1 every loop (its finds, the controller's getters, the creeps' stores; engine-
// shaped here, V189, V196, V199) but stays inert: W1N1 has no source and the creeps no bot worker name, and with no
// spawn there its extension placement (ADR-020) has nothing to place around, so no createConstructionSite call.
// npm run check runs it (row 17); standalone: node js/tools/route-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CARRY_CAPACITY, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { commandResults, simulate } from './smoke.mjs';

// @screeps/common 2.16.0 constants (the fixture the constants self-check reads).
const FIND = { HOSTILE_CREEPS: 103, SOURCES: 105, STRUCTURES: 107, MY_CONSTRUCTION_SITES: 114, MINERALS: 116, DEPOSITS: 122 };
const TERRAIN_MASK_WALL = 1;
const TERRAIN_MASK_SWAMP = 2;
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
/** MOVEMENT_MMO (crates/rustedscreep-core/src/config.rs): plain and swamp costs, route_max_ops, route_max_rooms. */
const COSTS = { road: 1, plain: 2, swamp: 10, maxOps: 4000, maxRooms: 16 };
const HOME = 'W1N1';
const TARGET = { room: 'W2N2', x: 40, y: 25, range: 1 };
const NOWHERE = { room: 'W3N2', x: 25, y: 25, range: 1 };
const ORDER_AT = 2;
const TICKS = 170;
const ORDER_CMD = `rustedscreep.moveTo("${TARGET.room}", ${TARGET.x}, ${TARGET.y}, ${TARGET.range}, "${HOME}")`;
const THROW_CMD = `rustedscreep.moveTo("${NOWHERE.room}", ${NOWHERE.x}, ${NOWHERE.y}, ${NOWHERE.range}, "${TARGET.room}")`;
const SUMMARY_CMD = 'rustedscreep.movement()';
const SUMMARY_AFTER = 6;

const id = (n) => `5bbcaa9099f9b0a0f7f4d${n.toString(16).padStart(3, '0')}`;

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

/** Row-major terrain (V156): wall edges except the open sides' tiles 20..30, corners always walls. */
function terrain(open, fill = 0) {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) {
      const gap = (v) => v >= 20 && v <= 30;
      const side = (x === 0 && open.includes('W') && gap(y)) || (x === 49 && open.includes('E') && gap(y))
        || (y === 0 && open.includes('N') && gap(x)) || (y === 49 && open.includes('S') && gap(x));
      const edge = x === 0 || y === 0 || x === 49 || y === 49;
      raw[y * 50 + x] = edge && !side ? TERRAIN_MASK_WALL : edge ? 0 : fill;
    }
  }
  return raw;
}

const TERRAIN = {
  W1N1: terrain(['W', 'N']),
  W2N1: terrain(['E', 'N'], TERRAIN_MASK_SWAMP),
  W1N2: terrain(['S', 'W']),
  W2N2: terrain(['S', 'E', 'W']), // the west edge leads to W3N2, which has no terrain data
};
const STATUS = { W1N2: 'novice' };
/** CONTROLLER_LEVELS[2] and CONTROLLER_DOWNGRADE[2] (@screeps/common 2.16.0). */
const PROGRESS_TOTAL_2 = 45000;
const DOWNGRADE_2 = 10000;
const controller = {
  id: id(1), structureType: 'controller', my: true, level: 2, pos: { x: 40, y: 40, roomName: HOME, __packedPos: packPos(HOME, 40, 40) },
  // The getters of an owned controller (V189): no progress yet, the level's total, a full downgrade timer.
  progress: 0, progressTotal: PROGRESS_TOTAL_2, ticksToDowngrade: DOWNGRADE_2,
};

function blocked(room, x, y) {
  if (!TERRAIN[room]) return 'no such room';
  if (TERRAIN[room][y * 50 + x] & TERRAIN_MASK_WALL) return 'wall';
  if (room === HOME && x === 40 && y === 40) return 'controller';
  return null;
}

/** The engine's RoomPosition, as far as the crate and the PathFinder model use it (V103). */
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
 * A model of the native PathFinder (driver lib/path-finder.js and native/src/pf.cc; V163 to V166): the driver's option
 * defaults and clamps; a room is indexed once per search, its terrain checked first (a room without terrain throws
 * 'Could not load terrain data') and then roomCallback asked (false blocks it, an object with a 2500-byte `_bits` is its
 * matrix, anything else leaves the terrain); a full room table (maxRooms) blocks further rooms; a matrix byte other
 * than 0 is the cost (255 an obstacle), 0 the terrain (walls obstacles); moves from an edge tile go straight across or
 * back in, never along it; A* by world Chebyshev range, one op per expansion; the path leaves the origin out.
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
    const record = { tick: state.offset, from, to, range, plainCost, swampCost, maxOps, maxRooms, callbacks: [] };
    state.searches.push(record);
    const table = new Map();
    const blockedRooms = new Set();
    const room = (name) => {
      if (table.has(name)) return table.get(name);
      if (blockedRooms.has(name) || table.size >= maxRooms) return null;
      if (!TERRAIN[name]) throw new Error('Could not load terrain data');
      const ret = typeof options.roomCallback === 'function' ? options.roomCallback(name) : undefined;
      record.callbacks.push([name, ret === false ? false : ret && ret._bits ? Array.from(ret._bits) : ret]);
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
      if (t & TERRAIN_MASK_WALL) return Infinity;
      return t & TERRAIN_MASK_SWAMP ? swampCost : plainCost;
    };
    const [gx, gy] = world(to);
    const h = (wx, wy) => Math.max(0, Math.max(Math.abs(wx - gx), Math.abs(wy - gy)) - range);
    const weight = Math.min(9, Math.max(1, options.heuristicWeight || 1.2));
    const [ox, oy] = world(from);
    if (h(ox, oy) === 0) return { path: [], ops: 0, cost: 0, incomplete: false };
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
    record.ops = ops;
    record.incomplete = best[0] !== 0;
    record.path = path.map((p) => [p.roomName, p.x, p.y]);
    return { path, ops, cost: g.get(key(best[1], best[2])), incomplete: best[0] !== 0 };
  }
  return { CostMatrix, search };
}

function makeWorld() {
  const creeps = [
    { id: id(100), name: 'A', room: HOME, x: 25, y: 25, rooms: [HOME] },
    { id: id(101), name: 'B', room: HOME, x: 30, y: 30, rooms: [HOME] },
  ];
  return { creeps, pending: [], calls: [], errors: [], searches: [], statusCalls: [], refused: [], offset: 0 };
}

function sceneFor(state, offset) {
  state.offset = offset;
  const own = state.creeps.map((c) => ({
    id: c.id, name: c.name, my: true, spawning: false, fatigue: 0, owner: { username: 'bot' },
    pos: { x: c.x, y: c.y, roomName: c.room, __packedPos: packPos(c.room, c.x, c.y) },
    store: makeStore({}, { capacity: CARRY_CAPACITY }),
    move(direction) {
      state.calls.push([offset, c.name, 'move', direction]);
      state.pending.push([c, direction]);
      return 0;
    },
    say() { return 0; },
  }));
  const visible = new Set([HOME, ...state.creeps.map((c) => c.room)]);
  const rooms = {};
  for (const name of visible) {
    rooms[name] = {
      name,
      controller: name === HOME ? controller : undefined,
      find(type, opts) {
        return findFiltered(type === FIND.STRUCTURES && name === HOME ? [controller] : [], opts);
      },
      getTerrain() { return { getRawBuffer: () => TERRAIN[name].slice() }; },
    };
  }
  return {
    creeps: Object.fromEntries(own.map((c) => [c.name, c])),
    rooms,
    structures: { [controller.id]: controller },
    map: {
      getRoomStatus(name) {
        state.statusCalls.push([offset, name]);
        return { status: STATUS[name] ?? 'normal', timestamp: STATUS[name] ? 1e13 : null };
      },
    },
  };
}

/** The engine's move resolution for this tick's calls (V51, V52), then the room transfers (V151, V152). */
function applyMoves(state, offset) {
  const moves = new Map();
  for (const [c, direction] of state.pending) {
    if (!Number.isInteger(direction) || direction < 1 || direction > 8) {
      state.errors.push(`offset ${offset}: ${c.name} moved with ${direction}`);
      continue;
    }
    if (moves.has(c)) state.errors.push(`offset ${offset}: ${c.name} got two moves`);
    const [dx, dy] = OFFSETS[direction];
    moves.set(c, { room: c.room, x: c.x + dx, y: c.y + dy });
  }
  state.pending = [];
  let changed = true;
  while (changed) {
    changed = false;
    for (const [c, d] of moves) {
      const same = (o, e) => e.room === d.room && e.x === d.x && e.y === d.y;
      const others = [...moves].filter(([o, e]) => o !== c && same(o, e));
      const stays = state.creeps.some((o) => o !== c && same(o, o) && !moves.has(o));
      const why = d.x < 0 || d.y < 0 || d.x > 49 || d.y > 49 ? 'off the room' : blocked(d.room, d.x, d.y);
      if (why || stays || others.length) {
        state.errors.push(`offset ${offset}: ${c.name}'s move to ${d.room} (${d.x}, ${d.y}) fails (${why ?? 'taken'})`);
        moves.delete(c);
        changed = true;
      }
    }
  }
  for (const [c, d] of moves) {
    c.x = d.x;
    c.y = d.y;
  }
  for (const c of state.creeps) {
    const edge = c.x === 0 || c.y === 0 || c.x === 49 || c.y === 49;
    if (!edge) continue;
    const [rx, ry] = roomXY(c.room);
    const next = c.x === 0 ? roomAt(rx - 1, ry) : c.x === 49 ? roomAt(rx + 1, ry) : c.y === 0 ? roomAt(rx, ry - 1) : roomAt(rx, ry + 1);
    if (STATUS[next] || !TERRAIN[next]) {
      state.refused.push([offset, c.name, c.room, c.x, c.y, next]);
      continue;
    }
    if (c.x === 0) c.x = 49; else if (c.x === 49) c.x = 0; else if (c.y === 0) c.y = 49; else c.y = 0;
    c.room = next;
    c.rooms.push(next);
  }
}

export function routeSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let phase = 'walking';
  let doneAt = null;
  let throwAt = null;
  const world = simulate(dir, {
    ticks: TICKS,
    scene: (offset) => sceneFor(state, offset),
    setup: (context) => {
      const RoomPosition = definePositions(context);
      context.PathFinder = makePathFinder(state, RoomPosition);
    },
    command: (w) => {
      const offset = w.ticksRun;
      applyMoves(state, offset);
      if (offset === ORDER_AT) return ORDER_CMD;
      if (phase === 'walking' && w.logs.some((l) => l.startsWith('rustedscreep moveTo: all'))) {
        phase = 'throwing';
        doneAt = offset;
        throwAt = offset;
        state.positionsAtThrow = state.creeps.map((c) => [c.room, c.x, c.y]);
        return THROW_CMD;
      }
      if (phase === 'throwing' && offset === throwAt + SUMMARY_AFTER) {
        phase = 'summary';
        return SUMMARY_CMD;
      }
      return null;
    },
  });
  const problems = [...state.errors];
  const warn = world.logs.filter((l) => l.includes('fault') || l.includes('[error]') || l.includes('trapped'));
  problems.push(...warn.map((l) => `log: ${l}`));
  if (world.halts) problems.push(`${world.halts} halt(s)`);
  if (state.refused.length) problems.push(`transfers refused or left on an edge: ${JSON.stringify(state.refused.slice(0, 4))}`);
  const opening = world.logs.filter((l) => l.startsWith('rustedscreep opening:'));
  if (opening.length) problems.push(`opening lines (it should stay inert): ${opening.join(' / ')}`);

  // The route: W1N1, then the swamp of W2N1, then W2N2; never the novice W1N2.
  for (const c of state.creeps) {
    if (JSON.stringify(c.rooms) !== JSON.stringify([HOME, 'W2N1', 'W2N2'])) problems.push(`${c.name} went through ${c.rooms.join(' → ')}`);
  }
  const accepted = commandResults(world, ORDER_CMD);
  if (accepted.length !== 1 || !accepted[0].includes(`own creeps in ${HOME} head for (${TARGET.x}, ${TARGET.y}) in ${TARGET.room} within ${TARGET.range}`)) {
    problems.push(`moveTo reply: ${accepted.join(' / ')}`);
  }
  const done = world.logs.filter((l) => l.startsWith('rustedscreep moveTo: all'));
  if (done.length !== 1 || !done[0].includes(`all 2 creep(s) in ${TARGET.room} are within ${TARGET.range} of (${TARGET.x}, ${TARGET.y}); order done`)) {
    problems.push(`order completion: ${done.join(' / ') || 'none'}`);
  }

  // One search for both creeps and both rooms, with the bot's costs and limits.
  const first = state.searches.filter((s) => doneAt === null || s.tick <= doneAt);
  if (first.length !== 1) problems.push(`${first.length} searches before the order ended, expected 1`);
  const s = first[0];
  if (s) {
    const want = { plainCost: COSTS.plain, swampCost: COSTS.swamp, maxOps: COSTS.maxOps, maxRooms: COSTS.maxRooms, range: TARGET.range };
    for (const [k, v] of Object.entries(want)) if (s[k] !== v) problems.push(`search ${k} ${s[k]}, expected ${v}`);
    if (s.from.roomName !== HOME || s.to.roomName !== TARGET.room || s.to.x !== TARGET.x || s.to.y !== TARGET.y) {
      problems.push(`search from ${JSON.stringify(s.from)} to ${JSON.stringify(s.to)}`);
    }
    if (s.incomplete) problems.push('the first search was incomplete');
    const answers = Object.fromEntries(s.callbacks);
    const home = answers[HOME];
    const at = (x, y) => home?.[x * 50 + y];
    if (!Array.isArray(home) || home.length !== 2500) problems.push(`roomCallback(${HOME}) gave ${JSON.stringify(home)?.slice(0, 40)}`);
    else {
      const cells = [[[0, 25], 0, 'an open exit (its terrain cost)'], [[0, 10], 255, 'a wall exit'], [[0, 0], 255, 'a corner'], [[25, 25], COSTS.plain, 'plain'], [[40, 40], 255, 'the controller']];
      for (const [[x, y], v, what] of cells) if (at(x, y) !== v) problems.push(`${HOME} matrix (${x}, ${y}) ${at(x, y)}, expected ${v} (${what})`);
    }
    const unknown = answers.W2N1;
    const corners = new Set([0, 49, 49 * 50, 49 * 50 + 49]);
    if (!Array.isArray(unknown) || unknown.some((v, i) => v !== (corners.has(i) ? 255 : 0))) problems.push('roomCallback(W2N1) is not the unknown-room matrix');
    if (answers.W1N2 !== false) problems.push(`roomCallback(W1N2) gave ${JSON.stringify(answers.W1N2)?.slice(0, 40)}, expected false`);
  }
  const statusRooms = state.statusCalls.map(([, n]) => n);
  if (new Set(statusRooms).size !== statusRooms.length) problems.push(`getRoomStatus read a room twice: ${JSON.stringify(statusRooms)}`);

  // The search that throws: caught, kept by each creep (one search each), the creeps hold.
  const later = state.searches.filter((x) => doneAt !== null && x.tick > doneAt);
  if (later.length !== 2) problems.push(`${later.length} searches after the second order, expected 2 (one per creep, each failure kept)`);
  if (later.some((x) => x.to.roomName !== NOWHERE.room)) problems.push(`a later search went to ${later.map((x) => x.to.roomName).join(', ')}`);
  if (new Set(later.map((x) => x.tick)).size !== 1) problems.push(`the later searches ran on ticks ${later.map((x) => x.tick).join(', ')}, expected one tick`);
  const after = state.calls.filter(([o]) => throwAt !== null && o > throwAt);
  if (after.length) problems.push(`moves after the order to ${NOWHERE.room}: ${JSON.stringify(after.slice(0, 4))}`);
  const summary = commandResults(world, SUMMARY_CMD);
  const parts = [
    'routes searched 3 (failed 2, edge tiles learned 0), rooms crossed 4 (transfers refused 0)',
    'PathFinder searches 3 (threw 2, incomplete 0',
    'blocked: closed 0, status ',
    `moveTo order: (${NOWHERE.x}, ${NOWHERE.y}) in ${NOWHERE.room} within ${NOWHERE.range} from ${TARGET.room} (2 creep(s))`,
  ];
  if (summary.length !== 1) problems.push(`${summary.length} movement summaries`);
  else for (const p of parts) if (!summary[0].includes(p)) problems.push(`movement summary lacks "${p}": ${summary[0]}`);

  if (problems.length) throw new Error(`route-sim:\n  ${problems.slice(0, 20).join('\n  ')}`);
  return { moves: state.calls.length, doneAt, ops: s?.ops, summary: summary[0] };
}

if (isMain(import.meta.url)) {
  try {
    const r = routeSim(process.argv[2]);
    console.log(`route-sim ok: ${r.moves} move calls, order done at offset ${r.doneAt}, first search ${r.ops} ops | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
