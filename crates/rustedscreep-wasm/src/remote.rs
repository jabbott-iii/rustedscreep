//! Remote mining's glue (ROADMAP P6.2, ADR-029, over `rustedscreep_core::economy::Remotes`): the
//! scheduler jobs, the remote rooms' sites, and the `rustedscreep.remote()` console summary.
//!
//! - `remote` (Critical, as every job that requests creeps: a request must come before the spawn
//!   job, P2.3), after movement's first reads and before the opening's jobs: per owned home its
//!   candidate rooms, their prices, the rooms it mines, its creeps' sources and the spawn requests.
//!   Remote mining is spare work (SPEC §2's bucket policy), so on a tick the Standard tier does not
//!   run no remote creep is requested and no way is searched there. After it, each home's remote
//!   energy goes to its upgraders' budget (`Upgrading::set_remote`), which the `upgrading` job reads
//!   in the same tick.
//! - `remote work` (Standard), after the opening's Standard jobs: per remote creep its intents and
//!   moves (a remote hauler delivering in its home room is the `hauling` job's).
//! - `remote placement` (Optional), after the opening's placement: per home one way search (a heavy
//!   unit), or the sites of the rooms it mines.
//!
//! JS calls of its own: one `Room.createConstructionSite(x, y, type)` per site, in the act step after
//! the opening's sites and before the creep intents (billed like an intent, V203). Everything it
//! reads comes from the snapshot, and its creep intents go through the buffer.

use std::fmt::Write as _;

use rustedscreep_core::config::{
    HaulingConfig, MovementConfig, RemoteConfig, SchedulerConfig, SpawnConfig,
};
use rustedscreep_core::economy::{AbandonReason, Hauling, RemoteEvent, Remotes, Upgrading};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{Reservation, Snapshot};
use rustedscreep_core::spawning::Spawner;
use screeps::{ErrorCode, StructureType};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

#[wasm_bindgen]
extern "C" {
    /// A creep, for its `owner` as a plain value (V268).
    type OwnerOf;

    #[wasm_bindgen(method, getter = owner)]
    fn owner_value(this: &OwnerOf) -> JsValue;

    /// An owner (`{username}`), read as a plain value.
    type NameOf;

    #[wasm_bindgen(method, getter = username)]
    fn username_value(this: &NameOf) -> JsValue;
}

use crate::opening::SiteRoom;
use crate::snapshot::JsWorld;

/// The choice of rooms and the requests run with the other spawn requests, Critical (P2.3: a
/// request must come before the spawn job; the scheduler runs every Critical job before any
/// Standard one). Its requests and searches wait for the Standard tier.
const PLAN_JOB: Job = Job {
    name: "remote",
    tier: Tier::Critical,
};

/// The remote creeps' harvests, hauls, builds and reserves: spare work, Standard.
const WORK_JOB: Job = Job {
    name: "remote work",
    tier: Tier::Standard,
};

/// The ways and the sites: planning, Optional.
const PLACE_JOB: Job = Job {
    name: "remote placement",
    tier: Tier::Optional,
};

/// The job name the act step's site faults are logged under.
const SITE_JOB: &str = "remote sites";

/// Remote mining's glue state for one global.
#[derive(Default)]
pub struct RemoteGlue {
    core: Remotes,
    plan_job: JobState,
    work_job: JobState,
    place_job: JobState,
    /// Sites placed and refused since the global started.
    placed: u32,
    refused: u32,
}

impl RemoteGlue {
    /// Starts the tick: the homes, the intel of the remote rooms read this tick, and the creeps.
    pub fn begin(&mut self, snapshot: &Snapshot) {
        self.core.begin(snapshot);
    }

