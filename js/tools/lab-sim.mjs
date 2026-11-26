// Lab reactions' glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/labs.rs and snapshot.rs `read_lab` and `read_terminal_stock` over rustedscreep-core
// `economy::labs`; ROADMAP P9.2, ADR-045: a target list with stock goals, a lab hauler, compounds only). W1N1 is owned
// at LEVEL, the first level CONTROLLER_STRUCTURES allows 3 labs: an open plain room with walls on its border ring, one
// spawn, the controller, no source and no mineral (so mining, hauling, the opening's workers and mineral mining stay
// inert), an own terminal, and the first 3 labs of the planner's lab stamp (its two source labs and the output nearest
// by tile; rustedscreep-core planner::bunker::LABS). As in mineral-sim.mjs, an idle own creep whose name the bot did
// not give (KEEPER) stands in the room from the start, since movement reads a room's terrain only while an own creep
// stands in it. The room's energy is kept at its capacity by the harness (no extensions stand).
// The run starts with the overrides of CONFIG_LINES in segment 97 (the runtime config's record): the planner, the
// interim sites, remote mining, the signer, scouting and claim scoring off, so nothing else asks for the spawn or
// places sites; `labs.batch_min` BATCH_MIN and `labs.read_ticks` READ_TICKS. The record loads on the global's second
// loop, so the first read runs with the profile's values.
// The terminal holds nothing at first, so the room has no job; at offset FILL_AT the harness puts LOAD of each base
// mineral in it, the next due read starts the first job (UL from U and L, the deepest step of XGHO2's chain, LOAD of
// it), and the run ends LINGER loops after the job's "finished" line, once the hauler carries no UL.
// The harness plays the engine's part as far as the bot can see it:
// - each lab's `mineralType`, `cooldown` and `store` getters count their reads (V406, V412, V413), and so does the
//   terminal's `store`; a lab holds energy and one mineral (V410, V411), the terminal is one pool (V401);
// - `runReaction(lab1, lab2)` makes the game side's checks in V403's order and records the intent; the processor
//   (V405) checks again, makes LAB_REACTION_AMOUNT of the product in the output, takes as much from each source, and
//   sets the output's cooldown to REACTION_TIME of the product, which reads 0 again REACTION_TIME loops later (V406);
// - `withdraw(target, resource, amount)` and `transfer(target, resource, amount)` make the game side's checks in V400's
//   and V399's orders (a lab refuses a second mineral, V411, V414) and record the intent; the processor moves what fits;
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52), with fatigue (V54, V55);
// - `spawnCreep(body, name)` makes the runtime's checks in V171's order and its `Memory.creeps[name]` write (V181); the
//   processor (V173) turns an order for a lab hauler's name (`l…`) into a creep that spawns for CREEP_SPAWN_TIME ×
//   parts loops with the spawn busy, then stands on the first free tile around the spawn in direction order (V174),
//   and drops any other order.
// It checks:
// - the lab read (each lab's `mineralType` and `cooldown`, the `store` of a lab holding a mineral, and the terminal's
//   `store`) all on a loop or none; with no job, on the first loop after the boot loop and then every READ_TICKS
//   loops; every loop a reaction or a hauler call happens on is a read loop; between the hauler's last load and its
//   first unload (the job reacting on its own), a read comes only on a reaction's loop or the READ_TICKS cadence;
// - no `spawnCreep` before FILL_AT; then one order for a lab hauler named for W1N1, of CARRY and MOVE parts only, with
//   HAULER_CARRY CARRY parts and a MOVE part for every MOVE_POWER / MOVE_COST_ROAD of them, within CAPACITY;
// - `runReaction` only by the output, with the two source labs, only on loops the output's cooldown reads 0 and both
//   sources hold LAB_REACTION_AMOUNT, every such loop while the job reacts; consecutive reactions REACTION_TIME.UL
//   loops apart while the sources hold enough;
// - the hauler's calls: `withdraw(terminal, U|L, amount)`, `withdraw(lab, type)` with no amount, `transfer(lab|terminal,
//   type)` with no amount, next to their targets;
// - the job's lines ("makes LOAD UL from U and L", "finished its UL job (N reactions)") once each, N × LAB_REACTION_AMOUNT
//   equal to LOAD; nothing lost: the terminal's U and L fell by LOAD each and its UL is LOAD;
// - `rustedscreep.labs()` at the end: the targets with their goals, the room's labs, roles and counters;
// - every call returns OK; no fault, error or trap line, no model error, no halt, one global.
// npm run check runs it (row 28); standalone: node js/tools/lab-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { findFiltered, makeStore, packPos } from './lib/world.mjs';
import { CONFIG_SEGMENT, ENGINE_CONSTANTS, commandResults, configRecord, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
const ROOM = 'W1N1';
const T0 = 50000000;
/** The first level that allows 3 labs (CONTROLLER_STRUCTURES.lab). */
const LEVEL = [1, 2, 3, 4, 5, 6, 7, 8].find((l) => C.CONTROLLER_STRUCTURES.lab[l] >= 3);
const CAPACITY = C.CONTROLLER_STRUCTURES.spawn[LEVEL] * C.SPAWN_ENERGY_CAPACITY
  + C.CONTROLLER_STRUCTURES.extension[LEVEL] * C.EXTENSION_ENERGY_CAPACITY[LEVEL];
const SPAWN = { x: 25, y: 32 };
const CONTROLLER = { x: 40, y: 10 };
const TERMINAL = { x: 30, y: 24 };
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
/** The planner's lab stamp (rustedscreep-core planner::bunker::LABS) laid at STAMP. */
const STAMP = { x: 24, y: 20 };
const LAB_STAMP = ['rll.', 'lrsl', 'lsrl', '.llr'];
/** Its source labs, then its first other lab by tile index (y × 50 + x), as the planner places them first. */
const LABS = (() => {
  const all = [];
  LAB_STAMP.forEach((row, dy) => [...row].forEach((c, dx) => {
    if (c === 's' || c === 'l') all.push({ x: STAMP.x + dx, y: STAMP.y + dy, source: c === 's' });
  }));
  all.sort((a, b) => Number(b.source) - Number(a.source) || (a.y * 50 + a.x) - (b.y * 50 + b.x));
  return all.slice(0, 3);
})();
const BASES = [C.RESOURCE_HYDROGEN, C.RESOURCE_OXYGEN, C.RESOURCE_UTRIUM, C.RESOURCE_LEMERGIUM, C.RESOURCE_KEANIUM,
  C.RESOURCE_ZYNTHIUM, C.RESOURCE_CATALYST];
const UL = C.RESOURCE_UTRIUM_LEMERGITE;
const REAGENTS = [C.RESOURCE_UTRIUM, C.RESOURCE_LEMERGIUM];
/** The fatigue a MOVE part removes a tick and a road's fatigue a part (screeps-game-api 0.23.5 extra.rs: MOVE_POWER,
 * MOVE_COST_ROAD; no engine global, V54, V55). */
const MOVE_POWER = 2;
const MOVE_COST_ROAD = 1;
/** LABS_MMO.hauler_carry (crates/rustedscreep-core/src/config.rs). */
const HAULER_CARRY = 10;
const BATCH_MIN = 50;
const LOAD = 2 * BATCH_MIN;
const READ_TICKS = 7;
const FILL_AT = 30;
const LINGER = 10;
const RUN = 700;
const SUMMARY = 'rustedscreep.labs()';
/** The overrides, in the config's field order (config::FIELDS). */
const CONFIG_LINES = [
  'opening.place_extensions=false',
  'mining.place_containers=false',
  'links.place_links=false',
  'hauling.place_container=false',
  'planner.enabled=false',
  'remote.enabled=false',
  'sign.enabled=false',
  'scout.enabled=false',
  'claim.enabled=false',
  `labs.batch_min=${BATCH_MIN}`,
  `labs.read_ticks=${READ_TICKS}`,
];
const START_LINE = `rustedscreep labs: ${ROOM} makes ${LOAD} ${UL} from ${REAGENTS[0]} and ${REAGENTS[1]}`;
const DONE_PREFIX = `rustedscreep labs: ${ROOM} finished its ${UL} job (`;

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
const count = (body, part) => body.filter((p) => p === part).length;
const cost = (body) => body.reduce((s, p) => s + C.BODYPART_COST[p], 0);
const used = (store) => Object.values(store).reduce((s, v) => s + v, 0);
const capacityOf = (c) => C.CARRY_CAPACITY * count(c.body, 'carry');
const isHauler = (name) => typeof name === 'string' && name.startsWith(`l${ROOM}-`);

/** Row-major terrain (V156): walls on the border ring. */
const RAW = (() => {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) if (x === 0 || y === 0 || x === 49 || y === 49) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  return raw;
})();
const isWall = (x, y) => (RAW[y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

function makeWorld() {
  return {
    labs: LABS.map((l, k) => ({ id: id(10 + k), x: l.x, y: l.y, source: l.source, store: {}, cooldownTime: 0 })),
    terminal: { id: id(4), ...TERMINAL, store: {} },
    spawn: { id: id(5), name: 'Spawn1', ...SPAWN, busy: null },
    controller: { id: id(6), ...CONTROLLER },
    creeps: [{ id: id(7), name: KEEPER.name, body: ['move'], x: KEEPER.x, y: KEEPER.y, store: {}, fatigue: 0, spawning: 0 }],
    nextId: 30,
    /** This tick's recorded intents, object id → { method: payload }. */
    intents: new Map(),
    reactionIntents: [],
    calls: [],
    spawnCalls: [],
    siteCalls: [],
    /** Per offset: the bot's reads of the lab read's getters. */
    reads: [],
    /** Per offset, the tick's start: each lab's cooldown and store. */
    starts: [],
    reactions: [],
    errors: [],
    context: null,
  };
}

const mineralOf = (store) => Object.keys(store).find((k) => k !== 'energy' && store[k] > 0);

/** Why a creep cannot stand on (x, y), if it cannot (V52): terrain, an obstacle structure. */
function isObstacle(state, x, y) {
  if (isWall(x, y)) return 'wall';
  const here = (o) => o.x === x && o.y === y;
  if ([state.spawn, state.terminal, state.controller, ...state.labs].some(here)) return 'obstacle';
  return null;
}

function intentsOf(state, objectId) {
  if (!state.intents.has(objectId)) state.intents.set(objectId, {});
  return state.intents.get(objectId);
}

function creepObject(state, c, offset, objects) {
  const capacity = capacityOf(c);
  const record = (method, target, args, result) => {
    state.calls.push({
      offset, actor: c.name, method, target: target?.id ?? null, kind: objects.kindOf(target), args, argCount: args.length, result,
      start: { x: c.x, y: c.y, store: { ...c.store } },
    });
    return result;
  };
  const near = (t) => t?.pos && range(c, t.pos) <= 1;
  const self = {
    id: c.id, name: c.name, my: true, spawning: c.spawning > 0, fatigue: c.fatigue, owner: { username: 'bot' }, pos: pos(c.x, c.y),
    body: c.body.map((type) => ({ type, hits: 100 })), hits: 100 * c.body.length, hitsMax: 100 * c.body.length,
    store: makeStore(c.store, { capacity }),
    ticksToLive: c.spawning > 0 ? undefined : C.CREEP_LIFE_TIME,
    move(...args) {
      const d = +args[0];
      if (c.spawning > 0) return record('move', null, args, C.ERR_BUSY);
      if (c.fatigue > 0) return record('move', null, args, C.ERR_TIRED);
      if (!count(c.body, 'move')) return record('move', null, args, C.ERR_NO_BODYPART);
      if (!Number.isInteger(d) || d < 1 || d > 8) return record('move', null, args, C.ERR_INVALID_ARGS);
      intentsOf(state, c.id).move = d;
      return record('move', null, args, C.OK);
    },
    withdraw(...args) {
      // V400, in its order.
      const [target, resource, amount] = args;
      const rest = args.slice(1);
      if (c.spawning > 0) return record('withdraw', target, rest, C.ERR_BUSY);
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('withdraw', target, rest, C.ERR_INVALID_ARGS);
      const from = objects.storeOf(target);
      if (!from) return record('withdraw', target, rest, C.ERR_INVALID_TARGET);
      if (!from[resource]) return record('withdraw', target, rest, C.ERR_INVALID_TARGET);
      if (!near(target)) return record('withdraw', target, rest, C.ERR_NOT_IN_RANGE);
      const free = capacity - used(c.store);
      if (free <= 0) return record('withdraw', target, rest, C.ERR_FULL);
      const n = amount || Math.min(free, from[resource]);
      if (n > free) return record('withdraw', target, rest, C.ERR_FULL);
      if (!n || from[resource] < n) return record('withdraw', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      intentsOf(state, c.id).withdraw = { id: target.id, resource, amount: n };
      return record('withdraw', target, rest, C.OK);
    },
    transfer(...args) {
      // V399, in its order; a lab's room for a resource is V411's.
      const [target, resource, amount] = args;
      const rest = args.slice(1);
      if (c.spawning > 0) return record('transfer', target, rest, C.ERR_BUSY);
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('transfer', target, rest, C.ERR_INVALID_ARGS);
      const to = objects.storeOf(target);
      if (!target?.id || !to) return record('transfer', target, rest, C.ERR_INVALID_TARGET);
      const room = objects.roomFor(target, resource);
      if (!near(target)) return record('transfer', target, rest, C.ERR_NOT_IN_RANGE);
      if (!c.store[resource]) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (room <= 0) return record('transfer', target, rest, C.ERR_FULL);
      const n = amount || Math.min(c.store[resource], room);
      if (c.store[resource] < n) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (n > room) return record('transfer', target, rest, C.ERR_FULL);
      intentsOf(state, c.id).transfer = { id: target.id, resource, amount: n };
      return record('transfer', target, rest, C.OK);
    },
  };
  for (const m of ['say', 'drop', 'heal', 'rangedHeal', 'rangedAttack', 'repair', 'pull', 'build', 'pickup', 'upgradeController', 'suicide', 'harvest']) {
    self[m] = (...args) => record(m, args[0], args.slice(1), C.OK);
  }
  return self;
}

/** A lab's room for `resource` (V411): a lab holding another mineral takes none. */
function labRoom(lab, resource) {
  if (resource === 'energy') return C.LAB_ENERGY_CAPACITY - (lab.store.energy ?? 0);
  const held = mineralOf(lab.store);
  if (held && held !== resource) return 0;
  return C.LAB_MINERAL_CAPACITY - (lab.store[resource] ?? 0);
}

function sceneFor(state, offset) {
  state.intents.clear();
  state.reactionIntents = [];
  const kinds = new Map();
  const stores = new Map();
  const rooms = new Map();
  const objects = {
    kindOf: (o) => (o && typeof o === 'object' ? kinds.get(o.id) ?? null : null),
    storeOf: (o) => (o && typeof o === 'object' ? stores.get(o.id) ?? null : null),
    roomFor: (o, res) => rooms.get(o?.id)?.(res) ?? 0,
  };
  const reads = (state.reads[offset] = { type: [0, 0, 0], cooldown: [0, 0, 0], store: [0, 0, 0], terminalStore: 0 });
  state.starts[offset] = {
    labs: state.labs.map((l) => ({ cooldown: Math.max(0, l.cooldownTime - offset), kind: mineralOf(l.store) ?? null, amount: used(l.store) })),
    haulers: state.creeps.filter((c) => isHauler(c.name)).map((c) => ({ out: !(c.spawning > 0), store: { ...c.store } })),
  };
  const labObjects = state.labs.map((l, k) => {
    const lab = {
      id: l.id, structureType: 'lab', my: true, owner: { username: 'bot' }, pos: pos(l.x, l.y), hits: C.LAB_HITS, hitsMax: C.LAB_HITS,
      get mineralType() {
        reads.type[k] += 1;
        return mineralOf(l.store);
      },
      get cooldown() {
        reads.cooldown[k] += 1;
        return Math.max(0, l.cooldownTime - offset);
      },
      get store() {
        reads.store[k] += 1;
        const kind = mineralOf(l.store);
        return makeStore(l.store, {
          capacity: C.LAB_ENERGY_CAPACITY + C.LAB_MINERAL_CAPACITY,
          resourceCapacity: kind ? { energy: C.LAB_ENERGY_CAPACITY, [kind]: C.LAB_MINERAL_CAPACITY } : { energy: C.LAB_ENERGY_CAPACITY },
        });
      },
      runReaction(...args) {
        // V403, in its order.
        const [a, b] = args;
        const call = (result) => {
          state.calls.push({ offset, actor: l.id, method: 'runReaction', target: a?.id ?? null, other: b?.id ?? null, argCount: args.length, result });
          return result;
        };
        if (l.cooldownTime > offset) return call(C.ERR_TIRED);
        const src = [a, b].map((x) => state.labs.find((m) => m.id === x?.id));
        if (src.some((m) => !m || m === l)) return call(C.ERR_INVALID_TARGET);
        if (src.some((m) => range(m, l) > 2)) return call(C.ERR_NOT_IN_RANGE);
        if (used(l.store) - (l.store.energy ?? 0) > C.LAB_MINERAL_CAPACITY - C.LAB_REACTION_AMOUNT) return call(C.ERR_FULL);
        if (src.some((m) => (m.store[mineralOf(m.store)] ?? 0) < C.LAB_REACTION_AMOUNT)) return call(C.ERR_NOT_ENOUGH_RESOURCES);
        const product = C.REACTIONS[mineralOf(src[0].store)]?.[mineralOf(src[1].store)];
        const held = mineralOf(l.store);
        if (!product || (held && held !== product)) return call(C.ERR_INVALID_ARGS);
        state.reactionIntents.push({ lab: l, src });
        return call(C.OK);
      },
    };
    kinds.set(l.id, 'lab');
    stores.set(l.id, l.store);
    rooms.set(l.id, (res) => labRoom(l, res));
    return lab;
  });
  const tm = state.terminal;
  const terminalStore = makeStore(tm.store, { capacity: C.TERMINAL_CAPACITY });
  const terminal = {
    id: tm.id, structureType: 'terminal', my: true, owner: { username: 'bot' }, pos: pos(tm.x, tm.y), hits: C.TERMINAL_HITS, hitsMax: C.TERMINAL_HITS,
    cooldown: 0,
    get store() {
      reads.terminalStore += 1;
      return terminalStore;
    },
  };
  const ctl = state.controller;
  const controller = {
    id: ctl.id, structureType: 'controller', my: true, owner: { username: 'bot' }, level: LEVEL, pos: pos(ctl.x, ctl.y),
    progress: 0, progressTotal: C.CONTROLLER_LEVELS[LEVEL], ticksToDowngrade: C.CONTROLLER_DOWNGRADE[LEVEL],
  };
  const sp = state.spawn;
  const created = new Set();
  const spawn = {
    id: sp.id, name: sp.name, structureType: 'spawn', my: true, owner: { username: 'bot' }, pos: pos(sp.x, sp.y),
    store: makeStore({ energy: C.SPAWN_ENERGY_CAPACITY }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    spawning: sp.busy === null ? null : { name: sp.busy.name, remainingTime: sp.busy.left },
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      const call = { offset, args: args.length, body: Array.isArray(body) ? [...body] : body, name, result: null };
      state.spawnCalls.push(call);
      call.result = (() => {
        // V171, in its order.
        if (!name || typeof opts !== 'object' || String(name).length > 100) return C.ERR_INVALID_ARGS;
        if (state.creeps.some((c) => c.name === name) || created.has(name)) return C.ERR_NAME_EXISTS;
        if (!Array.isArray(body) || !body.length || body.length > C.MAX_CREEP_SIZE || body.some((p) => !C.BODYPARTS_ALL.includes(p))) return C.ERR_INVALID_ARGS;
        if (sp.busy !== null) return C.ERR_BUSY;
        if (CAPACITY < cost(body)) return C.ERR_NOT_ENOUGH_ENERGY;
        if (opts.dryRun) return C.OK;
        created.add(name);
        const mem = state.context.Memory;
        if (mem.creeps === undefined) mem.creeps = {};
        if (mem.creeps && typeof mem.creeps === 'object') mem.creeps[name] = opts.memory || mem.creeps[name] || {};
        call.accepted = true;
        return C.OK;
      })();
      return call.result;
    },
  };
  kinds.set(terminal.id, 'terminal');
  kinds.set(controller.id, 'controller');
  kinds.set(spawn.id, 'spawn');
  stores.set(tm.id, tm.store);
  rooms.set(tm.id, () => C.TERMINAL_CAPACITY - used(tm.store));
  const creeps = state.creeps.map((c) => creepObject(state, c, offset, objects));
  const owned = [controller, spawn, terminal, ...labObjects];
  const lists = {
    [C.FIND_MINERALS]: [], [C.FIND_SOURCES]: [], [C.FIND_STRUCTURES]: owned, [C.FIND_MY_STRUCTURES]: owned,
    [C.FIND_MY_SPAWNS]: [spawn], [C.FIND_CREEPS]: creeps, [C.FIND_MY_CREEPS]: creeps,
  };
  const room = {
    name: ROOM, controller, energyAvailable: CAPACITY, energyCapacityAvailable: CAPACITY,
    find: (type, opts) => findFiltered(lists[type] ?? [], opts),
    getTerrain: () => ({ getRawBuffer: () => RAW.slice() }),
    createConstructionSite(...args) {
      state.siteCalls.push({ offset, args });
      return C.OK;
    },
  };
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries(owned.map((s) => [s.id, s])),
  };
}

/** The processor after loop `offset`: the reactions (V405), the creeps' intents, moves, then the object pass. */
function advance(state, offset) {
  for (const { lab, src } of state.reactionIntents) {
    // V405: checked again on the live objects, in order, so outputs sharing sources never overdraw them.
    if (lab.cooldownTime > offset) continue;
    const kinds = src.map((m) => mineralOf(m.store));
    if (src.some((m, k) => (m.store[kinds[k]] ?? 0) < C.LAB_REACTION_AMOUNT || range(m, lab) > 2)) continue;
    const product = C.REACTIONS[kinds[0]]?.[kinds[1]];
    const held = mineralOf(lab.store);
    if (!product || (held && held !== product) || (lab.store[product] ?? 0) + C.LAB_REACTION_AMOUNT > C.LAB_MINERAL_CAPACITY) continue;
    lab.store[product] = (lab.store[product] ?? 0) + C.LAB_REACTION_AMOUNT;
    lab.cooldownTime = offset + C.REACTION_TIME[product];
    src.forEach((m, k) => {
      m.store[kinds[k]] -= C.LAB_REACTION_AMOUNT;
      if (!m.store[kinds[k]]) delete m.store[kinds[k]];
    });
    state.reactions.push({ offset, lab: lab.id, product });
  }
  const holder = (targetId) => {
    if (targetId === state.terminal.id) return [state.terminal, (res) => C.TERMINAL_CAPACITY - used(state.terminal.store)];
    const lab = state.labs.find((l) => l.id === targetId);
    return lab ? [lab, (res) => labRoom(lab, res)] : [null, () => 0];
  };
  for (const c of state.creeps) {
    const i = state.intents.get(c.id);
    if (!i) continue;
    const capacity = capacityOf(c);
    if (i.transfer) {
      const [to, room] = holder(i.transfer.id);
      if (to && range(c, to) <= 1) {
        const r = i.transfer.resource;
        // V411, V414: the processor drops a transfer the target has no room for.
        const n = Math.min(i.transfer.amount, c.store[r] ?? 0, room(r));
        if (n <= 0) state.errors.push(`offset ${offset}: ${c.name}'s transfer of ${r} into ${to.id} finds no room`);
        to.store[r] = (to.store[r] ?? 0) + n;
        c.store[r] -= n;
        if (!c.store[r]) delete c.store[r];
      }
    }
    if (i.withdraw) {
      const [from] = holder(i.withdraw.id);
      if (from && range(c, from) <= 1) {
        const r = i.withdraw.resource;
        const n = Math.min(i.withdraw.amount, capacity - used(c.store), from.store[r] ?? 0);
        from.store[r] -= n;
        if (!from.store[r]) delete from.store[r];
        c.store[r] = (c.store[r] ?? 0) + n;
      }
    }
  }
  applyMoves(state, offset);
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  // The spawn (V173, V174): an accepted lab hauler order spawns; others are dropped.
  for (const c of state.creeps) if (c.spawning > 0) c.spawning -= 1;
  const sp = state.spawn;
  if (sp.busy !== null) {
    sp.busy.left -= 1;
    if (sp.busy.left <= 0) {
      const c = state.creeps.find((x) => x.name === sp.busy.name);
      const free = OFFSETS.slice(1).map(([dx, dy]) => ({ x: sp.x + dx, y: sp.y + dy }))
        .find((t) => !isObstacle(state, t.x, t.y) && !state.creeps.some((o) => o !== c && o.x === t.x && o.y === t.y));
      if (c && free) {
        c.x = free.x;
        c.y = free.y;
      }
      sp.busy = null;
    }
  }
  for (const call of state.spawnCalls.filter((x) => x.offset === offset && x.accepted)) {
    if (!isHauler(call.name)) continue;
    const ticks = C.CREEP_SPAWN_TIME * call.body.length;
    state.creeps.push({ id: id(state.nextId++), name: call.name, body: call.body, x: sp.x, y: sp.y, store: {}, fatigue: 0, spawning: ticks });
    sp.busy = { name: call.name, left: ticks };
  }
}

/** The engine's move resolution (V51, V52), then fatigue for the creeps that moved (V54). */
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
      const others = [...moves].filter(([o, e]) => o !== c && e.x === d.x && e.y === d.y);
      const stays = state.creeps.some((o) => o !== c && o.x === d.x && o.y === d.y && !moves.has(o));
      const edge = d.x <= 0 || d.y <= 0 || d.x >= 49 || d.y >= 49 ? 'an edge tile' : null;
      const why = edge ?? isObstacle(state, d.x, d.y) ?? (others.length ? `${others[0][0].name} moves there too` : stays ? 'a creep stays there' : null);
      if (why) {
        state.errors.push(`offset ${offset}: ${c.name}'s move to (${d.x}, ${d.y}) fails (${why})`);
        moves.delete(c);
        changed = true;
      }
    }
  }
  for (const [c, d] of moves) {
    c.x = d.x;
    c.y = d.y;
    const weight = c.body.filter((p) => p !== 'move' && p !== 'carry').length + Math.ceil(used(c.store) / C.CARRY_CAPACITY);
    c.fatigue += weight * 2;
  }
}

