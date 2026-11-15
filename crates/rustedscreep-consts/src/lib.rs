//! Constants facade (ADR-005). Besides `rustedscreep-wasm`, this is the only crate that may name the
//! `screeps` crate, and only for constants and pure local types.
//!
//! - The items `screeps::constants` exports (numbers, extra limits, enums such as `Part`,
//!   `StructureType` and `ResourceType`, and the grouped modules such as `creep` and `tower`) are
//!   re-exported at this crate's root, so code writes `rustedscreep_consts::NAME`. The `find` and
//!   `look` modules and the `FindConstant` and `LookConstant` traits are hidden, because they carry
//!   JS game-object types (`LookResult::Creep(Creep)`, find constants with `Item = Creep`; V89).
//!   The numeric `Find` enum (the `FIND_*` codes) is re-exported by name: it is a plain value with no
//!   JS type in it (the `FindConstant` impls belong to the typed find constants, `RoomObject` and
//!   `Exit`, which stay hidden). The constants self-check compares it with the server's `FIND_*`
//!   (P1.11). Upstream marks `Find` `#[doc(hidden)]` (find.rs:28-37, public because wasm-bindgen
//!   needs it), so re-check it on a crate bump (VERIFICATION_LOG V131).
//! - The pure `screeps::local` types are re-exported by name (below). `ObjectId` (its resolve
//!   methods call JS) and `LodashFilter` are left out as JS-backed. `RawObjectId` is pure but left
//!   out by policy: core works with interned `u32` handles, and the wasm crate names it directly.
//! - Values the API does not export live in [`extra`], which is never glob-re-exported, so they
//!   cannot shadow an API constant. The API's own `screeps::constants::extra` items are at the root.
//!
//! Rules for callers in `rustedscreep-core` (VERIFICATION_LOG V86, V89):
//! - Never call JS-backed APIs; they make JS calls on wasm and panic natively. That covers
//!   `Position`'s game methods (`look`, `create_construction_site`, `create_flag`, `find_path_to*`,
//!   and the `find_*`/`look_for` methods, which need a hidden find or look constant), the
//!   conversions from and to JS values (`From<RoomPosition>`, `RoomName` to `JsValue`/`JsString`,
//!   `LocalRoomTerrain` from `RoomTerrain`, `LocalCostMatrix` from `&CostMatrix`), the
//!   `from_js_value` function of every string enum, and `Terrain::from_look_constant_jsvalue`.
//! - Use the checked forms of APIs that panic or overflow on bad input:
//!   - `Position::from_packed` asserts x, y < `ROOM_SIZE`, and so does `Position`'s binary serde
//!     deserialization; check the coordinates before decoding persisted data.
//!   - `RoomXY`'s and `Position`'s `+`/`-` with a tuple or a `Direction`, and their `offset`: use
//!     `checked_add` or `checked_add_direction` (or `RoomXY`'s `saturating_*`).
//!   - `Position::from_world_coords`: use `checked_from_world_coords`.
//!   - `RoomName`'s `+`/`-`: use `checked_add` (negate the offset for `-`).
//!   - Never call `RoomXY::towards`, `between` or `midpoint_between`: their `i8` products overflow
//!     for ordinary inputs and they divide by zero for equal points. `Position`'s versions divide
//!     by zero when the points are equal and the distance is 0 or less; guard that first.
//!   - The crate's `linear_index_to_xy` and `terrain_index_to_xy` assert the index; they are not
//!     re-exported.
//! - The string enums (`Part`, `StructureType`, `ResourceType`, `IntershardResourceType`,
//!   `PowerCreepClass`) have a hidden `__Invalid` variant for strings the crate does not know.
//!   `Display` and `Serialize` panic on it and `Part::cost()` returns 0 for it, so the snapshot maps
//!   it to an explicit unknown case, and core iterates explicit lists (such as `RESOURCES_ALL`).
//!
//! The JS-carrying glob items are not reachable:
//!
//! ```compile_fail
//! use rustedscreep_consts::look::LookResult;
//! ```
//!
//! ```compile_fail
//! fn takes_find_constant<T: rustedscreep_consts::FindConstant>(_: T) {}
//! ```
//!
//! ```compile_fail
//! let _ = rustedscreep_consts::find::HOSTILE_CREEPS;
//! ```

