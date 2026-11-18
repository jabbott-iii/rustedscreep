// Writes the base planner's terrain corpus (ROADMAP P4.1): one fixture per room under
// crates/rustedscreep-core/tests/fixtures/rooms/<shard>/, read by the native tests and benches. No game logic.
//
// Input: a directory of saved responses from the official server's public room endpoints, two per room:
// `<room>.terrain.json` from `/api/game/room-terrain?room=<room>&shard=<shard>&encoded=1` and
// `<room>.objects.json` from `/api/game/room-objects?room=<room>&shard=<shard>` (fetching them is not this
// tool's job; the fixtures' README lists the requests). The tool reads local files only.
//
// Output keeps the terrain digits and the controller's, sources' and minerals' positions (and each mineral's
// type). Everything else in the responses (owners, usernames, structures, creeps, levels, signs) is dropped.
//
// Usage: node js/tools/rooms-fixture.mjs <directory of saved responses> <shard> <fetch date YYYY-MM-DD>
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { ROOT, fail, isMain } from './lib/common.mjs';

export const ROOMS_DIR = join(ROOT, 'crates', 'rustedscreep-core', 'tests', 'fixtures', 'rooms');

// ROOM_SIZE and ROOM_AREA (V156): the encoded terrain is one digit per tile, each the tile's
// TERRAIN_MASK_WALL (1) and TERRAIN_MASK_SWAMP (2) bits.
const SIZE = 50;
const TERRAIN = new RegExp(`^[0-3]{${SIZE * SIZE}}$`);
const ROOM = /^[EW][0-9]{1,3}[NS][0-9]{1,3}$/;
const SHARD = /^[a-z][a-z0-9]{0,15}$/;
const DATE = /^[0-9]{4}-[0-9]{2}-[0-9]{2}$/;
const MINERAL = /^[A-Z]$/;

function coordinate(o, key, room) {
  const v = o[key];
  if (!Number.isInteger(v) || v < 0 || v >= SIZE) throw new Error(`${room}: a ${o.type} has ${key} ${JSON.stringify(v)}`);
  return v;
}

const byPosition = (a, b) => a.x - b.x || a.y - b.y;

/**
 * The fixture text for `room` from its parsed room-terrain and room-objects responses. Throws on any
 * response that does not have the expected shape, so a bad fetch never becomes a fixture.
 */
export function roomFixture(room, shard, fetched, terrainResponse, objectsResponse) {
  if (!ROOM.test(room)) throw new Error(`not a room name: ${JSON.stringify(room)}`);
  if (!SHARD.test(shard)) throw new Error(`not a shard name: ${JSON.stringify(shard)}`);
  if (!DATE.test(fetched)) throw new Error(`not a date: ${JSON.stringify(fetched)}`);
  if (terrainResponse?.ok !== 1 || !Array.isArray(terrainResponse.terrain) || terrainResponse.terrain.length !== 1) {
    throw new Error(`${room}: the terrain response is not one terrain entry`);
  }
  const entry = terrainResponse.terrain[0];
  if (entry.room !== room) throw new Error(`${room}: the terrain response is for ${JSON.stringify(entry.room)}`);
  if (typeof entry.terrain !== 'string' || !TERRAIN.test(entry.terrain)) {
    throw new Error(`${room}: the terrain is not ${SIZE * SIZE} digits 0 to 3`);
  }
  if (objectsResponse?.ok !== 1 || !Array.isArray(objectsResponse.objects)) {
    throw new Error(`${room}: the objects response has no object list`);
  }
  const controllers = [];
  const sources = [];
  const minerals = [];
  for (const o of objectsResponse.objects) {
    if (o === null || typeof o !== 'object') continue;
    if (o.type !== 'controller' && o.type !== 'source' && o.type !== 'mineral') continue;
    if (o.room !== undefined && o.room !== room) throw new Error(`${room}: a ${o.type} is in ${JSON.stringify(o.room)}`);
    const at = { x: coordinate(o, 'x', room), y: coordinate(o, 'y', room) };
    if (o.type === 'controller') controllers.push(at);
    else if (o.type === 'source') sources.push(at);
    else {
      if (typeof o.mineralType !== 'string' || !MINERAL.test(o.mineralType)) {
        throw new Error(`${room}: a mineral has type ${JSON.stringify(o.mineralType)}`);
      }
      minerals.push({ ...at, type: o.mineralType });
    }
  }
  if (controllers.length !== 1) throw new Error(`${room}: ${controllers.length} controllers`);
  if (sources.length === 0) throw new Error(`${room}: no source`);
  sources.sort(byPosition);
  minerals.sort(byPosition);

  const lines = [
    `# ${shard} ${room}: terrain and natural objects from the official server's public room-terrain and`,
    `# room-objects endpoints, fetched ${fetched} (README.md in this directory; js/tools/rooms-fixture.mjs).`,
    `room ${room}`,
    `shard ${shard}`,
    `controller ${controllers[0].x} ${controllers[0].y}`,
    ...sources.map((s) => `source ${s.x} ${s.y}`),
    ...minerals.map((m) => `mineral ${m.type} ${m.x} ${m.y}`),
    'terrain',
  ];
  for (let y = 0; y < SIZE; y++) lines.push(entry.terrain.slice(y * SIZE, (y + 1) * SIZE));
  return `${lines.join('\n')}\n`;
}

/** Parses fixture text back (for the tool test): the header fields and the terrain as one string. */
export function parseRoomFixture(text) {
  const out = { sources: [], minerals: [] };
  const rows = [];
  let inTerrain = false;
  for (const line of text.split('\n')) {
    if (line === '' || line.startsWith('#')) continue;
    if (inTerrain) {
      rows.push(line);
      continue;
    }
    const [key, ...rest] = line.split(' ');
    if (key === 'room' || key === 'shard') out[key] = rest[0];
    else if (key === 'controller') out.controller = { x: Number(rest[0]), y: Number(rest[1]) };
    else if (key === 'source') out.sources.push({ x: Number(rest[0]), y: Number(rest[1]) });
    else if (key === 'mineral') out.minerals.push({ type: rest[0], x: Number(rest[1]), y: Number(rest[2]) });
    else if (key === 'terrain') inTerrain = true;
    else throw new Error(`bad fixture line: ${line.slice(0, 40)}`);
  }
  out.terrain = rows.join('');
  return out;
}

if (isMain(import.meta.url)) {
  const [source, shard, fetched] = process.argv.slice(2);
  if (!source || !shard || !fetched || !SHARD.test(shard) || !DATE.test(fetched)) {
    fail('usage: node js/tools/rooms-fixture.mjs <directory of saved responses> <shard> <fetch date YYYY-MM-DD>');
  }
  const dir = resolve(source);
  const rooms = readdirSync(dir)
    .map((name) => name.match(/^([EW][0-9]{1,3}[NS][0-9]{1,3})\.terrain\.json$/)?.[1])
    .filter(Boolean)
    .sort();
  if (rooms.length === 0) fail(`no <room>.terrain.json in ${dir}`);
  const out = join(ROOMS_DIR, shard);
  mkdirSync(out, { recursive: true });
  for (const room of rooms) {
    const read = (kind) => JSON.parse(readFileSync(join(dir, `${room}.${kind}.json`), 'utf8'));
    try {
      writeFileSync(join(out, `${room}.txt`), roomFixture(room, shard, fetched, read('terrain'), read('objects')));
    } catch (e) {
      fail(e.message);
    }
  }
  console.log(`wrote ${rooms.length} rooms to ${out}`);
}
