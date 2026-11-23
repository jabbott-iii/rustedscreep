'use strict';

// UTF-8 TextDecoder and TextEncoder for the wasm-bindgen glue, which constructs them at import time once
// strings cross the boundary (VERIFICATION_LOG V90). They are undefined on the MMO (V72). This follows the
// WHATWG Encoding Standard for UTF-8 only: decode with replacement (fatal mode is not supported), BOM
// handling per ignoreBOM, and encode with lone surrogates replaced by U+FFFD. main.ts imports this module
// before the glue, so Rollup evaluates it first.
const REPLACEMENT = 0xfffd;
function toUint8(input) {
    if (input instanceof Uint8Array)
        return input;
    if (ArrayBuffer.isView(input))
        return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    return new Uint8Array(input);
}
/** UTF-8 decoder of the WHATWG Encoding Standard (section 9.1.1), with replacement for errors. */
class Utf8Decoder {
    encoding = 'utf-8';
    fatal = false;
    ignoreBOM;
    constructor(label = 'utf-8', options = {}) {
        const l = String(label).trim().toLowerCase();
        if (l !== 'utf-8' && l !== 'utf8' && l !== 'unicode-1-1-utf-8')
            throw new RangeError(`unsupported encoding: ${label}`);
        if (options.fatal)
            throw new TypeError('fatal decoding is not supported by this polyfill');
        this.ignoreBOM = Boolean(options.ignoreBOM);
    }
    decode(input, options) {
        if (options && options.stream)
            throw new TypeError('streaming decode is not supported by this polyfill');
        if (input === undefined)
            return '';
        const bytes = toUint8(input);
        const out = [];
        let chunks = '';
        let codePoint = 0;
        let needed = 0;
        let seen = 0;
        let lower = 0x80;
        let upper = 0xbf;
        const emit = (cp) => {
            if (cp > 0xffff) {
                const v = cp - 0x10000;
                out.push(0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff));
            }
            else {
                out.push(cp);
            }
            if (out.length >= 8192) {
                chunks += String.fromCharCode(...out);
                out.length = 0;
            }
        };
        for (let i = 0; i < bytes.length; i++) {
            const b = bytes[i];
            if (needed === 0) {
                if (b <= 0x7f)
                    emit(b);
                else if (b >= 0xc2 && b <= 0xdf) {
                    needed = 1;
                    codePoint = b & 0x1f;
                }
                else if (b >= 0xe0 && b <= 0xef) {
                    if (b === 0xe0)
                        lower = 0xa0;
                    if (b === 0xed)
                        upper = 0x9f;
                    needed = 2;
                    codePoint = b & 0xf;
                }
                else if (b >= 0xf0 && b <= 0xf4) {
                    if (b === 0xf0)
                        lower = 0x90;
                    if (b === 0xf4)
                        upper = 0x8f;
                    needed = 3;
                    codePoint = b & 0x7;
                }
                else
                    emit(REPLACEMENT);
                continue;
            }
            if (b < lower || b > upper) {
                // Maximal subpart: drop the partial sequence, emit one replacement, and reprocess this byte.
                codePoint = 0;
                needed = 0;
                seen = 0;
                lower = 0x80;
                upper = 0xbf;
                emit(REPLACEMENT);
                i--;
                continue;
            }
            lower = 0x80;
            upper = 0xbf;
            codePoint = (codePoint << 6) | (b & 0x3f);
            seen++;
            if (seen === needed) {
                emit(codePoint);
                codePoint = 0;
                needed = 0;
                seen = 0;
            }
        }
        if (needed !== 0)
            emit(REPLACEMENT);
        let text = chunks + String.fromCharCode(...out);
        if (!this.ignoreBOM && text.charCodeAt(0) === 0xfeff)
            text = text.slice(1);
        return text;
    }
}
/** The code point at `i` (a surrogate pair, or U+FFFD for a lone surrogate) and the units it uses. */
function codePointAt(s, i) {
    const cu = s.charCodeAt(i);
    if (cu >= 0xd800 && cu <= 0xdbff) {
        const next = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
        if (next >= 0xdc00 && next <= 0xdfff)
            return [0x10000 + ((cu - 0xd800) << 10) + (next - 0xdc00), 2];
        return [REPLACEMENT, 1];
    }
    return [cu >= 0xdc00 && cu <= 0xdfff ? REPLACEMENT : cu, 1];
}
function utf8Length(cp) {
    return cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
}
function writeUtf8(cp, dest, at) {
    if (cp <= 0x7f)
        dest[at] = cp;
    else if (cp <= 0x7ff) {
        dest[at] = 0xc0 | (cp >> 6);
        dest[at + 1] = 0x80 | (cp & 0x3f);
    }
    else if (cp <= 0xffff) {
        dest[at] = 0xe0 | (cp >> 12);
        dest[at + 1] = 0x80 | ((cp >> 6) & 0x3f);
        dest[at + 2] = 0x80 | (cp & 0x3f);
    }
    else {
        dest[at] = 0xf0 | (cp >> 18);
        dest[at + 1] = 0x80 | ((cp >> 12) & 0x3f);
        dest[at + 2] = 0x80 | ((cp >> 6) & 0x3f);
        dest[at + 3] = 0x80 | (cp & 0x3f);
    }
}
/** UTF-8 encoder of the WHATWG Encoding Standard; lone surrogates become U+FFFD. */
class Utf8Encoder {
    encoding = 'utf-8';
    encode(input = '') {
        const s = String(input);
        const out = [];
        for (let i = 0; i < s.length;) {
            const [cp, units] = codePointAt(s, i);
            writeUtf8(cp, out, out.length);
            i += units;
        }
        return Uint8Array.from(out);
    }
    /** Writes whole code points while they fit; `read` counts UTF-16 units consumed (WHATWG encodeInto). */
    encodeInto(input, dest) {
        const s = String(input);
        let read = 0;
        let written = 0;
        while (read < s.length) {
            const [cp, units] = codePointAt(s, read);
            const len = utf8Length(cp);
            if (written + len > dest.length)
                break;
            writeUtf8(cp, dest, written);
            written += len;
            read += units;
        }
        return { read, written };
    }
}
/** Which of the two the runtime provided, and which this module installed. */
const textCodecSource = {
    TextDecoder: 'native',
    TextEncoder: 'native',
};
const g$1 = globalThis;
if (typeof g$1.TextDecoder === 'undefined') {
    g$1.TextDecoder = Utf8Decoder;
    textCodecSource.TextDecoder = 'polyfill';
}
if (typeof g$1.TextEncoder === 'undefined') {
    g$1.TextEncoder = Utf8Encoder;
    textCodecSource.TextEncoder = 'polyfill';
}

/* @ts-self-types="./rustedscreep_wasm.d.ts" */


function __wbg_reset_state () {
    __wbg_instance_id++;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    if (typeof numBytesDecoded !== 'undefined') numBytesDecoded = 0;
    if (typeof WASM_VECTOR_LEN !== 'undefined') WASM_VECTOR_LEN = 0;
    __wbg_reinit_scheduled = false;
    wasmInstance = new WebAssembly.Instance(wasmModule, __wbg_get_imports());
    wasm = wasmInstance.exports;
    wasm.__wbindgen_start();
}

/**
 * Exported to JS as `actSummary`, for the `rustedscreep.act()` console command: one line with the
 * last tick's intent counts (queued, refused, blocked, issued and their results) and totals.
 * @returns {string}
 */
function actSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.actSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `configReset`, for `rustedscreep.configReset()`: removes every override.
 * @returns {string}
 */
function configReset() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.configReset();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `configSet`, for `rustedscreep.configSet(key, value)`: overrides one field from
 * the next tick. A number outside the safe range is clamped; anything else invalid is refused.
 * @param {string} key
 * @param {string} value
 * @returns {string}
 */
