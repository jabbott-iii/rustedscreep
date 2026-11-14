// Runs a built bundle in a small Screeps-like runtime, following the open-source engine and driver as
// VERIFICATION_LOG V96 records them: each tick gets a fresh console with only log and logUnsafe, Game
// and Memory; Memory is parsed from a JSON string at the start of the tick and saved at its end; a halt
// discards that tick's Memory and starts a new global, which evaluates main.js again; console commands
// run after loop. TextDecoder and TextEncoder are absent (V72), and so are fetch, URL and document
// (assumed, V95). npm run build runs the plain smoke test; npm run check also runs a trap-test build
// through 10 forced traps. Standalone: node js/tools/smoke.mjs [dir].
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import vm from 'node:vm';
import { ROOT, WASM_MODULE, fail, isMain } from './lib/common.mjs';
import { sceneAt } from './lib/world.mjs';

const ABSENT_GLOBALS = ['TextDecoder', 'TextEncoder', 'fetch', 'URL', 'document'];

/**
 * Runs the bundle in `dir` for up to `ticks` ticks. `command(world)` may return console command source
 * to evaluate after that tick's loop, as the game does. Stops early once `until(world)` is true.
 * `scene(offset)` gives the tick's Game.creeps, Game.rooms and Game.structures (js/tools/lib/world.mjs).
 */
export function simulate(dir, {
  ticks = 6, bucket = 10000, command = null, until = null, memoryJson = '{}', withHalt = true, wasmBytes = null,
  scene = sceneAt,
} = {}) {
  const code = readFileSync(join(dir, 'main.js'), 'utf8');
  const bytes = wasmBytes ?? readFileSync(join(dir, `${WASM_MODULE}.wasm`));
  const world = { time: 50000000, memoryJson, logs: [], notifies: [], halts: 0, globals: 0, ticksRun: 0 };
  let ctx = null;

  function newGlobal() {
    world.globals += 1;
    const module = { exports: {} };
    const context = vm.createContext({ module, exports: module.exports });
    vm.runInContext('globalThis.global = globalThis;', context);
    const Uint8 = vm.runInContext('Uint8Array', context);
    context.require = (name) => {
      if (name === WASM_MODULE) return new Uint8(bytes);
      throw new Error(`Unknown module '${name}'`);
    };
    for (const g of ABSENT_GLOBALS) {
      if (vm.runInContext(`typeof ${g}`, context) !== 'undefined') throw new Error(`the sandbox exposes ${g}`);
    }
    return { context, module, evaluated: false };
  }

  for (let offset = 0; offset < ticks; offset++) {
    ctx ??= newGlobal();
    const { context } = ctx;
    let halted = false;
    const start = performance.now();
    // The engine's Memory getter sets it to null when the raw string is not valid JSON (V96).
    let memory;
    try {
      memory = JSON.parse(world.memoryJson || '{}');
    } catch {
      memory = null;
    }
    context.console = Object.create(null, {
      log: { value: (...a) => world.logs.push(a.map(String).join(' ')), writable: true, configurable: true },
      logUnsafe: { value: (...a) => world.logs.push(a.map(String).join(' ')), writable: true, configurable: true },
    });
    context.Memory = memory;
    context.Game = {
      ...scene(offset),
      time: world.time,
      cpu: {
        bucket, limit: 20, tickLimit: 500,
        getUsed: () => performance.now() - start,
        ...(withHalt ? { halt: () => { halted = true; world.halts += 1; } } : {}),
      },
      notify: (message) => world.notifies.push(String(message)),
    };
    if (!ctx.evaluated) {
      vm.runInContext(code, context, { filename: 'main.js' });
      ctx.evaluated = true;
      if (typeof ctx.module.exports.loop !== 'function') throw new Error('main.js does not export loop');
    }
    ctx.module.exports.loop();
    const cmd = !halted && command ? command(world) : null;
    if (cmd) {
      world.logs.push(`> ${cmd}`);
      world.logs.push(String(vm.runInContext(cmd, context)));
    }
    world.ticksRun += 1;
    world.time += 1;
    if (halted) {
      ctx = null; // halt: this tick's Memory is discarded, and the next tick starts a new global
    } else if (memory !== null) {
      world.memoryJson = JSON.stringify(memory); // a null Memory is not saved, so the raw string stays
    }
    if (until && until(world)) break;
  }
  return world;
}

