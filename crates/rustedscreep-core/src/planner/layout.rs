//! What every planner takes and gives: a room's fixed features in, and out every structure, road and
//! rampart with the controller level from which it belongs, plus the tiles the economy reads by
//! position (mining tiles, the containers' and links' roles; `economy::mining` classifies links and
//! containers by where they stand).

use rustedscreep_consts::StructureType;

use crate::movement::{TerrainGrid, Tile};

/// The highest controller level: the first level with no progress to a next one
/// (`controller_levels` is `None` there, V131).
pub const MAX_LEVEL: u8 = 8;

/// A room as a planner sees it.
pub struct PlanInput<'a> {
    pub terrain: &'a TerrainGrid,
    pub controller: Tile,
    /// The room's sources in a fixed order (the corpus and the snapshot sort them by (x, y)).
    pub sources: &'a [Tile],
    pub mineral: Option<Tile>,
    /// A spawn that already stands (a live room). `None` on a fresh room, where the planner places
    /// the first spawn itself.
    pub spawn: Option<Tile>,
}

/// One planned structure, road or rampart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: StructureType,
    pub tile: Tile,
    /// The controller level from which it belongs in the room (1 to [`MAX_LEVEL`]).
    pub level: u8,
}

/// The tiles whose role the economy infers from position (`economy::mining`, P3.1 to P3.4), so a
/// plan states them and the validator checks they read as meant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    /// Where the base's filler stands, next to the storage, the hub link and a spawn.
    pub hub: Option<Tile>,
    pub storage: Option<Tile>,
    pub hub_link: Option<Tile>,
    /// Per input source, in the same order: the tile its miner stands on, under a container.
    pub mining: Vec<Tile>,
    /// Per input source, in the same order: its link, next to the mining tile.
    pub source_links: Vec<Option<Tile>>,
    /// The container upgraders and haulers share, within `RECEIVER_RANGE` of the controller.
    pub controller_container: Option<Tile>,
    pub controller_link: Option<Tile>,
    pub mineral_container: Option<Tile>,
}

/// What the bot uses a planned structure for, by the planner's switch that places it
/// (`PlannerConfig`, `planner::build::Gates`; the user's choice, 2026-11-18: only what the bot
/// uses is built). A road serves the structures it was routed to, so it is needed only from the
/// earliest level one of them is placed ([`RoadNeed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gate {
    /// Always built: spawns, extensions, the economy's containers and links, roads.
    Base,
    /// The storage and the hub link.
    Storage,
    Towers,
    /// Ramparts and walls.
    Ramparts,
    /// The factory, the power spawn, and the nuker.
    Late,
    /// The observer (P8.1, ADR-039: split from [`Gate::Late`], since the atlas uses it).
    Observer,
    /// The terminal, the extractor and the mineral container (P9.1, ADR-044: split from
    /// [`Gate::Late`], since the mineral miner and its hauler use them).
    Minerals,
    /// The labs (P9.2, ADR-045: split from [`Gate::Late`], since lab reactions use them; placed
    /// only in a room that wants them, `economy::labs::Labs::wants_labs`).
    Labs,
}

/// How many [`Gate`]s there are.
pub const GATES: usize = 8;

impl Gate {
    /// Every gate, in index order.
    pub const ALL: [Self; GATES] = [
        Self::Base,
        Self::Storage,
        Self::Towers,
        Self::Ramparts,
        Self::Late,
        Self::Observer,
        Self::Minerals,
        Self::Labs,
    ];

    /// The gate of the plan's `kind` on `tile` (a container or a link is the economy's but for the
    /// mineral container and the hub link).
    pub fn of(kind: StructureType, tile: Tile, roles: &Roles) -> Self {
        match kind {
            StructureType::Spawn | StructureType::Extension | StructureType::Road => Self::Base,
            StructureType::Container if roles.mineral_container == Some(tile) => Self::Minerals,
            StructureType::Container => Self::Base,
            StructureType::Link if roles.hub_link == Some(tile) => Self::Storage,
            StructureType::Link => Self::Base,
            StructureType::Storage => Self::Storage,
            StructureType::Tower => Self::Towers,
            StructureType::Rampart | StructureType::Wall => Self::Ramparts,
            StructureType::Observer => Self::Observer,
            StructureType::Terminal | StructureType::Extractor => Self::Minerals,
            StructureType::Lab => Self::Labs,
            _ => Self::Late,
        }
    }

