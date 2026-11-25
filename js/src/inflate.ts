// Raw DEFLATE (RFC 1951) to bytes, for the wasm text modules the GitHub link delivers (ADR-040): the build compresses
// the wasm with Node's zlib (raw, level 9) before the base64, so the upload fits the MMO's 2 MB code cap (V76, V370),
// and the MMO's runtime has no decompression of its own (V373). Plain JS, after zlib's puff: one lookup table per
// Huffman code, built per block, indexed by the next `bits` input bits (DEFLATE packs codes from the least
// significant bit, so each table entry is filled at the code's bit-reversed index and every index it prefixes).
// Strict: an invalid block type, a stored block whose length check fails, an over-subscribed code or one with no
// end-of-block code, input bits that match no code, a length or distance symbol out of range, a distance before the
// output's start, output past or short of `size`, and a stream that reads past its end each throw a TypeError. The
// loader's CRC-32 then covers the content.

const MAXBITS = 15;
/** Length symbols 257 to 285: each length's base and its extra bits. */
const LENGTH_BASE = Uint16Array.of(3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
  115, 131, 163, 195, 227, 258);
const LENGTH_EXTRA = Uint8Array.of(0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0);
/** Distance symbols 0 to 29: each distance's base and its extra bits. */
const DIST_BASE = Uint16Array.of(1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
  2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577);
const DIST_EXTRA = Uint8Array.of(0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
  13, 13);
/** The order in which a dynamic block lists its code-length code's lengths. */
const ORDER = Uint8Array.of(16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15);
/** Zero bytes a stream may be read ahead past its end while its last code is looked up; more is a cut stream. */
const LOOKAHEAD = 4;

/** A decoding table: entry `(symbol << 4) | length` for each `bits`-bit window, -1 where no code matches. */
interface Code {
  table: Int32Array;
  bits: number;
}

let fixed: { lit: Code; dist: Code } | undefined;

function fail(what: string): never {
  throw new TypeError(`not a raw DEFLATE stream: ${what}`);
}

/** The canonical Huffman code (RFC 1951 section 3.2.2) of the first `n` of `lengths` (0: the symbol is unused). */
function build(lengths: Uint8Array, n: number): Code {
  const count = new Uint16Array(MAXBITS + 1);
  let max = 0;
  for (let s = 0; s < n; s++) {
    const l = lengths[s] as number;
    count[l] = (count[l] as number) + 1;
    if (l > max) max = l;
  }
  count[0] = 0;
  let left = 1;
  for (let b = 1; b <= MAXBITS; b++) {
    left = (left << 1) - (count[b] as number);
    if (left < 0) fail('an over-subscribed code');
  }
  // An incomplete code is legal (RFC 1951 allows one distance code); its unused windows stay -1.
  const bits = Math.max(max, 1);
  const size = 1 << bits;
  const table = new Int32Array(size).fill(-1);
  const next = new Uint16Array(MAXBITS + 1);
  let code = 0;
  for (let b = 1; b <= MAXBITS; b++) {
    code = (code + (count[b - 1] as number)) << 1;
    next[b] = code;
  }
  for (let s = 0; s < n; s++) {
    const l = lengths[s] as number;
    if (l === 0) continue;
    let c = next[l] as number;
    next[l] = c + 1;
    let reversed = 0;
    for (let i = 0; i < l; i++) {
      reversed = (reversed << 1) | (c & 1);
      c >>>= 1;
    }
    const entry = (s << 4) | l;
    for (let j = reversed; j < size; j += 1 << l) table[j] = entry;
  }
  return { table, bits };
}

/** The fixed codes of a type-1 block (RFC 1951 section 3.2.6), built once. */
function fixedCodes(): { lit: Code; dist: Code } {
  if (fixed) return fixed;
  const lit = new Uint8Array(288);
  lit.fill(8, 0, 144);
  lit.fill(9, 144, 256);
  lit.fill(7, 256, 280);
  lit.fill(8, 280, 288);
  fixed = { lit: build(lit, 288), dist: build(new Uint8Array(30).fill(5), 30) };
  return fixed;
}

/**
 * The `size` bytes that the raw DEFLATE stream `input` inflates to (ADR-040). Throws a TypeError for a stream that is
 * malformed, cut short, or that inflates to any other length.
 */
