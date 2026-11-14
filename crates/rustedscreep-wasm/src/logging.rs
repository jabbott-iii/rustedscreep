//! Logging (ROADMAP P1 module 10, ADR-006): `log` macros, a `fern` dispatch with one output that
//! prints through `console.log`, and the level from config.
//!
//! - [`init`] installs the dispatch once per instance, from the start function. Fern's own level
//!   stays at Trace, and the level from config is `log::set_max_level` (V126, V127): every `log`
//!   macro checks it with one relaxed atomic load before it builds its arguments, so a filtered line
//!   costs no formatting (the few lines first built in a `String`, all rate-limited and cold, check
//!   `log::log_enabled!` first when they are below warn). [`set_level`] changes it when an override
//!   changes `log.level`. The lowest level is warn, so warnings and errors always print.
//! - The output prints the message text only: lines read as they did before P1.10, and each starts
//!   with `rustedscreep`. A message without arguments is printed without allocating.
//! - Nothing in this module logs, and the output closure calls only `console.log`, so the logger
//!   never re-enters itself.
//!
//! Levels used: `warn!` for faults, failed JS calls, refused records and skipped objects; `info!`
//! for expected events (the self-test's verdicts when they pass, the profiler's cold loop, loaded
//! overrides).

use log::LevelFilter;
use rustedscreep_core::config::LogLevel;
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(message: &str);
}

/// The `log` filter for a config level.
const fn filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    }
}

/// Installs the logger with `level`. Once per instance: `log` keeps one logger per program, and a
/// second call keeps the first logger (only the level changes).
pub fn init(level: LogLevel) {
    let installed = fern::Dispatch::new()
        .level(LevelFilter::Trace)
        .chain(fern::Output::call(|record| match record.args().as_str() {
            Some(text) => console_log(text),
            None => console_log(&record.args().to_string()),
        }))
        .apply();
    if installed.is_err() {
        console_log("rustedscreep: a logger was already installed in this instance; keeping it");
    }
    // `apply` sets the max level to fern's own (Trace); the config's level replaces it.
    set_level(level);
}

/// Sets the most verbose level printed.
pub fn set_level(level: LogLevel) {
    log::set_max_level(filter(level));
}
