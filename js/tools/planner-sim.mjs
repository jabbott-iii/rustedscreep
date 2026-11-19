// The base planner's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/planner.rs and opening.rs over rustedscreep-core `planner::job`, `planner::build` and
// `economy::placement`; P4.3, ADR-007, ADR-026). E7N57 as the live room stands (STATUS 2026-11-17): its terrain,
// controller, two sources and mineral from the terrain corpus (crates/rustedscreep-core/tests/fixtures/rooms/shard3/
// E7N57.txt), owned at RCL4, its spawn at (43, 21) and the 20 extensions ADR-020's pattern gave it (EXTENSIONS: the
// replay of `next_extension` in tests/planner_build.rs, whose first five equal the live sites), all full, the
// containers on the plan's mining tiles and controller container tile (so the plan adopts them and the economy asks
// for none), and one idle worker (the movement layer reads a room only while an own creep is in it). The harness
// plays the engine's part as far as the bot can see it:
// - `room.createConstructionSite` makes the game side's checks (V200, V237, V238: arguments, the RCL limit counting
//   built structures and sites, the room's edge, terrain walls but for roads, any site on the tile, a structure of
//   the type, or a costed structure that is not a road or a rampart for every type but roads and ramparts,
//   MAX_CONSTRUCTION_SITES); an accepted site appears on the next tick and is built BUILD_TICKS later, a road as a
//   structure `room.find(FIND_STRUCTURES)` lists, an extension, the storage or a tower as an own structure;
// - every structure's `destroy()` makes the game side's checks (V248: the room's controller is the bot's, no hostile
//   creep) and the processor removes it after the loop (V251, V252; the ruin is not modelled: V253 lets a site go on
//   its tile);
// - `spawnCreep` follows the runtime's checks and writes `Memory.creeps[name]` on OK (V171, V181), and the processor
//   drops every accepted order (V173), so no creep appears; the worker's calls return OK and do nothing.
// It runs three phases, each until the placement has been quiet for QUIET loops: the profile's config; then
// `planner.storage` on; then `planner.towers` on. It checks:
// - one planner line, `rustedscreep planner: E7N57 planned in N steps: M items, anchor (43, 21), valid`;
// - the first phase places only roads and destroys nothing (the extensions on the plan are kept, the planned slots
//   beyond CONTROLLER_STRUCTURES.extension[4] wait, the storage and the towers are off);
// - the second phase destroys exactly the extension on the storage's tile (41, 21), places an extension site on a
//   planned slot (the freed count) and the storage site on (41, 21), and never leaves the room below 19 extensions;
//   the third destroys exactly the extension on the first tower's tile (43, 19) and places the tower's site there;
// - every site call returns OK on a tile the harness allows, with fewer than PLANNER_MMO.max_sites sites open;
//   every destroy returns OK;
// - `rustedscreep.planner()` at the end of the phases: the room's plan ready with the anchor, `destroys 2 (accepted 2,
//   dropped 0)`, and the CPU per step line (sandbox time, a stand-in for the wasm's cost on the MMO);
// - the planner's record (ADR-027, crates/rustedscreep-core/src/planner/record.rs) in segment PLANNER_SEGMENT: a frame
//   of owner kind 3, schema 1, a good CRC and one entry, E7N57 anchored on (43, 21) with the containers it adopted;
// - then a halt (a new global): the record loads (one log line), the plan comes back anchored on it ("from the record"
//   in the planner line and the summary), and nothing is destroyed; then the record corrupted and another halt: it is
//   refused (one warning), the room is planned from what stands, the same anchor, the record written again, and
//   nothing is destroyed;
// - no fault line, no incident, two halts, three globals.
// npm run check runs it (row 21); standalone: node js/tools/planner-sim.mjs [dir].
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { findFiltered, makeStore, packPos } from './lib/world.mjs';
import { crc32 } from './lib/crc32.mjs';
import { ENGINE_CONSTANTS, PLANNER_SEGMENT, commandResults, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
const ROOM = 'E7N57';
const T0 = 50000000;
const LEVEL = 4;
const SPAWN = { x: 43, y: 21 };
/** ADR-020's pattern in E7N57 (tests/planner_build.rs's replay), in the order it placed them. */
const EXTENSIONS = [
  [41, 19], [41, 21], [41, 23], [43, 19], [43, 23], [45, 19], [45, 21], [45, 23], [40, 20], [40, 22],
  [42, 18], [44, 18], [44, 24], [46, 18], [46, 20], [46, 22], [39, 19], [39, 21], [39, 23], [43, 17],
].map(([x, y]) => ({ x, y }));
/** The plan's mining tiles and controller container (tests/planner_build.rs's plan of E7N57). */
const CONTAINERS = [{ x: 34, y: 21 }, { x: 38, y: 17 }, { x: 38, y: 25 }];
/** The plan's storage (RCL4) and first tower (RCL3), each on an ADR-020 extension. */
const STORAGE = { x: 41, y: 21 };
const TOWER = { x: 43, y: 19 };
const WORKER_AT = { x: 36, y: 23 };
/** PLANNER_MMO.max_sites (crates/rustedscreep-core/src/config.rs). */
const MAX_SITES = 10;
/** Ticks from a site's first appearance to its structure. */
const BUILD_TICKS = 3;
/** Loops with no placement call that end a phase (more than APPEAR_TICKS and a rescan). */
const QUIET = 40;
const RUN = 900;
/** Frame layout (rustedscreep-core persist/frame.rs, ADR-004) and the planner's record (persist::kind::PLANNER,
 * planner::record::SCHEMA). */
const TAG = 'rs1b';
const KIND_PLANNER = 3;
const RECORD_SCHEMA = 1;
const HALT = 'Game.cpu.halt()';
const SUMMARY = 'rustedscreep.planner()';
const set = (key, value) => `rustedscreep.configSet(${JSON.stringify(key)}, ${JSON.stringify(value)})`;

const id = (n) => `5bbcaa9099f9b0a0f7f4d${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const at = (p) => `(${p.x},${p.y})`;
const same = (a, b) => a.x === b.x && a.y === b.y;
const botName = (role, time) => `${role}${ROOM}-${(BigInt(time) << 8n).toString(36)}`;
const WORKER = botName('w', T0 - 1000);

/** The corpus room: its terrain (row-major masks, V156) and natural objects. */
const FIXTURE = (() => {
  const text = readFileSync(join(ROOT, 'crates', 'rustedscreep-core', 'tests', 'fixtures', 'rooms', 'shard3', `${ROOM}.txt`), 'utf8');
  const out = { sources: [], raw: new Uint8Array(2500) };
  const lines = text.split('\n').filter((l) => l && !l.startsWith('#'));
  const t = lines.indexOf('terrain');
  for (const line of lines.slice(0, t)) {
    const w = line.split(' ');
    if (w[0] === 'controller') out.controller = { x: +w[1], y: +w[2] };
    if (w[0] === 'source') out.sources.push({ x: +w[1], y: +w[2] });
    if (w[0] === 'mineral') out.mineral = { type: w[1], x: +w[2], y: +w[3] };
  }
  lines.slice(t + 1, t + 51).forEach((row, y) => {
    for (let x = 0; x < 50; x++) out.raw[y * 50 + x] = Number(row[x]);
  });
  return out;
})();
const isWall = (x, y) => (FIXTURE.raw[y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

function makeWorld() {
  return {
    nextId: 100,
    spawn: { id: id(1), name: 'Spawn1', ...SPAWN },
    controller: { id: id(2), ...FIXTURE.controller },
    sources: FIXTURE.sources.map((s, i) => ({ id: id(3 + i), ...s })),
    mineral: { id: id(6), ...FIXTURE.mineral },
    /** Own structures beyond the spawn: { id, type, x, y }. */
    owned: EXTENSIONS.map((e, i) => ({ id: id(10 + i), type: 'extension', ...e })),
    containers: CONTAINERS.map((c, i) => ({ id: id(40 + i), ...c })),
    roads: [],
    /** Own sites: { id, type, x, y, since }, and this loop's accepted calls. */
    sites: [],
    siteOrders: [],
    destroyOrders: [],
    siteCalls: [],
    destroyCalls: [],
    spawnCalls: [],
    phase: 0,
    phaseAt: [0],
    haltAt: [],
    lastCall: 0,
    minExtensions: EXTENSIONS.length,
    errors: [],
    context: null,
  };
}

/** Why the game side refuses a site of `type` on (x, y), if it does (V200, V237, V238). */
function refuses(state, x, y, type) {
  if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x > 49 || y > 49) return C.ERR_INVALID_ARGS;
  if (!C.CONSTRUCTION_COST[type]) return C.ERR_INVALID_ARGS;
  const built = [...state.owned.map((o) => o.type), 'spawn', ...state.roads.map(() => 'road'), ...state.containers.map(() => 'container')];
  const have = [...built, ...state.sites.map((s) => s.type), ...state.siteOrders.map((s) => s.type)].filter((t) => t === type).length;
  if (have >= (C.CONTROLLER_STRUCTURES[type]?.[LEVEL] ?? 0)) return C.ERR_RCL_NOT_ENOUGH;
  const here = (o) => o.x === x && o.y === y;
  if (x <= 0 || y <= 0 || x >= 49 || y >= 49) return C.ERR_INVALID_TARGET;
  if (type !== 'road' && isWall(x, y)) return C.ERR_INVALID_TARGET;
  if (state.sites.some(here) || state.siteOrders.some(here)) return C.ERR_INVALID_TARGET;
  const costed = [{ ...state.spawn, type: 'spawn' }, ...state.owned, ...state.containers.map((c) => ({ ...c, type: 'container' }))].filter(here);
  if (costed.some((o) => o.type === type) || (type === 'road' && state.roads.some(here))) return C.ERR_INVALID_TARGET;
  if (type !== 'road' && type !== 'rampart' && costed.length) return C.ERR_INVALID_TARGET;
  if ([state.controller, ...state.sources, state.mineral].some(here)) return C.ERR_INVALID_TARGET;
  if (state.sites.length + state.siteOrders.length >= C.MAX_CONSTRUCTION_SITES) return C.ERR_FULL;
  return C.OK;
}

function sceneFor(state, offset) {
  state.siteOrders = [];
  state.destroyOrders = [];
  const time = T0 + offset;
  const ctl = state.controller;
  const controller = {
    id: ctl.id, structureType: 'controller', my: true, owner: { username: 'bot' }, level: LEVEL, pos: pos(ctl.x, ctl.y),
    progress: 0, progressTotal: C.CONTROLLER_LEVELS[LEVEL], ticksToDowngrade: C.CONTROLLER_DOWNGRADE[LEVEL],
  };
  const extCapacity = C.EXTENSION_ENERGY_CAPACITY[LEVEL];
  const extensions = state.owned.filter((o) => o.type === 'extension');
  const capacity = C.SPAWN_ENERGY_CAPACITY + extCapacity * extensions.length;
  const destroyable = (o, extra) => ({
    ...extra,
    destroy(...args) {
      const call = { offset, id: o.id, type: extra.structureType, x: o.x, y: o.y, args: args.length, result: C.OK };
      state.destroyCalls.push(call);
      state.lastCall = offset;
      // V248: the room's controller is the bot's and no hostile creep is in it.
      state.destroyOrders.push(o.id);
      return call.result;
    },
  });
  const created = new Set();
  const spawn = destroyable(state.spawn, {
    id: state.spawn.id, name: state.spawn.name, structureType: 'spawn', my: true, owner: { username: 'bot' }, pos: pos(SPAWN.x, SPAWN.y),
    store: makeStore({ energy: C.SPAWN_ENERGY_CAPACITY }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    spawning: null,
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      state.spawnCalls.push({ offset, name });
      if (!name || !Array.isArray(body) || created.has(name)) return C.ERR_INVALID_ARGS;
      if (opts.dryRun) return C.OK;
      created.add(name);
      const mem = state.context.Memory;
      if (mem.creeps === undefined) mem.creeps = {};
      if (mem.creeps && typeof mem.creeps === 'object') mem.creeps[name] = opts.memory || {};
      return C.OK;
    },
  });
  const owned = state.owned.map((o) => destroyable(o, {
    id: o.id, structureType: o.type, my: true, owner: { username: 'bot' }, pos: pos(o.x, o.y),
    ...(o.type === 'extension' ? { store: makeStore({ energy: extCapacity }, { resourceCapacity: { energy: extCapacity } }) } : {}),
    ...(o.type === 'storage' ? { store: makeStore({}, { capacity: C.STORAGE_CAPACITY }) } : {}),
    ...(o.type === 'tower' ? { store: makeStore({}, { resourceCapacity: { energy: C.TOWER_CAPACITY } }) } : {}),
  }));
  const containers = state.containers.map((c) => destroyable(c, {
    id: c.id, structureType: 'container', pos: pos(c.x, c.y), hits: C.CONTAINER_HITS, hitsMax: C.CONTAINER_HITS,
    store: makeStore({}, { capacity: C.CONTAINER_CAPACITY }),
  }));
  const roads = state.roads.map((r) => ({ id: r.id, structureType: 'road', pos: pos(r.x, r.y), hits: C.ROAD_HITS, hitsMax: C.ROAD_HITS }));
  const sources = state.sources.map((s) => ({
    id: s.id, energy: C.SOURCE_ENERGY_CAPACITY, energyCapacity: C.SOURCE_ENERGY_CAPACITY, pos: pos(s.x, s.y), ticksToRegeneration: undefined,
  }));
  const m = state.mineral;
  const mineral = { id: m.id, mineralType: m.type, mineralAmount: 0, density: 1, pos: pos(m.x, m.y), ticksToRegeneration: undefined };
  const sites = state.sites.map((s) => ({
    id: s.id, my: true, owner: { username: 'bot' }, structureType: s.type, progress: 0, progressTotal: C.CONSTRUCTION_COST[s.type], pos: pos(s.x, s.y),
  }));
  const body = ['work', 'carry', 'move'];
  const worker = {
    id: id(90), name: WORKER, my: true, spawning: false, fatigue: 0, owner: { username: 'bot' }, pos: pos(WORKER_AT.x, WORKER_AT.y),
    body: body.map((type) => ({ type, hits: 100 })), hits: 300, hitsMax: 300, ticksToLive: 1000,
    store: makeStore({}, { capacity: C.CARRY_CAPACITY }),
  };
  for (const name of ['move', 'say', 'drop', 'transfer', 'withdraw', 'harvest', 'build', 'repair', 'upgradeController', 'pickup', 'pull']) {
    worker[name] = () => C.OK;
  }
  const structures = [controller, spawn, ...owned];
  const lists = {
    [C.FIND_SOURCES]: sources, [C.FIND_MINERALS]: [mineral], [C.FIND_STRUCTURES]: [...structures, ...containers, ...roads],
    [C.FIND_MY_STRUCTURES]: structures, [C.FIND_MY_SPAWNS]: [spawn], [C.FIND_CREEPS]: [worker], [C.FIND_MY_CREEPS]: [worker],
    [C.FIND_MY_CONSTRUCTION_SITES]: sites, [C.FIND_CONSTRUCTION_SITES]: sites,
  };
  const room = {
    name: ROOM, controller, energyAvailable: capacity, energyCapacityAvailable: capacity,
    find: (type, opts) => findFiltered(lists[type] ?? [], opts),
    getTerrain: () => ({ getRawBuffer: () => FIXTURE.raw.slice() }),
    createConstructionSite(...args) {
      const [x, y, type] = args;
      const call = { offset, x, y, type, args: args.length, open: state.sites.length, result: refuses(state, x, y, type) };
      state.siteCalls.push(call);
      state.lastCall = offset;
      if (call.result === C.OK) state.siteOrders.push({ x, y, type });
      return call.result;
    },
  };
  return {
    creeps: { [WORKER]: worker },
    rooms: { [ROOM]: room },
    structures: Object.fromEntries(structures.map((s) => [s.id, s])),
    time,
  };
}

/** The processor after loop `offset`: destroys, then new sites, then sites done building. */
function advance(state, offset) {
  for (const target of state.destroyOrders) {
    const before = state.owned.length + state.containers.length;
    state.owned = state.owned.filter((o) => o.id !== target);
    state.containers = state.containers.filter((o) => o.id !== target);
    if (state.owned.length + state.containers.length === before) state.errors.push(`offset ${offset}: destroy of a structure that is gone`);
  }
  for (const s of state.siteOrders) state.sites.push({ id: id(state.nextId++), ...s, since: offset });
  for (const s of state.sites.filter((x) => offset - x.since >= BUILD_TICKS)) {
    state.sites = state.sites.filter((x) => x !== s);
    if (s.type === 'road') state.roads.push({ id: id(state.nextId++), x: s.x, y: s.y });
    else state.owned.push({ id: id(state.nextId++), type: s.type, x: s.x, y: s.y });
  }
  const extensions = state.owned.filter((o) => o.type === 'extension').length + state.sites.filter((s) => s.type === 'extension').length;
  state.minExtensions = Math.min(state.minExtensions, extensions);
}

/** Decodes a frame's header and payload (persist/frame.rs). */
function decodeFrame(text) {
  if (typeof text !== 'string' || !text.startsWith(TAG)) return null;
  const bytes = Buffer.from(text.slice(TAG.length), 'base64');
  const payload = bytes.subarray(16);
  const computed = crc32(Buffer.concat([bytes.subarray(0, 12), payload]));
  return {
    kind: bytes.readUInt16LE(0), schema: bytes.readUInt16LE(2), written: bytes.readUInt32LE(4),
    length: bytes.readUInt32LE(8), crcOk: bytes.readUInt32LE(12) === computed, payload,
  };
}

/** The record's entries (planner/record.rs): { room, anchor, adopted: [{ kind, tile }] }, or a string saying why not. */
function decodeRecord(payload) {
  let i = 0;
  const u8 = () => { if (i >= payload.length) throw new Error('short'); return payload[i++]; };
  const u16 = () => u8() | (u8() << 8);
  try {
    const entries = [];
    for (let n = u16(); n > 0; n--) {
      const room = u16();
      const anchor = u16();
      const adopted = [];
      for (let k = u8(); k > 0; k--) adopted.push({ kind: u8(), tile: u16() });
      entries.push({ room, anchor, adopted });
    }
    return i === payload.length ? entries : 'bytes after the last entry';
  } catch (e) {
    return e.message;
  }
}

export function plannerSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let finished = false;
  const records = [];
  const plannedLines = (w) => w.logs.filter((l) => l.includes(`rustedscreep planner: ${ROOM} planned`)).length;
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context) => {
      state.context = context;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      // Phases 0 to 2 in the first global; 3 halts it; 4 in the second global; 5 corrupts the record and halts; 6 in
      // the third global. Each waits for its global's plan and then QUIET loops with no call.
      const want = state.phase < 3 ? 1 : state.phase < 5 ? 2 : 3;
      const quiet = offset - Math.max(state.lastCall, state.phaseAt.at(-1)) >= QUIET;
      if (state.phase === 3 || state.phase === 5) {
        // The tick after a summary: the record as it stands, then the halt.
        records.push(w.segments[PLANNER_SEGMENT]);
        if (state.phase === 5) {
          const t = w.segments[PLANNER_SEGMENT];
          w.segments[PLANNER_SEGMENT] = t.slice(0, 10) + (t[10] === 'A' ? 'B' : 'A') + t.slice(11);
        }
        state.phase += 1;
        state.phaseAt.push(offset);
        state.haltAt.push(offset);
        return HALT;
      }
      if (plannedLines(w) < want || !quiet) return null;
      state.phase += 1;
      state.phaseAt.push(offset);
      if (state.phase === 1) return set('planner.storage', 'true');
      if (state.phase === 2) return set('planner.towers', 'true');
      if (state.phase === 7) {
        finished = true;
        records.push(w.segments[PLANNER_SEGMENT]);
      }
      return SUMMARY;
    },
    until: () => finished,
  });
  const problems = [...state.errors.slice(0, 10)];
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts !== 2 || world.globals !== 3) problems.push(`halts ${world.halts}, globals ${world.globals}: expected 2 and 3`);
  if (!finished) problems.push(`not finished in ${world.ticksRun} ticks (phase ${state.phase}, planned ${plannedLines(world)})`);

  const lines = world.logs.filter((l) => l.includes('rustedscreep planner:') && l.includes('planned'));
  const lineFor = (from) => new RegExp(`rustedscreep planner: ${ROOM} planned in \\d+ steps: \\d+ items, anchor \\(${SPAWN.x}, ${SPAWN.y}\\)${from}, valid$`);
  const wantLines = [lineFor(''), lineFor(' from the record'), lineFor('')];
  if (lines.length !== 3 || !wantLines.every((re, i) => re.test(lines[i]))) problems.push(`planner lines ${JSON.stringify(lines)}`);

  // The record after each global: one entry, the room anchored on the spawn, its adopted containers. The third
  // global wrote it again after refusing the corrupted one.
  const roomCode = packPos(ROOM, 0, 0) >>> 16;
  const anchorCode = SPAWN.x * 50 + SPAWN.y;
  const containerTiles = state.containers.map((c) => c.x * 50 + c.y);
  const entriesOf = (text, which) => {
    const f = decodeFrame(text);
    if (!f || f.kind !== KIND_PLANNER || f.schema !== RECORD_SCHEMA || !f.crcOk || f.length !== f.payload.length) {
      problems.push(`${which} record frame ${JSON.stringify(f && { ...f, payload: f.payload.length })}`);
      return null;
    }
    const entries = decodeRecord(f.payload);
    const e = Array.isArray(entries) ? entries[0] : null;
    if (!e || entries.length !== 1 || e.room !== roomCode || e.anchor !== anchorCode || !e.adopted.length
      || e.adopted.some((a) => a.kind !== 0 || !containerTiles.includes(a.tile))) {
      problems.push(`${which} record ${JSON.stringify(entries)}: expected ${ROOM} (${roomCode}) anchored on ${anchorCode} with its containers ${containerTiles}`);
    }
    return f;
  };
  const [record1, record2, record3] = records;
  entriesOf(record1, 'first global\'s');
  if (record2 !== record1) problems.push('the second global rewrote a record that did not change');
  const rewritten = entriesOf(record3, 'third global\'s');
  if (rewritten && rewritten.written <= state.haltAt[1] + T0) problems.push(`the refused record was not written again (written at ${rewritten.written})`);
  const loaded = world.logs.filter((l) => l.startsWith(`rustedscreep planner: record loaded from segment ${PLANNER_SEGMENT} (1 rooms, written at `));
  if (loaded.length !== 1) problems.push(`record load lines: ${loaded.length}, expected 1`);
  const refused = world.logs.filter((l) => l.startsWith(`rustedscreep planner: the record in segment ${PLANNER_SEGMENT} was not used (CRC-32 `));
  if (refused.length !== 1) problems.push(`record refusal lines: ${refused.length}, expected 1`);
  if (state.destroyCalls.some((d) => d.offset > state.haltAt[0])) problems.push(`destroys after the first global: ${JSON.stringify(state.destroyCalls)}`);

  const [, storageOn, towersOn, end] = state.phaseAt;
  const inPhase = (list, from, to) => list.filter((c) => c.offset > from && c.offset <= (to ?? Infinity));
  for (const c of state.siteCalls) {
    if (c.result !== C.OK || c.args !== 3) problems.push(`offset ${c.offset}: createConstructionSite(${c.x}, ${c.y}, ${c.type}) with ${c.args} arguments returned ${c.result}`);
    if (c.open >= MAX_SITES) problems.push(`offset ${c.offset}: a site with ${c.open} open`);
  }
  for (const c of state.destroyCalls) if (c.args !== 0) problems.push(`offset ${c.offset}: destroy with ${c.args} arguments`);
  const first = inPhase(state.siteCalls, 0, storageOn);
  if (!first.length || first.some((c) => c.type !== 'road')) problems.push(`phase 1 sites ${JSON.stringify(first.map((c) => `${c.type}${at(c)}`))}, expected roads only`);
  if (inPhase(state.destroyCalls, 0, storageOn).length) problems.push('phase 1 destroyed something');

  const phase = (from, to, tile, kind) => {
    const destroys = inPhase(state.destroyCalls, from, to);
    if (destroys.length !== 1 || destroys[0].type !== 'extension' || !same(destroys[0], tile)) {
      problems.push(`destroys after offset ${from}: ${JSON.stringify(destroys.map((d) => `${d.type}${at(d)}`))}, expected the extension at ${at(tile)}`);
      return;
    }
    const sites = inPhase(state.siteCalls, from, to);
    const own = sites.find((c) => c.type === kind && same(c, tile));
    if (!own || own.offset <= destroys[0].offset) problems.push(`no ${kind} site on ${at(tile)} after its destroy: ${JSON.stringify(sites.map((c) => `${c.offset}:${c.type}${at(c)}`))}`);
    return sites;
  };
  const second = phase(storageOn, towersOn, STORAGE, 'storage') ?? [];
  if (!second.some((c) => c.type === 'extension' && !same(c, STORAGE))) problems.push('no extension site took the freed count');
  phase(towersOn, end, TOWER, 'tower');
  if (state.minExtensions < EXTENSIONS.length - 1) problems.push(`extensions fell to ${state.minExtensions}`);

  const summaries = commandResults(world, SUMMARY);
  const summary = summaries[0] ?? '';
  for (const part of [`${ROOM}: plan ready at `, `anchor (${SPAWN.x}, ${SPAWN.y}))`, 'destroys 2 (accepted 2, dropped 0)', '| CPU per step (steps, mean, most):', 'record: none at boot, 1 rooms now;']) {
    if (!summary.includes(part)) problems.push(`planner summary lacks "${part}": ${summary}`);
  }
  for (const [i, parts] of [
    [1, ['record: loaded (1 rooms, written at ', `anchor (${SPAWN.x}, ${SPAWN.y}) from the record)`, 'destroys 0 (accepted 0, dropped 0)']],
    [2, ['record: refused at boot (CRC-32 ', `anchor (${SPAWN.x}, ${SPAWN.y}))`, 'destroys 0 (accepted 0, dropped 0)']],
  ]) {
    for (const part of parts) if (!(summaries[i] ?? '').includes(part)) problems.push(`summary ${i} lacks "${part}": ${summaries[i]}`);
  }
  if (problems.length) throw new Error(`planner-sim:\n  ${problems.slice(0, 25).join('\n  ')}`);
  return {
    ticks: world.ticksRun, phases: state.phaseAt, roads: state.siteCalls.filter((c) => c.type === 'road').length,
    sites: state.siteCalls.filter((c) => c.type !== 'road').map((c) => `${c.offset}:${c.type}${at(c)}`),
    destroys: state.destroyCalls.map((d) => `${d.offset}:${d.type}${at(d)}`), line: lines[0], summary,
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = plannerSim(process.argv[2]);
    console.log(`planner-sim ok: ${r.ticks} ticks, phases at ${r.phases.join(', ')}; ${r.roads} road sites; other sites ${r.sites.join(' ')}; ` +
      `destroys ${r.destroys.join(' ')} | ${r.line} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
