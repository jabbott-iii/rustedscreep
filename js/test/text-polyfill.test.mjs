// The polyfill must match the WHATWG UTF-8 decoder and encoder; Node's built-in TextDecoder and
// TextEncoder implement the same standard and serve as the reference.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { Utf8Decoder, Utf8Encoder } from '../build-test/text-polyfill.js';

// Deterministic PRNG (mulberry32), so failures reproduce.
function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const EDGE_BYTES = [
  [], [0x41], [0xc3, 0xa9], [0xe2, 0x82, 0xac], [0xf0, 0x9f, 0x98, 0x80],
  [0xc0, 0x80], [0xc1, 0xbf], [0xe0, 0x80, 0x80], [0xed, 0xa0, 0x80], [0xf4, 0x90, 0x80, 0x80], [0xf5], [0xff],
  [0xe2, 0x82], [0xf0, 0x9f, 0x98], [0x80], [0xbf, 0x41], [0xe2, 0x41, 0xac], [0xf0, 0x9f, 0x41],
  [0xef, 0xbb, 0xbf, 0x41], [0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf],
];

test('decode matches Node on edge cases, with and without ignoreBOM', () => {
  for (const ignoreBOM of [true, false]) {
    const ours = new Utf8Decoder('utf-8', { ignoreBOM });
    const node = new TextDecoder('utf-8', { ignoreBOM });
    for (const b of EDGE_BYTES) assert.equal(ours.decode(Uint8Array.from(b)), node.decode(Uint8Array.from(b)), `bytes ${b}`);
  }
});

test('decode matches Node on random bytes, biased toward multi-byte sequences', () => {
  const rnd = prng(0x5eed);
  const ours = new Utf8Decoder('utf-8', { ignoreBOM: true });
  const node = new TextDecoder('utf-8', { ignoreBOM: true });
  const interesting = [0x00, 0x7f, 0x80, 0xbf, 0xc0, 0xc2, 0xdf, 0xe0, 0xed, 0xef, 0xf0, 0xf4, 0xf5, 0xff];
  for (let n = 0; n < 2000; n++) {
    const len = Math.floor(rnd() * 24);
    const bytes = Uint8Array.from({ length: len }, () => (rnd() < 0.5 ? interesting[Math.floor(rnd() * interesting.length)] : Math.floor(rnd() * 256)));
    assert.equal(ours.decode(bytes), node.decode(bytes), `bytes ${Array.from(bytes)}`);
  }
});

test('decode accepts a subarray view, a DataView and an ArrayBuffer, and no argument', () => {
  const backing = new TextEncoder().encode('xx héllo €😀 yy');
  const view = backing.subarray(3, backing.length - 3);
  const ours = new Utf8Decoder('utf-8', { ignoreBOM: true });
  assert.equal(ours.decode(view), new TextDecoder().decode(view));
  assert.equal(ours.decode(new DataView(view.buffer, view.byteOffset, view.byteLength)), new TextDecoder().decode(view));
  assert.equal(ours.decode(view.slice().buffer), new TextDecoder().decode(view));
  assert.equal(ours.decode(), '');
});

test('a long input decodes in chunks without hitting argument limits', () => {
  const text = 'é€😀a'.repeat(20000);
  assert.equal(new Utf8Decoder().decode(new TextEncoder().encode(text)), text);
});

test('encode matches Node, including lone surrogates and astral characters', () => {
  const ours = new Utf8Encoder();
  const node = new TextEncoder();
  const cases = ['', 'ascii', 'héllo', '€', '😀', '\ud800', '\udc00', 'a\ud800b', '􏿿', '😀\ud83d'];
  for (const s of cases) assert.deepEqual(ours.encode(s), node.encode(s), JSON.stringify(s));
  const rnd = prng(42);
  for (let n = 0; n < 2000; n++) {
    const s = String.fromCharCode(...Array.from({ length: Math.floor(rnd() * 12) }, () => Math.floor(rnd() * 0x10000)));
    assert.deepEqual(ours.encode(s), node.encode(s), JSON.stringify(s));
  }
});

test('encodeInto matches Node for every destination size, including split surrogate pairs', () => {
  const ours = new Utf8Encoder();
  const node = new TextEncoder();
  for (const s of ['', 'abc', 'é€😀', 'a😀b\ud800c', '\udfff😀😀']) {
    for (let size = 0; size <= 14; size++) {
      const a = new Uint8Array(size);
      const b = new Uint8Array(size);
      assert.deepEqual(ours.encodeInto(s, a), node.encodeInto(s, b), `${JSON.stringify(s)} into ${size}`);
      assert.deepEqual(a, b, `${JSON.stringify(s)} into ${size}`);
    }
  }
});

test('unsupported options fail loudly instead of decoding wrongly', () => {
  assert.throws(() => new Utf8Decoder('latin1'), RangeError);
  assert.throws(() => new Utf8Decoder('utf-8', { fatal: true }), TypeError);
  assert.throws(() => new Utf8Decoder().decode(new Uint8Array(1), { stream: true }), TypeError);
});
