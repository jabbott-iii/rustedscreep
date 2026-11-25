//! Scouting's glue (ROADMAP P8.1, ADR-039, over `rustedscreep_core::expansion::Scouting`): its
//! three scheduler jobs, the observers' calls, the atlas's load and save, its log lines, and the
//! `rustedscreep.scout()` console summary.
//!
//! - `scout` (Critical, as every job that requests creeps: a request must come before the spawn
//!   job, P2.3): the explorer's request, made only on a tick that runs the Optional tier, with no
//!   explorer alive and a room due.
//! - `scout work` (Optional), after the signer's work: the extra explorers' ends, and the live
//!   explorer's trip and move (at most one route search a tick, which makes the unit heavy).
//! - `observe` (Optional), right after it: each own observer's look at the stalest due room in
//!   range (no JS call). The looks go out in the act step, after the towers' calls: one
//!   `observeRoom(name)` per look, billed when OK (V358, V360); the room is visible on the next
//!   tick only (V362), and that tick's snapshot reads it.
//!
//! JS calls of its own: the observers' calls. The atlas's reads come from the snapshot
//! (`push_intel`), and the explorer's moves and suicide go through the intent buffer.

use std::fmt::Write as _;

use js_sys::JsString;
use rustedscreep_core::config::{MovementConfig, SchedulerConfig, ScoutConfig};
use rustedscreep_core::expansion::{self, Reserved, ScoutEvent, Scouting};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::persist::frame::{DecodeError, Frame};
use rustedscreep_core::persist::{LoadError, kind};
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::signing::RecordLoad;
use rustedscreep_core::snapshot::{Controller, Snapshot};
use rustedscreep_core::spawning::Spawner;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// An observer, for `observeRoom` (engine game/structures.js, V358). Bound here: the crate's
    /// `observe_room` unwraps an unknown return code (V366).
    type ObserverActor;

    /// `observeRoom(roomName)`: OK, or ERR_NOT_OWNER, ERR_INVALID_ARGS, ERR_RCL_NOT_ENOUGH,
    /// ERR_NOT_IN_RANGE (V358); an OK call is billed (V360).
    #[wasm_bindgen(method, catch, js_name = observeRoom)]
    fn observe_room(this: &ObserverActor, room: &JsString) -> Result<f64, JsValue>;
}

/// The request runs with the other spawn requests, Critical; it waits for the Optional tier.
const PLAN_JOB: Job = Job {
    name: "scout",
    tier: Tier::Critical,
};

/// The explorer's trips: scouting, Optional.
const WORK_JOB: Job = Job {
    name: "scout work",
    tier: Tier::Optional,
};

/// The observers' looks: scouting, Optional.
const OBSERVE_JOB: Job = Job {
    name: "observe",
    tier: Tier::Optional,
};

/// The fault name of a failed observer call.
const OBSERVE_CALLS: &str = "observer calls";

/// The scouting glue's state for one global.
#[derive(Default)]
pub struct ScoutGlue {
    core: Scouting,
    plan_job: JobState,
    work_job: JobState,
    observe_job: JobState,
    /// Observer calls refused since the global started, and the tick the last was logged.
    refused: u32,
    refused_logged: Option<u32>,
}

impl ScoutGlue {
    pub fn core(&self) -> &Scouting {
        &self.core
    }

    /// Marks `room`'s atlas entry due now (P8.3: a claim the game refused).
    pub fn expire(&mut self, room: screeps::RoomName, cfg: &ScoutConfig) {
        self.core.expire(room, cfg);
    }

