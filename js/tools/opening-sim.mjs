// The RCL1–3 opening end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/opening.rs over rustedscreep-core `economy::opening` and `economy::placement`, with
// the spawn path and movement). A zero-creep recovery: W1N1 owned at RCL1 (an open plain room, walls on its border
// ring), one spawn holding SPAWN_ENERGY_CAPACITY, one source with SOURCE_ENERGY_CAPACITY and open neighbours, the
// controller, and no creep. The harness plays the engine's part as far as the bot can see it:
// - spawnCreep follows the runtime's checks and return codes (V170, V171), writes `Memory.creeps[name]` on OK
//   (V181), and the processor charges the spawn first, then the extensions nearest it, makes the creep at the spawn
//   (spawning) and places it next to the spawn CREEP_SPAWN_TIME per part later, on the first free of directions 1
//   to 8 (V173, V174); the spawn regains 1 energy a tick while it and the room hold less than SPAWN_ENERGY_CAPACITY;
// - the creeps' methods make the game side's checks against the tick's start (V108, V182, V185, V187, V190 to V192)
//   and record the intent; after the loop the processor applies them: harvest takes min(HARVEST_POWER per WORK part,
//   the source's energy) and drops what overflows the store on the creep's tile (V183), the source refills
//   ENERGY_REGEN_TIME ticks after it first drops below its capacity (V184), transfer and withdraw clamp to the free
//   space (V191, V192), pickup takes min(free space, pile) (V190), upgradeController spends min(WORK ×
//   UPGRADE_CONTROLLER_POWER, energy) as progress and levels the controller up at CONTROLLER_LEVELS[level] while its
//   timer is within CONTROLLER_DOWNGRADE_RESTORE of full (V188), an upgrade restores the timer (V189), and build adds
//   min(WORK × BUILD_POWER, energy, what is left) to a site, which becomes an extension with
//   EXTENSION_ENERGY_CAPACITY[level] when done (V185, V186);
// - `room.createConstructionSite` follows the game side's checks (V200: arguments, the owner, the RCL limit of
//   CONTROLLER_STRUCTURES, the tile, MAX_CONSTRUCTION_SITES), and the site appears on the next tick with
//   CONSTRUCTION_COST to build;
// - moves resolve like the engine (V51, V52, as in js/tools/move-sim.mjs: a move into a wall, an obstacle, an own
//   obstacle site or onto a creep that stays fails, and takes the moves behind it with it), a mover gains (non-MOVE
//   non-CARRY parts + loaded CARRY parts) × 2 fatigue on plain and every creep sheds 2 per MOVE part a tick (V54,
//   V55); piles decay (V193); a tombstone lasts TOMBSTONE_DECAY_PER_PART ticks per part of its creep (V194).
// It checks, until the controller reaches level 2, its first extension is built and a few ticks more:
// - the first spawnCreep comes on the global's first loop with WORKER_PATTERN once ([work, carry, move], what the
//   spawn's energy buys) and the name `wW1N1-<base 36 of Game.time × 256>`, with exactly two arguments;
// - the creeps call only harvest (the source), transfer (the spawn or an extension, energy), upgradeController (the
//   controller), build (a site), move (a direction 1 to 8), pickup (a pile) and withdraw (the tombstone, energy), at
//   most one work call and one move a creep a tick, never a move while fatigued; every call returns OK in the model,
//   and every move resolves;
// - `Memory.creeps` holds no worker's entry after any loop, though the harness wrote one on every OK (ADR-018);
// - the controller reaches level 2, before the first worker could die of old age (the model has no aging, so the
//   run is bounded by CREEP_LIFE_TIME after the first worker leaves its spawn);
// - the extension placement (ADR-020) calls createConstructionSite once a loop from the loop after the level-up,
//   CONTROLLER_STRUCTURES.extension[2] times, each on a legal tile (inside the margin of 3, not a wall, not taken,
//   not next to the spawn or the source, not within 3 of the controller), and none at RCL1; the workers build them;
// - the opening's log lines are exactly a recovery start on the first loop (one Game.notify) and its end on the
//   loop OPENING_MMO.min_workers workers can work, with the tick count between them;
// - `rustedscreep.opening()` on the first loop, the loop after the recovery, every loop while the pile or the
//   tombstone below holds energy, and the last shows the room, its workers as the harness counts them, the plan with
//   the source, its open tiles and its trip, the site count and the loose energy (piles and tombstones, before any
//   reservation, also on loops a worker is taking it), the request and the recovery, the counts since boot and the
//   extension sites;
// - a pile and a tombstone placed near the source after the recovery are emptied by pickup and withdraw calls,
//   with the energy accounted for (what was taken plus what decayed);
// - no fault line, no incident, no halt.
// npm run check runs it (row 19); standalone: node js/tools/opening-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { findFiltered, makeStore, packPos } from './lib/world.mjs';
import { ENGINE_CONSTANTS, commandResults, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
/**
 * OPENING_MMO (crates/rustedscreep-core/src/config.rs): min_workers (the recovery ends when this many can work) and
 * max_workers (a room's plan wants its sources' sum within min_workers..=max_workers).
 */
const MIN_WORKERS = 2;
const MAX_WORKERS = 20;
/** MOVEMENT_MMO's plain cost (config.rs): a trip's cost on plain is 2 per tile. */
const PLAIN_COST = 2;
/** spawning::WORKER_PATTERN; one repeat is what SPAWN_ENERGY_CAPACITY buys (2 × 200 > 300). */
const PATTERN = ['work', 'carry', 'move'];
/** economy::placement: EDGE_MARGIN, SPAWN_CLEAR and CONTROLLER_CLEAR, the rules a picked tile must keep. */
const EDGE_MARGIN = 3;
const SPAWN_CLEAR = 2;
const CONTROLLER_CLEAR = 3;
const ROOM = 'W1N1';
const T0 = 50000000;
const SPAWN = { x: 25, y: 20 };
const SOURCE = { x: 15, y: 25 };
const CONTROLLER = { x: 35, y: 25 };
/** East of the source, toward the spawn and the controller, where a gatherer coming back passes. */
const PILE = { x: 18, y: 24, amount: 200 };
const TOMBSTONE = { x: 18, y: 26, energy: 100, parts: C.MAX_CREEP_SIZE };
/** Loops after the recovery ends before the pile and the tombstone appear, and loops run after the first extension. */
const PLACE_AFTER = 20;
const SETTLE = 5;
const SUMMARY = 'rustedscreep.opening()';

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
/** The bot's name for the first order of loop `offset` (crates/rustedscreep-core/src/spawning/name.rs). */
const nameAt = (offset) => `w${ROOM}-${(BigInt(T0 + offset) << 8n).toString(36)}`;
const count = (body, part) => body.filter((p) => p === part).length;
const isWall = (x, y) => (RAW[y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

/** Row-major terrain (V156): walls on the border ring, plain inside. */
const RAW = (() => {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) if (x === 0 || y === 0 || x === 49 || y === 49) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  return raw;
})();

function makeWorld() {
  return {
    offset: 0,
    nextId: 100,
    spawn: { id: id(1), name: 'Spawn1', ...SPAWN, energy: C.SPAWN_ENERGY_CAPACITY, spawning: null },
    source: { id: id(2), ...SOURCE, energy: C.SOURCE_ENERGY_CAPACITY, capacity: C.SOURCE_ENERGY_CAPACITY, regenAt: null },
    // A full downgrade timer at RCL1 (V189: downgradeTime, so ticksToDowngrade = downgradeTime - Game.time).
    controller: { id: id(3), ...CONTROLLER, level: 1, progress: 0, downgradeTime: T0 + C.CONTROLLER_DOWNGRADE[1], upgraded: 0 },
    creeps: [],
    piles: [],
    tombstones: [],
    /** Own construction sites: { id, x, y, type, progress, total }; and built extensions: { id, x, y, energy }. */
    sites: [],
    extensions: [],
    /** This tick's recorded intents: creep name → { work: [method, target id, amount], move: direction }. */
    intents: new Map(),
    /** This tick's accepted spawn orders and site creations. */
    orders: [],
    siteOrders: [],
    /** Every creep call: { offset, creep, method, target, kind, args, result }; spawn and room calls apart. */
    calls: [],
    spawnCalls: [],
    siteCalls: [],
    errors: [],
    entries: [],
    taken: { pile: 0, tombstone: 0, pileDecayed: 0 },
    levelAt: null,
    builtAt: null,
    leftSpawn: [],
    placed: null,
    context: null,
  };
}

/** Why a creep cannot stand on (x, y), if it cannot: terrain, an obstacle structure or source, an own obstacle site (V52). */
function isObstacle(state, x, y) {
  if (isWall(x, y)) return 'wall';
  const here = (o) => o.x === x && o.y === y;
  if ([state.spawn, state.source, state.controller, ...state.extensions].some(here)) return 'obstacle';
  if (state.sites.some((s) => here(s) && C.OBSTACLE_OBJECT_TYPES.includes(s.type))) return 'obstacle site';
  return null;
}

/** _create-energy (V193): merge into the tile's pile or make one. */
function createEnergy(state, x, y, amount) {
  const a = Math.round(amount);
  if (a <= 0) return;
  const pile = state.piles.find((p) => p.x === x && p.y === y);
  if (pile) pile.amount += a;
  else state.piles.push({ id: id(state.nextId++), x, y, amount: a });
}

/** A creep's game object for this tick, with the game side's checks (refusals are recorded, not thrown). */
function creepObject(state, c, offset, objects) {
  const work = count(c.body, 'work');
  const capacity = C.CARRY_CAPACITY * count(c.body, 'carry');
  const intents = () => {
    if (!state.intents.has(c.name)) state.intents.set(c.name, {});
    return state.intents.get(c.name);
  };
  const record = (method, target, args, result) => {
    state.calls.push({ offset, creep: c.name, method, target: target?.id ?? null, kind: objects.kindOf(target), args, result });
    return result;
  };
  const near = (t) => t?.pos && range(c, t.pos) <= 1;
  // Every action method answers ERR_NOT_OWNER and then ERR_BUSY first (V108); all creeps here are the bot's.
  const busy = (method, target, args) => (c.spawning ? record(method, target, args, C.ERR_BUSY) : null);
  const self = {
    id: c.id, name: c.name, my: true, spawning: c.spawning, fatigue: c.fatigue, owner: { username: 'bot' }, pos: pos(c.x, c.y),
    body: c.body.map((type) => ({ type, hits: 100 })),
    store: makeStore({ energy: c.energy }, { capacity }),
    move(direction) {
      const args = [direction];
      if (busy('move', null, args) !== null) return C.ERR_BUSY;
      if (c.fatigue > 0) return record('move', null, args, C.ERR_TIRED);
      if (!count(c.body, 'move')) return record('move', null, args, C.ERR_NO_BODYPART);
      const d = +direction;
      if (!Number.isInteger(d) || d < 1 || d > 8) return record('move', null, args, C.ERR_INVALID_ARGS);
      if (intents().move !== undefined) state.errors.push(`offset ${offset}: ${c.name} moved twice`);
      intents().move = d;
      return record('move', null, [d], C.OK);
    },
    harvest(target) {
      if (busy('harvest', target, []) !== null) return C.ERR_BUSY;
      if (!work) return record('harvest', target, [], C.ERR_NO_BODYPART);
      if (!target?.id || objects.kindOf(target) !== 'source') return record('harvest', target, [], C.ERR_INVALID_TARGET);
      if (!target.energy) return record('harvest', target, [], C.ERR_NOT_ENOUGH_RESOURCES);
      if (!near(target)) return record('harvest', target, [], C.ERR_NOT_IN_RANGE);
      return self.queue('harvest', target, []);
    },
    transfer(target, resource, amount) {
      const args = [resource, amount];
      if (busy('transfer', target, args) !== null) return C.ERR_BUSY;
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('transfer', target, args, C.ERR_INVALID_ARGS);
      if (!target?.id) return record('transfer', target, args, C.ERR_INVALID_TARGET);
      if (objects.kindOf(target) === 'controller' && resource === C.RESOURCE_ENERGY) return self.upgradeController(target);
      if (!target.store || !['spawn', 'extension'].includes(objects.kindOf(target))) return record('transfer', target, args, C.ERR_INVALID_TARGET);
      if (!target.store.getCapacity(resource)) return record('transfer', target, args, C.ERR_INVALID_TARGET);
      if (!near(target)) return record('transfer', target, args, C.ERR_NOT_IN_RANGE);
      if (!c.energy) return record('transfer', target, args, C.ERR_NOT_ENOUGH_RESOURCES);
      const free = target.store.getFreeCapacity(resource);
      if (!free) return record('transfer', target, args, C.ERR_FULL);
      const n = amount || Math.min(c.energy, free);
      if (c.energy < n) return record('transfer', target, args, C.ERR_NOT_ENOUGH_RESOURCES);
      if (n > free) return record('transfer', target, args, C.ERR_FULL);
      return self.queue('transfer', target, args, n);
    },
    withdraw(target, resource, amount) {
      const args = [resource, amount];
      if (busy('withdraw', target, args) !== null) return C.ERR_BUSY;
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('withdraw', target, args, C.ERR_INVALID_ARGS);
      if (!target?.id || !target.store || objects.kindOf(target) !== 'tombstone') return record('withdraw', target, args, C.ERR_INVALID_TARGET);
      if (!near(target)) return record('withdraw', target, args, C.ERR_NOT_IN_RANGE);
      const free = capacity - c.energy;
      if (!free) return record('withdraw', target, args, C.ERR_FULL);
      const stock = target.store.getUsedCapacity(resource) ?? 0;
      const n = amount || Math.min(free, stock);
      if (n > free) return record('withdraw', target, args, C.ERR_FULL);
      if (!n || stock < n) return record('withdraw', target, args, C.ERR_NOT_ENOUGH_RESOURCES);
      return self.queue('withdraw', target, args, n);
    },
    pickup(target) {
      if (busy('pickup', target, []) !== null) return C.ERR_BUSY;
      if (!target?.id || objects.kindOf(target) !== 'pile') return record('pickup', target, [], C.ERR_INVALID_TARGET);
      if (c.energy >= capacity) return record('pickup', target, [], C.ERR_FULL);
      if (!near(target)) return record('pickup', target, [], C.ERR_NOT_IN_RANGE);
      return self.queue('pickup', target, []);
    },
    upgradeController(target) {
      // V187, in its order.
      if (busy('upgradeController', target, []) !== null) return C.ERR_BUSY;
      if (!work) return record('upgradeController', target, [], C.ERR_NO_BODYPART);
      if (!c.energy) return record('upgradeController', target, [], C.ERR_NOT_ENOUGH_RESOURCES);
      if (!target?.id || objects.kindOf(target) !== 'controller' || target.upgradeBlocked > 0) return record('upgradeController', target, [], C.ERR_INVALID_TARGET);
      if (range(c, target.pos) > 3) return record('upgradeController', target, [], C.ERR_NOT_IN_RANGE);
      if (!target.my) return record('upgradeController', target, [], C.ERR_NOT_OWNER);
      return self.queue('upgradeController', target, []);
    },
    build(target) {
      // V185, in its order: an obstacle site with a creep on its tile (the builder too) is refused.
      if (busy('build', target, []) !== null) return C.ERR_BUSY;
      if (!work) return record('build', target, [], C.ERR_NO_BODYPART);
      if (!c.energy) return record('build', target, [], C.ERR_NOT_ENOUGH_RESOURCES);
      if (!target?.id || objects.kindOf(target) !== 'site') return record('build', target, [], C.ERR_INVALID_TARGET);
      if (range(c, target.pos) > 3) return record('build', target, [], C.ERR_NOT_IN_RANGE);
      const on = (o) => o.x === target.pos.x && o.y === target.pos.y;
      if (C.OBSTACLE_OBJECT_TYPES.includes(target.structureType) && (state.creeps.some((o) => !o.spawning && on(o)) || isObstacle(state, target.pos.x, target.pos.y) === 'obstacle')) {
        return record('build', target, [], C.ERR_INVALID_TARGET);
      }
      return self.queue('build', target, []);
    },
    /** Records a work intent; a second call of the same method replaces the first (V108). */
    queue(method, target, args, amount) {
      const i = intents();
      if (i.work && i.work[0] !== method) state.errors.push(`offset ${offset}: ${c.name} called ${i.work[0]} and ${method}`);
      i.work = [method, target.id, amount];
      return record(method, target, args, C.OK);
    },
  };
  // The act layer's other bindings: not expected from the opening; recorded (and OK) so they show up as unexpected.
  for (const m of ['say', 'drop', 'heal', 'rangedHeal', 'repair', 'pull']) self[m] = (...args) => record(m, args[0], args.slice(1), C.OK);
  return self;
}

function sceneFor(state, offset) {
  state.offset = offset;
  state.intents.clear();
  state.orders = [];
  state.siteOrders = [];
  const time = T0 + offset;
  const kinds = new Map();
  const objects = { kindOf: (o) => (o && typeof o === 'object' ? kinds.get(o.id) ?? null : null) };
  const ctl = state.controller;
  const controller = {
    id: ctl.id, structureType: 'controller', my: true, level: ctl.level, pos: pos(ctl.x, ctl.y),
    // V189's getters: progress and progressTotal below level 8, ticksToDowngrade from downgradeTime; not upgrade-blocked.
    progress: ctl.progress, progressTotal: C.CONTROLLER_LEVELS[ctl.level], ticksToDowngrade: ctl.downgradeTime - time,
  };
  const sp = state.spawn;
  const extCapacity = C.EXTENSION_ENERGY_CAPACITY[ctl.level];
  // V175: the room's energy and capacity are its spawn's and extensions'.
  const energyAvailable = sp.energy + state.extensions.reduce((s, e) => s + e.energy, 0);
  const energyCapacityAvailable = C.SPAWN_ENERGY_CAPACITY + extCapacity * state.extensions.length;
  const created = new Set();
  const spawn = {
    id: sp.id, name: sp.name, structureType: 'spawn', my: true, pos: pos(sp.x, sp.y),
    store: makeStore({ energy: sp.energy }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    // Null when idle, a Spawning object while busy (V175).
    spawning: sp.spawning ? { name: sp.spawning.name, needTime: sp.spawning.needTime, remainingTime: sp.spawning.spawnTime - time } : null,
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      const call = { offset, args: args.length, body: Array.isArray(body) ? [...body] : body, name, result: null };
      state.spawnCalls.push(call);
      call.result = (() => {
        // V171, in its order.
        if (!name || typeof opts !== 'object' || String(name).length > 100) return C.ERR_INVALID_ARGS;
        if (state.creeps.some((c) => c.name === name) || created.has(name)) return C.ERR_NAME_EXISTS;
        if (sp.spawning) return C.ERR_BUSY;
        // V170: an array of 1 to MAX_CREEP_SIZE known parts.
        if (!Array.isArray(body) || !body.length || body.length > C.MAX_CREEP_SIZE || body.some((p) => !C.BODYPARTS_ALL.includes(p))) return C.ERR_INVALID_ARGS;
        if (energyAvailable < body.reduce((s, p) => s + C.BODYPART_COST[p], 0)) return C.ERR_NOT_ENOUGH_ENERGY;
        if (opts.dryRun) return C.OK;
        created.add(name);
        // V181: the runtime writes the creep's Memory entry (it throws on a null Memory, which this run never has).
        const mem = state.context.Memory;
        if (mem.creeps === undefined) mem.creeps = {};
        if (mem.creeps && typeof mem.creeps === 'object') {
          mem.creeps[name] = opts.memory || {};
          state.entries.push([offset, name]);
        }
        state.orders.push({ body: [...body], name });
        return C.OK;
      })();
      return call.result;
    },
  };
  const extensions = state.extensions.map((e) => ({
    id: e.id, structureType: 'extension', my: true, pos: pos(e.x, e.y),
    store: makeStore({ energy: e.energy }, { resourceCapacity: { energy: extCapacity } }),
  }));
  const src = state.source;
  const source = {
    id: src.id, energy: src.energy, energyCapacity: src.capacity, pos: pos(src.x, src.y),
    ticksToRegeneration: src.regenAt === null ? undefined : src.regenAt - time,
  };
  const piles = state.piles.map((p) => ({ id: p.id, resourceType: C.RESOURCE_ENERGY, amount: p.amount, pos: pos(p.x, p.y) }));
  const tombstones = state.tombstones.map((t) => ({
    id: t.id, pos: pos(t.x, t.y), deathTime: t.deathTime, ticksToDecay: t.decayTime - time, store: makeStore({ energy: t.energy }),
  }));
  const sites = state.sites.map((s) => ({
    id: s.id, my: true, owner: { username: 'bot' }, structureType: s.type, progress: s.progress, progressTotal: s.total, pos: pos(s.x, s.y),
  }));
  kinds.set(controller.id, 'controller');
  kinds.set(spawn.id, 'spawn');
  kinds.set(source.id, 'source');
  for (const e of extensions) kinds.set(e.id, 'extension');
  for (const p of piles) kinds.set(p.id, 'pile');
  for (const t of tombstones) kinds.set(t.id, 'tombstone');
  for (const s of sites) kinds.set(s.id, 'site');
  const creeps = state.creeps.map((c) => creepObject(state, c, offset, objects));
  const lists = {
    [C.FIND_SOURCES]: [source], [C.FIND_STRUCTURES]: [spawn, controller, ...extensions], [C.FIND_DROPPED_RESOURCES]: piles,
    [C.FIND_TOMBSTONES]: tombstones, [C.FIND_MY_CONSTRUCTION_SITES]: sites, [C.FIND_CONSTRUCTION_SITES]: sites,
  };
  const room = {
    name: ROOM, controller, energyAvailable, energyCapacityAvailable,
    find: (type, opts) => findFiltered(lists[type] ?? [], opts),
    getTerrain: () => ({ getRawBuffer: () => RAW.slice() }),
    createConstructionSite(...args) {
      const [x, y, type] = args;
      const call = { offset, args, result: null };
      state.siteCalls.push(call);
      call.result = (() => {
        // V200, in its order (the room is the bot's and nobody reserves it).
        if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x > 49 || y > 49) return C.ERR_INVALID_ARGS;
        if (!C.CONSTRUCTION_COST[type]) return C.ERR_INVALID_ARGS;
        const have = [...state.extensions.map(() => 'extension'), ...state.sites.map((s) => s.type)].filter((t) => t === type).length;
        if (have >= (C.CONTROLLER_STRUCTURES[type]?.[ctl.level] ?? 0)) return C.ERR_RCL_NOT_ENOUGH;
        const here = (o) => o.x === x && o.y === y;
        // checkConstructionSite: the edge, any site, an object of the type, the terrain; the model also refuses the
        // tile of an obstacle structure or the source.
        if (x <= 0 || y <= 0 || x >= 49 || y >= 49 || state.sites.some(here) || isObstacle(state, x, y) || state.siteOrders.some(here)) {
          return C.ERR_INVALID_TARGET;
        }
        if (state.sites.length + state.siteOrders.length >= C.MAX_CONSTRUCTION_SITES) return C.ERR_FULL;
        state.siteOrders.push({ x, y, type });
        return C.OK;
      })();
      return call.result;
    },
  };
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries([spawn, controller, ...extensions].map((s) => [s.id, s])),
  };
}

