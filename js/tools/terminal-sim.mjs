// Terminal balancing's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/terminals.rs and snapshot.rs `read_balance` over rustedscreep-core
// `economy::terminals`; ROADMAP P9.4, ADR-047: the base minerals move, the source haulers fill a send's fee).
// Four owned rooms, each an open plain room with walls on its border ring, a controller and an own terminal, with no
// source and no mineral (so mining, hauling, the opening's workers and mineral mining stay inert):
// - GIVER_H (W1N1) at LEVEL, the first level CONTROLLER_STRUCTURES allows a terminal, with the only spawn, holding
//   STOCK of hydrogen;
// - GIVER_O (W3N1) at LEVEL, holding STOCK of oxygen;
// - TAKER (W1N3) at LEVEL, holding nothing;
// - LOW (W2N2) at LEVEL - 1, whose terminal the level does not allow (V439): no member, never read, never sent to.
// As in factory-sim.mjs, an idle own creep whose name the bot did not give (KEEPER) stands in each room from the start,
// so neither the opening's zero-creep recovery nor a spawn's help for a room with no creep orders a worker.
// Each terminal of the three at LEVEL starts with ENERGY energy, enough for its sends' fees, so no send waits for one
// (the fee's bank needs hauled sources; the native tests cover it). The ranges are all RANGE (2).
// The run starts with the overrides of CONFIG_LINES in segment 97 (the runtime config's record): the planner, the
// interim sites, remote mining, the signer, scouting, claim scoring, the labs and the factory off, and
// `terminals.read_ticks` READ_TICKS. The record loads on the global's second loop, so the first read runs with the
// profile's values (the labs may read the terminals' stocks on it, which the balance read then reuses).
// The harness plays the engine's part as far as the bot can see it:
// - each terminal's `store` and `cooldown` getters count their reads; the store is one pool (V401);
// - `send(resourceType, amount, destination, description)` makes the game side's checks in V435's order and records
//   the intent; after the loop the global pass (V437, V438) drops it on a cooldown or to a room with no own terminal at
//   its level, clips the amount to the destination's free room, takes the amount and the fee for what arrives
//   (`ceil(amount × (1 - exp(-range / 30)))`, V432, over the larger room distance, V433), and sets `cooldownTime` to
//   the loop plus TERMINAL_COOLDOWN, so `cooldown` reads 0 again that many loops later (V441).
// It checks:
// - the balance reads (each member terminal's `cooldown`) all on one loop or none, never the low room's (whose `store`
//   only the labs may read, on the loop before their override loads); the first on
//   the first loop after the boot loop, the next TERMINAL_COOLDOWN loops after each loop that sent, and READ_TICKS
//   loops after a loop that sent nothing;
// - four sends of KEEP units each: hydrogen from GIVER_H and oxygen from GIVER_O to each other room at LEVEL, two on
//   the first read loop and two on the next (a terminal sends once a plan); each with three arguments (no
//   description), the fee the engine's, and every call OK;
// - their log lines, once each;
// - nothing lost: every room at LEVEL ends with KEEP of hydrogen and of oxygen, the givers with STOCK - 2 × KEEP, the
//   low room's terminal unchanged, and each giver's energy less its two fees;
// - `rustedscreep.terminals()` at the end: the settings, the members, the plans and each giver's counters;
// - no `spawnCreep` or `createConstructionSite` call; no fault, error or trap line, no model error, no halt, one global.
// npm run check runs it (row 30); standalone: node js/tools/terminal-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { findFiltered, makeStore, packPos } from './lib/world.mjs';
import { CONFIG_SEGMENT, ENGINE_CONSTANTS, commandResults, configRecord, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0, the fixture the sandbox puts on the global (smoke.mjs ENGINE_CONSTANTS). */
const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
const T0 = 50000000;
/** The first level that allows a terminal (CONTROLLER_STRUCTURES.terminal). */
const LEVEL = [1, 2, 3, 4, 5, 6, 7, 8].find((l) => C.CONTROLLER_STRUCTURES.terminal[l] >= 1);
const CAPACITY = C.CONTROLLER_STRUCTURES.spawn[LEVEL] * C.SPAWN_ENERGY_CAPACITY
  + C.CONTROLLER_STRUCTURES.extension[LEVEL] * C.EXTENSION_ENERGY_CAPACITY[LEVEL];
const H = C.RESOURCE_HYDROGEN;
const O = C.RESOURCE_OXYGEN;
/** TERMINALS_MMO (crates/rustedscreep-core/src/config.rs): mineral_keep is LAB_MINERAL_CAPACITY. */
const KEEP = C.LAB_MINERAL_CAPACITY;
const SEND_MIN = 1000;
const ENERGY_KEEP = 1000;
const MAX_RANGE = 10;
const STOCK = 3 * KEEP;
const ENERGY = 1000;
const READ_TICKS = 25;
/** The engine's fee divisor (utils.js calcTerminalEnergyCost, V432; screeps-game-api TERMINAL_SEND_COST_SCALE). */
const SEND_COST_SCALE = 30;
const RUN = 120;
const SUMMARY = 'rustedscreep.terminals()';
const KEEPER = { name: 'Keeper', x: 40, y: 40 };
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
  'factory.enabled=false',
  `terminals.read_ticks=${READ_TICKS}`,
];

