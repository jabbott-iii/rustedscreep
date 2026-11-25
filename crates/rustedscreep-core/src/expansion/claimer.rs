//! Claimers (ROADMAP P8.3, SPEC §3.7, ADR-042): the creep that takes the room claim scoring ranks
//! best ([`super::Claims::best`]), and the first spawn's site in a room the bot claimed. The
//! pioneers that build that spawn are the claimed room's own opening workers
//! ([`crate::economy::Opening`]), which the nearest spawn room within `spawn.help_range` spawns for
//! it (P5.1's help, [`crate::spawning::Spawner`]): the user's choice ("Helped workers", ADR-042).
//!
//! # When a claimer is ordered
//!
//! On a tick the Optional tier runs, the Critical plan orders one claimer when all of these hold:
//! - `claim.enabled` and `claim.claimers` are on.
//! - No claimer is alive, and every owned room has a spawn: one claim at a time, so a room still
//!   building its first spawn holds the next claim back.
//! - Claim scoring has a best candidate, planned ([`super::Claims::best`]), that no claim failed
//!   for within `claim.retry_ticks`.
//! - The bot owns fewer rooms than its global control level (GCL N allows N rooms, V378; the count
//!   is the shard's own rooms, V379, V380). The level (`Game.gcl.level`) is read only when every
//!   other check passes, at most once every `claim.score_ticks` ([`Claiming::gcl_due`]).
//! - The candidate's home has a spawn and a controller at `claim.min_home_level` or more, and a
//!   spawn room lies within `spawn.help_range` of the candidate, so its pioneers have a room to
//!   come from (the user's "Claim within help range").
//!
//! The order goes to the candidate's home at [`PRIORITY_CLAIMER`]: one CLAIM part and
//! [`CLAIMER_MOVES`] MOVE parts ([`claimer_body`]), so it walks a tile a tick on any terrain. Its
//! name carries the room it claims ([`Role::Claimer`] is a remote role), so a ranking that changes
//! on its way does not turn it, and a global reset does not lose its target.
//!
//! # The claimer's work (Standard)
//!
//! Out of its spawn, the claimer searches a route to its room's controller once a global, as the
//! explorer does (`scout.route_max_ops`, `scout.route_max_rooms`), keeping out of every room where
//! the atlas saw another owner's towers ([`super::RoomIntel::towered`]). A route that does not fit
//! its life (`CREEP_CLAIM_LIFE_TIME` less `scout.life_margin_ticks`), or no route, fails the claim.
//! It then walks there; next to the controller it calls `claimController` once a tick. The engine
//! checks the controller (level 0, no other user's reservation, the GCL and novice rules, V374 to
//! V376); a refused call is not billed (V108), so the claimer does not check those itself. On the
//! next tick the room is owned (V377), or the call was refused:
//! - **Claimed:** the claimer ends (`suicide`); its CLAIM part has nothing left to do.
//! - **Refused:** the room's atlas entry is marked stale ([`super::Scouting::expire`]), and the
//!   claimer waits in the room until the snapshot reads it again (at most [`REREAD_WAIT`] ticks,
//!   and not with scouting off), so the ranking sees what the claim met (an owner or a reservation
//!   that came after the last read), then ends.
//!
//! A claim fails, and the room waits `claim.retry_ticks` before another claimer goes, when there is
//! no route, the route is too long, the call is refused, or the claimer is gone before its room is
//! owned (killed, or out of life). The list is in the heap: a global reset forgets it.
//!
//! # The first spawn
//!
//! An owned room with no spawn and no own spawn site gets one spawn site at its plan's level-1 spawn
//! tile ([`super::Fit::spawn`]), in the Optional tier: from the outcome claim scoring kept, or after
//! a reset from a new plan of the room on the same facts (the planner is deterministic, so it gives
//! the same tile). Claim scoring plans such a room first ([`super::Claims::set_sites`]). A site
//! placed waits [`crate::economy::placement::APPEAR_TICKS`] to appear; a tile the game refuses is
//! not tried again this global. Once the spawn stands the room is a home: the base planner plans it
//! around that spawn (ADR-007), and its opening spawns its own workers.
//!
//! Nothing is persisted, and nothing is read from `Memory`.

