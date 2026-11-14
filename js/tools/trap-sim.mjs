// End-to-end recovery test in the Screeps-like sandbox (js/tools/smoke.mjs). For each ADR-003 recovery
// mode it builds a trap-test variant under target/, boots it, schedules 10 traps with the
// `rustedscreep.trapTest(10)` console command, and checks that every trap reaches console.error through
// the panic hook and the polyfill, leads to one recovery (a halt, or a re-instantiation through
// __wbg_reset_state), and that the bot runs again. The P1 gate repeats this on the MMO. npm run check
// runs it; standalone: node js/tools/trap-sim.mjs [halt|reinstantiate].
import { join } from 'node:path';
import { build } from './build.mjs';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { simulate } from './smoke.mjs';

const TRAPS = 10;

export const MODES = ['halt', 'reinstantiate'];

export function trapSim(mode) {
  if (!MODES.includes(mode)) throw new Error(`unknown recovery mode ${JSON.stringify(mode)}; use one of ${MODES.join(', ')}`);
  const dist = join(ROOT, 'target', `trap-test-${mode}`);
  // halt is config.rs's default; reinstantiate is option (b), tested through an override.
  build({ features: ['trap-test'], dist, loaderOverrides: mode === 'halt' ? {} : { recoveryMode: mode } });
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

if (isMain(import.meta.url)) {
  const modes = process.argv[2] ? [process.argv[2]] : MODES;
  for (const mode of modes) {
    try {
      const r = trapSim(mode);
      console.log(`${mode}: ${r.done} (${r.ticks} ticks, ${r.globals} globals)`);
    } catch (e) {
      fail(`trap recovery test failed: ${e.message}`);
    }
  }
}
