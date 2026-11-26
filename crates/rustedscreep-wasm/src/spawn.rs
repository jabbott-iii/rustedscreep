//! The spawn queue's glue (ROADMAP P2's minimal spawn path, P5's global queue; rustedscreep-core
//! `spawning`). After the snapshot, [`SpawnGlue::begin`] opens the tick's requests (the console's
//! included); the Critical `spawn` job plans each serving room with a request and a ready spawn (a
//! room's own spawns serve it; an owned room without one is served by a spawn room within
//! `spawn.help_range` whose capacity fits the body, on a tick that room starts full and the
//! Standard tier runs, P5.1); in the act step, before the creep intents, each order becomes one
//! `spawnCreep(body, name)` call on this tick's JS spawn (`JsWorld`, by structure row).
//!
//! The method is bound here, not through screeps-game-api's `spawn_creep`, which converts each part
//! with its own JS call and unwraps the return code, so an unlisted code would panic and trap
//! (V177). This binding passes the body as one slice of part indices (V178), reads the return as an
//! `f64` and catches a throw.
//!
//! Spawn orders are issued before the creep intents: the processor takes a user's objects in the
//! order of each one's first recorded intent (V179), so the spawn's charge comes before any of the
//! bot's own creeps' intents, a withdraw from the same spawn or extension included. Another
//! player's intents in the room may still be processed first (users go one at a time, V179).
//!
//! OK means the order was recorded, not that a creep will exist: the processor can still drop it
//! silently (a spawn beyond a just-lowered RCL's count, energy another player drained first, V173).
//! Jobs ask again every tick; the console's request is cleared on OK and stands again if no creep
//! of the order's identity is in the next snapshot.
//!
//! `Memory` (ADR-018, ADR-019): a successful call reads `Memory` (its parse and the end-of-tick
//! stringify fall on that tick) and writes `Memory.creeps[name]` (V181); this then deletes that
//! entry, so it never reaches the stored string (V180). With a null `Memory` (a raw string that
//! does not parse, or JSON null), every call that passes its checks throws (V181): this logs the
//! error's message, resets the raw
//! string to `{}` (`RawMemory.set`, which the end-of-tick save keeps, V180), notifies once per
//! global and releases the spawn, which tries again on the next tick; the tick's other orders are
//! not called (each would throw). Nothing here reads `Memory` for state.
//!
//! JS calls per tick: none without orders; per order one `spawnCreep` (its glue decodes the body
//! slice and the name string, then calls back `__wbindgen_free` for the slice, which the call's
//! ABI encoding allocates in wasm, V178), and on OK the entry's delete (8 calls: two key strings,
//! two `Reflect.get`, two object checks, the name and `Reflect.deleteProperty`) and one log line
//! (a `String` from the logger). A throw adds the message read (an object check, the key string,
//! `Reflect.get`, the string copy), the `Memory` check (the key string, `Reflect.get`, a null check)
//! and, on a null `Memory`, `RawMemory.set` and once per global `Game.notify`, with warning lines.
//! So an order tick allocates once per order for the body, plus the line on OK and a hold on a first
//! refusal; a tick without orders allocates nothing.

use std::fmt::Write as _;

