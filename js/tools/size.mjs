// Reports the bundle and wasm sizes in dist/ and checks the upload budget and the size of each module. Usage: npm run
// size. The budget is the safe default from VERIFICATION_LOG V76: keep JSON.stringify(modules) under 2 MiB until the
// real MMO limit is measured. Both upload shapes are counted and the larger is checked: the wasm as base64 inside
// { binary } (an API upload, V19) and as the text modules the GitHub link delivers (ADR-015, ADR-028). Each file the
// link delivers must also be at most MODULE_MAX_BYTES, the largest module seen arrive intact (V257, ADR-028).
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, WASM_MODULE, fail, isMain, readJson } from './lib/common.mjs';
import { MODULE_MAX_BYTES, partFileName, partIndex, partModuleName } from './lib/wasm-text.mjs';

export const UPLOAD_BUDGET = 2 * 1024 * 1024;

/** Sizes of the build in `dir`, and the upload estimate against UPLOAD_BUDGET. */
export function measure(dir = join(ROOT, 'dist')) {
  const info = readJson(join(dir, 'build-info.json'));
  const main = readFileSync(join(dir, 'main.js'), 'utf8');
  const wasm = readFileSync(join(dir, `${WASM_MODULE}.wasm`));
  // The wasm's text parts, part 0 first, which must be numbered without a gap (ADR-028).
  const count = readdirSync(dir).filter((f) => partIndex(f) >= 0).length;
  const texts = Array.from({ length: count }, (_, k) => readFileSync(join(dir, partFileName(k)), 'utf8'));
  if (!texts.length) throw new Error(`no ${partFileName(0)}`);
  const parts = texts.map((t, k) => ({ file: partFileName(k), bytes: Buffer.byteLength(t, 'utf8') }));
  const uploadBinaryChars = JSON.stringify({ main, [WASM_MODULE]: { binary: wasm.toString('base64') } }).length;
  const uploadTextChars = JSON.stringify({ main, ...Object.fromEntries(texts.map((t, k) => [partModuleName(k), t])) }).length;
  const upload = Math.max(uploadBinaryChars, uploadTextChars);
  const mainBytes = Buffer.byteLength(main, 'utf8');
  const largestModule = [{ file: 'main.js', bytes: mainBytes }, ...parts].reduce((a, b) => (b.bytes > a.bytes ? b : a));
  return {
    buildId: info.buildId,
    mainBytes,
    wasmBytes: wasm.length,
    wasmTextBytes: parts.reduce((sum, p) => sum + p.bytes, 0),
    parts,
    uploadBinaryChars,
    uploadTextChars,
    uploadChars: upload,
    budget: UPLOAD_BUDGET,
    withinBudget: upload <= UPLOAD_BUDGET,
    largestModule,
    moduleCap: MODULE_MAX_BYTES,
    withinModuleCap: largestModule.bytes <= MODULE_MAX_BYTES,
  };
}

export function report(m) {
  const pct = ((100 * m.uploadChars) / m.budget).toFixed(1);
  console.log(`build ${m.buildId}`);
  console.log(`  main.js                 ${m.mainBytes} bytes`);
  console.log(`  ${WASM_MODULE}.wasm  ${m.wasmBytes} bytes`);
  for (const p of m.parts) console.log(`  ${p.file.padEnd(22)}  ${p.bytes} bytes (the .wasm as base64, part of ${m.parts.length}, ADR-015, ADR-028)`);
  console.log(`  upload estimate         ${m.uploadChars} of ${m.budget} characters (${pct}%; binary ${m.uploadBinaryChars}, text ${m.uploadTextChars})`);
  console.log(`  largest module          ${m.largestModule.file} ${m.largestModule.bytes} of ${m.moduleCap} bytes (V257, ADR-028)`);
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
  if (!m.withinModuleCap) fail(`${m.largestModule.file} is above the ${m.moduleCap} bytes the GitHub link has delivered intact (V257, ADR-028)`);
}
