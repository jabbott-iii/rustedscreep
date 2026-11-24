import assert from 'node:assert/strict';
import { test } from 'node:test';
import { escapeRegExp, isWasmOptVersion, lockVersion, parseWasmBindgenVersion } from './common.mjs';

const LOCK = `
version = 4

[[package]]
name = "wasm-bindgen"
version = "0.2.130"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "wasm-bindgen-macro"
version = "0.2.130"
`;

test('lockVersion finds the exact package, not a prefix match', () => {
  assert.equal(lockVersion(LOCK, 'wasm-bindgen'), '0.2.130');
  assert.equal(lockVersion(LOCK, 'wasm-bindgen-macro'), '0.2.130');
  assert.equal(lockVersion(LOCK, 'missing'), null);
});

test('lockVersion refuses a lock with two versions of one package', () => {
  const two = `${LOCK}\n[[package]]\nname = "wasm-bindgen"\nversion = "0.2.129"\n`;
  assert.throws(() => lockVersion(two, 'wasm-bindgen'), /several versions/);
});

// Output formats observed on this machine (VERIFICATION_LOG V90, V91).
test('wasm-bindgen --version parsing', () => {
  assert.equal(parseWasmBindgenVersion('wasm-bindgen 0.2.130\n'), '0.2.130');
  assert.equal(parseWasmBindgenVersion('something else'), null);
});

test('wasm-opt --version check accepts an optional suffix and rejects other versions', () => {
  assert.ok(isWasmOptVersion('wasm-opt version 133\n', 133));
  assert.ok(isWasmOptVersion('wasm-opt version 133 (version_133)\n', 133));
  assert.ok(!isWasmOptVersion('wasm-opt version 1330\n', 133));
  assert.ok(!isWasmOptVersion('wasm-opt version 132\n', 133));
});

test('escapeRegExp makes every syntax character literal', () => {
  const text = 'a^b$c\\d.e*f+g?h(i)j[k]l{m}n|o';
  for (const flags of ['', 'u']) {
    assert.ok(new RegExp(`^${escapeRegExp(text)}$`, flags).test(text), flags);
  }
  assert.ok(!new RegExp(`^${escapeRegExp('a.c')}$`).test('abc'));
  assert.equal(escapeRegExp('source (10,12) open 8'), 'source \\(10,12\\) open 8');
});