export function inflateRaw(input: Uint8Array, size: number): Uint8Array {
  if (!Number.isInteger(size) || size < 0) throw new RangeError(`cannot inflate to ${size} bytes`);
  const out = new Uint8Array(size);
  const end = input.length;
  let op = 0;
  let ip = 0;
  let bitbuf = 0;
  let bitcnt = 0;

  /** Makes at least `n` bits available, reading zero bytes past the end for a lookahead only. */
  const need = (n: number): void => {
    while (bitcnt < n) {
      if (ip >= end + LOOKAHEAD) fail('it ends early');
      bitbuf |= (ip < end ? (input[ip] as number) : 0) << bitcnt;
      ip++;
      bitcnt += 8;
    }
  };
  const bits = (n: number): number => {
    need(n);
    const v = bitbuf & ((1 << n) - 1);
    bitbuf >>>= n;
    bitcnt -= n;
    return v;
  };
  const decode = (c: Code): number => {
    need(c.bits);
    const e = c.table[bitbuf & ((1 << c.bits) - 1)] as number;
    if (e < 0) fail('bits that match no code');
    const l = e & 15;
    bitbuf >>>= l;
    bitcnt -= l;
    return e >> 4;
  };

  for (;;) {
    const last = bits(1);
    const type = bits(2);
    if (type === 0) {
      // A stored block starts at the next byte: drop the partial byte, and give back the whole bytes read ahead.
      ip -= bitcnt >> 3;
      bitbuf = 0;
      bitcnt = 0;
      if (ip + 4 > end) fail('a stored block ends early');
      const len = (input[ip] as number) | ((input[ip + 1] as number) << 8);
      const nlen = (input[ip + 2] as number) | ((input[ip + 3] as number) << 8);
      ip += 4;
      if (len !== (~nlen & 0xffff)) fail('a stored block whose length check fails');
      if (ip + len > end) fail('a stored block ends early');
      if (op + len > size) fail(`it inflates past ${size} bytes`);
      out.set(input.subarray(ip, ip + len), op);
      ip += len;
      op += len;
    } else if (type === 3) {
      fail('a block of type 3');
    } else {
      let lit: Code;
      let dist: Code;
      if (type === 1) {
        ({ lit, dist } = fixedCodes());
      } else {
        const nlen = bits(5) + 257;
        const ndist = bits(5) + 1;
        const ncode = bits(4) + 4;
        if (nlen > 286 || ndist > 30) fail('a dynamic block with too many codes');
        const clen = new Uint8Array(19);
        for (let i = 0; i < ncode; i++) clen[ORDER[i] as number] = bits(3);
        const lencode = build(clen, 19);
        const total = nlen + ndist;
        const lengths = new Uint8Array(total);
        for (let i = 0; i < total;) {
          const sym = decode(lencode);
          if (sym < 16) {
            lengths[i++] = sym;
            continue;
          }
          let repeat: number;
          let value = 0;
          if (sym === 16) {
            if (i === 0) fail('a repeat with no length before it');
            value = lengths[i - 1] as number;
            repeat = 3 + bits(2);
          } else if (sym === 17) {
            repeat = 3 + bits(3);
          } else {
            repeat = 11 + bits(7);
          }
          if (i + repeat > total) fail('code lengths that run past their count');
          lengths.fill(value, i, i + repeat);
          i += repeat;
        }
        if (lengths[256] === 0) fail('a dynamic block with no end-of-block code');
        lit = build(lengths.subarray(0, nlen), nlen);
        dist = build(lengths.subarray(nlen), ndist);
      }
      for (;;) {
        const sym = decode(lit);
        if (sym < 256) {
          if (op >= size) fail(`it inflates past ${size} bytes`);
          out[op++] = sym;
          continue;
        }
        if (sym === 256) break;
        const k = sym - 257;
        if (k >= 29) fail('a length symbol out of range');
        const len = (LENGTH_BASE[k] as number) + bits(LENGTH_EXTRA[k] as number);
        const d = decode(dist);
        if (d >= 30) fail('a distance symbol out of range');
        const back = (DIST_BASE[d] as number) + bits(DIST_EXTRA[d] as number);
        if (back > op) fail('a distance before the start of the output');
        if (op + len > size) fail(`it inflates past ${size} bytes`);
        for (let i = 0; i < len; i++) {
          out[op] = out[op - back] as number;
          op++;
        }
      }
    }
    if (last) break;
  }
  // Every bit used must be the stream's own: the lookahead's zero bytes may only be left unread.
  if (ip * 8 - bitcnt > end * 8) fail('it ends early');
  if (op !== size) fail(`it inflates to ${op} bytes, not ${size}`);
  return out;
}
