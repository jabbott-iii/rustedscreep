//! Expansion (ROADMAP P8, SPEC §3.7). P8.1 (ADR-039) is scouting: the atlas of the rooms near the
//! homes, the explorer that walks into them, and the observers that look at them.
//!
//! # The atlas
//!
//! [`Atlas`] holds, per room that can have a controller ([`crate::economy::is_remote_room`]) within
//! `scout.max_distance` of a home, what the bot last saw of it ([`RoomIntel`]): its status, its
//! controller's owner, level and tile, its reservation and another owner's safe mode, its sources,
//! its mineral, the other owners' towers and spawns, keeper lairs, an invader core, and the hostile
//! creeps seen. It is persisted (ADR-039's "segment record"), so a global reset keeps it; until
//! the record's load has finished nothing is read or toured. A home is an owned room with a spawn
//! at `scout.min_level` or more.
//!
//! A room is **due** when the atlas holds nothing on it, or its entry was read (or marked
//! unreachable) `scout.refresh_ticks` ago or more ([`RoomIntel::due`]). The snapshot reads a
//! visible due room ([`crate::snapshot::IntelRead`]; [`Scouting::wants_read`] says which), at
//! most `scout.max_reads` a tick, whoever gives the vision: the signer, the remote creeps, the
//! explorer, or an observer's look. [`Scouting::begin`] puts each read in the atlas.
//!
//! # The explorer
//!
//! At most one explorer lives at a time ([`Role::Explorer`], one MOVE part). With none alive and
//! a due room the explorer could take (not one with another owner's towers seen, [`RoomIntel::towered`]),
//! the Critical plan requests one from the home nearest that room, at [`PRIORITY_EXPLORER`], on
//! ticks the Optional tier runs. Rooms the bot owns are never due (a claimed room below
//! `scout.min_level` or without a spawn is no home, and is not read), and a room an explorer died in
//! is neither a target nor on a route for `scout.refresh_ticks`. The explorer walks to the due room nearest it (one route search a
//! tick, avoiding every room with seen towers), two tiles into the room, and takes the next one
//! once the room is read. A room it finds no route to (another status, V168, or walled off) is
//! marked unreachable; a search that runs out of ops marks it from a home room and passes it over
//! elsewhere. A route longer than its remaining life marks the room unreachable from its home room
//! (no life would do); elsewhere the explorer ends its life (`suicide`), as it does with nothing
//! left it can reach, so a fresh one starts from a home instead of searching a far room a tick. One that walks into a room with
//! another owner's towers stands on the entrance's edge tile, so the engine carries it back out
//! (V151), as the signer does.
//!
//! # Observers
//!
//! Each own observer in a room at a level that allows one (`CONTROLLER_STRUCTURES`, V234) looks, on
//! each tick the Optional tier runs, at the due room within `OBSERVER_RANGE` of its room (a square
//! on the world axes, V359) and within reach of a home that was seen longest ago (a room never
//! seen first), is not visible this tick, and no other observer takes. Towered and unreachable
//! rooms are looked at too: an observer risks nothing. The look gives the room on the next tick
//! only (V362), and that tick's snapshot reads it. No look goes out when nothing is due. A look
//! whose room is not visible on the next tick (a room past the world's border, which `observeRoom`
//! accepts by its name, V358, or a refused call) marks the room unreachable, so it waits too.

mod atlas;
#[cfg(test)]
mod tests;

pub use atlas::{
    Atlas, Deposit, MAX_SOURCES, Mineral, RecordError, Reserved, RoomIntel, SCHEMA, entry_len,
};

use rustedscreep_consts::{
    CREEP_LIFE_TIME, INTENT_CPU_COST, OBSERVER_RANGE, Part, Position, RoomName, StructureType,
};

use crate::config::{MovementConfig, ScoutConfig};
use crate::economy::is_remote_room;
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{MoveError, MoveTo, Movement, RouteOutcome, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::signing::{RecordLoad, at, world_range};
use crate::snapshot::{Controller, CreepHandle, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, room_distance};

/// The explorer's spawn priority: above the signer's 70, below the remote scout's 75.
pub const PRIORITY_EXPLORER: u8 = 72;

/// A trip's goal range around the room's center: every tile at least two in from the edges (the
/// signer's look).
pub const LOOK_RANGE: u8 = crate::signing::LOOK_RANGE;

/// The room's center tile coordinate.
const CENTER: u8 = 25;

/// The explorer's move priority: the lowest, so it makes way for every other creep.
const MOVE_PRIORITY: u8 = 1;

/// A home the explorer can come from: an owned room with a spawn at `scout.min_level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Home {
    pub room: RoomName,
    /// Its controller's level. Unit: RCL.
    pub level: u8,
}