use rustedscreep_consts::{
    CREEP_CLAIM_LIFE_TIME, MOVE_COST_SWAMP, MOVE_POWER, Part, Position, RoomName, StructureType,
};

use super::atlas::{Atlas, RoomIntel};
use super::claim::Claims;
use crate::config::{ClaimConfig, MovementConfig, ScoutConfig};
use crate::economy::placement::APPEAR_TICKS;
use crate::intents::{CreepIntent, IntentBuffer};
use crate::movement::{MoveError, MoveTo, Movement, RouteOutcome, Tile};
use crate::scheduler::{Commit, Fault, Tier};
use crate::signing::at;
use crate::snapshot::{Controller, CreepHandle, ObjectKind, Snapshot};
use crate::spawning::{Body, BodySpec, EnergyBudget, Role, SpawnRequest, Spawner, room_distance};

/// The claimer's spawn priority: below the reserver's 80 and the economy's roles, above the remote
/// scout's 75, the explorer's 72 and the signer's 70.
pub const PRIORITY_CLAIMER: u8 = 78;

/// The claimer's MOVE parts: one CLAIM part makes `MOVE_COST_SWAMP` fatigue a swamp tile and each
/// MOVE part takes `MOVE_POWER` away a tick (V54, V55), so with these it walks a tile a tick on any
/// terrain.
///
/// Uses: `MOVE_COST_SWAMP`, `MOVE_POWER`.
pub const CLAIMER_MOVES: u32 = MOVE_COST_SWAMP.div_ceil(MOVE_POWER);

/// After a refused claim, the most ticks the claimer waits in the room for the atlas's read of it:
/// a read comes on the first tick after the expiry that follows one running the Optional tier
/// (the snapshot reads a due room only then), so a few ticks cover a bucket dip.
pub const REREAD_WAIT: u32 = 10;

/// The claimer's move priority (movement's traffic manager): above the signer's and explorer's 1,
/// as the remote creeps' 2.
const MOVE_PRIORITY: u8 = 2;

/// The claimer's body: one CLAIM part, then [`CLAIMER_MOVES`] MOVE parts.
pub fn claimer_body() -> Option<Body> {
    let mut parts = [Part::Move; 1 + CLAIMER_MOVES as usize];
    if let Some(first) = parts.first_mut() {
        *first = Part::Claim;
    }
    Body::new(&parts)
}

/// Why a claim failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No route to the controller (another status, walled off, or the search ran out of ops).
    NoRoute,
    /// The route is longer than what is left of the claimer's life.
    TooFar,
    /// The atlas holds no controller tile for the room.
    NoController,
    /// The game refused the `claimController` call.
    Refused,
    /// The claimer was gone before its room was owned.
    Lost,
}

impl Failure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRoute => "no route",
            Self::TooFar => "route too long for its life",
            Self::NoController => "no controller tile",
            Self::Refused => "claimController refused",
            Self::Lost => "claimer lost on the way",
        }
    }
}

/// What happened this tick, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimerEvent {
    /// A claimer came out of its home's spawn order (seen spawning or alive for the first time).
    Sent { home: RoomName, room: RoomName },
    /// The claimer's room is owned now.
    Claimed { room: RoomName },
    /// A claim failed; the room waits `claim.retry_ticks`.
    Failed { room: RoomName, why: Failure },
    /// The room has no plan to place its first spawn from (the planner's reason).
    NoSite { room: RoomName, why: &'static str },
}

/// The live claimer, as the bot tracks it from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimerState {
    pub handle: CreepHandle,
    pub home: RoomName,
    /// The room it claims, from its name.
    pub target: RoomName,
    pub spawning: bool,
    /// Where it stood, and its `ticksToLive` when known, on the last tick it was seen.
    pub at: Position,
    pub ttl: Option<u32>,
    /// A route was searched for it this global.
    pub routed: bool,
    /// Its last move found no way: a second miss right after a search fails the claim.
    pub missed: bool,
    /// The tick of its last `claimController` call.
    pub called: Option<u32>,
    /// Its room was seen owned.
    pub claimed: bool,
    /// The tick its call was found refused: it waits for the atlas's read of the room since.
    pub refused: Option<u32>,
    /// It was told to end its life (`suicide`).
    pub ending: bool,
}

