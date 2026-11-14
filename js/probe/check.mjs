#!/usr/bin/env node
// Local self-check for the P0 server feature probe. No network and no game access.
//   1. console-snippet.js holds exactly the expected paste units, one per line, each a single
//      `(function(){...})()` expression with no double quotes or backslashes, and each within the
//      console expression limit (JSON-encoded length <= 1024; screeps/backend-local, VERIFICATION_LOG V69).
//   2. ES5 only: parsed with acorn (ecmaVersion 5) when it can be imported, otherwise a token scan.
//   3. Every unit runs and returns one JSON line with the right `part`, both with full mocks and with
//      optional globals missing (WebAssembly, RawMemory, InterShardMemory, getHeapStatistics, Game.shard).
//   4. main.js holds the same five bodies in the same order; loop() logs them once per global and
//      run() returns them.
//   5. The hex in the wasm units decodes byte for byte to the wasm-feature-detect v1.10.1 reference
//      arrays below (verified against two independent fetches, VERIFICATION_LOG V67).
//   6. Prints what this Node's engine accepts, as a sanity signal for the vendored bytes.
// Usage: node js/probe/check.mjs
//
// The REFERENCE arrays are wasm-feature-detect test modules:
// Copyright 2019 Google Inc. All Rights Reserved.
// Copyright 2023 Google Inc. All Rights Reserved.
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//     http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const CONSOLE_EXPRESSION_LIMIT = 1024;
const PARTS = ['wasm1', 'wasm2', 'js', 'api', 'env'];
// Output key in the probe -> wasm-feature-detect export name.
const WASM_KEYS = {
  mutGlobals: 'mutableGlobals', signExt: 'signExtensions', satF2I: 'saturatedFloatToInt', bulkMem: 'bulkMemory',
  refTypes: 'referenceTypes', multiValue: 'multiValue', simd: 'simd', eh: 'exceptions', ehExnref: 'exceptionsFinal',
  tailCall: 'tailCall',
};
const REFERENCE = {
  mutableGlobals: [0, 97, 115, 109, 1, 0, 0, 0, 2, 8, 1, 1, 97, 1, 98, 3, 127, 1, 6, 6, 1, 127, 1, 65, 0, 11, 7, 5, 1, 1, 97, 3, 1],
  signExtensions: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 8, 1, 6, 0, 65, 0, 192, 26, 11],
  saturatedFloatToInt: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 12, 1, 10, 0, 67, 0, 0, 0, 0, 252, 0, 26, 11],
  bulkMemory: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 5, 3, 1, 0, 1, 10, 14, 1, 12, 0, 65, 0, 65, 0, 65, 0, 252, 10, 0, 0, 11],
  referenceTypes: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 7, 1, 5, 0, 208, 112, 26, 11],
  multiValue: [0, 97, 115, 109, 1, 0, 0, 0, 1, 6, 1, 96, 0, 2, 127, 127, 3, 2, 1, 0, 10, 8, 1, 6, 0, 65, 0, 65, 0, 11],
  simd: [0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0, 65, 0, 253, 15, 253, 98, 11],
  exceptions: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 8, 1, 6, 0, 6, 64, 25, 11, 11],
  exceptionsFinal: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 16, 1, 14, 0, 2, 105, 31, 64, 1, 3, 0, 0, 11, 0, 11, 26, 11],
  tailCall: [0, 97, 115, 109, 1, 0, 0, 0, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 10, 6, 1, 4, 0, 18, 0, 11],
};

const here = dirname(fileURLToPath(import.meta.url));
const snippet = readFileSync(join(here, 'console-snippet.js'), 'utf8');
const mainSrc = readFileSync(join(here, 'main.js'), 'utf8');
let failures = 0;
const fail = (msg) => {
  failures += 1;
  console.error(`FAIL ${msg}`);
};
const ok = (msg) => console.log(`ok   ${msg}`);

