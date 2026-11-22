// The spawn path's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/spawn.rs over rustedscreep-core `spawning`). One owned room, W1N1, at RCL2 with two
// spawns, listed in Game.structures in the opposite of their id order, and a creep whose name the bot did not give.
// The harness plays the engine's part as far as the glue can see it (V171-V175): an accepted spawnCreep charges the
// room's energy at the end of the tick, the new creep is in Game.creeps (spawning) from the next tick, and the spawn
// is busy until the tick its creep leaves, CREEP_SPAWN_TIME per part after the call; once (DROP_AT) it drops an
// accepted order silently, as the processor can (V173). It checks that:
// - no spawnCreep is called before the console asks (`rustedscreep.spawn("W1N1")`), and none while the room holds
//   less than the body's cost: the request is the opening's planned worker body (P3.6, solved from the room's
//   targets at its capacity; this room has no source, so nothing to size a worker for: the smallest worker, one
//   WORK, CARRY and MOVE, which is also the glue's fallback with no plan, so this sim cannot tell the two apart;
//   the live check of TEST_PLAN §8 compares a console worker with the plan's body);
// - the first call goes to the spawn with the lower id, with exactly two arguments: the body as an array of part
//   strings (one slice across the boundary, V178) and the name `wW1N1-<base 36 of Game.time × 256>`;
// - a refused call (ERR_RCL_NOT_ENOUGH) holds that spawn for the next SpawnConfig::hold_ticks (20) ticks, so the
//   other spawn takes the request on the next tick; its OK ends the console request (one info line);
// - a second request waits while one spawn is held and the other busy (no spawn job and no clock reading on those
//   ticks), then while the free one lacks energy (the first order drained the room; the spawn job plans it every
//   tick), and on the tick the room is full again, the held spawn's last, the spawn that worked takes it, not the
//   lower-id one, still held (`spawner()` shows it held that tick and free the next);
// - after that OK, a tick with a ready spawn (on probation), a full room and no new command makes no call: the
//   accepted request is gone; a third request goes to the spawn on probation (the only ready one), the harness drops
//   that order, and on the next tick the request stands again (one warning) and is spawned;
// - the console's refusals (not a room name, an unknown role, a number), a request for a room with no own spawn that
//   no other room serves (W5N5 is not owned: it is not in Game.rooms, so SpawnConfig::help_range never applies; a
//   note, counted as `no spawn`, not `helped`), and cancels; `rustedscreep.spawner()` reports the room, the totals, the last
//   accepted call and failure and the own creeps by home and role;
// - ADR-018: the harness's spawnCreep writes `Memory.creeps[name]` on every OK, as the engine's runtime does (V181),
//   and the stored Memory string never holds one of them (the bot deletes each entry in the same tick, V180), while
//   an entry another script wrote (seeded in Memory) stays;
// - ADR-019: with the raw Memory set to a string that does not parse (from the console, the tick before a request: a
//   tick with a null Memory and no call resets nothing), the harness's spawnCreep throws as the engine's does on a
//   null Memory (V181), on the lower-id spawn with both ready; the bot logs the message, resets the raw string to
//   `{}` (RawMemory.set, kept by the end-of-tick save, V180), notifies once and, the spawn released, spawns from it on
//   the next tick; a later throw with a usable Memory (the other spawn's, while the first is busy) resets nothing and
//   holds the spawn; the two throws' fault lines are the only faults; no trap or halt;
// - P5.3: every summary's since-boot idle spawn-ticks with a request (energy, held, help) never fall, help stays 0
//   (one room), and a held spawn-tick is counted once a refusal holds a spawn while the room asks.
// The P2.4 opening reads the room every loop (its finds, the controller's getters, the creeps' stores and, while the
// room is not full, the spawns' free energy; engine-shaped here, V189, V196, V199) but stays inert: the room has no
// source, so its plan wants no worker and requests none, and the console's workers (bot worker names) gather nothing
// and make way, with no intent; no watchdog line. Its extension placement (ADR-020: RCL2 allows 5, the room has
// none) calls `room.createConstructionSite` from the second loop; the harness records it, returns OK and places
// nothing, so the bot gives up on each tile after APPEAR_TICKS (10) and picks the next, LOST_MAX (3) times, then
// makes no more calls for the global: three calls.
// npm run check runs it (row 18); standalone: node js/tools/spawn-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CARRY_CAPACITY, SPAWN_ENERGY_CAPACITY, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { commandResults, simulate } from './smoke.mjs';

