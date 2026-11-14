//! The segment manager (ROADMAP P1.9): which segments to activate, which to read this tick, and which
//! dirty records to write within the driver's save cap. Pure; rustedscreep-wasm does the JS calls and
//! reports what it saw.
//!
//! The driver's rules it plans around (VERIFICATION_LOG V41, V85, V116 to V120):
//! - The active set lives on the server. A `setActiveSegments` call takes effect for the segments
//!   loaded on the next tick, persists until the next call (across global resets too), and the
//!   last call of a tick wins. Console commands run after `loop`, so one can override ours.
//! - An active segment never written reads as `""`; an inactive one is absent from
//!   `RawMemory.segments`.
//! - At the end of a tick every key of `RawMemory.segments` is saved, loaded ones included, and more
//!   than `MEMORY_SEGMENT_SAVE_LIMIT` keys throw and likely lose the tick. A segment can be written
//!   without being active.
//!
//! The policy:
//! - Records live in the heap, so a segment is read once per global (or when an owner asks) and then
//!   dropped from the active set: idle ticks load nothing and keep every save key free.
//! - On a global's first loop the server's active set is unknown, so every wanted segment is looked
//!   up at once (one left active by the previous global is read without waiting), and the active set
//!   is then set to what this global wants, possibly nothing.
//! - A wanted segment missing on a tick after its activation counts a try and re-issues the set (a
//!   console call may have replaced it). After `load_retries` tries the load fails and the owner
//!   rebuilds from game state.
//! - Writes go out in id order, at most `max_writes_per_tick` a tick. Writing a key already present
//!   costs no save key; any other write needs a free key after the keys present, the caller's extra
//!   keys (the profiler's segment) and the writes before it. A write that does not fit waits, and the
//!   active set is re-issued in case keys the manager did not request are taking the room.

use super::frame::DecodeError;
use super::set::{SegmentId, SegmentSet};
use crate::config::PersistConfig;
use rustedscreep_consts::MEMORY_SEGMENT_ACTIVE_LIMIT;
use rustedscreep_consts::extra::{MEMORY_SEGMENT_COUNT, MEMORY_SEGMENT_SAVE_LIMIT};

/// Why a requested load gave the owner nothing; every case means the owner rebuilds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The segment stayed missing on this many ticks after it was requested.
    Unavailable { tries: u8 },
    /// `RawMemory.segments[id]` held something other than a string.
    NotText,
    /// The text is not a valid frame of the owner's record.
    Decode(DecodeError),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable { tries } => {
                write!(f, "not loaded after {tries} tick(s) of requests")
            }
            Self::NotText => f.write_str("not a string"),
            Self::Decode(e) => write!(f, "{e}"),
        }
    }
}

/// The outcome of one write the caller attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    /// The text is in `RawMemory.segments`; the end-of-tick save stores it.
    Written,
    /// The JS write failed; the record stays dirty and is tried again next tick.
    Failed,
    /// The owner will not write this record now (it no longer fits a segment, or the owner gave up
    /// after a failed JS write); it is no longer dirty until the owner marks it again.
    Rejected,
}

/// The save keys of this tick, as the caller found them just before writing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// `Object.keys(RawMemory.segments).length`.
    pub count: u32,
    /// The ids among those keys (writing one of them adds no key).
    pub present: SegmentSet,
    /// Keys others will add later this tick (the profiler's stats segment on a window-closing tick).
    pub extra: u32,
}

/// This tick's writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WritePlan {
    /// The ids to write now, in id order.
    pub write: SegmentSet,
    /// Dirty ids that found no free save key.
    pub no_key: SegmentSet,
    /// The keys present plus the extra ones already exceed the save cap: keys this global did not
    /// request (a console `setActiveSegments`) are loaded, and only re-issuing the set drops them.
    pub overfull: bool,
}

