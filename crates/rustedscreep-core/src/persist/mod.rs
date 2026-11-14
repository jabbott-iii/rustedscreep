//! Persistence (ROADMAP P1 module 9): what must survive a global reset, in RawMemory segments.
//!
//! - [`frame`]: one segment's text (ADR-004, accepted 2026-11-14): an ASCII tag, then base64 of a
//!   16-byte header (owner kind, schema, write tick, length, CRC-32) and the payload. Any decode
//!   failure is an error the owner answers by rebuilding from game state, never a crash.
//! - [`manager`]: the segment manager: activation scheduling (one tick ahead, V116), dirty tracking,
//!   and writes that respect the driver's 10-key save cap shared with the profiler's segment.
//! - [`selftest`]: the storage round trip behind `rustedscreep.persistTest()`.
//! - [`base64`], [`crc32`]: the encoding and the checksum, allocation-free on reused buffers.
//!
//! Each record has an owner that knows its segment id and its [`kind`], and a payload layout it
//! versions with the frame's `schema`. Records live in the heap between ticks: a segment is read
//! once per global (or on request) and written only when its owner marks it dirty.

pub mod base64;
pub mod crc32;
pub mod frame;
pub mod manager;
pub mod selftest;
mod set;

pub use frame::{DecodeError, Frame, Header, MAX_PAYLOAD};
pub use manager::{Counters, Keys, LoadError, Segments, WritePlan, WriteResult};
pub use set::{Iter, SegmentId, SegmentSet};

/// Owner kinds written into each frame's header. A new owner takes the next number; a number is
/// never reused, so an old record of a removed owner is refused rather than misread.
pub mod kind {
    /// The self-test record (`rustedscreep.persistTest()`).
    pub const SELF_TEST: u16 = 1;
}
