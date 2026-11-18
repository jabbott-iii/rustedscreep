//! Interim extension placement (ADR-020), source containers (ADR-023) and links (ADR-024); the P4
//! base planner keeps or replaces what it builds: while an owned room with a spawn has fewer extensions and extension
//! sites than its level allows (`CONTROLLER_STRUCTURES`), the Optional job picks one tile per tick
//! and the act step calls `Room.createConstructionSite` on it. The opening's workers build the
//! sites.
//!
//! Containers go first: the mining tiles `economy::mining` lists as needing one
//! (`Mining::container_needs`: the room pays for the source's miner and the tile has no container
//! and no container site), while the room has fewer containers and container sites than its level
//! allows. They take the same one call per room per tick and the same refusal rules below.
//!
//! Links go next (ADR-024): the tiles `economy::mining` lists (`Mining::link_needs`: the controller
//! link, then the source links, farthest source first), while the room has fewer links and link
//! sites than its level allows. The game side does not count the tick's earlier link sites either,
//! and the processor drops a site past the level's count silently (V219), so the one call per tick
//! keeps the count right.
//!
//! The pattern is a checkerboard around the room's first spawn (by id): extensions take the tiles of
//! the spawn's parity (`(x + y) % 2`) at range 2 or more, nearest by path first. Diagonal steps keep a
//! tile's parity, so the tiles of the other parity, which the pattern never takes, stay connected to
//! each other; a tile is taken only when its free neighbours stay connected without it (it is a
//! simple tile), so narrow passages are never closed, and only when a free orthogonal neighbour can
//! reach the spawn, so a worker can fill it. Tiles next to a source or a mineral (where harvesters
//! stand), within the upgrade range of the controller, and within [`EDGE_MARGIN`] of the room's edge
//! (clear of the engine's edge and exit rules, V205) are never taken.
//!
//! One call per room per tick: the game side does not count the tick's earlier sites against the
//! level's limit and the processor drops the excess silently (V204), and a site placed on tick T
//! appears in the snapshot of T + 1 (V204, inferred), so the next pick counts it. A tile the game
//! refuses is skipped for the rest of the global; a pick not seen on its tile within
//! [`APPEAR_TICKS`], or refused for the whole room, counts toward [`LOST_MAX`]. A pick that finds no
//! tile is not tried again until the grid, the count, the level or the refused tiles change.
//!
//! The plan path (P4.3, ADR-026, [`Placement::plan_room`] with [`PlanView::Ready`]) keeps the one
//! call per room per tick and the pending check, with three differences: a road (or wall) site,
//! which once built has no owner and leaves the snapshot, also counts as done when movement's scan
//! shows it built, and past [`APPEAR_TICKS`] waits for a scan from the tick it was found gone
//! before it counts as lost; a site refused with `ERR_RCL_NOT_ENOUGH` holds its type for a while
//! instead of counting toward [`LOST_MAX`]; and a destroy's loss counts toward the room's own
//! destroy limit.

use rustedscreep_consts::{
    Direction, INTENT_CPU_COST, ROOM_AREA, ROOM_SIZE, RoomName, StructureType,
};

use rustedscreep_consts::ErrorCode;

use super::{ContainerNeed, LinkNeed};
use crate::config::{MovementConfig, PlannerConfig};
use crate::intents::Outcome;
use crate::movement::{
    CostGrid, CostProfile, Dial, FlowField, Footprint, Goal, Movement, Tile, UNREACHED,
};
use crate::planner::build::{self, Ask, Gates, Ground, Need, Next, TileState};
use crate::planner::job::Plan;
use crate::planner::layout::{Gate, Item, kind_rank};
use crate::planner::rules::allowed;
use crate::scheduler::{Commit, Fault};
use crate::snapshot::{Controller, ObjectKind, Snapshot};

/// Tiles from the room's edge that stay free: an extension needs x and y in
/// `EDGE_MARGIN..ROOM_SIZE - EDGE_MARGIN`. The engine refuses sites on the edge and, beside an exit,
/// at x or y of 1 or 48 (V205); 3 keeps clear of both and of the exits' traffic.
pub const EDGE_MARGIN: u8 = 3;

/// The least range from the spawn: its ring of neighbours stays free, so new creeps can step out
/// and fillers can reach it.
pub const SPAWN_CLEAR: u8 = 2;

/// The range of the controller that stays free: `upgradeController`'s range (`inRangeTo(target,
/// 3)`, V187), so upgraders keep their tiles.
pub const CONTROLLER_CLEAR: u8 = 3;

/// Ticks a pick may take to appear in the snapshot (a site created on tick T is seen on T + 1,
/// V204, inferred; the margin covers a lost tick) before it counts as refused.
pub const APPEAR_TICKS: u32 = 10;

/// Picks in a row that came to nothing for a reason that is not the tile: accepted but never seen
/// (the processor's silent checks, V204), or refused for the room as a whole (`ERR_RCL_NOT_ENOUGH`
/// when other players' extensions count toward the level, which the plan path holds by type
/// instead, `ERR_FULL` at `MAX_CONSTRUCTION_SITES`, `ERR_NOT_OWNER`, a throw). After this many the room gets no more calls for the rest of the
/// global: each accepted one is billed (V203), and the pattern cannot see the cause. A global reset
/// tries again.
pub const LOST_MAX: u32 = 3;

/// One `createConstructionSite(x, y, type)` call for the act step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteOrder {
    pub room: RoomName,
    /// The room's row in this tick's snapshot (its JS object for the call).
    pub room_row: usize,
    pub tile: Tile,
    pub kind: StructureType,
}

/// One `Structure.destroy()` call for the act step (ADR-026): an own structure off the room's plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestroyOrder {
    pub room: RoomName,
    pub tile: Tile,
    pub kind: StructureType,
    /// Its row in this tick's snapshot: in the own structures, or (a container) in the objects.
    pub target: DestroyTarget,
}

/// Where a destroy's structure is in this tick's snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestroyTarget {
    Structure(usize),
    Object(usize),
}

