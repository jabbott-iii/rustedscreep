//! The minimal spawn path's planner (ROADMAP P2; the P5 global queue replaces it, the order and its
//! issue stay). Each tick, jobs [`Spawner::request`] creeps for their rooms; [`Spawner::close`] ends
//! the requests and keeps the rooms with a request and a ready spawn (not spawning, not held); the
//! Critical `spawn` job plans each of them ([`Spawner::plan_room`]), so a tick with nothing asked,
//! or with every spawn of each asking room busy or held, runs no job and reads no clock (a room
//! that waits for energy with a ready spawn is planned every tick); the act layer issues the orders
//! and reports each call's [`Outcome`] ([`Spawner::record`]).
//!
//! Per room: the ready spawns (not spawning at the start of the tick, V171, V175, and not held), in
//! id order with the spawns on probation last, take the room's requests in priority order. A request sizes its body from the room's
//! capacity or from what it holds now ([`EnergyBudget`]); when the next request cannot be paid for,
//! the room waits and no request below it spawns, so energy is never spent around a waiting one. A
//! request whose body cannot be built even at the room's capacity (one repeat of its pattern costs
//! more, or it allows no repeat) is skipped, whatever its budget, so it never blocks the room.
//!
//! The planner keeps the room's energy left over its orders: the runtime checks every spawn against
//! the same tick-start `room.energyAvailable` (V172), and the processor silently drops a second
//! spawn's order when the first one drained the energy (V173), so the orders of one room never sum
//! above what it holds. A spawn whose call the game refuses (every refusal it can return points to
//! a lasting state, such as a spawn that is off, V171) is held for the next `spawn.hold_ticks`
//! ticks, so another
//! spawn of the room can take the request and the refused one is not called every tick. After the
//! hold it is on probation until a call of its own is accepted: it takes an order only after the
//! room's other ready spawns, so a spawn that still refuses never takes the top request from one
//! that works (which would let a lower request spawn first).
//!
//! No state outlives a global but the holds, which a reset drops (it fails open: one refused call
//! per held spawn). A tick allocates nothing once the vectors have grown.

use rustedscreep_consts::{INTENT_CPU_COST, Part, RoomName};

use super::body::Body;
use super::name::{CreepName, Identity, Role, serial};
use crate::config::SpawnConfig;
use crate::intents::Outcome;
use crate::scheduler::{Commit, Fault};
use crate::snapshot::{ObjectKey, Snapshot, StructureHandle};

/// What a request sizes its body from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnergyBudget {
    /// The room's capacity (`energyCapacityAvailable`, V175): the largest body the room can ever pay
    /// for, so the request waits until the room holds its cost.
    Capacity,
    /// What the room holds now (`energyAvailable` less this tick's earlier orders): spawn at once
    /// with the largest body that fits, waiting only for one repeat of the pattern. For recovery.
    Available,
    /// At most this much (and at most the capacity): the largest body within it, waiting until the
    /// room holds its cost. For a room that cannot fill its whole capacity (P2.4's wait fallback
    /// asks for the most the room was seen holding). Unit: energy.
    UpTo(u32),
}

/// One creep a job wants spawned this tick. Requests last one tick: a job asks again each tick
/// until the creep it wants exists (spawning creeps included, which the snapshot shows from the
/// tick after the order, V101).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// The room the creep works for; a spawn of that room spawns it (P5 lets others).
    pub home: RoomName,
    pub role: Role,
    /// The body is whole repeats of this ([`Body::scaled`]).
    pub pattern: &'static [Part],
    /// Most repeats of `pattern`. Unit: repeats.
    pub max_repeats: u8,
    pub budget: EnergyBudget,
    /// Higher first; equal priorities keep the order they were requested in.
    pub priority: u8,
}

/// An order for the act layer: one `spawnCreep(body, name)` on a spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnOrder {
    pub spawn: StructureHandle,
    /// The spawn's row in this tick's `Structures` (where the act layer finds its JS object).
    pub structure_row: u32,
    /// The request it serves: its index in this tick's requests.
    pub request: u32,
    pub body: Body,
    /// The new creep's identity, and the name that records it.
    pub identity: Identity,
    pub name: CreepName,
}

