// Loads a built bundle in a Screeps-like sandbox and runs a few ticks: CommonJS module/exports, a
// require() that returns only the wasm module's bytes, console.log only, and none of TextDecoder and
// TextEncoder (undefined on the MMO, VERIFICATION_LOG V72) or fetch, URL and document (assumed absent,
// V95). npm run build runs it on every build; standalone: node js/tools/smoke.mjs [dir].
// It is a smoke test of the bundle's shape, not of game behavior.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import vm from 'node:vm';
import { ROOT, WASM_MODULE, fail, isMain } from './lib/common.mjs';

const ABSENT_GLOBALS = ['TextDecoder', 'TextEncoder', 'fetch', 'URL', 'document'];

/** Runs `ticks` calls of the bundle's loop in a fresh context; returns what it logged. */
export function smoke(dir = join(ROOT, 'dist'), ticks = 3) {
  const code = readFileSync(join(dir, 'main.js'), 'utf8');
  const wasmBytes = readFileSync(join(dir, `${WASM_MODULE}.wasm`));
  const logs = [];
  const module = { exports: {} };
  // The context brings its own intrinsics (Object, WebAssembly, Uint8Array), as a Screeps global does.
  // Passing the host's would break the glue's Object.prototype check on initSync's argument.
  const sandbox = vm.createContext({ module, exports: module.exports, console: { log: (...a) => logs.push(a.join(' ')) } });
  const ContextUint8Array = vm.runInContext('Uint8Array', sandbox);
  sandbox.require = (name) => {
    if (name === WASM_MODULE) return new ContextUint8Array(wasmBytes);
    throw new Error(`require(${JSON.stringify(name)}) is not a bundled module`);
  };
  for (const g of ABSENT_GLOBALS) {
    if (vm.runInContext(`typeof ${g}`, sandbox) !== 'undefined') throw new Error(`the sandbox exposes ${g}`);
  }
  vm.runInContext(code, sandbox, { filename: 'main.js' });
  if (typeof module.exports.loop !== 'function') throw new Error('main.js does not export loop');
  for (let t = 0; t < ticks; t++) module.exports.loop();
  return logs;
}

if (isMain(import.meta.url)) {
  try {
    const logs = smoke(process.argv[2]);
    console.log(`3 ticks ran; ${logs.length} log line(s)`);
    for (const l of logs) console.log(`  ${l}`);
  } catch (e) {
    fail(`bundle smoke test failed: ${e.message}`);
  }
}