/// A room's plan as the placement sees it ([`Placement::plan_room`]).
#[derive(Clone, Copy, Debug)]
pub enum PlanView<'a> {
    /// The planner is off: the interim placement (ADR-020, ADR-023 to ADR-025).
    Off,
    /// The room's plan is being built: nothing is placed meanwhile.
    Waiting,
    /// The room has no valid plan: the interim placement.
    Fallback,
    Ready(&'a Plan),
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    tile: Tile,
    at: u32,
    kind: StructureType,
    /// A destroy: done once `kind` no longer stands on `tile`.
    destroy: bool,
    /// A site from the room's plan (P4.3): a refusal for the level's count holds its type
    /// ([`RoomPlace::full`]) instead of counting toward [`LOST_MAX`].
    planned: bool,
    /// For a road or a wall (no owner, so it leaves the snapshot once built): the tick its site was
    /// first found gone after [`APPEAR_TICKS`]. A road built from it stands by then, so a scan
    /// from that tick on judges it.
    missing_at: Option<u32>,
}

/// A destroyed structure's replacement still open (ADR-026: one at a time per room): no other
/// destroy until the plan's `kind` is sited or stands on `tile`, or `PlannerConfig::verify_ticks`
/// pass.
#[derive(Clone, Copy, Debug)]
struct Replacing {
    tile: Tile,
    kind: StructureType,
    since: u32,
}

/// What a room's last pick that found no tile was made from: the next pick waits until it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IdleKey {
    version: u32,
    have: u32,
    level: u8,
    refused: usize,
    occupied: usize,
}

#[derive(Debug)]
struct RoomPlace {
    name: RoomName,
    row: Option<usize>,
    refused: Vec<Tile>,
    pending: Option<Pending>,
    /// Picks lost in a row ([`LOST_MAX`]).
    lost: u32,
    idle: Option<IdleKey>,
    /// The plan path's watch (P4.3): what its last idle look was made from, and when.
    plan_idle: Option<(PlanKey, u32)>,
    replacing: Option<Replacing>,
    /// The last look's census, for the console.
    census: build::Census,
    /// The room's own structures, sites and containers when last counted, and the tick something was
    /// built, destroyed or removed (a structure or container count changed, or a site went): the
    /// plan path looks only at a movement scan from that tick on, since roads (no owner) and the
    /// tiles of structures gone show only in movement's scan. A new site needs no scan (the
    /// snapshot has it).
    /// `None` until the room's first look, which is no change.
    counted: Option<(usize, usize)>,
    /// The room's own site tiles when last looked at, sorted.
    site_tiles: Vec<Tile>,
    changed_at: u32,
    /// The tick a destroy came back `ERR_BUSY` (a hostile creep or power creep in the room, V248,
    /// which the snapshot may not see): no destroy for `PlannerConfig::verify_ticks` after it.
    busy_at: Option<u32>,
    /// Destroys lost in a row (refused, or accepted and never done, V251). At [`LOST_MAX`] the room
    /// gets no more destroys for the rest of the global; its sites go on.
    destroy_lost: u32,
    /// The room's level at its last pick.
    level: u8,
    /// The plan path's types the game refused a site of for the level's count
    /// (`ERR_RCL_NOT_ENOUGH`: structures of another owner count too, V238, and the snapshot holds
    /// only own ones), one bit per `kind_rank`: none is offered again until the level changes or
    /// `PlannerConfig::verify_ticks` pass since `full_at`.
    full: u32,
    full_at: u32,
    full_level: u8,
}

/// What a room's plan-path look found nothing to do from: the next look waits until it changes or
/// `PlannerConfig::verify_ticks` pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlanKey {
    version: u32,
    level: u8,
    structures: usize,
    /// Own sites and containers.
    objects: usize,
    hostile: bool,
    refused: usize,
    needs: usize,
    replacing: bool,
    full: u32,
    /// The config the look was made under (a console change looks again at once).
    gates: Gates,
    replace: bool,
    max_sites: u32,
}

/// Counters since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlacementTotals {
    pub picked: u32,
    /// Of `picked`, container sites (ADR-023).
    pub containers: u32,
    /// Of `picked`, link sites (ADR-024).
    pub links: u32,
    pub placed: u32,
    pub refused: u32,
    /// Picks that never appeared within [`APPEAR_TICKS`].
    pub lost: u32,
    /// Of `picked`, sites placed from a room's plan (P4.3).
    pub planned: u32,
    /// Destroys of off-plan structures called (ADR-026), and of those the game accepted.
    pub destroys: u32,
    pub destroyed: u32,
    /// Accepted destroys whose structure still stood after [`APPEAR_TICKS`] (the processor's
    /// silent checks, V251), kept apart from the sites' `lost`.
    pub destroys_dropped: u32,
    /// Plan-path sites refused for the level's count (`ERR_RCL_NOT_ENOUGH`, V238), which hold
    /// their type for a while instead of counting toward [`LOST_MAX`].
    pub full: u32,
    /// Heavy looks at a room's plan (a `Ground` rebuilt and `build::next` run): a quiet room makes
    /// none until what it watches changes or `PlannerConfig::verify_ticks` pass.
    pub looks: u32,
}

/// The placement's state for one global.
#[derive(Default)]
pub struct Placement {
    time: u32,
    rooms: Vec<RoomPlace>,
    orders: Vec<SiteOrder>,
    destroys: Vec<DestroyOrder>,
    totals: PlacementTotals,
    ground: Ground,
    reach: build::Scratch,
    tiles: Vec<Tile>,
    field: FlowField,
    dial: Dial,
    occupied: Vec<(Tile, bool)>,
    statics: Vec<Tile>,
}

