// Movement glue end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in dist/
// (crates/rustedscreep-wasm/src/movement.rs over rustedscreep-core `movement`). It checks that:
// - the owned room is read on the global's first loop: the terrain once (getTerrain, getRawBuffer), sources and
//   minerals once, and the scan's finds (structures, own construction sites, deposits; V158) then and again
//   `movement.scan_interval_ticks` later (the Standard rescan), never in between;
// - the obstacles matter: a wall splits the room, and of its five gaps only two can be walked (an own rampart
//   and a public one); the other three hold a constructed wall, another player's non-public rampart and an own
//   extension site, and the straight line to the target runs through them. A creep starts beside an exit, the
//   target lies between a source and a mineral, and the spawn stands in the way. The harness resolves every
//   move like the engine (V51, V52: a move into a blocked tile, or onto a creep that stays, fails and takes the
//   moves behind it down with it) and reports any failure, so the bot's own grid and traffic resolution must
//   be right for the test to pass; it also checks that creeps crossed the wall, and only at walkable gaps.
//   The source and the mineral are not on the route; that they reach the grid shows in the object count of
//   `rustedscreep.movement()` (the core scenario test `sources_and_minerals_block_the_route` checks they block);
// - `rustedscreep.moveTo(room, x, y, range)` walks the room's creeps there with `move(1..8)` calls only; no
//   creep ever ends a tick on an edge tile (V151), a wall, an obstacle, a non-public foreign rampart, a site, a
//   source, a mineral or another creep's tile; the fatigued creep is never asked to move while fatigued;
// - the order ends (one log line) once every creep is in range, which waits for the tired creep to recover;
//   then `rustedscreep.moveTo()` has nothing to cancel, a second order is cancelled after one tick of moves,
//   and no move is made after that; a bad room name is refused by the wasm;
// - `rustedscreep.movement()` reports the room, its grid version, the scan count and the objects it holds.
// The P2.4 opening reads the owned room every loop (its six finds, the source's energy, the site's progress, the
// container's store: engine-shaped here, V184, V196, V199) but stays inert: no creep has a bot worker name, and the
// room reports no energyAvailable or energyCapacityAvailable (read as 0, as before P2.4), so its plan has no worker
// body and wants no worker: no spawn request, no worker intent, no watchdog line. The find counts below include its
// finds; the find with a filter (containers) is its own and is checked apart from movement's scans. Its extension
// placement (ADR-020: RCL2 allows 5, the room has one site) calls `room.createConstructionSite` from the second
// loop; the harness records the call, returns OK and places nothing, so the bot gives up on each tile after
// APPEAR_TICKS (10) and picks the next, LOST_MAX (3) times, then makes no more calls for the global: three calls,
// each on a legal tile (V200 and the placement rules), and `rustedscreep.opening()` shows them never seen.
// npm run check runs it (row 16); standalone: node js/tools/move-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { CARRY_CAPACITY, findFiltered, makeStore, packPos } from './lib/world.mjs';
import { commandResults, simulate } from './smoke.mjs';

// @screeps/common 2.16.0 constants (the fixture the constants self-check reads).
const FIND = {
  HOSTILE_CREEPS: 103, SOURCES: 105, DROPPED_RESOURCES: 106, STRUCTURES: 107, MY_CONSTRUCTION_SITES: 114, MINERALS: 116,
  TOMBSTONES: 118, DEPOSITS: 122, RUINS: 123,
};
/** CONTROLLER_LEVELS[2], CONTROLLER_DOWNGRADE[2], SOURCE_ENERGY_CAPACITY, CONTAINER_CAPACITY, CONSTRUCTION_COST.extension. */
const PROGRESS_TOTAL_2 = 45000;
const DOWNGRADE_2 = 10000;
const SOURCE_ENERGY_CAPACITY = 3000;
const CONTAINER_CAPACITY = 2000;
const EXTENSION_COST = 3000;
const TERRAIN_MASK_WALL = 1;
const TERRAIN_MASK_SWAMP = 2;
/** The engine's offsetsByDirection (V155). */
const OFFSETS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];
/** MOVEMENT_MMO.scan_interval_ticks (crates/rustedscreep-core/src/config.rs). */
const SCAN_INTERVAL = 50;
/** economy::placement: APPEAR_TICKS and LOST_MAX (crates/rustedscreep-core/src/economy/placement.rs). */
const APPEAR_TICKS = 10;
const LOST_MAX = 3;
const ROOM = 'W1N1';
const T0 = 50000000;
const ORDER_AT = 2;
const BAD_ORDER_AT = 3;
const RECOVER_AT = 15;
const TARGET = { x: 30, y: 10, range: 2 };
const SECOND = { x: 10, y: 40, range: 1 };
const TICKS = SCAN_INTERVAL + 30;
const ORDER_CMD = `rustedscreep.moveTo("${ROOM}", ${TARGET.x}, ${TARGET.y}, ${TARGET.range})`;
const SECOND_CMD = `rustedscreep.moveTo("${ROOM}", ${SECOND.x}, ${SECOND.y}, ${SECOND.range})`;
const CANCEL_CMD = 'rustedscreep.moveTo()';
const BAD_ORDER_CMD = 'rustedscreep.moveTo("not a room", 1, 1)';
const SUMMARY_AT = TICKS - 1;
/** `rustedscreep.opening()`, once the second order is cancelled, on the loop before the movement summary. */
const OPENING_AT = TICKS - 2;
const OPENING_CMD = 'rustedscreep.opening()';

