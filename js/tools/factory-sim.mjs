// The factory's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/factory.rs and snapshot.rs `read_factory` and `read_terminal_stock` over rustedscreep-core
// `economy::factory`; ROADMAP P9.3, ADR-046: bars from the surplus, the haulers fill the terminal, a factory hauler, the
// factory built once it has work). W1N1 is owned at LEVEL, the first level CONTROLLER_STRUCTURES allows a factory: an
// open plain room with walls on its border ring, one spawn, the controller, no source and no mineral (so mining,
// hauling, the opening's workers and mineral mining stay inert), an own terminal and an own factory two tiles apart. As
// in lab-sim.mjs, an idle own creep whose name the bot did not give (KEEPER) stands in the room from the start, since
// movement reads a room's terrain only while an own creep stands in it. The room's energy is kept at its capacity by
// the harness (no extensions stand).
// The run starts with the overrides of CONFIG_LINES in segment 97 (the runtime config's record): the planner, the
// interim sites, remote mining, the signer, scouting, claim scoring and the labs off, so nothing else asks for the
// spawn, places sites or reads the terminal; `factory.mineral_keep` KEEP, `factory.energy_per_life` LIFE_ENERGY (a
// batch every PACE loops) and `factory.read_ticks` READ_TICKS. The record loads on the global's second loop, so the
// first read runs with the profile's values.
// The terminal holds KEEP of utrium and ENERGY at first, so the room has no job; at offset FILL_AT the harness adds
// BATCHES batches of utrium, the next due read starts the job, and the run ends LINGER loops after the job's "stopped"
// line, once the hauler carries nothing and the factory holds no bar.
// The harness plays the engine's part as far as the bot can see it:
// - the factory's `store` and `cooldown` getters count their reads (V423, V425), and so does the terminal's `store`;
//   each is one pool (V401, V425);
// - `produce(resourceType)` makes the game side's checks in V420's order and records the intent; the processor (V422,
//   V424) checks again, takes the recipe's components, adds its amount, and sets `cooldownTime` to the loop plus the
//   recipe's cooldown, so `cooldown` reads 0 again that many loops later (V423);
// - `withdraw(target, resource, amount)` and `transfer(target, resource, amount)` make the game side's checks in V400's
//   and V399's orders (no factory rule, V426) and record the intent; the processor moves what fits;
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52), with fatigue (V54, V55);
// - `spawnCreep(body, name)` makes the runtime's checks in V171's order and its `Memory.creeps[name]` write (V181); the
//   processor (V173) turns an order for a factory hauler's name (`f…`) into a creep that spawns for CREEP_SPAWN_TIME ×
//   parts loops with the spawn busy, then stands on the first free tile around the spawn in direction order (V174),
//   and drops any other order.
// It checks:
// - the factory read (the factory's `store` and `cooldown`) all on a loop or none, and with the terminal's `store` once;
//   with no job, on the first loop after the boot loop and then every READ_TICKS loops; every loop a batch or a hauler
//   call happens on is a read loop; never a terminal `store` read off a factory read (the room has no hauled source,
//   so its terminal never takes the haulers' energy and gets no bank read), and never two on a loop;
// - no `spawnCreep` before FILL_AT; then one order for a factory hauler named for W1N1, of CARRY and MOVE parts only,
//   with HAULER_CARRY CARRY parts and a MOVE part for every MOVE_POWER / MOVE_COST_ROAD of them, within CAPACITY;
// - `produce(utrium_bar)` only by the factory, only on loops its cooldown reads 0 with a batch's components in it and
//   PACE loops since the last batch, and on every such loop while the job runs; batches exactly PACE loops apart;
// - the hauler's calls: `withdraw(terminal, U|energy, amount)`, `withdraw(factory, type)` with no amount,
//   `transfer(factory|terminal, type)` with no amount, next to their targets;
// - the job's lines ("compresses its U into utrium_bar (N held, KEEP kept)", "stopped compressing its U (BATCHES
//   batches): at its reserve") once each; nothing lost: the terminal holds KEEP of utrium and BATCHES × the recipe's
//   amount of bars, the factory nothing, and every energy unit went into a batch;
// - `rustedscreep.factory()` at the end: the settings, the pace, the room and its counters;
// - every call returns OK; no fault, error or trap line, no model error, no halt, one global.
// npm run check runs it (row 29); standalone: node js/tools/factory-sim.mjs [dir].
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
/** The first level that allows a factory (CONTROLLER_STRUCTURES.factory). */
const LEVEL = [1, 2, 3, 4, 5, 6, 7, 8].find((l) => C.CONTROLLER_STRUCTURES.factory[l] >= 1);
const CAPACITY = C.CONTROLLER_STRUCTURES.spawn[LEVEL] * C.SPAWN_ENERGY_CAPACITY
  + C.CONTROLLER_STRUCTURES.extension[LEVEL] * C.EXTENSION_ENERGY_CAPACITY[LEVEL];