const id = (n) => `5bbcaa9099f9b0a0f7f4e${n.toString(16).padStart(3, '0')}`;
const used = (store) => Object.values(store).reduce((s, v) => s + v, 0);
/** The engine's roomNameToXY (utils.js L374-392, V433). */
const xy = (name) => {
  const m = /^([WE])(\d+)([NS])(\d+)$/.exec(name);
  const x = +m[2];
  const y = +m[4];
  return [m[1] === 'W' ? -x - 1 : x, m[3] === 'N' ? -y - 1 : y];
};
/** calcRoomsDistance without the wrap (V433): the sandbox's rooms are far from the world's edge. */
const roomRange = (a, b) => {
  const [ax, ay] = xy(a);
  const [bx, by] = xy(b);
  return Math.max(Math.abs(ax - bx), Math.abs(ay - by));
};
/** calcTerminalEnergyCost (V432). */
const fee = (amount, range) => Math.ceil(amount * (1 - Math.exp(-range / SEND_COST_SCALE)));
const allows = (level) => (C.CONTROLLER_STRUCTURES.terminal[level] ?? 0) >= 1;

const ROOMS = [
  { name: 'W1N1', level: LEVEL, store: { [H]: STOCK, energy: ENERGY }, spawn: true },
  { name: 'W3N1', level: LEVEL, store: { [O]: STOCK, energy: ENERGY } },
  { name: 'W1N3', level: LEVEL, store: { energy: ENERGY } },
  { name: 'W2N2', level: LEVEL - 1, store: { energy: ENERGY } },
];
const [GIVER_H, GIVER_O, TAKER, LOW] = ROOMS.map((r) => r.name);
const MEMBERS = [GIVER_H, GIVER_O, TAKER];
const RANGE = roomRange(GIVER_H, GIVER_O);
const FEE = fee(KEEP, RANGE);
const WANT_SENDS = [
  [GIVER_H, H, GIVER_O], [GIVER_H, H, TAKER], [GIVER_O, O, GIVER_H], [GIVER_O, O, TAKER],
];
const lineOf = ([from, kind, to]) => `rustedscreep terminals: ${from} sends ${KEEP} ${kind} to ${to} (fee ${FEE}, range ${RANGE})`;

/** Row-major terrain (V156): walls on the border ring. */
const RAW = (() => {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) if (x === 0 || y === 0 || x === 49 || y === 49) raw[y * 50 + x] = C.TERRAIN_MASK_WALL;
  }
  return raw;
})();

function makeWorld() {
  let n = 10;
  return {
    rooms: ROOMS.map((r) => ({
      ...r,
      terminal: { id: id(n++), x: 30, y: 24, store: { ...r.store }, cooldownTime: 0 },
      controller: { id: id(n++), x: 40, y: 10 },
      spawnObj: r.spawn ? { id: id(n++), name: 'Spawn1', x: 25, y: 32 } : null,
      keeperId: id(n++),
    })),
    /** This loop's recorded sends: terminal id → payload (one slot an object, V436). */
    intents: new Map(),
    calls: [],
    spawnCalls: [],
    siteCalls: [],
    /** Per offset, per room: the bot's reads of the terminal's `store` and `cooldown`. */
    reads: [],
    sent: [],
    errors: [],
  };
}

/** The idle own creep (KEEPER): any call it gets is recorded as a model error. */
function keeperObject(state, pos, r) {
  const self = {
    id: r.keeperId, name: `${KEEPER.name}${r.name}`, my: true, spawning: false, fatigue: 0, owner: { username: 'bot' }, pos: pos(KEEPER.x, KEEPER.y),
    body: [{ type: 'move', hits: 100 }], hits: 100, hitsMax: 100, store: makeStore({}, { capacity: 0 }), ticksToLive: C.CREEP_LIFE_TIME,
  };
  for (const m of ['move', 'say', 'withdraw', 'transfer', 'harvest', 'build', 'repair', 'pickup', 'drop', 'suicide', 'upgradeController']) {
    self[m] = (...args) => {
      state.errors.push(`the keeper got ${m}(${JSON.stringify(args.slice(1))})`);
      return C.OK;
    };
  }
  return self;
}

