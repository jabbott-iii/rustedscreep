//! The per-tick snapshot (ROADMAP P1 module 5). One pass at the start of the tick copies what the
//! decisions need from the game into struct-of-arrays tables, with stable numeric handles in place
//! of JS objects and id strings. `rustedscreep-wasm` fills it; everything here is pure Rust.
//!
//! One tick: [`Snapshot::begin`] with the [`TickContext`], one `push` per object into [`Rooms`],
//! [`Creeps`] and [`Structures`], then [`Snapshot::finish`], which releases the handles of objects
//! that were not pushed. The rows stay valid until the next `begin`. A failed read skips that one
//! object, is counted in [`Snapshot::skipped`], and the tick goes on.
//!
//! What it holds so far: identity, location and structure type; fatigue (P2.1); room energy, own
//! spawns and own creeps' name identities (P2.3); own creeps' energy, the room for energy in spawns
//! and extensions, owned controllers' progress and timers, and the room objects the opening works
//! with (P2.4); hostile creeps' hits in owned rooms, and their bodies read once per creep (P7.1,
//! [`Creeps::set_body`]). Later modules add the columns they consume, so
//! no tick reads data that nothing uses. Row order is the engine's object order, which
//! is deterministic for the same input but not guaranteed stable across ticks (VERIFICATION_LOG
//! V102), so a decision that needs a stable order sorts by id (`key_of`), which also survives a
//! global reset; handle order follows when objects were first seen.

mod intern;
mod room_name;
mod tables;
mod tick;

pub use intern::{Handle, Interner, ObjectKey};
pub use room_name::parse_room_name;
pub use tables::{
    BodyPartRead, BodyRead, Combat, Controller, ControllerState, CreepClass, CreepHandle, CreepRow,
    CreepStore, Creeps, INTEL_SOURCES, IntelRead, Links, MineralRead, ObjectClass, ObjectHandle,
    ObjectKind, ObjectRow, ObjectStatic, RemoteRead, Reservation, RoomEnergy, RoomObjects,
    RoomStatus, Rooms, SIGN_TOWERS, SafeModeState, SignRead, SignSeen, Spawns, StructureClass,
    StructureHandle, StructureStatic, Structures, Towers, known_structure_type,
    position_from_packed,
};
pub use tick::TickContext;

/// Why one object was left out of this tick's snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The object has no id. Only a creep that `spawnCreep` created during this tick lacks one
    /// (V101), so a snapshot taken at the start of the tick should never count this.
    NoId,
    /// The id is not 1 to 24 lowercase hex digits.
    BadId,
    /// The packed position has x or y outside the room.
    BadPosition,
    /// The structure type is not one this build knows.
    UnknownStructureType,
    /// The room name is not a string that parses as a room name.
    BadRoomName,
    /// The same id was pushed twice in one tick.
    Duplicate,
    /// No handle slot or row index is left (more than `u32::MAX`; cannot happen in practice).
    SlotsExhausted,
}

impl SkipReason {
    /// Every reason, in a fixed order (the order of [`SkipCounts`]).
    pub const ALL: [Self; 7] = [
        Self::NoId,
        Self::BadId,
        Self::BadPosition,
        Self::UnknownStructureType,
        Self::BadRoomName,
        Self::Duplicate,
        Self::SlotsExhausted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoId => "no id",
            Self::BadId => "bad id",
            Self::BadPosition => "bad position",
            Self::UnknownStructureType => "unknown structure type",
            Self::BadRoomName => "bad room name",
            Self::Duplicate => "duplicate",
            Self::SlotsExhausted => "slots exhausted",
        }
    }
}

/// Objects skipped this tick, per reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkipCounts {
    counts: [u32; SkipReason::ALL.len()],
}

impl SkipCounts {
    pub fn record(&mut self, reason: SkipReason) {
        if let Some(count) = self.counts.get_mut(reason as usize) {
            *count = count.saturating_add(1);
        }
    }