use js_sys::{Object, Reflect};
use rustedscreep_core::config::{SchedulerConfig, SpawnConfig};
use rustedscreep_core::economy::sizing::smallest_worker;
use rustedscreep_core::intents::{ActLog, Outcome, error_name};
use rustedscreep_core::scheduler::{
    Clock, Fault, FaultRecord, Job, JobState, Scheduler, TickCpu, Tier,
};
use rustedscreep_core::snapshot::{Snapshot, parse_room_name};
use rustedscreep_core::spawning::{
    Body, BodySpec, CreepName, EnergyBudget, Identity, Role, SpawnEvent, SpawnRequest, Spawner,
};
use screeps::{Part, RoomName};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::consts_check::GLOBAL_THIS;
use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A spawn, for `spawnCreep` (engine structures.js L1063-1217, V171, V181).
    type SpawnActor;

    /// `spawnCreep(body, name)` without options. The body crosses as one slice of part indices that
    /// the glue maps to strings (V178).
    #[wasm_bindgen(method, catch, js_name = spawnCreep)]
    fn spawn_creep(this: &SpawnActor, body: &[Part], name: &str) -> Result<f64, JsValue>;

    /// `RawMemory.set(value)`: stores the raw `Memory` string; throws on a non-string or above
    /// 2 MB (driver runtime.js L108-120, V180).
    #[wasm_bindgen(js_namespace = RawMemory, js_name = set, catch)]
    fn raw_memory_set(value: &str) -> Result<(), JsValue>;

    /// `Game.notify(message)`.
    #[wasm_bindgen(js_namespace = Game, js_name = notify, catch)]
    fn game_notify(message: &str) -> Result<(), JsValue>;
}

/// What the one notification per global says when `Memory` was reset (ADR-019).
const MEMORY_RESET_NOTICE: &str = "rustedscreep: Memory was null (its raw string did not parse, or \
     was JSON null), so spawnCreep threw; the raw Memory was reset to {} so spawning can go on \
     (ADR-019)";

/// Spawning is Critical (CLAUDE.md): it runs whenever the tick runs.
const JOB: Job = Job {
    name: "spawn",
    tier: Tier::Critical,
};

/// The job name the act step's spawn faults are logged under.
const ACT_JOB: &str = "spawn act";

/// Whether the console spawns `role`: a worker only. A miner's body comes from its source
/// (`economy::mining`, P3.1), a hauler's from its source's yield and trip (`economy::hauling`,
/// P3.3), an upgrader's from the energy that reaches the controller (`economy::upgrading`, P3.4),
/// a remote role's needs its remote room, which the console does not name (P6.2), the signer's
/// comes from its trip (ADR-030), and the mineral creeps' from their room's mineral plan (P9.1).
fn console_role(role: Role) -> bool {
    match role {
        Role::Worker => true,
        Role::Miner
        | Role::Hauler
        | Role::Upgrader
        | Role::Scout
        | Role::Reserver
        | Role::RemoteMiner
        | Role::RemoteHauler
        | Role::Signer
        | Role::Defender
        | Role::RemoteDefender
        | Role::Explorer
        | Role::Claimer
        | Role::MineralMiner
        | Role::MineralHauler
        | Role::LabHauler => false,
    }
}

/// A creep the console asked for, requested every tick until a spawn's call for it returns OK.
#[derive(Clone, Copy)]
struct ConsoleRequest {
    home: RoomName,
    role: Role,
}

/// The spawn path's state for one global.
#[derive(Default)]
pub struct SpawnGlue {
    spawner: Spawner,
    job: JobState,
    console: Option<ConsoleRequest>,
    /// The console request's index in this tick's requests.
    console_index: Option<u32>,
    /// The console request a spawn accepted, with the identity its creep must show next tick.
    accepted: Option<(ConsoleRequest, Identity, CreepName)>,
    /// Whether this global already notified a `Memory` reset (ADR-019).
    memory_notified: bool,
    /// `Memory` resets since the global started (ADR-019).
    memory_resets: u32,
}

