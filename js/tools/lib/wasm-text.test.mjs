import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { test } from 'node:test';
import vm from 'node:vm';
import { Loader, partModuleName as loaderPartModuleName } from '../../build-test/loader.js';
import { WASM_MODULE } from './common.mjs';
import {
  MODULE_MAX_BYTES, PART_MAX_CHARS, WRAPPER_BYTES, partFileName, partIndex, partModuleName, splitBase64, textModule,
  textModuleBase64, wasmModuleBytes, wasmModuleText, wasmModuleTexts, wasmPartsBytes,
} from './wasm-text.mjs';

test('the text module round-trips any bytes and is one ASCII line', () => {
  for (const n of [0, 1, 2, 3, 4, 230513]) {
    const bytes = randomBytes(n);
    const text = wasmModuleText(bytes);
    assert.match(text, /^module\.exports = "[A-Za-z0-9+/=]*";\n$/);
    assert.ok(Buffer.from(text, 'utf8').length === text.length, 'ASCII');
    assert.deepEqual(wasmModuleBytes(text), bytes, `n=${n}`);
  }
});

test('anything but the exact form is refused', () => {
  const good = wasmModuleText(Buffer.from([1, 2, 3, 4]));
  for (const bad of [
    `${good}require("x");\n`,
    good.replaceAll('\n', '\r\n'),
    good.trimEnd(),
    good.replace('AQIDBA==', 'AQIDB-=='),
    good.replace('AQIDBA==', 'AQIDB_=='),
    good.replace('AQIDBA==', 'AQIDBA='),
    'module.exports = "AQIDBB==";\n', // not canonical: non-zero trailing bits
  ]) {
    assert.throws(() => wasmModuleBytes(bad), /not a wasm text module|not canonical/, JSON.stringify(bad));
  }
});

test('evaluated in the driver\'s module wrapper, the text module exports the base64 string', () => {
  // screeps/driver lib/runtime/runtime-driver.js wraps a module's code as (function __module(module,exports){...}).
  const bytes = randomBytes(100);
  const context = vm.createContext({});
  const __module = vm.runInContext('({ exports: {} })', context);
  context.__module = __module;
  vm.runInContext(`(function __module(module,exports){ ${wasmModuleText(bytes)}\n})(__module, __module.exports)`, context);
  assert.equal(__module.exports, bytes.toString('base64'));
});

test('a part holds the largest module seen intact less the wrapper, in whole base64 groups (V257, ADR-028)', () => {
  assert.equal(WRAPPER_BYTES, 'module.exports = "";\n'.length);
  assert.equal(MODULE_MAX_BYTES, 905709);
  assert.equal(PART_MAX_CHARS % 4, 0);
  assert.ok(PART_MAX_CHARS + WRAPPER_BYTES <= MODULE_MAX_BYTES && PART_MAX_CHARS + 4 + WRAPPER_BYTES > MODULE_MAX_BYTES);
  // The 905709-byte module of f3e7dbd63bfe-cee9042d held 905688 characters (its boot line).
  assert.equal(PART_MAX_CHARS, 905688);
  assert.equal(Buffer.byteLength(textModule('A'.repeat(PART_MAX_CHARS))), MODULE_MAX_BYTES);
});

test('the base64 is cut into the fewest parts under the cap, of equal length within 4 characters', () => {
  for (const chars of [0, 4, 8, 12, 400, 404, 1200, 1204, 4000]) {
    const base64 = 'A'.repeat(chars);
    for (const maxChars of [4, 8, 400, 1200]) {
      const parts = splitBase64(base64, maxChars);
      assert.equal(parts.length, Math.max(1, Math.ceil(chars / maxChars)), `chars=${chars} max=${maxChars}`);
      assert.equal(parts.join(''), base64);
      const lengths = parts.map((p) => p.length);
      assert.ok(lengths.every((n) => n % 4 === 0 && n <= maxChars), JSON.stringify(lengths));
      assert.ok(Math.max(...lengths) - Math.min(...lengths) <= 4, JSON.stringify(lengths));
    }
  }
  assert.throws(() => splitBase64('AAA', 8), /multiple of 4/);
  for (const bad of [0, 3, 6, 1.5]) assert.throws(() => splitBase64('AAAA', bad), /positive multiple of 4/);
});

test('this build and the profiler build split into two parts, as ADR-028 records', () => {
  // 979566 and 1014021 bytes of wasm (2026-11-18) are 1306088 and 1352028 characters of base64.
  assert.deepEqual(splitBase64('A'.repeat(1306088)).map((p) => p.length), [653044, 653044]);
  assert.deepEqual(splitBase64('A'.repeat(1352028)).map((p) => p.length), [676016, 676012]);
  // A module of f3e7dbd's size stays one; two parts hold up to 2 x 905688 characters.
  assert.equal(splitBase64('A'.repeat(905688)).length, 1);
  assert.equal(splitBase64('A'.repeat(905692)).length, 2);
  assert.equal(splitBase64('A'.repeat(2 * 905688)).length, 2);
  assert.equal(splitBase64('A'.repeat(2 * 905688 + 4)).length, 3);
});

test('split text modules round-trip any bytes, only the last may end in =, and each is a text module', () => {
  for (const n of [0, 1, 2, 3, 4, 5, 1000, 230513]) {
    const bytes = randomBytes(n);
    for (const maxChars of [4, 400, PART_MAX_CHARS]) {
      const texts = wasmModuleTexts(bytes, maxChars);
      assert.deepEqual(wasmPartsBytes(texts), bytes, `n=${n} max=${maxChars}`);
      texts.slice(0, -1).forEach((t) => assert.ok(!textModuleBase64(t).endsWith('='), `n=${n}`));
      for (const t of texts) assert.match(t, /^module\.exports = "[A-Za-z0-9+/=]*";\n$/);
    }
  }
  assert.deepEqual(wasmModuleTexts(Buffer.from([1, 2, 3, 4])), [wasmModuleText(Buffer.from([1, 2, 3, 4]))]);
  assert.throws(() => wasmPartsBytes([textModule('AQ=='), textModule('AQID')]), /part 0 ends in '='/);
  assert.throws(() => wasmPartsBytes([]), /no wasm text parts/);
});

test('part names: rustedscreep_wasm, then a numbered suffix, in the tools and in the loader alike', () => {
  assert.equal(partModuleName(0), WASM_MODULE);
  assert.equal(partModuleName(1), `${WASM_MODULE}_1`);
  assert.equal(partFileName(2), `${WASM_MODULE}_2.js`);
  // The compiled loader (js/src/loader.ts) requires the same names the build writes.
  assert.equal(typeof Loader, 'function');
  for (let k = 0; k < 4; k++) assert.equal(loaderPartModuleName(k), partModuleName(k), `k=${k}`);
  for (let k = 0; k < 12; k++) assert.equal(partIndex(partFileName(k)), k);
  for (const other of ['main.js', `${WASM_MODULE}.wasm`, `${WASM_MODULE}_0.js`, `${WASM_MODULE}_01.js`, `${WASM_MODULE}_x.js`, `${WASM_MODULE}_1.js.bak`, `x${WASM_MODULE}.js`]) {
    assert.equal(partIndex(other), -1, other);
  }
});
