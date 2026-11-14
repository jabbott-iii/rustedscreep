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

/// Exported to JS as `loop`; the loader calls it once per tick. It does nothing yet: it gives the
/// build pipeline (P1 module 3) a real export until the tick path lands (P1 modules 5 to 7).
#[wasm_bindgen(js_name = "loop")]
pub fn game_loop() {}