impl SpawnGlue {
    /// Opens the tick's requests for `snapshot` with `cfg`'s help range, and queues the console's
    /// request: a worker of the body `worker` gives for its room (the opening's planned body, solved
    /// from the room's targets at its capacity, P3.6), else the smallest worker. A console request
    /// accepted on the last tick whose creep is not in this snapshot stands again (the processor
    /// dropped the order, V173).
    pub fn begin(
        &mut self,
        snapshot: &Snapshot,
        cfg: &SpawnConfig,
        worker: impl Fn(RoomName) -> Option<Body>,
    ) {
        if let Some((request, id, name)) = self.accepted.take() {
            let creeps = &snapshot.creeps;
            let born = creeps
                .my()
                .iter()
                .zip(creeps.identities())
                .any(|(&my, c)| my && *c == Some(id));
            if !born && self.console.is_none() {
                self.console = Some(request);
                log::warn!(
                    "rustedscreep spawn: {} was accepted but is not in Game.creeps; the console's \
                     request stands again",
                    name.as_str()
                );
            }
        }
        self.spawner.begin(snapshot, cfg);
        self.console_index = self.console.and_then(|c| {
            let body = match c.role {
                Role::Worker => worker(c.home).or_else(smallest_worker)?,
                Role::Miner
                | Role::Hauler
                | Role::Upgrader
                | Role::Scout
                | Role::Reserver
                | Role::RemoteMiner
                | Role::RemoteHauler
                | Role::Signer
                | Role::Defender
                | Role::RemoteDefender
                | Role::Explorer
                | Role::Claimer
                | Role::MineralMiner
                | Role::MineralHauler
                | Role::LabHauler => return None,
            };
            self.spawner.request(SpawnRequest {
                home: c.home,
                role: c.role,
                remote: None,
                body: BodySpec::Exact(body),
                budget: EnergyBudget::Capacity,
                priority: 0,
            })
        });
    }

    /// The spawner, for the jobs that request creeps (the opening, P2.4) between [`Self::begin`]
    /// and [`Self::plan`].
    pub fn spawner_mut(&mut self) -> &mut Spawner {
        &mut self.spawner
    }

    /// The Critical `spawn` job: one unit per serving room with a request it plans this tick and a
    /// ready spawn, so a tick with nothing asked, or with every spawn of each serving room busy or
    /// held, runs no job and reads no clock. A room serves others' requests only on a tick it
    /// starts full and the Standard tier runs (P5.1). Requests come from the jobs that ran before
    /// it (Critical jobs earlier in the tick; the console's at `begin`).
    pub fn plan<C: Clock>(&mut self, tick: &mut TickCpu<'_, C>, snapshot: &Snapshot) {
        // Serving another room is not Critical work: only while the Standard tier runs (P5.1).
        let count = self.spawner.close(snapshot, tick.allows(Tier::Standard));
        if count == 0 {
            return;
        }
        let Self { spawner, job, .. } = self;
        tick.run_units(&JOB, job, count, |unit, commit| {
            spawner.plan_room(snapshot, unit, commit)
        });
    }

