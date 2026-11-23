//! Placing a room's plan (ROADMAP P4.3, ADR-026): what stands in the room is compared with the
//! plan's items due at the room's level, and one action comes out a room a tick: a construction
//! site, a destroy of an own structure that stands in the plan's way, or nothing.
//!
//! - What is placed ([`Gates`]): the plan's spawns and extensions always; the storage and the hub
//!   link, the towers, the ramparts and walls, and the late structures (terminal, extractor and
//!   mineral container, labs, factory, power spawn, nuker, observer) only when their gate is on
//!   (the user's choice, 2026-11-18: only what the bot uses); a road from the earliest level a
//!   structure it serves is placed ([`Gates::due_level`], `layout::RoadNeed`), so the mineral's
//!   road waits with the extractor. The mining tiles' containers, the controller container and
//!   the controller and source links come from `economy::mining`, which keeps ADR-023 to
//!   ADR-025's timing; [`role_tile`] maps each to the plan's tile for its role.
//! - The order: by type in [`KINDS`]'s order (spawns and extensions first, roads late), then the
//!   item's level, then the tile, so a level-up's extensions go out before the roads behind them.
//! - A planned item is built when its type stands on its tile, sited when an own site of its type
//!   is there, capped when the room already holds as many of its type (structures and sites,
//!   V238) as the level allows or the game refused a site of it for the level's count
//!   ([`Ask::full`]: another owner's structures count too, and the snapshot holds only own
//!   ones), and blocked when its tile holds something else.
//! - Replacing (ADR-026, the user's choice: replace blockers only): an own structure that is not
//!   the plan's on its tile (no spawn) is destroyed when a planned item that is due, placed and not
//!   capped needs the tile, or when, with it standing, such an item has no free neighbor reached
//!   from the plan's anchor (it cuts the item off; the first cut-off item in the order that some
//!   destroy opens). Never while a hostile creep is in the room (`destroy` returns `ERR_BUSY`
//!   then, V248, V249), never while the room is at `max_sites` (the replacement could not be
//!   sited), and never while the caller holds the last destroy's replacement open. A road needs its
//!   tile too (ADR-026's amendment, the user's choice, 2026-11-18): it is not placed under an
//!   obstacle (V237 would accept it), and a road that stands under one counts as missing, so an
//!   own off-plan extension there is destroyed for it by the same rules ([`road_clears`]). Any
//!   other obstacle (a link, a storage, tower or late structure, one the snapshot does not hold)
//!   or an own site of another type leaves the road blocked: a road's many tiles must not take a
//!   structure whose store or service a destroy would lose.
//!
//! The ground is rebuilt from the snapshot and movement's last scan before each look, which the
//! caller makes only when something it watches changed (`economy::placement`).

use std::collections::VecDeque;

use rustedscreep_consts::{ROOM_AREA, StructureType};

use super::layout::{Gate, KINDS, Layout, Roles, kind_rank};
use super::rules::{allowed, blocks};
use crate::config::PlannerConfig;
use crate::movement::{TerrainGrid, TerrainKind, Tile};

/// No costed structure on the tile.
const NONE: u8 = 0;
/// An obstacle the snapshot holds no object for (another player's structure, a portal, or one
/// movement's last scan shows that the snapshot no longer has).
const FOREIGN: u8 = u8::MAX;

/// Which of the plan's types the bot places ([`PlannerConfig`]'s switches).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gates {
    pub storage: bool,
    pub towers: bool,
    pub ramparts: bool,
    pub late: bool,
}

impl Gates {
    pub const fn from_config(cfg: &PlannerConfig) -> Self {
        Self {
            storage: cfg.storage,
            towers: cfg.towers,
            ramparts: cfg.ramparts,
            late: cfg.late,
        }
    }

    /// Whether the structures of `gate` are built.
    pub const fn on(&self, gate: Gate) -> bool {
        match gate {
            Gate::Base => true,
            Gate::Storage => self.storage,
            Gate::Towers => self.towers,
            Gate::Ramparts => self.ramparts,
            Gate::Late => self.late,
        }
    }

    /// Whether the plan's `kind` on `tile` is placed by [`next`]: its gate is on, and it is not
    /// one of the economy's containers and links, which are placed through [`role_tile`]. A road
    /// is placed from the level [`Gates::due_level`] gives.
    pub fn places(&self, kind: StructureType, tile: Tile, roles: &Roles) -> bool {
        let economy = match kind {
            StructureType::Container => roles.mineral_container != Some(tile),
            StructureType::Link => roles.hub_link != Some(tile),
            _ => false,
        };
        !economy && self.on(Gate::of(kind, tile, roles))
    }

    /// The level `item` of `layout` is due from with these gates: its own, but for a road, the
    /// earliest level a structure it serves is placed from ([`Layout::needs`]); `None` for a road
    /// that serves only structures whose gate is off.
    pub fn due_level(&self, layout: &Layout, item: &super::layout::Item) -> Option<u8> {
        if item.kind != StructureType::Road {
            return Some(item.level);
        }
        match layout.road_need(item.tile) {
            Some(need) => need.level(|g| self.on(g)),
            None => Some(item.level),
        }
    }
}

/// What stands in one room, by tile: the costed structures (with whether the snapshot holds the
/// object, so it can be destroyed), the roads, the own sites, the counts by type and whether a
/// hostile creep is there.
pub struct Ground {
    what: Box<[u8; ROOM_AREA]>,
    held: Box<[bool; ROOM_AREA]>,
    road: Box<[bool; ROOM_AREA]>,
    rampart: Box<[bool; ROOM_AREA]>,
    site: Box<[u8; ROOM_AREA]>,
    counts: [u32; KINDS.len()],
    sites: u32,
    hostile: bool,
}

impl Default for Ground {
    fn default() -> Self {
        Self {
            what: Box::new([NONE; ROOM_AREA]),
            held: Box::new([false; ROOM_AREA]),
            road: Box::new([false; ROOM_AREA]),
            rampart: Box::new([false; ROOM_AREA]),
            site: Box::new([NONE; ROOM_AREA]),
            counts: [0; KINDS.len()],
            sites: 0,
            hostile: false,
        }
    }
}

fn code(kind: StructureType) -> Option<u8> {
    let r = kind_rank(kind);
    (r < KINDS.len())
        .then(|| u8::try_from(r + 1).ok())
        .flatten()
}

fn decode(c: u8) -> Option<StructureType> {
    match c {
        NONE | FOREIGN => None,
        c => KINDS.get(usize::from(c - 1)).copied(),
    }
}

impl Ground {
    /// Empties it for the next look.
    pub fn clear(&mut self) {
        self.what.fill(NONE);
        self.held.fill(false);
        self.road.fill(false);
        self.rampart.fill(false);
        self.site.fill(NONE);
        self.counts = [0; KINDS.len()];
        self.sites = 0;
        self.hostile = false;
    }

    fn count_up(&mut self, kind: StructureType) {
        if let Some(c) = self.counts.get_mut(kind_rank(kind)) {
            *c = c.saturating_add(1);
        }
    }

    /// A footprint from movement's scan: an obstacle of unknown type (call before the snapshot's
    /// structures and sites, which take its place where they stand).
    pub fn obstacle(&mut self, t: Tile) {
        if let Some(c) = self.what.get_mut(t.index())
            && *c == NONE
        {
            *c = FOREIGN;
        }
    }

    /// A road (movement's scan: roads have no owner, V238 counts them all).
    pub fn road(&mut self, t: Tile) {
        if let Some(r) = self.road.get_mut(t.index())
            && !*r
        {
            *r = true;
            self.count_up(StructureType::Road);
        }
    }

    /// A structure of `kind` on `t` that the snapshot holds (an own one or a container), so it can
    /// be destroyed. A road or a type with no construction cost (the controller) is ignored here.
    pub fn structure(&mut self, kind: StructureType, t: Tile) {
        if kind == StructureType::Road {
            self.road(t);
            return;
        }
        let Some(c) = code(kind) else {
            return;
        };
        self.count_up(kind);
        if kind == StructureType::Rampart {
            if let Some(r) = self.rampart.get_mut(t.index()) {
                *r = true;
            }
            return;
        }
        if let Some(w) = self.what.get_mut(t.index()) {
            *w = c;
        }
        if let Some(h) = self.held.get_mut(t.index()) {
            *h = true;
        }
    }

    /// An own construction site of `kind` on `t` (a site's tile holds no costed structure but a
    /// road or a rampart, V237, so an obstacle footprint there was the site's own).
    pub fn site(&mut self, kind: StructureType, t: Tile) {
        let Some(c) = code(kind) else {
            return;
        };
        self.count_up(kind);
        self.sites = self.sites.saturating_add(1);
        if let Some(s) = self.site.get_mut(t.index()) {
            *s = c;
        }
        if let Some(w) = self.what.get_mut(t.index())
            && *w == FOREIGN
        {
            *w = NONE;
        }
    }

    /// A hostile creep is in the room.
    pub fn hostile(&mut self) {
        self.hostile = true;
    }

    /// Own sites in the room.
    pub fn sites(&self) -> u32 {
        self.sites
    }

    /// Structures and sites of `kind` in the room.
    pub fn count(&self, kind: StructureType) -> u32 {
        self.counts.get(kind_rank(kind)).copied().unwrap_or(0)
    }

    fn what(&self, t: Tile) -> u8 {
        self.what.get(t.index()).copied().unwrap_or(FOREIGN)
    }

    fn site_at(&self, t: Tile) -> Option<StructureType> {
        decode(self.site.get(t.index()).copied().unwrap_or(NONE))
    }

    fn has_road(&self, t: Tile) -> bool {
        self.road.get(t.index()).copied().unwrap_or(false)
    }

