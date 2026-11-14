// CRC-32 (ISO-HDLC, the zlib/PNG CRC; reflected polynomial 0xEDB88320). npm run stage reports it
// for the .wasm so the in-game loader (P1 module 4) can compute the same value over the bytes that
// require() returns, without any crypto API (VERIFICATION_LOG V77).
const TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

/** CRC-32 of `bytes` (a Uint8Array or Buffer) as an unsigned 32-bit number. */
export function crc32(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) c = TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** CRC-32 as 8 lowercase hex digits. */
export function crc32Hex(bytes) {
  return crc32(bytes).toString(16).padStart(8, '0');
}
