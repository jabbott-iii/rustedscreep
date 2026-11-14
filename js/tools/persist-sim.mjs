// End-to-end persistence test in the Screeps-like sandbox (js/tools/smoke.mjs) on the plain build in dist/.
// The segment manager (crates/rustedscreep-wasm/src/persist.rs) runs against the driver's segment model
// (driverRawMemory: the active set applies on the next tick and the last call of a tick wins, V116 to V120)
// and the end-of-tick save with its checks (V41, V85), and `rustedscreep.persistTest(bytes)` checks that:
// - a run writes its frame on the loop after the command, asks for the segment, reads it back on the next
//   loop and logs "round trip ok", with setActiveSegments([98]) and then ([]);
// - a full-size run fills the segment to exactly MEMORY_SEGMENT_SIZE_LIMIT units and passes the save checks
//   (twice: the first large crossing of a global is the cold one);
// - each corruption of the stored text between the write and the read (a changed character, a cut, foreign
//   JSON, an empty string, another container version) gives a FAILED verdict that names the cause, with no
//   trap, incident or fault;
// - a console setActiveSegments that replaces the manager's set (console commands run after loop, V118)
//   costs one retry: the set is issued again and the run passes a tick later;
// - ten segments activated from the console leave no save key: the write waits a tick, the set is issued
//   again, the save never overflows, and the run passes;
// - the loader refuses an invalid size and the wasm refuses a second run while one is in progress;
// - a write the segments object refuses (frozen) ends the run with "FAILED ...: the write failed", logged
//   once, instead of retrying every tick;
// - `rustedscreep.persist()` counts what happened.
// It prints the sandbox CPU of both full-size runs (Node, the TextEncoder/TextDecoder polyfill; ADR-004).
// npm run check runs it (row 13) after the build; standalone, run npm run build first, since it uses
// whatever dist/ holds: node js/tools/persist-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { commandResults, driverRawMemory, simulate } from './smoke.mjs';

const T0 = 50000000;
/** PERSIST_MMO.self_test_segment and PROFILER_MMO.stats_segment (config.rs). */
const SEGMENT = 98;
const RESERVED = 99;
/** MEMORY_SEGMENT_SIZE_LIMIT (V41) and the frame layout (rustedscreep-core persist/frame.rs). */
const LIMIT = 100 * 1024;
const TAG = 'rs1b';
const HEADER = 16;
const encodedLen = (n) => Math.floor(n / 3) * 4 + [0, 2, 3][n % 3];
const decodedMax = (c) => Math.floor(c / 4) * 3 + [0, 0, 1, 2][c % 4];
const textUnits = (payload) => TAG.length + encodedLen(HEADER + payload);
const MAX_PAYLOAD = decodedMax(LIMIT - TAG.length) - HEADER;

const SMALL = 1000;
const CORRUPT_SIZE = 200;

/** Changes the stored text at the start of the tick the run reads it back. */
const CORRUPTIONS = [
  { name: 'a changed character', edit: (t) => t.slice(0, 100) + (t[100] === 'A' ? 'B' : 'A') + t.slice(101), cause: /load failed: CRC-32 [0-9a-f]{8} in the header, [0-9a-f]{8} computed/ },
  { name: 'a cut', edit: (t) => t.slice(0, -5), cause: /load failed: (length 200 in the header, \d+ in the body|non-canonical base64 end|base64 body of impossible length)/ },
  { name: 'foreign JSON', edit: () => '{"v":1}', cause: /load failed: not a rustedscreep frame/ },
  { name: 'an empty string', edit: () => '', cause: /load failed: empty \(never written or cleared\)/ },
  { name: 'another container version', edit: (t) => `rs2${t.slice(3)}`, cause: /load failed: unknown container version '2'/ },
];

