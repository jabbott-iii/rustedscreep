// Drives the loader's state machine with a fake Screeps runtime. A halt is modelled as a global reset:
// the harness builds a new Loader (fresh module state) on the same Memory record, as the next tick would.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { crc32Hex } from '../build-test/crc32.js';
import { Loader, loadWasm, partModuleName, sanitizeLoaderMemory, toBytes } from '../build-test/loader.js';

const CONFIG = {
  bootBucketMin: 1500,
  bootSplitTicks: true,
  recoveryMode: 'halt',
  backoffWindowTicks: 1000,
  backoffIncidents: 3,
  backoffBaseTicks: 100,
  backoffMaxTicks: 3200,
  notifyGroupMinutes: 60,
  trapTestMax: 20,
};
const WASM = Uint8Array.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x80, 0xff]);

/**
 * `wasm` is the build's bytes (its CRC goes into the build info); `moduleValue` is what require('rustedscreep_wasm')
 * returns, the bytes by default (a binary module) or, for the GitHub link's text module, a base64 string (ADR-015).
 * `wasmParts` is the build's record of its text parts (one part of the build's base64 by default, ADR-028), and
 * `modules` answers require for any other name: a value, or a function that throws.
 */
function harness({
  config = {}, glue = {}, bucket = 10000, wasm = WASM, moduleValue = wasm, compileThrows = false,
  wasmParts = [Buffer.from(wasm).toString('base64').length], modules = {},
} = {}) {
  const h = {
    time: 1000, bucket, cpu: 0, halts: 0, notifies: [], logs: [], memoryReads: 0, mem: null,
    initCount: 0, loops: 0, loopThrows: false, resets: 0, resetThrows: false, compiles: 0, requires: [],
  };
  const env = {
    time: () => h.time,
    bucket: () => h.bucket,
    cpuUsed: () => (h.cpu += 0.5),
    halt: () => {
      if (h.noHalt) return false;
      h.halts++;
      return true;
    },
    notify: (m) => h.notifies.push(m),
    log: (m) => h.logs.push(m),
    requireModule: (name) => {
      h.requires.push(name);
      if (name === 'rustedscreep_wasm') return typeof moduleValue === 'function' ? moduleValue() : moduleValue;
      if (Object.hasOwn(modules, name)) return typeof modules[name] === 'function' ? modules[name]() : modules[name];
      throw new Error(`Unknown module '${name}'`);
    },
    compile: (bytes) => {
      h.compiles++;
      h.compiledBytes = Array.from(bytes);
      if (compileThrows) throw new Error('CompileError: bad');
      return { compiledFrom: bytes.length };
    },
    memory: () => {
      h.memoryReads++;
      h.mem ??= { incidents: [], streak: 0, pendingTraps: 0 };
      return h.mem;
    },
    globalsReport: () => 'TextDecoder=polyfill',
  };
  const g = {
    init: () => { h.initCount++; },
    loop: () => {
      if (h.loopThrows) throw new Error('RuntimeError: unreachable');
      h.loops++;
    },
    ...glue,
  };
  const build = { id: 'abc-dirty-00000000', wasmCrc32: crc32Hex(wasm), features: [], wasmParts };
  h.newGlobal = () => new Loader(env, g, { ...CONFIG, ...config }, build);
  h.loader = h.newGlobal();
  /** One tick; a halt replaces the loader, like a global reset. */
  h.tick = (n = 1) => {
    for (let i = 0; i < n; i++) {
      const before = h.halts;
      h.loader.tick();
      h.time++;
      if (h.halts > before) h.loader = h.newGlobal();
    }
  };
  return h;
}

test('boot waits for the bucket gate and logs that at most once per 100 ticks', () => {
  const h = harness({ bucket: 1499 });
  h.tick(150);
  assert.equal(h.loader.phase, 'idle');
  assert.equal(h.logs.filter((l) => l.includes('boot deferred')).length, 2);
  h.bucket = 1500;
  h.tick();
  assert.equal(h.loader.phase, 'compiled');
});

test('a split boot compiles on one tick and instantiates and runs on the next, logging the costs', () => {
  const h = harness();
  h.tick();
  assert.equal(h.loader.phase, 'compiled');
  assert.equal(h.loops, 0);
  const boot = h.logs.find((l) => l.startsWith('rustedscreep boot:'));
  assert.match(boot, /build abc-dirty-00000000 \| Game\.time 1000 \| wasm Uint8Array 10 B crc32 [0-9a-f]{8} \(stage [0-9a-f]{8}: match\) \| sentinel absent/);
  h.tick();
  assert.equal(h.loader.phase, 'ready');
  assert.equal(h.loops, 1);
  assert.ok(h.logs.some((l) => /booted in 1 ticks: load\+crc .* compile .* instantiate .* first loop/.test(l)));
});

test('without the split, boot, instantiate and the first loop share one tick', () => {
  const h = harness({ config: { bootSplitTicks: false } });
  h.tick();
  assert.equal(h.loops, 1);
});

