//! Constants self-check glue (ROADMAP P1 module 11): reads the server's global object for
//! `rustedscreep_core::consts_check`, logs what the check finds once per global, and answers the
//! `rustedscreep.consts()` console command.
//!
//! An Optional-tier step job, once per global (the first loop of a global never runs Optional work,
//! and below its bucket gate the tier is off, so the check waits). JS calls:
//! - the first step: `Object.getOwnPropertyNames(globalThis)`, one `JSON.stringify` of that list and
//!   its text into wasm;
//! - one step per upper-case name (423 on engine common 2.16.0): `Array.get`,
//!   `Object.getOwnPropertyDescriptor`, `Reflect.get(descriptor, "value")`, `JSON.stringify` and the
//!   text into wasm (ASCII, the glue's fast path, V123). The value comes from the descriptor, so a
//!   getter is never called (the runtime's `Memory` is one, V129);
//! - the clock: one scheduler sample before the job on each tick it runs (for its cost in the
//!   summary) and the slicer's own samples, one per batch of steps. Every reading is a scheduler
//!   sample, counted in `rustedscreep.scheduler()`; a tick without room under the Optional cap reads
//!   no clock at all.
//!
//! The comparison steps make no JS call. Once the check is over, nothing runs: no JS call and no
//! clock reading. Every binding catches: a throw ends the check for this global with a fault line
//! (the scheduler's fault log), never the tick, except a throwing `JSON.stringify` of one value,
//! which reads as "no JSON value" for that constant.

use std::fmt::Write as _;

use js_sys::{Array, JSON, Object, Reflect};
use rustedscreep_core::config::LogConfig;
use rustedscreep_core::consts_check::{Check, Globals, KNOWN, Phase, Report};
use rustedscreep_core::profiler;
use rustedscreep_core::scheduler::{Clock, Job, JobState, Step, TickCpu, Tier};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// `globalThis`, the object that holds the game constants (V129); `None` if the name is
    /// undefined. The glue reads it as `typeof globalThis === 'undefined' ? undefined : globalThis`.
    #[wasm_bindgen(thread_local_v2, js_name = globalThis)]
    static GLOBAL_THIS: Option<Object>;

    /// `Object.getOwnPropertyNames(obj)`. Local and `catch`: js-sys's binding has no `catch`, so a
    /// throw there would trap (V130).
    #[wasm_bindgen(js_namespace = Object, js_name = getOwnPropertyNames, catch)]
    fn own_names(obj: &JsValue) -> Result<Array, JsValue>;

    /// `Object.getOwnPropertyDescriptor(obj, key)`: undefined when `obj` has no such own property.
    #[wasm_bindgen(js_namespace = Object, js_name = getOwnPropertyDescriptor, catch)]
    fn own_descriptor(obj: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
}

/// The check's job: Optional (a check of the crate's constants never decides this tick's work).
const JOB: Job = Job {
    name: "consts check",
    tier: Tier::Optional,
};

/// The JS side of one check: the global object and its names list, kept between the check's ticks
/// (plain JS values, not game objects, so they stay valid across ticks).
#[derive(Default)]
struct JsGlobals {
    global: Option<JsValue>,
    names: Option<Array>,
    /// The string "value", made once per check.
    value_key: Option<JsValue>,
}