/// This tick's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpawnStats {
    pub requests: u32,
    pub orders: u32,
    /// Rooms whose next request waited for energy.
    pub waiting: u32,
    /// Requests skipped because no body of theirs fits their room's capacity.
    pub unfit: u32,
    /// Requests whose home has no own spawn this tick (P5 lets other rooms spawn for it).
    pub no_spawn: u32,
    /// Spawns of asking rooms, not spawning, that a refused call holds.
    pub held: u32,
    /// Calls the game accepted (the order was recorded; the processor may still drop it, V173).
    pub ok: u32,
    /// Calls refused (a known error code), with an unknown result, or that threw.
    pub failed: u32,
}

/// Counters since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpawnTotals {
    pub ticks: u64,
    pub orders: u64,
    pub ok: u64,
    pub failed: u64,
}

/// The last accepted order, or the last call that was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnEvent {
    pub time: u32,
    pub name: CreepName,
    pub body: Body,
    pub outcome: Outcome,
}

/// A spawn whose last call was refused: held until `until`, then on probation until a call of its
/// own is accepted (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    spawn: StructureHandle,
    /// The first tick the spawn may take an order again.
    until: u32,
}

/// The spawn path's state for one global.
#[derive(Default)]
pub struct Spawner {
    /// `Game.time` of the snapshot [`Self::begin`] opened, until the plan's first room.
    open: Option<u32>,
    time: u32,
    requests: Vec<SpawnRequest>,
    orders: Vec<SpawnOrder>,
    /// Rooms with an own spawn this tick, by packed name.
    spawn_rooms: Vec<RoomName>,
    /// Those with a request, once [`Self::close`] ran: the `spawn` job's units.
    rooms: Vec<RoomName>,
    holds: Vec<Hold>,
    // Scratch, reused per room: (on probation, id, handle, structure row) of the ready spawns.
    idle: Vec<(bool, ObjectKey, StructureHandle, u32)>,
    queue: Vec<u32>,
    stats: SpawnStats,
    totals: SpawnTotals,
    last_ok: Option<SpawnEvent>,
    last_failure: Option<SpawnEvent>,
}

impl Spawner {
    /// Starts a tick for `snap`, which must already be taken: drops last tick's requests and
    /// orders and the holds whose spawn is gone, and lists the rooms with own spawns.
    pub fn begin(&mut self, snap: &Snapshot) {
        let time = snap.tick.time;
        self.open = Some(time);
        self.time = time;
        self.requests.clear();
        self.orders.clear();
        self.stats = SpawnStats::default();
        self.totals.ticks = self.totals.ticks.saturating_add(1);
        let structures = &snap.structures;
        self.holds.retain(|h| structures.row_of(h.spawn).is_some());
        self.spawn_rooms.clear();
        self.rooms.clear();
        for &row in snap.spawns.rows() {
            if let Some(pos) = structures.positions().get(row as usize) {
                let room = pos.room_name();
                if !self.spawn_rooms.contains(&room) {
                    // By packed name (a sorted insert: few rooms, and no sort instantiation).
                    let at = self
                        .spawn_rooms
                        .iter()
                        .position(|r| r.packed_repr() > room.packed_repr())
                        .unwrap_or(self.spawn_rooms.len());
                    self.spawn_rooms.insert(at, room);
                }
            }
        }
    }

    /// Queues `request` for this tick's plan and returns its index, or `None` once planning started
    /// (or before `begin`).
    pub fn request(&mut self, request: SpawnRequest) -> Option<u32> {
        self.open?;
        let index = u32::try_from(self.requests.len()).ok()?;
        self.requests.push(request);
        self.stats.requests = self.stats.requests.saturating_add(1);
        Some(index)
    }

