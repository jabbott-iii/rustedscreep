// UTF-8 TextDecoder and TextEncoder for the wasm-bindgen glue, which constructs them at import time once
// strings cross the boundary (VERIFICATION_LOG V90). They are undefined on the MMO (V72). This follows the
// WHATWG Encoding Standard for UTF-8 only: decode with replacement (fatal mode is not supported), BOM
// handling per ignoreBOM, and encode with lone surrogates replaced by U+FFFD. main.ts imports this module
// before the glue, so Rollup evaluates it first.

const REPLACEMENT = 0xfffd;

type Bytes = ArrayBuffer | ArrayBufferView;

function toUint8(input: Bytes): Uint8Array {
  if (input instanceof Uint8Array) return input;
  if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  return new Uint8Array(input);
}

/** UTF-8 decoder of the WHATWG Encoding Standard (section 9.1.1), with replacement for errors. */
export class Utf8Decoder {
  readonly encoding = 'utf-8';
  readonly fatal = false;
  readonly ignoreBOM: boolean;

  constructor(label = 'utf-8', options: { fatal?: boolean; ignoreBOM?: boolean } = {}) {
    const l = String(label).trim().toLowerCase();
    if (l !== 'utf-8' && l !== 'utf8' && l !== 'unicode-1-1-utf-8') throw new RangeError(`unsupported encoding: ${label}`);
    if (options.fatal) throw new TypeError('fatal decoding is not supported by this polyfill');
    this.ignoreBOM = Boolean(options.ignoreBOM);
  }

  decode(input?: Bytes, options?: { stream?: boolean }): string {
    if (options && options.stream) throw new TypeError('streaming decode is not supported by this polyfill');
    if (input === undefined) return '';
    const bytes = toUint8(input);
    const out: number[] = [];
    let chunks = '';
    let codePoint = 0;
    let needed = 0;
    let seen = 0;
    let lower = 0x80;
    let upper = 0xbf;
    const emit = (cp: number) => {
      if (cp > 0xffff) {
        const v = cp - 0x10000;
        out.push(0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff));
      } else {
        out.push(cp);
      }
      if (out.length >= 8192) {
        chunks += String.fromCharCode(...out);
        out.length = 0;
      }
    };
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i] as number;
      if (needed === 0) {
        if (b <= 0x7f) emit(b);
        else if (b >= 0xc2 && b <= 0xdf) { needed = 1; codePoint = b & 0x1f; }
        else if (b >= 0xe0 && b <= 0xef) {
          if (b === 0xe0) lower = 0xa0;
          if (b === 0xed) upper = 0x9f;
          needed = 2;
          codePoint = b & 0xf;
        } else if (b >= 0xf0 && b <= 0xf4) {
          if (b === 0xf0) lower = 0x90;
          if (b === 0xf4) upper = 0x8f;
          needed = 3;
          codePoint = b & 0x7;
        } else emit(REPLACEMENT);
        continue;
      }
      if (b < lower || b > upper) {
        // Maximal subpart: drop the partial sequence, emit one replacement, and reprocess this byte.
        codePoint = 0; needed = 0; seen = 0; lower = 0x80; upper = 0xbf;
        emit(REPLACEMENT);
        i--;
        continue;
      }
      lower = 0x80;
      upper = 0xbf;
      codePoint = (codePoint << 6) | (b & 0x3f);
      seen++;
      if (seen === needed) {
        emit(codePoint);
        codePoint = 0; needed = 0; seen = 0;
      }
    }
    if (needed !== 0) emit(REPLACEMENT);
    let text = chunks + String.fromCharCode(...out);
    if (!this.ignoreBOM && text.charCodeAt(0) === 0xfeff) text = text.slice(1);
    return text;
  }
}

/** The code point at `i` (a surrogate pair, or U+FFFD for a lone surrogate) and the units it uses. */
function codePointAt(s: string, i: number): [number, number] {
  const cu = s.charCodeAt(i);
  if (cu >= 0xd800 && cu <= 0xdbff) {
    const next = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
    if (next >= 0xdc00 && next <= 0xdfff) return [0x10000 + ((cu - 0xd800) << 10) + (next - 0xdc00), 2];
    return [REPLACEMENT, 1];
  }
  return [cu >= 0xdc00 && cu <= 0xdfff ? REPLACEMENT : cu, 1];
}

function utf8Length(cp: number): number {
  return cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
}

function writeUtf8(cp: number, dest: Uint8Array | number[], at: number): void {
  if (cp <= 0x7f) dest[at] = cp;
  else if (cp <= 0x7ff) {
    dest[at] = 0xc0 | (cp >> 6); dest[at + 1] = 0x80 | (cp & 0x3f);
  } else if (cp <= 0xffff) {
    dest[at] = 0xe0 | (cp >> 12); dest[at + 1] = 0x80 | ((cp >> 6) & 0x3f); dest[at + 2] = 0x80 | (cp & 0x3f);
  } else {
    dest[at] = 0xf0 | (cp >> 18); dest[at + 1] = 0x80 | ((cp >> 12) & 0x3f);
    dest[at + 2] = 0x80 | ((cp >> 6) & 0x3f); dest[at + 3] = 0x80 | (cp & 0x3f);
  }
}

/** UTF-8 encoder of the WHATWG Encoding Standard; lone surrogates become U+FFFD. */
export class Utf8Encoder {
  readonly encoding = 'utf-8';

  encode(input = ''): Uint8Array {
    const s = String(input);
    const out: number[] = [];
    for (let i = 0; i < s.length;) {
      const [cp, units] = codePointAt(s, i);
      writeUtf8(cp, out, out.length);
      i += units;
    }
    return Uint8Array.from(out);
  }

  /** Writes whole code points while they fit; `read` counts UTF-16 units consumed (WHATWG encodeInto). */
  encodeInto(input: string, dest: Uint8Array): { read: number; written: number } {
    const s = String(input);
    let read = 0;
    let written = 0;
    while (read < s.length) {
      const [cp, units] = codePointAt(s, read);
      const len = utf8Length(cp);
      if (written + len > dest.length) break;
      writeUtf8(cp, dest, written);
      written += len;
      read += units;
    }
    return { read, written };
  }
}

/** Which of the two the runtime provided, and which this module installed. */
export const textCodecSource: { TextDecoder: 'native' | 'polyfill'; TextEncoder: 'native' | 'polyfill' } = {
  TextDecoder: 'native',
  TextEncoder: 'native',
};

const g = globalThis as Record<string, unknown>;
if (typeof g.TextDecoder === 'undefined') {
  g.TextDecoder = Utf8Decoder;
  textCodecSource.TextDecoder = 'polyfill';
}
if (typeof g.TextEncoder === 'undefined') {
  g.TextEncoder = Utf8Encoder;
  textCodecSource.TextEncoder = 'polyfill';
}