export function labSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let doneAt = null;
  let finished = false;
  const memoryEntries = [];
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context, w) => {
      w.segments[CONFIG_SEGMENT] ??= configRecord(CONFIG_LINES);
      state.context = context;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      memoryEntries.push(...Object.keys(state.context.Memory?.creeps ?? {}).map((n) => [offset, n]));
      if (offset === FILL_AT - 1) for (const b of BASES) state.terminal.store[b] = LOAD;
      const carried = state.creeps.reduce((s, c) => s + (c.store[UL] ?? 0), 0);
      if (doneAt === null && w.logs.some((l) => l.startsWith(DONE_PREFIX))) doneAt = offset;
      if (doneAt !== null && !carried && offset >= doneAt + LINGER) {
        finished = true;
        return SUMMARY;
      }
      return null;
    },
    until: () => finished,
  });
  const problems = [...state.errors.slice(0, 10)];
  if (state.errors.length > 10) problems.push(`... ${state.errors.length - 10} more model errors`);
  if (!finished) problems.push(`the run did not finish within ${RUN} loops (done at ${doneAt})`);
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|was refused/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);
  const last = world.ticksRun - 1;

  // The reads: all of a read or none.
  const readLoops = [];
  for (let o = 0; o <= last; o++) {
    const r = state.reads[o];
    const s = state.starts[o];
    const holding = s.labs.map((l) => (l.kind ? 1 : 0));
    const read = r.terminalStore > 0;
    const want = read ? 1 : 0;
    const ok = r.terminalStore === want && r.type.every((n) => n === want) && r.cooldown.every((n) => n === want)
      && r.store.every((n, k) => n === want * holding[k]);
    if (!ok) problems.push(`offset ${o}: lab reads ${JSON.stringify(r)}, expected all or none (labs holding ${JSON.stringify(holding)})`);
    if (read) readLoops.push(o);
  }
  // With no job (before the fill): the first loop after the boot loop, then every READ_TICKS loops.
  const idle = readLoops.filter((o) => o < FILL_AT);
  const idleWant = [];
  for (let o = 2; o < FILL_AT; o += READ_TICKS) idleWant.push(o);
  if (JSON.stringify(idle) !== JSON.stringify(idleWant)) problems.push(`reads before the fill at ${JSON.stringify(idle)}, expected ${JSON.stringify(idleWant)}`);

  // The hauler's order.
  for (const c of state.spawnCalls) {
    if (c.args !== 2 || !Array.isArray(c.body) || !isHauler(c.name)) problems.push(`spawnCreep ${JSON.stringify(c)}`);
    if (c.offset < FILL_AT) problems.push(`offset ${c.offset}: spawnCreep before the terminal held reagents`);
  }
  const accepted = state.spawnCalls.filter((c) => c.result === C.OK);
  if (accepted.length !== 1) problems.push(`${accepted.length} accepted lab hauler orders, expected 1`);
  const body = accepted[0]?.body ?? [];
  const carry = count(body, 'carry');
  const moves = count(body, 'move');
  if (carry !== HAULER_CARRY || moves !== Math.ceil(carry / (MOVE_POWER / MOVE_COST_ROAD)) || carry + moves !== body.length || cost(body) > CAPACITY) {
    problems.push(`the hauler's body ${JSON.stringify(body)}`);
  }

  // Every call: methods, targets and arguments, and every result OK.
  const terminalId = state.terminal.id;
  const labIds = state.labs.map((l) => l.id);
  const byMethod = {};
  for (const c of state.calls) {
    byMethod[c.method] = (byMethod[c.method] ?? 0) + 1;
    const isLab = labIds.includes(c.target);
    const ok = (c.method === 'move' && c.argCount === 1 && Number.isInteger(c.args[0]) && isHauler(c.actor))
      || (c.method === 'withdraw' && isHauler(c.actor) && c.target === terminalId && c.argCount === 2 && Number.isInteger(c.args[1]) && c.args[1] > 0)
      || (c.method === 'withdraw' && isHauler(c.actor) && isLab && c.argCount === 2 && c.args[1] === undefined)
      || (c.method === 'transfer' && isHauler(c.actor) && (isLab || c.target === terminalId) && c.argCount === 2 && c.args[1] === undefined)
      || (c.method === 'runReaction' && labIds.includes(c.actor) && c.argCount === 2);
    if (!ok) problems.push(`offset ${c.offset}: unexpected call ${c.method} by ${c.actor} on ${c.target} (${c.kind}) with ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) problems.push(`offset ${c.offset}: ${c.method} by ${c.actor} returned ${c.result}`);
    if (!readLoops.includes(c.offset)) problems.push(`offset ${c.offset}: ${c.method} on a loop the room was not read`);
  }

  // The reactions: by one output with both sources; on each loop it could react while the UL job ran.
  const ulReactions = state.calls.filter((c) => c.method === 'runReaction');
  const outputs = new Set(ulReactions.map((c) => c.actor));
  if (outputs.size !== 1) problems.push(`reactions by ${JSON.stringify([...outputs])}, expected one output`);
  const output = state.labs.find((l) => outputs.has(l.id));
  const sources = state.labs.filter((l) => l !== output).map((l) => l.id).sort();
  for (const c of ulReactions) {
    if (JSON.stringify([c.target, c.other].sort()) !== JSON.stringify(sources)) problems.push(`offset ${c.offset}: a reaction with ${c.target} and ${c.other}`);
  }
  const ulMade = state.reactions.filter((r) => r.product === UL);
  const period = C.REACTION_TIME[UL];
  const reactLoops = ulMade.map((r) => r.offset);
  for (let k = 1; k < reactLoops.length; k++) {
    if (reactLoops[k] - reactLoops[k - 1] < period) problems.push(`reactions at ${reactLoops[k - 1]} and ${reactLoops[k]}: closer than ${period}`);
  }
  if (!reactLoops.slice(1).some((o, k) => o - reactLoops[k] === period)) problems.push(`no two reactions ${period} loops apart: ${JSON.stringify(reactLoops)}`);
  // Every loop the output's cooldown read 0 with both sources holding a reaction's worth, while UL was being made.
  const oi = state.labs.indexOf(output);
  const si = state.labs.map((l, k) => k).filter((k) => k !== oi);
  const firstReact = reactLoops[0] ?? Infinity;
  const lastReact = reactLoops.at(-1) ?? -1;
  for (let o = firstReact; o <= lastReact; o++) {
    const s = state.starts[o];
    const could = s.labs[oi].cooldown === 0 && si.every((k) => s.labs[k].amount >= C.LAB_REACTION_AMOUNT)
      && si.every((k) => REAGENTS.includes(s.labs[k].kind));
    if (could !== reactLoops.includes(o)) problems.push(`offset ${o}: reaction ${reactLoops.includes(o)}, expected ${could} (${JSON.stringify(s.labs)})`);
  }
  // Between the hauler's last load into a source and its first unload, reads only on reaction loops or the cadence.
  const loads = state.calls.filter((c) => c.method === 'transfer' && labIds.includes(c.target) && c.offset <= lastReact).map((c) => c.offset);
  const unloads = state.calls.filter((c) => c.method === 'withdraw' && labIds.includes(c.target)).map((c) => c.offset);
  const quietFrom = Math.max(...loads, 0) + 2;
  const quietTo = Math.min(...unloads.filter((o) => o > quietFrom), lastReact);
  let lastRead = readLoops.filter((o) => o < quietFrom).at(-1) ?? 0;
  for (const o of readLoops.filter((x) => x >= quietFrom && x < quietTo)) {
    if (!reactLoops.includes(o) && o - lastRead < READ_TICKS) problems.push(`offset ${o}: a read with no reaction while the job reacted on its own`);
    lastRead = o;
  }

  // The job's lines, and nothing lost.
  const starts = world.logs.filter((l) => l === START_LINE);
  if (starts.length !== 1) problems.push(`${starts.length} "${START_LINE}" lines, expected 1: ${JSON.stringify(world.logs.filter((l) => l.startsWith('rustedscreep labs')))}`);
  const dones = world.logs.filter((l) => l.startsWith(DONE_PREFIX));
  const doneLine = `${DONE_PREFIX}${LOAD / C.LAB_REACTION_AMOUNT} reactions)`;
  if (dones.length !== 1 || dones[0] !== doneLine) problems.push(`done lines ${JSON.stringify(dones)}, expected one "${doneLine}"`);
  if (ulMade.length * C.LAB_REACTION_AMOUNT !== LOAD) problems.push(`${ulMade.length} UL reactions, expected ${LOAD / C.LAB_REACTION_AMOUNT}`);
  for (const r of REAGENTS) {
    const left = state.terminal.store[r] ?? 0;
    const inLabs = state.labs.reduce((s, l) => s + (l.store[r] ?? 0), 0);
    const carried = state.creeps.reduce((s, c) => s + (c.store[r] ?? 0), 0);
    if (left + inLabs + carried !== 0) problems.push(`${r}: the terminal holds ${left}, the labs ${inLabs}, the hauler ${carried}; expected all ${LOAD} used`);
  }
  if ((state.terminal.store[UL] ?? 0) !== LOAD) problems.push(`the terminal holds ${state.terminal.store[UL] ?? 0} UL, expected ${LOAD}`);
  if (memoryEntries.length) problems.push(`Memory.creeps kept entries after the loop: ${JSON.stringify(memoryEntries.slice(0, 4))}`);
  if (state.siteCalls.length) problems.push(`createConstructionSite calls: ${JSON.stringify(state.siteCalls.slice(0, 3))}`);

  // The summary.
  const summary = commandResults(world, SUMMARY).at(-1) ?? '';
  const parts = [
    `rustedscreep labs at tick ${T0 + last}: on (batch min ${BATCH_MIN}, hauler carry ${HAULER_CARRY}) | targets XGHO2 ${C.LAB_MINERAL_CAPACITY} XZH2O`,
    `| rooms 1 | ${ROOM}: `,
    ', wants labs; read at',
    `: ${LABS.length} labs`,
    'outputs 1',
    `jobs 2, reactions ${state.reactions.length}, refused 0`,
  ];
  for (const p of parts) if (!summary.includes(p)) problems.push(`the summary lacks "${p}": ${summary}`);

  if (problems.length) throw new Error(`lab-sim:\n  ${problems.slice(0, 25).join('\n  ')}${problems.length > 25 ? `\n  ... ${problems.length - 25} more` : ''}`);
  return {
    ticks: world.ticksRun, byMethod, readLoops: readLoops.length, reactions: state.reactions.length, ul: ulMade.length,
    hauler: `${carry}C${moves}M`, doneAt, summary,
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = labSim(process.argv[2]);
    console.log(`lab-sim ok: ${r.ticks} ticks; ${r.readLoops} read loops; hauler ${r.hauler}; ${r.ul} UL reactions ` +
      `(${r.reactions} in all), the UL job done at ${r.doneAt}; calls ${JSON.stringify(r.byMethod)} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