impl Placement {
    /// Starts a tick: the owned rooms, by name, and no orders.
    pub fn begin(&mut self, snap: &Snapshot) {
        self.time = snap.tick.time;
        self.orders.clear();
        self.destroys.clear();
        for r in &mut self.rooms {
            r.row = None;
        }
        let rooms = &snap.rooms;
        for row in rooms.owned() {
            let Some(&name) = rooms.names().get(row) else {
                continue;
            };
            match self
                .rooms
                .binary_search_by_key(&name.packed_repr(), |r| r.name.packed_repr())
            {
                Ok(i) => {
                    if let Some(r) = self.rooms.get_mut(i) {
                        r.row = Some(row);
                    }
                }
                Err(i) => self.rooms.insert(
                    i,
                    RoomPlace {
                        name,
                        row: Some(row),
                        refused: Vec::new(),
                        pending: None,
                        lost: 0,
                        idle: None,
                        plan_idle: None,
                        replacing: None,
                        census: build::Census::default(),
                        counted: None,
                        site_tiles: Vec::new(),
                        changed_at: 0,
                        busy_at: None,
                        destroy_lost: 0,
                        level: 0,
                        full: 0,
                        full_at: 0,
                        full_level: 0,
                    },
                ),
            }
        }
        self.rooms.retain(|r| r.row.is_some());
    }

    pub fn room_count(&self) -> u32 {
        u32::try_from(self.rooms.len()).unwrap_or(u32::MAX)
    }

    /// The name of room `unit`.
    pub fn room_name(&self, unit: u32) -> Option<RoomName> {
        self.rooms.get(usize::try_from(unit).ok()?).map(|r| r.name)
    }