test('a wasm whose CRC-32 differs from the stage report is flagged and notified', () => {
  const h = harness();
  h.loader = new Loader(h.loader.env, h.loader.glue, CONFIG, { id: 'x', wasmCrc32: 'deadbeef', features: [] });
  h.tick();
  assert.ok(h.logs.some((l) => l.includes('MISMATCH')));
  assert.ok(h.notifies.some((m) => m.includes('does not match')));
});

test('a CRC-32 mismatch on repeated failed boots in one global is notified once', () => {
  const h = harness({ compileThrows: true });
  h.loader = new Loader(h.loader.env, h.loader.glue, CONFIG, { id: 'x', wasmCrc32: 'deadbeef', features: [] });
  h.tick(3);
  assert.equal(h.notifies.filter((m) => m.includes('does not match')).length, 1);
  assert.equal(h.notifies.filter((m) => m.includes('incident: compile')).length, 3, 'one notification per incident');
});

test('a trap is one incident with one notification; the next tick halts and the bot boots again', () => {
  const h = harness();
  h.tick(2);
  h.loopThrows = true;
  h.loader.noteError('panicked at src/lib.rs:1: boom');
  h.tick();
  assert.equal(h.loader.phase, 'poisoned');
  assert.equal(h.notifies.length, 1);
  assert.match(h.notifies[0], /incident: trap at tick 1002 .*unreachable \| console\.error: panicked at src\/lib\.rs:1: boom/);
  h.loopThrows = false;
  h.tick();
  assert.equal(h.halts, 1);
  h.tick(2);
  assert.equal(h.loader.phase, 'ready');
  assert.deepEqual(h.mem.incidents, [1002]);
});

test('a tick that ended inside the wasm is detected on the next tick and leads to a halt', () => {
  const h = harness();
  h.tick(2);
  h.loader.running = true; // what a CPU-limit kill leaves behind
  h.tick();
  assert.ok(h.logs.some((l) => l.includes('incident: aborted')));
  h.tick();
  assert.equal(h.halts, 1);
});

test('the running flag covers instantiation and reset, so a tick killed in either is detected as aborted', () => {
  const seen = {};
  const h = harness({ config: { recoveryMode: 'reinstantiate' }, glue: { reset: () => { seen.reset = h.loader.running; } } });
  h.loader.glue.init = () => { seen.init = h.loader.running; };
  h.tick(2);
  h.loopThrows = true;
  h.tick();
  h.loopThrows = false;
  h.tick();
  assert.deepEqual(seen, { init: true, reset: true });
  assert.equal(h.loader.running, false);
});

test('compile failures do not halt; the third in a row starts backoff', () => {
  const h = harness({ compileThrows: true });
  h.tick(3);
  assert.equal(h.halts, 0);
  assert.deepEqual(h.mem.incidents, [1000, 1001, 1002]);
  assert.equal(h.mem.streak, 3);
  assert.equal(h.loader.backoffUntil(h.mem, 1003), 1002 + 100);
  h.tick(50);
  assert.equal(h.mem.incidents.length, 3, 'no boot attempt while backing off');
});

test('the delay doubles per consecutive incident and stops at the cap', () => {
  const h = harness();
  const mem = { incidents: [5000], streak: 0, pendingTraps: 0 };
  assert.deepEqual([2, 3, 4, 5, 8, 9, 30].map((streak) => h.loader.backoffUntil({ ...mem, streak }, 5001) - 5000),
    [-5000, 100, 200, 400, 3200, 3200, 3200]);
});

test('under a persistent trap the gap between boots grows to the cap (halt mode)', () => {
  const h = harness();
  h.loopThrows = true;
  h.tick(12000);
  const gaps = h.mem.incidents.slice(1).map((t, i) => t - h.mem.incidents[i]);
  assert.deepEqual(gaps.slice(0, 8), [3, 3, 101, 201, 401, 801, 1601, 3201]);
  assert.ok(gaps.slice(8).every((g) => g === 3201), `capped gaps: ${gaps.slice(8)}`);
});

test('an isolated incident after a quiet spell starts a new streak', () => {
  const h = harness();
  h.tick(2);
  h.mem.incidents = [100, 101, 102];
  h.mem.streak = 3;
  // 102 + window 1000 + delay 100 = 1202: an incident at 1202 or later is not part of the streak.
  h.time = 1202;
  h.loopThrows = true;
  h.tick();
  assert.equal(h.mem.streak, 1);
});

test('an incident dated in the future does not hold the boot back', () => {
  const h = harness();
  assert.equal(h.loader.backoffUntil({ incidents: [84257673], streak: 9, pendingTraps: 0 }, 1000), 0);
});

