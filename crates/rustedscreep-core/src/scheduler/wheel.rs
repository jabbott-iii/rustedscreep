//! The sleep wheel (ROADMAP P1 module 6): an item (a creep, a room, a job) sleeps until a tick, or
//! until an event fires, whichever comes first, and costs nothing while it sleeps.
//!
//! Layout: a ring of [`RING`] slots holds the sleepers due in the next [`RING`] ticks, one slot per
//! tick; later ones wait in a min-heap and move into the ring as it turns. An item maps to one
//! entry, so sleeping again replaces the earlier sleep. Removal is lazy: a slot or the heap may
//! still hold a reference to a replaced entry, and the entry's generation makes it stale.
//!
//! Time (VERIFICATION_LOG V107): `Game.time` can jump by many ticks between two runs (skipped ticks),
//! so [`SleepWheel::advance`] wakes everything due up to `now`, not only what is due at `now`. A
//! repeated tick wakes nothing twice. A tick earlier than the last (which only a server-side reset
//! could cause) wakes every sleeper, because an item woken early only re-checks and sleeps again,
//! while an item that oversleeps loses work. A global reset empties the wheel the same way: every
//! item starts awake.
//!
//! Output order is deterministic: first the items an event woke (and sleeps made for a tick already
//! past), in the order that happened, then the sleepers by wake tick. Items due on the same tick come
//! out in an order fixed by the sequence of calls alone. The hash maps are only looked up, never
//! iterated.

use core::cmp::Reverse;
use core::hash::Hash;
use std::collections::BinaryHeap;

use rustc_hash::FxHashMap;

/// Ticks the ring covers; sleeps further out wait in the heap. A size, not a tunable: it changes
/// only where an entry waits, never when it wakes.
pub const RING: u32 = 256;

/// Emptied event lists kept for reuse (so a sleep after a fire does not allocate), at most this many.
const SPARE_LISTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ref {
    index: u32,
    generation: u32,
}

struct Entry<T, E> {
    item: T,
    wake: u32,
    /// The event it waits for, until that event fires.
    event: Option<E>,
    /// Its position in that event's list, so leaving the list costs O(1).
    event_pos: u32,
    generation: u32,
    live: bool,
}

/// Sleepers keyed by item `T`, optionally waiting for an event `E`.
pub struct SleepWheel<T, E> {
    entries: Vec<Entry<T, E>>,
    free: Vec<u32>,
    by_item: FxHashMap<T, u32>,
    ring: Vec<Vec<Ref>>,
    /// (wake tick, insertion order, entry index, generation).
    far: BinaryHeap<Reverse<(u32, u64, u32, u32)>>,
    order: u64,
    /// The current entries waiting for each event; a key exists only while its list is not empty.
    by_event: FxHashMap<E, Vec<Ref>>,
    /// Emptied event lists, reused by the next new key.
    spare: Vec<Vec<Ref>>,
    /// Woken by an event, or put to sleep for a tick already past; they come out first.
    ready: Vec<Ref>,
    /// The tick of the last [`Self::advance`], if any.
    now: Option<u32>,
}

impl<T, E> Default for SleepWheel<T, E> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            free: Vec::new(),
            by_item: FxHashMap::default(),
            ring: (0..RING).map(|_| Vec::new()).collect(),
            far: BinaryHeap::new(),
            order: 0,
            by_event: FxHashMap::default(),
            spare: Vec::new(),
            ready: Vec::new(),
            now: None,
        }
    }
}

impl<T: Copy + Eq + Hash, E: Copy + Eq + Hash> SleepWheel<T, E> {
    /// Puts `item` to sleep until tick `wake_at`, or until `event` fires if given, replacing any
    /// sleep it already had. A `wake_at` at or before the last advanced tick wakes it on the next
    /// [`Self::advance`]. Returns false (the item stays awake) only if no entry can be made.
    pub fn sleep(&mut self, item: T, wake_at: u32, event: Option<E>) -> bool {
        self.cancel(item);
        let Some(r) = self.allocate(item, wake_at, event) else {
            return false;
        };
        self.by_item.insert(item, r.index);
        self.place(r, wake_at);
        if let Some(e) = event {
            let spare = &mut self.spare;
            let list = self
                .by_event
                .entry(e)
                .or_insert_with(|| spare.pop().unwrap_or_default());
            let pos = u32::try_from(list.len()).unwrap_or(u32::MAX);
            list.push(r);
            if let Some(entry) = usize::try_from(r.index)
                .ok()
                .and_then(|i| self.entries.get_mut(i))
            {
                entry.event_pos = pos;
            }
        }
        true
    }