const SPAWN = { x: 25, y: 32 };
const CONTROLLER = { x: 40, y: 10 };
const TERMINAL = { x: 30, y: 24 };
const FACTORY = { x: 32, y: 24 };
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
const U = C.RESOURCE_UTRIUM;
const BAR = C.RESOURCE_UTRIUM_BAR;
const RECIPE = C.COMMODITIES[BAR];
const TAKE = RECIPE.components[U];
const BATCH_ENERGY = RECIPE.components[C.RESOURCE_ENERGY];
/** The fatigue a MOVE part removes a tick and a road's fatigue a part (screeps-game-api 0.23.5 extra.rs: MOVE_POWER,
 * MOVE_COST_ROAD; no engine global, V54, V55). */
const MOVE_POWER = 2;
const MOVE_COST_ROAD = 1;
/** FACTORY_MMO.hauler_carry and FACTORY_MMO.energy_keep (crates/rustedscreep-core/src/config.rs). */
const HAULER_CARRY = 10;
const ENERGY_KEEP = 1000;
const BAR_MAX = 20000;
const KEEP = 1000;
const BATCHES = 3;
const ENERGY = BATCHES * BATCH_ENERGY;
/** 50 batches a creep life: economy::factory::pace is CREEP_LIFE_TIME / 50 = 30 loops, above the recipe's cooldown. */
const LIFE_ENERGY = 50 * BATCH_ENERGY;
const PACE = Math.max(RECIPE.cooldown, Math.ceil(C.CREEP_LIFE_TIME / (LIFE_ENERGY / BATCH_ENERGY)));
const READ_TICKS = 7;
const FILL_AT = 30;
const LINGER = 10;
const RUN = 700;
const SUMMARY = 'rustedscreep.factory()';
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
  'labs.enabled=false',
  `factory.mineral_keep=${KEEP}`,
  `factory.energy_per_life=${LIFE_ENERGY}`,
  `factory.read_ticks=${READ_TICKS}`,
];
const START_LINE = `rustedscreep factory: ${ROOM} compresses its ${U} into ${BAR} (${KEEP + BATCHES * TAKE} held, ${KEEP} kept)`;
const STOP_LINE = `rustedscreep factory: ${ROOM} stopped compressing its ${U} (${BATCHES} batches): at its reserve`;

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
const count = (body, part) => body.filter((p) => p === part).length;
const cost = (body) => body.reduce((s, p) => s + C.BODYPART_COST[p], 0);
const used = (store) => Object.values(store).reduce((s, v) => s + v, 0);
const capacityOf = (c) => C.CARRY_CAPACITY * count(c.body, 'carry');
const isHauler = (name) => typeof name === 'string' && name.startsWith(`f${ROOM}-`);

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
    factory: { id: id(11), ...FACTORY, store: {}, cooldownTime: 0 },
    terminal: { id: id(4), ...TERMINAL, store: { [U]: KEEP, energy: ENERGY } },
    spawn: { id: id(5), name: 'Spawn1', ...SPAWN, busy: null },
    controller: { id: id(6), ...CONTROLLER },
    creeps: [{ id: id(7), name: KEEPER.name, body: ['move'], x: KEEPER.x, y: KEEPER.y, store: {}, fatigue: 0, spawning: 0 }],
    nextId: 30,
    /** This tick's recorded intents, object id → { method: payload }. */
    intents: new Map(),
    produceIntents: [],
    calls: [],
    spawnCalls: [],
    siteCalls: [],
    /** Per offset: the bot's reads of the factory read's getters. */
    reads: [],
    /** Per offset, the tick's start: the factory's cooldown and store. */
    starts: [],
    batches: [],
    errors: [],
    context: null,
  };
}