function configSet(key, value) {
    let deferred3_0;
    let deferred3_1;
    try {
        let ptr0, len0, ptr1, len1;
        try {
            ptr0 = passStringToWasm0(key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len0 = WASM_VECTOR_LEN;
            ptr1 = passStringToWasm0(value, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len1 = WASM_VECTOR_LEN;
        } catch (e) {
            if (ptr1) { wasm.__wbindgen_free(ptr1, len1 * 1, 1); }
            if (ptr0) { wasm.__wbindgen_free(ptr0, len0 * 1, 1); }
            throw e;
        }
        let ret;
        __wbg_call_guard();
        ret = wasm.configSet(ptr0, len0, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Exported to JS as `configSummary`, for the `rustedscreep.config()` console command: one line with
 * the profile, the stored record, the overrides and every overridable key with its value in force.
 * @returns {string}
 */
function configSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.configSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `configUnset`, for `rustedscreep.configUnset(key)`: the profile's value again.
 * @param {string} key
 * @returns {string}
 */
function configUnset(key) {
    let deferred2_0;
    let deferred2_1;
    try {
        const ptr0 = passStringToWasm0(key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        let ret;
        __wbg_call_guard();
        ret = wasm.configUnset(ptr0, len0);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Exported to JS as `constsSummary`, for the `rustedscreep.consts()` console command: one line with
 * the constants self-check's phase and, once it is done, its counts, findings and known differences.
 * @returns {string}
 */
function constsSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.constsSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `loop`; the loader calls it once per tick: the snapshot, then the scheduler's
 * tick (its budget from the snapshot's tick context), in which the persistence loads run and jobs
 * queue intents (P2 onward; the console's `say` until then), then the act step, which issues them
 * in one batch, then the persistence writes and the active segments (`persist::Persist::flush`).
 * The tick runs on the config in force at its start (`cfg`, a copy): a record that loads during
 * the decide step takes effect on the next tick, except for the log level and the flush's limits.
 * A `profiler` build reads the CPU clock around each step (`profiler::Probe`); the flush falls in
 * its `end` span with the fault log. Other builds compile the probe out.
 *
 * A JS exception inside the pass ends the call without unwinding, which would leave the state
 * borrowed. The loader treats that as a trap and replaces the instance (ADR-003), so the borrow
 * failing here would mean a loader bug. Then this throws instead of idling silently: the loader
 * logs one incident and replaces the instance. Nothing is borrowed when it throws.
 */
function loop$1() {
    __wbg_call_guard();
    wasm.loop();
}

/**
 * Exported to JS as `moveTo`, for `rustedscreep.moveTo(room, x, y, range, from)`: from the next tick,
 * every own creep in `from` (`room` when omitted) without a request of its own walks to within
 * `range` (1 when omitted) of (x, y) in `room`, across rooms if need be (P2.2), until all of them
 * are in range on one tick. With no room it cancels the order.
 * @param {string | null} [room]
 * @param {number | null} [x]
 * @param {number | null} [y]
 * @param {number | null} [range]
 * @param {string | null} [from]
 * @returns {string}
 */
function moveTo(room, x, y, range, from) {
    let deferred6_0;
    let deferred6_1;
    try {
        const val3 = +(isLikeNone(x) ? Number.MAX_SAFE_INTEGER : (x) >>> 0);
        const val4 = +(isLikeNone(y) ? Number.MAX_SAFE_INTEGER : (y) >>> 0);
        const val5 = +(isLikeNone(range) ? Number.MAX_SAFE_INTEGER : (range) >>> 0);
        let ptr0, len0, ptr1, len1;
        try {
            ptr0 = isLikeNone(room) ? 0 : passStringToWasm0(room, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len0 = WASM_VECTOR_LEN;
            ptr1 = isLikeNone(from) ? 0 : passStringToWasm0(from, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len1 = WASM_VECTOR_LEN;
        } catch (e) {
            if (ptr1) { wasm.__wbindgen_free(ptr1, len1 * 1, 1); }
            if (ptr0) { wasm.__wbindgen_free(ptr0, len0 * 1, 1); }
            throw e;
        }
        let ret;
        __wbg_call_guard();
        ret = wasm.moveTo(ptr0, len0, val3, val4, val5, ptr1, len1);
        deferred6_0 = ret[0];
        deferred6_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred6_0, deferred6_1, 1);
    }
}

/**
 * Exported to JS as `movementSummary`, for the `rustedscreep.movement()` console command: one line
 * with the rooms, grid versions, cached fields, this tick's and the global's movement counters, and
 * the console's move order.
 * @returns {string}
 */
function movementSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.movementSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `openingSummary`, for the `rustedscreep.opening()` console command: one line
 * with each owned room's workers and tasks, its plan, its spawn request and its watchdog (P2.4).
 * @returns {string}
 */
function openingSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.openingSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `persistSummary`, for the `rustedscreep.persist()` console command: one line
 * with the active set, wanted loads, dirty records, the counts since boot and the self-test.
 * @returns {string}
 */
function persistSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.persistSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `persistTest`, for the `rustedscreep.persistTest(bytes)` console command:
 * writes a test frame of `bytes` payload bytes (the largest that fits a segment when omitted) on
 * the next tick and reads it back through the segment manager (the P1 gate's live round trip).
 * @param {number | null} [bytes]
 * @returns {string}
 */
function persistTest(bytes) {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.persistTest(isLikeNone(bytes) ? Number.MAX_SAFE_INTEGER : (bytes) >>> 0);
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `plannerSummary`, for the `rustedscreep.planner()` console command: one line
 * with each owned room's plan and its placement's last look, the totals since boot and the CPU
 * per planner phase (P4.3).
 * @returns {string}
 */
function plannerSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.plannerSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `profilerSummary`, for the `rustedscreep.profiler()` console command: one line
 * with the open window's CPU statistics, the act cost per call, the last loop, the last closed
 * window, the stats-segment writes and the cold loop. A build without the `profiler` feature says so.
 * @returns {string}
 */
function profilerSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.profilerSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `queueSay`, for the `rustedscreep.say(text)` console command: every own creep
 * says `text` on the next tick (a free intent, V34), which checks the act path in game.
 * @param {string} text
 * @returns {string}
 */
function queueSay(text) {
    let deferred2_0;
    let deferred2_1;
    try {
        const ptr0 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        let ret;
        __wbg_call_guard();
        ret = wasm.queueSay(ptr0, len0);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Exported to JS as `remoteSummary`, for the `rustedscreep.remote()` console command: one line with
 * each owned home's candidate rooms, what the bot knows of them, the rooms it mines with their plan
 * and figures, their creeps, and the sites placed since boot (P6.2).
 * @returns {string}
 */
function remoteSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.remoteSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `schedulerSummary`, for the `rustedscreep.scheduler()` console command: one line
 * with the last tick's tier caps, what the scheduler did, and the fault counts.
 * @returns {string}
 */
function schedulerSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.schedulerSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `signSummary`, for the `rustedscreep.sign()` console command: one line with
 * the signer's record, its trip, the next signer's plan and its counts (ADR-030).
 * @returns {string}
 */
function signSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.signSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `snapshotSummary`, for the `rustedscreep.snapshot()` console command: one line
 * with this tick's tick context, table sizes, handle churn and skipped objects.
 * @returns {string}
 */
function snapshotSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.snapshotSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Exported to JS as `spawn`, for `rustedscreep.spawn(room, role)`: from the next tick, a spawn of
 * `room` spawns one creep of `role` (worker when omitted) of the room's planned body, once the
 * room holds its cost (P2.3); for an owned room without a spawn, a spawn room within
 * `spawn.help_range` whose capacity fits the body, on a tick that room starts full while the
 * Standard tier runs (P5.1). With no room it cancels the request.
 * @param {string | null} [room]
 * @param {string | null} [role]
 * @returns {string}
 */
function spawn(room, role) {
    let deferred3_0;
    let deferred3_1;
    try {
        let ptr0, len0, ptr1, len1;
        try {
            ptr0 = isLikeNone(room) ? 0 : passStringToWasm0(room, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len0 = WASM_VECTOR_LEN;
            ptr1 = isLikeNone(role) ? 0 : passStringToWasm0(role, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len1 = WASM_VECTOR_LEN;
        } catch (e) {
            if (ptr1) { wasm.__wbindgen_free(ptr1, len1 * 1, 1); }
            if (ptr0) { wasm.__wbindgen_free(ptr0, len0 * 1, 1); }
            throw e;
        }
        let ret;
        __wbg_call_guard();
        ret = wasm.spawn(ptr0, len0, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Exported to JS as `spawnerSummary`, for the `rustedscreep.spawner()` console command: one line
 * with each spawn room's energy and spawns, the last tick's requests and orders, totals since boot,
 * the last spawn and failure, the console's request and the own creeps by home and role.
 * @returns {string}
 */
function spawnerSummary() {
    let deferred1_0;
    let deferred1_1;
    try {
        let ret;
        __wbg_call_guard();
        ret = wasm.spawnerSummary();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        __wbg_call_guard();
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Debug export for the loader's trap-recovery test (ADR-003; the P1 gate forces 10 traps in a row).
 * With `npm run build -- --features trap-test` it panics; otherwise it does nothing. It exists in every
 * build so the loader can import it statically, and the loader calls it only in a trap-test build.
 */
function trapTest() {
    __wbg_call_guard();
    wasm.trapTest();
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_is_function_741ff2bb612836f5: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_null_c22311b1cc228e3a: function(arg0) {
            const ret = arg0 === null;
            return ret;
        },
        __wbg___wbindgen_is_object_8b968c3ab8e96595: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_undefined_be5b34249ff1fc15: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_934b640d0889af78: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64((arg0 >>> 0) + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_7ce8eee7fd327545: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
            getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_4c88d8bf264fd984: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_09736879a3f67559: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_amount_e891929ae63d07d1: function(arg0) {
            const ret = arg0.amount;
            return ret;
        },
        __wbg_attack_771f6157371b81ef: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.attack(arg1);
            return ret;
        }, arguments); },
        __wbg_body_internal_f326517b7e8784f9: function(arg0) {
            const ret = arg0.body;
            return ret;
        },
        __wbg_boost_a9e9c6b41b38e758: function(arg0) {
            const ret = arg0.boost;
            return isLikeNone(ret) ? 109 : ((__wbindgen_enum_ResourceType.indexOf(ret) + 1 || 109) - 1);
        },
        __wbg_bucket_fad1c4c5f46e674f: function() {
            const ret = Game.cpu.bucket;
            return ret;
        },
        __wbg_build_5a86f33d001e3538: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.build(arg1);
            return ret;
        }, arguments); },
        __wbg_controller_fb0c168695740499: function(arg0) {
            const ret = arg0.controller;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_cooldown_8fda531aeb5efb9e: function(arg0) {
            const ret = arg0.cooldown;
            return ret;
        },
        __wbg_createConstructionSite_42a040083aebe316: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.createConstructionSite(arg1, arg2, __wbindgen_enum_StructureType[arg3]);
            return ret;
        }, arguments); },
        __wbg_create_8853e9d4d0f625e5: function(arg0) {
            const ret = Object.create(arg0);
            return ret;
        },
        __wbg_creeps_22d33f09316c0dbe: function() {
            const ret = Game.creeps;
            return ret;
        },
        __wbg_deleteProperty_3ad7a085da623822: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.deleteProperty(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_destroy_c79477a7058f2e5c: function() { return handleError(function (arg0) {
            const ret = arg0.destroy();
            return ret;
        }, arguments); },
        __wbg_drop_2117aa4480fb3f7d: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.drop(__wbindgen_enum_ResourceType[arg1], arg2 === Number.MAX_SAFE_INTEGER ? undefined : arg2);
            return ret;
        }, arguments); },
        __wbg_energy_6bb0be1610992c84: function(arg0) {
            const ret = arg0.energy;
            return ret;
        },
        __wbg_energy_available_7d2c7245f8b19c79: function(arg0) {
            const ret = arg0.energyAvailable;
            return ret;
        },
        __wbg_energy_capacity_2cb98ecd36af0e40: function(arg0) {
            const ret = arg0.energyCapacity;
            return ret;
        },
        __wbg_energy_capacity_available_c5825c98a78c1863: function(arg0) {
            const ret = arg0.energyCapacityAvailable;
            return ret;
        },
        __wbg_error_757e9472f8410341: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                __wbg_call_guard();
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_fatigue_internal_63028d146a4ca98f: function(arg0) {
            const ret = arg0.fatigue;
            return ret;
        },
        __wbg_find_6531a7369afdae09: function(arg0, arg1) {
            const ret = arg0.find(arg1);
            return ret;
        },
        __wbg_find_bb0244b6161efd15: function(arg0, arg1, arg2) {
            const ret = arg0.find(arg1, arg2);
            return ret;
        },
        __wbg_getActiveBodyparts_7b56cd5966808609: function(arg0, arg1) {
            const ret = Creep.prototype.getActiveBodyparts.call(arg0, __wbindgen_enum_Part[arg1]);
            return ret;
        },
        __wbg_getCapacity_58e91c34604e6503: function(arg0, arg1) {
            const ret = arg0.getCapacity(__wbindgen_enum_ResourceType[arg1]);
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_getFreeCapacity_a033aac3fd423cb8: function(arg0, arg1) {
            const ret = arg0.getFreeCapacity(__wbindgen_enum_ResourceType[arg1]);
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >> 0;
        },
        __wbg_getOwnPropertyDescriptor_3a5a5757a1ebc308: function() { return handleError(function (arg0, arg1) {
            const ret = Object.getOwnPropertyDescriptor(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_getOwnPropertyNames_17a0c593e960f6f3: function() { return handleError(function (arg0) {
            const ret = Object.getOwnPropertyNames(arg0);
            return ret;
        }, arguments); },
        __wbg_getRawBuffer_195f4148d30ef151: function() { return handleError(function (arg0) {
            const ret = arg0.getRawBuffer();
            return ret;
        }, arguments); },
        __wbg_getRawBuffer_fe49ada5926118ec: function() { return handleError(function (arg0) {
            const ret = arg0.getRawBuffer();
            return ret;
        }, arguments); },
        __wbg_getRoomStatus_85aa0bdab6980593: function() { return handleError(function (arg0) {
            const ret = Game.map.getRoomStatus(arg0);
            return ret;
        }, arguments); },
        __wbg_getRoomTerrain_88585d21dfb98eff: function() { return handleError(function (arg0) {
            const ret = Game.map.getRoomTerrain(arg0);
            return ret;
        }, arguments); },
        __wbg_getTerrain_6f901cc94969eb1f: function() { return handleError(function (arg0) {
            const ret = arg0.getTerrain();
            return ret;
        }, arguments); },
        __wbg_getUsedCapacity_8be193d2eb4b368d: function(arg0, arg1) {
            const ret = arg0.getUsedCapacity(__wbindgen_enum_ResourceType[arg1]);
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_getUsed_34824cf35c877f8d: function() {
            const ret = Game.cpu.getUsed();
            return ret;
        },
        __wbg_get_2080c49015e35254: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_d7176cfb76d71114: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_get_unchecked_e6325ad374eff861: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_harvest_17d3aea7d5ff6b27: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.harvest(arg1);
            return ret;
        }, arguments); },
        __wbg_heal_2f2e402e3281a087: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.heal(arg1);
            return ret;
        }, arguments); },
        __wbg_heal_d2bc1a58d75e2495: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.heal(arg1);
            return ret;
        }, arguments); },
        __wbg_hits_internal_1b3b7aa07ffad599: function(arg0) {
            const ret = arg0.hits;
            return ret;
        },
        __wbg_hits_internal_2e8a79b1c6a0434d: function(arg0) {
            const ret = arg0.hits;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_hits_max_internal_82534dd55aa15333: function(arg0) {
            const ret = arg0.hitsMax;
            return ret;
        },
        __wbg_hits_max_value_cac2891262ceda34: function(arg0) {
            const ret = arg0.hitsMax;
            return ret;
        },
        __wbg_id_internal_579e500196cab6ff: function(arg0) {
            const ret = arg0.id;
            return ret;
        },
        __wbg_id_internal_be4f8e4146a4fb5c: function(arg0) {
            const ret = arg0.id;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_id_value_79b68e0ce3be3c73: function(arg0) {
            const ret = arg0.id;
            return ret;
        },
        __wbg_incomplete_9f9b741506ab9af7: function(arg0) {
            const ret = arg0.incomplete;
            return ret;
        },
        __wbg_is_public_value_6e13bdae71ccf1e6: function(arg0) {
            const ret = arg0.isPublic;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_js_pos_41efd72dc63bca14: function(arg0) {
            const ret = arg0.pos;
            return ret;
        },
        __wbg_keys_ebebf2dd9d489a7e: function() { return handleError(function (arg0) {
            const ret = Object.keys(arg0);
            return ret;
        }, arguments); },
        __wbg_length_186163212ffe79c3: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_b03a0fabb109b1e9: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_level_0f781a43f083552f: function(arg0) {
            const ret = arg0.level;
            return ret;
        },
        __wbg_limit_1050789f9e9aa13c: function() {
            const ret = Game.cpu.limit;
            return ret;
        },
        __wbg_log_d37c9bc0bff17576: function(arg0, arg1) {
            console.log(getStringFromWasm0(arg0, arg1));
        },
        __wbg_max_ops_d19592e3ba612469: function(arg0, arg1) {
            arg0.maxOps = arg1 >>> 0;
        },
        __wbg_max_rooms_664b8b593d1663b5: function(arg0, arg1) {
            arg0.maxRooms = arg1;
        },
        __wbg_move_a08b29fe96a5d874: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.move(arg1);
            return ret;
        }, arguments); },
        __wbg_move_ce2edb7b0b7cbb12: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.move(arg1);
            return ret;
        }, arguments); },
        __wbg_my_internal_5b06f7a23c65ba5c: function(arg0) {
            const ret = arg0.my;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_my_value_0d570eed41450015: function(arg0) {
            const ret = arg0.my;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg_name_value_4bf59167cdbf348e: function(arg0) {
            const ret = arg0.name;
            return ret;
        },
        __wbg_new_37eb45aaf5eeb820: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_bb74001155cb8ca8: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_cd4348290afae586: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_from_slice_b8daf545a41c6aa0: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_notify_b0fdf1c28cd8b667: function() { return handleError(function (arg0, arg1) {
            Game.notify(getStringFromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_ops_ea9b5651a796f698: function(arg0) {
            const ret = arg0.ops;
            return ret;
        },
        __wbg_owner_value_5b5a740859070f68: function(arg0) {
            const ret = arg0.owner;
            return ret;
        },
        __wbg_owner_value_7d2f433d09468f52: function(arg0) {
            const ret = arg0.owner;
            return ret;
        },
        __wbg_packed_3bb90a95d7bf1c16: function(arg0) {
            const ret = arg0.__packedPos;
            return ret;
        },
        __wbg_packed_pos_272b8eb5eff3b13b: function(arg0) {
            const ret = arg0.__packedPos;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_part_4437cd65f09f44cb: function(arg0) {
            const ret = arg0.type;
            return (__wbindgen_enum_Part.indexOf(ret) + 1 || 9) - 1;
        },
        __wbg_path_6241a46dd6269a4c: function(arg0) {
            const ret = arg0.path;
            return ret;
        },
        __wbg_pickup_effef9519f3c3c7d: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.pickup(arg1);
            return ret;
        }, arguments); },
        __wbg_plain_cost_161242c6d041507b: function(arg0, arg1) {
            arg0.plainCost = arg1;
        },
        __wbg_progress_72f0c05204157c60: function(arg0) {
            const ret = arg0.progress;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_progress_internal_c817d53483c2fe6c: function(arg0) {
            const ret = arg0.progress;
            return ret;
        },
        __wbg_progress_total_internal_a458567645824308: function(arg0) {
            const ret = arg0.progressTotal;
            return ret;
        },
        __wbg_prototypesetcall_13162965d1fe7aa3: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_pull_d97b426d6b5ad5d7: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.pull(arg1);
            return ret;
        }, arguments); },
        __wbg_push_c9f4893fc209f768: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_rangedHeal_eb3a00962fb020cc: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.rangedHeal(arg1);
            return ret;
        }, arguments); },
        __wbg_repair_f7ba31b7ee0ca146: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.repair(arg1);
            return ret;
        }, arguments); },
        __wbg_reservation_value_b9e7081c5acd8433: function(arg0) {
            const ret = arg0.reservation;
            return ret;
        },
        __wbg_reserveController_4ac1df0d784de33e: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.reserveController(arg1);
            return ret;
        }, arguments); },
        __wbg_resource_type_396cb78ee223f30d: function(arg0) {
            const ret = arg0.resourceType;
            return (__wbindgen_enum_ResourceType.indexOf(ret) + 1 || 109) - 1;
        },
        __wbg_room_callback_9a0c29bd82ef4976: function(arg0, arg1) {
            arg0.roomCallback = arg1;
        },
        __wbg_rooms_fb611d081330a50f: function() {
            const ret = Game.rooms;
            return ret;
        },
        __wbg_say_576bba1bc1eefbc2: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.say(getStringFromWasm0(arg1, arg2), arg3 !== 0);
            return ret;
        }, arguments); },
        __wbg_search_866389dc642268b1: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = PathFinder.search(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_segments_5ca117c1aa86a9a6: function() { return handleError(function () {
            const ret = RawMemory.segments;
            return ret;
        }, arguments); },
        __wbg_setActiveSegments_79267fae4b5b6fe9: function() { return handleError(function (arg0) {
            RawMemory.setActiveSegments(arg0);
        }, arguments); },
        __wbg_set_2793479888329cb6: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_a2f2575eb59fc9af: function() { return handleError(function (arg0, arg1) {
            RawMemory.set(getStringFromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_set_bits_14e2245e9f55d663: function(arg0, arg1) {
            arg0._bits = arg1;
        },
        __wbg_set_packed_dc4761bfc068bd46: function(arg0, arg1) {
            arg0.__packedPos = arg1 >>> 0;
        },
        __wbg_set_pos_0242bafae46ade05: function(arg0, arg1) {
            arg0.pos = arg1;
        },
        __wbg_set_range_38aca89a65a1a1de: function(arg0, arg1) {
            arg0.range = arg1 >>> 0;
        },
        __wbg_signController_b0400be3679de84a: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.signController(arg1, getStringFromWasm0(arg2, arg3));
            return ret;
        }, arguments); },
        __wbg_sign_text_e827113780d545f4: function(arg0) {
            const ret = arg0.text;
            return ret;
        },
        __wbg_sign_username_95d455a1ee0c5c71: function(arg0) {
            const ret = arg0.username;
            return ret;
        },
        __wbg_sign_value_b38c2f6adc426c1b: function(arg0) {
            const ret = arg0.sign;
            return ret;
        },
        __wbg_spawnCreep_115fb91e4a4a16ad: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            const v0 = [];
            let p0 = arg1;
            p0 = p0 >>> 0;
            const view0 = getDataViewMemory0();
            for (const end0 = p0 + arg2 * 4; p0 < end0; p0 += 4) {
                v0.push(__wbindgen_enum_Part[view0.getInt32(p0 + 0, true)]);
            }
            wasm.__wbindgen_free(arg1, arg2 * 4, 4);
            const ret = arg0.spawnCreep(v0, getStringFromWasm0(arg3, arg4));
            return ret;
        }, arguments); },
        __wbg_spawning_e75e1cef272b29c0: function(arg0) {
            const ret = arg0.spawning;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_spawning_internal_070a7d93e78a1d5d: function(arg0) {
            const ret = arg0.spawning;
            return ret;
        },
        __wbg_stack_575c4e0918581b9c: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
            getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_COST_MATRIX_PROTOTYPE_37be23abc40663c8: function() {
            const ret = PathFinder.CostMatrix.prototype;
            return ret;
        },
        __wbg_static_accessor_GLOBAL_THIS_003ee833af9699fc: function() {
            const ret = typeof globalThis === 'undefined' ? undefined : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_2b9a0b79f3f7b8ee: function() {
            const ret = typeof globalThis === 'undefined' ? undefined : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_e4293fb22cbe77da: function() {
            const ret = typeof global === 'undefined' ? undefined : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_ROOM_POSITION_PROTOTYPE_a3e59f2133c00861: function() {
            const ret = RoomPosition.prototype;
            return ret;
        },
        __wbg_static_accessor_SELF_53bb20457401453d: function() {
            const ret = typeof self === 'undefined' ? undefined : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_b5e0810bfecb7feb: function() {
            const ret = typeof window === 'undefined' ? undefined : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_store_internal_b93d8e18a4a73d36: function(arg0) {
            const ret = arg0.store;
            return ret;
        },
        __wbg_store_of_788d6b4f5985190e: function(arg0) {
            const ret = arg0.store;
            return ret;
        },
        __wbg_stringify_0a0fbf2ef722380a: function() { return handleError(function (arg0) {
            const ret = JSON.stringify(arg0);
            return ret;
        }, arguments); },
        __wbg_structure_type_f8187f22f321ec0a: function(arg0) {
            const ret = arg0.structureType;
            return (__wbindgen_enum_StructureType.indexOf(ret) + 1 || 22) - 1;
        },
        __wbg_structure_type_internal_2fe3feaa2f58acae: function(arg0) {
            const ret = arg0.structureType;
            return (__wbindgen_enum_StructureType.indexOf(ret) + 1 || 22) - 1;
        },
        __wbg_structures_object_25bbcb126ca3b768: function() {
            const ret = Game.structures;
            return ret;
        },
        __wbg_suicide_575f66139131ee84: function() { return handleError(function (arg0) {
            const ret = arg0.suicide();
            return ret;
        }, arguments); },
        __wbg_swamp_cost_32780161b36344b0: function(arg0, arg1) {
            arg0.swampCost = arg1;
        },
        __wbg_tickLimit_6b84f325cef2976d: function() {
            const ret = Game.cpu.tickLimit;
            return ret;
        },
        __wbg_ticks_to_downgrade_b676132f27fbfc30: function(arg0) {
            const ret = arg0.ticksToDowngrade;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_ticks_to_end_value_b97f876577dff9d7: function(arg0) {
            const ret = arg0.ticksToEnd;
            return ret;
        },
        __wbg_ticks_to_live_internal_819d2d1b62b60d7d: function(arg0) {
            const ret = arg0.ticksToLive;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_time_a0ddfc3f3fde445c: function() {
            const ret = Game.time;
            return ret;
        },
        __wbg_transferEnergy_62cd7a46deefacfe: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.transferEnergy(arg1, arg2 >>> 0);
            return ret;
        }, arguments); },
        __wbg_transfer_f77d375a9acac89d: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.transfer(arg1, __wbindgen_enum_ResourceType[arg2], arg3 === Number.MAX_SAFE_INTEGER ? undefined : arg3);
            return ret;
        }, arguments); },
        __wbg_upgradeController_9442c6806e4ed067: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.upgradeController(arg1);
            return ret;
        }, arguments); },
        __wbg_upgrade_blocked_11da7f0f81e340d5: function(arg0) {
            const ret = arg0.upgradeBlocked;
            return isLikeNone(ret) ? Number.MAX_SAFE_INTEGER : (ret) >>> 0;
        },
        __wbg_username_value_2c7fa6f0f922f6d8: function(arg0) {
            const ret = arg0.username;
            return ret;
        },
        __wbg_username_value_e6765b3332f8437c: function(arg0) {
            const ret = arg0.username;
            return ret;
        },
        __wbg_values_01b79ec198898dfb: function(arg0) {
            const ret = Object.values(arg0);
            return ret;
        },
        __wbg_withdraw_d6e104fe798cc1cb: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.withdraw(arg1, __wbindgen_enum_ResourceType[arg2], arg3 === Number.MAX_SAFE_INTEGER ? undefined : arg3);
            return ret;
        }, arguments); },
        __wbindgen_generic_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("string")], shim_idx: 19, ret: Externref, inner_ret: Some(Externref) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_66fc74bf8055cade___convert__closures_____invoke___wasm_bindgen_66fc74bf8055cade___JsValue__wasm_bindgen_66fc74bf8055cade___JsValue__true_);
            return ret;
        },
        __wbindgen_generic_0000000000000002: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_generic_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            table.set(0, undefined);
            table.set(1024, undefined);
            table.set(1025, null);
            table.set(1026, true);
            table.set(1027, false);
        },
    };
    return {
        __proto__: null,
        "./rustedscreep_wasm_bg.js": import0,
    };
}

