// Base64 (RFC 4648 section 4, the standard alphabet, padded) to bytes, for the wasm text module the GitHub link
// delivers (ADR-015: the link uploads files as text, so the wasm travels as base64 in a .js module, V137; ADR-028
// splits it across several, which decodeBase64Into writes into one buffer). Plain JS:
// TextDecoder is undefined on the MMO (V72), and atob and Buffer were never probed there (V95). Strict: a length
// that is not a multiple of 4, any character outside the alphabet, or a misplaced '=' throws. Non-canonical
// trailing bits are not refused; they do not change the bytes, and the loader's CRC-32 covers the content.

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const PAD = 61; // '='
const INVALID = 255;

let table: Uint8Array | undefined;

/** The value of every ASCII code in ALPHABET, INVALID for the rest. */
function lookup(): Uint8Array {
  if (table) return table;
  const t = new Uint8Array(128).fill(INVALID);
  for (let i = 0; i < 64; i++) t[ALPHABET.charCodeAt(i)] = i;
  table = t;
  return t;
}

/** The '=' that end `text` (0, 1 or 2); throws a TypeError when its length is not a multiple of 4. */
function padding(text: string): number {
  const n = text.length;
  if (n % 4 !== 0) throw new TypeError(`base64 text of ${n} characters is not a multiple of 4`);
  return n === 0 || text.charCodeAt(n - 1) !== PAD ? 0 : text.charCodeAt(n - 2) === PAD ? 2 : 1;
}

/** How many bytes padded standard base64 `text` decodes to; throws a TypeError when its length is not a multiple of 4. */
export function decodedLength(text: string): number {
  return (text.length / 4) * 3 - padding(text);
}

/** The bytes that padded standard base64 `text` encodes; throws a TypeError for anything else. */
export function decodeBase64(text: string): Uint8Array {
  const out = new Uint8Array(decodedLength(text));
  decodeBase64Into(text, out, 0);
  return out;
}

/**
 * Decodes padded standard base64 `text` into `out` from `offset`, for a wasm split across text modules (ADR-028),
 * and returns the number of bytes written (`decodedLength(text)`). Throws a TypeError for text that is not base64
 * and a RangeError when `out` has no room for it.
 */
export function decodeBase64Into(text: string, out: Uint8Array, offset: number): number {
  const n = text.length;
  const pad = padding(text);
  const length = (n / 4) * 3 - pad;
  if (offset < 0 || offset + length > out.length) {
    throw new RangeError(`no room for ${length} bytes at offset ${offset} of ${out.length}`);
  }
  const t = lookup();
  const full = pad ? n - 4 : n;
  let o = offset;
  for (let i = 0; i < full; i += 4) {
    const c0 = text.charCodeAt(i);
    const c1 = text.charCodeAt(i + 1);
    const c2 = text.charCodeAt(i + 2);
    const c3 = text.charCodeAt(i + 3);
    // Codes above 127 are outside the table; testing their OR first keeps every lookup in bounds.
    if ((c0 | c1 | c2 | c3) > 127) throw new TypeError(`not base64 at character ${i}`);
    const a = t[c0] as number;
    const b = t[c1] as number;
    const c = t[c2] as number;
    const d = t[c3] as number;
    if ((a | b | c | d) > 63) throw new TypeError(`not base64 at character ${i}`);
    out[o++] = (a << 2) | (b >> 4);
    out[o++] = ((b & 15) << 4) | (c >> 2);
    out[o++] = ((c & 3) << 6) | d;
  }
  if (pad) {
    // The last quantum: two characters and "==" (one byte), or three and "=" (two bytes).
    const i = n - 4;
    const c0 = text.charCodeAt(i);
    const c1 = text.charCodeAt(i + 1);
    const c2 = pad === 1 ? text.charCodeAt(i + 2) : 65; // 'A' = 0
    if ((c0 | c1 | c2) > 127) throw new TypeError(`not base64 at character ${i}`);
    const a = t[c0] as number;
    const b = t[c1] as number;
    const c = t[c2] as number;
    if ((a | b | c) > 63) throw new TypeError(`not base64 at character ${i}`);
    out[o++] = (a << 2) | (b >> 4);
    if (pad === 1) out[o] = ((b & 15) << 4) | (c >> 2);
  }
  return length;
}
