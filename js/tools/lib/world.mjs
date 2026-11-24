// A small fake game world for the Screeps-like sandbox (js/tools/smoke.mjs), shaped like the engine's
// objects as far as the snapshot reads them (VERIFICATION_LOG V101): Game.creeps keyed by name (spawning
// creeps included), Game.structures keyed by id (owned structures, the controller among them),
// Game.rooms keyed by name, room.find(FIND_HOSTILE_CREEPS) without spawning creeps (other find types give
// nothing, so the P2.4 opening finds no source in W1N1 and wants no worker), and room.getTerrain() with an
// open plain room (the movement glue reads it; V156). Positions carry the engine's __packedPos (V103).
// Creeps and spawns carry an engine-shaped Store (makeStore, V196): a creep one CARRY part's capacity, a spawn
// its energy; find applies a second argument's `filter` like the engine (findFiltered, V199). Each creep has
// the engine's `body` (one MOVE part of 100 hits, no `boost` key) and `hits` (V289): the snapshot reads a
// hostile creep's in an owned room (P7.1). Plain objects
// suffice: the wasm reads properties and never checks instanceof. Each creep has the action methods the act
// layer calls (crates/rustedscreep-wasm/src/act.rs), and each room `createConstructionSite` (the ADR-020
// extension placement, V200), which record the call and return a scripted value instead of acting: a site
// never appears. No game logic.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { FIXTURE_DIR, parseFixture } from '../consts-fixture.mjs';

/** common lib/constants.js: FIND_HOSTILE_CREEPS (V101). */
export const FIND_HOSTILE_CREEPS = 103;

/**
 * @screeps/common 2.16.0 (the constants fixture): CARRY_CAPACITY, SPAWN_ENERGY_CAPACITY, CONTROLLER_LEVELS,
 * CONTROLLER_DOWNGRADE and RESOURCES_ALL.
 */
const FIXTURE = Object.fromEntries(parseFixture(readFileSync(join(FIXTURE_DIR, 'constants-common-2.16.0.tsv'), 'utf8')));
export const CARRY_CAPACITY = JSON.parse(FIXTURE.CARRY_CAPACITY);
export const SPAWN_ENERGY_CAPACITY = JSON.parse(FIXTURE.SPAWN_ENERGY_CAPACITY);
const CONTROLLER_LEVELS = JSON.parse(FIXTURE.CONTROLLER_LEVELS);
const CONTROLLER_DOWNGRADE = JSON.parse(FIXTURE.CONTROLLER_DOWNGRADE);
const RESOURCES = new Set(JSON.parse(FIXTURE.RESOURCES_ALL));

/**
 * The engine's Store (game/store.js, V196) over an object's `store` contents and its capacity fields:
 * `capacity` is `storeCapacity` (one pool shared by every resource: a creep, CARRY_CAPACITY per active CARRY
 * part; a container), `resourceCapacity` is `storeCapacityResource` (per resource: a spawn or an extension,
 * `{ energy: N }`); a tombstone or a ruin has neither. Only truthy contents are own properties, and any other
 * resource name reads 0. `getCapacity(res)` is null where the capacity for `res` is 0 (and `getCapacity()` for a
 * per-resource store); `getUsedCapacity(res)` is the amount, or null when that is 0 in a per-resource store with
 * no capacity for `res`; `getFreeCapacity(res)` is null where the capacity is 0, else the capacity left (the
 * per-resource capacity minus that resource, or the pool minus everything held).
 */
export function makeStore(contents = {}, { capacity, resourceCapacity } = {}) {
  const held = Object.fromEntries(Object.entries(contents).filter(([, v]) => v));
  const sum = () => Object.values(held).reduce((s, v) => s + v, 0);
  const restricted = resourceCapacity !== undefined && resourceCapacity !== null;
  // utils.capacityForResource: the per-resource capacity, else what the pool leaves after those.
  const capacityFor = (res) => {
    const own = restricted ? resourceCapacity[res] : undefined;
    if (own !== undefined) return own;
    const reserved = restricted ? Object.values(resourceCapacity).reduce((s, v) => s + v, 0) : 0;
    return Math.max(0, (capacity ?? 0) - reserved);
  };
  const methods = {
    getCapacity(res) {
      if (!res) return restricted ? null : capacity || null;
      return capacityFor(res) || null;
    },
    getUsedCapacity(res) {
      if (!res) return restricted && !capacityFor(undefined) ? null : sum();
      const amount = held[res] ?? 0;
      if (amount) return amount;
      return restricted && !capacityFor(res) ? null : 0;
    },
    getFreeCapacity(res) {
      const cap = capacityFor(res);
      if (!cap) return null;
      return !capacity || restricted ? cap - (held[res] ?? 0) : cap - sum();
    },
  };
  return new Proxy(held, {
    get(target, prop) {
      if (Object.hasOwn(methods, prop)) return methods[prop];
      if (Object.hasOwn(target, prop)) return target[prop];
      return typeof prop === 'string' && RESOURCES.has(prop) ? 0 : undefined;
    },
  });
}

