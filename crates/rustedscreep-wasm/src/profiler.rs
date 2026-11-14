//! Profiler glue (ROADMAP P1 module 8): the `Game.cpu.getUsed()` readings of one loop, the stats
//! segment write, the cold-loop log line and the `rustedscreep.profiler()` summary, around
//! `rustedscreep_core::profiler`.
//!
//! With the `profiler` feature, JS calls per loop: six `Game.cpu.getUsed()` (two back to back at the
//! start, then after the snapshot, the decide step, the act step and the fault log). On a global's
//! first loop, one `console.log`. On the loop that closes a window: the `RawMemory.segments` getter,
//! an `is_object` check, two glue casts (the id to a number, the segment text to a JS string, which
//! the glue's TextDecoder decodes), one `Reflect.has` and, when the key is not present yet,
//! `Object.keys` and its length, one `Reflect.set`, one more reading, and a `console.log` if the
//! write failed. Every segment call catches a JS exception, so a missing or broken `RawMemory` costs
//! a log line, never a trap (V115).
//!
//! The segment is written without being activated; the driver saves every key of
//! `RawMemory.segments` at the end of the tick, and more than `MEMORY_SEGMENT_SAVE_LIMIT` keys throw
//! and likely lose the tick (V41, V85, V119). So the write goes only into a key already present or a
//! free one. The segment manager (P1 module 9, `persist.rs`) never writes this id and keeps a key
//! free for it; only keys it did not request (a console `setActiveSegments`) can take the last one,
//! and then the profiler skips the write and tells the manager, which re-issues its active set.
//!
//! Without the feature, `Probe` and `Profiler` are zero-sized types with empty inline methods, and
//! `summary` returns a fixed note: no JS call in the loop, and none of `rustedscreep_core::profiler`
//! is linked. The sandbox counts the calls in both builds (js/tools/smoke.mjs).

pub use imp::{Probe, Profiler, summary};

#[cfg(feature = "profiler")]
mod imp {
    use rustedscreep_consts::extra::MEMORY_SEGMENT_SAVE_LIMIT;
    use rustedscreep_core::config::{PROFILER_MMO, SCHEDULER_MMO};
    use rustedscreep_core::intents::IssueStats;
    use rustedscreep_core::profiler::{self, Marks, Recorded, Sample};
    use rustedscreep_core::snapshot::TickContext;
    use screeps::game;
    use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

    #[wasm_bindgen]
    extern "C" {
        type RawMemory;

        /// `RawMemory.segments`, this tick's object of segment strings (V41). `catch`: without a
        /// `RawMemory` global the getter throws, which becomes an `Err` here instead of a trap.
        #[wasm_bindgen(static_method_of = RawMemory, getter = segments, catch)]
        fn segments() -> Result<JsValue, JsValue>;

        /// `Object.keys(obj)`, what the driver's end-of-tick save counts (V119).
        #[wasm_bindgen(js_namespace = Object, js_name = keys, catch)]
        fn object_keys(obj: &JsValue) -> Result<js_sys::Array, JsValue>;

        /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
        #[wasm_bindgen(js_namespace = console, js_name = log)]
        fn console_log(message: &str);
    }

    /// Why a write was skipped when the tick already holds the most keys the driver saves.
    const NO_FREE_KEY: &str =
        "no free save key (the tick already holds the most segments the driver saves)";

    /// One loop's readings so far.
    pub struct Probe(Marks);

    impl Probe {
        /// Takes the loop's first two readings, back to back.
        pub fn start() -> Self {
            let start = game::cpu::get_used();
            let clock = game::cpu::get_used();
            Self(Marks {
                start,
                clock,
                ..Marks::default()
            })
        }

        pub fn snapshot(&mut self) {
            self.0.snapshot = game::cpu::get_used();
        }

        pub fn decide(&mut self) {
            self.0.decide = game::cpu::get_used();
        }

        pub fn act(&mut self) {
            self.0.act = game::cpu::get_used();
        }
    }

    /// The profiler's state for one global and its reused segment text.
    #[derive(Default)]
    pub struct Profiler {
        core: profiler::Profiler,
        text: String,
    }

