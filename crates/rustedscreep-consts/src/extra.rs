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