const pos = (x, y) => ({ x, y, roomName: ROOM, __packedPos: packPos(ROOM, x, y) });
const id = (n) => `5bbcaa9099f9b0a0f7f4c${n.toString(16).padStart(3, '0')}`;

/** The wall at x = WALL_X and its gaps, top to bottom; the straight line from the start runs through y = 10. */
const WALL_X = 20;
const GAPS = [8, 10, 12, 15, 18];

/** Row-major terrain (V156): walled edges with an exit at (0, 23..27), the split wall, and a swamp patch. */
function terrain() {
  const raw = new Uint8Array(2500);
  for (let y = 0; y < 50; y++) {
    for (let x = 0; x < 50; x++) {
      const edge = x === 0 || y === 0 || x === 49 || y === 49;
      let v = 0;
      if ((edge && !(x === 0 && y >= 23 && y <= 27)) || (x === WALL_X && !GAPS.includes(y))) v = TERRAIN_MASK_WALL;
      else if (x >= 23 && x <= 26 && y >= 14 && y <= 17) v = TERRAIN_MASK_SWAMP;
      raw[y * 50 + x] = v;
    }
  }
  return raw;
}

const RAW = terrain();
const isWall = (x, y) => (RAW[y * 50 + x] & TERRAIN_MASK_WALL) !== 0;

/** The world's objects. Owned ones carry `my`; ramparts `isPublic`. */
const controller = {
  id: id(1), structureType: 'controller', my: true, level: 2, pos: pos(40, 40),
  // The getters of an owned controller (V189): no progress yet, the level's total, a full downgrade timer.
  progress: 0, progressTotal: PROGRESS_TOTAL_2, ticksToDowngrade: DOWNGRADE_2,
};
const spawn = { id: id(2), structureType: 'spawn', my: true, name: 'Spawn1', pos: pos(8, 11) };
const ownRampart = { id: id(3), structureType: 'rampart', my: true, isPublic: false, pos: pos(WALL_X, 15) };
const publicRampart = { id: id(4), structureType: 'rampart', my: false, isPublic: true, pos: pos(WALL_X, 18) };
const STRUCTURES = [
  controller,
  spawn,
  ownRampart,
  publicRampart,
  { id: id(5), structureType: 'constructedWall', pos: pos(WALL_X, 8) },
  { id: id(6), structureType: 'rampart', my: false, isPublic: false, pos: pos(WALL_X, 10) },
  ...Array.from({ length: 8 }, (_, i) => ({ id: id(10 + i), structureType: 'road', pos: pos(22 + i, 10) })),
  { id: id(30), structureType: 'container', pos: pos(29, 12), store: makeStore({}, { capacity: CONTAINER_CAPACITY }) },
];
const SITES = [{ id: id(40), structureType: 'extension', my: true, progress: 0, progressTotal: EXTENSION_COST, pos: pos(WALL_X, 12) }];
const SOURCES = [{ id: id(50), energy: SOURCE_ENERGY_CAPACITY, energyCapacity: SOURCE_ENERGY_CAPACITY, pos: pos(31, 11) }];
const MINERALS = [{ id: id(51), pos: pos(31, 9) }];
/**
 * What the bot's grid must hold for this world: blocking footprints for the controller, the spawn, the
 * constructed wall, the foreign non-public rampart, the site, the source and the mineral, and a road footprint
 * per road; the own and the public rampart and the container leave their tiles as they are.
 */
