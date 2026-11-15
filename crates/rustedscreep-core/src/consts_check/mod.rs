//! The constants self-check (ROADMAP P1 module 11): once per global, compare what
//! `rustedscreep_consts` (screeps-game-api 0.23.5's constants and functions) says every game constant
//! holds with what the server's global object holds, and report each difference.
//!
//! The server installs every constant as an own data property of the player's global object, a JSON
//! deep copy of the engine's constants.js made at the start of every tick (VERIFICATION_LOG, P1.11).
//! So JSON text is the constants' own form: the wasm side ([`Globals`]) hands over the global's own
//! property names, then for each upper-case one the `JSON.stringify` text of its data value, and this
//! module compares the parsed values. All of it is pure and runs natively on the engine's own
//! constants (the fixture `tests/fixtures/constants-common-2.16.0.tsv`).
//!
//! What the check compares (`expect`):
//! - scalars by name (`CREEP_LIFE_TIME`, `TOWER_FALLOFF`, `OK`, `TOP`, ...): integers exactly, the
//!   crate's `f32` constants by rounding the server's double to `f32`, strings exactly;
//! - string-enum families (`STRUCTURE_*`, `RESOURCE_*`, ...) as sets of values, and the list constants
//!   (`RESOURCES_ALL`, `BODYPARTS_ALL`, ...) as sets;
//! - tables (`BODYPART_COST`, `CONTROLLER_STRUCTURES`, `REACTIONS`, `BOOSTS`, `COMMODITIES`, ...)
//!   entry by entry over the crate's key domain, plus any server key the crate does not have.
//!
//! Every server constant is then either read by a check, or on the documented list of names the crate
//! does not mirror (with the reason), or reported as unknown: a constant the server added.
//!
//! The check is a step job ([`Check::step`]): one step reads the names, one step per constant reads
//! its value, one step per comparison group compares, so the scheduler's Optional cap slices it.
//! CPU: about five JS calls per constant (the engine has 422) and one parse of its text, once per
//! global; nothing per tick once it is done.

mod expect;
pub mod json;
pub mod report;
pub mod server;
#[cfg(test)]
mod tests;

pub use expect::{KNOWN, Known};
pub use report::{Finding, FindingKind, MAX_FINDINGS, Report, Want};

use crate::scheduler::{Fault, Step};
use json::Json;
use server::{Server, is_constant_name};

/// Constants every Screeps server defines; a global with none of them holds no game constants.
const ANCHORS: [&str; 3] = ["OK", "WORK", "CREEP_LIFE_TIME"];

/// The server side of the check: the global object, as the wasm crate reads it.
pub trait Globals {
    /// The JSON text of the global object's own property names (`Object.getOwnPropertyNames`),
    /// whose positions [`Self::value`] takes. `Ok(None)`: `JSON.stringify` gave no text.
    fn names(&mut self) -> Result<Option<String>, &'static str>;

    /// The JSON text of the data value of the own property at `index` of that list. `Ok(None)` when
    /// `JSON.stringify` gave no text (an accessor, whose getter is never called, a function,
    /// undefined or a symbol). `Err` when a JS call threw; the check then fails for this global.
    fn value(&mut self, index: u32) -> Result<Option<String>, &'static str>;
}

/// Where the check is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No step has run yet.
    Waiting,
    /// Reading values: `read` of `of` constants so far.
    Reading {
        read: u32,
        of: u32,
    },
    /// Comparing: `group` of `of` comparison groups done.
    Comparing {
        group: u32,
        of: u32,
    },
    Done,
    /// A JS call threw or the names could not be read; the check does not retry in this global.
    Failed(&'static str),
}

/// One global's check.
#[derive(Debug)]
pub struct Check {
    phase: Phase,
    /// The upper-case names to read, with their positions in the names list.
    wanted: Vec<(u32, String)>,
    server: Server,
    report: Report,
}

