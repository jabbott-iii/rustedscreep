//! The RCL1–3 opening's glue (ROADMAP P2.4, over `rustedscreep_core::economy::Opening`), static
//! mining's (P3.1, `economy::Mining`), the link network's (P3.2), hauling's (P3.3,
//! `economy::Hauling`) and upgrading's (P3.4, `economy::Upgrading`): the scheduler jobs, the
//! watchdogs' log lines and notifications, the early rescan after a room's own sites change, the
//! link sends, and the `rustedscreep.opening()` console summary.
//!
//! - `mining` (Critical), after movement's first reads and before `opening`: per owned room each
//!   source's mining tile, its miner's spawn request, transfer into its source link, harvest and
//!   move, the sources the workers leave to it or take from the controller link, and the tiles that
//!   need a container or a link site.
//! - `hauling` (Critical, P3.3), after `mining` and before `opening`: per owned room the haulers'
//!   plan, their spawn requests, and their takes at the mining tiles, waits and fills.
//! - `upgrading` (Critical, P3.4), after `hauling` and before `opening`: per owned room with a
//!   controller store (a container or a site on the mining plan's controller container tile, or a
//!   controller link) or an upgrader, the upgraders' plan, posts and spawn request, and the view
//!   the opening sizes its workers by. No unit, and no clock reading, for the other rooms.
//! - `hauling work` (Standard, P3.3), after `opening work`: per owned room with a hauler to store
//!   this tick, its transfers into the controller container (hauling).
//! - `upgrading work` (Standard, P3.4), after `hauling work`: per owned room with an upgrader out of
//!   the spawn, its withdraws, upgrades and moves (upgrading), less what the workers took.
//! - `links` (Standard, P3.2), after `hauling work`: per owned room with a source link that may
//!   send, its sends to the controller link (moving energy is hauling, which CLAUDE.md puts in
//!   Standard; when the tier is off the source link fills and the container takes the harvest).
//! - `opening` (Critical), after `mining` and before the `spawn` job: per owned room
//!   the plan, the watchdog, the spawn request, every worker's task, and the intents and moves of
//!   harvesting, taking energy, filling spawns and extensions, and the downgrade guard's upgrade.
//! - `opening work` (Standard), after the persistence loads: building and upgrading.
//! - `opening placement` (Optional, after the constants self-check; ADR-020, ADR-023, ADR-024,
//!   ADR-025): per owned room, one tile to place a site on (`economy::placement`): a mining tile's
//!   container while `mining.place_containers` is on, or the controller container's while
//!   `hauling.place_container` is on, else a link tile while `links.place_links` is on and the level
//!   allows another link, else, while `opening.place_extensions` is on and the room has a spawn and
//!   fewer extensions and extension sites than its level allows, an extension. In a room whose
//!   base plan is ready (P4.3, ADR-026, `planner.enabled` on) the action comes from the plan
//!   instead (`economy::placement`'s plan path): the economy's containers and links on the plan's
//!   tiles, then the plan's spawns, extensions and roads, or a destroy of an own structure that
//!   stands in the plan's way; while the room's plan is being built nothing is placed there. The
//!   act step calls `Room.createConstructionSite(x, y, type)` for a site, after the spawn orders and
//!   before the creep intents: one JS call per site, billed like an intent (V203).
//!
//! JS calls of its own: those site calls; `Structure.destroy()` for a plan's destroy (ADR-026: at
//! most one a room a tick, before the site calls, billed like an intent, V250; `issue_destroys`
//! checks the snapshot row's type and tile first); one `StructureLink.transferEnergy(target,
//! amount)` per link send (after the sites, before the creep intents; billed, V34, V216); and
//! `Game.notify` on a recovery or a stall (once per kind per global). Everything it reads comes
//! from the snapshot, and its creep intents go through the buffer.

use std::fmt::Write as _;

