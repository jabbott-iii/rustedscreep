// The loader's base64 decoder (ADR-015) against Node's, and the tool's text module (js/tools/lib/wasm-text.mjs), so
// what npm run stage writes is what the loader reads.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { test } from 'node:test';
import { decodeBase64 } from '../build-test/base64.js';
import { wasmModuleText } from '../tools/lib/wasm-text.mjs';

test('every length and every byte value round-trips against Buffer', () => {
  for (let n = 0; n <= 3000; n++) {
    const bytes = n === 256 ? Uint8Array.from({ length: 256 }, (_, i) => i) : randomBytes(n);
    const text = Buffer.from(bytes).toString('base64');
    assert.deepEqual(Buffer.from(decodeBase64(text)), Buffer.from(bytes), `n=${n}`);
  }
});

test('malformed text is refused', () => {
  for (const bad of ['A', 'AA', 'AAA', 'AAAAA', 'AA=A', '=AAA', 'A===', '====', 'AAA-', 'AAA_', 'AAA ', 'AAA\n',
    'AAAé', 'AAĀA', 'AAAA\r\n', 'A=AA']) {
    assert.throws(() => decodeBase64(bad), TypeError, JSON.stringify(bad));
  }
  assert.equal(decodeBase64('').length, 0);
});

test('the loader decodes the text module the stage tool writes', () => {
  const bytes = randomBytes(230513);
  const text = wasmModuleText(bytes);
  const literal = /^module\.exports = "(.*)";\n$/.exec(text)[1];
  assert.deepEqual(Buffer.from(decodeBase64(literal)), bytes);
});
