// Release build (ADR-001, ADR-002, ADR-008): cargo build for wasm32, then the wasm-bindgen CLI at the
// exact Cargo.lock version (--target web), then the pinned wasm-opt -O4, then tsc and Rollup, then a
// smoke run in a Screeps-like sandbox, with the wasm as a binary module and as the base64 text module the
// GitHub link delivers (ADR-015). Output: dist/main.js, dist/rustedscreep_wasm.wasm, dist/rustedscreep_wasm.js (the
// .wasm as base64) and dist/build-info.json. Usage: npm run build [-- --features trap-test,profiler].
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  BINARYEN_VERSION, ROOT, WASM_MODULE, fail, isMain, isWasmOptVersion, lockVersion, parseWasmBindgenVersion,
  readJson, run, sha256, wasmOptPath,
} from './lib/common.mjs';
import { bundleProblems } from './lib/bundle-check.mjs';
import { crc32Hex } from './lib/crc32.mjs';
import { wasmModuleBytes, wasmModuleText } from './lib/wasm-text.mjs';
import { smoke } from './smoke.mjs';

export const DIST = join(ROOT, 'dist');
/** Cargo features of rustedscreep-wasm that a build may enable. */
export const KNOWN_FEATURES = ['act-test', 'profiler', 'trap-test'];
const PKG = join(ROOT, 'js', 'pkg');
const TS_OUT = join(ROOT, 'js', 'build');
const BIN = join(ROOT, 'node_modules', '.bin');

// The files wasm-bindgen --target web writes for this crate. Anything else (snippets/, package.json,
// a _bg.js start file) is not handled yet, so the build stops instead of shipping half a module.
const EXPECTED_PKG_FILES = [`${WASM_MODULE}.js`, `${WASM_MODULE}.d.ts`, `${WASM_MODULE}_bg.wasm`, `${WASM_MODULE}_bg.wasm.d.ts`];

function step(name) {
  console.log(`\n== ${name}`);
}

function mustRun(label, cmd, args, opts) {
  const res = run(cmd, args, opts);
  if (!res.ok) fail(`${label} failed (exit ${res.status ?? 'none'})${res.stderr ? `: ${res.stderr.trim()}` : ''}`);
  return res;
}

/** Checks that the wasm-bindgen CLI matches Cargo.lock and that wasm-opt is the pinned binaryen. */
export function checkTools() {
  const want = lockVersion(readFileSync(join(ROOT, 'Cargo.lock'), 'utf8'), 'wasm-bindgen');
  if (!want) fail('Cargo.lock has no wasm-bindgen entry');
  const wb = run('wasm-bindgen', ['--version'], { capture: true });
  const have = wb.ok ? parseWasmBindgenVersion(wb.stdout) : null;
  if (have !== want) {
    fail(`the wasm-bindgen CLI is ${have ?? 'missing'}, but Cargo.lock has ${want}. ` +
      `Install it with: cargo install wasm-bindgen-cli --version ${want} --locked`);
  }
  const wasmOpt = wasmOptPath();
  const wo = run(wasmOpt, ['--version'], { capture: true });
  if (!wo.ok || !isWasmOptVersion(wo.stdout, BINARYEN_VERSION)) {
    fail(`${wasmOpt} is not binaryen version ${BINARYEN_VERSION} (${(wo.stdout || wo.stderr).trim() || 'not found'}). ` +
      'Run: npm run setup:binaryen');
  }
  return { wasmBindgen: want, wasmOpt, wasmOptVersion: wo.stdout.trim() };
}

/** `<commit>[-dirty]`, where dirty means tracked or untracked changes outside deploy/. */
function gitDescription() {
  const sha = run('git', ['rev-parse', '--short=12', 'HEAD'], { capture: true });
  if (!sha.ok) return 'nogit';
  // --untracked-files=normal overrides a status.showUntrackedFiles=no setting, so new files count.
  const status = run('git', ['status', '--porcelain', '--untracked-files=normal', '--', '.', ':(exclude)deploy'], { capture: true });
  const dirty = !status.ok || status.stdout.trim() !== '';
  return `${sha.stdout.trim()}${dirty ? '-dirty' : ''}`;
}

