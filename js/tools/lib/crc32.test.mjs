import assert from 'node:assert/strict';
import { test } from 'node:test';
import { crc32, crc32Hex } from './crc32.mjs';

// Pinned example from a cited source: the catalogue "check" value of CRC-32/ISO-HDLC is 0xCBF43926
// over the ASCII bytes "123456789" (https://reveng.sourceforge.io/crc-catalogue/all.htm).
test('CRC-32 matches the catalogue check value', () => {
  assert.equal(crc32(Buffer.from('123456789', 'ascii')), 0xcbf43926);
  assert.equal(crc32Hex(Buffer.from('123456789', 'ascii')), 'cbf43926');
});

test('CRC-32 of no bytes is 0 and the hex form is zero-padded', () => {
  assert.equal(crc32(new Uint8Array(0)), 0);
  assert.equal(crc32Hex(new Uint8Array(0)), '00000000');
});

test('CRC-32 changes when any byte changes', () => {
  const a = Uint8Array.from({ length: 1024 }, (_, i) => i & 0xff);
  const b = Uint8Array.from(a);
  b[700] ^= 1;
  assert.notEqual(crc32(a), crc32(b));
});