function __wbg_call_guard() {
    if (__wbg_reinit_scheduled) {
        __wbg_reset_state();
        return;
    }
}
function wasm_bindgen_66fc74bf8055cade___convert__closures_____invoke___wasm_bindgen_66fc74bf8055cade___JsValue__wasm_bindgen_66fc74bf8055cade___JsValue__true_(arg0, arg1, arg2) {
    let ret;
    __wbg_call_guard();
    ret = wasm.wasm_bindgen_66fc74bf8055cade___convert__closures_____invoke___wasm_bindgen_66fc74bf8055cade___JsValue__wasm_bindgen_66fc74bf8055cade___JsValue__true_(arg0, arg1, arg2);
    return ret;
}


const __wbindgen_enum_Part = ["move", "work", "carry", "attack", "ranged_attack", "tough", "heal", "claim"];


const __wbindgen_enum_ResourceType = ["energy", "power", "H", "O", "U", "L", "K", "Z", "X", "G", "silicon", "metal", "biomass", "mist", "OH", "ZK", "UL", "UH", "UO", "KH", "KO", "LH", "LO", "ZH", "ZO", "GH", "GO", "UH2O", "UHO2", "KH2O", "KHO2", "LH2O", "LHO2", "ZH2O", "ZHO2", "GH2O", "GHO2", "XUH2O", "XUHO2", "XKH2O", "XKHO2", "XLH2O", "XLHO2", "XZH2O", "XZHO2", "XGH2O", "XGHO2", "ops", "utrium_bar", "lemergium_bar", "zynthium_bar", "keanium_bar", "ghodium_melt", "oxidant", "reductant", "purifier", "battery", "composite", "crystal", "liquid", "wire", "switch", "transistor", "microchip", "circuit", "device", "cell", "phlegm", "tissue", "muscle", "organoid", "organism", "alloy", "tube", "fixtures", "frame", "hydraulics", "machine", "condensate", "concentrate", "extract", "spirit", "emanation", "essence", "score", "symbol_aleph", "symbol_beth", "symbol_gimmel", "symbol_daleth", "symbol_he", "symbol_waw", "symbol_zayin", "symbol_heth", "symbol_teth", "symbol_yodh", "symbol_kaph", "symbol_lamedh", "symbol_mem", "symbol_nun", "symbol_samekh", "symbol_ayin", "symbol_pe", "symbol_tsade", "symbol_qoph", "symbol_res", "symbol_sim", "symbol_taw", "T"];