/** The processor after loop `offset`: intents, spawn orders and sites, moves, then the object ticks (V202). */
function advance(state, offset) {
  const time = T0 + offset;
  const byName = new Map(state.creeps.map((c) => [c.name, c]));
  const ctl = state.controller;
  const extCapacity = C.EXTENSION_ENERGY_CAPACITY[ctl.level];
  const fillable = (id) => (id === state.spawn.id ? [state.spawn, C.SPAWN_ENERGY_CAPACITY] : [state.extensions.find((e) => e.id === id), extCapacity]);
  // Work intents, creep by creep, against positions at the start of the tick (moves come after).
  for (const [name, i] of state.intents) {
    const c = byName.get(name);
    if (!c || c.spawning || !i.work) continue;
    const [method, target, amount] = i.work;
    const capacity = C.CARRY_CAPACITY * count(c.body, 'carry');
    const work = count(c.body, 'work');
    if (method === 'harvest') {
      const s = state.source;
      if (target !== s.id || !s.energy || range(c, s) > 1) continue;
      const n = Math.min(s.energy, C.HARVEST_POWER * work);
      s.energy -= n;
      c.energy += n;
      // V183: the overflow drops on the creep's tile, so it ends exactly full.
      if (c.energy > capacity) {
        createEnergy(state, c.x, c.y, c.energy - capacity);
        c.energy = capacity;
      }
    } else if (method === 'transfer') {
      const [to, cap] = fillable(target);
      if (!to || range(c, to) > 1 || c.energy < amount) continue;
      const n = Math.min(amount, cap - to.energy);
      if (n <= 0) continue;
      to.energy += n;
      c.energy -= n;
    } else if (method === 'upgradeController') {
      if (target !== ctl.id || !c.energy || range(c, ctl) > 3) continue;
      const effect = Math.min(work * C.UPGRADE_CONTROLLER_POWER, c.energy);
      const total = C.CONTROLLER_LEVELS[ctl.level];
      // V188: a level-up needs the progress and a timer within CONTROLLER_DOWNGRADE_RESTORE of full.
      if (ctl.progress + effect >= total && ctl.downgradeTime + C.CONTROLLER_DOWNGRADE_RESTORE >= time + C.CONTROLLER_DOWNGRADE[ctl.level]) {
        ctl.progress += effect - total;
        ctl.level += 1;
        ctl.downgradeTime = time + C.CONTROLLER_DOWNGRADE[ctl.level] / 2;
        if (ctl.level === 2 && state.levelAt === null) state.levelAt = offset;
      } else {
        ctl.progress += effect;
      }
      ctl.upgraded += effect;
      c.energy -= effect;
    } else if (method === 'build') {
      // V186: nothing while a creep stands on an obstacle site (positions before this tick's moves).
      const s = state.sites.find((x) => x.id === target);
      if (!s || !c.energy || range(c, s) > 3) continue;
      if (C.OBSTACLE_OBJECT_TYPES.includes(s.type) && state.creeps.some((o) => !o.spawning && o.x === s.x && o.y === s.y)) continue;
      const effect = Math.min(work * C.BUILD_POWER, s.total - s.progress, c.energy);
      s.progress += effect;
      c.energy -= effect;
      if (s.progress >= s.total) {
        state.sites = state.sites.filter((x) => x !== s);
        state.extensions.push({ id: id(state.nextId++), x: s.x, y: s.y, energy: 0 });
        state.builtAt ??= offset;
      }
    } else if (method === 'pickup') {
      const p = state.piles.find((x) => x.id === target);
      if (!p || c.energy >= capacity || range(c, p) > 1) continue;
      const n = Math.min(capacity - c.energy, p.amount);
      p.amount -= n;
      c.energy += n;
      if (p.scripted) state.taken.pile += n;
      if (p.amount <= 0) state.piles = state.piles.filter((x) => x !== p);
    } else if (method === 'withdraw') {
      const t = state.tombstones.find((x) => x.id === target);
      if (!t || range(c, t) > 1) continue;
      const n = Math.min(amount, capacity - c.energy, t.energy);
      if (n <= 0) continue;
      t.energy -= n;
      c.energy += n;
      state.taken.tombstone += n;
    }
  }
  // Spawn orders (V173): charge the spawn first, then the extensions nearest it; make the creep at the spawn.
  const sp = state.spawn;
  for (const order of state.orders) {
    let cost = order.body.reduce((s, p) => s + C.BODYPART_COST[p], 0);
    const stores = [sp, ...[...state.extensions].sort((a, b) => range(a, sp) - range(b, sp))];
    if (sp.spawning || stores.reduce((s, x) => s + x.energy, 0) < cost) continue;
    for (const x of stores) {
      const n = Math.min(x.energy, cost);
      x.energy -= n;
      cost -= n;
    }
    const needTime = C.CREEP_SPAWN_TIME * order.body.length;
    sp.spawning = { name: order.name, needTime, spawnTime: time + needTime };
    state.creeps.push({ id: id(state.nextId++), name: order.name, body: order.body, x: sp.x, y: sp.y, energy: 0, fatigue: 0, spawning: true });
  }
  // Site creations (V200's processor): the site is there from the next tick.
  for (const o of state.siteOrders) {
    state.sites.push({ id: id(state.nextId++), x: o.x, y: o.y, type: o.type, progress: 0, total: C.CONSTRUCTION_COST[o.type] });
  }
  applyMoves(state, offset, byName);
  // Creep ticks (V55): every creep, spawning ones too, sheds 2 fatigue per MOVE part.
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  // Spawn tick (V174): place the creep once gameTime >= spawnTime - 1, on the first free of directions 1..8; regenerate.
  if (sp.spawning && time >= sp.spawning.spawnTime - 1) {
    const c = state.creeps.find((x) => x.name === sp.spawning.name);
    const free = OFFSETS.slice(1).map(([dx, dy]) => [sp.x + dx, sp.y + dy])
      .find(([x, y]) => !isObstacle(state, x, y) && !state.creeps.some((o) => !o.spawning && o.x === x && o.y === y));
    if (c && free) {
      [c.x, c.y] = free;
      c.spawning = false;
      sp.spawning = null;
      state.leftSpawn.push([offset + 1, c.name]);
    }
  }
  const roomEnergy = sp.energy + state.extensions.reduce((s, e) => s + e.energy, 0);
  if (roomEnergy < C.SPAWN_ENERGY_CAPACITY && sp.energy < C.SPAWN_ENERGY_CAPACITY) sp.energy += 1;
  // Source tick (V184): the timer starts on the first tick below capacity; the refill comes at its end.
  const s = state.source;
  if (s.energy < s.capacity) {
    s.regenAt ??= time + C.ENERGY_REGEN_TIME;
    if (time >= s.regenAt - 1) {
      s.energy = s.capacity;
      s.regenAt = null;
    }
  }
  // Controller tick (V189): any upgrade restores the timer, capped at full; without one it would run down.
  if (ctl.upgraded) ctl.downgradeTime = Math.min(ctl.downgradeTime + C.CONTROLLER_DOWNGRADE_RESTORE + 1, time + C.CONTROLLER_DOWNGRADE[ctl.level] + 1);
  else if (time >= ctl.downgradeTime - 1) state.errors.push(`offset ${offset}: the controller downgrades`);
  ctl.upgraded = 0;
  // Piles decay (V193), after the tick's pickups; tombstones drop their store when they decay (V194).
  for (const p of state.piles) {
    const d = Math.ceil(p.amount / C.ENERGY_DECAY);
    p.amount -= d;
    if (p.scripted) state.taken.pileDecayed += d;
  }
  state.piles = state.piles.filter((p) => p.amount > 0);
  for (const t of state.tombstones.filter((x) => time >= x.decayTime - 1)) {
    if (t.energy > 0) state.errors.push(`offset ${offset}: the tombstone decayed with ${t.energy} energy left`);
    createEnergy(state, t.x, t.y, t.energy);
  }
  state.tombstones = state.tombstones.filter((t) => time < t.decayTime - 1);
}