/** `--features a,b` (or repeated) from argv, checked against KNOWN_FEATURES. */
export function parseFeatures(argv) {
  const features = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--features' && argv[i + 1]) features.push(...argv[++i].split(',').filter(Boolean));
    else fail(`unknown argument: ${argv[i]} (usage: [--features ${KNOWN_FEATURES.join(',')}])`);
  }
  const unknown = features.filter((f) => !KNOWN_FEATURES.includes(f));
  if (unknown.length) fail(`unknown feature(s): ${unknown.join(', ')}; known: ${KNOWN_FEATURES.join(', ')}`);
  return [...new Set(features)].sort();
}

/** The loader thresholds from rustedscreep-core's config.rs (LOADER_MMO), as printed by its example. */
function loaderConfig() {
  const res = mustRun('loader config', 'cargo', ['run', '--quiet', '-p', 'rustedscreep-core', '--example', 'loader_config'], { capture: true });
  const config = JSON.parse(res.stdout);
  const want = ['bootBucketMin', 'bootSplitTicks', 'recoveryMode', 'backoffWindowTicks', 'backoffIncidents',
    'backoffBaseTicks', 'backoffMaxTicks', 'notifyGroupMinutes', 'trapTestMax'];
  if (Object.keys(config).sort().join(',') !== [...want].sort().join(',')) fail(`loader config keys differ: ${res.stdout.trim()}`);
  return config;
}

/** The wasm cargo builds, under cargo's real target directory (CARGO_TARGET_DIR and config honoured). */
function cargoWasmPath() {
  const meta = mustRun('cargo metadata', 'cargo', ['metadata', '--format-version', '1', '--no-deps'], { capture: true });
  return join(JSON.parse(meta.stdout).target_directory, 'wasm32-unknown-unknown', 'release', `${WASM_MODULE}.wasm`);
}

/**
 * `loaderOverrides` replaces some loader values from config.rs. Only tests use it (js/tools/trap-sim.mjs runs
 * the reinstantiate mode); the build ID and build-info.json record it.
 */
