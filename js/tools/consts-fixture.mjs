// Writes the engine-constants fixture for the constants self-check's tests (ROADMAP P1 module 11): the
// native test in crates/rustedscreep-core/src/consts_check and the sandbox test js/tools/consts-sim.mjs.
// No game logic.
//
// Input: an open-source engine's @screeps/common lib/constants.js (plain data: `Object.assign(exports, {...})`).
// Output: crates/rustedscreep-core/tests/fixtures/constants-common-<version>.tsv, one line per constant,
// `NAME<TAB>JSON.stringify(value)` in the source's key order, after a `#` header naming the source and its
// SHA-256. That is the text the bot reads per global on the server (crates/rustedscreep-wasm/src/consts_check.rs).
//
// Usage: node js/tools/consts-fixture.mjs <path to lib/constants.js> <version>
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';

export const FIXTURE_DIR = join(ROOT, 'crates', 'rustedscreep-core', 'tests', 'fixtures');

/** The fixture text for the constants object `constants`, with `header` lines (without the `# `). */
export function fixtureText(constants, header) {
  const lines = header.map((h) => `# ${h}`);
  for (const name of Object.keys(constants)) {
    if (!/^[A-Z][A-Z0-9_]*$/.test(name)) throw new Error(`not an upper-case constant name: ${name}`);
    const json = JSON.stringify(constants[name]);
    if (json === undefined) throw new Error(`${name} has no JSON text`);
    // JSON.stringify escapes every control character, so a line never holds a raw tab or newline.
    lines.push(`${name}\t${json}`);
  }
  return `${lines.join('\n')}\n`;
}

/** Parses fixture text back into [name, json] pairs (for the sandbox test). */
export function parseFixture(text) {
  const out = [];
  for (const line of text.split('\n')) {
    if (line === '' || line.startsWith('#')) continue;
    const tab = line.indexOf('\t');
    if (tab <= 0) throw new Error(`bad fixture line: ${line.slice(0, 40)}`);
    out.push([line.slice(0, tab), line.slice(tab + 1)]);
  }
  return out;
}

if (isMain(import.meta.url)) {
  const [source, version] = process.argv.slice(2);
  if (!source || !version || !/^[0-9][0-9A-Za-z.-]*$/.test(version)) {
    fail('usage: node js/tools/consts-fixture.mjs <path to lib/constants.js> <version>');
  }
  const path = resolve(source);
  const sha = createHash('sha256').update(readFileSync(path)).digest('hex');
  const constants = createRequire(import.meta.url)(path);
  const out = join(FIXTURE_DIR, `constants-common-${version}.tsv`);
  // The ISC license asks for its copyright and permission notice in all copies (the package's LICENSE.txt).
  const license = readFileSync(join(dirname(path), '..', 'LICENSE.txt'), 'utf8')
    .split(/\n\s*\n/)
    .slice(0, 2)
    .map((paragraph) => paragraph.replace(/\s+/g, ' ').trim());
  if (license.length !== 2 || !license[0].startsWith('Copyright')) fail('the package has no ISC LICENSE.txt next to lib/');
  writeFileSync(out, fixtureText(constants, [
    'Engine constants for the constants self-check tests (ROADMAP P1 module 11).',
    'One line per constant: NAME<TAB>JSON.stringify(value), in the source key order.',
    `Source: @screeps/common ${version} lib/constants.js, SHA-256 ${sha} (github.com/screeps/common).`,
    `Regenerate: node js/tools/consts-fixture.mjs <path to lib/constants.js> ${version}`,
    'License of the source (ISC):',
    ...license,
  ]));
  console.log(`wrote ${out} (${Object.keys(constants).length} constants)`);
}
