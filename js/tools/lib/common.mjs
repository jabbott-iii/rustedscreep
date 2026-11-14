// Shared helpers for the build tools (Node, local only). No game logic.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');

/** Whether the module at `metaUrl` is the script node was started with (symlink-safe). */
export function isMain(metaUrl) {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(fileURLToPath(metaUrl)) === realpathSync(process.argv[1]);
  } catch {
    return false;
  }
}

/** `p` with symlinks resolved: the realpath of its nearest existing ancestor plus the rest. */
export function canonicalPath(p) {
  let head = resolve(p);
  const tail = [];
  while (!existsSync(head)) {
    const up = dirname(head);
    if (up === head) break;
    tail.unshift(basename(head));
    head = up;
  }
  return join(existsSync(head) ? realpathSync(head) : head, ...tail);
}

/** Whether `child` is `parent` or inside it, after resolving symlinks on both. */
export function isInside(child, parent) {
  const rel = relative(canonicalPath(parent), canonicalPath(child));
  return rel === '' || (rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

/** Pinned tool versions (ADR-001; VERIFICATION_LOG V4, V7, V90 and V91). */
export const BINARYEN_VERSION = 133;
export const BINARYEN_DIR = join(ROOT, '.tools', `binaryen-version_${BINARYEN_VERSION}`);

/** The wasm module name: the cdylib's name, which `require()` resolves in game (ADR-009, V19). */
export const WASM_MODULE = 'rustedscreep_wasm';

/** Runs a command, inheriting stdio unless `capture` is set. Never uses a shell. */
export function run(cmd, args, { capture = false, cwd = ROOT, env = process.env } = {}) {
  const res = spawnSync(cmd, args, {
    cwd,
    env,
    encoding: 'utf8',
    stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.error) {
    return { ok: false, status: null, stdout: '', stderr: String(res.error.message) };
  }
  return { ok: res.status === 0, status: res.status, stdout: res.stdout ?? '', stderr: res.stderr ?? '' };
}

export function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

/** The version of `name` in a Cargo.lock text, or null. Fails if the lock holds two versions. */
export function lockVersion(lockText, name) {
  const versions = [];
  for (const block of lockText.split('[[package]]')) {
    const n = block.match(/^name = "([^"]+)"$/m);
    const v = block.match(/^version = "([^"]+)"$/m);
    if (n && v && n[1] === name) versions.push(v[1]);
  }
  if (versions.length > 1) throw new Error(`Cargo.lock has several versions of ${name}: ${versions.join(', ')}`);
  return versions[0] ?? null;
}

/** The version a `wasm-bindgen --version` output reports (its last token), or null. */
export function parseWasmBindgenVersion(stdout) {
  const m = stdout.trim().match(/^wasm-bindgen (\S+)$/);
  return m ? m[1] : null;
}

/** Whether a `wasm-opt --version` output reports `version` ("wasm-opt version 133", optional suffix). */
export function isWasmOptVersion(stdout, version) {
  return new RegExp(`^wasm-opt version ${version}( |$)`).test(stdout.trim());
}

/** The wasm-opt to use: $WASM_OPT, then the project-local install, then PATH. */
export function wasmOptPath() {
  if (process.env.WASM_OPT) return process.env.WASM_OPT;
  const local = join(BINARYEN_DIR, 'bin', 'wasm-opt');
  return existsSync(local) ? local : 'wasm-opt';
}

export function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

export function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}