// No-panic rule for non-test code (.claude/rules/core-logic.md; lint names: VERIFICATION_LOG V83).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable
    )
)]

pub mod extra;

pub use screeps::constants::*;

// Private items with the same names hide these glob re-exports, which carry JS game-object types
// (V89). The compile_fail examples in the crate docs guard that they stay unreachable.
#[allow(dead_code, hidden_glob_reexports)]
mod find {}
#[allow(dead_code, hidden_glob_reexports)]
mod look {}
#[allow(dead_code, hidden_glob_reexports)]
trait FindConstant {}
#[allow(dead_code, hidden_glob_reexports)]
trait LookConstant {}

/// The `FIND_*` codes as plain values (`Find::HostileCreeps as u16` is 103). The `find` module and
/// its typed find constants stay hidden.
pub use screeps::constants::find::Find;

pub use screeps::local::{
    LocalCostMatrix, LocalRoomTerrain, OffsetOutOfBoundsError, OutOfBoundsError, Position,
    RoomCoordinate, RoomName, RoomNameParseError, RoomOffset, RoomXY,
    WorldPositionOutOfBoundsError, XMajor, YMajor, xy_to_linear_index, xy_to_terrain_index,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-005 smoke test: room names parse and print natively.
    #[test]
    fn room_name_round_trips_natively() {
        let name: RoomName = "W1N1".parse().expect("valid room name");
        assert_eq!(name.to_string(), "W1N1");
        assert!("not a room".parse::<RoomName>().is_err());
    }

    /// Uses: ROOM_SIZE, ROOM_AREA.
    #[test]
    fn room_xy_bounds_follow_room_size() {
        let last = ROOM_SIZE - 1;
        assert!(RoomXY::checked_new(ROOM_SIZE, 0).is_err());
        assert!(RoomXY::checked_new(0, ROOM_SIZE).is_err());
        let max = RoomXY::checked_new(last, last).expect("in bounds");
        assert_eq!(ROOM_AREA, usize::from(ROOM_SIZE) * usize::from(ROOM_SIZE));
        assert_eq!(xy_to_linear_index(max), ROOM_AREA - 1);
        assert_eq!(xy_to_terrain_index(max), ROOM_AREA - 1);
        assert!(RoomCoordinate::new(ROOM_SIZE).is_err());
    }

    /// Uses: ROOM_USIZE. Cost matrices are column-major (index `x * ROOM_USIZE + y`) and terrain
    /// is row-major (index `y * ROOM_USIZE + x`), so core grids must not mix them (V86).
    #[test]
    fn cost_matrix_and_terrain_layouts_differ() {
        let xy = RoomXY::checked_new(1, 0).expect("in bounds");
        assert_eq!(xy_to_linear_index(xy), ROOM_USIZE);
        assert_eq!(xy_to_terrain_index(xy), 1);

        let mut matrix = LocalCostMatrix::default();
        matrix.set(xy, 7);
        let bytes: Vec<u8> = matrix.into();
        assert_eq!(bytes.get(xy_to_linear_index(xy)), Some(&7));
    }

    /// Uses: `Terrain` discriminants (the engine's TERRAIN_MASK_WALL and TERRAIN_MASK_SWAMP).
    /// A wall+swamp byte (0b11) decodes as Wall in `LocalRoomTerrain::get_xy` (V86).
    #[test]
    fn terrain_bytes_decode_with_wall_precedence() {
        let wall = Terrain::Wall as u8;
        let swamp = Terrain::Swamp as u8;
        let cases = [
            (0, Terrain::Plain),
            (wall, Terrain::Wall),
            (swamp, Terrain::Swamp),
            (wall | swamp, Terrain::Wall),
        ];
        for (x, (byte, expected)) in (1u8..).zip(cases) {
            // Row-major: a wrong layout would read (x, 0) from index x * ROOM_USIZE and fail.
            let xy = RoomXY::checked_new(x, 0).expect("in bounds");
            let mut bits = Box::new([0u8; ROOM_AREA]);
            bits[xy_to_terrain_index(xy)] = byte;
            assert_eq!(LocalRoomTerrain::new_from_bits(bits).get_xy(xy), expected);
        }
    }

    /// Uses: ROOM_SIZE. A packed position survives a round trip, as persistence will rely on.
    #[test]
    fn position_packed_round_trip() {
        let x = RoomCoordinate::new(ROOM_SIZE - 1).expect("in bounds");
        let y = RoomCoordinate::new(0).expect("in bounds");
        let room: RoomName = "E5N6".parse().expect("valid room name");
        let pos = Position::new(x, y, room);
        assert_eq!((pos.x(), pos.y(), pos.room_name()), (x, y, room));
        assert_eq!(Position::from_packed(pos.packed_repr()), pos);
    }

    // Invariants between constants are checked at compile time, so a balance change that breaks
    // one fails the test build.

    /// The packed room byte is the room coordinate plus `extra::HALF_WORLD_SIZE`, at both ends of
    /// the range (the crate's own `x_coord` and `y_coord` read it back).
    #[test]
    fn half_world_size_matches_the_packed_layout() {
        let lowest = RoomName::from_packed(0);
        assert_eq!(
            (lowest.x_coord(), lowest.y_coord()),
            (-extra::HALF_WORLD_SIZE, -extra::HALF_WORLD_SIZE)
        );
        let highest = RoomName::from_packed(u16::MAX);
        assert_eq!(highest.x_coord(), extra::HALF_WORLD_SIZE - 1);
        assert_eq!(highest.y_coord(), extra::HALF_WORLD_SIZE - 1);
        assert_eq!("W127N127".parse::<RoomName>().ok(), Some(lowest));
    }

    /// Uses: MEMORY_SEGMENT_ACTIVE_LIMIT, extra::MEMORY_SEGMENT_COUNT, extra::MEMORY_SEGMENT_SAVE_LIMIT.
    #[test]
    fn segment_limits_are_consistent() {
        const { assert!(MEMORY_SEGMENT_ACTIVE_LIMIT >= 1) };
        const { assert!(MEMORY_SEGMENT_ACTIVE_LIMIT <= extra::MEMORY_SEGMENT_COUNT as u32) };
        // Every active segment can always be saved: the save cap is at least the active cap.
        const { assert!(extra::MEMORY_SEGMENT_SAVE_LIMIT >= MEMORY_SEGMENT_ACTIVE_LIMIT) };
        // A segment set fits a u128 bitset (rustedscreep-core persist::SegmentSet).
        const { assert!(extra::MEMORY_SEGMENT_COUNT <= 128) };
    }

    /// Uses: CPU_TICK_LIMIT_MAX, PIXEL_CPU_COST, CPU_BUCKET_MAX, INTENT_CPU_COST. A full bucket
    /// covers one tick at the cap, and a pixel costs at most a full bucket. The two together can
    /// exceed it (a pixel may take the whole bucket), which the scheduler (P1 module 6) must allow.
    #[test]
    fn cpu_limits_fit_in_the_bucket() {
        const { assert!(CPU_TICK_LIMIT_MAX <= CPU_BUCKET_MAX) };
        const { assert!(PIXEL_CPU_COST <= CPU_BUCKET_MAX) };
        const { assert!(INTENT_CPU_COST > 0.0) };
    }

    /// The API's enums and grouped modules are reachable at the root (compile-time check).
    #[test]
    fn api_items_are_reachable_at_the_root() {
        let _items: (Part, StructureType, ResourceType, Direction, Terrain) = (
            Part::Move,
            StructureType::Spawn,
            ResourceType::Energy,
            Direction::Top,
            Terrain::Plain,
        );
        assert_eq!(creep::MAX_CREEP_SIZE, MAX_CREEP_SIZE);
        assert_eq!(tower::TOWER_OPTIMAL_RANGE, TOWER_OPTIMAL_RANGE);
    }
}
