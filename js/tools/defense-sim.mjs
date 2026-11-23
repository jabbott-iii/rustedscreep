// Threat assessment (P7.1) end to end in the Screeps-like sandbox (js/tools/smoke.mjs), with the plain build in
// dist/ (crates/rustedscreep-wasm/src/defense.rs and the snapshot's hostile reads over rustedscreep-core
// `defense::threat`). The world is the smoke test's (js/tools/lib/world.mjs: W1N1 owned at RCL2), with W1N1's
// FIND_HOSTILE_CREEPS replaced by this script's hostiles, whose `body`, `hits`, `owner` and each part's `type` and
// `boost` count their reads (engine-shaped, V289: an unboosted part reads `boost` as undefined). While the two
// attackers step out, two more rooms are visible: a neutral one and another player's at the lab level. It checks:
// - the console sets `defense.threat_end_ticks` (END) and `defense.threat_log_ticks` (LOG) once the overrides load;
// - a hostile's `hits` is read once on each loop it stands in W1N1, and its `body` with `type` and `boost` once a
//   part, on the first loop it stands there: again only after it was seen in another player's room with labs,
//   where it may be boosted (V290, V291), not after a neutral room;
// - a scout (MOVE parts) opens no threat; a boosted attacker logs one start line, whose figures come from the
//   constants fixture (ATTACK_POWER, RANGED_ATTACK_POWER, BOOSTS, and the boosted TOUGH saving of V287), with a
//   compound string the crate does not know read as no boost and no trap; its damage, which empties its front
//   TOUGH part (V283), changes nothing logged; a second attacker inside the log window waits for one change line
//   LOG loops after the start; both stepping out for less than END loops keep the threat open, and their return
//   with the same counts logs nothing; END loops after the last sight one end line;
// - `owner` is read only on a start or change loop, for the dangerous hostiles, and on a loop the towers take a new
//   target, for that target;
// - the towers (P7.2): one tower in W1N1 at TOWER, full, whose `attack` and `heal` record their calls. Focus fire:
//   one `attack` a loop on the scout from its first loop (alone, harmless, and killable), on the boosted attacker from
//   its first loop (dangerous first), kept on it when the second attacker comes; while
//   both are out, on the scout (the only killable hostile left), and on the attacker again when it comes back; one
//   "towers fire" line for each new target, its damage, heal and ticks from the constants (TOWER_POWER_ATTACK, the
//   falloff, V239; the boosted TOUGH saving, V287). Heal priority: with everyone gone and the threat still open, one
//   `heal` a loop on the smoke world's Harvester, damaged here (its hits read only while the threat is open, from the
//   loop after it opened), up to the threat's end;
// - no fault, error or trap line, and no halt.
// npm run check runs it (row 25); standalone: node js/tools/defense-sim.mjs [dir].
import { join } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';
import { FIND_HOSTILE_CREEPS, IDS, findFiltered, makeStore, packPos, sceneAt } from './lib/world.mjs';
import { ENGINE_CONSTANTS, commandResults, simulate } from './smoke.mjs';

const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
const ROOM = 'W1N1';
/** Visible only while the attackers are out: nobody's, and another player's at the least level with labs. */
const NEUTRAL = 'W2N1';
const LAB_ROOM = 'W1N2';
const LAB_LEVEL = [1, 2, 3, 4, 5, 6, 7, 8].find((l) => (C.CONTROLLER_STRUCTURES.lab[l] ?? 0) > 0);
const T0 = 50000000;
/** The overrides, sent once the record has loaded (the console refuses a change before), from CONFIG_AT on. */
const END = 3;
const LOG = 4;
const END_CMD = `rustedscreep.configSet("defense.threat_end_ticks", "${END}")`;
const LOG_CMD = `rustedscreep.configSet("defense.threat_log_ticks", "${LOG}")`;
const CONFIG_AT = 3;
/** A part's hit points (V282) and the TOUGH compound on the attacker's front part. */
const PART_HITS = 100;
const TOUGH = 'XGHO2';
/** A compound string no crate version knows: it must read as no boost. */
const UNKNOWN = 'XQ';

