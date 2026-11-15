import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseFeatures } from '../build.mjs';
import { RETIRED_FILES, STAGE_FILES, stageRefusal, unexpectedDeployEntries } from './stage-rules.mjs';

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
  assert.ok(STAGE_FILES.every((f) => f.endsWith('.js')));
  assert.deepEqual(RETIRED_FILES, ['rustedscreep_wasm.wasm']);
});