    /// Whether `kind` stands on `t` (a road, an own rampart or a costed structure).
    pub fn stands(&self, kind: StructureType, t: Tile) -> bool {
        match kind {
            StructureType::Road => self.has_road(t),
            StructureType::Rampart => self.rampart.get(t.index()).copied().unwrap_or(false),
            _ => decode(self.what(t)) == Some(kind),
        }
    }

    /// Whether an obstacle stands on `t`: an obstacle structure, one of unknown type, or an own
    /// site of an obstacle type.
    fn obstructed(&self, t: Tile) -> bool {
        let w = self.what(t);
        w == FOREIGN || decode(w).is_some_and(blocks) || self.site_at(t).is_some_and(blocks)
    }

    /// The structure on `t` the bot may destroy for another type: one the snapshot holds, with a
    /// construction cost (V251), that is not a spawn.
    fn replaceable(&self, t: Tile) -> Option<StructureType> {
        let held = self.held.get(t.index()).copied().unwrap_or(false);
        decode(self.what(t)).filter(|&k| held && k != StructureType::Spawn)
    }
}

/// What a planned item's tile allows now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    /// Its type stands there.
    Built,
    /// An own site of its type is there.
    Sited,
    /// A site may go there.
    Free,
    /// An own structure of `kind` stands in the way and may be destroyed for it.
    Clear(StructureType),
    /// Something else is in the way: another site, a spawn, a structure the snapshot does not
    /// hold, or a destroy that may not happen now.
    Blocked,
}

/// What `kind` on `t` finds on the ground. `destroy` says whether a blocker may be destroyed now
/// (replacing is on, no hostile creep, no destroy's replacement still open).
///
/// Rules (V237): any site refuses a site; a road or a rampart may go over any structure, but a road
/// is not placed under an obstacle, which it may clear as other types do (ADR-026's amendment;
/// [`next`] narrows a road's clear to an extension, [`road_clears`]), and a road standing under
/// one is judged as missing; every other type is refused by any costed structure that is not a
/// road or a rampart.
pub fn tile_state(ground: &Ground, kind: StructureType, t: Tile, destroy: bool) -> TileState {
    // A road that stands under an obstacle (one built over it by hand) is not the plan's road: its
    // tile is judged as if the road were missing, so the obstacle may clear.
    let covered = kind == StructureType::Road && ground.obstructed(t);
    if ground.stands(kind, t) && !covered {
        return TileState::Built;
    }
    match ground.site_at(t) {
        Some(k) if k == kind => return TileState::Sited,
        Some(_) => return TileState::Blocked,
        None => {}
    }
    match kind {
        StructureType::Road => {
            if !covered {
                TileState::Free
            } else {
                // An own obstacle off the plan (no spawn) clears; a foreign one does not. An own
                // site of another type was refused above.
                match ground.replaceable(t) {
                    Some(k) if destroy && blocks(k) => TileState::Clear(k),
                    _ => TileState::Blocked,
                }
            }
        }
        StructureType::Rampart => TileState::Free,
        _ => match ground.what(t) {
            NONE => TileState::Free,
            _ => match ground.replaceable(t) {
                Some(k) if destroy => TileState::Clear(k),
                _ => TileState::Blocked,
            },
        },
    }
}

/// One action for the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// `createConstructionSite` for `kind` on `tile`.
    Site { kind: StructureType, tile: Tile },
    /// `destroy` the own `kind` on `tile`, for the plan's item there or for one it cuts off.
    Destroy { kind: StructureType, tile: Tile },
    /// Nothing to do now.
    Idle,
}

/// The plan's items due at the room's level that the bot places, by state, for the console.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    pub due: u32,
    pub built: u32,
    pub sited: u32,
    /// Free to place, or to place once a blocker is destroyed.
    pub missing: u32,
    /// The room holds as many of the type as the level allows (off-plan ones included).
    pub capped: u32,
    /// Blocked and not to be cleared now.
    pub blocked: u32,
    /// With no free neighbor reached from the plan's first spawn.
    pub cut_off: u32,
}

/// What [`next`] looks at besides the ground.
pub struct Ask<'a> {
    pub layout: &'a Layout,
    pub terrain: &'a TerrainGrid,
    /// The plan's anchor (its standing spawn, or the own spawn site it was anchored on): reach is
    /// measured from it.
    pub anchor: Tile,
    pub level: u8,
    pub gates: Gates,
    pub replace: bool,
    pub max_sites: u32,
    /// A destroy's replacement is still open: no destroy now.
    pub holding: bool,
    /// Tiles the game refused this global (`economy::placement`): their items count as blocked.
    pub skip: &'a [Tile],
    /// Types the game refused a site of for the level's count (`ERR_RCL_NOT_ENOUGH`: another
    /// owner's structures count too, V238), one bit per [`kind_rank`]: their items count as
    /// capped.
    pub full: u32,
}

/// Whether `kind` is at the level's count in the room: as many structures and sites of it as the
/// level allows (V238), or held by [`Ask::full`].
fn capped(ask: &Ask<'_>, ground: &Ground, kind: StructureType) -> bool {
    let held = u32::try_from(kind_rank(kind))
        .ok()
        .and_then(|r| 1u32.checked_shl(r))
        .is_some_and(|bit| ask.full & bit != 0);
    held || ground.count(kind) >= allowed(kind, ask.level)
}

/// Whether a due road may have an own off-plan `blocker` destroyed for it (ADR-026's amendment):
/// only an extension, which the bot places under every gate and builds again on a plan slot, and
/// whose store is at most `EXTENSION_ENERGY_CAPACITY`. A spawn is never cleared ([`tile_state`]);
/// one of the economy's links, and a storage, tower or late structure, whose store or service a
/// destroy would lose (V195, V252), keep the road blocked until the user decides otherwise for
/// their phases (P5, P7, P9).
fn road_clears(blocker: StructureType) -> bool {
    blocker == StructureType::Extension
}

/// The key a due item is offered by: its type's place in [`KINDS`], its level, its tile.
type Key = (usize, u8, usize);

/// Reused buffers for the reach check and the opening search.
pub struct Scratch {
    walk: Box<[bool; ROOM_AREA]>,
    reached: Box<[bool; ROOM_AREA]>,
    cost: Box<[u16; ROOM_AREA]>,
    parent: Box<[u16; ROOM_AREA]>,
    queue: VecDeque<Tile>,
    /// The due items cut off this look that a destroy could open for (their own tile could take
    /// their site, or already holds it), by key.
    cut: Vec<(Key, Tile)>,
    /// Per tile, the plan's costed structure there (not a road or a rampart), as [`code`] gives
    /// it: filled once per opening search.
    planned: Box<[u8; ROOM_AREA]>,
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            walk: Box::new([false; ROOM_AREA]),
            reached: Box::new([false; ROOM_AREA]),
            cost: Box::new([u16::MAX; ROOM_AREA]),
            parent: Box::new([u16::MAX; ROOM_AREA]),
            queue: VecDeque::new(),
            cut: Vec::new(),
            planned: Box::new([NONE; ROOM_AREA]),
        }
    }
}

fn get<T: Copy>(a: &[T; ROOM_AREA], t: Tile, or: T) -> T {
    a.get(t.index()).copied().unwrap_or(or)
}

fn set<T>(a: &mut [T; ROOM_AREA], t: Tile, v: T) {
    if let Some(c) = a.get_mut(t.index()) {
        *c = v;
    }
}

/// The room's next action and its census (module docs).
pub fn next(ask: &Ask<'_>, ground: &Ground, s: &mut Scratch) -> (Next, Census) {
    let destroy = ask.replace && !ground.hostile && !ask.holding;
    let mut census = Census::default();
    let roles = &ask.layout.roles;
    let due = |i: &&super::layout::Item| {
        ask.gates.places(i.kind, i.tile, roles)
            && ask
                .gates
                .due_level(ask.layout, i)
                .is_some_and(|l| l <= ask.level)
    };
    let key = |kind: StructureType, level: u8, t: Tile| (kind_rank(kind), level, t.index());
    let mut best: Option<(Key, Next)> = None;
    let offer = |k: Key, n: Next, best: &mut Option<_>| {
        if best.as_ref().is_none_or(|(b, _)| k < *b) {
            *best = Some((k, n));
        }
    };
    let room_for_sites = ground.sites() < ask.max_sites;
    // Reach: the ground as it will stand once the placed items due are built. An item cut off
    // gets no site (nothing could fill or use it); its turn goes to the destroy that opens it. A
    // capped item is not placed, so it neither asks for a destroy nor walls anything off.
    reach(ask, ground, s);
    s.cut.clear();
    for item in ask.layout.items.iter().filter(due) {
        census.due += 1;
        let k = key(item.kind, item.level, item.tile);
        let pending =
            !ground.stands(item.kind, item.tile) && ground.site_at(item.tile) != Some(item.kind);
        if pending && capped(ask, ground, item.kind) {
            census.capped += 1;
            continue;
        }
        let reached = item.kind == StructureType::Road
            || item.kind == StructureType::Rampart
            || item
                .tile
                .neighbors()
                .any(|(_, n)| get(&s.reached, n, false))
            || (!blocks(item.kind) && get(&s.reached, item.tile, false));
        if !reached {
            census.cut_off += 1;
            // Opening the way helps only an item its own tile will take (or holds): not one the
            // game refused this global or one something else stands on for good.
            let takes = !ask.skip.contains(&item.tile)
                && tile_state(ground, item.kind, item.tile, destroy) != TileState::Blocked;
            if takes {
                s.cut.push((k, item.tile));
            }
            continue;
        }
        let state = if ask.skip.contains(&item.tile) {
            TileState::Blocked
        } else {
            match tile_state(ground, item.kind, item.tile, destroy) {
                TileState::Clear(k) if item.kind == StructureType::Road && !road_clears(k) => {
                    TileState::Blocked
                }
                s => s,
            }
        };
        match state {
            TileState::Built => census.built += 1,
            TileState::Sited => census.sited += 1,
            TileState::Blocked => census.blocked += 1,
            TileState::Free | TileState::Clear(_) => {
                census.missing += 1;
                if !room_for_sites {
                    continue;
                }
                let n = match state {
                    TileState::Clear(kind) => Next::Destroy {
                        kind,
                        tile: item.tile,
                    },
                    _ => Next::Site {
                        kind: item.kind,
                        tile: item.tile,
                    },
                };
                offer(k, n, &mut best);
            }
        }
    }
    // The first cut-off item, in key order, that some destroy opens: one search from the anchor
    // serves them all, so one item no destroy can open does not hold back the rest. An item at or
    // after the action already chosen could not win, so the search runs only when one could.
    s.cut.sort_unstable();
    let beats = |k: &Key, best: &Option<(Key, Next)>| best.as_ref().is_none_or(|(b, _)| k < b);
    if destroy && room_for_sites && s.cut.first().is_some_and(|(k, _)| beats(k, &best)) {
        open_search(ask, ground, s);
        let mut i = 0;
        while let Some(&(k, target)) = s.cut.get(i) {
            if !beats(&k, &best) {
                break;
            }
            if let Some((kind, tile)) = opening(ground, target, s) {
                offer(k, Next::Destroy { kind, tile }, &mut best);
                break;
            }
            i += 1;
        }
    }
    (best.map_or(Next::Idle, |(_, n)| n), census)
}