    /// Ends this tick's requests and keeps the rooms with at least one request and at least one
    /// ready spawn (not spawning, not held): the `spawn` job's units ([`Self::rooms`]). Returns how
    /// many there are. Counts the held spawns of asking rooms and the requests with no spawn.
    pub fn close(&mut self, snap: &Snapshot) -> u32 {
        self.open = None;
        self.rooms.clear();
        for r in &self.requests {
            if !self.spawn_rooms.contains(&r.home) {
                self.stats.no_spawn = self.stats.no_spawn.saturating_add(1);
            }
        }
        let spawns = &snap.spawns;
        for &room in &self.spawn_rooms {
            if !self.requests.iter().any(|r| r.home == room) {
                continue;
            }
            let mut ready = false;
            for ((&handle, &row), &busy) in spawns
                .handles()
                .iter()
                .zip(spawns.rows())
                .zip(spawns.busy())
            {
                let here = snap
                    .structures
                    .positions()
                    .get(row as usize)
                    .is_some_and(|p| p.room_name() == room);
                if !here || busy {
                    continue;
                }
                if self.is_held(handle) {
                    self.stats.held = self.stats.held.saturating_add(1);
                } else {
                    ready = true;
                }
            }
            if ready {
                self.rooms.push(room);
            }
        }
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// The rooms the `spawn` job plans once [`Self::close`] ran, one unit each, by packed name.
    pub fn rooms(&self) -> &[RoomName] {
        &self.rooms
    }

    /// Every room with an own spawn this tick, by packed name.
    pub fn spawn_rooms(&self) -> &[RoomName] {
        &self.spawn_rooms
    }

    /// Plans room `unit` of [`Self::rooms`]: its idle spawns take its requests (module docs). Each
    /// order commits `INTENT_CPU_COST` (V34). Call after [`Self::close`].
    ///
    /// Uses: `INTENT_CPU_COST`; `BODYPART_COST` and `MAX_CREEP_SIZE` through [`Body::scaled`].
    pub fn plan_room(
        &mut self,
        snap: &Snapshot,
        unit: u32,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        if self.time != snap.tick.time || self.open.is_some() {
            return Err(Fault::new("planned for another tick or before close", unit));
        }
        let room = *self
            .rooms
            .get(unit as usize)
            .ok_or(Fault::new("no such spawn room", unit))?;
        let energy = snap
            .rooms
            .row_of(room)
            .and_then(|r| snap.rooms.energy().get(r))
            .copied()
            .unwrap_or_default();

        self.idle.clear();
        let spawns = &snap.spawns;
        for ((&handle, &row), &busy) in spawns
            .handles()
            .iter()
            .zip(spawns.rows())
            .zip(spawns.busy())
        {
            let in_room = snap
                .structures
                .positions()
                .get(row as usize)
                .is_some_and(|p| p.room_name() == room);
            if busy || !in_room {
                continue;
            }
            if self.is_held(handle) {
                continue;
            }
            let probation = self.holds.iter().any(|h| h.spawn == handle);
            let key = snap
                .structures
                .key_of(handle)
                .unwrap_or(ObjectKey(u128::MAX));
            // In id order, spawns on probation last. Sorted inserts, not a sort: a room has a few
            // spawns, and each generic sort instantiation costs kilobytes of wasm.
            let entry = (probation, key, handle, row);
            let at = self
                .idle
                .iter()
                .position(|e| (e.0, e.1) > (probation, key))
                .unwrap_or(self.idle.len());
            self.idle.insert(at, entry);
        }

        // Highest priority first; equal priorities in request order (each goes after its equals).
        self.queue.clear();
        let requests = &self.requests;
        for (index, r) in (0u32..).zip(requests) {
            if r.home != room {
                continue;
            }
            let at = self
                .queue
                .iter()
                .position(|&i| {
                    requests
                        .get(i as usize)
                        .is_some_and(|q| q.priority < r.priority)
                })
                .unwrap_or(self.queue.len());
            self.queue.insert(at, index);
        }

        let mut left = energy.available;
        let mut next = 0usize;
        for &(_, _, spawn, structure_row) in &self.idle {
            loop {
                let Some(&index) = self.queue.get(next) else {
                    return Ok(());
                };
                let Some(&req) = self.requests.get(index as usize) else {
                    return Err(Fault::new("request index out of range", unit));
                };
                // A request no body of which fits the room's capacity can never be paid for: skip
                // it, whatever its budget, so it never blocks the requests below it.
                let Some(at_capacity) = Body::scaled(req.pattern, energy.capacity, req.max_repeats)
                else {
                    self.stats.unfit = self.stats.unfit.saturating_add(1);
                    next += 1;
                    continue;
                };
                let body = match req.budget {
                    EnergyBudget::Capacity => Some(at_capacity),
                    EnergyBudget::Available => Body::scaled(req.pattern, left, req.max_repeats),
                    // Below one repeat it is the smallest body, as for `Available`.
                    EnergyBudget::UpTo(cap) => {
                        Body::scaled(req.pattern, cap.min(energy.capacity), req.max_repeats)
                            .or_else(|| Body::new(req.pattern))
                    }
                };
                let Some(body) = body.filter(|b| b.cost() <= left) else {
                    self.stats.waiting = self.stats.waiting.saturating_add(1);
                    return Ok(());
                };
                let order_index = u32::try_from(self.orders.len()).unwrap_or(u32::MAX);
                let serial = serial(self.time, order_index)
                    .ok_or(Fault::new("too many spawn orders in one tick", unit))?;
                let identity = Identity {
                    role: req.role,
                    home: room,
                    serial,
                };
                self.orders.push(SpawnOrder {
                    spawn,
                    structure_row,
                    request: index,
                    body,
                    identity,
                    name: CreepName::encode(&identity),
                });
                self.stats.orders = self.stats.orders.saturating_add(1);
                self.totals.orders = self.totals.orders.saturating_add(1);
                left -= body.cost();
                commit.add(INTENT_CPU_COST);
                next += 1;
                break;
            }
        }
        Ok(())
    }

    /// This tick's orders, in plan order.
    pub fn orders(&self) -> &[SpawnOrder] {
        &self.orders
    }

    /// Records what the game returned for order `index`: anything but OK holds its spawn for the
    /// next `spawn.hold_ticks` ticks, then puts it on probation; OK ends its probation (module
    /// docs).
    pub fn record(&mut self, index: usize, outcome: Outcome, cfg: &SpawnConfig) {
        let Some(&order) = self.orders.get(index) else {
            return;
        };
        let event = SpawnEvent {
            time: self.time,
            name: order.name,
            body: order.body,
            outcome,
        };
        if outcome == Outcome::Ok {
            self.stats.ok = self.stats.ok.saturating_add(1);
            self.totals.ok = self.totals.ok.saturating_add(1);
            self.last_ok = Some(event);
            self.holds.retain(|h| h.spawn != order.spawn);
        } else {
            self.stats.failed = self.stats.failed.saturating_add(1);
            self.totals.failed = self.totals.failed.saturating_add(1);
            self.last_failure = Some(event);
            // Refused at T: held on T + 1 ..= T + hold_ticks, ready from the tick after.
            let until = self.time.saturating_add(cfg.hold_ticks).saturating_add(1);
            match self.holds.iter_mut().find(|h| h.spawn == order.spawn) {
                Some(h) => h.until = until,
                None => self.holds.push(Hold {
                    spawn: order.spawn,
                    until,
                }),
            }
        }
    }

    /// Drops the spawn's hold and probation, so it takes orders again from the next tick: the
    /// cause of its refusal was repaired (rustedscreep-wasm resets a `Memory` that does not parse,
    /// ADR-019).
    pub fn release(&mut self, spawn: StructureHandle) {
        self.holds.retain(|h| h.spawn != spawn);
    }

    /// The request at `index` in this tick's requests.
    pub fn request_at(&self, index: u32) -> Option<&SpawnRequest> {
        self.requests.get(index as usize)
    }

    /// Whether the spawn is held after a refused call (it takes no order this tick).
    pub fn is_held(&self, spawn: StructureHandle) -> bool {
        self.holds
            .iter()
            .any(|h| h.spawn == spawn && h.until > self.time)
    }

    /// Whether the spawn's hold ran out and no call of its own was accepted since: it takes orders
    /// after the room's other ready spawns.
    pub fn on_probation(&self, spawn: StructureHandle) -> bool {
        self.holds
            .iter()
            .any(|h| h.spawn == spawn && h.until <= self.time)
    }

    pub fn stats(&self) -> &SpawnStats {
        &self.stats
    }

    pub fn totals(&self) -> &SpawnTotals {
        &self.totals
    }

    pub fn last_ok(&self) -> Option<&SpawnEvent> {
        self.last_ok.as_ref()
    }

    pub fn last_failure(&self) -> Option<&SpawnEvent> {
        self.last_failure.as_ref()
    }

    /// `Game.time` of the tick [`Self::begin`] last opened.
    pub fn time(&self) -> u32 {
        self.time
    }
}