/** @screeps/common 2.16.0: BODYPART_COST, CREEP_SPAWN_TIME, ERR_RCL_NOT_ENOUGH (V131, V170, V171). */
const COST = { work: 100, carry: 50, move: 50 };
const CREEP_SPAWN_TIME = 3;
const ERR_RCL_NOT_ENOUGH = -14;
/** SPAWN_MMO.hold_ticks (crates/rustedscreep-core/src/config.rs). */
const HOLD_TICKS = 20;
const ROOM = 'W1N1';
const OTHER = 'W5N5';
const T0 = 50000000;
/** RCL2: a spawn and five 50-energy extensions (CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY). */
const CAPACITY = 300 + 5 * 50;
/** The opening plan's worker body (economy::sizing::worker_plan, P3.6): with no source in the room, the smallest
 * worker (sizing::smallest_worker), 200 energy. */
const BODY = ['work', 'carry', 'move'];
/** CONTROLLER_LEVELS[2] and CONTROLLER_DOWNGRADE[2] (@screeps/common 2.16.0). */
const PROGRESS_TOTAL_2 = 45000;
const DOWNGRADE_2 = 10000;
/** economy::placement: APPEAR_TICKS and LOST_MAX (crates/rustedscreep-core/src/economy/placement.rs). */
const APPEAR_TICKS = 10;
const LOST_MAX = 3;
const BODY_COST = BODY.reduce((sum, p) => sum + COST[p], 0);
const SPAWN_TIME = BODY.length * CREEP_SPAWN_TIME;

const ORDER_AT = 2;
const FILL_AT = 4;
/** SpawnA (the lower id) refuses; it is held on FIRST + 1 ..= FIRST + HOLD_TICKS. */
const FIRST = FILL_AT + 1;
const A_READY = FIRST + HOLD_TICKS + 1;
const AGAIN_AT = FIRST + 3;
const BAD_ROOM_AT = AGAIN_AT + 2;
const BAD_ROLE_AT = BAD_ROOM_AT + 1;
const BAD_ARG_AT = BAD_ROLE_AT + 1;
/** SpawnB, accepted at FIRST + 1, is busy until its creep leaves SPAWN_TIME ticks after the call. That order drained
 * the room (FILL_AT gives one body's cost), so from here the second request waits for energy with SpawnB free. */
const B_FREE = FIRST + 1 + SPAWN_TIME;
/** SpawnA's last held tick: the room is full again (the harness refills it on the tick before), and SpawnB takes the
 * second request. */
const THIRD = A_READY - 1;
const REFILL_AT = THIRD - 1;
const HELD_SUMMARY_AT = THIRD;
const REFILL2_AT = THIRD;
const QUIET = THIRD + 1;
const SUMMARY_AT = QUIET;
const LATE_AT = QUIET + 1;
const DROP_AT = LATE_AT + 1;
const REARMED = DROP_AT + 1;
const SETUP_AT = REARMED;
const CANCEL_AT = SETUP_AT + 1;
const NO_SPAWN_AT = CANCEL_AT + 1;
const LAST_SUMMARY_AT = NO_SPAWN_AT + 1;
const CANCEL2_AT = LAST_SUMMARY_AT + 1;
const END_CANCEL_AT = CANCEL2_AT + 1;
/** The stored Memory after the end of this tick holds every entry the bot did not delete so far. */
const MEMORY_CHECK_AT = END_CANCEL_AT + 1;
/** SpawnB, accepted at THIRD, is free again here: it is busy from the quiet tick through the console's summary. */
const B_FREE2 = THIRD + SPAWN_TIME;
/** SpawnA, accepted at REARMED, is free again here, so both spawns are ready: the lower id takes the last request and
 * throws on the null Memory. The corrupt Memory comes the tick before the request (a tick with a null Memory and no
 * call), so the request is served on the tick after it, whatever the room holds. */
const A_FREE = REARMED + SPAWN_TIME;
const THROWN = A_FREE;
const REQUEST_AT = THROWN - 1;
const CORRUPT_AT = REQUEST_AT - 1;
/** Released by the reset, SpawnA (still the lower id) spawns; it is busy from the next tick. */
const RESPAWNED = THROWN + 1;
const FINAL_SUMMARY_AT = RESPAWNED;
/** While SpawnA is busy, SpawnB takes the next request and throws with a usable Memory. */
const REQUEST2_AT = RESPAWNED + 1;
const OTHER_THROW = REQUEST2_AT + 1;
const HELD_AGAIN_AT = OTHER_THROW + 1;
const TICKS = HELD_AGAIN_AT + 1;
/** Memory at the start: another script's creep entry, which the bot must leave alone (ADR-018). */
const SEEDED = { creeps: { Probe1: { keep: 1 } } };

