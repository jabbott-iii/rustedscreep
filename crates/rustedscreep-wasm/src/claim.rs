//! Claim scoring's glue (ROADMAP P8.2, ADR-041, over `rustedscreep_core::expansion::Claims`): its
//! Optional `claims` job, the candidates' terrain reads, its log lines, and the
//! `rustedscreep.claims()` console summary.
//!
//! The `claims` job runs after scouting's work, and only while there is something to do: the atlas
//! holds a room, and a score is due or a plan is wanted or running. Each of its steps is one piece
//! of work, marked heavy so the slicer reads the clock after it:
//! - the ranking rebuilt when the atlas's facts or the owned rooms change, and at least every
//!   `claim.score_ticks`; each home's status (`Game.map.getRoomStatus`, V168, one call a home) is
//!   read again only when the homes change or `claim.score_ticks` has passed since its read;
//! - a candidate's terrain, from movement's cache or through `Game.map.getRoomTerrain`, which needs
//!   no vision (V156; 5 calls, V157), to start its plan;
//! - one phase of the plan, or its validation.
//!
//! The claimer (P8.3, ADR-042, over `rustedscreep_core::expansion::Claiming`):
//! - `claimer` (Critical, before the spawn plan, only on a tick that runs the Optional tier): the
//!   claimer's request; `Game.gcl.level` is read in it (3 reads through `Reflect`) only when every
//!   other check passes and the level was not read this global or `claim.score_ticks` ago.
//! - `claimer work` (Standard): the claimer's route search (heavy, once a global), its move, its
//!   `claimController` call next to the controller, and its `suicide` once done.
//! - In the Optional tier, after the `claims` job: the owned rooms with no spawn and no spawn site
//!   are handed to claim scoring to plan first, and the first spawn's site is picked from the plan.
//!   The act step calls `Room.createConstructionSite(x, y, "spawn")` for it (one call, billed on
//!   OK, V203; a spawn site without a name gets one, V200).

use std::fmt::Write as _;

use js_sys::Reflect;
use rustedscreep_core::config::{
    ClaimConfig, MovementConfig, PlannerConfig, RemoteConfig, SchedulerConfig, ScoutConfig,
    SpawnConfig,
};
use rustedscreep_core::expansion::claim::SHOWN;
use rustedscreep_core::expansion::{
    Candidate, ClaimEvent, ClaimHome, ClaimerEvent, Claiming, Claims, Scouting, SitePick,
};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::planner::flood::Params;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, Step, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{RoomStatus, Snapshot};
use rustedscreep_core::spawning::Spawner;
use screeps::ErrorCode;
use wasm_bindgen::{JsCast, JsValue};

use crate::consts_check::GLOBAL_THIS;
use crate::opening::SiteRoom;
use crate::route::{Status, read_status};
use crate::sign::read_terrain;
use crate::snapshot::JsWorld;

/// Claim scoring: Optional.
const CLAIM_JOB: Job = Job {
    name: "claims",
    tier: Tier::Optional,
};

/// The claimer's request runs with the other spawn requests, Critical; it waits for the Optional
/// tier.
const CLAIMER_JOB: Job = Job {
    name: "claimer",
    tier: Tier::Critical,
};

/// The claimer's walk and claim: Standard.
const CLAIMER_WORK_JOB: Job = Job {
    name: "claimer work",
    tier: Tier::Standard,
};

/// The fault name of a failed spawn site call.
const SITE_JOB: &str = "claim sites";

/// The claim glue's state for one global.
#[derive(Default)]
pub struct ClaimGlue {
    core: Claims,
    job: JobState,
    claiming: Claiming,
    claimer_job: JobState,
    claimer_work_job: JobState,
    /// `Game.gcl.level` reads since the global started.
    gcl_reads: u32,
    /// The homes and their statuses, and the tick the statuses were read.
    homes: Vec<ClaimHome>,
    statuses_at: Option<u32>,
    /// `Game.map.getRoomTerrain` reads since the global started, and the ones that failed.
    terrain_reads: u32,
    terrain_failures: u32,
    /// Spawn site calls refused since the global started.
    sites_refused: u32,
}

/// A room's status for the filters; one that could not be read, or is unknown, counts as closed.
const fn status_of(s: Option<Status>) -> RoomStatus {
    match s {
        Some(Status::Normal) => RoomStatus::Normal,
        Some(Status::Novice) => RoomStatus::Novice,
        Some(Status::Respawn) => RoomStatus::Respawn,
        Some(Status::Closed | Status::Other) | None => RoomStatus::Closed,
    }
}

/// `v` me/t as energy a tick with three decimals.
fn energy(v: i64) -> String {
    let a = v.unsigned_abs();
    format!(
        "{}{}.{:03}",
        if v < 0 { "-" } else { "" },
        a / 1000,
        a % 1000
    )
}

