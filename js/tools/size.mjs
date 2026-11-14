// Reports the bundle and wasm sizes in dist/ and checks the upload budget. Usage: npm run size.
// The budget is the safe default from VERIFICATION_LOG V76: keep JSON.stringify(modules) under 2 MiB,
// with the wasm counted as base64 inside { binary }, the shape of a code upload (V19), until the real
// MMO limit is measured.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, WASM_MODULE, fail, isMain, readJson } from './lib/common.mjs';

export const UPLOAD_BUDGET = 2 * 1024 * 1024;

/** Sizes of the build in `dir`, and the upload estimate against UPLOAD_BUDGET. */
export function measure(dir = join(ROOT, 'dist')) {
  const info = readJson(join(dir, 'build-info.json'));
  const main = readFileSync(join(dir, 'main.js'), 'utf8');
  const wasm = readFileSync(join(dir, `${WASM_MODULE}.wasm`));
  const upload = JSON.stringify({ main, [WASM_MODULE]: { binary: wasm.toString('base64') } }).length;
  return {
    buildId: info.buildId,
    mainBytes: Buffer.byteLength(main, 'utf8'),
    wasmBytes: wasm.length,
    uploadChars: upload,
    budget: UPLOAD_BUDGET,
    withinBudget: upload <= UPLOAD_BUDGET,
  };
}

export function report(m) {
  const pct = ((100 * m.uploadChars) / m.budget).toFixed(1);
  console.log(`build ${m.buildId}`);
  console.log(`  main.js                 ${m.mainBytes} bytes`);
  console.log(`  ${WASM_MODULE}.wasm  ${m.wasmBytes} bytes`);
  console.log(`  upload estimate         ${m.uploadChars} of ${m.budget} characters (${pct}%)`);
}

if (isMain(import.meta.url)) {
  let m;
  try {
    m = measure();
  } catch (e) {
    fail(`no complete build in dist/ (${e.message}); run npm run build first`);
  }
  report(m);
  if (!m.withinBudget) fail('the upload estimate is over budget (V76)');
}
