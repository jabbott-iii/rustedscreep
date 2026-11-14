//! The per-tick snapshot (ROADMAP P1 module 5). One pass at the start of the tick copies what the
//! decisions need from the game into struct-of-arrays tables, with stable numeric handles in place
//! of JS objects and id strings. `rustedscreep-wasm` fills it; everything here is pure Rust.
//!
//! One tick: [`Snapshot::begin`] with the [`TickContext`], one `push` per object into [`Rooms`],
//! [`Creeps`] and [`Structures`], then [`Snapshot::finish`], which releases the handles of objects
//! that were not pushed. The rows stay valid until the next `begin`. A failed read skips that one
//! object, is counted in [`Snapshot::skipped`], and the tick goes on.
//!
//! What it holds so far: identity, location and structure type. Later modules add the columns they
//! consume, so no tick reads data that nothing uses. Row order is the engine's object order, which
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
    Controller, CreepClass, CreepHandle, CreepRow, Creeps, Rooms, StructureClass, StructureHandle,
    StructureStatic, Structures, known_structure_type, position_from_packed,
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
}

/// Everything the snapshot read this tick.
#[derive(Default)]
pub struct Snapshot {
    pub tick: TickContext,
    pub rooms: Rooms,
    pub creeps: Creeps,
    pub structures: Structures,
    pub skipped: SkipCounts,
}

impl Snapshot {
    /// Starts a tick: clears last tick's rows (keeping their capacity) and the skip counts.
    pub fn begin(&mut self, tick: TickContext) {
        self.tick = tick;
        self.rooms.begin();
        self.creeps.begin();
        self.structures.begin();
        self.skipped = SkipCounts::default();
    }

    /// Ends the pass: objects not pushed since [`Self::begin`] lose their handles.
    pub fn finish(&mut self) {
        self.creeps.finish();
        self.structures.finish();
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