impl Globals for JsGlobals {
    fn names(&mut self) -> Result<Option<String>, &'static str> {
        let global: JsValue = GLOBAL_THIS
            .with(Option::clone)
            .map(JsValue::from)
            .filter(|g| !g.is_undefined() && !g.is_null())
            .ok_or("globalThis is undefined")?;
        let names = own_names(&global).map_err(|_| "Object.getOwnPropertyNames threw")?;
        let text = JSON::stringify(&names).map_err(|_| "JSON.stringify of the names threw")?;
        self.global = Some(global);
        self.names = Some(names);
        Ok(text.as_string())
    }

    fn value(&mut self, index: u32) -> Result<Option<String>, &'static str> {
        let (Some(global), Some(names)) = (&self.global, &self.names) else {
            return Err("the names were not read");
        };
        // The array came from getOwnPropertyNames and the index from its JSON text, so it is in range.
        let key = names.get(index);
        let descriptor =
            own_descriptor(global, &key).map_err(|_| "Object.getOwnPropertyDescriptor threw")?;
        if descriptor.is_undefined() {
            // Deleted since the names were read: no value.
            return Ok(None);
        }
        let value_key = self
            .value_key
            .get_or_insert_with(|| JsValue::from_str("value"));
        let value = Reflect::get(&descriptor, value_key).map_err(|_| "Reflect.get threw")?;
        // A value JSON.stringify throws on (a cycle, a BigInt) has no JSON text; that is a finding
        // about this constant, not a reason to stop the check.
        Ok(JSON::stringify(&value).ok().and_then(|t| t.as_string()))
    }
}

/// The check for this global, with its timing.
#[derive(Default)]
pub struct ConstsCheck {
    check: Check,
    js: JsGlobals,
    /// The read steps' and the comparison steps' job states. A comparison group costs far more than a
    /// read, and the slicer sizes a batch from the last measured cost, so each phase measures its own.
    read_job: JobState,
    compare_job: JobState,
    /// `Game.time` of the first tick the job ran, and of the tick it finished.
    started: Option<u32>,
    finished: Option<u32>,
    /// Ticks the job ran in, and their measured CPU. Unit: µCPU.
    ticks: u32,
    cpu: u64,
    /// A reading was bad, so `cpu` is a lower bound.
    cpu_partial: bool,
}

