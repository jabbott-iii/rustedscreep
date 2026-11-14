import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FIND_HOSTILE_CREEPS, IDS, packPos, sceneAt } from './world.mjs';

// Worked examples from the engine's formula (VERIFICATION_LOG V103): W1N1 is room (-2, -2), so both
// room bytes are 126 (0x7E); E0S0 is (0, 0), so both are 128 (0x80). Like the engine, packPos returns a
// signed int32, negative once the top bit is set; `| 0` gives the same int32 for an unsigned literal.
test('packPos follows the engine layout', () => {
  assert.equal(packPos('W1N1', 25, 10), 0x7e7e190a);
  assert.equal(packPos('E0S0', 25, 10), 0x8080190a | 0);
  assert.ok(packPos('E0S0', 25, 10) < 0);
  assert.equal(packPos('W0N0', 0, 0), 0x7f7f0000);
  assert.equal(packPos('E127S127', 49, 49), 0xffff3131 | 0);
  assert.throws(() => packPos('sim', 1, 1), /not a room name/);
});

test('the scene churns as its comment says', () => {
  const at3 = sceneAt(3);
  assert.deepEqual(Object.keys(at3.creeps), ['Harvester', 'Scout', 'Scout2', 'Builder']);
  assert.deepEqual(Object.keys(at3.rooms), ['W1N1', 'W2N1', 'E0S0']);
  assert.equal(at3.rooms.E0S0.controller, undefined);
  assert.equal(at3.creeps.Builder.spawning, true);
  assert.deepEqual(at3.rooms.W2N1.find(FIND_HOSTILE_CREEPS).map((c) => c.id), [IDS.invader]);
  assert.equal(at3.rooms.W2N1.controller.my, undefined);
  const at5 = sceneAt(5);
  assert.deepEqual(Object.keys(at5.rooms), ['W1N1', 'E0S0']);
  assert.deepEqual(Object.keys(at5.structures), [IDS.spawn, IDS.controller, IDS.mystery]);
  assert.equal(at5.rooms.W1N1.find(FIND_HOSTILE_CREEPS).length, 0);
});
