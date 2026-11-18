//! The base planner's glue (ROADMAP P4.3, over `rustedscreep_core::planner::job::Planner`): the
//! Optional `planner` job, the plan each room's placement reads (`economy::placement::PlanView`),
//! the per-phase CPU timings, the plan's log lines and the `rustedscreep.planner()` console
//! summary.
//!
//! - `planner` (Optional, before `opening placement`): one step a unit, each marked heavy (a phase
//!   of `planner::flood` or the plan's validation: the minimum cut, the largest, took 8.5 ms
//!   natively at worst on the corpus, ADR-007), so the slicer reads the clock after each and stops
//!   at the tier's cap; a room's plan spreads over the ticks the cap allows. Nothing runs while no
//!   room is planning (a room is planned once a global, ADR-007, and again only when a spawn the
//!   plan did not know of stands off its spawn tiles; a lost anchor keeps the plan,
//!   `planner::job`).
//!
//! JS calls of its own: two `Game.cpu.getUsed()` per step for the timings: nine steps per anchor
//! tried (eight phases and the validation), one anchor in a room with one spawn and no spawn site,
//! and at most `allowed(Spawn, 8)` (3) after a reset at RCL7 or later: all of them when the room
//! holds something off its plan, usually one when it holds nothing off it (the room's first spawn
//! is usually tried first, `planner::job`). Everything else comes from the snapshot and movement's
//! caches.

use std::fmt::Write as _;

use rustedscreep_core::config::PlannerConfig;
use rustedscreep_core::economy::placement::{Placement, PlanView};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::planner::flood::Params;
use rustedscreep_core::planner::job::{Planner, Status};
use rustedscreep_core::scheduler::{Clock, Job, JobState, TickCpu, Tier};
use rustedscreep_core::snapshot::Snapshot;
use screeps::RoomName;

use crate::scheduler::GameClock;

/// Planning is Optional (CLAUDE.md).
const PLANNER_JOB: Job = Job {
    name: "planner",
    tier: Tier::Optional,
};

/// CPU per phase since the global started: steps, total, most. Unit: CPU.
#[derive(Clone, Copy, Debug)]
struct Timing {
    phase: &'static str,
    steps: u32,
    total: f64,
    most: f64,
}

#[derive(Default)]
pub struct PlannerGlue {
    core: Planner,
    job: JobState,
    timings: Vec<Timing>,
    /// Rooms whose plan (or failure) was logged, so each is logged once a plan.
    logged: Vec<RoomName>,
}

impl PlannerGlue {
    /// Starts the tick: the owned rooms to plan, while `planner.enabled` is on.
    pub fn begin(&mut self, snapshot: &Snapshot, movement: &Movement, cfg: &PlannerConfig) {
        if cfg.enabled {
            self.core.begin(snapshot, movement);
        }
    }

