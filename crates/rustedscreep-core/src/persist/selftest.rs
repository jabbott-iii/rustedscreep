//! The persistence self-test behind `rustedscreep.persistTest(bytes)`: a storage round trip through
//! the real segment path, for the P1 gate's live check and ADR-004's in-game cost.
//!
//! One run: the console asks for a size (the full [`MAX_PAYLOAD`] by default); the next output step
//! writes a frame of that many pattern bytes (seeded by the write tick) to the self-test segment and
//! requests it; a later tick reads it back through the manager, decodes it and compares header and
//! bytes. The wasm side measures each step with `Game.cpu.getUsed()` and logs one verdict line.
//! Any failure (a decode error, a stale or wrong record, a write that could not go out) is the
//! rebuild path: the verdict says why, and nothing else depends on the segment.

use super::frame::{Frame, MAX_PAYLOAD};
use super::kind;
use super::manager::LoadError;

/// Payload layout version of the self-test record.
pub const SCHEMA: u16 = 1;

/// The bytes of a run: xorshift32 from the seed, one byte per step. The seed is the write tick, so
/// a record left from an earlier run never matches by accident.
pub fn pattern(seed: u32, len: usize, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(len);
    // xorshift32 needs a non-zero state.
    let mut x = seed ^ 0x9e37_79b9;
    if x == 0 {
        x = 1;
    }
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        out.push((x >> 24) as u8);
    }
}

/// Why a run failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The write did not go out: the JS assignment failed, or `RawMemory.segments` or its keys
    /// could not be read; the text is not in the segment.
    Write,
    /// The load gave nothing usable.
    Load(LoadError),
    /// The segment holds a valid record from another write.
    Stale { written: u32 },
    /// The payload length differs from what was written.
    Size { found: usize },
    /// The first payload byte that differs.
    Mismatch { at: usize },
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Write => f.write_str("the write failed"),
            Self::Load(e) => write!(f, "load failed: {e}"),
            Self::Stale { written } => write!(f, "found the record written at {written}"),
            Self::Size { found } => write!(f, "payload of {found} B"),
            Self::Mismatch { at } => write!(f, "payload differs at byte {at}"),
        }
    }
}

/// CPU of one run's steps, from `Game.cpu.getUsed()` readings. Unit: µCPU (`None` for a bad reading).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Costs {
    /// Frame encode in Rust.
    pub encode: Option<u32>,
    /// The JS write: the text crossing to JS and the `RawMemory.segments` assignment.
    pub set: Option<u32>,
    /// The JS read: `RawMemory.segments[id]` and the text crossing into wasm.
    pub get: Option<u32>,
    /// Frame decode and the comparison in Rust.
    pub decode: Option<u32>,
}

/// The result of the last run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Tick of the write (0 if it never went out).
    pub written: u32,
    /// Tick of the verdict.
    pub at: u32,
    pub size: usize,
    /// Code units of the frame.
    pub units: usize,
    pub result: Result<(), Failure>,
    pub costs: Costs,
}

/// Where a run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// Asked for; the frame goes out on the next output step.
    Write { size: usize, asked: u32 },
    /// Written on `written`; waiting for the load.
    Read {
        size: usize,
        written: u32,
        units: usize,
        costs: Costs,
    },
}

/// A run's request, as the console gets it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requested {
    /// The run starts with this many payload bytes (the request clamped to [`MAX_PAYLOAD`]).
    Started { size: usize, clamped: bool },
    /// A run is in progress.
    Busy,
}

/// The self-test's state for one global.
#[derive(Clone, Debug, Default)]
pub struct SelfTest {
    phase: Phase,
    last: Option<Verdict>,
    runs: u32,
}

impl SelfTest {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn last(&self) -> Option<&Verdict> {
        self.last.as_ref()
    }

    /// Runs finished in this global.
    pub fn runs(&self) -> u32 {
        self.runs
    }

    /// Starts a run of `size` bytes (`None`: [`MAX_PAYLOAD`]) on tick `now`.
    ///
    /// Uses: [`MAX_PAYLOAD`] (from `MEMORY_SEGMENT_SIZE_LIMIT`).
    pub fn request(&mut self, size: Option<u32>, now: u32) -> Requested {
        if self.phase != Phase::Idle {
            return Requested::Busy;
        }
        let want = size.map_or(MAX_PAYLOAD, |s| usize::try_from(s).unwrap_or(usize::MAX));
        let size = want.min(MAX_PAYLOAD);
        self.phase = Phase::Write { size, asked: now };
        Requested::Started {
            size,
            clamped: size != want,
        }
    }

