//! Values that `screeps-game-api` 0.23.5 does not export. Each item cites its source and a
//! "last verified" date; `docs/VERIFICATION_LOG.md` holds the full finding.
//!
//! This module is never glob-re-exported at the crate root, so nothing here can shadow an API
//! constant. Refer to these values as `rustedscreep_consts::extra::NAME`.

/// Number of RawMemory segments. Valid ids are `0..MEMORY_SEGMENT_COUNT` (0 to 99).
///
/// The API exports the size and active-count limits (`MEMORY_SEGMENT_SIZE_LIMIT`,
/// `MEMORY_SEGMENT_ACTIVE_LIMIT`) but not the id range; its segment functions take `u8` ids.
///
/// Source: screeps/driver `lib/runtime/runtime.js` at cf63d8a
/// (<https://github.com/screeps/driver/blob/cf63d8adf902663e2ebddd7f8c5b7baa425dc928/lib/runtime/runtime.js#L129-L146>),
/// VERIFICATION_LOG V41 and V85. Last verified: 2026-11-13.
pub const MEMORY_SEGMENT_COUNT: u8 = 100;

/// Most keys `RawMemory.segments` may hold at the end of a tick. The driver saves every key present
/// then, loaded active segments included even when unchanged, and throws "Cannot save more than 10
/// memory segments on the same tick" above this count, which likely loses the tick (V41, inferred).
/// It is the same literal as `MEMORY_SEGMENT_ACTIVE_LIMIT` but a separate rule, so a segment written
/// without being activated counts here too.
///
/// Source: screeps/driver `lib/runtime/runtime.js` at cf63d8a, the end-of-tick save
/// (<https://github.com/screeps/driver/blob/cf63d8adf902663e2ebddd7f8c5b7baa425dc928/lib/runtime/runtime.js#L250-L268>),
/// VERIFICATION_LOG V85 and V119. Last verified: 2026-11-14.
pub const MEMORY_SEGMENT_SAVE_LIMIT: u32 = 10;

/// Offset of room coordinates in a packed room name or position: a room coordinate `c` (`W`n and
/// `N`n are -n-1, `E`n and `S`n are n) is stored as the byte `c + HALF_WORLD_SIZE`, so room
/// coordinates run over `-HALF_WORLD_SIZE..HALF_WORLD_SIZE`. The API keeps its own copy private.
///
/// Source: screeps-game-api 0.23.5 `src/local.rs` L14-24 (`HALF_WORLD_SIZE`,
/// `VALID_ROOM_NAME_COORDINATES`), the same layout as the engine's `__packedPos` (VERIFICATION_LOG
/// V103, V114). Last verified: 2026-11-14.
pub const HALF_WORLD_SIZE: i32 = 128;

/// Longest creep name `StructureSpawn.spawnCreep` accepts, in UTF-16 code units (`name.length`):
/// a longer one returns `ERR_INVALID_ARGS`. The runtime checks no character set.
///
/// Source: screeps/engine `src/game/structures.js` at a90d26f
/// (<https://github.com/screeps/engine/blob/a90d26fc9eb943147799e65d0bf9d4d0d79a2722/src/game/structures.js#L1065-L1071>),
/// VERIFICATION_LOG V171. Last verified: 2026-11-15.
pub const CREEP_NAME_MAX_LENGTH: usize = 100;
