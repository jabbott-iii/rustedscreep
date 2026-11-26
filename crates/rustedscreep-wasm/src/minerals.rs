//! Mineral mining's glue (ROADMAP P9.1, ADR-044, over `rustedscreep_core::economy::Minerals`): its
//! two jobs, the rooms the next snapshot reads, its log lines, and the `rustedscreep.minerals()`
//! console summary.
//!
//! - `minerals` (Critical, before the opening's job and the spawn plan): one unit a room with an
//!   own extractor or a mineral creep: the room's plan (two flow fields when its key changes,
//!   heavy), why it mines or not, and its requests on ticks the Standard tier runs. Then each
//!   room's upkeep goes to the upgraders' budget (`Upgrading::set_minerals`). No JS call.
//! - `minerals work` (Standard): one unit a room read this tick with a creep out of the spawn:
//!   the miner's move and `harvest`, each hauler's move, `withdraw` and `transfer` (the act step
//!   issues them with the other creep intents).
//! - After the jobs: `Minerals::reads` gives the next snapshot its rooms (`JsWorld::set_mineral_reads`),
//!   where `snapshot.rs` reads the mineral, the container next to it, the extractor's cooldown and
//!   the terminal's stock and room: about 12 calls a room.

use std::fmt::Write as _;

use rustedscreep_core::config::{HaulingConfig, MineralsConfig, MovementConfig};
use rustedscreep_core::economy::{MineralEvent, MineralWhy, Minerals, Upgrading};
use rustedscreep_core::intents::IntentBuffer;
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{Clock, Job, JobState, TickCpu, Tier};
use rustedscreep_core::snapshot::Snapshot;
use rustedscreep_core::spawning::Spawner;
use screeps::RoomName;

/// The rooms' plans and requests: Critical, with the other spawn requests.
const MINERALS_JOB: Job = Job {
    name: "minerals",
    tier: Tier::Critical,
};

/// The miners' and haulers' work: Standard (mining a mineral is hauling's tier, not the energy
/// harvest's).
const MINERALS_WORK_JOB: Job = Job {
    name: "minerals work",
    tier: Tier::Standard,
};

/// The mineral glue's state for one global.
#[derive(Default)]
pub struct MineralsGlue {
    core: Minerals,
    job: JobState,
    work_job: JobState,
    /// Whether this tick runs the Standard tier: the creeps act only then, so only then are
    /// their rooms read every tick.
    standard: bool,
}

impl MineralsGlue {
    /// Starts the tick: the rooms, their reads and their creeps from the snapshot.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &MineralsConfig) {
        self.core.begin(snapshot, cfg);
    }

    /// The Critical `minerals` job (not run with no room), then the upgraders' upkeep.
    #[allow(clippy::too_many_arguments)]
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        upgrading: &mut Upgrading,
        cfg: &MineralsConfig,
        hcfg: &HaulingConfig,
        mcfg: &MovementConfig,
    ) {
        let standard = tick.allows(Tier::Standard);
        self.standard = standard;
        let units = self.core.units();
        if units > 0 {
            let Self { core, job, .. } = self;
            tick.run_units(&MINERALS_JOB, job, units, |unit, commit| {
                core.plan_room(
                    unit, snapshot, movement, spawner, cfg, hcfg, mcfg, standard, commit,
                )
            });
        }
        // This tick's plan runs, and a 0 for each room dropped at `begin`.
        for &(room, energy) in self.core.upkeeps() {
            upgrading.set_minerals(room, energy);
        }
    }

    /// The Standard `minerals work` job (not run with no room to work).
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        mcfg: &MovementConfig,
    ) {
        if !self.core.has_work() {
            return;
        }
        let units = self.core.units();
        let Self { core, work_job, .. } = self;
        tick.run_units(&MINERALS_WORK_JOB, work_job, units, |unit, commit| {
            core.work_room(unit, snapshot, movement, buffer, mcfg, commit)
        });
    }

    /// The rooms the next snapshot reads for their minerals.
    pub fn reads(&mut self, cfg: &MineralsConfig) -> &[RoomName] {
        self.core.reads(cfg, self.standard)
    }

    /// This tick's starts and stops, at info.
    pub fn log_events(&self) {
        for &e in self.core.events() {
            match e {
                MineralEvent::Started { room, kind, amount } => log::info!(
                    "rustedscreep minerals: {room} mines its {} ({amount} left)",
                    kind_name(kind)
                ),
                MineralEvent::Stopped { room, why } => {
                    log::info!(
                        "rustedscreep minerals: {room} stopped mining: {}",
                        why.as_str()
                    );
                }
            }
        }
    }

    /// The `rustedscreep.minerals()` summary: per room why it mines or not, what its last read
    /// saw, its plan, its creeps and its counters since the global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &MineralsConfig) -> String {
        let rooms = self.core.rooms();
        let mut text = format!(
            "rustedscreep minerals at tick {}: {} (stock cap {}, terminal free min {}, max work {}) | rooms {}",
            snapshot.tick.time,
            if cfg.enabled { "on" } else { "off" },
            cfg.stock_max,
            cfg.free_min,
            cfg.max_work,
            rooms.len()
        );
        for r in rooms {
            let _ = write!(text, " | {}: {}", r.name, r.why.as_str());
            if let Some(s) = r.seen {
                let _ = write!(
                    text,
                    "; read at {}: {} {} left",
                    s.time,
                    s.kind.map_or("?", kind_name),
                    s.amount
                );
                if r.why == MineralWhy::Regenerating
                    && let Some(at) = r.regen_at
                {
                    let _ = write!(text, " (regenerates at {at})");
                }
                if s.container.is_some() {
                    let _ = write!(
                        text,
                        ", container {} of it ({} held)",
                        s.container_stock, s.container_used
                    );
                }
                if s.terminal.is_some() {
                    let _ = write!(
                        text,
                        ", terminal {} of it ({} free)",
                        s.terminal_stock, s.terminal_free
                    );
                }
            }
            if let Some(p) = r.plan {
                let _ = write!(
                    text,
                    "; plan: miner {} WORK ({} energy, walk {}), {} hauler(s) of {} ({} energy, trip {})",
                    p.work(),
                    p.miner.map_or(0, |b| b.cost()),
                    p.miner_trip.cost,
                    p.haulers,
                    p.load(),
                    p.hauler.map_or(0, |b| b.cost()),
                    p.haul_trip.cost
                );
            }
            let st = r.stats;
            let _ = write!(
                text,
                "; miners {}, haulers {}; since boot: reads {}, requests {} miner {} hauler, harvests {}, withdraws {}, transfers {}",
                r.miners.len(),
                r.haulers.len(),
                st.reads,
                st.miner_requests,
                st.hauler_requests,
                st.harvests,
                st.withdraws,
                st.transfers
            );
        }
        text
    }
}

/// A mineral's resource code, as the game writes it (`H`, `O`, ...); `?` for any other resource.
fn kind_name(kind: screeps::ResourceType) -> &'static str {
    use screeps::ResourceType as R;
    match kind {
        R::Hydrogen => "H",
        R::Oxygen => "O",
        R::Utrium => "U",
        R::Lemergium => "L",
        R::Keanium => "K",
        R::Zynthium => "Z",
        R::Catalyst => "X",
        _ => "?",
    }
}