impl Default for Check {
    fn default() -> Self {
        Self {
            phase: Phase::Waiting,
            wanted: Vec::new(),
            server: Server::default(),
            report: Report::default(),
        }
    }
}

impl Check {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether the check is over (done or failed).
    pub fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Done | Phase::Failed(_))
    }

    /// The report, once the check is done.
    pub fn report(&self) -> Option<&Report> {
        match self.phase {
            Phase::Done => Some(&self.report),
            _ => None,
        }
    }

    /// Runs one step: read the names, read one value, or run one comparison group. `Done` once the
    /// check is over; a JS failure ends it with a [`Fault`] (whose unit is the step's position) that
    /// the scheduler logs, and later calls return `Done` at once.
    pub fn step<G: Globals>(&mut self, globals: &mut G) -> Result<Step, Fault> {
        match self.phase {
            Phase::Waiting => self.read_names(globals),
            Phase::Reading { read, of } => self.read_value(globals, read, of),
            Phase::Comparing { group, of } => Ok(self.compare(group, of)),
            Phase::Done | Phase::Failed(_) => Ok(Step::Done),
        }
    }

    fn fail(&mut self, what: &'static str, unit: u32) -> Result<Step, Fault> {
        self.phase = Phase::Failed(what);
        self.wanted = Vec::new();
        self.server = Server::default();
        Err(Fault::new(what, unit))
    }

    fn read_names<G: Globals>(&mut self, globals: &mut G) -> Result<Step, Fault> {
        const NOT_NAMES: &str = "the global's own names are not a JSON array of strings";
        let text = match globals.names() {
            Ok(Some(t)) => t,
            Ok(None) => return self.fail(NOT_NAMES, 0),
            Err(what) => return self.fail(what, 0),
        };
        let Ok(Json::Arr(names)) = json::parse(&text) else {
            return self.fail(NOT_NAMES, 0);
        };
        for (index, name) in (0u32..).zip(names) {
            match name {
                Json::Str(name) if is_constant_name(&name) => self.wanted.push((index, name)),
                Json::Str(_) => {}
                _ => return self.fail(NOT_NAMES, 0),
            }
        }
        let of = u32::try_from(self.wanted.len()).unwrap_or(u32::MAX);
        self.phase = Phase::Reading { read: 0, of };
        Ok(Step::More)
    }

    fn read_value<G: Globals>(
        &mut self,
        globals: &mut G,
        read: u32,
        of: u32,
    ) -> Result<Step, Fault> {
        if read >= of {
            self.server.seal();
            self.wanted = Vec::new();
            // A global without these holds no game constants (a read path that sees none, or not a
            // Screeps runtime): one failure, not a finding for every constant.
            if !ANCHORS.iter().any(|name| self.server.has(name)) {
                return self.fail("the global object holds no game constants", 0);
            }
            self.phase = Phase::Comparing {
                group: 0,
                of: expect::group_count(),
            };
            return Ok(Step::More);
        }
        let Some((index, name)) = usize::try_from(read)
            .ok()
            .and_then(|i| self.wanted.get_mut(i))
            .map(|(index, name)| (*index, core::mem::take(name)))
        else {
            return self.fail("the names list changed while it was read", read);
        };
        match globals.value(index) {
            Ok(text) => self.server.push(name, text),
            Err(what) => return self.fail(what, index),
        }
        self.phase = Phase::Reading {
            read: read.saturating_add(1),
            of,
        };
        Ok(Step::More)
    }

    fn compare(&mut self, group: u32, of: u32) -> Step {
        if group < of {
            expect::run_group(group, &self.server, &mut self.report);
            self.phase = Phase::Comparing {
                group: group.saturating_add(1),
                of,
            };
            return Step::More;
        }
        expect::finish(&self.server, &mut self.report);
        self.report.server_names = u32::try_from(self.server.len()).unwrap_or(u32::MAX);
        self.server = Server::default();
        self.phase = Phase::Done;
        Step::Done
    }
}