const OBJECTS = 7 + 8;
const OBSTACLE_TYPES = new Set(['spawn', 'controller', 'constructedWall', 'extension', 'link', 'storage', 'tower']);

/** Why a creep of ours may not end a tick on (x, y) in this world, apart from other creeps (null if it may). */
function blocked(x, y) {
  if (x <= 0 || y <= 0 || x >= 49 || y >= 49) return 'edge';
  const here = (o) => o.pos.x === x && o.pos.y === y;
  const road = STRUCTURES.some((s) => here(s) && s.structureType === 'road');
  if (isWall(x, y) && !road) return 'wall';
  for (const s of STRUCTURES) {
    if (!here(s)) continue;
    if (OBSTACLE_TYPES.has(s.structureType)) return s.structureType;
    if (s.structureType === 'rampart' && !s.my && !s.isPublic) return 'foreign rampart';
  }
  if (SITES.some(here)) return 'own obstacle site';
  if (SOURCES.some(here) || MINERALS.some(here)) return 'source or mineral';
  return null;
}

function makeWorld() {
  const creeps = [
    { id: id(100), name: 'A', x: 5, y: 10, fatigue: 0 },
    { id: id(101), name: 'B', x: 1, y: 24, fatigue: 0 }, // beside the exit at (0, 23..27)
    { id: id(102), name: 'C', x: 6, y: 12, fatigue: 0 },
    { id: id(103), name: 'Tired', x: 9, y: 13, fatigue: 4 },
  ];
  const hostile = { id: id(200), name: 'Invader', x: 26, y: 20 };
  return { creeps, hostile, pending: [], calls: [], errors: [], finds: [], filtered: [], sites: [], terrainReads: [], crossed: new Set() };
}

function sceneFor(state, offset) {
  const own = state.creeps.map((c) => ({
    id: c.id, name: c.name, my: true, spawning: false, fatigue: c.fatigue, owner: { username: 'bot' }, pos: pos(c.x, c.y),
    store: makeStore({}, { capacity: CARRY_CAPACITY }),
    move(direction) {
      state.calls.push([offset, c.name, 'move', direction]);
      state.pending.push([c, direction]);
      return 0;
    },
    say() { return 0; },
  }));
  const h = state.hostile;
  const hostile = { id: h.id, name: h.name, my: false, spawning: false, owner: { username: 'Invader' }, pos: pos(h.x, h.y) };
  const room = {
    name: ROOM,
    controller,
    find(type, opts) {
      // A find with a filter (the opening's containers, P2.4) is recorded apart from the plain ones.
      (opts?.filter ? state.filtered : state.finds).push([offset, type]);
      const list = {
        [FIND.HOSTILE_CREEPS]: [hostile],
        [FIND.STRUCTURES]: STRUCTURES,
        [FIND.MY_CONSTRUCTION_SITES]: SITES,
        [FIND.SOURCES]: SOURCES,
        [FIND.MINERALS]: MINERALS,
      }[type] ?? [];
      return findFiltered(list, opts);
    },
    getTerrain() {
      state.terrainReads.push(offset);
      return { getRawBuffer: () => RAW.slice() };
    },
    createConstructionSite(...args) {
      state.sites.push([offset, ...args]);
      return 0;
    },
  };
  return {
    creeps: Object.fromEntries(own.map((c) => [c.name, c])),
    rooms: { [ROOM]: room },
    structures: Object.fromEntries(STRUCTURES.filter((s) => s.my).map((s) => [s.id, s])),
  };
}

