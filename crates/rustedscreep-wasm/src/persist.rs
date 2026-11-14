//! Persistence glue (ROADMAP P1 module 9): the RawMemory calls around
//! `rustedscreep_core::persist`'s segment manager, the self-test behind
//! `rustedscreep.persistTest(bytes)`, and the `rustedscreep.persist()` summary.
//!
//! Each tick, in this order:
//! - In the decide step, the loads: a Standard-tier step job, one wanted segment per step, so the
//!   scheduler's cap time-slices a burst. JS calls: none unless a load is due; then the
//!   `RawMemory.segments` getter once, and per segment one `Reflect.get` and, for a string, its
//!   crossing into wasm (ASCII, so the glue's fast path, V123).
//! - After the act step, while the tick runs the Standard tier: the writes and the active set.
//!   Writes: none unless a record is dirty; then the getter, `Object.keys` and one `Array` read plus
//!   a string crossing per key present (at most 10), and per write one text crossing to JS (the
//!   TextDecoder polyfill on the MMO) and one `Reflect.set`. The active set: one `setActiveSegments`
//!   call when the manager asks for one (a global's first loop, a change, a retry).
//!
//! An idle tick (nothing due, nothing dirty, the set unchanged) makes no JS call. Every binding
//! catches a JS exception, so a missing or broken `RawMemory` costs a log line, never a trap; each
//! kind of failure is logged once per global and counted after that.
//!
//! In a `profiler` build one save key stays free for the profiler's stats segment unless that key
//! is already present; the manager never activates or writes that segment in any build.

use std::fmt::Write as _;

use js_sys::{Array, Reflect};
use rustedscreep_core::config::{PERSIST_MMO, PROFILER_MMO};
use rustedscreep_core::persist::selftest::{self, Failure, Phase, Requested, SelfTest, Verdict};
use rustedscreep_core::persist::{
    Keys, LoadError, SegmentId, SegmentSet, Segments, WriteResult, frame, kind,
};
use rustedscreep_core::profiler;
use rustedscreep_core::scheduler::{Clock, Job, JobState, Step, TickCpu, Tier};
use screeps::game;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    type RawMemory;

    /// `RawMemory.segments`, this tick's object of loaded and written segment strings (V120).
    #[wasm_bindgen(static_method_of = RawMemory, getter = segments, catch)]
    fn segments() -> Result<JsValue, JsValue>;

    /// `RawMemory.setActiveSegments(ids)`: takes effect on the next tick (V116, V118). Local and
    /// `catch`: the crate's binding has no `catch`, so a throw there would trap (V124).
    #[wasm_bindgen(static_method_of = RawMemory, js_name = setActiveSegments, catch)]
    fn set_active_segments(ids: &Array) -> Result<(), JsValue>;

    /// `Object.keys(obj)`, what the driver's end-of-tick save counts (V119).
    #[wasm_bindgen(js_namespace = Object, js_name = keys, catch)]
    fn object_keys(obj: &JsValue) -> Result<Array, JsValue>;

    /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(message: &str);
}

/// The load job: Standard tier (persisted records are never needed for Critical work: an owner
/// rebuilds without them).
const LOAD: Job = Job {
    name: "persist load",
    tier: Tier::Standard,
};

/// JS operations whose failures are logged once per global.
#[derive(Clone, Copy)]
enum Op {
    Segments,
    Keys,
    Get,
    Set,
    Activate,
}

impl Op {
    const fn bit(self) -> u8 {
        1 << self as u8
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Segments => "reading RawMemory.segments",
            Self::Keys => "counting RawMemory.segments keys",
            Self::Get => "reading a segment",
            Self::Set => "writing a segment",
            Self::Activate => "RawMemory.setActiveSegments",
        }
    }
}

/// The persistence state for one global, with its reused buffers.
pub struct Persist {
    mgr: Segments,
    job: JobState,
    test: SelfTest,
    test_id: Option<SegmentId>,
    /// Frame text for writes.
    text: String,
    /// Decoded frame bytes for loads.
    bytes: Vec<u8>,
    /// Payload bytes for writes, and the self-test's expected pattern on loads.
    scratch: Vec<u8>,
    /// Bit per [`Op`] already logged in this global.
    logged: u8,
    js_failures: u32,
}

