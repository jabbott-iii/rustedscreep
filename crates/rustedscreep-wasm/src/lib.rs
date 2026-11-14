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

use wasm_bindgen::prelude::wasm_bindgen;

/// Runs once per instance, when the loader instantiates it (wasm-bindgen's start function). A panic
/// then reports its message and a stack through `console.error`, which the loader routes; with
/// `panic = "abort"` the panic still ends in a trap that the loader recovers from (ADR-003).
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Exported to JS as `loop`; the loader calls it once per tick. It does nothing yet: the tick path
/// lands in P1 modules 5 to 7.
#[wasm_bindgen(js_name = "loop")]
pub fn game_loop() {}

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