/// Where the explorer is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    pub room: RoomName,
    /// The room's center; the trip ends [`LOOK_RANGE`] from it, or once the room is read.
    pub goal: Position,
}

/// The live explorer, as the bot tracks it from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Explorer {
    pub handle: CreepHandle,
    pub home: RoomName,
    pub spawning: bool,
    /// Where it stood, and its `ticksToLive` when known, on the last tick it was seen.
    pub at: Position,
    pub ttl: Option<u32>,
    pub trip: Option<Trip>,
    /// The room of the last trip whose move found no way: a second miss there passes it over.
    pub missed: Option<RoomName>,
    /// It was told to end its life (`suicide`): when it is gone, that is no death.
    pub ending: bool,
}

/// One observer's look this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// The observer's row in the snapshot's structures (its JS object's row in the glue).
    pub row: u32,
    pub room: RoomName,
}

/// What happened this tick, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoutEvent {
    /// The room was read into the atlas: its first read, or one that changed its holder (owner,
    /// reservation, towers, core, or status), else a refresh that found it the same.
    Read {
        room: RoomName,
        first: bool,
        changed: bool,
    },
    /// The explorer found no route there; it waits `scout.refresh_ticks`.
    Unreachable { room: RoomName },
    /// An observer's look there gave no vision on the next tick (a room past the world's border,
    /// or a refused call); it waits `scout.refresh_ticks`.
    Unseen { room: RoomName },
    /// The explorer saw this many towers of another owner on entering and stepped back out.
    Retreated { room: RoomName, towers: u8 },
    /// The explorer died before its life ended, in this room.
    Died { room: RoomName },
    /// The explorer had nothing left it could reach, in this room, and ends its life.
    Done { room: RoomName },
    /// Rooms out of every home's reach were dropped from the atlas.
    Pruned { rooms: usize },
}

/// Counters since the global started, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScoutStats {
    pub reads: u32,
    pub searches: u32,
    pub unreachable: u32,
    pub deaths: u32,
    pub retreats: u32,
    pub explorers: u32,
    pub looks: u32,
}

/// Scouting for one global (module docs).
#[derive(Default)]
pub struct Scouting {
    atlas: Atlas,
    loaded: bool,
    /// The segment must not be written this global (it could not be read, or there is none).
    readonly: bool,
    dirty: bool,
    saved_at: Option<u32>,
    time: u32,
    homes: Vec<Home>,
    /// The rooms whose controller the bot owns this tick, sorted (homes or not): never due.
    owned: Vec<RoomName>,
    /// The owned rooms and reach the atlas was last pruned for.
    pruned_for: Option<(Vec<RoomName>, u32)>,
    /// Rooms an explorer died in, with the tick: neither targets nor on a route for
    /// `scout.refresh_ticks` (the heap only).
    deaths: Vec<(RoomName, u32)>,
    explorer: Option<Explorer>,
    /// Explorers beyond the first (a reset can leave two): each ends its life.
    extras: Vec<CreepHandle>,
    /// Rooms the live explorer passed over this life (too far for what it has left).
    skip: Vec<RoomName>,
    /// With no explorer and nothing due it could take, nothing is requested before this tick.
    idle_until: Option<u32>,
    path: Vec<Position>,
    avoid: Vec<RoomName>,
    looks: Vec<Look>,
    events: Vec<ScoutEvent>,
    stats: ScoutStats,
}

impl Scouting {
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    pub fn explorer(&self) -> Option<&Explorer> {
        self.explorer.as_ref()
    }

    pub fn homes(&self) -> &[Home] {
        &self.homes
    }

    pub fn events(&self) -> &[ScoutEvent] {
        &self.events
    }

    pub fn stats(&self) -> &ScoutStats {
        &self.stats
    }

    /// This tick's observer looks ([`Self::observe`]), for the act step.
    pub fn looks(&self) -> &[Look] {
        &self.looks
    }

    /// Whether the record's load has finished (or there is none to load).
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Whether the record has changes not written yet.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the record should be loaded now: scouting runs and a home has reached
    /// `scout.min_level` (until then its segment is not even requested).
    pub fn wants_record(&self, cfg: &ScoutConfig) -> bool {
        cfg.enabled && !self.loaded && !self.homes.is_empty()
    }