    /// Issues the tick's orders, one `spawnCreep` each, and records each outcome in the spawner
    /// (a refusal holds the spawn) and in the act counters. Call in the act step before the creep
    /// intents are issued (module docs), after the scheduler's tick has ended.
    pub fn issue(
        &mut self,
        snapshot: &Snapshot,
        js: &JsWorld,
        log: &mut ActLog,
        sched: &mut Scheduler,
        cfg: &SchedulerConfig,
        spawn_cfg: &SpawnConfig,
    ) {
        for index in 0..self.spawner.orders().len() {
            let Some(&order) = self.spawner.orders().get(index) else {
                break;
            };
            let Some(spawn) = js.structures.get(order.structure_row as usize) else {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for the spawn row",
                    order.structure_row,
                );
                log.missing();
                continue;
            };
            let call = spawn
                .unchecked_ref::<SpawnActor>()
                .spawn_creep(order.body.parts(), order.name.as_str());
            let mut reset = false;
            let outcome = match call {
                Ok(value) => Outcome::from_return(value),
                Err(e) => {
                    let slot = order.spawn.slot();
                    fault(sched, cfg, snapshot, "the spawnCreep call threw", slot);
                    // At most once per spawn per hold (the throw holds the spawn), except after an
                    // ADR-019 reset, which releases it.
                    log_threw(&e, &order.name);
                    // ADR-019: a null `Memory` makes every call throw (V181); reset it.
                    reset = memory_is_null() && self.reset_memory();
                    Outcome::Threw
                }
            };
            log.record_structure(outcome);
            self.spawner.record(index, outcome, spawn_cfg);
            if reset {
                // Repaired: the spawn tries again next tick. The tick's other orders would throw
                // the same way (`Memory` stays null for the tick), so they are not called.
                self.spawner.release(order.spawn);
                break;
            }
            if outcome == Outcome::Ok {
                // ADR-018: the engine's `Memory.creeps[name]` entry goes before the tick's save.
                forget_entry(order.name.as_str());
                let console = self.console_index == Some(order.request);
                if console && let Some(request) = self.console.take() {
                    self.accepted = Some((request, order.identity, order.name));
                }
                if log::log_enabled!(log::Level::Info) {
                    log_accepted(&order.name, &order.body, console);
                }
            }
        }
    }

    /// ADR-019: sets the raw `Memory` to `{}`, which the end-of-tick save keeps while the tick's
    /// `Memory` is null (V180); logs it, and notifies once per global. False if the set threw.
    fn reset_memory(&mut self) -> bool {
        if raw_memory_set("{}").is_err() {
            log::warn!("rustedscreep spawn: Memory is null, and RawMemory.set threw");
            return false;
        }
        self.memory_resets = self.memory_resets.saturating_add(1);
        log::warn!(
            "rustedscreep spawn: Memory is null (its raw string did not parse, or was JSON null), so \
             spawnCreep threw; the raw Memory is reset to {{}} (ADR-019, reset {} of this global)",
            self.memory_resets
        );
        if !self.memory_notified {
            self.memory_notified = true;
            let _ = game_notify(MEMORY_RESET_NOTICE);
        }
        true
    }

    /// For `rustedscreep.spawn(room, role)`: from the next tick, a spawn of `room` spawns one creep
    /// of `role` (worker when omitted) of the room's planned worker body ([`Self::begin`]), once the
    /// room holds its cost; for an owned room without a spawn, a spawn room within
    /// `spawn.help_range` whose capacity fits the body spawns it, on a tick that room starts full
    /// while the Standard tier runs (P5.1). With no room it cancels the request. The reply reads
    /// the last tick's spawns, owned rooms and help range.
    pub fn set_console(&mut self, room: Option<&str>, role: Option<&str>) -> String {
        let Some(room) = room else {
            return match self.console.take() {
                Some(_) => String::from("rustedscreep spawn: console request cancelled"),
                None => String::from("rustedscreep spawn: no console request to cancel"),
            };
        };
        let Some(home) = parse_room_name(room.as_bytes()) else {
            return String::from("rustedscreep spawn: not a room name");
        };
        let role = match role {
            None => Role::Worker,
            Some(name) => match Role::from_name(name) {
                Some(role) if console_role(role) => role,
                found => {
                    let mut text = String::from(match found {
                        Some(Role::Hauler) => {
                            "rustedscreep spawn: the hauling job sizes and orders haulers; console roles:"
                        }
                        Some(Role::Upgrader) => {
                            "rustedscreep spawn: the upgrading job sizes and orders upgraders; console roles:"
                        }
                        Some(_) => {
                            "rustedscreep spawn: the mining job sizes and orders miners; console roles:"
                        }
                        None => "rustedscreep spawn: unknown role; roles:",
                    });
                    for r in Role::ALL.into_iter().filter(|&r| console_role(r)) {
                        let _ = write!(text, " {}", r.as_str());
                    }
                    return text;
                }
            },
        };
        self.console = Some(ConsoleRequest { home, role });
        self.accepted = None;
        let role = role.as_str();
        match self.spawner.server_of(home) {
            Some(server) if server != home => format!(
                "rustedscreep spawn: from the next tick, a spawn room within spawn.help_range \
                 whose capacity fits the body spawns one {role} of {home}'s planned body for it, \
                 on a tick that room starts full while the Standard tier runs; \
                 rustedscreep.spawn() cancels (no own spawn in {home} on the last tick; the \
                 nearest spawn room was {server})"
            ),
            server => {
                let note = if server.is_some() {
                    ""
                } else {
                    " (no own spawn there on the last tick: nothing spawns until one exists)"
                };
                format!(
                    "rustedscreep spawn: from the next tick, a spawn of {home} spawns one {role} \
                     of the room's planned body, once the room holds its cost; \
                     rustedscreep.spawn() cancels{note}"
                )
            }
        }
    }

    /// One line for `rustedscreep.spawner()`: each spawn room's energy and spawns, the last tick's
    /// requests and orders, totals since boot (with the spawn-ticks a spawn stood idle while its room
    /// had a request, by cause: energy, held, help; P5.3), the last spawn and failure, the console
    /// request and the own creeps by home and role.
    pub fn summary(&self, snapshot: &Snapshot) -> String {
        let sp = &self.spawner;
        let s = sp.stats();
        let t = sp.totals();
        let mut text = format!("rustedscreep spawner at tick {}:", sp.time());
        if sp.spawn_rooms().is_empty() {
            text.push_str(" no own spawn");
        }
        let spawns = &snapshot.spawns;
        for (i, &room) in sp.spawn_rooms().iter().enumerate() {
            let energy = snapshot
                .rooms
                .row_of(room)
                .and_then(|r| snapshot.rooms.energy().get(r))
                .copied()
                .unwrap_or_default();
            let (mut idle, mut busy, mut held) = (0, 0, 0);
            for ((&h, &row), &b) in spawns
                .handles()
                .iter()
                .zip(spawns.rows())
                .zip(spawns.busy())
            {
                let here = snapshot
                    .structures
                    .positions()
                    .get(row as usize)
                    .is_some_and(|p| p.room_name() == room);
                if !here {
                    continue;
                }
                if b {
                    busy += 1;
                } else if sp.is_held(h) {
                    held += 1;
                } else {
                    idle += 1;
                }
            }
            let _ = write!(
                text,
                "{} {room} energy {}/{}, spawns idle {idle} busy {busy} held {held}",
                if i == 0 { "" } else { ";" },
                energy.available,
                energy.capacity,
            );
        }
        let _ = write!(
            text,
            " | requests {}, orders {} (ok {}, failed {}), waiting {}, unfit {}, no spawn {}, helped {}, \
             held {} | since boot: ticks {}, orders {}, ok {}, failed {}, idle spawn-ticks with a \
             request: energy {}, held {}, help {}",
            s.requests,
            s.orders,
            s.ok,
            s.failed,
            s.waiting,
            s.unfit,
            s.no_spawn,
            s.helped,
            s.held,
            t.ticks,
            t.orders,
            t.ok,
            t.failed,
            t.idle_energy,
            t.idle_held,
            t.idle_help,
        );
        if let Some(e) = sp.last_ok() {
            let _ = write!(text, " | last accepted ");
            event(&mut text, e);
        }
        if let Some(e) = sp.last_failure() {
            let _ = write!(text, " | last failure ");
            event(&mut text, e);
        }
        match self.console {
            Some(c) => {
                let _ = write!(text, " | console request: {} {}", c.home, c.role.as_str());
            }
            None => text.push_str(" | console request: none"),
        }
        let _ = write!(text, " | Memory resets {}", self.memory_resets);
        text.push_str(" | own creeps:");
        let mut counts: Vec<(RoomName, Role, u32)> = Vec::new();
        let mut unnamed = 0u32;
        let creeps = &snapshot.creeps;
        for (&my, id) in creeps.my().iter().zip(creeps.identities()) {
            if !my {
                continue;
            }
            match id {
                Some(id) => {
                    // By home (packed) and role, as a sorted insert: no sort instantiation.
                    let key = (id.home.packed_repr(), id.role);
                    match counts.iter().position(|c| (c.0.packed_repr(), c.1) >= key) {
                        Some(at)
                            if counts
                                .get(at)
                                .is_some_and(|c| (c.0.packed_repr(), c.1) == key) =>
                        {
                            if let Some(c) = counts.get_mut(at) {
                                c.2 += 1;
                            }
                        }
                        Some(at) => counts.insert(at, (id.home, id.role, 1)),
                        None => counts.push((id.home, id.role, 1)),
                    }
                }
                None => unnamed += 1,
            }
        }
        for (home, role, n) in counts {
            let _ = write!(text, " {home} {} {n},", role.as_str());
        }
        let _ = write!(text, " other names {unnamed}");
        text
    }
}