    /// Starts the tick: the homes, the explorers, and this tick's reads (no JS call).
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &ScoutConfig) {
        self.core.begin(snapshot, cfg);
    }

    /// Whether the atlas should be loaded now (a home has reached `scout.min_level`).
    pub fn wants_record(&self, cfg: &ScoutConfig) -> bool {
        self.core.wants_record(cfg)
    }

    /// No segment keeps the atlas: start at once, and never write.
    pub fn no_record(&mut self) {
        self.core.no_record();
    }

    /// The Critical `scout` job: the explorer's request (not run without one to make).
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        spawner: &mut Spawner,
        cfg: &ScoutConfig,
    ) {
        let optional = tick.allows(Tier::Optional);
        let Self { core, plan_job, .. } = self;
        // With no request to make the job is not run at all, so an idle scout leaves no trace in
        // the scheduler (no clock read, no skip counted).
        if !(optional && core.wants_request(cfg)) {
            return;
        }
        tick.run_units(&PLAN_JOB, plan_job, 1, |_, _| {
            core.request(spawner, cfg, optional);
            Ok(())
        });
    }

    /// The Optional `scout work` job, then the `observe` job: one unit each, and neither run with
    /// nothing to do.
    #[allow(clippy::too_many_arguments)]
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &ScoutConfig,
        mcfg: &MovementConfig,
    ) {
        let Self {
            core,
            work_job,
            observe_job,
            ..
        } = self;
        if core.has_work(cfg) {
            // A trip choice's route search marks the unit heavy itself (`Movement::search_route`).
            tick.run_units(&WORK_JOB, work_job, 1, |_, commit| {
                core.work(snapshot, movement, buffer, cfg, mcfg, commit)
            });
        }
        let observers = cfg.enabled
            && cfg.observers
            && core.is_loaded()
            && snapshot
                .structures
                .kinds()
                .contains(&screeps::StructureType::Observer);
        if observers {
            tick.run_units(&OBSERVE_JOB, observe_job, 1, |_, commit| {
                core.observe(snapshot, cfg, commit);
                Ok(())
            });
        }
    }

    /// Calls `observeRoom` for this tick's looks and records each outcome. Call in the act step,
    /// after the towers' calls. A refused call is counted and logged at info at most once per
    /// `scout.refresh_ticks`; a call that throws, or a look with no JS object, is a fault under
    /// `observer calls`.
    pub fn issue_looks(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        scfg: &ScoutConfig,
    ) {
        for (index, look) in self.core.looks().iter().enumerate() {
            let Some(o) = usize::try_from(look.row)
                .ok()
                .and_then(|r| js.structures.get(r))
            else {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for an observer look",
                    index,
                );
                log.missing();
                continue;
            };
            let outcome = match o
                .unchecked_ref::<ObserverActor>()
                .observe_room(&JsString::from(look.room))
            {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    fault(sched, cfg, snapshot, "an observer call threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome == Outcome::Ok {
                continue;
            }
            self.refused = self.refused.saturating_add(1);
            let now = snapshot.tick.time;
            let due = self
                .refused_logged
                .is_none_or(|t| now.saturating_sub(t) >= scfg.refresh_ticks);
            if due {
                self.refused_logged = Some(now);
                log::info!(
                    "rustedscreep scout: an observer's look at {} was refused at tick {now}: {} ({} \
                     refused since the global started)",
                    look.room,
                    outcome_name(outcome),
                    self.refused
                );
            }
        }
    }

    /// Whether the atlas should be written: it is then counted as saved, and the caller marks its
    /// segment dirty.
    pub fn take_save(&mut self, cfg: &ScoutConfig) -> bool {
        self.core.take_save(cfg)
    }

    /// The atlas's payload, for the persistence writes: at most `max_len` bytes. Returns how many
    /// rooms it left out.
    pub fn payload(&self, out: &mut Vec<u8>, max_len: usize) -> usize {
        self.core.payload(out, max_len)
    }

    /// The atlas's load finished. Returns whether it was accepted (an empty segment is).
    pub fn loaded(&mut self, segment: u8, frame: Result<Frame<'_>, LoadError>) -> bool {
        let frame = frame.and_then(|f| {
            f.header
                .expect(kind::ATLAS, expansion::SCHEMA)
                .map_err(LoadError::Decode)
                .map(|()| f)
        });
        let load = match &frame {
            Ok(f) => RecordLoad::Payload(f.payload),
            Err(LoadError::Decode(DecodeError::Empty)) => RecordLoad::Empty,
            Err(LoadError::Unavailable { .. }) => RecordLoad::Unavailable,
            Err(_) => RecordLoad::Refused,
        };
        let result = self.core.loaded(load);
        match (&frame, result) {
            (Ok(f), Ok(())) => {
                log::info!(
                    "rustedscreep scout: atlas loaded from segment {segment} ({} rooms, written at {})",
                    self.core.atlas().len(),
                    f.header.written
                );
                true
            }
            (Ok(_), Err(why)) => {
                log::warn!(
                    "rustedscreep scout: the atlas in segment {segment} was not used ({why:?}); \
                     scouting starts again and the atlas is written again"
                );
                false
            }
            (Err(LoadError::Decode(DecodeError::Empty)), _) => true,
            (Err(e @ LoadError::Unavailable { .. }), _) => {
                log::warn!(
                    "rustedscreep scout: the atlas in segment {segment} was not read ({e}); \
                     scouting starts again and the segment is left for the next global"
                );
                false
            }
            (Err(e), _) => {
                log::warn!(
                    "rustedscreep scout: the atlas in segment {segment} was not used ({e}); \
                     scouting starts again and the atlas is written again"
                );
                false
            }
        }
    }

    /// The tick's events, as log lines: a room's first read or a change of who holds it at info, a
    /// refresh that found it the same at debug.
    pub fn log_events(&self, cfg: &ScoutConfig) {
        let core = &self.core;
        for &e in core.events() {
            match e {
                ScoutEvent::Read {
                    room,
                    first,
                    changed,
                } => {
                    let level = if first || changed {
                        log::Level::Info
                    } else {
                        log::Level::Debug
                    };
                    if log::log_enabled!(level) {
                        let what = describe(core, room);
                        let how = if first {
                            "read"
                        } else if changed {
                            "changed"
                        } else {
                            "refreshed"
                        };
                        log::log!(level, "rustedscreep scout: {room} {how}: {what}");
                    }
                }
                ScoutEvent::Unseen { room } => log::info!(
                    "rustedscreep scout: an observer's look at {room} gave no vision; it waits {} \
                     ticks",
                    cfg.refresh_ticks
                ),
                ScoutEvent::Unreachable { room } => log::info!(
                    "rustedscreep scout: no route to {room}; it waits {} ticks",
                    cfg.refresh_ticks
                ),
                ScoutEvent::Retreated { room, towers } => log::info!(
                    "rustedscreep scout: {towers} tower(s) in {room}; the explorer stepped back out"
                ),
                ScoutEvent::Died { room } => {
                    log::warn!("rustedscreep scout: the explorer was lost in {room}")
                }
                ScoutEvent::Done { room } => log::info!(
                    "rustedscreep scout: nothing left in reach from {room}; the explorer ends"
                ),
                ScoutEvent::Pruned { rooms } => log::info!(
                    "rustedscreep scout: {rooms} room(s) out of every home's reach left the atlas"
                ),
            }
        }
    }

    /// One line for `rustedscreep.scout()`: the atlas, the rooms due, the explorer and its trip,
    /// this tick's looks, and the counts since the global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &ScoutConfig) -> String {
        let core = &self.core;
        let now = snapshot.tick.time;
        let atlas = core.atlas();
        let known = atlas.entries().iter().filter(|e| e.known).count();
        let unreachable = atlas.entries().iter().filter(|e| e.unreachable).count();
        let towered = atlas.entries().iter().filter(|e| e.towered()).count();
        let mut text = format!(
            "rustedscreep scout at tick {now}: {} (observers {}) | atlas {} rooms ({known} read, \
             {unreachable} unreachable, {towered} towered), {}",
            if cfg.enabled { "on" } else { "off" },
            if cfg.observers { "on" } else { "off" },
            atlas.len(),
            match (core.is_loaded(), core.is_dirty()) {
                (false, _) => "loading",
                (true, true) => "loaded, changes not written",
                (true, false) => "loaded",
            }
        );
        let homes: Vec<String> = core.homes().iter().map(|h| h.room.to_string()).collect();
        let (due, next) = core.due_count(cfg);
        let _ = write!(text, " | homes [{}] | {due} due", homes.join(", "));
        if due == 0 && next != u32::MAX {
            let _ = write!(text, ", next at {next}");
        }
        match core.explorer() {
            None => text.push_str(" | no explorer"),
            Some(e) => {
                let _ = write!(
                    text,
                    " | explorer in {} ({}, {}){}",
                    e.at.room_name(),
                    e.at.x().u8(),
                    e.at.y().u8(),
                    if e.spawning { " spawning" } else { "" }
                );
                if let Some(ttl) = e.ttl {
                    let _ = write!(text, ", ttl {ttl}");
                }
                match e.trip {
                    Some(t) => {
                        let _ = write!(text, ", to {}", t.room);
                    }
                    None => text.push_str(", no trip"),
                }
            }
        }
        if !core.looks().is_empty() {
            let looks: Vec<String> = core.looks().iter().map(|l| l.room.to_string()).collect();
            let _ = write!(text, " | looks [{}]", looks.join(", "));
        }
        let st = core.stats();
        let _ = write!(
            text,
            " | since the global: reads {}, searches {}, unreachable {}, lost {}, retreats {}, \
             explorers {}, looks {} (refused {})",
            st.reads,
            st.searches,
            st.unreachable,
            st.deaths,
            st.retreats,
            st.explorers,
            st.looks,
            self.refused
        );
        text
    }
}