const id = (n) => `5bbcaa9099f9b0a0f7f4d${n.toString(16).padStart(3, '0')}`;
/** Hostiles: [type, boost] bodies, the first part first. */
const SCOUT = { id: id(1), name: 'Scout9', owner: 'Scouter', x: 30, y: 30, body: [['move'], ['move']] };
const RAIDER_A = {
  id: id(2), name: 'RaiderA', owner: 'Raider', x: 32, y: 30,
  body: [['tough', TOUGH], ['attack', 'UH2O'], ['attack'], ['ranged_attack', UNKNOWN], ['move'], ['move']],
};
const RAIDER_B = { id: id(3), name: 'RaiderB', owner: 'Raider', x: 33, y: 31, body: [['attack'], ['move']] };
/** The tower (full: its fills are not modelled) and the smoke world's own creep it heals, with its lost hits. */
const TOWER = { id: id(50), x: 30, y: 25 };
const HURT_OWN = 'Harvester';
const OWN_LOST = 40;
/** The timeline, by loop offset (the first loop runs at offset 1). */
const SCOUT_FROM = 6;
const A_FROM = 8;
const A_HURT = 9; // from here A has lost its first part and half of the second
const B_FROM = 10;
const OUT_FROM = 14; // A in NEUTRAL and B in LAB_ROOM until BACK
const BACK = 16;
const GONE = 17; // everyone gone for good
const TICKS = GONE + END + 4;
const HURT = PART_HITS + PART_HITS / 2;

/** Where each hostile stands at a loop, or nowhere. */
function roomOf(h, offset) {
  if (h === SCOUT) return offset >= SCOUT_FROM && offset < GONE ? ROOM : null;
  const from = h === RAIDER_A ? A_FROM : B_FROM;
  if (offset < from || offset >= GONE) return null;
  if (offset >= OUT_FROM && offset < BACK) return h === RAIDER_A ? NEUTRAL : LAB_ROOM;
  return ROOM;
}

const HOSTILES = [SCOUT, RAIDER_A, RAIDER_B];
const inRoom = (room, offset) => HOSTILES.filter((h) => roomOf(h, offset) === room);
const fullHits = (h) => PART_HITS * h.body.length;
const hitsAt = (h, offset) => (h === RAIDER_A && offset >= A_HURT ? fullHits(h) - HURT : fullHits(h));

function count(reads, id, what) {
  const r = (reads[id] ??= {});
  r[what] = (r[what] ?? 0) + 1;
}

/** A hostile creep as the engine shows it (V289), counting the reads the snapshot and the log lines make. */
function hostileObject(h, room, offset, reads) {
  const parts = h.body.map(([type, boost]) => Object.defineProperties({ hits: PART_HITS }, {
    type: { get() { count(reads, h.id, 'type'); return type; }, enumerable: true },
    // An unboosted part has no `boost` key; a getter returning undefined reads the same and counts the read.
    boost: { get() { count(reads, h.id, 'boost'); return boost; }, enumerable: boost !== undefined },
  }));
  return Object.defineProperties({
    id: h.id, name: h.name, my: false, spawning: false, hitsMax: fullHits(h),
    pos: { x: h.x, y: h.y, roomName: room, __packedPos: packPos(room, h.x, h.y) },
  }, {
    body: { get() { count(reads, h.id, `body@${offset}`); return parts; }, enumerable: true },
    hits: { get() { count(reads, h.id, `hits@${offset}`); return hitsAt(h, offset); }, enumerable: true },
    owner: { get() { count(reads, h.id, `owner@${offset}`); return { username: h.owner }; }, enumerable: true },
  });
}

/** A room the bot does not own, with these hostiles in it (the snapshot reads its controller's `my` and `level`). */
function otherRoom(name, controller, hostiles) {
  return {
    name,
    controller: { id: id(name === NEUTRAL ? 40 : 41), structureType: 'controller', pos: { x: 20, y: 20, roomName: name, __packedPos: packPos(name, 20, 20) }, ...controller },
    find: (type, opts) => findFiltered(type === FIND_HOSTILE_CREEPS ? hostiles : [], opts),
    getTerrain: () => ({ getRawBuffer: () => new Uint8Array(2500) }),
  };
}