use rustedscreep_core::config::{
    HaulingConfig, LinkConfig, MiningConfig, MovementConfig, OpeningConfig, PlannerConfig,
    RepairConfig, SchedulerConfig, SpawnConfig, UpgradingConfig,
};
use rustedscreep_core::economy::placement::{DestroyTarget, Placement};
use rustedscreep_core::economy::{
    Crew, Event, HaulTask, Hauling, LIFE_CYCLES, Mining, Opening, SourceHarvest, Task, UpgradeTask,
    Upgrading,
};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{ObjectKind, Snapshot};
use rustedscreep_core::spawning::{Body, EnergyBudget, Spawner};
use screeps::{Part, RoomName, StructureType};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::planner::PlannerGlue;
use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// `Game.notify(message)`.
    #[wasm_bindgen(js_namespace = Game, js_name = notify, catch)]
    fn game_notify(message: &str) -> Result<(), JsValue>;

    /// A room, for `createConstructionSite` (engine rooms.js; V200, V203). The remote glue places
    /// its sites through it too (P6.2).
    pub(crate) type SiteRoom;

    /// `createConstructionSite(x, y, structureType)`; the type crosses as its string.
    #[wasm_bindgen(method, catch, js_name = createConstructionSite)]
    pub(crate) fn create_site(
        this: &SiteRoom,
        x: u8,
        y: u8,
        kind: StructureType,
    ) -> Result<f64, JsValue>;

    /// A link, for `transferEnergy` (engine game/structures.js L488-533 at a90d26f, V216). Bound
    /// here, not through screeps-game-api's `transfer_energy`, which unwraps the return code (V111).
    type LinkActor;

    /// `transferEnergy(target, amount)`, always with an explicit amount: with none, or 0, into a
    /// full link the call returns OK and is billed for nothing (V216).
    #[wasm_bindgen(method, catch, js_name = transferEnergy)]
    fn transfer_energy(this: &LinkActor, target: &JsValue, amount: u32) -> Result<f64, JsValue>;

    /// A structure, for `destroy` (engine game/structures.js, V248 to V250). Bound here, not
    /// through screeps-game-api's `Structure::destroy`, which unwraps the return code (V254).
    type Destroyable;

    /// `destroy()`: OK, ERR_NOT_OWNER, ERR_BUSY (a hostile creep in the room) or
    /// ERR_INVALID_TARGET (V248); an OK call is billed (V250).
    #[wasm_bindgen(method, catch, js_name = destroy)]
    fn destroy(this: &Destroyable) -> Result<f64, JsValue>;
}

/// Harvesting is Critical (CLAUDE.md).
const MINE_JOB: Job = Job {
    name: "mining",
    tier: Tier::Critical,
};

/// The haulers' takes and fills keep the spawns filled, Critical (as the opening's filling).
const HAUL_JOB: Job = Job {
    name: "hauling",
    tier: Tier::Critical,
};

/// The haulers' transfers into the controller container are hauling, Standard (CLAUDE.md).
const HAUL_WORK_JOB: Job = Job {
    name: "hauling work",
    tier: Tier::Standard,
};

/// The upgraders' plan and spawn request run with the other spawn requests, Critical (P2.3: a
/// request must come before the spawn job); their work is upgrading, Standard.
const UPGRADE_JOB: Job = Job {
    name: "upgrading",
    tier: Tier::Critical,
};

/// Upgrading is Standard (CLAUDE.md).
const UPGRADE_WORK_JOB: Job = Job {
    name: "upgrading work",
    tier: Tier::Standard,
};

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

/// Moving energy between links is hauling, Standard (CLAUDE.md).
const SEND_JOB: Job = Job {
    name: "links",
    tier: Tier::Standard,
};

/// Planning is Optional (CLAUDE.md): the interim extension placement (ADR-020).
const PLACE_JOB: Job = Job {
    name: "opening placement",
    tier: Tier::Optional,
};

