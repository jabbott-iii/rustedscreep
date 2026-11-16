// End-to-end runtime-config test in the Screeps-like sandbox (js/tools/smoke.mjs) on the plain build in dist/
// (ROADMAP P1 module 10). The overrides (crates/rustedscreep-core/src/config/overrides.rs, glue in
// crates/rustedscreep-wasm/src/config.rs) live in segment CONFIG_SEGMENT across four globals (each new one
// started with a console Game.cpu.halt()), and the script checks that:
// - a change before the record has loaded is refused; after it loaded, a change takes effect on the next tick
//   (the scheduler's ceiling follows scheduler.tick_reserve_pct) and is written on the next tick, and only a
//   change that changes something is written;
// - a number outside the safe range is clamped, and an unknown key, a profile-only field, a value of the wrong
//   kind and a change that breaks a rule are refused and change nothing;
// - the record is an ADR-004 frame (kind 2, schema 1, the write tick, CRC-32) whose payload is the key=value
//   lines in field order, and the next global loads it, logs it and applies it;
// - log.level filters: at "warn" the self-test's passing verdict (info) is not logged, though
//   rustedscreep.persist() shows it, and "error" (below the lowest level) is refused;
// - cache.structure_static=false reads every structure's type every tick (the naive baseline), and true
//   reads it only when a structure is first seen;
// - a corrupted record is refused with its cause, the profile's values apply, and the next change writes a new
//   record; clearing the segment from the console (RawMemory.segments[97] = "") leaves the next global with none;
// - the naive guard (ADR-022): a record's span is not ended on the loop that loads it, though that loop's bucket
//   is below cache.naive_stop_bucket (the guard reads the loop's config); with cache.naive_max_ticks at 3, the
//   three caches turned off in one console entry run off for exactly 3 loops, then the bot removes those
//   overrides itself (one info line) and writes the record that tick; turned off again, a bucket below the stop
//   level and the Standard gate ends the span on its first loop (one warning) and the record waits for the next
//   Standard tick; the other override stays both times;
// - nothing traps, faults or raises an incident (checked on the fault and JS-failure counters, which do not
//   depend on the log level), and the only halts are the three console ones.
// npm run check runs it (row 14) after the build; standalone, run npm run build first, since it uses
// whatever dist/ holds: node js/tools/config-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { crc32 } from './lib/crc32.mjs';
import { sceneAt } from './lib/world.mjs';
import { CONFIG_SEGMENT, commandResults, driverRawMemory, simulate } from './smoke.mjs';

const T0 = 50000000;
/** Frame layout (rustedscreep-core persist/frame.rs, ADR-004) and the config record (persist::kind::CONFIG, overrides::SCHEMA). */
const TAG = 'rs1b';
const KIND_CONFIG = 2;
const SCHEMA = 1;
/** tickLimit 500 in the sandbox; the ceiling is tickLimit × (100 − tick_reserve_pct) / 100 (SCHEDULER_MMO, budget.rs). */
const ceiling = (pct) => ((500 * (100 - pct)) / 100).toFixed(1);
const set = (key, value) => `rustedscreep.configSet(${JSON.stringify(key)}, ${JSON.stringify(value)})`;
const SHOW = 'rustedscreep.config()';
const HALT = 'Game.cpu.halt()';
/** The three cache toggles off in one console entry (the naive baseline, TEST_PLAN §4). */
const NAIVE = "['cache.structure_static', 'cache.movement_grid', 'cache.flow_fields'].map((k) => rustedscreep.configSet(k, 'false')).join('\\n')";
const NAIVE_ENDS = '; the naive baseline ends by itself after 3 ticks (cache.naive_max_ticks), or at once below bucket 8000 (cache.naive_stop_bucket)';

