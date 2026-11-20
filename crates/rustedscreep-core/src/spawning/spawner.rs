//! The global spawn queue (ROADMAP P5.1, from P2's minimal spawn path). Each tick, jobs
//! [`Spawner::request`] creeps for their rooms. [`Spawner::close`] ends the requests and gives each
//! to the room whose spawns serve it: its home when the home has an own spawn; for an owned home
//! without one, a room with an own spawn within `spawn.help_range` ([`room_distance`]) whose
//! capacity fits the request's body, the nearest that helps this tick with a ready spawn, else the
//! nearest (equal distances to the lower packed name); else none (counted as `no_spawn`). It keeps
//! the serving rooms with a request they may plan this tick and a ready spawn (not spawning, not
//! held); the Critical `spawn` job plans each of them
//! ([`Spawner::plan_room`]), so a tick with nothing asked, or with every spawn of each serving room
//! busy or held, runs no job and reads no clock (a room that waits for energy with a ready spawn is
//! planned every tick); the act layer issues the orders and reports each call's [`Outcome`]
//! ([`Spawner::record`]).
//!
//! The queue is rebuilt every tick from the jobs' requests, so nothing about it outlives a global
//! and a reset loses nothing; a request's serving room follows this tick's spawns.
//!
//! Per serving room: the ready spawns (not spawning at the start of the tick, V171, V175, and not
//! held), in id order with the spawns on probation last, take the room's requests in order: its own
//! requests first, then those it serves for other rooms, each by priority (equal priorities in
//! request order). A room serves others only on a tick it starts full (`energyAvailable` at its
//! `energyCapacityAvailable`, V175) and the Standard tier runs: helping is not Critical work, and
//! only a full room's energy is spare. So within a tick a room's own requests always come first; a
//! helped creep then keeps its spawn busy for its spawn time and spends the room's energy, which the
//! room's later requests wait for (P5.2's replacement lead and P8's colony support weigh that). A
//! spawn spends its own room's energy (V171, V173), so a [`BodySpec::Scaled`] body is sized from the
//! serving room's capacity or from what that room holds now ([`EnergyBudget`]) and an exact body
//! waits for its cost there. A job that solves an exact body for a home without a spawn reads the
//! spawn room nearest it ([`Spawner::spawn_side`]): the opening, the only job that asks for one
//! before P8's pioneers, plans such a home's workers at the smaller of the home's capacity and that
//! room's, so the body fits there. The creep's name keeps the request's home. When the next request cannot be paid for, the room waits
//! and no request below it spawns, so energy is never spent around a waiting one. A request whose
//! body cannot be built even at its room's capacity (one repeat of its pattern costs more, it allows
//! no repeat, or its exact body costs more) is skipped, whatever its budget, so it never blocks the
//! room.
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
use crate::snapshot::{ObjectKey, RoomEnergy, Snapshot, StructureHandle};

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

/// The body a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodySpec {
    /// Whole repeats of `pattern`, at most `max_repeats` of them ([`Body::scaled`]), sized by the
    /// request's budget.
    Scaled {
        pattern: &'static [Part],
        /// Unit: repeats.
        max_repeats: u8,
    },
    /// This body, whatever the budget: unfit when it costs more than the room's capacity, else the
    /// request waits until the room holds its cost (a body a job computed from its targets, such
    /// as a static miner's, P3.1).
    Exact(Body),
}

/// One creep a job wants spawned this tick. Requests last one tick: a job asks again each tick
/// until the creep it wants exists (spawning creeps included, which the snapshot shows from the
/// tick after the order, V101).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// The room the creep works for, which its name records. A spawn of that room spawns it; with
    /// none there and the room owned, a spawn of the nearest room with one within
    /// `spawn.help_range` whose capacity fits the body (P5.1).
    pub home: RoomName,
    pub role: Role,
    pub body: BodySpec,
    /// What a [`BodySpec::Scaled`] body is sized to; an exact body only waits for its cost.
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
    /// Requests skipped because no body of theirs fits their serving room's capacity.
    pub unfit: u32,
    /// Requests whose home has no own spawn this tick and is not owned or has no room with one
    /// within `spawn.help_range` whose capacity fits the body: nothing spawns them.
    pub no_spawn: u32,
    /// Requests whose home has no own spawn, given to another room's spawns (P5.1); whether they
    /// spawn this tick is in `orders`, `waiting` and the serving room's state.
    pub helped: u32,
    /// Spawns of serving rooms (those given a request this tick), not spawning, that a refused
    /// call holds.
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