// The script, by offset (the first loop runs at offset 1). A run asked for after loop k writes at k + 1 and
// reads at k + 2 unless something delays it.
const COMMANDS = {
  2: `rustedscreep.persistTest(${SMALL})`,
  6: 'rustedscreep.persistTest()',
  8: 'rustedscreep.persistTest()',
};
const CORRUPT_AT = {};
// `offset` is the loop that logs the verdict; `wrote` the loop that wrote the frame (default offset - 1).
const VERDICTS = [
  { offset: 4, ok: true, size: SMALL },
  { offset: 8, ok: true, size: MAX_PAYLOAD },
  { offset: 10, ok: true, size: MAX_PAYLOAD },
];
CORRUPTIONS.forEach((c, i) => {
  const at = 10 + 4 * i;
  COMMANDS[at] = `rustedscreep.persistTest(${CORRUPT_SIZE})`;
  CORRUPT_AT[at + 2] = c;
  VERDICTS.push({ offset: at + 2, ok: false, cause: c.cause, name: c.name });
});
const OVERRIDE_AT = 30;
COMMANDS[OVERRIDE_AT] = 'rustedscreep.persistTest(10)';
COMMANDS[OVERRIDE_AT + 1] = 'RawMemory.setActiveSegments([])';
// The override drops the set for one tick: the write is on time, the read a tick late.
VERDICTS.push({ offset: OVERRIDE_AT + 3, wrote: OVERRIDE_AT + 1, ok: true, size: 10 });
const KEYS_AT = 36;
COMMANDS[KEYS_AT] = 'RawMemory.setActiveSegments([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]); rustedscreep.persistTest(10)';
// The write waits a tick for a save key, then the read follows on time.
VERDICTS.push({ offset: KEYS_AT + 3, wrote: KEYS_AT + 2, ok: true, size: 10 });
const BAD_SIZE_CMD = 'rustedscreep.persistTest(-1)';
const BUSY_AT = 42;
COMMANDS[41] = BAD_SIZE_CMD;
COMMANDS[BUSY_AT] = 'rustedscreep.persistTest(5)';
COMMANDS[BUSY_AT + 1] = 'rustedscreep.persistTest(6)';
VERDICTS.push({ offset: BUSY_AT + 2, ok: true, size: 5 });
// The segments object refuses the write (frozen) on the write tick.
const FROZEN_AT = 46;
COMMANDS[FROZEN_AT - 1] = 'rustedscreep.persistTest(10)';
const WRITE_FAILED = /FAILED at \d+: the write failed$/;
VERDICTS.push({ offset: FROZEN_AT, ok: false, cause: WRITE_FAILED, name: 'a refused write', rebuild: false });
const SUMMARY_AT = 47;
COMMANDS[SUMMARY_AT] = 'rustedscreep.persist()';
const TICKS = SUMMARY_AT + 1;