/// One spawn site to place this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SitePick {
    pub room: RoomName,
    pub tile: Tile,
}

impl SitePick {
    /// Its structure type.
    pub const KIND: StructureType = StructureType::Spawn;
}

/// Counters since the global started, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimerStats {
    pub claimers: u32,
    pub claims: u32,
    pub failures: u32,
    pub sites: u32,
    pub searches: u32,
}

/// An owned room this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owned {
    room: RoomName,
    level: u8,
    spawn: bool,
    spawn_site: bool,
}

/// Claimers and first spawn sites for one global (module docs).
#[derive(Default)]
pub struct Claiming {
    time: u32,
    claimer: Option<ClaimerState>,
    /// Own claimers past the first: each ends.
    extras: Vec<CreepHandle>,
    /// The owned rooms, sorted by packed name.
    owned: Vec<Owned>,
    /// Rooms whose claim failed, and the tick each may be claimed again.
    failed: Vec<(RoomName, u32)>,
    /// `Game.gcl.level` and the tick it was read.
    gcl: Option<(u32, u32)>,
    /// The rooms whose atlas entry should be marked stale this tick (a refused claim).
    expire: Vec<RoomName>,
    /// This tick's spawn sites to place.
    sites: Vec<SitePick>,
    /// Spawn sites placed, and the tick: none again in the room before it may have appeared.
    placed: Vec<(RoomName, u32)>,
    /// Rooms whose spawn tile the game refused: not tried again this global.
    refused_sites: Vec<RoomName>,
    /// Rooms whose missing plan was logged.
    no_site: Vec<RoomName>,
    events: Vec<ClaimerEvent>,
    stats: ClaimerStats,
    /// Scratch for the route search.
    avoid: Vec<RoomName>,
    path: Vec<Position>,
}

impl Claiming {
    pub fn claimer(&self) -> Option<&ClaimerState> {
        self.claimer.as_ref()
    }

    pub fn events(&self) -> &[ClaimerEvent] {
        &self.events
    }

    pub fn stats(&self) -> &ClaimerStats {
        &self.stats
    }

    /// The rooms whose claim failed, with the tick each may be claimed again.
    pub fn failed(&self) -> &[(RoomName, u32)] {
        &self.failed
    }

    /// `Game.gcl.level` as last read, if it was this global.
    pub fn gcl(&self) -> Option<u32> {
        self.gcl.map(|g| g.0)
    }

    /// The rooms whose atlas entry the glue marks stale this tick ([`super::Scouting::expire`]).
    pub fn expired(&self) -> &[RoomName] {
        &self.expire
    }

    /// This tick's spawn sites to place.
    pub fn sites(&self) -> &[SitePick] {
        &self.sites
    }