function sceneFor(state, offset) {
  state.intents.clear();
  const reads = (state.reads[offset] = {});
  const rooms = {};
  const structures = {};
  const allCreeps = {};
  for (const r of state.rooms) {
    const pos = (x, y) => ({ x, y, roomName: r.name, __packedPos: packPos(r.name, x, y) });
    const tm = r.terminal;
    const count = (reads[r.name] = { store: 0, cooldown: 0 });
    const store = makeStore(tm.store, { capacity: allows(r.level) ? C.TERMINAL_CAPACITY : 0 });
    const terminal = {
      id: tm.id, structureType: 'terminal', my: true, owner: { username: 'bot' }, pos: pos(tm.x, tm.y),
      hits: C.TERMINAL_HITS, hitsMax: C.TERMINAL_HITS,
      get store() {
        count.store += 1;
        return store;
      },
      get cooldown() {
        count.cooldown += 1;
        return Math.max(0, tm.cooldownTime - offset);
      },
      send(...args) {
        // V435, in its order (the terminal is the bot's).
        const [resource, amount, target, description] = args;
        const call = (result) => {
          state.calls.push({ offset, room: r.name, method: 'send', args, argCount: args.length, result });
          return result;
        };
        if (!allows(r.level)) return call(C.ERR_RCL_NOT_ENOUGH);
        if (!/^(W|E)\d+(N|S)\d+$/.test(target)) return call(C.ERR_INVALID_ARGS);
        if (!C.RESOURCES_ALL.includes(resource)) return call(C.ERR_INVALID_ARGS);
        if (!tm.store[resource] || tm.store[resource] < amount) return call(C.ERR_NOT_ENOUGH_RESOURCES);
        if (tm.cooldownTime > offset) return call(C.ERR_TIRED);
        const cost = fee(amount, roomRange(r.name, target));
        const energy = tm.store.energy ?? 0;
        if ((resource !== C.RESOURCE_ENERGY && energy < cost) || (resource === C.RESOURCE_ENERGY && energy < amount + cost)) {
          return call(C.ERR_NOT_ENOUGH_RESOURCES);
        }
        if (description && (typeof description !== 'string' || description.length > 100)) return call(C.ERR_INVALID_ARGS);
        state.intents.set(tm.id, { room: r.name, resource, amount, target });
        return call(C.OK);
      },
    };
    const controller = {
      id: r.controller.id, structureType: 'controller', my: true, owner: { username: 'bot' }, level: r.level,
      pos: pos(r.controller.x, r.controller.y), progress: 0, progressTotal: C.CONTROLLER_LEVELS[r.level],
      ticksToDowngrade: C.CONTROLLER_DOWNGRADE[r.level],
    };
    const owned = [controller, terminal];
    const spawns = [];
    if (r.spawnObj) {
      const sp = r.spawnObj;
      const spawn = {
        id: sp.id, name: sp.name, structureType: 'spawn', my: true, owner: { username: 'bot' }, pos: pos(sp.x, sp.y),
        store: makeStore({ energy: C.SPAWN_ENERGY_CAPACITY }, { resourceCapacity: { energy: C.SPAWN_ENERGY_CAPACITY } }),
        spawning: null,
        spawnCreep(...args) {
          state.spawnCalls.push({ offset, args });
          return C.ERR_BUSY;
        },
      };
      owned.push(spawn);
      spawns.push(spawn);
    }
    const creeps = [keeperObject(state, pos, r)];
    for (const c of creeps) allCreeps[c.name] = c;
    const lists = {
      [C.FIND_MINERALS]: [], [C.FIND_SOURCES]: [], [C.FIND_STRUCTURES]: owned, [C.FIND_MY_STRUCTURES]: owned,
      [C.FIND_MY_SPAWNS]: spawns, [C.FIND_CREEPS]: creeps, [C.FIND_MY_CREEPS]: creeps,
    };
    const capacity = r.spawnObj ? CAPACITY : 0;
    rooms[r.name] = {
      name: r.name, controller, energyAvailable: capacity, energyCapacityAvailable: capacity,
      find: (type, opts) => findFiltered(lists[type] ?? [], opts),
      getTerrain: () => ({ getRawBuffer: () => RAW.slice() }),
      createConstructionSite(...args) {
        state.siteCalls.push({ offset, args });
        return C.OK;
      },
    };
    for (const s of owned) structures[s.id] = s;
  }
  return { creeps: allCreeps, rooms, structures };
}

