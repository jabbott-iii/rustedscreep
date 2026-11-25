// The loader's raw DEFLATE decoder (ADR-040) against Node's zlib, which the build compresses with: every block type
// zlib writes, every level and strategy, empty and large inputs, the release wasm when a build is present, and the
// malformed streams it must refuse.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { constants, deflateRawSync } from 'node:zlib';
import { inflateRaw } from '../build-test/inflate.js';

const ROOT = join(import.meta.dirname, '..', '..');

/** Compressible bytes: words from a small vocabulary with runs, as code and text are. */
function texty(n, seed = 1) {
  const words = ['local', 'get', 'i32', 'call', 'br_if', 'end', 'block', 'loop', '\n', '    ', 'rustedscreep', '0'];
  let x = seed;
  const out = [];
  while (out.length < n) {
    x = (x * 1103515245 + 12345) >>> 0;
    for (const c of Buffer.from(words[x % words.length])) out.push(c);
  }
  return Uint8Array.from(out.slice(0, n));
}

function roundTrip(bytes, options, what) {
  const stream = deflateRawSync(bytes, options);
  const out = inflateRaw(stream, bytes.length);
  assert.ok(Buffer.from(out).equals(Buffer.from(bytes)), `${what}: ${bytes.length} bytes, ${stream.length} compressed`);
}

test('every level, every strategy, and sizes from 0 up round-trip against zlib', () => {
  const strategies = [constants.Z_DEFAULT_STRATEGY, constants.Z_FILTERED, constants.Z_HUFFMAN_ONLY, constants.Z_RLE, constants.Z_FIXED];
  for (const n of [0, 1, 2, 3, 10, 100, 257, 1000, 4096, 65535, 65536, 70000]) {
    for (let level = 0; level <= 9; level++) {
      for (const strategy of strategies) {
        roundTrip(texty(n, n + level), { level, strategy }, `texty n=${n} level=${level} strategy=${strategy}`);
      }
    }
  }
});

test('random bytes (stored blocks), runs (long distances and lengths), and all byte values round-trip', () => {
  for (const n of [1, 5, 1000, 65535, 65536, 200000]) roundTrip(randomBytes(n), { level: 9 }, `random n=${n}`);
  roundTrip(new Uint8Array(300000).fill(7), { level: 9 }, 'one run');
  const span = Uint8Array.from({ length: 256 * 40 }, (_, i) => i & 255);
  roundTrip(span, { level: 9 }, 'every byte value');
  // A pattern repeated at the window's far end: the longest distances.
  const far = new Uint8Array(70000);
  far.set(texty(300, 9), 0);
  far.set(texty(300, 9), 32768 - 300 + 10);
  roundTrip(far, { level: 9, windowBits: 15 }, 'far distances');
});

test('the release wasm round-trips at the build\'s settings', { skip: !existsSync(join(ROOT, 'dist', 'rustedscreep_wasm.wasm')) }, () => {
  const wasm = readFileSync(join(ROOT, 'dist', 'rustedscreep_wasm.wasm'));
  roundTrip(wasm, { level: 9 }, 'dist/rustedscreep_wasm.wasm');
});

test('a stream that inflates to another length is refused', () => {
  const bytes = texty(5000);
  const stream = deflateRawSync(bytes, { level: 9 });
  assert.throws(() => inflateRaw(stream, bytes.length - 1), /inflates past 4999 bytes/);
  assert.throws(() => inflateRaw(stream, bytes.length + 1), /inflates to 5000 bytes, not 5001/);
  assert.throws(() => inflateRaw(stream, -1), RangeError);
  assert.throws(() => inflateRaw(stream, 1.5), RangeError);
});

test('a cut, padded-out, or malformed stream is refused', () => {
  const bytes = texty(20000);
  const stream = deflateRawSync(bytes, { level: 9 });
  // Cut anywhere: an error, never a wrong result.
  for (const at of [0, 1, 2, 10, stream.length >> 1, stream.length - 2, stream.length - 1]) {
    assert.throws(() => inflateRaw(stream.subarray(0, at), bytes.length), TypeError, `cut at ${at}`);
  }
  // A block of type 3.
  assert.throws(() => inflateRaw(Uint8Array.of(0b111), 0), /a block of type 3/);
  // A stored block whose length check fails, and one that ends early.
  assert.throws(() => inflateRaw(Uint8Array.of(0b001, 2, 0, 0xfd, 0xfe, 9, 9), 2), /length check fails/);
  assert.throws(() => inflateRaw(Uint8Array.of(0b001, 5, 0, 0xfa, 0xff, 9), 5), /stored block ends early/);
  // A fixed block whose first code is a distance with nothing before it: length 3 (symbol 257, code 0000001), then
  // distance 1 (symbol 0, code 00000).
  assert.throws(() => inflateRaw(Uint8Array.of(0b00000011, 0b00000010, 0, 0), 3), /distance before the start/);
  // Flipped bits in the middle: refused or caught by the length check, never a crash of another kind.
  for (let i = 0; i < 200; i++) {
    const bad = Uint8Array.from(stream);
    const at = 5 + ((i * 7919) % (bad.length - 10));
    bad[at] ^= 1 << (i % 8);
    try {
      const out = inflateRaw(bad, bytes.length);
      assert.equal(out.length, bytes.length);
    } catch (e) {
      assert.ok(e instanceof TypeError, `flip ${i}: ${e}`);
    }
  }
});