function sceneFor(offset, reads, calls) {
  const scene = sceneAt(offset);
  // The tower: an own structure in Game.structures, full, recording its calls by target id.
  const tower = {
    id: TOWER.id, structureType: 'tower', my: true,
    pos: { x: TOWER.x, y: TOWER.y, roomName: ROOM, __packedPos: packPos(ROOM, TOWER.x, TOWER.y) },
    store: makeStore({ energy: C.TOWER_CAPACITY }, { resourceCapacity: { energy: C.TOWER_CAPACITY } }),
    attack(target) { calls.push([offset, 'attack', target?.id ?? target?.name]); return 0; },
    heal(target) { calls.push([offset, 'heal', target?.id ?? target?.name]); return 0; },
  };
  scene.structures[TOWER.id] = tower;
  const own = scene.creeps[HURT_OWN];
  own.hits -= OWN_LOST;
  const room = scene.rooms[ROOM];
  const base = room.find;
  const here = (name) => inRoom(name, offset).map((h) => hostileObject(h, name, offset, reads));
  const hostiles = here(ROOM);
  room.find = (type, opts) => (type === FIND_HOSTILE_CREEPS ? findFiltered(hostiles, opts) : base(type, opts));
  if (offset >= OUT_FROM && offset < BACK) {
    // An unowned controller's `my` is undefined (V101); another player's is false.
    scene.rooms[NEUTRAL] = otherRoom(NEUTRAL, { level: 0 }, here(NEUTRAL));
    scene.rooms[LAB_ROOM] = otherRoom(LAB_ROOM, { my: false, level: LAB_LEVEL, owner: { username: 'Raider' } }, here(LAB_ROOM));
  }
  return scene;
}

/** What the constants say a hostile can do, as the bot's line gives it (module docs of `defense::threat`). */
function assess(h, hits) {
  const len = h.body.length;
  const partHits = (i) => Math.min(PART_HITS, Math.max(0, hits - PART_HITS * (len - 1 - i)));
  let melee = 0;
  let ranged = 0;
  let saving = 0;
  let boosted = false;
  let dangerous = false;
  h.body.forEach(([type, boost], i) => {
    const known = boost !== undefined && C.BOOSTS[type]?.[boost] !== undefined;
    boosted ||= Object.values(C.BOOSTS).some((byType) => boost in byType);
    if (partHits(i) <= 0) return;
    const m = (action) => (known ? C.BOOSTS[type][boost][action] ?? 1 : 1);
    if (type === 'attack') melee += C.ATTACK_POWER * m('attack');
    if (type === 'ranged_attack') ranged += C.RANGED_ATTACK_POWER * m('rangedAttack');
    if (type === 'attack' || type === 'ranged_attack' || type === 'work' || type === 'claim') dangerous = true;
    const ratio = known ? C.BOOSTS[type][boost].damage : undefined;
    if (ratio) saving += (partHits(i) / ratio) * (1 - ratio);
  });
  // A whole creep with its boosted TOUGH parts in front: the saving lands in full (V287).
  return { melee, ranged, boosted, dangerous, hits, effective: hits + Math.round(saving) };
}

/** The engine's `_applyDamage` (V287) on a body at `hits` (its parts filled from the last, V283), and the towers' sim
 * (V287's order: damage, then heal, then death) for `damage` a tick and no heal. */
function taken(h, hits, damage) {
  const len = h.body.length;
  if (!h.body.some(([, b]) => b !== undefined)) return damage;
  let reduce = 0;
  let left = damage;
  h.body.forEach(([type, boost], i) => {
    if (left <= 0) return;
    const ratio = (boost !== undefined && C.BOOSTS[type]?.[boost]?.damage) || 1;
    const partHits = Math.min(PART_HITS, Math.max(0, hits - PART_HITS * (len - 1 - i)));
    const take = Math.min(partHits / ratio, left);
    reduce += take * (1 - ratio);
    left -= take;
  });
  return damage - Math.round(reduce);
}
function killTicks(h, hits, damage) {
  for (let t = 1; t <= 50; t++) {
    const after = hits - taken(h, hits, damage);
    if (after <= 0) return t;
    hits = after;
  }
  return null;
}
/** A tower's attack at range (towers/attack.js, V239). */
function towerDamage(range) {
  const p = C.TOWER_POWER_ATTACK;
  if (range <= C.TOWER_OPTIMAL_RANGE) return p;
  const r = Math.min(range, C.TOWER_FALLOFF_RANGE);
  return Math.floor(p - p * C.TOWER_FALLOFF * (r - C.TOWER_OPTIMAL_RANGE) / (C.TOWER_FALLOFF_RANGE - C.TOWER_OPTIMAL_RANGE));
}
function fireLine(h, offset) {
  const range = Math.max(Math.abs(h.x - TOWER.x), Math.abs(h.y - TOWER.y));
  const damage = towerDamage(range);
  return `rustedscreep defense: ${ROOM} towers fire: 1 tower(s) on the hostile at (${h.x}, ${h.y}) of ${h.owner}, ${damage} damage `
    + `a tick against 0 heal, dead in ${killTicks(h, hitsAt(h, offset), damage)} tick(s) by the sim`;
}

