// End-to-end act-layer test in the Screeps-like sandbox (js/tools/smoke.mjs). It builds the `act-test`
// variant under target/, whose scripted producer (crates/rustedscreep-wasm/src/act_test.rs) queues every
// intent variant, a replacement, blocked intents and one push per refusal reason the scene allows each tick,
// and checks that:
// - exactly the right intents reach JS, by actor row and then blockers first, with the right method names and
//   arguments (a target by its game object, a resource by its string, an omitted amount as undefined, a
//   public flag as true);
// - a blocker that is accepted skips what it blocks, and one that the game refuses (b's rangedHeal on even
//   ticks) lets the intent it would block (b's repair) be issued;
// - each return value is classified: OK, a known error code, undefined as unknown, and a throw caught by the
//   binding and logged as a rate-limited fault, after which the tick goes on (on the last tick every method
//   throws);
// - `rustedscreep.act()` reports the same counts.
// npm run check runs it; standalone: node js/tools/act-sim.mjs. stage.mjs refuses this build for deploy/.
import { join } from 'node:path';
import { build } from './build.mjs';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { packPos, sceneAt } from './lib/world.mjs';
import { checkResults, simulate } from './smoke.mjs';

// @screeps/common constants (screeps-game-api ErrorCode): ERR_NOT_ENOUGH_RESOURCES, ERR_FULL, ERR_NOT_IN_RANGE.
const ERR_NOT_ENOUGH = -6;
const ERR_FULL = -8;
const ERR_NOT_IN_RANGE = -9;
const ALL_THROW_AT = 6;
const TICKS = ALL_THROW_AT + 1;

function thrower() {
  throw new Error('act-sim: scripted throw');
}

/** What the fake creep methods return at `offset` (OK for any method not listed). */
function returnsAt(offset) {
  if (offset === ALL_THROW_AT) {
    return Object.fromEntries(['move', 'say', 'drop', 'transfer', 'withdraw', 'heal', 'rangedHeal', 'repair',
      'upgradeController', 'pull'].map((m) => [m, thrower]));
  }
  return {
    transfer: ERR_FULL,
    withdraw: ERR_NOT_ENOUGH,
    upgradeController: () => undefined,
    pull: thrower,
    // On even ticks b's rangedHeal is refused, so it does not block b's repair (V45, V108).
    ...(offset % 2 === 0 ? { rangedHeal: ERR_NOT_IN_RANGE } : {}),
  };
}

function place(object, x, y) {
  object.pos = { x, y, roomName: 'W1N1', __packedPos: packPos('W1N1', x, y) };
}

/**
 * The shared scene with every scripted target in range: a (Harvester) at (24,24) next to the spawn (25,25) and
 * 3 from the controller (24,21); b (the Scout, then Scout2) next to a; the spawning Builder on the spawn; and a
 * far extension at (40,40) for the one out-of-range push. The structures' places are read once (first seen),
 * so they are set from the first tick.
 */
function actScene(offset, actions) {
  const scene = sceneAt(offset, actions);
  const { creeps, structures } = scene;
  place(creeps.Harvester, 24, 24);
  if (creeps.Scout) place(creeps.Scout, 25, 24);
  place(creeps.Scout2, 23, 24);
  if (creeps.Builder) place(creeps.Builder, 25, 25);
  for (const s of Object.values(structures)) {
    if (s.structureType === 'controller') place(s, 24, 21);
  }
  const far = { id: '5bbcaa9099f9b0a0f7f4b3f1', structureType: 'extension', my: true };
  place(far, 40, 40);
  structures[far.id] = far;
  return scene;
}

/**
 * The calls one loop must make. a's plan, blockers first: heal, rangedHeal, repair, drop, transfer, withdraw,
 * move (its second push, Right = 3), say, upgradeController, pull; an accepted heal skips its rangedHeal and
 * repair. b's plan: rangedHeal, repair, move toward a; an accepted rangedHeal skips the repair. On the
 * all-throw tick nothing is recorded, so nothing is skipped.
 */
function expectedCalls(offset) {
  const b = offset <= 3 ? 'Scout' : 'Scout2';
  const allThrow = offset === ALL_THROW_AT;
  const bRefused = allThrow || offset % 2 === 0;
  return [
    ['heal', 'Harvester', b],
    ...(allThrow ? [['rangedHeal', 'Harvester', b], ['repair', 'Harvester', 'Spawn1']] : []),
    ['drop', 'Harvester', 'energy', 5],
    ['transfer', 'Harvester', 'Spawn1', 'energy', '<undefined>'],
    ['withdraw', 'Harvester', 'Spawn1', 'energy', 10],
    ['move', 'Harvester', 3],
    ['say', 'Harvester', 'act test', true],
    ['upgradeController', 'Harvester', 'controller'],
    ['pull', 'Harvester', b],
    ['rangedHeal', b, 'Harvester'],
    ...(bRefused ? [['repair', b, 'Spawn1']] : []),
    ['move', b, 'Harvester'],
  ];
}

