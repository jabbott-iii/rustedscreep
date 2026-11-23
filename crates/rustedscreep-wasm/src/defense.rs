//! Defense's glue (ROADMAP P7, over `rustedscreep_core::defense`): the tick's threat assessment, the
//! towers' job and calls, and their log lines.
//!
//! - The assessment (P7.1) runs every tick a hostile creep stands in an owned room or a threat is
//!   open, right after the snapshot, as the other `begin` steps do: it is Critical work (defense)
//!   that needs no CPU gate, since it is bounded by the few hostiles in the bot's rooms (at most
//!   `MAX_CREEP_SIZE` parts each, plus a short search for a boosted creep's effective hits). On a
//!   tick with neither, nothing runs, and no clock sample is taken. After it, the rooms with an open
//!   threat go to the snapshot ([`JsWorld::set_threat_rooms`]), which reads own creeps' hits there
//!   on the next tick.
//! - `towers` (Critical; P7.2, `defense::TowerControl`): one unit per owned room with a tower that
//!   holds energy for a shot and a hostile or a damaged own creep in it; none, and no clock sample,
//!   on a tick without. It runs right after movement's first reads, before the other Critical jobs.
//! - The towers' calls go out in the act step, right after the spawn orders: `attack(creep)` or
//!   `heal(creep)` per order, one JS call each, billed when OK (V297, V298).
//! - The log lines report each threat's start and changes (warn) and its end (info), the towers'
//!   new target, a hold of fire with a dangerous hostile in the room, and a target the shot check
//!   gives up on (warn): the live
//!   record of the event and the bot's response that the P7 gate asks for.
//!
//! JS calls of its own: the towers' calls; and on a tick a threat starts or changes, or the towers
//! take a new target, each named hostile's `owner` and its `username` (4 JS calls a creep in the
//! generated glue: the two getters, the object check, and the string copy), for the line alone and
//! only while warn is on (V268, as the remote threat line does). Everything else comes from the
//! snapshot.

use rustedscreep_core::config::{DefenseConfig, SchedulerConfig};
use rustedscreep_core::defense::{
    RoomThreat, ThreatEvent, Threats, TowerControl, TowerEvent, TowerOrder,
};
use rustedscreep_core::intents::{ActLog, Outcome, error_name};
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::Snapshot;
use screeps::RoomName;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};

use crate::remote::{NameOf, OwnerOf};
use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A tower, for `attack` and `heal` (engine game/structures.js, V297). Bound here, not through
    /// screeps-game-api's methods, which unwrap the return code (as V111 found for the others).
    type TowerActor;

    /// `attack(target)`: OK, or ERR_NOT_OWNER, ERR_INVALID_TARGET, ERR_NOT_ENOUGH_ENERGY,
    /// ERR_RCL_NOT_ENOUGH (V297); an OK call is billed.
    #[wasm_bindgen(method, catch, js_name = attack)]
    fn attack(this: &TowerActor, target: &JsValue) -> Result<f64, JsValue>;

    /// `heal(target)`, the same codes.
    #[wasm_bindgen(method, catch, js_name = heal)]
    fn heal(this: &TowerActor, target: &JsValue) -> Result<f64, JsValue>;
}

/// The towers' plan: defense, Critical.
const TOWER_JOB: Job = Job {
    name: "towers",
    tier: Tier::Critical,
};

/// The job name the act step's tower faults are logged under.
const TOWER_CALLS: &str = "tower calls";

/// Defense's glue state for one global.
#[derive(Default)]
pub struct DefenseGlue {
    core: Threats,
    towers: TowerControl,
    tower_job: JobState,
    /// Tower calls the game refused since the global started, and the tick the last was logged.
    refused: u32,
    refused_logged: Option<u32>,
}

