//! The RCL1–3 opening's glue (ROADMAP P2.4, over `rustedscreep_core::economy::Opening`): two
//! scheduler jobs, the watchdogs' log lines and notifications, the early rescan after a room's own
//! sites change, and the `rustedscreep.opening()` console summary.
//!
//! - `opening` (Critical), after movement's first reads and before the `spawn` job: per owned room
//!   the plan, the watchdog, the spawn request, every worker's task, and the intents and moves of
//!   harvesting, taking energy, filling spawns and extensions, and the downgrade guard's upgrade.
//! - `opening work` (Standard), after the persistence loads: building and upgrading.
//! - `opening placement` (Optional, after the constants self-check; ADR-020): per owned room with
//!   a spawn and fewer extensions and extension sites than its level allows, one tile to place an
//!   extension site on (`economy::placement`), unless `opening.place_extensions` is off. The act
//!   step calls `Room.createConstructionSite(x, y, "extension")` for it, after the spawn orders and
//!   before the creep intents: one JS call per site, billed like an intent (V203).
//!
//! JS calls of its own: those site calls, and `Game.notify` on a recovery or a stall (once per kind
//! per global). Everything it reads comes from the snapshot, and its creep intents go through the
//! buffer.

use std::fmt::Write as _;

use rustedscreep_core::config::{MovementConfig, OpeningConfig, SchedulerConfig};
use rustedscreep_core::economy::placement::Placement;
use rustedscreep_core::economy::{Event, Opening, Task};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{ObjectKind, Snapshot};
use rustedscreep_core::spawning::{EnergyBudget, Spawner};
use screeps::{RoomName, StructureType};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// `Game.notify(message)`.
    #[wasm_bindgen(js_namespace = Game, js_name = notify, catch)]
    fn game_notify(message: &str) -> Result<(), JsValue>;

    /// A room, for `createConstructionSite` (engine rooms.js; V200, V203).
    type SiteRoom;

    /// `createConstructionSite(x, y, structureType)`; the type crosses as its string.
    #[wasm_bindgen(method, catch, js_name = createConstructionSite)]
    fn create_site(this: &SiteRoom, x: u8, y: u8, kind: StructureType) -> Result<f64, JsValue>;
}

/// Harvesting and filling the spawns are Critical (CLAUDE.md).
const PLAN_JOB: Job = Job {
    name: "opening",
    tier: Tier::Critical,
};

/// Building and upgrading are Standard.
const WORK_JOB: Job = Job {
    name: "opening work",
    tier: Tier::Standard,
};

/// Planning is Optional (CLAUDE.md): the interim extension placement (ADR-020).
const PLACE_JOB: Job = Job {
    name: "opening placement",
    tier: Tier::Optional,
};

/// The job name the act step's site faults are logged under.
const SITE_JOB: &str = "opening sites";

/// The opening's state for one global.
#[derive(Default)]
pub struct OpeningGlue {
    core: Opening,
    placement: Placement,
    plan_job: JobState,
    work_job: JobState,
    place_job: JobState,
    /// Own sites per owned room on the last tick, to ask movement for an early rescan when the
    /// count changes (a new obstacle site blocks its tile; a finished one becomes a structure).
    sites: Vec<(RoomName, u32)>,
    sites_now: Vec<(RoomName, u32)>,
    /// A tick has run in this global (its first loop has no last tick to compare with, and its
    /// first reads scan every room anyway).
    started: bool,
    /// Whether this global already notified a recovery and a stall.
    notified_recovery: bool,
    notified_stall: bool,
    /// Events since the global started, per kind: recoveries started and ended, guards, wait
    /// fallbacks, stalls.
    counts: [u32; 5],
}

impl OpeningGlue {
    /// Starts the tick: the owned rooms and their workers, and an early rescan of each room whose
    /// own sites changed in number since the last tick.
    pub fn begin(&mut self, snapshot: &Snapshot, movement: &mut Movement) {
        self.core.begin(snapshot);
        self.placement.begin(snapshot);
        self.sites_now.clear();
        let objects = &snapshot.objects;
        for (kind, pos) in objects.kinds().iter().zip(objects.positions()) {
            if !matches!(kind, ObjectKind::Site(_)) {
                continue;
            }
            let room = pos.room_name();
            match self.sites_now.iter_mut().find(|(r, _)| *r == room) {
                Some((_, n)) => *n += 1,
                None => self.sites_now.push((room, 1)),
            }
        }
        for room in self.core.rooms() {
            let count = |list: &[(RoomName, u32)]| {
                list.iter().find(|(r, _)| *r == room).map_or(0, |&(_, n)| n)
            };
            if self.started && count(&self.sites) != count(&self.sites_now) {
                movement.invalidate(room);
            }
        }
        std::mem::swap(&mut self.sites, &mut self.sites_now);
        self.started = true;
    }

