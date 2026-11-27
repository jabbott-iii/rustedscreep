// P9.5 end-to-end test of the release wasm against the market API in V447–V454.
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { ROOT } from './lib/common.mjs';
import { CONFIG_SEGMENT, ENGINE_CONSTANTS, commandResults, configRecord, simulate } from './smoke.mjs';
import { CONFIG_LINES, makeWorld, sceneFor } from './terminal-sim.mjs';

const C = Object.fromEntries(ENGINE_CONSTANTS.map(([name, json]) => [name, JSON.parse(json)]));
const HOME = 'W1N1';
const REMOTE = 'W3N1';
const LEVEL = Number(Object.keys(C.CONTROLLER_STRUCTURES.terminal).find((l) => C.CONTROLLER_STRUCTURES.terminal[l] > 0));
const SCAN = C.TERMINAL_COOLDOWN;
const START = 50000000;
// H is the first base mineral. The next slot for it is the start of a 14-slot cycle.
const FIRST = SCAN * 14 - START % (SCAN * 14);
const SELL = FIRST + SCAN;
const STANDING = FIRST + 2 * SCAN;
const LOW_BUCKET = FIRST + 4 * SCAN;
const FAILED_FEE = FIRST + 6 * SCAN;
const REFUSED = FIRST + 8 * SCAN;
const FAILED_TERMINAL = FIRST + 10 * SCAN;
const FLOOR = 1000;
const KEEP = 30000;
const ENERGY_KEEP = 1000;
const VOLUME = 1000;
const PRICE = 1.25;
const AFFORDABLE = 150;
const H = C.RESOURCE_HYDROGEN;
const L = C.RESOURCE_LEMERGIUM;
const K = C.RESOURCE_KEANIUM;
const BUY_ORDER = C.ORDER_BUY;
const SCALE = 30; // TERMINAL_SEND_COST_SCALE, V432.
const SELL_ORDER = C.ORDER_SELL;