/**
 * The engine's `Room.find(type, opts)` over the room's list for `type` (game/rooms.js, V199): with
 * `opts.filter` it returns lodash's `_.filter(list, filter)` (a function, a matcher object compared as a partial
 * deep match, or a property name), else a copy; a new array either way, holding the same objects.
 */
export function findFiltered(list, opts) {
  const filter = opts?.filter;
  if (!filter) return [...list];
  if (typeof filter === 'function') return list.filter(filter);
  if (typeof filter === 'string') return list.filter((o) => o?.[filter]);
  const matches = (value, pattern) => {
    if (pattern === null || typeof pattern !== 'object') return Object.is(value, pattern) || value === pattern;
    if (value === null || typeof value !== 'object') return false;
    return Object.entries(pattern).every(([k, v]) => matches(value[k], v));
  };
  return list.filter((o) => matches(o, filter));
}

/**
 * The engine's RoomPosition.__packedPos (V103): (roomX + 128) << 24 | (roomY + 128) << 16 | x << 8 | y,
 * where W<n> is roomX = -n - 1, E<n> is n, N<n> is roomY = -n - 1 and S<n> is n. Like the engine's, the value is
 * a signed int32, so E and S rooms give negative numbers; the wasm boundary keeps the same 32 bits (V100).
 */
export function packPos(roomName, x, y) {
  const m = /^([WE])(\d+)([NS])(\d+)$/.exec(roomName);
  if (!m) throw new Error(`not a room name: ${roomName}`);
  const rx = m[1] === 'W' ? -Number(m[2]) - 1 : Number(m[2]);
  const ry = m[3] === 'N' ? -Number(m[4]) - 1 : Number(m[4]);
  return ((rx + 128) << 24) | ((ry + 128) << 16) | (x << 8) | y;
}

function pos(roomName, x, y) {
  return { x, y, roomName, __packedPos: packPos(roomName, x, y) };
}

/** The Creep methods the act layer binds (crates/rustedscreep-wasm/src/act.rs; engine game/creeps.js, V108). */
export const CREEP_METHODS = ['move', 'say', 'drop', 'transfer', 'withdraw', 'heal', 'rangedHeal', 'rangedAttack', 'repair', 'upgradeController', 'pull',
  'harvest', 'build', 'pickup', 'reserveController', 'suicide'];

/** How a recorded call shows an argument: a game object by name (or structure type), undefined as a marker. */
export function describeArg(arg) {
  if (arg === undefined) return '<undefined>';
  if (arg && typeof arg === 'object') return arg.name ?? arg.structureType ?? arg.id;
  return arg;
}

/**
 * `actions` (optional): `{ calls, returns, sites }`. Each method call is pushed onto `calls` as
 * [method, creep name, ...arguments] (see describeArg); it returns `returns[method]()` when that is a
 * function (which may return undefined or throw), `returns[method]` when set, and OK (0) otherwise.
 * Every creep carries nothing in one CARRY part's capacity (V196, V197).
 */
function creep(id, name, roomName, x, y, { my = true, spawning = false } = {}, actions = null) {
  const c = {
    id, name, my, spawning, owner: { username: my ? 'bot' : 'Invader' }, pos: pos(roomName, x, y),
    store: makeStore({}, { capacity: CARRY_CAPACITY }),
    body: [{ type: 'move', hits: 100 }], hits: 100, hitsMax: 100,
  };
  for (const method of CREEP_METHODS) {
    c[method] = function recorded(...args) {
      actions?.calls.push([method, this.name, ...args.map(describeArg)]);
      const r = actions?.returns?.[method];
      return typeof r === 'function' ? r() : (r ?? 0);
    };
  }
  return c;
}

function structure(id, structureType, roomName, x, y, extra = {}) {
  return { id, structureType, my: true, pos: pos(roomName, x, y), ...extra };
}

/**
 * `actions.sites` (optional) gets each `createConstructionSite` call as [room name, ...arguments]; it returns
 * `actions.returns.createConstructionSite` like a creep method (OK by default), and places nothing.
 */
