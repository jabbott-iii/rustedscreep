// The full validation gate (/check). Runs every row even after a failure (rows that need the build are
// reported as not run when it failed), prints a table, and exits non-zero if any row failed.
// Usage: npm run check. Row order follows .claude/skills/check/SKILL.md.
import { join } from 'node:path';
import { ROOT, isMain, run } from './lib/common.mjs';
import { measure } from './size.mjs';

const BIN = join(ROOT, 'node_modules', '.bin');
const JS_CRATES = /^(wasm-bindgen|js-sys|web-sys) /;

/**
 * JS crates in rustedscreep-core's normal dependency tree, outside the rustedscreep-consts subtree, from
 * `cargo tree --prefix depth` output (each line starts with its depth). ADR-005 lets the facade pull in
 * screeps-game-api and its JS crates; guard-purity.mjs covers the facade's own module boundary.
 */
export function coreJsCrates(treeOutput) {
  const found = [];
  let skipBelow = null;
  for (const line of treeOutput.split('\n')) {
    const m = line.match(/^(\d+)(.*)$/);
    if (!m) continue;
    const depth = Number(m[1]);
    const crate = m[2];
    if (skipBelow !== null && depth > skipBelow) continue;
    skipBelow = null;
    if (crate.startsWith('rustedscreep-consts ')) {
      skipBelow = depth;
      continue;
    }
    if (JS_CRATES.test(crate)) found.push(crate.split(' (')[0]);
  }
  return found;
}

function purity() {
  const res = run('cargo', ['tree', '-p', 'rustedscreep-core', '-e', 'normal', '--prefix', 'depth'], { capture: true });
  if (!res.ok) return { ok: false, detail: res.stderr.trim().split('\n').slice(-3).join(' / ') };
  const js = coreJsCrates(res.stdout);
  return js.length
    ? { ok: false, detail: `rustedscreep-core reaches ${js.join(', ')}` }
    : { ok: true, detail: 'no JS crates outside the rustedscreep-consts subtree (ADR-005)' };
}

// The wasm crate's feature-gated code (trap-test, act-test, profiler) compiles only with its feature, so lint both ways.
function wasmClippy() {
  for (const extra of [[], ['--all-features']]) {
    const args = ['clippy', '-p', 'rustedscreep-wasm', '--target', 'wasm32-unknown-unknown', ...extra, '--', '-D', 'warnings'];
    const res = run('cargo', args, { capture: true });
    if (!res.ok) {
      const tail = `${res.stdout}\n${res.stderr}`.trim().split('\n').filter(Boolean).slice(-3).join(' / ');
      return { ok: false, detail: `${extra.length ? 'all features' : 'default features'}: ${tail}` };
    }
  }
  return { ok: true, detail: 'default and all features' };
}

function size() {
  try {
    const m = measure();
    return { ok: m.withinBudget, detail: `main.js ${m.mainBytes} B, wasm ${m.wasmBytes} B, upload ${m.uploadChars}/${m.budget}` };
  } catch (e) {
    return { ok: false, detail: `no build to measure (${e.message})` };
  }
}

// [name, command or function, args, needsBuild]. The build also runs the sandbox smoke test (build.mjs).
const ROWS = [
  ['1 Format', 'cargo', ['fmt', '--all', '--', '--check']],
  ['2 Clippy, native', 'cargo', ['clippy', '-p', 'rustedscreep-core', '-p', 'rustedscreep-consts', '--all-targets', '--', '-D', 'warnings']],
  ['3 Clippy, wasm', wasmClippy],
  ['4 Native tests', 'cargo', ['test', '-p', 'rustedscreep-core', '-p', 'rustedscreep-consts']],
  ['5 Purity', purity],
  ['6 Tool tests', 'npm', ['run', 'test', '--silent']],
  ['7 Release build + bundle + smoke', 'node', [join(ROOT, 'js', 'tools', 'build.mjs')]],
  ['8 TypeScript', join(BIN, 'tsc'), ['--noEmit', '-p', join(ROOT, 'js')], true],
  ['9 Sizes', size, [], true],
  ['10 Trap recovery (sandbox, both ADR-003 modes)', 'node', [join(ROOT, 'js', 'tools', 'trap-sim.mjs')], true],
  ['11 Act layer (sandbox, act-test build)', 'node', [join(ROOT, 'js', 'tools', 'act-sim.mjs')], true],
  ['12 Profiler (sandbox, profiler build)', 'node', [join(ROOT, 'js', 'tools', 'profiler-sim.mjs')], true],
  ['13 Persistence (sandbox, plain build)', 'node', [join(ROOT, 'js', 'tools', 'persist-sim.mjs')], true],
  ['14 Runtime config (sandbox, plain build)', 'node', [join(ROOT, 'js', 'tools', 'config-sim.mjs')], true],
  ['15 Constants self-check (sandbox, plain build)', 'node', [join(ROOT, 'js', 'tools', 'consts-sim.mjs')], true],
  ['16 Movement (sandbox, plain build)', 'node', [join(ROOT, 'js', 'tools', 'move-sim.mjs')], true],
];

function main() {
  const results = [];
  let buildOk = true;
  for (const [name, cmdOrFn, args, needsBuild] of ROWS) {
    process.stdout.write(`${name} ... `);
    let r;
    if (needsBuild && !buildOk) {
      r = { ok: null, detail: 'not run (the build failed)' };
    } else if (typeof cmdOrFn === 'function') {
      r = cmdOrFn();
    } else {
      const res = run(cmdOrFn, args, { capture: true });
      const tail = `${res.stdout}\n${res.stderr}`.trim().split('\n').filter(Boolean).slice(-3).join(' / ');
      r = { ok: res.ok, detail: res.ok ? '' : tail };
    }
    if (name.startsWith('7 ')) buildOk = r.ok === true;
    console.log(r.ok === null ? 'n/a' : r.ok ? 'pass' : 'FAIL');
    results.push({ name, ...r });
  }

  console.log('\n| # | Check | Result | Detail |\n|---|---|---|---|');
  for (const r of results) {
    const [num, ...rest] = r.name.split(' ');
    const result = r.ok === null ? 'n/a' : r.ok ? 'pass' : 'FAIL';
    console.log(`| ${num} | ${rest.join(' ')} | ${result} | ${(r.detail ?? '').replaceAll('|', '/')} |`);
  }
  const failed = results.filter((r) => r.ok === false).length;
  const notRun = results.filter((r) => r.ok === null).length;
  console.log(failed || notRun ? `\n${failed} checks failed / ${notRun} not run` : '\nAll checks passed');
  process.exit(failed || notRun ? 1 : 0);
}

if (isMain(import.meta.url)) main();