    /// Room `unit`'s pick for this tick, if it needs one (module docs). With the room's plan ready
    /// (P4.3, [`PlanView::Ready`]) it comes from the plan ([`Self::by_plan`]); while the plan is
    /// being built nothing is placed; otherwise the interim placement: a container site on the
    /// first of `containers` (the room's mining tiles that need one) not refused yet, else a link
    /// site on the first of `links` not refused yet, else, with `extensions`, an extension site. An
    /// extension pick and a look at the plan are heavy units (a flow field from the spawn; a pass
    /// over the room and the plan); each order commits one intent (`INTENT_CPU_COST`: each accepted
    /// call is billed, V203, V250).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        mcfg: &MovementConfig,
        extensions: bool,
        containers: &[ContainerNeed],
        links: &[LinkNeed],
        plan: PlanView<'_>,
        pcfg: &PlannerConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let now = self.time;
        let ri = usize::try_from(unit).map_err(|_| Fault::new("unit out of range", unit))?;
        let r = self
            .rooms
            .get_mut(ri)
            .ok_or(Fault::new("no room for the unit", unit))?;
        let name = r.name;
        let row = r.row.ok_or(Fault::new("room not in this tick", unit))?;
        let level = match snap.rooms.controllers().get(row) {
            Some(&Controller::Mine { level }) => level,
            _ => return Ok(()),
        };
        if matches!(plan, PlanView::Waiting) {
            return Ok(());
        }
        r.level = level;
        if let Some(p) = r.pending {
            // Judged by its own tile, so a structure lost elsewhere does not hide it. A road or a
            // wall has no owner: once built it shows only in movement's scan.
            let done = if p.destroy {
                !stands_at(snap, name, p.tile, p.kind)
            } else {
                placed_at(snap, name, p.tile, p.kind) || scanned_at(movement, name, p.tile, p.kind)
            };
            let age = now.wrapping_sub(p.at);
            if done {
                r.pending = None;
                if p.destroy {
                    r.destroy_lost = 0;
                } else {
                    r.lost = 0;
                }
                r.plan_idle = None;
                if p.destroy {
                    // The tile's footprint changed: rescan it before the plan looks again (a scan
                    // this tick already shows it).
                    if movement.room_scanned(name) != Some(now) {
                        movement.invalidate(name);
                    }
                    // Held open only for a replacement the bot will place now: one whose gate is on
                    // and that is due at the level (a tile planned for a switched-off type, or for
                    // a road only those need, holds nothing).
                    if let PlanView::Ready(plan) = plan
                        && let Some(kind) = due_replacement(
                            plan,
                            p.tile,
                            Gates::from_config(pcfg),
                            level,
                            containers,
                            links,
                        )
                    {
                        r.replacing = Some(Replacing {
                            tile: p.tile,
                            kind,
                            since: now,
                        });
                    }
                }
            } else if age < APPEAR_TICKS
                || (!p.destroy && unowned(p.kind) && unseen_wait(r, movement, mcfg, name, now))
            {
                // Not judged yet: in time to appear, or a road or wall waiting for a scan.
                return Ok(());
            } else {
                // A site never seen (V204), or a structure still standing after its destroy (the
                // processor's silent checks, V251): a lost destroy stops only destroys.
                r.pending = None;
                if p.destroy {
                    r.destroy_lost = r.destroy_lost.saturating_add(1);
                    self.totals.destroys_dropped = self.totals.destroys_dropped.saturating_add(1);
                } else {
                    r.refused.push(p.tile);
                    r.lost = r.lost.saturating_add(1);
                    self.totals.lost = self.totals.lost.saturating_add(1);
                }
            }
        }
        if self.rooms.get(ri).is_some_and(|r| r.lost >= LOST_MAX) {
            return Ok(());
        }
        match plan {
            PlanView::Ready(plan) => self.by_plan(
                ri, row, name, level, snap, movement, containers, links, plan, pcfg, commit,
            ),
            _ => self.interim(
                ri, row, name, level, snap, movement, mcfg, extensions, containers, links, commit,
            ),
        }
    }

    /// The interim placement (ADR-020, ADR-023 to ADR-025; module docs).
    #[allow(clippy::too_many_arguments)]
    fn interim(
        &mut self,
        ri: usize,
        row: usize,
        name: RoomName,
        level: u8,
        snap: &Snapshot,
        movement: &mut Movement,
        mcfg: &MovementConfig,
        extensions: bool,
        containers: &[ContainerNeed],
        links: &[LinkNeed],
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let now = self.time;
        let Some(r) = self.rooms.get_mut(ri) else {
            return Ok(());
        };
        let allowed = StructureType::Extension.controller_structures(u32::from(level));
        let have = extensions_in(snap, name);
        // A container for a mining tile first (ADR-023): no pattern, no flow field.
        let container = StructureType::Container;
        if let Some(tile) = containers
            .iter()
            .map(|c| c.tile)
            .find(|t| !r.refused.contains(t))
            && containers_in(snap, name) < container.controller_structures(u32::from(level))
        {
            self.orders.push(SiteOrder {
                room: name,
                room_row: row,
                tile,
                kind: container,
            });
            commit.add(INTENT_CPU_COST);
            self.totals.picked = self.totals.picked.saturating_add(1);
            self.totals.containers = self.totals.containers.saturating_add(1);
            r.pending = Some(Pending {
                tile,
                at: now,
                kind: container,
                destroy: false,
                planned: false,
                missing_at: None,
            });
            return Ok(());
        }
        // Then a link (ADR-024): no pattern, no flow field.
        let link = StructureType::Link;
        if let Some(tile) = links
            .iter()
            .map(|l| l.tile)
            .find(|t| !r.refused.contains(t))
            && links_in(snap, name) < link.controller_structures(u32::from(level))
        {
            self.orders.push(SiteOrder {
                room: name,
                room_row: row,
                tile,
                kind: link,
            });
            commit.add(INTENT_CPU_COST);
            self.totals.picked = self.totals.picked.saturating_add(1);
            self.totals.links = self.totals.links.saturating_add(1);
            r.pending = Some(Pending {
                tile,
                at: now,
                kind: link,
                destroy: false,
                planned: false,
                missing_at: None,
            });
            return Ok(());
        }
        if !extensions || have >= allowed {
            return Ok(());
        }
        // The first spawn by id anchors the pattern; the controller keeps its upgrade range.
        let st = &snap.structures;
        let mut spawn: Option<(u128, Tile)> = None;
        let mut controller = None;
        self.occupied.clear();
        for ((&h, &kind), &pos) in st.handles().iter().zip(st.kinds()).zip(st.positions()) {
            if pos.room_name() != name {
                continue;
            }
            let t = Tile::of(pos);
            self.occupied.push((t, kind.is_obstacle()));
            match kind {
                StructureType::Spawn => {
                    let key = st.key_of(h).map_or(u128::MAX, |k| k.0);
                    if spawn.is_none_or(|(k, _)| key < k) {
                        spawn = Some((key, t));
                    }
                }
                StructureType::Controller => controller = Some(t),
                _ => {}
            }
        }
        let Some((_, spawn)) = spawn else {
            return Ok(());
        };
        let ob = &snap.objects;
        for (&kind, &pos) in ob.kinds().iter().zip(ob.positions()) {
            if pos.room_name() != name {
                continue;
            }
            let blocks = match kind {
                ObjectKind::Site(t) => t.is_obstacle(),
                ObjectKind::Source => true,
                ObjectKind::Container => false,
                // Piles, tombstones and ruins do not stand in a site's way.
                _ => continue,
            };
            self.occupied.push((Tile::of(pos), blocks));
        }
        self.statics.clear();
        self.statics.extend(movement.room_statics(name));
        let profile = crate::movement::profile(mcfg);
        let Some((grid, version)) = movement.room_grid(name, mcfg, commit) else {
            return Ok(());
        };
        let key = IdleKey {
            version,
            have,
            level,
            refused: self.rooms.get(ri).map_or(0, |r| r.refused.len()),
            occupied: self.occupied.len(),
        };
        if self.rooms.get(ri).is_some_and(|r| r.idle == Some(key)) {
            return Ok(());
        }
        commit.heavy();
        let Self {
            rooms,
            orders,
            totals,
            field,
            dial,
            occupied,
            statics,
            ..
        } = self;
        let refused = rooms.get(ri).map_or(&[][..], |r| r.refused.as_slice());
        let pick = next_extension(
            grid, profile, spawn, controller, statics, occupied, refused, field, dial,
        );
        if let Some(tile) = pick {
            orders.push(SiteOrder {
                room: name,
                room_row: row,
                tile,
                kind: StructureType::Extension,
            });
            commit.add(INTENT_CPU_COST);
            totals.picked = totals.picked.saturating_add(1);
            if let Some(r) = rooms.get_mut(ri) {
                r.pending = Some(Pending {
                    tile,
                    at: now,
                    kind: StructureType::Extension,
                    destroy: false,
                    planned: false,
                    missing_at: None,
                });
                r.idle = None;
            }
        } else if let Some(r) = rooms.get_mut(ri) {
            // No tile qualifies: wait until something that could change that does.
            r.idle = Some(key);
        }
        Ok(())
    }

    /// The plan path (P4.3, ADR-026): the economy's container and link needs on the plan's tiles
    /// first (their timing stays ADR-023 to ADR-025's), then [`build::next`]. A look rebuilds the
    /// room's [`Ground`] from the snapshot and movement's last scan; one that finds nothing to do
    /// waits until what it was made from changes or `PlannerConfig::verify_ticks` pass.
    #[allow(clippy::too_many_arguments)]
    fn by_plan(
        &mut self,
        ri: usize,
        row: usize,
        name: RoomName,
        level: u8,
        snap: &Snapshot,
        movement: &mut Movement,
        containers: &[ContainerNeed],
        links: &[LinkNeed],
        plan: &Plan,
        pcfg: &PlannerConfig,
        commit: &mut Commit,
    ) -> Result<(), Fault> {
        let now = self.time;
        let (structures, sites, containers_now, hostile) = room_scan(snap, name, &mut self.tiles);
        let objects = sites.saturating_add(containers_now);
        // A site gone or a structure gone (built or destroyed) shows on the roads and the obstacles
        // only in movement's next scan: ask for one early and wait for it. The first look has
        // nothing to compare with.
        if let Some(r) = self.rooms.get_mut(ri) {
            let gone = r
                .site_tiles
                .iter()
                .any(|t| self.tiles.binary_search(t).is_err());
            if r.counted.is_some_and(|c| c != (structures, containers_now)) || gone {
                r.changed_at = now;
                // A scan this tick already shows it (the opening's begin asks for one when a site
                // count changes).
                if movement.room_scanned(name) != Some(now) {
                    movement.invalidate(name);
                }
            }
            r.counted = Some((structures, containers_now));
            std::mem::swap(&mut r.site_tiles, &mut self.tiles);
        }
        if self
            .rooms
            .get(ri)
            .is_some_and(|r| movement.room_scanned(name).is_none_or(|t| t < r.changed_at))
        {
            return Ok(());
        }
        let Self {
            rooms,
            orders,
            destroys,
            totals,
            ground,
            reach,
            ..
        } = self;
        let Some(r) = rooms.get_mut(ri) else {
            return Ok(());
        };
        // A destroy refused as busy holds every destroy back for a while (the snapshot does not
        // see hostile power creeps, V248); the hold counts as a hostile creep.
        if r.busy_at
            .is_some_and(|t| now.wrapping_sub(t) >= pcfg.verify_ticks)
        {
            r.busy_at = None;
        }
        let hostile = hostile || r.busy_at.is_some();
        if r.full != 0
            && (r.full_level != level || now.wrapping_sub(r.full_at) >= pcfg.verify_ticks)
        {
            r.full = 0;
        }
        let full = r.full;
        let key = PlanKey {
            version: u32::try_from(movement.room_objects(name).len()).unwrap_or(u32::MAX),
            level,
            structures,
            objects,
            hostile,
            refused: r.refused.len(),
            needs: containers.len().saturating_add(links.len()),
            replacing: r.replacing.is_some(),
            full,
            gates: Gates::from_config(pcfg),
            replace: pcfg.replace,
            max_sites: pcfg.max_sites,
        };
        if let Some((k, at)) = r.plan_idle
            && k == key
            && now.wrapping_sub(at) < pcfg.verify_ticks
        {
            return Ok(());
        }
        commit.heavy();
        totals.looks = totals.looks.saturating_add(1);
        ground.clear();
        for &(t, f) in movement.room_objects(name) {
            match f {
                Footprint::Road => ground.road(t),
                Footprint::Blocked => ground.obstacle(t),
            }
        }
        let st = &snap.structures;
        for (&kind, &pos) in st.kinds().iter().zip(st.positions()) {
            if pos.room_name() == name {
                ground.structure(kind, Tile::of(pos));
            }
        }
        let ob = &snap.objects;
        for (&kind, &pos) in ob.kinds().iter().zip(ob.positions()) {
            if pos.room_name() != name {
                continue;
            }
            match kind {
                ObjectKind::Container => ground.structure(StructureType::Container, Tile::of(pos)),
                ObjectKind::Site(k) => ground.site(k, Tile::of(pos)),
                _ => {}
            }
        }
        if hostile {
            ground.hostile();
        }
        // A replacement already standing or sited, or one `build::next` will not place now (its
        // type at the level's count, off-plan ones included, V238, or held for the count), holds
        // nothing; nor one older than `verify_ticks`.
        if let Some(rep) = r.replacing
            && (ground.stands(rep.kind, rep.tile)
                || site_at(snap, name, rep.tile, rep.kind)
                || held_full(full, rep.kind)
                || ground.count(rep.kind) >= allowed(rep.kind, level)
                || now.wrapping_sub(rep.since) >= pcfg.verify_ticks)
        {
            r.replacing = None;
        }
        let destroy =
            pcfg.replace && !hostile && r.replacing.is_none() && r.destroy_lost < LOST_MAX;
        let facts = &plan.facts;
        let roles = &plan.layout.roles;
        let mut action: Option<(Next, bool)> = None;
        // The economy's containers and links first, on the plan's tiles for their roles (ADR-023
        // to ADR-025).
        let container_needs = containers.iter().map(container_need);
        let link_needs = links.iter().map(link_need);
        let economy = container_needs
            .map(|n| (StructureType::Container, n))
            .chain(link_needs.map(|n| (StructureType::Link, n)));
        for (kind, need) in economy {
            let have = match kind {
                StructureType::Container => containers_in(snap, name),
                _ => links_in(snap, name),
            };
            if have >= kind.controller_structures(u32::from(level)) || held_full(full, kind) {
                continue;
            }
            let Some(t) = build::role_tile(roles, &facts.sources, need) else {
                continue;
            };
            if r.refused.contains(&t) || ground.sites() >= pcfg.max_sites {
                continue;
            }
            match build::tile_state(ground, kind, t, destroy) {
                TileState::Free => action = Some((Next::Site { kind, tile: t }, true)),
                TileState::Clear(k) => action = Some((Next::Destroy { kind: k, tile: t }, true)),
                _ => continue,
            }
            break;
        }
        if action.is_none() {
            let ask = Ask {
                layout: &plan.layout,
                terrain: &facts.terrain,
                anchor: plan.anchor,
                level,
                gates: Gates::from_config(pcfg),
                replace: pcfg.replace && r.destroy_lost < LOST_MAX,
                max_sites: pcfg.max_sites,
                holding: r.replacing.is_some(),
                skip: &r.refused,
                full,
            };
            let (next, census) = build::next(&ask, ground, reach);
            r.census = census;
            if next != Next::Idle {
                action = Some((next, false));
            }
        }
        match action {
            Some((Next::Site { kind, tile }, economy)) => {
                orders.push(SiteOrder {
                    room: name,
                    room_row: row,
                    tile,
                    kind,
                });
                commit.add(INTENT_CPU_COST);
                totals.picked = totals.picked.saturating_add(1);
                totals.planned = totals.planned.saturating_add(1);
                if economy {
                    match kind {
                        StructureType::Container => {
                            totals.containers = totals.containers.saturating_add(1);
                        }
                        StructureType::Link => totals.links = totals.links.saturating_add(1),
                        _ => {}
                    }
                }
                r.pending = Some(Pending {
                    tile,
                    at: now,
                    kind,
                    destroy: false,
                    planned: true,
                    missing_at: None,
                });
                r.plan_idle = None;
            }
            Some((Next::Destroy { kind, tile }, _)) => {
                if let Some(target) = destroy_target(snap, name, tile, kind) {
                    destroys.push(DestroyOrder {
                        room: name,
                        tile,
                        kind,
                        target,
                    });
                    commit.add(INTENT_CPU_COST);
                    totals.destroys = totals.destroys.saturating_add(1);
                    r.pending = Some(Pending {
                        tile,
                        at: now,
                        kind,
                        destroy: true,
                        planned: true,
                        missing_at: None,
                    });
                    r.plan_idle = None;
                } else {
                    r.plan_idle = Some((key, now));
                }
            }
            _ => r.plan_idle = Some((key, now)),
        }
        Ok(())
    }

    /// This tick's orders, for the act step.
    pub fn orders(&self) -> &[SiteOrder] {
        &self.orders
    }

    /// The outcome of order `index`. A refusal of the tile (`ERR_INVALID_TARGET`, V205) skips the
    /// tile for the rest of the global. A plan-path site refused for the level's count
    /// (`ERR_RCL_NOT_ENOUGH`: another owner's structures count too, V238) holds its type
    /// ([`RoomPlace::full`]). Any other refusal is about the room and counts toward [`LOST_MAX`]
    /// without marking the tile.
    pub fn record(&mut self, index: usize, outcome: Outcome) {
        let Some(&order) = self.orders.get(index) else {
            return;
        };
        let time = self.time;
        let Some(r) = self.rooms.iter_mut().find(|r| r.name == order.room) else {
            return;
        };
        let planned = r
            .pending
            .is_some_and(|p| p.planned && !p.destroy && p.tile == order.tile);
        match outcome {
            Outcome::Ok => self.totals.placed = self.totals.placed.saturating_add(1),
            Outcome::Failed(ErrorCode::InvalidTarget) => {
                r.refused.push(order.tile);
                r.pending = None;
                self.totals.refused = self.totals.refused.saturating_add(1);
            }
            Outcome::Failed(ErrorCode::RclNotEnough) if planned => {
                if r.full == 0 {
                    r.full_at = time;
                    r.full_level = r.level;
                }
                r.full |= kind_bit(order.kind);
                r.pending = None;
                r.plan_idle = None;
                self.totals.refused = self.totals.refused.saturating_add(1);
                self.totals.full = self.totals.full.saturating_add(1);
            }
            _ => {
                r.pending = None;
                r.lost = r.lost.saturating_add(1);
                self.totals.refused = self.totals.refused.saturating_add(1);
            }
        }
    }

    /// This tick's destroys, for the act step (ADR-026).
    pub fn destroys(&self) -> &[DestroyOrder] {
        &self.destroys
    }

    /// The outcome of destroy `index`. OK leaves it pending until the structure is gone (the
    /// processor may still drop it, V251); `ERR_BUSY` (a hostile creep or power creep, V248) holds
    /// the room's destroys for `PlannerConfig::verify_ticks`; any other outcome counts toward the
    /// room's destroy limit ([`LOST_MAX`] in a row), never toward its sites'.
    pub fn record_destroy(&mut self, index: usize, outcome: Outcome) {
        let Some(&order) = self.destroys.get(index) else {
            return;
        };
        let Some(r) = self.rooms.iter_mut().find(|r| r.name == order.room) else {
            return;
        };
        match outcome {
            Outcome::Ok => self.totals.destroyed = self.totals.destroyed.saturating_add(1),
            Outcome::Failed(ErrorCode::Busy) => {
                r.pending = None;
                r.busy_at = Some(self.time);
                r.plan_idle = None;
            }
            _ => {
                r.pending = None;
                r.destroy_lost = r.destroy_lost.saturating_add(1);
                r.plan_idle = None;
            }
        }
    }

    /// Room `room`'s last look at its plan (P4.3), for the console.
    pub fn census(&self, room: RoomName) -> Option<&build::Census> {
        self.rooms
            .iter()
            .find(|r| r.name == room)
            .map(|r| &r.census)
    }

    pub fn totals(&self) -> &PlacementTotals {
        &self.totals
    }

    /// The tiles of `room` this global gave up on (refused by the game, or never seen within
    /// [`APPEAR_TICKS`]); `economy::mining` picks no link tile there.
    pub fn refused_tiles(&self, room: RoomName) -> &[Tile] {
        self.rooms
            .iter()
            .find(|r| r.name == room)
            .map_or(&[], |r| r.refused.as_slice())
    }

    /// Tiles skipped per room, for the console.
    pub fn refused(&self) -> impl Iterator<Item = (RoomName, usize)> + '_ {
        self.rooms.iter().map(|r| (r.name, r.refused.len()))
    }
}

