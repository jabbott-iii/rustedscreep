//! The controller signer (ADR-030): one cheap creep for the whole bot that signs every controller
//! it can reach with [`SIGN_TEXT`], whoever owns the room.
//!
//! # What it knows
//!
//! [`Book`] holds, per room, what the bot last saw of its controller ([`SignState`]) and when, the
//! controller's tile, the other owner's towers, and the signers lost and signs replaced there. It is
//! persisted (ADR-030's "persist and recheck"), so a global reset does not send a signer back to
//! rooms already signed; until the record's load has finished nothing is planned. A room is due
//! ([`Entry::due_at`]) when the book holds nothing on it, when it was seen without the text, or
//! when its recheck (`sign.recheck_ticks`) or retry (`sign.retry_ticks`) comes due.
//!
//! The candidates are the rooms whose names say they can have a controller
//! ([`crate::economy::is_remote_room`]) within `sign.max_distance` rooms of a home (an owned room
//! with a spawn), the homes' own rooms included. The room an own signer stands in is read each tick
//! ([`crate::snapshot::SignRead`]: the controller's sign, and the towers of another owner), and the
//! read updates the room's entry.
//!
//! # The way first
//!
//! A signer is given a room only once a route there is known: [`Movement::search_route`] from where
//! the signer stands (or from the home spawn, for the next signer) to a tile next to the controller
//! (when its tile is known), or two tiles into the room (a look, for a room no creep has seen; vision
//! then gives the controller). The route search fails at once for a room whose status differs from
//! the start's (walled off or closed, V168). It avoids every room with seen towers but the target.
//! The route it finds is the one the creep walks. A room with no route, or farther than one creep
//! life from the home spawn, is [`SignState::Unreachable`]; one farther than the live signer's
//! remaining life is passed over by that signer only. A controller no tile next to which can be
//! reached is unreachable too.
//!
//! # One signer, and its body
//!
//! At most one signer lives at a time. With none alive, the Optional work plans the next one
//! ([`Plan`]): the due room nearest a home, a route from that home's spawn, and a body. A trip that
//! no seen tower reaches gets [`plain_signer`] (one MOVE part). A room with seen towers gets
//! [`signer_body`] for the worst case of its towers along the route, after its rooms' terrain has
//! been fed in ([`Signing::terrain_wanted`], [`Signing::feed_terrain`]); a room no body survives is
//! [`SignState::Deadly`]. The next tick's Critical plan requests the planned body, at
//! [`PRIORITY_SIGNER`], while the Optional tier runs. When nothing is due, nothing is planned until
//! the earliest due tick.
//!
//! The live signer walks its trip, signs when it stands next to a controller that does not show the
//! text, and then takes the due room nearest it that it can reach and that no seen tower reaches.
//! A signer that walks into a room with another owner's towers it was not sized for stands on the
//! entrance's edge tile, so the engine carries it back out (V151). One that dies before its life
//! ends counts a death in the room it was in. One with nothing left it can reach ends its life
//! (`suicide`), so the next can start from home.

mod body;
mod book;
#[cfg(test)]
mod tests;

pub use body::{Exposure, Step, Walked, damage_at, plain_signer, signer_body, step_at};
pub use book::{Book, Entry, MAX_TOWERS, RecordError, SCHEMA, SignState, payload_len};

use rustedscreep_consts::{CREEP_LIFE_TIME, Position, RoomCoordinate, RoomName, StructureType};

use crate::config::{MovementConfig, SignConfig};
use crate::economy::is_remote_room;
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{MoveError, MoveTo, Movement, RouteOutcome, TerrainGrid, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::snapshot::{
    Controller, CreepHandle, ObjectHandle, ObjectKind, SignRead, SignSeen, Snapshot,
};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, room_distance};

/// The text every controller gets (the user's, 2026-11-22).
pub const SIGN_TEXT: &str =
    "jabbott.dev rustedscreep bot - https://github.com/jabbott-iii/rustedscreep";

/// The signer's spawn priority: below every other role (the scout's is 75).
pub const PRIORITY_SIGNER: u8 = 70;

/// A look's goal range around the room's center: every tile at least two in from the edges.
pub const LOOK_RANGE: u8 = 23;

/// The room's center tile coordinate.
const CENTER: u8 = 25;

/// The signer's move priority: the lowest, so it makes way for every other creep.
const MOVE_PRIORITY: u8 = 1;

/// A home a signer can come from: an owned room with a spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Home {
    pub room: RoomName,
    /// Its first spawn by key: where the next signer's route search starts.
    pub spawn: Position,
    /// `energyCapacityAvailable`. Unit: energy.
    pub capacity: u32,
}

