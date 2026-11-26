// The loader's boot, run and recovery state machine (ADR-003). Every Screeps global it needs comes in
// through `Env` and the wasm through `Glue`, so js/test/loader.test.mjs can drive it natively; main.ts
// wires it to the real runtime. No game logic lives here.
import { decodeBase64, decodeBase64Into, decodedLength } from './base64.js';
import { crc32Hex } from './crc32.js';
import { inflateRaw } from './inflate.js';

/** Loader thresholds, injected at build time from rustedscreep-core's config.rs (LOADER_MMO). */
export interface LoaderConfig {
  bootBucketMin: number;
  bootSplitTicks: boolean;
  recoveryMode: 'halt' | 'reinstantiate';
  backoffWindowTicks: number;
  backoffIncidents: number;
  backoffBaseTicks: number;
  backoffMaxTicks: number;
  notifyGroupMinutes: number;
  trapTestMax: number;
}

/** What npm run build knows about this bundle. */
export interface BuildInfo {
  id: string;
  /** CRC-32 of the .wasm that npm run stage reports, to compare with the bytes require() returns (V77). */
  wasmCrc32: string;
  features: string[];
  /**
   * The characters of each part of the wasm's base64 text, part 0 first (ADR-028): the GitHub link delivers the
   * wasm as `rustedscreep_wasm` and, when one module would pass the largest size seen intact (V257), also as
   * `rustedscreep_wasm_1`, `rustedscreep_wasm_2`, … A text part of any other length is a load incident.
   */
  wasmParts: number[];
  /**
   * The wasm's length once inflated, when the text parts hold the base64 of a raw DEFLATE stream of it (ADR-040: the
   * upload must fit the MMO's 2 MB code cap, V76, V370); absent or 0 when they hold the wasm itself (ADR-015's form).
   * A binary module under `rustedscreep_wasm` is the wasm itself either way.
   */
  inflatedBytes?: number;
}

/** The wasm-bindgen glue, as the loader uses it. */
export interface Glue {
  /** Instantiates `module` (initSync); this runs the Rust start function. */
  init(module: WebAssembly.Module): void;
  /**
   * Re-instantiates from the module the glue holds and reruns the start function (wasm-bindgen's
   * __wbg_reset_state, from --experimental-reset-state-function; V97). `init` cannot do this: it
   * returns early once an instance exists, even a poisoned one.
   */
  reset?: () => void;
  loop(): void;
  /** Present only in a `--features trap-test` build. */
  trapTest?: () => void;
  /** One line about the wasm's latest snapshot (P1 module 5), for a console command. */
  snapshotSummary?: () => string;
  /** One line about the wasm's scheduler: tier caps, work done, faults (P1 module 6), for a console command. */
  schedulerSummary?: () => string;
  /** One line about the act layer's last tick: intents queued, refused, blocked, issued (P1 module 7). */
  actSummary?: () => string;
  /** One line of CPU statistics from a `profiler` build, or a note that the build has none (P1 module 8). */
  profilerSummary?: () => string;
  /** Asks every own creep to say `text` on the next tick (P1 module 7); returns a confirmation line. */
  queueSay?: (text: string) => string;
  /** One line about the segment manager: active set, wanted loads, dirty records, counts, self-test (P1 module 9). */
  persistSummary?: () => string;
  /** Starts the persistence self-test with `bytes` payload bytes, the largest that fits when undefined (P1 module 9). */
  persistTest?: (bytes?: number) => string;
  /** One line about the runtime config: profile, stored record, overrides, the values in force (P1 module 10). */
  configSummary?: () => string;
  /** Overrides config field `key` with `value` (as text) from the next tick; returns the outcome (P1 module 10). */
  configSet?: (key: string, value: string) => string;
  /** Removes the override of config field `key`; returns the outcome (P1 module 10). */
  configUnset?: (key: string) => string;
  /** Removes every config override; returns the outcome (P1 module 10). */
  configReset?: () => string;
  /** One line about the constants self-check: its phase, counts, findings and known differences (P1 module 11). */
  constsSummary?: () => string;
  /** One line about movement: rooms, grids, cached fields, counters and the console's move order (P2 movement). */
  movementSummary?: () => string;
  /**
   * Sets the console's move order: own creeps in `room` walk to within `range` of (x, y) from the next tick. With
   * every argument undefined it cancels the order. Returns the outcome (P2 movement).
   */
  moveTo?: (room?: string, x?: number, y?: number, range?: number, from?: string) => string;
  /** One line about the spawn path: spawn rooms, the last tick's requests and orders, totals, creeps by role (P2 spawn). */
  spawnerSummary?: () => string;
  /**
   * Sets the console's spawn request: a spawn of `room` spawns one creep of `role` (worker when undefined). With
   * every argument undefined it cancels the request. Returns the outcome (P2 spawn).
   */
  spawn?: (room?: string, role?: string) => string;
  /** One line about the RCL1–3 opening: per owned room its workers, tasks, plan, spawn request and watchdog (P2 opening). */
  openingSummary?: () => string;
  /** One line about the base planner: per owned room its plan and its placement's last look, totals, CPU per phase (P4.3). */
  plannerSummary?: () => string;
  /** One line about remote mining: per owned home its candidate rooms, the rooms it mines and their creeps (P6.2). */
  remoteSummary?: () => string;
  /** One line about the controller signer: its record's rooms, the signer and its trip, the next one's plan (ADR-030). */
  signSummary?: () => string;
  /** One line about scouting: the atlas, the rooms due, the explorer and its trip, the observers' looks (ADR-039). */
  scoutSummary?: () => string;
  /** One line about claim scoring: the candidates, why rooms were left out, the plan running, the best (ADR-041). */
  claimsSummary?: () => string;
  /** One line about mineral mining: per room why it mines, its last read, its plan, its creeps (P9.1, ADR-044). */
  mineralsSummary?: () => string;
  /** One line about lab reactions: the targets, per room its labs' state, roles, job and hauler (P9.2, ADR-045). */
  labsSummary?: () => string;
  /** One line about the factory: its settings and pace, per room its job, stocks and hauler (P9.3, ADR-046). */
  factorySummary?: () => string;
  /** One line about terminal balancing: its settings, the next plan, per terminal its read, waiting send and counters (P9.4, ADR-047). */
  terminalsSummary?: () => string;
}