impl ClaimGlue {
    /// Starts the tick: no events yet; the owned rooms and the claimer from the snapshot.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &ClaimConfig) {
        self.core.begin();
        self.claiming.begin(snapshot, cfg);
    }

    /// The rooms whose atlas entry is marked stale this tick (a refused claim).
    pub fn expired(&self) -> &[screeps::RoomName] {
        self.claiming.expired()
    }

    /// The Critical `claimer` job: the GCL read when due, and the claimer's request (not run with
    /// neither to do, or on a tick without the Optional tier).
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        spawner: &mut Spawner,
        cfg: &ClaimConfig,
        spawn: &SpawnConfig,
    ) {
        let optional = tick.allows(Tier::Optional);
        let Self {
            core,
            claiming,
            claimer_job,
            gcl_reads,
            ..
        } = self;
        let range = spawn.help_range;
        if !(optional
            && (claiming.gcl_due(core, cfg, range) || claiming.wants_request(core, cfg, range)))
        {
            return;
        }
        tick.run_units(&CLAIMER_JOB, claimer_job, 1, |_, _| {
            if claiming.gcl_due(core, cfg, range) {
                *gcl_reads = gcl_reads.saturating_add(1);
                // An unreadable level allows no claim until the next read.
                claiming.set_gcl(read_gcl().unwrap_or(0));
            }
            claiming.request(spawner, core, cfg, range, optional);
            Ok(())
        });
    }

    /// The Standard `claimer work` job (one unit; not run with no claimer to work or end). A route
    /// search marks the unit heavy itself (`Movement::search_route`).
    #[allow(clippy::too_many_arguments)]
    pub fn work_claimer<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        scout: &Scouting,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &ClaimConfig,
        scout_cfg: &ScoutConfig,
        mcfg: &MovementConfig,
    ) {
        let Self {
            claiming,
            claimer_work_job,
            ..
        } = self;
        if !claiming.has_work() {
            return;
        }
        tick.run_units(&CLAIMER_WORK_JOB, claimer_work_job, 1, |_, commit| {
            claiming.work(
                snapshot,
                scout.atlas(),
                movement,
                buffer,
                cfg,
                scout_cfg,
                mcfg,
                commit,
            )
        });
    }

    /// Calls `createConstructionSite` for this tick's spawn sites and records each outcome. Call in
    /// the act step, after the remote sites and before the creep intents. A refused tile is not
    /// tried again this global (at the site cap, `ERR_FULL`, it is offered again) and logged at
    /// info; a call that throws, or a site whose room has no JS object, is a fault under
    /// `claim sites`.
    pub fn issue_sites(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
    ) {
        for index in 0..self.claiming.sites().len() {
            let Some(&pick) = self.claiming.sites().get(index) else {
                break;
            };
            let Some(room) = snapshot
                .rooms
                .row_of(pick.room)
                .and_then(|row| js.rooms.get(row))
            else {
                site_fault(sched, cfg, snapshot, "no JS object for the room", index);
                log.missing();
                continue;
            };
            let call = room.unchecked_ref::<SiteRoom>().create_site(
                pick.tile.x(),
                pick.tile.y(),
                SitePick::KIND,
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
                self.claiming.site_placed(pick);
                // The act step comes after the tick's log pass, so the line goes out here.
                log::info!(
                    "rustedscreep claims: {}'s first spawn site placed at ({}, {})",
                    pick.room,
                    pick.tile.x(),
                    pick.tile.y()
                );
                continue;
            }
            self.sites_refused = self.sites_refused.saturating_add(1);
            let what = match outcome {
                Outcome::Failed(code) => error_name(code),
                Outcome::Unknown => "unknown return",
                Outcome::Threw => "threw",
                Outcome::Ok => "ok",
            };
            // At MAX_CONSTRUCTION_SITES (V200) the tile is offered again: the cap passes.
            let full = outcome == Outcome::Failed(ErrorCode::Full);
            log::info!(
                "rustedscreep claims: no spawn site at ({}, {}) in {}: {what}; {}",
                pick.tile.x(),
                pick.tile.y(),
                pick.room,
                if full {
                    "it is offered again"
                } else {
                    "the tile is not tried again this global"
                }
            );
            self.claiming.site_refused(pick, full);
        }
    }

    /// The Optional `claims` job (module docs).
    #[allow(clippy::too_many_arguments)]
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        scout: &Scouting,
        movement: &Movement,
        now: u32,
        cfg: &ClaimConfig,
        scout_cfg: &ScoutConfig,
        remote: &RemoteConfig,
        planner: &PlannerConfig,
    ) {
        let params = Params::from_config(planner);
        self.core.set_params(params);
        let rooms = remote.max_rooms;
        // P8.3: the owned rooms waiting for their first spawn site are planned first, and their
        // site is picked once their plan is in, on every tick that runs the Optional tier.
        if tick.allows(Tier::Optional) && scout.is_loaded() {
            self.core
                .set_sites(self.claiming.site_rooms(cfg), scout.atlas());
            self.claiming.pick_sites(&self.core, cfg);
        }
        if !scout_cfg.enabled
            || !scout.is_loaded()
            || (scout.atlas().is_empty() && self.core.candidates().is_empty())
            || !self
                .core
                .has_work(now, scout.revision(), scout.owned(), rooms, cfg)
        {
            return;
        }
        let Self {
            core,
            job,
            homes,
            statuses_at,
            terrain_reads,
            terrain_failures,
            ..
        } = self;
        tick.run_steps(&CLAIM_JOB, job, |_, commit| {
            commit.heavy();
            let atlas = scout.atlas();
            let owned = scout.owned();
            let revision = scout.revision();
            if core.score_due(now, revision, owned, rooms, cfg) {
                let same = homes.len() == scout.homes().len()
                    && homes
                        .iter()
                        .zip(scout.homes())
                        .all(|(a, b)| a.room == b.room);
                let fresh = statuses_at.is_some_and(|t| now < t.saturating_add(cfg.score_ticks));
                if !(same && fresh) {
                    homes.clear();
                    homes.extend(scout.homes().iter().map(|h| ClaimHome {
                        room: h.room,
                        status: status_of(read_status(h.room)),
                    }));
                    *statuses_at = Some(now);
                }
                core.score(atlas, revision, homes, owned, now, rooms, cfg);
            } else if core.planning().is_some() {
                core.step(params, atlas, owned, cfg);
            } else if let Some(room) = core.wanted(cfg) {
                let grid = match movement.room_terrain(room) {
                    Some(t) => Some(t.clone()),
                    None => {
                        *terrain_reads = terrain_reads.saturating_add(1);
                        let grid = read_terrain(room);
                        if grid.is_none() {
                            *terrain_failures = terrain_failures.saturating_add(1);
                        }
                        grid
                    }
                };
                core.feed_terrain(room, grid, atlas);
            }
            Ok(if core.has_work(now, revision, owned, rooms, cfg) {
                Step::More
            } else {
                Step::Done
            })
        });
    }

    /// This tick's log lines.
    pub fn log_events(&self, cfg: &ClaimConfig) {
        for &e in self.claiming.events() {
            match e {
                ClaimerEvent::Sent { home, room } => {
                    log::info!("rustedscreep claims: a claimer from {home} goes to claim {room}");
                }
                ClaimerEvent::Claimed { room } => {
                    log::info!(
                        "rustedscreep claims: claimed {room}; its pioneers come from the nearest \
                         spawn room, and its first spawn site follows from its plan"
                    );
                }
                ClaimerEvent::Failed { room, why } => {
                    log::warn!(
                        "rustedscreep claims: the claim of {room} failed ({}); it waits {} ticks",
                        why.as_str(),
                        cfg.retry_ticks
                    );
                }
                ClaimerEvent::NoSite { room, why } => {
                    log::warn!(
                        "rustedscreep claims: {room} has no plan to place its first spawn from \
                         ({why})"
                    );
                }
            }
        }
        for &e in self.core.events() {
            match e {
                ClaimEvent::Best {
                    room: Some(room), ..
                } => {
                    if let Some(c) = self.core.best().filter(|c| c.room == room) {
                        log::info!("rustedscreep claims: best {}", describe(c));
                    }
                }
                ClaimEvent::Best { room: None, .. } => {
                    log::info!("rustedscreep claims: no planned candidate");
                }
                ClaimEvent::Unfit { room, why } => {
                    log::info!("rustedscreep claims: {room} has no valid base plan ({why})");
                }
                ClaimEvent::NoTerrain { room } => {
                    log::warn!(
                        "rustedscreep claims: {room}'s terrain could not be read; it is left out \
                         until the next timed score"
                    );
                }
            }
        }
    }

    /// The console summary (`rustedscreep.claims()`): the counts, the reasons rooms were left out,
    /// the plan running, and the first [`SHOWN`] candidates.
    pub fn summary(&self, cfg: &ClaimConfig, scout: &Scouting) -> String {
        let mut text = String::from("rustedscreep claims:");
        if !cfg.enabled {
            text.push_str(" off");
            return text;
        }
        if !scout.is_loaded() {
            text.push_str(" waiting for the atlas");
            return text;
        }
        let core = &self.core;
        let list = core.candidates();
        let planned = list.iter().filter(|c| c.plan.is_some()).count();
        let _ = write!(text, " {} candidates ({planned} planned)", list.len());
        let mut first = true;
        for (why, n) in core.skipped().filter(|&(_, n)| n > 0) {
            let _ = write!(
                text,
                "{} {} {n}",
                if first { "; left out:" } else { "," },
                why.as_str()
            );
            first = false;
        }
        if let Some((room, phase)) = core.planning() {
            let _ = write!(text, "; planning {room} ({phase})");
        }
        match core.best() {
            Some(c) => {
                let _ = write!(text, "; best {} at {} e/t", c.room, energy(c.value()));
            }
            None => text.push_str("; no best yet"),
        }
        let s = core.stats();
        let _ = write!(
            text,
            "; scores {}, plans {} ({} fit), steps {}, terrain reads {} ({} failed)",
            s.scores, s.plans, s.fits, s.steps, self.terrain_reads, self.terrain_failures
        );
        self.claimer_summary(&mut text, cfg, scout);
        for (i, c) in list.iter().take(SHOWN).enumerate() {
            let _ = write!(text, " | {}. {}", i + 1, describe(c));
        }
        text
    }
}