/// The job name the act step's site faults are logged under.
const SITE_JOB: &str = "opening sites";

/// The job name the act step's link-send faults are logged under.
const LINK_JOB: &str = "link sends";

/// The job name the act step's destroy faults are logged under (ADR-026).
const DESTROY_JOB: &str = "planner destroys";

/// The opening's state for one global.
#[derive(Default)]
pub struct OpeningGlue {
    core: Opening,
    mining: Mining,
    hauling: Hauling,
    upgrading: Upgrading,
    placement: Placement,
    mine_job: JobState,
    haul_job: JobState,
    haul_work_job: JobState,
    upgrade_job: JobState,
    upgrade_work_job: JobState,
    plan_job: JobState,
    work_job: JobState,
    send_job: JobState,
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
    /// The worker body the opening planned for `room` at its capacity on its last plan (P3.6), for
    /// the console's spawn request.
    pub fn worker_body(&self, room: RoomName) -> Option<Body> {
        self.core.plan(room).and_then(|p| p.body)
    }

    /// Starts the tick: the owned rooms and their workers, and an early rescan of each room whose
    /// own sites changed in number since the last tick.
    pub fn begin(&mut self, snapshot: &Snapshot, movement: &mut Movement) {
        self.core.begin(snapshot);
        self.mining.begin(snapshot);
        self.hauling.begin(snapshot);
        self.upgrading.begin(snapshot);
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

    /// The Critical `mining` job, then the Critical `hauling` job, then the Critical `upgrading` job
    /// (for the rooms with a controller store or an upgrader), then the Critical `opening` job: one
    /// unit per owned room each. Run them after movement's first reads and before the `spawn` job,
    /// which plans the requests they make. A miner's, a hauler's or an upgrader's request is held
    /// back while it costs more than the opening's watchdog caps the room's bodies at (from the
    /// opening's last run).
    #[allow(clippy::too_many_arguments)]
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &OpeningConfig,
        mining_cfg: &MiningConfig,
        link_cfg: &LinkConfig,
        repair_cfg: &RepairConfig,
        hauling_cfg: &HaulingConfig,
        upgrading_cfg: &UpgradingConfig,
        mcfg: &MovementConfig,
        spawn_cfg: &SpawnConfig,
    ) {
        let Self {
            core,
            mining,
            hauling,
            upgrading,
            placement,
            mine_job,
            haul_job,
            upgrade_job,
            plan_job,
            ..
        } = self;
        let controller_container = (hauling_cfg.enabled && hauling_cfg.place_container)
            .then_some(hauling_cfg.container_level);
        let mines = mining.room_count();
        if mines > 0 {
            tick.run_units(&MINE_JOB, mine_job, mines, |unit, commit| {
                let room = mining.room_name(unit);
                let cap = room.and_then(|room| core.cap(room));
                let refused = room.map_or(&[][..], |room| placement.refused_tiles(room));
                mining.plan_room(
                    unit,
                    snapshot,
                    movement,
                    spawner,
                    buffer,
                    mining_cfg,
                    link_cfg,
                    mcfg,
                    spawn_cfg,
                    cap,
                    refused,
                    controller_container,
                    commit,
                )
            });
        }
        let hauls = hauling.room_count();
        if hauls > 0 {
            tick.run_units(&HAUL_JOB, haul_job, hauls, |unit, commit| {
                let room = hauling.room_name(unit);
                let cap = room.and_then(|room| core.cap(room));
                let plan = room.and_then(|room| mining.plan(room));
                let mined = room.map_or(&[][..], |room| mining.mined(room));
                hauling.plan_room(
                    unit,
                    snapshot,
                    movement,
                    spawner,
                    buffer,
                    hauling_cfg,
                    mcfg,
                    spawn_cfg,
                    plan,
                    mined,
                    cap,
                    commit,
                )
            });
        }
        upgrading.select(|room| {
            mining.plan(room).is_some_and(|p| p.controller_store)
                || !mining.receivers(room).is_empty()
        });
        let upgrades = upgrading.unit_count();
        if upgrades > 0 {
            tick.run_units(&UPGRADE_JOB, upgrade_job, upgrades, |unit, commit| {
                let room = upgrading.unit_room(unit);
                let cap = room.and_then(|room| core.cap(room));
                let crew = room.map_or(Crew::default(), |room| Crew {
                    miners: mining.stats(room).map_or(0, |s| s.miners),
                    haulers: hauling.stats(room).map_or(0, |s| s.haulers),
                });
                upgrading.plan_room(
                    unit,
                    snapshot,
                    movement,
                    spawner,
                    upgrading_cfg,
                    mcfg,
                    spawn_cfg,
                    room.and_then(|room| mining.plan(room)),
                    room.map_or(&[][..], |room| mining.receivers(room)),
                    room.and_then(|room| hauling.plan(room)),
                    crew,
                    room.and_then(|room| core.plan(room)),
                    cap,
                    commit,
                )
            });
        }
        let count = core.room_count();
        if count == 0 {
            return;
        }
        tick.run_units(&PLAN_JOB, plan_job, count, |unit, commit| {
            let room = core.room_name(unit);
            let mined = room.map_or(&[][..], |room| mining.mined(room));
            let tiles = room.map_or(&[][..], |room| mining.mine_tiles(room));
            let receivers = room.map_or(&[][..], |room| mining.receivers(room));
            let haul = room.map(|room| hauling.view(room)).unwrap_or_default();
            let upgrade = room.map(|room| upgrading.view(room)).unwrap_or_default();
            core.plan_room(
                unit, snapshot, movement, spawner, buffer, cfg, repair_cfg, mcfg, mined, tiles,
                receivers, &haul, &upgrade, commit,
            )
        });
    }

