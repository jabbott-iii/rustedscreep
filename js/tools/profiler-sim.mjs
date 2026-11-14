// End-to-end profiler test in the Screeps-like sandbox (js/tools/smoke.mjs). It builds the `profiler`
// variant under target/ (the build also runs the plain smoke test on it) and runs it through six windows
// of PROFILER_MMO.window_loops loops (crates/rustedscreep-core/src/config.rs), checking that:
// - the first loop of the global is logged once as the cold loop and kept out of the windows;
// - each window closes after exactly window_loops loops and only then touches RawMemory;
// - the first two writes reach segment 99 with the driver's save rules (one key, a string under the size
//   limit), and the segment parses as JSON with the documented layout (crates/rustedscreep-core/src/profiler/text.rs);
// - the loop makes six getUsed calls, seven when it closes a window, and reads RawMemory.segments only then;
// - a say's two calls appear in the first window's calls and act counters, counted as calls made (the act
//   layer's `issued`) even though the game refuses one of them;
// - each failure the binding guards against (no RawMemory, a segments value that is not an object, a set
//   that throws, a set the object refuses) costs one log line, never a trap, and leaves the segment as it was;
// - rustedscreep.profiler() reports the cold loop, the open window, the say, and the writes;
// - in a second run (keySim, P1.9) the profiler and the segment manager share the 10-key save cap: with 9
//   segments activated from the console, the manager keeps the closing tick's last key for segment 99 and
//   its write waits a tick; with 10, the profiler skips its write and the manager re-issues its set.
// npm run check runs it; standalone: node js/tools/profiler-sim.mjs.
import { join } from 'node:path';
import { build } from './build.mjs';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { checkResults, commandResults, driverRawMemory, simulate } from './smoke.mjs';
import { sceneAt } from './lib/world.mjs';

/** PROFILER_MMO (config.rs): window length, kept windows and segment id. */
const WINDOW = 1000;
const SEGMENT = '99';
/** INTENT_CPU_COST × tick_reserve_pct / (100 − tick_reserve_pct) in µCPU, with 0.2 and SCHEDULER_MMO's 10. */
const CALL_BOUND = Math.floor((0.2 * 1e6 * 10) / 90);
const T0 = 50000000;
/** The first loop runs at offset 1 (the boot is split over two ticks), so window k closes at offset k × WINDOW + 1. */
const closeAt = (k) => k * WINDOW + 1;
const WINDOWS = 6;
const TICKS = closeAt(WINDOWS) + 1;
const SAY_AT = 9;
/** @screeps/common ERR_BUSY: the say's second call is refused, so the act layer counts ok 1 of issued 2. */
const ERR_BUSY = -4;

/** Window 1 and 2 write normally; windows 3 to 6 each meet one failure. */
const FAILURES = [
  { window: 3, why: 'reading RawMemory.segments threw', raw: () => undefined },
  { window: 4, why: 'RawMemory.segments is not an object', raw: () => ({ segments: 42 }) },
  {
    window: 5,
    why: 'writing RawMemory.segments threw',
    raw: () => ({ segments: new Proxy({}, { set() { throw new Error('profiler-sim: scripted set failure'); } }) }),
  },
  { window: 6, why: 'RawMemory.segments refused the write', raw: () => ({ segments: Object.freeze({}) }) },
];

/** The RawMemory global at `offset`: each failure applies to every tick of its window; otherwise the driver model. */
function rawMemoryAt(offset, world) {
  const f = FAILURES.find((x) => offset > closeAt(x.window - 1) && offset <= closeAt(x.window));
  return f ? f.raw() : driverRawMemory(offset, world);
}

/** What rustedscreep.profiler() must show at these offsets. */
const EXPECTED = {
  1: [
    `rustedscreep profiler at tick ${T0 + 1}: loops 1 in this global | open window 0/${WINDOW} loops (empty)`,
    ' | windows closed 0 (kept 0) | segment 99: writes 0, failed 0 | cold loop at tick 50000001: start ',
  ],
  [SAY_AT + 1]: [
    `loops ${SAY_AT + 1} in this global | open window ${SAY_AT}/${WINDOW} loops from tick ${T0 + 2}: `,
    '; calls total 2 (max 2); billed total 0 (max 0)',
    ' CPU over 2 calls in 1 ticks (bound 0.0222)',
    `last loop at tick ${T0 + SAY_AT + 1}: `,
    ', calls 2, billed 0 | windows closed 0',
  ],
  [closeAt(2)]: [
    `loops ${closeAt(2)} in this global | open window 0/${WINDOW} loops (empty)`,
    `windows closed 2 (kept 2), last from tick ${T0 + closeAt(1) + 1} to ${T0 + closeAt(2)}: end mean/p95 `,
    ' | segment 99: writes 2, failed 0, last write ',
  ],
  [closeAt(WINDOWS)]: [
    `windows closed ${WINDOWS} (kept ${WINDOWS})`,
    ' | segment 99: writes 2, failed 4, last write ',
  ],
};

