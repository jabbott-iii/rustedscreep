//! Runtime config glue (ROADMAP P1 module 10): the active server profile, the overrides store
//! (`rustedscreep_core::config::overrides`), the console commands behind `rustedscreep.config()`,
//! `configSet(key, value)`, `configUnset(key)` and `configReset()`, and the log level.
//!
//! The record lives in segment `PersistConfig::config_segment` (97 on `mmo`). persist.rs loads it
//! once per global, as a Standard-tier job a tick or two after the global starts, and writes it on
//! the first tick that runs the Standard tier after a change. Until its load completes, the profile's
//! values apply and console changes are refused, so a change cannot overwrite a record not yet read.
//! A load that fails (the segment stayed missing, or its text is not a valid record) is logged, and
//! the next change then writes a new record over whatever the segment holds.
//! To drop every override without the bot (for example from a broken build), clear the segment from
//! the console with `RawMemory.segments[97] = ""`: the next global then finds no record.
//!
//! JS calls per tick: none. The tick reads [`Runtime::get`], a plain struct that changes only when a
//! console command or the load changes it.

use std::fmt::Write as _;

use rustedscreep_core::config::overrides::{Note, Record, Refusal, Store};
use rustedscreep_core::config::{self, Config, FIELDS, Profile};
use rustedscreep_core::persist::{Frame, LoadError};

use crate::logging;

/// The active server profile. The optional tracks select theirs here (PS1, SE2).
pub const PROFILE: &Profile = &config::MMO;

/// The config of one global: the store and its reused note buffer.
pub struct Runtime {
    store: Store,
    notes: Vec<Note>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            store: Store::new(PROFILE.config),
            notes: Vec::new(),
        }
    }
}

impl Runtime {
    /// The config in force.
    #[inline]
    pub fn get(&self) -> &Config {
        self.store.config()
    }

    /// The record's load finished (from persist.rs): applies it, logs what was refused or clamped,
    /// and sets the log level. Returns whether the segment held a usable record or none at all.
    #[cold]
    pub fn loaded(&mut self, segment: u8, frame: Result<Frame<'_>, LoadError>) -> bool {
        let ok = self.store.loaded(frame, &mut self.notes);
        for note in self.notes.drain(..) {
            log::warn!("rustedscreep config: record in segment {segment}: {note}");
        }
        match self.store.record() {
            Record::Loaded { written } => {
                let overrides = self.store.overrides();
                if !overrides.is_empty() && log::log_enabled!(log::Level::Info) {
                    let mut text = format!(
                        "rustedscreep config: {} override(s) from segment {segment} (written at tick {written}):",
                        overrides.len()
                    );
                    for (f, v) in overrides.iter() {
                        let _ = write!(text, " {}={v}", f.key);
                    }
                    log::info!("{text}");
                }
            }
            Record::Failed(e) => log::warn!(
                "rustedscreep config: the overrides in segment {segment} were not used ({e}); the profile's values apply, and the next configSet writes a new record"
            ),
            Record::Empty | Record::Loading | Record::Written { .. } => {}
        }
        logging::set_level(self.get().log.level);
        ok
    }

    /// persist.rs wrote the record on tick `now`.
    pub fn written(&mut self, now: u32) {
        self.store.written(now);
    }

    /// Appends the record's payload, for persist.rs's write.
    pub fn payload(&self, out: &mut Vec<u8>) {
        self.store.payload(out);
    }

    fn refused(r: &Refusal) -> String {
        format!("rustedscreep config: not changed: {r}")
    }

    /// `rustedscreep.configSet(key, value)`. Returns the console reply and whether the overrides
    /// changed (the caller then marks the record for writing).
    pub fn set(&mut self, key: &str, value: &str) -> (String, bool) {
        match self.store.set(key, value) {
            Err(r) => (Self::refused(&r), false),
            Ok(c) => {
                logging::set_level(self.get().log.level);
                let mut text = format!(
                    "rustedscreep config: {} = {} from the next tick (profile {}: {}",
                    c.field.key,
                    c.value,
                    PROFILE.name,
                    (c.field.get)(self.store.base())
                );
                if let Some(old) = c.previous {
                    let _ = write!(text, "; override was {old}");
                }
                text.push(')');
                if let Some(asked) = c.clamped {
                    let _ = write!(
                        text,
                        "; {asked} is outside the safe range {}, so it was clamped",
                        c.field.kind
                    );
                }
                let changed = c.previous != Some(c.value);
                if changed {
                    text.push_str("; stored on the next tick that runs the Standard tier");
                }
                (text, changed)
            }
        }
    }

    /// `rustedscreep.configUnset(key)`.
    pub fn unset(&mut self, key: &str) -> (String, bool) {
        match self.store.unset(key) {
            Err(r) => (Self::refused(&r), false),
            Ok(None) => (format!("rustedscreep config: {key} has no override"), false),
            Ok(Some(old)) => {
                logging::set_level(self.get().log.level);
                let now = config::field(key).map_or_else(String::new, |(_, f)| {
                    format!(" {}", (f.get)(self.store.base()))
                });
                (
                    format!(
                        "rustedscreep config: {key} override {old} removed; the profile's value{now} applies from the next tick"
                    ),
                    true,
                )
            }
        }
    }

    /// `rustedscreep.configReset()`.
    pub fn reset(&mut self) -> (String, bool) {
        match self.store.reset() {
            Err(r) => (Self::refused(&r), false),
            Ok(0) => (
                String::from("rustedscreep config: no overrides to remove"),
                false,
            ),
            Ok(n) => {
                logging::set_level(self.get().log.level);
                (
                    format!(
                        "rustedscreep config: {n} override(s) removed; the profile's values apply from the next tick"
                    ),
                    true,
                )
            }
        }
    }

    /// One line for `rustedscreep.config()`: the profile, the record, the overrides and every key a
    /// runtime override can change, with the value in force. `dirty` says a write is pending.
    pub fn summary(&self, dirty: bool) -> String {
        let segment = PROFILE.config.persist.config_segment;
        let mut s = format!(
            "rustedscreep config: profile {} | record in segment {segment}: ",
            PROFILE.name
        );
        match self.store.record() {
            Record::Loading => s.push_str("loading (changes wait for it)"),
            Record::Empty => s.push_str("none stored"),
            Record::Loaded { written } => {
                let _ = write!(s, "loaded (written at tick {written})");
            }
            Record::Written { written } => {
                let _ = write!(s, "written at tick {written} by this global");
            }
            Record::Failed(e) => {
                let _ = write!(s, "not used ({e})");
            }
        }
        if dirty {
            s.push_str(", write pending");
        }
        let overrides = self.store.overrides();
        let _ = write!(s, " | overrides {}", overrides.len());
        for (i, (f, v)) in overrides.iter().enumerate() {
            let _ = write!(
                s,
                "{} {}={v} (profile {})",
                if i == 0 { ":" } else { "," },
                f.key,
                (f.get)(self.store.base())
            );
        }
        s.push_str(" | in force:");
        for f in FIELDS.iter().filter(|f| f.runtime) {
            let _ = write!(s, " {}={}", f.key, (f.get)(self.get()));
        }
        s
    }
}
