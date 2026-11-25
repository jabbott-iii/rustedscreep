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
//! No intents.

use std::fmt::Write as _;

use rustedscreep_core::config::{ClaimConfig, PlannerConfig, RemoteConfig, ScoutConfig};
use rustedscreep_core::expansion::claim::SHOWN;
use rustedscreep_core::expansion::{Candidate, ClaimEvent, ClaimHome, Claims, Scouting};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::planner::flood::Params;
use rustedscreep_core::scheduler::{Clock, Job, JobState, Step, TickCpu, Tier};
use rustedscreep_core::snapshot::RoomStatus;

use crate::route::{Status, read_status};
use crate::sign::read_terrain;

/// Claim scoring: Optional.
const CLAIM_JOB: Job = Job {
    name: "claims",
    tier: Tier::Optional,
};

/// The claim glue's state for one global.
#[derive(Default)]
pub struct ClaimGlue {
    core: Claims,
    job: JobState,
    /// The homes and their statuses, and the tick the statuses were read.
    homes: Vec<ClaimHome>,
    statuses_at: Option<u32>,
    /// `Game.map.getRoomTerrain` reads since the global started, and the ones that failed.
    terrain_reads: u32,
    terrain_failures: u32,
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
    /// Starts the tick: no events yet.
    pub fn begin(&mut self) {
        self.core.begin();
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
    pub fn log_events(&self) {
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
        for (i, c) in list.iter().take(SHOWN).enumerate() {
            let _ = write!(text, " | {}. {}", i + 1, describe(c));
        }
        text
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