const __wbindgen_enum_StructureType = ["spawn", "extension", "road", "constructedWall", "rampart", "keeperLair", "portal", "controller", "link", "storage", "tower", "observer", "powerBank", "powerSpawn", "extractor", "lab", "terminal", "container", "nuker", "factory", "invaderCore"];


let __wbg_instance_id = 0;
(typeof FinalizationRegistry === 'undefined')
    ? { }
    : new FinalizationRegistry(({ ptr, instance }) => {
    if (instance === __wbg_instance_id) wasm.__wbg_searchgoal_free(ptr, 1);
});

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => {
    if (state.instance === __wbg_instance_id) {
        wasm.__wbindgen_destroy_closure(state.a, state.b);
    }
});

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1, instance: __wbg_instance_id };
    const real = (...args) => {

        if (state.instance !== __wbg_instance_id) {
            throw new Error('Cannot invoke closure from previous WASM instance');
        }

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_destroy_closure(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

let __wbg_reinit_scheduled = false;

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(bytes) {
    numBytesDecoded += bytes.length;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true });
        cachedTextDecoder.decode();
        numBytesDecoded = bytes.length;
    }
    return cachedTextDecoder.decode(bytes);
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module);
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead');
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

// Base64 (RFC 4648 section 4, the standard alphabet, padded) to bytes, for the wasm text module the GitHub link
// delivers (ADR-015: the link uploads files as text, so the wasm travels as base64 in a .js module, V137; ADR-028
// splits it across several, which decodeBase64Into writes into one buffer). Plain JS:
// TextDecoder is undefined on the MMO (V72), and atob and Buffer were never probed there (V95). Strict: a length
// that is not a multiple of 4, any character outside the alphabet, or a misplaced '=' throws. Non-canonical
// trailing bits are not refused; they do not change the bytes, and the loader's CRC-32 covers the content.
const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const PAD = 61; // '='
const INVALID = 255;
let table$1;
/** The value of every ASCII code in ALPHABET, INVALID for the rest. */
function lookup() {
    if (table$1)
        return table$1;
    const t = new Uint8Array(128).fill(INVALID);
    for (let i = 0; i < 64; i++)
        t[ALPHABET.charCodeAt(i)] = i;
    table$1 = t;
    return t;
}
/** The '=' that end `text` (0, 1 or 2); throws a TypeError when its length is not a multiple of 4. */
function padding(text) {
    const n = text.length;
    if (n % 4 !== 0)
        throw new TypeError(`base64 text of ${n} characters is not a multiple of 4`);
    return n === 0 || text.charCodeAt(n - 1) !== PAD ? 0 : text.charCodeAt(n - 2) === PAD ? 2 : 1;
}
/** How many bytes padded standard base64 `text` decodes to; throws a TypeError when its length is not a multiple of 4. */
function decodedLength(text) {
    return (text.length / 4) * 3 - padding(text);
}
/** The bytes that padded standard base64 `text` encodes; throws a TypeError for anything else. */
function decodeBase64(text) {
    const out = new Uint8Array(decodedLength(text));
    decodeBase64Into(text, out, 0);
    return out;
}
/**
 * Decodes padded standard base64 `text` into `out` from `offset`, for a wasm split across text modules (ADR-028),
 * and returns the number of bytes written (`decodedLength(text)`). Throws a TypeError for text that is not base64
 * and a RangeError when `out` has no room for it.
 */