    /// The payload to write on tick `now` into `out`, if a write is due.
    pub fn payload(&self, now: u32, out: &mut Vec<u8>) -> bool {
        match self.phase {
            Phase::Write { size, .. } => {
                pattern(now, size, out);
                true
            }
            _ => false,
        }
    }

    /// The frame went out on tick `now`; the caller now requests the load.
    pub fn wrote(&mut self, now: u32, units: usize, encode: Option<u32>, set: Option<u32>) {
        if let Phase::Write { size, .. } = self.phase {
            self.phase = Phase::Read {
                size,
                written: now,
                units,
                costs: Costs {
                    encode,
                    set,
                    ..Costs::default()
                },
            };
        }
    }

    /// The write failed on tick `now`; the run ends.
    pub fn write_failed(&mut self, now: u32) -> Option<Verdict> {
        let Phase::Write { size, .. } = self.phase else {
            return None;
        };
        self.finish(Verdict {
            written: 0,
            at: now,
            size,
            units: 0,
            result: Err(Failure::Write),
            costs: Costs::default(),
        })
    }

    /// The load finished on tick `now` (a frame, or why there is none). Checks the record: kind and
    /// schema, the write tick, the size and every byte against the pattern (`scratch` is reused for
    /// it). Returns the verdict, or `None` if no run was waiting for a load.
    pub fn loaded(
        &mut self,
        now: u32,
        frame: Result<Frame<'_>, LoadError>,
        get: Option<u32>,
        scratch: &mut Vec<u8>,
    ) -> Option<Verdict> {
        let Phase::Read {
            size,
            written,
            units,
            costs,
        } = self.phase
        else {
            return None;
        };
        let result = frame.and_then(|f| {
            f.header
                .expect(kind::SELF_TEST, SCHEMA)
                .map_err(LoadError::Decode)
                .map(|()| f)
        });
        let result = match result {
            Err(e) => Err(Failure::Load(e)),
            Ok(f) if f.header.written != written => Err(Failure::Stale {
                written: f.header.written,
            }),
            Ok(f) if f.payload.len() != size => Err(Failure::Size {
                found: f.payload.len(),
            }),
            Ok(f) => {
                pattern(written, size, scratch);
                match f
                    .payload
                    .iter()
                    .zip(scratch.iter())
                    .position(|(a, b)| a != b)
                {
                    Some(at) => Err(Failure::Mismatch { at }),
                    None => Ok(()),
                }
            }
        };
        self.finish(Verdict {
            written,
            at: now,
            size,
            units,
            result,
            costs: Costs { get, ..costs },
        })
    }

    /// Adds the decode cost, measured after [`Self::loaded`] returned, to the last verdict.
    pub fn note_decode(&mut self, decode: Option<u32>) {
        if let Some(v) = self.last.as_mut() {
            v.costs.decode = decode;
        }
    }