/// Whether a creep could stand on `t` once the placed items due are built: inside the room, not a
/// wall, and no obstacle there or planned there (a planned obstacle of a type the room already holds
/// as many of as its level allows will not be built, so it walls nothing off).
fn fill_walk(ask: &Ask<'_>, ground: &Ground, s: &mut Scratch) {
    for i in 0..ROOM_AREA {
        let Some(t) = Tile::from_index(i) else {
            continue;
        };
        let open =
            !t.is_edge() && ask.terrain.kind(t) != TerrainKind::Wall && !ground.obstructed(t);
        set(&mut s.walk, t, open);
    }
    let roles = &ask.layout.roles;
    for item in &ask.layout.items {
        if item.level <= ask.level
            && ask.gates.places(item.kind, item.tile, roles)
            && blocks(item.kind)
            && !capped(ask, ground, item.kind)
        {
            set(&mut s.walk, item.tile, false);
        }
    }
}

/// Fills `s.reached`: the tiles a creep reaches from the anchor's free neighbors.
fn reach(ask: &Ask<'_>, ground: &Ground, s: &mut Scratch) {
    fill_walk(ask, ground, s);
    s.reached.fill(false);
    s.queue.clear();
    for (_, n) in ask.anchor.neighbors() {
        if get(&s.walk, n, false) && !get(&s.reached, n, true) {
            set(&mut s.reached, n, true);
            s.queue.push_back(n);
        }
    }
    while let Some(u) = s.queue.pop_front() {
        for (_, n) in u.neighbors() {
            if get(&s.walk, n, false) && !get(&s.reached, n, true) {
                set(&mut s.reached, n, true);
                s.queue.push_back(n);
            }
        }
    }
}

/// The search [`opening`] reads: a 0-1 search from the anchor's neighbors through walkable tiles
/// (cost 0) and replaceable obstacles that are not the plan's on their tile (cost 1), over the
/// whole room, into `s.cost` and `s.parent`.
fn open_search(ask: &Ask<'_>, ground: &Ground, s: &mut Scratch) {
    s.planned.fill(NONE);
    for i in &ask.layout.items {
        if i.kind != StructureType::Road
            && i.kind != StructureType::Rampart
            && let Some(c) = code(i.kind)
            && get(&s.planned, i.tile, NONE) == NONE
        {
            set(&mut s.planned, i.tile, c);
        }
    }
    let walk = &s.walk;
    let planned = &s.planned;
    let passable = |t: Tile| -> Option<u16> {
        if get(walk, t, false) {
            return Some(0);
        }
        if t.is_edge() || ask.terrain.kind(t) == TerrainKind::Wall {
            return None;
        }
        let k = ground.replaceable(t)?;
        (blocks(k) && decode(get(planned, t, NONE)) != Some(k)).then_some(1)
    };
    s.cost.fill(u16::MAX);
    s.parent.fill(u16::MAX);
    s.queue.clear();
    for (_, n) in ask.anchor.neighbors() {
        let Some(c) = passable(n) else {
            continue;
        };
        if c < get(&s.cost, n, u16::MAX) {
            set(&mut s.cost, n, c);
            if c == 0 {
                s.queue.push_front(n);
            } else {
                s.queue.push_back(n);
            }
        }
    }
    while let Some(u) = s.queue.pop_front() {
        let cu = get(&s.cost, u, u16::MAX);
        for (_, n) in u.neighbors() {
            let Some(c) = passable(n) else {
                continue;
            };
            let total = cu.saturating_add(c);
            if total < get(&s.cost, n, u16::MAX) {
                set(&mut s.cost, n, total);
                set(
                    &mut s.parent,
                    n,
                    u16::try_from(u.index()).unwrap_or(u16::MAX),
                );
                if c == 0 {
                    s.queue.push_front(n);
                } else {
                    s.queue.push_back(n);
                }
            }
        }
    }
}

/// The replaceable own obstacle whose destroy opens the way to `target`, from [`open_search`]: of
/// `target`'s neighbors the cheapest to reach (then the lowest tile), and of the obstacles on its
/// way, the one nearest the anchor. `None` when no destroy opens it (or nothing walls it off).
fn opening(ground: &Ground, target: Tile, s: &Scratch) -> Option<(StructureType, Tile)> {
    let goal = target
        .neighbors()
        .map(|(_, n)| n)
        .filter(|&n| get(&s.cost, n, u16::MAX) != u16::MAX)
        .min_by_key(|&n| (get(&s.cost, n, u16::MAX), n.index()))?;
    if get(&s.cost, goal, u16::MAX) == 0 {
        return None;
    }
    // Walk back to the anchor; the last obstacle met is the one nearest it.
    let mut at = Some(goal);
    let mut found = None;
    let mut guard = 0usize;
    while let Some(t) = at {
        if !get(&s.walk, t, false)
            && let Some(k) = ground.replaceable(t)
        {
            found = Some((k, t));
        }
        let p = get(&s.parent, t, u16::MAX);
        at = Tile::from_index(usize::from(p));
        guard += 1;
        if guard > ROOM_AREA {
            break;
        }
    }
    found
}

/// A site `economy::mining` asks for, by its role ([`role_tile`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// The mining tile's container of the source on this tile.
    MiningContainer(Tile),
    /// The controller container.
    ControllerContainer,
    /// The controller link.
    ControllerLink,
    /// The link of the source on this tile.
    SourceLink(Tile),
}

