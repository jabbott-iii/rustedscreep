import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CARRY_CAPACITY, CREEP_METHODS, FIND_HOSTILE_CREEPS, IDS, SPAWN_ENERGY_CAPACITY, findFiltered, makeStore, packPos, sceneAt } from './world.mjs';

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

test('creep methods record their calls and return the scripted value', () => {
  const actions = { calls: [], returns: { transfer: -8, upgradeController: () => undefined, pull: () => { throw new Error('boom'); } } };
  const { creeps, structures } = sceneAt(1, actions);
  const h = creeps.Harvester;
  for (const m of CREEP_METHODS) assert.equal(typeof h[m], 'function', m);
  assert.equal(h.move(3), 0);
  assert.equal(h.transfer(structures[IDS.spawn], 'energy', undefined), -8);
  assert.equal(h.upgradeController(structures[IDS.controller]), undefined);
  assert.throws(() => h.pull(creeps.Scout), /boom/);
  assert.deepEqual(actions.calls, [
    ['move', 'Harvester', 3],
    ['transfer', 'Harvester', 'Spawn1', 'energy', '<undefined>'],
    ['upgradeController', 'Harvester', 'controller'],
    ['pull', 'Harvester', 'Scout'],
  ]);
  // Without a recorder the methods still return OK, so the plain smoke world takes intents.
  assert.equal(sceneAt(1).creeps.Harvester.say('hi', false), 0);
});

// The engine's Store per object kind (VERIFICATION_LOG V196): a creep's pool from its CARRY parts, a spawn's
// per-resource capacity, a tombstone with neither, and a creep with no CARRY part (capacity 0 reads null).
test('makeStore answers like the engine Store', () => {
  const creep = makeStore({ energy: 20 }, { capacity: 2 * CARRY_CAPACITY });
  assert.deepEqual([creep.getUsedCapacity('energy'), creep.getCapacity('energy'), creep.getFreeCapacity('energy')], [20, 2 * CARRY_CAPACITY, 2 * CARRY_CAPACITY - 20]);
  assert.equal(creep.energy, 20);
  assert.equal(creep.H, 0, 'a resource it does not hold reads 0');
  assert.equal(creep.nonsense, undefined);
  assert.deepEqual(Object.keys(makeStore({ energy: 0 }, { capacity: CARRY_CAPACITY })), [], 'zero entries are not own properties');
  const spawn = makeStore({ energy: 100 }, { resourceCapacity: { energy: SPAWN_ENERGY_CAPACITY } });
  assert.deepEqual([spawn.getCapacity('energy'), spawn.getCapacity(), spawn.getUsedCapacity('energy'), spawn.getFreeCapacity('energy')], [SPAWN_ENERGY_CAPACITY, null, 100, SPAWN_ENERGY_CAPACITY - 100]);
  assert.deepEqual([spawn.getUsedCapacity('H'), spawn.getFreeCapacity('H')], [null, null]);
  const tombstone = makeStore({ energy: 30 });
  assert.deepEqual([tombstone.getUsedCapacity('energy'), tombstone.getCapacity('energy'), tombstone.getFreeCapacity('energy'), tombstone.getUsedCapacity()], [30, null, null, 30]);
  assert.equal(makeStore({}).getUsedCapacity('energy'), 0);
  const noCarry = makeStore({}, { capacity: 0 });
  assert.deepEqual([noCarry.getUsedCapacity('energy'), noCarry.getCapacity('energy'), noCarry.getFreeCapacity('energy')], [0, null, null]);
});

// Room.find's opts.filter (V199): lodash's _.filter with a matcher object, a function or a property name.
test('findFiltered applies the filter like the engine', () => {
  const list = [{ structureType: 'container', pos: { x: 1 } }, { structureType: 'road', pos: { x: 2 } }, { structureType: 'container', pos: { x: 3 }, my: true }];
  const all = findFiltered(list, undefined);
  assert.deepEqual(all, list);
  assert.notEqual(all, list, 'a copy');
  assert.deepEqual(findFiltered(list, { filter: { structureType: 'container' } }), [list[0], list[2]]);
  assert.deepEqual(findFiltered(list, { filter: { pos: { x: 2 } } }), [list[1]], 'a partial deep match');
  assert.deepEqual(findFiltered(list, { filter: (o) => o.pos.x > 1 }), [list[1], list[2]]);
  assert.deepEqual(findFiltered(list, { filter: 'my' }), [list[2]]);
  assert.equal(findFiltered(list, { filter: { structureType: 'container' } })[0], list[0], 'the same objects');
});

test('the scene is shaped for the opening reads and keeps it inert', () => {
  const { creeps, rooms, structures } = sceneAt(3);
  assert.equal(creeps.Harvester.store.getUsedCapacity('energy'), 0);
  assert.equal(creeps.Harvester.store.getCapacity('energy'), CARRY_CAPACITY);
  assert.equal(structures[IDS.spawn].store.getFreeCapacity('energy'), 0);
  assert.equal(rooms.W1N1.energyAvailable, rooms.W1N1.energyCapacityAvailable);
  const c = structures[IDS.controller];
  assert.ok(c.progress === 0 && c.progressTotal > 0 && c.ticksToDowngrade > 0);
  // FIND_SOURCES and FIND_STRUCTURES with the opening's container filter give nothing.
  assert.deepEqual(rooms.W1N1.find(105), []);
  assert.deepEqual(rooms.W1N1.find(107, { filter: { structureType: 'container' } }), []);
  assert.ok(Object.keys(creeps).every((n) => !/^w[WE]\d+[NS]\d+-/.test(n)), 'no bot worker name');
});

// The extension placement's call (ADR-020): recorded, the scripted code returned, nothing placed.
test('rooms record createConstructionSite and place nothing', () => {
  const actions = { calls: [], sites: [], returns: { createConstructionSite: -7 } };
  const { rooms } = sceneAt(1, actions);
  assert.equal(rooms.W1N1.createConstructionSite(23, 23, 'extension'), -7);
  assert.equal(sceneAt(1).rooms.W1N1.createConstructionSite(23, 23, 'extension'), 0);
  assert.deepEqual(actions.sites, [['W1N1', 23, 23, 'extension']]);
  assert.deepEqual(actions.calls, []);
  assert.deepEqual(sceneAt(2, actions).rooms.W1N1.find(114), []);
});