    fn finish(&mut self, v: Verdict) -> Option<Verdict> {
        self.phase = Phase::Idle;
        self.runs = self.runs.saturating_add(1);
        self.last = Some(v);
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::super::frame::{self, DecodeError};
    use super::*;

    fn write_frame(t: &mut SelfTest, now: u32) -> String {
        let mut payload = Vec::new();
        assert!(t.payload(now, &mut payload));
        let mut text = String::new();
        let units = frame::encode(kind::SELF_TEST, SCHEMA, now, &payload, &mut text).expect("fits");
        t.wrote(now, units, Some(10), Some(20));
        text
    }

    /// Uses: `MAX_PAYLOAD`. A full-size run writes a frame that fits a segment and reads back as Ok,
    /// with the costs carried into the verdict.
    #[test]
    fn a_run_round_trips() {
        let mut t = SelfTest::default();
        assert_eq!(
            t.request(None, 10),
            Requested::Started {
                size: MAX_PAYLOAD,
                clamped: false
            }
        );
        assert_eq!(t.request(Some(1), 10), Requested::Busy);
        let text = write_frame(&mut t, 11);
        assert!(text.len() <= frame::MAX_TEXT_UNITS);
        let mut buf = Vec::new();
        let mut scratch = Vec::new();
        let f = frame::decode(&text, &mut buf);
        let v = t
            .loaded(12, f.map_err(LoadError::Decode), Some(30), &mut scratch)
            .expect("verdict");
        assert_eq!(v.result, Ok(()));
        assert_eq!(
            (v.written, v.at, v.size, v.units),
            (11, 12, MAX_PAYLOAD, text.len())
        );
        t.note_decode(Some(40));
        let c = t.last().expect("kept").costs;
        assert_eq!(
            (c.encode, c.set, c.get, c.decode),
            (Some(10), Some(20), Some(30), Some(40))
        );
        assert_eq!((t.phase(), t.runs()), (Phase::Idle, 1));
    }

    /// Sizes above the maximum are clamped, and every failure the read can see is named.
    #[test]
    fn failures_are_named() {
        let mut t = SelfTest::default();
        assert_eq!(
            t.request(Some(u32::MAX), 1),
            Requested::Started {
                size: MAX_PAYLOAD,
                clamped: true
            }
        );
        assert_eq!(
            t.write_failed(2).map(|v| v.result),
            Some(Err(Failure::Write))
        );
        let mut scratch = Vec::new();
        let mut buf = Vec::new();

        // A corrupt segment: the decode error reaches the verdict.
        t.request(Some(100), 3);
        let text = write_frame(&mut t, 4);
        let bad = text.replacen("rs1b", "rs1x", 1);
        let f = frame::decode(&bad, &mut buf).map_err(LoadError::Decode);
        let v = t.loaded(5, f, None, &mut scratch).expect("verdict");
        assert_eq!(
            v.result,
            Err(Failure::Load(LoadError::Decode(DecodeError::Encoding(
                b'x'
            ))))
        );

        // An older record (another write tick) is stale, not a pass.
        t.request(Some(100), 6);
        let _new = write_frame(&mut t, 9);
        let mut old = String::new();
        let mut bytes = Vec::new();
        pattern(7, 100, &mut bytes);
        frame::encode(kind::SELF_TEST, SCHEMA, 7, &bytes, &mut old).expect("fits");
        let f = frame::decode(&old, &mut buf).map_err(LoadError::Decode);
        let v = t.loaded(10, f, None, &mut scratch).expect("verdict");
        assert_eq!(v.result, Err(Failure::Stale { written: 7 }));

        // Another owner's record.
        t.request(Some(4), 11);
        let _ = write_frame(&mut t, 12);
        let mut other = String::new();
        frame::encode(kind::SELF_TEST + 1, SCHEMA, 12, &[0; 4], &mut other).expect("fits");
        let f = frame::decode(&other, &mut buf).map_err(LoadError::Decode);
        let v = t.loaded(13, f, None, &mut scratch).expect("verdict");
        assert!(matches!(
            v.result,
            Err(Failure::Load(LoadError::Decode(DecodeError::Kind { .. })))
        ));

        // A record of the right tick with other bytes or another size.
        for (len, want) in [
            (4usize, Failure::Mismatch { at: 0 }),
            (5, Failure::Size { found: 5 }),
        ] {
            t.request(Some(4), 14);
            let _ = write_frame(&mut t, 15);
            let mut forged = String::new();
            let mut bytes = Vec::new();
            pattern(15, 4, &mut bytes);
            bytes[0] ^= 0xff;
            bytes.resize(len, 0);
            frame::encode(kind::SELF_TEST, SCHEMA, 15, &bytes, &mut forged).expect("fits");
            let f = frame::decode(&forged, &mut buf).map_err(LoadError::Decode);
            let v = t.loaded(16, f, None, &mut scratch).expect("verdict");
            assert_eq!(v.result, Err(want));
        }

        // A segment that never arrived.
        t.request(Some(4), 17);
        let _ = write_frame(&mut t, 18);
        let v = t
            .loaded(
                22,
                Err(LoadError::Unavailable { tries: 4 }),
                None,
                &mut scratch,
            )
            .expect("verdict");
        assert_eq!(
            v.result,
            Err(Failure::Load(LoadError::Unavailable { tries: 4 }))
        );
        // No run waiting: nothing to report.
        assert_eq!(
            t.loaded(23, Err(LoadError::NotText), None, &mut scratch),
            None
        );
        for f in [
            Failure::Write,
            Failure::Load(LoadError::NotText),
            Failure::Stale { written: 1 },
            Failure::Size { found: 1 },
            Failure::Mismatch { at: 1 },
        ] {
            assert!(!f.to_string().is_empty());
        }
    }

    /// The pattern depends on the seed, covers every byte value at the full size, and is stable.
    #[test]
    fn the_pattern_is_seeded() {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        pattern(1, MAX_PAYLOAD, &mut a);
        pattern(2, MAX_PAYLOAD, &mut b);
        assert_ne!(a, b);
        let mut seen = [false; 256];
        for &x in &a {
            seen[usize::from(x)] = true;
        }
        assert!(seen.iter().all(|&s| s));
        let mut again = Vec::new();
        pattern(1, MAX_PAYLOAD, &mut again);
        assert_eq!(a, again);
        pattern(0x9e37_79b9, 3, &mut a);
        assert_eq!(a.len(), 3, "the seed that zeroes the state still runs");
    }
}