/** The engine's move resolution for this tick's calls (V51, V52), then the invariants. */
function applyMoves(state, offset) {
  const moves = new Map();
  for (const [c, direction] of state.pending) {
    if (!Number.isInteger(direction) || direction < 1 || direction > 8) {
      state.errors.push(`offset ${offset}: ${c.name} moved with ${direction}`);
      continue;
    }
    if (c.fatigue > 0) state.errors.push(`offset ${offset}: the fatigued ${c.name} was asked to move`);
    if (moves.has(c)) state.errors.push(`offset ${offset}: ${c.name} got two moves`);
    const [dx, dy] = OFFSETS[direction];
    moves.set(c, { x: c.x + dx, y: c.y + dy });
  }
  state.pending = [];
  const all = [...state.creeps, state.hostile];
  let changed = true;
  while (changed) {
    changed = false;
    for (const [c, d] of moves) {
      const others = [...moves].filter(([o, e]) => o !== c && e.x === d.x && e.y === d.y);
      if (others.length) state.errors.push(`offset ${offset}: ${c.name} and ${others[0][0].name} both move to (${d.x}, ${d.y})`);
      const stays = all.some((o) => o !== c && o.x === d.x && o.y === d.y && !moves.has(o));
      const why = blocked(d.x, d.y);
      if (why || stays || others.length) {
        state.errors.push(`offset ${offset}: ${c.name}'s move to (${d.x}, ${d.y}) fails (${why ?? 'a creep stays there'})`);
        moves.delete(c);
        changed = true;
      }
    }
  }
  for (const [c, d] of moves) {
    c.x = d.x;
    c.y = d.y;
    if (d.x === WALL_X) state.crossed.add(d.y);
  }
  for (const c of state.creeps) {
    const why = blocked(c.x, c.y);
    if (why) state.errors.push(`offset ${offset}: ${c.name} ended on (${c.x}, ${c.y}): ${why}`);
  }
}