    /// The owned rooms waiting for their first spawn site: no spawn, no own spawn site, and no tile
    /// the game refused. None while the claimers are off.
    pub fn site_rooms(&self, cfg: &ClaimConfig) -> impl Iterator<Item = RoomName> + '_ {
        let on = cfg.enabled && cfg.claimers;
        self.owned
            .iter()
            .filter(move |o| on && !o.spawn && !o.spawn_site)
            .map(|o| o.room)
            .filter(|r| !self.refused_sites.contains(r))
    }

    /// Starts the tick from the snapshot: the owned rooms, their spawns and spawn sites, and the
    /// own claimers; a claim that succeeded, was refused, or lost its claimer.
    pub fn begin(&mut self, snap: &Snapshot, cfg: &ClaimConfig) {
        self.time = snap.tick.time;
        self.events.clear();
        self.extras.clear();
        self.expire.clear();
        self.sites.clear();
        let now = self.time;
        self.failed.retain(|&(_, until)| now < until);
        self.placed
            .retain(|&(_, t)| now < t.saturating_add(APPEAR_TICKS));
        self.read_owned(snap, cfg);
        self.find_claimer(snap, cfg);
        let owned = self.claimer.is_some_and(|c| self.is_owned(c.target));
        let Some(c) = self.claimer.as_mut() else {
            return;
        };
        if owned && !c.claimed {
            c.claimed = true;
            self.stats.claims = self.stats.claims.saturating_add(1);
            self.events.push(ClaimerEvent::Claimed { room: c.target });
        } else if !owned && c.refused.is_none() && c.called.is_some_and(|t| t < now) {
            // The call on an earlier tick left the room unowned: the game refused it.
            c.refused = Some(now);
            let room = c.target;
            self.expire.push(room);
            self.fail(room, Failure::Refused, cfg);
        }
    }

    /// The owned rooms, then one pass over the spawns; the spawn sites only while the claimers are
    /// on and an owned room has no spawn (only [`Self::site_rooms`] and [`Self::pick_sites`] read
    /// them, both off otherwise), with the kind checked before the room.
    fn read_owned(&mut self, snap: &Snapshot, cfg: &ClaimConfig) {
        self.owned.clear();
        let rooms = &snap.rooms;
        for (&room, &controller) in rooms.names().iter().zip(rooms.controllers()) {
            if let Controller::Mine { level } = controller {
                self.owned.push(Owned {
                    room,
                    level,
                    spawn: false,
                    spawn_site: false,
                });
            }
        }
        self.owned.sort_unstable_by_key(|o| o.room.packed_repr());
        let positions = snap.structures.positions();
        for &r in snap.spawns.rows() {
            let Some(p) = usize::try_from(r).ok().and_then(|r| positions.get(r)) else {
                continue;
            };
            if let Some(o) = self.owned_mut(p.room_name()) {
                o.spawn = true;
            }
        }
        if !(cfg.enabled && cfg.claimers) || self.owned.iter().all(|o| o.spawn) {
            return;
        }
        let objects = &snap.objects;
        for (&k, p) in objects.kinds().iter().zip(objects.positions()) {
            if k != ObjectKind::Site(StructureType::Spawn) {
                continue;
            }
            if let Some(o) = self.owned_mut(p.room_name()) {
                o.spawn_site = true;
            }
        }
    }

    fn owned_mut(&mut self, room: RoomName) -> Option<&mut Owned> {
        self.owned
            .binary_search_by_key(&room.packed_repr(), |o| o.room.packed_repr())
            .ok()
            .and_then(|i| self.owned.get_mut(i))
    }

    fn owned_room(&self, room: RoomName) -> Option<&Owned> {
        self.owned
            .binary_search_by_key(&room.packed_repr(), |o| o.room.packed_repr())
            .ok()
            .and_then(|i| self.owned.get(i))
    }

    fn is_owned(&self, room: RoomName) -> bool {
        self.owned_room(room).is_some()
    }

    fn find_claimer(&mut self, snap: &Snapshot, cfg: &ClaimConfig) {
        let creeps = &snap.creeps;
        let now = self.time;
        let tracked = self.claimer.map(|c| c.handle);
        let mut kept = None;
        let mut found: Option<(CreepHandle, u64, usize)> = None;
        for (row, (id, (&mine, &h))) in creeps
            .identities()
            .iter()
            .zip(creeps.my().iter().zip(creeps.handles()))
            .enumerate()
        {
            let Some(id) = id.filter(|id| id.role == Role::Claimer && id.remote.is_some()) else {
                continue;
            };
            if !mine {
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
            if let Some(gone) = self.claimer.take() {
                self.lost(gone, cfg);
            }
            return;
        };
        let id = creeps.identities().get(row).copied().flatten();
        let at = creeps.positions().get(row).copied();
        let (Some(id), Some(at)) = (id, at) else {
            return;
        };
        let Some(target) = id.remote else {
            return;
        };
        let spawning = creeps.spawning().get(row).copied().unwrap_or(false);
        let ttl = creeps.ticks_to_live(row, now);
        match self.claimer.as_mut() {
            Some(c) if c.handle == handle => {
                c.at = at;
                c.ttl = ttl;
                c.spawning = spawning;
            }
            _ => {
                if let Some(gone) = self.claimer.take() {
                    self.lost(gone, cfg);
                }
                self.stats.claimers = self.stats.claimers.saturating_add(1);
                self.events.push(ClaimerEvent::Sent {
                    home: id.home,
                    room: target,
                });
                self.claimer = Some(ClaimerState {
                    handle,
                    home: id.home,
                    target,
                    spawning,
                    at,
                    ttl,
                    routed: false,
                    missed: false,
                    called: None,
                    claimed: false,
                    refused: None,
                    ending: false,
                });
            }
        }
    }

    /// The tracked claimer is gone: a lost claim unless its room is owned, it was told to end, or
    /// it never left the spawn.
    fn lost(&mut self, gone: ClaimerState, cfg: &ClaimConfig) {
        // A refused claim failed already.
        if gone.claimed
            || gone.ending
            || gone.spawning
            || gone.refused.is_some()
            || self.is_owned(gone.target)
        {
            return;
        }
        self.fail(gone.target, Failure::Lost, cfg);
    }

    /// The candidate a claimer would be ordered for, checking all but the GCL (module docs): its
    /// home and room.
    fn candidate(
        &self,
        claims: &Claims,
        cfg: &ClaimConfig,
        help_range: u32,
    ) -> Option<(RoomName, RoomName)> {
        if !(cfg.enabled && cfg.claimers)
            || self.claimer.is_some()
            || !self.extras.is_empty()
            || self.owned.iter().any(|o| !o.spawn)
        {
            return None;
        }
        let best = claims.best()?;
        if self.failed.iter().any(|f| f.0 == best.room) || self.is_owned(best.room) {
            return None;
        }
        let home = self.owned_room(best.home)?;
        if !home.spawn || u32::from(home.level) < cfg.min_home_level {
            return None;
        }
        let served = self
            .owned
            .iter()
            .any(|o| o.spawn && room_distance(o.room, best.room) <= help_range);
        served.then_some((best.home, best.room))
    }

    /// Whether the glue should read `Game.gcl.level` now: a claimer would be ordered but for the
    /// GCL, and the level was not read this global or `claim.score_ticks` ago or more.
    pub fn gcl_due(&self, claims: &Claims, cfg: &ClaimConfig, help_range: u32) -> bool {
        let now = self.time;
        self.candidate(claims, cfg, help_range).is_some()
            && self
                .gcl
                .is_none_or(|(_, at)| now >= at.saturating_add(cfg.score_ticks))
    }

    /// Takes `Game.gcl.level`, read this tick.
    pub fn set_gcl(&mut self, level: u32) {
        self.gcl = Some((level, self.time));
    }

    /// Whether the GCL allows one more room: the bot owns fewer rooms than its level (V378).
    fn gcl_allows(&self) -> bool {
        let owned = u32::try_from(self.owned.len()).unwrap_or(u32::MAX);
        self.gcl.is_some_and(|(level, _)| owned < level)
    }

    /// Whether the Critical plan has a request to make this tick.
    pub fn wants_request(&self, claims: &Claims, cfg: &ClaimConfig, help_range: u32) -> bool {
        self.gcl_allows() && self.candidate(claims, cfg, help_range).is_some()
    }

    /// The Critical plan (one unit): the claimer's request from the best candidate's home, while
    /// the Optional tier runs (`optional`) and the module docs' checks pass.
    pub fn request(
        &mut self,
        spawner: &mut Spawner,
        claims: &Claims,
        cfg: &ClaimConfig,
        help_range: u32,
        optional: bool,
    ) {
        if !optional || !self.gcl_allows() {
            return;
        }
        let Some((home, room)) = self.candidate(claims, cfg, help_range) else {
            return;
        };
        let Some(body) = claimer_body() else {
            return;
        };
        let _ = spawner.request(SpawnRequest {
            home,
            role: Role::Claimer,
            remote: Some(room),
            body: BodySpec::Exact(body),
            budget: EnergyBudget::Capacity,
            priority: PRIORITY_CLAIMER,
        });
    }

    /// Whether the Standard work has a claimer to work or end. With none, its job runs no unit.
    pub fn has_work(&self) -> bool {
        !self.extras.is_empty() || self.claimer.is_some_and(|c| !c.spawning && !c.ending)
    }

    /// The Standard work (one unit): the extra claimers end, and the live one walks, claims, or
    /// ends (module docs).
    #[allow(clippy::too_many_arguments)]
    pub fn work(
        &mut self,
        snap: &Snapshot,
        atlas: &Atlas,
        movement: &mut Movement,
        buffer: &mut IntentBuffer,
        cfg: &ClaimConfig,
        scout: &ScoutConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        for i in 0..self.extras.len() {
            if let Some(&h) = self.extras.get(i) {
                let _ = buffer.push(snap, h, CreepIntent::Suicide, commit);
            }
        }
        let Some(c) = self.claimer else {
            return Ok(());
        };
        if c.spawning || c.ending {
            return Ok(());
        }
        let entry = atlas.get(c.target);
        let now = self.time;
        // After a refusal it waits for the atlas's read, at most REREAD_WAIT ticks and only while
        // scouting runs (none comes otherwise).
        let done = !(cfg.enabled && cfg.claimers)
            || c.claimed
            || c.refused.is_some_and(|since| {
                !scout.enabled
                    || now >= since.saturating_add(REREAD_WAIT)
                    || entry.is_none_or(|e| e.seen >= since)
            });
        if done {
            self.end(snap, c.handle, buffer, commit);
            return Ok(());
        }
        if c.refused.is_some() {
            // Waiting in the room for the atlas's read.
            return Ok(());
        }
        let Some(goal) = entry
            .and_then(|e| e.controller_tile)
            .and_then(|t| at(c.target, t))
        else {
            self.fail(c.target, Failure::NoController, cfg);
            self.end(snap, c.handle, buffer, commit);
            return Ok(());
        };
        let here = c.at;
        let tile = Tile::of(goal);
        if here.room_name() == c.target && Tile::of(here).range(tile) <= 1 {
            if let Some(controller) = controller_object(snap, c.target)
                && c.called != Some(now)
                && buffer
                    .push(
                        snap,
                        c.handle,
                        CreepIntent::ClaimController { target: controller },
                        commit,
                    )
                    .is_ok()
                && let Some(me) = self.claimer.as_mut()
            {
                me.called = Some(now);
            }
            return Ok(());
        }
        if !c.routed {
            match self.search(c, goal, atlas, movement, scout, mcfg, commit) {
                Some(Ok(())) => {}
                Some(Err(why)) => {
                    self.fail(c.target, why, cfg);
                    self.end(snap, c.handle, buffer, commit);
                    return Ok(());
                }
                // No router installed: wait.
                None => return Ok(()),
            }
        }
        let to = MoveTo {
            target: goal,
            range: 1,
            priority: MOVE_PRIORITY,
            tier: Tier::Standard,
        };
        match movement.request(snap, c.handle, to, mcfg, commit) {
            Err(MoveError::NoPath) => {
                let missed = self.claimer.is_some_and(|me| me.missed);
                if missed {
                    // A second miss with a search between: no way there.
                    self.fail(c.target, Failure::NoRoute, cfg);
                    self.end(snap, c.handle, buffer, commit);
                } else if let Some(me) = self.claimer.as_mut() {
                    // Movement's route was dropped or failed: search again next tick.
                    me.missed = true;
                    me.routed = false;
                }
            }
            Ok(_) => {
                if let Some(me) = self.claimer.as_mut() {
                    me.missed = false;
                }
            }
            Err(_) => {}
        }
        Ok(())
    }

    /// The claimer's route search (module docs): `Some(Ok)` when a route fits its life, `Some(Err)`
    /// when the claim fails, `None` without a router.
    #[allow(clippy::too_many_arguments)]
    fn search(
        &mut self,
        c: ClaimerState,
        goal: Position,
        atlas: &Atlas,
        movement: &mut Movement,
        scout: &ScoutConfig,
        mcfg: &MovementConfig,
        commit: &mut Commit,
    ) -> Option<Result<(), Failure>> {
        if !movement.has_router() {
            return None;
        }
        let start = c.at.room_name();
        self.avoid.clear();
        self.avoid.extend(
            atlas
                .entries()
                .iter()
                .filter(|e| e.towered() && e.room != c.target && e.room != start)
                .map(|e: &RoomIntel| e.room),
        );
        let limits = (
            scout.route_max_ops,
            u8::try_from(scout.route_max_rooms).unwrap_or(u8::MAX),
        );
        self.stats.searches = self.stats.searches.saturating_add(1);
        let outcome = movement.search_route(
            c.at,
            goal,
            1,
            limits,
            &self.avoid,
            mcfg,
            commit,
            &mut self.path,
        )?;
        Some(match outcome {
            RouteOutcome::Found => {
                let steps = u32::try_from(self.path.len()).unwrap_or(u32::MAX);
                let life = c.ttl.unwrap_or(CREEP_CLAIM_LIFE_TIME);
                if steps.saturating_add(scout.life_margin_ticks) <= life {
                    // A miss is cleared only by a move that finds its way (`work`), so a route
                    // found and then not walked fails on the second miss.
                    if let Some(me) = self.claimer.as_mut() {
                        me.routed = true;
                    }
                    Ok(())
                } else {
                    Err(Failure::TooFar)
                }
            }
            RouteOutcome::Failed | RouteOutcome::Incomplete => Err(Failure::NoRoute),
        })
    }

    /// Ends the claimer's life (`suicide`).
    fn end(
        &mut self,
        snap: &Snapshot,
        handle: CreepHandle,
        buffer: &mut IntentBuffer,
        commit: &mut Commit,
    ) {
        if buffer
            .push(snap, handle, CreepIntent::Suicide, commit)
            .is_ok()
            && let Some(me) = self.claimer.as_mut()
        {
            me.ending = true;
        }
    }

    /// The Optional pick of this tick's spawn sites (module docs), from claim scoring's outcomes.
    pub fn pick_sites(&mut self, claims: &Claims, cfg: &ClaimConfig) {
        self.sites.clear();
        for i in 0..self.owned.len() {
            let Some(o) = self.owned.get(i).copied() else {
                continue;
            };
            if !(cfg.enabled && cfg.claimers)
                || o.spawn
                || o.spawn_site
                || self.refused_sites.contains(&o.room)
                || self.placed.iter().any(|p| p.0 == o.room)
            {
                continue;
            }
            match claims.spawn_tile(o.room) {
                Some(Ok(tile)) => self.sites.push(SitePick { room: o.room, tile }),
                Some(Err(why)) if !self.no_site.contains(&o.room) => {
                    self.no_site.push(o.room);
                    self.events.push(ClaimerEvent::NoSite { room: o.room, why });
                }
                Some(Err(_)) | None => {}
            }
        }
    }

    /// The glue placed `pick`'s site (the call returned OK).
    pub fn site_placed(&mut self, pick: SitePick) {
        self.placed.push((pick.room, self.time));
        self.stats.sites = self.stats.sites.saturating_add(1);
    }

    /// The game refused `pick`'s site; unless it was only the site cap (`full`), its room is not
    /// tried again this global.
    pub fn site_refused(&mut self, pick: SitePick, full: bool) {
        if full {
            self.placed.push((pick.room, self.time));
        } else if !self.refused_sites.contains(&pick.room) {
            self.refused_sites.push(pick.room);
        }
    }

    /// Fails the claim of `room`: it waits `claim.retry_ticks`.
    fn fail(&mut self, room: RoomName, why: Failure, cfg: &ClaimConfig) {
        let until = self.time.saturating_add(cfg.retry_ticks);
        match self.failed.iter_mut().find(|f| f.0 == room) {
            Some(f) => f.1 = f.1.max(until),
            None => self.failed.push((room, until)),
        }
        self.stats.failures = self.stats.failures.saturating_add(1);
        self.events.push(ClaimerEvent::Failed { room, why });
    }
}

/// The controller of `room` among the snapshot's room objects (the snapshot pushes it for a room an
/// own claimer stands in).
fn controller_object(snap: &Snapshot, room: RoomName) -> Option<crate::snapshot::ObjectHandle> {
    let objects = &snap.objects;
    objects
        .kinds()
        .iter()
        .zip(objects.positions())
        .zip(objects.handles())
        .find(|&((&k, &p), _)| k == ObjectKind::Controller && p.room_name() == room)
        .map(|(_, &h)| h)
}
