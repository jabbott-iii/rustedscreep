//! The factory's glue (ROADMAP P9.3, ADR-046, over `rustedscreep_core::economy::Factories`): its
//! two jobs, the batches' `produce` calls, the terminals the haulers fill, the rooms the next
//! snapshot reads, the rooms that want a factory built, its log lines, and the
//! `rustedscreep.factory()` console summary.
//!
//! - `factory` (Critical, before the hauling job and the spawn plan): one unit a room with an own
//!   factory, an own terminal at a factory's level, or a factory hauler: on a read tick the job
//!   and whether the room wants a factory, the bank and the hauler's request on ticks the Standard
//!   tier runs. Then each room's upkeep goes to the upgraders' budget (`Upgrading::set_factory`)
//!   and the banks to the haulers (`Hauling::set_banks`). No JS call.
//! - `factory work` (Standard): one unit a room read this tick: the batch (an order for the act
//!   step) and the hauler's move, `withdraw` and `transfer` (the act step issues them with the
//!   other creep intents).
//! - In the act step, `produce(resourceType)` for each order ([`FactoryGlue::issue`]).
//! - After the jobs: `Factories::reads` gives the next snapshot its rooms, read in full or for
//!   their terminal's energy alone (`JsWorld::set_factory_reads`); `Factories::wants_factory` gives the placement the rooms
//!   whose factory it places (`Placement::set_factory_rooms`).

use std::fmt::Write as _;

use rustedscreep_core::config::{FactoryConfig, MovementConfig, SchedulerConfig};
use rustedscreep_core::economy::{
    Factories, FactoryEvent, Hauling, Upgrading, factory_life_energy, factory_pace,
};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{FACTORY_RESOURCES, Snapshot};
use rustedscreep_core::spawning::Spawner;
use screeps::{ResourceType, RoomName};
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A factory, for `produce` (engine game/structures.js L1434 at a90d26f, V420). Bound here
    /// with `catch`, not through screeps-game-api's `produce`, which converts the return with no
    /// `catch` (as the labs' `runReaction`, V408).
    type ProduceActor;

    /// `produce(resourceType)`: OK, or one of V420's error codes; an OK call is billed.
    #[wasm_bindgen(method, catch, js_name = produce)]
    fn produce(this: &ProduceActor, resource: ResourceType) -> Result<f64, wasm_bindgen::JsValue>;
}

/// The rooms' jobs, banks and requests: Critical, with the other spawn requests.
const FACTORY_JOB: Job = Job {
    name: "factory",
    tier: Tier::Critical,
};

/// The batches and the hauler's work: Standard (hauling's tier).
const FACTORY_WORK_JOB: Job = Job {
    name: "factory work",
    tier: Tier::Standard,
};

/// The act step's batches, for its faults.
const PRODUCE_JOB: &str = "factory batches";

/// The factory glue's state for one global.
#[derive(Default)]
pub struct FactoryGlue {
    core: Factories,
    job: JobState,
    work_job: JobState,
    /// The rooms whose source haulers carry energy home this tick (their hauling plan wants
    /// haulers), scratch.
    hauled: Vec<RoomName>,
    /// Whether this tick runs the Standard tier: the hauler acts only then, so only then is its
    /// room read every tick.
    standard: bool,
}

impl FactoryGlue {
    /// Starts the tick: the rooms, their reads and their haulers from the snapshot.
    pub fn begin(&mut self, snapshot: &Snapshot) {
        self.core.begin(snapshot);
    }