/** Why a creep cannot stand on (x, y), if it cannot (V52): terrain, an obstacle structure. */
function isObstacle(state, x, y) {
  if (isWall(x, y)) return 'wall';
  const here = (o) => o.x === x && o.y === y;
  if ([state.spawn, state.terminal, state.controller, state.factory].some(here)) return 'obstacle';
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
      // V399, in its order.
      const [target, resource, amount] = args;
      const rest = args.slice(1);
      if (c.spawning > 0) return record('transfer', target, rest, C.ERR_BUSY);
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('transfer', target, rest, C.ERR_INVALID_ARGS);
      const to = objects.storeOf(target);
      if (!target?.id || !to) return record('transfer', target, rest, C.ERR_INVALID_TARGET);
      const room = objects.roomFor(target);
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

/** Whether `store` holds a batch of `recipe`'s components. */
const holdsComponents = (store, recipe) => Object.entries(recipe.components).every(([r, n]) => (store[r] ?? 0) >= n);
/** Whether a batch of `recipe` fits a store of FACTORY_CAPACITY holding `store` (V425: net of its components). */
const fits = (store, recipe) => used(store) - used(recipe.components) + (recipe.amount || 1) <= C.FACTORY_CAPACITY;

function sceneFor(state, offset) {
  state.intents.clear();
  state.produceIntents = [];
  const kinds = new Map();
  const stores = new Map();
  const rooms = new Map();
  const objects = {
    kindOf: (o) => (o && typeof o === 'object' ? kinds.get(o.id) ?? null : null),
    storeOf: (o) => (o && typeof o === 'object' ? stores.get(o.id) ?? null : null),
    roomFor: (o) => rooms.get(o?.id)?.() ?? 0,
  };
  const reads = (state.reads[offset] = { factoryStore: 0, factoryCooldown: 0, terminalStore: 0 });
  const fc = state.factory;
  state.starts[offset] = {
    cooldown: Math.max(0, fc.cooldownTime - offset),
    store: { ...fc.store },
    terminal: { ...state.terminal.store },
  };
  const factory = {
    id: fc.id, structureType: 'factory', my: true, owner: { username: 'bot' }, pos: pos(fc.x, fc.y), hits: C.FACTORY_HITS, hitsMax: C.FACTORY_HITS,
    level: undefined,
    get store() {
      reads.factoryStore += 1;
      return makeStore(fc.store, { capacity: C.FACTORY_CAPACITY });
    },
    get cooldown() {
      reads.factoryCooldown += 1;
      return Math.max(0, fc.cooldownTime - offset);
    },
    produce(...args) {
      // V420, in its order (the factory is the bot's, at a level that allows it).
      const [resource] = args;
      const call = (result) => {
        state.calls.push({ offset, actor: fc.id, method: 'produce', args, argCount: args.length, result });
        return result;
      };
      if (fc.cooldownTime > offset) return call(C.ERR_TIRED);
      const recipe = C.COMMODITIES[resource];
      if (!recipe) return call(C.ERR_INVALID_ARGS);
      if (recipe.level) return call(C.ERR_INVALID_TARGET);
      if (!holdsComponents(fc.store, recipe)) return call(C.ERR_NOT_ENOUGH_RESOURCES);
      if (!fits(fc.store, recipe)) return call(C.ERR_FULL);
      state.produceIntents.push(resource);
      return call(C.OK);
    },
  };
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
  kinds.set(factory.id, 'factory');
  kinds.set(terminal.id, 'terminal');
  kinds.set(controller.id, 'controller');
  kinds.set(spawn.id, 'spawn');
  stores.set(fc.id, fc.store);
  stores.set(tm.id, tm.store);
  rooms.set(fc.id, () => C.FACTORY_CAPACITY - used(fc.store));
  rooms.set(tm.id, () => C.TERMINAL_CAPACITY - used(tm.store));
  const creeps = state.creeps.map((c) => creepObject(state, c, offset, objects));
  const owned = [controller, spawn, terminal, factory];
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

/** The processor after loop `offset`: the batch (V422, V424), the creeps' intents, moves, then the object pass. */
function advance(state, offset) {
  const fc = state.factory;
  for (const resource of state.produceIntents) {
    // V424: checked again on the live object.
    const recipe = C.COMMODITIES[resource];
    if (fc.cooldownTime > offset || !holdsComponents(fc.store, recipe) || !fits(fc.store, recipe)) {
      state.errors.push(`offset ${offset}: the processor drops a produce of ${resource}`);
      continue;
    }
    for (const [r, n] of Object.entries(recipe.components)) {
      fc.store[r] -= n;
      if (!fc.store[r]) delete fc.store[r];
    }
    fc.store[resource] = (fc.store[resource] ?? 0) + (recipe.amount || 1);
    fc.cooldownTime = offset + recipe.cooldown;
    state.batches.push({ offset, resource });
  }
  const holder = (targetId) => {
    if (targetId === state.terminal.id) return [state.terminal, () => C.TERMINAL_CAPACITY - used(state.terminal.store)];
    if (targetId === fc.id) return [fc, () => C.FACTORY_CAPACITY - used(fc.store)];
    return [null, () => 0];
  };
  for (const c of state.creeps) {
    const i = state.intents.get(c.id);
    if (!i) continue;
    const capacity = capacityOf(c);
    if (i.transfer) {
      const [to, room] = holder(i.transfer.id);
      if (to && range(c, to) <= 1) {
        const r = i.transfer.resource;
        const n = Math.min(i.transfer.amount, c.store[r] ?? 0, room());
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
  // The spawn (V173, V174): an accepted factory hauler order spawns; others are dropped.
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

export function factorySim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let stoppedAt = null;
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
      if (offset === FILL_AT - 1) state.terminal.store[U] += BATCHES * TAKE;
      const carried = state.creeps.reduce((s, c) => s + used(c.store), 0);
      if (stoppedAt === null && w.logs.includes(STOP_LINE)) stoppedAt = offset;
      if (stoppedAt !== null && !carried && !state.factory.store[BAR] && offset >= stoppedAt + LINGER) {
        finished = true;
        return SUMMARY;
      }
      return null;
    },
    until: () => finished,
  });
  const problems = [...state.errors.slice(0, 10)];
  if (state.errors.length > 10) problems.push(`... ${state.errors.length - 10} more model errors`);
  if (!finished) problems.push(`the run did not finish within ${RUN} loops (stopped at ${stoppedAt})`);
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|was refused/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);
  const last = world.ticksRun - 1;
  const batchLoops = state.batches.map((b) => b.offset);
  const firstBatch = batchLoops[0] ?? Infinity;
  const lastBatch = batchLoops.at(-1) ?? -1;

  // The reads: the factory's getters all or none, with the terminal's store once; otherwise at most a bank read.
  const readLoops = [];
  const bankLoops = [];
  for (let o = 0; o <= last; o++) {
    const r = state.reads[o];
    const read = r.factoryCooldown > 0;
    if (read && (r.factoryCooldown !== 1 || r.factoryStore !== 1 || r.terminalStore !== 1)) {
      problems.push(`offset ${o}: factory reads ${JSON.stringify(r)}, expected one of each`);
    }
    if (!read && (r.factoryStore !== 0 || r.terminalStore > 1)) problems.push(`offset ${o}: reads ${JSON.stringify(r)} off a factory read`);
    if (read) readLoops.push(o);
    else if (r.terminalStore === 1 && o > 3) bankLoops.push(o);
  }
  // With no job (before the fill): the first loop after the boot loop, then every READ_TICKS loops.
  const idle = readLoops.filter((o) => o < FILL_AT);
  const idleWant = [];
  for (let o = 2; o < FILL_AT; o += READ_TICKS) idleWant.push(o);
  if (JSON.stringify(idle) !== JSON.stringify(idleWant)) problems.push(`reads before the fill at ${JSON.stringify(idle)}, expected ${JSON.stringify(idleWant)}`);
  // The room has no source, so no hauler can fill its terminal: the bank never opens (economy::factory).
  if (bankLoops.length) problems.push(`bank reads with no hauled source: ${JSON.stringify(bankLoops.slice(0, 5))}`);

  // The hauler's order.
  for (const c of state.spawnCalls) {
    if (c.args !== 2 || !Array.isArray(c.body) || !isHauler(c.name)) problems.push(`spawnCreep ${JSON.stringify(c)}`);
    if (c.offset < FILL_AT) problems.push(`offset ${c.offset}: spawnCreep before the terminal held a surplus`);
  }
  const accepted = state.spawnCalls.filter((c) => c.result === C.OK);
  if (accepted.length !== 1) problems.push(`${accepted.length} accepted factory hauler orders, expected 1`);
  const body = accepted[0]?.body ?? [];
  const carry = count(body, 'carry');
  const moves = count(body, 'move');
  if (carry !== HAULER_CARRY || moves !== Math.ceil(carry / (MOVE_POWER / MOVE_COST_ROAD)) || carry + moves !== body.length || cost(body) > CAPACITY) {
    problems.push(`the hauler's body ${JSON.stringify(body)}`);
  }

  // Every call: methods, targets and arguments, and every result OK.
  const terminalId = state.terminal.id;
  const factoryId = state.factory.id;
  const byMethod = {};
  for (const c of state.calls) {
    byMethod[c.method] = (byMethod[c.method] ?? 0) + 1;
    const ok = (c.method === 'move' && c.argCount === 1 && Number.isInteger(c.args[0]) && isHauler(c.actor))
      || (c.method === 'withdraw' && isHauler(c.actor) && c.target === terminalId && c.argCount === 2
        && [U, C.RESOURCE_ENERGY].includes(c.args[0]) && Number.isInteger(c.args[1]) && c.args[1] > 0)
      || (c.method === 'withdraw' && isHauler(c.actor) && c.target === factoryId && c.argCount === 2 && c.args[1] === undefined)
      || (c.method === 'transfer' && isHauler(c.actor) && [factoryId, terminalId].includes(c.target) && c.argCount === 2 && c.args[1] === undefined)
      || (c.method === 'produce' && c.actor === factoryId && c.argCount === 1 && c.args[0] === BAR);
    if (!ok) problems.push(`offset ${c.offset}: unexpected call ${c.method} by ${c.actor} on ${c.target} (${c.kind}) with ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) problems.push(`offset ${c.offset}: ${c.method} by ${c.actor} returned ${c.result}`);
    if (!readLoops.includes(c.offset)) problems.push(`offset ${c.offset}: ${c.method} on a loop the room was not read`);
  }

  // The batches: on each loop the factory could make one while the job ran, PACE loops apart.
  if (state.batches.length !== BATCHES) problems.push(`${state.batches.length} batches, expected ${BATCHES}`);
  for (let k = 1; k < batchLoops.length; k++) {
    if (batchLoops[k] - batchLoops[k - 1] !== PACE) problems.push(`batches at ${batchLoops[k - 1]} and ${batchLoops[k]}: not ${PACE} apart`);
  }
  let lastAt = null;
  for (let o = firstBatch; o <= lastBatch; o++) {
    const s = state.starts[o];
    const could = s.cooldown === 0 && holdsComponents(s.store, RECIPE) && (lastAt === null || o - lastAt >= PACE);
    const did = batchLoops.includes(o);
    if (could !== did) problems.push(`offset ${o}: a batch ${did}, expected ${could} (${JSON.stringify(s)})`);
    if (did) lastAt = o;
  }

  // The job's lines, and nothing lost.
  const lines = world.logs.filter((l) => l.startsWith('rustedscreep factory'));
  if (world.logs.filter((l) => l === START_LINE).length !== 1) problems.push(`expected one "${START_LINE}": ${JSON.stringify(lines)}`);
  if (world.logs.filter((l) => l === STOP_LINE).length !== 1) problems.push(`expected one "${STOP_LINE}": ${JSON.stringify(lines)}`);
  const tm = state.terminal.store;
  if ((tm[U] ?? 0) !== KEEP) problems.push(`the terminal holds ${tm[U] ?? 0} ${U}, expected ${KEEP}`);
  if ((tm[BAR] ?? 0) !== BATCHES * RECIPE.amount) problems.push(`the terminal holds ${tm[BAR] ?? 0} ${BAR}, expected ${BATCHES * RECIPE.amount}`);
  if (used(state.factory.store)) problems.push(`the factory still holds ${JSON.stringify(state.factory.store)}`);
  if (tm.energy) problems.push(`the terminal kept ${tm.energy} energy, expected every unit in a batch`);
  if (memoryEntries.length) problems.push(`Memory.creeps kept entries after the loop: ${JSON.stringify(memoryEntries.slice(0, 4))}`);
  if (state.siteCalls.length) problems.push(`createConstructionSite calls: ${JSON.stringify(state.siteCalls.slice(0, 3))}`);

  // The summary.
  const summary = commandResults(world, SUMMARY).at(-1) ?? '';
  const lifeEnergy = Math.ceil(C.CREEP_LIFE_TIME / PACE) * BATCH_ENERGY;
  const parts = [
    `rustedscreep factory at tick ${T0 + last}: on (keep ${KEEP}, bar cap ${BAR_MAX}, energy keep ${ENERGY_KEEP}, energy a life ${LIFE_ENERGY}, hauler carry ${HAULER_CARRY})`,
    `| a batch every ${PACE} ticks (${lifeEnergy} energy a life)`,
    `| rooms 1 | ${ROOM}: idle, wants a factory; read at`,
    `${BAR} ${BATCHES * RECIPE.amount}`,
    `jobs 1, batches ${BATCHES}, refused 0`,
  ];
  for (const p of parts) if (!summary.includes(p)) problems.push(`the summary lacks "${p}": ${summary}`);

  if (problems.length) throw new Error(`factory-sim:\n  ${problems.slice(0, 25).join('\n  ')}${problems.length > 25 ? `\n  ... ${problems.length - 25} more` : ''}`);
  return {
    ticks: world.ticksRun, byMethod, readLoops: readLoops.length, batches: batchLoops,
    hauler: `${carry}C${moves}M`, stoppedAt, summary,
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = factorySim(process.argv[2]);
    console.log(`factory-sim ok: ${r.ticks} ticks; ${r.readLoops} read loops; hauler ${r.hauler}; ` +
      `batches at ${JSON.stringify(r.batches)}, stopped at ${r.stoppedAt}; calls ${JSON.stringify(r.byMethod)} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
