import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { WASM_MODULE } from './lib/common.mjs';
import { smoke } from './smoke.mjs';

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

test('a CommonJS bundle that compiles the required wasm and logs passes', () => {
  const logs = withBundle(
    "let n = 0; exports.loop = function () { new WebAssembly.Module(require('rustedscreep_wasm')); console.log('tick ' + n++); };",
    (dir) => smoke(dir, 2),
  );
  assert.deepEqual(logs, ['tick 0', 'tick 1']);
});

test('a bundle that uses TextDecoder fails, as it would in Screeps', () => {
  withBundle('exports.loop = function () { new TextDecoder(); };', (dir) => {
    assert.throws(() => smoke(dir), /TextDecoder is not defined/);
  });
});

test('a bundle that requires a module that is not bundled fails', () => {
  withBundle("exports.loop = function () { require('url'); };", (dir) => {
    assert.throws(() => smoke(dir), /is not a bundled module/);
  });
});

test('a bundle without exports.loop fails', () => {
  withBundle('exports.other = 1;', (dir) => {
    assert.throws(() => smoke(dir), /does not export loop/);
  });
});