function decodeBase64Into(text, out, offset) {
    const n = text.length;
    const pad = padding(text);
    const length = (n / 4) * 3 - pad;
    if (offset < 0 || offset + length > out.length) {
        throw new RangeError(`no room for ${length} bytes at offset ${offset} of ${out.length}`);
    }
    const t = lookup();
    const full = pad ? n - 4 : n;
    let o = offset;
    for (let i = 0; i < full; i += 4) {
        const c0 = text.charCodeAt(i);
        const c1 = text.charCodeAt(i + 1);
        const c2 = text.charCodeAt(i + 2);
        const c3 = text.charCodeAt(i + 3);
        // Codes above 127 are outside the table; testing their OR first keeps every lookup in bounds.
        if ((c0 | c1 | c2 | c3) > 127)
            throw new TypeError(`not base64 at character ${i}`);
        const a = t[c0];
        const b = t[c1];
        const c = t[c2];
        const d = t[c3];
        if ((a | b | c | d) > 63)
            throw new TypeError(`not base64 at character ${i}`);
        out[o++] = (a << 2) | (b >> 4);
        out[o++] = ((b & 15) << 4) | (c >> 2);
        out[o++] = ((c & 3) << 6) | d;
    }
    if (pad) {
        // The last quantum: two characters and "==" (one byte), or three and "=" (two bytes).
        const i = n - 4;
        const c0 = text.charCodeAt(i);
        const c1 = text.charCodeAt(i + 1);
        const c2 = pad === 1 ? text.charCodeAt(i + 2) : 65; // 'A' = 0
        if ((c0 | c1 | c2) > 127)
            throw new TypeError(`not base64 at character ${i}`);
        const a = t[c0];
        const b = t[c1];
        const c = t[c2];
        if ((a | b | c) > 63)
            throw new TypeError(`not base64 at character ${i}`);
        out[o++] = (a << 2) | (b >> 4);
        if (pad === 1)
            out[o] = ((b & 15) << 4) | (c >> 2);
    }
    return length;
}