impl Default for Persist {
    fn default() -> Self {
        let reserved =
            SegmentId::new(PROFILER_MMO.stats_segment).map_or(SegmentSet::EMPTY, SegmentSet::of);
        Self {
            mgr: Segments::new(reserved),
            job: JobState::default(),
            test: SelfTest::default(),
            test_id: SegmentId::new(PERSIST_MMO.self_test_segment),
            text: String::new(),
            bytes: Vec::new(),
            scratch: Vec::new(),
            logged: 0,
            js_failures: 0,
        }
    }
}

/// `RawMemory.segments` as an object, or why not.
fn read_segments() -> Result<JsValue, &'static str> {
    let object = RawMemory::segments().map_err(|_| "the getter threw")?;
    if object.is_object() {
        Ok(object)
    } else {
        Err("it is not an object")
    }
}

fn key(id: SegmentId) -> JsValue {
    JsValue::from_f64(f64::from(id.get()))
}

/// `RawMemory.segments[id] = text`.
fn set_segment(segs: &JsValue, id: SegmentId, text: &str) -> Result<(), &'static str> {
    match Reflect::set(segs, &key(id), &JsValue::from_str(text)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the object refused the property"),
        Err(_) => Err("the assignment threw"),
    }
}

/// The ids among the keys of `segs` and the key count, as the driver's save sees them (V119).
fn present_keys(segs: &JsValue) -> Result<(u32, SegmentSet), &'static str> {
    let keys = object_keys(segs).map_err(|_| "Object.keys threw")?;
    let count = keys.length();
    let mut present = SegmentSet::EMPTY;
    for i in 0..count {
        if let Some(id) = keys
            .get(i)
            .as_string()
            .as_deref()
            .and_then(SegmentId::from_key)
        {
            present.insert(id);
        }
    }
    Ok((count, present))
}

impl Persist {
    /// Logs the first failure of `op` in this global; counts every one.
    #[cold]
    fn fail(&mut self, op: Op, now: u32, why: &str) {
        self.js_failures = self.js_failures.saturating_add(1);
        if self.logged & op.bit() == 0 {
            self.logged |= op.bit();
            console_log(&format!(
                "rustedscreep persist: {} failed at tick {now}: {why} (later failures of this kind are counted, not logged)",
                op.name()
            ));
        }
    }