/** The engine's move resolution (V51, V52), then fatigue for the creeps that moved (V54). */
function applyMoves(state, offset, byName) {
  const moves = new Map();
  for (const [name, i] of state.intents) {
    const c = byName.get(name);
    if (!c || c.spawning || i.move === undefined) continue;
    const [dx, dy] = OFFSETS[i.move];
    moves.set(c, { x: c.x + dx, y: c.y + dy });
  }
  const standing = state.creeps.filter((c) => !c.spawning);
  let changed = true;
  while (changed) {
    changed = false;
    for (const [c, d] of moves) {
      const others = [...moves].filter(([o, e]) => o !== c && e.x === d.x && e.y === d.y);
      const stays = standing.some((o) => o !== c && o.x === d.x && o.y === d.y && !moves.has(o));
      const why = isObstacle(state, d.x, d.y) ?? (others.length ? `${others[0][0].name} moves there too` : stays ? 'a creep stays there' : null);
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
    const weight = c.body.filter((p) => p !== 'move' && p !== 'carry').length + Math.ceil(c.energy / C.CARRY_CAPACITY);
    c.fatigue += weight * 2;
  }
}

/** Places the pile and the tombstone (from a MAX_CREEP_SIZE-part creep, so it lasts long enough). */
function place(state, offset) {
  const time = T0 + offset;
  state.piles.push({ id: id(state.nextId++), x: PILE.x, y: PILE.y, amount: PILE.amount, scripted: true });
  state.tombstones.push({
    id: id(state.nextId++), x: TOMBSTONE.x, y: TOMBSTONE.y, energy: TOMBSTONE.energy, deathTime: time,
    decayTime: time + TOMBSTONE.parts * C.TOMBSTONE_DECAY_PER_PART,
  });
  state.placed = offset;
}

/** Why a picked extension tile breaks ADR-020's rules or the engine's, if it does. */
function illegalTile(state, x, y) {
  const near = (o, r) => range(o, { x, y }) <= r;
  const high = 49 - EDGE_MARGIN;
  if (x < EDGE_MARGIN || y < EDGE_MARGIN || x > high || y > high) return 'within the edge margin';
  if (isWall(x, y)) return 'a wall';
  if (near(state.spawn, SPAWN_CLEAR - 1)) return 'next to the spawn';
  if (near(state.source, 1)) return 'next to the source';
  if (near(state.controller, CONTROLLER_CLEAR)) return 'within 3 of the controller';
  return null;
}

export function openingSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  const summaries = {};
  let recoveredAt = null;
  const memoryWorkers = [];
  // The model has no aging: stop before the first worker could die (CREEP_LIFE_TIME after it leaves its spawn, V170).
  const bound = 1 + C.CREEP_SPAWN_TIME * PATTERN.length + C.CREEP_LIFE_TIME;
  const done = () => state.builtAt !== null && state.offset >= state.builtAt + SETTLE;
  const world = simulate(dir, {
    ticks: bound,
    setup: (context) => {
      state.context = context;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      // What this loop saw: the creeps, extensions and sites before the processor runs this tick's intents.
      const seen = {
        creeps: state.creeps.length, working: state.creeps.filter((c) => !c.spawning).length, extensions: state.extensions.length,
        sites: state.sites.length, level: state.controller.level,
        // Energy in piles and tombstones (no ruin or container here), before any worker's reservation.
        loose: state.piles.reduce((s, p) => s + p.amount, 0) + state.tombstones.reduce((s, t) => s + t.energy, 0),
        scripted: state.piles.some((p) => p.scripted) || state.tombstones.some((t) => t.energy > 0),
      };
      // The loop's own site call counts in its summary.
      seen.siteCalls = state.siteCalls.length;
      advance(state, offset);
      // The bot deletes the entry on the tick of the OK (ADR-018), so none is left after the loop.
      const mem = state.context.Memory?.creeps ?? {};
      memoryWorkers.push(...Object.keys(mem).filter((n) => n.startsWith(`w${ROOM}-`)).map((n) => [offset, n]));
      if (recoveredAt === null && w.logs.some((l) => l.includes(`rustedscreep opening: ${ROOM} recovered after`))) recoveredAt = offset;
      if (recoveredAt !== null && state.placed === null && offset === recoveredAt + PLACE_AFTER) place(state, offset);
      // Also every loop while the pile or the tombstone holds energy: the summary's loose energy is the total, before
      // the reservations of the workers taking it.
      if (offset === 1 || offset === recoveredAt + 1 || seen.scripted || done()) {
        summaries[offset] = seen;
        return SUMMARY;
      }
      return null;
    },
    until: done,
  });
  const problems = [...state.errors.slice(0, 10)];
  if (state.errors.length > 10) problems.push(`... ${state.errors.length - 10} more model errors`);
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);

  // The first order: the global's first loop, one WORKER_PATTERN repeat, the bot's name, two arguments.
  const first = state.spawnCalls[0];
  if (!first || first.offset !== 1 || first.args !== 2 || JSON.stringify(first.body) !== JSON.stringify(PATTERN) || first.name !== nameAt(1)) {
    problems.push(`first spawnCreep ${JSON.stringify(first)}, expected offset 1, 2 arguments, body ${JSON.stringify(PATTERN)}, name ${nameAt(1)}`);
  }
  for (const c of state.spawnCalls) {
    if (c.args !== 2 || !Array.isArray(c.body) || !c.name.startsWith(`w${ROOM}-`) || c.result !== C.OK) problems.push(`spawnCreep ${JSON.stringify(c)}`);
  }

  // The creeps' calls: methods and targets, all OK.
  const fills = new Set(['spawn', 'extension']);
  const allowed = {
    harvest: (c) => c.target === state.source.id && c.args.length === 0,
    transfer: (c) => fills.has(c.kind) && c.args[0] === C.RESOURCE_ENERGY && c.args[1] === undefined,
    upgradeController: (c) => c.target === state.controller.id && c.args.length === 0,
    build: (c) => c.kind === 'site' && c.args.length === 0,
    move: (c) => c.target === null && Number.isInteger(c.args[0]),
    pickup: (c) => c.kind === 'pile',
    withdraw: (c) => c.kind === 'tombstone' && c.args[0] === C.RESOURCE_ENERGY && c.args[1] === undefined,
  };
  const byMethod = {};
  const notOk = {};
  for (const c of state.calls) {
    byMethod[c.method] = (byMethod[c.method] ?? 0) + 1;
    if (!allowed[c.method]?.(c)) problems.push(`offset ${c.offset}: unexpected call ${c.method} by ${c.creep} on ${c.target} (${c.kind}) ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) notOk[`${c.method} ${c.result}`] = (notOk[`${c.method} ${c.result}`] ?? 0) + 1;
  }
  if (Object.keys(notOk).length) problems.push(`calls that did not return OK: ${JSON.stringify(notOk)}`);

  // ADR-018: an entry on every OK, none left after the loop, none stored.
  const oks = state.spawnCalls.filter((c) => c.result === C.OK).map((c) => c.offset);
  if (JSON.stringify(state.entries.map(([o]) => o)) !== JSON.stringify(oks)) problems.push(`Memory entries written at ${JSON.stringify(state.entries)}, expected one per OK at ${JSON.stringify(oks)}`);
  if (memoryWorkers.length) problems.push(`Memory.creeps kept worker entries after the loop: ${JSON.stringify(memoryWorkers.slice(0, 4))}`);
  if (/"w[WE]\d+[NS]\d+-/.test(world.memoryJson)) problems.push(`the stored Memory holds a worker entry: ${world.memoryJson.slice(0, 200)}`);

  // Level 2 and the first extension, within the bound.
  if (state.levelAt === null || state.builtAt === null) {
    problems.push(`after ${world.ticksRun} ticks: level ${state.controller.level} (progress ${state.controller.progress}), extensions ${state.extensions.length}, sites ${JSON.stringify(state.sites)}`);
  }

  // ADR-020: none at RCL1; from the loop after the level-up one call a loop (each site is seen the next loop) until
  // the room holds CONTROLLER_STRUCTURES.extension[2] extensions and sites, each OK on a legal tile.
  const allowedSites = C.CONTROLLER_STRUCTURES.extension[2];
  const siteOffsets = state.siteCalls.map((c) => c.offset);
  const wantSiteOffsets = state.levelAt === null ? [] : Array.from({ length: allowedSites }, (_, k) => state.levelAt + 1 + k);
  if (JSON.stringify(siteOffsets) !== JSON.stringify(wantSiteOffsets)) problems.push(`createConstructionSite at offsets ${JSON.stringify(siteOffsets)}, expected ${JSON.stringify(wantSiteOffsets)}`);
  for (const { offset, args, result } of state.siteCalls) {
    const [x, y, type, ...rest] = args;
    const why = result !== C.OK ? `returned ${result}` : rest.length || type !== 'extension' ? 'arguments' : illegalTile(state, x, y);
    if (why) problems.push(`offset ${offset}: createConstructionSite(${args.join(', ')}): ${why}`);
  }

  // The watchdog's lines: the recovery's start on the first loop and its end when MIN_WORKERS can work.
  const opening = world.logs.filter((l) => l.includes('rustedscreep opening:'));
  const secondWorker = state.leftSpawn[MIN_WORKERS - 1]?.[0];
  const wantLines = [
    `rustedscreep opening: ${ROOM} has no worker that can work; spawning at the energy it holds until opening.min_workers can`,
    `rustedscreep opening: ${ROOM} recovered after ${secondWorker - 1} ticks`,
  ];
  if (opening.length !== 2 || !opening[0].endsWith(wantLines[0]) || !opening[1].endsWith(wantLines[1])) {
    problems.push(`opening lines ${JSON.stringify(opening)}, expected ${JSON.stringify(wantLines)}`);
  }
  if (recoveredAt !== secondWorker) problems.push(`recovered at offset ${recoveredAt}, expected ${secondWorker} (the loop the second worker can work)`);
  const notices = world.notifies.filter((n) => n.includes('rustedscreep opening:'));
  if (notices.length !== 1 || !notices[0].endsWith(wantLines[0])) problems.push(`opening notifications ${JSON.stringify(world.notifies)}, expected one recovery`);

  // The summaries: the room, the workers as the harness counts them, the plan with the source, the request, the
  // counts since boot and the extension sites (all OK: placed as picked, none refused, lost or skipped).
  const results = commandResults(world, SUMMARY);
  const offsets = Object.keys(summaries).map(Number);
  const tiles = range(SOURCE, CONTROLLER) - 1 - 3;
  if (results.length !== offsets.length) problems.push(`${results.length} opening summaries, expected ${offsets.length}`);
  offsets.forEach((o, i) => {
    const r = results[i] ?? '';
    const seen = summaries[o];
    const capacity = C.SPAWN_ENERGY_CAPACITY + C.EXTENSION_ENERGY_CAPACITY[seen.level] * seen.extensions;
    const plan = `plan at capacity ${capacity}: source (${SOURCE.x},${SOURCE.y}) open 8 want `;
    const n = seen.siteCalls;
    const parts = [
      `rustedscreep opening at tick ${T0 + o}: ${ROOM}: workers `,
      ` | extension sites: picked ${n}, placed ${n}, refused 0, never seen 0, tiles skipped 0`,
    ];
    // The first loop has no plan: movement reads a room only once an own creep is in it (its first creep is in
    // Game.creeps from the next tick), and until its grid exists the room asks for min_workers. The recovery's
    // request is sized to the energy the room holds, at PRIORITY_RECOVERY (200).
    if (o === 1) {
      parts.push(`workers 0 of ${MIN_WORKERS} (working 0), requesting at available energy (priority 200), no plan yet, sites 0, loose energy 0, ` +
        `RECOVERING since tick ${T0 + 1}; since boot: recoveries 1 (ended 0), guards 0, wait fallbacks 0, stalls 0`);
    } else {
      parts.push(plan, ` trip ${tiles} tiles cost ${tiles * PLAIN_COST}, sites ${seen.sites}, loose energy ${seen.loose};`,
        'since boot: recoveries 1 (ended 1), guards 0, wait fallbacks 0, stalls 0');
    }
    for (const p of parts) if (!r.includes(p)) problems.push(`opening summary at offset ${o} lacks "${p}": ${r}`);
    if (o !== 1 && r.includes('RECOVERING')) problems.push(`opening summary at offset ${o} still recovering: ${r}`);
    const want = /W1N1: workers (\d+) of (\d+) \(working (\d+)\)/.exec(r);
    if (!want) {
      problems.push(`opening summary at offset ${o} has no worker counts: ${r}`);
      return;
    }
    const [, alive, wanted, working] = want.map(Number);
    if (alive !== seen.creeps || working !== seen.working) {
      problems.push(`summary at offset ${o}: workers ${alive} (working ${working}), the harness had ${seen.creeps} (${seen.working})`);
    }
    if (o === 1) return;
    // One source: the room wants what it wants, within min_workers..=max_workers.
    const planWant = Number(new RegExp(`${plan.replace(/[()]/g, '\\$&')}(\\d+)`).exec(r)?.[1]);
    if (wanted !== Math.min(Math.max(planWant, MIN_WORKERS), MAX_WORKERS)) problems.push(`summary at offset ${o}: wants ${wanted}, the plan's source wants ${planWant}`);
  });

  // The loose energy is the total also while a worker takes it (the reservation is not subtracted: P2.4 review).
  const taking = offsets.filter((o, i) => summaries[o].scripted && /, tasks [^;]*take \d/.test(results[i] ?? ''));
  if (!taking.length) problems.push('no opening summary while a worker takes from the pile or the tombstone');

  // The pile and the tombstone: emptied by pickups and withdraws, the energy accounted for.
  const scripted = state.calls.filter((c) => c.offset > (state.placed ?? Infinity));
  const pickups = scripted.filter((c) => c.method === 'pickup');
  const withdraws = scripted.filter((c) => c.method === 'withdraw');
  if (state.placed === null) problems.push('the pile and the tombstone were never placed');
  else {
    if (!pickups.length || state.taken.pile + state.taken.pileDecayed !== PILE.amount || state.piles.some((p) => p.scripted)) {
      problems.push(`pile: ${pickups.length} pickups took ${state.taken.pile}, ${state.taken.pileDecayed} decayed, of ${PILE.amount}`);
    }
    if (!withdraws.length || state.taken.tombstone !== TOMBSTONE.energy) problems.push(`tombstone: ${withdraws.length} withdraws took ${state.taken.tombstone} of ${TOMBSTONE.energy}`);
  }

  if (problems.length) throw new Error(`opening-sim:\n  ${problems.slice(0, 25).join('\n  ')}`);
  return {
    ticks: world.ticksRun, firstSpawn: first.offset, recoveredAt, levelAt: state.levelAt, builtAt: state.builtAt,
    workers: state.creeps.length, spawned: oks.length, byMethod, pickups: pickups.length, withdraws: withdraws.length,
    pileTaken: state.taken.pile, pileDecayed: state.taken.pileDecayed, placed: state.placed, taking: taking.length,
    summaries: offsets.length,
    sites: state.siteCalls.map((c) => `${c.offset}:(${c.args[0]},${c.args[1]})`), summary: results.at(-1),
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = openingSim(process.argv[2]);
    console.log(`opening-sim ok: ${r.ticks} ticks; first spawnCreep at offset ${r.firstSpawn}, recovery over at offset ${r.recoveredAt}, ` +
      `level 2 at offset ${r.levelAt}, first extension at offset ${r.builtAt}; ${r.spawned} workers spawned (${r.workers} at the end); ` +
      `calls ${JSON.stringify(r.byMethod)}; extension sites ${r.sites.join(' ')}; pile and tombstone at offset ${r.placed}: ` +
      `${r.pickups} pickups (${r.pileTaken} taken, ${r.pileDecayed} decayed), ${r.withdraws} withdraws; ${r.summaries} summaries ` +
      `(${r.taking} while a worker takes) | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