impl ClaimGlue {
    /// The claimer's part of the console summary (P8.3): the GCL, the claimer, the rooms waiting
    /// after a failed claim, the rooms waiting for their first spawn site, and the counts.
    fn claimer_summary(&self, text: &mut String, cfg: &ClaimConfig, scout: &Scouting) {
        if !cfg.claimers {
            text.push_str("; claimers off");
            return;
        }
        let c = &self.claiming;
        let owned = scout.owned().len();
        match c.gcl() {
            Some(0) => {
                let _ = write!(text, "; GCL unreadable, {owned} owned");
            }
            Some(level) => {
                let _ = write!(text, "; GCL {level} for {owned} owned");
            }
            None => {
                let _ = write!(text, "; GCL not read, {owned} owned");
            }
        }
        match c.claimer() {
            Some(k) => {
                let what = if k.spawning {
                    "spawning"
                } else if k.ending {
                    "ending"
                } else if k.refused.is_some() {
                    "refused, waiting for the room's read"
                } else if k.claimed {
                    "claimed"
                } else if k.called.is_some() {
                    "claiming"
                } else {
                    "walking"
                };
                let _ = write!(
                    text,
                    "; claimer for {} from {}: {what}, at {}",
                    k.target, k.home, k.at
                );
                if let Some(t) = k.ttl {
                    let _ = write!(text, ", {t} ticks left");
                }
            }
            None => text.push_str("; no claimer"),
        }
        let mut first = true;
        for &(room, until) in c.failed() {
            let _ = write!(
                text,
                "{} {room} until {until}",
                if first {
                    "; waiting after a failed claim:"
                } else {
                    ","
                }
            );
            first = false;
        }
        let mut first = true;
        for room in c.site_rooms(cfg) {
            let _ = write!(
                text,
                "{} {room}",
                if first {
                    "; waiting for a spawn site:"
                } else {
                    ","
                }
            );
            first = false;
        }
        let k = c.stats();
        let _ = write!(
            text,
            "; claimers {}, claims {}, failed {}, spawn sites {} ({} refused), route searches {}, \
             GCL reads {}",
            k.claimers,
            k.claims,
            k.failures,
            k.sites,
            self.sites_refused,
            k.searches,
            self.gcl_reads
        );
    }
}

