// Stages a fresh release build for the MMO GitHub link (ADR-012, ADR-015). Usage:
//   npm run stage                 build, then copy main.js and the wasm text modules (rustedscreep_wasm.js and, for a
//                                 split wasm, rustedscreep_wasm_1.js, …: the .wasm as base64, since the link uploads
//                                 files as text, V137, and delivers no module above MODULE_MAX_BYTES, V257, ADR-028)
//                                 into the tracked deploy/ folder
//   npm run stage -- --out <dir>  the same into <dir> (tests and dry runs); <dir> must be new or empty
//   --features trap-test           a trap-test build, for the P1 gate's recovery test (ADR-003)
//   --features profiler            a profiler build, for the live CPU measurements (ROADMAP P1 module 8)
//   (an act-test build is refused for deploy/: it issues scripted intents and is for the sandbox only)
// It reports the module names, sizes, the hashes of the wasm bytes and the build ID. A .wasm an earlier stage left in
// deploy/ is removed, and so is a wasm part file beyond this build's parts. It never commits or pushes: committing and pushing deploy/ to main is the user's deploy step.
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { build, parseFeatures } from './build.mjs';
import { ROOT, WASM_MODULE, canonicalPath, fail, isInside, sha256 } from './lib/common.mjs';
import { crc32Hex } from './lib/crc32.mjs';
import { RETIRED_FILES, stageFiles, stageRefusal, staleParts, unexpectedDeployEntries } from './lib/stage-rules.mjs';
import { MODULE_MAX_BYTES, wasmPartsBytes } from './lib/wasm-text.mjs';
import { measure, report } from './size.mjs';

const DEPLOY = join(ROOT, 'deploy');
const STAGE_DIST = join(ROOT, 'target', 'stage-dist');
// Writing into the Steam client's script mirror uploads at once (VERIFICATION_LOG V74), so never stage there.
const STEAM_MIRROR = join(homedir(), '.config', 'Screeps');

function parseArgs(argv) {
  const args = { out: null, rest: [] };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--out' && argv[i + 1]) args.out = argv[++i];
    else args.rest.push(argv[i]);
  }
  args.features = parseFeatures(args.rest);
  return args;
}

function isSymlink(p) {
  try {
    return lstatSync(p).isSymbolicLink();
  } catch {
    return false;
  }
}

const args = parseArgs(process.argv.slice(2));
const refusal = stageRefusal(args);
if (refusal) fail(refusal);
if (!args.out && isSymlink(DEPLOY)) fail('deploy/ is a symlink; refusing to stage through it');
// Symlinks are resolved before every check (isInside canonicalizes both sides), so a link cannot slip past them.
const target = canonicalPath(args.out ?? DEPLOY);
if (isInside(target, STEAM_MIRROR)) fail(`refusing to stage into the Steam client's script mirror (${target}; V74)`);
if (args.out && isInside(target, ROOT) && !isInside(target, join(ROOT, 'target'))) {
  fail('--out must be outside the repository (or under target/), so a test stage cannot be committed by mistake');
}
if (existsSync(target)) {
  const entries = readdirSync(target);
  if (args.out && entries.length) fail(`--out ${target} is not empty`);
  if (!args.out) {
    // lstat, so a symlinked entry counts as unexpected instead of being written through or removed.
    const unexpected = unexpectedDeployEntries(entries, (e) => lstatSync(join(target, e)).isFile());
    if (unexpected.length) {
      fail(`deploy/ holds files that are not part of a staged build: ${unexpected.join(', ')}. ` +
        'The GitHub link syncs every top-level file (ADR-012), so remove them by hand first.');
    }
  }
}

// A build of its own, so a concurrent `npm run build` cannot replace dist/ between this build and the copy.
const info = build({ features: args.features, dist: STAGE_DIST });
const size = measure(STAGE_DIST);
if (!size.withinBudget) fail('the upload estimate is over budget (V76); not staging');
if (!size.withinModuleCap) {
  fail(`${size.largestModule.file} is ${size.largestModule.bytes} bytes, above the ${MODULE_MAX_BYTES} the GitHub link has delivered intact (V257, ADR-028); not staging`);
}

// Every file must be the one this build produced, and the text modules must decode to its .wasm, before anything is
// written; the copies are checked again after.
const parts = info.modules[WASM_MODULE].text;
const FILES = stageFiles(parts.length);
const wantSha = { 'main.js': info.modules.main.sha256, ...Object.fromEntries(parts.map((p) => [p.file, p.sha256])) };
const check = (dir, f) => {
  if (sha256(readFileSync(join(dir, f))) !== wantSha[f]) fail(`${join(dir, f)} is not the file this build produced`);
};
for (const f of FILES) check(STAGE_DIST, f);
const staged = wasmPartsBytes(parts.map((p) => readFileSync(join(STAGE_DIST, p.file), 'utf8')));
if (sha256(staged) !== info.modules[WASM_MODULE].sha256) fail("the wasm text modules do not decode to the build's .wasm");

mkdirSync(target, { recursive: true });
for (const f of FILES) copyFileSync(join(STAGE_DIST, f), join(target, f));
for (const f of FILES) check(target, f);
// Only with the new build in place: remove a .wasm an earlier stage left (both forms map to one module name) and any
// part an earlier, larger build left, so the folder holds only the staged files (the link syncs every top-level file).
const retired = [...RETIRED_FILES.filter((f) => existsSync(join(target, f))), ...staleParts(readdirSync(target).sort(), parts.length)];
for (const f of retired) rmSync(join(target, f));
const held = readdirSync(target).sort();
if (held.join(',') !== [...FILES].sort().join(',')) fail(`the staged folder holds ${held.join(', ')}, expected ${FILES.join(', ')}`);

console.log(`\n== staged into ${target}`);
console.log(`modules: main (main.js), ${parts.map((p) => `${p.file.replace(/\.js$/, '')} (${p.file})`).join(', ')}: the .wasm as base64 in ${parts.length} part(s), ADR-015, ADR-028`);
for (const f of retired) console.log(`removed ${f} (${f.endsWith('.wasm') ? 'the GitHub link mangles a .wasm, V137' : 'a wasm part this build does not have'})`);
console.log(`features: ${info.features.length ? info.features.join(', ') : 'none'}`);
report(size);
console.log(`  wasm crc32 ${crc32Hex(staged)}  sha256 ${sha256(staged)}`);
if (!args.out) {
  console.log('\nCommitting and pushing deploy/ to main deploys this build to the live MMO branch (ADR-012).');
  console.log(`After the push: once rustedscreep.status() shows build ${info.buildId}, and if the previous build logged`);
  console.log('incidents, run `delete Memory.rustedscreepLoader` in the console (its backoff streak carries over).');
}