// CRC-32 (ISO-HDLC, the zlib/PNG CRC): the same algorithm as js/tools/lib/crc32.mjs, so the boot log
// can be compared with what npm run stage reports for the .wasm (VERIFICATION_LOG V77). A tool test checks
// that both give the same value on the same bytes.
let table;
function crcTable() {
    if (table)
        return table;
    const t = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
        let c = n;
        for (let k = 0; k < 8; k++)
            c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
        t[n] = c >>> 0;
    }
    table = t;
    return t;
}
/** CRC-32 of `bytes` as 8 lowercase hex digits. */
function crc32Hex(bytes) {
    const t = crcTable();
    let c = 0xffffffff;
    for (let i = 0; i < bytes.length; i++)
        c = t[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
    return ((c ^ 0xffffffff) >>> 0).toString(16).padStart(8, '0');
}

// The loader's boot, run and recovery state machine (ADR-003). Every Screeps global it needs comes in
// through `Env` and the wasm through `Glue`, so js/test/loader.test.mjs can drive it natively; main.ts
// wires it to the real runtime. No game logic lives here.
const WASM_MODULE = 'rustedscreep_wasm';
/** A module the user may add in the in-game editor to see whether a GitHub sync deletes it (V77). */
const SENTINEL_MODULE = 'sentinel';
const MAX_INCIDENTS_KEPT = 20;
const DEFERRED_LOG_EVERY = 100;
const ERROR_TEXT_MAX = 500;
const CONFIG_KEY_HELP = 'rustedscreep: give the config key as a string, for example rustedscreep.configSet("scheduler.tick_reserve_pct", 12); rustedscreep.config() lists the keys';
function isFiniteNumber(v) {
    return typeof v === 'number' && Number.isFinite(v);
}
/**
 * Repairs a loader record read from Memory, in place where it can (CLAUDE.md: missing or corrupt data
 * triggers a rebuild, never a crash). A malformed trap-test run is dropped together with its pending traps.
 */
function sanitizeLoaderMemory(raw, trapTestMax) {
    if (raw === null || typeof raw !== 'object' || Array.isArray(raw))
        return { incidents: [], streak: 0, pendingTraps: 0 };
    const rec = raw;
    const incidents = Array.isArray(rec.incidents) ? rec.incidents.filter(isFiniteNumber).slice(-MAX_INCIDENTS_KEPT) : [];
    const streak = isFiniteNumber(rec.streak) && rec.streak >= 0 && incidents.length > 0 ? Math.trunc(rec.streak) : 0;
    const run = rec.trapRun;
    let trapRun;
    if (run !== null && typeof run === 'object' && isFiniteNumber(run.total) && isFiniteNumber(run.startedAt) &&
        Array.isArray(run.recoveryTicks) && (run.awaitingSince === undefined || isFiniteNumber(run.awaitingSince))) {
        trapRun = { total: run.total, startedAt: run.startedAt, recoveryTicks: run.recoveryTicks.filter(isFiniteNumber) };
        if (run.awaitingSince !== undefined)
            trapRun.awaitingSince = run.awaitingSince;
    }
    const pending = trapRun && isFiniteNumber(rec.pendingTraps) ? Math.min(Math.max(0, Math.trunc(rec.pendingTraps)), trapTestMax) : 0;
    const out = rec;
    out.incidents = incidents;
    out.streak = streak;
    out.pendingTraps = pending;
    if (trapRun)
        out.trapRun = trapRun;
    else
        delete out.trapRun;
    return out;
}
function typeTag(value) {
    const tag = Object.prototype.toString.call(value).slice(8, -1);
    const ctor = value !== null && typeof value === 'object' ? value.constructor?.name : undefined;
    return ctor && ctor !== tag ? `${tag}(${ctor})` : tag;
}
/**
 * The wasm's bytes, whatever require() returned: a binary module's view or ArrayBuffer (an API upload, the private
 * server, the sandbox; V31: never trust instanceof), or the base64 string the text module exports (the GitHub link,
 * ADR-015).
 */
function toBytes(value) {
    if (typeof value === 'string') {
        try {
            return decodeBase64(value);
        }
        catch (e) {
            throw new TypeError(`require('${WASM_MODULE}') returned a String that is not base64 (${message(e)})`);
        }
    }
    if (ArrayBuffer.isView(value))
        return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (Object.prototype.toString.call(value) === '[object ArrayBuffer]')
        return new Uint8Array(value);
    throw new TypeError(`require('${WASM_MODULE}') returned ${typeTag(value)}, not bytes`);
}
/** The module that holds part `k` of the wasm's base64 text (ADR-028): `rustedscreep_wasm`, then `rustedscreep_wasm_1`, … */
function partModuleName(k) {
    return k === 0 ? WASM_MODULE : `${WASM_MODULE}_${k}`;
}
/**
 * The wasm's bytes, and the form that ran for the boot line. A binary module under `rustedscreep_wasm` (an API
 * upload, the private server, the sandbox) is used alone, as `toBytes` reads it. A string there is part 0 of the
 * text form the GitHub link delivers (ADR-015, ADR-028): every part `wasmParts` lists must be a string of exactly
 * its recorded length, only the last may end in '=', and the parts are decoded into one buffer, so a missing,
 * empty, cut or misplaced part throws a TypeError that names it before anything is decoded or compiled (an empty
 * part 0 with toBytes's text, `returned Object, not bytes`, since a non-string part 0 is read as a binary module).
 * A part of the recorded length with other content decodes, and the CRC-32 check reports it (ADR-015).
 */
function loadWasm(requireModule, wasmParts) {
    // A part that is missing, or cut short (a SyntaxError that does not name its module), is named, part 0 too.
    let first;
    try {
        first = requireModule(WASM_MODULE);
    }
    catch (e) {
        throw new TypeError(`require('${WASM_MODULE}') threw ${message(e)}`);
    }
    // A non-string part 0 is read as a binary module; an empty text module ({}) fails here, as on 5217308 (V257).
    if (typeof first !== 'string')
        return { bytes: toBytes(first), tag: typeTag(first) };
    const count = Array.isArray(wasmParts) ? wasmParts.length : 0;
    if (count === 0)
        throw new TypeError(`require('${WASM_MODULE}') returned a String, but this build records no text parts`);
    const texts = [];
    let chars = 0;
    for (let k = 0; k < count; k++) {
        const name = partModuleName(k);
        const want = wasmParts[k];
        let value = first;
        if (k > 0) {
            try {
                value = requireModule(name);
            }
            catch (e) {
                throw new TypeError(`require('${name}') threw ${message(e)}`);
            }
        }
        if (typeof value !== 'string')
            throw new TypeError(`require('${name}') returned ${typeTag(value)}, not a ${want}-character string`);
        if (value.length !== want)
            throw new TypeError(`require('${name}') returned ${value.length} characters, not ${want}`);
        if (k < count - 1 && value.charCodeAt(want - 1) === 61)
            throw new TypeError(`require('${name}') ends in '=' but is not the last part`);
        texts.push(value);
        chars += want;
    }
    const notBase64 = (k, e) => new TypeError(`require('${partModuleName(k)}') returned a String that is not base64 (${message(e)})`);
    let total = 0;
    for (let k = 0; k < count; k++) {
        try {
            total += decodedLength(texts[k]);
        }
        catch (e) {
            throw notBase64(k, e);
        }
    }
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (let k = 0; k < count; k++) {
        try {
            offset += decodeBase64Into(texts[k], bytes, offset);
        }
        catch (e) {
            throw notBase64(k, e);
        }
    }
    return { bytes, tag: `String(base64, ${chars} chars${count > 1 ? ` in ${count} parts` : ''})` };
}
function message(e) {
    return e instanceof Error ? `${e.name}: ${e.message}` : String(e);
}
class Loader {
    env;
    glue;
    config;
    build;
    phase = 'idle';
    /** Set while the wasm runs; still set at the start of a tick means that tick was aborted. */
    running = false;
    module;
    lastError = '';
    lastDeferredLog = -Infinity;
    reinstantiations = [];
    /** Whether the incident that poisoned the instance was a trap-test trap (exempt from the caps). */
    poisonedByTest = false;
    /** A CRC-32 mismatch is notified once per global; repeated boot attempts only log it. */
    mismatchNotified = false;
    stoppedLogged = false;
    /** Set while a capped or failed re-instantiation waits to be retried (logged once per episode). */
    retryWaitLogged = false;
    bootCpu;
    bootStartedAt;
    constructor(env, glue, config, build) {
        this.env = env;
        this.glue = glue;
        this.config = config;
        this.build = build;
    }
    /** The text of the last console.error, attached to the next incident (the panic hook writes there). */
    noteError(text) {
        this.lastError = text.length > ERROR_TEXT_MAX ? `${text.slice(0, ERROR_TEXT_MAX)}...` : text;
    }
    tick() {
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
            if (!this.boot())
                return;
            if (this.config.bootSplitTicks)
                return;
        }
        if (this.phase === 'compiled') {
            if (!this.instantiate())
                return;
        }
        if (this.phase === 'ready')
            this.run();
    }
    deferred(text) {
        const t = this.env.time();
        if (t - this.lastDeferredLog >= DEFERRED_LOG_EVERY) {
            this.lastDeferredLog = t;
            this.env.log(`rustedscreep boot deferred: ${text}`);
        }
    }
    /** The boot delay after `streak` consecutive incidents: 0 below backoffIncidents, then doubling to the cap. */
    delayFor(streak) {
        if (streak < this.config.backoffIncidents)
            return 0;
        return Math.min(this.config.backoffMaxTicks, this.config.backoffBaseTicks * 2 ** (streak - this.config.backoffIncidents));
    }
    /** The tick before which the next boot waits (0 = boot now). An incident dated in the future is ignored. */
    backoffUntil(mem, now) {
        const last = mem.incidents.at(-1);
        if (last === undefined || last > now)
            return 0;
        const delay = this.delayFor(mem.streak);
        return delay > 0 ? last + delay : 0;
    }
    boot() {
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
        let bytes;
        let tag;
        try {
            // The boot line shows which delivery ran: a binary module's type, or the text modules' base64 (ADR-015, ADR-028).
            ({ bytes, tag } = loadWasm((name) => this.env.requireModule(name), this.build.wasmParts));
        }
        catch (e) {
            this.incident('load', message(e));
            return false;
        }
        const crc = crc32Hex(bytes);
        let sentinel = 'absent';
        try {
            this.env.requireModule(SENTINEL_MODULE);
            sentinel = 'present';
        }
        catch {
            // A missing module throws; absent is the normal case.
        }
        const c1 = this.env.cpuUsed();
        this.env.log(`rustedscreep boot: build ${this.build.id} | Game.time ${now} | wasm ${tag} ${bytes.length} B crc32 ${crc} ` +
            `(stage ${this.build.wasmCrc32}: ${crc === this.build.wasmCrc32 ? 'match' : 'MISMATCH'}) | sentinel ${sentinel} | ` +
            `${this.env.globalsReport()}`);
        if (crc !== this.build.wasmCrc32 && !this.mismatchNotified) {
            this.mismatchNotified = true;
            this.env.notify(`rustedscreep: wasm crc32 ${crc} does not match the build's ${this.build.wasmCrc32} (build ${this.build.id})`, this.config.notifyGroupMinutes);
        }
        try {
            this.module = this.env.compile(bytes);
        }
        catch (e) {
            this.incident('compile', message(e));
            return false;
        }
        const c2 = this.env.cpuUsed();
        this.bootCpu = { load: c1 - c0, compile: c2 - c1, instantiate: 0 };
        this.phase = 'compiled';
        return true;
    }
    instantiate() {
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
        }
        catch (e) {
            // A failed start function can leave the glue half-initialized, so treat it like a trap.
            this.running = false;
            this.phase = 'poisoned';
            this.incident('instantiate', message(e));
            return false;
        }
        if (this.bootCpu)
            this.bootCpu.instantiate = this.env.cpuUsed() - c0;
        this.phase = 'ready';
        return true;
    }
    run() {
        const firstAfterBoot = this.bootCpu !== undefined;
        const c0 = firstAfterBoot ? this.env.cpuUsed() : 0;
        // Memory is read only on the first loop after a boot (to see a pending recovery) and in trap-test
        // builds; a normal tick never touches it.
        const record = firstAfterBoot || this.glue.trapTest ? this.env.memory() : undefined;
        // A trap counts as recovered once a fresh instance is about to run, even if that run is the next trap.
        if (record?.trapRun?.awaitingSince !== undefined)
            this.recovered(record);
        const trapNow = record !== undefined && record.pendingTraps > 0 && this.glue.trapTest !== undefined;
        this.running = true;
        try {
            if (trapNow && record && this.glue.trapTest) {
                record.pendingTraps -= 1;
                if (record.trapRun)
                    record.trapRun.awaitingSince = this.env.time();
                this.glue.trapTest();
            }
            else {
                this.glue.loop();
            }
            this.running = false;
        }
        catch (e) {
            this.running = false;
            this.phase = 'poisoned';
            this.poisonedByTest = trapNow;
            this.incident(trapNow ? 'trap-test' : 'trap', message(e));
            return;
        }
        if (firstAfterBoot && this.bootCpu) {
            const loopCpu = this.env.cpuUsed() - c0;
            const b = this.bootCpu;
            this.env.log(`rustedscreep booted in ${this.env.time() - (this.bootStartedAt ?? this.env.time())} ticks: ` +
                `load+crc ${b.load.toFixed(2)} CPU, compile ${b.compile.toFixed(2)}, instantiate ${b.instantiate.toFixed(2)}, ` +
                `first loop ${loopCpu.toFixed(2)}`);
            this.bootCpu = undefined;
        }
        if (record)
            this.finishTrapRun(record);
    }
    recovered(record) {
        const run = record.trapRun;
        if (!run || run.awaitingSince === undefined)
            return;
        run.recoveryTicks.push(this.env.time() - run.awaitingSince);
        run.awaitingSince = undefined;
        this.env.log(`rustedscreep trap-test: recovery ${run.recoveryTicks.length}/${run.total} after ${run.recoveryTicks.at(-1)} ticks`);
    }
    /** After a successful loop: if every scheduled trap has recovered, report the run and clear it. */
    finishTrapRun(record) {
        const run = record.trapRun;
        if (run && record.pendingTraps === 0 && run.recoveryTicks.length >= run.total) {
            const summary = `rustedscreep trap-test done: ${run.total} traps, each recovered (ticks: ${run.recoveryTicks.join(', ')})`;
            this.env.log(summary);
            this.env.notify(summary, this.config.notifyGroupMinutes);
            record.trapRun = undefined;
        }
    }
    incident(kind, detail) {
        const now = this.env.time();
        const mem = this.env.memory();
        const text = `rustedscreep incident: ${kind} at tick ${now} (build ${this.build.id}): ${detail}` +
            (this.lastError ? ` | console.error: ${this.lastError}` : '');
        this.lastError = '';
        this.env.log(text);
        if (kind === 'trap-test')
            return;
        const prev = mem.incidents.at(-1);
        const continues = prev !== undefined && now >= prev && now - prev < this.config.backoffWindowTicks + this.delayFor(mem.streak);
        mem.streak = continues ? mem.streak + 1 : 1;
        mem.incidents.push(now);
        if (mem.incidents.length > MAX_INCIDENTS_KEPT)
            mem.incidents.splice(0, mem.incidents.length - MAX_INCIDENTS_KEPT);
        this.env.notify(text, this.config.notifyGroupMinutes);
    }
    recover() {
        // Nothing is left to try once stopped; the next global reset (a code push or a VM restart) boots again.
        if (this.stoppedLogged)
            return;
        const now = this.env.time();
        let result;
        if (this.config.recoveryMode === 'halt') {
            if (!this.retryWaitLogged)
                this.env.log('rustedscreep recovering: Game.cpu.halt() resets the global; the next tick boots again (ADR-003 a)');
            if (this.env.halt())
                return;
            // No halt here: re-instantiate instead, so the bot still recovers without human action.
            result = this.reinstantiate(now, 'Game.cpu.halt is not available');
        }
        else {
            result = this.reinstantiate(now, 'recovery mode reinstantiate');
            if (result === 'done')
                return;
            this.env.log('rustedscreep recovering: Game.cpu.halt() resets the global; the next tick boots again (ADR-003 a)');
            if (this.env.halt())
                return;
        }
        if (result === 'done')
            return;
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
    reinstantiate(now, why) {
        if (!this.glue.reset)
            return 'unavailable';
        this.reinstantiations = this.reinstantiations.filter((t) => now - t < this.config.backoffWindowTicks);
        // Trap-test recoveries do not count toward the cap, so a 10-trap run measures option (b) every time.
        const byTest = this.poisonedByTest;
        this.poisonedByTest = false;
        if (!byTest && this.reinstantiations.length >= this.config.backoffIncidents)
            return 'capped';
        if (!byTest)
            this.reinstantiations.push(now);
        try {
            this.running = true;
            this.glue.reset();
            this.running = false;
            this.phase = 'ready';
            this.retryWaitLogged = false;
            this.env.log(`rustedscreep recovered by re-instantiating from the cached module (ADR-003 b; ${why})`);
            return 'done';
        }
        catch (e) {
            this.running = false;
            this.env.log(`rustedscreep re-instantiation failed (${message(e)})`);
            return 'failed';
        }
    }
    stopped(why) {
        if (this.stoppedLogged)
            return;
        this.stoppedLogged = true;
        const text = `rustedscreep stopped: ${why}; the wasm stays stopped until the next global reset`;
        this.env.log(text);
        this.env.notify(text, this.config.notifyGroupMinutes);
    }
    /** Console command: schedule `n` traps, one per boot, to test recovery (P1 gate: 10 in a row). */
    scheduleTraps(n) {
        if (!this.glue.trapTest)
            return 'rustedscreep: this build has no trapTest export; build with npm run build -- --features trap-test';
        if (!Number.isInteger(n) || n < 1 || n > this.config.trapTestMax)
            return `rustedscreep: give a whole number from 1 to ${this.config.trapTestMax}`;
        const mem = this.env.memory();
        mem.pendingTraps = n;
        mem.trapRun = { total: n, startedAt: this.env.time(), recoveryTicks: [] };
        return `rustedscreep: ${n} trap(s) scheduled; each fires in the next loop after a boot`;
    }
    /** Console command: the wasm's summary of this tick's snapshot. */
    snapshot() {
        return this.summary('snapshotSummary', 'snapshot');
    }
    /** Console command: the wasm's summary of the scheduler's last tick. */
    scheduler() {
        return this.summary('schedulerSummary', 'scheduler summary');
    }
    /** Console command: the wasm's summary of the act layer's last tick. */
    act() {
        return this.summary('actSummary', 'act summary');
    }
    /** Console command: the wasm's profiler statistics (open window, act cost per call, segment writes). */
    profiler() {
        return this.summary('profilerSummary', 'profiler summary');
    }
    /** Console command: every own creep says `text` on the next tick (a free intent; checks the act path). */
    say(text) {
        const call = this.glue.queueSay;
        if (!call)
            return 'rustedscreep: this build has no queueSay export';
        if (typeof text !== 'string')
            return 'rustedscreep: give the text as a string, for example rustedscreep.say("hi")';
        return this.guarded('queueSay', 'say', () => call(text));
    }
    /** Console command: the wasm's segment-manager summary (active set, loads, writes, the self-test). */
    persist() {
        return this.summary('persistSummary', 'persist summary');
    }
    /**
     * Console command: a storage round trip through the segment manager (the P1 gate's live check). `bytes`
     * is the payload size, or undefined for the largest that fits one segment; the wasm clamps larger sizes.
     */
    persistTest(bytes) {
        const call = this.glue.persistTest;
        if (!call)
            return 'rustedscreep: this build has no persistTest export';
        if (bytes !== undefined && !(typeof bytes === 'number' && Number.isInteger(bytes) && bytes >= 0 && bytes <= 0xffffffff)) {
            return 'rustedscreep: give the payload size as a whole number of bytes, or nothing for a full segment, for example rustedscreep.persistTest(1000)';
        }
        return this.guarded('persistTest', 'persist test', () => call(bytes));
    }
    /** Console command `rustedscreep.config()`: the wasm's runtime config (profile, stored record, overrides, values in force). */
    runtimeConfig() {
        return this.summary('configSummary', 'config summary');
    }
    /**
     * Console command: overrides config field `key` from the next tick. `value` is a number, true or false, or a
     * level name; the wasm clamps a number to the field's safe range and refuses what it cannot use.
     */
    configSet(key, value) {
        const call = this.glue.configSet;
        if (!call)
            return 'rustedscreep: this build has no configSet export';
        if (typeof key !== 'string')
            return CONFIG_KEY_HELP;
        const ok = (typeof value === 'number' && Number.isFinite(value)) || typeof value === 'boolean' || typeof value === 'string';
        if (!ok) {
            return 'rustedscreep: give the value as a number, true or false, or a level name, for example rustedscreep.configSet("log.level", "debug")';
        }
        const text = String(value);
        return this.guarded('configSet', 'config change', () => call(key, text));
    }
    /** Console command: removes the override of config field `key`, so the profile's value applies again. */
    configUnset(key) {
        const call = this.glue.configUnset;
        if (!call)
            return 'rustedscreep: this build has no configUnset export';
        if (typeof key !== 'string')
            return CONFIG_KEY_HELP;
        return this.guarded('configUnset', 'config change', () => call(key));
    }
    /** Console command: removes every config override. */
    configReset() {
        const call = this.glue.configReset;
        if (!call)
            return 'rustedscreep: this build has no configReset export';
        return this.guarded('configReset', 'config change', call);
    }
    /** Console command `rustedscreep.consts()`: the wasm's constants self-check (phase, counts, findings). */
    consts() {
        return this.summary('constsSummary', 'consts summary');
    }
    /** Console command `rustedscreep.movement()`: the wasm's movement summary (rooms, grids, fields, counters, order). */
    movement() {
        return this.summary('movementSummary', 'movement summary');
    }
    /**
     * Console command `rustedscreep.moveTo(room, x, y, range, from)`: own creeps in `from` (`room` when omitted) walk
     * to within `range` (1 when omitted) of (x, y) in `room`, across rooms if need be, until all are in range;
     * `rustedscreep.moveTo()` cancels. Only room name strings and whole numbers from 0 to 49 reach the wasm.
     */
    moveTo(room, x, y, range, from) {
        const call = this.glue.moveTo;
        if (!call)
            return 'rustedscreep: this build has no moveTo export';
        if (room === undefined && x === undefined && y === undefined && range === undefined && from === undefined) {
            return this.guarded('moveTo', 'move order', () => call());
        }
        const coord = (v) => typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= 49;
        const fromOk = from === undefined || typeof from === 'string';
        if (typeof room !== 'string' || !coord(x) || !coord(y) || !(range === undefined || coord(range)) || !fromOk) {
            return 'rustedscreep: give a room name and x, y (and a range) from 0 to 49, and optionally the room the creeps start in, for example rustedscreep.moveTo("W1N1", 25, 25, 1) or rustedscreep.moveTo("W2N1", 25, 25, 1, "W1N1"); rustedscreep.moveTo() cancels';
        }
        return this.guarded('moveTo', 'move order', () => call(room, x, y, range, from));
    }
    /** Console command `rustedscreep.spawner()`: the wasm's spawn path summary (rooms, requests, orders, creeps). */
    spawner() {
        return this.summary('spawnerSummary', 'spawner summary');
    }
    /** Console command `rustedscreep.opening()`: the wasm's opening summary (workers, tasks, plan, watchdogs). */
    opening() {
        return this.summary('openingSummary', 'opening summary');
    }
    /** Console command `rustedscreep.planner()`: the wasm's planner summary (plans, placement, CPU per phase). */
    planner() {
        return this.summary('plannerSummary', 'planner summary');
    }
    /** Console command `rustedscreep.remote()`: the wasm's remote-mining summary (candidates, mined rooms, creeps). */
    remote() {
        return this.summary('remoteSummary', 'remote summary');
    }
    /** Console command `rustedscreep.sign()`: the wasm's controller-signer summary (record, signer, plan; ADR-030). */
    sign() {
        return this.summary('signSummary', 'sign summary');
    }
    /**
     * Console command `rustedscreep.spawn(room, role)`: a spawn of `room` spawns one creep of `role` (worker when
     * omitted) sized to the room's capacity, once the room holds its cost; `rustedscreep.spawn()` cancels. Only
     * strings reach the wasm, which checks the room name and the role.
     */
    spawn(room, role) {
        const call = this.glue.spawn;
        if (!call)
            return 'rustedscreep: this build has no spawn export';
        if (room === undefined && role === undefined)
            return this.guarded('spawn', 'spawn request', () => call());
        if (typeof room !== 'string' || !(role === undefined || typeof role === 'string')) {
            return 'rustedscreep: give a room name and optionally a role, for example rustedscreep.spawn("W1N1") or rustedscreep.spawn("W1N1", "worker"); rustedscreep.spawn() cancels';
        }
        return this.guarded('spawn', 'spawn request', () => call(room, role));
    }
    /** Calls a wasm summary export for a console command (see `guarded`). */
    summary(name, noun) {
        const call = this.glue[name];
        if (!call)
            return `rustedscreep: this build has no ${name} export`;
        return this.guarded(name, noun, call);
    }
    /**
     * Calls a wasm export for a console command, only while the instance is healthy. A throw poisons the
     * instance like a trap in loop, so the next tick recovers.
     */
    guarded(name, noun, call) {
        if (this.phase !== 'ready' || this.running)
            return `rustedscreep: no ${noun} while the loader is ${this.phase}`;
        this.running = true;
        try {
            const text = call();
            this.running = false;
            return text;
        }
        catch (e) {
            this.running = false;
            this.phase = 'poisoned';
            this.incident('trap', `${name}: ${message(e)}`);
            return `rustedscreep: ${name} trapped; the loader recovers on the next tick`;
        }
    }
    /** Console command: a one-line summary of the loader's state. */
    status() {
        const mem = this.env.memory();
        return `rustedscreep: build ${this.build.id} | phase ${this.phase} | recovery ${this.config.recoveryMode} | ` +
            `incidents ${mem.incidents.join(',') || 'none'} | pending traps ${mem.pendingTraps}`;
    }
}