    pub fn get(&self, reason: SkipReason) -> u32 {
        self.counts.get(reason as usize).copied().unwrap_or(0)
    }

    pub fn total(&self) -> u32 {
        self.counts.iter().fold(0, |sum, &c| sum.saturating_add(c))
    }

    /// Adds `other`'s counts, saturating.
    pub fn add(&mut self, other: &Self) {
        for (c, &o) in self.counts.iter_mut().zip(&other.counts) {
            *c = c.saturating_add(o);
        }
    }
}

/// The skipped objects of the ticks one [`SkipLog`] line covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkipReport {
    /// The first and last tick with skips in the span.
    pub first: u32,
    pub last: u32,
    /// Ticks in the span that skipped anything.
    pub ticks: u32,
    pub counts: SkipCounts,
}

/// Rate limit for logging skipped objects (`LogConfig::skip_window_ticks`). The first tick that
/// skips anything is reported at once; after a report, further skips are summed and reported once
/// `window_ticks` have passed since it, on the first tick after that (with skips or not). So a
/// steady skip costs one line per window and a one-off skip shows up at once. A global reset starts
/// it over.
#[derive(Clone, Copy, Debug, Default)]
pub struct SkipLog {
    pending: SkipCounts,
    ticks: u32,
    first: u32,
    last: u32,
    reported: Option<u32>,
}

impl SkipLog {
    /// Notes tick `now`'s skips; returns the report that is due, if one is. A `window_ticks` of 0
    /// counts as 1.
    #[inline]
    pub fn note(
        &mut self,
        now: u32,
        skipped: &SkipCounts,
        window_ticks: u32,
    ) -> Option<SkipReport> {
        if skipped.total() > 0 {
            if self.ticks == 0 {
                self.first = now;
            }
            self.pending.add(skipped);
            self.ticks = self.ticks.saturating_add(1);
            self.last = now;
        }
        if self.ticks == 0 {
            return None;
        }
        let due = self
            .reported
            .is_none_or(|at| now.wrapping_sub(at) >= window_ticks.max(1));
        if !due {
            return None;
        }
        self.reported = Some(now);
        let report = SkipReport {
            first: self.first,
            last: self.last,
            ticks: self.ticks,
            counts: self.pending,
        };
        self.pending = SkipCounts::default();
        self.ticks = 0;
        Some(report)
    }
}

/// Everything the snapshot read this tick.
#[derive(Default)]
pub struct Snapshot {
    pub tick: TickContext,
    pub rooms: Rooms,
    pub creeps: Creeps,
    pub structures: Structures,
    /// The bot's spawns (P2.3), a view over `structures`.
    pub spawns: Spawns,
    /// The bot's links (P3.2), a view over `structures`.
    pub links: Links,
    /// The bot's towers (P7.2), a view over `structures`.
    pub towers: Towers,
    /// Sources, own sites, piles, tombstones, ruins and containers in owned rooms (P2.4).
    pub objects: RoomObjects,
    pub skipped: SkipCounts,
}

impl Snapshot {
    /// Starts a tick: clears last tick's rows (keeping their capacity) and the skip counts.
    pub fn begin(&mut self, tick: TickContext) {
        self.tick = tick;
        self.rooms.begin();
        self.creeps.begin();
        self.structures.begin();
        self.spawns.begin();
        self.links.begin();
        self.towers.begin();
        self.objects.begin();
        self.skipped = SkipCounts::default();
    }