/** Loader state kept in Memory, so it survives a halt. Read at boot and written only on incidents. */
export interface LoaderMemory {
  /** Game.time of recent incidents that count toward backoff (newest last; the last 20 are kept). */
  incidents: number[];
  /**
   * Consecutive incidents, each within backoffWindowTicks plus the delay then in force of the previous
   * one. Backoff starts when it reaches backoffIncidents and doubles with each further incident.
   */
  streak: number;
  /** Traps still to fire in a trap-test run. */
  pendingTraps: number;
  trapRun?: {
    total: number;
    startedAt: number;
    /** Game.time of the last trap, while its recovery is not yet seen. */
    awaitingSince?: number;
    /** Ticks from each trap to the next successful loop. */
    recoveryTicks: number[];
  };
}

export interface Env {
  time(): number;
  bucket(): number;
  cpuUsed(): number;
  /** Game.cpu.halt(); false when the server has none, so the loader must recover another way. */
  halt(): boolean;
  notify(message: string, groupMinutes: number): void;
  log(message: string): void;
  /** require() of a bundled module; throws when it does not exist. */
  requireModule(name: string): unknown;
  compile(bytes: Uint8Array): WebAssembly.Module;
  /** The loader's Memory record, created on first use. */
  memory(): LoaderMemory;
  /** `typeof` of the globals VERIFICATION_LOG V95 asks about, for the boot log. */
  globalsReport(): string;
}

export const WASM_MODULE = 'rustedscreep_wasm';
/** A module the user may add in the in-game editor to see whether a GitHub sync deletes it (V77). */
export const SENTINEL_MODULE = 'sentinel';
const MAX_INCIDENTS_KEPT = 20;
const DEFERRED_LOG_EVERY = 100;
const ERROR_TEXT_MAX = 500;
const CONFIG_KEY_HELP = 'rustedscreep: give the config key as a string, for example rustedscreep.configSet("scheduler.tick_reserve_pct", 12); rustedscreep.config() lists the keys';

type Phase = 'idle' | 'compiled' | 'ready' | 'poisoned';

function isFiniteNumber(v: unknown): v is number {
  return typeof v === 'number' && Number.isFinite(v);
}

/**
 * Repairs a loader record read from Memory, in place where it can (CLAUDE.md: missing or corrupt data
 * triggers a rebuild, never a crash). A malformed trap-test run is dropped together with its pending traps.
 */
export function sanitizeLoaderMemory(raw: unknown, trapTestMax: number): LoaderMemory {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return { incidents: [], streak: 0, pendingTraps: 0 };
  const rec = raw as Record<string, unknown>;
  const incidents = Array.isArray(rec.incidents) ? rec.incidents.filter(isFiniteNumber).slice(-MAX_INCIDENTS_KEPT) : [];
  const streak = isFiniteNumber(rec.streak) && rec.streak >= 0 && incidents.length > 0 ? Math.trunc(rec.streak) : 0;
  const run = rec.trapRun as Record<string, unknown> | null | undefined;
  let trapRun: LoaderMemory['trapRun'];
  if (
    run !== null && typeof run === 'object' && isFiniteNumber(run.total) && isFiniteNumber(run.startedAt) &&
    Array.isArray(run.recoveryTicks) && (run.awaitingSince === undefined || isFiniteNumber(run.awaitingSince))
  ) {
    trapRun = { total: run.total, startedAt: run.startedAt, recoveryTicks: run.recoveryTicks.filter(isFiniteNumber) };
    if (run.awaitingSince !== undefined) trapRun.awaitingSince = run.awaitingSince;
  }
  const pending = trapRun && isFiniteNumber(rec.pendingTraps) ? Math.min(Math.max(0, Math.trunc(rec.pendingTraps)), trapTestMax) : 0;
  const out = rec as unknown as LoaderMemory;
  out.incidents = incidents;
  out.streak = streak;
  out.pendingTraps = pending;
  if (trapRun) out.trapRun = trapRun;
  else delete out.trapRun;
  return out;
}

