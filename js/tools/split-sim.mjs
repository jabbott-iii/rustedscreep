// End-to-end test of the wasm split across text modules (ADR-028) in the Screeps-like sandbox (js/tools/smoke.mjs):
// a plain build into target/split-sim whose base64 is cut into three parts (build.mjs's `partMaxChars`, which only
// tests lower; the real cap gives two today). The build's own smoke test boots it in both forms, with `in 3 parts` in
// its boot line. Then, in the text form, the script checks that:
// - the three parts boot on the first tick with a matching CRC-32, and a stale part beyond them (an earlier, larger
//   build's rustedscreep_wasm_3, which throws if evaluated) is never required;
// - each part delivered empty, as 5217308's 1306109-byte module was (V257), is a load incident that names the part
//   (part 0 with the live failure's own text), nothing is compiled, and the loader backs off after backoffIncidents;
// - part 1 missing is an incident naming it, part 2 and part 0 cut short each throw a SyntaxError that the incident
//   names, and part 0 with '=' in its last group (its recorded length) is refused as not the last part.
// The CRC-32 policy for parts of the recorded lengths with other content is in js/test/loader.test.mjs: real wasm
// spliced that way fails to compile here, so it would test the compiler, not the loader.
// npm run check runs it after the build. Standalone: node js/tools/split-sim.mjs.
import { join } from 'node:path';
import { build } from './build.mjs';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { partModuleName } from './lib/wasm-text.mjs';
import { partFiles, simulate } from './smoke.mjs';

const PARTS = 3;
const DIST = join(ROOT, 'target', 'split-sim');
const BACKOFF = /^rustedscreep boot deferred: backing off after 3 consecutive incidents/;

/** The incident lines of `world`. */
const incidents = (world) => world.logs.filter((l) => l.startsWith('rustedscreep incident:'));

function run(textModules, ticks) {
  return simulate(DIST, { ticks, wasmForm: 'base64', textModules });
}

export function splitSim() {
  build({ dist: DIST, partMaxChars: (chars) => Math.ceil(chars / PARTS / 4) * 4 });
  const files = partFiles(DIST);
  const names = Object.keys(files);
  const problems = [];
  if (names.join(',') !== Array.from({ length: PARTS }, (_, k) => partModuleName(k)).join(',')) {
    throw new Error(`the build wrote ${names.join(', ')}, expected ${PARTS} parts`);
  }
  const chars = names.map((n) => /^module\.exports = "(.*)";\n$/.exec(files[n])[1].length);

  // Intact, with a stale fourth part next to it.
  const ok = run({ ...files, [partModuleName(PARTS)]: 'throw new Error("a stale part was required");' }, 3);
  const boot = ok.logs.find((l) => l.startsWith('rustedscreep boot:')) ?? '';
  const total = chars.reduce((a, b) => a + b, 0);
  if (!boot.includes(`wasm String(base64, ${total} chars in ${PARTS} parts)`) || !boot.includes(': match)')) {
    problems.push(`intact: boot line ${JSON.stringify(boot)}`);
  }
  if (!ok.logs.some((l) => l.startsWith('rustedscreep booted in 1 ticks'))) problems.push('intact: did not boot in 1 tick');
  if (incidents(ok).length) problems.push(`intact: ${incidents(ok).join(' / ')}`);

  // Each part delivered empty, as the live failure was: an empty module exports {}.
  for (let k = 0; k < PARTS; k++) {
    const w = run({ ...files, [partModuleName(k)]: '' }, 5);
    const want = k === 0
      ? "TypeError: require('rustedscreep_wasm') returned Object, not bytes"
      : `TypeError: require('${partModuleName(k)}') returned Object, not a ${chars[k]}-character string`;
    const lines = incidents(w);
    if (lines.length !== 3 || !lines.every((l) => l.startsWith('rustedscreep incident: load at tick') && l.endsWith(want))) {
      problems.push(`part ${k} empty: incidents ${JSON.stringify(lines)}, expected 3 ending ${JSON.stringify(want)}`);
    }
    if (!w.logs.some((l) => BACKOFF.test(l))) problems.push(`part ${k} empty: no backoff line`);
    if (w.logs.some((l) => l.startsWith('rustedscreep boot:') || l.startsWith('rustedscreep booted'))) problems.push(`part ${k} empty: it booted`);
  }

  // Part 1 missing, part 2 cut short, part 0 padded in its last group.
  const missing = { ...files };
  delete missing[partModuleName(1)];
  const half = (k) => files[partModuleName(k)].slice(0, Math.floor(files[partModuleName(k)].length / 2));
  const cut = { ...files, [partModuleName(2)]: half(2) };
  const padded0 = files[partModuleName(0)].replace(/....";\n$/, 'AA==";\n');
  const cases = [
    ['part 1 missing', missing, `TypeError: require('${partModuleName(1)}') threw Error: Unknown module '${partModuleName(1)}'`],
    ['part 2 cut', cut, `TypeError: require('${partModuleName(2)}') threw SyntaxError: `],
    ['part 0 cut', { ...files, [partModuleName(0)]: half(0) }, `TypeError: require('${partModuleName(0)}') threw SyntaxError: `],
    ['part 0 padded', { ...files, [partModuleName(0)]: padded0 }, `TypeError: require('${partModuleName(0)}') ends in '=' but is not the last part`],
  ];
  for (const [label, modules, want] of cases) {
    const w = run(modules, 2);
    const lines = incidents(w);
    if (!lines.length || !lines.every((l) => l.includes(want))) problems.push(`${label}: incidents ${JSON.stringify(lines)}, expected ${JSON.stringify(want)}`);
    if (w.logs.some((l) => l.startsWith('rustedscreep boot:'))) problems.push(`${label}: it booted`);
  }
  if (problems.length) throw new Error(problems.join('\n'));
  return { parts: chars, boot };
}

if (isMain(import.meta.url)) {
  try {
    const r = splitSim();
    console.log(`split: ${r.parts.length} parts of ${r.parts.join(', ')} characters; ${r.boot}`);
  } catch (e) {
    fail(`split wasm test failed: ${e.message}`);
  }
}