// The loader (P1 module 4): boots the wasm when the bucket allows, calls its loop every tick, and
// recovers from traps and aborted ticks (ADR-003). The state machine is in loader.ts; this file wires it
// to the Screeps runtime. No game logic. The polyfill import must stay first: once strings cross the
// boundary, the glue constructs TextDecoder and TextEncoder while it loads (V90).
const BUILD = ({"id":"42a94e0b1805-dirty-8883819a","wasmCrc32":"8883819a","features":[],"wasmParts":[823412,823408]});
const CONFIG = ({"bootBucketMin":480,"bootSplitTicks":true,"recoveryMode":"halt","backoffWindowTicks":1000,"backoffIncidents":3,"backoffBaseTicks":100,"backoffMaxTicks":3200,"notifyGroupMinutes":60,"trapTestMax":20});
/** The open-source driver keeps the first 500 characters of a notification; the docs say 1000 (V96). */
const NOTIFY_MAX = 500;
const MEMORY_KEY = 'rustedscreepLoader';
const g = globalThis;
/** Used when Memory itself is unusable, so the loader keeps working for this global (V96). */
let heapRecord;
let memoryUnusableNotified = false;
/**
 * The loader's record in Memory, repaired if malformed. Screeps sets Memory to null when its raw string is
 * not valid JSON (V96); then the loader keeps its record in the heap and avoids halting, which would lose it.
 * Once Memory is an object again (the bot resets one that did not parse, ADR-019), the heap record moves back
 * into it, so this global's incidents carry on and halting is allowed again.
 */
