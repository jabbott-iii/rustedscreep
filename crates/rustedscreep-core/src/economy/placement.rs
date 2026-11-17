//! Interim extension placement (ADR-020) and source containers (ADR-023); the P4 base planner keeps
//! or replaces what it builds: while an owned room with a spawn has fewer extensions and extension
//! sites than its level allows (`CONTROLLER_STRUCTURES`), the Optional job picks one tile per tick
//! and the act step calls `Room.createConstructionSite` on it. The opening's workers build the
//! sites.
//!
//! Containers go first: the mining tiles `economy::mining` lists as needing one
//! (`Mining::container_needs`: the room pays for the source's miner and the tile has no container
//! and no container site), while the room has fewer containers and container sites than its level
//! allows. They take the same one call per room per tick and the same refusal rules below.
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

use rustedscreep_consts::{
    Direction, INTENT_CPU_COST, ROOM_AREA, ROOM_SIZE, RoomName, StructureType,
};

use rustedscreep_consts::ErrorCode;

use crate::config::MovementConfig;
use crate::intents::Outcome;
use crate::movement::{CostGrid, CostProfile, Dial, FlowField, Goal, Movement, Tile, UNREACHED};
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
/// when other players' extensions count toward the level, `ERR_FULL` at `MAX_CONSTRUCTION_SITES`,
/// `ERR_NOT_OWNER`, a throw). After this many the room gets no more calls for the rest of the
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

#[derive(Clone, Copy, Debug)]
struct Pending {
    tile: Tile,
    at: u32,
    kind: StructureType,
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
}

/// Counters since the global started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlacementTotals {
    pub picked: u32,
    /// Of `picked`, container sites (ADR-023).
    pub containers: u32,
    pub placed: u32,
    pub refused: u32,
    /// Picks that never appeared within [`APPEAR_TICKS`].
    pub lost: u32,
}

/// The placement's state for one global.
#[derive(Default)]
pub struct Placement {
    time: u32,
    rooms: Vec<RoomPlace>,
    orders: Vec<SiteOrder>,
    totals: PlacementTotals,
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

    /// Room `unit`'s pick for this tick, if it needs one (module docs): a container site on the
    /// first of `containers` (the room's mining tiles that need one) not refused yet, else, with
    /// `extensions`, an extension site. An extension pick is a heavy unit (a flow field from the
    /// spawn); either commits one intent (`INTENT_CPU_COST`: each accepted call is billed, V203).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_room(
        &mut self,
        unit: u32,
        snap: &Snapshot,
        movement: &mut Movement,
        mcfg: &MovementConfig,
        extensions: bool,
        containers: &[Tile],
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
        let allowed = StructureType::Extension.controller_structures(u32::from(level));
        let have = extensions_in(snap, name);
        if let Some(p) = r.pending {
            // Judged by its own tile, so a structure lost elsewhere does not hide it.
            if placed_at(snap, name, p.tile, p.kind) {
                r.pending = None;
                r.lost = 0;
            } else if now.wrapping_sub(p.at) < APPEAR_TICKS {
                return Ok(());
            } else {
                r.refused.push(p.tile);
                r.pending = None;
                r.lost = r.lost.saturating_add(1);
                self.totals.lost = self.totals.lost.saturating_add(1);
            }
        }
        if r.lost >= LOST_MAX {
            return Ok(());
        }
        // A container for a mining tile first (ADR-023): no pattern, no flow field.
        let container = StructureType::Container;
        if let Some(&tile) = containers.iter().find(|t| !r.refused.contains(t))
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
                });
                r.idle = None;
            }
        } else if let Some(r) = rooms.get_mut(ri) {
            // No tile qualifies: wait until something that could change that does.
            r.idle = Some(key);
        }
        Ok(())
    }

    /// This tick's orders, for the act step.
    pub fn orders(&self) -> &[SiteOrder] {
        &self.orders
    }

    /// The outcome of order `index`. A refusal of the tile (`ERR_INVALID_TARGET`, V205) skips the
    /// tile for the rest of the global; any other refusal is about the room and counts toward
    /// [`LOST_MAX`] without marking the tile.
    pub fn record(&mut self, index: usize, outcome: Outcome) {
        let Some(&order) = self.orders.get(index) else {
            return;
        };
        let Some(r) = self.rooms.iter_mut().find(|r| r.name == order.room) else {
            return;
        };
        match outcome {
            Outcome::Ok => self.totals.placed = self.totals.placed.saturating_add(1),
            Outcome::Failed(ErrorCode::InvalidTarget) => {
                r.refused.push(order.tile);
                r.pending = None;
                self.totals.refused = self.totals.refused.saturating_add(1);
            }
            _ => {
                r.pending = None;
                r.lost = r.lost.saturating_add(1);
                self.totals.refused = self.totals.refused.saturating_add(1);
            }
        }
    }

    pub fn totals(&self) -> &PlacementTotals {
        &self.totals
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
fn keeps_neighbours_connected(t: Tile, blocked: impl Fn(Tile) -> bool) -> bool {
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