// 1. Paste units.
const units = snippet.split(/\r?\n/).filter((l) => l.startsWith('(function'));
if (units.length !== PARTS.length) fail(`console-snippet.js: expected ${PARTS.length} paste units, found ${units.length}`);
units.forEach((u, i) => {
  const n = JSON.stringify(u).length;
  if (!/^\(function\(\)\{.*\}\)\(\)$/.test(u)) fail(`unit ${i + 1}: not a single (function(){...})() line`);
  if (/["\\]/.test(u)) fail(`unit ${i + 1}: contains a double quote or backslash`);
  if (n > CONSOLE_EXPRESSION_LIMIT) fail(`unit ${i + 1}: JSON length ${n} > ${CONSOLE_EXPRESSION_LIMIT}`);
  else ok(`unit ${i + 1} (${PARTS[i]}): JSON length ${n} <= ${CONSOLE_EXPRESSION_LIMIT}`);
});

// 2. ES5 only.
let acorn = null;
try {
  acorn = await import('acorn');
} catch {
  acorn = null;
}
function stripCommentsAndStrings(src) {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/\/\/[^\n]*/g, ' ')
    .replace(/"(?:[^"\\\n]|\\.)*"/g, '""')
    .replace(/'(?:[^'\\\n]|\\.)*'/g, "''");
}
const ES2015_PLUS = [
  [/=>/, 'arrow function'], [/`/, 'template literal'], [/\blet\b/, 'let'], [/\bconst\b/, 'const'],
  [/\bclass\b/, 'class'], [/\.\.\./, 'spread/rest'], [/\*\*/, 'exponent operator'], [/\?\./, 'optional chaining'],
  [/\?\?/, 'nullish coalescing'], [/\basync\b/, 'async'], [/\bawait\b/, 'await'], [/function\s*\*/, 'generator'],
  [/\bof\b/, 'for...of'], [/\b0[bBoO][0-7]/, 'binary/octal literal'], [/function\s*[\w$]*\s*\([^)]*=/, 'default parameter'],
];
const es5Sources = [...units.map((u, i) => [`unit ${i + 1}`, u]), ['main.js', mainSrc]];
for (const [name, src] of es5Sources) {
  if (acorn) {
    try {
      acorn.parse(src, { ecmaVersion: 5, sourceType: 'script' });
      ok(`${name}: parses as ES5 (acorn)`);
    } catch (e) {
      fail(`${name}: not ES5 (acorn): ${e.message}`);
    }
  } else {
    const code = stripCommentsAndStrings(src);
    const hits = ES2015_PLUS.filter(([re]) => re.test(code)).map(([, what]) => what);
    if (/\\u\{/.test(src)) hits.push('code-point escape');
    if (hits.length) fail(`${name}: ES2015+ syntax outside strings: ${hits.join(', ')}`);
    else ok(`${name}: ES5-only tokens (token scan; install acorn for a full parse)`);
  }
}

// 3. Run every unit against mocks.
function makeGlobals(variant) {
  const cpu = {
    limit: 20, tickLimit: 500, bucket: 10000, getUsed: () => 1, halt: () => {}, generatePixel: () => 0,
    getHeapStatistics: () => ({ total_heap_size: 10, used_heap_size: 5, heap_size_limit: 100 }),
    shardLimits: { shard0: 0, shard3: 20 },
  };
  const g = {
    Game: { time: 12345, shard: { name: 'shardX', type: 'normal', ptr: false }, cpu },
    RawMemory: { segments: {}, setActiveSegments: () => {} },
    InterShardMemory: { getLocal: () => '' },
  };
  if (variant === 'sparse') {
    delete cpu.getHeapStatistics;
    delete cpu.shardLimits;
    delete cpu.generatePixel;
    delete g.Game.shard;
    delete g.RawMemory;
    delete g.InterShardMemory;
    cpu.limit = Infinity;
    cpu.bucket = Infinity;
  }
  return g;
}
function run(src, variant, extra = {}) {
  const ctx = vm.createContext({ ...makeGlobals(variant), ...extra });
  if (variant === 'sparse') vm.runInContext('delete globalThis.WebAssembly;', ctx);
  return vm.runInContext(src, ctx);
}
const outputs = [];
units.forEach((u, i) => {
  for (const variant of ['full', 'sparse']) {
    try {
      const out = run(u, variant);
      const parsed = JSON.parse(out);
      if (parsed.part !== PARTS[i] || parsed.probe !== 'rustedscreep') fail(`unit ${i + 1} (${variant}): wrong part/probe tag`);
      if (variant === 'full') outputs.push(out);
    } catch (e) {
      fail(`unit ${i + 1} (${variant}): did not return JSON: ${e.message}`);
    }
  }
});
if (outputs.length === PARTS.length) ok('all units return one tagged JSON line, with full and with sparse globals');
try {
  const env = JSON.parse(run(units[4], 'sparse'));
  if (env.cpu.bucket !== 'Infinity' || env.heap !== null || env.shard !== null) fail('env (sparse): Infinity/heap/shard not reported as expected');
  else ok('env (sparse): Infinity kept as a string; missing heap and shard reported as null');
} catch (e) {
  fail(`env (sparse): ${e.message}`);
}

// 4. main.js: same bodies, same order; loop() once per global; run() returns the same lines.
const snippetBodies = units.map((u) => u.slice('(function(){'.length, -'})()'.length));
const mainBodies = mainSrc.split(/\r?\n/).filter((l) => l.startsWith('PARTS.push(function(){')).map((l) => l.slice('PARTS.push(function(){'.length, -'});'.length));
if (mainBodies.length !== snippetBodies.length || mainBodies.some((b, i) => b !== snippetBodies[i])) fail('main.js: probe bodies differ from console-snippet.js');
else ok('main.js: the five probe bodies are identical to console-snippet.js');
try {
  const logs = [];
  const module = { exports: {} };
  const ctx = vm.createContext({ ...makeGlobals('full'), console: { log: (s) => logs.push(String(s)) }, module });
  vm.runInContext(mainSrc, ctx);
  module.exports.loop();
  module.exports.loop();
  const lines = logs.map((s) => s.replace(/^rustedscreep-probe /, ''));
  if (logs.length !== PARTS.length || !logs.every((s) => s.startsWith('rustedscreep-probe '))) fail(`main.js: loop() logged ${logs.length} lines over two ticks, expected ${PARTS.length} once`);
  else if (lines.join('\n') !== outputs.join('\n')) fail('main.js: loop() output differs from the console units');
  else if (module.exports.run() !== outputs.join('\n')) fail('main.js: run() output differs from the console units');
  else ok('main.js: loop() logs the five lines once per global; run() returns the same lines');
} catch (e) {
  fail(`main.js: did not run: ${e.message}`);
}

// 5. Vendored bytes.
const decoded = {};
for (const u of units.slice(0, 2)) {
  const m = u.match(/M=\{([^}]*)\}/);
  if (!m) continue;
  for (const pair of m[1].split(',')) {
    const [k, v] = pair.split(':');
    decoded[k] = v.replace(/'/g, '');
  }
}
const HEAD_V1 = [0, 97, 115, 109, 1, 0, 0, 0];
const toBytes = (hex) => (hex.match(/../g) || []).map((h) => parseInt(h, 16));
let byteErrors = 0;
for (const [key, exportName] of Object.entries(WASM_KEYS)) {
  const got = JSON.stringify([...HEAD_V1, ...toBytes(decoded[key] ?? 'zz')]);
  if (got !== JSON.stringify(REFERENCE[exportName])) {
    byteErrors += 1;
    fail(`bytes: ${key} does not match reference ${exportName}`);
  }
}
if (decoded.mvp !== '' || decoded.badver !== '') fail('bytes: mvp and badver controls must have empty tails');
if (!byteErrors) ok('bytes: all 10 wasm units decode exactly to the reference arrays');

// 6. Sanity signal from this Node's engine.
if (outputs.length >= 2) {
  const w = { ...JSON.parse(outputs[0]), ...JSON.parse(outputs[1]) };
  delete w.probe;
  delete w.part;
  delete w.v;
  console.log(`info this Node ${process.version} (V8 ${process.versions.v8}): wasm=${JSON.stringify(w)}`);
  if (w.mvp !== 1) fail('the MVP control module does not validate on this Node');
  if (w.badver !== 0) fail('the bad-version control module validates on this Node');
  const js = JSON.parse(outputs[2]);
  if (js.es5 !== 1) fail('the ES5 syntax control did not compile on this Node');
}

if (failures) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log('all probe checks passed');