// The script, by offset (a new global compiles on the tick after a halt and runs its first loop a tick later;
// the record loads on the loop after that).
const COMMANDS = {
  1: set('scheduler.tick_reserve_pct', 12), // refused: the record is still loading
  2: SHOW,
  3: set('scheduler.tick_reserve_pct', 12),
  4: 'rustedscreep.scheduler()',
  5: set('scheduler.tick_reserve_pct', 999), // clamped to 50
  6: set('scheduler.nope', 1),
  7: set('loader.boot_bucket_min', 10),
  8: set('scheduler.standard_bucket_min', 9500), // above the Optional gate
  9: set('log.level', 'error'), // below the lowest level, warn
  10: set('cache.structure_static', false),
  11: set('log.level', 'warn'),
  12: 'rustedscreep.persistTest(10)', // writes on 13, reads on 14
  13: SHOW,
  14: 'rustedscreep.persist()',
  15: set('scheduler.tick_reserve_pct', 50), // the same value: nothing to write
  16: 'rustedscreep.scheduler()',
  17: HALT,
  // Global 2: compiles on 18, first loop 19, the record loads on 20.
  20: SHOW,
  21: 'rustedscreep.scheduler()',
  22: 'rustedscreep.configReset()',
  23: SHOW,
  24: HALT, // the record is corrupted at the start of this tick
  // Global 3: compiles on 25, first loop 26, the corrupted record is refused on 27.
  27: SHOW,
  28: set('cache.structure_static', false),
  29: SHOW,
  30: `RawMemory.segments[${CONFIG_SEGMENT}] = ""`,
  31: HALT,
  // Global 4: compiles on 32, first loop 33, the empty segment loads on 34.
  34: SHOW,
  35: set('cache.naive_max_ticks', 3),
  36: NAIVE, // naive loops 37, 38 and 39; the guard ends the span on 39
  38: SHOW,
  40: SHOW,
  41: NAIVE, // the bucket on 42 is below the stop level and the Standard gate
  43: SHOW,
  44: 'rustedscreep.configReset()',
};
const CORRUPT_AT = 24;
const TICKS = 46;
/**
 * The bucket per offset: 10000, except two ticks below CACHE_MMO.naive_stop_bucket (8000). On LOAD_LOOP (global 2's
 * record load, which runs on the profile's values) it is above the Standard gate so the record still loads, and the
 * guard must not end the loaded span there (it runs with the loop's config copy, ADR-022). On LOW_BUCKET_AT it is
 * below the Standard gate (2000): the guard still ends the span (it runs whatever the tier), and the record waits
 * for the next Standard tick.
 */
const LOAD_LOOP = 20;
const LOAD_BUCKET = 5000;
const LOW_BUCKET_AT = 42;
const LOW_BUCKET = 1500;
const bucketAt = (offset) => (offset === LOAD_LOOP ? LOAD_BUCKET : offset === LOW_BUCKET_AT ? LOW_BUCKET : 10000);