const ORDER_CMD = `rustedscreep.spawn("${ROOM}")`;
const ROLE_CMD = `rustedscreep.spawn("${ROOM}", "worker")`;
const NO_SPAWN_CMD = `rustedscreep.spawn("${OTHER}")`;
const BAD_ROOM_CMD = 'rustedscreep.spawn("not a room")';
const BAD_ROLE_CMD = `rustedscreep.spawn("${ROOM}", "nobody")`;
const BAD_ARG_CMD = 'rustedscreep.spawn(5)';
const CANCEL_CMD = 'rustedscreep.spawn()';
const SUMMARY_CMD = 'rustedscreep.spawner()';
const BAD_MEMORY = '{"oops":';
const CORRUPT_CMD = `RawMemory.set('${BAD_MEMORY}')`;

const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const id = (n) => `5bbcaa9099f9b0a0f7f4d${n.toString(16).padStart(3, '0')}`;
/** The name the bot gives the order of loop `offset`, the first of its tick: role, home, base 36 of time × 256. */
const nameAt = (offset) => `w${ROOM}-${(BigInt(T0 + offset) << 8n).toString(36)}`;

function makeWorld() {
  const state = {
    // Less than the body's cost: no call until FILL_AT.
    energy: BODY.reduce((sum, p) => sum + COST[p], 0) - 50,
    calls: [],
    /** Return values of the spawnCreep calls, in call order (OK once they run out). */
    returns: [ERR_RCL_NOT_ENOUGH],
    creeps: [{ id: id(1), name: 'Probe1', x: 10, y: 10, spawning: false, fatigue: 0, carry: 1 }],
    accepted: [],
    nextId: 100,
    errors: [],
    /** [offset, name] of every `Memory.creeps` entry the harness's spawnCreep wrote. */
    entries: [],
    /** The sandbox global, for the tick's `Memory` (set by simulate's `setup`). */
    context: null,
    /** [offset, ...arguments] of every `room.createConstructionSite` call (ADR-020). */
    sites: [],
  };
  const spawn = (n, name, x) => ({ id: id(n), name, structureType: 'spawn', my: true, x, busyFrom: -1, busyUntil: -1 });
  // In Game.structures order: B (the higher id) first.
  state.spawns = [spawn(20, 'SpawnB', 30), spawn(10, 'SpawnA', 20)];
  return state;
}

