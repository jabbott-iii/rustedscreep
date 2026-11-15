import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { test } from 'node:test';
import vm from 'node:vm';
import { wasmModuleBytes, wasmModuleText } from './wasm-text.mjs';

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
    good.replace('\n', '\r\n'),
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
