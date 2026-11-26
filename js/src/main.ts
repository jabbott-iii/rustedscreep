// The loader (P1 module 4): boots the wasm when the bucket allows, calls its loop every tick, and
// recovers from traps and aborted ticks (ADR-003). The state machine is in loader.ts; this file wires it
// to the Screeps runtime. No game logic. The polyfill import must stay first: once strings cross the
// boundary, the glue constructs TextDecoder and TextEncoder while it loads (V90).
import { textCodecSource } from './text-polyfill.js';
import {
  __wbg_reset_state, actSummary as wasmActSummary, configReset as wasmConfigReset, configSet as wasmConfigSet,
  claimsSummary as wasmClaimsSummary, configSummary as wasmConfigSummary, configUnset as wasmConfigUnset, constsSummary as wasmConstsSummary, initSync,
  labsSummary as wasmLabsSummary, loop as wasmLoop, mineralsSummary as wasmMineralsSummary, moveTo as wasmMoveTo, movementSummary as wasmMovementSummary,
  openingSummary as wasmOpeningSummary,
  persistSummary as wasmPersistSummary, persistTest as wasmPersistTest, plannerSummary as wasmPlannerSummary,
  profilerSummary as wasmProfilerSummary, remoteSummary as wasmRemoteSummary, signSummary as wasmSignSummary,
  queueSay as wasmQueueSay, schedulerSummary as wasmSchedulerSummary, scoutSummary as wasmScoutSummary,
  snapshotSummary as wasmSnapshotSummary,
  spawn as wasmSpawn, spawnerSummary as wasmSpawnerSummary, trapTest as wasmTrapTest,
} from '../pkg/rustedscreep_wasm.js';
import { type BuildInfo, type Env, type Glue, Loader, type LoaderConfig, type LoaderMemory, sanitizeLoaderMemory } from './loader.js';

// Replaced with object literals by rollup.config.mjs; npm run build sets them.
declare const __RUSTEDSCREEP_BUILD__: BuildInfo;
declare const __RUSTEDSCREEP_LOADER_CONFIG__: LoaderConfig;

const BUILD = __RUSTEDSCREEP_BUILD__;
const CONFIG = __RUSTEDSCREEP_LOADER_CONFIG__;
/** The open-source driver keeps the first 500 characters of a notification; the docs say 1000 (V96). */
const NOTIFY_MAX = 500;
const MEMORY_KEY = 'rustedscreepLoader';

const g = globalThis as Record<string, unknown>;

/** Used when Memory itself is unusable, so the loader keeps working for this global (V96). */
let heapRecord: LoaderMemory | undefined;
let memoryUnusableNotified = false;

/**
 * The loader's record in Memory, repaired if malformed. Screeps sets Memory to null when its raw string is
 * not valid JSON (V96); then the loader keeps its record in the heap and avoids halting, which would lose it.
 * Once Memory is an object again (the bot resets one that did not parse, ADR-019), the heap record moves back
 * into it, so this global's incidents carry on and halting is allowed again.
 */
function loaderMemory(): LoaderMemory {
  const mem: unknown = Memory;
  if (mem === null || typeof mem !== 'object' || Array.isArray(mem)) {
    heapRecord ??= sanitizeLoaderMemory(undefined, CONFIG.trapTestMax);
    if (!memoryUnusableNotified) {
      memoryUnusableNotified = true;
      const text = 'rustedscreep: Memory is unusable (its raw string is not a JSON object); loader state stays in the heap';
      console.log(text);
      Game.notify(text, CONFIG.notifyGroupMinutes);
    }
    return heapRecord;
  }
  const record = mem as Record<string, unknown>;
  if (heapRecord !== undefined) {
    record[MEMORY_KEY] = heapRecord;
    heapRecord = undefined;
    memoryUnusableNotified = false;
  }
  const repaired = sanitizeLoaderMemory(record[MEMORY_KEY], CONFIG.trapTestMax);
  record[MEMORY_KEY] = repaired;
  return repaired;
}

/** typeof console.error and console.warn before the loader adds its own, for the boot log (V95). */
let nativeConsole: { error: string; warn: string } | undefined;

const env: Env = {
  time: () => Game.time,
  bucket: () => Game.cpu.bucket,
  cpuUsed: () => Game.cpu.getUsed(),
  halt: () => {
    // With Memory unusable, a halt would also lose the loader's heap record, so recover in place instead.
    if (typeof Game.cpu.halt !== 'function' || heapRecord !== undefined) return false;
    Game.cpu.halt(); // ends the tick on Screeps (V96)
    return true;
  },
  notify: (message, groupMinutes) => Game.notify(message.slice(0, NOTIFY_MAX), groupMinutes),
  log: (message) => console.log(message),
  requireModule: (name) => require(name),
  compile: (bytes) => new WebAssembly.Module(bytes),
  memory: loaderMemory,
  globalsReport: () => {
    const kind = (name: string) => typeof g[name];
    return `TextDecoder ${textCodecSource.TextDecoder}, TextEncoder ${textCodecSource.TextEncoder}, ` +
      `fetch ${kind('fetch')}, document ${kind('document')}, URL ${kind('URL')}, ` +
      `console.error ${nativeConsole?.error ?? '?'}, console.warn ${nativeConsole?.warn ?? '?'}, ` +
      `atob ${kind('atob')}, Buffer ${kind('Buffer')}`;
  },
};

