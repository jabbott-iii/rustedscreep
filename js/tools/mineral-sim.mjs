// Mineral mining's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/minerals.rs and snapshot.rs `push_mineral` over rustedscreep-core `economy::minerals`;
// ROADMAP P9.1, ADR-044: a mineral miner on the container next to the mineral, mineral haulers to the terminal, under a
// stock cap). W1N1 is owned at LEVEL, the first level CONTROLLER_STRUCTURES allows an extractor and a terminal (V393):
// an open plain room with walls on its border ring, one spawn, the controller, no source (so mining, hauling and the
// opening's workers stay inert), a hydrogen mineral with an own extractor on it, a container next to it and an own
// terminal. As in scout-sim.mjs, an idle own creep whose name the bot did not give (KEEPER) stands in the room from the
// start, since movement reads a room's terrain only while an own creep stands in it. The room's energy is kept at its
// capacity by the harness (no extensions stand; `energyAvailable` and `energyCapacityAvailable` are CAPACITY).
// The run starts with the overrides of CONFIG_LINES in segment 97 (the runtime config's record): the planner, the
// interim extension, container and link sites, remote mining, the signer, scouting and claim scoring off, so nothing
// else asks for the spawn or places sites; `minerals.read_ticks` READ_TICKS and `minerals.stock_max` STOCK_MAX. The
// record loads on the global's second loop, so the first read runs with the profile's values.
// The terminal holds MMO_STOCK_MAX of the mineral at first (the profile's cap, above STOCK_MAX), so the room does not
// mine under either value; at offset EMPTY_AT the harness empties it, and the next due read starts the mining; the run
// ends LINGER loops after the room stopped at the cap and the last load is delivered.
// The harness plays the engine's part as far as the bot can see it:
// - the mineral's `mineralAmount`, `ticksToRegeneration` and `mineralType` getters count their reads (V391, V394); the
//   extractor's `cooldown`, and the terminal's and the container's `store` and its `getUsedCapacity`,
//   `getFreeCapacity` arguments are counted likewise (makeStore, V196, V397; one pool each, V401);
// - `harvest(mineral)` makes the game side's checks in V385's order (no WORK part, a target that is not this tick's
//   mineral, no amount, not next to it, no extractor on its tile, an extractor of another owner, the extractor's
//   cooldown) and records the intent; the processor (V386, V387) takes min(HARVEST_MINERAL_POWER per WORK part, the
//   amount), and what the creep's store cannot hold goes into the container on its tile (V193), the rest onto a pile
//   (a model error here: the bot must never cause one); the harvest sets the extractor's cooldown to EXTRACTOR_COOLDOWN,
//   which this tick's object pass does not count down (V388, V389);
// - `withdraw(target, resource, amount)` and `transfer(target, resource, amount)` make the game side's checks in V400's
//   and V399's orders and record the intent; the processor moves what fits (V402: one pool for a creep's store);
// - `move` makes the game side's checks (V108) and moves resolve like the engine (V51, V52), with fatigue (V54, V55);
// - `spawnCreep(body, name)` makes the runtime's checks in V171's order and its `Memory.creeps[name]` write (V181); the
//   processor (V173) turns an order for a mineral miner's or hauler's name (`e…`, `t…`) into a creep that spawns for
//   CREEP_SPAWN_TIME × parts loops with the spawn busy, then stands on the first free tile around the spawn in direction
//   order (V174), and drops any other order.
// It checks:
// - the mineral read (its three getters, the extractor's `cooldown`, the terminal's `store` with `getUsedCapacity('H')`
//   and `getFreeCapacity()`, and the container's `getUsedCapacity('H')` and `getUsedCapacity()`) once on each loop the
//   room is read, and on no other: the first loop after the boot loop, every loop after one whose mineral creeps out of
//   the spawn could act (the room mining, its container holding some H at the last read, or a hauler carrying some;
//   `MineralRoom::can_act`), and otherwise READ_TICKS loops after the last read (`Minerals::reads`);
// - no `spawnCreep` while the terminal holds STOCK_MAX; once it is emptied, on the first read loop the "mines its H"
//   line and one order for a mineral miner (WORK and MOVE parts only, at most `minerals.max_work` WORK, within CAPACITY),
//   then, once it lives, mineral haulers (CARRY and MOVE parts only, within CAPACITY, none before the miner's order),
//   each named for W1N1, and never a second miner;
// - the miner walks onto the container and calls `harvest(mineral)` with no other argument only on loops it stands there
//   with the extractor's cooldown at 0 at the tick's start and the container able to take a whole harvest, every such
//   loop while the room mines; consecutive harvests come HARVEST_PERIOD (EXTRACTOR_COOLDOWN + 1) loops apart while the
//   container has room; none once the room stopped;
// - the haulers call `withdraw(container, 'H')` and `transfer(terminal, 'H')` with no amount, next to their targets;
// - the room stops at the cap with one "stopped mining: terminal stocked" line, and nothing is lost: what the mineral
//   gave equals what the terminal gained, the container holds and the haulers carry;
// - `rustedscreep.minerals()` at the end: the room `terminal stocked`, its plan's miner WORK count the order's, and its
//   counters (harvests, withdraws, transfers) the harness's calls;
// - every call returns OK; no fault, error or trap line, no model error, no halt, one global.
// npm run check runs it (row 27); standalone: node js/tools/mineral-sim.mjs [dir].
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
/** The first level that allows the extractor and the terminal (V393). */
const LEVEL = [1, 2, 3, 4, 5, 6, 7, 8].find((l) => C.CONTROLLER_STRUCTURES.extractor[l] > 0 && C.CONTROLLER_STRUCTURES.terminal[l] > 0);
const CAPACITY = C.CONTROLLER_STRUCTURES.spawn[LEVEL] * C.SPAWN_ENERGY_CAPACITY
  + C.CONTROLLER_STRUCTURES.extension[LEVEL] * C.EXTENSION_ENERGY_CAPACITY[LEVEL];
