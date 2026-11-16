// Runs a built bundle in a small Screeps-like runtime, following the open-source engine and driver as
// VERIFICATION_LOG V96 records them: each tick gets a fresh console with only log and logUnsafe, Game
// and Memory; Memory is parsed from a JSON string at the start of the tick and saved at its end; a halt
// discards that tick's Memory and starts a new global, which evaluates main.js again; console commands
// run after loop. RawMemory follows the driver's segment lifecycle (driverRawMemory: the active set lives
// in the world, applies on the next tick and survives globals; V116 to V120), and every key of
// RawMemory.segments is saved at the end of the tick with the driver's checks (V41, V85). TextDecoder and TextEncoder are absent (V72), and so are fetch, URL and document
// (assumed, V95). npm run build runs the plain smoke test; npm run check also runs a trap-test build
// through 10 forced traps. npm run build runs the smoke test twice, with the wasm as a binary module and as the base64
// text module the GitHub link delivers (ADR-015). Standalone: node js/tools/smoke.mjs [dir] [binary|base64].
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import vm from 'node:vm';
import { FIXTURE_DIR, parseFixture } from './consts-fixture.mjs';
import { ROOT, WASM_MODULE, fail, isMain } from './lib/common.mjs';
import { wasmModuleText } from './lib/wasm-text.mjs';
import { sceneAt } from './lib/world.mjs';

// atob, btoa and Buffer were never probed on the MMO (V95), so the sandbox has none and the loader needs none.
const ABSENT_GLOBALS = ['TextDecoder', 'TextEncoder', 'fetch', 'URL', 'document', 'atob', 'btoa', 'Buffer'];

/**
 * The engine's game constants as [name, JSON text] pairs: @screeps/common 2.16.0 lib/constants.js, the fixture the
 * constants self-check's native test reads (js/tools/consts-fixture.mjs). The engine copies them onto the player's
 * global object at the start of every tick as `_.extend(globals, JSON.parse(JSON.stringify(C)))` (V129); the
 * sandbox does the same.
 */
export const ENGINE_CONSTANTS = parseFixture(readFileSync(join(FIXTURE_DIR, 'constants-common-2.16.0.tsv'), 'utf8'));

/** Driver runtime.js at cf63d8a (V41, V85): segment ids 0-99, at most 10 saved per tick, 100 × 1024 UTF-16 units each. */
const SEGMENT_IDS = 100;
const SEGMENTS_PER_TICK = 10;
const SEGMENT_SIZE_LIMIT = 100 * 1024;

/**
 * The driver's end-of-tick segment save (V85): it throws, in this order, on more than 10 keys, a key outside
 * 0-99, a value that is not a string and a value over the size limit, and otherwise stores `'' + value` for
 * every key. A throw there would likely lose the tick (V41, inferred); the sandbox records it instead.
 */
export function saveSegments(segments, world, offset) {
  if (segments === null || typeof segments !== 'object') return;
  const keys = Object.keys(segments);
  let error = null;
  if (keys.length > SEGMENTS_PER_TICK) error = `more than ${SEGMENTS_PER_TICK} segments (${keys.length})`;
  for (const key of keys) {
    if (error) break;
    const id = Number.parseInt(key, 10);
    const value = segments[key];
    if (!(id >= 0 && id < SEGMENT_IDS)) error = `segment id ${key}`;
    else if (typeof value !== 'string') error = `memory segment #${key} is not a string`;
    else if (value.length > SEGMENT_SIZE_LIMIT) error = `memory segment #${key} is ${value.length} long`;
  }
  if (error) {
    world.segmentErrors.push(`offset ${offset}: ${error}`);
    return;
  }
  for (const key of keys) world.segments[key] = `${segments[key]}`;
  if (keys.length) world.segmentSaves.push({ offset, keys });
}

/**
 * The tick's RawMemory, following the driver at cf63d8a (V116 to V120): `segments` holds the segments of
 * the active set stored in the world (one never written loads as '', V117); `setActiveSegments` checks
 * like the driver (an array of at most 10 ids, each parseInt'ed into 0 to 99; a bad id throws after the
 * ids before it are kept, V118), the last call of a tick wins, and `endTick` makes it the world's set for
 * the next tick, across globals. Calls are recorded in `world.activeCalls` as [offset, ids]. `set` stores the
 * raw Memory string like the driver (V180: it throws on a non-string or above 2 MB); `simulate` then saves that
 * string at the end of a tick that did not halt, instead of stringifying the tick's Memory. Calls go to
 * `world.rawSets` as [offset, value].
 */