const glue: Glue = {
  init: (module) => {
    initSync({ module });
  },
  reset: __wbg_reset_state,
  loop: wasmLoop,
  trapTest: BUILD.features.includes('trap-test') ? wasmTrapTest : undefined,
  snapshotSummary: wasmSnapshotSummary,
  schedulerSummary: wasmSchedulerSummary,
  actSummary: wasmActSummary,
  profilerSummary: wasmProfilerSummary,
  queueSay: wasmQueueSay,
  persistSummary: wasmPersistSummary,
  persistTest: wasmPersistTest,
  configSummary: wasmConfigSummary,
  configSet: wasmConfigSet,
  configUnset: wasmConfigUnset,
  configReset: wasmConfigReset,
  constsSummary: wasmConstsSummary,
  movementSummary: wasmMovementSummary,
  moveTo: wasmMoveTo,
  spawnerSummary: wasmSpawnerSummary,
  spawn: wasmSpawn,
  openingSummary: wasmOpeningSummary,
  plannerSummary: wasmPlannerSummary,
  remoteSummary: wasmRemoteSummary,
  signSummary: wasmSignSummary,
  scoutSummary: wasmScoutSummary,
  claimsSummary: wasmClaimsSummary,
  mineralsSummary: wasmMineralsSummary,
  labsSummary: wasmLabsSummary,
};

const loader = new Loader(env, glue, CONFIG, BUILD);

const routedError = (...args: unknown[]): void => {
  const text = args.map(String).join(' ');
  loader.noteError(text);
  console.log(`[error] ${text}`);
};
const routedWarn = (...args: unknown[]): void => console.log(`[warn] ${args.map(String).join(' ')}`);

/**
 * Screeps builds a new console before every tick, without error or warn (V96). The panic hook writes to
 * console.error and the glue may call console.warn, so both are (re)defined at the start of each tick.
 */
function routeConsole(): void {
  const c = console as unknown as Record<string, unknown>;
  nativeConsole ??= { error: typeof c.error, warn: typeof c.warn };
  try {
    c.error = routedError;
    if (typeof c.warn !== 'function') c.warn = routedWarn;
  } catch {
    // A console that does not take new properties keeps its own behavior.
  }
}

// Console commands, for example `rustedscreep.trapTest(10)`, `rustedscreep.status()`, `rustedscreep.snapshot()`,
// `rustedscreep.scheduler()`, `rustedscreep.act()`, `rustedscreep.profiler()`, `rustedscreep.say("hi")`,
// `rustedscreep.persist()`, `rustedscreep.persistTest()`, `rustedscreep.config()`,
// `rustedscreep.configSet("log.level", "debug")`, `rustedscreep.configUnset("log.level")`,
// `rustedscreep.configReset()`, `rustedscreep.consts()`, `rustedscreep.movement()`,
// `rustedscreep.moveTo("W1N1", 25, 25, 1)` (from another room: `rustedscreep.moveTo("W2N1", 25, 25, 1, "W1N1")`;
// `rustedscreep.moveTo()` cancels), `rustedscreep.spawner()` or `rustedscreep.spawn("W1N1", "worker")`
// (`rustedscreep.spawn()` cancels), `rustedscreep.opening()`, `rustedscreep.planner()`, `rustedscreep.remote()`,
// `rustedscreep.sign()`, `rustedscreep.scout()`, `rustedscreep.claims()`, `rustedscreep.minerals()`,
// `rustedscreep.labs()`.
g.rustedscreep = {
  trapTest: (n: unknown = 1) => loader.scheduleTraps(Number(n)),
  status: () => loader.status(),
  snapshot: () => loader.snapshot(),
  scheduler: () => loader.scheduler(),
  act: () => loader.act(),
  profiler: () => loader.profiler(),
  say: (text: unknown) => loader.say(text),
  persist: () => loader.persist(),
  persistTest: (bytes?: unknown) => loader.persistTest(bytes),
  config: () => loader.runtimeConfig(),
  configSet: (key: unknown, value: unknown) => loader.configSet(key, value),
  configUnset: (key: unknown) => loader.configUnset(key),
  configReset: () => loader.configReset(),
  consts: () => loader.consts(),
  movement: () => loader.movement(),
  moveTo: (room?: unknown, x?: unknown, y?: unknown, range?: unknown, from?: unknown) =>
    loader.moveTo(room, x, y, range, from),
  spawner: () => loader.spawner(),
  spawn: (room?: unknown, role?: unknown) => loader.spawn(room, role),
  opening: () => loader.opening(),
  planner: () => loader.planner(),
  remote: () => loader.remote(),
  sign: () => loader.sign(),
  scout: () => loader.scout(),
  claims: () => loader.claims(),
  minerals: () => loader.minerals(),
  labs: () => loader.labs(),
};

export function loop(): void {
  try {
    routeConsole();
    loader.tick();
  } catch (e) {
    // The loader catches the wasm's own errors. This only keeps a loader bug from escaping loop, which
    // would also skip the tick's console commands (V96).
    console.log(`rustedscreep loader error: ${e instanceof Error ? (e.stack ?? e.message) : String(e)}`);
  }
}