    /// The Standard `opening work` job (one unit per owned room), then the Standard `hauling work`
    /// job (one per room with a hauler to store this tick; none, and no clock reading, without),
    /// then the Standard `upgrading work` job (one per room with an upgrader out of the spawn; none,
    /// and no clock reading, without).
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
    ) {
        let Self {
            core,
            hauling,
            upgrading,
            work_job,
            haul_work_job,
            upgrade_work_job,
            ..
        } = self;
        let count = core.room_count();
        if count > 0 {
            tick.run_units(&WORK_JOB, work_job, count, |unit, commit| {
                core.work_room(unit, snapshot, movement, buffer, mcfg, commit)
            });
        }
        let hauls = hauling.work_count();
        if hauls > 0 {
            tick.run_units(&HAUL_WORK_JOB, haul_work_job, hauls, |unit, commit| {
                hauling.work_room(unit, snapshot, movement, buffer, mcfg, commit)
            });
        }
        let upgrades = upgrading.work_count();
        if upgrades > 0 {
            tick.run_units(
                &UPGRADE_WORK_JOB,
                upgrade_work_job,
                upgrades,
                |unit, commit| {
                    let room = upgrading.work_room_name(unit);
                    let taken = room.map_or(&[][..], |room| core.taken(room));
                    let hauled = room.map_or(&[][..], |room| hauling.view(room).taken);
                    upgrading.work_room(
                        unit, snapshot, movement, buffer, mcfg, taken, hauled, commit,
                    )
                },
            );
        }
    }

    /// The Standard `links` job (P3.2): one unit per owned room whose `mining` run found a source
    /// link that may send; none, and no clock reading, on a tick with none.
    pub fn send_links<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, link_cfg: &LinkConfig) {
        let count = self.mining.send_count();
        if count == 0 {
            return;
        }
        let Self {
            mining, send_job, ..
        } = self;
        tick.run_units(&SEND_JOB, send_job, count, |unit, commit| {
            mining.send_room(unit, link_cfg, commit)
        });
    }

    /// The Optional `opening placement` job: one unit per owned room, while `planner.enabled`,
    /// `opening.place_extensions`, `mining.place_containers`, `links.place_links` or
    /// `hauling.place_container` is on (the mining job lists no container tile or link tile while
    /// the switch for it is off). A room with a plan places from it (P4.3); one whose plan is being
    /// built places nothing; the rest place as before (ADR-020, ADR-023 to ADR-025).
    #[allow(clippy::too_many_arguments)]
    pub fn place<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        planner: &PlannerGlue,
        cfg: &OpeningConfig,
        mining_cfg: &MiningConfig,
        link_cfg: &LinkConfig,
        hauling_cfg: &HaulingConfig,
        pcfg: &PlannerConfig,
        mcfg: &MovementConfig,
    ) {
        let count = self.placement.room_count();
        let any = pcfg.enabled
            || cfg.place_extensions
            || mining_cfg.place_containers
            || link_cfg.place_links
            || (hauling_cfg.enabled && hauling_cfg.place_container);
        if !any || count == 0 {
            return;
        }
        let Self {
            placement,
            mining,
            place_job,
            ..
        } = self;
        let extensions = cfg.place_extensions;
        tick.run_units(&PLACE_JOB, place_job, count, |unit, commit| {
            let room = placement.room_name(unit);
            let containers = room.map_or(&[][..], |room| mining.container_needs(room));
            let links = room.map_or(&[][..], |room| mining.link_needs(room));
            let view = room.map_or(
                rustedscreep_core::economy::placement::PlanView::Off,
                |room| planner.view(room, pcfg),
            );
            placement.plan_room(
                unit, snapshot, movement, mcfg, extensions, containers, links, view, pcfg, commit,
            )
        });
    }

    /// The placement (P4.3's console summary reads its census and totals).
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    /// The hauling state, which delivers what the remote haulers bring, and the upgrading state,
    /// whose budget counts it (P6.2, for the remote glue).
    pub fn haul_parts(&mut self) -> (&Hauling, &mut Upgrading) {
        (&self.hauling, &mut self.upgrading)
    }

    /// Calls `destroy` for this tick's off-plan structures (ADR-026) and records each outcome.
    /// Call in the act step, before the sites. A call that throws, or an order with no JS object,
    /// is a fault under `planner destroys`; each call is logged at info (a destroy is rare and
    /// loses the structure's cost).
    pub fn issue_destroys(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
    ) {
        for index in 0..self.placement.destroys().len() {
            let Some(&order) = self.placement.destroys().get(index) else {
                break;
            };
            // A destroy cannot be undone: the row must still hold that kind on that tile.
            let here = |p: &screeps::Position| {
                p.room_name() == order.room
                    && rustedscreep_core::movement::Tile::of(*p) == order.tile
            };
            let (object, matches): (Option<&JsValue>, bool) = match order.target {
                DestroyTarget::Structure(row) => (
                    js.structures.get(row).map(AsRef::as_ref),
                    snapshot.structures.kinds().get(row) == Some(&order.kind)
                        && snapshot.structures.positions().get(row).is_some_and(here),
                ),
                DestroyTarget::Object(row) => (
                    js.objects.get(row).map(AsRef::as_ref),
                    order.kind == StructureType::Container
                        && snapshot.objects.kinds().get(row) == Some(&ObjectKind::Container)
                        && snapshot.objects.positions().get(row).is_some_and(here),
                ),
            };
            let Some(object) = object.filter(|_| matches) else {
                destroy_fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for the structure row",
                    index,
                );
                log.missing();
                self.placement.record_destroy(index, Outcome::Unknown);
                continue;
            };
            let outcome = match object.unchecked_ref::<Destroyable>().destroy() {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    destroy_fault(sched, cfg, snapshot, "destroy threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            let what = match outcome {
                Outcome::Ok => "ok",
                Outcome::Failed(code) => error_name(code),
                Outcome::Unknown => "unknown return",
                Outcome::Threw => "threw",
            };
            log::info!(
                "rustedscreep planner: destroy {:?} at ({}, {}) in {} for the plan: {what}",
                order.kind,
                order.tile.x(),
                order.tile.y(),
                order.room
            );
            self.placement.record_destroy(index, outcome);
        }
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
                let kind = match order.kind {
                    StructureType::Container => "container",
                    StructureType::Extension => "extension",
                    StructureType::Link => "link",
                    StructureType::Road => "road",
                    StructureType::Spawn => "spawn",
                    _ => "construction",
                };
                log::info!(
                    "rustedscreep opening: no {kind} site at ({}, {}) in {}: {what}; the tile is \
                     skipped",
                    order.tile.x(),
                    order.tile.y(),
                    order.room
                );
            }
            self.placement.record(index, outcome);
        }
    }

    /// Calls `transferEnergy` for this tick's link sends (P3.2) and records each outcome. Call in
    /// the act step, after the sites and before the creep intents. A call that throws, or a send
    /// with no JS object for one of its links, is a fault under `link sends`; a refusal is logged
    /// at info, and its link sends no more for `links.hold_ticks`, so the line repeats at most once
    /// per hold.
    pub fn issue_links(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        link_cfg: &LinkConfig,
    ) {
        for index in 0..self.mining.link_orders().len() {
            let Some(&order) = self.mining.link_orders().get(index) else {
                break;
            };
            let (Some(from), Some(to)) = (
                js.structures.get(order.from_row as usize),
                js.structures.get(order.to_row as usize),
            ) else {
                link_fault(sched, cfg, snapshot, "no JS object for a link row", index);
                log.missing();
                continue;
            };
            let call = from
                .unchecked_ref::<LinkActor>()
                .transfer_energy(to.as_ref(), order.amount);
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    link_fault(sched, cfg, snapshot, "transferEnergy threw", index);
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
                    "rustedscreep links: a send of {} energy was refused at tick {}: {what}; the \
                     link waits {} ticks",
                    order.amount,
                    snapshot.tick.time,
                    link_cfg.hold_ticks
                );
            }
            self.mining.record_send(index, outcome, link_cfg);
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
                        if s.mined {
                            text.push_str(" mined");
                        }
                        if s.hauled {
                            text.push_str(" hauled");
                        } else if s.linked {
                            text.push_str(" linked");
                        }
                    }
                    // The solved worker body at the plan's capacity (P3.6).
                    if let Some(b) = p.body {
                        let _ = write!(
                            text,
                            ", body {} WORK {} CARRY {} MOVE",
                            b.count(Part::Work),
                            b.count(Part::Carry),
                            b.count(Part::Move)
                        );
                    }
                }
            }
            let _ = write!(
                text,
                ", sites {}, loose energy {}",
                stats.sites, stats.loose
            );
            if stats.containers > 0 {
                let _ = write!(
                    text,
                    ", containers {} (under repair {}",
                    stats.containers, stats.repairing
                );
                if let Some(h) = stats.lowest_hits {
                    let _ = write!(text, ", lowest hits {h}");
                }
                text.push(')');
            }
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
        self.mining_summary(&mut text);
        self.hauling_summary(&mut text);
        self.upgrading_summary(&mut text);
        let p = self.placement.totals();
        let skipped: usize = self.placement.refused().map(|(_, n)| n).sum();
        let _ = write!(
            text,
            " | sites: picked {} ({} containers, {} links), placed {}, refused {}, never seen {}, \
             tiles skipped {}",
            p.picked, p.containers, p.links, p.placed, p.refused, p.lost, skipped
        );
        let l = self.mining.link_totals();
        let _ = write!(
            text,
            " | link sends since boot: ordered {}, sent {}, failed {}, energy {}",
            l.ordered, l.sent, l.failed, l.energy
        );
        text
    }

    /// The hauling part of the summary (P3.3): per owned room its haulers and their tasks, this
    /// tick's requests, the controller container, and per hauled source the plan's trip, body and
    /// count.
    fn hauling_summary(&self, text: &mut String) {
        text.push_str(" | hauling:");
        for (room, plan, stats) in self.hauling.room_states() {
            let _ = write!(
                text,
                " {room}: haulers {} of {} (working {}, due {}, requested {}, hauled sources {})",
                stats.haulers, stats.want, stats.working, stats.due, stats.requested, stats.hauled
            );
            let mut first = true;
            for (&n, kind) in stats.tasks.iter().zip(HaulTask::KINDS) {
                if n > 0 {
                    let _ = write!(text, "{} {kind} {n}", if first { ", tasks" } else { "," });
                    first = false;
                }
            }
            let Some(p) = plan else {
                text.push_str(if stats.container {
                    ", no plan yet;"
                } else {
                    ", no controller container;"
                });
                continue;
            };
            let _ = write!(
                text,
                ", controller container ({},{})",
                p.container.x().u8(),
                p.container.y().u8()
            );
            for s in &p.sources {
                let _ = write!(
                    text,
                    ", source ({},{}) trip {} tiles cost {}",
                    s.source.x().u8(),
                    s.source.y().u8(),
                    s.trip.tiles,
                    s.trip.cost
                );
                match s.body {
                    Some(b) => {
                        let _ = write!(
                            text,
                            " want {} of {} CARRY {} MOVE ({} energy)",
                            s.want,
                            b.count(Part::Carry),
                            b.count(Part::Move),
                            b.cost()
                        );
                    }
                    None => {
                        let _ = write!(text, " no hauler at capacity {}", p.capacity);
                    }
                }
            }
            text.push(';');
        }
    }

    /// The upgrading part of the summary (P3.4): per owned room its upgraders and what they did in
    /// the last work run, whether they take the controller's energy, and the plan's budget a tick
    /// (what reaches the controller less the other creeps' bodies), body, count and posts.
    fn upgrading_summary(&self, text: &mut String) {
        text.push_str(" | upgrading:");
        for (room, plan, stats) in self.upgrading.room_states() {
            let _ = write!(
                text,
                " {room}: upgraders {} of {} (working {}, due {}, requested {}, upgrades {}{})",
                stats.upgraders,
                stats.want,
                stats.working,
                stats.due,
                stats.requested,
                stats.upgrades,
                if stats.takes { ", taking" } else { "" }
            );
            let mut first = true;
            for (&n, kind) in stats.tasks.iter().zip(UpgradeTask::KINDS) {
                if n > 0 {
                    let _ = write!(text, "{} {kind} {n}", if first { ", tasks" } else { "," });
                    first = false;
                }
            }
            let Some(p) = plan else {
                text.push_str(", no controller store;");
                continue;
            };
            let life = u64::from(screeps::CREEP_LIFE_TIME.max(1));
            let _ = write!(
                text,
                ", budget {} a tick ({} in, {} other bodies), posts {}",
                p.budget() / life,
                p.delivered / life,
                p.upkeep / life,
                p.posts.len()
            );
            match p.body {
                Some(b) => {
                    let _ = write!(
                        text,
                        ", want {} of {} WORK {} CARRY {} MOVE ({} energy);",
                        p.want,
                        b.count(Part::Work),
                        b.count(Part::Carry),
                        b.count(Part::Move),
                        b.cost()
                    );
                }
                None => {
                    let _ = write!(text, ", no upgrader at capacity {};", p.capacity);
                }
            }
        }
    }

    /// The mining part of the summary: per owned room its miners, the sources they mine, this
    /// tick's harvests and requests, and per source its tile, the miner's walk and body, and its
    /// harvest per regeneration cycle (the P3 gate).
    fn mining_summary(&self, text: &mut String) {
        text.push_str(" | mining:");
        for (room, plan, stats) in self.mining.room_states() {
            let _ = write!(
                text,
                " {room}: miners {} (mined {}, harvests {}, requested {}, due {}, next {}, idle {}), \
                 links {} (linked {}, transfers {}, sends {})",
                stats.miners,
                stats.mined,
                stats.harvests,
                stats.requested,
                stats.due,
                stats.next,
                stats.idle,
                stats.links,
                stats.linked,
                stats.transfers,
                stats.sends
            );
            let Some(p) = plan else {
                text.push_str(", no plan yet;");
                continue;
            };
            match p.controller_link {
                Some(c) => {
                    let _ = write!(text, ", controller link ({},{})", c.x().u8(), c.y().u8());
                }
                None => text.push_str(", no controller link tile"),
            }
            for s in &p.sources {
                let _ = write!(
                    text,
                    ", source ({},{})",
                    s.source.x().u8(),
                    s.source.y().u8()
                );
                match (s.tile, s.trip) {
                    (Some(tile), Some(t)) => {
                        let _ = write!(
                            text,
                            " tile ({},{}) trip {} tiles cost {}",
                            tile.x().u8(),
                            tile.y().u8(),
                            t.tiles,
                            t.cost
                        );
                    }
                    _ => text.push_str(" no tile"),
                }
                if let Some(l) = s.link {
                    let _ = write!(text, " link ({},{})", l.x().u8(), l.y().u8());
                }
                match s.body {
                    Some(b) => {
                        let _ = write!(text, " body {} WORK", b.count(Part::Work));
                        if b.count(Part::Carry) > 0 {
                            let _ = write!(text, " {} CARRY", b.count(Part::Carry));
                        }
                        let _ = write!(text, " {} MOVE ({} energy)", b.count(Part::Move), b.cost());
                    }
                    None => {
                        let _ = write!(text, " no miner at capacity {}", p.capacity);
                    }
                }
                let harvest = self.mining.harvest(room);
                if let Some(h) = harvest.iter().find(|h| h.source == s.source) {
                    harvest_summary(text, h);
                }
            }
            text.push(';');
        }
    }
}