    /// The Critical `remote` job, then each home's remote energy for its upgraders' budget.
    #[allow(clippy::too_many_arguments)]
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        hauling: &Hauling,
        upgrading: &mut Upgrading,
        cfg: &RemoteConfig,
        hcfg: &HaulingConfig,
        scfg: &SpawnConfig,
        mcfg: &MovementConfig,
    ) {
        let Self { core, plan_job, .. } = self;
        let homes = core.home_count();
        if homes == 0 {
            return;
        }
        let spare = tick.allows(Tier::Standard);
        tick.run_units(&PLAN_JOB, plan_job, homes, |unit, commit| {
            let hauled = core
                .home_name(unit)
                .is_some_and(|h| hauling.plan(h).is_some());
            core.plan_home(
                unit, snapshot, movement, spawner, cfg, hcfg, scfg, mcfg, hauled, spare, commit,
            )
        });
        for unit in 0..homes {
            if let Some(home) = core.home_name(unit) {
                upgrading.set_remote(home, core.delivered(home));
            }
        }
    }

    /// The Standard `remote work` job: one unit per remote creep.
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &RemoteConfig,
        mcfg: &MovementConfig,
    ) {
        let Self { core, work_job, .. } = self;
        let creeps = core.creep_count();
        if creeps == 0 {
            return;
        }
        tick.run_units(&WORK_JOB, work_job, creeps, |unit, commit| {
            core.work_creep(unit, snapshot, movement, buffer, cfg, mcfg, commit)
        });
    }

    /// The Optional `remote placement` job: one unit per home.
    pub fn place<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        cfg: &RemoteConfig,
        mcfg: &MovementConfig,
    ) {
        let Self {
            core, place_job, ..
        } = self;
        let homes = core.home_count();
        if homes == 0 || !cfg.enabled {
            return;
        }
        tick.run_units(&PLACE_JOB, place_job, homes, |unit, commit| {
            core.place_home(unit, snapshot, movement, cfg, mcfg, commit)
        });
    }

    /// Logs this tick's events (P6.3: threats, their end, abandoned rooms; the P6 gate's live record
    /// of a threat and the bot's response). For a threat it reads each armed hostile's
    /// `owner.username` (2 calls a creep, only on the tick the threat starts; V268, V272).
    pub fn log_events(&self, snapshot: &Snapshot, js: &JsWorld) {
        for event in self.core.events() {
            match *event {
                RemoteEvent::Threatened {
                    home,
                    room,
                    armed,
                    until,
                } => {
                    // The owner reads are for the line alone.
                    if !log::log_enabled!(log::Level::Warn) {
                        continue;
                    }
                    let creeps = &snapshot.creeps;
                    let (mut attack, mut ranged) = (0u32, 0u32);
                    let mut owners: Vec<String> = Vec::new();
                    for (row, ((&my, p), c)) in creeps
                        .my()
                        .iter()
                        .zip(creeps.positions())
                        .zip(creeps.combat())
                        .enumerate()
                    {
                        if my || p.room_name() != room || !c.armed() {
                            continue;
                        }
                        attack = attack.saturating_add(c.attack);
                        ranged = ranged.saturating_add(c.ranged_attack);
                        let owner = js.creeps.get(row).and_then(|creep| {
                            let o = creep.unchecked_ref::<OwnerOf>().owner_value();
                            o.is_object()
                                .then(|| o.unchecked_ref::<NameOf>().username_value().as_string())
                                .flatten()
                        });
                        let name = owner.unwrap_or_else(|| String::from("unknown"));
                        if !owners.contains(&name) {
                            owners.push(name);
                        }
                    }
                    log::warn!(
                        "rustedscreep remote: {room} (of {home}) threatened by {armed} armed hostile \
                         creep(s) of {} ({attack} ATTACK, {ranged} RANGED_ATTACK parts); its creeps \
                         wait in {home} until tick {until}",
                        owners.join(", ")
                    );
                }
                RemoteEvent::Resumed { home, room } => {
                    log::info!(
                        "rustedscreep remote: {room} (of {home}): the wait is over, its creeps go back"
                    );
                }
                RemoteEvent::Abandoned {
                    home,
                    room,
                    reason,
                    until,
                } => {
                    let then = match reason {
                        AbandonReason::Unprofitable => "its creeps work on until they die",
                        AbandonReason::Contested | AbandonReason::Threats => {
                            "its haulers bring their loads home and its other creeps end"
                        }
                    };
                    log::warn!(
                        "rustedscreep remote: {room} (of {home}) abandoned ({}) until tick {until}; {then}",
                        reason.as_str()
                    );
                }
            }
        }
    }

    /// Calls `createConstructionSite` for this tick's remote sites and records each outcome. Call
    /// in the act step, after the opening's sites and before the creep intents. A refused tile is
    /// skipped for the rest of the global and logged at info; a call that throws, or a site whose
    /// room has no JS object, is a fault under `remote sites`.
    pub fn issue_sites(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
    ) {
        for index in 0..self.core.sites().len() {
            let Some(&site) = self.core.sites().get(index) else {
                break;
            };
            let Some(room) = snapshot
                .rooms
                .row_of(site.room)
                .and_then(|row| js.rooms.get(row))
            else {
                site_fault(sched, cfg, snapshot, "no JS object for the room", index);
                log.missing();
                continue;
            };
            let call = room.unchecked_ref::<SiteRoom>().create_site(
                site.tile.x(),
                site.tile.y(),
                site.kind,
            );
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    site_fault(sched, cfg, snapshot, "createConstructionSite threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome == Outcome::Ok {
                self.placed = self.placed.saturating_add(1);
                self.core.site_placed(site);
                continue;
            }
            self.refused = self.refused.saturating_add(1);
            let what = match outcome {
                Outcome::Failed(code) => error_name(code),
                Outcome::Unknown => "unknown return",
                Outcome::Threw => "threw",
                Outcome::Ok => "ok",
            };
            let kind = if site.kind == StructureType::Road {
                "road"
            } else {
                "container"
            };
            // At MAX_CONSTRUCTION_SITES (V200) the tile is offered again: the cap passes.
            let full = outcome == Outcome::Failed(ErrorCode::Full);
            let then = if full {
                "it is offered again"
            } else {
                "the tile is skipped"
            };
            log::info!(
                "rustedscreep remote: no {kind} site at ({}, {}) in {}: {what}; {then}",
                site.tile.x(),
                site.tile.y(),
                site.room
            );
            if !full {
                self.core.site_refused(site);
            }
        }
    }

    /// One line for `rustedscreep.remote()`: per home each candidate, what the bot knows of it,
    /// its plan and figures, its creeps, and the sites placed since boot.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &RemoteConfig) -> String {
        let now = snapshot.tick.time;
        let mut text = format!(
            "rustedscreep remote at tick {now}: {} (max rooms {}, spawn {} per mille) | sites placed {}, refused {}",
            if cfg.enabled { "on" } else { "off" },
            cfg.max_rooms,
            cfg.spawn_permille,
            self.placed,
            self.refused
        );
        for (home, remotes) in self.core.home_states() {
            let _ = write!(text, " | {home}:");
            if remotes.is_empty() {
                text.push_str(" no candidates yet");
            }
            for r in remotes {
                let _ = write!(text, " {}", r.room);
                match self.core.intel(r.room) {
                    None => text.push_str(" unseen"),
                    Some(i) => {
                        let _ = write!(
                            text,
                            " seen {} ago ({} sources{})",
                            now.saturating_sub(i.seen),
                            i.sources.len(),
                            if i.minable() { "" } else { ", not minable" }
                        );
                    }
                }
                if r.active {
                    text.push_str(" mined");
                }
                if let Some(t) = r.threat.filter(|t| t.until > now) {
                    let _ = write!(text, " threatened ({} armed) until {}", t.armed, t.until);
                }
                if let Some(a) = r.abandoned {
                    let _ = write!(text, " abandoned ({}) until {}", a.reason.as_str(), a.until);
                }
                if let Some(p) = r.profit {
                    let per_tick = |x: i64| x / i64::from(rustedscreep_consts::CREEP_LIFE_TIME);
                    let _ = write!(
                        text,
                        " [{}{}, net {}/tick, cpu {}.{:02}, spawn {} per mille]",
                        if p.plan.reserve {
                            "reserve"
                        } else {
                            "no reserve"
                        },
                        if p.plan.roads { ", roads" } else { "" },
                        per_tick(p.net),
                        p.cpu_ucpu / u64::from(rustedscreep_consts::CREEP_LIFE_TIME) / 1_000_000,
                        p.cpu_ucpu / u64::from(rustedscreep_consts::CREEP_LIFE_TIME) % 1_000_000
                            / 10_000,
                        p.spawn_permille()
                    );
                } else if r.ways.is_some() {
                    text.push_str(" [no plan pays]");
                }
                let s = r.stats;
                if s.scouts + s.reservers + s.miners + s.haulers + s.requested > 0 {
                    let _ = write!(
                        text,
                        " creeps s{} r{} m{} h{} (asked {})",
                        s.scouts, s.reservers, s.miners, s.haulers, s.requested
                    );
                }
                match s.reservation {
                    Reservation::Mine { ticks } => {
                        let _ = write!(text, " reserved {ticks}");
                    }
                    Reservation::Other { ticks } => {
                        let _ = write!(text, " reserved by another {ticks}");
                    }
                    Reservation::None => {}
                }
                if s.hostiles > 0 {
                    let _ = write!(text, " hostiles {}", s.hostiles);
                }
                text.push(';');
            }
        }
        text
    }
}

/// A fault of the act step's remote sites (`SITE_JOB`), rate-limited like a job's.
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
