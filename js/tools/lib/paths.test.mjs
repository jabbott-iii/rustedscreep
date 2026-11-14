import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { canonicalPath, isInside } from './common.mjs';

function withTree(fn) {
  const base = mkdtempSync(join(tmpdir(), 'rustedscreep-paths-'));
  try {
    mkdirSync(join(base, 'mirror', 'scripts'), { recursive: true });
    mkdirSync(join(base, 'elsewhere'), { recursive: true });
    symlinkSync(join(base, 'mirror', 'scripts'), join(base, 'elsewhere', 'link'));
    return fn(base);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
}

test('a child whose name starts with two dots is inside its parent', () => {
  withTree((base) => {
    assert.ok(isInside(join(base, 'mirror', '..stage'), join(base, 'mirror')));
    assert.ok(isInside(join(base, 'mirror', '...', 'x'), join(base, 'mirror')));
    assert.ok(!isInside(join(base, 'elsewhere'), join(base, 'mirror')));
    assert.ok(isInside(join(base, 'mirror'), join(base, 'mirror')));
  });
});

test('a path that runs through a symlink counts as inside the link target', () => {
  withTree((base) => {
    const viaLink = join(base, 'elsewhere', 'link', 'branch', 'new');
    assert.ok(isInside(viaLink, join(base, 'mirror')));
    assert.equal(canonicalPath(viaLink), join(canonicalPath(join(base, 'mirror', 'scripts')), 'branch', 'new'));
  });
});
