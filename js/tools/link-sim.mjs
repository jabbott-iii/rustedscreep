// The link network's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/opening.rs and snapshot.rs over rustedscreep-core `economy::mining` and
// `economy::opening`; P3.2, ADR-024). W1N1 owned at RCL5, where CONTROLLER_STRUCTURES.link[5] allows two links: an
// open plain room with walls on its border ring, a spawn and ten extensions, all full; the controller; one source
// whose only walkable neighbour is the mining tile (walls on its other seven), with a container on that tile, worn
// between repair.walk_pct and repair.start_pct of CONTAINER_HITS and one hit lower on each loop (P3.5; the harness's
// own change, not the engine's decay, to show the snapshot reads `hits` every tick), and a source link next to it; and a controller link at CONTROLLER_LINK_RANGE (2) of the controller, so the bot places
// no link site. A miner (a bot miner name: role code m, home W1N1) with miner_work(SOURCE_ENERGY_CAPACITY) WORK, one
// CARRY and three MOVE parts stands on the mining tile with a full store, and an empty worker (role code w) stands
// next to the controller link. The source link holds more than the controller link has room for, so its first send
// leaves enough behind to send again while it cools down. The harness plays the engine's part as far as the bot
// can see it:
// - links are engine-shaped (V216): `store` is a Store over LINK_CAPACITY energy (makeStore, V196), `cooldown` the
//   raw `cooldown || 0`; the harness counts the bot's reads of `store`, `getUsedCapacity` and `cooldown`;
// - `transferEnergy(target, amount)` makes the game side's checks against the tick's start in V216's order (a
//   negative amount; a target that is missing, not this tick's object, not a link or the sender itself; not own; the
//   sender's cooldown; the sender inactive by V218's rule; no energy; a falsy amount made min(energy, the target's
//   room); less energy than the amount; a target the amount would overflow) and records the intent (one per object
//   and method, the last kept, V108). The processor (V217) clamps to the target's room, the target gets the amount
//   less Math.ceil(amount × LINK_LOSS_RATIO), the sender's cooldown rises by LINK_COOLDOWN × the Chebyshev range
//   between them, and every link's cooldown falls by 1 in the same tick's object pass. An inactive link could still
//   receive (V218); both links here are active;
// - the creeps' methods make the game side's checks against the tick's start (V108, V182, V187, V191, V192; for a
//   link, V220) and record the intent. After the loop the processor applies each object's intents, objects in the
//   order of their first call (the bot calls its link sends before the creep intents; that the intent list keeps
//   call order is assumed), and a creep's in creepActions order (V45: transfer, withdraw, harvest,
//   upgradeController): transfer and withdraw clamp to the room and the stock (V191, V192, V220), harvest takes
//   min(HARVEST_POWER per WORK part, the source's energy) and what overflows the store goes into the container on
//   the creep's tile (V183, V193), upgradeController spends min(WORK × UPGRADE_CONTROLLER_POWER, energy) (V188) and
//   restores the timer (V189); the source refills ENERGY_REGEN_TIME ticks after it first drops below its capacity
//   (V184, V215); moves resolve like the engine (V51, V52, as in js/tools/move-sim.mjs), with fatigue (V54, V55);
// - `spawnCreep` follows the runtime's checks (V171) and writes `Memory.creeps[name]` on OK (V181), and the processor
//   drops every accepted order, as it can (V173): no creep appears and nothing is charged, so the room stays full;
// - `room.createConstructionSite` is recorded, returns OK and places nothing (as in js/tools/spawn-sim.mjs).
// It checks, over the loops at offsets 1 to RUN - 1:
// - the snapshot reads each link's `store` and `getUsedCapacity('energy')` once a loop, and its `cooldown` once a loop
//   while it holds energy at the tick's start (an empty link cannot send, V216), none on the boot tick (the scene never
//   empties a link, so the skipped read is not reached here);
// - the miner harvests the source every loop and calls `transfer(sourceLink, 'energy')` with no amount on exactly
//   the loops its store would overflow with a harvest (store + miner_work × HARVEST_POWER > capacity) and the source
//   link's room at the tick's start fits its whole store; the first comes on the first loop (it starts full);
// - the source link calls `transferEnergy(controllerLink, amount)` on exactly the loops the network allows (active,
//   cooldown 0, at least links.send_min energy, a receiver with at least that much room), with that tick's controller
//   link object and two arguments, the amount min(sender energy, receiver room) at the tick's start, an integer of
//   at least links.send_min; never during its cooldown, never to itself; every call returns OK. The scene must show
//   a send, cooldown loops that would otherwise have sent, a send on the loop the cooldown ends, a loop held by a
//   full receiver, and a later send;
// - the worker calls `withdraw(controllerLink, 'energy')` with no amount on every loop it is empty next to a
//   controller link that holds its free space, and only when the link holds its free space, or when no send fits
//   the link (room below links.send_min) and it holds the smaller of opening.scavenge_min and the free space; the
//   worker also upgrades the controller;
// - the creeps call nothing else, every call returns OK, the only sites placed are extensions, every spawnCreep
//   names a worker with two arguments, and `Memory.creeps` keeps no entry after a loop (ADR-018);
// - `rustedscreep.opening()` after every loop: the mining part `W1N1: miners 1 (mined 1, harvests 1, requested 0,
//   idle 0), links 2 (linked 1, transfers T, sends S)` with that loop's transfer and send calls, `, controller link
//   (x,y)`, the source's tile, trip (range to the spawn - 1 tiles, PLAIN_COST each) and ` link (x,y)`, and a body of
//   miner_work WORK, `1 CARRY` and MOVE parts at a cost the room's capacity pays; the opening's plan shows the source
//   mined and linked; the room part `, containers 1 (under repair 1, lowest hits H)` with that loop's `hits` (P3.5:
//   the snapshot reads it every tick; under repair below repair.start_pct; the room's one worker is taken by
//   opening.min_upgraders, so this does not test the walk rule, which the native tests do); the sites part
//   `(0 containers, 0 links)`; and the end ` | link sends since boot: ordered O, sent S, failed F, energy E` against
//   the harness's calls;
// - no fault line, no incident, no link refusal line, no halt, one global.
// npm run check runs it (row 20); standalone: node js/tools/link-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { findFiltered, makeStore, packPos } from './lib/world.mjs';
import { ENGINE_CONSTANTS, commandResults, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
/** LINK_MMO.send_min (crates/rustedscreep-core/src/config.rs): LINK_CAPACITY / 4. */
const SEND_MIN = C.LINK_CAPACITY / 4;
/** REPAIR_MMO.start_pct and walk_pct (crates/rustedscreep-core/src/config.rs): percent of CONTAINER_HITS. */
const REPAIR_START_PCT = 80;
const REPAIR_WALK_PCT = 50;
/** The container's hits at offset 0: halfway between the two shares, so it is under repair (P3.5). */
const CONTAINER_WORN_HITS = Math.floor((C.CONTAINER_HITS * (REPAIR_START_PCT + REPAIR_WALK_PCT)) / 200);
/** OPENING_MMO.scavenge_min (config.rs): with no send fitting a controller link, a take of this much is worth it. */
const SCAVENGE_MIN = 50;
/** MOVEMENT_MMO's plain cost (config.rs): a trip's cost on plain is 2 per tile. */
const PLAIN_COST = 2;
/** economy::sizing::miner_work: WORK parts that empty a source in one regeneration window. */
const MINER_WORK = Math.ceil(C.SOURCE_ENERGY_CAPACITY / (C.ENERGY_REGEN_TIME * C.HARVEST_POWER));
/** What the miner's harvest adds a tick: the per-tick yield economy::mining checks the overflow with. */
const PER_TICK = MINER_WORK * C.HARVEST_POWER;
const ROOM = 'W1N1';
const T0 = 50000000;
const LEVEL = 5;
const SPAWN = { x: 25, y: 32 };
const SOURCE = { x: 15, y: 25 };
/** The source's only walkable neighbour: the bot's mining tile, where the container stands. */
const MINE = { x: 16, y: 25 };
const SOURCE_LINK = { x: 17, y: 24 };
const CONTROLLER = { x: 35, y: 25 };
/** At CONTROLLER_LINK_RANGE (2) of the controller, within RECEIVER_RANGE (3): a controller link. */
const CONTROLLER_LINK = { x: 33, y: 25 };
const WORKER_AT = { x: 34, y: 24 };
/** The first send moves min(700, 800 - 400) = 400, so 300 stay and the miner adds to them during the cooldown. */
const SOURCE_LINK_ENERGY = 700;
const CONTROLLER_LINK_ENERGY = 400;
const EXTENSIONS = Array.from({ length: 10 }, (_, i) => ({ x: 20 + i, y: 40 }));
const CAPACITY = C.SPAWN_ENERGY_CAPACITY + EXTENSIONS.length * C.EXTENSION_ENERGY_CAPACITY[LEVEL];
const MINER_BODY = [...Array(MINER_WORK).fill('work'), 'carry', 'move', 'move', 'move'];
const WORKER_BODY = [...Array(4).fill('work'), ...Array(4).fill('carry'), ...Array(4).fill('move')];
/** Long enough for the worker to empty its first load and take a second: three sends. */
const RUN = 70;
const SUMMARY = 'rustedscreep.opening()';

const id = (n) => `5bbcaa9099f9b0a0f7f4f${n.toString(16).padStart(3, '0')}`;
const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const range = (a, b) => Math.max(Math.abs(a.x - b.x), Math.abs(a.y - b.y));
const count = (body, part) => body.filter((p) => p === part).length;
const at = (p) => `(${p.x},${p.y})`;
/** A bot name (crates/rustedscreep-core/src/spawning/name.rs): role code, home room, `-`, base 36 serial. */
const botName = (role, time) => `${role}${ROOM}-${(BigInt(time) << 8n).toString(36)}`;
const MINER = botName('m', T0 - 2000);
const WORKER = botName('w', T0 - 1000);
const MINER_CAPACITY = C.CARRY_CAPACITY * count(MINER_BODY, 'carry');
const WORKER_CAPACITY = C.CARRY_CAPACITY * count(WORKER_BODY, 'carry');

/** Row-major terrain (V156): walls on the border ring and on the source's neighbours but the mining tile. */
const RAW = (() => {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) if (x === 0 || y === 0 || x === 49 || y === 49) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  for (const [dx, dy] of OFFSETS.slice(1)) {
    const x = SOURCE.x + dx;
    const y = SOURCE.y + dy;
    if (x !== MINE.x || y !== MINE.y) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  return raw;
})();
const isWall = (x, y) => (RAW[y * 50 + x] & C.TERRAIN_MASK_WALL) !== 0;

function makeWorld() {
  return {
    spawn: { id: id(1), name: 'Spawn1', ...SPAWN, energy: C.SPAWN_ENERGY_CAPACITY },
    extensions: EXTENSIONS.map((e, i) => ({ id: id(10 + i), ...e, energy: C.EXTENSION_ENERGY_CAPACITY[LEVEL] })),
    source: { id: id(2), ...SOURCE, energy: C.SOURCE_ENERGY_CAPACITY, capacity: C.SOURCE_ENERGY_CAPACITY, regenAt: null },
    controller: { id: id(3), ...CONTROLLER, level: LEVEL, progress: 0, downgradeTime: T0 + C.CONTROLLER_DOWNGRADE[LEVEL], upgraded: 0 },
    container: { id: id(4), ...MINE, energy: 0, hits: CONTAINER_WORN_HITS },
    // In Game.structures order (V218's tie order; the two stand at different ranges of the controller).
    links: [
      { id: id(5), name: 'source link', ...SOURCE_LINK, energy: SOURCE_LINK_ENERGY, cooldown: 0 },
      { id: id(6), name: 'controller link', ...CONTROLLER_LINK, energy: CONTROLLER_LINK_ENERGY, cooldown: 0 },
    ],
    creeps: [
      { id: id(30), name: MINER, body: MINER_BODY, ...MINE, energy: MINER_CAPACITY, fatigue: 0 },
      { id: id(31), name: WORKER, body: WORKER_BODY, ...WORKER_AT, energy: 0, fatigue: 0 },
    ],
    /** This tick's recorded intents, object id → { method: payload }, in the order of each object's first call. */
    intents: new Map(),
    /** Every creep and link call: { offset, actor, method, target, kind, args, result, start, ... }. */
    calls: [],
    /** What the processor did with each send: { offset, amount, loss, cooldown }. */
    sends: [],
    /** Per offset, per link id: the bot's reads of `store`, the `getUsedCapacity` arguments, the `cooldown` reads. */
    reads: [],
    /** Each tick's start: the links (energy, cooldown, active), the miner, the worker, the source's energy. */
    starts: [],
    spawnCalls: [],
    siteCalls: [],
    errors: [],
    context: null,
  };
}

/**
 * V218: a link is active while fewer than CONTROLLER_STRUCTURES.link[level] links stand nearer the controller, or as
 * near and earlier in the object order.
 */
function isActive(state, link) {
  const ctl = state.controller;
  const allowed = C.CONTROLLER_STRUCTURES.link[ctl.level] ?? 0;
  const mine = range(link, ctl);
  const index = state.links.indexOf(link);
  return state.links.filter((o, i) => o !== link && (range(o, ctl) < mine || (range(o, ctl) === mine && i < index))).length < allowed;
}

/** Why a creep cannot stand on (x, y), if it cannot: terrain, an obstacle structure or the source (V52). */
function isObstacle(state, x, y) {
  if (isWall(x, y)) return 'wall';
  const here = (o) => o.x === x && o.y === y;
  if ([state.spawn, state.source, state.controller, ...state.extensions, ...state.links].some(here)) return 'obstacle';
  return null;
}

function intentsOf(state, objectId) {
  if (!state.intents.has(objectId)) state.intents.set(objectId, {});
  return state.intents.get(objectId);
}

/** A link's game object for this tick: engine-shaped getters that count the bot's reads, and transferEnergy (V216). */
function linkObject(state, l, offset, objects) {
  const reads = (state.reads[offset] ??= {});
  const r = (reads[l.id] ??= { store: 0, used: [], cooldown: 0 });
  const inner = makeStore({ energy: l.energy }, { resourceCapacity: { energy: C.LINK_CAPACITY } });
  const store = new Proxy(inner, {
    get(target, prop) {
      if (prop !== 'getUsedCapacity') return target[prop];
      return (res) => {
        r.used.push(res);
        return target.getUsedCapacity(res);
      };
    },
  });
  const self = {
    id: l.id, structureType: 'link', my: true, owner: { username: 'bot' }, pos: pos(l.x, l.y), hits: C.LINK_HITS, hitsMax: C.LINK_HITS_MAX,
    energy: l.energy, energyCapacity: C.LINK_CAPACITY,
    get store() {
      r.store += 1;
      return store;
    },
    get cooldown() {
      r.cooldown += 1;
      return l.cooldown || 0;
    },
    transferEnergy(...args) {
      const [target, amount] = args;
      const to = objects.linkOf(target);
      const call = {
        offset, actor: l.name, method: 'transferEnergy', target: target?.id ?? null, kind: objects.kindOf(target),
        args: [target?.id ?? target, ...args.slice(1)], argCount: args.length, sameObject: target === objects.controllerLink, toSelf: target === self,
        result: null, start: { energy: l.energy, cooldown: l.cooldown, toEnergy: to?.energy ?? null },
      };
      state.calls.push(call);
      call.result = (() => {
        // V216, in its order.
        if (amount < 0) return C.ERR_INVALID_ARGS;
        if (!target?.id || !objects.registered(target) || !to || target === self) return C.ERR_INVALID_TARGET;
        if (!target.my) return C.ERR_NOT_OWNER;
        if (l.cooldown > 0) return C.ERR_TIRED;
        if (!isActive(state, l)) return C.ERR_RCL_NOT_ENOUGH;
        if (!l.energy) return C.ERR_NOT_ENOUGH_ENERGY;
        const n = amount || Math.min(l.energy, C.LINK_CAPACITY - to.energy);
        if (l.energy < n) return C.ERR_NOT_ENOUGH_ENERGY;
        if (to.energy + n > C.LINK_CAPACITY) return C.ERR_FULL;
        intentsOf(state, l.id).transfer = { id: to.id, amount: n };
        return C.OK;
      })();
      return call.result;
    },
  };
  return self;
}

/**
 * A creep's game object for this tick, with the game side's checks (refusals are recorded, not thrown). The checks
 * read the harness's own state of a target (`objects.stateOf`), so only the bot's reads of a link's getters count.
 */
function creepObject(state, c, offset, objects) {
  const work = count(c.body, 'work');
  const capacity = C.CARRY_CAPACITY * count(c.body, 'carry');
  const record = (method, target, args, result) => {
    state.calls.push({
      offset, actor: c.name, method, target: target?.id ?? null, kind: objects.kindOf(target), args, argCount: args.length, result,
      start: { energy: c.energy, x: c.x, y: c.y },
    });
    return result;
  };
  const near = (t) => t?.pos && range(c, t.pos) <= 1;
  const self = {
    id: c.id, name: c.name, my: true, spawning: false, fatigue: c.fatigue, owner: { username: 'bot' }, pos: pos(c.x, c.y),
    body: c.body.map((type) => ({ type, hits: 100 })), hits: 100 * c.body.length, hitsMax: 100 * c.body.length,
    store: makeStore({ energy: c.energy }, { capacity }),
    move(...args) {
      const d = +args[0];
      if (c.fatigue > 0) return record('move', null, args, C.ERR_TIRED);
      if (!count(c.body, 'move')) return record('move', null, args, C.ERR_NO_BODYPART);
      if (!Number.isInteger(d) || d < 1 || d > 8) return record('move', null, args, C.ERR_INVALID_ARGS);
      intentsOf(state, c.id).move = d;
      return record('move', null, args, C.OK);
    },
    harvest(...args) {
      const [target] = args;
      const rest = args.slice(1);
      if (!work) return record('harvest', target, rest, C.ERR_NO_BODYPART);
      if (!target?.id || objects.kindOf(target) !== 'source') return record('harvest', target, rest, C.ERR_INVALID_TARGET);
      if (!target.energy) return record('harvest', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (!near(target)) return record('harvest', target, rest, C.ERR_NOT_IN_RANGE);
      intentsOf(state, c.id).harvest = { id: target.id };
      return record('harvest', target, rest, C.OK);
    },
    transfer(...args) {
      // V191 and V220, in their order.
      const [target, resource, amount] = args;
      const rest = args.slice(1);
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('transfer', target, rest, C.ERR_INVALID_ARGS);
      if (!target?.id) return record('transfer', target, rest, C.ERR_INVALID_TARGET);
      if (objects.kindOf(target) === 'controller' && resource === C.RESOURCE_ENERGY) return self.upgradeController(target);
      const to = objects.stateOf(target);
      if (!to || !to.capacityFor(resource)) return record('transfer', target, rest, C.ERR_INVALID_TARGET);
      if (!near(target)) return record('transfer', target, rest, C.ERR_NOT_IN_RANGE);
      if (!c.energy) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      const free = to.capacity - to.energy;
      if (free <= 0) return record('transfer', target, rest, C.ERR_FULL);
      const n = amount || Math.min(c.energy, free);
      if (c.energy < n) return record('transfer', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (n > free) return record('transfer', target, rest, C.ERR_FULL);
      intentsOf(state, c.id).transfer = { id: target.id, amount: n };
      return record('transfer', target, rest, C.OK);
    },
    withdraw(...args) {
      // V192 and V220, in their order.
      const [target, resource, amount] = args;
      const rest = args.slice(1);
      if (amount < 0 || !C.RESOURCES_ALL.includes(resource)) return record('withdraw', target, rest, C.ERR_INVALID_ARGS);
      const from = objects.stateOf(target);
      if (!target?.id || !from || !from.capacityFor(resource)) return record('withdraw', target, rest, C.ERR_INVALID_TARGET);
      if (!near(target)) return record('withdraw', target, rest, C.ERR_NOT_IN_RANGE);
      const free = capacity - c.energy;
      if (!free) return record('withdraw', target, rest, C.ERR_FULL);
      const n = amount || Math.min(free, from.energy);
      if (n > free) return record('withdraw', target, rest, C.ERR_FULL);
      if (!n || from.energy < n) return record('withdraw', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      intentsOf(state, c.id).withdraw = { id: target.id, amount: n };
      return record('withdraw', target, rest, C.OK);
    },
    upgradeController(...args) {
      // V187, in its order.
      const [target] = args;
      const rest = args.slice(1);
      if (!work) return record('upgradeController', target, rest, C.ERR_NO_BODYPART);
      if (!c.energy) return record('upgradeController', target, rest, C.ERR_NOT_ENOUGH_RESOURCES);
      if (!target?.id || objects.kindOf(target) !== 'controller') return record('upgradeController', target, rest, C.ERR_INVALID_TARGET);
      if (range(c, target.pos) > 3) return record('upgradeController', target, rest, C.ERR_NOT_IN_RANGE);
      if (!target.my) return record('upgradeController', target, rest, C.ERR_NOT_OWNER);
      intentsOf(state, c.id).upgradeController = { id: target.id };
      return record('upgradeController', target, rest, C.OK);
    },
  };
  // The act layer's other bindings: nothing in this room calls for them; recorded (OK) so they show up as unexpected.
  for (const m of ['say', 'drop', 'heal', 'rangedHeal', 'repair', 'pull', 'build', 'pickup']) {
    self[m] = (...args) => record(m, args[0], args.slice(1), C.OK);
  }
  return self;
}

function sceneFor(state, offset) {
  state.intents.clear();
  const time = T0 + offset;
  const kinds = new Map();
  const states = new Map();
  const registered = new Set();
  const objects = {
    kindOf: (o) => (o && typeof o === 'object' ? kinds.get(o.id) ?? null : null),
    /** The harness's state of this tick's object `o` that has a store: { energy, capacity, capacityFor(res) }. */
    stateOf: (o) => (o && typeof o === 'object' && registered.has(o) ? states.get(o.id) ?? null : null),
    linkOf: (o) => (o && typeof o === 'object' && registered.has(o) && kinds.get(o.id) === 'link' ? state.links.find((l) => l.id === o.id) : null),
    registered: (o) => registered.has(o),
    controllerLink: null,
  };
  // A per-resource store holds energy only; a container's pool takes any resource (V196).
  const stored = (o, capacity, pool = false) => ({
    get energy() {
      return o.energy;
    },
    capacity,
    capacityFor: (res) => (pool || res === C.RESOURCE_ENERGY ? capacity : 0),
  });
  const ctl = state.controller;
  const controller = {
    id: ctl.id, structureType: 'controller', my: true, owner: { username: 'bot' }, level: ctl.level, pos: pos(ctl.x, ctl.y),
    // V189's getters: progress and progressTotal below level 8, ticksToDowngrade from downgradeTime; not upgrade-blocked.
    progress: ctl.progress, progressTotal: C.CONTROLLER_LEVELS[ctl.level], ticksToDowngrade: ctl.downgradeTime - time,
  };
  const sp = state.spawn;
  const extCapacity = C.EXTENSION_ENERGY_CAPACITY[ctl.level];
  // V175: the room's energy and capacity are its spawn's and extensions'.
  const energyAvailable = sp.energy + state.extensions.reduce((s, e) => s + e.energy, 0);
  const created = new Set();
  const spawn = {
    id: sp.id, name: sp.name, structureType: 'spawn', my: true, owner: { username: 'bot' }, pos: pos(sp.x, sp.y),
    store: makeStore({ energy: sp.energy }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
    // Never busy: the harness drops every order (V175: null when idle).
    spawning: null,
    spawnCreep(...args) {
      const [body, name, opts = {}] = args;
      const call = { offset, args: args.length, body: Array.isArray(body) ? [...body] : body, name, result: null };
      state.spawnCalls.push(call);
      call.result = (() => {
        // V171, in its order.
        if (!name || typeof opts !== 'object' || String(name).length > 100) return C.ERR_INVALID_ARGS;
        if (state.creeps.some((c) => c.name === name) || created.has(name)) return C.ERR_NAME_EXISTS;
        if (!Array.isArray(body) || !body.length || body.length > C.MAX_CREEP_SIZE || body.some((p) => !C.BODYPARTS_ALL.includes(p))) return C.ERR_INVALID_ARGS;
        if (energyAvailable < body.reduce((s, p) => s + C.BODYPART_COST[p], 0)) return C.ERR_NOT_ENOUGH_ENERGY;
        if (opts.dryRun) return C.OK;
        created.add(name);
        // V181: the runtime writes the creep's Memory entry; the processor then drops the order (V173).
        const mem = state.context.Memory;
        if (mem.creeps === undefined) mem.creeps = {};
        if (mem.creeps && typeof mem.creeps === 'object') mem.creeps[name] = opts.memory || mem.creeps[name] || {};
        return C.OK;
      })();
      return call.result;
    },
  };
  const extensions = state.extensions.map((e) => ({
    id: e.id, structureType: 'extension', my: true, owner: { username: 'bot' }, pos: pos(e.x, e.y),
    store: makeStore({ energy: e.energy }, { resourceCapacity: { energy: extCapacity } }),
  }));
  const links = state.links.map((l) => linkObject(state, l, offset, objects));
  objects.controllerLink = links[1];
  const ct = state.container;
  const container = {
    id: ct.id, structureType: 'container', pos: pos(ct.x, ct.y), hits: ct.hits - offset, hitsMax: C.CONTAINER_HITS,
    store: makeStore({ energy: ct.energy }, { capacity: C.CONTAINER_CAPACITY }),
  };
  const src = state.source;
  const source = {
    id: src.id, energy: src.energy, energyCapacity: src.capacity, pos: pos(src.x, src.y),
    ticksToRegeneration: src.regenAt === null ? undefined : src.regenAt - time,
  };
  kinds.set(controller.id, 'controller');
  kinds.set(spawn.id, 'spawn');
  kinds.set(source.id, 'source');
  kinds.set(container.id, 'container');
  for (const e of extensions) kinds.set(e.id, 'extension');
  for (const l of links) kinds.set(l.id, 'link');
  for (const o of [controller, spawn, source, container, ...extensions, ...links]) registered.add(o);
  states.set(sp.id, stored(sp, C.SPAWN_ENERGY_CAPACITY));
  for (const e of state.extensions) states.set(e.id, stored(e, extCapacity));
  for (const l of state.links) states.set(l.id, stored(l, C.LINK_CAPACITY));
  states.set(ct.id, stored(ct, C.CONTAINER_CAPACITY, true));
  const creeps = state.creeps.map((c) => creepObject(state, c, offset, objects));
  const owned = [controller, spawn, ...extensions, ...links];
  const lists = {
    [C.FIND_SOURCES]: [source], [C.FIND_STRUCTURES]: [...owned, container], [C.FIND_MY_STRUCTURES]: owned,
    [C.FIND_MY_SPAWNS]: [spawn], [C.FIND_CREEPS]: creeps, [C.FIND_MY_CREEPS]: creeps,
  };
  const room = {
    name: ROOM, controller, energyAvailable, energyCapacityAvailable: CAPACITY,
    find: (type, opts) => findFiltered(lists[type] ?? [], opts),
    getTerrain: () => ({ getRawBuffer: () => RAW.slice() }),
    createConstructionSite(...args) {
      state.siteCalls.push({ offset, args });
      return C.OK;
    },
  };
  const miner = state.creeps.find((c) => c.name === MINER);
  const worker = state.creeps.find((c) => c.name === WORKER);
  state.starts[offset] = {
    links: state.links.map((l) => ({ energy: l.energy, cooldown: l.cooldown, active: isActive(state, l) })),
    miner: { energy: miner.energy, x: miner.x, y: miner.y },
    worker: { energy: worker.energy, x: worker.x, y: worker.y },
    source: src.energy,
  };
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries(owned.map((s) => [s.id, s])),
  };
}

/** The processor after loop `offset`: each object's intents in call order, moves, then the object pass (V215). */
function advance(state, offset) {
  const time = T0 + offset;
  const byId = new Map([...state.creeps, ...state.links].map((o) => [o.id, o]));
  const holder = (targetId) => {
    if (targetId === state.spawn.id) return [state.spawn, C.SPAWN_ENERGY_CAPACITY];
    if (targetId === state.container.id) return [state.container, C.CONTAINER_CAPACITY];
    const e = state.extensions.find((x) => x.id === targetId);
    if (e) return [e, C.EXTENSION_ENERGY_CAPACITY[state.controller.level]];
    const l = state.links.find((x) => x.id === targetId);
    return l ? [l, C.LINK_CAPACITY] : [null, 0];
  };
  for (const [objectId, i] of state.intents) {
    const o = byId.get(objectId);
    if (!o) continue;
    if (state.links.includes(o)) {
      // links/transfer.js (V217): nothing (and no cooldown) unless every check passes; then the clamp, the loss, the cooldown.
      const t = i.transfer;
      const to = t && state.links.find((x) => x.id === t.id);
      if (!to || o.energy < t.amount || t.amount < 0 || o.cooldown > 0 || !isActive(state, o) || to.energy >= C.LINK_CAPACITY) continue;
      const amount = Math.min(t.amount, C.LINK_CAPACITY - to.energy);
      const loss = Math.ceil(amount * C.LINK_LOSS_RATIO);
      to.energy += amount - loss;
      o.energy -= amount;
      o.cooldown += C.LINK_COOLDOWN * range(o, to);
      state.sends.push({ offset, amount, loss, cooldown: o.cooldown });
      continue;
    }
    const c = o;
    const capacity = C.CARRY_CAPACITY * count(c.body, 'carry');
    const work = count(c.body, 'work');
    // creepActions order (V45), positions at the start of the tick (moves come after).
    if (i.transfer) {
      const [to, cap] = holder(i.transfer.id);
      if (to && range(c, to) <= 1 && c.energy >= i.transfer.amount && to.energy < cap) {
        const n = Math.min(i.transfer.amount, cap - to.energy);
        to.energy += n;
        c.energy -= n;
      }
    }
    if (i.withdraw) {
      const [from] = holder(i.withdraw.id);
      if (from && range(c, from) <= 1) {
        const n = Math.min(i.withdraw.amount, capacity - c.energy, from.energy);
        if (n > 0) {
          from.energy -= n;
          c.energy += n;
        }
      }
    }
    if (i.harvest) {
      const s = state.source;
      if (s.energy && range(c, s) <= 1) {
        const n = Math.min(s.energy, C.HARVEST_POWER * work);
        s.energy -= n;
        c.energy += n;
        // V183, V193: what overflows goes into the container on the creep's tile, the rest onto a pile there.
        if (c.energy > capacity) {
          const over = c.energy - capacity;
          c.energy = capacity;
          const ct = state.container;
          const kept = ct.x === c.x && ct.y === c.y ? Math.min(over, C.CONTAINER_CAPACITY - ct.energy) : 0;
          ct.energy += kept;
          if (kept < over) state.errors.push(`offset ${offset}: ${c.name} dropped ${over - kept} energy on a pile, which the model has none of`);
        }
      }
    }
    if (i.upgradeController) {
      const ctl = state.controller;
      if (c.energy && range(c, ctl) <= 3) {
        const effect = Math.min(work * C.UPGRADE_CONTROLLER_POWER, c.energy);
        if (ctl.progress + effect >= C.CONTROLLER_LEVELS[ctl.level]) state.errors.push(`offset ${offset}: the controller would level up, which the model has no level-up for`);
        ctl.progress += effect;
        ctl.upgraded += effect;
        c.energy -= effect;
      }
    }
  }
  applyMoves(state, offset);
  // Creep ticks (V55): every creep sheds 2 fatigue per MOVE part.
  for (const c of state.creeps) c.fatigue = Math.max(0, c.fatigue - 2 * count(c.body, 'move'));
  // Link ticks (V217): the cooldown falls by 1, on the tick of a send too.
  for (const l of state.links) if (l.cooldown > 0) l.cooldown -= 1;
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
  const ctl = state.controller;
  if (ctl.upgraded) ctl.downgradeTime = Math.min(ctl.downgradeTime + C.CONTROLLER_DOWNGRADE_RESTORE + 1, time + C.CONTROLLER_DOWNGRADE[ctl.level] + 1);
  else if (time >= ctl.downgradeTime - 1) state.errors.push(`offset ${offset}: the controller downgrades`);
  ctl.upgraded = 0;
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
    const weight = c.body.filter((p) => p !== 'move' && p !== 'carry').length + Math.ceil(c.energy / C.CARRY_CAPACITY);
    c.fatigue += weight * 2;
  }
}

/** What economy::mining must order at a tick's start `s`: the miner's transfer, and the send's amount (0 for none). */
function expected(s) {
  const [sl, cl] = s.links;
  const store = s.miner.energy;
  const nextToLink = range(s.miner, SOURCE_LINK) <= 1;
  const transfer = nextToLink && store > 0 && store + PER_TICK > MINER_CAPACITY && C.LINK_CAPACITY - sl.energy >= store;
  const room = C.LINK_CAPACITY - cl.energy;
  const send = sl.active && sl.cooldown === 0 && sl.energy >= SEND_MIN ? Math.min(sl.energy, room) : 0;
  return { transfer, send: send >= SEND_MIN ? send : 0, room };
}

export function linkSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  const memoryEntries = [];
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context) => {
      state.context = context;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      // The bot deletes an accepted order's entry on the tick of the OK (ADR-018), so none is left after the loop.
      memoryEntries.push(...Object.keys(state.context.Memory?.creeps ?? {}).map((n) => [offset, n]));
      return offset >= 1 ? SUMMARY : null;
    },
  });
  const problems = [...state.errors.slice(0, 10)];
  if (state.errors.length > 10) problems.push(`... ${state.errors.length - 10} more model errors`);
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|rustedscreep links:/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);
  const loops = Array.from({ length: RUN - 1 }, (_, k) => k + 1);
  const sl = state.links[0];
  const cl = state.links[1];

  // The snapshot's link reads: store and getUsedCapacity('energy') once a loop per link, cooldown once a loop while the
  // link holds energy at the tick's start; none on the boot tick.
  for (let o = 0; o < RUN; o++) {
    state.links.forEach((l, i) => {
      const r = state.reads[o]?.[l.id] ?? { store: 0, used: [], cooldown: 0 };
      const want = o === 0 ? 0 : 1;
      const held = state.starts[o]?.links?.[i]?.energy ?? 0;
      const wantCooldown = want && held > 0 ? 1 : 0;
      const used = r.used.filter((res) => res === C.RESOURCE_ENERGY).length;
      if (r.store !== want || used !== want || r.used.length !== want || r.cooldown !== wantCooldown) {
        problems.push(`offset ${o}: the ${l.name}'s store read ${r.store}, getUsedCapacity(${r.used.map(String).join(', ')}), cooldown read ${r.cooldown} with ${held} energy; expected ${want}, ${want} and ${wantCooldown}, for energy`);
      }
    });
  }

  // Every call: the creeps' methods and targets, and every result OK.
  const allowed = {
    [`${MINER} harvest`]: (c) => c.target === state.source.id && c.argCount === 0,
    [`${MINER} transfer`]: (c) => c.target === sl.id && c.argCount === 2 && c.args[0] === C.RESOURCE_ENERGY && c.args[1] === undefined,
    [`${WORKER} withdraw`]: (c) => c.target === cl.id && c.argCount === 2 && c.args[0] === C.RESOURCE_ENERGY && c.args[1] === undefined,
    [`${WORKER} upgradeController`]: (c) => c.target === state.controller.id && c.argCount === 0,
    [`${MINER} move`]: (c) => c.argCount === 1 && Number.isInteger(c.args[0]),
    [`${WORKER} move`]: (c) => c.argCount === 1 && Number.isInteger(c.args[0]),
    [`${sl.name} transferEnergy`]: (c) => c.argCount === 2 && c.sameObject && !c.toSelf && c.target === cl.id,
  };
  const byMethod = {};
  for (const c of state.calls) {
    const key = `${c.actor} ${c.method}`;
    byMethod[c.method] = (byMethod[c.method] ?? 0) + 1;
    if (!allowed[key]?.(c)) problems.push(`offset ${c.offset}: unexpected call ${c.method} by ${c.actor} on ${c.target} (${c.kind}) with ${c.argCount} argument(s) ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) problems.push(`offset ${c.offset}: ${c.method} by ${c.actor} returned ${c.result}`);
  }
  const callsAt = (o, actor, method) => state.calls.filter((c) => c.offset === o && c.actor === actor && c.method === method);

  // The miner: a harvest every loop, a transfer into the source link exactly when its store would overflow and the
  // link has room for all of it; the source link: a send exactly when the network allows, of the amount it allows.
  const held = { cooldown: [], full: [] };
  for (const o of loops) {
    const s = state.starts[o];
    const want = expected(s);
    if (callsAt(o, MINER, 'harvest').length !== 1) problems.push(`offset ${o}: ${callsAt(o, MINER, 'harvest').length} miner harvests, expected 1`);
    const transfers = callsAt(o, MINER, 'transfer');
    if (transfers.length !== (want.transfer ? 1 : 0)) {
      problems.push(`offset ${o}: ${transfers.length} miner transfers with ${s.miner.energy} of ${MINER_CAPACITY} held and the source link at ${s.links[0].energy}; expected ${want.transfer ? 1 : 0}`);
    }
    const sends = callsAt(o, sl.name, 'transferEnergy');
    const amounts = sends.map((c) => c.args[1]);
    if (JSON.stringify(amounts) !== JSON.stringify(want.send ? [want.send] : [])) {
      problems.push(`offset ${o}: sends ${JSON.stringify(amounts)} with the source link at ${s.links[0].energy} (cooldown ${s.links[0].cooldown}) and the controller link at ${s.links[1].energy}; expected ${want.send ? `[${want.send}]` : 'none'}`);
    }
    for (const c of sends) {
      const a = c.args[1];
      if (!Number.isInteger(a) || a < SEND_MIN || a > Math.min(c.start.energy, C.LINK_CAPACITY - c.start.toEnergy) || c.start.cooldown !== 0) {
        problems.push(`offset ${o}: transferEnergy(${a}) with ${c.start.energy} energy, cooldown ${c.start.cooldown}, the receiver at ${c.start.toEnergy}`);
      }
    }
    // Loops on which only the cooldown, or only a full receiver, held a send back.
    const [l0, l1] = s.links;
    const ready = l0.energy >= SEND_MIN && Math.min(l0.energy, C.LINK_CAPACITY - l1.energy) >= SEND_MIN;
    if (ready && l0.cooldown > 0) held.cooldown.push(o);
    if (l0.cooldown === 0 && l0.energy >= SEND_MIN && C.LINK_CAPACITY - l1.energy < SEND_MIN) held.full.push(o);
  }
  const sendOffsets = state.calls.filter((c) => c.method === 'transferEnergy').map((c) => c.offset);
  const transferOffsets = state.calls.filter((c) => c.actor === MINER && c.method === 'transfer').map((c) => c.offset);
  const cooldownTicks = C.LINK_COOLDOWN * range(SOURCE_LINK, CONTROLLER_LINK);
  if (transferOffsets[0] !== 1 || transferOffsets.length < 2) problems.push(`miner transfers at offsets ${JSON.stringify(transferOffsets)}, expected the first at 1 and more after`);
  if (sendOffsets.length < 3 || sendOffsets[0] !== 1 || sendOffsets[1] !== 1 + cooldownTicks) {
    problems.push(`sends at offsets ${JSON.stringify(sendOffsets)}, expected one at 1, one at ${1 + cooldownTicks} (the cooldown's end) and a later one`);
  }
  if (!held.cooldown.some((o) => o > sendOffsets[0] && o < sendOffsets[1])) problems.push(`no loop between the first two sends where only the cooldown held one back (${JSON.stringify(held.cooldown)})`);
  if (!held.full.length) problems.push('no loop where only a full controller link held a send back');
  if (state.sends.length !== sendOffsets.length) problems.push(`the processor applied ${state.sends.length} of ${sendOffsets.length} sends`);

  // The worker: a withdraw from the controller link whenever it is empty next to one holding its free space, and only
  // when the link is worth a take; and upgrades.
  const withdraws = state.calls.filter((c) => c.actor === WORKER && c.method === 'withdraw');
  for (const o of loops) {
    const s = state.starts[o];
    const free = WORKER_CAPACITY - s.worker.energy;
    const link = s.links[1].energy;
    const got = callsAt(o, WORKER, 'withdraw').length;
    const full = C.LINK_CAPACITY - link < SEND_MIN;
    const worth = link >= free || (full && link >= Math.min(SCAVENGE_MIN, free));
    if (got && !worth) problems.push(`offset ${o}: the worker withdrew with ${free} free from a controller link holding ${link}`);
    if (!got && s.worker.energy === 0 && range(s.worker, CONTROLLER_LINK) <= 1 && link >= free) {
      problems.push(`offset ${o}: the empty worker next to a controller link holding ${link} did not withdraw`);
    }
  }
  const upgrades = state.calls.filter((c) => c.actor === WORKER && c.method === 'upgradeController').length;
  if (withdraws.length < 2 || !upgrades) problems.push(`the worker withdrew ${withdraws.length} time(s) and upgraded ${upgrades} time(s), expected 2 or more and some`);

  // Sites: extensions only (the container and both links stand, and RCL5 allows two links); spawnCreep: workers only.
  for (const { offset, args } of state.siteCalls) if (args[2] !== 'extension') problems.push(`offset ${offset}: createConstructionSite(${args.join(', ')})`);
  for (const c of state.spawnCalls) {
    if (c.args !== 2 || !Array.isArray(c.body) || !c.name?.startsWith(`w${ROOM}-`) || c.result !== C.OK) problems.push(`spawnCreep ${JSON.stringify(c)}`);
  }
  if (memoryEntries.length) problems.push(`Memory.creeps kept entries after the loop: ${JSON.stringify(memoryEntries.slice(0, 4))}`);
  if (/"[mw][WE]\d+[NS]\d+-/.test(world.memoryJson)) problems.push(`the stored Memory holds a creep entry: ${world.memoryJson.slice(0, 200)}`);

  // The summary after every loop.
  const results = commandResults(world, SUMMARY);
  if (results.length !== loops.length) problems.push(`${results.length} opening summaries, expected ${loops.length}`);
  const tiles = range(MINE, SPAWN) - 1;
  const bodyRe = new RegExp(` link ${at(SOURCE_LINK).replace(/[()]/g, '\\$&')} body ${MINER_WORK} WORK 1 CARRY (\\d+) MOVE \\((\\d+) energy\\);`);
  loops.forEach((o, k) => {
    const r = results[k] ?? '';
    const sent = state.calls.filter((c) => c.method === 'transferEnergy' && c.offset <= o);
    const ok = sent.filter((c) => c.result === C.OK);
    const sites = state.siteCalls.filter((c) => c.offset <= o).length;
    const parts = [
      `rustedscreep opening at tick ${T0 + o}: ${ROOM}: workers 1 of `,
      ` source ${at(SOURCE)} open 1 want `,
      ' mined linked, ',
      `, containers 1 (under repair 1, lowest hits ${CONTAINER_WORN_HITS - o});`,
      ` | mining: ${ROOM}: miners 1 (mined 1, harvests 1, requested 0, idle 0), links 2 (linked 1, transfers ` +
        `${callsAt(o, MINER, 'transfer').length}, sends ${callsAt(o, sl.name, 'transferEnergy').length}), controller link ${at(CONTROLLER_LINK)}, ` +
        `source ${at(SOURCE)} tile ${at(MINE)} trip ${tiles} tiles cost ${tiles * PLAIN_COST} link ${at(SOURCE_LINK)} body `,
      ` | sites: picked ${sites} (0 containers, 0 links), placed ${sites}, refused 0, never seen `,
    ];
    for (const p of parts) if (!r.includes(p)) problems.push(`opening summary at offset ${o} lacks "${p}": ${r}`);
    const end = ` | link sends since boot: ordered ${sent.length}, sent ${ok.length}, failed ${sent.length - ok.length}, energy ${ok.reduce((s, c) => s + c.args[1], 0)}`;
    if (!r.endsWith(end)) problems.push(`opening summary at offset ${o} does not end with "${end}": ${r}`);
    const body = bodyRe.exec(r);
    const moves = Number(body?.[1]);
    const cost = MINER_WORK * C.BODYPART_COST.work + C.BODYPART_COST.carry + moves * C.BODYPART_COST.move;
    if (!body || moves < 1 || Number(body[2]) !== cost || cost > CAPACITY) problems.push(`opening summary at offset ${o}: no miner body of ${MINER_WORK} WORK, 1 CARRY and MOVE within ${CAPACITY} energy: ${r}`);
  });

  if (problems.length) throw new Error(`link-sim:\n  ${problems.slice(0, 25).join('\n  ')}${problems.length > 25 ? `\n  ... ${problems.length - 25} more` : ''}`);
  return {
    ticks: world.ticksRun, byMethod, transferOffsets, withdraws: withdraws.map((c) => c.offset), upgrades,
    sends: state.sends.map((s) => `${s.offset}:${s.amount}-${s.loss}`), heldCooldown: held.cooldown.length, heldFull: held.full.length,
    spawnCalls: state.spawnCalls.length, sites: state.siteCalls.map((c) => `${c.offset}:(${c.args[0]},${c.args[1]})`), summary: results.at(-1),
  };
}

if (isMain(import.meta.url)) {
  try {
    const r = linkSim(process.argv[2]);
    console.log(`link-sim ok: ${r.ticks} ticks; sends (offset:amount-loss) ${r.sends.join(' ')}; held back by the cooldown on ${r.heldCooldown} loop(s), ` +
      `by a full controller link on ${r.heldFull}; miner transfers at ${r.transferOffsets.join(' ')}; worker withdraws at ${r.withdraws.join(' ')}, ` +
      `${r.upgrades} upgrades; calls ${JSON.stringify(r.byMethod)}; ${r.spawnCalls} spawnCreep calls (orders dropped); extension sites ${r.sites.join(' ')} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