const SPAWN = { x: 20, y: 20 };
const CONTROLLER = { x: 40, y: 10 };
const MINERAL = { x: 10, y: 10 };
const CONTAINER = { x: 11, y: 11 };
const TERMINAL = { x: 16, y: 14 };
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
const KIND = C.RESOURCE_HYDROGEN;
const AMOUNT = C.MINERAL_DENSITY[C.DENSITY_MODERATE];
/** Ticks between two harvests of one mineral (V389): economy::minerals::HARVEST_PERIOD. */
const HARVEST_PERIOD = C.EXTRACTOR_COOLDOWN + 1;
/** MINERALS_MMO.max_work (crates/rustedscreep-core/src/config.rs). */
const MAX_WORK = 24;
const READ_TICKS = 7;
const STOCK_MAX = 300;
/** MINERALS_MMO.stock_max (crates/rustedscreep-core/src/config.rs): the cap before the overrides load. */
const MMO_STOCK_MAX = 100000;
/** The harness empties the terminal here, past a few due reads with no mining. */
const EMPTY_AT = 30;
const LINGER = 10;
const RUN = 900;
const SUMMARY = 'rustedscreep.minerals()';
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
  `minerals.stock_max=${STOCK_MAX}`,
  `minerals.read_ticks=${READ_TICKS}`,
];
const START_LINE = `rustedscreep minerals: ${ROOM} mines its H (${AMOUNT} left)`;
const STOP_LINE = `rustedscreep minerals: ${ROOM} stopped mining: terminal stocked`;

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
const count = (body, part) => body.filter((p) => p === part).length;
const cost = (body) => body.reduce((s, p) => s + C.BODYPART_COST[p], 0);

