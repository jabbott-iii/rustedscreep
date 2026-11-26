//! The scheduler (ROADMAP P1 module 6): CPU tiers, the per-tick budget, time-sliced jobs, a sleep
//! wheel, and per-unit error isolation. All pure Rust; the clock is passed in.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. [`Scheduler::begin`] computes this tick's [`Budget`] from the snapshot's [`TickContext`] (no
//!    JS call) and returns a [`TickCpu`].
//! 2. The subsystems run their jobs through the `TickCpu`: every subsystem's Critical jobs first,
//!    then every Standard job, then every Optional job. The tiers share one ceiling, so this order
//!    across subsystems is what gives Critical its priority, and [`TickStats::out_of_order`] counts
//!    breaches. [`TickCpu::run_units`] runs a list of units (creeps, rooms) round robin, and
//!    [`TickCpu::run_steps`] runs heavy work split into resumable steps. Both stop at the tier's cap
//!    and record a failing unit's [`Fault`] instead of stopping the tick.
//! 3. Dropping the `TickCpu` (or [`TickCpu::finish`]) records the tick for the console summary, and
//!    the wasm side drains the rate-limited fault reports into the log.
//!
//! What lives here between ticks, all in the heap only: the loop count of this global (for the
//! Optional warm-up) and the fault log. Each subsystem keeps its own [`JobState`]s and
//! [`SleepWheel`]s. A global reset loses all of it, which fails open: the next tick starts every
//! cursor at unit 0, every item awake and Optional in warm-up.
//!
//! ```
//! use rustedscreep_core::config::SCHEDULER_MMO;
//! use rustedscreep_core::scheduler::{Clock, Fault, Job, JobState, Scheduler, Tier};
//! use rustedscreep_core::snapshot::TickContext;
//!
//! struct Fixed(f64);
//! impl Clock for Fixed {
//!     fn used(&mut self) -> f64 {
//!         self.0
//!     }
//! }
//!
//! const HAUL: Job = Job { name: "haul", tier: Tier::Standard };
//! let mut sched = Scheduler::default();
//! let mut haul = JobState::default();
//! let ctx = TickContext::from_raw(100, 20, 500.0, 10_000);
//! let mut tick = sched.begin(&ctx, &SCHEDULER_MMO, Fixed(3.0));
//! let report = tick.run_units(&HAUL, &mut haul, 3, |unit, _commit| {
//!     if unit == 1 { Err(Fault::new("no path", unit)) } else { Ok(()) }
//! });
//! assert_eq!((report.ran, report.faults), (3, 1));
//! tick.finish();
//! assert_eq!(sched.faults().total(), 1);
//! ```

mod budget;
mod fault;
mod load;
mod slice;
mod wheel;

pub use budget::{Budget, Cap, Off, Tier};
pub use fault::{Fault, FaultLog, FaultRecord, SUPPRESSED_JOBS, Suppressed};
pub use load::CpuLoad;
pub use slice::{Clock, Commit, Job, JobState, Report, Step, Stop, TickCpu, TickStats};
pub use wheel::{RING, SleepWheel};

use crate::config::SchedulerConfig;
use crate::snapshot::TickContext;

/// The last finished tick, for the console summary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickRecord {
    pub time: u32,
    /// Loops this global ran before that tick (0 on the first tick after a reset).
    pub loop_index: u32,
    pub budget: Budget,
    pub stats: TickStats,
    /// The tick's CPU at the last reading, plus committed CPU (infinity if the clock broke). Unit: CPU.
    pub spent: f64,
    /// CPU committed for the act layer. Unit: CPU.
    pub committed: f64,
    /// CPU spent inside the tick's Optional job calls: from the first to the last clock reading of
    /// each call, with the CPU its units committed. Optional work spends the bucket's surplus
    /// ([`Budget::compute`]), so it grows and shrinks with the bucket; [`Self::base`] leaves it out.
    /// Unit: CPU.
    pub optional: f64,
    pub clock_broken: bool,
}

impl TickRecord {
    /// The tick's base load: its CPU less its Optional work's ([`Self::optional`]), the CPU the
    /// Critical and Standard work and the fixed costs (the loader, the snapshot) took. Infinity if
    /// the clock broke. Unit: CPU.
    ///
    /// Like [`Self::spent`] it ends at the tick's last clock reading, so it leaves out the traffic
    /// manager's own work after that reading and everything after the decide step: the act step's
    /// cost beyond `INTENT_CPU_COST` an intent, the log, and persistence writes. Some Optional cost
    /// lands in it: the intents of the moves the traffic manager resolves for creeps Optional work
    /// runs (the explorer, the signer), the snapshot's reads of rooms they or an observer see, and an
    /// Optional call that had a bad clock reading.
    pub fn base(&self) -> f64 {
        if self.clock_broken || !self.spent.is_finite() {
            f64::INFINITY
        } else {
            (self.spent - self.optional).max(0.0)
        }
    }
}

