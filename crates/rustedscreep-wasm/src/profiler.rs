//! Profiler glue (ROADMAP P1 module 8): the `Game.cpu.getUsed()` readings of one loop, the stats
//! segment write, the cold-loop log line and the `rustedscreep.profiler()` summary, around
//! `rustedscreep_core::profiler`.
//!
//! With the `profiler` feature, JS calls per loop: six `Game.cpu.getUsed()` (two back to back at the
//! start, then after the snapshot, the decide step, the act step and the fault log). On a global's
//! first loop, one `console.log`. On the loop that closes a window: the `RawMemory.segments` getter,
//! an `is_object` check, two glue casts (the id to a number, the segment text to a JS string, which
//! the glue's TextDecoder decodes), one `Reflect.set`, one more reading, and a `console.log` if the
//! write failed. Both segment calls catch a JS exception, so a missing or broken `RawMemory` costs a
//! log line, never a trap (V115).
//!
//! The segment is written without being activated; the driver saves every key of
//! `RawMemory.segments` at the end of the tick, and a tick may save at most 10 (V41, V85). Until P1
//! module 9's segment manager counts this key, nothing else writes segments.
//!
//! Without the feature, `Probe` and `Profiler` are zero-sized types with empty inline methods, and
//! `summary` returns a fixed note: no JS call in the loop, and none of `rustedscreep_core::profiler`
//! is linked. The sandbox counts the calls in both builds (js/tools/smoke.mjs).

pub use imp::{Probe, Profiler, summary};

#[cfg(feature = "profiler")]
mod imp {
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

        /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
        #[wasm_bindgen(js_namespace = console, js_name = log)]
        fn console_log(message: &str);
    }

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
        /// stats segment when the loop closed a window.
        pub fn finish(&mut self, probe: Probe, ctx: &TickContext, issues: &IssueStats) {
            let mut marks = probe.0;
            marks.end = game::cpu::get_used();
            let sample = Sample {
                marks,
                calls: issues.issued,
                billed: issues.billed,
            };
            match self.core.record(ctx, &sample, &PROFILER_MMO) {
                Recorded::Open => {}
                Recorded::Cold => self.log_cold(),
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
        /// reading (`end`). Once per window; kept out of the per-tick `loop` function.
        #[cold]
        #[inline(never)]
        fn flush(&mut self, now: u32, end: f64) {
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
        }
    }

    /// `RawMemory.segments[id] = text`.
    fn write_segment(id: u8, text: &str) -> Result<(), &'static str> {
        let object = RawMemory::segments().map_err(|_| "reading RawMemory.segments threw")?;
        if !object.is_object() {
            return Err("RawMemory.segments is not an object");
        }
        let key = JsValue::from_f64(f64::from(id));
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
        #[inline(always)]
        pub fn finish(&mut self, _probe: Probe, _ctx: &TickContext, _issues: &IssueStats) {}
    }

    pub fn summary(_prof: &Profiler, _now: u32) -> String {
        String::from(
            "rustedscreep profiler: not in this build; build with npm run build -- --features profiler",
        )
    }
}
