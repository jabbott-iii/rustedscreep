// Build-pipeline placeholder (P1 module 3). P1 module 4 replaces it with the real loader: bucket-gated
// boot, trap and abort recovery (ADR-003), the TextEncoder/TextDecoder polyfill and the boot log.
// npm run stage refuses to stage a bundle that still contains PLACEHOLDER_MARKER into deploy/.
import { initSync, loop as wasmLoop } from '../pkg/rustedscreep_wasm.js';

// Replaced with a string literal by rollup.config.mjs; npm run build sets it.
declare const __RUSTEDSCREEP_BUILD_ID__: string;

const BUILD_ID = __RUSTEDSCREEP_BUILD_ID__;
const PLACEHOLDER_MARKER = 'rustedscreep build-pipeline placeholder';
/** The binary module that holds the wasm (ADR-009, V19). */
const WASM_MODULE = 'rustedscreep_wasm';

let ready = false;

export function loop(): void {
  if (!ready) {
    initSync({ module: new WebAssembly.Module(require(WASM_MODULE) as BufferSource) });
    ready = true;
    console.log(`${PLACEHOLDER_MARKER} ${BUILD_ID} loaded`);
  }
  wasmLoop();
}