/// Whether a structure of type `kind` (an own one, or a container, which has no owner) or an own
/// site of it stands on `tile` in `room`.
fn placed_at(snap: &Snapshot, room: RoomName, tile: Tile, kind: StructureType) -> bool {
    let here = |p: &rustedscreep_consts::Position| p.room_name() == room && Tile::of(*p) == tile;
    let st = &snap.structures;
    let ob = &snap.objects;
    st.kinds()
        .iter()
        .zip(st.positions())
        .any(|(&k, p)| k == kind && here(p))
        || ob.kinds().iter().zip(ob.positions()).any(|(&k, p)| {
            (k == ObjectKind::Site(kind)
                || (kind == StructureType::Container && k == ObjectKind::Container))
                && here(p)
        })
}

/// Whether the room's road or wall pick, past [`APPEAR_TICKS`] with no site and no structure in the
/// snapshot, still waits for a scan before it counts as lost: its site may have been built while
/// the placement did not run. It waits from the tick the site was first found gone
/// (`Pending::missing_at`, when a road built from it already stands) until a scan from then on
/// (asking for one), for at most `APPEAR_TICKS` plus `MovementConfig::rescan_min_ticks` (the
/// earliest a rescan comes).
fn unseen_wait(
    r: &mut RoomPlace,
    movement: &mut Movement,
    mcfg: &MovementConfig,
    name: RoomName,
    now: u32,
) -> bool {
    let Some(p) = r.pending.as_mut() else {
        return false;
    };
    let since = *p.missing_at.get_or_insert(now);
    let scanned = movement.room_scanned(name);
    if scanned.is_some_and(|t| t >= since) {
        return false;
    }
    if now.wrapping_sub(since) >= APPEAR_TICKS.saturating_add(mcfg.rescan_min_ticks) {
        return false;
    }
    if scanned != Some(now) {
        movement.invalidate(name);
    }
    true
}