    /// The Critical `opening` job: one unit per owned room. Run it after movement's first reads
    /// and before the `spawn` job, which plans the requests it makes.
    #[allow(clippy::too_many_arguments)]
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &OpeningConfig,
        mcfg: &MovementConfig,
    ) {
        let count = self.core.room_count();
        if count == 0 {
            return;
        }
        let Self { core, plan_job, .. } = self;
        tick.run_units(&PLAN_JOB, plan_job, count, |unit, commit| {
            core.plan_room(unit, snapshot, movement, spawner, buffer, cfg, mcfg, commit)
        });
    }

    /// The Standard `opening work` job: one unit per owned room.
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
    ) {
        let count = self.core.room_count();
        if count == 0 {
            return;
        }
        let Self { core, work_job, .. } = self;
        tick.run_units(&WORK_JOB, work_job, count, |unit, commit| {
            core.work_room(unit, snapshot, movement, buffer, mcfg, commit)
        });
    }

    /// The Optional `opening placement` job: one unit per owned room, while
    /// `opening.place_extensions` is on.
    pub fn place<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        cfg: &OpeningConfig,
        mcfg: &MovementConfig,
    ) {
        let count = self.placement.room_count();
        if !cfg.place_extensions || count == 0 {
            return;
        }
        let Self {
            placement,
            place_job,
            ..
        } = self;
        tick.run_units(&PLACE_JOB, place_job, count, |unit, commit| {
            placement.plan_room(unit, snapshot, movement, mcfg, commit)
        });
    }

    /// Calls `createConstructionSite` for this tick's picks and records each outcome (a refused
    /// tile is skipped for the rest of the global). Call in the act step, after the scheduler's
    /// tick has ended. A call that throws is a fault under `opening sites`.
    pub fn issue_sites(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
    ) {
        for index in 0..self.placement.orders().len() {
            let Some(&order) = self.placement.orders().get(index) else {
                break;
            };
            let Some(room) = js.rooms.get(order.room_row) else {
                site_fault(sched, cfg, snapshot, "no JS object for the room row", index);
                log.missing();
                continue;
            };
            let call = room.unchecked_ref::<SiteRoom>().create_site(
                order.tile.x(),
                order.tile.y(),
                order.kind,
            );
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    site_fault(sched, cfg, snapshot, "createConstructionSite threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome != Outcome::Ok {
                let what = match outcome {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    Outcome::Threw => "threw",
                    Outcome::Ok => "ok",
                };
                log::info!(
                    "rustedscreep opening: no extension site at ({}, {}) in {}: {what}; the tile \
                     is skipped",
                    order.tile.x(),
                    order.tile.y(),
                    order.room
                );
            }
            self.placement.record(index, outcome);
        }
    }

    /// Logs what the watchdogs reported this tick. A recovery and a stall also notify, once per
    /// kind per global.
    pub fn log_events(&mut self) {
        let Self {
            core,
            notified_recovery,
            notified_stall,
            counts,
            ..
        } = self;
        for (room, event) in core.drain_events() {
            log_event(room, event, counts, notified_recovery, notified_stall);
        }
    }

    /// One line for `rustedscreep.opening()`: per owned room its workers and tasks, the plan, the
    /// spawn request, the watchdog, and the event counts since the global started.
    pub fn summary(&self, snapshot: &Snapshot) -> String {
        let mut text = format!("rustedscreep opening at tick {}:", snapshot.tick.time);
        let mut any = false;
        for (room, plan, watch, stats) in self.core.room_states() {
            any = true;
            let _ = write!(
                text,
                " {room}: workers {} of {} (working {})",
                stats.alive, stats.want, stats.working
            );
            let mut first = true;
            for (&n, kind) in stats.tasks.iter().zip(Task::KINDS) {
                if n > 0 {
                    let _ = write!(text, "{} {kind} {n}", if first { ", tasks" } else { "," });
                    first = false;
                }
            }
            match stats.request {
                None => text.push_str(", no request"),
                Some((budget, priority)) => {
                    let _ = match budget {
                        EnergyBudget::Capacity => write!(text, ", requesting at capacity"),
                        EnergyBudget::Available => write!(text, ", requesting at available energy"),
                        EnergyBudget::UpTo(cap) => write!(text, ", requesting up to {cap} energy"),
                    };
                    let _ = write!(text, " (priority {priority})");
                }
            }
            match plan {
                None => text.push_str(", no plan yet"),
                Some(p) => {
                    let _ = write!(text, ", plan at capacity {}:", p.capacity);
                    for (i, s) in p.sources.iter().enumerate() {
                        let _ = write!(
                            text,
                            "{} source ({},{}) open {} want {}",
                            if i == 0 { "" } else { "," },
                            s.pos.x().u8(),
                            s.pos.y().u8(),
                            s.open,
                            s.want
                        );
                        if let Some(t) = s.trip {
                            let _ = write!(text, " trip {} tiles cost {}", t.tiles, t.cost);
                        }
                    }
                }
            }
            let _ = write!(
                text,
                ", sites {}, loose energy {}",
                stats.sites, stats.loose
            );
            if let Some(since) = watch.recovering_since() {
                let _ = write!(text, ", RECOVERING since tick {since}");
            }
            if watch.guard() {
                text.push_str(", downgrade guard on");
            }
            if let Some(cap) = watch.cap() {
                let _ = write!(text, ", bodies capped at {cap} energy (it never filled)");
            }
            if watch.stalled() {
                text.push_str(", STALLED");
            }
            text.push(';');
        }
        if !any {
            text.push_str(" no owned room;");
        }
        let [started, recovered, guards, fallbacks, stalls] = self.counts;
        let _ = write!(
            text,
            " since boot: recoveries {started} (ended {recovered}), guards {guards}, wait fallbacks \
             {fallbacks}, stalls {stalls}"
        );
        let p = self.placement.totals();
        let skipped: usize = self.placement.refused().map(|(_, n)| n).sum();
        let _ = write!(
            text,
            " | extension sites: picked {}, placed {}, refused {}, never seen {}, tiles skipped {}",
            p.picked, p.placed, p.refused, p.lost, skipped
        );
        text
    }
}