    /// Wakes `item` silently: it is no longer asleep and will not come out of [`Self::advance`].
    /// Returns whether it was asleep.
    pub fn cancel(&mut self, item: T) -> bool {
        match self.by_item.get(&item).copied() {
            Some(index) => {
                self.release(index);
                true
            }
            None => false,
        }
    }

    /// The tick `item` sleeps until, if it is asleep.
    pub fn wake_at(&self, item: T) -> Option<u32> {
        let index = *self.by_item.get(&item)?;
        let entry = self.entries.get(usize::try_from(index).ok()?)?;
        entry.live.then_some(entry.wake)
    }

    pub fn is_asleep(&self, item: T) -> bool {
        self.by_item.contains_key(&item)
    }

    /// Sleeping items.
    pub fn len(&self) -> usize {
        self.by_item.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    /// Fires `event`: every item waiting for it wakes on the next [`Self::advance`]. Returns how
    /// many.
    pub fn fire(&mut self, event: E) -> u32 {
        let Some(mut waiting) = self.by_event.remove(&event) else {
            return 0;
        };
        let mut woken = 0u32;
        for r in waiting.drain(..) {
            if let Some(entry) = self.current_mut(r) {
                // The event has fired; releasing the entry later need not look for it again.
                entry.event = None;
                self.ready.push(r);
                woken = woken.saturating_add(1);
            }
        }
        self.keep_spare(waiting);
        woken
    }

    /// Keeps an emptied event list for reuse, up to [`SPARE_LISTS`].
    fn keep_spare(&mut self, list: Vec<Ref>) {
        if self.spare.len() < SPARE_LISTS {
            self.spare.push(list);
        }
    }

    /// Moves time to `now` and appends every item that wakes to `out`: those an event woke, then
    /// those due at or before `now`. Each woken item is no longer asleep.
    pub fn advance(&mut self, now: u32, out: &mut Vec<T>) {
        for i in 0..self.ready.len() {
            if let Some(&r) = self.ready.get(i) {
                self.emit(r, out);
            }
        }
        self.ready.clear();

        match self.now {
            Some(last) if now == last => {}
            Some(last) if now < last => {
                // Time went back: wake everyone, in wake order.
                self.drain_ring_from(last, RING, out);
                while let Some(Reverse((_, _, index, generation))) = self.far.pop() {
                    self.emit(Ref { index, generation }, out);
                }
                self.now = Some(now);
            }
            Some(last) => {
                self.drain_ring_from(last, (now - last).min(RING), out);
                self.now = Some(now);
                self.pull_far(now, out);
            }
            None => {
                self.now = Some(now);
                self.pull_far(now, out);
            }
        }
    }

    /// Empties the ring slots of ticks `last + 1 ..= last + steps`, in tick order.
    fn drain_ring_from(&mut self, last: u32, steps: u32, out: &mut Vec<T>) {
        for k in 1..=steps {
            let slot = usize::try_from(last.wrapping_add(k) % RING).unwrap_or(0);
            let Some(refs) = self.ring.get_mut(slot) else {
                continue;
            };
            let mut refs = core::mem::take(refs);
            for r in refs.drain(..) {
                self.emit(r, out);
            }
            // Put the emptied vector back, so the slot keeps its capacity.
            if let Some(empty) = self.ring.get_mut(slot) {
                *empty = refs;
            }
        }
    }

    /// Moves heap entries that are now within the ring's reach into it, and emits those already due.
    fn pull_far(&mut self, now: u32, out: &mut Vec<T>) {
        while let Some(&Reverse((wake, _, index, generation))) = self.far.peek() {
            if wake > now && wake - now > RING {
                break;
            }
            self.far.pop();
            let r = Ref { index, generation };
            if self.current(r).is_none() {
                continue;
            }
            if wake <= now {
                self.emit(r, out);
            } else {
                self.push_ring(r, wake);
            }
        }
    }

    /// Files a new entry by its wake tick relative to the last advanced tick.
    fn place(&mut self, r: Ref, wake: u32) {
        match self.now {
            Some(now) if wake <= now => self.ready.push(r),
            Some(now) if wake - now <= RING => self.push_ring(r, wake),
            _ => {
                self.far
                    .push(Reverse((wake, self.order, r.index, r.generation)));
                self.order = self.order.wrapping_add(1);
                self.compact_far();
            }
        }
    }

    fn push_ring(&mut self, r: Ref, wake: u32) {
        let slot = usize::try_from(wake % RING).unwrap_or(0);
        if let Some(refs) = self.ring.get_mut(slot) {
            refs.push(r);
        }
    }

    /// Drops stale heap entries once they outnumber the live sleepers, so an item that keeps
    /// re-sleeping far ahead cannot grow the heap without bound.
    fn compact_far(&mut self) {
        if self.far.len() <= self.by_item.len().saturating_mul(2).saturating_add(64) {
            return;
        }
        let entries = &self.entries;
        self.far.retain(|&Reverse((_, _, index, generation))| {
            is_current(entries, Ref { index, generation })
        });
    }

    /// Outputs the entry's item and releases it, if the reference is still current.
    fn emit(&mut self, r: Ref, out: &mut Vec<T>) {
        if let Some(entry) = self.current(r) {
            out.push(entry.item);
            self.release(r.index);
        }
    }

    fn allocate(&mut self, item: T, wake: u32, event: Option<E>) -> Option<Ref> {
        if let Some(index) = self.free.pop() {
            let entry = self.entries.get_mut(usize::try_from(index).ok()?)?;
            entry.item = item;
            entry.wake = wake;
            entry.event = event;
            entry.event_pos = 0;
            entry.live = true;
            return Some(Ref {
                index,
                generation: entry.generation,
            });
        }
        let index = u32::try_from(self.entries.len()).ok()?;
        self.entries.push(Entry {
            item,
            wake,
            event,
            event_pos: 0,
            generation: 0,
            live: true,
        });
        Some(Ref {
            index,
            generation: 0,
        })
    }

    /// Frees a live entry: the item is awake, every reference to the entry turns stale, and it
    /// leaves its event's list.
    fn release(&mut self, index: u32) {
        let Some(entry) = usize::try_from(index)
            .ok()
            .and_then(|i| self.entries.get_mut(i))
        else {
            return;
        };
        if !entry.live {
            return;
        }
        let r = Ref {
            index,
            generation: entry.generation,
        };
        entry.live = false;
        entry.generation = entry.generation.wrapping_add(1);
        let item = entry.item;
        let event = entry.event.take();
        let event_pos = entry.event_pos;
        self.by_item.remove(&item);
        if let Some(e) = event
            && let Some(list) = self.by_event.get_mut(&e)
        {
            // The stored position is always right (see the invariant test); the scan is a fallback.
            let pos = usize::try_from(event_pos)
                .ok()
                .filter(|&p| list.get(p) == Some(&r))
                .or_else(|| list.iter().position(|&x| x == r));
            if let Some(pos) = pos {
                list.swap_remove(pos);
                // The last reference moved into `pos`: record its new position.
                if let Some(&moved) = list.get(pos)
                    && let Some(m) = usize::try_from(moved.index)
                        .ok()
                        .and_then(|i| self.entries.get_mut(i))
                {
                    m.event_pos = u32::try_from(pos).unwrap_or(u32::MAX);
                }
            }
            if list.is_empty()
                && let Some(emptied) = self.by_event.remove(&e)
            {
                self.keep_spare(emptied);
            }
        }
        self.free.push(index);
    }

    fn current(&self, r: Ref) -> Option<&Entry<T, E>> {
        let entry = self.entries.get(usize::try_from(r.index).ok()?)?;
        (entry.live && entry.generation == r.generation).then_some(entry)
    }

    fn current_mut(&mut self, r: Ref) -> Option<&mut Entry<T, E>> {
        let entry = self.entries.get_mut(usize::try_from(r.index).ok()?)?;
        (entry.live && entry.generation == r.generation).then_some(entry)
    }
}

fn is_current<T, E>(entries: &[Entry<T, E>], r: Ref) -> bool {
    usize::try_from(r.index)
        .ok()
        .and_then(|i| entries.get(i))
        .is_some_and(|e| e.live && e.generation == r.generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type Wheel = SleepWheel<u32, u8>;

    fn advance(w: &mut Wheel, now: u32) -> Vec<u32> {
        let mut out = Vec::new();
        w.advance(now, &mut out);
        out
    }

    /// An item wakes exactly at its tick, never before, both inside the ring and beyond it.
    #[test]
    fn wakes_exactly_at_the_tick() {
        for delay in [1, 2, RING - 1, RING, RING + 1, 3 * RING + 7] {
            let mut w = Wheel::default();
            advance(&mut w, 1000);
            assert!(w.sleep(1, 1000 + delay, None));
            assert_eq!(w.wake_at(1), Some(1000 + delay));
            for t in 1001..1000 + delay {
                assert!(advance(&mut w, t).is_empty(), "delay {delay} woke at {t}");
            }
            assert_eq!(advance(&mut w, 1000 + delay), [1], "delay {delay}");
            assert!(w.is_empty() && !w.is_asleep(1));
        }
    }

    /// A jump over many ticks (skipped ticks, V107) wakes everything due, in wake order.
    #[test]
    fn a_time_jump_wakes_everything_due_in_order() {
        let mut w = Wheel::default();
        advance(&mut w, 0);
        let wakes = [5, RING * 4, 3, RING + 2, 9, RING * 2, 3];
        for (item, wake) in (10u32..).zip(wakes) {
            assert!(w.sleep(item, wake, None));
        }
        assert!(w.sleep(99, RING * 10, None));
        let out = advance(&mut w, RING * 5);
        // By wake tick, ties in sleep order: 3 (item 12), 3 (item 16), 5, 9, RING+2, 2*RING, 4*RING.
        assert_eq!(out, [12, 16, 10, 14, 13, 15, 11]);
        assert_eq!(w.len(), 1);
        assert_eq!(advance(&mut w, RING * 10), [99]);
    }

    /// The same tick twice wakes nothing twice; a sleep for a past tick wakes on the next advance.
    #[test]
    fn a_repeated_tick_does_not_double_fire() {
        let mut w = Wheel::default();
        advance(&mut w, 50);
        w.sleep(1, 51, None);
        assert_eq!(advance(&mut w, 51), [1]);
        assert!(advance(&mut w, 51).is_empty());
        w.sleep(2, 40, None);
        w.sleep(3, 51, None);
        assert_eq!(advance(&mut w, 51), [2, 3]);
    }

    /// Time going back wakes every sleeper (fail open), and the wheel keeps working from there.
    #[test]
    fn time_going_back_wakes_everyone() {
        let mut w = Wheel::default();
        advance(&mut w, 10_000);
        w.sleep(1, 10_010, None);
        w.sleep(2, 10_000 + RING * 3, Some(7));
        w.sleep(3, 10_001, None);
        assert_eq!(advance(&mut w, 500), [3, 1, 2]);
        assert!(w.is_empty());
        assert_eq!(w.fire(7), 0, "the woken item left its event list");
        w.sleep(4, 501, None);
        assert_eq!(advance(&mut w, 501), [4]);
    }

    /// An event wakes its sleepers on the next advance, before timed ones, and they do not wake
    /// again at their tick.
    #[test]
    fn events_wake_sleepers_early() {
        let mut w = Wheel::default();
        advance(&mut w, 0);
        w.sleep(1, 100, Some(9));
        w.sleep(2, 100, Some(9));
        w.sleep(3, 5, None);
        w.sleep(4, 100, Some(8));
        assert_eq!(w.fire(9), 2);
        assert_eq!(w.fire(9), 0, "each sleeper wakes once");
        assert_eq!(advance(&mut w, 5), [1, 2, 3]);
        assert_eq!(advance(&mut w, 100), [4]);
        assert!(w.is_empty());
    }

    /// Sleeping again replaces the earlier sleep; cancel wakes silently.
    #[test]
    fn resleep_and_cancel() {
        let mut w = Wheel::default();
        advance(&mut w, 0);
        w.sleep(1, 10, Some(1));
        w.sleep(1, 20, None);
        assert_eq!(w.len(), 1);
        assert_eq!(w.fire(1), 0, "the replaced sleep left its event list");
        assert!(advance(&mut w, 10).is_empty());
        assert_eq!(advance(&mut w, 20), [1]);
        w.sleep(2, 30, Some(2));
        assert!(w.cancel(2));
        assert!(!w.cancel(2));
        assert_eq!(w.fire(2), 0);
        assert!(advance(&mut w, 40).is_empty());
    }

    /// Sleeps made before the first advance wait in the heap and come out correctly.
    #[test]
    fn sleeps_before_the_first_advance() {
        let mut w = Wheel::default();
        w.sleep(1, 7, None);
        w.sleep(2, 3, None);
        w.sleep(3, 7 + RING * 2, None);
        assert_eq!(advance(&mut w, 5), [2]);
        assert_eq!(advance(&mut w, 7), [1]);
        assert_eq!(advance(&mut w, 7 + RING * 2), [3]);
    }

    /// Re-sleeping far ahead every tick does not grow the heap without bound.
    #[test]
    fn the_heap_stays_bounded_under_churn() {
        let mut w = Wheel::default();
        let mut peak = 0;
        for t in 0..10_000u32 {
            advance(&mut w, t);
            for item in 0..4 {
                w.sleep(item, t + RING * 8, None);
                peak = peak.max(w.far.len());
            }
        }
        assert_eq!(w.len(), 4);
        // Compaction runs once the heap passes 2 × live + 64.
        assert!(peak <= 2 * 4 + 64 + 1, "heap peaked at {peak}");
        assert!(
            w.entries.len() <= 16,
            "entries are reused: {}",
            w.entries.len()
        );
    }

    /// Checks the wheel's internal links: every event list holds exactly the live entries waiting
    /// for that event, each at its recorded position; no key has an empty list; spare lists are
    /// empty and bounded; and `by_item` covers exactly the live entries.
    fn check_invariants(w: &Wheel) {
        let mut listed = 0;
        for (&event, list) in &w.by_event {
            assert!(!list.is_empty(), "event {event} has an empty list");
            for (pos, r) in list.iter().enumerate() {
                let entry = &w.entries[usize::try_from(r.index).expect("fits")];
                assert!(entry.live && entry.generation == r.generation);
                assert_eq!(entry.event, Some(event));
                assert_eq!(usize::try_from(entry.event_pos).expect("fits"), pos);
                listed += 1;
            }
        }
        let waiting = w
            .entries
            .iter()
            .filter(|e| e.live && e.event.is_some())
            .count();
        assert_eq!(listed, waiting);
        assert!(w.spare.len() <= SPARE_LISTS && w.spare.iter().all(Vec::is_empty));
        assert_eq!(w.by_item.len(), w.entries.iter().filter(|e| e.live).count());
    }

    /// Seeded random operations against a naive model: the same items wake on the same ticks
    /// (compared as sets per advance; the order tests above pin the order), over gaps, repeats,
    /// rewinds and events, with the internal links checked after every operation.
    #[test]
    fn matches_a_naive_model() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15; // xorshift seed
        let mut rand = move |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            u32::try_from(state % u64::from(n)).expect("fits")
        };
        let mut w = Wheel::default();
        // item -> (wake, event, sleep order)
        let mut model: BTreeMap<u32, (u32, Option<u8>, u64)> = BTreeMap::new();
        let mut fired: Vec<u32> = Vec::new();
        let mut order = 0u64;
        let mut now = 1_000u32;
        let mut advanced = false;
        for _ in 0..20_000 {
            match rand(10) {
                0..=4 => {
                    let item = rand(64);
                    let wake = if advanced { now } else { 1_000 } + rand(RING * 3);
                    let wake = wake.saturating_sub(rand(4));
                    let event = (rand(3) == 0).then(|| u8::try_from(rand(4)).expect("small"));
                    assert!(w.sleep(item, wake, event));
                    fired.retain(|&x| x != item);
                    model.insert(item, (wake, event, order));
                    order += 1;
                }
                5 => {
                    let item = rand(64);
                    let was = model.remove(&item).is_some() || fired.contains(&item);
                    fired.retain(|&x| x != item);
                    assert_eq!(w.cancel(item), was);
                }
                6 => {
                    let e = u8::try_from(rand(4)).expect("small");
                    let mut hit: Vec<(u64, u32)> = model
                        .iter()
                        .filter(|(_, (_, ev, _))| *ev == Some(e))
                        .map(|(&i, &(_, _, o))| (o, i))
                        .collect();
                    hit.sort_unstable();
                    let woken = w.fire(e);
                    assert_eq!(woken, u32::try_from(hit.len()).expect("small"));
                    for (_, i) in hit {
                        model.remove(&i);
                        fired.push(i);
                    }
                }
                _ => {
                    let step = match rand(20) {
                        0 => 0,
                        1 => RING * 2 + rand(RING),
                        2 if advanced => {
                            // Rare rewind.
                            now = now.saturating_sub(rand(50) + 1);
                            0
                        }
                        _ => 1 + rand(5),
                    };
                    let rewound = advanced && step == 0 && w.now.is_some_and(|l| now < l);
                    now += step;
                    let mut expected: Vec<u32> = core::mem::take(&mut fired);
                    let mut due: Vec<(u32, u64, u32)> = model
                        .iter()
                        .filter(|(_, (wake, _, _))| rewound || *wake <= now)
                        .map(|(&i, &(wake, _, o))| (wake, o, i))
                        .collect();
                    due.sort_unstable();
                    for &(_, _, i) in &due {
                        model.remove(&i);
                    }
                    let got = advance(&mut w, now);
                    // Compared as sets: which items wake on this advance.
                    let timed: Vec<u32> = due.iter().map(|&(_, _, i)| i).collect();
                    expected.extend(timed);
                    let mut g = got.clone();
                    let mut e = expected.clone();
                    g.sort_unstable();
                    e.sort_unstable();
                    assert_eq!(g, e, "at {now}: got {got:?}, expected {expected:?}");
                    advanced = true;
                }
            }
            assert_eq!(w.len(), model.len() + fired.len());
            check_invariants(&w);
        }
    }

