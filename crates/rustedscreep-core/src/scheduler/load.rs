//! The bot's CPU load across loops (ROADMAP P8.4): a running mean of each loop's base load
//! ([`TickRecord::base`]: the tick's CPU less its Optional work's), for a decision that adds work
//! for a long time, such as claiming a room ([`crate::expansion::Claiming`]).
//!
//! # Why the base load, not the whole tick
//!
//! Optional work spends the bucket's surplus: above `scheduler.bucket_target` its cap is above
//! `Game.cpu.limit` ([`super::Budget::compute`]), so with a full bucket a tick may use more than the
//! limit while the bucket stays full. The whole tick's mean would then read near the limit with
//! little steady work behind it. The base load is the CPU that does not shrink when the bucket
//! does: the Critical and Standard work, the loader's and the snapshot's fixed costs, and the
//! intents they commit.
//!
//! # The mean
//!
//! For the first `length` loops it is the plain mean of the loops so far; after that each loop moves
//! it by `1 / length` of its distance to the loop's base load (an exponential moving average), so
//! the last `length` loops carry about 63% of its weight. It holds three numbers, takes no clock
//! reading of its own and allocates nothing. It is heap state: a global reset starts it again.
//!
//! Two kinds of loop are left out, since neither shows the steady load:
//! - a global's first loop: its instantiation and every cache built at once;
//! - a loop with the Standard tier off (the bucket below `scheduler.standard_bucket_min`): it ran
//!   only the Critical work, so its base load is lower than the bot's, and it comes exactly when
//!   the bot has been spending more than its limit.

use super::{TickRecord, Tier};

/// A running mean of the base load (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuLoad {
    mean: f64,
    loops: u32,
    /// `Game.time` of the last record folded in.
    last: Option<u32>,
}

impl CpuLoad {
    /// Folds in one finished tick over a mean of `length` loops (0 reads as 1). Left out: the record
    /// already folded in (same `time`), a global's first loop, a tick with the Standard tier off,
    /// and a tick whose clock broke.
    pub fn add(&mut self, rec: &TickRecord, length: u32) {
        if self.last == Some(rec.time) {
            return;
        }
        self.last = Some(rec.time);
        let base = rec.base();
        if rec.loop_index == 0 || !rec.budget.allows(Tier::Standard) || !base.is_finite() {
            return;
        }
        self.loops = self.loops.saturating_add(1);
        let n = self.loops.min(length.max(1));
        self.mean += (base - self.mean) / f64::from(n);
    }

    /// The mean, once at least `length` loops are in it. Unit: CPU a tick.
    pub fn mean(&self, length: u32) -> Option<f64> {
        (self.loops >= length.max(1)).then_some(self.mean)
    }

    /// Loops folded in this global.
    pub fn loops(&self) -> u32 {
        self.loops
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEDULER_MMO;
    use crate::scheduler::{Budget, TickStats};
    use crate::snapshot::TickContext;

    fn record(time: u32, loop_index: u32, spent: f64, optional: f64) -> TickRecord {
        at_bucket(time, loop_index, spent, optional, 10_000)
    }

    fn at_bucket(time: u32, loop_index: u32, spent: f64, optional: f64, bucket: i32) -> TickRecord {
        let ctx = TickContext::from_raw(time, 20, 500.0, bucket);
        TickRecord {
            time,
            loop_index,
            budget: Budget::compute(&ctx, &SCHEDULER_MMO, loop_index),
            stats: TickStats::default(),
            spent,
            committed: 0.0,
            optional,
            clock_broken: false,
        }
    }

    /// The first loop of a global, a repeated record, a broken clock and a tick with the Standard
    /// tier off are left out; the mean is the plain mean of the base loads until `length` loops are
    /// in, and only then given.
    #[test]
    fn the_mean_waits_for_its_length() {
        let length = 4;
        let mut load = CpuLoad::default();
        load.add(&record(10, 0, 50.0, 0.0), length);
        assert_eq!(load.loops(), 0);
        let bases = [2.0, 4.0, 3.0];
        for (i, &b) in bases.iter().enumerate() {
            let t = 11 + u32::try_from(i).expect("fits");
            // Optional work on top of the base is left out.
            load.add(&record(t, t - 10, b + 7.5, 7.5), length);
            load.add(&record(t, t - 10, 99.0, 0.0), length);
        }
        let broken = TickRecord {
            clock_broken: true,
            ..record(14, 4, 1.0, 0.0)
        };
        load.add(&broken, length);
        // Below the Standard gate only Critical work ran: not the steady load.
        let low = i32::try_from(SCHEDULER_MMO.standard_bucket_min).expect("fits") - 1;
        let starved = at_bucket(16, 6, 0.5, 0.0, low);
        assert!(!starved.budget.allows(Tier::Standard));
        load.add(&starved, length);
        assert_eq!(load.loops(), 3);
        assert_eq!(load.mean(length), None);
        load.add(&record(17, 7, 3.0, 0.0), length);
        let mean = load.mean(length).expect("full");
        assert!(
            (mean - (2.0 + 4.0 + 3.0 + 3.0) / 4.0).abs() < 1e-12,
            "{mean}"
        );
    }

    /// Past its length the mean moves `1 / length` of the way to each new loop, so it converges on
    /// a steady load and follows a step in it within a few lengths; it never leaves the range of
    /// the loads it saw.
    #[test]
    fn the_mean_follows_the_load() {
        let length = 100;
        let mut load = CpuLoad::default();
        let mut t = 1u32;
        for _ in 0..length {
            load.add(&record(t, t, 2.0, 0.0), length);
            t += 1;
        }
        assert!((load.mean(length).expect("full") - 2.0).abs() < 1e-12);
        let mut last = 2.0;
        for _ in 0..length * 5 {
            load.add(&record(t, t, 7.0, 1.0), length);
            t += 1;
            let m = load.mean(length).expect("full");
            assert!(m >= last && m <= 6.0, "{m}");
            last = m;
        }
        // After 5 lengths the step's gap is (1 - 1/length)^(5 length) of its start, below e^-5.
        let gap = 6.0 - last;
        assert!(gap > 0.0 && gap < 4.0 * (-5.0f64).exp(), "{gap}");
    }

    /// A zero length reads as 1: the mean is the last loop's base load.
    #[test]
    fn a_zero_length_reads_as_one() {
        let mut load = CpuLoad::default();
        load.add(&record(1, 1, 3.0, 0.0), 0);
        load.add(&record(2, 2, 5.0, 0.0), 0);
        assert_eq!(load.mean(0), Some(5.0));
    }
}