/// Whether `kind` has no owner and is left out of the snapshot once built: a road or a wall.
fn unowned(kind: StructureType) -> bool {
    matches!(kind, StructureType::Road | StructureType::Wall)
}

/// Whether movement's last scan of `room` shows a built `kind` that has no owner ([`unowned`]) on
/// `tile`: a road's footprint, or an obstacle's for a wall.
fn scanned_at(movement: &Movement, room: RoomName, tile: Tile, kind: StructureType) -> bool {
    let want = match kind {
        StructureType::Road => Footprint::Road,
        StructureType::Wall => Footprint::Blocked,
        _ => return false,
    };
    movement
        .room_objects(room)
        .iter()
        .any(|&(t, f)| t == tile && f == want)
}

/// `kind`'s bit in [`RoomPlace::full`].
fn kind_bit(kind: StructureType) -> u32 {
    u32::try_from(kind_rank(kind))
        .ok()
        .and_then(|r| 1u32.checked_shl(r))
        .unwrap_or(0)
}

/// Whether `full` holds `kind` ([`RoomPlace::full`]).
fn held_full(full: u32, kind: StructureType) -> bool {
    full & kind_bit(kind) != 0
}

/// Whether a structure of `kind` (an own one, or a container) stands on `tile` in `room`.
fn stands_at(snap: &Snapshot, room: RoomName, tile: Tile, kind: StructureType) -> bool {
    destroy_target(snap, room, tile, kind).is_some()
}