    /// The record's load finished. A refused record is written again on the next save; with
    /// [`RecordLoad::Unavailable`] the segment is never written this global, so a read that failed
    /// cannot replace a good record with an empty one.
    pub fn loaded(&mut self, load: RecordLoad<'_>) -> Result<(), RecordError> {
        self.loaded = true;
        self.idle_until = None;
        match load {
            RecordLoad::Empty => Ok(()),
            RecordLoad::Payload(bytes) => match Atlas::decode(bytes) {
                Ok(atlas) => {
                    self.atlas = atlas;
                    Ok(())
                }
                Err(e) => {
                    self.dirty = true;
                    Err(e)
                }
            },
            RecordLoad::Refused => {
                self.dirty = true;
                Ok(())
            }
            RecordLoad::Unavailable => {
                self.readonly = true;
                Ok(())
            }
        }
    }

    /// No segment keeps the record: start at once, and never write.
    pub fn no_record(&mut self) {
        if !self.loaded {
            self.loaded = true;
            self.readonly = true;
        }
    }

    /// Whether the record should be written now: it has loaded, it changed, it may be written this
    /// global, and `scout.save_ticks` passed since the last write.
    pub fn save_due(&self, cfg: &ScoutConfig) -> bool {
        self.loaded
            && !self.readonly
            && self.dirty
            && self
                .saved_at
                .is_none_or(|t| self.time >= t.saturating_add(cfg.save_ticks))
    }

    /// [`Self::save_due`], and if so the record counts as saved now (the persistence glue then
    /// writes it, and retries a failed write itself).
    pub fn take_save(&mut self, cfg: &ScoutConfig) -> bool {
        let due = self.save_due(cfg);
        if due {
            self.dirty = false;
            self.saved_at = Some(self.time);
        }
        due
    }

    /// Appends the record's payload to `out`, at most `max_len` bytes ([`Atlas::encode`]).
    /// Returns how many rooms it left out.
    pub fn payload(&self, out: &mut Vec<u8>, max_len: usize) -> usize {
        self.atlas.encode(out, max_len)
    }

    /// Whether `room` lies within `scout.max_distance` of a home.
    fn in_reach(&self, room: RoomName, cfg: &ScoutConfig) -> bool {
        self.homes
            .iter()
            .any(|h| room_distance(h.room, room) <= cfg.max_distance)
    }

    /// Whether the snapshot should read `room`, a visible room the bot does not own, for the atlas
    /// at `now` (before [`Self::begin`]: the homes are last tick's): scouting runs, the record has
    /// loaded, the room can have a controller, lies within reach of a home, and is due.
    pub fn wants_read(&self, room: RoomName, now: u32, cfg: &ScoutConfig) -> bool {
        cfg.enabled
            && self.loaded
            && is_remote_room(room)
            && self.in_reach(room, cfg)
            && self.atlas.due(room, now, cfg.refresh_ticks)
    }

    /// Starts the tick (no JS call): the owned rooms and homes, last tick's looks that gave no
    /// vision, the explorers, a lost explorer, this tick's reads, and a prune when the owned rooms
    /// or the reach changed.
    pub fn begin(&mut self, snap: &Snapshot, cfg: &ScoutConfig) {
        self.time = snap.tick.time;
        self.events.clear();
        self.extras.clear();
        self.read_owned(snap);
        if self.loaded && cfg.enabled {
            self.check_looks(snap);
        }
        self.looks.clear();
        let now = self.time;
        self.deaths
            .retain(|&(_, t)| now < t.saturating_add(cfg.refresh_ticks));
        self.read_homes(snap, cfg);
        self.find_explorer(snap);
        if self.loaded && cfg.enabled {
            self.record_reads(snap);
            self.prune(cfg);
        }
    }

    fn read_owned(&mut self, snap: &Snapshot) {
        let rooms = &snap.rooms;
        self.owned.clear();
        self.owned.extend(
            rooms
                .names()
                .iter()
                .zip(rooms.controllers())
                .filter(|(_, c)| matches!(c, Controller::Mine { .. }))
                .map(|(&n, _)| n),
        );
        self.owned.sort_unstable_by_key(|r| r.packed_repr());
    }

    /// Whether the bot owns `room` this tick.
    fn is_owned(&self, room: RoomName) -> bool {
        self.owned
            .binary_search_by_key(&room.packed_repr(), |r| r.packed_repr())
            .is_ok()
    }