/// `NAME at tick T (n parts, c energy): outcome`.
fn event(text: &mut String, e: &SpawnEvent) {
    let _ = write!(
        text,
        "{} at tick {} ({} parts, {} energy): ",
        e.name.as_str(),
        e.time,
        e.body.len(),
        e.body.cost()
    );
    let _ = match e.outcome {
        Outcome::Ok => write!(text, "ok"),
        Outcome::Failed(code) => write!(text, "{}", error_name(code)),
        Outcome::Unknown => write!(text, "unknown return"),
        Outcome::Threw => write!(text, "threw"),
    };
}

#[cold]
#[inline(never)]
fn log_accepted(name: &CreepName, body: &Body, console: bool) {
    log::info!(
        "rustedscreep spawn: {} accepted ({} parts, {} energy, {} ticks){}",
        name.as_str(),
        body.len(),
        body.cost(),
        body.spawn_time(),
        if console {
            ", the console's request"
        } else {
            ""
        }
    );
}

/// The `Memory` global as it stands this tick: the engine's getter parses on first access and then
/// holds the result as a plain value for the tick (V181), so after a `spawnCreep` call this reads
/// no new parse. `None` when `globalThis` or the read is unavailable.
fn memory_global() -> Option<JsValue> {
    let global = GLOBAL_THIS.with(Option::clone)?;
    Reflect::get(&global, &JsValue::from_str("Memory")).ok()
}

