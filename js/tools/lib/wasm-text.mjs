// The wasm as a CommonJS text module (ADR-015): the GitHub link uploads every file as text, so a .wasm arrives
// mangled under its file name (VERIFICATION_LOG V137); a .js module that exports the bytes as base64 arrives intact
// as module `rustedscreep_wasm`, and the loader decodes it (js/src/base64.ts). This is the only definition of the
// text module's form: build.mjs writes it, stage.mjs checks it, and smoke.mjs serves it. No game logic.

/** One ASCII line: `module.exports = "<padded standard base64>";` and a newline. */
const FORM = /^module\.exports = "([A-Za-z0-9+/]*={0,2})";\n$/;

/** The text module for `bytes`. */
export function wasmModuleText(bytes) {
  return `module.exports = "${Buffer.from(bytes).toString('base64')}";\n`;
}

/** The bytes a wasmModuleText text encodes; throws on any other form or non-canonical base64. */
export function wasmModuleBytes(text) {
  const m = FORM.exec(text);
  if (!m || m[1].length % 4 !== 0) throw new Error('not a wasm text module (module.exports = "<base64>";)');
  const bytes = Buffer.from(m[1], 'base64');
  if (bytes.toString('base64') !== m[1]) throw new Error('the wasm text module is not canonical base64');
  return bytes;
}