function sceneFor(state, offset) {
  // Every creep carries nothing (store, V196); its capacity is CARRY_CAPACITY per CARRY part of its body.
  const creeps = state.creeps.map((c) => ({
    id: c.id, name: c.name, my: true, spawning: c.spawning, fatigue: c.fatigue, pos: pos(c.x, c.y),
    owner: { username: 'bot' }, store: makeStore({}, { capacity: CARRY_CAPACITY * c.carry }),
  }));
  // The getters of an owned controller (V189): no progress yet, the level's total, a full downgrade timer.
  const controller = {
    id: id(2), structureType: 'controller', my: true, level: 2, pos: pos(40, 40),
    progress: 0, progressTotal: PROGRESS_TOTAL_2, ticksToDowngrade: DOWNGRADE_2,
  };
  // The room's energy as the spawns hold it: SpawnA (the lower id) first, up to SPAWN_ENERGY_CAPACITY each, the rest
  // in extensions the harness does not list.
  const held = (s) => Math.min(SPAWN_ENERGY_CAPACITY, Math.max(0, state.energy - (s.name === 'SpawnA' ? 0 : SPAWN_ENERGY_CAPACITY)));
  const spawns = state.spawns.map((s) => {
    const busy = offset >= s.busyFrom && offset < s.busyUntil;
    return {
      id: s.id, name: s.name, structureType: 'spawn', my: true, pos: pos(s.x, 25),
      store: makeStore({ energy: held(s) }, { resourceCapacity: { energy: SPAWN_ENERGY_CAPACITY } }),
      // Null when idle, a Spawning object while busy (V175).
      spawning: busy ? { name: s.creep, needTime: SPAWN_TIME, remainingTime: s.busyUntil - offset } : null,
      spawnCreep(...args) {
        const [body, name] = args;
        state.calls.push({ offset, spawn: this.name, args: args.length, isArray: Array.isArray(body), body: Array.isArray(body) ? [...body] : body, name });
        // A failure that has nothing to do with Memory (ADR-019 must not reset a usable Memory for it).
        if (offset === OTHER_THROW) throw new RangeError('scripted failure');
        const r = state.returns.shift() ?? 0;
        if (r === 0) {
          // The runtime's success path (structures.js L1122-1130, V181): it reads Memory.creeps, which throws on a
          // null Memory before anything is recorded, and writes the creep's entry.
          const mem = state.context.Memory;
          if (mem === null) throw new TypeError("Cannot read properties of null (reading 'creeps')");
          if (mem.creeps === undefined) mem.creeps = {};
          if (mem.creeps && typeof mem.creeps === 'object') {
            mem.creeps[name] = mem.creeps[name] || {};
            state.entries.push([offset, name]);
          }
        }
        // At DROP_AT the processor drops the accepted order: no charge, no creep, the spawn stays free (V173).
        if (r === 0 && offset !== DROP_AT) state.accepted.push({ offset, spawn: s, body: [...body], name });
        return r;
      },
    };
  });
  const room = {
    name: ROOM, controller, energyAvailable: state.energy, energyCapacityAvailable: CAPACITY,
    find: (type, opts) => findFiltered([], opts),
    getTerrain: () => ({ getRawBuffer: () => new Uint8Array(2500) }),
    createConstructionSite(...args) {
      state.sites.push([offset, ...args]);
      return 0;
    },
  };
  return {
    creeps: Object.fromEntries(creeps.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries([controller, ...spawns].map((s) => [s.id, s])),
  };
}

/** The engine's part after loop `offset`: charge accepted calls, add their creeps, and let creeps leave spawns. */
function advance(state, offset) {
  for (const a of state.accepted.filter((x) => x.offset === offset)) {
    const cost = a.body.reduce((sum, p) => sum + COST[p], 0);
    if (state.energy < cost) state.errors.push(`offset ${offset}: the harness charged ${cost} with ${state.energy}`);
    state.energy -= cost;
    a.spawn.busyFrom = offset + 1;
    a.spawn.busyUntil = offset + SPAWN_TIME;
    a.spawn.creep = a.name;
    const carry = a.body.filter((p) => p === 'carry').length;
    state.creeps.push({ id: id(state.nextId++), name: a.name, x: a.spawn.x, y: 25, spawning: true, fatigue: 0, carry, leaves: offset + SPAWN_TIME });
  }
  for (const c of state.creeps) {
    if (c.spawning && c.leaves === offset + 1) {
      c.spawning = false;
      c.y = 26;
    }
  }
  // The first fill is one body's cost, so the first order drains the room and the second request waits for the
  // refill (REFILL_AT) once SpawnB is free. The others make the room full for the quiet tick and the third request
  // (REFILL2_AT) and for each throw (CORRUPT_AT, REQUEST2_AT).
  if (offset === FILL_AT) state.energy = BODY_COST;
  if ([REFILL_AT, REFILL2_AT, CORRUPT_AT, REQUEST2_AT].includes(offset)) state.energy = CAPACITY;
}

const COMMANDS = {
  [ORDER_AT]: ORDER_CMD, [AGAIN_AT]: ROLE_CMD, [BAD_ROOM_AT]: BAD_ROOM_CMD, [BAD_ROLE_AT]: BAD_ROLE_CMD,
  [BAD_ARG_AT]: BAD_ARG_CMD, [HELD_SUMMARY_AT]: SUMMARY_CMD, [SUMMARY_AT]: SUMMARY_CMD, [LATE_AT]: ORDER_CMD,
  [SETUP_AT]: ORDER_CMD, [CANCEL_AT]: CANCEL_CMD, [NO_SPAWN_AT]: NO_SPAWN_CMD, [LAST_SUMMARY_AT]: SUMMARY_CMD,
  [CANCEL2_AT]: CANCEL_CMD, [END_CANCEL_AT]: CANCEL_CMD, [REQUEST_AT]: ORDER_CMD, [CORRUPT_AT]: CORRUPT_CMD,
  [FINAL_SUMMARY_AT]: SUMMARY_CMD, [REQUEST2_AT]: ORDER_CMD, [HELD_AGAIN_AT]: SUMMARY_CMD,
};

export function spawnSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let memoryAtCheck = null;
  const world = simulate(dir, {
    ticks: TICKS,
    memoryJson: JSON.stringify(SEEDED),
    setup: (context) => {
      state.context = context;
    },
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      // What the end of tick MEMORY_CHECK_AT - 1 stored (this tick's save comes after the command).
      if (offset === MEMORY_CHECK_AT) memoryAtCheck = w.memoryJson;
      return COMMANDS[offset] ?? null;
    },
  });
  const problems = [...state.errors];
  const thrownFaults = [THROWN, OTHER_THROW].map((o) => `rustedscreep fault at tick ${T0 + o}: job spawn act unit `);
  const expectedFault = (l) => thrownFaults.some((p) => l.startsWith(p));
  const warn = world.logs.filter((l) => (l.includes('fault') && !expectedFault(l)) || l.includes('[error]') || l.includes('trapped'));
  problems.push(...warn.map((l) => `log: ${l}`));
  const faults = world.logs.filter(expectedFault);
  if (faults.length !== 2 || !faults.every((f) => f.endsWith(': the spawnCreep call threw'))) problems.push(`spawn act faults ${JSON.stringify(faults)}, expected two "the spawnCreep call threw"`);
  if (world.halts) problems.push(`${world.halts} halt(s)`);
  if (Object.keys(COMMANDS).length !== 19) problems.push('two commands share a tick');
  if (!(AGAIN_AT + 1 < B_FREE && B_FREE <= REFILL_AT)) problems.push('the scenario lost its timing: the second request must wait on a held and a busy spawn, then on energy with SpawnB free');
  if (!(A_READY === QUIET && THIRD + 1 === QUIET)) problems.push('the scenario lost its timing: SpawnA must become ready on the quiet tick');
  if (!(LAST_SUMMARY_AT < B_FREE2)) problems.push('the scenario lost its timing: SpawnB must stay busy from the quiet tick through the console\'s summary');
  if (!(END_CANCEL_AT < MEMORY_CHECK_AT && MEMORY_CHECK_AT <= CORRUPT_AT && Math.max(A_FREE, B_FREE2) <= THROWN)) problems.push('the scenario lost its timing: both spawns must be ready for the Memory phase, after the stored-Memory check');
  if (!(RESPAWNED < REQUEST2_AT && B_FREE2 <= OTHER_THROW && HELD_AGAIN_AT < RESPAWNED + SPAWN_TIME)) problems.push('the scenario lost its timing: SpawnB must take the last request while SpawnA is busy');

  // The calls: the lower id refused; the other spawn the next tick, and again once it is free and the room full (the
  // lower id still held); nothing on the quiet tick; the third request to the lower id (on probation), dropped by
  // the harness, and again on the next tick; with both spawns ready, the lower id's throw on the null Memory and, the
  // reset having released it, its spawn on the next tick (a hold would give that call to SpawnB); SpawnB's throw with
  // a usable Memory while SpawnA is busy.
  const want = [
    { offset: FIRST, spawn: 'SpawnA', name: nameAt(FIRST) },
    { offset: FIRST + 1, spawn: 'SpawnB', name: nameAt(FIRST + 1) },
    { offset: THIRD, spawn: 'SpawnB', name: nameAt(THIRD) },
    { offset: DROP_AT, spawn: 'SpawnA', name: nameAt(DROP_AT) },
    { offset: REARMED, spawn: 'SpawnA', name: nameAt(REARMED) },
    { offset: THROWN, spawn: 'SpawnA', name: nameAt(THROWN) },
    { offset: RESPAWNED, spawn: 'SpawnA', name: nameAt(RESPAWNED) },
    { offset: OTHER_THROW, spawn: 'SpawnB', name: nameAt(OTHER_THROW) },
  ];
  const got = state.calls.map((c) => ({ offset: c.offset, spawn: c.spawn, name: c.name }));
  if (JSON.stringify(got) !== JSON.stringify(want)) problems.push(`spawnCreep calls ${JSON.stringify(got)}, expected ${JSON.stringify(want)}`);
  for (const c of state.calls) {
    if (c.args !== 2) problems.push(`offset ${c.offset}: spawnCreep with ${c.args} arguments, expected 2`);
    if (!c.isArray || JSON.stringify(c.body) !== JSON.stringify(BODY)) problems.push(`offset ${c.offset}: body ${JSON.stringify(c.body)}, expected ${JSON.stringify(BODY)}`);
  }
  // No request can be served while SpawnA is held and SpawnB busy, nor on the quiet tick (no request), nor once both
  // are busy again, nor on the tick with a null Memory (no request): no spawn job runs, so the plain build reads the clock in its loop only for the economy's seven
  // jobs, one unit each for W1N1 with a sample before and after (14: the P3.1 static miners' Critical job, the P3.3
  // haulers' Critical job, the P2.4 opening's Critical and Standard jobs, the Optional site placement of ADR-020, and
  // the P6.2 remote manager's Critical job and Optional placement; was 10 before P6.2, 8 before P3.3, 6 before P3.1
  // and 0 before P2.4; the haulers' Standard job has a unit only for a room with a hauler to store, the remote
  // creeps' only with a remote creep; the constants self-check is done by then; movement's next rescan is 50 ticks
  // after the first).
  const OPENING_SAMPLES = 14;
  const idle = [];
  for (let o = AGAIN_AT + 1; o < B_FREE; o++) idle.push(o);
  // The tick with a null Memory has no request either.
  idle.push(QUIET, REARMED + 1, REQUEST_AT);
  for (const o of idle) {
    const used = world.loopCalls[o]?.getUsed;
    if (used !== OPENING_SAMPLES) problems.push(`offset ${o}: ${used} getUsed calls with no request to serve, expected ${OPENING_SAMPLES}`);
  }
  // While the second request waits for energy with SpawnB free, the room is planned every tick: the spawn job runs.
  for (let o = B_FREE; o <= REFILL_AT; o++) {
    const used = world.loopCalls[o]?.getUsed;
    if (!(used > OPENING_SAMPLES)) problems.push(`offset ${o}: ${used} getUsed calls while the request waits for energy, expected more than ${OPENING_SAMPLES}`);
  }
  const opening = world.logs.filter((l) => l.startsWith('rustedscreep opening:'));
  if (opening.length) problems.push(`opening lines (it should stay inert): ${opening.join(' / ')}`);
  // The extension placement: a call on the first loop the Optional job runs after the constants self-check (its
  // last loop, or the next when the check used up that loop's Optional cap: wall time), then one APPEAR_TICKS (10)
  // loops later until LOST_MAX (3) were never seen, and none after (was one every 10 loops to the end before
  // LOST_MAX), each with x, y and "extension", on distinct tiles inside the margin of 3 and at range 2 or more from
  // both spawns.
  const constsAt = Number(/^rustedscreep consts: checked at tick (\d+) /.exec(world.logs.find((l) => l.startsWith('rustedscreep consts: checked')) ?? '')?.[1]) - T0;
  const firstSite = state.sites[0]?.[0];
  const wantSites = firstSite >= constsAt && firstSite <= constsAt + 1
    ? Array.from({ length: LOST_MAX }, (_, k) => firstSite + APPEAR_TICKS * k) : ['an offset near', constsAt];
  if (JSON.stringify(state.sites.map(([o]) => o)) !== JSON.stringify(wantSites)) problems.push(`createConstructionSite at offsets ${JSON.stringify(state.sites.map(([o]) => o))}, expected ${JSON.stringify(wantSites)}`);
  const tiles = new Set();
  for (const [o, x, y, type, ...rest] of state.sites) {
    const nearSpawn = state.spawns.some((sp) => Math.max(Math.abs(sp.x - x), Math.abs(25 - y)) <= 1);
    if (rest.length || type !== 'extension' || x < 3 || y < 3 || x > 46 || y > 46 || nearSpawn || tiles.has(`${x},${y}`)) {
      problems.push(`offset ${o}: createConstructionSite(${[x, y, type, ...rest].join(', ')}) is not a legal new extension tile`);
    }
    tiles.add(`${x},${y}`);
  }

  // Console replies.
  const reply = (cmd) => commandResults(world, cmd);
  const accepted = reply(ORDER_CMD).concat(reply(ROLE_CMD));
  const acceptedPrefix = `rustedscreep spawn: from the next tick, a spawn of ${ROOM} spawns one worker`;
  if (accepted.length !== 6 || !accepted.every((r) => r.startsWith(acceptedPrefix) && !r.includes('no own spawn'))) {
    problems.push(`spawn replies: ${JSON.stringify(accepted)}`);
  }
  const expectReply = (cmd, part) => {
    const r = reply(cmd);
    if (r.length !== 1 || !r[0].includes(part)) problems.push(`${cmd} replied ${JSON.stringify(r)}, expected "${part}"`);
  };
  expectReply(NO_SPAWN_CMD, `a spawn of ${OTHER} spawns one worker`);
  expectReply(NO_SPAWN_CMD, '(no own spawn there on the last tick: nothing spawns until one exists)');
  expectReply(BAD_ROOM_CMD, 'not a room name');
  expectReply(BAD_ROLE_CMD, 'unknown role; roles: worker');
  expectReply(BAD_ARG_CMD, 'give a room name and optionally a role');
  const cancels = reply(CANCEL_CMD);
  const wantCancels = ['cancelled', 'cancelled', 'none'].map((k) => (k === 'none'
    ? 'rustedscreep spawn: no console request to cancel' : 'rustedscreep spawn: console request cancelled'));
  if (JSON.stringify(cancels) !== JSON.stringify(wantCancels)) problems.push(`cancel replies: ${JSON.stringify(cancels)}`);

  // Log lines: one per accepted call, and one warning when the dropped order's creep did not appear.
  const lines = world.logs.filter((l) => l.startsWith('rustedscreep spawn: w') && l.includes(' accepted ('));
  const wantLines = [FIRST + 1, THIRD, DROP_AT, REARMED, RESPAWNED].map((o) => `rustedscreep spawn: ${nameAt(o)} accepted (${BODY.length} parts, ${BODY_COST} energy, ${SPAWN_TIME} ticks), the console's request`);
  if (JSON.stringify(lines) !== JSON.stringify(wantLines)) problems.push(`accepted log lines ${JSON.stringify(lines)}, expected ${JSON.stringify(wantLines)}`);
  const rearm = world.logs.filter((l) => l.includes('was accepted but is not in Game.creeps'));
  const wantRearm = `rustedscreep spawn: ${nameAt(DROP_AT)} was accepted but is not in Game.creeps; the console's request stands again`;
  if (rearm.length !== 1 || !rearm[0].endsWith(wantRearm)) problems.push(`re-arm warnings ${JSON.stringify(rearm)}, expected one "${wantRearm}"`);

  // Summaries: SpawnA still held on THIRD; then the quiet tick (SpawnB busy, SpawnA free); then with a request for a
  // room without a spawn and both spawns busy; then on the respawn after the reset; then SpawnB held after its throw
  // while SpawnA is busy.
  const summaries = reply(SUMMARY_CMD);
  const event = (o, outcome) => `${nameAt(o)} at tick ${T0 + o} (${BODY.length} parts, ${BODY_COST} energy): ${outcome}`;
  const wantParts = [
    [
      `rustedscreep spawner at tick ${T0 + THIRD}: ${ROOM} energy ${CAPACITY}/${CAPACITY}, spawns idle 1 busy 0 held 1`,
      '| requests 1, orders 1 (ok 1, failed 0), waiting 0, unfit 0, no spawn 0, helped 0, held 1 |',
    ],
    [
      `rustedscreep spawner at tick ${T0 + QUIET}: ${ROOM} energy ${CAPACITY}/${CAPACITY}, spawns idle 1 busy 1 held 0`,
      '| requests 0, orders 0 (ok 0, failed 0), waiting 0, unfit 0, no spawn 0, helped 0, held 0 |',
      `since boot: ticks ${QUIET}, orders 3, ok 2, failed 1`,
      `| last accepted ${event(THIRD, 'ok')}`,
      `| last failure ${event(FIRST, 'rcl not enough')}`,
      '| console request: none',
      `| own creeps: ${ROOM} worker 2, other names 1`,
    ],
    [
      `rustedscreep spawner at tick ${T0 + LAST_SUMMARY_AT}: ${ROOM} energy ${CAPACITY - BODY_COST}/${CAPACITY}, spawns idle 0 busy 2 held 0`,
      '| requests 1, orders 0 (ok 0, failed 0), waiting 0, unfit 0, no spawn 1, helped 0, held 0 |',
      `since boot: ticks ${LAST_SUMMARY_AT}, orders 5, ok 4, failed 1`,
      `| last accepted ${event(REARMED, 'ok')}`,
      `| console request: ${OTHER} worker`,
      `| own creeps: ${ROOM} worker 3, other names 1`,
    ],
  ];
  wantParts.push([
    `rustedscreep spawner at tick ${T0 + RESPAWNED}:`,
    '| requests 1, orders 1 (ok 1, failed 0)',
    `since boot: ticks ${RESPAWNED}, orders 7, ok 5, failed 2`,
    `| last accepted ${event(RESPAWNED, 'ok')}`,
    `| last failure ${event(THROWN, 'threw')}`,
    '| console request: none | Memory resets 1 |',
    // The tick's own creep is in Game.creeps from the next tick.
    `| own creeps: ${ROOM} worker 3, other names 1`,
  ]);
  wantParts.push([
    `rustedscreep spawner at tick ${T0 + HELD_AGAIN_AT}: ${ROOM} energy ${CAPACITY}/${CAPACITY}, spawns idle 0 busy 1 held 1`,
    '| requests 1, orders 0 (ok 0, failed 0), waiting 0, unfit 0, no spawn 0, helped 0, held 1 |',
    `since boot: ticks ${HELD_AGAIN_AT}, orders 8, ok 5, failed 3`,
    `| last failure ${event(OTHER_THROW, 'threw')}`,
    `| console request: ${ROOM} worker | Memory resets 1 |`,
  ]);
  if (summaries.length !== wantParts.length) problems.push(`${summaries.length} spawner summaries, expected ${wantParts.length}`);
  else {
    wantParts.forEach((parts, i) => {
      for (const p of parts) if (!summaries[i].includes(p)) problems.push(`spawner summary ${i + 1} lacks "${p}": ${summaries[i]}`);
    });
    // P5.3: the since-boot idle spawn-ticks by cause, never falling; one room, so none gated off helping, and the
    // held spawn counts from its refusal on.
    let last = [0, 0, 0];
    summaries.forEach((r, i) => {
      const m = /, idle spawn-ticks with a request: energy (\d+), held (\d+), help (\d+) \|/.exec(r);
      if (!m) {
        problems.push(`spawner summary ${i + 1} has no idle spawn-ticks: ${r}`);
        return;
      }
      const now = m.slice(1).map(Number);
      if (now.some((v, k) => v < last[k]) || now[2] !== 0) problems.push(`spawner summary ${i + 1}: idle spawn-ticks ${now} after ${last}`);
      last = now;
    });
    if (last[1] === 0) problems.push('no held spawn-tick counted, though a refusal held a spawn while its room asked');
  }

  // ADR-018: the harness wrote an entry on every OK with a usable Memory (the dropped order included: the runtime
  // writes it before the processor runs); none reached a stored string.
  const wantEntries = [FIRST + 1, THIRD, DROP_AT, REARMED, RESPAWNED];
  if (JSON.stringify(state.entries.map(([o]) => o)) !== JSON.stringify(wantEntries)) problems.push(`Memory.creeps entries written at ${JSON.stringify(state.entries.map(([o]) => o))}, expected ${JSON.stringify(wantEntries)}`);
  // Before the reset only the seeded entry is stored; after it (which dropped the seed with the rest) nothing.
  for (const [label, text, want] of [['before the corruption', memoryAtCheck, SEEDED.creeps], ['at the end', world.memoryJson, {}]]) {
    let mem = null;
    try {
      mem = JSON.parse(text);
    } catch {
      problems.push(`stored Memory ${label} does not parse: ${text}`);
      continue;
    }
    if (JSON.stringify(mem?.creeps) !== JSON.stringify(want)) problems.push(`stored Memory.creeps ${label}: ${JSON.stringify(mem?.creeps)}, expected ${JSON.stringify(want)}`);
  }
  // ADR-019: the corrupt string from the console, then the bot's reset, both stored; one notification; the warnings.
  const sets = JSON.stringify(world.rawSets ?? []);
  if (sets !== JSON.stringify([[CORRUPT_AT, BAD_MEMORY], [THROWN, '{}']])) problems.push(`RawMemory.set calls ${sets}`);
  const notices = world.notifies.filter((n) => n.includes('Memory was null'));
  if (notices.length !== 1 || !notices[0].includes('reset to {}')) problems.push(`Memory notifications ${JSON.stringify(notices)}, expected one`);
  const threw = world.logs.filter((l) => l.includes(`rustedscreep spawn: spawnCreep for ${nameAt(THROWN)} threw: Cannot read properties of null (reading 'creeps')`));
  const otherThrew = world.logs.filter((l) => l.includes(`rustedscreep spawn: spawnCreep for ${nameAt(OTHER_THROW)} threw: scripted failure`));
  if (otherThrew.length !== 1) problems.push(`${otherThrew.length} warnings for the throw with a usable Memory, expected 1`);
  const resetLines = world.logs.filter((l) => l.includes('rustedscreep spawn: Memory is null (its raw string did not parse, or was JSON null), so spawnCreep threw; the raw Memory is reset to {} (ADR-019, reset 1 of this global)'));
  if (threw.length !== 1 || resetLines.length !== 1) problems.push(`throw and reset warnings: ${threw.length} and ${resetLines.length}, expected 1 and 1`);

  if (problems.length) throw new Error(`spawn-sim:\n  ${problems.slice(0, 20).join('\n  ')}`);
  return { calls: state.calls.length, summary: summaries[1] };
}

if (isMain(import.meta.url)) {
  try {
    const r = spawnSim(process.argv[2]);
    console.log(`spawn-sim ok: ${r.calls} spawnCreep calls | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