/// Counts since the global started, for the console summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub loads: u32,
    pub load_failures: u32,
    /// Ticks a wanted segment was missing after its activation.
    pub retries: u32,
    pub writes: u32,
    pub write_failures: u32,
    pub write_rejections: u32,
    /// Ticks on which some dirty record found no free save key.
    pub no_key_ticks: u32,
    pub activations: u32,
    pub activation_failures: u32,
}

/// The manager's state for one global.
#[derive(Clone, Debug)]
pub struct Segments {
    /// Ids the manager never activates or writes (the profiler's stats segment).
    reserved: SegmentSet,
    want: SegmentSet,
    dirty: SegmentSet,
    /// The active set this global last set.
    requested: SegmentSet,
    requested_at: u32,
    /// `requested` is the server's active set, as far as the manager knows.
    known: bool,
    /// This global has called `setActiveSegments` (successfully or not).
    attempted: bool,
    /// Issue the active set again even if it is unchanged.
    stale: bool,
    tries: [u8; MEMORY_SEGMENT_COUNT as usize],
    counters: Counters,
}

impl Segments {
    pub fn new(reserved: SegmentSet) -> Self {
        Self {
            reserved,
            want: SegmentSet::EMPTY,
            dirty: SegmentSet::EMPTY,
            requested: SegmentSet::EMPTY,
            requested_at: 0,
            known: false,
            attempted: false,
            stale: false,
            tries: [0; MEMORY_SEGMENT_COUNT as usize],
            counters: Counters::default(),
        }
    }

    pub fn reserved(&self) -> SegmentSet {
        self.reserved
    }

    /// Wanted loads not yet delivered.
    pub fn wanted(&self) -> SegmentSet {
        self.want
    }

    pub fn dirty(&self) -> SegmentSet {
        self.dirty
    }