/** Replies that must contain these parts, by offset. */
const REPLIES = {
  1: ['not changed: the stored overrides have not loaded yet (they load with the Standard tier)'],
  2: [`record in segment ${CONFIG_SEGMENT}: none stored | overrides 0 | in force: scheduler.tick_reserve_pct=10 `, ' log.level=info ', ' cache.structure_static=true'],
  3: ['scheduler.tick_reserve_pct = 12 from the next tick (profile mmo: 10); stored on the next tick that runs the Standard tier'],
  4: [`ceiling ${ceiling(12)}`, '| faults since boot 0 '],
  5: ['scheduler.tick_reserve_pct = 50 from the next tick (profile mmo: 10; override was 12); 999 is outside the safe range 5 to 50, so it was clamped; stored'],
  6: ["not changed: no config field is named 'scheduler.nope'"],
  7: ['not changed: loader.boot_bucket_min cannot be overridden at runtime'],
  8: ['not changed: scheduler.standard_bucket_min refused: scheduler.standard_bucket_min must be at most scheduler.optional_bucket_min'],
  9: ["not changed: log.level: 'error' is not one of: warn, info, debug or trace"],
  10: ['cache.structure_static = false from the next tick (profile mmo: true); stored'],
  11: ['log.level = warn from the next tick (profile mmo: info); stored'],
  13: [
    `record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 12} by this global | overrides 3: `
      + 'scheduler.tick_reserve_pct=50 (profile 10), log.level=warn (profile info), cache.structure_static=false (profile true) | in force:',
  ],
  14: ['self-test: round trip ok: 10 B', '| JS failures 0 |'],
  15: ['scheduler.tick_reserve_pct = 50 from the next tick (profile mmo: 10; override was 50)'],
  16: [`ceiling ${ceiling(50)}`, '| faults since boot 0 '],
  20: [`record in segment ${CONFIG_SEGMENT}: loaded (written at tick ${T0 + 12}) | overrides 3: scheduler.tick_reserve_pct=50 (profile 10)`],
  21: [`ceiling ${ceiling(50)}`, '| faults since boot 0 '],
  22: ['3 override(s) removed; the profile\'s values apply from the next tick'],
  23: [`record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 23} by this global | overrides 0 | in force: scheduler.tick_reserve_pct=10 `],
  27: [`record in segment ${CONFIG_SEGMENT}: not used (CRC-32 `, '| overrides 0 |'],
  28: ['cache.structure_static = false from the next tick (profile mmo: true); stored'],
  29: [`record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 29} by this global | overrides 1: cache.structure_static=false (profile true)`],
  34: [`record in segment ${CONFIG_SEGMENT}: none stored | overrides 0 |`, ' cache.structure_static=true'],
  35: ['cache.naive_max_ticks = 3 from the next tick (profile mmo: 1000); stored'],
  36: [
    `cache.structure_static = false from the next tick (profile mmo: true); stored on the next tick that runs the Standard tier${NAIVE_ENDS}`,
    `cache.movement_grid = false from the next tick (profile mmo: true); stored on the next tick that runs the Standard tier${NAIVE_ENDS}`,
    `cache.flow_fields = false from the next tick (profile mmo: true); stored on the next tick that runs the Standard tier${NAIVE_ENDS}`,
  ],
  38: [
    `record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 37} by this global | overrides 4: cache.structure_static=false (profile true)`,
    ` | naive baseline since tick ${T0 + 37}, ends after 3 ticks or below bucket 8000`,
  ],
  40: [`record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 39} by this global | overrides 1: cache.naive_max_ticks=3 (profile 1000) | in force:`],
  41: [`cache.flow_fields = false from the next tick (profile mmo: true); stored on the next tick that runs the Standard tier${NAIVE_ENDS}`],
  43: [`record in segment ${CONFIG_SEGMENT}: written at tick ${T0 + 43} by this global | overrides 1: cache.naive_max_ticks=3 (profile 1000) | in force:`],
  44: ['1 override(s) removed'],
};

/**
 * `structureType` reads per offset (sceneAt(1) every tick: a spawn and a controller). With the cache on a
 * structure's type is read when a global first sees it; with it off, on every loop. The override applies
 * on the loop after the console change, or after the loop that loaded the record.
 */
const S = 2;
const READS = {
  1: S, ...Object.fromEntries([11, 12, 13, 14, 15, 16, 17].map((o) => [o, S])),
  19: S, 21: S, 22: S, // global 2: first sight, then the loaded override until the reset (C 22)
  26: S, 29: S, 30: S, 31: S, // global 3: first sight, then the override set at C 28
  33: S, // global 4: first sight
  37: S, 38: S, 39: S, // the naive span the guard ends after 3 loops
  42: S, // the span the low bucket ends on its first loop
};

/** Offsets whose end-of-tick save includes the config segment: writes, re-saves of a loaded one, the console's "". */
const CONFIG_SAVES = [2, 4, 6, 11, 12, 20, 23, 27, 29, 30, 34, 36, 37, 39, 43, 45];

/** Decodes a frame's header and payload (persist/frame.rs). */
function decodeFrame(text) {
  if (!text.startsWith(TAG)) return null;
  const bytes = Buffer.from(text.slice(TAG.length), 'base64');
  const kind = bytes.readUInt16LE(0);
  const schema = bytes.readUInt16LE(2);
  const written = bytes.readUInt32LE(4);
  const length = bytes.readUInt32LE(8);
  const crc = bytes.readUInt32LE(12);
  const payload = bytes.subarray(16);
  const computed = crc32(Buffer.concat([bytes.subarray(0, 12), payload]));
  return { kind, schema, written, length, crcOk: crc === computed, payload: payload.toString('latin1') };
}