function run(enabled) {
  const world = makeWorld([{ name: HOME, level: LEVEL, spawn: true, store: { energy: 3000 } }]);
  const terminal = world.rooms[0].terminal;
  const config = [
    ...CONFIG_LINES,
    `market.enabled=${enabled}`,
    `market.scan_ticks=${SCAN}`,
    'market.scan_orders=4',
    `market.max_amount=${VOLUME}`,
    `market.credit_floor=${FLOOR}`,
    `market.sell_keep=${KEEP}`,
    `market.energy_keep=${ENERGY_KEEP}`,
  ];
  let credits = FLOOR + AFFORDABLE * PRICE;
  const accesses = [];
  const books = [];
  const deals = [];
  const fills = [];
  const costCalls = [];
  let pending;
  const quote = (id, resourceType, type, price, roomName = REMOTE) => ({
    id, resourceType, type, price, roomName, amount: 2 * VOLUME, remainingAmount: 3 * VOLUME,
  });
  const publicBook = (offset) => {
    if (offset === FIRST) {
      const rows = [
        quote('over-buy-ceiling', H, SELL_ORDER, 2.001),
        quote('own-room', H, SELL_ORDER, 0.01, HOME),
        quote('buy', H, SELL_ORDER, PRICE),
        quote('sub-milli-price', H, SELL_ORDER, 0.0311),
        quote('beyond-scan-cap', H, SELL_ORDER, 0.01),
      ];
      const start = Math.floor((START + FIRST) / (14 * SCAN)) * 4 % rows.length;
      return rows.map((_, i) => rows[(i + rows.length - start) % rows.length]);
    }
    if (offset === SELL) return [
      quote('under-sell-floor', H, BUY_ORDER, 0.099),
      quote('sell', H, BUY_ORDER, 1),
    ];
    if (offset === FAILED_FEE) return [quote('fee-fault', L, SELL_ORDER, 1)];
    if (offset === REFUSED) return [quote('refused', K, SELL_ORDER, 1)];
    return [];
  };

  const exactFee = (amount) => Math.ceil(amount * (1 - Math.exp(-2 / SCALE)));
  const result = simulate(join(ROOT, 'dist'), {
    ticks: FAILED_TERMINAL + 4,
    bucket: (o) => o === LOW_BUCKET ? 1000 : 10000,
    setup: (_context, w) => { w.segments[CONFIG_SEGMENT] ??= configRecord(config); },
    scene: (offset) => {
      if (offset === SELL) terminal.store[H] = KEEP + 2 * VOLUME;
      const orders = publicBook(offset);
      const scene = sceneFor(world, offset);
      if (offset === FAILED_TERMINAL) {
        Object.defineProperty(scene.structures[terminal.id], 'cooldown', {
          get() { throw new Error('test terminal getter error'); },
        });
      }
      Object.defineProperty(scene, 'market', { enumerable: true, value: {
        get orders() {
          accesses.push(offset);
          return offset === STANDING ? { standing: { type: BUY_ORDER } } : {};
        },
        get credits() { return credits; },
        getAllOrders(filter) {
          books.push([offset, { ...filter }]);
          return orders;
        },
        calcTransactionCost(amount, from, to) {
          assert.deepEqual([from, to], [HOME, REMOTE]);
          costCalls.push(offset);
          if (offset === FAILED_FEE) throw new Error('test error');
          return exactFee(amount);
        },
        deal(id, amount, room) {
          deals.push([offset, id, amount, room]);
          const order = orders.find((o) => o.id === id);
          assert(order && room === HOME && amount > 0 && amount <= VOLUME);
          const selling = order.type === BUY_ORDER;
          const fee = exactFee(amount);
          assert(terminal.cooldownTime <= offset && terminal.store.energy - fee >= ENERGY_KEEP);
          if (offset === REFUSED) return C.ERR_TIRED;
          pending = () => {
            const stock = terminal.store[order.resourceType] ?? 0;
            terminal.store[order.resourceType] = stock + (selling ? -amount : amount);
            terminal.store.energy -= fee;
            terminal.cooldownTime = offset + C.TERMINAL_COOLDOWN;
            credits += (selling ? 1 : -1) * amount * order.price;
            assert(credits >= FLOOR);
            fills.push({ credits, stock: terminal.store[order.resourceType] });
          };
          return C.OK;
        },
      } });
      return scene;
    },
    command: (w) => {
      pending?.();
      pending = undefined;
      return w.ticksRun === FAILED_TERMINAL + 3 ? 'rustedscreep.market()' : null;
    },
  });

  assert.equal(result.halts, 0);
  assert.equal(result.globals, 1);
  const [summary] = commandResults(result, 'rustedscreep.market()');
  if (!enabled) {
    assert.equal(accesses.length + books.length + costCalls.length + deals.length, 0);
    assert.match(summary, /off \| scans 0, queued 0, refused 0/);
    return;
  }
  assert.deepEqual(deals, [
    [FIRST, 'buy', AFFORDABLE, HOME],
    [SELL, 'sell', VOLUME, HOME],
    [REFUSED, 'refused', VOLUME, HOME],
  ]);
  assert.deepEqual(fills, [
    { credits: FLOOR, stock: AFFORDABLE },
    { credits: FLOOR + VOLUME, stock: KEEP + VOLUME },
  ]);
  assert.deepEqual(books, [
    [FIRST, { resourceType: H, type: SELL_ORDER }],
    [SELL, { resourceType: H, type: BUY_ORDER }],
    [FAILED_FEE, { resourceType: L, type: SELL_ORDER }],
    [REFUSED, { resourceType: K, type: SELL_ORDER }],
  ]);
  assert(accesses.includes(STANDING) && !accesses.includes(LOW_BUCKET));
  assert.deepEqual(costCalls, [FIRST, SELL, FAILED_FEE, REFUSED]);
  assert.match(summary, /on \| scans 4, queued 2, refused 1/);
  assert(result.logs.some((line) => line.includes('invalid market transaction cost')));
  assert(result.logs.some((line) => line.includes('market property read failed')));
  assert(!accesses.includes(FAILED_TERMINAL));
}

function internal_send_has_priority() {
  const receiver = 'W1N2';
  const mineralKeep = C.LAB_MINERAL_CAPACITY;
  const state = makeWorld([
    { name: HOME, level: LEVEL, spawn: true, store: { [H]: 2 * mineralKeep, energy: 3000 } },
    { name: receiver, level: LEVEL, store: { energy: 3000 } },
  ]);
  const config = [...CONFIG_LINES, 'market.enabled=true', `market.scan_ticks=${SCAN}`];
  const marketReads = [];
  simulate(join(ROOT, 'dist'), {
    ticks: FIRST + 1,
    setup: (_context, w) => { w.segments[CONFIG_SEGMENT] ??= configRecord(config); },
    scene: (offset) => {
      // A new member triggers a fresh balancing read on the due market tick.
      const rooms = offset >= FIRST - 1 ? state.rooms : state.rooms.slice(0, 1);
      return {
        ...sceneFor({ ...state, rooms }, offset),
        market: { get orders() { marketReads.push(offset); return {}; } },
      };
    },
  });
  assert(state.calls.some((call) => call.offset === FIRST && call.result === C.OK));
  assert(!marketReads.includes(FIRST));
}

run(false);
run(true);
internal_send_has_priority();
console.log('market-sim ok: buy to the credit floor, sell at the volume cap, gated scans and isolated refusals');
