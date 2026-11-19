// Argument and folder rules for npm run stage (js/tools/stage.mjs), kept pure so they can be tested without staging.
import { WASM_MODULE } from './common.mjs';
import { partFileName, partIndex } from './wasm-text.mjs';

/**
 * What a staged build in deploy/ holds (ADR-015, ADR-028): the loader bundle, and the wasm as base64 text modules
 * (`rustedscreep_wasm.js`, then `rustedscreep_wasm_1.js`, … for a split wasm, `parts` in all), since the GitHub link
 * uploads every file as text (VERIFICATION_LOG V137). Each becomes the module of its name without `.js`.
 */
export function stageFiles(parts) {
  if (!Number.isInteger(parts) || parts < 1) throw new Error(`a staged build has at least one wasm part, not ${parts}`);
  return ['main.js', ...Array.from({ length: parts }, (_, k) => partFileName(k))];
}

/** Files an earlier stage wrote that a stage now removes: the .wasm, which the link mangles (V137). */
export const RETIRED_FILES = [`${WASM_MODULE}.wasm`];

/**
 * The wasm part files among `entries` that a build of `parts` parts does not write (an earlier, larger build's), which
 * a stage removes: the link syncs every top-level file (ADR-012), and a stray part counts toward the code limit (V76).
 */
export function staleParts(entries, parts) {
  return entries.filter((e) => partIndex(e) >= parts);
}

/**
 * The entries of deploy/ that are not part of a staged build: anything but main.js, a wasm part file and
 * RETIRED_FILES, and any of those that is not a plain file (`isFile(name)`; a symlink must not be written through).
 */
export function unexpectedDeployEntries(entries, isFile) {
  return entries.filter((e) => !(e === 'main.js' || partIndex(e) >= 0 || RETIRED_FILES.includes(e)) || !isFile(e));
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