/** Runs the script on the build in `dir` and checks every expectation. */
export function configSim(dir = join(ROOT, 'dist')) {
  const problems = [];
  const reads = {};
  const stored = {};
  let corrupted = null;
  const world = simulate(dir, {
    ticks: TICKS,
    bucket: bucketAt,
    command: (w) => COMMANDS[w.ticksRun] ?? null,
    scene: (offset) => {
      const scene = sceneAt(1);
      for (const [id, s] of Object.entries(scene.structures)) {
        const { structureType } = s;
        const counted = { ...s };
        Object.defineProperty(counted, 'structureType', {
          get: () => { reads[offset] = (reads[offset] ?? 0) + 1; return structureType; },
          enumerable: true,
        });
        scene.structures[id] = counted;
      }
      return scene;
    },
    rawMemory: (offset, w) => {
      // What the config segment held after the previous tick's save.
      stored[offset] = w.segments[CONFIG_SEGMENT];
      if (offset === CORRUPT_AT && typeof w.segments[CONFIG_SEGMENT] === 'string') {
        const t = w.segments[CONFIG_SEGMENT];
        corrupted = t.slice(0, 10) + (t[10] === 'A' ? 'B' : 'A') + t.slice(11);
        w.segments[CONFIG_SEGMENT] = corrupted;
      }
      return driverRawMemory(offset, w);
    },
  });

  // Console replies: the nth time a command appears in the script, its nth reply.
  const replyAt = (offset) => {
    const cmd = COMMANDS[offset];
    const nth = Object.keys(COMMANDS).filter((o) => COMMANDS[o] === cmd && Number(o) <= Number(offset)).length - 1;
    return commandResults(world, cmd)[nth] ?? '';
  };
  for (const [offset, parts] of Object.entries(REPLIES)) {
    const reply = replyAt(offset);
    const missing = parts.filter((p) => !reply.includes(p));
    if (missing.length) problems.push(`offset ${offset} (${COMMANDS[offset]}): lacks ${missing.join(' / ')}: ${reply}`);
  }
  // Refused changes and one that changes nothing are not written.
  for (const offset of [1, 6, 7, 8, 9, 15]) {
    if (replyAt(offset).includes('stored on the next tick')) problems.push(`offset ${offset}: "${replyAt(offset)}" would write a record`);
  }

  // The record global 1 left: the three overrides in field order, written on 12.
  const frame = decodeFrame(stored[18] ?? '');
  const payload = 'scheduler.tick_reserve_pct=50\nlog.level=warn\ncache.structure_static=false\n';
  if (!frame || frame.kind !== KIND_CONFIG || frame.schema !== SCHEMA || frame.written !== T0 + 12
    || frame.length !== payload.length || !frame.crcOk || frame.payload !== payload) {
    problems.push(`stored record after global 1: ${JSON.stringify(frame)} from ${stored[18]}`);
  }
  const reset = decodeFrame(stored[24] ?? '');
  if (!reset || reset.written !== T0 + 23 || reset.payload !== '' || !reset.crcOk) problems.push(`record after the reset: ${JSON.stringify(reset)}`);
  // The halting tick saves nothing, so the next global finds the corrupted text.
  if (!corrupted || corrupted === stored[CORRUPT_AT] || stored[CORRUPT_AT + 1] !== corrupted) problems.push('the record was not corrupted');
  if (stored[34] !== '') problems.push(`the console's clear did not stick: ${JSON.stringify(stored[34])}`);
  // The guard's writes: only the length override is left, after each stop.
  for (const [offset, written] of [[40, T0 + 39], [44, T0 + 43]]) {
    const f = decodeFrame(stored[offset] ?? '');
    if (!f || f.written !== written || f.payload !== 'cache.naive_max_ticks=3\n' || !f.crcOk) problems.push(`record after the guard's stop (offset ${offset}): ${JSON.stringify(f)}`);
  }

  // Saves of the config segment happen only on the expected ticks.
  const saves = world.segmentSaves.filter((s) => s.keys.includes(String(CONFIG_SEGMENT))).map((s) => s.offset);
  if (JSON.stringify(saves) !== JSON.stringify(CONFIG_SAVES)) problems.push(`config segment saved at ${saves.join(',')}, expected ${CONFIG_SAVES.join(',')}`);
  if (world.segmentErrors.length) problems.push(`save errors: ${world.segmentErrors.join(' / ')}`);

  // The structure-type reads follow cache.structure_static.
  for (let offset = 0; offset < TICKS; offset++) {
    if ((reads[offset] ?? 0) !== (READS[offset] ?? 0)) problems.push(`offset ${offset}: ${reads[offset] ?? 0} structureType reads, expected ${READS[offset] ?? 0}`);
  }

  // Log lines: the loaded record once (printed at the profile's level, before the override applies), the
  // refused one once, no passing verdict while the level was "warn".
  const loadedLine = `rustedscreep config: 3 override(s) from segment ${CONFIG_SEGMENT} (written at tick ${T0 + 12}): scheduler.tick_reserve_pct=50 log.level=warn cache.structure_static=false`;
  if (world.logs.filter((l) => l === loadedLine).length !== 1) problems.push(`no single "${loadedLine}"`);
  const refused = world.logs.filter((l) => l.startsWith(`rustedscreep config: the overrides in segment ${CONFIG_SEGMENT} were not used (CRC-32 `));
  if (refused.length !== 1 || !refused[0].endsWith("the profile's values apply, and the next configSet writes a new record")) problems.push(`refused-record lines: ${refused.join(' / ') || 'none'}`);
  const verdicts = world.logs.filter((l) => l.startsWith('rustedscreep persist self-test: round trip ok'));
  if (verdicts.length) problems.push(`a passing verdict was logged at level error: ${verdicts.join(' / ')}`);
  const tail = (since, last) => `ticks ${since} to ${last}: 3 cache override(s) removed, so every cache is on from the next tick; stored this tick if it runs the Standard tier, else on the next one that does`;
  const ended = [
    `rustedscreep config: naive baseline ended after 3 ticks (cache.naive_max_ticks 3), ${tail(T0 + 37, T0 + 39)}`,
    `rustedscreep config: naive baseline ended early at bucket ${LOW_BUCKET} (below cache.naive_stop_bucket 8000) after 1 ticks, ${tail(T0 + 42, T0 + 42)}`,
  ];
  const endLines = world.logs.filter((l) => l.startsWith('rustedscreep config: naive baseline'));
  if (JSON.stringify(endLines) !== JSON.stringify(ended)) problems.push(`naive guard lines: ${endLines.join(' / ') || 'none'}`);
  const notes = world.logs.filter((l) => l.startsWith(`rustedscreep config: record in segment`));
  if (notes.length) problems.push(`unexpected record notes: ${notes.join(' / ')}`);

  const boots = world.logs.filter((l) => l.startsWith('rustedscreep booted in')).length;
  if (boots !== 4 || world.halts !== 3 || world.globals !== 4) problems.push(`boots ${boots}, halts ${world.halts}, globals ${world.globals}: expected 4, 3 and 4`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]|rustedscreep fault/.test(l));
  if (bad.length) problems.push(`unexpected trouble: ${bad.join(' / ')}`);
  if (problems.length) throw new Error(`${problems.join('\n')}\nlast logs:\n${world.logs.slice(-12).join('\n')}`);
  return { ticks: world.ticksRun, globals: world.globals, replies: Object.keys(REPLIES).length, saves: saves.length };
}

if (isMain(import.meta.url)) {
  try {
    const r = configSim(process.argv[2]);
    console.log(`config-sim: ${r.ticks} ticks over ${r.globals} globals, ${r.replies} console replies and ${r.saves} saves of segment ${CONFIG_SEGMENT}, all as expected`);
  } catch (e) {
    fail(`runtime-config sandbox test failed:\n${e.message}`);
  }
}