    /// Whether an explorer died in `room` within `scout.refresh_ticks`.
    fn lost_in(&self, room: RoomName) -> bool {
        self.deaths.iter().any(|&(r, _)| r == room)
    }

    /// Last tick's looks whose room is not visible now gave no vision (a room past the world's
    /// border, or a refused call): each waits `scout.refresh_ticks`, so no observer looks at it
    /// every tick.
    fn check_looks(&mut self, snap: &Snapshot) {
        let now = self.time;
        for i in 0..self.looks.len() {
            let Some(room) = self.looks.get(i).map(|l| l.room) else {
                continue;
            };
            if snap.rooms.row_of(room).is_none() {
                self.atlas.mark_unreachable(room, now);
                self.dirty = true;
                self.events.push(ScoutEvent::Unseen { room });
            }
        }
    }

    fn read_homes(&mut self, snap: &Snapshot, cfg: &ScoutConfig) {
        let before = self.homes.len();
        let mut changed = false;
        let mut n = 0;
        if cfg.enabled {
            let rooms = &snap.rooms;
            let st = &snap.structures;
            let spawns = &snap.spawns;
            for (&name, &controller) in rooms.names().iter().zip(rooms.controllers()) {
                let Controller::Mine { level } = controller else {
                    continue;
                };
                if u32::from(level) < cfg.min_level {
                    continue;
                }
                let has_spawn = spawns.rows().iter().any(|&r| {
                    usize::try_from(r)
                        .ok()
                        .and_then(|r| st.positions().get(r))
                        .is_some_and(|p| p.room_name() == name)
                });
                if !has_spawn {
                    continue;
                }
                let home = Home { room: name, level };
                match self.homes.get_mut(n) {
                    Some(h) if *h == home => {}
                    Some(h) => {
                        *h = home;
                        changed = true;
                    }
                    None => {
                        self.homes.push(home);
                        changed = true;
                    }
                }
                n += 1;
            }
        }
        self.homes.truncate(n);
        if changed || n != before {
            self.homes.sort_by_key(|h| h.room.packed_repr());
            self.idle_until = None;
        }
    }

    fn find_explorer(&mut self, snap: &Snapshot) {
        let creeps = &snap.creeps;
        let now = self.time;
        let mut found: Option<(CreepHandle, u64, usize)> = None;
        let mut kept = None;
        let tracked = self.explorer.map(|e| e.handle);
        for (row, (id, (&mine, &h))) in creeps
            .identities()
            .iter()
            .zip(creeps.my().iter().zip(creeps.handles()))
            .enumerate()
        {
            let Some(id) = id else {
                continue;
            };
            if !mine || id.role != Role::Explorer {
                continue;
            }
            if Some(h) == tracked {
                kept = Some((h, id.serial, row));
            }
            if found.is_none_or(|f| id.serial < f.1) {
                found = Some((h, id.serial, row));
            }
            self.extras.push(h);
        }
        let chosen = kept.or(found);
        self.extras.retain(|&h| Some(h) != chosen.map(|c| c.0));
        let Some((handle, _, row)) = chosen else {
            if let Some(gone) = self.explorer.take() {
                self.lost(gone);
            }
            return;
        };
        let at = creeps.positions().get(row).copied();
        let spawning = creeps.spawning().get(row).copied().unwrap_or(false);
        let ttl = creeps.ticks_to_live(row, now);
        let home = creeps
            .identities()
            .get(row)
            .copied()
            .flatten()
            .map(|id| id.home);
        let (Some(at), Some(home)) = (at, home) else {
            return;
        };
        match self.explorer.as_mut() {
            Some(e) if e.handle == handle => {
                e.at = at;
                e.ttl = ttl;
                e.spawning = spawning;
            }
            _ => {
                if let Some(gone) = self.explorer.take() {
                    self.lost(gone);
                }
                self.skip.clear();
                self.stats.explorers = self.stats.explorers.saturating_add(1);
                self.explorer = Some(Explorer {
                    handle,
                    home,
                    spawning,
                    at,
                    ttl,
                    trip: None,
                    missed: None,
                    ending: false,
                });
            }
        }
    }

    /// The tracked explorer is gone: a death unless it died of age (its last `ticksToLive` 1,
    /// V198), was told to end, or never left the spawn.
    fn lost(&mut self, gone: Explorer) {
        self.skip.clear();
        self.idle_until = None;
        let aged = gone.ttl.is_none_or(|t| t <= 1);
        if aged || gone.ending || gone.spawning {
            return;
        }
        let room = gone.at.room_name();
        self.stats.deaths = self.stats.deaths.saturating_add(1);
        self.events.push(ScoutEvent::Died { room });
        // The next explorers keep out of it for a while, so they do not die there one after another.
        self.deaths.retain(|&(r, _)| r != room);
        self.deaths.push((room, self.time));
    }