test('sanitizeLoaderMemory repairs every malformed shape and keeps a valid record', () => {
  const fresh = { incidents: [], streak: 0, pendingTraps: 0 };
  for (const raw of [undefined, null, 'garbage', 5, [], [1, 2]]) assert.deepEqual(sanitizeLoaderMemory(raw, 20), fresh);
  assert.deepEqual(sanitizeLoaderMemory({ incidents: [1, 'x', NaN, 3], streak: 'y', pendingTraps: 2 }, 20),
    { incidents: [1, 3], streak: 0, pendingTraps: 0 }, 'no trapRun: no pending traps');
  for (const trapRun of [{ awaitingSince: 5 }, { total: 2 }, 'x', null, { total: 2, startedAt: 1, recoveryTicks: null }]) {
    const r = sanitizeLoaderMemory({ incidents: [], streak: 0, pendingTraps: 5, trapRun }, 20);
    assert.equal(r.trapRun, undefined, JSON.stringify(trapRun));
    assert.equal(r.pendingTraps, 0);
  }
  const big = sanitizeLoaderMemory({ incidents: [], streak: 0, pendingTraps: 100000, trapRun: { total: 3, startedAt: 1, recoveryTicks: [3, 'z'] } }, 20);
  assert.equal(big.pendingTraps, 20);
  assert.deepEqual(big.trapRun, { total: 3, startedAt: 1, recoveryTicks: [3] });
  const valid = { incidents: [7], streak: 1, pendingTraps: 1, trapRun: { total: 2, startedAt: 1, awaitingSince: 4, recoveryTicks: [3] }, other: 'kept' };
  assert.deepEqual(sanitizeLoaderMemory(structuredClone(valid), 20), valid);
});

test('a malformed trap-test record cannot stop the loop from running', () => {
  const h = harness();
  h.mem = sanitizeLoaderMemory({ incidents: [], pendingTraps: 0, trapRun: { awaitingSince: 5 } }, 20);
  h.tick(3);
  assert.equal(h.loops, 2);
});

test('trap-test without the export explains how to build it', () => {
  const h = harness();
  assert.match(h.loader.scheduleTraps(3), /--features trap-test/);
});

test('trap-test: 10 forced traps in a row each recover, without counting toward backoff', () => {
  const h = harness({ glue: { trapTest: () => { throw new Error('RuntimeError: unreachable'); } } });
  h.tick(2);
  assert.match(h.loader.scheduleTraps(10), /10 trap\(s\) scheduled/);
  assert.match(h.loader.scheduleTraps(21), /from 1 to 20/);
  h.loader.scheduleTraps(10);
  h.tick(60);
  assert.equal(h.halts, 10);
  assert.equal(h.mem.pendingTraps, 0);
  assert.equal(h.mem.trapRun, undefined, 'the run finished and was cleared');
  assert.deepEqual(h.mem.incidents, []);
  const done = h.logs.find((l) => l.startsWith('rustedscreep trap-test done'));
  assert.match(done, /10 traps, each recovered \(ticks: 3(, 3){9}\)/);
  assert.equal(h.notifies.length, 1, 'one summary notification, not one per trap');
  assert.equal(h.loader.phase, 'ready');
});

test('reinstantiate mode recovers without a halt, and halts after too many re-instantiations', () => {
  const h = harness({ config: { recoveryMode: 'reinstantiate' }, glue: { reset: () => { h.resets++; } } });
  const initsBefore = () => h.initCount;
  h.tick(2);
  for (let i = 0; i < 3; i++) {
    h.loopThrows = true;
    h.tick();
    h.loopThrows = false;
    h.tick();
  }
  assert.equal(h.resets, 3);
  assert.equal(h.halts, 0);
  assert.equal(initsBefore(), 1, 'reset re-instantiates by itself; init runs only at boot');
  h.loopThrows = true;
  h.tick();
  h.loopThrows = false;
  h.tick();
  assert.equal(h.halts, 1, 'the fourth recovery in the window falls back to a halt');
});

test('a reset that throws falls back to a halt', () => {
  const h = harness({ config: { recoveryMode: 'reinstantiate' }, glue: { reset: () => { throw new Error('RuntimeError: unreachable'); } } });
  h.tick(2);
  h.loopThrows = true;
  h.tick();
  h.tick();
  assert.equal(h.halts, 1);
  assert.ok(h.logs.some((l) => l.includes('re-instantiation failed')));
});

test('without Game.cpu.halt, halt mode re-instantiates instead, and stops once when it cannot', () => {
  const h = harness({ glue: { reset: () => { h.resets++; } } });
  h.noHalt = true;
  h.tick(2);
  h.loopThrows = true;
  h.tick();
  h.loopThrows = false;
  h.tick(2);
  assert.equal(h.resets, 1);
  assert.equal(h.loops, 2, 'the loop runs again after the re-instantiation');
  assert.ok(h.logs.some((l) => l.includes('Game.cpu.halt is not available')));

  const s = harness();
  s.noHalt = true;
  s.tick(2);
  s.loopThrows = true;
  s.tick(5);
  assert.equal(s.logs.filter((l) => l.startsWith('rustedscreep stopped:')).length, 1);
  assert.equal(s.notifies.filter((m) => m.startsWith('rustedscreep stopped:')).length, 1);
  assert.equal(s.logs.filter((l) => l.startsWith('rustedscreep recovering')).length, 1, 'no log spam once stopped');
});