/// The scheduler's state for one global.
#[derive(Default)]
pub struct Scheduler {
    loops: u32,
    faults: FaultLog,
    last: Option<TickRecord>,
}

impl Scheduler {
    /// Starts a tick: computes the budget, opens the fault window, and counts the loop.
    pub fn begin<'a, C: Clock>(
        &'a mut self,
        ctx: &TickContext,
        cfg: &'a SchedulerConfig,
        clock: C,
    ) -> TickCpu<'a, C> {
        let loop_index = self.loops;
        self.loops = self.loops.saturating_add(1);
        let budget = Budget::compute(ctx, cfg, loop_index);
        self.faults.begin_tick(ctx.time, cfg);
        TickCpu::new(self, cfg, clock, ctx.time, loop_index, budget)
    }

    /// Loops begun in this global.
    pub fn loops(&self) -> u32 {
        self.loops
    }

    /// The last finished tick, if any.
    pub fn last_tick(&self) -> Option<&TickRecord> {
        self.last.as_ref()
    }

    pub fn faults(&self) -> &FaultLog {
        &self.faults
    }

    /// For draining the fault reports into the log.
    pub fn faults_mut(&mut self) -> &mut FaultLog {
        &mut self.faults
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEDULER_MMO;
    use rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX};

    struct Zero;
    impl Clock for Zero {
        fn used(&mut self) -> f64 {
            0.0
        }
    }

    /// The first tick of a global never allows Optional work; later ones do (with a full bucket),
    /// and each tick's record keeps its loop index and budget.
    #[test]
    fn the_first_tick_of_a_global_runs_no_optional_work() {
        let cfg = SCHEDULER_MMO;
        let bucket = i32::try_from(CPU_BUCKET_MAX).expect("fits");
        let ctx = |t| TickContext::from_raw(t, 20, f64::from(CPU_TICK_LIMIT_MAX), bucket);
        let mut sched = Scheduler::default();
        for t in 0..cfg.optional_warmup_loops + 2 {
            let tick = sched.begin(&ctx(100 + t), &cfg, Zero);
            let warm = t >= cfg.optional_warmup_loops;
            assert_eq!(tick.allows(Tier::Optional), warm, "loop {t}");
            assert!(tick.allows(Tier::Critical) && tick.allows(Tier::Standard));
            tick.finish();
            let rec = sched.last_tick().expect("recorded");
            assert_eq!((rec.time, rec.loop_index), (100 + t, t));
            assert_eq!(rec.budget, Budget::compute(&ctx(100 + t), &cfg, t));
        }
        assert_eq!(sched.loops(), cfg.optional_warmup_loops + 2);
        // A global reset is a new Scheduler: warm-up starts again.
        let mut fresh = Scheduler::default();
        assert!(!fresh.begin(&ctx(200), &cfg, Zero).allows(Tier::Optional));
    }

    /// `begin` opens and rotates the fault window: a unit failing every tick for three windows and
    /// one tick is reported `fault_reports_per_window` times per window, with one note per closed
    /// window.
    #[test]
    fn begin_rotates_the_fault_window() {
        const J: Job = Job {
            name: "j",
            tier: Tier::Critical,
        };
        let cfg = SCHEDULER_MMO;
        let bucket = i32::try_from(CPU_BUCKET_MAX).expect("fits");
        let ctx = |t| TickContext::from_raw(t, 20, f64::from(CPU_TICK_LIMIT_MAX), bucket);
        let mut sched = Scheduler::default();
        let (mut reports, mut notes) = (0u32, 0u32);
        for t in 0..cfg.fault_window_ticks * 3 + 1 {
            let mut tick = sched.begin(&ctx(1000 + t), &cfg, Zero);
            tick.isolate(&J, Err(Fault::new("x", 0)));
            tick.finish();
            let (r, n) = sched.faults_mut().drain();
            reports += u32::try_from(r.count()).expect("small");
            notes += u32::from(n.is_some());
        }
        assert_eq!(reports, cfg.fault_reports_per_window * 3 + 1);
        assert_eq!(notes, 3);
    }
}
