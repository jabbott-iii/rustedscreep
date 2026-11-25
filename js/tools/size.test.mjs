import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { deflateRawSync } from 'node:zlib';
import { WASM_MODULE } from './lib/common.mjs';
import { MODULE_MAX_BYTES, PART_MAX_CHARS, partFileName, wasmModuleText, wasmModuleTexts } from './lib/wasm-text.mjs';
import { UPLOAD_BUDGET, measure } from './size.mjs';

/** A dist/ with a wasm of `wasmBytes` zero bytes as text parts of at most `maxChars` characters (one by default). */
function fakeBuild(wasmBytes, { maxChars = null, main = 'exports.loop = function loop() {};\n' } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'rustedscreep-size-'));
  writeFileSync(join(dir, 'build-info.json'), JSON.stringify({ buildId: 'test' }));
  writeFileSync(join(dir, 'main.js'), main);
  writeFileSync(join(dir, `${WASM_MODULE}.wasm`), Buffer.alloc(wasmBytes));
  const texts = maxChars ? wasmModuleTexts(Buffer.alloc(wasmBytes), maxChars) : [wasmModuleText(Buffer.alloc(wasmBytes))];
  texts.forEach((t, k) => writeFileSync(join(dir, partFileName(k)), t));
  return dir;
}

// base64 turns 3 bytes into 4 characters, so a wasm of 3/4 of the budget is already over it (V76).
test('a wasm whose base64 exceeds the upload budget fails the check', () => {
  const dir = fakeBuild(Math.ceil((UPLOAD_BUDGET * 3) / 4));
  try {
    const m = measure(dir);
    assert.ok(m.uploadChars > UPLOAD_BUDGET);
    assert.equal(m.withinBudget, false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a small build is within budget and its sizes are reported', () => {
  const dir = fakeBuild(1000);
  try {
    const m = measure(dir);
    assert.equal(m.wasmBytes, 1000);
    assert.equal(m.withinBudget, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// The text module (ADR-015) wraps the same base64 in `module.exports = "...";\n` instead of {"binary":"..."}: 13
// characters more, whatever the size.
test('with the wasm itself as text, the text-module upload is 13 characters larger than the binary one, and it is the one checked', () => {
  for (const n of [0, 1000, 230513]) {
    const dir = fakeBuild(n);
    try {
      const m = measure(dir);
      assert.equal(m.uploadTextChars - m.uploadBinaryChars, 13, `n=${n}`);
      assert.equal(m.uploadChars, m.uploadTextChars);
      assert.equal(m.wasmTextBytes, Buffer.byteLength(wasmModuleText(Buffer.alloc(n))));
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }
});

// ADR-040: the text parts hold the .wasm's DEFLATE stream, so the text upload is what the budget checks, even when the
// .wasm as a binary module would be over it.
test('a build whose text parts hold a DEFLATE stream is checked on the text upload, not the binary one', () => {
  const n = Math.ceil((UPLOAD_BUDGET * 3) / 4);
  const dir = fakeBuild(n);
  try {
    const stream = deflateRawSync(Buffer.alloc(n), { level: 9 });
    writeFileSync(join(dir, partFileName(0)), wasmModuleText(stream));
    const m = measure(dir);
    assert.ok(m.uploadBinaryChars > UPLOAD_BUDGET);
    assert.equal(m.uploadChars, m.uploadTextChars);
    assert.ok(m.uploadChars < UPLOAD_BUDGET);
    assert.equal(m.withinBudget, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a module file above the largest seen intact fails the module cap, and a split build passes it (V257, ADR-028)', () => {
  // 979566 bytes of wasm in one module is the 1306109-byte module of build 614db580737b-421df93f.
  const one = fakeBuild(979566);
  try {
    const m = measure(one);
    assert.equal(m.withinBudget, true);
    assert.deepEqual(m.largestModule, { file: `${WASM_MODULE}.js`, bytes: 1306109 });
    assert.equal(m.moduleCap, MODULE_MAX_BYTES);
    assert.equal(m.withinModuleCap, false);
  } finally {
    rmSync(one, { recursive: true, force: true });
  }
  const two = fakeBuild(979566, { maxChars: PART_MAX_CHARS });
  try {
    const m = measure(two);
    assert.deepEqual(m.parts, [{ file: `${WASM_MODULE}.js`, bytes: 653065 }, { file: `${WASM_MODULE}_1.js`, bytes: 653065 }]);
    assert.equal(m.withinModuleCap, true);
    assert.equal(m.wasmTextBytes, 2 * 653065);
  } finally {
    rmSync(two, { recursive: true, force: true });
  }
  // main.js is a module the link delivers too.
  const big = fakeBuild(10, { main: `exports.loop = function loop() {};\n//${'x'.repeat(MODULE_MAX_BYTES)}\n` });
  try {
    const m = measure(big);
    assert.equal(m.largestModule.file, 'main.js');
    assert.equal(m.withinModuleCap, false);
  } finally {
    rmSync(big, { recursive: true, force: true });
  }
});

test('the text upload counts every part: about 49 characters more for each part after the first', () => {
  for (const n of [1000, 230513]) {
    const one = fakeBuild(n);
    const three = fakeBuild(n, { maxChars: Math.ceil(Math.ceil(n / 3) * 4 / 3 / 4) * 4 });
    try {
      const a = measure(one);
      const b = measure(three);
      assert.equal(b.parts.length, 3, `n=${n}`);
      assert.equal(b.uploadBinaryChars, a.uploadBinaryChars);
      // Each further part adds its key, a comma, a colon and the wrapper with its escaped quotes and newline.
      const perPart = JSON.stringify({ [`${WASM_MODULE}_1`]: 'module.exports = "";\n' }).length - 1;
      assert.equal(b.uploadTextChars - a.uploadTextChars, 2 * perPart, `n=${n}`);
      assert.equal(perPart, 49);
    } finally {
      rmSync(one, { recursive: true, force: true });
      rmSync(three, { recursive: true, force: true });
    }
  }
});

test('a build with a gap in its parts is not measured', () => {
  const dir = fakeBuild(1000, { maxChars: 400 });
  try {
    rmSync(join(dir, `${WASM_MODULE}_1.js`));
    assert.throws(() => measure(dir), /ENOENT/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the module cap admits a module of exactly the largest size seen intact, and one byte more fails (V257, ADR-028)', () => {
  // 679266 bytes of wasm are 905688 characters of base64, a 905709-byte module: the size of f3e7dbd's, which arrived intact.
  const at = fakeBuild(679266);
  const over = fakeBuild(679267);
  const split = fakeBuild(679267, { maxChars: PART_MAX_CHARS });
  const full = fakeBuild(1358532, { maxChars: PART_MAX_CHARS });
  try {
    assert.deepEqual(measure(at).largestModule, { file: `${WASM_MODULE}.js`, bytes: MODULE_MAX_BYTES });
    assert.equal(measure(at).withinModuleCap, true);
    assert.equal(measure(over).largestModule.bytes, MODULE_MAX_BYTES + 4);
    assert.equal(measure(over).withinModuleCap, false);
    assert.equal(measure(split).parts.length, 2);
    assert.equal(measure(split).withinModuleCap, true);
    // Two full parts: the splitter's largest output is exactly the cap.
    assert.deepEqual(measure(full).parts.map((p) => p.bytes), [MODULE_MAX_BYTES, MODULE_MAX_BYTES]);
    assert.equal(measure(full).withinModuleCap, true);
  } finally {
    for (const d of [at, over, split, full]) rmSync(d, { recursive: true, force: true });
  }
});