    /// Puts each room read for the atlas this tick in it.
    fn record_reads(&mut self, snap: &Snapshot) {
        let rooms = &snap.rooms;
        let now = self.time;
        for ((&name, &controller), read) in rooms
            .names()
            .iter()
            .zip(rooms.controllers())
            .zip(rooms.intel_reads())
        {
            if !read.read {
                continue;
            }
            let intel = RoomIntel::from_read(name, controller, read, now);
            let before = self.atlas.put(intel);
            let first = before.is_none_or(|b| !b.known);
            let changed = !first && before.is_some_and(|b| !b.same_holder(&intel));
            self.dirty = true;
            self.idle_until = None;
            self.stats.reads = self.stats.reads.saturating_add(1);
            self.events.push(ScoutEvent::Read {
                room: name,
                first,
                changed,
            });
        }
    }

    /// Drops the entries out of every owned room's reach, when the owned rooms or
    /// `scout.max_distance` changed (never with none: a lost room's neighbors stay known). Owned
    /// rooms, not homes, so a home that loses its spawns keeps its rooms.
    fn prune(&mut self, cfg: &ScoutConfig) {
        if self.owned.is_empty() {
            return;
        }
        let same = self
            .pruned_for
            .as_ref()
            .is_some_and(|(rooms, reach)| *reach == cfg.max_distance && *rooms == self.owned);
        if same {
            return;
        }
        let owned = &self.owned;
        let dropped = self.atlas.retain(|e| {
            owned
                .iter()
                .any(|&o| room_distance(o, e.room) <= cfg.max_distance)
        });
        if dropped > 0 {
            self.dirty = true;
            self.events.push(ScoutEvent::Pruned { rooms: dropped });
        }
        self.pruned_for = Some((self.owned.clone(), cfg.max_distance));
    }

    /// Whether the Critical plan has a request to make: scouting runs, the record has loaded, no
    /// explorer is alive, a home exists, and the idle wait (nothing due) is over.
    pub fn wants_request(&self, cfg: &ScoutConfig) -> bool {
        cfg.enabled
            && self.loaded
            && self.explorer.is_none()
            && !self.homes.is_empty()
            && self.idle_until.is_none_or(|t| self.time >= t)
    }

    /// The Critical plan (one unit): an explorer's request from the home nearest the due room
    /// nearest a home, while the Optional tier runs (`optional`) and none is alive. With nothing
    /// due, nothing is requested before the earliest tick a room comes due.
    pub fn request(&mut self, spawner: &mut Spawner, cfg: &ScoutConfig, optional: bool) {
        if !optional || !self.wants_request(cfg) {
            return;
        }
        let (room, next) = self.nearest_due(None, cfg);
        let Some(room) = room else {
            self.idle_until = Some(next);
            return;
        };
        let Some(home) = self.nearest_home(room) else {
            return;
        };
        let Some(body) = Body::new(&[Part::Move]) else {
            return;
        };
        let _ = spawner.request(SpawnRequest {
            home: home.room,
            role: Role::Explorer,
            remote: None,
            body: BodySpec::Exact(body),
            budget: EnergyBudget::Capacity,
            priority: PRIORITY_EXPLORER,
        });
    }

    /// Whether the Optional work has an explorer to work or end. With none, its job runs no unit.
    pub fn has_work(&self, cfg: &ScoutConfig) -> bool {
        self.loaded
            && (!self.extras.is_empty()
                || (cfg.enabled && self.explorer.is_some_and(|e| !e.spawning)))
    }