/// Whether an own site of `kind` is on `tile` in `room`.
fn site_at(snap: &Snapshot, room: RoomName, tile: Tile, kind: StructureType) -> bool {
    let ob = &snap.objects;
    ob.kinds()
        .iter()
        .zip(ob.positions())
        .any(|(&k, p)| k == ObjectKind::Site(kind) && p.room_name() == room && Tile::of(*p) == tile)
}

/// The snapshot row of the `kind` on `tile` in `room`: an own structure, or a container.
fn destroy_target(
    snap: &Snapshot,
    room: RoomName,
    tile: Tile,
    kind: StructureType,
) -> Option<DestroyTarget> {
    let here = |p: &rustedscreep_consts::Position| p.room_name() == room && Tile::of(*p) == tile;
    if kind == StructureType::Container {
        let ob = &snap.objects;
        return ob
            .kinds()
            .iter()
            .zip(ob.positions())
            .position(|(&k, p)| k == ObjectKind::Container && here(p))
            .map(DestroyTarget::Object);
    }
    let st = &snap.structures;
    st.kinds()
        .iter()
        .zip(st.positions())
        .position(|(&k, p)| k == kind && here(p))
        .map(DestroyTarget::Structure)
}

/// The plan's role for an economy container need ([`ContainerNeed`]).
fn container_need(c: &ContainerNeed) -> Need {
    match c.source {
        None => Need::ControllerContainer,
        Some(s) => Need::MiningContainer(s),
    }
}

/// The plan's role for an economy link need ([`LinkNeed`]).
fn link_need(l: &LinkNeed) -> Need {
    match l.source {
        None => Need::ControllerLink,
        Some(s) => Need::SourceLink(s),
    }
}

/// What a destroy on `tile` makes way for, if the bot places it now: of the plan's items on the
/// tile that are, the costed structure (not a road or a rampart), else the road. An item is placed
/// now when its gate is on and it is due at `level` ([`Gates::due_level`]), and, for one of the
/// economy's containers and links (which `build::next` never places), when the economy asks for
/// it this tick (one of `containers` or `links` maps to the tile, [`build::role_tile`]). Whether
/// the level's count leaves room for it is judged at each look (`by_plan`).
fn due_replacement(
    plan: &Plan,
    tile: Tile,
    gates: Gates,
    level: u8,
    containers: &[ContainerNeed],
    links: &[LinkNeed],
) -> Option<StructureType> {
    let layout = &plan.layout;
    let roles = &layout.roles;
    let sources = &plan.facts.sources;
    let asked = |i: &Item| match i.kind {
        StructureType::Container if !gates.places(i.kind, i.tile, roles) => containers
            .iter()
            .any(|c| build::role_tile(roles, sources, container_need(c)) == Some(tile)),
        StructureType::Link if !gates.places(i.kind, i.tile, roles) => links
            .iter()
            .any(|l| build::role_tile(roles, sources, link_need(l)) == Some(tile)),
        _ => true,
    };
    let placed = |i: &&Item| {
        gates.on(Gate::of(i.kind, i.tile, roles))
            && gates.due_level(layout, i).is_some_and(|l| l <= level)
            && asked(i)
    };
    let at = || {
        layout
            .items
            .iter()
            .filter(|i| i.tile == tile)
            .filter(placed)
    };
    at().find(|i| i.kind != StructureType::Road && i.kind != StructureType::Rampart)
        .or_else(|| at().find(|i| i.kind == StructureType::Road))
        .map(|i| i.kind)
}

/// `room`'s own structures, its own sites (their tiles into `site_tiles`, sorted), its containers
/// (not piles, tombstones or ruins, which change often and stand in no plan's way) and whether a
/// hostile creep is there, from the snapshot, one pass a table: what a plan-path look watches.
fn room_scan(
    snap: &Snapshot,
    room: RoomName,
    site_tiles: &mut Vec<Tile>,
) -> (usize, usize, usize, bool) {
    let st = &snap.structures;
    let structures = st
        .positions()
        .iter()
        .filter(|p| p.room_name() == room)
        .count();
    let ob = &snap.objects;
    let (mut sites, mut containers) = (0usize, 0usize);
    site_tiles.clear();
    for (k, p) in ob.kinds().iter().zip(ob.positions()) {
        if p.room_name() != room {
            continue;
        }
        match k {
            ObjectKind::Site(_) => {
                sites += 1;
                site_tiles.push(Tile::of(*p));
            }
            ObjectKind::Container => containers += 1,
            _ => {}
        }
    }
    site_tiles.sort_unstable();
    let cr = &snap.creeps;
    let hostile = cr
        .positions()
        .iter()
        .zip(cr.my())
        .any(|(p, &my)| !my && p.room_name() == room);
    (structures, sites, containers, hostile)
}

