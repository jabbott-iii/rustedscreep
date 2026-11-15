// End-to-end test of the constants self-check (ROADMAP P1 module 11) in the Screeps-like sandbox
// (js/tools/smoke.mjs) on the plain build in dist/. The check (crates/rustedscreep-core/src/consts_check, glue in
// crates/rustedscreep-wasm/src/consts_check.rs) reads every upper-case own property of the global object once per
// global and compares it with the crate's constants. The sandbox sets the engine's constants (common 2.16.0, the
// fixture) on its global every tick, as the engine does (V129). The script checks that:
// - the check waits for the Optional tier (not on a global's first loop, not below the bucket gate), then reads
//   the names once and each upper-case value through its property descriptor: one getOwnPropertyDescriptor and
//   one JSON.stringify per name in all, and a getter is never called; a value JSON.stringify throws on (a cycle)
//   is reported as unreadable and the check goes on; once it is done it calls nothing;
// - every clock reading inside the loop is a scheduler sample (the loop's getUsed calls equal the samples
//   rustedscreep.scheduler() reports for that tick), so the check's readings are counted and bounded;
// - drift is logged once per global: a warning with the counts and one line per finding up to log.consts_findings,
//   which a runtime override lowers (to 0 here: the next global prints only the counts and how many it held back);
// - a global without game constants fails the check once, with a fault line, and it is not retried;
// - below the Optional gate the check waits with no JS call, and the second loop's only readings are the config
//   record load's two samples;
// - rustedscreep.consts() reports each phase, the findings and the known differences with their reasons.
// The sandbox clock is wall time, so the check may take more than one tick on a loaded machine: the script reads
// the ticks from the check's own line and never assumes them. npm run check runs it after the build; standalone,
// run npm run build first, since it uses whatever dist/ holds: node js/tools/consts-sim.mjs [dir].
import { join } from 'node:path';
import vm from 'node:vm';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { ENGINE_CONSTANTS, commandResults, simulate } from './smoke.mjs';