/// Where a signer is going, and what for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    pub room: RoomName,
    pub goal: Position,
    pub range: u8,
    /// A look: the controller is not known yet, so the signer walks in to see it.
    pub look: bool,
    /// Seen towers reach the way: only the signer sized for it takes it.
    pub dangerous: bool,
}

/// The next signer, planned while none is alive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Plan {
    #[default]
    None,
    /// A towered room's way, whose rooms' terrain is fed in one room at a time.
    Sizing {
        home: Home,
        trip: Trip,
        path: Vec<Position>,
        /// The next path tile to classify.
        cursor: usize,
        exposure: Exposure,
        towers: Vec<Tile>,
    },
    /// The body to request and the trip the new signer starts with.
    Ready {
        home: RoomName,
        body: Body,
        trip: Trip,
        /// The tick it was planned: a plan older than a creep life is made again.
        at: u32,
    },
}

/// What happened this tick, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignEvent {
    /// The signer's sign shows in this room.
    Signed { room: RoomName },
    /// The room waits `sign.retry_ticks`, in this state.
    GaveUp { room: RoomName, state: SignState },
    /// The signer died before its life ended, in this room, the room's `deaths`-th.
    Died { room: RoomName, deaths: u8 },
    /// A signer saw this many towers of another owner on entering and stepped back out.
    Retreated { room: RoomName, towers: u32 },
    /// The next signer is planned for this room, with `tough` TOUGH and `moves` MOVE parts.
    Planned {
        room: RoomName,
        tough: u32,
        moves: u32,
        cost: u32,
    },
    /// The signer had nothing left it could reach, in this room, and ends its life.
    Done { room: RoomName },
}

/// Counters since the global started, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignStats {
    pub signed: u32,
    pub searches: u32,
    pub deaths: u32,
    pub retreats: u32,
    pub planned: u32,
}

/// The live signer, as the bot tracks it from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signer {
    pub handle: CreepHandle,
    pub home: RoomName,
    pub spawning: bool,
    /// Where it stood, and its `ticksToLive` when known, on the last tick it was seen.
    pub at: Position,
    pub ttl: Option<u32>,
    pub trip: Option<Trip>,
    /// Its trip's route was searched from where it stands: a planned trip is searched again on its
    /// first tick out of the spawn, so the route movement keeps for it is the signer's own (its
    /// limits, the towered rooms avoided), not one movement found after the plan's was dropped.
    pub checked: bool,
    /// The room of the last trip whose move found no way: a second miss there passes it over.
    pub missed: Option<RoomName>,
    /// It was told to end its life (`suicide`): when it is gone, that is no death.
    pub ending: bool,
}

/// How the record's load ended, for [`Signing::loaded`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordLoad<'a> {
    /// A frame of this owner and schema: its payload.
    Payload(&'a [u8]),
    /// An empty segment (never written, or cleared).
    Empty,
    /// Text that is not this owner's frame, or of another schema.
    Refused,
    /// The segment was not loaded after its retries.
    Unavailable,
}

/// The controller signer for one global (module docs).
#[derive(Default)]
pub struct Signing {
    book: Book,
    loaded: bool,
    /// The segment must not be written this global (it could not be read, or there is none).
    readonly: bool,
    dirty: bool,
    saved_at: Option<u32>,
    time: u32,
    homes: Vec<Home>,
    signer: Option<Signer>,
    /// Signers beyond the first (a reset can leave two): each ends its life.
    extras: Vec<CreepHandle>,
    plan: Plan,
    /// Rooms the live signer passed over this life (too far for what it has left).
    skip: Vec<RoomName>,
    /// With no signer and nothing due, nothing is planned before this tick.
    idle_until: Option<u32>,
    /// The room a sign was issued in, until a read shows it.
    signing: Option<RoomName>,
    path: Vec<Position>,
    avoid: Vec<RoomName>,
    events: Vec<SignEvent>,
    stats: SignStats,
}

impl Signing {
    pub fn book(&self) -> &Book {
        &self.book
    }

