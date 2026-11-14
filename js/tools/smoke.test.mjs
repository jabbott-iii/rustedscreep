import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { WASM_MODULE } from './lib/common.mjs';
import { simulate } from './smoke.mjs';

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