/** Checks the parsed segment written when window 2 closed. */
function checkSegment(seg, problems) {
  const want = {
    format: 'rustedscreep-profiler', v: 1, unit: '1e-6 CPU', written: T0 + closeAt(2), loops: closeAt(2),
    windowLoops: WINDOW, callBound: CALL_BOUND, flushes: 1, flushErrors: 0,
  };
  for (const [k, v] of Object.entries(want)) if (seg[k] !== v) problems.push(`segment ${k}: ${seg[k]}, expected ${v}`);
  const series = ['start', 'clock', 'snapshot', 'decide', 'act', 'end', 'calls', 'billed'];
  if (JSON.stringify(seg.series) !== JSON.stringify(series)) problems.push(`segment series: ${seg.series}`);
  if (JSON.stringify(seg.stats) !== JSON.stringify(['n', 'sum', 'mean', 'p95', 'max', 'bad'])) problems.push(`segment stats: ${seg.stats}`);
  if (!Number.isInteger(seg.lastFlush) || seg.lastFlush < 0) problems.push(`segment lastFlush: ${seg.lastFlush}`);
  if (seg.cold?.time !== T0 + 1 || seg.cold?.limit !== 20 || seg.cold?.bucket !== 10000 || seg.cold?.values?.length !== 8) {
    problems.push(`segment cold: ${JSON.stringify(seg.cold)}`);
  }
  if (!Array.isArray(seg.windows) || seg.windows.length !== 2) {
    problems.push(`segment windows: ${seg.windows?.length}`);
    return;
  }
  seg.windows.forEach((w, i) => {
    const first = T0 + closeAt(i) + 1;
    const head = [w.first, w.last, w.loops, w.gaps, w.limit, ...w.bucket].join(',');
    const wantHead = [first, first + WINDOW - 1, WINDOW, 0, 20, 10000, 10000, 10000].join(',');
    if (head !== wantHead) problems.push(`window ${i + 1}: ${head}, expected ${wantHead}`);
    if (w.stats.length !== 8 || w.stats.some((s) => s.length !== 6)) problems.push(`window ${i + 1} stats shape`);
    w.stats.forEach(([n, sum, mean, p95, max, bad], j) => {
      // The sandbox clock is monotone, so every reading is good.
      if (n !== WINDOW || bad !== 0) problems.push(`window ${i + 1} ${series[j]}: n ${n}, bad ${bad}`);
      if (!(mean <= max && p95 <= max && sum >= max)) problems.push(`window ${i + 1} ${series[j]}: ${[n, sum, mean, p95, max, bad]}`);
    });
    const [calls, billed] = [w.stats[6], w.stats[7]];
    const says = i === 0 ? 2 : 0;
    if (calls[1] !== says || billed[1] !== 0) problems.push(`window ${i + 1}: calls ${calls[1]}, billed ${billed[1]}`);
    if (w.act.ticks !== (says ? 1 : 0) || w.act.calls !== says || !Number.isInteger(w.act.net)) {
      problems.push(`window ${i + 1} act: ${JSON.stringify(w.act)}`);
    }
  });
}

/**
 * A second run of the profiler build (P1.9): the segment manager and the profiler share the driver's
 * 10-key save cap (V119). After the loop before window 1 closes, the console activates 9 segments and
 * asks for a self-test: on the closing tick the manager keeps the last key for segment 99 and its write
 * waits a tick. After the loop before window 2 closes, the console activates 10 segments: the profiler
 * skips its write (no free key) and the manager re-issues its active set on the next tick. No tick saves
 * more than 10 keys.
 */
