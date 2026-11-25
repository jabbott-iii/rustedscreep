// The wasm as CommonJS text modules (ADR-015, ADR-028): the GitHub link uploads every file as text, so a .wasm arrives
// mangled under its file name (VERIFICATION_LOG V137); a .js module that exports the bytes as base64 arrives intact
// as module `rustedscreep_wasm`, and the loader decodes it (js/src/base64.ts). The bytes are the .wasm's raw DEFLATE
// stream since ADR-040 (the upload must fit the MMO's 2 MB cap), which the loader inflates (js/src/inflate.ts); these
// helpers carry any bytes. A module larger than the link
// delivers arrived empty (V257), so the base64 is split across `rustedscreep_wasm`, `rustedscreep_wasm_1`, … whenever
// one module would pass MODULE_MAX_BYTES. This is the only definition of the text form: build.mjs writes it, stage.mjs
// checks it, size.mjs measures it, and smoke.mjs serves it. No game logic.
import { WASM_MODULE } from './common.mjs';

/** One ASCII line: `module.exports = "<padded standard base64>";` and a newline. */
const FORM = /^module\.exports = "([A-Za-z0-9+/]*={0,2})";\n$/;

/** The bytes a text module adds around its base64: `module.exports = "` and `";` and a newline. */
export const WRAPPER_BYTES = Buffer.byteLength('module.exports = "";\n');

/**
 * The largest module file the GitHub link has delivered intact (V257, V259; last verified 2026-11-18): the
 * 905709-byte text module of build f3e7dbd63bfe-cee9042d. A 1306109-byte one arrived empty. Raise it only after a
 * larger module has arrived intact. Every staged file, main.js included, must be at most this size (ADR-028).
 */
export const MODULE_MAX_BYTES = 905709;

/** The most base64 characters one part holds: MODULE_MAX_BYTES less the wrapper, in whole 4-character groups. */
export const PART_MAX_CHARS = Math.floor((MODULE_MAX_BYTES - WRAPPER_BYTES) / 4) * 4;

/** The module that holds part `k`: `rustedscreep_wasm`, then `rustedscreep_wasm_1`, … (js/src/loader.ts's partModuleName). */
export function partModuleName(k) {
  return k === 0 ? WASM_MODULE : `${WASM_MODULE}_${k}`;
}

/** The file of part `k` in dist/ and deploy/; the link names a module after its file without `.js` (V140). */
export function partFileName(k) {
  return `${partModuleName(k)}.js`;
}

/** The part index of a part file's name, or -1 for any other name. */
export function partIndex(fileName) {
  const m = new RegExp(`^${WASM_MODULE}(?:_([1-9][0-9]*))?\\.js$`).exec(fileName);
  return m ? (m[1] === undefined ? 0 : Number(m[1])) : -1;
}

/**
 * `base64` cut into as few parts as hold at most `maxChars` characters each, of equal length within 4 characters
 * (whole 4-character groups, so each part is base64 on its own and only the last can end in '=').
 */
export function splitBase64(base64, maxChars = PART_MAX_CHARS) {
  if (base64.length % 4 !== 0) throw new Error(`base64 of ${base64.length} characters is not a multiple of 4`);
  if (!Number.isInteger(maxChars) || maxChars < 4 || maxChars % 4 !== 0) throw new Error(`a part must hold a positive multiple of 4 characters, not ${maxChars}`);
  const groups = base64.length / 4;
  const count = Math.max(1, Math.ceil(base64.length / maxChars));
  const base = Math.floor(groups / count);
  const extra = groups % count;
  const parts = [];
  let at = 0;
  for (let k = 0; k < count; k++) {
    const chars = (base + (k < extra ? 1 : 0)) * 4;
    parts.push(base64.slice(at, at + chars));
    at += chars;
  }
  return parts;
}

/** The text module that exports `base64`. */
export function textModule(base64) {
  return `module.exports = "${base64}";\n`;
}

/** The text module for `bytes`, in one part (the form of ADR-015). */
export function wasmModuleText(bytes) {
  return textModule(Buffer.from(bytes).toString('base64'));
}

/** The text modules for `bytes`, part 0 first, each at most `maxChars` characters of base64 (ADR-028). */
export function wasmModuleTexts(bytes, maxChars = PART_MAX_CHARS) {
  return splitBase64(Buffer.from(bytes).toString('base64'), maxChars).map(textModule);
}

/** The base64 a text module exports; throws on any other form. */
export function textModuleBase64(text) {
  const m = FORM.exec(text);
  if (!m || m[1].length % 4 !== 0) throw new Error('not a wasm text module (module.exports = "<base64>";)');
  return m[1];
}

/** The bytes a wasmModuleText text encodes; throws on any other form or non-canonical base64. */
export function wasmModuleBytes(text) {
  return wasmPartsBytes([text]);
}

/**
 * The bytes that wasmModuleTexts texts encode, part 0 first; throws on any other form, on '=' in a part that is not
 * the last, or on non-canonical base64.
 */
export function wasmPartsBytes(texts) {
  if (!texts.length) throw new Error('no wasm text parts');
  const parts = texts.map(textModuleBase64);
  parts.forEach((p, k) => {
    if (k < parts.length - 1 && p.endsWith('=')) throw new Error(`wasm text part ${k} ends in '=' but is not the last`);
  });
  const base64 = parts.join('');
  const bytes = Buffer.from(base64, 'base64');
  if (bytes.toString('base64') !== base64) throw new Error('the wasm text module is not canonical base64');
  return bytes;
}
