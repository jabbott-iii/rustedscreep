// The loader's CRC-32 must equal the stage tool's on the same bytes, or the in-game hash check (V77)
// would report a mismatch for a correct .wasm.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { crc32Hex as loaderCrc } from '../build-test/crc32.js';
import { crc32Hex as toolCrc } from '../tools/lib/crc32.mjs';

test('the loader CRC-32 matches the catalogue check value and the tool', () => {
  // Pinned example: CRC-32/ISO-HDLC check value 0xCBF43926 over "123456789"
  // (https://reveng.sourceforge.io/crc-catalogue/all.htm).
  assert.equal(loaderCrc(Buffer.from('123456789', 'ascii')), 'cbf43926');
  const bytes = Uint8Array.from({ length: 70000 }, (_, i) => (i * 2654435761) >>> 24);
  assert.equal(loaderCrc(bytes), toolCrc(bytes));
});
