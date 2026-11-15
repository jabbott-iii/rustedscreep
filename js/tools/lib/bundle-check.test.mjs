import assert from 'node:assert/strict';
import { test } from 'node:test';
import { bundleProblems } from './bundle-check.mjs';

const OK_TAIL = "\nconst WASM_MODULE = 'rustedscreep_wasm';\nexports.loop = function loop() { require(WASM_MODULE); require('rustedscreep_wasm'); };\n";

test('comments, strings and property names do not count', () => {
  const code = `/** Never use fetch() or import.meta here; document.currentScript is absent. */\n` +
    `const s = "fetch('x') import.meta"; const o = { fetch: 1, URL: 2 }; o.document = o.fetch;${OK_TAIL}`;
  assert.deepEqual(bundleProblems(code, 'rustedscreep_wasm'), []);
});

test('real uses of banned globals are reported', () => {
  assert.deepEqual(bundleProblems(`fetch('x');${OK_TAIL}`, 'rustedscreep_wasm'), ['fetch']);
  assert.deepEqual(bundleProblems(`const u = new URL('a');${OK_TAIL}`, 'rustedscreep_wasm'), ['URL']);
  assert.deepEqual(bundleProblems(`const c = document.currentScript;${OK_TAIL}`, 'rustedscreep_wasm'), ['document']);
  // Never probed on the MMO (V95); the wasm text module's decoder is plain JS (ADR-015).
  assert.deepEqual(bundleProblems(`atob('AA==');${OK_TAIL}`, 'rustedscreep_wasm'), ['atob']);
  assert.deepEqual(bundleProblems(`const b = Buffer.from('AA==', 'base64');${OK_TAIL}`, 'rustedscreep_wasm'), ['Buffer']);
  assert.deepEqual(bundleProblems(`const g = globalThis; g['Buffer']; g.atob;${OK_TAIL}`, 'rustedscreep_wasm'), []);
});

test("Rollup's CJS import.meta.url shim and other requires are reported", () => {
  // The shim is caught by its require('u' + 'rl'); `.pathToFileURL` there is a property, not a global.
  const shim = "const u = require('u' + 'rl').pathToFileURL(__filename).href;";
  assert.deepEqual(bundleProblems(`${shim}${OK_TAIL}`, 'rustedscreep_wasm'), ['require(BinaryExpression)']);
  assert.deepEqual(bundleProblems(`pathToFileURL(__filename);${OK_TAIL}`, 'rustedscreep_wasm'), ['pathToFileURL']);
  assert.deepEqual(bundleProblems(`require('url');${OK_TAIL}`, 'rustedscreep_wasm'), ['require("url")']);
});

test('import.meta and ES module syntax are reported', () => {
  assert.deepEqual(bundleProblems(`const u = import.meta.url;${OK_TAIL}`, 'rustedscreep_wasm'), ['import.meta']);
  assert.deepEqual(bundleProblems(`export function loop() {}`, 'rustedscreep_wasm'), ['ES module syntax (ExportNamedDeclaration)']);
});
