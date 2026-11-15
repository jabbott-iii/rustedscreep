// The spawn path's glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/spawn.rs over rustedscreep-core `spawning`). One owned room, W1N1, at RCL2 with two
// spawns, listed in Game.structures in the opposite of their id order, and a creep whose name the bot did not give.
// The harness plays the engine's part as far as the glue can see it (V171-V175): an accepted spawnCreep charges the
// room's energy at the end of the tick, the new creep is in Game.creeps (spawning) from the next tick, and the spawn
// is busy until the tick its creep leaves, CREEP_SPAWN_TIME per part after the call; once (DROP_AT) it drops an
// accepted order silently, as the processor can (V173). It checks that:
// - no spawnCreep is called before the console asks (`rustedscreep.spawn("W1N1")`), and none while the room holds
//   less than the body's cost: the request is sized to the room's capacity (550: two WORK, CARRY, MOVE repeats);
// - the first call goes to the spawn with the lower id, with exactly two arguments: the body as an array of part
//   strings (one slice across the boundary, V178) and the name `wW1N1-<base 36 of Game.time × 256>`;
// - a refused call (ERR_RCL_NOT_ENOUGH) holds that spawn for the next SpawnConfig::hold_ticks (20) ticks, so the
//   other spawn takes the request on the next tick; its OK ends the console request (one info line);
// - a second request waits while one spawn is held and the other busy (no spawn job and no clock reading on those
//   ticks), then while the free one lacks energy, and on the tick the room is full again the spawn that worked takes
//   it, not the lower-id one, still held (`spawner()` shows it held that tick and free the next);
// - after that OK, a tick with a ready spawn (on probation), a full room and no new command makes no call: the
//   accepted request is gone; a third request goes to the spawn on probation (the only ready one), the harness drops
//   that order, and on the next tick the request stands again (one warning) and is spawned;
// - the console's refusals (not a room name, an unknown role, a number), a request for a room with no own spawn (a
//   note, counted as `no spawn`), and cancels; `rustedscreep.spawner()` reports the room, the totals, the last
//   accepted call and failure and the own creeps by home and role; no fault, trap or halt.
// npm run check runs it (row 18); standalone: node js/tools/spawn-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { packPos } from './lib/world.mjs';
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
/** The worker body at that capacity: WORKER_PATTERN repeated in place, two times (400 energy). */
const BODY = ['work', 'work', 'carry', 'carry', 'move', 'move'];
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
/** SpawnB, accepted at FIRST + 1, is busy until its creep leaves SPAWN_TIME ticks after the call. */
const B_FREE = FIRST + 1 + SPAWN_TIME;
const REFILL_AT = B_FREE;
const THIRD = REFILL_AT + 1;
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
const TICKS = END_CANCEL_AT + 1;

const ORDER_CMD = `rustedscreep.spawn("${ROOM}")`;
const ROLE_CMD = `rustedscreep.spawn("${ROOM}", "worker")`;
const NO_SPAWN_CMD = `rustedscreep.spawn("${OTHER}")`;
const BAD_ROOM_CMD = 'rustedscreep.spawn("not a room")';
const BAD_ROLE_CMD = `rustedscreep.spawn("${ROOM}", "hauler")`;
const BAD_ARG_CMD = 'rustedscreep.spawn(5)';
const CANCEL_CMD = 'rustedscreep.spawn()';
const SUMMARY_CMD = 'rustedscreep.spawner()';

const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const id = (n) => `5bbcaa9099f9b0a0f7f4d${n.toString(16).padStart(3, '0')}`;
/** The name the bot gives the order of loop `offset`, the first of its tick: role, home, base 36 of time × 256. */
const nameAt = (offset) => `w${ROOM}-${(BigInt(T0 + offset) << 8n).toString(36)}`;

function makeWorld() {
  const state = {
    energy: 250,
    calls: [],
    /** Return values of the spawnCreep calls, in call order (OK once they run out). */
    returns: [ERR_RCL_NOT_ENOUGH],
    creeps: [{ id: id(1), name: 'Probe1', x: 10, y: 10, spawning: false, fatigue: 0 }],
    accepted: [],
    nextId: 100,
    errors: [],
  };
  const spawn = (n, name, x) => ({ id: id(n), name, structureType: 'spawn', my: true, x, busyFrom: -1, busyUntil: -1 });
  // In Game.structures order: B (the higher id) first.
  state.spawns = [spawn(20, 'SpawnB', 30), spawn(10, 'SpawnA', 20)];
  return state;
}

