//! Terminal balancing's glue (ROADMAP P9.4, ADR-047, over `rustedscreep_core::economy::Terminals`):
//! its job, the sends' `send` calls, the terminals the haulers fill with a send's fee, the
//! terminals the next snapshot reads, its log lines, and the `rustedscreep.terminals()` console
//! summary.
//!
//! - After the factory's Critical job, before the hauling job: which members' sources are hauled
//!   (last tick's hauling plans) and the banks of the sends waiting for their fee, merged with the
//!   factory's for `Hauling::set_banks` ([`TerminalsGlue::banks`]). No JS call.
//! - `terminals` (Standard): one unit, the whole plan on a tick every member was read, or the
//!   waiting fees on another. No JS call.
//! - In the act step, `send(resourceType, amount, destination)` for each order
//!   ([`TerminalsGlue::issue`]).
//! - After the jobs: `Terminals::reads` gives the next snapshot the terminals it reads in full or
//!   for their energy alone (`JsWorld::set_terminal_reads`).

use std::fmt::Write as _;

use js_sys::JsString;
use rustedscreep_core::config::{SchedulerConfig, TerminalsConfig};
use rustedscreep_core::economy::{Hauling, TerminalWait, Terminals};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{BALANCED_RESOURCES, Snapshot, StructureHandle};
use screeps::{Position, ResourceType, RoomName};
use wasm_bindgen::{JsCast, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A terminal, for `send` (engine game/structures.js L714 at a90d26f, V435). Bound here with
    /// `catch`, not through screeps-game-api's `send`, which converts the return with no `catch`
    /// and unwraps an unknown code (V444).
    type SendActor;

    /// `send(resourceType, amount, destination)`: OK, or one of V435's error codes; an OK call is
    /// billed. No description is passed.
    #[wasm_bindgen(method, catch, js_name = send)]
    fn send(
        this: &SendActor,
        resource: ResourceType,
        amount: u32,
        destination: &JsString,
    ) -> Result<f64, wasm_bindgen::JsValue>;
}

/// The plan and the waiting fees: Standard (hauling's tier).
const TERMINALS_JOB: Job = Job {
    name: "terminals",
    tier: Tier::Standard,
};

/// The act step's sends, for its faults.
const SEND_JOB: &str = "terminal sends";

/// Terminal balancing's glue state for one global.
#[derive(Default)]
pub struct TerminalsGlue {
    core: Terminals,
    job: JobState,
    /// The factory's banks and the terminals', scratch for `Hauling::set_banks`.
    banks: Vec<(RoomName, StructureHandle, Position, u32)>,
    /// Whether this tick's `terminals` job ran: only then are the terminals read for the next.
    standard: bool,
}

impl TerminalsGlue {
    /// Starts the tick: the members and this tick's reads from the snapshot.
    pub fn begin(&mut self, snapshot: &Snapshot, cfg: &TerminalsConfig) {
        self.core.begin(snapshot, cfg);
    }

    /// The banks for `Hauling::set_banks`: the factory's (`factory`) and, while `standard` (the
    /// Standard tier runs this tick), the terminals' whose send waits for its fee in a room whose
    /// sources are hauled (`hauling` is last tick's plans); the hauling keeps the larger want of a
    /// room named twice. No JS call.
    pub fn banks(
        &mut self,
        hauling: &Hauling,
        factory: &[(RoomName, StructureHandle, Position, u32)],
        standard: bool,
        cfg: &TerminalsConfig,
    ) -> &[(RoomName, StructureHandle, Position, u32)] {
        let hauled = |room: RoomName| {
            hauling.room_states().any(|(name, plan, _)| {
                name == room && plan.is_some_and(|p| p.sources.iter().any(|s| s.want > 0))
            })
        };
        self.core.set_hauled(hauled, standard, cfg);
        self.banks.clear();
        self.banks.extend_from_slice(factory);
        self.banks.extend_from_slice(self.core.banks());
        &self.banks
    }