    pub fn signer(&self) -> Option<&Signer> {
        self.signer.as_ref()
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn homes(&self) -> &[Home] {
        &self.homes
    }

    pub fn events(&self) -> &[SignEvent] {
        &self.events
    }

    pub fn stats(&self) -> &SignStats {
        &self.stats
    }

    /// Whether the record's load has finished (or there is none to load).
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Whether the record should be loaded now: the signer runs and a home has reached
    /// `sign.min_level` (until then its segment is not even requested).
    pub fn wants_record(&self, cfg: &SignConfig) -> bool {
        cfg.enabled && !self.loaded && !self.homes.is_empty()
    }

    /// Whether the Critical plan has a request to make: a planned signer and none alive.
    pub fn wants_request(&self, cfg: &SignConfig) -> bool {
        cfg.enabled
            && self.loaded
            && self.signer.is_none()
            && matches!(self.plan, Plan::Ready { .. })
    }

    /// Whether the Optional work has anything to do this tick: a signer to work or end, or (with a
    /// route search installed) a plan to make or check. With none, its job runs no unit.
    pub fn has_work(&self, cfg: &SignConfig, router: bool) -> bool {
        if !self.loaded {
            return false;
        }
        if !self.extras.is_empty() {
            return true;
        }
        if let Some(s) = self.signer {
            return !s.spawning;
        }
        if !cfg.enabled || !router || self.homes.is_empty() {
            return false;
        }
        let now = self.time;
        match &self.plan {
            Plan::None => self.idle_until.is_none_or(|t| now >= t),
            Plan::Sizing { .. } => true,
            // Only to drop a plan whose room is no longer due, or one older than a creep life.
            Plan::Ready { trip, at, .. } => {
                self.book.due_at(trip.room, cfg) > now || now >= at.saturating_add(CREEP_LIFE_TIME)
            }
        }
    }

    /// The record's load finished. A refused record is written again on the next save; with
    /// [`RecordLoad::Unavailable`] the segment is never written this global, so a read that failed
    /// cannot replace a good record with an empty one.
    pub fn loaded(&mut self, load: RecordLoad<'_>) -> Result<(), RecordError> {
        self.loaded = true;
        self.idle_until = None;
        match load {
            RecordLoad::Empty => Ok(()),
            RecordLoad::Payload(bytes) => match Book::decode(bytes) {
                Ok(book) => {
                    self.book = book;
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
    /// global, and `sign.save_ticks` passed since the last write.
    pub fn save_due(&self, cfg: &SignConfig) -> bool {
        self.loaded
            && !self.readonly
            && self.dirty
            && self
                .saved_at
                .is_none_or(|t| self.time >= t.saturating_add(cfg.save_ticks))
    }

    /// [`Self::save_due`], and if so the record counts as saved now (the persistence glue then
    /// writes it, and retries a failed write itself).
    pub fn take_save(&mut self, cfg: &SignConfig) -> bool {
        let due = self.save_due(cfg);
        if due {
            self.saved();
        }
        due
    }

    /// Appends the record's payload to `out`.
    pub fn payload(&self, out: &mut Vec<u8>) {
        self.book.encode(out);
    }

    /// The record was written this tick.
    pub fn saved(&mut self) {
        self.dirty = false;
        self.saved_at = Some(self.time);
    }

    /// Starts the tick (no JS call): the homes, the signers, a lost signer, and what this tick's
    /// reads show.
    ///
    /// Before the record has loaded nothing is written into the book (a read or a death then would
    /// be saved over the record, or the save would cancel its load).
    pub fn begin(&mut self, snap: &Snapshot, cfg: &SignConfig) {
        self.time = snap.tick.time;
        self.events.clear();
        self.extras.clear();
        self.read_homes(snap, cfg);
        self.find_signer(snap);
        if self.loaded {
            self.observe(snap, cfg);
        }
    }

    fn read_homes(&mut self, snap: &Snapshot, cfg: &SignConfig) {
        self.homes.clear();
        if !cfg.enabled {
            return;
        }
        let rooms = &snap.rooms;
        let st = &snap.structures;
        let spawns = &snap.spawns;
        for (row, (&name, &controller)) in rooms.names().iter().zip(rooms.controllers()).enumerate()
        {
            let Controller::Mine { level } = controller else {
                continue;
            };
            if u32::from(level) < cfg.min_level {
                continue;
            }
            let spawn = spawns
                .handles()
                .iter()
                .zip(spawns.rows())
                .filter_map(|(&h, &r)| {
                    let p = *st.positions().get(usize::try_from(r).ok()?)?;
                    (p.room_name() == name).then_some((h, p))
                })
                .min_by_key(|&(h, _)| st.key_of(h).map_or(u128::MAX, |k| k.0))
                .map(|(_, p)| p);
            let Some(spawn) = spawn else {
                continue;
            };
            let capacity = rooms.energy().get(row).map_or(0, |e| e.capacity);
            self.homes.push(Home {
                room: name,
                spawn,
                capacity,
            });
        }
        self.homes.sort_by_key(|h| h.room.packed_repr());
    }

    fn find_signer(&mut self, snap: &Snapshot) {
        let creeps = &snap.creeps;
        let now = self.time;
        let mut found: Option<(CreepHandle, u64, usize)> = None;
        let mut kept = None;
        let tracked = self.signer.map(|s| s.handle);
        for (row, (id, (&mine, &h))) in creeps
            .identities()
            .iter()
            .zip(creeps.my().iter().zip(creeps.handles()))
            .enumerate()
        {
            let Some(id) = id else {
                continue;
            };
            if !mine || id.role != Role::Signer {
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
            if let Some(gone) = self.signer.take() {
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
        match self.signer.as_mut() {
            Some(s) if s.handle == handle => {
                s.at = at;
                s.ttl = ttl;
                s.spawning = spawning;
            }
            _ => {
                if let Some(gone) = self.signer.take() {
                    self.lost(gone);
                }
                // A new signer starts with the planned trip when it is the planned one.
                let trip = match &self.plan {
                    Plan::Ready { home: h, trip, .. } if *h == home => Some(*trip),
                    _ => None,
                };
                if trip.is_some() {
                    self.plan = Plan::None;
                }
                self.skip.clear();
                self.signer = Some(Signer {
                    handle,
                    home,
                    spawning,
                    at,
                    ttl,
                    trip,
                    checked: false,
                    missed: None,
                    ending: false,
                });
            }
        }
    }

    /// The tracked signer is gone: a death in its room unless it died of age (its last
    /// `ticksToLive` 1, V198), was told to end, never left the spawn, or the record has not loaded.
    fn lost(&mut self, gone: Signer) {
        self.skip.clear();
        self.idle_until = None;
        self.signing = None;
        let room = gone.at.room_name();
        let aged = gone.ttl.is_none_or(|t| t <= 1);
        if !self.loaded
            || aged
            || gone.ending
            || gone.spawning
            || self.homes.iter().any(|h| h.room == room)
        {
            return;
        }
        let now = self.time;
        if let Some(e) = self.book.entry(room, now) {
            e.deaths = e.deaths.saturating_add(1);
            // An unsigned room's retry counts from the death; any other state keeps its clock.
            if e.state == SignState::Unsigned {
                e.seen = now;
            }
            let deaths = e.deaths;
            self.events.push(SignEvent::Died { room, deaths });
            self.stats.deaths = self.stats.deaths.saturating_add(1);
            self.dirty = true;
        }
    }

    /// Updates each room read for the signer this tick (rooms that can have a controller only).
    ///
    /// A sign issued last tick that does not show now counts as replaced, like a sign found
    /// replaced: an owner whose code signs in the same tick would otherwise draw a sign every tick.
    /// An unsigned room given up on keeps its retry clock while a signer passes through.
    fn observe(&mut self, snap: &Snapshot, cfg: &SignConfig) {
        let rooms = &snap.rooms;
        let now = self.time;
        for ((&name, &controller), read) in rooms
            .names()
            .iter()
            .zip(rooms.controllers())
            .zip(rooms.sign_reads())
        {
            if !read.read || !is_remote_room(name) {
                continue;
            }
            let tile = controller_at(snap, name).map(|(_, p)| Tile::of(p));
            let Some(e) = self.book.entry(name, now) else {
                continue;
            };
            let before = e.clone();
            let was_signed = e.state == SignState::Signed;
            e.state = match (controller, read.sign) {
                (Controller::Absent, _) => SignState::NoController,
                (_, SignSeen::System) => SignState::System,
                (_, SignSeen::Ours) => SignState::Signed,
                (_, SignSeen::None | SignSeen::Other) => SignState::Unsigned,
            };
            let tried = self.signing == Some(name);
            if e.state == SignState::Signed {
                e.deaths = 0;
            } else if (was_signed || tried) && e.state == SignState::Unsigned {
                e.overwrites = e.overwrites.saturating_add(1);
            }
            e.controller = tile.or(e.controller);
            e.towers.clear();
            if matches!(controller, Controller::Foreign { .. }) {
                e.towers.extend(
                    read.tower_positions()
                        .filter(|p| p.room_name() == name)
                        .map(Tile::of)
                        .take(MAX_TOWERS),
                );
            }
            let waiting = before.state == SignState::Unsigned
                && e.state == SignState::Unsigned
                && before.given_up(cfg);
            if !waiting {
                e.seen = now;
            }
            let changed = Entry {
                seen: e.seen,
                ..before
            } != *e;
            if tried {
                self.signing = None;
                if e.state == SignState::Signed {
                    self.stats.signed = self.stats.signed.saturating_add(1);
                    self.events.push(SignEvent::Signed { room: name });
                }
            }
            if changed {
                self.dirty = true;
                self.idle_until = None;
            }
        }
    }

    /// The Critical plan (one unit): the planned signer's request, while the Optional tier runs
    /// (`optional`) and none is alive.
    pub fn request(&mut self, spawner: &mut Spawner, cfg: &SignConfig, optional: bool) {
        if !cfg.enabled || !self.loaded || !optional || self.signer.is_some() {
            return;
        }
        if let Plan::Ready { home, body, .. } = &self.plan {
            let _ = spawner.request(SpawnRequest {
                home: *home,
                role: Role::Signer,
                remote: None,
                body: BodySpec::Exact(*body),
                budget: EnergyBudget::Capacity,
                priority: PRIORITY_SIGNER,
            });
        }
    }

    /// The room whose terrain the plan needs next, if it is sizing a body.
    pub fn terrain_wanted(&self) -> Option<RoomName> {
        match &self.plan {
            Plan::Sizing { path, cursor, .. } => path.get(*cursor).map(|p| p.room_name()),
            _ => None,
        }
    }

    /// Feeds `room`'s terrain to the plan: its path tiles there are classified, and once the whole
    /// path is, the body is sized.
    pub fn feed_terrain(&mut self, room: RoomName, terrain: &TerrainGrid, cfg: &SignConfig) {
        let Plan::Sizing {
            home,
            trip,
            path,
            cursor,
            exposure,
            towers,
        } = &mut self.plan
        else {
            return;
        };
        let last = path.len().saturating_sub(1);
        while *cursor < last {
            let Some(p) = path.get(*cursor) else {
                break;
            };
            if p.room_name() != room {
                return;
            }
            if let Some(step) = step_at(home.spawn, path, *cursor, terrain) {
                let damage = if p.room_name() == trip.room {
                    damage_at(Tile::of(*p), towers)
                } else {
                    0
                };
                exposure.add(step, damage);
            }
            *cursor += 1;
        }
        let life = CREEP_LIFE_TIME.saturating_sub(cfg.life_margin_ticks);
        let body = signer_body(exposure, life, home.capacity, cfg.damage_margin_pct);
        let (home, trip) = (*home, *trip);
        self.plan = Plan::None;
        match body {
            Some(body) => {
                self.planned(home.room, body, trip);
            }
            None => self.give_up(trip.room, SignState::Deadly),
        }
    }

    fn planned(&mut self, home: RoomName, body: Body, trip: Trip) {
        let tough = u32::try_from(body.count(rustedscreep_consts::Part::Tough)).unwrap_or(0);
        let moves = u32::try_from(body.count(rustedscreep_consts::Part::Move)).unwrap_or(0);
        self.events.push(SignEvent::Planned {
            room: trip.room,
            tough,
            moves,
            cost: body.cost(),
        });
        self.stats.planned = self.stats.planned.saturating_add(1);
        self.plan = Plan::Ready {
            home,
            body,
            trip,
            at: self.time,
        };
    }

    fn give_up(&mut self, room: RoomName, state: SignState) {
        let now = self.time;
        if let Some(e) = self.book.entry(room, now) {
            e.state = state;
            e.seen = now;
        }
        self.dirty = true;
        self.events.push(SignEvent::GaveUp { room, state });
    }

    /// The Optional work (one unit): the extra signers end, the live signer works, or the next one
    /// is planned.
    #[allow(clippy::too_many_arguments)]
    pub fn work(
        &mut self,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &SignConfig,
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
        match self.signer {
            Some(s) if !s.spawning => {
                self.work_signer(s, snap, movement, buffer, cfg, mcfg, commit)
            }
            Some(_) => Ok(()),
            None => {
                self.advance_plan(snap, movement, cfg, mcfg, commit);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn work_signer(
        &mut self,
        s: Signer,
        snap: &Snapshot,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &SignConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let here = s.at;
        let room = here.room_name();
        let now = self.time;
        let on_edge = Tile::of(here).is_edge();
        let (controller, read) = room_read(snap, room);
        let mut trip = s.trip;
        // A planned trip is searched again from where the signer first stands ([`Signer::checked`]).
        if !s.checked {
            if let Some(t) = trip {
                trip = match self.reach(here, t.room, snap, movement, cfg, mcfg, commit) {
                    Reach::Found { trip: found, .. } => Some(Trip {
                        dangerous: t.dangerous || found.dangerous,
                        ..found
                    }),
                    Reach::NoRouter => Some(t),
                    Reach::Unreachable => {
                        self.pass_over(t.room);
                        None
                    }
                };
            }
            if let Some(me) = self.signer.as_mut() {
                me.checked = true;
            }
        }
        // A towered room it was not sized for: on the entrance's edge it stands, and the engine
        // carries it back (V151).
        let sized_for = trip.is_some_and(|t| t.dangerous && t.room == room);
        if matches!(controller, Controller::Foreign { .. }) && read.towers > 0 && !sized_for {
            if !self.skip.contains(&room) {
                self.skip.push(room);
                self.stats.retreats = self.stats.retreats.saturating_add(1);
                self.events.push(SignEvent::Retreated {
                    room,
                    towers: read.towers,
                });
            }
            if trip.is_some_and(|t| t.room == room) {
                trip = None;
            }
            if on_edge {
                self.set_trip(None);
                return Ok(());
            }
        }
        // Next to a controller that does not show the text, in a room that is due (not one given
        // up on, so a sign war ends with the trip): sign.
        if !s.ending
            && read.read
            && self.book.due_at(room, cfg) <= now
            && matches!(read.sign, SignSeen::None | SignSeen::Other)
            && let Some((Some(target), at)) = controller_at(snap, room)
            && Tile::of(here).range(Tile::of(at)) <= 1
            && buffer
                .push(
                    snap,
                    s.handle,
                    CreepIntent::SignController { target },
                    commit,
                )
                .is_ok()
        {
            self.signing = Some(room);
        }
        // The trip: done once its room is no longer due; a look becomes a sign once the room is
        // seen.
        if let Some(t) = trip {
            if self.book.due_at(t.room, cfg) > now {
                trip = None;
            } else if t.look && room == t.room && read.read {
                trip = controller_at(snap, room).map(|(_, at)| Trip {
                    goal: at,
                    range: 1,
                    look: false,
                    ..t
                });
            }
        }
        if trip.is_none() && !s.ending {
            match self.choose(here, s.ttl, snap, movement, cfg, mcfg, commit) {
                Choice::Trip(t) => trip = Some(t),
                Choice::Later => {}
                // A sign issued this tick is read on the next one first.
                Choice::Nothing if self.signing.is_none() => {
                    self.events.push(SignEvent::Done { room });
                    self.set_trip(None);
                    if buffer
                        .push(snap, s.handle, CreepIntent::Suicide, commit)
                        .is_ok()
                        && let Some(me) = self.signer.as_mut()
                    {
                        me.ending = true;
                    }
                    return Ok(());
                }
                Choice::Nothing => {}
            }
        }
        self.set_trip(trip);
        let Some(t) = trip else {
            // With no trip on an edge tile, step in: left there, the engine carries it across
            // (V151), back into a room it may just have stepped out of.
            if on_edge
                && !s.ending
                && let Some(c) = Tile::new(CENTER, CENTER).and_then(|c| at(room, c))
            {
                let to = MoveTo {
                    target: c,
                    range: LOOK_RANGE,
                    priority: MOVE_PRIORITY,
                    tier: Tier::Optional,
                };
                let _ = movement.request(snap, s.handle, to, mcfg, commit);
            }
            return Ok(());
        };
        let to = MoveTo {
            target: t.goal,
            range: t.range,
            priority: MOVE_PRIORITY,
            tier: Tier::Optional,
        };
        if let Err(MoveError::NoPath) = movement.request(snap, s.handle, to, mcfg, commit) {
            if room == t.room && !t.look {
                // No tile next to the controller can be reached from inside its room: walled off.
                self.give_up(t.room, SignState::Unreachable);
            } else if s.missed == Some(t.room) {
                self.pass_over(t.room);
            } else if let Some(me) = self.signer.as_mut() {
                // Movement's own route was dropped or failed: the next choice searches again with
                // the signer's limits and avoided rooms.
                me.missed = Some(t.room);
            }
            self.set_trip(None);
        }
        Ok(())
    }

    fn set_trip(&mut self, trip: Option<Trip>) {
        if let Some(s) = self.signer.as_mut() {
            s.trip = trip;
        }
    }

    /// The live signer leaves `room` out for the rest of its life.
    fn pass_over(&mut self, room: RoomName) {
        if !self.skip.contains(&room) {
            self.skip.push(room);
        }
    }

    /// The live signer's next trip from `here`: the due room nearest it that is not passed over and
    /// has no seen towers, if a route there fits its life. One search a tick, none when even the
    /// shortest walk (the Chebyshev distance in world tiles, less the range) would not fit: then
    /// nothing nearer is left. A route not found from here passes the room over for this signer
    /// only; whether a room is unreachable is the home spawn's search to say ([`Self::advance_plan`]).
    #[allow(clippy::too_many_arguments)]
    fn choose(
        &mut self,
        here: Position,
        ttl: Option<u32>,
        snap: &Snapshot,
        movement: &mut Movement,
        cfg: &SignConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Choice {
        let Some(room) = self.nearest_due(Some(here.room_name()), cfg).0 else {
            return Choice::Nothing;
        };
        let life = ttl.unwrap_or(CREEP_LIFE_TIME);
        let spare = cfg.life_margin_ticks.saturating_add(2);
        if let Some((goal, range, _)) = self.goal_for(room, snap) {
            let shortest = world_range(here, goal).saturating_sub(u32::from(range));
            if shortest.saturating_add(spare) > life {
                return Choice::Nothing;
            }
        }
        match self.reach(here, room, snap, movement, cfg, mcfg, commit) {
            Reach::Found { trip, steps } => {
                if steps.saturating_add(spare) <= life {
                    self.fresh_try(room, cfg);
                    if let Some(me) = self.signer.as_mut()
                        && me.missed != Some(room)
                    {
                        me.missed = None;
                    }
                    Choice::Trip(trip)
                } else {
                    self.pass_over(room);
                    Choice::Later
                }
            }
            Reach::Unreachable => {
                self.pass_over(room);
                Choice::Later
            }
            Reach::NoRouter => Choice::Later,
        }
    }

    /// Where a trip to `room` goes: next to its controller once its tile is known (range 1), else
    /// two tiles into the room (a look).
    fn goal_for(&self, room: RoomName, snap: &Snapshot) -> Option<(Position, u8, bool)> {
        let known = self.book.get(room).and_then(|e| e.controller);
        let seen = controller_at(snap, room).map(|(_, p)| p);
        match seen.or_else(|| known.and_then(|t| at(room, t))) {
            Some(p) => Some((p, 1, false)),
            None => Tile::new(CENTER, CENTER)
                .and_then(|t| at(room, t))
                .map(|p| (p, LOOK_RANGE, true)),
        }
    }

    /// The route search toward `room`'s controller (or into it, for a look), avoiding the rooms
    /// that kill signers but `room`.
    #[allow(clippy::too_many_arguments)]
    fn reach(
        &mut self,
        from: Position,
        room: RoomName,
        snap: &Snapshot,
        movement: &mut Movement,
        cfg: &SignConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Reach {
        let Some((goal, range, look)) = self.goal_for(room, snap) else {
            return Reach::Unreachable;
        };
        let dangerous = self.book.get(room).is_some_and(|e| !e.towers.is_empty());
        self.avoid.clear();
        // Rooms with seen towers, and rooms that killed `sign.max_deaths` signers; never the
        // target's room, nor the start's (a search could not leave it).
        let start = from.room_name();
        let deadly = |e: &Entry| !e.towers.is_empty() || u32::from(e.deaths) >= cfg.max_deaths;
        self.avoid.extend(
            self.book
                .entries()
                .iter()
                .filter(|e| deadly(e) && e.room != room && e.room != start)
                .map(|e| e.room),
        );
        let limits = (
            cfg.route_max_ops,
            u8::try_from(cfg.route_max_rooms).unwrap_or(u8::MAX),
        );
        self.stats.searches = self.stats.searches.saturating_add(1);
        let outcome = movement.search_route(
            from,
            goal,
            range,
            limits,
            &self.avoid,
            mcfg,
            commit,
            &mut self.path,
        );
        match outcome {
            None => Reach::NoRouter,
            Some(RouteOutcome::Found) => Reach::Found {
                trip: Trip {
                    room,
                    goal,
                    range,
                    look,
                    dangerous,
                },
                steps: u32::try_from(self.path.len()).unwrap_or(u32::MAX),
            },
            Some(RouteOutcome::Incomplete | RouteOutcome::Failed) => Reach::Unreachable,
        }
    }

    /// With no signer alive: plans the next one, one search a tick.
    fn advance_plan(
        &mut self,
        snap: &Snapshot,
        movement: &mut Movement,
        cfg: &SignConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) {
        let now = self.time;
        match &self.plan {
            Plan::Ready { trip, at, .. } => {
                if self.book.due_at(trip.room, cfg) > now
                    || now >= at.saturating_add(CREEP_LIFE_TIME)
                {
                    self.plan = Plan::None;
                }
                return;
            }
            Plan::Sizing { .. } => return,
            Plan::None => {}
        }
        if self.idle_until.is_some_and(|t| now < t) {
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
        match self.reach(home.spawn, room, snap, movement, cfg, mcfg, commit) {
            Reach::Found { trip, steps } => {
                if trip.dangerous {
                    self.fresh_try(room, cfg);
                    let towers = self
                        .book
                        .get(room)
                        .map(|e| e.towers.clone())
                        .unwrap_or_default();
                    self.plan = Plan::Sizing {
                        home,
                        trip,
                        path: self.path.clone(),
                        cursor: 0,
                        exposure: Exposure::default(),
                        towers,
                    };
                    return;
                }
                let need = steps
                    .saturating_add(2)
                    .saturating_add(cfg.life_margin_ticks);
                match plain_signer() {
                    Some(body) if need <= CREEP_LIFE_TIME => {
                        self.fresh_try(room, cfg);
                        self.planned(home.room, body, trip);
                    }
                    _ => self.give_up(room, SignState::Unreachable),
                }
            }
            Reach::Unreachable => self.give_up(room, SignState::Unreachable),
            Reach::NoRouter => {}
        }
    }

    /// A room given up on for its deaths or replaced signs is tried again: its counts start over, so
    /// the look on arrival does not end the trip.
    fn fresh_try(&mut self, room: RoomName, cfg: &SignConfig) {
        let now = self.time;
        if let Some(e) = self.book.entry(room, now)
            && e.given_up(cfg)
        {
            e.deaths = 0;
            e.overwrites = 0;
            self.dirty = true;
        }
    }

    /// The home nearest `room` (then the first by name).
    fn nearest_home(&self, room: RoomName) -> Option<Home> {
        self.homes
            .iter()
            .min_by_key(|h| (room_distance(h.room, room), h.room.packed_repr()))
            .copied()
    }

    /// The due candidate nearest `origin` (the live signer's room; `None` for the next signer: the
    /// nearest home), then the first by packed name. For the live signer it leaves out the rooms it
    /// passed over and the rooms with seen towers. With none, also the earliest tick one comes due.
    fn nearest_due(&self, origin: Option<RoomName>, cfg: &SignConfig) -> (Option<RoomName>, u32) {
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
                    if !is_remote_room(room) {
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
                    let entry = self.book.get(room);
                    let due = entry.map_or(0, |e| e.due_at(cfg));
                    if due > now {
                        next = next.min(due);
                        continue;
                    }
                    let distance = match origin {
                        Some(o) => {
                            if self.skip.contains(&room)
                                || entry.is_some_and(|e| !e.towers.is_empty())
                            {
                                continue;
                            }
                            room_distance(o, room)
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
}

/// How a trip choice ended this tick.
enum Choice {
    Trip(Trip),
    /// Nothing yet: a room was passed over or given up; the next tick tries the next.
    Later,
    /// No due room is left that this signer could take.
    Nothing,
}

/// A route search's answer for a trip.
enum Reach {
    Found { trip: Trip, steps: u32 },
    Unreachable,
    NoRouter,
}

/// The Chebyshev distance between two positions in world tiles: no path between them is shorter
/// (a step moves one tile on each axis at most). Unit: tiles.
pub(crate) fn world_range(a: Position, b: Position) -> u32 {
    let (a, b) = (a.world_coords(), b.world_coords());
    let d = (a.0 - b.0).abs().max((a.1 - b.1).abs());
    u32::try_from(d).unwrap_or(u32::MAX)
}

pub(crate) fn at(room: RoomName, t: Tile) -> Option<Position> {
    Some(Position::new(
        RoomCoordinate::new(t.x()).ok()?,
        RoomCoordinate::new(t.y()).ok()?,
        room,
    ))
}

/// The room's controller this tick: its object (pushed for a room an own signer stands in) and
/// position, or an owned room's controller structure's position (no object handle).
fn controller_at(snap: &Snapshot, room: RoomName) -> Option<(Option<ObjectHandle>, Position)> {
    let objects = &snap.objects;
    let object = objects
        .kinds()
        .iter()
        .zip(objects.positions())
        .zip(objects.handles())
        .find(|&((&k, &p), _)| k == ObjectKind::Controller && p.room_name() == room)
        .map(|((_, &p), &h)| (Some(h), p));
    if object.is_some() {
        return object;
    }
    let st = &snap.structures;
    st.kinds()
        .iter()
        .zip(st.positions())
        .find(|&(&k, &p)| k == StructureType::Controller && p.room_name() == room)
        .map(|(_, &p)| (None, p))
}

/// The room's controller kind and signer read this tick (none and not read when not visible).
fn room_read(snap: &Snapshot, room: RoomName) -> (Controller, SignRead) {
    let rooms = &snap.rooms;
    rooms.names().iter().position(|&n| n == room).map_or(
        (Controller::Absent, SignRead::default()),
        |row| {
            (
                rooms
                    .controllers()
                    .get(row)
                    .copied()
                    .unwrap_or(Controller::Absent),
                rooms.sign_reads().get(row).copied().unwrap_or_default(),
            )
        },
    )
}
