// End-to-end recovery test in the Screeps-like sandbox (js/tools/smoke.mjs). For each ADR-003 recovery
// mode it builds a trap-test variant under target/, boots it, schedules 10 traps with the
// `rustedscreep.trapTest(10)` console command, and checks that every trap reaches console.error through
// the panic hook and the polyfill, leads to one recovery (a halt, or a re-instantiation through
// __wbg_reset_state), and that the bot runs again. The P1 gate repeats this on the MMO. A third scenario,
// `repair`, boots the halt build on a raw Memory that does not parse (the loader keeps its record in the heap
// and does not halt, V96), sets the raw Memory to '{}' from the console (as the bot does when a spawn order
// throws on it, ADR-019), then forces one trap: the loader must take its record back into Memory and halt,
// not re-instantiate. npm run check runs all three; standalone: node js/tools/trap-sim.mjs
// [halt|reinstantiate|repair].
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { build } from './build.mjs';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { simulate } from './smoke.mjs';

const TRAPS = 10;

export const MODES = ['halt', 'reinstantiate'];

/** Whether this process already built the halt variant (so `repairSim` reuses that build, not a stale one). */
let haltBuilt = false;

export function trapSim(mode) {
  if (!MODES.includes(mode)) throw new Error(`unknown recovery mode ${JSON.stringify(mode)}; use one of ${MODES.join(', ')}`);
  const dist = join(ROOT, 'target', `trap-test-${mode}`);
  // halt is config.rs's default; reinstantiate is option (b), tested through an override.
  build({ features: ['trap-test'], dist, loaderOverrides: mode === 'halt' ? {} : { recoveryMode: mode } });
  if (mode === 'halt') haltBuilt = true;
  let scheduled = false;
  const world = simulate(dist, {
    ticks: 200,
    command: (w) => {
      if (scheduled || !w.logs.some((l) => l.startsWith('rustedscreep booted in'))) return null;
      scheduled = true;
      return `rustedscreep.trapTest(${TRAPS})`;
    },
    until: (w) => w.logs.some((l) => l.startsWith('rustedscreep trap-test done')),
  });
  const logs = world.logs;
  const done = logs.find((l) => l.startsWith('rustedscreep trap-test done'));
  const problems = [];
  if (!done) problems.push('the trap-test run never finished');
  const wantHalts = mode === 'halt' ? TRAPS : 0;
  if (world.halts !== wantHalts) problems.push(`${world.halts} halts, expected ${wantHalts}`);
  const resets = logs.filter((l) => l.startsWith('rustedscreep recovered by re-instantiating')).length;
  if (resets !== (mode === 'halt' ? 0 : TRAPS)) problems.push(`${resets} re-instantiations`);
  const panics = logs.filter((l) => l.startsWith('[error] panicked at') && l.includes('rustedscreep trap-test: deliberate panic'));
  if (panics.length !== TRAPS) problems.push(`${panics.length} panic messages reached console.error, expected ${TRAPS}`);
  const incidents = logs.filter((l) => l.startsWith('rustedscreep incident:'));
  if (incidents.length !== TRAPS || incidents.some((l) => !l.includes('incident: trap-test'))) {
    problems.push(`incidents: ${incidents.join(' / ')}`);
  }
  if (world.notifies.length !== 1) problems.push(`${world.notifies.length} notifications, expected one summary`);
  if (problems.length) throw new Error(`${mode}: ${problems.join('; ')}\nlogs:\n${logs.join('\n')}`);
  return { mode, done, ticks: world.ticksRun, globals: world.globals };
}

/** The `repair` scenario (see the header): halt mode again once a reset Memory is an object. */
export function repairSim() {
  const dist = join(ROOT, 'target', 'trap-test-halt');
  if (!haltBuilt || !existsSync(join(dist, 'main.js'))) {
    build({ features: ['trap-test'], dist, loaderOverrides: {} });
    haltBuilt = true;
  }
  let step = 0;
  const world = simulate(dist, {
    ticks: 60,
    memoryJson: '{"oops":',
    command: (w) => {
      if (!w.logs.some((l) => l.startsWith('rustedscreep booted in'))) return null;
      step += 1;
      if (step === 1) return "RawMemory.set('{}')";
      if (step === 2) return 'rustedscreep.trapTest(1)';
      return null;
    },
    until: (w) => w.logs.some((l) => l.startsWith('rustedscreep trap-test done')),
  });
  const logs = world.logs;
  const problems = [];
  const unusable = logs.filter((l) => l.startsWith('rustedscreep: Memory is unusable'));
  if (unusable.length !== 1) problems.push(`${unusable.length} "Memory is unusable" lines, expected 1 (the boot's read)`);
  const done = logs.find((l) => l.startsWith('rustedscreep trap-test done'));
  if (!done || !done.includes('1 traps, each recovered')) problems.push(`trap-test run: ${done ?? 'never finished'}`);
  if (world.halts !== 1) problems.push(`${world.halts} halts, expected 1: the repaired Memory holds the record again`);
  const resets = logs.filter((l) => l.startsWith('rustedscreep recovered by re-instantiating')).length;
  if (resets !== 0) problems.push(`${resets} re-instantiations, expected 0`);
  let record = null;
  try {
    record = JSON.parse(world.memoryJson).rustedscreepLoader;
  } catch {
    problems.push(`the stored Memory does not parse: ${world.memoryJson}`);
  }
  if (!record || typeof record !== 'object') problems.push(`no loader record in the stored Memory: ${world.memoryJson}`);
  if (problems.length) throw new Error(`repair: ${problems.join('; ')}\nlogs:\n${logs.join('\n')}`);
  return { mode: 'repair', done, ticks: world.ticksRun, globals: world.globals };
}

if (isMain(import.meta.url)) {
  const arg = process.argv[2];
  const modes = arg ? [arg] : [...MODES, 'repair'];
  for (const mode of modes) {
    try {
      const r = mode === 'repair' ? repairSim() : trapSim(mode);
      console.log(`${mode}: ${r.done} (${r.ticks} ticks, ${r.globals} globals)`);
    } catch (e) {
      fail(`trap recovery test failed: ${e.message}`);
    }
  }
}