    /// Ends the pass: objects not pushed since [`Self::begin`] lose their handles.
    pub fn finish(&mut self) {
        self.creeps.finish();
        self.structures.finish();
        self.objects.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustedscreep_consts::{Position, RoomCoordinate, RoomName, StructureType};

    fn pos(x: u8, y: u8) -> Position {
        let room: RoomName = "W1N1".parse().expect("valid room name");
        Position::new(
            RoomCoordinate::new(x).expect("in room"),
            RoomCoordinate::new(y).expect("in room"),
            room,
        )
    }

    #[test]
    fn skip_counts_count_each_reason() {
        let mut counts = SkipCounts::default();
        for (n, reason) in (1u32..).zip(SkipReason::ALL) {
            for _ in 0..n {
                counts.record(reason);
            }
        }
        for (n, reason) in (1u32..).zip(SkipReason::ALL) {
            assert_eq!(counts.get(reason), n, "{}", reason.as_str());
        }
        let n = u32::try_from(SkipReason::ALL.len()).expect("small");
        assert_eq!(counts.total(), n * (n + 1) / 2);
    }

    fn skips(reasons: &[SkipReason]) -> SkipCounts {
        let mut c = SkipCounts::default();
        for &r in reasons {
            c.record(r);
        }
        c
    }

    /// The first skip is reported at once; later skips are summed and reported once a window has
    /// passed since the last report, on the first tick after that even without new skips; quiet
    /// ticks report nothing.
    #[test]
    fn skip_log_reports_at_most_once_per_window() {
        let w = crate::config::LOG_MMO.skip_window_ticks;
        let none = SkipCounts::default();
        let bad = skips(&[SkipReason::BadId, SkipReason::BadPosition]);
        let mut log = SkipLog::default();
        assert_eq!(log.note(10, &none, w), None);
        let first = log.note(11, &bad, w).expect("first skip at once");
        assert_eq!(
            (first.first, first.last, first.ticks, first.counts),
            (11, 11, 1, bad)
        );
        // Skips on every tick from 12 to 11 + w + w/2: one report when the window since tick 11 has
        // passed (ticks 12 to 11 + w), the rest pending.
        let stop = 11 + w + w / 2;
        let reports: Vec<SkipReport> = (12..=stop).filter_map(|t| log.note(t, &bad, w)).collect();
        assert_eq!(reports.len(), 1, "{reports:?}");
        let r0 = reports[0];
        assert_eq!((r0.first, r0.last, r0.ticks), (12, 11 + w, w));
        assert_eq!(r0.counts.get(SkipReason::BadId), w);
        // Skips stop: what is pending is reported once a window has passed since that report, on a
        // tick without skips, and then nothing.
        let quiet: Vec<(u32, SkipReport)> = (stop + 1..stop + 3 * w)
            .filter_map(|t| log.note(t, &none, w).map(|r| (t, r)))
            .collect();
        assert_eq!(quiet.len(), 1, "{quiet:?}");
        let (at, r1) = quiet[0];
        assert_eq!(at, 11 + 2 * w);
        assert_eq!((r1.first, r1.last, r1.ticks), (12 + w, stop, stop - 11 - w));
        // A skip less than a window after the last report waits; one a window after it shows at once.
        assert_eq!(log.note(at + w - 1, &bad, w), None);
        let again = log
            .note(at + w, &bad, w)
            .expect("a window after the last report");
        assert_eq!((again.first, again.ticks), (at + w - 1, 2));
        // A window of 0 counts as 1: every tick with skips may report.
        let mut every = SkipLog::default();
        assert!((0..5).all(|t| every.note(t, &bad, 0).is_some()));
    }

    #[test]
    fn skip_counts_add_saturating() {
        let mut a = skips(&[SkipReason::NoId]);
        a.add(&skips(&[SkipReason::NoId, SkipReason::Duplicate]));
        assert_eq!(
            (
                a.get(SkipReason::NoId),
                a.get(SkipReason::Duplicate),
                a.total()
            ),
            (2, 1, 3)
        );
        let mut big = SkipCounts {
            counts: [u32::MAX; SkipReason::ALL.len()],
        };
        big.add(&a);
        assert_eq!(big.get(SkipReason::NoId), u32::MAX);
    }

    #[test]
    fn every_reason_has_its_own_counter() {
        for (i, reason) in SkipReason::ALL.into_iter().enumerate() {
            assert_eq!(reason as usize, i);
        }
    }

    /// A scripted world over four ticks, driven the way rustedscreep-wasm drives the snapshot:
    /// handles stay stable, a gone creep loses its handle, static data is read once per structure,
    /// a skipped object changes nothing, and rows reset every tick.
    #[test]
    fn a_scripted_world_over_four_ticks() {
        let mut snap = Snapshot::default();
        let spawn = StructureStatic {
            kind: StructureType::Spawn,
            pos: pos(25, 25),
        };
        let mut static_reads = 0;
        let mine = |x| CreepRow {
            pos: pos(x, 10),
            my: true,
            spawning: false,
            fatigue: 0,
        };

        // Tick 1: one room, two creeps, a spawn, and an object with a bad id.
        snap.begin(TickContext::from_raw(100, 20, 500.0, 10_000));
        snap.rooms
            .push("W1N1".parse().expect("room"), Controller::Mine { level: 1 });
        let a = snap.creeps.push(ObjectKey(1), mine(10)).expect("a");
        let b = snap.creeps.push(ObjectKey(2), mine(11)).expect("b");
        let s = snap
            .structures
            .push(ObjectKey(3), || {
                static_reads += 1;
                Ok(spawn)
            })
            .expect("spawn");
        snap.skipped.record(SkipReason::BadId);
        snap.finish();
        assert_eq!(
            (
                snap.creeps.len(),
                snap.creeps.added(),
                snap.structures.added()
            ),
            (2, 2, 1)
        );
        assert_eq!(snap.skipped.total(), 1);

        // Tick 2: creep b is gone, a moved.
        snap.begin(TickContext::from_raw(101, 20, 500.0, 10_000));
        assert_eq!(snap.skipped.total(), 0, "skips reset each tick");
        assert!(
            snap.rooms.is_empty() && snap.creeps.is_empty(),
            "rows reset each tick"
        );
        assert_eq!(snap.creeps.push(ObjectKey(1), mine(12)), Ok(a));
        let again = snap.structures.push(ObjectKey(3), || {
            static_reads += 1;
            Ok(spawn)
        });
        assert_eq!(again, Ok(s));
        snap.finish();
        assert_eq!(snap.creeps.released(), 1);
        assert_eq!(snap.creeps.row_of(b), None);
        assert_eq!(snap.creeps.positions(), [pos(12, 10)]);

        // Tick 3: a hostile appears; b's id does not come back, and a new creep takes b's slot.
        snap.begin(TickContext::from_raw(102, 20, 500.0, 10_000));
        snap.creeps.push(ObjectKey(1), mine(13)).expect("a");
        let c = snap.creeps.push(ObjectKey(4), mine(14)).expect("c");
        let hostile = CreepRow {
            pos: pos(40, 40),
            my: false,
            spawning: false,
            fatigue: 0,
        };
        let h = snap.creeps.push(ObjectKey(5), hostile).expect("hostile");
        snap.structures
            .push(ObjectKey(3), || Ok(spawn))
            .expect("spawn");
        snap.finish();
        assert_eq!(c.slot(), b.slot(), "the free slot is reused");
        assert_ne!(c, b);
        assert_eq!(snap.creeps.row_of(h), Some(2));
        assert_eq!(snap.creeps.my(), [true, true, false]);
        assert_eq!(snap.creeps.live(), 3);

        // Tick 4: the spawn is destroyed.
        snap.begin(TickContext::from_raw(103, 20, 500.0, 10_000));
        snap.finish();
        assert_eq!((snap.structures.released(), snap.structures.live()), (1, 0));
        assert_eq!(snap.structures.row_of(s), None);
        assert_eq!(
            static_reads, 1,
            "the spawn's type and position were read once"
        );
    }
}
