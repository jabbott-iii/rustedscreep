//! Lab reactions' glue (ROADMAP P9.2, ADR-045, over `rustedscreep_core::economy::Labs`): its two
//! jobs, the reactions' `runReaction` calls, the rooms the next snapshot reads, the rooms that
//! want labs built, its log lines, and the `rustedscreep.labs()` console summary.
//!
//! - `labs` (Critical, before the opening's job and the spawn plan): one unit a room with an own
//!   terminal or lab or a lab hauler: on a read tick the lab roles and the job, whether the room
//!   wants labs, the hauler's request on ticks the Standard tier runs. Then each room's upkeep goes
//!   to the upgraders' budget (`Upgrading::set_labs`). No JS call.
//! - `labs work` (Standard): one unit a room read this tick: the reactions (orders for the act
//!   step) and the hauler's move, `withdraw` and `transfer` (the act step issues them with the
//!   other creep intents).
//! - In the act step, `runReaction(lab1, lab2)` for each order ([`LabsGlue::issue`]).
//! - After the jobs: `Labs::reads` gives the next snapshot its rooms (`JsWorld::set_lab_reads`),
//!   where `snapshot.rs` reads the terminal's stock and the labs; `Labs::wants_labs` gives the
//!   placement the rooms whose labs it places (`Placement::set_lab_rooms`).

use std::fmt::Write as _;

use rustedscreep_core::config::{LabsConfig, MovementConfig, SchedulerConfig};
use rustedscreep_core::economy::{LAB_TARGETS, LabEvent, LabPhase, Labs, Upgrading, lab_goals};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::Snapshot;
use rustedscreep_core::spawning::Spawner;
use screeps::RoomName;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A lab, for `runReaction` (engine game/structures.js from L317 at a90d26f, V403). Bound
    /// here with `catch`, not through screeps-game-api's `run_reaction`, which converts the return
    /// with no `catch` (V408).
    type ReactionActor;

    /// `runReaction(lab1, lab2)`: OK, or one of V403's error codes; an OK call is billed.
    #[wasm_bindgen(method, catch, js_name = runReaction)]
    fn run_reaction(this: &ReactionActor, lab1: &JsValue, lab2: &JsValue) -> Result<f64, JsValue>;
}

/// The rooms' plans and requests: Critical, with the other spawn requests.
const LABS_JOB: Job = Job {
    name: "labs",
    tier: Tier::Critical,
};

/// The reactions and the hauler's work: Standard (hauling's tier).
const LABS_WORK_JOB: Job = Job {
    name: "labs work",
    tier: Tier::Standard,
};

/// The act step's reactions, for its faults.
const REACTION_JOB: &str = "lab reactions";

/// The lab glue's state for one global.
#[derive(Default)]
pub struct LabsGlue {
    core: Labs,
    job: JobState,
    work_job: JobState,
    /// Whether this tick runs the Standard tier: the hauler acts only then, so only then is its
    /// room read every tick.
    standard: bool,
}