impl DefenseGlue {
    /// Assesses this tick's hostile creeps in owned rooms and updates the rooms' threats (module
    /// docs), then gives the snapshot the rooms with an open threat. Call after the snapshot, before
    /// the scheduler's jobs.
    pub fn begin(&mut self, snapshot: &Snapshot, js: &mut JsWorld, cfg: &DefenseConfig) {
        if self.core.needed(snapshot) {
            self.core.assess(snapshot, cfg);
        } else {
            self.core.clear();
        }
        // Only rooms with a tower: nothing heals elsewhere.
        let towered = |room: RoomName| {
            let st = &snapshot.structures;
            snapshot.towers.rows().iter().any(|&r| {
                usize::try_from(r)
                    .ok()
                    .and_then(|i| st.positions().get(i))
                    .is_some_and(|p| p.room_name() == room)
            })
        };
        js.set_threat_rooms(self.core.open_rooms().filter(|&r| towered(r)));
    }

    /// The Critical `towers` job: one unit per room to plan (module docs).
    pub fn plan_towers<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        cfg: &DefenseConfig,
    ) {
        let Self {
            core,
            towers,
            tower_job,
            ..
        } = self;
        towers.begin(snapshot, core, cfg);
        let count = towers.unit_count();
        if count > 0 {
            tick.run_units(&TOWER_JOB, tower_job, count, |unit, _| {
                towers.plan_room(unit, snapshot, core, cfg)
            });
        }
        towers.finish(snapshot.tick.time, core);
    }

    /// Calls `attack` or `heal` for this tick's tower orders and records each outcome. Call in the
    /// act step, right after the spawn orders. A refused call is counted and logged at info at most once per
    /// `defense.threat_log_ticks`; a call that throws, or an order with no JS object, is a fault
    /// under `tower calls`.
    pub fn issue_towers(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        dcfg: &DefenseConfig,
    ) {
        for (index, order) in self.towers.orders().iter().enumerate() {
            let (tower, target, heal) = match *order {
                TowerOrder::Attack { tower, target } => (tower, target, false),
                TowerOrder::Heal { tower, target } => (tower, target, true),
            };
            let (Some(t), Some(c)) = (
                js.structures.get(tower as usize),
                js.creeps.get(target as usize),
            ) else {
                tower_fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for a tower order",
                    index,
                );
                log.missing();
                continue;
            };
            let actor = t.unchecked_ref::<TowerActor>();
            let call = if heal {
                actor.heal(c.as_ref())
            } else {
                actor.attack(c.as_ref())
            };
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    tower_fault(sched, cfg, snapshot, "a tower call threw", index);
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome == Outcome::Ok {
                continue;
            }
            self.refused = self.refused.saturating_add(1);
            let now = snapshot.tick.time;
            let due = self
                .refused_logged
                .is_none_or(|t| now.saturating_sub(t) >= dcfg.threat_log_ticks);
            if due {
                self.refused_logged = Some(now);
                let what = match outcome {
                    Outcome::Failed(code) => error_name(code),
                    Outcome::Unknown => "unknown return",
                    Outcome::Threw => "threw",
                    Outcome::Ok => "ok",
                };
                log::info!(
                    "rustedscreep defense: a tower's {} was refused at tick {now}: {what} ({} refused \
                     since the global started)",
                    if heal { "heal" } else { "attack" },
                    self.refused
                );
            }
        }
    }

    /// Logs this tick's threat and tower events.
    pub fn log_events(&self, snapshot: &Snapshot, js: &JsWorld, cfg: &DefenseConfig) {
        for event in self.core.events() {
            match *event {
                ThreatEvent::Started { room } => {
                    if !log::log_enabled!(log::Level::Warn) {
                        continue;
                    }
                    let Some(r) = self.core.room(room) else {
                        continue;
                    };
                    log::warn!(
                        "rustedscreep defense: {room} threatened by {}",
                        self.describe(r, snapshot, js)
                    );
                }
                ThreatEvent::Changed { room, since } => {
                    if !log::log_enabled!(log::Level::Warn) {
                        continue;
                    }
                    let Some(r) = self.core.room(room) else {
                        continue;
                    };
                    log::warn!(
                        "rustedscreep defense: {room} threat (since tick {since}) now {}",
                        self.describe(r, snapshot, js)
                    );
                }
                ThreatEvent::Ended { room, since, last } => {
                    log::info!(
                        "rustedscreep defense: {room} threat over: no dangerous hostile seen since \
                         tick {last} (it started at tick {since})"
                    );
                }
            }
        }
        if !log::log_enabled!(log::Level::Warn) {
            return;
        }
        for event in self.towers.events() {
            match *event {
                TowerEvent::Fire {
                    room,
                    target,
                    towers,
                    damage,
                    heal,
                    ticks,
                } => {
                    let row = usize::try_from(target).unwrap_or(usize::MAX);
                    let at = snapshot
                        .creeps
                        .positions()
                        .get(row)
                        .map(|p| format!("({}, {})", p.x().u8(), p.y().u8()))
                        .unwrap_or_else(|| String::from("(?)"));
                    log::warn!(
                        "rustedscreep defense: {room} towers fire: {towers} tower(s) on the \
                         hostile at {at} of {}, {damage} damage a tick against {heal} heal, dead \
                         in {ticks} tick(s) by the sim",
                        owner_of(js, row)
                    );
                }
                TowerEvent::Distrust {
                    room,
                    target,
                    misses,
                } => {
                    let at = snapshot
                        .creeps
                        .positions()
                        .get(usize::try_from(target).unwrap_or(usize::MAX))
                        .map(|p| format!("({}, {})", p.x().u8(), p.y().u8()))
                        .unwrap_or_else(|| String::from("(?)"));
                    log::warn!(
                        "rustedscreep defense: {room} towers stop firing at the hostile at {at}: its \
                         hits came out above the sim's on {misses} ticks in a row (a rampart on its \
                         tile, a power effect, or an inactive tower)"
                    );
                }
                TowerEvent::Hold { room, dangerous } => {
                    log::warn!(
                        "rustedscreep defense: {room} towers hold fire: no hostile dies within {} \
                         ticks by the sim ({dangerous} dangerous in the room)",
                        cfg.max_kill_ticks
                    );
                }
            }
        }
    }

    /// A room's threat for a log line: its dangerous hostiles and their owners, then the room's sums.
    /// Reads each dangerous hostile's `owner.username` (module docs).
    fn describe(&self, r: &RoomThreat, snapshot: &Snapshot, js: &JsWorld) -> String {
        let mut owners: Vec<String> = Vec::new();
        for h in self.core.hostiles(r) {
            if !h.power.dangerous() {
                continue;
            }
            let row = usize::try_from(h.row).unwrap_or(usize::MAX);
            // The rows are this tick's, so the JS object is the creep the row describes.
            if snapshot.creeps.handles().get(row).is_none() {
                continue;
            }
            let name = owner_of(js, row);
            if !owners.contains(&name) {
                owners.push(name);
            }
        }
        let p = r.power;
        let unread = if r.unread > 0 {
            format!("; {} bodies unread", r.unread)
        } else {
            String::new()
        };
        format!(
            "{} dangerous hostile creep(s) of {} ({} in the room, {} boosted): melee {}, ranged {}, \
             heal {}, dismantle {} a tick, {} CLAIM; hits {}, effective {}{unread}",
            r.dangerous,
            owners.join(", "),
            r.hostiles,
            r.boosted,
            p.melee_damage(),
            p.ranged_damage(),
            p.heal_adjacent(),
            p.dismantle_damage(),
            p.claim,
            r.hits,
            r.effective_hits,
        )
    }
}

/// The username of the creep at `row`'s owner, read as plain values (V268), or "unknown".
fn owner_of(js: &JsWorld, row: usize) -> String {
    js.creeps
        .get(row)
        .and_then(|creep| {
            let o = creep.unchecked_ref::<OwnerOf>().owner_value();
            o.is_object()
                .then(|| o.unchecked_ref::<NameOf>().username_value().as_string())
                .flatten()
        })
        .unwrap_or_else(|| String::from("unknown"))
}

fn tower_fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: TOWER_CALLS,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