function typeTag(value: unknown): string {
  const tag = Object.prototype.toString.call(value).slice(8, -1);
  const ctor = value !== null && typeof value === 'object' ? (value as { constructor?: { name?: string } }).constructor?.name : undefined;
  return ctor && ctor !== tag ? `${tag}(${ctor})` : tag;
}

/**
 * The wasm's bytes, whatever require() returned: a binary module's view or ArrayBuffer (an API upload, the private
 * server, the sandbox; V31: never trust instanceof), or the base64 string the text module exports (the GitHub link,
 * ADR-015).
 */
export function toBytes(value: unknown): Uint8Array {
  if (typeof value === 'string') {
    try {
      return decodeBase64(value);
    } catch (e) {
      throw new TypeError(`require('${WASM_MODULE}') returned a String that is not base64 (${message(e)})`);
    }
  }
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  if (Object.prototype.toString.call(value) === '[object ArrayBuffer]') return new Uint8Array(value as ArrayBuffer);
  throw new TypeError(`require('${WASM_MODULE}') returned ${typeTag(value)}, not bytes`);
}

/** The module that holds part `k` of the wasm's base64 text (ADR-028): `rustedscreep_wasm`, then `rustedscreep_wasm_1`, … */
export function partModuleName(k: number): string {
  return k === 0 ? WASM_MODULE : `${WASM_MODULE}_${k}`;
}

/**
 * The wasm's bytes, and the form that ran for the boot line. A binary module under `rustedscreep_wasm` (an API
 * upload, the private server, the sandbox) is used alone, as `toBytes` reads it. A string there is part 0 of the
 * text form the GitHub link delivers (ADR-015, ADR-028): every part `wasmParts` lists must be a string of exactly
 * its recorded length, only the last may end in '=', and the parts are decoded into one buffer, so a missing,
 * empty, cut or misplaced part throws a TypeError that names it before anything is decoded or compiled (an empty
 * part 0 with toBytes's text, `returned Object, not bytes`, since a non-string part 0 is read as a binary module).
 * A part of the recorded length with other content decodes, and the CRC-32 check reports it (ADR-015). With
 * `inflatedBytes` above 0 the decoded text is a raw DEFLATE stream (ADR-040), inflated to exactly that many bytes; a
 * stream that does not inflate throws a TypeError, and the boot line's tag adds the stream's length.
 */
