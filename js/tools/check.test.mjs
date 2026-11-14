import assert from 'node:assert/strict';
import { test } from 'node:test';
import { coreJsCrates } from './check.mjs';

// `cargo tree --prefix depth` shapes: today's core, and core once it depends on the facade (ADR-005).
const CORE_ALONE = '0rustedscreep-core v0.1.0 (/repo/crates/rustedscreep-core)\n';
const CORE_WITH_FACADE = [
  '0rustedscreep-core v0.1.0 (/repo/crates/rustedscreep-core)',
  '1rustedscreep-consts v0.1.0 (/repo/crates/rustedscreep-consts)',
  '2screeps-game-api v0.23.5',
  '3js-sys v0.3.107',
  '4wasm-bindgen v0.2.130',
  '1rustc-hash v2.1.3',
].join('\n');

test('core alone and core with the facade have no JS crates outside the facade subtree', () => {
  assert.deepEqual(coreJsCrates(CORE_ALONE), []);
  assert.deepEqual(coreJsCrates(CORE_WITH_FACADE), []);
});

test('a JS crate core reaches directly or through another dependency is reported', () => {
  const direct = `${CORE_WITH_FACADE}\n1wasm-bindgen v0.2.130`;
  const indirect = `${CORE_WITH_FACADE}\n1some-crate v1.0.0\n2js-sys v0.3.107`;
  assert.deepEqual(coreJsCrates(direct), ['wasm-bindgen v0.2.130']);
  assert.deepEqual(coreJsCrates(indirect), ['js-sys v0.3.107']);
});