function loaderMemory() {
    const mem = Memory;
    if (mem === null || typeof mem !== 'object' || Array.isArray(mem)) {
        heapRecord ??= sanitizeLoaderMemory(undefined, CONFIG.trapTestMax);
        if (!memoryUnusableNotified) {
            memoryUnusableNotified = true;
            const text = 'rustedscreep: Memory is unusable (its raw string is not a JSON object); loader state stays in the heap';
            console.log(text);
            Game.notify(text, CONFIG.notifyGroupMinutes);
        }
        return heapRecord;
    }
    const record = mem;
    if (heapRecord !== undefined) {
        record[MEMORY_KEY] = heapRecord;
        heapRecord = undefined;
        memoryUnusableNotified = false;
    }
    const repaired = sanitizeLoaderMemory(record[MEMORY_KEY], CONFIG.trapTestMax);
    record[MEMORY_KEY] = repaired;
    return repaired;
}
/** typeof console.error and console.warn before the loader adds its own, for the boot log (V95). */
let nativeConsole;
const env = {
    time: () => Game.time,
    bucket: () => Game.cpu.bucket,
    cpuUsed: () => Game.cpu.getUsed(),
    halt: () => {
        // With Memory unusable, a halt would also lose the loader's heap record, so recover in place instead.
        if (typeof Game.cpu.halt !== 'function' || heapRecord !== undefined)
            return false;
        Game.cpu.halt(); // ends the tick on Screeps (V96)
        return true;
    },
    notify: (message, groupMinutes) => Game.notify(message.slice(0, NOTIFY_MAX), groupMinutes),
    log: (message) => console.log(message),
    requireModule: (name) => require(name),
    compile: (bytes) => new WebAssembly.Module(bytes),
    memory: loaderMemory,
    globalsReport: () => {
        const kind = (name) => typeof g[name];
        return `TextDecoder ${textCodecSource.TextDecoder}, TextEncoder ${textCodecSource.TextEncoder}, ` +
            `fetch ${kind('fetch')}, document ${kind('document')}, URL ${kind('URL')}, ` +
            `console.error ${nativeConsole?.error ?? '?'}, console.warn ${nativeConsole?.warn ?? '?'}, ` +
            `atob ${kind('atob')}, Buffer ${kind('Buffer')}`;
    },
};
const glue = {
    init: (module) => {
        initSync({ module });
    },
    reset: __wbg_reset_state,
    loop: loop$1,
    trapTest: BUILD.features.includes('trap-test') ? trapTest : undefined,
    snapshotSummary: snapshotSummary,
    schedulerSummary: schedulerSummary,
    actSummary: actSummary,
    profilerSummary: profilerSummary,
    queueSay: queueSay,
    persistSummary: persistSummary,
    persistTest: persistTest,
    configSummary: configSummary,
    configSet: configSet,
    configUnset: configUnset,
    configReset: configReset,
    constsSummary: constsSummary,
    movementSummary: movementSummary,
    moveTo: moveTo,
    spawnerSummary: spawnerSummary,
    spawn: spawn,
    openingSummary: openingSummary,
    plannerSummary: plannerSummary,
    remoteSummary: remoteSummary,
    signSummary: signSummary,
};
const loader = new Loader(env, glue, CONFIG, BUILD);
const routedError = (...args) => {
    const text = args.map(String).join(' ');
    loader.noteError(text);
    console.log(`[error] ${text}`);
};
const routedWarn = (...args) => console.log(`[warn] ${args.map(String).join(' ')}`);
/**
 * Screeps builds a new console before every tick, without error or warn (V96). The panic hook writes to
 * console.error and the glue may call console.warn, so both are (re)defined at the start of each tick.
 */
function routeConsole() {
    const c = console;
    nativeConsole ??= { error: typeof c.error, warn: typeof c.warn };
    try {
        c.error = routedError;
        if (typeof c.warn !== 'function')
            c.warn = routedWarn;
    }
    catch {
        // A console that does not take new properties keeps its own behavior.
    }
}
// Console commands, for example `rustedscreep.trapTest(10)`, `rustedscreep.status()`, `rustedscreep.snapshot()`,
// `rustedscreep.scheduler()`, `rustedscreep.act()`, `rustedscreep.profiler()`, `rustedscreep.say("hi")`,
// `rustedscreep.persist()`, `rustedscreep.persistTest()`, `rustedscreep.config()`,
// `rustedscreep.configSet("log.level", "debug")`, `rustedscreep.configUnset("log.level")`,
// `rustedscreep.configReset()`, `rustedscreep.consts()`, `rustedscreep.movement()`,
// `rustedscreep.moveTo("W1N1", 25, 25, 1)` (from another room: `rustedscreep.moveTo("W2N1", 25, 25, 1, "W1N1")`;
// `rustedscreep.moveTo()` cancels), `rustedscreep.spawner()` or `rustedscreep.spawn("W1N1", "worker")`
// (`rustedscreep.spawn()` cancels), `rustedscreep.opening()`, `rustedscreep.planner()`, `rustedscreep.remote()`,
// `rustedscreep.sign()`.
g.rustedscreep = {
    trapTest: (n = 1) => loader.scheduleTraps(Number(n)),
    status: () => loader.status(),
    snapshot: () => loader.snapshot(),
    scheduler: () => loader.scheduler(),
    act: () => loader.act(),
    profiler: () => loader.profiler(),
    say: (text) => loader.say(text),
    persist: () => loader.persist(),
    persistTest: (bytes) => loader.persistTest(bytes),
    config: () => loader.runtimeConfig(),
    configSet: (key, value) => loader.configSet(key, value),
    configUnset: (key) => loader.configUnset(key),
    configReset: () => loader.configReset(),
    consts: () => loader.consts(),
    movement: () => loader.movement(),
    moveTo: (room, x, y, range, from) => loader.moveTo(room, x, y, range, from),
    spawner: () => loader.spawner(),
    spawn: (room, role) => loader.spawn(room, role),
    opening: () => loader.opening(),
    planner: () => loader.planner(),
    remote: () => loader.remote(),
    sign: () => loader.sign(),
};
function loop() {
    try {
        routeConsole();
        loader.tick();
    }
    catch (e) {
        // The loader catches the wasm's own errors. This only keeps a loader bug from escaping loop, which
        // would also skip the tick's console commands (V96).
        console.log(`rustedscreep loader error: ${e instanceof Error ? (e.stack ?? e.message) : String(e)}`);
    }
}

exports.loop = loop;
