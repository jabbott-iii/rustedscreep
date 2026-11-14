// CRC-32 (ISO-HDLC, the zlib/PNG CRC): the same algorithm as js/tools/lib/crc32.mjs, so the boot log
// can be compared with what npm run stage reports for the .wasm (VERIFICATION_LOG V77). A tool test checks
// that both give the same value on the same bytes.

let table: Uint32Array | undefined;

function crcTable(): Uint32Array {
  if (table) return table;
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  table = t;
  return t;
}

/** CRC-32 of `bytes` as 8 lowercase hex digits. */
export function crc32Hex(bytes: Uint8Array): string {
  const t = crcTable();
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) c = (t[(c ^ (bytes[i] as number)) & 0xff] as number) ^ (c >>> 8);
  return ((c ^ 0xffffffff) >>> 0).toString(16).padStart(8, '0');
}
