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

/// A room's plan. `items` are sorted by [`Item::level`], then [`kind_rank`], then tile index, so
/// two equal plans compare equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub items: Vec<Item>,
    pub roles: Roles,
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
/// and spawns first, defence and late-game last. It fixes the order of [`Layout::items`] within a
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