    /// The active set this global last set, if it set one that is still believed current.
    pub fn active(&self) -> Option<(SegmentSet, u32)> {
        self.known.then_some((self.requested, self.requested_at))
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Asks for `id`'s text on a coming tick. Refused (false) for a reserved id.
    pub fn request_load(&mut self, id: SegmentId) -> bool {
        if self.reserved.contains(id) {
            return false;
        }
        self.want.insert(id);
        true
    }

    /// Drops a wanted load.
    pub fn cancel_load(&mut self, id: SegmentId) {
        self.want.remove(id);
        self.set_tries(id, 0);
    }

    /// Marks `id`'s record for writing. A pending load of it is dropped: the owner's heap copy is
    /// newer than what the segment holds. Refused (false) for a reserved id.
    pub fn mark_dirty(&mut self, id: SegmentId) -> bool {
        if self.reserved.contains(id) {
            return false;
        }
        self.dirty.insert(id);
        self.cancel_load(id);
        true
    }

    fn tries_of(&self, id: SegmentId) -> u8 {
        self.tries.get(id.index()).copied().unwrap_or(0)
    }

    fn set_tries(&mut self, id: SegmentId, n: u8) {
        if let Some(t) = self.tries.get_mut(id.index()) {
            *t = n;
        }
    }

    /// The wanted ids to look up in `RawMemory.segments` this tick. While the server's active set is
    /// unknown (a global's first loop, or after a failed `setActiveSegments`) that is every wanted id;
    /// otherwise the wanted ids that an activation on an earlier tick covers.
    pub fn due(&self, now: u32) -> SegmentSet {
        if !self.known {
            return self.want;
        }
        if self.requested_at == now {
            return SegmentSet::EMPTY;
        }
        self.want.intersection(self.requested)
    }

    /// A due id was missing from `RawMemory.segments` (or reading it threw). Returns the error once
    /// the load gives up; the owner then rebuilds. A global's first look-up, before any request,
    /// costs no try.
    pub fn absent(&mut self, id: SegmentId, cfg: &PersistConfig) -> Option<LoadError> {
        if !self.want.contains(id) || !self.attempted {
            return None;
        }
        self.stale = true;
        self.counters.retries = self.counters.retries.saturating_add(1);
        let tries = self.tries_of(id).saturating_add(1);
        if tries > cfg.load_retries {
            self.want.remove(id);
            self.set_tries(id, 0);
            self.counters.load_failures = self.counters.load_failures.saturating_add(1);
            return Some(LoadError::Unavailable { tries });
        }
        self.set_tries(id, tries);
        None
    }

    /// A due id's text was read and handed to its owner; `ok` says whether it held the owner's
    /// record. Either way the load is complete.
    pub fn loaded(&mut self, id: SegmentId, ok: bool) {
        self.want.remove(id);
        self.set_tries(id, 0);
        let c = &mut self.counters;
        if ok {
            c.loads = c.loads.saturating_add(1);
        } else {
            c.load_failures = c.load_failures.saturating_add(1);
        }
    }

    /// This tick's writes, given the keys present now.
    ///
    /// Uses: `rustedscreep_consts::extra::MEMORY_SEGMENT_SAVE_LIMIT` (the driver's cap on keys saved
    /// per tick, V85, V119).
    pub fn plan_writes(&self, keys: &Keys, cfg: &PersistConfig) -> WritePlan {
        let taken = keys.count.saturating_add(keys.extra);
        let mut free = MEMORY_SEGMENT_SAVE_LIMIT.saturating_sub(taken);
        let mut plan = WritePlan {
            overfull: taken > MEMORY_SEGMENT_SAVE_LIMIT,
            ..WritePlan::default()
        };
        let mut n: u8 = 0;
        for id in self.dirty.minus(self.reserved).iter() {
            if n >= cfg.max_writes_per_tick {
                break;
            }
            if keys.present.contains(id) {
                plan.write.insert(id);
                n += 1;
            } else if free > 0 {
                free -= 1;
                plan.write.insert(id);
                n += 1;
            } else {
                plan.no_key.insert(id);
            }
        }
        plan
    }

    /// Records the plan's outcome for the keys: dirty records that found no key, or more keys than
    /// the cap, re-issue the active set, which drops keys this global did not request.
    pub fn planned(&mut self, plan: &WritePlan) {
        if !plan.no_key.is_empty() {
            self.counters.no_key_ticks = self.counters.no_key_ticks.saturating_add(1);
        }
        if !plan.no_key.is_empty() || plan.overfull {
            self.stale = true;
        }
    }

    /// Someone else found no free save key (the profiler's stats segment on a window-closing tick):
    /// the next output step re-issues the active set, which drops keys this global did not request.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Records one attempted write.
    pub fn written(&mut self, id: SegmentId, result: WriteResult) {
        let c = &mut self.counters;
        match result {
            WriteResult::Written => {
                self.dirty.remove(id);
                c.writes = c.writes.saturating_add(1);
            }
            WriteResult::Failed => c.write_failures = c.write_failures.saturating_add(1),
            WriteResult::Rejected => {
                self.dirty.remove(id);
                c.write_rejections = c.write_rejections.saturating_add(1);
            }
        }
    }

    /// The active set to pass to `setActiveSegments` this tick, when a call is needed: the lowest
    /// `max_active_segments` wanted ids (at least 1, at most `MEMORY_SEGMENT_ACTIVE_LIMIT`). A call
    /// is needed on a global's first loop (to clear a set a previous global or the console left),
    /// whenever the set changes, after a missing segment or a write without a key, and after a
    /// failed call while loads are wanted (a failed call with nothing wanted is not repeated).
    ///
    /// Uses: `rustedscreep_consts::MEMORY_SEGMENT_ACTIVE_LIMIT` (V41).
    pub fn plan_active(&self, cfg: &PersistConfig) -> Option<SegmentSet> {
        let cap = u32::from(cfg.max_active_segments).clamp(1, MEMORY_SEGMENT_ACTIVE_LIMIT);
        let desired = self.want.minus(self.reserved).lowest(cap);
        let needed = if self.known {
            self.stale || desired != self.requested
        } else {
            !self.attempted || !desired.is_empty()
        };
        needed.then_some(desired)
    }

    /// `setActiveSegments(set)` returned normally on tick `now`.
    pub fn activated(&mut self, set: SegmentSet, now: u32) {
        self.requested = set;
        self.requested_at = now;
        self.known = true;
        self.attempted = true;
        self.stale = false;
        self.counters.activations = self.counters.activations.saturating_add(1);
    }

    /// `setActiveSegments` threw: the server's set is unknown again (a throw can leave a partial
    /// list, V118), so the next tick looks up every wanted id and calls again.
    pub fn activation_failed(&mut self) {
        self.known = false;
        self.attempted = true;
        self.counters.activation_failures = self.counters.activation_failures.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PERSIST_MMO;
    use std::collections::BTreeMap;

    fn id(n: u8) -> SegmentId {
        SegmentId::new(n).expect("valid id")
    }

    fn set(ids: &[u8]) -> SegmentSet {
        ids.iter().map(|&n| id(n)).collect()
    }

    const CFG: PersistConfig = PERSIST_MMO;

    /// A global's first loop looks up every wanted id, then sets the active set; the next tick reads
    /// what that call covers; a delivered load leaves the set, and an empty set is issued once.
    #[test]
    fn activation_follows_the_wanted_loads() {
        let mut m = Segments::new(set(&[99]));
        assert!(m.request_load(id(5)) && m.request_load(id(3)));
        assert!(!m.request_load(id(99)), "reserved");
        // First loop: the server's set is unknown, so both are looked up; one is already there.
        assert_eq!(m.due(100), set(&[3, 5]));
        m.loaded(id(3), true);
        assert_eq!(m.absent(id(5), &CFG), None, "a first look-up costs no try");
        assert_eq!(m.plan_active(&CFG), Some(set(&[5])));
        m.activated(set(&[5]), 100);
        assert_eq!(m.plan_active(&CFG), None);
        assert_eq!(m.due(100), SegmentSet::EMPTY, "the call applies next tick");
        assert_eq!(m.due(101), set(&[5]));
        m.loaded(id(5), true);
        assert_eq!(m.plan_active(&CFG), Some(SegmentSet::EMPTY));
        m.activated(SegmentSet::EMPTY, 101);
        assert_eq!(m.plan_active(&CFG), None, "idle: no call");
        assert_eq!(m.due(102), SegmentSet::EMPTY);
        assert_eq!((m.counters().loads, m.counters().activations), (2, 2));

        // With nothing wanted, the first loop still sets the (empty) set once.
        let fresh = Segments::new(SegmentSet::EMPTY);
        assert_eq!(fresh.plan_active(&CFG), Some(SegmentSet::EMPTY));
    }

    /// Uses: `MEMORY_SEGMENT_ACTIVE_LIMIT`. At most `max_active_segments` (clamped to the API's limit
    /// and to at least 1) ids are active at once; the rest follow as loads complete.
    #[test]
    fn the_active_set_is_capped() {
        let mut m = Segments::new(SegmentSet::EMPTY);
        for n in 0..20 {
            m.request_load(id(n));
        }
        let first = m.plan_active(&CFG).expect("call");
        assert_eq!(first.len(), u32::from(CFG.max_active_segments));
        assert_eq!(first, (0..CFG.max_active_segments).map(id).collect());
        let wide = PersistConfig {
            max_active_segments: u8::MAX,
            ..CFG
        };
        assert_eq!(
            m.plan_active(&wide).map(SegmentSet::len),
            Some(MEMORY_SEGMENT_ACTIVE_LIMIT)
        );
        let none = PersistConfig {
            max_active_segments: 0,
            ..CFG
        };
        assert_eq!(m.plan_active(&none), Some(set(&[0])));
        m.activated(first, 10);
        for n in 0..CFG.max_active_segments {
            m.loaded(id(n), true);
        }
        let next = m.plan_active(&CFG).expect("call");
        assert_eq!(next.iter().next(), Some(id(CFG.max_active_segments)));
    }

    /// A segment missing after its activation counts a try and re-issues the set; after
    /// `load_retries` tries the load fails with `Unavailable`.
    #[test]
    fn missing_segments_are_retried_then_given_up() {
        let mut m = Segments::new(SegmentSet::EMPTY);
        m.request_load(id(7));
        m.absent(id(7), &CFG); // first loop
        m.activated(set(&[7]), 1);
        for tick in 2..2 + u32::from(CFG.load_retries) {
            assert_eq!(m.due(tick), set(&[7]));
            assert_eq!(m.absent(id(7), &CFG), None);
            assert_eq!(
                m.plan_active(&CFG),
                Some(set(&[7])),
                "re-issued although unchanged"
            );
            m.activated(set(&[7]), tick);
        }
        let last = 2 + u32::from(CFG.load_retries);
        assert_eq!(
            m.absent(id(7), &CFG),
            Some(LoadError::Unavailable {
                tries: CFG.load_retries + 1
            })
        );
        assert_eq!(m.wanted(), SegmentSet::EMPTY);
        assert_eq!(m.plan_active(&CFG), Some(SegmentSet::EMPTY));
        assert_eq!(m.due(last + 1), SegmentSet::EMPTY);
        assert_eq!(m.counters().load_failures, 1);
        assert_eq!(m.counters().retries, u32::from(CFG.load_retries) + 1);
        // A new request starts counting from zero.
        m.request_load(id(7));
        m.activated(set(&[7]), last + 1);
        assert_eq!(m.absent(id(7), &CFG), None);
    }

    /// A failed call makes the set unknown: every wanted id is looked up, absences count, and the
    /// call is repeated.
    #[test]
    fn a_failed_activation_is_retried() {
        let mut m = Segments::new(SegmentSet::EMPTY);
        m.request_load(id(1));
        m.activation_failed();
        assert_eq!(m.due(5), set(&[1]));
        assert_eq!(m.absent(id(1), &CFG), None);
        assert_eq!(
            m.counters().retries,
            1,
            "after an attempt, an absence counts"
        );
        assert_eq!(m.plan_active(&CFG), Some(set(&[1])));
        assert_eq!(m.active(), None);
        m.activated(set(&[1]), 5);
        assert_eq!(m.active(), Some((set(&[1]), 5)));

        // A failed first call with nothing wanted is not repeated every tick.
        let mut idle = Segments::new(SegmentSet::EMPTY);
        assert_eq!(idle.plan_active(&CFG), Some(SegmentSet::EMPTY));
        idle.activation_failed();
        assert_eq!(idle.plan_active(&CFG), None);
        idle.request_load(id(2));
        assert_eq!(idle.plan_active(&CFG), Some(set(&[2])));
    }

    /// Uses: `MEMORY_SEGMENT_SAVE_LIMIT`. Writes fit the save cap after the keys present and the extra
    /// keys; a key already present is free; the per-tick cap holds; reserved ids are never written.
    #[test]
    fn writes_fit_the_save_cap() {
        let mut m = Segments::new(set(&[99]));
        for n in [1, 2, 3, 4] {
            assert!(m.mark_dirty(id(n)));
        }
        assert!(!m.mark_dirty(id(99)));
        let cfg = PersistConfig {
            max_writes_per_tick: 10,
            ..CFG
        };
        let limit = MEMORY_SEGMENT_SAVE_LIMIT;
        // Two keys free after the present ones and the profiler's; id 3 is present, so it is free.
        let keys = Keys {
            count: limit - 3,
            present: set(&[3]),
            extra: 1,
        };
        let plan = m.plan_writes(&keys, &cfg);
        assert_eq!(plan.write, set(&[1, 2, 3]));
        assert_eq!(plan.no_key, set(&[4]));
        // A full set of keys: only the present one.
        let full = Keys {
            count: limit,
            present: set(&[3]),
            extra: 0,
        };
        assert_eq!(m.plan_writes(&full, &cfg).write, set(&[3]));
        // More keys than the cap (someone else overfilled it) writes only present ones.
        let over = Keys {
            count: limit + 2,
            present: SegmentSet::EMPTY,
            extra: 1,
        };
        assert_eq!(m.plan_writes(&over, &cfg).write, SegmentSet::EMPTY);
        // The per-tick cap.
        let empty = Keys::default();
        let plan = m.plan_writes(&empty, &CFG);
        assert_eq!(plan.write.len(), u32::from(CFG.max_writes_per_tick));
        assert_eq!(
            plan.no_key,
            SegmentSet::EMPTY,
            "the tick cap is not a key shortage"
        );
    }

    /// Written and rejected records stop being dirty; a failed write stays dirty; a write without a
    /// key re-issues the active set; marking a record dirty drops its pending load.
    #[test]
    fn dirty_tracking() {
        let mut m = Segments::new(SegmentSet::EMPTY);
        m.request_load(id(4));
        m.mark_dirty(id(4));
        assert_eq!(m.wanted(), SegmentSet::EMPTY, "the heap copy wins");
        m.mark_dirty(id(5));
        m.mark_dirty(id(6));
        m.written(id(4), WriteResult::Written);
        m.written(id(5), WriteResult::Failed);
        m.written(id(6), WriteResult::Rejected);
        assert_eq!(m.dirty(), set(&[5]));
        let c = *m.counters();
        assert_eq!((c.writes, c.write_failures, c.write_rejections), (1, 1, 1));
        m.activated(SegmentSet::EMPTY, 1);
        assert_eq!(m.plan_active(&CFG), None);
        let plan = WritePlan {
            no_key: set(&[5]),
            ..WritePlan::default()
        };
        m.planned(&plan);
        assert_eq!(m.plan_active(&CFG), Some(SegmentSet::EMPTY));
        assert_eq!(m.counters().no_key_ticks, 1);
        m.activated(SegmentSet::EMPTY, 2);
        // An overfull tick re-issues the set even when every write fits (a present key).
        let over = Keys {
            count: MEMORY_SEGMENT_SAVE_LIMIT + 1,
            present: set(&[5]),
            extra: 0,
        };
        let plan = m.plan_writes(&over, &CFG);
        assert!(plan.overfull && plan.no_key.is_empty() && plan.write == set(&[5]));
        m.planned(&plan);
        assert_eq!(m.plan_active(&CFG), Some(SegmentSet::EMPTY));
        assert_eq!(
            m.counters().no_key_ticks,
            1,
            "an overfull tick is not a key shortage"
        );
        m.activated(SegmentSet::EMPTY, 3);
        let exact = Keys {
            count: MEMORY_SEGMENT_SAVE_LIMIT - 1,
            present: SegmentSet::EMPTY,
            extra: 1,
        };
        assert!(
            !m.plan_writes(&exact, &CFG).overfull,
            "at the cap is not over it"
        );
        // Someone else's refusal (the profiler) re-issues the set too.
        assert_eq!(m.plan_active(&CFG), None);
        m.mark_stale();
        assert_eq!(m.plan_active(&CFG), Some(SegmentSet::EMPTY));
    }

    /// A seeded xorshift64.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// A model of the driver (V116 to V120): the stored segments, the server's active set (applied
    /// on the next tick, kept across ticks and globals, the last call of a tick winning) and the
    /// end-of-tick save with its key cap.
    #[derive(Default)]
    struct Driver {
        stored: BTreeMap<u8, String>,
        active: Vec<u8>,
    }

    struct Tick {
        segments: BTreeMap<u8, String>,
        next_active: Option<Vec<u8>>,
    }

    impl Driver {
        fn begin(&self) -> Tick {
            let segments = self
                .active
                .iter()
                .map(|&i| (i, self.stored.get(&i).cloned().unwrap_or_default()))
                .collect();
            Tick {
                segments,
                next_active: None,
            }
        }
        fn end(&mut self, tick: Tick, extra: &[u8]) -> Result<(), String> {
            let mut keys = tick.segments;
            for &e in extra {
                keys.entry(e).or_insert_with(|| "profiler".into());
            }
            if keys.len() > MEMORY_SEGMENT_SAVE_LIMIT as usize {
                return Err(format!("{} keys saved", keys.len()));
            }
            if let Some(a) = tick.next_active {
                assert!(a.len() <= MEMORY_SEGMENT_ACTIVE_LIMIT as usize);
                self.active = a;
            }
            self.stored.extend(keys);
            Ok(())
        }
    }

    /// 20000 seeded ticks across global resets, console overrides of the active set and profiler
    /// ticks, driving the manager the way rustedscreep-wasm does: the save cap is never exceeded,
    /// reserved ids are never touched, a load delivers what was last written (or the empty string for
    /// a segment never written), and every load and write completes once the disturbances stop.
    #[test]
    fn a_driver_model_never_overflows_and_always_completes() {
        let mut rng = Rng(0x5eed_0f5e_6d0b_1234);
        let reserved = set(&[99]);
        let mut driver = Driver::default();
        let mut m = Segments::new(reserved);
        // What each id should hold: the last text written.
        let mut written: BTreeMap<u8, String> = BTreeMap::new();
        let mut content = 0u64;
        let mut delivered = 0u32;
        let total_ticks = 20_000u32;
        let calm_from = total_ticks - 200;
        for now in 1..=total_ticks {
            let calm = now >= calm_from;
            if !calm && rng.below(500) == 0 {
                m = Segments::new(reserved); // a global reset
            }
            let mut tick = driver.begin();
            // Load step.
            for i in m.due(now).iter() {
                match tick.segments.get(&i.get()) {
                    None => {
                        let _ = m.absent(i, &CFG);
                    }
                    Some(text) => {
                        let expect = written.get(&i.get()).cloned().unwrap_or_default();
                        assert_eq!(text, &expect, "segment {} at {now}", i.get());
                        m.loaded(i, true);
                        delivered += 1;
                    }
                }
            }
            // Owners: loads and writes on random ids 0..12, sometimes the reserved one.
            if !calm {
                for _ in 0..rng.below(3) {
                    let n = if rng.below(50) == 0 {
                        99
                    } else {
                        rng.below(12) as u8
                    };
                    if rng.below(2) == 0 {
                        m.request_load(id(n));
                    } else {
                        m.mark_dirty(id(n));
                    }
                }
            }
            // Output step: writes, then the active set.
            let profiler = !calm && rng.below(7) == 0;
            let keys = Keys {
                count: tick.segments.len() as u32,
                present: m
                    .dirty()
                    .iter()
                    .filter(|i| tick.segments.contains_key(&i.get()))
                    .collect(),
                extra: u32::from(profiler && !tick.segments.contains_key(&99)),
            };
            let plan = m.plan_writes(&keys, &CFG);
            m.planned(&plan);
            assert!(plan.write.intersection(reserved).is_empty());
            for i in plan.write.iter() {
                content += 1;
                let text = format!("rec{content}");
                tick.segments.insert(i.get(), text.clone());
                written.insert(i.get(), text);
                m.written(i, WriteResult::Written);
            }
            if let Some(a) = m.plan_active(&CFG) {
                assert!(a.intersection(reserved).is_empty());
                tick.next_active = Some(a.iter().map(|i| i.get()).collect());
                m.activated(a, now);
            }
            // A console command runs after the loop and may replace the active set. At most 9 ids,
            // so that with the profiler's key the tick starts within the cap: an overflow the
            // manager did not cause is not what this test checks.
            if !calm && rng.below(40) == 0 {
                let n = rng.below(10) as usize;
                tick.next_active = Some((0..n as u8).map(|k| k * 9).collect());
            }
            let extra: Vec<u8> = if profiler { vec![99] } else { vec![] };
            driver
                .end(tick, &extra)
                .unwrap_or_else(|e| panic!("tick {now}: {e}"));
        }
        assert_eq!(m.wanted(), SegmentSet::EMPTY, "every load completed");
        assert_eq!(m.dirty(), SegmentSet::EMPTY, "every write went out");
        assert!(delivered > 1000, "{delivered} loads delivered");
        assert!(
            !driver
                .stored
                .keys()
                .any(|&k| k == 99 && driver.stored[&k] != "profiler")
        );
    }
}
