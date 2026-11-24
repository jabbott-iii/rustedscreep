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
//! - `safe mode` (Critical; P7.4, ADR-035, `defense::SafeModes`): one unit per owned room with a
//!   dangerous hostile this tick, right after the towers' plan (it reads their kill times); none,
//!   and no clock sample, on a tick without. With `defense.safe_mode` off, no unit.
//! - `defenders` (Critical; P7.5, ADR-036, `defense::Defenders`): one unit per owned room with a
//!   dangerous hostile this tick, after the safe-mode triggers and before the spawn plan: the
//!   room's need, its spawn requests and this tick's target. `defender work` (Critical): one unit
//!   per defender of the bot: its `rangedAttack`, its move, or a give-way. Neither runs, and no
//!   clock sample is taken, on a tick with no hostile in an owned room and every defender home;
//!   each defender then only gives way. The defenders' list goes to the towers' plan first, whose
//!   sim counts them.
//! - The calls go out in the act step, right after the spawn orders: first `activateSafeMode()` on
//!   a room's controller (at most once per room per threat; billed when OK, a refusal is free, V325),
//!   then the towers' `attack(creep)` or `heal(creep)` per order, one JS call each, billed when OK
//!   (V297, V298).
//! - The log lines report each threat's start and changes (warn) and its end (info), the towers'
//!   new target, a hold of fire with a dangerous hostile in the room, and a target the shot check
//!   gives up on (warn), a safe-mode call's outcome with its reason and a trigger the room cannot
//!   meet (warn), a threat in a room already in its safe mode (info), and the defenders a room
//!   needs and a hostile beyond them (warn): the live record of the event and the bot's response
//!   that the P7 gate asks for.
//!
//! JS calls of its own: the towers' calls; and on a tick a threat starts or changes, or the towers
//! take a new target, each named hostile's `owner` and its `username` (4 JS calls a creep in the
//! generated glue: the two getters, the object check, and the string copy), for the line alone and
//! only while warn is on (V268, as the remote threat line does). Everything else comes from the
//! snapshot.

use rustedscreep_core::config::{DefenseConfig, MovementConfig, SchedulerConfig};
use rustedscreep_core::defense::{
    Blocked, DefenderEvent, Defenders, Reason, RoomThreat, SafeModeEvent, SafeModes, ThreatEvent,
    Threats, TowerControl, TowerEvent, TowerOrder,
};
use rustedscreep_core::intents::{ActLog, IntentBuffer, Outcome, error_name};
use rustedscreep_core::movement::Movement;
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::Snapshot;
use rustedscreep_core::spawning::Spawner;
use screeps::{RoomName, StructureType};
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

    /// An own controller, for `activateSafeMode` (V325). Bound here: the crate's method unwraps an
    /// unexpected return code (V336).
    type SafeModeActor;

    /// `activateSafeMode()`: OK, or ERR_NOT_OWNER, ERR_BUSY, ERR_NOT_ENOUGH_RESOURCES, ERR_TIRED
    /// (V325); only an OK call is billed.
    #[wasm_bindgen(method, catch, js_name = activateSafeMode)]
    fn activate_safe_mode(this: &SafeModeActor) -> Result<f64, JsValue>;
}

/// The towers' plan: defense, Critical.
const TOWER_JOB: Job = Job {
    name: "towers",
    tier: Tier::Critical,
};

/// The safe-mode triggers (P7.4): defense, Critical.
const SAFE_MODE_JOB: Job = Job {
    name: "safe mode",
    tier: Tier::Critical,
};

/// The defenders' plan (P7.5): defense, Critical.
const DEFENDER_JOB: Job = Job {
    name: "defenders",
    tier: Tier::Critical,
};

/// The defenders' shots and moves (P7.5): defense, Critical.
const DEFENDER_WORK_JOB: Job = Job {
    name: "defender work",
    tier: Tier::Critical,
};

/// The job name the act step's tower faults are logged under.
const TOWER_CALLS: &str = "tower calls";

/// The job name the act step's safe-mode faults are logged under.
const SAFE_MODE_CALLS: &str = "safe mode calls";

