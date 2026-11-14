//! WASM boundary: snapshot (JS → Rust), act (Rust → intents), persistence (RawMemory), the profiler
//! and the exported `loop`. The only crate that uses `wasm-bindgen`, `js-sys` or game-object APIs.

// No-panic rule for non-test code (.claude/rules/wasm-bot.md; lint names: VERIFICATION_LOG V83).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable
    )
)]

mod act;
#[cfg(feature = "act-test")]
mod act_test;
mod config;
mod logging;
mod persist;
mod profiler;
mod scheduler;
mod snapshot;

use std::cell::RefCell;

use rustedscreep_core::scheduler::{Scheduler, Tier};
use rustedscreep_core::snapshot::{SkipLog, Snapshot};
use wasm_bindgen::prelude::wasm_bindgen;

/// Everything the bot keeps between ticks. It lives in Rust memory only: a global reset (or a
/// re-instantiation by the loader) starts it empty, and the next tick rebuilds it.
#[derive(Default)]
struct State {
    /// The config in force: the active profile with the runtime overrides (P1.10).
    config: config::Runtime,
    snapshot: Snapshot,
    js: snapshot::JsWorld,
    skip_log: SkipLog,
    scheduler: Scheduler,
    act: act::Act,
    persist: persist::Persist,
    /// Zero-sized without the `profiler` feature.
    profiler: profiler::Profiler,
    #[cfg(feature = "act-test")]
    act_test: act_test::Script,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Runs once per instance, when the loader instantiates it (wasm-bindgen's start function). A panic
/// then reports its message and a stack through `console.error`, which the loader routes; with
/// `panic = "abort"` the panic still ends in a trap that the loader recovers from (ADR-003). The
/// logger starts at the profile's level; a loaded override changes it a tick or two later.
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    logging::init(config::PROFILE.config.log.level);
}

/// Exported to JS as `loop`; the loader calls it once per tick: the snapshot, then the scheduler's
/// tick (its budget from the snapshot's tick context), in which the persistence loads run and jobs
/// queue intents (P2 onward; the console's `say` until then), then the act step, which issues them
/// in one batch, then the persistence writes and the active segments (`persist::Persist::flush`).
/// The tick runs on the config in force at its start (`cfg`, a copy): a record that loads during
/// the decide step takes effect on the next tick, except for the log level and the flush's limits.
/// A `profiler` build reads the CPU clock around each step (`profiler::Probe`); the flush falls in
/// its `end` span with the fault log. Other builds compile the probe out.
///
/// A JS exception inside the pass ends the call without unwinding, which would leave the state
/// borrowed. The loader treats that as a trap and replaces the instance (ADR-003), so the borrow
/// failing here would mean a loader bug. Then this throws instead of idling silently: the loader
/// logs one incident and replaces the instance. Nothing is borrowed when it throws.
#[wasm_bindgen(js_name = "loop")]
pub fn game_loop() {
    let borrowed = STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => {
                let State {
                    config: runtime,
                    snapshot,
                    js,
                    skip_log,
                    scheduler: sched,
                    act,
                    persist,
                    profiler: prof,
                    #[cfg(feature = "act-test")]
                    act_test,
                } = &mut *state;
                let mut probe = profiler::Probe::start();
                let cfg = *runtime.get();
                snapshot::take(snapshot, js, &cfg.cache);
                snapshot::log_skips(skip_log, snapshot, cfg.log.skip_window_ticks);
                probe.snapshot();
                act.begin(snapshot);
                let mut tick = sched.begin(&snapshot.tick, &cfg.scheduler, scheduler::GameClock);
                // Critical, then Standard, then Optional jobs run here and queue intents (P2 onward).
                // The persistence loads are a Standard job; Critical jobs will run before them (P2).
                persist.load(&mut tick, snapshot.tick.time, runtime);
                act.queue_console_say(snapshot, tick.commit());
                #[cfg(feature = "act-test")]
                act_test.queue(&mut act.buffer, snapshot, tick.commit());
                tick.finish();
                probe.decide();
                // After the decide step: the scheduler reads the CPU clock no more (scheduler docs); the
                // profiler's readings from here on measure the act step outside the budget. The
                // `TickCpu` held `&mut Scheduler` until `finish`, so `act::issue` cannot run earlier.
                act::issue(act, snapshot, js, sched, &cfg.scheduler);
                probe.act();
                let standard = sched
                    .last_tick()
                    .is_some_and(|r| r.budget.allows(Tier::Standard));
                persist.flush(snapshot.tick.time, standard, runtime);
                scheduler::log_faults(sched);
                if prof.finish(probe, &snapshot.tick, act.issues(), &cfg.scheduler) {
                    persist.keys_short();
                }
                true
            }
            Err(_) => false,
        })
        .unwrap_or(false);
    if !borrowed {
        wasm_bindgen::throw_str("rustedscreep: the bot state is unavailable or still borrowed");
    }
}

/// Exported to JS as `snapshotSummary`, for the `rustedscreep.snapshot()` console command: one line
/// with this tick's tick context, table sizes, handle churn and skipped objects.
#[wasm_bindgen(js_name = "snapshotSummary")]
pub fn snapshot_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => snapshot::summary(&state.snapshot, &state.js),
            Err(_) => String::from("rustedscreep snapshot: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep snapshot: state unavailable"))
}