    /// The Critical `factory` job (not run with no room), then the upgraders' upkeep and the
    /// haulers' banks. `hauling` is last tick's plans (which rooms' sources are hauled).
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        spawner: &mut Spawner,
        hauling: &Hauling,
        upgrading: &mut Upgrading,
        cfg: &FactoryConfig,
    ) {
        let standard = tick.allows(Tier::Standard);
        self.standard = standard;
        let units = self.core.units();
        if units > 0 {
            self.hauled.clear();
            self.hauled.extend(
                hauling
                    .room_states()
                    .filter(|(_, plan, _)| {
                        plan.is_some_and(|p| p.sources.iter().any(|s| s.want > 0))
                    })
                    .map(|(name, _, _)| name),
            );
            let Self {
                core, job, hauled, ..
            } = self;
            tick.run_units(&FACTORY_JOB, job, units, |unit, _commit| {
                let name = core.rooms().get(unit as usize).map(|r| r.name);
                let is_hauled = name.is_some_and(|n| hauled.contains(&n));
                core.plan_room(unit, snapshot, spawner, cfg, standard, is_hauled)
            });
        }
        for &(room, energy) in self.core.upkeeps() {
            upgrading.set_factory(room, energy);
        }
    }

    /// The terminals the haulers deliver energy to this tick, for `Hauling::set_banks`.
    pub fn banks(
        &self,
    ) -> &[(
        RoomName,
        rustedscreep_core::snapshot::StructureHandle,
        screeps::Position,
        u32,
    )] {
        self.core.banks()
    }

    /// The Standard `factory work` job (not run with no room to work).
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &FactoryConfig,
        mcfg: &MovementConfig,
    ) {
        if !self.core.has_work() {
            return;
        }
        let units = self.core.units();
        let Self { core, work_job, .. } = self;
        tick.run_units(&FACTORY_WORK_JOB, work_job, units, |unit, commit| {
            core.work_room(unit, snapshot, movement, buffer, cfg, mcfg, commit)
        });
    }

    /// The rooms that want a factory built, for the placement.
    pub fn wants_factory(&mut self) -> &[RoomName] {
        self.core.wants_factory()
    }

    /// Gives the next snapshot the rooms it reads for their factory, and those it reads for
    /// their terminal's energy alone.
    pub fn set_reads(&mut self, js: &mut JsWorld, cfg: &FactoryConfig) {
        let (reads, banks) = self.core.reads(cfg, self.standard);
        js.set_factory_reads(reads, banks);
    }

    /// Calls `produce` for this tick's batches and records each outcome. Call in the act step,
    /// after the labs' reactions and before the creep intents. A call that throws, or an order
    /// with no JS object for its factory, is a fault under `factory batches`; a refusal holds the
    /// room's batches for `factory.read_ticks` and is logged at info when it starts the hold.
    pub fn issue(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        factory_cfg: &FactoryConfig,
    ) {
        for index in 0..self.core.orders().len() {
            let Some(&order) = self.core.orders().get(index) else {
                break;
            };
            let Some(factory) = js.structures.get(order.row as usize) else {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for a factory row",
                    index,
                );
                log.missing();
                continue;
            };
            let call = factory
                .unchecked_ref::<ProduceActor>()
                .produce(order.product);
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    fault(sched, cfg, snapshot, "produce threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if self.core.record_produce(index, outcome, factory_cfg) {
                let what = match outcome {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    Outcome::Threw => "threw",
                    Outcome::Ok => "ok",
                };
                log::info!(
                    "rustedscreep factory: {}'s batch of {} was refused at tick {}: {what}; the \
                     factory waits {} ticks",
                    order.room,
                    order.product,
                    snapshot.tick.time,
                    factory_cfg.read_ticks
                );
            }
        }
    }

    /// This tick's job starts and ends, at info.
    pub fn log_events(&self, cfg: &FactoryConfig) {
        for &e in self.core.events() {
            match e {
                FactoryEvent::Started { room, recipe, held } => log::info!(
                    "rustedscreep factory: {room} compresses its {} into {} ({held} held, {} kept)",
                    recipe.mineral,
                    recipe.bar,
                    cfg.mineral_keep
                ),
                FactoryEvent::Stopped {
                    room,
                    recipe,
                    batches,
                    stop,
                } => log::info!(
                    "rustedscreep factory: {room} stopped compressing its {} ({batches} batches): {}",
                    recipe.mineral,
                    stop.as_str()
                ),
            }
        }
    }

    /// The `rustedscreep.factory()` summary: the settings and the pace of each bar, then per room
    /// what its factory does, what its last read saw, its job, its hauler and its counters since
    /// the global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &FactoryConfig) -> String {
        let rooms = self.core.rooms();
        let mut text = format!(
            "rustedscreep factory at tick {}: {} (keep {}, bar cap {}, energy keep {}, energy a \
             life {}, hauler carry {})",
            snapshot.tick.time,
            if cfg.enabled { "on" } else { "off" },
            cfg.mineral_keep,
            cfg.bar_max,
            cfg.energy_keep,
            cfg.energy_per_life,
            cfg.hauler_carry
        );
        if let Some(r) = self.core.recipes().first() {
            match factory_pace(r, cfg) {
                Some(p) => {
                    let _ = write!(
                        text,
                        " | a batch every {p} ticks ({} energy a life)",
                        factory_life_energy(r, cfg)
                    );
                }
                None => text.push_str(" | no batch: the energy a life pays for none"),
            }
        }
        let _ = write!(text, " | rooms {}", rooms.len());
        for r in rooms {
            let _ = write!(
                text,
                " | {}: {}{}",
                r.name,
                r.why.as_str(),
                if r.wants { ", wants a factory" } else { "" }
            );
            if let Some(s) = r.seen {
                let _ = write!(text, "; read at {}", r.seen_time);
                if let Some(t) = s.terminal {
                    let _ = write!(
                        text,
                        ", terminal free {}, energy {}{}",
                        t.free,
                        t.energy,
                        held(&s.stock)
                    );
                }
                if let Some(f) = s.factory {
                    let _ = write!(
                        text,
                        ", factory cooldown {}, free {}, energy {}{}",
                        f.cooldown,
                        f.free,
                        f.energy,
                        held(&f.stock)
                    );
                }
            }
            if let Some(j) = r.job {
                let _ = write!(
                    text,
                    "; job {} into {} ({} batches",
                    j.mineral, j.bar, r.batches
                );
                if let Some(n) = r.next_produce() {
                    let _ = write!(text, ", next at {n}");
                }
                text.push(')');
            }
            if r.bank {
                text.push_str("; terminal takes energy");
            }
            let st = r.stats;
            let _ = write!(
                text,
                "; haulers {}; since boot: reads {}, bank reads {}, requests {}, jobs {}, batches {}, refused {}, withdraws {}, transfers {}",
                r.haulers.len(),
                st.reads,
                st.bank_reads,
                st.requests,
                st.jobs,
                st.batches,
                st.refused,
                st.withdraws,
                st.transfers
            );
        }
        text
    }
}

/// The nonzero entries of a factory stock, as " (type n, ...)", or nothing.
fn held(stock: &rustedscreep_core::snapshot::FactoryStock) -> String {
    let held: Vec<String> = FACTORY_RESOURCES
        .iter()
        .zip(stock)
        .filter(|&(_, &n)| n > 0)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    if held.is_empty() {
        String::new()
    } else {
        format!(" ({})", held.join(", "))
    }
}

/// A fault of the act step's batches (`PRODUCE_JOB`), rate-limited like a job's.
fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: PRODUCE_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