/// Defense's glue state for one global.
#[derive(Default)]
pub struct DefenseGlue {
    core: Threats,
    towers: TowerControl,
    tower_job: JobState,
    safe: SafeModes,
    safe_job: JobState,
    defenders: Defenders,
    defender_job: JobState,
    defender_work_job: JobState,
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
        // Only rooms with a tower: nothing heals elsewhere, and no tower's sim counts a defender
        // there (P7.5 counts one at its whole body).
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
            safe,
            safe_job,
            defenders,
            ..
        } = self;
        // P7.5: the defenders first, whom the towers' sim counts.
        defenders.begin(snapshot, core);
        towers.set_allies(defenders.allies());
        towers.begin(snapshot, core, cfg);
        let count = towers.unit_count();
        if count > 0 {
            tick.run_units(&TOWER_JOB, tower_job, count, |unit, _| {
                towers.plan_room(unit, snapshot, core, cfg)
            });
        }
        towers.finish(snapshot.tick.time, core);
        // P7.4: the safe-mode triggers, after the towers' plan (their kill times and shot check).
        safe.begin(snapshot, core, cfg);
        let count = safe.unit_count();
        if count > 0 {
            tick.run_units(&SAFE_MODE_JOB, safe_job, count, |unit, _| {
                safe.plan_room(unit, snapshot, core, towers, cfg)
            });
        }
        safe.finish(snapshot.tick.time, core);
    }

    /// The Critical `defenders` job (one unit per owned room with a dangerous hostile), then the
    /// Critical `defender work` job (one unit per defender), P7.5 (module docs). Call after
    /// [`Self::plan_towers`], before the spawn plan.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_defenders<C: Clock>(
        &mut self,
        tick: &mut TickCpu<'_, C>,
        snapshot: &Snapshot,
        movement: &mut Movement,
        spawner: &mut Spawner,
        buffer: &mut IntentBuffer,
        cfg: &DefenseConfig,
        mcfg: &MovementConfig,
    ) {
        let Self {
            core,
            towers,
            defenders,
            defender_job,
            defender_work_job,
            ..
        } = self;
        let rooms = defenders.unit_count();
        if rooms > 0 {
            tick.run_units(&DEFENDER_JOB, defender_job, rooms, |unit, commit| {
                defenders.plan_room(
                    unit, snapshot, core, towers, movement, spawner, cfg, mcfg, commit,
                )
            });
        }
        let members = defenders.member_count();
        if members > 0 && defenders.idle(core) {
            // Nothing to fight and nobody away: each holds and gives way, with no clock reading.
            defenders.hold_all(snapshot, movement);
        } else if members > 0 {
            tick.run_units(
                &DEFENDER_WORK_JOB,
                defender_work_job,
                members,
                |unit, commit| {
                    defenders.work(unit, snapshot, core, towers, movement, buffer, mcfg, commit)
                },
            );
        }
        defenders.finish(core);
    }

    /// Calls `activateSafeMode` for this tick's safe-mode orders (at most one per room per threat)
    /// and logs each outcome with its reason at warn. Call in the act step, right after the spawn
    /// orders. A call that throws, or an order with no JS object, is a fault under
    /// `safe mode calls`.
    pub fn issue_safe_mode(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        dcfg: &DefenseConfig,
    ) {
        let now = snapshot.tick.time;
        for index in 0..self.safe.orders().len() {
            // Copied out, so `not_accepted` may update the room's state below.
            let Some(&order) = self.safe.orders().get(index) else {
                continue;
            };
            let Some(c) = js.structures.get(order.controller as usize) else {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    SAFE_MODE_CALLS,
                    "no JS object for a safe-mode order",
                    index,
                );
                log.missing();
                self.safe.not_accepted(order.room, true, dcfg);
                continue;
            };
            let outcome = match c.unchecked_ref::<SafeModeActor>().activate_safe_mode() {
                Ok(value) => Outcome::from_return(value),
                Err(_) => {
                    fault(
                        sched,
                        cfg,
                        snapshot,
                        SAFE_MODE_CALLS,
                        "an activateSafeMode call threw",
                        index,
                    );
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            if outcome != Outcome::Ok {
                // A refusal's reasons last; a call that threw may be made again.
                let retry = matches!(outcome, Outcome::Threw | Outcome::Unknown);
                self.safe.not_accepted(order.room, retry, dcfg);
            }
            if !log::log_enabled!(log::Level::Warn) {
                continue;
            }
            let room = order.room;
            let why = reason_text(&order.reason, js, dcfg);
            if outcome == Outcome::Ok {
                // Accepted; the next read confirms it (the "is in safe mode" line) or not (V338).
                log::warn!(
                    "rustedscreep defense: {room} safe mode activation accepted at tick {now}: {why}"
                );
            } else {
                log::warn!(
                    "rustedscreep defense: {room} safe mode refused at tick {now}: {} ({why})",
                    outcome_name(outcome)
                );
            }
        }
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
                fault(
                    sched,
                    cfg,
                    snapshot,
                    TOWER_CALLS,
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
                    fault(
                        sched,
                        cfg,
                        snapshot,
                        TOWER_CALLS,
                        "a tower call threw",
                        index,
                    );
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
                let what = outcome_name(outcome);
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
        for event in self.safe.events() {
            match *event {
                SafeModeEvent::Protected { room, ticks } => {
                    log::info!(
                        "rustedscreep defense: {room} is in safe mode for {ticks} more ticks"
                    );
                }
                SafeModeEvent::NotApplied { room, again } => {
                    if log::log_enabled!(log::Level::Warn) {
                        log::warn!(
                            "rustedscreep defense: {room} safe mode did not take effect (no \
                             activation spent){}",
                            if again {
                                "; calling again"
                            } else {
                                "; no call left in this threat"
                            }
                        );
                    }
                }
                SafeModeEvent::Unavailable {
                    room,
                    reason,
                    blocked,
                } => {
                    if log::log_enabled!(log::Level::Warn) {
                        log::warn!(
                            "rustedscreep defense: {room} needs safe mode but cannot activate it \
                             ({}): {}",
                            blocked_text(&blocked),
                            reason_text(&reason, js, cfg)
                        );
                    }
                }
            }
        }
        if !log::log_enabled!(log::Level::Warn) {
            return;
        }
        let at_row = |row: u32| {
            snapshot
                .creeps
                .positions()
                .get(usize::try_from(row).unwrap_or(usize::MAX))
                .map(|p| format!("({}, {})", p.x().u8(), p.y().u8()))
                .unwrap_or_else(|| String::from("(?)"))
        };
        for event in self.defenders.events() {
            match *event {
                DefenderEvent::Need {
                    room,
                    target,
                    parts,
                    defenders,
                    per_body,
                    cost,
                    ticks,
                    have,
                } => {
                    let asked = if cfg.defenders {
                        ""
                    } else {
                        "; none are requested (defense.defenders is off)"
                    };
                    log::warn!(
                        "rustedscreep defense: {room} needs {defenders} defender(s) of {per_body} \
                         RANGED_ATTACK and {per_body} MOVE ({cost} energy each): the towers alone do \
                         not kill the hostile at {} of {} within {} ticks; with {parts} \
                         RANGED_ATTACK part(s) they do in {ticks} tick(s) by the sim ({have} \
                         alive){asked}",
                        at_row(target),
                        owner_of(js, usize::try_from(target).unwrap_or(usize::MAX)),
                        cfg.max_kill_ticks
                    );
                }
                DefenderEvent::Outmatched {
                    room,
                    target,
                    defenders,
                    per_body,
                } => {
                    log::warn!(
                        "rustedscreep defense: {room} defenders cannot win: the hostile at {} of {} \
                         survives the towers and {defenders} defender(s) of {per_body} \
                         RANGED_ATTACK by the sim; none are requested for it",
                        at_row(target),
                        owner_of(js, usize::try_from(target).unwrap_or(usize::MAX))
                    );
                }
            }
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

/// An outcome's name for a log line.
fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Failed(code) => error_name(code),
        Outcome::Unknown => "unknown return",
        Outcome::Threw => "threw",
        Outcome::Ok => "ok",
    }
}

/// A structure type's name for a log line (the core's four, P7.4).
fn kind_name(kind: StructureType) -> &'static str {
    match kind {
        StructureType::Spawn => "spawn",
        StructureType::Tower => "tower",
        StructureType::Storage => "storage",
        StructureType::Terminal => "terminal",
        _ => "structure",
    }
}

/// What fired a safe-mode trigger, for a log line (ADR-035).
fn reason_text(reason: &Reason, js: &JsWorld, cfg: &DefenseConfig) -> String {
    let towers_say = |towers: u32, kill: Option<u32>, them: &str| match kill {
        Some(k) => format!("the towers kill {them} in {k} tick(s) by the sim"),
        None if towers == 0 && them == "it" => String::from("no tower fires at it this tick"),
        None if towers == 0 => String::from("no tower can fire"),
        None => format!(
            "the towers cannot kill {them} within {} ticks",
            cfg.max_kill_ticks
        ),
    };
    match *reason {
        Reason::Core {
            kind,
            at,
            hits,
            hostiles,
            damage,
            towers,
            kill,
        } => format!(
            "the {} at ({}, {}) has {hits} hits and {hostiles} hostile(s) in reach deal up to \
             {damage} a tick; {}",
            kind_name(kind),
            at.x().u8(),
            at.y().u8(),
            towers_say(towers, kill, "them")
        ),
        Reason::Claim {
            row,
            claim,
            range,
            towers,
            kill,
        } => format!(
            "a hostile of {} with {claim} CLAIM part(s) is {range} tile(s) from the controller; {}",
            owner_of(js, usize::try_from(row).unwrap_or(usize::MAX)),
            towers_say(towers, kill, "it")
        ),
    }
}

/// A reason safe mode cannot be activated, for a log line (V325's checks).
fn blocked_text(blocked: &Blocked) -> String {
    match *blocked {
        Blocked::NotRead => String::from("its state was not read"),
        Blocked::NoCharges => String::from("no activation left"),
        Blocked::Cooldown { ticks } => format!("cooling down for {ticks} more ticks"),
        Blocked::UpgradeBlocked => String::from("the controller was attacked (upgrade blocked)"),
        Blocked::Downgrade { ticks, least } => {
            format!("the downgrade timer is {ticks} ticks, below {least}")
        }
        Blocked::Active { ticks } => format!("already in safe mode for {ticks} more ticks"),
        Blocked::OtherRoom { room } => format!("{room} is in safe mode"),
    }
}

fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    job: &'static str,
    what: &'static str,
    index: usize,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job,
        fault: Fault::new(what, u32::try_from(index).unwrap_or(u32::MAX)),
    };
    sched.faults_mut().record(record, cfg);
}