/** Row-major terrain (V156): walls on the border ring and under the mineral (V228). */
const RAW = (() => {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) if (x === 0 || y === 0 || x === 49 || y === 49) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  raw[MINERAL.y * 50 + MINERAL.x] = C.TERRAIN_MASK_WALL;
  return raw;
})();
const isWall = (x, y) => (RAW[y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

function makeWorld() {
  return {
    mineral: { id: id(1), ...MINERAL, amount: AMOUNT },
    extractor: { id: id(2), ...MINERAL, cooldown: 0 },
    container: { id: id(3), ...CONTAINER, store: { [KIND]: 0 } },
    terminal: { id: id(4), ...TERMINAL, store: { [KIND]: MMO_STOCK_MAX } },
    spawn: { id: id(5), name: 'Spawn1', ...SPAWN, busy: null },
    controller: { id: id(6), ...CONTROLLER },
    creeps: [{ id: id(7), name: KEEPER.name, body: ['move'], x: KEEPER.x, y: KEEPER.y, store: {}, fatigue: 0, spawning: 0 }],
    nextId: 20,
    /** This tick's recorded intents, object id → { method: payload }. */
    intents: new Map(),
    calls: [],
    spawnCalls: [],
    siteCalls: [],
    /** Per offset: the bot's reads of the mineral read's getters. */
    reads: [],
    /** Per offset, the tick's start: the extractor's cooldown, the container's stock, the creeps' places. */
    starts: [],
    harvested: 0,
    errors: [],
    context: null,
  };
}

const used = (store) => Object.values(store).reduce((s, v) => s + v, 0);
const capacityOf = (c) => C.CARRY_CAPACITY * count(c.body, 'carry');

/** Why a creep cannot stand on (x, y), if it cannot (V52): terrain, the mineral, an obstacle structure. */
function isObstacle(state, x, y) {
  if (isWall(x, y)) return 'wall';
  const here = (o) => o.x === x && o.y === y;
  if ([state.mineral, state.spawn, state.terminal, state.controller].some(here)) return 'obstacle';
  return null;
}

function intentsOf(state, objectId) {
  if (!state.intents.has(objectId)) state.intents.set(objectId, {});
  return state.intents.get(objectId);
}

/** A store whose `getUsedCapacity` and `getFreeCapacity` record their arguments into `log`. */
function countedStore(contents, capacity, log) {
  const inner = makeStore(contents, { capacity });
  return new Proxy(inner, {
    get(target, prop) {
      if (prop === 'getUsedCapacity' || prop === 'getFreeCapacity') {
        return (res) => {
          log.push(`${prop}(${res === undefined ? '' : res})`);
          return target[prop](res);
        };
      }
      return target[prop];
    },
  });
}

function creepObject(state, c, offset, objects) {
  const work = count(c.body, 'work');
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
    harvest(...args) {
      // V385, in its order.
      const [target] = args;
      const rest = args.slice(1);
      if (c.spawning > 0) return record('harvest', target, rest, C.ERR_BUSY);
      if (!work) return record('harvest', target, rest, C.ERR_NO_BODYPART);
      if (!target?.id || objects.kindOf(target) !== 'mineral') return record('harvest', target, rest, C.ERR_INVALID_TARGET);
      if (!state.mineral.amount) return record('harvest', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (!near(target)) return record('harvest', target, rest, C.ERR_NOT_IN_RANGE);
      const ex = state.extractor;
      if (!ex || ex.x !== state.mineral.x || ex.y !== state.mineral.y) return record('harvest', target, rest, C.ERR_NOT_FOUND);
      if (ex.cooldown) return record('harvest', target, rest, C.ERR_TIRED);
      intentsOf(state, c.id).harvest = { id: target.id };
      return record('harvest', target, rest, C.OK);
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
      const cap = objects.capacityOf(target);
      if (!near(target)) return record('transfer', target, rest, C.ERR_NOT_IN_RANGE);
      if (!c.store[resource]) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (used(to) >= cap) return record('transfer', target, rest, C.ERR_FULL);
      const n = amount || Math.min(c.store[resource], cap - used(to));
      if (c.store[resource] < n) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (n + used(to) > cap) return record('transfer', target, rest, C.ERR_FULL);
      intentsOf(state, c.id).transfer = { id: target.id, resource, amount: n };
      return record('transfer', target, rest, C.OK);
    },
  };
  // The act layer's other bindings: nothing here calls for them; recorded (OK) so they show up as unexpected.
  for (const m of ['say', 'drop', 'heal', 'rangedHeal', 'rangedAttack', 'repair', 'pull', 'build', 'pickup', 'upgradeController', 'suicide']) {
    self[m] = (...args) => record(m, args[0], args.slice(1), C.OK);
  }
  return self;
}

function sceneFor(state, offset) {
  state.intents.clear();
  const kinds = new Map();
  const stores = new Map();
  const capacities = new Map();
  const objects = {
    kindOf: (o) => (o && typeof o === 'object' ? kinds.get(o.id) ?? null : null),
    storeOf: (o) => (o && typeof o === 'object' ? stores.get(o.id) ?? null : null),
    capacityOf: (o) => capacities.get(o?.id) ?? 0,
  };
  const reads = (state.reads[offset] = { amount: 0, regen: 0, type: 0, cooldown: 0, terminal: [], terminalStore: 0, container: [] });
  const m = state.mineral;
  const mineral = {
    id: m.id, pos: pos(m.x, m.y), density: C.DENSITY_MODERATE,
    get mineralAmount() {
      reads.amount += 1;
      return m.amount;
    },
    get ticksToRegeneration() {
      reads.regen += 1;
      return undefined;
    },
    get mineralType() {
      reads.type += 1;
      return KIND;
    },
  };
  const ex = state.extractor;
  const extractor = {
    id: ex.id, structureType: 'extractor', my: true, owner: { username: 'bot' }, pos: pos(ex.x, ex.y), hits: C.EXTRACTOR_HITS, hitsMax: C.EXTRACTOR_HITS,
    get cooldown() {
      reads.cooldown += 1;
      return ex.cooldown;
    },
  };
  const ct = state.container;
  const container = {
    id: ct.id, structureType: 'container', pos: pos(ct.x, ct.y), hits: C.CONTAINER_HITS, hitsMax: C.CONTAINER_HITS,
    store: countedStore(ct.store, C.CONTAINER_CAPACITY, reads.container),
  };
  const tm = state.terminal;
  const terminalStore = countedStore(tm.store, C.TERMINAL_CAPACITY, reads.terminal);
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
    // V175: null when idle.
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
  kinds.set(mineral.id, 'mineral');
  kinds.set(extractor.id, 'extractor');
  kinds.set(container.id, 'container');
  kinds.set(terminal.id, 'terminal');
  kinds.set(controller.id, 'controller');
  kinds.set(spawn.id, 'spawn');
  stores.set(ct.id, ct.store);
  stores.set(tm.id, tm.store);
  capacities.set(ct.id, C.CONTAINER_CAPACITY);
  capacities.set(tm.id, C.TERMINAL_CAPACITY);
  const creeps = state.creeps.map((c) => creepObject(state, c, offset, objects));
  const owned = [controller, spawn, extractor, terminal];
  const lists = {
    [C.FIND_MINERALS]: [mineral], [C.FIND_SOURCES]: [], [C.FIND_STRUCTURES]: [...owned, container], [C.FIND_MY_STRUCTURES]: owned,
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
  state.starts[offset] = {
    cooldown: ex.cooldown,
    container: used(ct.store),
    miners: state.creeps.filter((c) => c.name.startsWith(`e${ROOM}-`)).map((c) => ({ name: c.name, x: c.x, y: c.y, spawning: c.spawning > 0 })),
    /** Mineral creeps out of the spawn, and whether one of the haulers among them carries the mineral. */
    out: state.creeps.filter((c) => /^[et]W1N1-/.test(c.name) && !(c.spawning > 0)).length,
    carrying: state.creeps.some((c) => c.name.startsWith(`t${ROOM}-`) && !(c.spawning > 0) && (c.store[KIND] ?? 0) > 0),
  };
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries(owned.map((s) => [s.id, s])),
  };
}

/** The processor after loop `offset`: intents in creepActions order (V45), moves, then the object pass. */
function advance(state, offset) {
  let harvested = false;
  const holder = (targetId) => (targetId === state.container.id ? [state.container.store, C.CONTAINER_CAPACITY]
    : targetId === state.terminal.id ? [state.terminal.store, C.TERMINAL_CAPACITY] : [null, 0]);
  for (const c of state.creeps) {
    const i = state.intents.get(c.id);
    if (!i) continue;
    const capacity = capacityOf(c);
    if (i.transfer) {
      const [to, cap] = holder(i.transfer.id);
      if (to && range(c, state.terminal) <= 1) {
        const n = Math.min(i.transfer.amount, c.store[i.transfer.resource] ?? 0, cap - used(to));
        to[i.transfer.resource] = (to[i.transfer.resource] ?? 0) + n;
        c.store[i.transfer.resource] -= n;
      }
    }
    if (i.withdraw) {
      const [from] = holder(i.withdraw.id);
      if (from && range(c, state.container) <= 1) {
        const n = Math.min(i.withdraw.amount, capacity - used(c.store), from[i.withdraw.resource] ?? 0);
        from[i.withdraw.resource] -= n;
        c.store[i.withdraw.resource] = (c.store[i.withdraw.resource] ?? 0) + n;
      }
    }
    if (i.harvest) {
      // V386, V387: the processor's guards, the amount, and the overflow into the container on the creep's tile.
      const mn = state.mineral;
      if (mn.amount && range(c, mn) <= 1 && !state.extractor.cooldown) {
        const n = Math.min(mn.amount, C.HARVEST_MINERAL_POWER * count(c.body, 'work'));
        mn.amount -= n;
        state.harvested += n;
        harvested = true;
        const over = Math.max(0, n - (capacity - used(c.store)));
        c.store[KIND] = (c.store[KIND] ?? 0) + n - over;
        const ct = state.container;
        const kept = ct.x === c.x && ct.y === c.y ? Math.min(over, C.CONTAINER_CAPACITY - used(ct.store)) : 0;
        ct.store[KIND] += kept;
        if (kept < over) state.errors.push(`offset ${offset}: ${c.name} dropped ${over - kept} ${KIND} on a pile`);
      }
    }
  }
  applyMoves(state, offset);
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  // V389: a harvest sets the cooldown; this pass counts down only a cooldown that was already there.
  if (harvested) state.extractor.cooldown = C.EXTRACTOR_COOLDOWN;
  else if (state.extractor.cooldown > 0) state.extractor.cooldown -= 1;
  // The spawn (V173, V174): an accepted mineral order spawns; others are dropped.
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
    if (!/^[et]W1N1-/.test(call.name)) continue;
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

export function mineralSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let stoppedAt = null;
  let startedAt = null;
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
      if (offset === EMPTY_AT - 1) state.terminal.store[KIND] = 0;
      const carried = state.creeps.reduce((s, c) => s + (c.store[KIND] ?? 0), 0);
      if (startedAt === null && w.logs.includes(START_LINE)) startedAt = offset;
      if (stoppedAt === null && offset > EMPTY_AT && w.logs.includes(STOP_LINE)) stoppedAt = offset;
      if (stoppedAt !== null && !used(state.container.store) && !carried && offset >= stoppedAt + LINGER) {
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
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);
  const last = world.ticksRun - 1;

  // The reads: once on each loop the room is read (Minerals::reads), none on any other. A loop is read after one whose
  // mineral creeps out of the spawn could act (the room mining, the container holding some H at the last read, or a
  // hauler carrying some; MineralRoom::can_act), and otherwise READ_TICKS loops after the last read.
  const mining = (x) => startedAt !== null && x >= startedAt && (stoppedAt === null || x < stoppedAt);
  let lastRead = null;
  let seenContainer = 0;
  const readLoops = [];
  for (let o = 1; o <= last; o++) {
    const prev = state.starts[o - 1];
    const canAct = (prev?.out ?? 0) > 0 && (mining(o - 1) || seenContainer > 0 || prev.carrying);
    const want = o >= 2 && (lastRead === null || canAct || o - lastRead >= READ_TICKS);
    const r = state.reads[o];
    const n = want ? 1 : 0;
    const terminalUsed = r.terminal.filter((a) => a === `getUsedCapacity(${KIND})`).length;
    const terminalFree = r.terminal.filter((a) => a === 'getFreeCapacity()').length;
    const containerH = r.container.filter((a) => a === `getUsedCapacity(${KIND})`).length;
    const containerAll = r.container.filter((a) => a === 'getUsedCapacity()').length;
    const got = [r.amount, r.regen, r.type, r.cooldown, r.terminalStore, terminalUsed, terminalFree, containerH, containerAll];
    if (got.some((g) => g !== n) || r.terminal.length !== 2 * n) {
      problems.push(`offset ${o}: mineral reads ${JSON.stringify(r)}, expected ${n} of each`);
    }
    if (want) {
      lastRead = o;
      seenContainer = state.starts[o].container;
      readLoops.push(o);
    }
  }

  // The orders: none while the terminal is stocked; then one miner and the haulers.
  const minerCalls = state.spawnCalls.filter((c) => c.name?.startsWith(`e${ROOM}-`));
  const haulerCalls = state.spawnCalls.filter((c) => c.name?.startsWith(`t${ROOM}-`));
  for (const c of state.spawnCalls) {
    if (c.args !== 2 || !Array.isArray(c.body) || !/^[et]W1N1-/.test(c.name ?? '')) problems.push(`spawnCreep ${JSON.stringify(c)}`);
    if (c.offset < EMPTY_AT) problems.push(`offset ${c.offset}: spawnCreep while the terminal held the cap`);
  }
  const accepted = (calls) => calls.filter((c) => c.result === C.OK);
  const firstRead = readLoops.find((o) => o >= EMPTY_AT);
  if (accepted(minerCalls).length !== 1) problems.push(`${accepted(minerCalls).length} accepted miner orders, expected 1`);
  if (minerCalls[0]?.offset !== firstRead) problems.push(`the miner was ordered at ${minerCalls[0]?.offset}, expected on the first read after ${EMPTY_AT} (${firstRead})`);
  const minerBody = minerCalls[0]?.body ?? [];
  const minerWork = count(minerBody, 'work');
  if (!minerWork || minerWork > MAX_WORK || count(minerBody, 'move') < 1 || minerWork + count(minerBody, 'move') !== minerBody.length || cost(minerBody) > CAPACITY) {
    problems.push(`the miner's body ${JSON.stringify(minerBody)}`);
  }
  if (!accepted(haulerCalls).length) problems.push('no hauler order was accepted');
  if (haulerCalls.some((c) => c.offset <= (minerCalls[0]?.offset ?? Infinity))) problems.push(`a hauler order at or before the miner's: ${JSON.stringify(haulerCalls.map((c) => c.offset))}`);
  for (const c of haulerCalls) {
    const carry = count(c.body, 'carry');
    if (!carry || count(c.body, 'move') < 1 || carry + count(c.body, 'move') !== c.body.length || cost(c.body) > CAPACITY) problems.push(`a hauler's body ${JSON.stringify(c.body)}`);
  }
  const starts = world.logs.filter((l) => l === START_LINE);
  if (starts.length !== 1) problems.push(`${starts.length} "${START_LINE}" lines, expected 1`);
  const stops = world.logs.filter((l) => l.startsWith(`rustedscreep minerals: ${ROOM} stopped mining`));
  if (stops.length !== 1 || stops[0] !== STOP_LINE) problems.push(`stop lines ${JSON.stringify(stops)}, expected one "${STOP_LINE}"`);

  // Every call: the creeps' methods and targets, and every result OK.
  const miner = minerCalls[0]?.name;
  const isHauler = (name) => name?.startsWith(`t${ROOM}-`);
  const byMethod = {};
  for (const c of state.calls) {
    byMethod[c.method] = (byMethod[c.method] ?? 0) + 1;
    const ok = (c.method === 'move' && c.argCount === 1 && Number.isInteger(c.args[0]) && (c.actor === miner || isHauler(c.actor)))
      || (c.method === 'harvest' && c.actor === miner && c.target === state.mineral.id && c.argCount === 0)
      || (c.method === 'withdraw' && isHauler(c.actor) && c.target === state.container.id && c.argCount === 2 && c.args[0] === KIND && c.args[1] === undefined)
      || (c.method === 'transfer' && isHauler(c.actor) && c.target === state.terminal.id && c.argCount === 2 && c.args[0] === KIND && c.args[1] === undefined);
    if (!ok) problems.push(`offset ${c.offset}: unexpected call ${c.method} by ${c.actor} on ${c.target} (${c.kind}) with ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) problems.push(`offset ${c.offset}: ${c.method} by ${c.actor} returned ${c.result}`);
  }

  // The miner's harvests: on every loop it stands on the container with the cooldown at 0, while the room mines and the
  // container can take a whole harvest; on no other.
  const harvests = state.calls.filter((c) => c.method === 'harvest').map((c) => c.offset);
  const per = C.HARVEST_MINERAL_POWER * minerWork;
  for (let o = 1; o <= last; o++) {
    const s = state.starts[o];
    const m = s.miners[0];
    const mining = o >= (firstRead ?? Infinity) && (stoppedAt === null || o < stoppedAt);
    const want = Boolean(mining && m && !m.spawning && m.x === CONTAINER.x && m.y === CONTAINER.y && s.cooldown === 0
      && C.CONTAINER_CAPACITY - s.container >= per);
    const got = harvests.includes(o);
    if (want !== got && !(o === stoppedAt)) problems.push(`offset ${o}: harvest ${got}, expected ${want} (cooldown ${s.cooldown}, container ${s.container}, miner ${JSON.stringify(m)})`);
  }
  if (harvests.length < 3) problems.push(`harvests at ${JSON.stringify(harvests)}, expected several`);
  for (let k = 1; k < harvests.length; k++) {
    const gap = harvests[k] - harvests[k - 1];
    if (gap < HARVEST_PERIOD) problems.push(`harvests at ${harvests[k - 1]} and ${harvests[k]}: closer than ${HARVEST_PERIOD}`);
  }
  if (!harvests.slice(1).some((o, k) => o - harvests[k] === HARVEST_PERIOD)) problems.push(`no two harvests ${HARVEST_PERIOD} loops apart: ${JSON.stringify(harvests)}`);
  if (stoppedAt !== null && harvests.some((o) => o > stoppedAt)) problems.push(`a harvest after the stop at ${stoppedAt}`);

  // Nothing lost; the terminal at or past the cap.
  const carried = state.creeps.reduce((s, c) => s + (c.store[KIND] ?? 0), 0);
  const gained = state.terminal.store[KIND];
  if (state.harvested !== AMOUNT - state.mineral.amount) problems.push(`harvested ${state.harvested}, the mineral lost ${AMOUNT - state.mineral.amount}`);
  if (state.harvested !== gained + state.container.store[KIND] + carried) {
    problems.push(`harvested ${state.harvested}, but the terminal gained ${gained}, the container holds ${state.container.store[KIND]} and the haulers carry ${carried}`);
  }
  if (gained < STOCK_MAX) problems.push(`the terminal ends at ${gained}, below the cap ${STOCK_MAX}`);
  if (memoryEntries.length) problems.push(`Memory.creeps kept entries after the loop: ${JSON.stringify(memoryEntries.slice(0, 4))}`);
  if (state.siteCalls.length) problems.push(`createConstructionSite calls: ${JSON.stringify(state.siteCalls.slice(0, 3))}`);

  // The summary.
  const summary = commandResults(world, SUMMARY).at(-1) ?? '';
  const count1 = (method) => state.calls.filter((c) => c.method === method).length;
  const parts = [
    `rustedscreep minerals at tick ${T0 + last}: on (stock cap ${STOCK_MAX}, terminal free min ${C.CONTAINER_CAPACITY}, max work ${MAX_WORK}) | rooms 1 | ${ROOM}: terminal stocked;`,
    `; plan: miner ${minerWork} WORK (${cost(minerBody)} energy,`,
    `harvests ${count1('harvest')}, withdraws ${count1('withdraw')}, transfers ${count1('transfer')}`,
  ];
  for (const p of parts) if (!summary.includes(p)) problems.push(`the summary lacks "${p}": ${summary}`);

  if (problems.length) throw new Error(`mineral-sim:\n  ${problems.slice(0, 25).join('\n  ')}${problems.length > 25 ? `\n  ... ${problems.length - 25} more` : ''}`);
  return {
    ticks: world.ticksRun, byMethod, readLoops: readLoops.length, firstRead, stoppedAt, harvests: harvests.length,
    harvested: state.harvested, terminal: gained, minerBody: `${minerWork} WORK ${count(minerBody, 'move')} MOVE`,
    haulers: haulerCalls.map((c) => `${count(c.body, 'carry')}C${count(c.body, 'move')}M`).join(' '), summary,
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = mineralSim(process.argv[2]);
    console.log(`mineral-sim ok: ${r.ticks} ticks; ${r.readLoops} read loops, mining from ${r.firstRead} to ${r.stoppedAt}; ` +
      `miner ${r.minerBody}, haulers ${r.haulers}; ${r.harvests} harvests (${r.harvested} H), terminal ${r.terminal}; ` +
      `calls ${JSON.stringify(r.byMethod)} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