    /// Its index in [`RoadNeed::from`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Why a planned road is there: per [`Gate`] (by [`Gate::index`]), the earliest level a structure
/// of that gate it serves belongs from, `u8::MAX` for none. The road's item level is the least of
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoadNeed {
    pub tile: Tile,
    pub from: [u8; GATES],
}

impl RoadNeed {
    /// The level the road is needed from with the gates `on` says are placed, or `None` when it
    /// serves only gates that are off.
    pub fn level(&self, on: impl Fn(Gate) -> bool) -> Option<u8> {
        Gate::ALL
            .into_iter()
            .filter(|&g| on(g))
            .filter_map(|g| self.from.get(g.index()).copied())
            .filter(|&l| l != u8::MAX)
            .min()
    }
}

/// A room's plan. `items` are sorted by [`Item::level`], then [`kind_rank`], then tile index, so
/// two equal plans compare equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub items: Vec<Item>,
    pub roles: Roles,
    /// What each planned road serves, sorted by tile index. A road missing here serves the base
    /// ([`Gate::Base`]) from its item's level.
    pub needs: Vec<RoadNeed>,
}

impl Layout {
    /// How many items of `kind` belong in the room at `level`.
    pub fn count(&self, kind: StructureType, level: u8) -> u32 {
        self.items
            .iter()
            .filter(|i| i.kind == kind && i.level <= level)
            .count() as u32
    }

    /// Sorts the items into their canonical order.
    pub fn sort(&mut self) {
        self.items
            .sort_by_key(|i| (i.level, kind_rank(i.kind), i.tile.index()));
    }

    /// The items of `kind`, in order.
    pub fn of(&self, kind: StructureType) -> impl Iterator<Item = &Item> + '_ {
        self.items.iter().filter(move |i| i.kind == kind)
    }

    /// What the road planned on `t` serves, if the planner said ([`Layout::needs`]).
    pub fn road_need(&self, t: Tile) -> Option<&RoadNeed> {
        self.needs
            .binary_search_by_key(&t.index(), |n| n.tile.index())
            .ok()
            .and_then(|i| self.needs.get(i))
    }
}

/// Why a planner gave up on a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// No place fits what the planner must place (the name says what).
    NoRoom(&'static str),
    /// An input the planner cannot use: no source, or a fixed spawn on an unusable tile.
    BadInput(&'static str),
}

/// The structure types a plan may hold, in the order a build would want them: what makes energy
/// and spawns first, defense and late-game last. It fixes the order of [`Layout::items`] within a
/// level, and the validator's walk over types.
pub const KINDS: [StructureType; 16] = [
    StructureType::Spawn,
    StructureType::Extension,
    StructureType::Container,
    StructureType::Storage,
    StructureType::Link,
    StructureType::Tower,
    StructureType::Road,
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

/// The position of `kind` in [`KINDS`], or `KINDS.len()` for a type no plan holds.
pub fn kind_rank(kind: StructureType) -> usize {
    KINDS.iter().position(|&k| k == kind).unwrap_or(KINDS.len())
}

/// Whether `level` is a controller level (1 to [`MAX_LEVEL`]).
pub fn is_level(level: u8) -> bool {
    (1..=MAX_LEVEL).contains(&level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::controller_levels;

    /// Uses: `controller_levels` (the progress to the next level, `None` at the top, V131).
    #[test]
    fn the_top_level_is_the_one_with_no_next() {
        assert!(controller_levels(u32::from(MAX_LEVEL)).is_none());
        assert!(controller_levels(u32::from(MAX_LEVEL) - 1).is_some());
    }

    #[test]
    fn every_kind_has_a_rank_and_a_cost() {
        for (i, &k) in KINDS.iter().enumerate() {
            assert_eq!(kind_rank(k), i);
            assert!(k.construction_cost().is_some(), "{k:?}");
        }
        assert_eq!(kind_rank(StructureType::Controller), KINDS.len());
    }
}