/// A room's atlas entry in a few words, for the log.
fn describe(core: &Scouting, room: screeps::RoomName) -> String {
    let Some(e) = core.atlas().get(room) else {
        return String::from("no entry");
    };
    let owner = match e.controller {
        Controller::Absent => String::from("no controller"),
        Controller::Neutral => String::from("neutral"),
        Controller::Mine { level } => format!("ours at RCL{level}"),
        Controller::Foreign { level } => format!("another player's at RCL{level}"),
    };
    let reserved = match e.reserved {
        Reserved::None => "",
        Reserved::Mine { .. } => ", reserved by us",
        Reserved::Other { .. } => ", reserved by another",
    };
    let mut text = format!("{owner}{reserved}, {} source(s), ", e.source_count());
    match e.mineral {
        Some(d) => {
            let _ = write!(text, "mineral {}", d.mineral.as_str());
        }
        None => text.push_str("no mineral"),
    }
    if e.towers > 0 {
        let _ = write!(text, ", {} tower(s)", e.towers);
    }
    if e.lairs > 0 {
        let _ = write!(text, ", {} keeper lair(s)", e.lairs);
    }
    if let Some(level) = e.core {
        let _ = write!(text, ", an invader core at level {level}");
    }
    let status = match e.status {
        rustedscreep_core::snapshot::RoomStatus::Normal => None,
        rustedscreep_core::snapshot::RoomStatus::Closed => Some("closed"),
        rustedscreep_core::snapshot::RoomStatus::Novice => Some("a novice area"),
        rustedscreep_core::snapshot::RoomStatus::Respawn => Some("a respawn area"),
    };
    if let Some(s) = status {
        let _ = write!(text, ", {s}");
    }
    text
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Failed(code) => error_name(code),
        Outcome::Unknown => "unknown return",
        Outcome::Threw => "threw",
        Outcome::Ok => "ok",
    }
}

fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: OBSERVE_CALLS,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
