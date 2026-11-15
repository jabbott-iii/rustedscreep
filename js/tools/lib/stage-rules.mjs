// Argument and folder rules for npm run stage (js/tools/stage.mjs), kept pure so they can be tested without staging.
import { WASM_MODULE } from './common.mjs';

/**
 * What a staged build in deploy/ holds (ADR-015): the loader bundle, and the wasm as a base64 text module, since the
 * GitHub link uploads every file as text (VERIFICATION_LOG V137). Each becomes the module of its name without `.js`.
 */
export const STAGE_FILES = ['main.js', `${WASM_MODULE}.js`];

/** Files an earlier stage wrote that a stage now removes: the .wasm, which the link mangles (V137). */
export const RETIRED_FILES = [`${WASM_MODULE}.wasm`];

/**
 * The entries of deploy/ that are not part of a staged build: anything but STAGE_FILES and RETIRED_FILES, and any of
 * those that is not a plain file (`isFile(name)`; a symlink must not be written through).
 */
export function unexpectedDeployEntries(entries, isFile) {
  return entries.filter((e) => !(STAGE_FILES.includes(e) || RETIRED_FILES.includes(e)) || !isFile(e));
}

/**
 * Features that exist only for sandbox tests. An act-test build issues scripted intents (moves, transfers,
 * pulls, a drop) every tick, so it must never reach the live account through deploy/ and the GitHub link.
 */
export const SANDBOX_ONLY_FEATURES = ['act-test'];

/** Why staging with these arguments must stop, or null. `out` is the --out directory (null for deploy/). */
export function stageRefusal({ out = null, features = [] } = {}) {
  const sandboxOnly = features.filter((f) => SANDBOX_ONLY_FEATURES.includes(f));
  if (!out && sandboxOnly.length) {
    return `refusing to stage a ${sandboxOnly.join(', ')} build into deploy/: it is for the sandbox only`;
  }
  return null;
}
