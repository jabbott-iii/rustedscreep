import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { WASM_MODULE } from './lib/common.mjs';
import { wasmModuleText } from './lib/wasm-text.mjs';
import { UPLOAD_BUDGET, measure } from './size.mjs';

function fakeBuild(wasmBytes) {
  const dir = mkdtempSync(join(tmpdir(), 'rustedscreep-size-'));
  writeFileSync(join(dir, 'build-info.json'), JSON.stringify({ buildId: 'test' }));
  writeFileSync(join(dir, 'main.js'), 'exports.loop = function loop() {};\n');
  writeFileSync(join(dir, `${WASM_MODULE}.wasm`), Buffer.alloc(wasmBytes));
  writeFileSync(join(dir, `${WASM_MODULE}.js`), wasmModuleText(Buffer.alloc(wasmBytes)));
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
test('the text-module upload is 13 characters larger than the binary one, and the larger one is checked', () => {
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
