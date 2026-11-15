import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { WASM_MODULE } from './lib/common.mjs';
import { wasmModuleText } from './lib/wasm-text.mjs';
import { saveSegments, simulate } from './smoke.mjs';

// The smallest valid wasm module: magic number and version 1 (WebAssembly core spec, binary format).
const EMPTY_WASM = Uint8Array.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

function withBundle(mainJs, fn) {
  const dir = mkdtempSync(join(tmpdir(), 'rustedscreep-smoke-'));
  writeFileSync(join(dir, 'main.js'), mainJs);
  writeFileSync(join(dir, `${WASM_MODULE}.wasm`), EMPTY_WASM);
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('a CommonJS bundle that compiles the required wasm, logs and keeps Memory passes', () => {
  const world = withBundle(
    "exports.loop = function () { new WebAssembly.Module(require('rustedscreep_wasm')); Memory.n = (Memory.n || 0) + 1; console.log('tick ' + Game.time + ' n ' + Memory.n); };",
    (dir) => simulate(dir, { ticks: 2 }),
  );
  assert.deepEqual(world.logs, ['tick 50000000 n 1', 'tick 50000001 n 2']);
});

test('a halt discards the tick\'s Memory and starts a new global', () => {
  // Each tick reads Memory before writing it: if the halted tick's write were kept, tick 2 would see it.
  const world = withBundle(
    "let evals = (globalThis.evals = (globalThis.evals || 0) + 1); exports.loop = function () { console.log('g' + evals + ' saw ' + Memory.t); Memory.t = Game.time; if (Game.time === 50000000) Game.cpu.halt(); };",
    (dir) => simulate(dir, { ticks: 3 }),
  );
  assert.equal(world.halts, 1);
  assert.equal(world.globals, 2);
  assert.deepEqual(world.logs, ['g1 saw undefined', 'g1 saw undefined', 'g1 saw 50000001']);
});

test('the console is rebuilt each tick and has no error or warn', () => {
  const world = withBundle(
    "exports.loop = function () { console.log(typeof console.error + ' ' + typeof console.warn + ' ' + (console.mark === undefined)); console.mark = 1; };",
    (dir) => simulate(dir, { ticks: 2 }),
  );
  assert.deepEqual(world.logs, ['undefined undefined true', 'undefined undefined true']);
});

test('a bundle that uses TextDecoder fails, as it would in Screeps', () => {
  withBundle('exports.loop = function () { new TextDecoder(); };', (dir) => {
    assert.throws(() => simulate(dir), /TextDecoder is not defined/);
  });
});

test('a bundle that requires a module that is not bundled fails', () => {
  withBundle("exports.loop = function () { require('url'); };", (dir) => {
    assert.throws(() => simulate(dir), /Unknown module 'url'/);
  });
});

test('a bundle without exports.loop fails', () => {
  withBundle('exports.other = 1;', (dir) => {
    assert.throws(() => simulate(dir), /does not export loop/);
  });
});

/** A fresh world for saveSegments. */
const segmentWorld = () => ({ segments: {}, segmentSaves: [], segmentErrors: [] });

test('the segment save keeps the driver\'s limits: 10 keys, ids 0-99, strings of at most 100 × 1024 characters (V41, V85)', () => {
  const ten = Object.fromEntries(Array.from({ length: 10 }, (_, i) => [String(i * 11), `s${i}`]));
  const ok = segmentWorld();
  saveSegments({ ...ten }, ok, 1);
  assert.deepEqual(ok.segmentErrors, []);
  assert.equal(Object.keys(ok.segments).length, 10);
  assert.deepEqual(ok.segmentSaves, [{ offset: 1, keys: Object.keys(ten) }]);
  const full = segmentWorld();
  saveSegments({ 5: 'x'.repeat(100 * 1024) }, full, 2);
  assert.equal(full.segments[5].length, 100 * 1024);

  const refused = [
    [{ ...ten, 99: 'one more' }, /more than 10 segments \(11\)/],
    [{ 100: 'x' }, /segment id 100/],
    [{ '-1': 'x' }, /segment id -1/],
    [{ 7: 42 }, /memory segment #7 is not a string/],
    [{ 7: 'x'.repeat(100 * 1024 + 1) }, /memory segment #7 is 102401 long/],
  ];
  for (const [segments, why] of refused) {
    const w = segmentWorld();
    saveSegments({ 3: 'kept only if the whole save passes', ...segments }, w, 9);
    assert.equal(w.segmentErrors.length, 1, String(why));
    assert.match(w.segmentErrors[0], why);
    assert.deepEqual(w.segments, {}, 'a failed save stores nothing');
    assert.deepEqual(w.segmentSaves, []);
  }
  const none = segmentWorld();
  for (const v of [undefined, null, 42]) saveSegments(v, none, 0);
  assert.deepEqual(none, segmentWorld(), 'no segments object, nothing to save');
});

test('a text module is evaluated like the driver, once per global, and require returns its exports (ADR-015)', () => {
  const world = withBundle(
    "exports.loop = function () { const r = require('rustedscreep_wasm'); console.log(typeof r + ' ' + r.length + ' ' + (r === require('rustedscreep_wasm'))); };",
    (dir) => {
      writeFileSync(join(dir, `${WASM_MODULE}.js`), wasmModuleText(EMPTY_WASM));
      return simulate(dir, { ticks: 2, wasmForm: 'base64' });
    },
  );
  // 8 bytes are 12 base64 characters.
  assert.deepEqual(world.logs, ['string 12 true', 'string 12 true']);
});

test('the base64 form without the text module reproduces the live Unknown module error (V136)', () => {
  assert.throws(
    () => withBundle("exports.loop = function () { require('rustedscreep_wasm'); };", (dir) => simulate(dir, { ticks: 1, wasmForm: 'base64' })),
    /Unknown module 'rustedscreep_wasm'/,
  );
});

test('the sandbox has no atob, btoa or Buffer, as assumed for the MMO (V95)', () => {
  const world = withBundle(
    "exports.loop = function () { console.log([typeof atob, typeof btoa, typeof Buffer].join(' ')); };",
    (dir) => simulate(dir, { ticks: 1 }),
  );
  assert.deepEqual(world.logs, ['undefined undefined undefined']);
});