/// Exported to JS as `schedulerSummary`, for the `rustedscreep.scheduler()` console command: one line
/// with the last tick's tier caps, what the scheduler did, and the fault counts.
#[wasm_bindgen(js_name = "schedulerSummary")]
pub fn scheduler_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => scheduler::summary(&state.scheduler, &state.snapshot.tick),
            Err(_) => String::from("rustedscreep scheduler: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep scheduler: state unavailable"))
}

/// Exported to JS as `actSummary`, for the `rustedscreep.act()` console command: one line with the
/// last tick's intent counts (queued, refused, blocked, issued and their results) and totals.
#[wasm_bindgen(js_name = "actSummary")]
pub fn act_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => act::summary(&state.act),
            Err(_) => String::from("rustedscreep act: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep act: state unavailable"))
}

/// Exported to JS as `profilerSummary`, for the `rustedscreep.profiler()` console command: one line
/// with the open window's CPU statistics, the act cost per call, the last loop, the last closed
/// window, the stats-segment writes and the cold loop. A build without the `profiler` feature says so.
#[wasm_bindgen(js_name = "profilerSummary")]
pub fn profiler_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => profiler::summary(
                &state.profiler,
                state.snapshot.tick.time,
                &state.config.get().scheduler,
            ),
            Err(_) => String::from("rustedscreep profiler: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep profiler: state unavailable"))
}

/// Exported to JS as `persistSummary`, for the `rustedscreep.persist()` console command: one line
/// with the active set, wanted loads, dirty records, the counts since boot and the self-test.
#[wasm_bindgen(js_name = "persistSummary")]
pub fn persist_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state.persist.summary(),
            Err(_) => String::from("rustedscreep persist: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep persist: state unavailable"))
}

/// Exported to JS as `persistTest`, for the `rustedscreep.persistTest(bytes)` console command:
/// writes a test frame of `bytes` payload bytes (the largest that fits a segment when omitted) on
/// the next tick and reads it back through the segment manager (the P1 gate's live round trip).
#[wasm_bindgen(js_name = "persistTest")]
pub fn persist_test(bytes: Option<u32>) -> String {
    STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => {
                let now = state.snapshot.tick.time;
                state.persist.request_test(bytes, now)
            }
            Err(_) => String::from("rustedscreep persist self-test: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep persist self-test: state unavailable"))
}

/// Exported to JS as `configSummary`, for the `rustedscreep.config()` console command: one line with
/// the profile, the stored record, the overrides and every overridable key with its value in force.
#[wasm_bindgen(js_name = "configSummary")]
pub fn config_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state.config.summary(state.persist.config_dirty()),
            Err(_) => String::from("rustedscreep config: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep config: state unavailable"))
}

/// A console change to the overrides: `change` runs on the config, and a change that took is
/// written to the config segment on the next tick that runs the Standard tier.
fn config_change(change: impl FnOnce(&mut config::Runtime) -> (String, bool)) -> String {
    STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => {
                let (reply, changed) = change(&mut state.config);
                if changed {
                    state.persist.config_changed();
                }
                reply
            }
            Err(_) => String::from("rustedscreep config: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep config: state unavailable"))
}

/// Exported to JS as `configSet`, for `rustedscreep.configSet(key, value)`: overrides one field from
/// the next tick. A number outside the safe range is clamped; anything else invalid is refused.
#[wasm_bindgen(js_name = "configSet")]
pub fn config_set(key: &str, value: &str) -> String {
    config_change(|c| c.set(key, value))
}

/// Exported to JS as `configUnset`, for `rustedscreep.configUnset(key)`: the profile's value again.
#[wasm_bindgen(js_name = "configUnset")]
pub fn config_unset(key: &str) -> String {
    config_change(|c| c.unset(key))
}

/// Exported to JS as `configReset`, for `rustedscreep.configReset()`: removes every override.
#[wasm_bindgen(js_name = "configReset")]
pub fn config_reset() -> String {
    config_change(config::Runtime::reset)
}

/// Exported to JS as `queueSay`, for the `rustedscreep.say(text)` console command: every own creep
/// says `text` on the next tick (a free intent, V34), which checks the act path in game.
#[wasm_bindgen(js_name = "queueSay")]
pub fn queue_say(text: &str) -> String {
    STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => state.act.request_say(text),
            Err(_) => String::from("rustedscreep say: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep say: state unavailable"))
}

/// Debug export for the loader's trap-recovery test (ADR-003; the P1 gate forces 10 traps in a row).
/// With `npm run build -- --features trap-test` it panics; otherwise it does nothing. It exists in every
/// build so the loader can import it statically, and the loader calls it only in a trap-test build.
#[wasm_bindgen(js_name = "trapTest")]
pub fn trap_test() {
    #[cfg(feature = "trap-test")]
    deliberate_panic();
}

#[cfg(feature = "trap-test")]
#[allow(
    clippy::panic,
    reason = "the trap-test export exists to panic on demand"
)]
fn deliberate_panic() {
    panic!("rustedscreep trap-test: deliberate panic");
}
