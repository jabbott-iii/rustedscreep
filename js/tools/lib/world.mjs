// A small fake game world for the Screeps-like sandbox (js/tools/smoke.mjs), shaped like the engine's
// objects as far as the snapshot reads them (VERIFICATION_LOG V101): Game.creeps keyed by name (spawning
// creeps included), Game.structures keyed by id (owned structures, the controller among them),
// Game.rooms keyed by name, and room.find(FIND_HOSTILE_CREEPS) without spawning creeps. Positions carry
// the engine's __packedPos (V103). Plain objects suffice: the wasm reads properties and never checks
// instanceof. Each creep has the action methods the act layer calls (crates/rustedscreep-wasm/src/act.rs),
// which record the call and return a scripted value instead of acting. No game logic.

/** common lib/constants.js: FIND_HOSTILE_CREEPS (V101). */
export const FIND_HOSTILE_CREEPS = 103;

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
export const CREEP_METHODS = ['move', 'say', 'drop', 'transfer', 'withdraw', 'heal', 'rangedHeal', 'repair', 'upgradeController', 'pull'];

/** How a recorded call shows an argument: a game object by name (or structure type), undefined as a marker. */
export function describeArg(arg) {
  if (arg === undefined) return '<undefined>';
  if (arg && typeof arg === 'object') return arg.name ?? arg.structureType ?? arg.id;
  return arg;
}

/**
 * `actions` (optional): `{ calls, returns }`. Each method call is pushed onto `calls` as
 * [method, creep name, ...arguments] (see describeArg); it returns `returns[method]()` when that is a
 * function (which may return undefined or throw), `returns[method]` when set, and OK (0) otherwise.
 */
function creep(id, name, roomName, x, y, { my = true, spawning = false } = {}, actions = null) {
  const c = { id, name, my, spawning, owner: { username: my ? 'bot' : 'Invader' }, pos: pos(roomName, x, y) };
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

function room(name, controller, creeps) {
  return {
    name,
    controller,
    energyAvailable: 300,
    energyCapacityAvailable: 300,
    find: (type) => (type === FIND_HOSTILE_CREEPS ? creeps.filter((c) => !c.my && !c.spawning) : []),
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
 * - always: W1N1 owned at level 2, with a spawn, the controller and a harvester that moves;
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

  const controller = structure(IDS.controller, 'controller', 'W1N1', 20, 5, { level: 2 });
  const structures = [structure(IDS.spawn, 'spawn', 'W1N1', 25, 25, { name: 'Spawn1' }), controller];
  if (offset >= 4) structures.push(structure(IDS.mystery, 'mysteryStructure', 'W1N1', 30, 5));

  const homeHostiles = offset >= 6 ? [creep(IDS.invader2, 'Invader2', 'W1N1', 30, 30, { my: false }, actions)] : [];
  const rooms = [room('W1N1', controller, [...creeps, ...homeHostiles])];
  if (scoutAlive) {
    // An unowned controller has no user, so the engine's `my` is undefined (V101).
    const neutral = { id: '5bbcaa9099f9b0a0f7f4b3c8', structureType: 'controller', level: 0, pos: pos('W2N1', 20, 20) };
    rooms.push(room('W2N1', neutral, [...creeps, ...hostiles]));
  }
  rooms.push(room('E0S0', undefined, creeps));
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: Object.fromEntries(rooms.map((r) => [r.name, r])),
    structures: Object.fromEntries(structures.map((s) => [s.id, s])),
  };
}