    /// Ties in the heap (sleeps made before the first advance, or beyond the ring) come out in the
    /// order they went to sleep.
    #[test]
    fn heap_ties_wake_in_sleep_order() {
        let mut w = Wheel::default();
        for item in [5, 3, 9, 1] {
            w.sleep(item, 50, None);
        }
        assert_eq!(advance(&mut w, 60), [5, 3, 9, 1]);
        let far = 60 + RING * 3;
        for item in [7, 2, 8] {
            w.sleep(item, far, None);
        }
        assert_eq!(advance(&mut w, far + RING * 2), [7, 2, 8]);
    }

    /// Event lists are reused: a fire, then a new sleep on the same event, reuses the same buffer,
    /// and sleepers on events that never fire leave no stale references behind.
    #[test]
    fn event_lists_are_reused_and_stay_clean() {
        let mut w = Wheel::default();
        advance(&mut w, 0);
        w.sleep(1, 100, Some(5));
        let buffer = w.by_event[&5].as_ptr();
        w.fire(5);
        assert_eq!(w.spare.len(), 1, "the fired list is kept");
        advance(&mut w, 1);
        assert!(w.by_event.is_empty());
        w.sleep(1, 100, Some(5));
        assert_eq!(w.spare.len(), 0, "and taken by the next new key");
        assert_eq!(w.by_event[&5].as_ptr(), buffer, "the fired list was reused");
        w.cancel(1);
        assert_eq!(w.spare.len(), 1, "a list emptied by a cancel is kept too");
        check_invariants(&w);

        // Re-sleeping, cancelling and timed wakes on events that never fire.
        for t in 2..2_000u32 {
            advance(&mut w, t);
            for item in 10..20u32 {
                let event = u8::try_from(item % 3).expect("small");
                match (t + item) % 5 {
                    0 => {
                        w.cancel(item);
                    }
                    _ => {
                        w.sleep(item, t + 1 + (item % 4), Some(event));
                    }
                }
            }
            check_invariants(&w);
        }
        let refs: usize = w.by_event.values().map(Vec::len).sum();
        assert!(refs <= w.len(), "{refs} refs for {} sleepers", w.len());
        assert!(w.by_event.len() <= 4);
    }

    /// Removing sleepers that share one event keeps every other sleeper's recorded position right
    /// (the swap in `release`), whatever the order they leave in.
    #[test]
    fn shared_event_positions_stay_right() {
        let mut w = Wheel::default();
        advance(&mut w, 0);
        for item in 0..50u32 {
            w.sleep(item, 10 + item % 7, Some(1));
        }
        for item in [0, 49, 25, 1, 48, 30, 2] {
            w.cancel(item);
            check_invariants(&w);
        }
        for t in 1..=20 {
            advance(&mut w, t);
            check_invariants(&w);
        }
        assert!(w.is_empty() && w.by_event.is_empty());
    }
}