impl LabsGlue {
    /// Starts the tick: the rooms, their reads and their haulers from the snapshot.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &LabsConfig) {
        self.core.begin(snapshot, cfg);
    }

    /// The Critical `labs` job (not run with no room), then the upgraders' upkeep.
    pub fn plan<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        spawner: &mut Spawner,
        upgrading: &mut Upgrading,
        cfg: &LabsConfig,
    ) {
        let standard = tick.allows(Tier::Standard);
        self.standard = standard;
        let units = self.core.units();
        if units > 0 {
            let Self { core, job, .. } = self;
            tick.run_units(&LABS_JOB, job, units, |unit, _commit| {
                core.plan_room(unit, snapshot, spawner, cfg, standard)
            });
        }
        for &(room, energy) in self.core.upkeeps() {
            upgrading.set_labs(room, energy);
        }
    }

    /// The Standard `labs work` job (not run with no room to work).
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &LabsConfig,
        mcfg: &MovementConfig,
    ) {
        if !self.core.has_work() {
            return;
        }
        let units = self.core.units();
        let Self { core, work_job, .. } = self;
        tick.run_units(&LABS_WORK_JOB, work_job, units, |unit, commit| {
            core.work_room(unit, snapshot, movement, buffer, cfg, mcfg, commit)
        });
    }

    /// The rooms that want labs built, for the placement.
    pub fn wants_labs(&mut self) -> &[RoomName] {
        self.core.wants_labs()
    }

    /// The rooms the next snapshot reads for their labs.
    pub fn reads(&mut self, cfg: &LabsConfig) -> &[RoomName] {
        self.core.reads(cfg, self.standard)
    }

    /// Calls `runReaction` for this tick's reactions and records each outcome. Call in the act
    /// step, after the link sends and before the creep intents. A call that throws, or an order
    /// with no JS object for one of its labs, is a fault under `lab reactions`; a refusal holds the
    /// room's reactions for `labs.read_ticks` and is logged at info when it starts the hold.
    pub fn issue(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        lab_cfg: &LabsConfig,
    ) {
        for index in 0..self.core.orders().len() {
            let Some(&order) = self.core.orders().get(index) else {
                break;
            };
            let [a, b] = order.source_rows;
            let (Some(lab), Some(s1), Some(s2)) = (
                js.structures.get(order.lab_row as usize),
                js.structures.get(a as usize),
                js.structures.get(b as usize),
            ) else {
                fault(sched, cfg, snapshot, "no JS object for a lab row", index);
                log.missing();
                continue;
            };
            let call = lab
                .unchecked_ref::<ReactionActor>()
                .run_reaction(s1.as_ref(), s2.as_ref());
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    fault(sched, cfg, snapshot, "runReaction threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if self.core.record_reaction(index, outcome, lab_cfg) {
                let what = match outcome {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    Outcome::Threw => "threw",
                    Outcome::Ok => "ok",
                };
                log::info!(
                    "rustedscreep labs: {}'s reaction to {} was refused at tick {}: {what}; the \
                     labs wait {} ticks",
                    order.room,
                    order.product,
                    snapshot.tick.time,
                    lab_cfg.read_ticks
                );
            }
        }
    }

    /// This tick's job starts and ends, at info.
    pub fn log_events(&self) {
        for &e in self.core.events() {
            match e {
                LabEvent::Started { room, step } => log::info!(
                    "rustedscreep labs: {room} makes {} {} from {} and {}",
                    step.amount,
                    step.product,
                    step.reagents[0],
                    step.reagents[1]
                ),
                LabEvent::Done {
                    room,
                    product,
                    reactions,
                } => log::info!(
                    "rustedscreep labs: {room} finished its {product} job ({reactions} reactions)"
                ),
            }
        }
    }

    /// The `rustedscreep.labs()` summary: the targets and their goals, then per room what its
    /// labs do, what its last read saw, its roles, its job, its hauler and its counters since the
    /// global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &LabsConfig) -> String {
        let rooms = self.core.rooms();
        let mut text = format!(
            "rustedscreep labs at tick {}: {} (batch min {}, hauler carry {}) | targets",
            snapshot.tick.time,
            if cfg.enabled { "on" } else { "off" },
            cfg.batch_min,
            cfg.hauler_carry
        );
        for (t, goal) in LAB_TARGETS.iter().zip(lab_goals(cfg)) {
            let _ = write!(text, " {t} {goal}");
        }
        let _ = write!(text, " | rooms {}", rooms.len());
        for r in rooms {
            let _ = write!(
                text,
                " | {}: {}{}",
                r.name,
                r.why.as_str(),
                if r.wants { ", wants labs" } else { "" }
            );
            if let Some(s) = r.seen {
                let _ = write!(text, "; read at {}: {} labs", r.seen_time, r.labs.len());
                if let Some(t) = s.terminal {
                    let _ = write!(text, ", terminal free {}", t.free);
                    let held: Vec<String> = rustedscreep_core::snapshot::LAB_RESOURCES
                        .iter()
                        .zip(s.stock)
                        .filter(|&(_, n)| n > 0)
                        .map(|(k, n)| format!("{k} {n}"))
                        .collect();
                    if !held.is_empty() {
                        let _ = write!(text, " ({})", held.join(", "));
                    }
                }
            }
            if let Some([a, b]) = r.sources {
                let _ = write!(text, "; sources {a} and {b}, outputs {}", r.outputs.len());
            }
            if let Some(j) = r.job {
                let _ = write!(
                    text,
                    "; job {} {} from {} and {} ({}, loaded {} and {}, {} reactions)",
                    j.amount,
                    j.product,
                    j.reagents[0],
                    j.reagents[1],
                    match j.phase {
                        LabPhase::Run => "running",
                        LabPhase::Empty => "emptying",
                    },
                    j.loaded[0],
                    j.loaded[1],
                    j.reactions
                );
            }
            let st = r.stats;
            let _ = write!(
                text,
                "; haulers {}; since boot: reads {}, requests {}, jobs {}, reactions {}, refused {}, withdraws {}, transfers {}",
                r.haulers.len(),
                st.reads,
                st.requests,
                st.jobs,
                st.reactions,
                st.refused,
                st.withdraws,
                st.transfers
            );
        }
        text
    }
}

/// A fault of the act step's reactions (`REACTION_JOB`), rate-limited like a job's.
fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: REACTION_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