export function loadWasm(
  requireModule: (name: string) => unknown,
  wasmParts: readonly number[],
  inflatedBytes = 0,
): { bytes: Uint8Array; tag: string } {
  // A part that is missing, or cut short (a SyntaxError that does not name its module), is named, part 0 too.
  let first: unknown;
  try {
    first = requireModule(WASM_MODULE);
  } catch (e) {
    throw new TypeError(`require('${WASM_MODULE}') threw ${message(e)}`);
  }
  // A non-string part 0 is read as a binary module; an empty text module ({}) fails here, as on 5217308 (V257).
  if (typeof first !== 'string') return { bytes: toBytes(first), tag: typeTag(first) };
  const count = Array.isArray(wasmParts) ? wasmParts.length : 0;
  if (count === 0) throw new TypeError(`require('${WASM_MODULE}') returned a String, but this build records no text parts`);
  const texts: string[] = [];
  let chars = 0;
  for (let k = 0; k < count; k++) {
    const name = partModuleName(k);
    const want = wasmParts[k] as number;
    let value: unknown = first;
    if (k > 0) {
      try {
        value = requireModule(name);
      } catch (e) {
        throw new TypeError(`require('${name}') threw ${message(e)}`);
      }
    }
    if (typeof value !== 'string') throw new TypeError(`require('${name}') returned ${typeTag(value)}, not a ${want}-character string`);
    if (value.length !== want) throw new TypeError(`require('${name}') returned ${value.length} characters, not ${want}`);
    if (k < count - 1 && value.charCodeAt(want - 1) === 61) throw new TypeError(`require('${name}') ends in '=' but is not the last part`);
    texts.push(value);
    chars += want;
  }
  const notBase64 = (k: number, e: unknown) =>
    new TypeError(`require('${partModuleName(k)}') returned a String that is not base64 (${message(e)})`);
  let total = 0;
  for (let k = 0; k < count; k++) {
    try {
      total += decodedLength(texts[k] as string);
    } catch (e) {
      throw notBase64(k, e);
    }
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (let k = 0; k < count; k++) {
    try {
      offset += decodeBase64Into(texts[k] as string, bytes, offset);
    } catch (e) {
      throw notBase64(k, e);
    }
  }
  const parts = `${chars} chars${count > 1 ? ` in ${count} parts` : ''}`;
  if (!(inflatedBytes > 0)) return { bytes, tag: `String(base64, ${parts})` };
  let wasm: Uint8Array;
  try {
    wasm = inflateRaw(bytes, inflatedBytes);
  } catch (e) {
    throw new TypeError(`the wasm text modules' ${bytes.length}-byte DEFLATE stream does not inflate to ${inflatedBytes} bytes (${message(e)})`);
  }
  return { bytes: wasm, tag: `String(base64, ${parts}, deflate ${bytes.length} B)` };
}

function message(e: unknown): string {
  return e instanceof Error ? `${e.name}: ${e.message}` : String(e);
}

export class Loader {
  phase: Phase = 'idle';
  /** Set while the wasm runs; still set at the start of a tick means that tick was aborted. */
  running = false;
  private module: WebAssembly.Module | undefined;
  private lastError = '';
  private lastDeferredLog = -Infinity;
  private reinstantiations: number[] = [];
  /** Whether the incident that poisoned the instance was a trap-test trap (exempt from the caps). */
  private poisonedByTest = false;
  /** A CRC-32 mismatch is notified once per global; repeated boot attempts only log it. */
  private mismatchNotified = false;
  private stoppedLogged = false;
  /** Set while a capped or failed re-instantiation waits to be retried (logged once per episode). */
  private retryWaitLogged = false;
  private bootCpu: { load: number; compile: number; instantiate: number } | undefined;
  private bootStartedAt: number | undefined;

  constructor(
    private readonly env: Env,
    private readonly glue: Glue,
    private readonly config: LoaderConfig,
    private readonly build: BuildInfo,
  ) {}

  /** The text of the last console.error, attached to the next incident (the panic hook writes there). */
  noteError(text: string): void {
    this.lastError = text.length > ERROR_TEXT_MAX ? `${text.slice(0, ERROR_TEXT_MAX)}...` : text;
  }

  tick(): void {
    if (this.running) {
      this.running = false;
      this.phase = 'poisoned';
      this.incident('aborted', 'the previous tick ended inside the wasm (CPU limit?)');
      return;
    }
    if (this.phase === 'poisoned') {
      this.recover();
      return;
    }
    if (this.phase === 'idle') {
      if (!this.boot()) return;
      if (this.config.bootSplitTicks) return;
    }
    if (this.phase === 'compiled') {
      if (!this.instantiate()) return;
    }
    if (this.phase === 'ready') this.run();
  }

  private deferred(text: string): void {
    const t = this.env.time();
    if (t - this.lastDeferredLog >= DEFERRED_LOG_EVERY) {
      this.lastDeferredLog = t;
      this.env.log(`rustedscreep boot deferred: ${text}`);
    }
  }

  /** The boot delay after `streak` consecutive incidents: 0 below backoffIncidents, then doubling to the cap. */
  delayFor(streak: number): number {
    if (streak < this.config.backoffIncidents) return 0;
    return Math.min(this.config.backoffMaxTicks, this.config.backoffBaseTicks * 2 ** (streak - this.config.backoffIncidents));
  }

  /** The tick before which the next boot waits (0 = boot now). An incident dated in the future is ignored. */
  backoffUntil(mem: LoaderMemory, now: number): number {
    const last = mem.incidents.at(-1);
    if (last === undefined || last > now) return 0;
    const delay = this.delayFor(mem.streak);
    return delay > 0 ? last + delay : 0;
  }

  private boot(): boolean {
    const now = this.env.time();
    const bucket = this.env.bucket();
    if (bucket < this.config.bootBucketMin) {
      this.deferred(`bucket ${bucket} < ${this.config.bootBucketMin}`);
      return false;
    }
    const mem = this.env.memory();
    const until = this.backoffUntil(mem, now);
    if (now < until) {
      this.deferred(`backing off after ${mem.streak} consecutive incidents, until tick ${until}`);
      return false;
    }

    this.bootStartedAt = now;
    const c0 = this.env.cpuUsed();
    let bytes: Uint8Array;
    let tag: string;
    try {
      // The boot line shows which delivery ran: a binary module's type, or the text modules' base64 (ADR-015, ADR-028)
      // and, for a DEFLATE stream, its length (ADR-040; the inflate falls in `load+crc`).
      ({ bytes, tag } = loadWasm((name) => this.env.requireModule(name), this.build.wasmParts, this.build.inflatedBytes));
    } catch (e) {
      this.incident('load', message(e));
      return false;
    }
    const crc = crc32Hex(bytes);
    let sentinel = 'absent';
    try {
      this.env.requireModule(SENTINEL_MODULE);
      sentinel = 'present';
    } catch {
      // A missing module throws; absent is the normal case.
    }
    const c1 = this.env.cpuUsed();
    this.env.log(
      `rustedscreep boot: build ${this.build.id} | Game.time ${now} | wasm ${tag} ${bytes.length} B crc32 ${crc} ` +
        `(stage ${this.build.wasmCrc32}: ${crc === this.build.wasmCrc32 ? 'match' : 'MISMATCH'}) | sentinel ${sentinel} | ` +
        `${this.env.globalsReport()}`,
    );
    if (crc !== this.build.wasmCrc32 && !this.mismatchNotified) {
      this.mismatchNotified = true;
      this.env.notify(`rustedscreep: wasm crc32 ${crc} does not match the build's ${this.build.wasmCrc32} (build ${this.build.id})`,
        this.config.notifyGroupMinutes);
    }
    try {
      this.module = this.env.compile(bytes);
    } catch (e) {
      this.incident('compile', message(e));
      return false;
    }
    const c2 = this.env.cpuUsed();
    this.bootCpu = { load: c1 - c0, compile: c2 - c1, instantiate: 0 };
    this.phase = 'compiled';
    return true;
  }

  private instantiate(): boolean {
    if (!this.module) {
      this.phase = 'idle';
      return false;
    }
    const c0 = this.env.cpuUsed();
    // Instantiation runs Rust (the start function), so a tick killed here must also count as aborted.
    this.running = true;
    try {
      this.glue.init(this.module);
      this.running = false;
    } catch (e) {
      // A failed start function can leave the glue half-initialized, so treat it like a trap.
      this.running = false;
      this.phase = 'poisoned';
      this.incident('instantiate', message(e));
      return false;
    }
    if (this.bootCpu) this.bootCpu.instantiate = this.env.cpuUsed() - c0;
    this.phase = 'ready';
    return true;
  }

  private run(): void {
    const firstAfterBoot = this.bootCpu !== undefined;
    const c0 = firstAfterBoot ? this.env.cpuUsed() : 0;
    // Memory is read only on the first loop after a boot (to see a pending recovery) and in trap-test
    // builds; a normal tick never touches it.
    const record = firstAfterBoot || this.glue.trapTest ? this.env.memory() : undefined;
    // A trap counts as recovered once a fresh instance is about to run, even if that run is the next trap.
    if (record?.trapRun?.awaitingSince !== undefined) this.recovered(record);
    const trapNow = record !== undefined && record.pendingTraps > 0 && this.glue.trapTest !== undefined;
    this.running = true;
    try {
      if (trapNow && record && this.glue.trapTest) {
        record.pendingTraps -= 1;
        if (record.trapRun) record.trapRun.awaitingSince = this.env.time();
        this.glue.trapTest();
      } else {
        this.glue.loop();
      }
      this.running = false;
    } catch (e) {
      this.running = false;
      this.phase = 'poisoned';
      this.poisonedByTest = trapNow;
      this.incident(trapNow ? 'trap-test' : 'trap', message(e));
      return;
    }
    if (firstAfterBoot && this.bootCpu) {
      const loopCpu = this.env.cpuUsed() - c0;
      const b = this.bootCpu;
      this.env.log(
        `rustedscreep booted in ${this.env.time() - (this.bootStartedAt ?? this.env.time())} ticks: ` +
          `load+crc ${b.load.toFixed(2)} CPU, compile ${b.compile.toFixed(2)}, instantiate ${b.instantiate.toFixed(2)}, ` +
          `first loop ${loopCpu.toFixed(2)}`,
      );
      this.bootCpu = undefined;
    }
    if (record) this.finishTrapRun(record);
  }

  private recovered(record: LoaderMemory): void {
    const run = record.trapRun;
    if (!run || run.awaitingSince === undefined) return;
    run.recoveryTicks.push(this.env.time() - run.awaitingSince);
    run.awaitingSince = undefined;
    this.env.log(`rustedscreep trap-test: recovery ${run.recoveryTicks.length}/${run.total} after ${run.recoveryTicks.at(-1)} ticks`);
  }

  /** After a successful loop: if every scheduled trap has recovered, report the run and clear it. */
  private finishTrapRun(record: LoaderMemory): void {
    const run = record.trapRun;
    if (run && record.pendingTraps === 0 && run.recoveryTicks.length >= run.total) {
      const summary = `rustedscreep trap-test done: ${run.total} traps, each recovered (ticks: ${run.recoveryTicks.join(', ')})`;
      this.env.log(summary);
      this.env.notify(summary, this.config.notifyGroupMinutes);
      record.trapRun = undefined;
    }
  }

  private incident(kind: 'aborted' | 'load' | 'compile' | 'instantiate' | 'trap' | 'trap-test', detail: string): void {
    const now = this.env.time();
    const mem = this.env.memory();
    const text = `rustedscreep incident: ${kind} at tick ${now} (build ${this.build.id}): ${detail}` +
      (this.lastError ? ` | console.error: ${this.lastError}` : '');
    this.lastError = '';
    this.env.log(text);
    if (kind === 'trap-test') return;
    const prev = mem.incidents.at(-1);
    const continues = prev !== undefined && now >= prev && now - prev < this.config.backoffWindowTicks + this.delayFor(mem.streak);
    mem.streak = continues ? mem.streak + 1 : 1;
    mem.incidents.push(now);
    if (mem.incidents.length > MAX_INCIDENTS_KEPT) mem.incidents.splice(0, mem.incidents.length - MAX_INCIDENTS_KEPT);
    this.env.notify(text, this.config.notifyGroupMinutes);
  }

  private recover(): void {
    // Nothing is left to try once stopped; the next global reset (a code push or a VM restart) boots again.
    if (this.stoppedLogged) return;
    const now = this.env.time();
    let result: 'done' | 'unavailable' | 'capped' | 'failed';
    if (this.config.recoveryMode === 'halt') {
      if (!this.retryWaitLogged) this.env.log('rustedscreep recovering: Game.cpu.halt() resets the global; the next tick boots again (ADR-003 a)');
      if (this.env.halt()) return;
      // No halt here: re-instantiate instead, so the bot still recovers without human action.
      result = this.reinstantiate(now, 'Game.cpu.halt is not available');
    } else {
      result = this.reinstantiate(now, 'recovery mode reinstantiate');
      if (result === 'done') return;
      this.env.log('rustedscreep recovering: Game.cpu.halt() resets the global; the next tick boots again (ADR-003 a)');
      if (this.env.halt()) return;
    }
    if (result === 'done') return;
    if (result === 'unavailable') {
      this.stopped('neither Game.cpu.halt nor a glue reset is available');
      return;
    }
    // Capped or failed, with no halt to fall back on: wait for the cap window and try again.
    if (!this.retryWaitLogged) {
      this.retryWaitLogged = true;
      const text = `rustedscreep: re-instantiation ${result} and Game.cpu.halt is not available; retrying as the ` +
        `${this.config.backoffWindowTicks}-tick cap window allows`;
      this.env.log(text);
      this.env.notify(text, this.config.notifyGroupMinutes);
    }
  }

  /** ADR-003 option (b). Capped at backoffIncidents per window (trap-test traps exempt). */
  private reinstantiate(now: number, why: string): 'done' | 'unavailable' | 'capped' | 'failed' {
    if (!this.glue.reset) return 'unavailable';
    this.reinstantiations = this.reinstantiations.filter((t) => now - t < this.config.backoffWindowTicks);
    // Trap-test recoveries do not count toward the cap, so a 10-trap run measures option (b) every time.
    const byTest = this.poisonedByTest;
    this.poisonedByTest = false;
    if (!byTest && this.reinstantiations.length >= this.config.backoffIncidents) return 'capped';
    if (!byTest) this.reinstantiations.push(now);
    try {
      this.running = true;
      this.glue.reset();
      this.running = false;
      this.phase = 'ready';
      this.retryWaitLogged = false;
      this.env.log(`rustedscreep recovered by re-instantiating from the cached module (ADR-003 b; ${why})`);
      return 'done';
    } catch (e) {
      this.running = false;
      this.env.log(`rustedscreep re-instantiation failed (${message(e)})`);
      return 'failed';
    }
  }

  private stopped(why: string): void {
    if (this.stoppedLogged) return;
    this.stoppedLogged = true;
    const text = `rustedscreep stopped: ${why}; the wasm stays stopped until the next global reset`;
    this.env.log(text);
    this.env.notify(text, this.config.notifyGroupMinutes);
  }

  /** Console command: schedule `n` traps, one per boot, to test recovery (P1 gate: 10 in a row). */
  scheduleTraps(n: number): string {
    if (!this.glue.trapTest) return 'rustedscreep: this build has no trapTest export; build with npm run build -- --features trap-test';
    if (!Number.isInteger(n) || n < 1 || n > this.config.trapTestMax) return `rustedscreep: give a whole number from 1 to ${this.config.trapTestMax}`;
    const mem = this.env.memory();
    mem.pendingTraps = n;
    mem.trapRun = { total: n, startedAt: this.env.time(), recoveryTicks: [] };
    return `rustedscreep: ${n} trap(s) scheduled; each fires in the next loop after a boot`;
  }

  /** Console command: the wasm's summary of this tick's snapshot. */
  snapshot(): string {
    return this.summary('snapshotSummary', 'snapshot');
  }

  /** Console command: the wasm's summary of the scheduler's last tick. */
  scheduler(): string {
    return this.summary('schedulerSummary', 'scheduler summary');
  }

  /** Console command: the wasm's summary of the act layer's last tick. */
  act(): string {
    return this.summary('actSummary', 'act summary');
  }

  /** Console command: the wasm's profiler statistics (open window, act cost per call, segment writes). */
  profiler(): string {
    return this.summary('profilerSummary', 'profiler summary');
  }

  /** Console command: every own creep says `text` on the next tick (a free intent; checks the act path). */
  say(text: unknown): string {
    const call = this.glue.queueSay;
    if (!call) return 'rustedscreep: this build has no queueSay export';
    if (typeof text !== 'string') return 'rustedscreep: give the text as a string, for example rustedscreep.say("hi")';
    return this.guarded('queueSay', 'say', () => call(text));
  }

  /** Console command: the wasm's segment-manager summary (active set, loads, writes, the self-test). */
  persist(): string {
    return this.summary('persistSummary', 'persist summary');
  }

  /**
   * Console command: a storage round trip through the segment manager (the P1 gate's live check). `bytes`
   * is the payload size, or undefined for the largest that fits one segment; the wasm clamps larger sizes.
   */
  persistTest(bytes: unknown): string {
    const call = this.glue.persistTest;
    if (!call) return 'rustedscreep: this build has no persistTest export';
    if (bytes !== undefined && !(typeof bytes === 'number' && Number.isInteger(bytes) && bytes >= 0 && bytes <= 0xffffffff)) {
      return 'rustedscreep: give the payload size as a whole number of bytes, or nothing for a full segment, for example rustedscreep.persistTest(1000)';
    }
    return this.guarded('persistTest', 'persist test', () => call(bytes));
  }

  /** Console command `rustedscreep.config()`: the wasm's runtime config (profile, stored record, overrides, values in force). */
  runtimeConfig(): string {
    return this.summary('configSummary', 'config summary');
  }

  /**
   * Console command: overrides config field `key` from the next tick. `value` is a number, true or false, or a
   * level name; the wasm clamps a number to the field's safe range and refuses what it cannot use.
   */
  configSet(key: unknown, value: unknown): string {
    const call = this.glue.configSet;
    if (!call) return 'rustedscreep: this build has no configSet export';
    if (typeof key !== 'string') return CONFIG_KEY_HELP;
    const ok = (typeof value === 'number' && Number.isFinite(value)) || typeof value === 'boolean' || typeof value === 'string';
    if (!ok) {
      return 'rustedscreep: give the value as a number, true or false, or a level name, for example rustedscreep.configSet("log.level", "debug")';
    }
    const text = String(value);
    return this.guarded('configSet', 'config change', () => call(key, text));
  }

  /** Console command: removes the override of config field `key`, so the profile's value applies again. */
  configUnset(key: unknown): string {
    const call = this.glue.configUnset;
    if (!call) return 'rustedscreep: this build has no configUnset export';
    if (typeof key !== 'string') return CONFIG_KEY_HELP;
    return this.guarded('configUnset', 'config change', () => call(key));
  }

  /** Console command: removes every config override. */
  configReset(): string {
    const call = this.glue.configReset;
    if (!call) return 'rustedscreep: this build has no configReset export';
    return this.guarded('configReset', 'config change', call);
  }

  /** Console command `rustedscreep.consts()`: the wasm's constants self-check (phase, counts, findings). */
  consts(): string {
    return this.summary('constsSummary', 'consts summary');
  }

  /** Console command `rustedscreep.movement()`: the wasm's movement summary (rooms, grids, fields, counters, order). */
  movement(): string {
    return this.summary('movementSummary', 'movement summary');
  }

  /**
   * Console command `rustedscreep.moveTo(room, x, y, range, from)`: own creeps in `from` (`room` when omitted) walk
   * to within `range` (1 when omitted) of (x, y) in `room`, across rooms if need be, until all are in range;
   * `rustedscreep.moveTo()` cancels. Only room name strings and whole numbers from 0 to 49 reach the wasm.
   */
  moveTo(room: unknown, x: unknown, y: unknown, range: unknown, from?: unknown): string {
    const call = this.glue.moveTo;
    if (!call) return 'rustedscreep: this build has no moveTo export';
    if (room === undefined && x === undefined && y === undefined && range === undefined && from === undefined) {
      return this.guarded('moveTo', 'move order', () => call());
    }
    const coord = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= 49;
    const fromOk = from === undefined || typeof from === 'string';
    if (typeof room !== 'string' || !coord(x) || !coord(y) || !(range === undefined || coord(range)) || !fromOk) {
      return 'rustedscreep: give a room name and x, y (and a range) from 0 to 49, and optionally the room the creeps start in, for example rustedscreep.moveTo("W1N1", 25, 25, 1) or rustedscreep.moveTo("W2N1", 25, 25, 1, "W1N1"); rustedscreep.moveTo() cancels';
    }
    return this.guarded('moveTo', 'move order', () => call(room, x, y, range, from));
  }

  /** Console command `rustedscreep.spawner()`: the wasm's spawn path summary (rooms, requests, orders, creeps). */
  spawner(): string {
    return this.summary('spawnerSummary', 'spawner summary');
  }

  /** Console command `rustedscreep.opening()`: the wasm's opening summary (workers, tasks, plan, watchdogs). */
  opening(): string {
    return this.summary('openingSummary', 'opening summary');
  }

  /** Console command `rustedscreep.planner()`: the wasm's planner summary (plans, placement, CPU per phase). */
  planner(): string {
    return this.summary('plannerSummary', 'planner summary');
  }

  /** Console command `rustedscreep.remote()`: the wasm's remote-mining summary (candidates, mined rooms, creeps). */
  remote(): string {
    return this.summary('remoteSummary', 'remote summary');
  }

  /** Console command `rustedscreep.sign()`: the wasm's controller-signer summary (record, signer, plan; ADR-030). */
  sign(): string {
    return this.summary('signSummary', 'sign summary');
  }

  /** Console command `rustedscreep.scout()`: the wasm's scouting summary (atlas, explorer, looks; ADR-039). */
  scout(): string {
    return this.summary('scoutSummary', 'scout summary');
  }

  /** Console command `rustedscreep.claims()`: the wasm's claim-scoring summary (ranking, plans, best; ADR-041). */
  claims(): string {
    return this.summary('claimsSummary', 'claims summary');
  }

  /** Console command `rustedscreep.minerals()`: the wasm's mineral-mining summary (rooms, plans, creeps; ADR-044). */
  minerals(): string {
    return this.summary('mineralsSummary', 'minerals summary');
  }

  /** Console command `rustedscreep.labs()`: the wasm's lab-reactions summary (targets, rooms, jobs; ADR-045). */
  labs(): string {
    return this.summary('labsSummary', 'labs summary');
  }

  /** Console command `rustedscreep.factory()`: the wasm's factory summary (settings, pace, rooms, jobs; ADR-046). */
  factory(): string {
    return this.summary('factorySummary', 'factory summary');
  }

  /** Console command `rustedscreep.terminals()`: the wasm's terminal-balancing summary (settings, terminals, sends; ADR-047). */
  terminals(): string {
    return this.summary('terminalsSummary', 'terminals summary');
  }

  /**
   * Console command `rustedscreep.spawn(room, role)`: a spawn of `room` spawns one creep of `role` (worker when
   * omitted) sized to the room's capacity, once the room holds its cost; `rustedscreep.spawn()` cancels. Only
   * strings reach the wasm, which checks the room name and the role.
   */
  spawn(room?: unknown, role?: unknown): string {
    const call = this.glue.spawn;
    if (!call) return 'rustedscreep: this build has no spawn export';
    if (room === undefined && role === undefined) return this.guarded('spawn', 'spawn request', () => call());
    if (typeof room !== 'string' || !(role === undefined || typeof role === 'string')) {
      return 'rustedscreep: give a room name and optionally a role, for example rustedscreep.spawn("W1N1") or rustedscreep.spawn("W1N1", "worker"); rustedscreep.spawn() cancels';
    }
    return this.guarded('spawn', 'spawn request', () => call(room, role));
  }

  /** Calls a wasm summary export for a console command (see `guarded`). */
  private summary(
    name:
      | 'snapshotSummary' | 'schedulerSummary' | 'actSummary' | 'profilerSummary' | 'persistSummary' | 'configSummary'
      | 'constsSummary' | 'movementSummary' | 'spawnerSummary' | 'openingSummary' | 'plannerSummary'
      | 'remoteSummary' | 'signSummary' | 'scoutSummary' | 'claimsSummary' | 'mineralsSummary'
      | 'labsSummary' | 'factorySummary' | 'terminalsSummary',
    noun: string,
  ): string {
    const call = this.glue[name];
    if (!call) return `rustedscreep: this build has no ${name} export`;
    return this.guarded(name, noun, call);
  }

  /**
   * Calls a wasm export for a console command, only while the instance is healthy. A throw poisons the
   * instance like a trap in loop, so the next tick recovers.
   */
  private guarded(name: string, noun: string, call: () => string): string {
    if (this.phase !== 'ready' || this.running) return `rustedscreep: no ${noun} while the loader is ${this.phase}`;
    this.running = true;
    try {
      const text = call();
      this.running = false;
      return text;
    } catch (e) {
      this.running = false;
      this.phase = 'poisoned';
      this.incident('trap', `${name}: ${message(e)}`);
      return `rustedscreep: ${name} trapped; the loader recovers on the next tick`;
    }
  }

  /** Console command: a one-line summary of the loader's state. */
  status(): string {
    const mem = this.env.memory();
    return `rustedscreep: build ${this.build.id} | phase ${this.phase} | recovery ${this.config.recoveryMode} | ` +
      `incidents ${mem.incidents.join(',') || 'none'} | pending traps ${mem.pendingTraps}`;
  }
}