/**
 * What `rustedscreep.snapshot()` must report after the loops at these offsets of sceneAt (js/tools/lib/world.mjs):
 * the first loop runs at offset 1, so at 3 the Invader (new at 2) is old and the spawning builder is new,
 * and at 5 the scout and the Invader are gone and two objects are skipped each tick. The per-room counts
 * come from decoded positions, so they also cover W, E, N and S room bits and the room without a controller.
 */
const SNAPSHOT_EXPECTED = {
  3: [
    'rustedscreep snapshot at tick 50000003: cpu limit 20, tickLimit 500, bucket 10000',
    'rooms 3 (owned 1): W1N1 mine RCL 2 (creeps 2, structures 2), W2N1 neutral (creeps 2, structures 0), ' +
      'E0S0 no controller (creeps 1, structures 0)',
    'creeps 5 (mine 4, hostile 1, spawning 1; new 1, gone 0, live handles 5)',
    'structures 2 (new 0, gone 0, live handles 2)',
    'skipped 0',
  ],
  5: [
    'rooms 2 (owned 1): W1N1 mine RCL 2 (creeps 2, structures 2), E0S0 no controller (creeps 1, structures 0)',
    'creeps 3 (mine 3, hostile 0, spawning 1; new 0, gone 0, live handles 3)',
    'structures 2 (new 0, gone 0, live handles 2)',
    'skipped 2, bad id 1, unknown structure type 1',
  ],
};

/**
 * What `rustedscreep.scheduler()` must report after the loops at these offsets. The fake world has CPU limit
 * 20, tickLimit 500 and bucket 10000, so with SCHEDULER_MMO (crates/rustedscreep-core/src/config.rs) the
 * ceiling is 500 × (100 − 10) / 100 = 450 and the Optional cap 20 + (10000 − 9000) / 100 = 30. The first loop
 * of a global runs no Optional work (warm-up), and no job runs yet, so the clock is never sampled.
 */
const SCHEDULER_EXPECTED = {
  1: [
    'rustedscreep scheduler at tick 50000001 (loop 0 of this global): cpu limit 20, tickLimit 500, bucket 10000',
    'ceiling 450.0; critical on; standard on (cap 450.0); optional off (warm-up)',
    'samples 0 (bad 0), units 0, budget stops 0, tier-off skips 0, out of order 0, faults 0',
    'faults since boot 0 (this window 0)',
  ],
  4: ['(loop 3 of this global)', 'optional on (cap 30.0)', ' | clock now '],
};

/** The console command the smoke test sends at offset SAY_AT, and what the creeps must say on the next loop. */
const SAY_AT = 6;
const SAY_CMD = 'rustedscreep.say("hi")';
/** A say with a number, at offset BAD_SAY_AT: the loader refuses it before the wasm. */
const BAD_SAY_AT = 2;
const BAD_SAY_CMD = 'rustedscreep.say(42)';

/**
 * What `rustedscreep.act()` must report at offset SAY_AT + 1, after the loop that issued the say: the two own
 * creeps that are not spawning say it (one call each, free, so billed 0, V34), the spawning builder is refused
 * before any call, and the Invader in W1N1 is not asked (the say goes to own creeps only). The loops ran at
 * offsets 1 to 7, so 7 ticks since boot. One more loop runs after it, which must call nothing.
 */
const ACT_EXPECTED = {
  7: [
    'rustedscreep act at tick 50000007: queued 2 (replaced 0), refused 1, spawning 1 | blocked 0 | ' +
      'issued 2: ok 2 (billed 0), failed 0, unknown 0, threw 0, missing 0 | since boot: ticks 7, queued 2, ' +
      'refused 1, blocked 0, issued 2, ok 2, billed 0, failed 0, unknown 0, threw 0, missing 0',
  ],
};

/** The results of `cmd` in tick order: each command's result is logged right after it. */
export function commandResults(world, cmd) {
  return world.logs.filter((l, i) => i > 0 && world.logs[i - 1] === `> ${cmd}`);
}

/** Checks each result of `cmd` against the parts expected at its offset. */
export function checkResults(world, cmd, expectedByOffset, label) {
  const results = commandResults(world, cmd);
  const expected = Object.entries(expectedByOffset);
  if (results.length !== expected.length) throw new Error(`${results.length} ${label} summaries, expected ${expected.length}`);
  expected.forEach(([offset, parts], i) => {
    const missing = parts.filter((p) => !results[i].includes(p));
    if (missing.length) throw new Error(`${label} at offset ${offset} lacks ${missing.join(' / ')}: ${results[i]}`);
  });
  return results;
}