export function driverRawMemory(offset, world) {
  world.activeSegments ??= [];
  world.activeCalls ??= [];
  const segments = Object.create(null);
  for (const id of world.activeSegments) segments[id] = world.segments[id] ?? '';
  let next = null;
  let memorySet;
  return {
    segments,
    setActiveSegments(ids) {
      if (!Array.isArray(ids)) throw new Error('RawMemory.setActiveSegments: argument must be an array');
      if (ids.length > SEGMENTS_PER_TICK) throw new Error(`RawMemory.setActiveSegments: only ${SEGMENTS_PER_TICK} segments can be active`);
      next = [];
      for (const x of ids) {
        const id = Number.parseInt(x, 10);
        if (Number.isNaN(id) || id > SEGMENT_IDS - 1 || id < 0) throw new Error(`RawMemory.setActiveSegments: "${x}" is not a valid segment ID`);
        next.push(id);
      }
      world.activeCalls.push([offset, [...next]]);
    },
    set(value) {
      if (typeof value !== 'string') throw new Error('Raw memory value is not a string');
      if (value.length > 2 * 1024 * 1024) throw new Error('Raw memory length exceeded 2 MB limit');
      world.rawSets ??= [];
      world.rawSets.push([offset, value]);
      memorySet = value;
    },
    /** The string the last `set` of the tick stored, if any (the end-of-tick save keeps it). */
    get memorySet() {
      return memorySet;
    },
    endTick() {
      if (next !== null) world.activeSegments = next;
    },
  };
}

/**
 * Runs the bundle in `dir` for up to `ticks` ticks. `command(world)` may return console command source
 * to evaluate after that tick's loop, as the game does. Stops early once `until(world)` is true.
 * `scene(offset)` gives the tick's Game.creeps, Game.rooms and Game.structures (js/tools/lib/world.mjs).
 * `rawMemory(offset, world)` gives the tick's RawMemory global (undefined for none); by default
 * driverRawMemory, whose optional `endTick()` runs after a tick that did not halt. World: `segments` holds
 * the saved segment strings by id, `segmentSaves` the ticks that saved any, and `segmentErrors` the saves
 * the driver would have thrown on. `loopCalls[offset]` counts what the bundle did inside that tick's loop
 * call (`getUsed` calls, `RawMemory.segments` reads and the id lists passed to `setActiveSegments`), not
 * in console commands or the end-of-tick save. `constants` ([name, JSON text] pairs, the engine's by default) are
 * set on the global at the start of every tick, as the engine does (V129); `[]` gives a global without them.
 * `setup(context, world)` runs once per new global, before the bundle is evaluated (to instrument the realm).
 * `bucket` is Game.cpu.bucket: a number, or `bucket(offset)` for one per tick.
 * `wasmForm` is how require('rustedscreep_wasm') answers: 'binary', the module's bytes (an API upload, V31), or
 * 'base64', the text module the GitHub link delivers (ADR-015), read from `${dir}/rustedscreep_wasm.js` (or made from
 * `wasmBytes`) and evaluated in the driver's module wrapper once per global (V138); without the file it throws
 * "Unknown module", as the live link did with only the .wasm (V136).
 */