function room(name, controller, creeps, actions = null) {
  return {
    name,
    controller,
    energyAvailable: SPAWN_ENERGY_CAPACITY,
    energyCapacityAvailable: SPAWN_ENERGY_CAPACITY,
    find: (type, opts) => findFiltered(type === FIND_HOSTILE_CREEPS ? creeps.filter((c) => !c.my && !c.spawning) : [], opts),
    getTerrain: () => ({ getRawBuffer: () => new Uint8Array(2500) }),
    createConstructionSite(...args) {
      actions?.sites?.push([name, ...args]);
      const r = actions?.returns?.createConstructionSite;
      return typeof r === 'function' ? r() : (r ?? 0);
    },
  };
}

// 24 lowercase hex digits, like MongoDB ObjectIds on the MMO.
export const IDS = {
  spawn: '5bbcaa9099f9b0a0f7f4b3c6',
  controller: '5bbcaa9099f9b0a0f7f4b3c7',
  harvester: '5bbcaa9099f9b0a0f7f4b3d0',
  scout: '5bbcaa9099f9b0a0f7f4b3d1',
  scout2: '5bbcaa9099f9b0a0f7f4b3d3',
  builder: '5bbcaa9099f9b0a0f7f4b3d2',
  invader: '5bbcaa9099f9b0a0f7f4b3e0',
  invader2: '5bbcaa9099f9b0a0f7f4b3e1',
  mystery: '5bbcaa9099f9b0a0f7f4b3f0',
};

/**
 * The scene at tick `offset` (0 = the first sandbox tick). The bundle's first loop runs at offset 1.
 * - always: W1N1 owned at level 2, with a full spawn (its room holds SPAWN_ENERGY_CAPACITY of as much), the
 *   controller (its downgrade timer full) and a harvester that moves; no creep has a bot worker name, and the room
 *   no source (the opening wants no worker) and no extension or site (the bot places extension sites, which never
 *   appear);
 * - offsets 0-3: a scout in W2N1 (neutral controller, `my` undefined), which makes W2N1 visible;
 * - always: a second scout in E0S0, a highway room with no controller, whose positions are negative int32s;
 * - offsets 2-3: an Invader in W2N1; from 3: a builder still spawning at the spawn;
 * - from 4: a creep with an id that is not hex and a structure of an unknown type (both skipped);
 * - from 6: a second Invader, in W1N1.
 * `actions` records the creeps' method calls (see creep()).
 */
export function sceneAt(offset, actions = null) {
  const creeps = [creep(IDS.harvester, 'Harvester', 'W1N1', 10, 10 + (offset % 3), {}, actions)];
  const scoutAlive = offset <= 3;
  if (scoutAlive) creeps.push(creep(IDS.scout, 'Scout', 'W2N1', 30, 30, {}, actions));
  creeps.push(creep(IDS.scout2, 'Scout2', 'E0S0', 25, 10, {}, actions));
  if (offset >= 3) creeps.push(creep(IDS.builder, 'Builder', 'W1N1', 25, 25, { spawning: true }, actions));
  if (offset >= 4) creeps.push(creep('NOT-HEX!', 'Broken', 'W1N1', 11, 11, {}, actions));
  const hostiles = offset >= 2 && offset <= 3
    ? [creep(IDS.invader, 'Invader1', 'W2N1', 40, 40, { my: false }, actions)]
    : [];

  // An owned controller's getters (V189): no progress yet, the level's total, and a full downgrade timer.
  const controller = structure(IDS.controller, 'controller', 'W1N1', 20, 5, {
    level: 2, progress: 0, progressTotal: CONTROLLER_LEVELS[2], ticksToDowngrade: CONTROLLER_DOWNGRADE[2],
  });
  const spawnStore = makeStore({ energy: SPAWN_ENERGY_CAPACITY }, { resourceCapacity: { energy: SPAWN_ENERGY_CAPACITY } });
  const structures = [structure(IDS.spawn, 'spawn', 'W1N1', 25, 25, { name: 'Spawn1', store: spawnStore }), controller];
  if (offset >= 4) structures.push(structure(IDS.mystery, 'mysteryStructure', 'W1N1', 30, 5));

  const homeHostiles = offset >= 6 ? [creep(IDS.invader2, 'Invader2', 'W1N1', 30, 30, { my: false }, actions)] : [];
  const rooms = [room('W1N1', controller, [...creeps, ...homeHostiles], actions)];
  if (scoutAlive) {
    // An unowned controller has no user, so the engine's `my` is undefined (V101).
    const neutral = { id: '5bbcaa9099f9b0a0f7f4b3c8', structureType: 'controller', level: 0, pos: pos('W2N1', 20, 20) };
    rooms.push(room('W2N1', neutral, [...creeps, ...hostiles], actions));
  }
  rooms.push(room('E0S0', undefined, creeps, actions));
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: Object.fromEntries(rooms.map((r) => [r.name, r])),
    structures: Object.fromEntries(structures.map((s) => [s.id, s])),
  };
}