    /// The Optional work (one unit): the extra explorers end, and the live explorer walks.
    #[allow(clippy::too_many_arguments)]
    pub fn work(
        &mut self,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &ScoutConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        if !self.loaded {
            return Ok(());
        }
        for i in 0..self.extras.len() {
            if let Some(&h) = self.extras.get(i) {
                let _ = buffer.push(snap, h, CreepIntent::Suicide, commit);
            }
        }
        if !cfg.enabled {
            return Ok(());
        }
        match self.explorer {
            Some(e) if !e.spawning => {
                self.work_explorer(e, snap, movement, buffer, cfg, mcfg, commit)
            }
            _ => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn work_explorer(
        &mut self,
        e: Explorer,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &ScoutConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let here = e.at;
        let room = here.room_name();
        let on_edge = Tile::of(here).is_edge();
        let now = self.time;
        // A room with another owner's towers: on the entrance's edge it stands, and the engine
        // carries it back (V151).
        if let Some(entry) = self.atlas.get(room)
            && entry.towered()
            && !self.homes.iter().any(|h| h.room == room)
        {
            if !self.skip.contains(&room) {
                self.skip.push(room);
                self.stats.retreats = self.stats.retreats.saturating_add(1);
                self.events.push(ScoutEvent::Retreated {
                    room,
                    towers: entry.towers,
                });
            }
            if on_edge {
                self.set_trip(None);
                return Ok(());
            }
        }
        // The trip ends once its room is no longer due.
        let mut trip = e
            .trip
            .filter(|t| self.atlas.due(t.room, now, cfg.refresh_ticks) && !self.is_owned(t.room));
        if trip.is_none() && !e.ending {
            match self.choose(here, e.ttl, movement, cfg, mcfg, commit) {
                Choice::Trip(t) => trip = Some(t),
                Choice::Later => {}
                Choice::Nothing => {
                    self.events.push(ScoutEvent::Done { room });
                    self.set_trip(None);
                    if buffer
                        .push(snap, e.handle, CreepIntent::Suicide, commit)
                        .is_ok()
                        && let Some(me) = self.explorer.as_mut()
                    {
                        me.ending = true;
                    }
                    return Ok(());
                }
            }
        }
        self.set_trip(trip);
        let Some(t) = trip else {
            // With no trip on an edge tile, step in: left there, the engine carries it across
            // (V151), back into a room it may just have stepped out of.
            if on_edge
                && !e.ending
                && let Some(c) = Tile::new(CENTER, CENTER).and_then(|c| at(room, c))
            {
                let to = MoveTo {
                    target: c,
                    range: LOOK_RANGE,
                    priority: MOVE_PRIORITY,
                    tier: Tier::Optional,
                };
                let _ = movement.request(snap, e.handle, to, mcfg, commit);
            }
            return Ok(());
        };
        let to = MoveTo {
            target: t.goal,
            range: LOOK_RANGE,
            priority: MOVE_PRIORITY,
            tier: Tier::Optional,
        };
        if let Err(MoveError::NoPath) = movement.request(snap, e.handle, to, mcfg, commit) {
            if e.missed == Some(t.room) {
                self.pass_over(t.room);
            } else if let Some(me) = self.explorer.as_mut() {
                // Movement's own route was dropped or failed: the next choice searches again with
                // the explorer's limits and avoided rooms.
                me.missed = Some(t.room);
            }
            self.set_trip(None);
        }
        Ok(())
    }

    fn set_trip(&mut self, trip: Option<Trip>) {
        if let Some(e) = self.explorer.as_mut() {
            e.trip = trip;
        }
    }

    /// The live explorer leaves `room` out for the rest of its life.
    fn pass_over(&mut self, room: RoomName) {
        if !self.skip.contains(&room) {
            self.skip.push(room);
        }
    }

    fn unreachable(&mut self, room: RoomName) {
        self.atlas.mark_unreachable(room, self.time);
        self.dirty = true;
        self.stats.unreachable = self.stats.unreachable.saturating_add(1);
        self.events.push(ScoutEvent::Unreachable { room });
    }

    /// The live explorer's next trip from `here`: the due room nearest it that it has not passed
    /// over and that shows no towers, if a route there fits its life. One search a tick, none when
    /// even the shortest walk (the Chebyshev distance in world tiles, less the range) would not
    /// fit: then nothing nearer is left.
    fn choose(
        &mut self,
        here: Position,
        ttl: Option<u32>,
        movement: &mut Movement,
        cfg: &ScoutConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Choice {
        let start = here.room_name();
        let Some(room) = self.nearest_due(Some(here), cfg).0 else {
            return Choice::Nothing;
        };
        let Some(goal) = Tile::new(CENTER, CENTER).and_then(|c| at(room, c)) else {
            return Choice::Nothing;
        };
        let life = ttl.unwrap_or(CREEP_LIFE_TIME);
        let spare = cfg.life_margin_ticks.saturating_add(2);
        let shortest = world_range(here, goal).saturating_sub(u32::from(LOOK_RANGE));
        if shortest.saturating_add(spare) > life {
            return Choice::Nothing;
        }
        // Rooms with seen towers; never the target's room, nor the start's (a search could not
        // leave it).
        self.avoid.clear();
        self.avoid.extend(
            self.atlas
                .entries()
                .iter()
                .filter(|e| e.towered() && e.room != room && e.room != start)
                .map(|e| e.room),
        );
        // And the rooms explorers died in lately.
        for &(r, _) in &self.deaths {
            if r != room && r != start && !self.avoid.contains(&r) {
                self.avoid.push(r);
            }
        }
        let limits = (
            cfg.route_max_ops,
            u8::try_from(cfg.route_max_rooms).unwrap_or(u8::MAX),
        );
        self.stats.searches = self.stats.searches.saturating_add(1);
        let outcome = movement.search_route(
            here,
            goal,
            LOOK_RANGE,
            limits,
            &self.avoid,
            mcfg,
            commit,
            &mut self.path,
        );
        match outcome {
            None => Choice::Later,
            Some(RouteOutcome::Found) => {
                let steps = u32::try_from(self.path.len()).unwrap_or(u32::MAX);
                if steps.saturating_add(spare) <= life {
                    if let Some(me) = self.explorer.as_mut()
                        && me.missed != Some(room)
                    {
                        me.missed = None;
                    }
                    Choice::Trip(Trip { room, goal })
                } else if self.homes.iter().any(|h| h.room == start) {
                    // From a home no explorer's life would do.
                    self.unreachable(room);
                    Choice::Later
                } else {
                    // The nearest due room is too far for what is left, and farther ones mostly
                    // are too: end, so a fresh explorer starts from a home (no search a tick for
                    // each far room in turn).
                    self.pass_over(room);
                    Choice::Nothing
                }
            }
            // A room of another status (V168) or walled off: no route from anywhere.
            Some(RouteOutcome::Failed) => {
                self.unreachable(room);
                Choice::Later
            }
            // The search ran out of ops: unreachable from a home, passed over from elsewhere.
            Some(RouteOutcome::Incomplete) => {
                if self.homes.iter().any(|h| h.room == start) {
                    self.unreachable(room);
                } else {
                    self.pass_over(room);
                }
                Choice::Later
            }
        }
    }

    /// The home nearest `room` (then the first by name).
    fn nearest_home(&self, room: RoomName) -> Option<Home> {
        self.homes
            .iter()
            .min_by_key(|h| (room_distance(h.room, room), h.room.packed_repr()))
            .copied()
    }

    /// The due room the explorer could take nearest `origin` (the live explorer's position, by world
    /// tiles to the room's center; `None` for the next explorer: the nearest home, in rooms), then
    /// the first by packed name, leaving out the homes, the rooms with seen towers and, for the
    /// live explorer, the rooms it passed over. With none, also the earliest tick one comes due.
    fn nearest_due(&self, origin: Option<Position>, cfg: &ScoutConfig) -> (Option<RoomName>, u32) {
        let now = self.time;
        let reach = i32::try_from(cfg.max_distance).unwrap_or(0);
        let mut best: Option<(u32, u16, RoomName)> = None;
        let mut next = u32::MAX;
        for (i, h) in self.homes.iter().enumerate() {
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    let Some(room) = h.room.checked_add((dx, dy)) else {
                        continue;
                    };
                    if !is_remote_room(room) || self.is_owned(room) || self.lost_in(room) {
                        continue;
                    }
                    // A room within reach of an earlier home was counted there.
                    if self
                        .homes
                        .iter()
                        .take(i)
                        .any(|o| room_distance(o.room, room) <= cfg.max_distance)
                    {
                        continue;
                    }
                    let entry = self.atlas.get(room);
                    if entry.is_some_and(RoomIntel::towered) {
                        continue;
                    }
                    if let Some(e) = entry
                        && !e.due(now, cfg.refresh_ticks)
                    {
                        next = next.min(e.seen.saturating_add(cfg.refresh_ticks));
                        continue;
                    }
                    let distance = match origin {
                        Some(o) => {
                            if self.skip.contains(&room) {
                                continue;
                            }
                            Tile::new(CENTER, CENTER)
                                .and_then(|c| at(room, c))
                                .map_or(u32::MAX, |c| world_range(o, c))
                        }
                        None => self
                            .homes
                            .iter()
                            .map(|o| room_distance(o.room, room))
                            .min()
                            .unwrap_or(u32::MAX),
                    };
                    let key = (distance, room.packed_repr(), room);
                    if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
                        best = Some(key);
                    }
                }
            }
        }
        (best.map(|b| b.2), next)
    }

