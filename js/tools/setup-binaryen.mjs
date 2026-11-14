// Installs the pinned binaryen release (wasm-opt) into the git-ignored .tools/ folder (ADR-001).
// Usage: npm run setup:binaryen. It downloads about 111 MB once, checks the SHA-256 pinned below,
// extracts it, and checks that wasm-opt reports the pinned version. Rerunning it is a no-op.
import { createWriteStream, existsSync, mkdirSync, readFileSync, renameSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { BINARYEN_DIR, BINARYEN_VERSION, ROOT, fail, isWasmOptVersion, run, sha256 } from './lib/common.mjs';

// Release asset and digest for Linux x86_64, read from the release's .sha256 file (VERIFICATION_LOG V91).
// The digest is pinned here rather than fetched, so a changed asset fails instead of being trusted.
const ASSET = `binaryen-version_${BINARYEN_VERSION}-x86_64-linux.tar.gz`;
const URL = `https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/${ASSET}`;
const SHA256 = '2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489';

const wasmOpt = join(BINARYEN_DIR, 'bin', 'wasm-opt');

function installedVersionOk() {
  if (!existsSync(wasmOpt)) return false;
  const res = run(wasmOpt, ['--version'], { capture: true });
  return res.ok && isWasmOptVersion(res.stdout, BINARYEN_VERSION);
}

if (process.platform !== 'linux' || process.arch !== 'x64') {
  fail(`only linux x64 is set up here (this is ${process.platform} ${process.arch}). ` +
    `Install binaryen version_${BINARYEN_VERSION} yourself and set WASM_OPT to its wasm-opt.`);
}

if (installedVersionOk()) {
  console.log(`binaryen version_${BINARYEN_VERSION} is already installed: ${wasmOpt}`);
  process.exit(0);
}

const tools = join(ROOT, '.tools');
mkdirSync(tools, { recursive: true });
const tarball = join(tools, ASSET);
const partial = `${tarball}.partial`;

console.log(`downloading ${URL}`);
const res = await fetch(URL, { redirect: 'follow' });
if (!res.ok || !res.body) fail(`download failed: HTTP ${res.status}`);
await pipeline(Readable.fromWeb(res.body), createWriteStream(partial));

const digest = sha256(readFileSync(partial));
if (digest !== SHA256) {
  rmSync(partial, { force: true });
  fail(`SHA-256 mismatch for ${ASSET}: got ${digest}, expected ${SHA256}. Nothing was installed.`);
}
renameSync(partial, tarball);
console.log(`SHA-256 ok (${digest})`);

rmSync(BINARYEN_DIR, { recursive: true, force: true });
const untar = run('tar', ['-xzf', tarball, '-C', tools]);
if (!untar.ok) fail('tar could not extract the archive');
rmSync(tarball, { force: true });

if (!installedVersionOk()) {
  fail(`after extraction, ${wasmOpt} is missing or does not report version ${BINARYEN_VERSION}`);
}
console.log(`installed binaryen version_${BINARYEN_VERSION}: ${wasmOpt}`);
