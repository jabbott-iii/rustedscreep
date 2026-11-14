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
mod profiler;
mod scheduler;
mod snapshot;

use std::cell::RefCell;

use rustedscreep_core::config::SCHEDULER_MMO;
use rustedscreep_core::scheduler::Scheduler;
use rustedscreep_core::snapshot::Snapshot;
use wasm_bindgen::prelude::wasm_bindgen;

/// Everything the bot keeps between ticks. It lives in Rust memory only: a global reset (or a
/// re-instantiation by the loader) starts it empty, and the next tick rebuilds it.
#[derive(Default)]
struct State {
    snapshot: Snapshot,
    js: snapshot::JsWorld,
    scheduler: Scheduler,
    act: act::Act,
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
/// `panic = "abort"` the panic still ends in a trap that the loader recovers from (ADR-003).
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Exported to JS as `loop`; the loader calls it once per tick: the snapshot, then the scheduler's
/// tick (its budget from the snapshot's tick context), in which jobs queue intents (P2 onward; the
/// console's `say` until then), then the act step, which issues them in one batch. A `profiler`
/// build reads the CPU clock around each step (`profiler::Probe`); other builds compile it out.
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
                    snapshot,
                    js,
                    scheduler: sched,
                    act,
                    profiler: prof,
                    #[cfg(feature = "act-test")]
                    act_test,
                } = &mut *state;
                let mut probe = profiler::Probe::start();
                snapshot::take(snapshot, js);
                probe.snapshot();
                act.begin(snapshot);
                let mut tick = sched.begin(&snapshot.tick, &SCHEDULER_MMO, scheduler::GameClock);
                // Critical, then Standard, then Optional jobs run here and queue intents (P2 onward).
                act.queue_console_say(snapshot, tick.commit());
                #[cfg(feature = "act-test")]
                act_test.queue(&mut act.buffer, snapshot, tick.commit());
                tick.finish();
                probe.decide();
                // After the decide step: the scheduler reads the CPU clock no more (scheduler docs); the
                // profiler's readings from here on measure the act step outside the budget. The
                // `TickCpu` held `&mut Scheduler` until `finish`, so `act::issue` cannot run earlier.
                act::issue(act, snapshot, js, sched);
                probe.act();
                scheduler::log_faults(sched);
                prof.finish(probe, &snapshot.tick, act.issues());
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
            Ok(state) => profiler::summary(&state.profiler, state.snapshot.tick.time),
            Err(_) => String::from("rustedscreep profiler: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep profiler: state unavailable"))
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