    /// How many rooms in reach the explorer could take are due now, and the earliest tick another
    /// comes due (`u32::MAX` for none), for the console.
    pub fn due_count(&self, cfg: &ScoutConfig) -> (u32, u32) {
        let now = self.time;
        let reach = i32::try_from(cfg.max_distance).unwrap_or(0);
        let mut due = 0u32;
        let mut next = u32::MAX;
        for (i, h) in self.homes.iter().enumerate() {
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    let Some(room) = h.room.checked_add((dx, dy)) else {
                        continue;
                    };
                    if !is_remote_room(room)
                        || self.is_owned(room)
                        || self
                            .homes
                            .iter()
                            .take(i)
                            .any(|o| room_distance(o.room, room) <= cfg.max_distance)
                    {
                        continue;
                    }
                    match self.atlas.get(room) {
                        Some(e) if !e.due(now, cfg.refresh_ticks) => {
                            next = next.min(e.seen.saturating_add(cfg.refresh_ticks));
                        }
                        _ => due = due.saturating_add(1),
                    }
                }
            }
        }
        (due, next)
    }

    /// The Optional observers' looks (one unit, no JS call): for each own observer that is active
    /// at its room's level, the due room within `OBSERVER_RANGE` and within reach of a home seen
    /// longest ago, not visible this tick and not taken by another look. The act step issues them.
    ///
    /// Uses: `OBSERVER_RANGE` (V359), `StructureType::controller_structures` (V234).
    pub fn observe(&mut self, snap: &Snapshot, cfg: &ScoutConfig, commit: &mut Commit) {
        self.looks.clear();
        if !cfg.enabled || !cfg.observers || !self.loaded {
            return;
        }
        let now = self.time;
        let st = &snap.structures;
        let rooms = &snap.rooms;
        let range = i32::try_from(OBSERVER_RANGE).unwrap_or(0);
        for (row, (&kind, &p)) in st.kinds().iter().zip(st.positions()).enumerate() {
            if kind != StructureType::Observer {
                continue;
            }
            let from = p.room_name();
            let level = rooms
                .row_of(from)
                .and_then(|r| match rooms.controllers().get(r) {
                    Some(Controller::Mine { level }) => Some(*level),
                    _ => None,
                });
            let Some(level) = level else {
                continue;
            };
            if StructureType::Observer.controller_structures(u32::from(level)) == 0 {
                continue;
            }
            let Ok(row) = u32::try_from(row) else {
                continue;
            };
            let mut best: Option<(u32, u32, u16, RoomName)> = None;
            for dx in -range..=range {
                for dy in -range..=range {
                    let Some(room) = from.checked_add((dx, dy)) else {
                        continue;
                    };
                    if !is_remote_room(room) || !self.in_reach(room, cfg) {
                        continue;
                    }
                    let entry = self.atlas.get(room);
                    if entry.is_some_and(|e| !e.due(now, cfg.refresh_ticks))
                        || rooms.row_of(room).is_some()
                        || self.looks.iter().any(|l| l.room == room)
                    {
                        continue;
                    }
                    // Never read first (0), then the oldest read, then the nearest.
                    let seen = entry
                        .filter(|e| e.known)
                        .map_or(0, |e| e.seen.saturating_add(1));
                    let key = (seen, room_distance(from, room), room.packed_repr(), room);
                    if best.is_none_or(|b| (key.0, key.1, key.2) < (b.0, b.1, b.2)) {
                        best = Some(key);
                    }
                }
            }
            if let Some(b) = best {
                self.looks.push(Look { row, room: b.3 });
                self.stats.looks = self.stats.looks.saturating_add(1);
                // The call is billed (V360): counted against the tier's cap.
                commit.add(INTENT_CPU_COST);
            }
        }
    }
}

/// How a trip choice ended this tick.
enum Choice {
    Trip(Trip),
    /// Nothing yet: a room was passed over or marked unreachable, or no route search is installed;
    /// the next tick tries again.
    Later,
    /// No due room is left that this explorer could take.
    Nothing,
}

/// Whether `to` lies within an observer's range of `from` (a square of `OBSERVER_RANGE` on the
/// world axes, with no wrap, V359; the power effect `PWR_OPERATE_OBSERVER`, which lifts the limit,
/// is not modeled).
///
/// Uses: `OBSERVER_RANGE`.
pub fn in_observer_range(from: RoomName, to: RoomName) -> bool {
    room_distance(from, to) <= OBSERVER_RANGE
}