#[cold]
#[inline(never)]
fn log_event(
    room: RoomName,
    event: Event,
    counts: &mut [u32; 5],
    notified_recovery: &mut bool,
    notified_stall: &mut bool,
) {
    let bump = |counts: &mut [u32; 5], i: usize| {
        if let Some(c) = counts.get_mut(i) {
            *c = c.saturating_add(1);
        }
    };
    match event {
        Event::RecoveryStarted => {
            bump(counts, 0);
            let text = format!(
                "rustedscreep opening: {room} has no worker that can work; spawning at the energy \
                 it holds until opening.min_workers can"
            );
            log::warn!("{text}");
            if !*notified_recovery {
                *notified_recovery = true;
                // A refused notification changes nothing; the warning above stands.
                let _ = game_notify(&text);
            }
        }
        Event::Recovered { ticks } => {
            bump(counts, 1);
            log::info!("rustedscreep opening: {room} recovered after {ticks} ticks");
        }
        Event::GuardOn { downgrade_ticks } => {
            bump(counts, 2);
            log::warn!(
                "rustedscreep opening: {room}'s controller downgrades in {downgrade_ticks} ticks, \
                 below opening.downgrade_guard_ticks; one worker upgrades first"
            );
        }
        Event::GuardOff => {
            log::info!("rustedscreep opening: {room}'s controller is above the downgrade guard");
        }
        Event::WaitFallback { ticks, energy } => {
            bump(counts, 3);
            log::info!(
                "rustedscreep opening: {room}'s worker request waited {ticks} ticks for the full \
                 capacity; its bodies are capped at {energy} energy, the most it held, until it \
                 fills"
            );
        }
        Event::Stalled { ticks } => {
            bump(counts, 4);
            let text = format!(
                "rustedscreep opening: {room} stood still for {ticks} ticks (no controller \
                 progress, no change in spawn energy, no construction); planning it again"
            );
            log::warn!("{text}");
            if !*notified_stall {
                *notified_stall = true;
                let _ = game_notify(&text);
            }
        }
        Event::StallEnded { ticks } => {
            log::info!("rustedscreep opening: {room} moves again, {ticks} ticks after its stall");
        }
    }
}

fn site_fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: SITE_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
