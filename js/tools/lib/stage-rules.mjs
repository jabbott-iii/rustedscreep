// Argument rules for npm run stage (js/tools/stage.mjs), kept pure so they can be tested without staging.

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