/// The plan's tile for the role of a site `economy::mining` asks for (ADR-023 to ADR-025 keep
/// their timing; the plan says where), by the role the economy gives it, not by where the
/// economy's own tile is (two sources may share a neighbor, and a source link may stand within
/// `RECEIVER_RANGE` of the controller). `sources` are in the plan's input order. `None` where the
/// plan has no such role, or the source is not one of `sources`.
pub fn role_tile(roles: &Roles, sources: &[Tile], need: Need) -> Option<Tile> {
    let index = |source: Tile| sources.iter().position(|&s| s == source);
    match need {
        Need::MiningContainer(source) => index(source).and_then(|i| roles.mining.get(i).copied()),
        Need::ControllerContainer => roles.controller_container,
        Need::ControllerLink => roles.controller_link,
        Need::SourceLink(source) => {
            index(source).and_then(|i| roles.source_links.get(i).copied().flatten())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PLANNER_MMO;
    use crate::planner::layout::{GATES, Item, MAX_LEVEL, RoadNeed};
    use crate::planner::rules::earliest_level;
    use rustedscreep_consts::{ROOM_SIZE, Terrain};

    fn tile(x: u8, y: u8) -> Tile {
        Tile::new(x, y).expect("in room")
    }

    fn open() -> TerrainGrid {
        let raw = vec![0u8; ROOM_AREA];
        TerrainGrid::from_row_major(&raw).expect("terrain")
    }

    fn item(kind: StructureType, x: u8, y: u8, level: u8) -> Item {
        Item {
            kind,
            tile: tile(x, y),
            level,
        }
    }

    fn layout(items: Vec<Item>) -> Layout {
        let mut l = Layout {
            items,
            roles: Roles::default(),
            needs: Vec::new(),
        };
        l.sort();
        l
    }

    fn ask<'a>(layout: &'a Layout, terrain: &'a TerrainGrid, level: u8) -> Ask<'a> {
        Ask {
            layout,
            terrain,
            anchor: tile(25, 25),
            level,
            gates: Gates::default(),
            replace: true,
            max_sites: 10,
            holding: false,
            skip: &[],
            full: 0,
        }
    }

    fn spawned() -> Ground {
        let mut g = Ground::default();
        g.structure(StructureType::Spawn, tile(25, 25));
        g
    }

    #[test]
    fn extensions_go_before_roads_and_levels_wait() {
        let t = open();
        let l = layout(vec![
            item(StructureType::Road, 26, 26, 1),
            item(StructureType::Extension, 27, 25, 2),
            item(StructureType::Extension, 23, 25, 3),
        ]);
        let g = spawned();
        let mut s = Scratch::default();
        let (n, c) = next(&ask(&l, &t, 2), &g, &mut s);
        assert_eq!(
            n,
            Next::Site {
                kind: StructureType::Extension,
                tile: tile(27, 25)
            }
        );
        assert_eq!(c.due, 2, "the level-3 extension is not due at level 2");
        assert_eq!(c.missing, 2);
    }

    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`): an off-plan extension counts against the
    /// level's limit, so a planned one waits (V238).
    #[test]
    fn off_plan_structures_count_against_the_level() {
        let t = open();
        let lvl = 2;
        let n = allowed(StructureType::Extension, lvl);
        let mut items = Vec::new();
        let mut g = spawned();
        for i in 0..n {
            let x = 20 + u8::try_from(i).expect("small") * 2;
            items.push(item(StructureType::Extension, x, 20, lvl));
            // Each planned slot's twin stands off the plan.
            g.structure(StructureType::Extension, tile(x, 30));
        }
        let l = layout(items);
        let (next_, c) = next(&ask(&l, &t, lvl), &g, &mut Scratch::default());
        assert_eq!(next_, Next::Idle);
        assert_eq!(c.capped, n);
        assert_eq!(c.cut_off, 0);
    }

    #[test]
    fn a_blocker_is_destroyed_only_when_its_tile_is_due_and_placed() {
        let t = open();
        let l = layout(vec![item(StructureType::Storage, 27, 25, 4)]);
        let mut g = spawned();
        g.structure(StructureType::Extension, tile(27, 25));
        let mut s = Scratch::default();
        // The storage's gate is off: nothing is due, nothing is destroyed.
        let (n, c) = next(&ask(&l, &t, 4), &g, &mut s);
        assert_eq!((n, c.due), (Next::Idle, 0));
        let mut a = ask(&l, &t, 4);
        a.gates.storage = true;
        let (n, _) = next(&a, &g, &mut s);
        assert_eq!(
            n,
            Next::Destroy {
                kind: StructureType::Extension,
                tile: tile(27, 25)
            }
        );
        // Not below the storage's level, not with replacing off, not while a replacement is open,
        // not with a hostile creep in the room.
        a.level = 3;
        assert_eq!(next(&a, &g, &mut s).0, Next::Idle);
        a.level = 4;
        a.replace = false;
        assert_eq!(next(&a, &g, &mut s).0, Next::Idle);
        a.replace = true;
        a.holding = true;
        assert_eq!(next(&a, &g, &mut s).0, Next::Idle);
        a.holding = false;
        g.hostile();
        assert_eq!(next(&a, &g, &mut s).0, Next::Idle);
    }

    #[test]
    fn spawns_and_foreign_structures_are_never_destroyed() {
        let t = open();
        let l = layout(vec![
            item(StructureType::Extension, 27, 25, 2),
            item(StructureType::Extension, 23, 25, 2),
        ]);
        let mut g = spawned();
        g.structure(StructureType::Spawn, tile(27, 25));
        g.obstacle(tile(23, 25));
        let (n, c) = next(&ask(&l, &t, 2), &g, &mut Scratch::default());
        assert_eq!(n, Next::Idle);
        assert_eq!(c.blocked, 2);
    }

    /// ADR-026's amendment (2026-11-18): a due road clears an own off-plan extension under it, and
    /// goes over a container; the free road goes first by tile, then the destroy.
    #[test]
    fn a_road_replaces_an_off_plan_extension_and_goes_over_a_container() {
        let t = open();
        let l = layout(vec![
            item(StructureType::Road, 27, 25, 1),
            item(StructureType::Road, 23, 25, 1),
        ]);
        let mut g = spawned();
        g.structure(StructureType::Extension, tile(27, 25));
        g.structure(StructureType::Container, tile(23, 25));
        let mut s = Scratch::default();
        let (n, c) = next(&ask(&l, &t, 1), &g, &mut s);
        assert_eq!(
            n,
            Next::Site {
                kind: StructureType::Road,
                tile: tile(23, 25)
            }
        );
        assert_eq!(
            (c.missing, c.blocked),
            (2, 0),
            "the extension's road is missing, not blocked"
        );
        g.site(StructureType::Road, tile(23, 25));
        let (n, c) = next(&ask(&l, &t, 1), &g, &mut s);
        assert_eq!(
            n,
            Next::Destroy {
                kind: StructureType::Extension,
                tile: tile(27, 25)
            }
        );
        assert_eq!((c.sited, c.missing), (1, 1));
    }

    /// A road's destroy keeps ADR-026's rules: not with replacing off, not while a replacement is
    /// held open, not at the site cap, not with a hostile creep in the room.
    #[test]
    fn a_road_destroy_waits_like_any_other() {
        let t = open();
        let l = layout(vec![item(StructureType::Road, 27, 25, 1)]);
        let mut g = spawned();
        g.structure(StructureType::Extension, tile(27, 25));
        let mut s = Scratch::default();
        let mut a = ask(&l, &t, 1);
        a.replace = false;
        let (n, c) = next(&a, &g, &mut s);
        assert_eq!((n, c.blocked), (Next::Idle, 1));
        a.replace = true;
        a.holding = true;
        assert_eq!(
            next(&a, &g, &mut s),
            (
                Next::Idle,
                Census {
                    due: 1,
                    blocked: 1,
                    ..Census::default()
                }
            )
        );
        a.holding = false;
        a.max_sites = 0;
        let (n, c) = next(&a, &g, &mut s);
        assert_eq!(
            (n, c.missing),
            (Next::Idle, 1),
            "at the cap it waits, counted missing"
        );
        a.max_sites = 10;
        g.hostile();
        let (n, c) = next(&a, &g, &mut s);
        assert_eq!((n, c.blocked), (Next::Idle, 1));
    }

    /// A road clears only an off-plan extension (ADR-026's amendment): a tower, a storage and a
    /// lab stay with their gates on, as do a spawn, one of the economy's links, a structure the
    /// snapshot does not hold and another type's own site.
    #[test]
    fn a_road_clears_only_an_off_plan_extension() {
        let t = open();
        let l = layout(vec![
            item(StructureType::Road, 27, 25, 1),
            item(StructureType::Road, 27, 27, 1),
            item(StructureType::Road, 29, 25, 1),
            item(StructureType::Road, 23, 25, 1),
            item(StructureType::Road, 23, 27, 1),
            item(StructureType::Road, 25, 22, 1),
            item(StructureType::Road, 25, 28, 1),
            item(StructureType::Road, 21, 25, 1),
        ]);
        let mut g = spawned();
        g.structure(StructureType::Tower, tile(27, 25));
        g.structure(StructureType::Storage, tile(27, 27));
        g.structure(StructureType::Lab, tile(29, 25));
        g.structure(StructureType::Link, tile(23, 25));
        g.structure(StructureType::Spawn, tile(23, 27));
        g.obstacle(tile(25, 22));
        g.site(StructureType::Extension, tile(25, 28));
        g.structure(StructureType::Extension, tile(21, 25));
        let mut s = Scratch::default();
        let mut a = ask(&l, &t, 1);
        for gates in [
            Gates::default(),
            Gates {
                storage: true,
                towers: true,
                ramparts: true,
                late: true,
            },
        ] {
            a.gates = gates;
            let (n, c) = next(&a, &g, &mut s);
            assert_eq!(
                n,
                Next::Destroy {
                    kind: StructureType::Extension,
                    tile: tile(21, 25)
                },
                "{gates:?}"
            );
            assert_eq!((c.due, c.blocked, c.missing), (8, 7, 1), "{gates:?}");
        }
    }

    /// A road that stands under an own off-plan extension (one built over it by hand) is not
    /// counted as built: the extension clears for it, and with replacing off the road is blocked.
    #[test]
    fn a_road_standing_under_an_extension_is_cleared_not_counted_built() {
        let t = open();
        let l = layout(vec![item(StructureType::Road, 27, 25, 1)]);
        let mut g = spawned();
        g.road(tile(27, 25));
        g.structure(StructureType::Extension, tile(27, 25));
        let mut s = Scratch::default();
        let (n, c) = next(&ask(&l, &t, 1), &g, &mut s);
        assert_eq!(
            n,
            Next::Destroy {
                kind: StructureType::Extension,
                tile: tile(27, 25)
            }
        );
        assert_eq!((c.built, c.missing), (0, 1));
        let mut a = ask(&l, &t, 1);
        a.replace = false;
        let (n, c) = next(&a, &g, &mut s);
        assert_eq!((n, c.built, c.blocked), (Next::Idle, 0, 1));
        // A plain standing road is built.
        let mut plain = spawned();
        plain.road(tile(27, 25));
        let (_, c) = next(&ask(&l, &t, 1), &plain, &mut s);
        assert_eq!((c.built, c.missing), (1, 0));
    }

    #[test]
    fn the_site_cap_holds_new_sites_back() {
        let t = open();
        let l = layout(vec![item(StructureType::Extension, 27, 25, 2)]);
        let mut g = spawned();
        let mut a = ask(&l, &t, 2);
        a.max_sites = 1;
        g.site(StructureType::Road, tile(10, 10));
        assert_eq!(next(&a, &g, &mut Scratch::default()).0, Next::Idle);
    }

    #[test]
    fn an_off_plan_wall_of_extensions_is_opened_one_at_a_time() {
        let t = open();
        // A planned extension at (30, 25), boxed in by off-plan extensions on every side.
        let target = tile(30, 25);
        let l = layout(vec![Item {
            kind: StructureType::Extension,
            tile: target,
            level: 2,
        }]);
        let mut g = spawned();
        for (_, n) in target.neighbors() {
            g.structure(StructureType::Extension, n);
        }
        let mut a = ask(&l, &t, 8);
        a.level = 8;
        let (n, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(c.cut_off, 1);
        let Next::Destroy { kind, tile: at } = n else {
            panic!("a destroy, got {n:?}");
        };
        assert_eq!(kind, StructureType::Extension);
        assert_eq!(at.range(target), 1, "a neighbour of the boxed-in slot");
        assert_eq!(at.x(), 29, "the one nearest the anchor's side");
    }

    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`). A planned slot the level's count rules
    /// out is not placed, so it asks for no destroy even when off-plan extensions box it in.
    #[test]
    fn a_capped_slot_boxed_in_asks_for_nothing() {
        let t = open();
        let target = tile(30, 25);
        let l = layout(vec![Item {
            kind: StructureType::Extension,
            tile: target,
            level: 2,
        }]);
        let mut g = spawned();
        for (_, n) in target.neighbors() {
            g.structure(StructureType::Extension, n);
        }
        let level = 2;
        assert!(g.count(StructureType::Extension) >= allowed(StructureType::Extension, level));
        let (n, c) = next(&ask(&l, &t, level), &g, &mut Scratch::default());
        assert_eq!(n, Next::Idle);
        assert_eq!((c.capped, c.cut_off), (1, 0), "{c:?}");
    }

    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`). Planned obstacles the level's count rules
    /// out wall nothing off: a built storage ringed by capped extension slots is still reached.
    #[test]
    fn capped_slots_wall_nothing_off() {
        let t = open();
        let storage = tile(30, 25);
        let level = 4;
        let mut items = vec![Item {
            kind: StructureType::Storage,
            tile: storage,
            level,
        }];
        items.extend(storage.neighbors().map(|(_, n)| Item {
            kind: StructureType::Extension,
            tile: n,
            level,
        }));
        let l = layout(items);
        let mut g = spawned();
        g.structure(StructureType::Storage, storage);
        // The level's count of extensions stands elsewhere, off the plan.
        let n = allowed(StructureType::Extension, level);
        for i in 0..n {
            g.structure(
                StructureType::Extension,
                tile(
                    5 + u8::try_from(i % 20).expect("small"),
                    5 + u8::try_from(i / 20).expect("small") * 2,
                ),
            );
        }
        let mut a = ask(&l, &t, level);
        a.gates.storage = true;
        let (next_, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(next_, Next::Idle);
        assert_eq!(c.cut_off, 0, "{c:?}");
        assert_eq!(c.built, 1);
        assert_eq!(c.capped, 8);
    }

    /// Each role maps to the plan's tile for it, whatever the economy's own tile: two sources two
    /// apart (sharing neighbors) each get their own mining container and link, the controller's
    /// roles go to the plan's controller tiles, and a source that is not the plan's gets nothing.
    #[test]
    fn roles_map_to_the_plans_tiles() {
        let sources = [tile(10, 10), tile(12, 10)];
        assert_eq!(
            sources[0].range(sources[1]),
            2,
            "the sources share neighbours"
        );
        let roles = Roles {
            mining: vec![tile(9, 10), tile(13, 10)],
            source_links: vec![Some(tile(8, 10)), None],
            controller_container: Some(tile(25, 38)),
            controller_link: Some(tile(26, 38)),
            ..Roles::default()
        };
        let role = |need| role_tile(&roles, &sources, need);
        assert_eq!(role(Need::MiningContainer(sources[0])), Some(tile(9, 10)));
        assert_eq!(role(Need::MiningContainer(sources[1])), Some(tile(13, 10)));
        assert_eq!(role(Need::ControllerContainer), Some(tile(25, 38)));
        assert_eq!(role(Need::ControllerLink), Some(tile(26, 38)));
        assert_eq!(role(Need::SourceLink(sources[0])), Some(tile(8, 10)));
        assert_eq!(role(Need::SourceLink(sources[1])), None);
        assert_eq!(role(Need::MiningContainer(tile(30, 30))), None);
        assert_eq!(role(Need::SourceLink(tile(30, 30))), None);
        let bare = Roles::default();
        assert_eq!(role_tile(&bare, &sources, Need::ControllerContainer), None);
        assert_eq!(
            role_tile(&bare, &sources, Need::MiningContainer(sources[0])),
            None
        );
    }

    /// Terrain walls on `walls`, plain elsewhere, from a row-major raw buffer as `getRawBuffer`
    /// lays it out (V156).
    ///
    /// Uses: `ROOM_AREA`, `ROOM_SIZE`, the `Terrain::Wall` mask (TERRAIN_MASK_WALL).
    fn walled(walls: &[Tile]) -> TerrainGrid {
        let mut raw = vec![0u8; ROOM_AREA];
        for w in walls {
            raw[usize::from(w.y()) * usize::from(ROOM_SIZE) + usize::from(w.x())] =
                Terrain::Wall as u8;
        }
        TerrainGrid::from_row_major(&raw).expect("terrain")
    }

    fn item_on(kind: StructureType, t: Tile, level: u8) -> Item {
        Item {
            kind,
            tile: t,
            level,
        }
    }

    /// The level the first structure of `kind` is allowed from. Uses: `CONTROLLER_STRUCTURES`.
    fn first(kind: StructureType) -> u8 {
        earliest_level(kind, 0).expect("allowed at some level")
    }

    /// Gates from four bits: storage, towers, ramparts, late.
    fn gates(bits: u8) -> Gates {
        Gates {
            storage: bits & 1 != 0,
            towers: bits & 2 != 0,
            ramparts: bits & 4 != 0,
            late: bits & 8 != 0,
        }
    }

    /// Every gate on.
    const ALL_ON: u8 = 0b1111;

    /// The bit of `kind` in [`Ask::full`].
    fn full_bit(kind: StructureType) -> u32 {
        1u32 << kind_rank(kind)
    }

    /// What a room holds, rebuilt into a fresh [`Ground`] for each look the way
    /// `economy::placement` builds it: movement's footprints first, then the snapshot's own
    /// structures and own sites.
    #[derive(Default)]
    struct World {
        foreign: Vec<Tile>,
        structures: Vec<(StructureType, Tile)>,
        sites: Vec<(StructureType, Tile)>,
    }

    impl World {
        /// The anchor's spawn ([`ask`]'s anchor) and nothing else.
        fn spawned() -> Self {
            Self {
                structures: vec![(StructureType::Spawn, tile(25, 25))],
                ..Self::default()
            }
        }

        fn ground(&self) -> Ground {
            let mut g = Ground::default();
            for &t in &self.foreign {
                g.obstacle(t);
            }
            for &(k, t) in &self.structures {
                g.structure(k, t);
            }
            for &(k, t) in &self.sites {
                g.site(k, t);
            }
            g
        }

        fn count(&self, kind: StructureType) -> usize {
            self.structures.iter().filter(|s| s.0 == kind).count()
                + self.sites.iter().filter(|s| s.0 == kind).count()
        }
    }

    /// The plan's type a destroy on `t` is held open for, as `economy::placement::due_replacement`
    /// reads it with no economy need this tick: of the items there whose gate is on, that are due
    /// at `ask`'s level and that `next` places, a structure, else a road.
    fn planned_kind(ask: &Ask<'_>, t: Tile) -> Option<StructureType> {
        let layout = ask.layout;
        let placed = |i: &&Item| {
            ask.gates.places(i.kind, i.tile, &layout.roles)
                && ask
                    .gates
                    .due_level(layout, i)
                    .is_some_and(|l| l <= ask.level)
        };
        let at = || layout.items.iter().filter(|i| i.tile == t).filter(placed);
        at().find(|i| i.kind != StructureType::Road && i.kind != StructureType::Rampart)
            .or_else(|| at().find(|i| i.kind == StructureType::Road))
            .map(|i| i.kind)
    }

    /// Plays [`next`] to `Idle` the way `economy::placement` does: a destroy takes its structure
    /// off the ground, and while the destroyed tile's planned replacement is neither sited nor
    /// built the look runs with `holding` (no further destroy); each site is built at once. The
    /// actions, in order.
    fn play(ask: &Ask<'_>, world: &mut World) -> Vec<Next> {
        let mut s = Scratch::default();
        let mut out = Vec::new();
        let mut replacing: Option<(StructureType, Tile)> = None;
        for _ in 0..ROOM_AREA {
            let g = world.ground();
            if let Some((k, t)) = replacing
                && (g.stands(k, t) || world.sites.contains(&(k, t)))
            {
                replacing = None;
            }
            let a = Ask {
                holding: replacing.is_some(),
                ..*ask
            };
            let (n, _) = next(&a, &g, &mut s);
            match n {
                Next::Idle => return out,
                Next::Site { kind, tile } => world.structures.push((kind, tile)),
                Next::Destroy { kind, tile } => {
                    assert!(
                        replacing.is_none(),
                        "a destroy while {replacing:?} is still open: {out:?}"
                    );
                    let at = world
                        .structures
                        .iter()
                        .position(|&s| s == (kind, tile))
                        .expect("the destroyed structure stands");
                    world.structures.remove(at);
                    replacing = planned_kind(ask, tile).map(|k| (k, tile));
                }
            }
            out.push(n);
        }
        panic!("next never went idle: {out:?}");
    }

    /// C14: the economy's containers and links (the mining tiles', the controller's and the
    /// sources') never come out of [`next`], under any of the 16 gate combinations at any level;
    /// the hub link only with the storage gate on, the mineral container only with the late gate
    /// on, each from its level.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`: each container and link at a level
    /// that allows it, the mineral container with the extractor), `MAX_LEVEL`.
    #[test]
    fn the_economys_containers_and_links_never_come_from_next() {
        let t = open();
        let mining = [tile(10, 10), tile(40, 10)];
        let source_links = [tile(11, 11), tile(41, 11)];
        let controller_container = tile(25, 40);
        let controller_link = tile(27, 41);
        let hub_link = tile(24, 27);
        let mineral_container = tile(40, 40);
        let container_level =
            |n| earliest_level(StructureType::Container, n).expect("containers are allowed");
        let link_level = |n| earliest_level(StructureType::Link, n).expect("links are allowed");
        let hub_level = link_level(0);
        let mineral_level = first(StructureType::Extractor);
        let mut l = layout(vec![
            item_on(StructureType::Container, mining[0], container_level(0)),
            item_on(StructureType::Container, mining[1], container_level(1)),
            item_on(
                StructureType::Container,
                controller_container,
                container_level(2),
            ),
            item_on(StructureType::Container, mineral_container, mineral_level),
            item_on(StructureType::Link, hub_link, hub_level),
            item_on(StructureType::Link, controller_link, link_level(1)),
            item_on(StructureType::Link, source_links[0], link_level(2)),
            item_on(StructureType::Link, source_links[1], link_level(3)),
        ]);
        l.roles = Roles {
            mining: mining.to_vec(),
            source_links: source_links.iter().copied().map(Some).collect(),
            controller_container: Some(controller_container),
            controller_link: Some(controller_link),
            hub_link: Some(hub_link),
            mineral_container: Some(mineral_container),
            ..Roles::default()
        };
        // Every item at a level that allows as many of its type as the plan holds by then.
        for i in &l.items {
            assert!(
                l.count(i.kind, i.level) <= allowed(i.kind, i.level),
                "{i:?} is not allowed at its level"
            );
        }
        let economy = [
            mining[0],
            mining[1],
            source_links[0],
            source_links[1],
            controller_container,
            controller_link,
        ];
        for bits in 0..=ALL_ON {
            let g = gates(bits);
            for level in 1..=MAX_LEVEL {
                let mut a = ask(&l, &t, level);
                a.gates = g;
                // In key order: containers before links.
                let mut want = Vec::new();
                if g.late && level >= mineral_level {
                    want.push(Next::Site {
                        kind: StructureType::Container,
                        tile: mineral_container,
                    });
                }
                if g.storage && level >= hub_level {
                    want.push(Next::Site {
                        kind: StructureType::Link,
                        tile: hub_link,
                    });
                }
                let (_, c) = next(&a, &World::spawned().ground(), &mut Scratch::default());
                assert_eq!(
                    c.due as usize,
                    want.len(),
                    "{g:?} at level {level}: only the hub link and the mineral container are due"
                );
                let played = play(&a, &mut World::spawned());
                for n in &played {
                    if let Next::Site { tile: at, .. } | Next::Destroy { tile: at, .. } = n {
                        assert!(
                            !economy.contains(at),
                            "{g:?} at level {level}: {n:?} is the economy's"
                        );
                    }
                }
                assert_eq!(played, want, "{g:?} at level {level}");
            }
        }
    }

    /// C14: under the MMO profile's gates no storage, rampart, wall or late structure (terminal,
    /// extractor and mineral container, lab, factory, power spawn, nuker, observer) is due at any
    /// level, so none is counted or offered; the tower is due, and offered, from the first level
    /// that allows it (P7.2 turned the towers' gate on, ADR-026); with every gate on, each is due
    /// at the top level.
    ///
    /// Uses: `PLANNER_MMO` (its gates), `CONTROLLER_STRUCTURES` (through `earliest_level`: each
    /// at the first level that allows it), `MAX_LEVEL`.
    #[test]
    fn the_mmo_gates_leave_out_storage_ramparts_and_late_structures() {
        let t = open();
        let kinds = [
            StructureType::Storage,
            StructureType::Tower,
            StructureType::Terminal,
            StructureType::Extractor,
            StructureType::Lab,
            StructureType::Factory,
            StructureType::PowerSpawn,
            StructureType::Nuker,
            StructureType::Observer,
            StructureType::Rampart,
            StructureType::Wall,
        ];
        let mut items: Vec<Item> = kinds
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                let x = 10 + 3 * u8::try_from(i).expect("small");
                item_on(k, tile(x, 10), first(k))
            })
            .collect();
        let mineral_container = tile(10, 20);
        items.push(item_on(
            StructureType::Container,
            mineral_container,
            first(StructureType::Extractor),
        ));
        let total = items.len();
        let mut l = layout(items);
        l.roles.mineral_container = Some(mineral_container);
        for i in &l.items {
            assert_ne!(Gate::of(i.kind, i.tile, &l.roles), Gate::Base, "{i:?}");
        }
        let g = World::spawned().ground();
        let mut s = Scratch::default();
        for level in 1..=MAX_LEVEL {
            let mut a = ask(&l, &t, level);
            a.gates = Gates::from_config(&PLANNER_MMO);
            let (n, c) = next(&a, &g, &mut s);
            let tower = level >= first(StructureType::Tower);
            assert_eq!(c.due, u32::from(tower), "level {level}: {c:?}");
            if tower {
                assert!(
                    matches!(
                        n,
                        Next::Site {
                            kind: StructureType::Tower,
                            ..
                        }
                    ),
                    "level {level}: {n:?}"
                );
            } else {
                assert_eq!(n, Next::Idle, "level {level}");
            }
        }
        let mut a = ask(&l, &t, MAX_LEVEL);
        a.gates = gates(ALL_ON);
        let (_, c) = next(&a, &g, &mut s);
        assert_eq!(c.due as usize, total, "{c:?}");
    }

    /// C15: a planned slot whose side toward the anchor is the plan's own built extensions and
    /// whose other sides are walled `depth` deep by off-plan extensions. Played the way
    /// `economy::placement` plays it, only off-plan extensions are destroyed, as many as the wall
    /// is deep on the cheapest way, each layer from the outside (nearest the anchor) in and each
    /// next to the last, and the slot is sited at the end.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`, `earliest_level`: the room stays under
    /// the top level's extension count), `MAX_LEVEL`.
    #[test]
    fn a_deep_off_plan_wall_is_opened_from_the_anchors_side() {
        let t = open();
        let target = tile(32, 25);
        let on_plan: Vec<Tile> = (23..=27).map(|y| tile(31, y)).collect();
        let inner: Vec<Tile> = target
            .neighbors()
            .map(|(_, n)| n)
            .filter(|n| !on_plan.contains(n))
            .collect();
        for depth in 1..=3usize {
            // Layer k: the tiles next to layer k - 1 (the slot's free neighbours for the first)
            // that are in no earlier layer, the slot or the plan's extensions. A way in from
            // outside crosses every layer.
            let mut seen: Vec<Tile> = on_plan.iter().chain(&inner).copied().collect();
            seen.push(target);
            let mut layers: Vec<Vec<Tile>> = Vec::new();
            let mut front = inner.clone();
            for _ in 0..depth {
                let mut ring = Vec::new();
                for f in &front {
                    for (_, n) in f.neighbors() {
                        if !seen.contains(&n) {
                            seen.push(n);
                            ring.push(n);
                        }
                    }
                }
                layers.push(ring.clone());
                front = ring;
            }
            let wall: Vec<Tile> = layers.iter().flatten().copied().collect();
            let mut l = layout(
                on_plan
                    .iter()
                    .map(|&p| item_on(StructureType::Extension, p, first(StructureType::Extension)))
                    .collect(),
            );
            l.items.push(item_on(
                StructureType::Extension,
                target,
                first(StructureType::Extension),
            ));
            l.sort();
            let mut w = World::spawned();
            w.structures
                .extend(on_plan.iter().map(|&p| (StructureType::Extension, p)));
            w.structures
                .extend(wall.iter().map(|&o| (StructureType::Extension, o)));
            let room = u32::try_from(w.count(StructureType::Extension) + 1).expect("small");
            assert!(room <= allowed(StructureType::Extension, MAX_LEVEL));
            let a = ask(&l, &t, MAX_LEVEL);
            let (_, c) = next(&a, &w.ground(), &mut Scratch::default());
            assert_eq!(c.cut_off, 1, "depth {depth}: {c:?}");
            assert_eq!(c.built as usize, on_plan.len(), "depth {depth}: {c:?}");
            let played = play(&a, &mut w);
            let destroys: Vec<Tile> = played
                .iter()
                .filter_map(|n| match *n {
                    Next::Destroy { kind, tile: at } => {
                        assert_eq!(kind, StructureType::Extension);
                        Some(at)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(destroys.len(), depth, "depth {depth}: {played:?}");
            for (i, d) in destroys.iter().enumerate() {
                assert!(wall.contains(d), "depth {depth}: {d:?} is the plan's own");
                assert!(
                    layers[depth - 1 - i].contains(d),
                    "depth {depth}: destroy {i} at {d:?} is not in layer {}",
                    depth - i
                );
                if i > 0 {
                    assert_eq!(destroys[i - 1].range(*d), 1, "depth {depth}: {destroys:?}");
                }
            }
            assert_eq!(
                played.last(),
                Some(&Next::Site {
                    kind: StructureType::Extension,
                    tile: target
                }),
                "depth {depth}: {played:?}"
            );
            assert_eq!(played.len(), depth + 1, "depth {depth}: {played:?}");
            for p in &on_plan {
                assert!(w.structures.contains(&(StructureType::Extension, *p)));
            }
        }
    }

    /// C15: of the off-plan obstacles on the way to a cut-off slot, the one nearest the anchor is
    /// destroyed first, not the one nearest the slot: a corridor through terrain walls with two
    /// off-plan extensions in it.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`), the `Terrain::Wall` mask.
    #[test]
    fn the_obstacle_nearest_the_anchor_goes_first() {
        // Walls over x 27..=37, y 23..=27, but a corridor along y 25 from x 27 to the slot at 35.
        let mut walls = Vec::new();
        for x in 27..=37 {
            for y in 23..=27 {
                if !(y == 25 && x <= 35) {
                    walls.push(tile(x, y));
                }
            }
        }
        let t = walled(&walls);
        let target = tile(35, 25);
        let near_anchor = tile(28, 25);
        let near_target = tile(33, 25);
        let l = layout(vec![item_on(
            StructureType::Extension,
            target,
            first(StructureType::Extension),
        )]);
        let mut w = World::spawned();
        w.structures.push((StructureType::Extension, near_target));
        w.structures.push((StructureType::Extension, near_anchor));
        let played = play(&ask(&l, &t, MAX_LEVEL), &mut w);
        assert_eq!(
            played,
            vec![
                Next::Destroy {
                    kind: StructureType::Extension,
                    tile: near_anchor
                },
                Next::Destroy {
                    kind: StructureType::Extension,
                    tile: near_target
                },
                Next::Site {
                    kind: StructureType::Extension,
                    tile: target
                },
            ]
        );
    }

    /// C16: the site cap holds destroys back too (the replacement could not be sited): at
    /// `max_sites` own sites neither a due item's off-plan blocker nor a cut-off item's opening is
    /// destroyed; with one site fewer both are.
    ///
    /// Uses: `PLANNER_MMO.max_sites`, `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn the_site_cap_holds_destroys_back() {
        let t = open();
        let cap = PLANNER_MMO.max_sites;
        assert!(cap >= 1);
        let far = |i: u32| {
            tile(
                5 + u8::try_from(i % 40).expect("small"),
                45 - u8::try_from(i / 40).expect("small"),
            )
        };
        // A due storage on an off-plan extension's tile.
        let storage = tile(27, 25);
        let level = first(StructureType::Storage);
        let l = layout(vec![item_on(StructureType::Storage, storage, level)]);
        for sites in [cap, cap - 1] {
            let mut w = World::spawned();
            w.structures.push((StructureType::Extension, storage));
            w.sites
                .extend((0..sites).map(|i| (StructureType::Road, far(i))));
            let g = w.ground();
            assert_eq!(g.sites(), sites);
            assert_eq!(
                tile_state(&g, StructureType::Storage, storage, true),
                TileState::Clear(StructureType::Extension)
            );
            let mut a = ask(&l, &t, level);
            a.gates.storage = true;
            a.max_sites = cap;
            let (n, c) = next(&a, &g, &mut Scratch::default());
            assert_eq!(c.missing, 1, "{sites} sites: {c:?}");
            let want = if sites < cap {
                Next::Destroy {
                    kind: StructureType::Extension,
                    tile: storage,
                }
            } else {
                Next::Idle
            };
            assert_eq!(n, want, "{sites} sites");
        }
        // A planned extension boxed in by off-plan ones.
        let target = tile(30, 25);
        let l = layout(vec![item_on(
            StructureType::Extension,
            target,
            first(StructureType::Extension),
        )]);
        for sites in [cap, cap - 1] {
            let mut w = World::spawned();
            w.structures.extend(
                target
                    .neighbors()
                    .map(|(_, n)| (StructureType::Extension, n)),
            );
            w.sites
                .extend((0..sites).map(|i| (StructureType::Road, far(i))));
            let g = w.ground();
            assert_eq!(g.sites(), sites);
            let mut a = ask(&l, &t, MAX_LEVEL);
            a.max_sites = cap;
            let (n, c) = next(&a, &g, &mut Scratch::default());
            assert_eq!(c.cut_off, 1, "{sites} sites: {c:?}");
            if sites < cap {
                assert!(
                    matches!(n, Next::Destroy { kind: StructureType::Extension, tile: at }
                        if at.range(target) == 1),
                    "{sites} sites: {n:?}"
                );
            } else {
                assert_eq!(n, Next::Idle, "{sites} sites");
            }
        }
    }

    /// X0: own sites count toward the level's count like structures (V238): with the level's
    /// count of extensions less one standing and one own extension site elsewhere, a free planned
    /// slot is capped and nothing is offered; without the site the slot is offered.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`, `earliest_level`).
    #[test]
    fn own_sites_count_toward_the_levels_limit() {
        let t = open();
        let level = first(StructureType::Extension);
        let n = allowed(StructureType::Extension, level);
        assert!(n >= 1);
        let slot = tile(27, 25);
        let l = layout(vec![item_on(StructureType::Extension, slot, level)]);
        let mut w = World::spawned();
        for i in 0..n - 1 {
            let x = 5 + 2 * u8::try_from(i).expect("small");
            w.structures.push((StructureType::Extension, tile(x, 40)));
        }
        let a = ask(&l, &t, level);
        let (free, c) = next(&a, &w.ground(), &mut Scratch::default());
        assert_eq!(
            free,
            Next::Site {
                kind: StructureType::Extension,
                tile: slot
            }
        );
        assert_eq!(c.capped, 0);
        w.sites.push((StructureType::Extension, tile(5, 44)));
        let g = w.ground();
        assert_eq!(g.count(StructureType::Extension), n);
        let (held, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(held, Next::Idle);
        assert_eq!((c.due, c.capped), (1, 1), "{c:?}");
    }

    /// X0: any own site on a planned tile refuses a site of another type there (V237): a road site
    /// on a planned extension's tile leaves the extension blocked, destroying allowed or not.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`).
    #[test]
    fn a_road_site_on_a_planned_tile_blocks_it() {
        let t = open();
        let level = first(StructureType::Extension);
        let slot = tile(27, 25);
        let l = layout(vec![item_on(StructureType::Extension, slot, level)]);
        let mut w = World::spawned();
        w.sites.push((StructureType::Road, slot));
        let g = w.ground();
        for destroy in [true, false] {
            assert_eq!(
                tile_state(&g, StructureType::Extension, slot, destroy),
                TileState::Blocked
            );
        }
        let (n, c) = next(&ask(&l, &t, level), &g, &mut Scratch::default());
        assert_eq!(n, Next::Idle);
        assert_eq!((c.due, c.blocked), (1, 1), "{c:?}");
    }

    /// X0: an own site of an obstacle type blocks reach like the structure would: a planned slot
    /// ringed by own extension sites is cut off (and nothing destroys a site), as it is when
    /// ringed by off-plan extensions (which a destroy opens); ringed by road sites it is reached.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`, `earliest_level`), `MAX_LEVEL`.
    #[test]
    fn own_obstacle_sites_wall_off_like_structures() {
        let t = open();
        let target = tile(30, 25);
        let l = layout(vec![item_on(
            StructureType::Extension,
            target,
            first(StructureType::Extension),
        )]);
        let ring: Vec<Tile> = target.neighbors().map(|(_, n)| n).collect();
        let a = ask(&l, &t, MAX_LEVEL);
        let ring_len = u32::try_from(ring.len()).expect("small");
        assert!(ring_len < a.max_sites);
        assert!(ring_len < allowed(StructureType::Extension, MAX_LEVEL));
        // Road sites: not obstacles.
        let mut w = World::spawned();
        w.sites
            .extend(ring.iter().map(|&n| (StructureType::Road, n)));
        let (n, c) = next(&a, &w.ground(), &mut Scratch::default());
        assert_eq!(
            n,
            Next::Site {
                kind: StructureType::Extension,
                tile: target
            }
        );
        assert_eq!(c.cut_off, 0);
        // Extension sites: cut off, and no destroy opens it.
        let mut w = World::spawned();
        w.sites
            .extend(ring.iter().map(|&n| (StructureType::Extension, n)));
        assert!(blocks(StructureType::Extension));
        let (n, c) = next(&a, &w.ground(), &mut Scratch::default());
        assert_eq!(n, Next::Idle);
        assert_eq!(c.cut_off, 1, "{c:?}");
        // Extensions standing: cut off the same way; a destroy opens it.
        let mut w = World::spawned();
        w.structures
            .extend(ring.iter().map(|&n| (StructureType::Extension, n)));
        let (n, c) = next(&a, &w.ground(), &mut Scratch::default());
        assert_eq!(c.cut_off, 1, "{c:?}");
        assert!(
            matches!(n, Next::Destroy { kind: StructureType::Extension, tile: at }
                if ring.contains(&at)),
            "{n:?}"
        );
    }

    /// C4: a cut-off item no destroy can open (walled by obstacles of unknown owner and a spawn)
    /// does not hold back the next one in key order: the destroy of the one own off-plan
    /// extension that walls off the second is offered.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`), the `Terrain::Wall` mask,
    /// `MAX_LEVEL`.
    #[test]
    fn a_cut_off_item_no_destroy_opens_does_not_hold_back_the_next() {
        let shut = tile(15, 25);
        let pocket = tile(35, 25);
        let door = tile(34, 25);
        // The second slot sits in a pocket of terrain walls whose only way in is `door`.
        let walls: Vec<Tile> = pocket
            .neighbors()
            .map(|(_, n)| n)
            .filter(|&n| n != door)
            .collect();
        let t = walled(&walls);
        let level = first(StructureType::Extension);
        let l = layout(vec![
            item_on(StructureType::Extension, shut, level),
            item_on(StructureType::Extension, pocket, level),
        ]);
        assert!(
            key_of(shut, level) < key_of(pocket, level),
            "the walled-shut slot is first in key order"
        );
        let mut w = World::spawned();
        let spawn = tile(16, 25);
        w.structures.push((StructureType::Spawn, spawn));
        w.foreign
            .extend(shut.neighbors().map(|(_, n)| n).filter(|&n| n != spawn));
        w.structures.push((StructureType::Extension, door));
        let (n, c) = next(
            &ask(&l, &t, MAX_LEVEL),
            &w.ground(),
            &mut Scratch::default(),
        );
        assert_eq!(c.cut_off, 2, "{c:?}");
        assert_eq!(
            n,
            Next::Destroy {
                kind: StructureType::Extension,
                tile: door
            }
        );
    }

    /// A cut-off item whose own tile could never take its site gets no opening destroy, even when
    /// one would reach it and an unopenable item comes before it: the second slot of the C4 case,
    /// its tile held by an obstacle of unknown owner (and, in a second look, refused by the game
    /// this global, `Ask::skip`), is still walled off by an own off-plan extension, and nothing is
    /// destroyed. With the tile free the destroy is offered (the C4 case itself).
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`), the `Terrain::Wall` mask,
    /// `MAX_LEVEL`.
    #[test]
    fn a_cut_off_item_its_tile_will_not_take_gets_no_opening() {
        let shut = tile(15, 25);
        let pocket = tile(35, 25);
        let door = tile(34, 25);
        let walls: Vec<Tile> = pocket
            .neighbors()
            .map(|(_, n)| n)
            .filter(|&n| n != door)
            .collect();
        let t = walled(&walls);
        let level = first(StructureType::Extension);
        let l = layout(vec![
            item_on(StructureType::Extension, shut, level),
            item_on(StructureType::Extension, pocket, level),
        ]);
        let mut w = World::spawned();
        let spawn = tile(16, 25);
        w.structures.push((StructureType::Spawn, spawn));
        w.foreign
            .extend(shut.neighbors().map(|(_, n)| n).filter(|&n| n != spawn));
        w.structures.push((StructureType::Extension, door));
        // The pocket's own tile held by something no destroy removes.
        let mut held = World::spawned();
        held.structures = w.structures.clone();
        held.foreign = w.foreign.clone();
        held.foreign.push(pocket);
        let (n, c) = next(
            &ask(&l, &t, MAX_LEVEL),
            &held.ground(),
            &mut Scratch::default(),
        );
        assert_eq!(c.cut_off, 2, "{c:?}");
        assert_eq!(n, Next::Idle, "held: {c:?}");
        // The pocket's tile refused by the game this global.
        let skip = [pocket];
        let mut a = ask(&l, &t, MAX_LEVEL);
        a.skip = &skip;
        let (n, c) = next(&a, &w.ground(), &mut Scratch::default());
        assert_eq!(c.cut_off, 2, "{c:?}");
        assert_eq!(n, Next::Idle, "skipped: {c:?}");
        // Free and not refused: the opening goes out.
        let (n, _) = next(
            &ask(&l, &t, MAX_LEVEL),
            &w.ground(),
            &mut Scratch::default(),
        );
        assert_eq!(
            n,
            Next::Destroy {
                kind: StructureType::Extension,
                tile: door
            }
        );
    }

    fn key_of(t: Tile, level: u8) -> Key {
        (kind_rank(StructureType::Extension), level, t.index())
    }

    /// C2: a type the game refused for the level's count ([`Ask::full`]) is capped: a free due
    /// extension is not offered while a due road is; with no bit set the extension goes first.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `earliest_level`), `kind_rank` (the bit).
    #[test]
    fn a_full_type_is_capped_and_other_types_go_on() {
        let t = open();
        let slot = tile(27, 25);
        let road = tile(23, 25);
        let level = first(StructureType::Extension);
        let l = layout(vec![
            item_on(StructureType::Extension, slot, level),
            item_on(StructureType::Road, road, first(StructureType::Road)),
        ]);
        let g = World::spawned().ground();
        let mut a = ask(&l, &t, level);
        a.full = full_bit(StructureType::Extension);
        let (n, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(
            n,
            Next::Site {
                kind: StructureType::Road,
                tile: road
            }
        );
        assert_eq!((c.due, c.capped), (2, 1), "{c:?}");
        a.full = 0;
        let (n, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(
            n,
            Next::Site {
                kind: StructureType::Extension,
                tile: slot
            }
        );
        assert_eq!(c.capped, 0);
    }

    /// C2: planned obstacles of a type held by [`Ask::full`] will not be built, so they wall
    /// nothing off: a built storage ringed by extension slots is reached with the bit set, and
    /// cut off by the ring without it.
    ///
    /// Uses: `CONTROLLER_STRUCTURES` (through `allowed`, `earliest_level`), `kind_rank`.
    #[test]
    fn planned_obstacles_of_a_full_type_wall_nothing_off() {
        let t = open();
        let storage = tile(30, 25);
        let level = first(StructureType::Storage);
        let ring: Vec<Tile> = storage.neighbors().map(|(_, n)| n).collect();
        let ring_len = u32::try_from(ring.len()).expect("small");
        assert!(ring_len <= allowed(StructureType::Extension, level));
        let mut items = vec![item_on(StructureType::Storage, storage, level)];
        items.extend(
            ring.iter()
                .map(|&n| item_on(StructureType::Extension, n, first(StructureType::Extension))),
        );
        let l = layout(items);
        let mut w = World::spawned();
        w.structures.push((StructureType::Storage, storage));
        let g = w.ground();
        let mut a = ask(&l, &t, level);
        a.gates.storage = true;
        a.full = full_bit(StructureType::Extension);
        let (n, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(n, Next::Idle);
        assert_eq!((c.built, c.capped, c.cut_off), (1, ring_len, 0), "{c:?}");
        a.full = 0;
        let (n, c) = next(&a, &g, &mut Scratch::default());
        assert_eq!(c.cut_off, 1, "the ring walls the storage off: {c:?}");
        let lowest = ring.iter().copied().min_by_key(|r| r.index());
        assert_eq!(
            Some(n),
            lowest.map(|at| Next::Site {
                kind: StructureType::Extension,
                tile: at
            })
        );
    }

    /// A road need serving `serves` (gate, level) pairs.
    fn need(t: Tile, serves: &[(Gate, u8)]) -> RoadNeed {
        let mut from = [u8::MAX; GATES];
        for &(g, l) in serves {
            from[g.index()] = l;
        }
        RoadNeed { tile: t, from }
    }

    /// C13: a road is due from the least level of what it serves whose gate is on
    /// ([`Gates::due_level`], `Layout::needs`): one serving only a late structure is due with the
    /// late gate on and never under the MMO profile's gates; one serving the base at a later level
    /// and a late structure at an earlier one is due from the base's level with the late gate
    /// off and from the late one's with it on; one with no recorded need is due from its item's
    /// level. [`next`] offers exactly the due ones, at every level.
    ///
    /// Uses: `PLANNER_MMO` (its gates), `CONTROLLER_STRUCTURES` (through `earliest_level`: the
    /// extractor's, the first lab's, the third spawn's and the first spawn's levels), `MAX_LEVEL`.
    #[test]
    fn roads_are_due_from_what_they_serve() {
        let t = open();
        let mmo = Gates::from_config(&PLANNER_MMO);
        assert!(!mmo.late, "the MMO profile places no late structures");
        let all = gates(ALL_ON);
        let mineral = first(StructureType::Extractor);
        let lab = first(StructureType::Lab);
        let third_spawn = earliest_level(StructureType::Spawn, 2).expect("a third spawn");
        assert!(lab < third_spawn, "the late need comes first here");
        let core = first(StructureType::Spawn);
        let mineral_road = tile(20, 20);
        let shared_road = tile(21, 22);
        let core_road = tile(22, 24);
        let mut l = layout(vec![
            item_on(StructureType::Road, mineral_road, mineral),
            item_on(StructureType::Road, shared_road, lab.min(third_spawn)),
            item_on(StructureType::Road, core_road, core),
        ]);
        l.needs = vec![
            need(mineral_road, &[(Gate::Late, mineral)]),
            need(shared_road, &[(Gate::Base, third_spawn), (Gate::Late, lab)]),
        ];
        l.needs.sort_by_key(|n| n.tile.index());
        assert!(l.road_need(core_road).is_none());
        // (road, due level under the MMO gates, due level with every gate on)
        let expect = [
            (mineral_road, None, Some(mineral)),
            (shared_road, Some(third_spawn), Some(lab)),
            (core_road, Some(core), Some(core)),
        ];
        let road = |at: Tile| {
            *l.items
                .iter()
                .find(|i| i.tile == at)
                .expect("a planned road")
        };
        for &(at, off, on) in &expect {
            assert_eq!(
                mmo.due_level(&l, &road(at)),
                off,
                "{at:?} under the MMO gates"
            );
            assert_eq!(
                all.due_level(&l, &road(at)),
                on,
                "{at:?} with every gate on"
            );
        }
        for (g, late) in [(mmo, false), (all, true)] {
            for level in 1..=MAX_LEVEL {
                let mut want: Vec<Tile> = expect
                    .iter()
                    .filter(|&&(_, off, on)| {
                        (if late { on } else { off }).is_some_and(|d| d <= level)
                    })
                    .map(|&(at, _, _)| at)
                    .collect();
                want.sort_by_key(|w| w.index());
                let mut a = ask(&l, &t, level);
                a.gates = g;
                let (_, c) = next(&a, &World::spawned().ground(), &mut Scratch::default());
                assert_eq!(c.due as usize, want.len(), "{g:?} at level {level}");
                let mut got: Vec<Tile> = play(&a, &mut World::spawned())
                    .into_iter()
                    .map(|n| match n {
                        Next::Site {
                            kind: StructureType::Road,
                            tile: at,
                        } => at,
                        other => panic!("only roads here, got {other:?}"),
                    })
                    .collect();
                got.sort_by_key(|w| w.index());
                assert_eq!(got, want, "{g:?} at level {level}");
            }
        }
    }
}