function roomLine(offset) {
  const here = inRoom(ROOM, offset).map((h) => assess(h, hitsAt(h, offset)));
  const sum = (k) => here.reduce((n, a) => n + a[k], 0);
  const dangerous = here.filter((a) => a.dangerous).length;
  const boosted = here.filter((a) => a.boosted).length;
  return `${dangerous} dangerous hostile creep(s) of Raider (${here.length} in the room, ${boosted} boosted): melee ${sum('melee')}, `
    + `ranged ${sum('ranged')}, heal 0, dismantle 0 a tick, 0 CLAIM; hits ${sum('hits')}, effective ${sum('effective')}`;
}

export function defenseSim(dir = join(ROOT, 'dist')) {
  const reads = {};
  const calls = [];
  const state = { configured: null };
  const world = simulate(dir, {
    ticks: TICKS,
    scene: (offset) => sceneFor(offset, reads, calls),
    command: (w) => {
      const offset = w.ticksRun;
      if (offset < CONFIG_AT || state.configured === 'done') return null;
      if (state.configured === null) {
        state.configured = 'end';
        return END_CMD;
      }
      if (state.configured === 'end') {
        const reply = commandResults(w, END_CMD).at(-1) ?? '';
        // Refused before the record loaded: ask again.
        if (!reply.includes('from the next tick')) return END_CMD;
        state.configured = 'log';
        return LOG_CMD;
      }
      state.configured = 'done';
      state.configuredAt = offset;
      return null;
    },
  });
  const problems = [];
  const bad = world.logs.filter((l) => l.includes('fault') || l.includes('[error]') || l.includes('trapped') || l.includes('incident'));
  problems.push(...bad.map((l) => `log: ${l}`));
  if (world.halts) problems.push(`${world.halts} halt(s)`);
  if (!LAB_LEVEL) problems.push('no level allows labs in the fixture');
  if (state.configured !== 'done' || state.configuredAt >= SCOUT_FROM) problems.push(`the overrides were set late (${state.configured} at ${state.configuredAt})`);
  for (const cmd of [END_CMD, LOG_CMD]) {
    // The last reply: earlier ones may be refusals from before the overrides' record loaded.
    const reply = commandResults(world, cmd).at(-1) ?? '';
    if (!reply.includes('from the next tick')) problems.push(`${cmd}: ${reply}`);
  }

  // The lines: the scout shot from its first loop (killable and alone; it opens no threat), a start at A_FROM with the
  // attacker taken over from it (dangerous first), a change LOG loops later, the scout taken while both
  // attackers are out and the attacker again at their return (the same counts: no change line), an end END loops after
  // the last sight.
  const lines = world.logs.filter((l) => l.startsWith('rustedscreep defense:'));
  const prefix = `rustedscreep defense: ${ROOM} `;
  const want = [
    fireLine(SCOUT, SCOUT_FROM),
    `${prefix}threatened by ${roomLine(A_FROM)}`,
    fireLine(RAIDER_A, A_FROM),
    `${prefix}threat (since tick ${T0 + A_FROM}) now ${roomLine(A_FROM + LOG)}`,
    fireLine(SCOUT, OUT_FROM),
    fireLine(RAIDER_A, BACK),
    `${prefix}threat over: no dangerous hostile seen since tick ${T0 + BACK} (it started at tick ${T0 + A_FROM})`,
  ];
  if (JSON.stringify(lines) !== JSON.stringify(want)) {
    problems.push(`defense lines:\n    ${lines.join('\n    ') || 'none'}\n  expected:\n    ${want.join('\n    ')}`);
  }
  // Each line's loop shows in the owner reads below, which only a start or change loop makes.
  const at = (h, what) => Object.keys(reads[h.id] ?? {}).filter((k) => k.startsWith(`${what}@`)).map((k) => Number(k.split('@')[1]));
  const loops = Array.from({ length: TICKS }, (_, o) => o);
  const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  // The body is read on a loop in W1N1 after a loop the creep was not in the snapshot, or was in the lab room.
  const bodyLoops = (h) => loops.filter((o) => roomOf(h, o) === ROOM && (roomOf(h, o - 1) === null || roomOf(h, o - 1) === LAB_ROOM));
  const wantReads = { [SCOUT.name]: [SCOUT_FROM], [RAIDER_A.name]: [A_FROM], [RAIDER_B.name]: [B_FROM, BACK] };
  for (const h of HOSTILES) {
    const inside = loops.filter((o) => roomOf(h, o) === ROOM);
    if (!eq(at(h, 'hits'), inside)) problems.push(`${h.name}: hits read at ${JSON.stringify(at(h, 'hits'))}, expected ${JSON.stringify(inside)}`);
    if (!eq(bodyLoops(h), wantReads[h.name])) problems.push(`${h.name}: the timeline gives body reads at ${JSON.stringify(bodyLoops(h))}`);
    if (!eq(at(h, 'body'), wantReads[h.name])) problems.push(`${h.name}: body read at ${JSON.stringify(at(h, 'body'))}, expected ${JSON.stringify(wantReads[h.name])}`);
    const perPart = wantReads[h.name].length * h.body.length;
    for (const what of ['type', 'boost']) {
      const n = reads[h.id]?.[what] ?? 0;
      if (n !== perPart) problems.push(`${h.name}: ${what} read ${n} times, expected ${perPart}`);
    }
  }
  const owners = (h) => at(h, 'owner');
  if (!eq(owners(SCOUT), [SCOUT_FROM, OUT_FROM])) problems.push(`the scout's owner was read at ${JSON.stringify(owners(SCOUT))}, expected ${JSON.stringify([SCOUT_FROM, OUT_FROM])}`);
  if (!eq(owners(RAIDER_A), [A_FROM, A_FROM + LOG, BACK])) problems.push(`RaiderA's owner read at ${JSON.stringify(owners(RAIDER_A))}, expected ${JSON.stringify([A_FROM, A_FROM + LOG, BACK])}`);
  if (!eq(owners(RAIDER_B), [A_FROM + LOG])) problems.push(`RaiderB's owner read at ${JSON.stringify(owners(RAIDER_B))}, expected ${JSON.stringify([A_FROM + LOG])}`);

  // The towers' calls: one a loop. Attacks on A while it is in the room, on the scout while A is out; heals on the
  // damaged own creep from the loop after everyone left until the threat ends (its hits are read while it is open).
  const wantCalls = [];
  for (const o of loops) {
    if (roomOf(RAIDER_A, o) === ROOM) wantCalls.push([o, 'attack', RAIDER_A.id]);
    else if (roomOf(SCOUT, o) === ROOM) wantCalls.push([o, 'attack', SCOUT.id]);
    else if (o >= GONE && o <= BACK + END) wantCalls.push([o, 'heal', IDS.harvester]);
  }
  if (!eq(calls, wantCalls)) problems.push(`tower calls ${JSON.stringify(calls)}, expected ${JSON.stringify(wantCalls)}`);

  if (problems.length) throw new Error(`defense-sim:\n  ${problems.slice(0, 20).join('\n  ')}`);
  return { ticks: world.ticksRun, lines: lines.length, start: lines[0] };
}

if (isMain(import.meta.url)) {
  try {
    const r = defenseSim(process.argv[2]);
    console.log(`defense-sim ok: ${r.ticks} ticks, ${r.lines} defense lines | ${r.start}`);
  } catch (e) {
    fail(e.message);
  }
}