/**
 * The plain smoke test npm run build runs: the bundle boots, logs a matching CRC-32, runs clean, its
 * snapshot and scheduler summaries of the fake world match SNAPSHOT_EXPECTED and SCHEDULER_EXPECTED, and a
 * console say reaches exactly the right creeps through the act layer (ACT_EXPECTED).
 */
export function smoke(dir = join(ROOT, 'dist'), { features = [] } = {}) {
  // An act-test build queues scripted intents every tick, so its act path is checked by act-sim.mjs instead.
  const checkAct = !features.includes('act-test');
  const calls = [];
  const world = simulate(dir, {
    ticks: 9,
    command: (w) => {
      if (SNAPSHOT_EXPECTED[w.ticksRun]) return 'rustedscreep.snapshot()';
      if (SCHEDULER_EXPECTED[w.ticksRun]) return 'rustedscreep.scheduler()';
      if (checkAct && w.ticksRun === BAD_SAY_AT) return BAD_SAY_CMD;
      if (checkAct && w.ticksRun === SAY_AT) return SAY_CMD;
      if (checkAct && ACT_EXPECTED[w.ticksRun]) return 'rustedscreep.act()';
      return null;
    },
    scene: (offset) => sceneAt(offset, {
      calls: { push: (call) => calls.push([offset, ...call]) },
    }),
  });
  const summaries = checkResults(world, 'rustedscreep.snapshot()', SNAPSHOT_EXPECTED, 'snapshot');
  summaries.forEach((s) => {
    if (s.includes('NOT aligned')) throw new Error(`snapshot: JS objects and rows differ: ${s}`);
  });
  for (const s of checkResults(world, 'rustedscreep.scheduler()', SCHEDULER_EXPECTED, 'scheduler')) {
    // The sandbox's getUsed is real elapsed time since the tick started, so a reading of 0 or more.
    if (!/ \| clock now \d+\.\d\d \|/.test(s)) throw new Error(`scheduler: no clock reading: ${s}`);
  }
  if (checkAct) {
    checkResults(world, 'rustedscreep.act()', ACT_EXPECTED, 'act');
    const said = commandResults(world, SAY_CMD);
    if (said.length !== 1 || !said[0].includes('says "hi" on the next tick')) throw new Error(`say command: ${said.join(' / ')}`);
    const refused = commandResults(world, BAD_SAY_CMD);
    if (refused.length !== 1 || !refused[0].includes('give the text as a string')) throw new Error(`say(42): ${refused.join(' / ')}`);
    // The only JS calls a build without act-test makes are the console's says, on the loop after the command
    // (offset 8, the loop after that, must call nothing).
    const wantCalls = [[SAY_AT + 1, 'say', 'Harvester', 'hi', false], [SAY_AT + 1, 'say', 'Scout2', 'hi', false]];
    if (JSON.stringify(calls) !== JSON.stringify(wantCalls)) throw new Error(`creep calls ${JSON.stringify(calls)}, expected ${JSON.stringify(wantCalls)}`);
  }
  if (world.logs.some((l) => l.startsWith('rustedscreep fault'))) throw new Error('a fault was logged with no job running');
  const boot = world.logs.find((l) => l.startsWith('rustedscreep boot:'));
  if (!boot) throw new Error(`no boot log; logs: ${world.logs.join(' / ')}`);
  if (!boot.includes(': match)')) throw new Error(`the boot log does not report a matching CRC-32: ${boot}`);
  const booted = world.logs.findIndex((l) => l.startsWith('rustedscreep booted in'));
  if (booted < 0) throw new Error(`the bundle never finished booting; logs: ${world.logs.join(' / ')}`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]/.test(l));
  if (bad.length || world.halts) throw new Error(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts})`);
  return { ticks: world.ticksRun, bootedAt: booted, logs: world.logs };
}

if (isMain(import.meta.url)) {
  try {
    const result = smoke(process.argv[2]);
    console.log(`${result.ticks} ticks ran`);
    for (const l of result.logs) console.log(`  ${l}`);
  } catch (e) {
    fail(`bundle smoke test failed: ${e.message}`);
  }
}
