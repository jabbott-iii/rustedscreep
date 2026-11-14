import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseFeatures } from '../build.mjs';
import { stageRefusal } from './stage-rules.mjs';

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