export function build({ features = [], dist = DIST, loaderOverrides = {} } = {}) {
  // Remove every output first, so a build that fails part way cannot leave an older build to be checked or staged.
  for (const dir of [PKG, TS_OUT, dist]) rmSync(dir, { recursive: true, force: true });

  step('tools');
  const tools = checkTools();
  console.log(`wasm-bindgen ${tools.wasmBindgen}; ${tools.wasmOptVersion} (${tools.wasmOpt})`);

  step('loader config (rustedscreep-core config.rs)');
  const base = loaderConfig();
  const unknownOverrides = Object.keys(loaderOverrides).filter((k) => !(k in base));
  if (unknownOverrides.length) fail(`unknown loader override(s): ${unknownOverrides.join(', ')}`);
  const config = { ...base, ...loaderOverrides };
  console.log(JSON.stringify(config));

  step(`cargo build (release, wasm32-unknown-unknown${features.length ? `, features ${features.join(',')}` : ''})`);
  const featureArgs = features.length ? ['--features', features.join(',')] : [];
  mustRun('cargo build', 'cargo', ['build', '--release', '--target', 'wasm32-unknown-unknown', '-p', 'rustedscreep-wasm', ...featureArgs]);
  const cargoWasm = cargoWasmPath();

  step('wasm-bindgen --target web');
  // --omit-default-module-path drops the glue's `new URL(..., import.meta.url)` default (V90).
  // --experimental-reset-state-function exports __wbg_reset_state, ADR-003's re-instantiation path (V97).
  mustRun('wasm-bindgen', 'wasm-bindgen', [cargoWasm, '--target', 'web', '--out-dir', PKG, '--omit-default-module-path',
    '--experimental-reset-state-function']);
  const pkgFiles = readdirSync(PKG).sort();
  if (pkgFiles.join(',') !== [...EXPECTED_PKG_FILES].sort().join(',')) {
    fail(`unexpected wasm-bindgen output in js/pkg: ${pkgFiles.join(', ')}; expected ${EXPECTED_PKG_FILES.join(', ')}`);
  }

  step(`wasm-opt -O4 (binaryen ${BINARYEN_VERSION})`);
  mkdirSync(dist, { recursive: true });
  const distWasm = join(dist, `${WASM_MODULE}.wasm`);
  // No --enable-* flags: wasm-opt reads the target_features section that strip="debuginfo" keeps (V81, V91).
  mustRun('wasm-opt', tools.wasmOpt, [join(PKG, `${WASM_MODULE}_bg.wasm`), '-O4', '-o', distWasm]);
  const wasmBytes = readFileSync(distWasm);
  const wasmCrc = crc32Hex(wasmBytes);
  // The text module the GitHub link can carry (ADR-015), checked to decode to the exact .wasm.
  const wasmText = wasmModuleText(wasmBytes);
  if (!wasmModuleBytes(wasmText).equals(wasmBytes)) fail('the wasm text module does not decode to the .wasm');
  writeFileSync(join(dist, `${WASM_MODULE}.js`), wasmText);
  const overrideTag = Object.keys(loaderOverrides).length ? `+override(${Object.entries(loaderOverrides).map(([k, v]) => `${k}=${v}`).join(',')})` : '';
  const buildId = `${gitDescription()}-${wasmCrc}${features.map((f) => `+${f}`).join('')}${overrideTag}`;
  const buildValue = { id: buildId, wasmCrc32: wasmCrc, features };

  step('tsc (ES2022)');
  mustRun('tsc', join(BIN, 'tsc'), ['-p', join(ROOT, 'js')]);

  step('rollup (CommonJS)');
  const mainPath = join(dist, 'main.js');
  mustRun('rollup', join(BIN, 'rollup'), ['-c', join(ROOT, 'rollup.config.mjs'), '--file', mainPath], {
    env: { ...process.env, RUSTEDSCREEP_BUILD: JSON.stringify(buildValue), RUSTEDSCREEP_LOADER_CONFIG: JSON.stringify(config) },
  });
  const mainText = readFileSync(mainPath, 'utf8');
  if (!/^exports\.loop = loop;$/m.test(mainText)) fail('dist/main.js does not export loop as exports.loop');
  const problems = bundleProblems(mainText, WASM_MODULE);
  if (problems.length) fail(`dist/main.js contains code Screeps cannot run: ${problems.join(', ')}`);
  if (!mainText.includes(JSON.stringify(buildId))) fail('dist/main.js does not contain the build ID');

  step('smoke test (Screeps-like sandbox)');
  for (const wasmForm of ['binary', 'base64']) {
    try {
      const result = smoke(dist, { features, wasmForm });
      console.log(`${wasmForm}: ${result.ticks} ticks ran; booted on tick ${result.bootedAt}; ${result.logs.length} log line(s)`);
    } catch (e) {
      fail(`bundle smoke test (${wasmForm} wasm module) failed: ${e.message}`);
    }
  }

  const mainBytes = Buffer.from(mainText, 'utf8');
  const info = {
    buildId,
    features,
    loaderConfig: config,
    loaderOverrides,
    tools: {
      wasmBindgen: tools.wasmBindgen,
      wasmOpt: tools.wasmOptVersion,
      typescript: readJson(join(ROOT, 'node_modules', 'typescript', 'package.json')).version,
      rollup: readJson(join(ROOT, 'node_modules', 'rollup', 'package.json')).version,
    },
    modules: {
      main: { file: 'main.js', bytes: mainBytes.length, sha256: sha256(mainBytes) },
      [WASM_MODULE]: {
        file: `${WASM_MODULE}.wasm`, bytes: wasmBytes.length, sha256: sha256(wasmBytes), crc32: wasmCrc,
        text: { file: `${WASM_MODULE}.js`, bytes: Buffer.byteLength(wasmText), sha256: sha256(Buffer.from(wasmText)) },
      },
    },
  };
  writeFileSync(join(dist, 'build-info.json'), `${JSON.stringify(info, null, 2)}\n`);

  step('done');
  console.log(`build ${buildId}`);
  console.log(`  main.js             ${mainBytes.length} bytes`);
  console.log(`  ${WASM_MODULE}.wasm  ${wasmBytes.length} bytes, crc32 ${wasmCrc}, sha256 ${info.modules[WASM_MODULE].sha256}`);
  console.log(`  ${WASM_MODULE}.js    ${Buffer.byteLength(wasmText)} bytes (the .wasm as base64, ADR-015)`);
  return info;
}

if (isMain(import.meta.url)) build({ features: parseFeatures(process.argv.slice(2)) });