/** The global pass after loop `offset` (V437, V438): each recorded send, in the terminals' order. */
function advance(state, offset) {
  for (const r of state.rooms) {
    const intent = state.intents.get(r.terminal.id);
    if (!intent) continue;
    const tm = r.terminal;
    if (tm.cooldownTime > offset) {
      state.errors.push(`offset ${offset}: ${r.name}'s send dropped on its cooldown`);
      continue;
    }
    const to = state.rooms.find((x) => x.name === intent.target);
    if (!to || !allows(to.level)) {
      state.errors.push(`offset ${offset}: ${r.name}'s send to ${intent.target} dropped: no own terminal at its level`);
      continue;
    }
    if (!(tm.store[intent.resource] >= intent.amount)) {
      state.errors.push(`offset ${offset}: ${r.name} no longer holds its send`);
      continue;
    }
    const amount = Math.min(intent.amount, Math.max(0, C.TERMINAL_CAPACITY - used(to.terminal.store)));
    if (!(amount > 0)) {
      state.errors.push(`offset ${offset}: ${r.name}'s send to ${to.name} clipped to nothing`);
      continue;
    }
    const cost = fee(amount, roomRange(r.name, to.name));
    if ((tm.store.energy ?? 0) < cost) {
      state.errors.push(`offset ${offset}: ${r.name} lacks the fee in the global pass`);
      continue;
    }
    if (amount !== intent.amount) state.errors.push(`offset ${offset}: ${r.name}'s send clipped from ${intent.amount} to ${amount}`);
    to.terminal.store[intent.resource] = (to.terminal.store[intent.resource] ?? 0) + amount;
    tm.store[intent.resource] -= amount;
    if (!tm.store[intent.resource]) delete tm.store[intent.resource];
    tm.store.energy -= cost;
    tm.cooldownTime = offset + C.TERMINAL_COOLDOWN;
    state.sent.push({ offset, from: r.name, kind: intent.resource, to: to.name, amount, fee: cost });
  }
}