const T0 = 50000000;
const SHOW = 'rustedscreep.consts()';
const SCHED = 'rustedscreep.scheduler()';
/** Both in one console command, so every tick can report its samples and the check's phase. */
const BOTH = `[${SCHED}, ${SHOW}].join(" || ")`;
const HALT = 'Game.cpu.halt()';
/** The check's completion line, either form: [whole, tick, ticks]. */
const DONE = /^rustedscreep consts: (?:checked|\d+ finding\(s\)) at tick (\d+) \((\d+) tick\(s\), /;

/** The fixture's value of `name`. */
function engine(name) {
  const found = ENGINE_CONSTANTS.find(([n]) => n === name);
  if (!found) throw new Error(`the fixture has no ${name}`);
  return JSON.parse(found[1]);
}

/** The fixture with `changes` applied: name → JSON text (added if new). */
function constantsWith(changes) {
  const out = ENGINE_CONSTANTS.map(([n, j]) => [n, n in changes ? changes[n] : j]);
  for (const [n, j] of Object.entries(changes)) if (!out.some(([m]) => m === n)) out.push([n, j]);
  return out;
}

/**
 * Counts, by offset, the calls made to the realm's Object.getOwnPropertyNames, Object.getOwnPropertyDescriptor
 * and JSON.stringify (the loader calls none of them), and adds an accessor and a cyclic value under upper-case
 * names when `traps` is set.
 */
function instrument(counts, traps) {
  return (context, world) => {
    // The realm's own built-ins, which the bundle's glue resolves (the sandbox object does not carry them).
    const O = vm.runInContext('Object', context);
    const J = vm.runInContext('JSON', context);
    const bump = (key) => {
      const at = (counts[world.ticksRun] ??= { names: 0, descriptors: 0, stringify: 0 });
      at[key] += 1;
    };
    const names = O.getOwnPropertyNames;
    const descriptor = O.getOwnPropertyDescriptor;
    const stringify = J.stringify;
    O.getOwnPropertyNames = function (...a) { bump('names'); return names.apply(this, a); };
    O.getOwnPropertyDescriptor = function (...a) { bump('descriptors'); return descriptor.apply(this, a); };
    J.stringify = function (...a) { bump('stringify'); return stringify.apply(this, a); };
    if (traps) {
      context.countGetterCall = () => { counts.getterCalls = (counts.getterCalls ?? 0) + 1; };
      vm.runInContext(`
        Object.defineProperty(globalThis, 'SNEAKY_GETTER', {
          get() { countGetterCall(); return 1; }, enumerable: true, configurable: true,
        });
        globalThis.CYCLIC_VALUE = {};
        globalThis.CYCLIC_VALUE.self = globalThis.CYCLIC_VALUE;`, context);
    }
  };
}

function expectParts(text, parts, label) {
  const missing = parts.filter((p) => !text.includes(p));
  if (missing.length) throw new Error(`${label} lacks ${missing.map((m) => JSON.stringify(m)).join(' / ')}: ${text}`);
}

/** The check's log lines (console replies, which follow their `> command` line, excluded). */
function constsLines(world) {
  return world.logs.filter((l, i) => l.startsWith('rustedscreep consts:') && !world.logs[i - 1]?.startsWith('> '));
}

/** The completion lines with their tick offset and tick count. */
function completions(world) {
  return constsLines(world).flatMap((l) => {
    const m = DONE.exec(l);
    return m ? [{ line: l, offset: Number(m[1]) - T0, ticks: Number(m[2]) }] : [];
  });
}

function noTrouble(world, label, { faults = 0, halts = 0 } = {}) {
  const faultLines = world.logs.filter((l) => l.startsWith('rustedscreep fault'));
  if (faultLines.length !== faults) throw new Error(`${label}: ${faultLines.length} fault line(s), expected ${faults}: ${faultLines.join(' / ')}`);
  const bad = world.logs.filter((l) => /incident|loader error|\[error\]/.test(l));
  if (bad.length || world.halts !== halts) throw new Error(`${label}: trouble ${bad.join(' / ')} (halts ${world.halts}, expected ${halts})`);
  return faultLines;
}

/** The sum of the instrumented counts over offsets from..to. */
function callsBetween(counts, from, to) {
  const sum = { names: 0, descriptors: 0, stringify: 0 };
  for (let o = from; o <= to; o++) for (const k of Object.keys(sum)) sum[k] += counts[o]?.[k] ?? 0;
  return sum;
}

/** Every reading inside the loop is a scheduler sample: getUsed calls equal the tick's reported samples. */
function samplesMatch(world, label) {
  const replies = commandResults(world, BOTH);
  // BOTH runs after every tick's loop from offset 1; replies[i] belongs to offset i + 1.
  replies.forEach((reply, i) => {
    const offset = i + 1;
    if (offset < 2) return; // the loader also reads the clock on the boot ticks and the first loop
    const samples = Number(/ \| samples (\d+) \(bad/.exec(reply)?.[1] ?? NaN);
    const used = world.loopCalls[offset]?.getUsed;
    if (used !== samples) throw new Error(`${label}: offset ${offset}: ${used} getUsed calls in the loop, the scheduler counted ${samples} samples: ${reply}`);
  });
  return replies;
}

/** A: the engine's constants plus an accessor and a cyclic value; the read path and the summary by phase. */
function readPath(dir) {
  const counts = {};
  const world = simulate(dir, {
    ticks: 12,
    command: (w) => (w.ticksRun >= 1 ? BOTH : null),
    until: (w) => completions(w).length === 1 && w.ticksRun >= completions(w)[0].offset + 2,
    setup: instrument(counts, true),
  });
  noTrouble(world, 'read path');
  const [done] = completions(world);
  if (!done) throw new Error(`read path: the check did not complete in ${world.ticksRun} ticks: ${constsLines(world).join(' / ')}`);
  const replies = samplesMatch(world, 'read path');
  expectParts(replies[0], ['rustedscreep consts: not run yet in this global'], 'consts() on the first loop');
  // The global's upper-case names: the constants set each tick, the two traps and the built-in JSON.
  const upper = ENGINE_CONSTANTS.length + 3;
  const during = callsBetween(counts, 2, done.offset);
  if (during.names !== 1 || during.descriptors !== upper || during.stringify !== upper + 1) {
    throw new Error(`read path: calls from offset 2 to ${done.offset} ${JSON.stringify(during)}, expected names 1, descriptors ${upper}, stringify ${upper + 1}`);
  }
  const before = callsBetween(counts, 0, 1);
  const after = callsBetween(counts, done.offset + 1, world.ticksRun);
  if ([before, after].some((c) => c.names || c.descriptors || c.stringify)) {
    throw new Error(`read path: calls outside the check's ticks: before ${JSON.stringify(before)}, after ${JSON.stringify(after)}`);
  }
  if (done.ticks < 1 || done.ticks > done.offset - 1) throw new Error(`read path: ${done.ticks} tick(s) ending at offset ${done.offset}`);
  if (counts.getterCalls) throw new Error(`read path: the getter ran ${counts.getterCalls} time(s)`);
  const lines = constsLines(world);
  if (lines.length !== 3) throw new Error(`read path: ${lines.length} consts line(s), expected 3: ${lines.join(' / ')}`);
  expectParts(lines[0], [`rustedscreep consts: 2 finding(s) at tick ${T0 + done.offset} (`, ` µCPU): ${upper} server names, `,
    ', 0 mismatched, 0 missing on the server, 0 not in the crate, 0 unreadable, 2 unknown constants; '], 'read path summary');
  if (lines[1] !== 'rustedscreep consts: CYCLIC_VALUE (unknown constant): server no JSON value'
    || lines[2] !== 'rustedscreep consts: SNEAKY_GETTER (unknown constant): server no JSON value') {
    throw new Error(`read path findings: ${lines.slice(1).join(' / ')}`);
  }
  expectParts(replies.at(-1), [
    `rustedscreep consts: checked at tick ${T0 + done.offset} (${done.ticks} tick(s) from tick `,
    ' | findings: CYCLIC_VALUE (unknown constant): server no JSON value; SNEAKY_GETTER (unknown constant): server no JSON value;',
    // Known differences, in the order the checks first met them.
    ' | known: ERR_ACCESS_DENIED (the engine added it in common 49c0fc6',
    ' JSON (the JavaScript built-in, not a game constant); ',
    ' PWR_DISRUPT_SOURCE (the crate\'s PowerType skips 11',
    ' LOOK_CREEPS (the crate\'s Look strings are private',
  ], 'consts() when done');
  return { upper, summary: lines[0] };
}

/** B: drift, logged in full by one global and as counts only by the next after log.consts_findings = 0. */
function drift(dir) {
  const life = engine('CREEP_LIFE_TIME');
  const cost = engine('BODYPART_COST');
  delete cost.work;
  const constants = constantsWith({
    CREEP_LIFE_TIME: String(life - 100),
    BODYPART_COST: JSON.stringify(cost),
    BRAND_NEW_CONSTANT: '7',
  });
  const SET = 'rustedscreep.configSet("log.consts_findings", 0)';
  // Global 1 completes the check; then the override is set, written on the next tick, and a halt starts global 2,
  // which loads the record on its second loop and checks with it.
  let setAt = null;
  let halted = false;
  const command = (w) => {
    const done = completions(w).length;
    if (done === 1 && setAt === null) { setAt = w.ticksRun; return SET; }
    if (done === 1 && !halted && w.ticksRun >= setAt + 2) { halted = true; return HALT; }
    if (done === 2) return SHOW;
    return null;
  };
  const world = simulate(dir, { ticks: 24, constants, command, until: (w) => commandResults(w, SHOW).length === 1 });
  noTrouble(world, 'drift', { halts: 1 });
  const [first, second] = completions(world);
  if (!first || !second) throw new Error(`drift: completions ${JSON.stringify(completions(world))}`);
  const lines = constsLines(world);
  const want = [
    `rustedscreep consts: 3 finding(s) at tick ${T0 + first.offset} `,
    `rustedscreep consts: CREEP_LIFE_TIME (mismatch): crate ${life}, server ${life - 100}`,
    `rustedscreep consts: BODYPART_COST.work (missing on the server): crate ${engine('BODYPART_COST').work}`,
    'rustedscreep consts: BRAND_NEW_CONSTANT (unknown constant): server 7',
    `rustedscreep consts: 3 finding(s) at tick ${T0 + second.offset} `,
    'rustedscreep consts: 3 more finding(s); rustedscreep.consts() lists the first 3',
  ];
  if (lines.length !== want.length || want.some((w, i) => !lines[i].startsWith(w))) {
    throw new Error(`drift lines:\n  ${lines.join('\n  ')}\nexpected:\n  ${want.join('\n  ')}`);
  }
  const [reply] = commandResults(world, SET);
  expectParts(reply, ['log.consts_findings = 0 from the next tick'], 'configSet reply');
  const [show] = commandResults(world, SHOW);
  expectParts(show, [`checked at tick ${T0 + second.offset}`, `CREEP_LIFE_TIME (mismatch): crate ${life}, server ${life - 100};`], 'consts() in global 2');
}

/** C: a global without game constants: one fault, no retry. */
function noConstants(dir) {
  const counts = {};
  const world = simulate(dir, {
    ticks: 10,
    constants: [],
    command: (w) => (w.logs.some((l) => l.startsWith('rustedscreep fault')) ? SHOW : null),
    until: (w) => commandResults(w, SHOW).length === 2,
    setup: instrument(counts, false),
  });
  const [fault] = noTrouble(world, 'no constants', { faults: 1 });
  const at = /^rustedscreep fault at tick (\d+): job consts check unit 0: the global object holds no game constants$/.exec(fault);
  if (!at) throw new Error(`no constants: fault line ${fault}`);
  const [show] = commandResults(world, SHOW);
  expectParts(show, [`rustedscreep consts: failed at tick ${at[1]}: the global object holds no game constants; it does not retry`], 'consts() failed');
  const later = callsBetween(counts, Number(at[1]) - T0 + 1, world.ticksRun);
  if (later.names || later.descriptors) throw new Error(`no constants: the check retried: ${JSON.stringify(later)}`);
  if (constsLines(world).length) throw new Error(`no constants: ${constsLines(world).join(' / ')}`);
}

/** D: below the Optional gate the check waits, and makes no JS call. */
function lowBucket(dir) {
  const counts = {};
  const world = simulate(dir, { ticks: 6, bucket: 3000, command: (w) => (w.ticksRun >= 1 ? BOTH : null), setup: instrument(counts, false) });
  noTrouble(world, 'low bucket');
  const replies = samplesMatch(world, 'low bucket');
  expectParts(replies.at(-1), ['optional off', 'tier-off skips 1,', 'rustedscreep consts: not run yet in this global'], 'low bucket');
  // The second loop's only readings are the config record load's two samples (P1.10).
  if (world.loopCalls[2]?.getUsed !== 2) throw new Error(`low bucket: offset 2 made ${world.loopCalls[2]?.getUsed} getUsed calls, expected 2`);
  if (Object.values(counts).some((c) => c.names || c.descriptors)) throw new Error(`low bucket: the check read the global: ${JSON.stringify(counts)}`);
}

export function constsSim(dir = join(ROOT, 'dist')) {
  const read = readPath(dir);
  drift(dir);
  noConstants(dir);
  lowBucket(dir);
  return read;
}

if (isMain(import.meta.url)) {
  try {
    const read = constsSim(process.argv[2]);
    console.log(`consts-sim: ok (${read.upper} upper-case names read once each; ${read.summary})`);
  } catch (e) {
    fail(`consts sandbox test failed: ${e.message}`);
  }
}