    /// The decide step's part: reads the segments due this tick, one per step of the load job.
    #[inline]
    pub fn load<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, now: u32) {
        let due = self.mgr.due(now);
        if !due.is_empty() && tick.allows(LOAD.tier) {
            self.load_due(tick, due, now);
        }
    }

    /// Kept out of the per-tick `loop` function: loads are rare (once per global, or on request).
    #[inline(never)]
    fn load_due<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, due: SegmentSet, now: u32) {
        let segs = match read_segments() {
            Ok(s) => s,
            Err(why) => {
                self.fail(Op::Segments, now, why);
                // Each due segment counts as missing, so the loads give up after `load_retries`.
                for id in due.iter() {
                    if let Some(err) = self.mgr.absent(id, &PERSIST_MMO) {
                        self.deliver(id, now, err, None);
                    }
                }
                return;
            }
        };
        let mut ids = due.iter().peekable();
        let mut job = self.job;
        tick.run_steps(&LOAD, &mut job, |_, _| {
            if let Some(id) = ids.next() {
                self.load_one(&segs, id, now);
            }
            Ok(if ids.peek().is_some() {
                Step::More
            } else {
                Step::Done
            })
        });
        self.job = job;
    }

    fn load_one(&mut self, segs: &JsValue, id: SegmentId, now: u32) {
        let testing = Some(id) == self.test_id && matches!(self.test.phase(), Phase::Read { .. });
        let t0 = if testing { game::cpu::get_used() } else { 0.0 };
        let value = match Reflect::get(segs, &key(id)) {
            Ok(v) => v,
            Err(_) => {
                self.fail(Op::Get, now, "Reflect.get threw");
                JsValue::UNDEFINED
            }
        };
        if value.is_undefined() {
            if let Some(err) = self.mgr.absent(id, &PERSIST_MMO) {
                self.deliver(id, now, err, None);
            }
            return;
        }
        let text = value.as_string();
        let get = if testing {
            profiler::span(t0, game::cpu::get_used())
        } else {
            None
        };
        let ok = match text {
            None => self.deliver(id, now, LoadError::NotText, get),
            Some(text) => self.deliver_text(id, now, &text, get),
        };
        self.mgr.loaded(id, ok);
    }

    /// Decodes `text` and hands it to `id`'s owner. Returns whether the owner accepted it.
    fn deliver_text(&mut self, id: SegmentId, now: u32, text: &str, get: Option<u32>) -> bool {
        if Some(id) != self.test_id {
            // No other owner exists before P2.
            return false;
        }
        let t1 = game::cpu::get_used();
        let mut bytes = std::mem::take(&mut self.bytes);
        let frame = frame::decode(text, &mut bytes).map_err(LoadError::Decode);
        let done = self
            .test
            .loaded(now, frame, get, &mut self.scratch)
            .is_some();
        self.bytes = bytes;
        if !done {
            return false;
        }
        self.test
            .note_decode(profiler::span(t1, game::cpu::get_used()));
        self.test.last().is_some_and(|v| {
            log_verdict(v);
            v.result.is_ok()
        })
    }

    /// Hands a failed load to `id`'s owner (the self-test before P2). Returns false.
    fn deliver(&mut self, id: SegmentId, now: u32, err: LoadError, get: Option<u32>) -> bool {
        if Some(id) == self.test_id
            && let Some(v) = self.test.loaded(now, Err(err), get, &mut self.scratch)
        {
            log_verdict(&v);
        }
        false
    }

    /// The output step's part, after the act step: writes the dirty records the save cap allows,
    /// then sets the active set if the manager asks. Runs only while the tick runs the Standard tier.
    #[inline]
    pub fn flush(&mut self, now: u32, standard: bool) {
        if standard
            && (!self.mgr.dirty().is_empty() || self.mgr.plan_active(&PERSIST_MMO).is_some())
        {
            self.flush_work(now);
        }
    }

    /// Kept out of the per-tick `loop` function: writes and activations are rare.
    #[inline(never)]
    fn flush_work(&mut self, now: u32) {
        if !self.mgr.dirty().is_empty() {
            match read_segments() {
                Ok(segs) => self.write(&segs, now),
                Err(why) => {
                    self.fail(Op::Segments, now, why);
                    self.abort_test_write(now);
                }
            }
        }
        if let Some(set) = self.mgr.plan_active(&PERSIST_MMO) {
            let ids = Array::new();
            for id in set.iter() {
                ids.push(&key(id));
            }
            match RawMemory::set_active_segments(&ids) {
                Ok(()) => self.mgr.activated(set, now),
                Err(e) => {
                    self.mgr.activation_failed();
                    let why = e.as_string().unwrap_or_else(|| String::from("it threw"));
                    self.fail(Op::Activate, now, &why);
                }
            }
        }
    }

    fn write(&mut self, segs: &JsValue, now: u32) {
        let (count, present) = match present_keys(segs) {
            Ok(k) => k,
            Err(why) => {
                self.fail(Op::Keys, now, why);
                return self.abort_test_write(now);
            }
        };
        // The profiler writes its segment at the end of a window-closing tick; it checks for a free
        // key itself (profiler.rs), and this keeps one free for it.
        let profiler_key = cfg!(feature = "profiler")
            && SegmentId::new(PROFILER_MMO.stats_segment).is_some_and(|p| !present.contains(p));
        let keys = Keys {
            count,
            present,
            extra: u32::from(profiler_key),
        };
        let plan = self.mgr.plan_writes(&keys, &PERSIST_MMO);
        self.mgr.planned(&plan);
        for id in plan.write.iter() {
            let result = if Some(id) == self.test_id {
                self.write_test(segs, id, now)
            } else {
                // No other owner exists before P2: nothing to write.
                WriteResult::Rejected
            };
            self.mgr.written(id, result);
        }
    }

    /// Writes the self-test frame; on success requests it back.
    fn write_test(&mut self, segs: &JsValue, id: SegmentId, now: u32) -> WriteResult {
        if !self.test.payload(now, &mut self.scratch) {
            return WriteResult::Rejected;
        }
        let t0 = game::cpu::get_used();
        let encoded = frame::encode(
            kind::SELF_TEST,
            selftest::SCHEMA,
            now,
            &self.scratch,
            &mut self.text,
        );
        let t1 = game::cpu::get_used();
        let Ok(units) = encoded else {
            // The request is clamped to the maximum, so this does not happen; end the run if it does.
            if let Some(v) = self.test.write_failed(now) {
                log_verdict(&v);
            }
            return WriteResult::Rejected;
        };
        match set_segment(segs, id, &self.text) {
            Ok(()) => {
                let t2 = game::cpu::get_used();
                self.test
                    .wrote(now, units, profiler::span(t0, t1), profiler::span(t1, t2));
                self.mgr.request_load(id);
                WriteResult::Written
            }
            Err(why) => {
                self.fail(Op::Set, now, why);
                // The run ends with a verdict rather than retrying every tick (real owners keep
                // `Failed`, which retries).
                if let Some(v) = self.test.write_failed(now) {
                    log_verdict(&v);
                }
                WriteResult::Rejected
            }
        }
    }

    /// A self-test write that cannot go out this tick (the segments object or its keys could not be
    /// read) ends the run with a verdict, and the record stops being dirty.
    fn abort_test_write(&mut self, now: u32) {
        let Some(id) = self.test_id else { return };
        if self.mgr.dirty().contains(id) && matches!(self.test.phase(), Phase::Write { .. }) {
            if let Some(v) = self.test.write_failed(now) {
                log_verdict(&v);
            }
            self.mgr.written(id, WriteResult::Rejected);
        }
    }

    /// The profiler found no free save key for its stats segment: the next output step re-issues the
    /// active set, which drops keys this global did not request (a console `setActiveSegments`).
    pub fn keys_short(&mut self) {
        self.mgr.mark_stale();
    }

    /// `rustedscreep.persistTest(bytes)`: starts a self-test run on the next tick.
    pub fn request_test(&mut self, size: Option<u32>, now: u32) -> String {
        let Some(id) = self.test_id else {
            return String::from(
                "rustedscreep persist self-test: the configured segment id is not valid",
            );
        };
        match self.test.request(size, now) {
            Requested::Busy => String::from(
                "rustedscreep persist self-test: a run is in progress; see rustedscreep.persist()",
            ),
            Requested::Started { size, clamped } => {
                self.mgr.mark_dirty(id);
                let note = if clamped {
                    " (clamped to the maximum)"
                } else {
                    ""
                };
                format!(
                    "rustedscreep persist self-test: {size} B{note} into segment {} on the next tick; the verdict follows a tick later",
                    id.get()
                )
            }
        }
    }

    /// One line for `rustedscreep.persist()`.
    pub fn summary(&self) -> String {
        let m = &self.mgr;
        let c = m.counters();
        let mut s = String::from("rustedscreep persist: ");
        match m.active() {
            Some((set, at)) => {
                let _ = write!(s, "active {set} (set at {at})");
            }
            None => s.push_str("active set unknown (not set in this global)"),
        }
        let _ = write!(
            s,
            " | wanted {} | dirty {} | reserved {} | loads {} ({} failed, {} retries) | writes {} ({} failed, {} rejected, {} ticks short of a key) | activations {} ({} failed) | JS failures {}",
            m.wanted(),
            m.dirty(),
            m.reserved(),
            c.loads,
            c.load_failures,
            c.retries,
            c.writes,
            c.write_failures,
            c.write_rejections,
            c.no_key_ticks,
            c.activations,
            c.activation_failures,
            self.js_failures,
        );
        s.push_str(" | self-test: ");
        match self.test.phase() {
            Phase::Write { size, asked } => {
                let _ = write!(s, "{size} B asked at {asked}, writing");
            }
            Phase::Read {
                size,
                written,
                units,
                ..
            } => {
                let _ = write!(s, "{size} B in {units} units written at {written}, reading");
            }
            Phase::Idle => match self.test.last() {
                None => s.push_str("none run in this global"),
                Some(v) => verdict_text(&mut s, v),
            },
        }
        s
    }
}

fn cost(c: Option<u32>) -> String {
    c.map_or_else(|| String::from("?"), |v| v.to_string())
}

fn verdict_text(s: &mut String, v: &Verdict) {
    match v.result {
        Ok(()) => {
            let _ = write!(
                s,
                "round trip ok: {} B in {} units, written at {}, read at {}; µCPU encode {}, set {}, get {}, decode {}",
                v.size,
                v.units,
                v.written,
                v.at,
                cost(v.costs.encode),
                cost(v.costs.set),
                cost(v.costs.get),
                cost(v.costs.decode),
            );
        }
        Err(why) => {
            let _ = write!(s, "FAILED at {}: {why}", v.at);
            if matches!(why, Failure::Load(_)) {
                s.push_str(" (an owner would rebuild from game state)");
            }
        }
    }
}

#[cold]
fn log_verdict(v: &Verdict) {
    let mut s = String::from("rustedscreep persist self-test: ");
    verdict_text(&mut s, v);
    console_log(&s);
}