/// Whether this tick's `Memory` is null: its raw string did not parse, or was JSON null (the
/// getter's `__proto__` assignment throws on null in its strict code, game.js L485-490; V181).
fn memory_is_null() -> bool {
    memory_global().is_some_and(|m| m.is_null())
}

/// ADR-018: deletes `Memory.creeps[name]`, the entry an accepted `spawnCreep` wrote (V181), so the
/// end-of-tick save does not store it (V180). Nothing when `Memory` or `Memory.creeps` is not an
/// object; a throw is ignored (the entry stays, as before ADR-018).
fn forget_entry(name: &str) {
    let Some(memory) = memory_global().filter(JsValue::is_object) else {
        return;
    };
    let Some(creeps) = Reflect::get(&memory, &JsValue::from_str("creeps"))
        .ok()
        .filter(JsValue::is_object)
    else {
        return;
    };
    let _ = Reflect::delete_property(creeps.unchecked_ref::<Object>(), &JsValue::from_str(name));
}

/// The JS error's message, so a throw such as the one from an unusable `Memory` (V181) shows its
/// cause; read as a plain value (a non-string message is left out).
#[cold]
#[inline(never)]
fn log_threw(e: &JsValue, name: &CreepName) {
    if !log::log_enabled!(log::Level::Warn) {
        return;
    }
    // `message` through Reflect, not `instanceof Error`, so an error from another realm shows too.
    let message = if e.is_object() {
        Reflect::get(e, &JsValue::from_str("message"))
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_default()
    } else {
        e.as_string().unwrap_or_default()
    };
    log::warn!(
        "rustedscreep spawn: spawnCreep for {} threw: {message}",
        name.as_str()
    );
}

fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    unit: u32,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: ACT_JOB,
        fault: Fault::new(what, unit),
    };
    sched.faults_mut().record(record, cfg);
}