/// One candidate for the log and the console.
fn describe(c: &Candidate) -> String {
    let t = &c.terms;
    let mut text = format!(
        "{} from {}, {} room{}, {} source{}: {} e/t (sources {}, remotes {}, distance {}, foreign \
         {}, walks {}, ramparts {})",
        c.room,
        c.home,
        c.distance,
        if c.distance == 1 { "" } else { "s" },
        c.sources,
        if c.sources == 1 { "" } else { "s" },
        energy(c.value()),
        energy(t.sources),
        energy(t.remotes),
        energy(t.distance),
        energy(t.foreign),
        energy(t.walks),
        energy(t.ramparts)
    );
    match c.plan {
        Some(f) => {
            let _ = write!(
                text,
                ", first spawn ({}, {}), {} ramparts",
                f.spawn.x(),
                f.spawn.y(),
                f.ramparts
            );
        }
        None => text.push_str(", not planned"),
    }
    text
}

/// `Game.gcl.level` (V379: `Game.gcl` is `{level, progress, progressTotal}`), through `Reflect` so a
/// world without `Game.gcl` (a sandbox) reads `None` instead of throwing: 3 JS reads.
fn read_gcl() -> Option<u32> {
    let global = GLOBAL_THIS.with(Option::clone)?;
    let game = Reflect::get(&global, &JsValue::from_str("Game")).ok()?;
    let gcl = Reflect::get(&game, &JsValue::from_str("gcl")).ok()?;
    let level = Reflect::get(&gcl, &JsValue::from_str("level"))
        .ok()?
        .as_f64()?;
    // A level is a whole number of at least 1; `as` saturates and maps NaN to 0.
    (level >= 1.0).then_some(level as u32)
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