/** Runs the script on the build in `dir` and checks every expectation; returns the full-size run's CPU. */
export function persistSim(dir = join(ROOT, 'dist')) {
  const problems = [];
  const corrupted = [];
  const sizes = {};
  const world = simulate(dir, {
    ticks: TICKS,
    command: (w) => COMMANDS[w.ticksRun] ?? null,
    rawMemory: (offset, w) => {
      const c = CORRUPT_AT[offset];
      if (c && typeof w.segments[SEGMENT] === 'string') {
        w.segments[SEGMENT] = c.edit(w.segments[SEGMENT]);
        corrupted.push(offset);
      }
      if (typeof w.segments[SEGMENT] === 'string') sizes[offset] = w.segments[SEGMENT].length;
      const raw = driverRawMemory(offset, w);
      if (offset === FROZEN_AT) Object.freeze(raw.segments);
      return raw;
    },
  });
  if (corrupted.length !== CORRUPTIONS.length) problems.push(`corrupted at ${corrupted.join(',')}, expected ${CORRUPTIONS.length} edits`);

  // Verdict lines, in order, each at its tick.
  const lines = world.logs.filter((l) => l.startsWith('rustedscreep persist self-test: round trip ok') || l.startsWith('rustedscreep persist self-test: FAILED'));
  if (lines.length !== VERDICTS.length) problems.push(`${lines.length} verdicts, expected ${VERDICTS.length}: ${lines.join(' / ')}`);
  VERDICTS.forEach((v, i) => {
    const line = lines[i] ?? '';
    const tick = T0 + v.offset;
    if (v.ok) {
      const want = `round trip ok: ${v.size} B in ${textUnits(v.size)} units, written at ${T0 + (v.wrote ?? v.offset - 1)}, read at ${tick}; µCPU encode `;
      if (!line.includes(want)) problems.push(`verdict ${i}: expected "${want}", got "${line}"`);
    } else if (!line.includes(`FAILED at ${tick}: `) || !v.cause.test(line)
      || line.endsWith('(an owner would rebuild from game state)') !== (v.rebuild ?? true)) {
      problems.push(`verdict ${i} (${v.name}): expected a failure at ${tick} matching ${v.cause}, got "${line}"`);
    }
  });

  // The first run: one write, the set on and off around the read.
  const active = world.loopCalls.flatMap((c, offset) => c.setActive.map((ids) => [offset, ids]));
  const want = [[1, []], [3, [SEGMENT]], [4, []]];
  if (JSON.stringify(active.slice(0, 3)) !== JSON.stringify(want)) problems.push(`first setActiveSegments calls ${JSON.stringify(active.slice(0, 3))}, expected ${JSON.stringify(want)}`);
  if (sizes[4] !== textUnits(SMALL)) problems.push(`the small run stored ${sizes[4]} units, expected ${textUnits(SMALL)}`);
  if (sizes[8] !== LIMIT || sizes[10] !== LIMIT || textUnits(MAX_PAYLOAD) !== LIMIT) problems.push(`the full runs stored ${sizes[8]} and ${sizes[10]} units, expected the limit ${LIMIT}`);
  if (active.some(([, ids]) => ids.includes(RESERVED))) problems.push('the manager activated the profiler segment');
  if (world.activeSegments.length) problems.push(`segments left active: ${world.activeSegments.join(',')}`);
  // The key-budget run: the write waits while the console's ten segments are loaded.
  const keysSave = world.segmentSaves.find((s) => s.offset === KEYS_AT + 1);
  if (!keysSave || keysSave.keys.length !== 10 || keysSave.keys.includes(String(SEGMENT))) problems.push(`the tick with ten loaded segments saved ${JSON.stringify(keysSave)}`);
  if (!world.segmentSaves.some((s) => s.offset === KEYS_AT + 2 && s.keys.includes(String(SEGMENT)))) problems.push('the waiting write did not go out a tick later');
  if (world.segmentErrors.length) problems.push(`save errors: ${world.segmentErrors.join(' / ')}`);

  // Console results.
  const refused = commandResults(world, BAD_SIZE_CMD);
  if (refused.length !== 1 || !refused[0].includes('give the payload size as a whole number of bytes')) problems.push(`bad size: ${refused.join(' / ')}`);
  const busy = commandResults(world, COMMANDS[BUSY_AT + 1]);
  if (busy.length !== 1 || !busy[0].includes('a run is in progress')) problems.push(`second run: ${busy.join(' / ')}`);
  const started = commandResults(world, 'rustedscreep.persistTest()');
  if (started.length !== 2 || !started.every((l) => l.includes(`${MAX_PAYLOAD} B into segment ${SEGMENT} on the next tick`))) problems.push(`full runs: ${started.join(' / ')}`);
  const summary = commandResults(world, 'rustedscreep.persist()')[0] ?? '';
  const activations = active.length;
  const runs = VERDICTS.length;
  const failedLoads = CORRUPTIONS.length;
  // Every run but the refused write wrote; every run that wrote also loaded.
  const written = runs - 1;
  for (const part of [
    `active [] (set at ${T0 + BUSY_AT + 2})`,
    `| wanted [] | dirty [] | reserved [${RESERVED}] |`,
    `| loads ${written - failedLoads} (${failedLoads} failed, 1 retries) |`,
    `| writes ${written} (0 failed, 1 rejected, 1 ticks short of a key) |`,
    `| activations ${activations} (0 failed) | JS failures 1 |`,
    `| self-test: FAILED at ${T0 + FROZEN_AT}: the write failed`,
  ]) {
    if (!summary.includes(part)) problems.push(`summary lacks "${part}": ${summary}`);
  }
  const refusedWrite = `rustedscreep persist: writing a segment failed at tick ${T0 + FROZEN_AT}: the object refused the property (later failures of this kind are counted, not logged)`;
  const jsFailures = world.logs.filter((l) => /rustedscreep persist: .* failed at tick/.test(l));
  if (JSON.stringify(jsFailures) !== JSON.stringify([refusedWrite])) problems.push(`JS failure lines: ${jsFailures.join(' / ') || 'none'}, expected "${refusedWrite}"`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]|rustedscreep fault/.test(l));
  if (bad.length || world.halts) problems.push(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts})`);
  if (problems.length) throw new Error(`${problems.join('\n')}\nlast logs:\n${world.logs.slice(-12).join('\n')}`);
  const costs = (line) => (line ?? '').match(/µCPU encode (\S+), set (\S+), get (\S+), decode (\S+)$/)?.slice(1) ?? [];
  return { ticks: world.ticksRun, runs, activations, cold: costs(lines[1]), warm: costs(lines[2]), lines };
}

if (isMain(import.meta.url)) {
  try {
    const r = persistSim(process.argv[2]);
    console.log(`persist-sim: ${r.ticks} ticks, ${r.runs} self-test runs (${CORRUPTIONS.length} on corrupted segments, 1 refused write), ${r.activations} setActiveSegments calls, all as expected`);
    const cpu = (u) => (Number.isFinite(Number(u)) ? (Number(u) / 1e6).toFixed(3) : u);
    for (const [name, c] of [['cold', r.cold], ['warm', r.warm]]) {
      console.log(`  full segment (${MAX_PAYLOAD} B in ${LIMIT} units), ${name}, sandbox CPU (ms): encode ${cpu(c[0])}, set ${cpu(c[1])}, get ${cpu(c[2])}, decode and compare ${cpu(c[3])}`);
    }
    if (process.env.PERSIST_SIM_VERBOSE) for (const l of r.lines) console.log(`  ${l}`);
  } catch (e) {
    fail(`persistence sandbox test failed:\n${e.message}`);
  }
}