export function moveSim(dir = join(ROOT, 'dist')) {
  const state = makeWorld();
  let doneAt = null;
  let phase = 'walking';
  const world = simulate(dir, {
    ticks: TICKS,
    scene: (offset) => sceneFor(state, offset),
    command: (w) => {
      const offset = w.ticksRun;
      applyMoves(state, offset);
      if (offset === RECOVER_AT) state.creeps.find((c) => c.name === 'Tired').fatigue = 0;
      if (offset === ORDER_AT) return ORDER_CMD;
      if (offset === BAD_ORDER_AT) return BAD_ORDER_CMD;
      if (offset === SUMMARY_AT) return 'rustedscreep.movement()';
      // After the order ended: nothing to cancel, then a second order cancelled after one tick of moves.
      if (phase === 'walking' && w.logs.some((l) => l.startsWith('rustedscreep moveTo: all'))) {
        doneAt = offset;
        phase = 'done';
        return CANCEL_CMD;
      }
      if (phase === 'done') {
        phase = 'second';
        return SECOND_CMD;
      }
      if (phase === 'second') {
        phase = 'cancelled';
        state.cancelledAt = offset;
        return CANCEL_CMD;
      }
      if (phase === 'cancelled' && offset === OPENING_AT) return OPENING_CMD;
      return null;
    },
  });
  const problems = [...state.errors];
  const warn = world.logs.filter((l) => l.includes('fault') || l.includes('[error]') || l.includes('trapped'));
  problems.push(...warn.map((l) => `log: ${l}`));
  if (world.halts) problems.push(`${world.halts} halt(s)`);

  // The room is owned, so its first read comes on the global's first loop: the terrain, sources and
  // minerals once, and the scan's finds then and one interval later (the Standard rescan).
  if (JSON.stringify(state.terrainReads) !== '[1]') problems.push(`terrain reads at offsets ${JSON.stringify(state.terrainReads)}, expected [1]`);
  // P2.4: the opening finds the owned room's sources, own sites, piles, tombstones and ruins once per loop, from
  // the first (offset 1), and its containers with a filter; before P2.4 sources and sites were found only by
  // movement (sources [1], sites [1, 1 + SCAN_INTERVAL]) and the other three not at all.
  const loops = Array.from({ length: TICKS - 1 }, (_, i) => i + 1);
  const withOpening = (movement) => [...movement, ...loops].sort((a, b) => a - b);
  const wantFinds = [
    [FIND.SOURCES, withOpening([1])], [FIND.MINERALS, [1]],
    [FIND.STRUCTURES, [1, 1 + SCAN_INTERVAL]], [FIND.MY_CONSTRUCTION_SITES, withOpening([1, 1 + SCAN_INTERVAL])], [FIND.DEPOSITS, [1, 1 + SCAN_INTERVAL]],
    [FIND.DROPPED_RESOURCES, loops], [FIND.TOMBSTONES, loops], [FIND.RUINS, loops],
  ];
  for (const [type, want] of wantFinds) {
    const at = state.finds.filter(([, t]) => t === type).map(([o]) => o);
    if (JSON.stringify(at) !== JSON.stringify(want)) problems.push(`find(${type}) at offsets ${JSON.stringify(at)}, expected ${JSON.stringify(want)}`);
  }
  const unknown = state.finds.filter(([, t]) => !Object.values(FIND).includes(t));
  if (unknown.length) problems.push(`finds of other types: ${JSON.stringify(unknown.slice(0, 4))}`);
  // The opening's containers: FIND_STRUCTURES with a filter, once per loop.
  const filtered = state.filtered.map(([o, t]) => (t === FIND.STRUCTURES ? o : `type ${t} at ${o}`));
  if (JSON.stringify(filtered) !== JSON.stringify(loops)) problems.push(`filtered finds ${JSON.stringify(filtered)}, expected find(${FIND.STRUCTURES}) at ${JSON.stringify(loops)}`);
  // Inert: no watchdog line (a spawnCreep or work call would throw here, the creeps and the spawn have no such
  // method, and show as a fault line above), and no refused site.
  const opening = world.logs.filter((l) => l.startsWith('rustedscreep opening:'));
  if (opening.length) problems.push(`opening lines: ${opening.join(' / ')}`);
  // The extension placement (ADR-020): a call on the first loop the Optional job runs after the constants
  // self-check (its last loop, or the next when the check used up that loop's Optional cap: wall time), then one
  // APPEAR_TICKS (10) loops later until LOST_MAX (3) were never seen, and none after (was one every 10 loops to the
  // end before LOST_MAX), each with x, y and "extension", on distinct legal tiles: inside the margin of 3, not a
  // wall, not on a structure or a site, at range 2 or more from the spawn, not next to the source or the mineral,
  // not within 3 of the controller.
  const constsAt = Number(/^rustedscreep consts: checked at tick (\d+) /.exec(world.logs.find((l) => l.startsWith('rustedscreep consts: checked')) ?? '')?.[1]) - T0;
  const firstSite = state.sites[0]?.[0];
  const wantSites = firstSite >= constsAt && firstSite <= constsAt + 1
    ? Array.from({ length: LOST_MAX }, (_, k) => firstSite + APPEAR_TICKS * k) : ['an offset near', constsAt];
  if (JSON.stringify(state.sites.map(([o]) => o)) !== JSON.stringify(wantSites)) problems.push(`createConstructionSite at offsets ${JSON.stringify(state.sites.map(([o]) => o))}, expected ${JSON.stringify(wantSites)}`);
  // The opening's summary: the room wants no worker (no energy capacity) and counts the three lost picks, each tile
  // skipped for the global.
  const openingSummary = commandResults(world, OPENING_CMD);
  const openingParts = [
    `rustedscreep opening at tick ${T0 + OPENING_AT}: ${ROOM}: workers 0 of 0 (working 0), no request, plan at capacity 0: `,
    ` | extension sites: picked ${LOST_MAX}, placed ${LOST_MAX}, refused 0, never seen ${LOST_MAX}, tiles skipped ${LOST_MAX}`,
  ];
  if (openingSummary.length !== 1) problems.push(`${openingSummary.length} opening summaries, expected 1`);
  else for (const p of openingParts) if (!openingSummary[0].includes(p)) problems.push(`opening summary lacks "${p}": ${openingSummary[0]}`);
  const tiles = new Set();
  for (const [o, x, y, type, ...rest] of state.sites) {
    const here = (p) => p.x === x && p.y === y;
    const near = (p, r) => Math.max(Math.abs(p.x - x), Math.abs(p.y - y)) <= r;
    const why = rest.length || type !== 'extension' ? 'arguments' : x < 3 || y < 3 || x > 46 || y > 46 ? 'margin'
      : isWall(x, y) ? 'wall' : STRUCTURES.some((st) => here(st.pos)) || SITES.some((st) => here(st.pos)) ? 'taken'
        : near(spawn.pos, 1) ? 'next to the spawn' : [...SOURCES, ...MINERALS].some((st) => near(st.pos, 1)) ? 'next to a source or mineral'
          : near(controller.pos, 3) ? 'within 3 of the controller' : tiles.has(`${x},${y}`) ? 'picked twice' : null;
    if (why) problems.push(`offset ${o}: createConstructionSite(${x}, ${y}, ${type}${rest.length ? ', ...' : ''}): ${why}`);
    tiles.add(`${x},${y}`);
  }

  // The route through the wall: only the own and the public rampart gaps, and both were used by someone or
  // at least one of them (the blind straight line would have taken y = 8, 10 or 12).
  const crossed = [...state.crossed].sort((a, b) => a - b);
  if (!crossed.length || crossed.some((y) => y !== 15 && y !== 18)) problems.push(`wall crossings at y = ${JSON.stringify(crossed)}, expected only 15 and 18`);

  // The orders.
  const accepted = commandResults(world, ORDER_CMD);
  if (accepted.length !== 1 || !accepted[0].includes(`head for (${TARGET.x}, ${TARGET.y}) within ${TARGET.range}`)) {
    problems.push(`moveTo reply: ${accepted.join(' / ')}`);
  }
  const bad = commandResults(world, BAD_ORDER_CMD);
  if (bad.length !== 1 || !bad[0].includes('not a room name')) problems.push(`bad moveTo reply: ${bad.join(' / ')}`);
  const done = world.logs.filter((l) => l.startsWith('rustedscreep moveTo: all'));
  if (done.length !== 1 || !done[0].includes(`all 4 creep(s) in ${ROOM} are within ${TARGET.range} of (${TARGET.x}, ${TARGET.y}); order done`)) {
    problems.push(`order completion: ${done.join(' / ') || 'none'}`);
  }
  if (doneAt !== null && doneAt <= RECOVER_AT) problems.push(`the order ended at offset ${doneAt}, before the tired creep could move`);
  const cancels = commandResults(world, CANCEL_CMD);
  if (JSON.stringify(cancels) !== JSON.stringify(['rustedscreep moveTo: no order to cancel', 'rustedscreep moveTo: order cancelled'])) {
    problems.push(`cancel replies: ${JSON.stringify(cancels)}`);
  }
  const moves = state.calls.filter((c) => c[2] === 'move');
  const before = moves.filter(([o]) => o <= ORDER_AT);
  if (before.length) problems.push(`moves before the order: ${JSON.stringify(before)}`);
  const tiredEarly = moves.filter(([o, name]) => name === 'Tired' && o <= RECOVER_AT);
  if (tiredEarly.length) problems.push(`the tired creep moved before it recovered: ${JSON.stringify(tiredEarly)}`);
  if (state.cancelledAt !== undefined) {
    const second = moves.filter(([o]) => o === state.cancelledAt);
    const after = moves.filter(([o]) => o > state.cancelledAt);
    if (!second.length) problems.push('the second order made no move before it was cancelled');
    if (after.length) problems.push(`moves after the cancel: ${JSON.stringify(after.slice(0, 4))}`);
  } else {
    problems.push('the second order was never cancelled');
  }

  const summary = commandResults(world, 'rustedscreep.movement()');
  const parts = [
    'rooms tracked 1 (terrain 1, active 1, read 1)',
    `(${ROOM}: grid v1, ${OBJECTS} objects, scanned at ${T0 + 1 + SCAN_INTERVAL})`,
    'scans 2 (changed 1), grids built 1, early rescans 0, suspect tiles 0, routes searched 0 (failed 0, edge tiles learned 0), rooms crossed 0 (transfers refused 0) | read failures 0, scan skips 0',
    'routes kept 0, closed rooms none, PathFinder searches 0',
    'no moveTo order',
  ];
  if (summary.length !== 1) problems.push(`${summary.length} movement summaries`);
  else for (const p of parts) if (!summary[0].includes(p)) problems.push(`movement summary lacks "${p}": ${summary[0]}`);

  if (problems.length) throw new Error(`move-sim:\n  ${problems.slice(0, 20).join('\n  ')}`);
  return { moves: moves.length, crossed, doneAt, summary: summary[0] };
}

if (isMain(import.meta.url)) {
  try {
    const r = moveSim(process.argv[2]);
    console.log(`move-sim ok: ${r.moves} move calls, wall crossed at y ${r.crossed.join(', ')}, order done at offset ${r.doneAt} | ${r.summary}`);
  } catch (e) {
    fail(e.message);
  }
}