export function simulate(dir, {
  ticks = 6, bucket = 10000, command = null, until = null, memoryJson = '{}', withHalt = true, wasmBytes = null,
  scene = sceneAt, rawMemory = driverRawMemory, constants = ENGINE_CONSTANTS, setup = null, wasmForm = 'binary',
} = {}) {
  if (wasmForm !== 'binary' && wasmForm !== 'base64') throw new Error(`unknown wasm form ${wasmForm}`);
  const code = readFileSync(join(dir, 'main.js'), 'utf8');
  const textPath = join(dir, `${WASM_MODULE}.js`);
  const bytes = wasmForm === 'binary' ? (wasmBytes ?? readFileSync(join(dir, `${WASM_MODULE}.wasm`))) : null;
  const wasmText = wasmForm === 'base64' ? (wasmBytes ? wasmModuleText(wasmBytes) : existsSync(textPath) ? readFileSync(textPath, 'utf8') : null) : null;
  const world = {
    time: 50000000, memoryJson, logs: [], notifies: [], halts: 0, globals: 0, ticksRun: 0,
    segments: {}, segmentSaves: [], segmentErrors: [], loopCalls: [],
  };
  let ctx = null;

  function newGlobal() {
    world.globals += 1;
    const module = { exports: {} };
    const context = vm.createContext({ module, exports: module.exports });
    vm.runInContext('globalThis.global = globalThis;', context);
    const Uint8 = vm.runInContext('Uint8Array', context);
    // The engine caches a module's exports per global (engine game.js:551-594), and the driver evaluates a text
    // module as (function __module(module,exports){ <code> })(__module, __module.exports) (V138).
    const cache = new Map();
    context.require = (name) => {
      if (cache.has(name)) return cache.get(name);
      let value;
      if (name === WASM_MODULE && wasmForm === 'binary') value = new Uint8(bytes);
      else if (name === WASM_MODULE && wasmText !== null) {
        const __module = vm.runInContext('({ exports: {} })', context);
        context.__module = __module;
        vm.runInContext(`(function __module(module,exports){ ${wasmText}\n})(__module, __module.exports)`, context, { filename: `${WASM_MODULE}.js` });
        delete context.__module;
        value = __module.exports;
      } else throw new Error(`Unknown module '${name}'`);
      cache.set(name, value);
      return value;
    };
    for (const g of ABSENT_GLOBALS) {
      if (vm.runInContext(`typeof ${g}`, context) !== 'undefined') throw new Error(`the sandbox exposes ${g}`);
    }
    setup?.(context, world);
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
    for (const [name, json] of constants) context[name] = JSON.parse(json);
    const raw = rawMemory(offset, world);
    const calls = { getUsed: 0, segments: 0, setActive: [] };
    let inLoop = false;
    // A view of `raw` whose `segments` getter and `setActiveSegments` count the bundle's calls; the save
    // reads `raw` itself.
    context.RawMemory = raw === undefined || raw === null ? raw : Object.create(null, {
      segments: { get: () => { if (inLoop) calls.segments += 1; return raw.segments; }, enumerable: true },
      ...(typeof raw.setActiveSegments === 'function' ? {
        setActiveSegments: {
          value: (ids) => {
            if (inLoop) calls.setActive.push(Array.isArray(ids) ? [...ids] : ids);
            return raw.setActiveSegments(ids);
          },
          enumerable: true,
        },
      } : {}),
      ...(typeof raw.set === 'function' ? { set: { value: (v) => raw.set(v), enumerable: true } } : {}),
    });
    context.Game = {
      ...scene(offset),
      time: world.time,
      cpu: {
        bucket: typeof bucket === 'function' ? bucket(offset) : bucket, limit: 20, tickLimit: 500,
        getUsed: () => {
          if (inLoop) calls.getUsed += 1;
          return performance.now() - start;
        },
        ...(withHalt ? { halt: () => { halted = true; world.halts += 1; } } : {}),
      },
      notify: (message) => world.notifies.push(String(message)),
    };
    if (!ctx.evaluated) {
      vm.runInContext(code, context, { filename: 'main.js' });
      ctx.evaluated = true;
      if (typeof ctx.module.exports.loop !== 'function') throw new Error('main.js does not export loop');
    }
    inLoop = true;
    try {
      ctx.module.exports.loop();
    } finally {
      inLoop = false;
    }
    world.loopCalls[offset] = calls;
    const cmd = !halted && command ? command(world) : null;
    if (cmd) {
      world.logs.push(`> ${cmd}`);
      world.logs.push(String(vm.runInContext(cmd, context)));
    }
    world.ticksRun += 1;
    world.time += 1;
    if (halted) {
      ctx = null; // halt: this tick's Memory and segments are discarded, and the next tick starts a new global
    } else {
      // The driver stringifies the parsed Memory unless it is null; a RawMemory.set deletes the parse (V180), so the
      // string it set is kept.
      const rawSet = raw?.memorySet;
      if (rawSet !== undefined) world.memoryJson = rawSet;
      else if (memory !== null) world.memoryJson = JSON.stringify(memory);
      saveSegments(raw?.segments, world, offset);
      raw?.endTick?.();
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

/** PERSIST_MMO.config_segment (crates/rustedscreep-core/src/config.rs): the runtime config overrides. */
export const CONFIG_SEGMENT = 97;

/**
 * What `rustedscreep.scheduler()` must report after the loops at these offsets. The fake world has CPU limit
 * 20, tickLimit 500 and bucket 10000, so with SCHEDULER_MMO (crates/rustedscreep-core/src/config.rs) the
 * ceiling is 500 × (100 − 10) / 100 = 450 and the Optional cap 20 + (10000 − 9000) / 100 = 30. The first loop
 * of a global runs no Optional work (warm-up); its jobs are movement's first read of the owned room (P2.1), the
 * opening's two jobs for that room (P2.4: `opening`, Critical, and `opening work`, Standard) and the persistence
 * load's one step, the config record's look-up (P1.10), each sampling the clock twice, and the two Optional jobs,
 * the constants self-check (P1.11) and the opening's extension placement (ADR-020), are tier-off skips. Both run
 * from the second loop (see CONSTS_DONE and PLACEMENT_SAMPLES).
 */
const SCHEDULER_EXPECTED = {
  1: [
    'rustedscreep scheduler at tick 50000001 (loop 0 of this global): cpu limit 20, tickLimit 500, bucket 10000',
    'ceiling 450.0; critical on; standard on (cap 450.0); optional off (warm-up)',
    // Four jobs of one unit each, two samples each: movement's first read of the owned W1N1, where the
    // Harvester is (P2.1, a Critical job), the opening's two jobs for W1N1 (P2.4, Critical and Standard: was
    // samples 4, units 2 before them), and the persistence load job's one step, the config segment's
    // look-up (P1.10). Tier-off skips 2 (was 1 before ADR-020): the constants self-check and the extension
    // placement.
    'samples 8 (bad 0), units 4, budget stops 0, tier-off skips 2, out of order 0, faults 0',
    'faults since boot 0 (this window 0)',
  ],
  4: ['(loop 3 of this global)', 'optional on (cap 30.0)', ' | clock now '],
};

/**
 * The constants self-check's line against the engine's own constants (the sandbox sets the fixture on its global,
 * ENGINE_CONSTANTS): every upper-case global read (the constants and the built-in JSON), no finding, and the
 * documented differences counted (js/tools/consts-sim.mjs checks the findings and the JS calls in detail).
 */
const CONSTS_DONE = new RegExp(
  `^rustedscreep consts: checked at tick (\\d+) \\((\\d+) tick\\(s\\), \\d+ µCPU\\): ${ENGINE_CONSTANTS.length + 1} server names, ` +
    '\\d+ values compared: \\d+ match, 0 mismatched, 0 missing on the server, 0 not in the crate, 0 unreadable, ' +
    '0 unknown constants; \\d+ known differences$',
);

/**
 * At offset PROFILER_AT, `rustedscreep.profiler()`. A build without the profiler says so; a profiler build
 * shows 8 loops (offsets 1 to 8), the cold loop at the first, a window of the other 7, the say's two calls
 * (offset 7) with no billed intent (say is free, V34), and the extension placement's createConstructionSite call
 * (SITE_CALL, offset 2 or 3), billed as one intent (V203): 3 calls in 2 ticks, 1 billed (were 2 calls in 1 tick
 * and none billed before ADR-020).
 */
const PROFILER_AT = 8;
const PROFILER_EXPECTED = [
  'rustedscreep profiler at tick 50000008: loops 8 in this global | open window 7/1000 loops from tick 50000002: ',
  '; calls total 3 (max 2); billed total 1 (max 1) | act per call ',
  ' CPU over 3 calls in 2 ticks (bound 0.0222)',
  ' | last loop at tick 50000008: ',
  ' | windows closed 0 (kept 0) | segment 99: writes 0, failed 0 | cold loop at tick 50000001: start ',
];

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
 * offsets 1 to 7, so 7 ticks since boot. One more loop runs after it, which must call nothing. Since boot it also
 * counts the extension placement's one createConstructionSite call (ADR-020, see SITE_CALL): issued 3, ok 3 and
 * billed 1 (V203; were 2, 2 and 0 before ADR-020).
 */
const ACT_EXPECTED = {
  7: [
    'rustedscreep act at tick 50000007: queued 2 (replaced 0), refused 1, spawning 1 | blocked 0 | ' +
      'issued 2: ok 2 (billed 0), failed 0, unknown 0, threw 0, missing 0 | since boot: ticks 7, queued 2, ' +
      'refused 1, blocked 0, issued 3, ok 3, billed 1, failed 0, unknown 0, threw 0, missing 0',
  ],
};

/**
 * The extension placement (ADR-020) in the smoke world: W1N1 at RCL2 allows CONTROLLER_STRUCTURES.extension[2] = 5
 * and has none, so the Optional job picks the first tile of Spawn1's parity at range 2 (SPAWN_CLEAR), nearest by
 * path and then by tile index: (23, 23) for the spawn at (25, 25). The fake room places nothing, so the next pick
 * waits APPEAR_TICKS (10) loops, past this run: one call. It comes on the first loop the placement job runs, after
 * the constants self-check in the Optional tier: the check's last loop, or the next when the check used up that
 * loop's Optional cap (the sandbox clock is wall time).
 */
const SITE_CALL = ['W1N1', 23, 23, 'extension'];

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
export function smoke(dir = join(ROOT, 'dist'), { features = [], wasmForm = 'binary' } = {}) {
  // An act-test build queues scripted intents every tick, so its act path is checked by act-sim.mjs instead.
  const checkAct = !features.includes('act-test');
  const calls = [];
  const sites = [];
  const world = simulate(dir, {
    ticks: 9,
    wasmForm,
    command: (w) => {
      if (SNAPSHOT_EXPECTED[w.ticksRun]) return 'rustedscreep.snapshot()';
      if (SCHEDULER_EXPECTED[w.ticksRun]) return 'rustedscreep.scheduler()';
      if (checkAct && w.ticksRun === BAD_SAY_AT) return BAD_SAY_CMD;
      if (checkAct && w.ticksRun === SAY_AT) return SAY_CMD;
      if (checkAct && ACT_EXPECTED[w.ticksRun]) return 'rustedscreep.act()';
      if (w.ticksRun === PROFILER_AT) return 'rustedscreep.profiler()';
      return null;
    },
    scene: (offset) => sceneAt(offset, {
      calls: { push: (call) => calls.push([offset, ...call]) },
      sites: { push: (call) => sites.push([offset, ...call]) },
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
  const profiled = commandResults(world, 'rustedscreep.profiler()');
  if (profiled.length !== 1) throw new Error(`${profiled.length} profiler summaries, expected 1`);
  const cold = world.logs.filter((l) => l.startsWith('rustedscreep profiler: first loop of this global'));
  if (features.includes('profiler')) {
    const missing = checkAct ? PROFILER_EXPECTED.filter((p) => !profiled[0].includes(p)) : [];
    if (missing.length) throw new Error(`profiler summary lacks ${missing.join(' / ')}: ${profiled[0]}`);
    if (cold.length !== 1 || !cold[0].includes('at tick 50000001 (cpu limit 20, bucket 10000): start ')) {
      throw new Error(`profiler cold line: ${cold.join(' / ') || 'none'}`);
    }
  } else {
    if (!profiled[0].startsWith('rustedscreep profiler: not in this build')) throw new Error(`profiler: ${profiled[0]}`);
    if (cold.length) throw new Error(`a build without the profiler logged ${cold[0]}`);
  }
  // Readings inside loop once booted (the loader also reads the clock on the boot ticks and the first
  // loop): none without the profiler, six with it (crates/rustedscreep-wasm/src/profiler.rs), so the
  // feature compiles out and its documented count holds. The segment manager
  // (crates/rustedscreep-wasm/src/persist.rs) wants the config record (segment CONFIG_SEGMENT, P1.10) from
  // the global's start: the first loop (offset 1) looks it up (one RawMemory.segments read, inside the
  // load job's one step) and activates it; the next loop reads it (one more read and the job's two clock
  // samples; a segment never written loads as '', no overrides) and clears the set. After that no loop
  // touches the segments. The constants self-check runs from offset 2 (the first loop that allows Optional work)
  // until it is done, with a sample before its job and the slicer's samples (one per batch: about 30 for the whole
  // check, how many depends on the measured cost per step; js/tools/consts-sim.mjs checks that every reading is a
  // scheduler sample); after that it reads no clock but the opening's (P2.4): its two jobs run one unit each for
  // the owned W1N1 on every loop, a sample before and after each, so 4 per loop (was 0 before P2.4), and from the
  // second loop the extension placement's Optional job (ADR-020), one unit for W1N1, two more (was 0 before it).
  const OPENING_SAMPLES = 4;
  const PLACEMENT_SAMPLES = 2;
  const perLoop = (features.includes('profiler') ? 6 : 0) + OPENING_SAMPLES;
  const constsLines = world.logs.filter((l) => l.startsWith('rustedscreep consts:'));
  const constsDone = constsLines.length === 1 ? CONSTS_DONE.exec(constsLines[0]) : null;
  if (!constsDone) throw new Error(`constants self-check: expected one line matching ${CONSTS_DONE}, got ${constsLines.join(' / ') || 'none'}`);
  const constsLast = Number(constsDone[1]) - 50000000;
  const constsTicks = Number(constsDone[2]);
  if (constsTicks < 1 || constsTicks > constsLast - 1) throw new Error(`constants self-check: ${constsTicks} tick(s) from offset 2 to ${constsLast}`);
  const siteAt = sites[0]?.[0];
  if (sites.length !== 1 || JSON.stringify(sites[0].slice(1)) !== JSON.stringify(SITE_CALL) || siteAt < constsLast || siteAt > constsLast + 1) {
    throw new Error(`createConstructionSite calls ${JSON.stringify(sites)}, expected one ${JSON.stringify(SITE_CALL)} at offset ${constsLast} or ${constsLast + 1}`);
  }
  world.loopCalls.forEach((c, offset) => {
    const wantUsed = perLoop + (offset >= 2 ? PLACEMENT_SAMPLES : 0) + (offset === 2 ? 2 : 0);
    const checking = offset >= 2 && offset <= constsLast;
    if (checking && (c.getUsed < wantUsed || c.getUsed > wantUsed + 64)) throw new Error(`offset ${offset}: ${c.getUsed} getUsed calls in loop, expected ${wantUsed} to ${wantUsed + 64} with the constants self-check`);
    if (offset >= 2 && !checking && c.getUsed !== wantUsed) throw new Error(`offset ${offset}: ${c.getUsed} getUsed calls in loop, expected ${wantUsed}`);
    const wantReads = offset === 1 || offset === 2 ? 1 : 0;
    if (c.segments !== wantReads) throw new Error(`offset ${offset}: the loop read RawMemory.segments ${c.segments} time(s), expected ${wantReads}`);
    const wantActive = JSON.stringify({ 1: [[CONFIG_SEGMENT]], 2: [[]] }[offset] ?? []);
    if (JSON.stringify(c.setActive) !== wantActive) throw new Error(`offset ${offset}: setActiveSegments calls ${JSON.stringify(c.setActive)}, expected ${wantActive}`);
  });
  // No window closes in 9 ticks and no override is set, so no build writes a segment here. The only save
  // is the driver's re-save of the config segment the second loop loaded (every key present is saved,
  // V119), with the value it loaded: '' for a segment never written.
  const wantSaves = JSON.stringify([{ offset: 2, keys: [String(CONFIG_SEGMENT)] }]);
  if (JSON.stringify(world.segmentSaves) !== wantSaves || world.segments[CONFIG_SEGMENT] !== '' || world.segmentErrors.length) {
    throw new Error(`segments: saves ${JSON.stringify(world.segmentSaves)} (expected ${wantSaves}), errors ${world.segmentErrors.join(' / ')}`);
  }
  if (world.logs.some((l) => l.startsWith('rustedscreep fault'))) throw new Error('a fault was logged with no job running');
  const boot = world.logs.find((l) => l.startsWith('rustedscreep boot:'));
  if (!boot) throw new Error(`no boot log; logs: ${world.logs.join(' / ')}`);
  if (!boot.includes(': match)')) throw new Error(`the boot log does not report a matching CRC-32: ${boot}`);
  // The boot line names the delivery that ran: the text module's base64 (ADR-015) or a binary module's bytes.
  const form = wasmForm === 'base64' ? / \| wasm String\(base64, \d+ chars\) \d+ B crc32 / : / \| wasm Uint8Array \d+ B crc32 /;
  if (!form.test(boot)) throw new Error(`the boot log does not show the ${wasmForm} wasm module: ${boot}`);
  if (!boot.includes(', atob undefined, Buffer undefined')) throw new Error(`the boot log does not report atob and Buffer: ${boot}`);
  const booted = world.logs.findIndex((l) => l.startsWith('rustedscreep booted in'));
  if (booted < 0) throw new Error(`the bundle never finished booting; logs: ${world.logs.join(' / ')}`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]/.test(l));
  if (bad.length || world.halts) throw new Error(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts})`);
  return { ticks: world.ticksRun, bootedAt: booted, logs: world.logs };
}

if (isMain(import.meta.url)) {
  try {
    const result = smoke(process.argv[2], { wasmForm: process.argv[3] ?? 'binary' });
    console.log(`${result.ticks} ticks ran`);
    for (const l of result.logs) console.log(`  ${l}`);
  } catch (e) {
    fail(`bundle smoke test failed: ${e.message}`);
  }
}