    /// The Optional `planner` job: steps while a room is planning, then one log line per room whose
    /// plan came out (or failed).
    pub fn run<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, cfg: &PlannerConfig) {
        if !cfg.enabled {
            return;
        }
        if !self.core.busy() {
            // A room found unplannable when first seen fails without a step: log it here.
            self.log_outcomes();
            return;
        }
        let params = Params::from_config(cfg);
        let Self {
            core, job, timings, ..
        } = self;
        tick.run_steps(&PLANNER_JOB, job, |_, commit| {
            commit.heavy();
            let phase = core.next_phase().unwrap_or("none");
            let before = GameClock.used();
            let step = core.step(params);
            let cost = GameClock.used() - before;
            if cost.is_finite() && cost >= 0.0 {
                match timings.iter_mut().find(|t| t.phase == phase) {
                    Some(t) => {
                        t.steps = t.steps.saturating_add(1);
                        t.total += cost;
                        t.most = t.most.max(cost);
                    }
                    None => timings.push(Timing {
                        phase,
                        steps: 1,
                        total: cost,
                        most: cost,
                    }),
                }
            }
            Ok(step)
        });
        self.log_outcomes();
    }

    fn log_outcomes(&mut self) {
        for (room, status) in self.core.rooms() {
            match status {
                Status::Ready(plan) if !self.logged.contains(&room) => {
                    log::info!(
                        "rustedscreep planner: {room} planned in {} steps: {} items, anchor ({}, {}), \
                         valid",
                        plan.steps,
                        plan.layout.items.len(),
                        plan.anchor.x(),
                        plan.anchor.y()
                    );
                    self.logged.push(room);
                }
                Status::Failed(why) if !self.logged.contains(&room) => {
                    log::warn!(
                        "rustedscreep planner: {room} has no plan ({why}); the interim placement runs \
                         there"
                    );
                    self.logged.push(room);
                }
                Status::Planning(_) => self.logged.retain(|&r| r != room),
                _ => {}
            }
        }
    }

    /// What `room`'s placement works from.
    pub fn view(&self, room: RoomName, cfg: &PlannerConfig) -> PlanView<'_> {
        if !cfg.enabled {
            return PlanView::Off;
        }
        match self.core.status(room) {
            Status::Ready(plan) => PlanView::Ready(plan),
            Status::Failed(_) => PlanView::Fallback,
            Status::None | Status::Planning(_) => PlanView::Waiting,
        }
    }

    /// One line for `rustedscreep.planner()`: per room its plan's state and its placement's last
    /// look, then the totals and the CPU per phase since the global started.
    pub fn summary(
        &self,
        snapshot: &Snapshot,
        placement: &Placement,
        cfg: &PlannerConfig,
    ) -> String {
        let mut text = format!(
            "rustedscreep planner at tick {}: {}",
            snapshot.tick.time,
            if cfg.enabled { "on" } else { "off" }
        );
        let _ = write!(
            text,
            " (replace {}, storage {}, towers {}, ramparts {}, late {}, max sites {});",
            cfg.replace, cfg.storage, cfg.towers, cfg.ramparts, cfg.late, cfg.max_sites
        );
        let mut any = false;
        for (room, status) in self.core.rooms() {
            any = true;
            match status {
                Status::Ready(plan) => {
                    let _ = write!(
                        text,
                        " {room}: plan ready at {} ({} steps, {} items, anchor ({}, {}))",
                        plan.ready_at,
                        plan.steps,
                        plan.layout.items.len(),
                        plan.anchor.x(),
                        plan.anchor.y()
                    );
                    if let Some(c) = placement.census(room) {
                        let _ = write!(
                            text,
                            ", due {} (built {}, sited {}, missing {}, at the level's count {}, \
                             blocked {}, cut off {})",
                            c.due, c.built, c.sited, c.missing, c.capped, c.blocked, c.cut_off
                        );
                    }
                    text.push(';');
                }
                Status::Planning(phase) => {
                    let _ = write!(text, " {room}: planning (next {phase});");
                }
                Status::Failed(why) => {
                    let _ = write!(text, " {room}: no plan ({why}), interim placement;");
                }
                Status::None => {}
            }
        }
        if !any {
            text.push_str(" no room planned;");
        }
        let t = self.core.totals();
        let p = placement.totals();
        let _ = write!(
            text,
            " since boot: plans started {}, ready {}, failed {}, steps {}; looks {}, sites from plans \
             {} (held for the level's count {}), destroys {} (accepted {}, dropped {})",
            t.started,
            t.ready,
            t.failed,
            t.steps,
            p.looks,
            p.planned,
            p.full,
            p.destroys,
            p.destroyed,
            p.destroys_dropped
        );
        if !self.timings.is_empty() {
            text.push_str(" | CPU per step (steps, mean, most):");
            for tm in &self.timings {
                let mean = tm.total / f64::from(tm.steps.max(1));
                let _ = write!(
                    text,
                    " {} {} {:.2} {:.2},",
                    tm.phase, tm.steps, mean, tm.most
                );
            }
            text.pop();
        }
        text
    }
}
