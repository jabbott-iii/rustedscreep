import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseFeatures } from '../build.mjs';
import { RETIRED_FILES, stageFiles, stageRefusal, staleParts, unexpectedDeployEntries } from './stage-rules.mjs';

test('an act-test build is refused for deploy/ in any spelling, and allowed into --out', () => {
  for (const argv of [['--features', 'act-test'], ['--features', 'trap-test,act-test'], ['--features', 'act-test', '--features', 'act-test']]) {
    const features = parseFeatures(argv);
    assert.match(stageRefusal({ features }), /refusing to stage a act-test build into deploy\//, argv.join(' '));
    assert.equal(stageRefusal({ out: '/tmp/x', features }), null);
  }
});

test('builds without a sandbox-only feature may be staged', () => {
  assert.equal(stageRefusal({ features: [] }), null);
  assert.equal(stageRefusal({ features: parseFeatures(['--features', 'trap-test']) }), null);
  assert.equal(stageRefusal({ features: parseFeatures(['--features', 'profiler']) }), null);
  assert.equal(stageRefusal({ features: parseFeatures(['--features', 'profiler,trap-test']) }), null);
  assert.equal(stageRefusal(), null);
});

test('deploy/ may hold the staged files and the retired .wasm, nothing else, and only plain files', () => {
  const isFile = () => true;
  assert.deepEqual(unexpectedDeployEntries(['main.js', 'rustedscreep_wasm.js', 'rustedscreep_wasm.wasm'], isFile), []);
  assert.deepEqual(unexpectedDeployEntries(['main.js', 'foo.js', 'README.md'], isFile), ['foo.js', 'README.md']);
  assert.deepEqual(unexpectedDeployEntries(['rustedscreep_wasm.wasm'], (e) => e !== 'rustedscreep_wasm.wasm'), ['rustedscreep_wasm.wasm']);
  // The link mangles a .wasm (V137), so a stage never writes one.
  for (const n of [1, 2, 3]) assert.ok(stageFiles(n).every((f) => f.endsWith('.js')));
  assert.deepEqual(RETIRED_FILES, ['rustedscreep_wasm.wasm']);
});

test('a staged build holds main.js and its wasm parts, numbered from rustedscreep_wasm.js (ADR-028)', () => {
  assert.deepEqual(stageFiles(1), ['main.js', 'rustedscreep_wasm.js']);
  assert.deepEqual(stageFiles(3), ['main.js', 'rustedscreep_wasm.js', 'rustedscreep_wasm_1.js', 'rustedscreep_wasm_2.js']);
  for (const bad of [0, -1, 1.5, undefined]) assert.throws(() => stageFiles(bad), /at least one wasm part/);
  const isFile = () => true;
  assert.deepEqual(unexpectedDeployEntries(['main.js', 'rustedscreep_wasm.js', 'rustedscreep_wasm_1.js', 'rustedscreep_wasm_7.js'], isFile), []);
  assert.deepEqual(unexpectedDeployEntries(['rustedscreep_wasm_0.js', 'rustedscreep_wasm_01.js', 'rustedscreep_wasm_1.wasm'], isFile),
    ['rustedscreep_wasm_0.js', 'rustedscreep_wasm_01.js', 'rustedscreep_wasm_1.wasm']);
  assert.deepEqual(unexpectedDeployEntries(['rustedscreep_wasm_1.js'], () => false), ['rustedscreep_wasm_1.js']);
});

test('a stage removes the parts an earlier, larger build left, and only those', () => {
  const held = ['main.js', 'rustedscreep_wasm.js', 'rustedscreep_wasm_1.js', 'rustedscreep_wasm_2.js', 'rustedscreep_wasm.wasm'];
  assert.deepEqual(staleParts(held, 3), []);
  assert.deepEqual(staleParts(held, 2), ['rustedscreep_wasm_2.js']);
  // The rollback to one module (d3a81da) must not keep a part: the link would upload it (ADR-012, V76).
  assert.deepEqual(staleParts(held, 1), ['rustedscreep_wasm_1.js', 'rustedscreep_wasm_2.js']);
  assert.deepEqual(staleParts(['rustedscreep_wasm_10.js', 'rustedscreep_wasm_9.js'], 10), ['rustedscreep_wasm_10.js']);
});
