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
mod consts_check;
mod logging;
mod movement;
mod opening;
mod persist;
mod profiler;
mod route;
mod scheduler;
mod snapshot;
mod spawn;

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
    /// Movement (P2.1): grids, fields, detours, traffic, the console's move order.
    movement: movement::MovementGlue,
    /// The minimal spawn path (P2.3): requests, the plan, the orders and the console's request.
    spawn: spawn::SpawnGlue,
    /// The RCL1–3 opening (P2.4): workers, their spawn requests, recovery and the watchdogs.
    opening: opening::OpeningGlue,
    persist: persist::Persist,
    /// The constants self-check, once per global (P1.11).
    consts: consts_check::ConstsCheck,
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
                    movement,
                    spawn,
                    opening,
                    persist,
                    consts,
                    profiler: prof,
                    #[cfg(feature = "act-test")]
                    act_test,
                } = &mut *state;
                let mut probe = profiler::Probe::start();
                let cfg = *runtime.get();
                snapshot::take(snapshot, js, &cfg.cache);
                // Movement's tick starts with the snapshot (no JS call); its room reads are jobs.
                movement.begin(snapshot, &cfg.movement, &cfg.cache);
                snapshot::log_skips(skip_log, snapshot, cfg.log.skip_window_ticks);
                probe.snapshot();
                act.begin(snapshot);
                spawn.begin(snapshot);
                opening.begin(snapshot, &mut movement.core);
                let mut tick = sched.begin(&snapshot.tick, &cfg.scheduler, scheduler::GameClock);
                // Critical: the rooms movement needs before anything in them can move, first.
                movement.read_first(&mut tick, snapshot, js, &cfg.movement);
                // Critical: the static miners (P3.1), the opening's workers and their spawn requests
                // (P2.4), then the spawn plan, after the Critical jobs that request creeps (P2.3).
                opening.plan(
                    &mut tick,
                    snapshot,
                    &mut movement.core,
                    spawn.spawner_mut(),
                    &mut act.buffer,
                    &cfg.opening,
                    &cfg.mining,
                    &cfg.movement,
                );
                spawn.plan(&mut tick, snapshot);
                // Standard: the persistence loads, then the opening's building and upgrading.
                persist.load(&mut tick, snapshot.tick.time, runtime);
                opening.work(
                    &mut tick,
                    snapshot,
                    &mut movement.core,
                    &mut act.buffer,
                    &cfg.movement,
                );
                // Standard: the periodic room rescans, then the console's move order.
                movement.rescan(&mut tick, snapshot, js, &cfg.movement);
                movement.run_console(&mut tick, snapshot, &cfg.movement);
                // The constants self-check is an Optional job, after every Standard one. It logs with
                // the config in force after the loads (on a global's second loop the record loads in
                // this same step, and the check usually completes in it).
                consts.run(&mut tick, snapshot.tick.time, &runtime.get().log);
                // Optional: the interim container and extension placement (ADR-023, ADR-020),
                // after the self-check.
                opening.place(
                    &mut tick,
                    snapshot,
                    &mut movement.core,
                    &cfg.opening,
                    &cfg.mining,
                    &cfg.movement,
                );
                act.queue_console_say(snapshot, tick.commit());
                #[cfg(feature = "act-test")]
                act_test.queue(&mut act.buffer, snapshot, tick.commit());
                // After every job: the traffic manager turns the tick's move requests into moves.
                movement.resolve(&mut tick, snapshot, &mut act.buffer, &cfg.movement);
                tick.finish();
                opening.log_events();
                probe.decide();
                // After the decide step: the scheduler reads the CPU clock no more (scheduler docs); the
                // profiler's readings from here on measure the act step outside the budget. The
                // `TickCpu` held `&mut Scheduler` until `finish`, so `act::issue` cannot run earlier.
                // Spawn orders go first, so the processor charges them before any creep intent.
                spawn.issue(
                    snapshot,
                    js,
                    act.log_mut(),
                    sched,
                    &cfg.scheduler,
                    &cfg.spawn,
                );
                // Then the sites the placement picked (ADR-020, ADR-023), then the creep intents.
                opening.issue_sites(snapshot, js, act.log_mut(), sched, &cfg.scheduler);
                act::issue(act, snapshot, js, sched, &cfg.scheduler);
                movement.after_act(snapshot, act.refused_moves(), &cfg.movement);
                probe.act();
                // The naive guard (ADR-022), whatever the tier: it may end a naive span, whose record
                // the flush below writes when the tick runs the Standard tier.
                if runtime.naive_tick(snapshot.tick.time, snapshot.tick.bucket, cfg.cache.naive()) {
                    persist.config_changed();
                }
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

