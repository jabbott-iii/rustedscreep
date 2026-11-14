// Runs every *.test.mjs under js/tools and js/test with node:test, after compiling the loader's pure
// modules (js/tsconfig.test.json) that js/test imports. Usage: npm run test. Listing the files here keeps
// it working on Node 18 and on later versions, whose --test argument handling differs.
import { readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT, fail, run } from './lib/common.mjs';

function testFiles(dir) {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...testFiles(p));
    else if (name.endsWith('.test.mjs')) out.push(p);
  }
  return out;
}

const BUILD_TEST = join(ROOT, 'js', 'build-test');
rmSync(BUILD_TEST, { recursive: true, force: true });
const tsc = run(join(ROOT, 'node_modules', '.bin', 'tsc'), ['-p', join(ROOT, 'js', 'tsconfig.test.json')]);
if (!tsc.ok) fail('tsc -p js/tsconfig.test.json failed');
// tsc emits ES modules with a .js extension; mark the folder so Node loads them as such.
writeFileSync(join(BUILD_TEST, 'package.json'), '{ "type": "module" }\n');

const files = [...testFiles(join(ROOT, 'js', 'tools')), ...testFiles(join(ROOT, 'js', 'test'))];
const res = run(process.execPath, ['--test', ...files]);
process.exit(res.ok ? 0 : 1);