// Each loop queues 14 (a's second move replaces its first). Refused every tick: a drop of 0, a transfer to the
// controller, one to the far extension; at offsets 2 and 3 the Invader's move (not mine) and from 6 the second
// Invader's; from 3 the spawning Builder's say and a's heal of it; from 4 a's rangedHeal of the gone Scout and
// a move by it. Odd ticks: 10 calls, 3 skipped, 6 OK (heal, drop, move, say, rangedHeal, b's move), 5 billed
// (say is free, V34), full 1, not enough 1, unknown 1 (upgradeController), threw 1 (pull). Even ticks: b's
// rangedHeal fails (not in range), so b's repair is called and OK: 11 calls, 2 skipped, 6 OK, 5 billed,
// 3 failed. The all-throw tick: 13 calls, all thrown, none skipped. Since boot the counts also hold the opening's one
// createConstructionSite call (ADR-020: W1N1 at RCL2 has one extension, the far one, of the 5 it allows), on the loop
// after the constants self-check (offset 2): issued, ok and billed each one more than before ADR-020 from offset 3 on
// (V203; the fake room places nothing, so the next pick waits 10 loops, past this run).
const ACT_EXPECTED = {
  3: [
    'rustedscreep act at tick 50000003: queued 14 (replaced 1), refused 6, not mine 1, spawning 1, ' +
      'spawning target 1, wrong target 1, not in range 1, zero amount 1 | blocked 3 | issued 10: ok 6 (billed 5), ' +
      'failed 2, not enough 1, full 1, unknown 1, threw 1, missing 0 | since boot: ticks 3, queued 42, refused 13, ' +
      'blocked 8, issued 32, ok 19, billed 16, failed 7, unknown 3, threw 3, missing 0 | last failure at tick ' +
      '50000003: pull threw',
  ],
  4: [
    'rustedscreep act at tick 50000004: queued 14 (replaced 1), refused 7, gone actor 1, spawning 1, gone target 1, ' +
      'spawning target 1, wrong target 1, not in range 1, zero amount 1 | blocked 2 | issued 11: ok 6 (billed 5), ' +
      'failed 3, not enough 1, full 1, not in range 1, unknown 1, threw 1, missing 0 | since boot: ticks 4, ' +
      'queued 56, refused 20, blocked 10, issued 43, ok 25, billed 21, failed 10, unknown 4, threw 4, missing 0 | ' +
      'last failure at tick 50000004: rangedHeal not in range',
  ],
  6: [
    'rustedscreep act at tick 50000006: queued 14 (replaced 1), refused 8, gone actor 1, not mine 1, spawning 1, ' +
      'gone target 1, spawning target 1, wrong target 1, not in range 1, zero amount 1 | blocked 0 | issued 13: ' +
      'ok 0 (billed 0), failed 0, unknown 0, threw 13, missing 0 | since boot: ticks 6, queued 84, refused 35, ' +
      'blocked 13, issued 66, ok 31, billed 26, failed 12, unknown 5, threw 18, missing 0 | last failure at tick ' +
      '50000006: move threw',
  ],
};

/**
 * Fault lines: one per pull throw at offsets 1 to 5 (unit 0, the Harvester's handle slot), then the all-throw
 * tick's first five throws, which use up the window's 10 reports (SchedulerConfig::fault_reports_per_window);
 * the other 8 are counted, and the window's closing note would come 100 ticks later.
 */
function expectedFaults() {
  const at = (o, what) => `rustedscreep fault at tick ${50000000 + o}: job act unit 0: the ${what} call threw`;
  return [
    ...[1, 2, 3, 4, 5].map((o) => at(o, 'pull')),
    ...['heal', 'rangedHeal', 'repair', 'drop', 'transfer'].map((m) => at(ALL_THROW_AT, m)),
  ];
}

export function actSim() {
  const dist = join(ROOT, 'target', 'act-test');
  build({ features: ['act-test'], dist });
  const calls = {};
  const world = simulate(dist, {
    ticks: TICKS,
    command: (w) => (ACT_EXPECTED[w.ticksRun] ? 'rustedscreep.act()' : null),
    scene: (offset) => {
      calls[offset] = [];
      return actScene(offset, { calls: calls[offset], returns: returnsAt(offset) });
    },
  });
  const problems = [];
  // The first loop runs at offset 1 (the boot is split over two ticks).
  if (calls[0].length) problems.push(`calls before the first loop: ${JSON.stringify(calls[0])}`);
  for (let offset = 1; offset < TICKS; offset++) {
    const want = expectedCalls(offset);
    if (JSON.stringify(calls[offset]) !== JSON.stringify(want)) {
      problems.push(`offset ${offset}: calls ${JSON.stringify(calls[offset])}\n  expected ${JSON.stringify(want)}`);
    }
  }
  try {
    checkResults(world, 'rustedscreep.act()', ACT_EXPECTED, 'act');
  } catch (e) {
    problems.push(e.message);
  }
  const faults = world.logs.filter((l) => l.startsWith('rustedscreep fault'));
  const wantFaults = expectedFaults();
  if (JSON.stringify(faults) !== JSON.stringify(wantFaults)) problems.push(`fault lines:\n  ${faults.join('\n  ')}`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]/.test(l));
  if (bad.length || world.halts) problems.push(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts})`);
  if (problems.length) throw new Error(`${problems.join('\n')}\nlogs:\n${world.logs.join('\n')}`);
  return { ticks: world.ticksRun, calls: Object.values(calls).reduce((n, c) => n + c.length, 0) };
}

if (isMain(import.meta.url)) {
  try {
    const r = actSim();
    console.log(`act layer: ${r.calls} calls over ${r.ticks} ticks, all as scripted`);
  } catch (e) {
    fail(`act layer test failed: ${e.message}`);
  }
}
