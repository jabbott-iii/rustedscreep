// Stages a fresh release build for the MMO GitHub link (ADR-012). Usage:
//   npm run stage                 build, then copy main.js and the .wasm into the tracked deploy/ folder
//   npm run stage -- --out <dir>  the same into <dir> (tests and dry runs); <dir> must be new or empty
// It reports the module names, sizes, the .wasm hashes and the build ID. It never commits or pushes:
// committing and pushing deploy/ to main is the user's deploy step.
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { build, DIST } from './build.mjs';
import { ROOT, WASM_MODULE, canonicalPath, fail, isInside, sha256 } from './lib/common.mjs';
import { crc32Hex } from './lib/crc32.mjs';
import { measure, report } from './size.mjs';

const DEPLOY = join(ROOT, 'deploy');
const FILES = ['main.js', `${WASM_MODULE}.wasm`];
// Writing into the Steam client's script mirror uploads at once (VERIFICATION_LOG V74), so never stage there.
const STEAM_MIRROR = join(homedir(), '.config', 'Screeps');
const PLACEHOLDER_MARKER = 'rustedscreep build-pipeline placeholder';

function parseArgs(argv) {
  const args = { out: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--out' && argv[i + 1]) args.out = argv[++i];
    else fail(`unknown argument: ${argv[i]} (usage: npm run stage [-- --out <dir>])`);
  }
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
    // lstat, so a symlinked entry counts as unexpected instead of being written through.
    const unexpected = entries.filter((e) => !FILES.includes(e) || !lstatSync(join(target, e)).isFile());
    if (unexpected.length) {
      fail(`deploy/ holds files that are not part of a staged build: ${unexpected.join(', ')}. ` +
        'The GitHub link syncs every top-level file (ADR-012), so remove them by hand first.');
    }
  }
}

const info = build();
const mainText = readFileSync(join(DIST, 'main.js'), 'utf8');
if (!args.out && mainText.includes(PLACEHOLDER_MARKER)) {
  fail('dist/main.js is the build-pipeline placeholder (P1 module 3), not the loader; it must not reach deploy/');
}
const size = measure(DIST);
if (!size.withinBudget) fail('the upload estimate is over budget (V76); not staging');

mkdirSync(target, { recursive: true });
for (const f of FILES) copyFileSync(join(DIST, f), join(target, f));
const staged = readFileSync(join(target, `${WASM_MODULE}.wasm`));
if (sha256(staged) !== info.modules[WASM_MODULE].sha256) fail('the staged .wasm differs from the build');

console.log(`\n== staged into ${target}`);
console.log(`modules: main (main.js), ${WASM_MODULE} (${WASM_MODULE}.wasm)`);
report(size);
console.log(`  wasm crc32 ${crc32Hex(staged)}  sha256 ${sha256(staged)}`);
if (!args.out) {
  console.log('\nCommitting and pushing deploy/ to main deploys this build to the live MMO branch (ADR-012).');
}