export function terminalSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  const world = simulate(dir, {
    ticks: RUN,
    setup: (context, w) => {
      w.segments[CONFIG_SEGMENT] ??= configRecord(CONFIG_LINES);
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      return offset === RUN - 1 ? SUMMARY : null;
    },
  });
  const problems = [...state.errors.slice(0, 10)];
  if (state.errors.length > 10) problems.push(`... ${state.errors.length - 10} more model errors`);
  const bad = world.logs.filter((l) => /rustedscreep fault|incident|loader error|\[error\]|trapped|was refused/.test(l));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts || world.globals !== 1) problems.push(`halts ${world.halts}, globals ${world.globals}`);
  const last = world.ticksRun - 1;

  // The balance reads: every member's cooldown on one loop or none, never the low room's.
  const readLoops = [];
  for (let o = 0; o <= last; o++) {
    const r = state.reads[o];
    const counts = MEMBERS.map((n) => r[n].cooldown);
    if (r[LOW].cooldown || (r[LOW].store && o > 2)) problems.push(`offset ${o}: the low room's terminal read ${JSON.stringify(r[LOW])}`);
    if (counts.some((c) => c > 1)) problems.push(`offset ${o}: a terminal's cooldown read twice ${JSON.stringify(r)}`);
    if (counts.some((c) => c) && !counts.every((c) => c === 1)) problems.push(`offset ${o}: not every member read ${JSON.stringify(r)}`);
    if (counts.every((c) => c === 1)) {
      readLoops.push(o);
      for (const n of MEMBERS) if (r[n].store !== 1) problems.push(`offset ${o}: ${n}'s store read ${r[n].store} times`);
    }
  }
  // The plans: the first loop after the boot loop, TERMINAL_COOLDOWN after each loop that sent, READ_TICKS after one
  // that did not.
  const sendLoops = [...new Set(state.sent.map((s) => s.offset))];
  const wantReads = [];
  for (let o = 2; o <= last; ) {
    wantReads.push(o);
    o += sendLoops.includes(o) ? C.TERMINAL_COOLDOWN : READ_TICKS;
  }
  if (JSON.stringify(readLoops) !== JSON.stringify(wantReads)) {
    problems.push(`balance reads at ${JSON.stringify(readLoops)}, expected ${JSON.stringify(wantReads)}`);
  }
  if (JSON.stringify(sendLoops) !== JSON.stringify(wantReads.slice(0, 2))) {
    problems.push(`sends on loops ${JSON.stringify(sendLoops)}, expected the first two read loops ${JSON.stringify(wantReads.slice(0, 2))}`);
  }

  // Every call: a send of three arguments, OK, on a read loop.
  for (const c of state.calls) {
    if (c.argCount !== 3 || !Number.isInteger(c.args[1]) || c.args[1] < SEND_MIN) problems.push(`offset ${c.offset}: send ${JSON.stringify(c.args)}`);
    if (c.result !== C.OK) problems.push(`offset ${c.offset}: ${c.room}'s send returned ${c.result}`);
    if (!readLoops.includes(c.offset)) problems.push(`offset ${c.offset}: a send on a loop the terminals were not read`);
  }
  // The sends: the four expected, each once, KEEP units at the engine's fee, two a loop.
  const got = state.sent.map((s) => `${s.from} ${s.kind} ${s.to} ${s.amount} ${s.fee}`).sort();
  const want = WANT_SENDS.map(([f, k, t]) => `${f} ${k} ${t} ${KEEP} ${FEE}`).sort();
  if (JSON.stringify(got) !== JSON.stringify(want)) problems.push(`sends ${JSON.stringify(got)}, expected ${JSON.stringify(want)}`);
  for (const o of sendLoops) {
    const n = state.sent.filter((s) => s.offset === o).length;
    if (n !== 2) problems.push(`offset ${o}: ${n} sends, expected 2 (one a giver)`);
  }
  for (const s of WANT_SENDS) {
    const n = world.logs.filter((l) => l === lineOf(s)).length;
    if (n !== 1) problems.push(`${n} lines "${lineOf(s)}", expected 1`);
  }

  // Nothing lost.
  const store = (n) => state.rooms.find((r) => r.name === n).terminal.store;
  for (const n of MEMBERS) {
    for (const k of [H, O]) {
      const giver = (k === H && n === GIVER_H) || (k === O && n === GIVER_O);
      const expect = giver ? STOCK - 2 * KEEP : KEEP;
      if ((store(n)[k] ?? 0) !== expect) problems.push(`${n} holds ${store(n)[k] ?? 0} ${k}, expected ${expect}`);
    }
    const expectEnergy = [GIVER_H, GIVER_O].includes(n) ? ENERGY - 2 * FEE : ENERGY;
    if (store(n).energy !== expectEnergy) problems.push(`${n} holds ${store(n).energy} energy, expected ${expectEnergy}`);
  }
  if (JSON.stringify(store(LOW)) !== JSON.stringify(ROOMS[3].store)) problems.push(`the low room's terminal changed: ${JSON.stringify(store(LOW))}`);
  if (state.spawnCalls.length) problems.push(`spawnCreep calls: ${JSON.stringify(state.spawnCalls.slice(0, 3))}`);
  if (state.siteCalls.length) problems.push(`createConstructionSite calls: ${JSON.stringify(state.siteCalls.slice(0, 3))}`);

  // The summary.
  const summary = commandResults(world, SUMMARY).at(-1) ?? '';
  const parts = [
    `rustedscreep terminals at tick ${T0 + last}: on (keep ${KEEP}, send min ${SEND_MIN}, energy keep ${ENERGY_KEEP}, max range ${MAX_RANGE}, read every ${READ_TICKS}) | members 3`,
    `| plans ${readLoops.length}, next at ${T0 + wantReads.at(-1) + READ_TICKS}`,
    `| ${GIVER_H}: read at`,
    `sends 2 (${2 * KEEP} units, fees ${2 * FEE}), received ${KEEP}, refused 0`,
    `| ${TAKER}: read at`,
    `sends 0 (0 units, fees 0), received ${2 * KEEP}, refused 0`,
  ];
  for (const p of parts) if (!summary.includes(p)) problems.push(`the summary lacks "${p}": ${summary}`);
  if (summary.includes(LOW)) problems.push(`the summary names the low room: ${summary}`);

  if (problems.length) throw new Error(`terminal-sim:\n  ${problems.slice(0, 25).join('\n  ')}${problems.length > 25 ? `\n  ... ${problems.length - 25} more` : ''}`);
  return { ticks: world.ticksRun, readLoops, sendLoops, sends: state.sent.length, summary };
}

if (isMain(import.meta.url)) {
  try {
    const r = terminalSim(process.argv[2]);
    console.log(`terminal-sim ok: ${r.ticks} ticks; balance reads at ${JSON.stringify(r.readLoops)}; ` +
      `${r.sends} sends on loops ${JSON.stringify(r.sendLoops)} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