/// The room whose spawns would spawn a home's creep, for a job sizing its body
/// ([`Spawner::spawn_side`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnSide {
    /// The home itself when it has an own spawn, else the room that would serve it.
    pub room: RoomName,
    /// That room's energy at the start of the tick.
    pub energy: RoomEnergy,
    /// Whether a spawn of that room is not spawning at the start of the tick (it may still be held
    /// after a refused call).
    pub idle: bool,
}

/// A spawn room's state for the tick.
#[derive(Clone, Copy, Debug, Default)]
struct RoomSpawns {
    energy: RoomEnergy,
    /// It starts the tick full (`energyAvailable` at a non-zero `energyCapacityAvailable`).
    full: bool,
    /// A spawn of it is not spawning.
    idle: bool,
    /// A spawn of it is not spawning and not held ([`Spawner::close`]).
    ready: bool,
    /// Its spawns, not spawning, that a refused call holds ([`Spawner::close`]).
    held: u32,
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
    /// Each request's serving room, by request index, once [`Self::close`] ran.
    servers: Vec<Option<RoomName>>,
    /// `spawn.help_range` for this tick. Unit: rooms.
    help_range: u32,
    orders: Vec<SpawnOrder>,
    /// Rooms with an own spawn this tick, by packed name.
    spawn_rooms: Vec<RoomName>,
    /// Each spawn room's energy and spawns this tick, by `spawn_rooms` index.
    spawn_state: Vec<RoomSpawns>,
    /// The owned rooms this tick (the homes another room may serve).
    owned: Vec<RoomName>,
    /// Whether rooms may serve others this tick ([`Self::close`]).
    help: bool,
    /// Those that serve a request this tick, once [`Self::close`] ran: the `spawn` job's units.
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
    /// orders and the holds whose spawn is gone, lists the rooms with own spawns (with their
    /// capacity and whether they are full) and the owned rooms, and takes `cfg.help_range` for the
    /// tick's serving rooms.
    pub fn begin(&mut self, snap: &Snapshot, cfg: &SpawnConfig) {
        let time = snap.tick.time;
        self.open = Some(time);
        self.time = time;
        self.help_range = cfg.help_range;
        self.requests.clear();
        self.servers.clear();
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
        let rooms = &snap.rooms;
        let spawns = &snap.spawns;
        self.spawn_state.clear();
        self.spawn_state
            .extend(self.spawn_rooms.iter().map(|&room| {
                let energy = rooms
                    .row_of(room)
                    .and_then(|r| rooms.energy().get(r))
                    .copied()
                    .unwrap_or_default();
                let idle = spawns
                    .rows()
                    .iter()
                    .zip(spawns.busy())
                    .any(|(&row, &busy)| {
                        !busy
                            && structures
                                .positions()
                                .get(row as usize)
                                .is_some_and(|p| p.room_name() == room)
                    });
                RoomSpawns {
                    energy,
                    full: energy.capacity > 0 && energy.available >= energy.capacity,
                    idle,
                    ready: false,
                    held: 0,
                }
            }));
        self.owned.clear();
        self.owned
            .extend(rooms.owned().filter_map(|r| rooms.names().get(r).copied()));
        self.help = false;
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

    /// The nearest room that could serve `home` this tick, whatever the body and the room's state
    /// (call after [`Self::begin`]): `home` when it has an own spawn; for an owned home without
    /// one, the nearest room with one at most `spawn.help_range` away by [`room_distance`] (ties to
    /// the lower packed name); else `None`. At [`Self::close`] a request may go on to a farther
    /// room in range: one its body fits, or one that helps this tick with a ready spawn.
    pub fn server_of(&self, home: RoomName) -> Option<RoomName> {
        self.nearest(home, |_| true)
    }

    /// [`Self::server_of`]'s room with its energy and whether one of its spawns is idle, for a job
    /// that sizes a body for `home` (call after [`Self::begin`]).
    pub fn spawn_side(&self, home: RoomName) -> Option<SpawnSide> {
        let room = self.server_of(home)?;
        let at = self.spawn_rooms.iter().position(|&r| r == room)?;
        let state = self.spawn_state.get(at)?;
        Some(SpawnSide {
            room,
            energy: state.energy,
            idle: state.idle,
        })
    }

    /// The room that serves request `index` this tick, once [`Self::close`] ran.
    pub fn server_at(&self, index: u32) -> Option<RoomName> {
        self.servers.get(index as usize).copied().flatten()
    }

    /// Whether a body of `body` can be built at `capacity` ([`Self::plan_room`]'s fit).
    ///
    /// Uses: `BODYPART_COST` and `MAX_CREEP_SIZE` through [`Body::scaled`] and [`Body::cost`].
    fn fits(body: BodySpec, capacity: u32) -> bool {
        match body {
            BodySpec::Scaled {
                pattern,
                max_repeats,
            } => Body::scaled(pattern, capacity, max_repeats).is_some(),
            BodySpec::Exact(b) => b.cost() <= capacity,
        }
    }

    /// The room that serves `request` (module docs): its home when it has an own spawn; for an
    /// owned home, the nearest in range whose capacity fits its body and that helps this tick with
    /// a ready spawn, else the nearest in range its body fits. Call in [`Self::close`].
    fn server_for(&self, request: &SpawnRequest) -> Option<RoomName> {
        let fits = |i: usize| {
            self.spawn_state
                .get(i)
                .is_some_and(|s| Self::fits(request.body, s.energy.capacity))
        };
        self.nearest(request.home, |i| fits(i) && self.helps_at(i))
            .or_else(|| self.nearest(request.home, fits))
    }

    /// `home` when it has an own spawn, else the nearest spawn room in range for which `ok` holds
    /// (by `spawn_rooms` index), for an owned `home` (ties to the lower packed name).
    fn nearest(&self, home: RoomName, ok: impl Fn(usize) -> bool) -> Option<RoomName> {
        if self.spawn_rooms.contains(&home) {
            return Some(home);
        }
        if !self.owned.contains(&home) {
            return None;
        }
        let mut best: Option<(u32, RoomName)> = None;
        // By packed name, so the first of equal distances is the lower name.
        for (i, &room) in self.spawn_rooms.iter().enumerate() {
            let d = room_distance(home, room);
            if d <= self.help_range && best.is_none_or(|(b, _)| d < b) && ok(i) {
                best = Some((d, room));
            }
        }
        best.map(|(_, room)| room)
    }

    /// Whether spawn room `i` plans the requests it serves for others this tick: [`Self::close`]
    /// allowed help, it started the tick full and it has a ready spawn.
    fn helps_at(&self, i: usize) -> bool {
        self.help && self.spawn_state.get(i).is_some_and(|s| s.full && s.ready)
    }

    /// [`Self::helps_at`] by room name.
    fn helps(&self, room: RoomName) -> bool {
        self.spawn_rooms
            .iter()
            .position(|&r| r == room)
            .is_some_and(|i| self.helps_at(i))
    }

    /// Ends this tick's requests, gives each its serving room (module docs) and keeps the serving
    /// rooms with at least one request they plan this tick (their own, or those of others on a tick
    /// they help) and at least one ready spawn (not spawning, not held): the `spawn` job's units
    /// ([`Self::rooms`]). `help`: whether rooms may serve others this tick (rustedscreep-wasm passes
    /// whether the Standard tier runs). Returns how many units there are. Counts the held spawns of
    /// serving rooms, the requests given to another room and those with no serving room.
    pub fn close(&mut self, snap: &Snapshot, help: bool) -> u32 {
        self.open = None;
        self.help = help;
        self.rooms.clear();
        // Each spawn room's ready and held spawns.
        let spawns = &snap.spawns;
        let (holds, time) = (&self.holds, self.time);
        for (&room, state) in self.spawn_rooms.iter().zip(self.spawn_state.iter_mut()) {
            state.ready = false;
            state.held = 0;
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
                if holds.iter().any(|h| h.spawn == handle && h.until > time) {
                    state.held = state.held.saturating_add(1);
                } else {
                    state.ready = true;
                }
            }
        }
        // Each request's serving room (`servers` is moved out so the requests can be read).
        let mut servers = std::mem::take(&mut self.servers);
        servers.clear();
        servers.extend(self.requests.iter().map(|r| self.server_for(r)));
        self.servers = servers;
        for (r, server) in self.requests.iter().zip(&self.servers) {
            match server {
                None => self.stats.no_spawn = self.stats.no_spawn.saturating_add(1),
                Some(s) if *s != r.home => self.stats.helped = self.stats.helped.saturating_add(1),
                Some(_) => {}
            }
        }
        for (i, &room) in self.spawn_rooms.iter().enumerate() {
            let helps = self.helps_at(i);
            let Some(state) = self.spawn_state.get(i) else {
                continue;
            };
            let mut serves = false;
            let mut asked = false;
            for (r, s) in self.requests.iter().zip(&self.servers) {
                if *s == Some(room) {
                    serves = true;
                    asked |= r.home == room || helps;
                }
            }
            if serves {
                self.stats.held = self.stats.held.saturating_add(state.held);
            }
            if asked && state.ready {
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

    /// Plans room `unit` of [`Self::rooms`]: its idle spawns take its own requests, then, on a tick
    /// it helps, those it serves for others (module docs). Each order commits `INTENT_CPU_COST`
    /// (V34). Call after [`Self::close`].
    ///
    /// Uses: `INTENT_CPU_COST`; `BODYPART_COST` and `MAX_CREEP_SIZE` through [`Body::scaled`] and
    /// [`Body::cost`].
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

        // The room's own requests first, then those it serves for others; within each, highest
        // priority first and equal priorities in request order (each goes after its equals).
        self.queue.clear();
        let helps = self.helps(room);
        let requests = &self.requests;
        let rank = |r: &SpawnRequest| (r.home == room, r.priority);
        for ((index, r), server) in (0u32..).zip(requests).zip(&self.servers) {
            if *server != Some(room) || (r.home != room && !helps) {
                continue;
            }
            let key = rank(r);
            let at = self
                .queue
                .iter()
                .position(|&i| requests.get(i as usize).is_some_and(|q| rank(q) < key))
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
                let fit = match req.body {
                    BodySpec::Scaled {
                        pattern,
                        max_repeats,
                    } => Body::scaled(pattern, energy.capacity, max_repeats).map(|at_capacity| {
                        match req.budget {
                            EnergyBudget::Capacity => Some(at_capacity),
                            EnergyBudget::Available => Body::scaled(pattern, left, max_repeats),
                            // Below one repeat it is the smallest body, as for `Available`.
                            EnergyBudget::UpTo(cap) => {
                                Body::scaled(pattern, cap.min(energy.capacity), max_repeats)
                                    .or_else(|| Body::new(pattern))
                            }
                        }
                    }),
                    BodySpec::Exact(b) => (b.cost() <= energy.capacity).then_some(Some(b)),
                };
                let Some(body) = fit else {
                    self.stats.unfit = self.stats.unfit.saturating_add(1);
                    next += 1;
                    continue;
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
                    home: req.home,
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

/// The distance between two rooms, in rooms: the larger of the differences of their world
/// coordinates (`RoomName::x_coord` and `y_coord`, V169), so each of a room's eight neighbours is 1
/// away and W0 and E0 are neighbours. The world does not wrap (V169).
pub fn room_distance(a: RoomName, b: RoomName) -> u32 {
    a.x_coord()
        .abs_diff(b.x_coord())
        .max(a.y_coord().abs_diff(b.y_coord()))
}