    impl Profiler {
        /// Takes the loop's last reading and records the loop; logs the cold loop, or writes the
        /// stats segment when the loop closed a window. Returns true when that write found no free
        /// save key.
        pub fn finish(&mut self, probe: Probe, ctx: &TickContext, issues: &IssueStats) -> bool {
            let mut marks = probe.0;
            marks.end = game::cpu::get_used();
            let sample = Sample {
                marks,
                calls: issues.issued,
                billed: issues.billed,
            };
            match self.core.record(ctx, &sample, &PROFILER_MMO) {
                Recorded::Open => false,
                Recorded::Cold => {
                    self.log_cold();
                    false
                }
                Recorded::Closed => self.flush(ctx.time, marks.end),
            }
        }

        /// Once per global; kept out of the per-tick `loop` function.
        #[cold]
        #[inline(never)]
        fn log_cold(&self) {
            if let Some(cold) = self.core.cold() {
                console_log(&profiler::cold_line(cold));
            }
        }

        /// Writes the stats segment and records the outcome and its cost from the loop's last
        /// reading (`end`). Once per window; kept out of the per-tick `loop` function. Returns true
        /// when the write found no free save key.
        #[cold]
        #[inline(never)]
        fn flush(&mut self, now: u32, end: f64) -> bool {
            self.text.clear();
            self.core
                .write_segment(now, &PROFILER_MMO, &SCHEDULER_MMO, &mut self.text);
            let result = write_segment(PROFILER_MMO.stats_segment, &self.text);
            let after = game::cpu::get_used();
            // The same rule as the spans: a bad or falling reading measures nothing.
            let cost = profiler::span(end, after);
            self.core.note_flush(result.is_ok(), cost);
            if let Err(why) = result {
                console_log(&format!(
                    "rustedscreep profiler: writing stats segment {} failed at tick {now}: {why}",
                    PROFILER_MMO.stats_segment
                ));
            }
            result == Err(NO_FREE_KEY)
        }
    }

    /// `RawMemory.segments[id] = text`, unless the key is new and the tick already holds
    /// `MEMORY_SEGMENT_SAVE_LIMIT` keys (V119).
    ///
    /// Uses: `rustedscreep_consts::extra::MEMORY_SEGMENT_SAVE_LIMIT`.
    fn write_segment(id: u8, text: &str) -> Result<(), &'static str> {
        let object = RawMemory::segments().map_err(|_| "reading RawMemory.segments threw")?;
        if !object.is_object() {
            return Err("RawMemory.segments is not an object");
        }
        let key = JsValue::from_f64(f64::from(id));
        let present =
            js_sys::Reflect::has(&object, &key).map_err(|_| "checking RawMemory.segments threw")?;
        if !present {
            let keys =
                object_keys(&object).map_err(|_| "counting RawMemory.segments keys threw")?;
            if keys.length() >= MEMORY_SEGMENT_SAVE_LIMIT {
                return Err(NO_FREE_KEY);
            }
        }
        match js_sys::Reflect::set(&object, &key, &JsValue::from_str(text)) {
            Ok(true) => Ok(()),
            Ok(false) => Err("RawMemory.segments refused the write"),
            Err(_) => Err("writing RawMemory.segments threw"),
        }
    }

    /// One line for `rustedscreep.profiler()`.
    pub fn summary(prof: &Profiler, now: u32) -> String {
        prof.core.summary(now, &PROFILER_MMO, &SCHEDULER_MMO)
    }
}

#[cfg(not(feature = "profiler"))]
mod imp {
    use rustedscreep_core::intents::IssueStats;
    use rustedscreep_core::snapshot::TickContext;

    /// No readings in a build without the `profiler` feature.
    pub struct Probe;

    impl Probe {
        #[inline(always)]
        pub fn start() -> Self {
            Self
        }

        #[inline(always)]
        pub fn snapshot(&mut self) {}

        #[inline(always)]
        pub fn decide(&mut self) {}

        #[inline(always)]
        pub fn act(&mut self) {}
    }

    #[derive(Default)]
    pub struct Profiler;

    impl Profiler {
        /// Never writes a segment, so it never lacks a save key.
        #[inline(always)]
        pub fn finish(&mut self, _probe: Probe, _ctx: &TickContext, _issues: &IssueStats) -> bool {
            false
        }
    }

    pub fn summary(_prof: &Profiler, _now: u32) -> String {
        String::from(
            "rustedscreep profiler: not in this build; build with npm run build -- --features profiler",
        )
    }
}