/// One source's harvest cycles for the summary (the P3 gate, `economy::SourceHarvest`): the cycles
/// counted since the global started; the last [`LIFE_CYCLES`] (one miner life), what they harvested
/// of what they held and the worst of them; all of them; the refills kept out; and the handovers
/// between its miners with their gaps (P5.2; `last -1` before the first).
fn harvest_summary(text: &mut String, h: &SourceHarvest) {
    let pct = |permille: u64| (permille / 10, permille % 10);
    let _ = write!(text, ", harvest cycles {}", h.cycles);
    if let Some((mean, worst)) = h.life() {
        let (m, md) = pct(mean);
        let (w, wd) = pct(worst);
        let _ = write!(
            text,
            ", last {} {m}.{md}% (worst {w}.{wd}%; a life is {LIFE_CYCLES})",
            h.recent().len()
        );
    }
    if let Some(all) = h.overall() {
        let (a, ad) = pct(all);
        let _ = write!(text, ", all {a}.{ad}%");
    }
    let _ = write!(text, ", unmined {}, missed {}", h.unmined, h.missed);
    // P5.2: the handovers between its miners and the ticks they left it with none.
    let _ = write!(
        text,
        ", handovers {} (gap ticks {}, last {}, most {})",
        h.handovers,
        h.gap_ticks,
        h.last_gap.map_or(-1, i64::from),
        h.max_gap
    );
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
                 progress, no change in its sources' energy, no construction progress); \
                 planning it again"
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

/// A fault of the act step's destroys (`DESTROY_JOB`), rate-limited like a job's.
#[cold]
#[inline(never)]
fn destroy_fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: DESTROY_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}

/// A fault of the act step's link sends (`LINK_JOB`), rate-limited like a job's.
#[cold]
#[inline(never)]
fn link_fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: LINK_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