/// Containers and own container sites in `room` (containers have no owner, so the snapshot keeps
/// them with the room objects, not the own structures).
pub fn containers_in(snap: &Snapshot, room: RoomName) -> u32 {
    let ob = &snap.objects;
    let n = ob
        .kinds()
        .iter()
        .zip(ob.positions())
        .filter(|&(&k, p)| {
            matches!(
                k,
                ObjectKind::Container | ObjectKind::Site(StructureType::Container)
            ) && p.room_name() == room
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Own links and link sites in `room`.
pub fn links_in(snap: &Snapshot, room: RoomName) -> u32 {
    let st = &snap.structures;
    let built = st
        .kinds()
        .iter()
        .zip(st.positions())
        .filter(|&(&k, p)| k == StructureType::Link && p.room_name() == room)
        .count();
    let ob = &snap.objects;
    let sites = ob
        .kinds()
        .iter()
        .zip(ob.positions())
        .filter(|&(&k, p)| k == ObjectKind::Site(StructureType::Link) && p.room_name() == room)
        .count();
    u32::try_from(built + sites).unwrap_or(u32::MAX)
}

/// Own extensions and extension sites in `room`.
pub fn extensions_in(snap: &Snapshot, room: RoomName) -> u32 {
    let st = &snap.structures;
    let built = st
        .kinds()
        .iter()
        .zip(st.positions())
        .filter(|&(&k, p)| k == StructureType::Extension && p.room_name() == room)
        .count();
    let ob = &snap.objects;
    let sites = ob
        .kinds()
        .iter()
        .zip(ob.positions())
        .filter(|&(&k, p)| k == ObjectKind::Site(StructureType::Extension) && p.room_name() == room)
        .count();
    u32::try_from(built + sites).unwrap_or(u32::MAX)
}

/// The next extension tile (module docs), or `None` when no tile qualifies. `occupied` holds every
/// tile with a structure, a site, a source or a container, with whether it blocks movement (the
/// grid may not show a site placed a tick ago yet); `statics` the sources and minerals; `refused`
/// the tiles to skip. `field` and `dial` are scratch: a flow field from the spawn (range 1) orders
/// the candidates by path, then by tile index.
///
/// Uses: [`EDGE_MARGIN`], [`SPAWN_CLEAR`], [`CONTROLLER_CLEAR`], `ROOM_SIZE`.
#[allow(clippy::too_many_arguments)]
pub fn next_extension(
    grid: &CostGrid,
    profile: CostProfile,
    spawn: Tile,
    controller: Option<Tile>,
    statics: &[Tile],
    occupied: &[(Tile, bool)],
    refused: &[Tile],
    field: &mut FlowField,
    dial: &mut Dial,
) -> Option<Tile> {
    let blocked = |t: Tile| !grid.walkable(t) || occupied.iter().any(|&(o, b)| o == t && b);
    let taken = |t: Tile| occupied.iter().any(|&(o, _)| o == t);
    if field.compute(
        grid,
        Goal {
            tile: spawn,
            range: 1,
        },
        dial,
    ) == 0
    {
        return None;
    }
    let parity = (spawn.x() + spawn.y()) % 2;
    let high = ROOM_SIZE - EDGE_MARGIN;
    let mut best: Option<(u16, usize, Tile)> = None;
    for index in 0..ROOM_AREA {
        let Some(t) = Tile::from_index(index) else {
            continue;
        };
        let (x, y) = (t.x(), t.y());
        if x < EDGE_MARGIN || y < EDGE_MARGIN || x >= high || y >= high {
            continue;
        }
        if (x + y) % 2 != parity || t.range(spawn) < SPAWN_CLEAR {
            continue;
        }
        if controller.is_some_and(|c| t.range(c) <= CONTROLLER_CLEAR)
            || statics.iter().any(|&s| t.range(s) <= 1)
            || refused.contains(&t)
        {
            continue;
        }
        // A road keeps its tile (the cost tells it from plain while the profile does).
        if blocked(t) || taken(t) || (grid.cost(t) == profile.road && profile.road != profile.plain)
        {
            continue;
        }
        let d = field.distance(t);
        if d == UNREACHED {
            continue;
        }
        if best.is_some_and(|(bd, bi, _)| (bd, bi) <= (d, index)) {
            continue;
        }
        let fillable = [
            Direction::Top,
            Direction::Right,
            Direction::Bottom,
            Direction::Left,
        ]
        .into_iter()
        .filter_map(|dir| t.step(dir))
        .any(|n| !blocked(n) && field.distance(n) != UNREACHED);
        if fillable && keeps_neighbours_connected(t, blocked) {
            best = Some((d, index, t));
        }
    }
    best.map(|(_, _, t)| t)
}

/// Whether the free neighbours of `t` stay one group (8-connected among themselves) once `t` is
/// blocked: then any way through `t` can go around it, so blocking it never splits the room.
pub(crate) fn keeps_neighbours_connected(t: Tile, blocked: impl Fn(Tile) -> bool) -> bool {
    let mut free = [None; 8];
    let mut n = 0;
    for (_, nb) in t.neighbors() {
        if !blocked(nb)
            && let Some(slot) = free.get_mut(n)
        {
            *slot = Some(nb);
            n += 1;
        }
    }
    if n == 0 {
        return false;
    }
    // Flood from the first free neighbour over free neighbours one step apart.
    let mut seen = [false; 8];
    if let Some(s) = seen.first_mut() {
        *s = true;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n {
            if !seen.get(i).copied().unwrap_or(true) {
                continue;
            }
            let Some(Some(a)) = free.get(i).copied() else {
                continue;
            };
            for j in 0..n {
                if seen.get(j).copied().unwrap_or(true) {
                    continue;
                }
                let Some(Some(b)) = free.get(j).copied() else {
                    continue;
                };
                if a.range(b) <= 1
                    && let Some(s) = seen.get_mut(j)
                {
                    *s = true;
                    changed = true;
                }
            }
        }
    }
    seen.iter().take(n).all(|&s| s)
}