function keySim(dist, problems) {
  const reserveAt = closeAt(1);
  const refuseAt = closeAt(2);
  const summaryAt = refuseAt + 2;
  const world = simulate(dist, {
    ticks: summaryAt + 1,
    command: (w) => {
      if (w.ticksRun === reserveAt - 1) return 'RawMemory.setActiveSegments([0, 1, 2, 3, 4, 5, 6, 7, 8]); rustedscreep.persistTest(10)';
      if (w.ticksRun === refuseAt - 1) return 'RawMemory.setActiveSegments([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])';
      return w.ticksRun === summaryAt ? 'rustedscreep.persist()' : null;
    },
  });
  const p = (m) => problems.push(`key sim: ${m}`);
  if (world.segmentErrors.length) p(`save errors: ${world.segmentErrors.join(' / ')}`);
  const saved = (offset) => world.segmentSaves.find((x) => x.offset === offset)?.keys ?? [];
  const nine = ['0', '1', '2', '3', '4', '5', '6', '7', '8'];
  if (JSON.stringify(saved(reserveAt)) !== JSON.stringify([...nine, SEGMENT])) p(`closing tick saved ${JSON.stringify(saved(reserveAt))}: the nine loaded keys and 99 expected`);
  if (!saved(reserveAt + 1).includes('98')) p(`the waiting self-test write is not saved a tick later: ${JSON.stringify(saved(reserveAt + 1))}`);
  const ok = `rustedscreep persist self-test: round trip ok: 10 B in 39 units, written at ${T0 + reserveAt + 1}, read at ${T0 + reserveAt + 2}`;
  if (!world.logs.some((l) => l.startsWith(ok))) p(`no verdict "${ok}"`);
  if (JSON.stringify(saved(refuseAt)) !== JSON.stringify([...nine, '9'])) p(`refusing tick saved ${JSON.stringify(saved(refuseAt))}: the ten loaded keys expected`);
  const refusals = world.logs.filter((l) => l.startsWith('rustedscreep profiler: writing stats segment'));
  const want = `rustedscreep profiler: writing stats segment 99 failed at tick ${T0 + refuseAt}: no free save key`;
  if (refusals.length !== 1 || !refusals[0].startsWith(want)) p(`profiler refusals: ${refusals.join(' / ') || 'none'}`);
  const reissue = world.loopCalls[refuseAt + 1]?.setActive ?? [];
  if (JSON.stringify(reissue) !== '[[]]') p(`the manager's set after the refusal: ${JSON.stringify(reissue)}, expected [[]]`);
  if (world.activeSegments.length) p(`segments left active: ${world.activeSegments.join(',')}`);
  const summary = commandResults(world, 'rustedscreep.persist()')[0] ?? '';
  for (const part of ['| writes 1 (0 failed, 0 rejected, 1 ticks short of a key) |', '| JS failures 0 |']) {
    if (!summary.includes(part)) p(`summary lacks "${part}": ${summary}`);
  }
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]|rustedscreep fault/.test(l));
  if (bad.length || world.halts) p(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts})`);
}

export function profilerSim() {
  const dist = join(ROOT, 'target', 'profiler');
  build({ features: ['profiler'], dist });
  let says = 0;
  const actions = { calls: { push() {} }, returns: { say: () => (says++ % 2 === 0 ? 0 : ERR_BUSY) } };
  const world = simulate(dist, {
    ticks: TICKS,
    rawMemory: rawMemoryAt,
    scene: (offset) => sceneAt(offset, actions),
    command: (w) => {
      if (w.ticksRun === SAY_AT) return 'rustedscreep.say("hi")';
      return EXPECTED[w.ticksRun] ? 'rustedscreep.profiler()' : null;
    },
  });
  const problems = [];
  try {
    checkResults(world, 'rustedscreep.profiler()', EXPECTED, 'profiler');
  } catch (e) {
    problems.push(e.message);
  }
  if (says !== 2) problems.push(`say calls: ${says}, expected 2`);
  const closing = new Set(Array.from({ length: WINDOWS }, (_, k) => closeAt(k + 1)));
  const noRawMemory = FAILURES.find((f) => f.raw() === undefined);
  world.loopCalls.forEach((c, offset) => {
    if (offset < 2) return; // the loader also reads the clock on the boot ticks and the first loop
    const reads = closing.has(offset) && offset !== closeAt(noRawMemory.window) ? 1 : 0;
    const want = { getUsed: closing.has(offset) ? 7 : 6, segments: reads };
    if (c.getUsed !== want.getUsed || c.segments !== want.segments) {
      problems.push(`offset ${offset}: loop calls ${JSON.stringify(c)}, expected ${JSON.stringify(want)}`);
    }
  });
  const cold = world.logs.filter((l) => l.startsWith('rustedscreep profiler: first loop'));
  if (cold.length !== 1) problems.push(`cold lines: ${cold.length}`);
  const saves = JSON.stringify(world.segmentSaves);
  const wantSaves = JSON.stringify([closeAt(1), closeAt(2)].map((offset) => ({ offset, keys: [SEGMENT] })));
  if (saves !== wantSaves) problems.push(`segment saves ${saves}, expected ${wantSaves}`);
  if (world.segmentErrors.length) problems.push(`segment errors: ${world.segmentErrors.join(' / ')}`);
  const failures = world.logs.filter((l) => l.startsWith('rustedscreep profiler: writing stats segment'));
  const wantFailures = FAILURES.map((f) => `rustedscreep profiler: writing stats segment 99 failed at tick ${T0 + closeAt(f.window)}: ${f.why}`);
  if (JSON.stringify(failures) !== JSON.stringify(wantFailures)) problems.push(`failure lines:\n  ${failures.join('\n  ')}`);
  let seg = null;
  try {
    seg = JSON.parse(world.segments[SEGMENT]);
  } catch (e) {
    problems.push(`segment 99 is not JSON: ${e.message}`);
  }
  if (seg) checkSegment(seg, problems);
  keySim(dist, problems);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]|rustedscreep fault/.test(l));
  if (bad.length || world.halts || world.globals !== 1) problems.push(`unexpected trouble: ${bad.join(' / ')} (halts ${world.halts}, globals ${world.globals})`);
  if (problems.length) throw new Error(`${problems.join('\n')}\nlast logs:\n${world.logs.slice(-12).join('\n')}`);
  return { ticks: world.ticksRun, bytes: world.segments[SEGMENT].length, failures: failures.length };
}

if (isMain(import.meta.url)) {
  try {
    const r = profilerSim();
    console.log(`profiler: ${r.ticks} ticks, ${WINDOWS} windows, segment ${SEGMENT} ${r.bytes} characters, ${r.failures} guarded write failures; the save-key run passed`);
  } catch (e) {
    fail(`profiler test failed: ${e.message}`);
  }
}