test('without halt, a capped re-instantiation waits and retries instead of stopping', () => {
  const h = harness({ glue: { reset: () => { h.resets++; } } });
  h.noHalt = true;
  h.tick(2);
  h.loopThrows = true;
  h.tick(400);
  assert.equal(h.resets, 3, 'three per window');
  h.tick(1000);
  assert.ok(h.resets >= 6, `retried after the window (${h.resets} resets)`);
  assert.ok(!h.logs.some((l) => l.startsWith('rustedscreep stopped:')));
  assert.equal(h.notifies.filter((m) => m.includes('retrying as the')).length, 2, 'one notice per capped episode');
});

test('reinstantiate mode without a reset function halts', () => {
  const h = harness({ config: { recoveryMode: 'reinstantiate' } });
  h.tick(2);
  h.loopThrows = true;
  h.tick();
  h.tick();
  assert.equal(h.halts, 1);
});

test('steady-state ticks of a normal build never read Memory', () => {
  const h = harness();
  h.tick(3);
  const reads = h.memoryReads;
  h.tick(100);
  assert.equal(h.memoryReads, reads);
});

test('toBytes accepts views, ArrayBuffers and base64 strings, and rejects anything else', () => {
  const buf = Buffer.from([1, 2, 3, 4]);
  assert.deepEqual(Array.from(toBytes(buf.subarray(1, 3))), [2, 3]);
  assert.deepEqual(Array.from(toBytes(new Uint8Array([5]).buffer)), [5]);
  // The GitHub link's text module exports the wasm as base64 (ADR-015).
  assert.deepEqual(Array.from(toBytes('AQID')), [1, 2, 3]);
  assert.throws(() => toBytes('not bytes'), /returned a String that is not base64 \(TypeError: /);
  assert.throws(() => toBytes(42), /returned Number, not bytes/);
});

test('a base64 string module boots like the binary one, and the boot line names it', () => {
  const h = harness({ moduleValue: Buffer.from(WASM).toString('base64') });
  h.tick();
  assert.equal(h.loader.phase, 'compiled');
  assert.deepEqual(h.compiledBytes, Array.from(WASM));
  const boot = h.logs.find((l) => l.startsWith('rustedscreep boot:'));
  assert.match(boot, /\| wasm String\(base64, 16 chars\) 10 B crc32 [0-9a-f]{8} \(stage [0-9a-f]{8}: match\) \|/);
  h.tick();
  assert.equal(h.loops, 1);
  assert.equal(h.notifies.length, 0);
});

test('a string that is not base64 is a load incident and is never compiled', () => {
  // WASM's base64 is 16 characters, so these have the build's length and fail the decode itself.
  for (const bad of ['AAAAAAAAAAAAAA=A', 'AAAAAAAAAAAAAAA-', 'AAAAAAAAAAAAAAA\n', 'not bytes, sorry']) {
    const h = harness({ moduleValue: bad });
    h.tick(3);
    assert.equal(h.compiles, 0, JSON.stringify(bad));
    assert.ok(h.logs.some((l) => /incident: load/.test(l) && /returned a String that is not base64/.test(l)), `${JSON.stringify(bad)}: ${h.logs.join(' / ')}`);
    assert.ok(h.mem.streak >= 1);
  }
});

test('a string of another length than the build records is a load incident before any decode (ADR-028)', () => {
  for (const bad of ['', 'abc', 'AAAA', 'AA=A', Buffer.from(WASM).toString('base64').slice(4), `${Buffer.from(WASM).toString('base64')}AAAA`]) {
    const h = harness({ moduleValue: bad });
    h.tick(3);
    assert.equal(h.compiles, 0, JSON.stringify(bad));
    assert.ok(h.logs.some((l) => /incident: load/.test(l) && new RegExp(`require\\('rustedscreep_wasm'\\) returned ${bad.length} characters, not 16`).test(l)),
      `${JSON.stringify(bad)}: ${h.logs.join(' / ')}`);
  }
});

test('a base64 string that decodes to other bytes is a CRC-32 MISMATCH and is notified once', () => {
  const good = Buffer.from(WASM).toString('base64');
  // Change one character inside the alphabet: still base64, other bytes.
  const corrupt = `${good[0] === 'A' ? 'B' : 'A'}${good.slice(1)}`;
  const h = harness({ moduleValue: corrupt });
  h.tick();
  const boot = h.logs.find((l) => l.startsWith('rustedscreep boot:'));
  assert.match(boot, /: MISMATCH\)/);
  assert.equal(h.notifies.filter((n) => /does not match the build's/.test(n)).length, 1);
});

// A wasm of 40 bytes is 56 base64 characters: three parts of 20, 20 and 16 with 20-character parts (ADR-028).
const BIG_WASM = Uint8Array.from([...WASM, ...Array.from({ length: 30 }, (_, i) => (i * 37) & 255)]);
function split(bytes = BIG_WASM, sizes = [20, 20, 16]) {
  const b64 = Buffer.from(bytes).toString('base64');
  const parts = [];
  let at = 0;
  for (const n of sizes) parts.push(b64.slice(at, (at += n)));
  assert.equal(parts.join(''), b64);
  return parts;
}
function splitHarness(parts, overrides = {}, extra = {}) {
  const modules = {};
  parts.slice(1).forEach((p, i) => { modules[partModuleName(i + 1)] = p; });
  return harness({ wasm: BIG_WASM, moduleValue: parts[0], wasmParts: parts.map((p) => p.length), modules: { ...modules, ...overrides }, ...extra });
}

test('a split text module boots from its parts in order, and the boot line counts them (ADR-028)', () => {
  const parts = split();
  const h = splitHarness(parts);
  h.tick();
  assert.equal(h.loader.phase, 'compiled');
  assert.deepEqual(h.compiledBytes, Array.from(BIG_WASM));
  assert.deepEqual(h.requires.slice(0, 3), ['rustedscreep_wasm', 'rustedscreep_wasm_1', 'rustedscreep_wasm_2']);
  const boot = h.logs.find((l) => l.startsWith('rustedscreep boot:'));
  assert.match(boot, /\| wasm String\(base64, 56 chars in 3 parts\) 40 B crc32 [0-9a-f]{8} \(stage [0-9a-f]{8}: match\) \|/);
  h.tick();
  assert.equal(h.loops, 1);
  assert.equal(h.notifies.length, 0);
});

test('a part beyond the build\'s count is never required, and a binary module ignores the parts', () => {
  const parts = split();
  const h = splitHarness(parts, { rustedscreep_wasm_3: () => { throw new Error('must not be required'); } });
  h.tick(2);
  assert.equal(h.loops, 1);
  assert.ok(!h.requires.includes('rustedscreep_wasm_3'));
  const bin = harness({ wasm: BIG_WASM, wasmParts: parts.map((p) => p.length), modules: { rustedscreep_wasm_1: () => { throw new Error('no'); } } });
  bin.tick(2);
  assert.equal(bin.loops, 1);
  assert.deepEqual([...new Set(bin.requires)], ['rustedscreep_wasm', 'sentinel']);
});

test('a missing, empty, cut, short or misplaced part is a load incident that names it, and is never compiled', () => {
  const parts = split();
  const cases = [
    // The live failure of 5217308 (V257): the module arrived empty, and an empty module exports {}.
    [{ rustedscreep_wasm_1: {} }, /require\('rustedscreep_wasm_1'\) returned Object, not a 20-character string/],
    [{ rustedscreep_wasm_2: () => { throw new Error("Unknown module 'rustedscreep_wasm_2'"); } }, /require\('rustedscreep_wasm_2'\) threw Error: Unknown module 'rustedscreep_wasm_2'/],
    [{ rustedscreep_wasm_1: () => { throw new SyntaxError('Invalid or unexpected token'); } }, /require\('rustedscreep_wasm_1'\) threw SyntaxError: Invalid or unexpected token/],
    [{ rustedscreep_wasm_1: parts[1].slice(4) }, /require\('rustedscreep_wasm_1'\) returned 16 characters, not 20/],
    [{ rustedscreep_wasm_2: `${parts[2]}AAAA` }, /require\('rustedscreep_wasm_2'\) returned 20 characters, not 16/],
    [{ rustedscreep_wasm_1: 42 }, /require\('rustedscreep_wasm_1'\) returned Number, not a 20-character string/],
    [{ rustedscreep_wasm_1: `${parts[1].slice(0, 16)}AA==` }, /require\('rustedscreep_wasm_1'\) ends in '=' but is not the last part/],
    [{ rustedscreep_wasm_2: `${parts[2].slice(0, 12)}AA-A` }, /require\('rustedscreep_wasm_2'\) returned a String that is not base64/],
    [{ rustedscreep_wasm_1: `${parts[1].slice(0, 16)}AA-A` }, /require\('rustedscreep_wasm_1'\) returned a String that is not base64/],
  ];
  for (const [overrides, want] of cases) {
    const h = splitHarness(parts, overrides);
    h.tick(3);
    assert.equal(h.compiles, 0, String(want));
    assert.ok(h.logs.some((l) => /incident: load/.test(l) && want.test(l)), `${want}: ${h.logs.join(' / ')}`);
    assert.ok(h.mem.streak >= 1);
  }
  // Part 0 is named the same way: a wrong-length string, other content that is not base64, a cut file, a missing one.
  const part0 = [
    [parts[0].slice(4), /require\('rustedscreep_wasm'\) returned 16 characters, not 20/],
    [`${parts[0].slice(0, 16)}A-AA`, /require\('rustedscreep_wasm'\) returned a String that is not base64/],
    [() => { throw new SyntaxError('Invalid or unexpected token'); }, /require\('rustedscreep_wasm'\) threw SyntaxError: Invalid or unexpected token/],
    [() => { throw new Error("Unknown module 'rustedscreep_wasm'"); }, /require\('rustedscreep_wasm'\) threw Error: Unknown module 'rustedscreep_wasm'/],
    // The live failure of 5217308 (V257): part 0 empty, read as a binary module that is not bytes.
    [{}, /TypeError: require\('rustedscreep_wasm'\) returned Object, not bytes/],
  ];
  for (const [moduleValue, want] of part0) {
    const h = splitHarness(parts, {}, { moduleValue });
    h.tick();
    assert.equal(h.compiles, 0, String(want));
    assert.ok(h.logs.some((l) => /incident: load/.test(l) && want.test(l)), `${want}: ${h.logs.join(' / ')}`);
  }
});

test('parts of the recorded lengths with other content decode and boot with a CRC-32 MISMATCH (ADR-015, ADR-028)', () => {
  // Swapping two parts of equal length gives a wasm no build produced; only the CRC-32 sees it, and it boots.
  const parts = split();
  const h = splitHarness(parts, { rustedscreep_wasm_1: parts[0] }, { moduleValue: parts[1] });
  h.tick();
  const boot = h.logs.find((l) => l.startsWith('rustedscreep boot:'));
  assert.match(boot, /in 3 parts\) 40 B crc32 [0-9a-f]{8} \(stage [0-9a-f]{8}: MISMATCH\)/);
  assert.equal(h.notifies.filter((n) => /does not match the build's/.test(n)).length, 1);
});

test('loadWasm refuses a text module when the build records no parts', () => {
  assert.throws(() => loadWasm(() => 'AQID', []), /records no text parts/);
  assert.throws(() => loadWasm(() => 'AQID', undefined), /records no text parts/);
  assert.deepEqual(Array.from(loadWasm(() => 'AQID', [4]).bytes), [1, 2, 3]);
  assert.equal(loadWasm(() => 'AQID', [4]).tag, 'String(base64, 4 chars)');
  assert.equal(loadWasm(() => Uint8Array.from([1]), []).tag, 'Uint8Array');
});

test('the snapshot command calls the wasm only while it is ready, and reports a missing export', () => {
  const none = harness();
  assert.match(none.loader.snapshot(), /no snapshotSummary export/);
  let calls = 0;
  const h = harness({ glue: { snapshotSummary: () => `summary ${++calls}` } });
  assert.match(h.loader.snapshot(), /no snapshot while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.phase, 'ready');
  assert.equal(h.loader.snapshot(), 'summary 1');
  assert.equal(h.loader.running, false);
  h.loopThrows = true;
  h.tick();
  assert.equal(h.loader.phase, 'poisoned');
  assert.match(h.loader.snapshot(), /no snapshot while the loader is poisoned/);
  assert.equal(calls, 1);
});

test('a snapshot command that throws poisons the instance like a trap, and the next tick recovers', () => {
  const h = harness({ glue: { snapshotSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  h.tick(2);
  assert.match(h.loader.snapshot(), /trapped; the loader recovers on the next tick/);
  assert.equal(h.loader.phase, 'poisoned');
  assert.equal(h.loader.running, false);
  assert.equal(h.mem.incidents.length, 1);
  assert.ok(h.logs.some((l) => l.includes('incident: trap at') && /snapshotSummary: Error: RuntimeError/.test(l)));
  h.tick();
  assert.equal(h.halts, 1, 'the next tick halts, as after a trap in loop');
});

test('the scheduler command calls the wasm only while it is ready, reports a missing export, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.scheduler(), /no schedulerSummary export/);
  let calls = 0;
  const h = harness({ glue: { schedulerSummary: () => `scheduler ${++calls}` } });
  assert.match(h.loader.scheduler(), /no scheduler summary while the loader is idle/);
  assert.match(h.loader.snapshot(), /no snapshotSummary export/, 'each command checks its own export');
  h.tick(2);
  assert.equal(h.loader.scheduler(), 'scheduler 1');
  assert.equal(h.loader.running, false);
  h.loopThrows = true;
  h.tick();
  assert.equal(h.loader.phase, 'poisoned');
  assert.match(h.loader.scheduler(), /no scheduler summary while the loader is poisoned/);
  assert.equal(calls, 1, 'the wasm is not called while poisoned');

  const bad = harness({ glue: { schedulerSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.scheduler(), /schedulerSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  assert.equal(bad.loader.running, false);
  assert.equal(bad.mem.incidents.length, 1);
  assert.ok(bad.logs.some((l) => l.includes('incident: trap at') && /schedulerSummary: Error: RuntimeError/.test(l)));
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the act command shares the guard: missing export, idle, poisoned, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.act(), /no actSummary export/);
  let calls = 0;
  const h = harness({ glue: { actSummary: () => `act ${++calls}` } });
  assert.match(h.loader.act(), /no act summary while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.act(), 'act 1');
  h.loopThrows = true;
  h.tick();
  assert.match(h.loader.act(), /no act summary while the loader is poisoned/);
  assert.equal(calls, 1);

  const bad = harness({ glue: { actSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.act(), /actSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  assert.ok(bad.logs.some((l) => l.includes('incident: trap at') && /actSummary: Error: RuntimeError/.test(l)));
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the profiler command shares the guard: missing export, idle, poisoned, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.profiler(), /no profilerSummary export/);
  let calls = 0;
  const h = harness({ glue: { profilerSummary: () => `profiler ${++calls}` } });
  assert.match(h.loader.profiler(), /no profiler summary while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.profiler(), 'profiler 1');
  assert.equal(h.loader.running, false);
  h.loopThrows = true;
  h.tick();
  assert.match(h.loader.profiler(), /no profiler summary while the loader is poisoned/);
  assert.equal(calls, 1);

  const bad = harness({ glue: { profilerSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.profiler(), /profilerSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  assert.ok(bad.logs.some((l) => l.includes('incident: trap at') && /profilerSummary: Error: RuntimeError/.test(l)));
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the say command passes a string to the wasm only while it is ready, and refuses other values', () => {
  const none = harness();
  assert.match(none.loader.say('hi'), /no queueSay export/);
  const said = [];
  const h = harness({ glue: { queueSay: (text) => { said.push(text); return `ok ${text}`; } } });
  assert.match(h.loader.say('hi'), /no say while the loader is idle/);
  h.tick(2);
  for (const bad of [undefined, 42, null, { text: 'hi' }]) {
    assert.match(h.loader.say(bad), /give the text as a string/);
  }
  assert.equal(h.loader.say('hi'), 'ok hi');
  assert.equal(h.loader.running, false);
  assert.deepEqual(said, ['hi'], 'only the string reached the wasm');

  const bad = harness({ glue: { queueSay: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.say('hi'), /queueSay trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('every console command refuses while the wasm is running, without calling it', () => {
  let calls = 0;
  const count = () => { calls += 1; return 'called'; };
  const h = harness({
    glue: {
      snapshotSummary: count, schedulerSummary: count, actSummary: count, queueSay: count, persistSummary: count, persistTest: count,
      configSummary: count, configSet: count, configUnset: count, configReset: count, constsSummary: count,
      movementSummary: count, moveTo: count,
    },
  });
  h.tick(2);
  assert.equal(h.loader.phase, 'ready');
  h.loader.running = true;
  assert.match(h.loader.snapshot(), /no snapshot while the loader is ready/);
  assert.match(h.loader.scheduler(), /no scheduler summary while the loader is ready/);
  assert.match(h.loader.act(), /no act summary while the loader is ready/);
  assert.match(h.loader.say('hi'), /no say while the loader is ready/);
  assert.match(h.loader.persist(), /no persist summary while the loader is ready/);
  assert.match(h.loader.persistTest(undefined), /no persist test while the loader is ready/);
  assert.match(h.loader.runtimeConfig(), /no config summary while the loader is ready/);
  assert.match(h.loader.configSet('log.level', 'debug'), /no config change while the loader is ready/);
  assert.match(h.loader.configUnset('log.level'), /no config change while the loader is ready/);
  assert.match(h.loader.configReset(), /no config change while the loader is ready/);
  assert.match(h.loader.consts(), /no consts summary while the loader is ready/);
  assert.match(h.loader.movement(), /no movement summary while the loader is ready/);
  assert.match(h.loader.moveTo('W1N1', 25, 25, 1), /no move order while the loader is ready/);
  assert.match(h.loader.moveTo(), /no move order while the loader is ready/);
  assert.equal(calls, 0);
  h.loader.running = false;
  assert.equal(h.loader.act(), 'called');
});

test('the persist command shares the guard: missing export, idle, poisoned, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.persist(), /no persistSummary export/);
  let calls = 0;
  const h = harness({ glue: { persistSummary: () => `persist ${++calls}` } });
  assert.match(h.loader.persist(), /no persist summary while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.persist(), 'persist 1');
  h.loopThrows = true;
  h.tick();
  assert.match(h.loader.persist(), /no persist summary while the loader is poisoned/);
  assert.equal(calls, 1);

  const bad = harness({ glue: { persistSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.persist(), /persistSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the consts command shares the guard: missing export, idle, ready, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.consts(), /no constsSummary export/);
  let calls = 0;
  const h = harness({ glue: { constsSummary: () => `consts ${++calls}` } });
  assert.match(h.loader.consts(), /no consts summary while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.consts(), 'consts 1');
  const bad = harness({ glue: { constsSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.consts(), /constsSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
});

test('the persistTest command passes a whole byte count or nothing, and refuses other values', () => {
  const none = harness();
  assert.match(none.loader.persistTest(undefined), /no persistTest export/);
  const sizes = [];
  const h = harness({ glue: { persistTest: (bytes) => { sizes.push(bytes); return `ok ${bytes}`; } } });
  assert.match(h.loader.persistTest(10), /no persist test while the loader is idle/);
  h.tick(2);
  for (const bad of [-1, 1.5, Number.NaN, Infinity, 2 ** 32, '100', null, {}]) {
    assert.match(h.loader.persistTest(bad), /give the payload size as a whole number of bytes/, String(bad));
  }
  assert.equal(h.loader.persistTest(undefined), 'ok undefined');
  assert.equal(h.loader.persistTest(0), 'ok 0');
  assert.equal(h.loader.persistTest(2 ** 32 - 1), `ok ${2 ** 32 - 1}`);
  assert.deepEqual(sizes, [undefined, 0, 2 ** 32 - 1], 'only valid sizes reached the wasm');

  const bad = harness({ glue: { persistTest: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.persistTest(undefined), /persistTest trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the config commands share the guard, pass keys and values as strings, and refuse other arguments', () => {
  const none = harness();
  assert.match(none.loader.runtimeConfig(), /no configSummary export/);
  assert.match(none.loader.configSet('log.level', 'debug'), /no configSet export/);
  assert.match(none.loader.configUnset('log.level'), /no configUnset export/);
  assert.match(none.loader.configReset(), /no configReset export/);
  const calls = [];
  const h = harness({
    glue: {
      configSummary: () => 'summary',
      configSet: (key, value) => { calls.push(['set', key, value]); return `set ${key}=${value}`; },
      configUnset: (key) => { calls.push(['unset', key]); return `unset ${key}`; },
      configReset: () => { calls.push(['reset']); return 'reset'; },
    },
  });
  assert.match(h.loader.configSet('log.level', 'debug'), /no config change while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.runtimeConfig(), 'summary');
  for (const bad of [undefined, null, 12, {}]) {
    assert.match(h.loader.configSet(bad, 1), /give the config key as a string/, String(bad));
    assert.match(h.loader.configUnset(bad), /give the config key as a string/, String(bad));
  }
  for (const bad of [undefined, null, Number.NaN, Infinity, {}, [1]]) {
    assert.match(h.loader.configSet('scheduler.tick_reserve_pct', bad), /give the value as a number, true or false, or a level name/, String(bad));
  }
  assert.equal(h.loader.configSet('scheduler.tick_reserve_pct', 12), 'set scheduler.tick_reserve_pct=12');
  assert.equal(h.loader.configSet('cache.structure_static', false), 'set cache.structure_static=false');
  assert.equal(h.loader.configSet('log.level', 'debug'), 'set log.level=debug');
  assert.equal(h.loader.configUnset('log.level'), 'unset log.level');
  assert.equal(h.loader.configReset(), 'reset');
  assert.deepEqual(calls, [
    ['set', 'scheduler.tick_reserve_pct', '12'], ['set', 'cache.structure_static', 'false'], ['set', 'log.level', 'debug'],
    ['unset', 'log.level'], ['reset'],
  ], 'only valid arguments reached the wasm, as strings');

  const bad = harness({ glue: { configSet: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.configSet('log.level', 'info'), /configSet trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  bad.tick();
  assert.equal(bad.halts, 1);
});

test('the movement command shares the guard: missing export, idle, ready, and a throw poisons', () => {
  const none = harness();
  assert.match(none.loader.movement(), /no movementSummary export/);
  let calls = 0;
  const h = harness({ glue: { movementSummary: () => `movement ${++calls}` } });
  assert.match(h.loader.movement(), /no movement summary while the loader is idle/);
  h.tick(2);
  assert.equal(h.loader.movement(), 'movement 1');
  const bad = harness({ glue: { movementSummary: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.movement(), /movementSummary trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
});

test('the moveTo command passes only room strings and whole numbers 0 to 49, or nothing to cancel', () => {
  const none = harness();
  assert.match(none.loader.moveTo('W1N1', 1, 1), /no moveTo export/);
  const got = [];
  const h = harness({ glue: { moveTo: (...args) => { got.push(args); return 'ok'; } } });
  assert.match(h.loader.moveTo('W1N1', 25, 25, 1), /no move order while the loader is idle/);
  h.tick(2);
  const refused = [
    [42, 25, 25, 1], ['W1N1', 25], ['W1N1', -1, 25], ['W1N1', 25, 50], ['W1N1', 2.5, 3], ['W1N1', 25, 25, 50],
    ['W1N1', 25, 25, '1'], ['W1N1', '25', 25], [undefined, 25, 25], [null, 1, 1], ['W1N1', Number.NaN, 1],
    ['W2N1', 25, 25, 1, 7], ['W2N1', 25, 25, 1, null], [undefined, undefined, undefined, undefined, 'W1N1'],
  ];
  for (const args of refused) {
    assert.match(h.loader.moveTo(...args), /give a room name and x, y/, JSON.stringify(args));
  }
  assert.equal(h.loader.moveTo('W1N1', 0, 49), 'ok');
  assert.equal(h.loader.moveTo('W1N1', 25, 25, 3), 'ok');
  assert.equal(h.loader.moveTo('W2N1', 25, 25, 1, 'W1N1'), 'ok');
  assert.equal(h.loader.moveTo('W2N1', 25, 25, undefined, 'W1N1'), 'ok');
  assert.equal(h.loader.moveTo(), 'ok');
  assert.deepEqual(got, [
    ['W1N1', 0, 49, undefined, undefined], ['W1N1', 25, 25, 3, undefined], ['W2N1', 25, 25, 1, 'W1N1'],
    ['W2N1', 25, 25, undefined, 'W1N1'], [],
  ]);
  assert.equal(h.loader.running, false);
  const bad = harness({ glue: { moveTo: () => { throw new Error('RuntimeError: unreachable'); } } });
  bad.tick(2);
  assert.match(bad.loader.moveTo('W1N1', 1, 1), /moveTo trapped; the loader recovers on the next tick/);
  assert.equal(bad.loader.phase, 'poisoned');
  bad.tick();
  assert.equal(bad.halts, 1);
});
