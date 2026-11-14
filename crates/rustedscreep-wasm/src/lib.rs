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

mod snapshot;

use std::cell::RefCell;

use rustedscreep_core::snapshot::Snapshot;
use wasm_bindgen::prelude::wasm_bindgen;

/// Everything the bot keeps between ticks. It lives in Rust memory only: a global reset (or a
/// re-instantiation by the loader) starts it empty, and the next tick rebuilds it.
#[derive(Default)]
struct State {
    snapshot: Snapshot,
    js: snapshot::JsWorld,
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

/// Exported to JS as `loop`; the loader calls it once per tick. So far it takes the snapshot; the
/// scheduler and act layer land in P1 modules 6 and 7.
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
                let State { snapshot, js } = &mut *state;
                snapshot::take(snapshot, js);
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