function sceneFor(state, offset) {
  const creeps = state.creeps.map((c) => ({
    id: c.id, name: c.name, my: true, spawning: c.spawning, fatigue: c.fatigue, pos: pos(c.x, c.y),
    owner: { username: 'bot' },
  }));
  const controller = { id: id(2), structureType: 'controller', my: true, level: 2, pos: pos(40, 40) };
  const spawns = state.spawns.map((s) => {
    const busy = offset >= s.busyFrom && offset < s.busyUntil;
    return {
      id: s.id, name: s.name, structureType: 'spawn', my: true, pos: pos(s.x, 25),
      // Null when idle, a Spawning object while busy (V175).
      spawning: busy ? { name: s.creep, needTime: SPAWN_TIME, remainingTime: s.busyUntil - offset } : null,
      spawnCreep(...args) {
        const [body, name] = args;
        state.calls.push({ offset, spawn: this.name, args: args.length, isArray: Array.isArray(body), body: Array.isArray(body) ? [...body] : body, name });
        const r = state.returns.shift() ?? 0;
        // At DROP_AT the processor drops the accepted order: no charge, no creep, the spawn stays free (V173).
        if (r === 0 && offset !== DROP_AT) state.accepted.push({ offset, spawn: s, body: [...body], name });
        return r;
      },
    };
  });
  const room = {
    name: ROOM, controller, energyAvailable: state.energy, energyCapacityAvailable: CAPACITY,
    find: () => [],
    getTerrain: () => ({ getRawBuffer: () => new Uint8Array(2500) }),
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
    state.creeps.push({ id: id(state.nextId++), name: a.name, x: a.spawn.x, y: 25, spawning: true, fatigue: 0, leaves: offset + SPAWN_TIME });
  }
  for (const c of state.creeps) {
    if (c.spawning && c.leaves === offset + 1) {
      c.spawning = false;
      c.y = 26;
    }
  }
  if (offset === FILL_AT || offset === REFILL_AT || offset === REFILL2_AT) state.energy = CAPACITY;
}

const COMMANDS = {
  [ORDER_AT]: ORDER_CMD, [AGAIN_AT]: ROLE_CMD, [BAD_ROOM_AT]: BAD_ROOM_CMD, [BAD_ROLE_AT]: BAD_ROLE_CMD,
  [BAD_ARG_AT]: BAD_ARG_CMD, [HELD_SUMMARY_AT]: SUMMARY_CMD, [SUMMARY_AT]: SUMMARY_CMD, [LATE_AT]: ORDER_CMD,
  [SETUP_AT]: ORDER_CMD, [CANCEL_AT]: CANCEL_CMD, [NO_SPAWN_AT]: NO_SPAWN_CMD, [LAST_SUMMARY_AT]: SUMMARY_CMD,
  [CANCEL2_AT]: CANCEL_CMD, [END_CANCEL_AT]: CANCEL_CMD,
};