/// Exported to JS as `movementSummary`, for the `rustedscreep.movement()` console command: one line
/// with the rooms, grid versions, cached fields, this tick's and the global's movement counters, and
/// the console's move order.
#[wasm_bindgen(js_name = "movementSummary")]
pub fn movement_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state
                .movement
                .summary(&state.snapshot, &state.config.get().movement),
            Err(_) => String::from("rustedscreep movement: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep movement: state unavailable"))
}

/// Exported to JS as `moveTo`, for `rustedscreep.moveTo(room, x, y, range, from)`: from the next tick,
/// every own creep in `from` (`room` when omitted) without a request of its own walks to within
/// `range` (1 when omitted) of (x, y) in `room`, across rooms if need be (P2.2), until all of them
/// are in range on one tick. With no room it cancels the order.
#[wasm_bindgen(js_name = "moveTo")]
pub fn move_to(
    room: Option<String>,
    x: Option<u32>,
    y: Option<u32>,
    range: Option<u32>,
    from: Option<String>,
) -> String {
    STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => {
                state
                    .movement
                    .set_order(room.as_deref(), (x, y, range), from.as_deref())
            }
            Err(_) => String::from("rustedscreep moveTo: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep moveTo: state unavailable"))
}

/// Exported to JS as `spawnerSummary`, for the `rustedscreep.spawner()` console command: one line
/// with each spawn room's energy and spawns, the last tick's requests and orders, totals since boot,
/// the last spawn and failure, the console's request and the own creeps by home and role.
#[wasm_bindgen(js_name = "spawnerSummary")]
pub fn spawner_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state.spawn.summary(&state.snapshot),
            Err(_) => String::from("rustedscreep spawner: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep spawner: state unavailable"))
}

/// Exported to JS as `spawn`, for `rustedscreep.spawn(room, role)`: from the next tick, a spawn of
/// `room` spawns one creep of `role` (worker when omitted) sized to the room's capacity, once the
/// room holds its cost (P2.3). With no room it cancels the request.
#[wasm_bindgen(js_name = "spawn")]
pub fn spawn_request(room: Option<String>, role: Option<String>) -> String {
    STATE
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut state) => state.spawn.set_console(room.as_deref(), role.as_deref()),
            Err(_) => String::from("rustedscreep spawn: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep spawn: state unavailable"))
}

/// Exported to JS as `openingSummary`, for the `rustedscreep.opening()` console command: one line
/// with each owned room's workers and tasks, its plan, its spawn request and its watchdog (P2.4).
#[wasm_bindgen(js_name = "openingSummary")]
pub fn opening_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state.opening.summary(&state.snapshot),
            Err(_) => String::from("rustedscreep opening: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep opening: state unavailable"))
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

/// Exported to JS as `constsSummary`, for the `rustedscreep.consts()` console command: one line with
/// the constants self-check's phase and, once it is done, its counts, findings and known differences.
#[wasm_bindgen(js_name = "constsSummary")]
pub fn consts_summary() -> String {
    STATE
        .try_with(|cell| match cell.try_borrow() {
            Ok(state) => state.consts.summary(),
            Err(_) => String::from("rustedscreep consts: state busy"),
        })
        .unwrap_or_else(|_| String::from("rustedscreep consts: state unavailable"))
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