impl ConstsCheck {
    /// Runs the job's steps within the Optional cap; does nothing once the check is over. When it
    /// completes, logs the report once (`log.consts_findings` lines at most).
    pub fn run<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, now: u32, log: &LogConfig) {
        if self.check.is_finished() {
            return;
        }
        // Room under the Optional cap, judged from this tick's last reading (no clock read).
        let room = tick
            .budget()
            .cap(JOB.tier)
            .is_some_and(|cap| tick.spent() < cap);
        if !room {
            // `run_steps` counts a tier-off skip or a budget stop, with no step and no clock reading.
            let Self {
                check,
                js,
                read_job,
                ..
            } = self;
            tick.run_steps(&JOB, read_job, |_, _| check.step(js));
            return;
        }
        self.run_now(tick, now, log);
    }

    /// Kept out of the per-tick `loop` function: the check runs on a few ticks per global.
    #[inline(never)]
    fn run_now<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, now: u32, log: &LogConfig) {
        let reading = |p: Phase| matches!(p, Phase::Waiting | Phase::Reading { .. });
        let t0 = tick.sample();
        let mut ran = 0u32;
        let Self {
            check,
            js,
            read_job,
            compare_job,
            ..
        } = self;
        if reading(check.phase()) {
            let report = tick.run_steps(&JOB, read_job, |_, _| match check.step(js) {
                // End this call where the reads end, so the comparisons start a call of their own,
                // whose first batch is one step that measures their cost.
                Ok(Step::More) if !reading(check.phase()) => Ok(Step::Done),
                other => other,
            });
            ran = ran.saturating_add(report.ran);
        }
        if matches!(check.phase(), Phase::Comparing { .. }) {
            let report = tick.run_steps(&JOB, compare_job, |_, _| check.step(js));
            ran = ran.saturating_add(report.ran);
        }
        // The slicer's last sample (it samples after every batch); no clock read.
        let t1 = tick.spent();
        if ran > 0 {
            self.started.get_or_insert(now);
            self.ticks = self.ticks.saturating_add(1);
            match profiler::span(t0, t1) {
                Some(us) => self.cpu = self.cpu.saturating_add(u64::from(us)),
                None => self.cpu_partial = true,
            }
        }
        if self.check.is_finished() {
            self.finished = Some(now);
            // The JS handles are not needed any more.
            self.js = JsGlobals::default();
            if let Some(r) = self.check.report() {
                log_report(r, now, self.ticks, self.cost(), log);
            }
        }
    }

    /// The measured CPU, with a mark when a reading was bad.
    fn cost(&self) -> Cost {
        Cost(self.cpu, self.cpu_partial)
    }

    /// One line for `rustedscreep.consts()`: the phase, and once done the counts, every kept finding
    /// and the known differences seen, with their reasons.
    pub fn summary(&self) -> String {
        let since = |text: &mut String| {
            if let Some(t) = self.started {
                let _ = write!(text, " (started at tick {t})");
            }
        };
        match self.check.phase() {
            Phase::Waiting => String::from(
                "rustedscreep consts: not run yet in this global (an Optional-tier job: it waits out \
                 the first loop of a global, then runs on a tick when the Optional tier is on and has \
                 CPU left under its cap; see rustedscreep.scheduler())",
            ),
            Phase::Reading { read, of } => {
                let mut text =
                    format!("rustedscreep consts: reading the server's constants, {read} of {of}");
                since(&mut text);
                text
            }
            Phase::Comparing { group, of } => {
                let mut text = format!("rustedscreep consts: comparing, group {group} of {of}");
                since(&mut text);
                text
            }
            Phase::Failed(what) => {
                let at = self.finished.unwrap_or_default();
                format!(
                    "rustedscreep consts: failed at tick {at}: {what}; it does not retry in this global"
                )
            }
            Phase::Done => match self.check.report() {
                Some(r) => self.done_summary(r),
                None => String::from("rustedscreep consts: done, no report"),
            },
        }
    }

    fn done_summary(&self, r: &Report) -> String {
        let mut text = format!(
            "rustedscreep consts: checked at tick {} ({} tick(s) from tick {}, {}): {}",
            self.finished.unwrap_or_default(),
            self.ticks,
            self.started.unwrap_or_default(),
            self.cost(),
            r.counts()
        );
        if !r.findings.is_empty() {
            let more = r
                .finding_count()
                .saturating_sub(u32::try_from(r.findings.len()).unwrap_or(u32::MAX));
            let _ = write!(text, " | findings:");
            for f in &r.findings {
                let _ = write!(text, " {f};");
            }
            if more > 0 {
                let _ = write!(text, " and {more} more not kept");
            }
        }
        if !r.known_seen.is_empty() {
            let _ = write!(text, " | known:");
            for &i in &r.known_seen {
                if let Some(k) = KNOWN.get(usize::from(i)) {
                    let _ = write!(text, " {} ({});", k.path, k.reason);
                }
            }
        }
        text
    }
}

/// CPU in µCPU, marked as a lower bound when a reading was bad.
struct Cost(u64, bool);

impl std::fmt::Display for Cost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(us, partial) = *self;
        let bound = if partial { "at least " } else { "" };
        write!(f, "{bound}{us} µCPU")
    }
}

/// The completion lines: one info line when nothing differs; otherwise a warning with the counts,
/// one per finding up to `log.consts_findings`, and one for the rest.
#[cold]
fn log_report(r: &Report, now: u32, ticks: u32, cost: Cost, log: &LogConfig) {
    let count = r.finding_count();
    if count == 0 {
        log::info!(
            "rustedscreep consts: checked at tick {now} ({ticks} tick(s), {cost}): {}",
            r.counts()
        );
        return;
    }
    log::warn!(
        "rustedscreep consts: {count} finding(s) at tick {now} ({ticks} tick(s), {cost}): {}",
        r.counts()
    );
    let lines = usize::try_from(log.consts_findings).unwrap_or(usize::MAX);
    for f in r.findings.iter().take(lines) {
        log::warn!("rustedscreep consts: {f}");
    }
    let shown = u32::try_from(r.findings.len().min(lines)).unwrap_or(u32::MAX);
    let rest = count.saturating_sub(shown);
    if rest > 0 {
        log::warn!(
            "rustedscreep consts: {rest} more finding(s); rustedscreep.consts() lists the first {}",
            r.findings.len()
        );
    }
}
