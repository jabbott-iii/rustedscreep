// The loader's base64 decoder (ADR-015) against Node's, and the tool's text module (js/tools/lib/wasm-text.mjs), so
// what npm run stage writes is what the loader reads.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { test } from 'node:test';
import { decodeBase64, decodeBase64Into, decodedLength } from '../build-test/base64.js';
import { splitBase64, textModuleBase64, wasmModuleText, wasmModuleTexts } from '../tools/lib/wasm-text.mjs';

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

test('decodedLength and decodeBase64Into agree with decodeBase64, and refuse what it refuses', () => {
  for (let n = 0; n <= 300; n++) {
    const bytes = randomBytes(n);
    const text = Buffer.from(bytes).toString('base64');
    assert.equal(decodedLength(text), n, `n=${n}`);
    const out = new Uint8Array(n + 5).fill(7);
    assert.equal(decodeBase64Into(text, out, 2), n);
    assert.deepEqual(Buffer.from(out.subarray(2, 2 + n)), Buffer.from(bytes), `n=${n}`);
    // Only its own bytes are written.
    assert.deepEqual([out[0], out[1], out[2 + n], out[3 + n], out[4 + n]], [7, 7, 7, 7, 7], `n=${n}`);
  }
  for (const bad of ['A', 'AA=A', 'AAA-', 'AAAé']) {
    assert.throws(() => decodeBase64Into(bad, new Uint8Array(8), 0), TypeError, JSON.stringify(bad));
  }
  assert.throws(() => decodedLength('AAAAA'), TypeError);
  assert.throws(() => decodeBase64Into('AQID', new Uint8Array(4), 2), RangeError, 'no room');
  assert.throws(() => decodeBase64Into('AQID', new Uint8Array(4), -1), RangeError, 'negative offset');
});

test('split parts decode into one buffer to the same bytes as the whole text (ADR-028)', () => {
  for (const n of [0, 1, 2, 3, 4, 5, 6, 100, 101, 102, 3000]) {
    const bytes = randomBytes(n);
    const whole = Buffer.from(bytes).toString('base64');
    for (const maxChars of [4, 8, 12, 400]) {
      const parts = splitBase64(whole, maxChars);
      const out = new Uint8Array(parts.reduce((sum, p) => sum + decodedLength(p), 0));
      let at = 0;
      for (const p of parts) at += decodeBase64Into(p, out, at);
      assert.equal(at, n, `n=${n} maxChars=${maxChars}`);
      assert.deepEqual(Buffer.from(out), Buffer.from(bytes), `n=${n} maxChars=${maxChars}`);
    }
  }
});

test('the loader decodes the split text modules the stage tool writes', () => {
  const bytes = randomBytes(230513);
  const texts = wasmModuleTexts(bytes, 100000);
  assert.equal(texts.length, 4);
  const parts = texts.map(textModuleBase64);
  const out = new Uint8Array(parts.reduce((sum, p) => sum + decodedLength(p), 0));
  let at = 0;
  for (const p of parts) at += decodeBase64Into(p, out, at);
  assert.deepEqual(Buffer.from(out), bytes);
});
