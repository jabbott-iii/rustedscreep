import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseRoomFixture, roomFixture } from './rooms-fixture.mjs';

const SIZE = 50;

// Row-major digits (V224): a wall at (3, 1), a swamp at (1, 3), wall and swamp at (4, 4).
function terrainString() {
  const t = Array(SIZE * SIZE).fill('0');
  t[1 * SIZE + 3] = '1';
  t[3 * SIZE + 1] = '2';
  t[4 * SIZE + 4] = '3';
  return t.join('');
}

const terrainResponse = (room, terrain = terrainString()) => ({
  ok: 1,
  terrain: [{ _id: 'a', room, terrain, type: 'terrain' }],
});

const objectsResponse = (room) => ({
  ok: 1,
  objects: [
    { _id: 'b', room, type: 'source', x: 40, y: 9, energy: 3000 },
    { _id: 'c', room, type: 'spawn', x: 20, y: 20, user: 'uid-7f3a', name: 'SpawnName-7f3a' },
    { _id: 'd', room, type: 'controller', x: 33, y: 15, level: 4, user: 'uid-7f3a', sign: { text: 'sign-text-7f3a', user: 'uid-7f3a' } },
    { _id: 'e', room, type: 'creep', x: 21, y: 20, user: 'uid-7f3a', name: 'CreepName-7f3a' },
    { _id: 'f', type: 'mineral', x: 12, y: 40, mineralType: 'H', density: 2, room },
    { _id: 'g', room, type: 'source', x: 7, y: 9, energy: 3000 },
  ],
  users: { 'uid-7f3a': { _id: 'uid-7f3a', username: 'player-name-7f3a' } },
});

test('a fixture keeps the terrain and the natural objects in a fixed order', () => {
  const text = roomFixture('E3N53', 'shard3', '2026-11-17', terrainResponse('E3N53'), objectsResponse('E3N53'));
  const parsed = parseRoomFixture(text);
  assert.equal(parsed.room, 'E3N53');
  assert.equal(parsed.shard, 'shard3');
  assert.deepEqual(parsed.controller, { x: 33, y: 15 });
  assert.deepEqual(parsed.sources, [{ x: 7, y: 9 }, { x: 40, y: 9 }]);
  assert.deepEqual(parsed.minerals, [{ type: 'H', x: 12, y: 40 }]);
  assert.equal(parsed.terrain, terrainString());
  // 50 rows of 50 digits after the `terrain` line, row y = 1 holding the wall at x = 3.
  const rows = text.split('\n');
  const first = rows.indexOf('terrain') + 1;
  assert.equal(rows.slice(first, first + SIZE).every((r) => r.length === SIZE), true);
  assert.equal(rows[first + 1][3], '1');
});

test('a fixture drops owners, usernames, structures and creeps', () => {
  const text = roomFixture('E3N53', 'shard3', '2026-11-17', terrainResponse('E3N53'), objectsResponse('E3N53'));
  for (const leak of ['7f3a', 'spawn', 'creep', 'level', 'user', '"']) {
    assert.equal(text.includes(leak), false, leak);
  }
});

test('a response of the wrong shape is refused', () => {
  const ok = [terrainResponse('E3N53'), objectsResponse('E3N53')];
  const refuse = (terrain, objects, room = 'E3N53') =>
    assert.throws(() => roomFixture(room, 'shard3', '2026-11-17', terrain, objects));
  refuse({ ok: 0, terrain: [] }, ok[1]);
  refuse(terrainResponse('E3N54'), ok[1]);
  refuse(terrainResponse('E3N53', terrainString().slice(1)), ok[1]);
  refuse(terrainResponse('E3N53', `4${terrainString().slice(1)}`), ok[1]);
  refuse(ok[0], { ok: 1 });
  refuse(ok[0], { ok: 1, objects: objectsResponse('E3N53').objects.filter((o) => o.type !== 'controller') });
  refuse(ok[0], { ok: 1, objects: objectsResponse('E3N53').objects.filter((o) => o.type !== 'source') });
  refuse(ok[0], { ok: 1, objects: [...ok[1].objects, { type: 'source', room: 'E3N53', x: 50, y: 1 }] });
  refuse(ok[0], { ok: 1, objects: [...ok[1].objects, { type: 'mineral', room: 'E3N53', x: 5, y: 5, mineralType: 'XH' }] });
  refuse(ok[0], { ok: 1, objects: [...ok[1].objects, { type: 'source', room: 'E3N54', x: 5, y: 5 }] });
  refuse(ok[0], ok[1], 'not a room');
  assert.throws(() => roomFixture('E3N53', 'Shard 3', '2026-11-17', ...ok));
  assert.throws(() => roomFixture('E3N53', 'shard3', 'today', ...ok));
});