    /// The Standard `terminals` job (not run with fewer than two members). `buffer` holds the
    /// creep intents the jobs before it queued, so a send waits while a creep withdraws from its
    /// terminal. The next tick's reads are asked for only when the job ran (not when the budget
    /// cut it), so a cut job does not read every terminal on each tick it is cut.
    pub fn work<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        buffer: &IntentBuffer,
        cfg: &TerminalsConfig,
    ) {
        self.standard = false;
        if self.core.rooms().len() < 2 {
            return;
        }
        let Self { core, job, .. } = self;
        let report = tick.run_units(&TERMINALS_JOB, job, 1, |_, commit| {
            core.run(cfg, buffer, commit)
        });
        self.standard = report.ran > 0;
    }

    /// Gives the next snapshot the terminals it reads in full, and those it reads for their
    /// energy alone.
    pub fn set_reads(&mut self, js: &mut JsWorld, cfg: &TerminalsConfig) {
        let (full, energy) = self.core.reads(cfg, self.standard);
        js.set_terminal_reads(full, energy);
    }

    /// Calls `send` for this tick's orders and records each outcome. Call in the act step, after
    /// the factory's batches and before the creep intents. A call that throws, or an order with no
    /// JS object for its terminal, is a fault under `terminal sends`; an accepted send is logged
    /// at info; a refusal holds the giver for `terminals.read_ticks` and is logged at info when it
    /// starts the hold.
    pub fn issue(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        terminals_cfg: &TerminalsConfig,
    ) {
        for index in 0..self.core.orders().len() {
            let Some(&order) = self.core.orders().get(index) else {
                break;
            };
            let Some(terminal) = js.structures.get(order.row as usize) else {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for a terminal row",
                    index,
                );
                log.missing();
                continue;
            };
            let s = order.send;
            let destination = JsString::from(s.to);
            let call = terminal
                .unchecked_ref::<SendActor>()
                .send(s.kind, s.amount, &destination);
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    fault(sched, cfg, snapshot, "send threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome == Outcome::Ok {
                log::info!(
                    "rustedscreep terminals: {} sends {} {} to {} (fee {}, range {})",
                    s.from,
                    s.amount,
                    s.kind,
                    s.to,
                    s.fee,
                    s.range
                );
            }
            if self.core.record_send(index, outcome, terminals_cfg) {
                let what = match outcome {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    Outcome::Threw => "threw",
                    Outcome::Ok => "ok",
                };
                log::info!(
                    "rustedscreep terminals: {}'s send of {} {} to {} was refused at tick {}: \
                     {what}; it waits {} ticks",
                    s.from,
                    s.amount,
                    s.kind,
                    s.to,
                    snapshot.tick.time,
                    terminals_cfg.read_ticks
                );
            }
        }
    }

    /// The `rustedscreep.terminals()` summary: the settings, the next plan, then per member its
    /// last read, the send it waits to make, and its counters since the global started.
    pub fn summary(&self, snapshot: &Snapshot, cfg: &TerminalsConfig) -> String {
        let rooms = self.core.rooms();
        let mut text = format!(
            "rustedscreep terminals at tick {}: {} (keep {}, send min {}, energy keep {}, max \
             range {}, read every {}) | members {}",
            snapshot.tick.time,
            if cfg.enabled { "on" } else { "off" },
            cfg.mineral_keep,
            cfg.send_min,
            cfg.energy_keep,
            cfg.max_range,
            cfg.read_ticks,
            rooms.len()
        );
        if rooms.len() >= 2 {
            let _ = write!(
                text,
                " | plans {}, next at {}",
                self.core.plans(),
                self.core.next_plan()
            );
        } else {
            text.push_str(" | no plan: fewer than two terminals");
        }
        for r in rooms {
            let _ = write!(text, " | {}", r.name);
            match r.seen {
                Some(s) => {
                    let _ = write!(
                        text,
                        ": read at {}, cooldown {}, free {}, energy {}",
                        r.seen_time, s.cooldown, s.free, s.energy
                    );
                    let held: Vec<String> = BALANCED_RESOURCES
                        .iter()
                        .zip(s.stock)
                        .filter(|&(_, n)| n > 0)
                        .map(|(k, n)| format!("{k} {n}"))
                        .collect();
                    if !held.is_empty() {
                        let _ = write!(text, " ({})", held.join(", "));
                    }
                }
                None => text.push_str(": not read"),
            }
            if let Some(w) = r.waiting {
                let _ = write!(
                    text,
                    "; {} to send {} {} to {} (fee {})",
                    w.wait.as_str(),
                    w.amount,
                    w.kind,
                    w.to,
                    w.fee
                );
                if w.wait == TerminalWait::Fee && r.bank {
                    text.push_str(", terminal takes energy");
                }
            }
            if r.held_until > snapshot.tick.time {
                let _ = write!(text, "; held until {}", r.held_until);
            }
            let st = r.stats;
            let _ = write!(
                text,
                "; since boot: reads {}, energy reads {}, sends {} ({} units, fees {}), received \
                 {}, refused {}",
                st.reads, st.energy_reads, st.sends, st.sent, st.fees, st.received, st.refused
            );
        }
        text
    }
}

/// A fault of the act step's sends (`SEND_JOB`), rate-limited like a job's.
fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: SEND_JOB,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
