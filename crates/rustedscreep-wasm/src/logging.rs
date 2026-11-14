//! Logging (ROADMAP P1 module 10, ADR-014): `log` macros, a minimal logger that prints through
//! `console.log`, and the level from config.
//!
//! - [`init`] installs [`Console`] once per instance, from the start function, with
//!   `log::set_logger` on a static (no allocation; a second call keeps the first logger, V127,
//!   V128). The level from config is `log::set_max_level`: every `log` macro checks it with one
//!   relaxed atomic load before it builds its arguments, so a filtered line costs no formatting (the
//!   few lines first built in a `String`, all rate-limited and cold, check `log::log_enabled!` first
//!   when they are below warn). [`set_level`] changes it when an override changes `log.level`. The
//!   lowest level is warn, so warnings and errors always print.
//! - The logger prints the message text only: lines read as they did before P1.10, and each starts
//!   with `rustedscreep`. A message without arguments is printed without allocating.
//! - Nothing in this module logs, and [`Console::log`] calls only `console.log`, so the logger never
//!   re-enters itself.
//!
//! ADR-014 replaced fern (SPEC §1, ADR-006) with this logger: the bot used one fern output, and
//! fern cost 47140 bytes of release wasm (V126).
//!
//! Levels used: `warn!` for faults, failed JS calls, refused records and skipped objects; `info!`
//! for expected events (the self-test's verdicts when they pass, the profiler's cold loop, loaded
//! overrides).

use log::{LevelFilter, Log, Metadata, Record};
use rustedscreep_core::config::LogLevel;
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    /// `console.log`, looked up on every call: Screeps builds a new console before every tick (V96).
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(message: &str);
}

/// The logger: every record that passes `log::max_level()` goes to `console.log`.
struct Console;

impl Log for Console {
    /// The macros never call this (V127); `log_enabled!` does, after the max-level check, so every
    /// level that passes that check is enabled.
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        match record.args().as_str() {
            Some(text) => console_log(text),
            None => console_log(&record.args().to_string()),
        }
    }

    fn flush(&self) {}
}

static CONSOLE: Console = Console;

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
    if log::set_logger(&CONSOLE).is_err() {
        console_log("rustedscreep: a logger was already installed in this instance; keeping it");
    }
    set_level(level);
}

/// Sets the most verbose level printed.
pub fn set_level(level: LogLevel) {
    log::set_max_level(filter(level));
}