export function spawnSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  const world = simulate(dir, {
    ticks: TICKS,
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      advance(state, offset);
      return COMMANDS[offset] ?? null;
    },
  });
  const problems = [...state.errors];
  const warn = world.logs.filter((l) => l.includes('fault') || l.includes('[error]') || l.includes('trapped'));
  problems.push(...warn.map((l) => `log: ${l}`));
  if (world.halts) problems.push(`${world.halts} halt(s)`);
  if (Object.keys(COMMANDS).length !== 14) problems.push('two commands share a tick');
  if (!(A_READY === QUIET && B_FREE < THIRD)) problems.push('the scenario lost its timing: SpawnA must become ready on the quiet tick');

  // The calls: the lower id refused; the other spawn the next tick, and again once it is free and the room full (the
  // lower id still held); nothing on the quiet tick; the third request to the lower id (on probation), dropped by
  // the harness, and again on the next tick.
  const want = [
    { offset: FIRST, spawn: 'SpawnA', name: nameAt(FIRST) },
    { offset: FIRST + 1, spawn: 'SpawnB', name: nameAt(FIRST + 1) },
    { offset: THIRD, spawn: 'SpawnB', name: nameAt(THIRD) },
    { offset: DROP_AT, spawn: 'SpawnA', name: nameAt(DROP_AT) },
    { offset: REARMED, spawn: 'SpawnA', name: nameAt(REARMED) },
  ];
  const got = state.calls.map((c) => ({ offset: c.offset, spawn: c.spawn, name: c.name }));
  if (JSON.stringify(got) !== JSON.stringify(want)) problems.push(`spawnCreep calls ${JSON.stringify(got)}, expected ${JSON.stringify(want)}`);
  for (const c of state.calls) {
    if (c.args !== 2) problems.push(`offset ${c.offset}: spawnCreep with ${c.args} arguments, expected 2`);
    if (!c.isArray || JSON.stringify(c.body) !== JSON.stringify(BODY)) problems.push(`offset ${c.offset}: body ${JSON.stringify(c.body)}, expected ${JSON.stringify(BODY)}`);
  }
  // No request can be served while SpawnA is held and SpawnB busy, nor on the quiet tick (no request), nor once both
  // are busy again: no spawn job runs, so the plain build reads no clock in its loop (the constants self-check is done
  // by then; movement's next rescan is 50 ticks after the first).
  const idle = [];
  for (let o = AGAIN_AT + 1; o < B_FREE; o++) idle.push(o);
  idle.push(QUIET, REARMED + 1);
  for (const o of idle) {
    const used = world.loopCalls[o]?.getUsed;
    if (used !== 0) problems.push(`offset ${o}: ${used} getUsed calls with no request to serve, expected 0`);
  }

  // Console replies.
  const reply = (cmd) => commandResults(world, cmd);
  const accepted = reply(ORDER_CMD).concat(reply(ROLE_CMD));
  const acceptedPrefix = `rustedscreep spawn: from the next tick, a spawn of ${ROOM} spawns one worker`;
  if (accepted.length !== 4 || !accepted.every((r) => r.startsWith(acceptedPrefix) && !r.includes('no own spawn'))) {
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
  const wantLines = [FIRST + 1, THIRD, DROP_AT, REARMED].map((o) => `rustedscreep spawn: ${nameAt(o)} accepted (${BODY.length} parts, ${BODY_COST} energy, ${SPAWN_TIME} ticks), the console's request`);
  if (JSON.stringify(lines) !== JSON.stringify(wantLines)) problems.push(`accepted log lines ${JSON.stringify(lines)}, expected ${JSON.stringify(wantLines)}`);
  const rearm = world.logs.filter((l) => l.includes('was accepted but is not in Game.creeps'));
  const wantRearm = `rustedscreep spawn: ${nameAt(DROP_AT)} was accepted but is not in Game.creeps; the console's request stands again`;
  if (rearm.length !== 1 || !rearm[0].endsWith(wantRearm)) problems.push(`re-arm warnings ${JSON.stringify(rearm)}, expected one "${wantRearm}"`);

  // Summaries: SpawnA still held on THIRD; then the quiet tick (SpawnB busy, SpawnA free); then with a request for a
  // room without a spawn and both spawns busy.
  const summaries = reply(SUMMARY_CMD);
  const event = (o, outcome) => `${nameAt(o)} at tick ${T0 + o} (${BODY.length} parts, ${BODY_COST} energy): ${outcome}`;
  const wantParts = [
    [
      `rustedscreep spawner at tick ${T0 + THIRD}: ${ROOM} energy ${CAPACITY}/${CAPACITY}, spawns idle 1 busy 0 held 1`,
      '| requests 1, orders 1 (ok 1, failed 0), waiting 0, unfit 0, no spawn 0, held 1 |',
    ],
    [
      `rustedscreep spawner at tick ${T0 + QUIET}: ${ROOM} energy ${CAPACITY}/${CAPACITY}, spawns idle 1 busy 1 held 0`,
      '| requests 0, orders 0 (ok 0, failed 0), waiting 0, unfit 0, no spawn 0, held 0 |',
      `since boot: ticks ${QUIET}, orders 3, ok 2, failed 1`,
      `| last accepted ${event(THIRD, 'ok')}`,
      `| last failure ${event(FIRST, 'rcl not enough')}`,
      '| console request: none',
      `| own creeps: ${ROOM} worker 2, other names 1`,
    ],
    [
      `rustedscreep spawner at tick ${T0 + LAST_SUMMARY_AT}: ${ROOM} energy ${CAPACITY - BODY_COST}/${CAPACITY}, spawns idle 0 busy 2 held 0`,
      '| requests 1, orders 0 (ok 0, failed 0), waiting 0, unfit 0, no spawn 1, held 0 |',
      `since boot: ticks ${LAST_SUMMARY_AT}, orders 5, ok 4, failed 1`,
      `| last accepted ${event(REARMED, 'ok')}`,
      `| console request: ${OTHER} worker`,
      `| own creeps: ${ROOM} worker 3, other names 1`,
    ],
  ];
  if (summaries.length !== wantParts.length) problems.push(`${summaries.length} spawner summaries, expected ${wantParts.length}`);
  else {
    wantParts.forEach((parts, i) => {
      for (const p of parts) if (!summaries[i].includes(p)) problems.push(`spawner summary ${i + 1} lacks "${p}": ${summaries[i]}`);
    });
  }

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
