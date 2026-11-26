//! Time slicing within one tick (ROADMAP P1 module 6): [`TickCpu`] holds this tick's [`Budget`] and
//! the clock, and runs a job's units or steps until the job's tier reaches its cap.
//!
//! The clock (`Game.cpu.getUsed()` in rustedscreep-wasm) is a JS call, so it is read sparsely: once
//! when a job starts (unless the tick is already at the job's cap) and once after each batch of
//! units. The first batch of every call is one unit, so a cost that changed since the last tick is
//! measured before it is trusted. After that a batch is at most `max_units_per_sample` units, and at
//! the measured cost per unit it spends at most half of what is left before the cap and at most a
//! quarter of the tick's reserve. So a cost that jumps up to 4 times inside a batch passes the cap
//! by at most half the reserve, and one that jumps up to 6 times by at most the reserve (apart from
//! a single unit that alone costs more than that). Every reading is an absolute level of the tick's
//! CPU (it already includes the loader, the snapshot and the intents made so far, VERIFICATION_LOG
//! V106), so nothing assumes the tick started at 0.
//!
//! Read the clock only in the decide step: the act layer issues the committed intents after
//! [`TickCpu::finish`], so a reading taken after them would count them twice.

use super::budget::{Budget, Tier};
use super::fault::{Fault, FaultRecord};
use super::{Scheduler, TickRecord};
use crate::config::SchedulerConfig;
use rustedscreep_consts::INTENT_CPU_COST;

/// The tick's CPU clock: `Game.cpu.getUsed()` in the game, a scripted reading in tests.
pub trait Clock {
    /// CPU used so far this tick. Unit: CPU (ms).
    fn used(&mut self) -> f64;
}

/// A job's fixed description. Each subsystem declares its jobs as constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    /// Shown in fault reports and summaries.
    pub name: &'static str,
    pub tier: Tier,
}

/// A job's state between ticks: where its round robin resumes and what one unit costs. It lives
/// in the heap with the subsystem that owns the job; after a global reset it starts at unit 0 with
/// no estimate, which only costs a few more clock samples on the first tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JobState {
    next: u32,
    unit_cost: Option<f64>,
}

impl JobState {
    /// The unit the next tick starts at (before it is reduced modulo that tick's unit count).
    pub fn next(&self) -> u32 {
        self.next
    }

    /// CPU per unit (or step), as measured by the last batches. Unit: CPU.
    pub fn unit_cost(&self) -> Option<f64> {
        self.unit_cost
    }

    /// Folds in one batch's cost per unit: a higher cost replaces the estimate at once, a lower one
    /// moves it halfway, so the estimate errs high and the next batch errs small.
    fn observe(&mut self, per_unit: f64) {
        if !per_unit.is_finite() {
            return;
        }
        let measured = per_unit.max(0.0);
        self.unit_cost = Some(match self.unit_cost {
            Some(old) if measured < old => (old + measured) / 2.0,
            _ => measured,
        });
    }
}

/// What a step job's step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// More steps remain.
    More,
    /// The job's work is complete.
    Done,
}

/// Why a job stopped this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Every unit ran (a unit job) or a step returned [`Step::Done`] (a step job).
    Done,
    /// The tier's cap was reached; the job resumes next tick.
    Budget,
    /// The job's tier does not run this tick; nothing ran.
    TierOff,
    /// A step failed; the fault is logged, and the job decides whether to restart its work.
    Fault,
}

/// The outcome of one `run_units` or `run_steps` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// Units or steps run, failed ones included.
    pub ran: u32,
    pub faults: u32,
    pub stop: Stop,
}

/// CPU a unit commits for later in the tick: the intents it queues for the act layer, which the
/// clock sees only once they are issued (V106). The budget counts it as spent at once. A unit can
/// also mark itself heavy ([`Commit::heavy`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Commit {
    cpu: f64,
    heavy: bool,
}

impl Commit {
    /// Adds `cpu` (ignored unless it is above 0; NaN is ignored). Unit: CPU.
    pub fn add(&mut self, cpu: f64) {
        if cpu > 0.0 {
            self.cpu += cpu;
        }
    }

    /// Adds `count` state-changing intents. Uses: `INTENT_CPU_COST` (0.2 CPU per intent the engine
    /// accepts, V34).
    pub fn intents(&mut self, count: u32) {
        self.add(f64::from(count) * INTENT_CPU_COST);
    }

    /// Total committed this tick. Unit: CPU.
    pub fn cpu(&self) -> f64 {
        self.cpu
    }

    /// Marks the unit now running as heavy: work far above the job's usual cost per unit (a flow
    /// field build, a path search, a room scan). The slicer then reads the clock right after the
    /// unit, ending its batch, and leaves that batch out of the cost estimate, so one heavy unit can
    /// neither run on into the reserve with cheap units behind it nor shrink the following batches.
    /// Outside a sliced job it has no effect.
    pub fn heavy(&mut self) {
        self.heavy = true;
    }
}

/// This tick's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickStats {
    /// Clock readings (each one JS call in the game).
    pub samples: u32,
    /// Readings that were NaN, infinite or negative; they keep the last good reading.
    pub bad_samples: u32,
    /// Units and steps run.
    pub units: u32,
    pub faults: u32,
    /// Jobs stopped by their tier's cap.
    pub budget_stops: u32,
    /// Job calls skipped because their tier was off.
    pub tier_off: u32,
    /// Job calls whose tier came after a later tier had already run this tick. A tick runs every
    /// subsystem's Critical jobs before any Standard job, and those before any Optional job (the
    /// tiers share one ceiling, so order is what gives Critical its priority); this counts breaches.
    pub out_of_order: u32,
}

/// One tick's scheduler: the budget, the clock and the counters. Made by [`Scheduler::begin`];
/// dropping it (or [`Self::finish`]) records the tick for the summary.
pub struct TickCpu<'a, C: Clock> {
    sched: &'a mut Scheduler,
    cfg: &'a SchedulerConfig,
    clock: C,
    time: u32,
    loop_index: u32,
    budget: Budget,
    /// The highest good reading this tick.
    used: f64,
    commit: Commit,
    clock_broken: bool,
    /// The highest tier a job call has used this tick.
    highest: Option<Tier>,
    /// CPU spent inside Optional job calls this tick (see [`TickRecord::optional`]).
    optional: f64,
    stats: TickStats,
}

impl<'a, C: Clock> TickCpu<'a, C> {
    pub(super) fn new(
        sched: &'a mut Scheduler,
        cfg: &'a SchedulerConfig,
        clock: C,
        time: u32,
        loop_index: u32,
        budget: Budget,
    ) -> Self {
        Self {
            sched,
            cfg,
            clock,
            time,
            loop_index,
            budget,
            used: 0.0,
            commit: Commit::default(),
            clock_broken: false,
            highest: None,
            optional: 0.0,
            stats: TickStats::default(),
        }
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Whether `tier` runs at all this tick (no clock read).
    pub fn allows(&self, tier: Tier) -> bool {
        self.budget.allows(tier)
    }

    /// `Game.time` of this tick.
    pub fn time(&self) -> u32 {
        self.time
    }

    /// Reads the clock and returns the tick's CPU so far (the reading plus committed CPU).
    ///
    /// The reading is sanitized: one lower than the last is raised to it (`getUsed` drops 0.2 after
    /// a `cancelOrder`, V106); a NaN, infinite or negative one keeps the last good reading, and once
    /// `bad_samples_to_break` such readings came in a tick the clock counts as broken, which stops
    /// every tier for the rest of the tick (ADR-013). Stopping costs that tick's remaining
    /// work; running past `tickLimit` blind would risk an aborted tick and a global reset (ADR-003,
    /// V105).
    pub fn sample(&mut self) -> f64 {
        self.stats.samples = self.stats.samples.saturating_add(1);
        let reading = self.clock.used();
        if reading.is_finite() && reading >= 0.0 {
            if reading > self.used {
                self.used = reading;
            }
        } else {
            self.stats.bad_samples = self.stats.bad_samples.saturating_add(1);
            if self.stats.bad_samples >= self.cfg.bad_samples_to_break.max(1) {
                self.clock_broken = true;
            }
        }
        self.spent()
    }

    /// The tick's CPU so far without reading the clock: the last reading plus committed CPU, or
    /// infinity once the clock is broken.
    pub fn spent(&self) -> f64 {
        if self.clock_broken {
            f64::INFINITY
        } else {
            self.used + self.commit.cpu
        }
    }

    /// CPU left before `tier`'s cap (reads the clock); 0 when the tier is off or the cap is reached.
    pub fn remaining(&mut self, tier: Tier) -> f64 {
        let Some(cap) = self.budget.cap(tier) else {
            return 0.0;
        };
        let left = cap - self.sample();
        if left > 0.0 { left } else { 0.0 }
    }

    /// Commits CPU outside a sliced job (see [`Commit`]).
    pub fn commit(&mut self) -> &mut Commit {
        &mut self.commit
    }

    /// Records the fault of a unit that ran outside a sliced job, so it is isolated the same way.
    /// Returns whether the unit succeeded. It does not gate the work: check [`Self::allows`] or
    /// [`Self::remaining`] for the job's tier before running it.
    pub fn isolate(&mut self, job: &Job, result: Result<(), Fault>) -> bool {
        match result {
            Ok(()) => true,
            Err(fault) => {
                self.stats.faults = self.stats.faults.saturating_add(1);
                self.record(job, fault);
                false
            }
        }
    }

    /// Runs units `0..count` of `job` round robin from where it stopped last tick, until all ran or
    /// the tier's cap is reached. Each unit is isolated: an `Err` is logged (rate-limited) and the
    /// next unit runs.
    ///
    /// Pass the units in a stable order (by handle slot or id, not by snapshot row, whose order the
    /// engine does not keep, V102), or a unit can be skipped on consecutive ticks under a tight cap.
    pub fn run_units<F>(
        &mut self,
        job: &Job,
        state: &mut JobState,
        count: u32,
        mut unit: F,
    ) -> Report
    where
        F: FnMut(u32, &mut Commit) -> Result<(), Fault>,
    {
        self.slice(job, state, Some(count), |i, commit| {
            unit(i, commit).map(|()| Step::More)
        })
    }

    /// Runs `job`'s steps (heavy work split into resumable pieces, such as a planner pass) until one
    /// returns [`Step::Done`], the tier's cap is reached, or a step fails. The job keeps its own
    /// progress; the step index passed in counts this tick's steps.
    pub fn run_steps<F>(&mut self, job: &Job, state: &mut JobState, mut step: F) -> Report
    where
        F: FnMut(u32, &mut Commit) -> Result<Step, Fault>,
    {
        self.slice(job, state, None, |i, commit| step(i, commit))
    }

    /// Records the tick for the summary. Dropping does the same; this only reads better.
    pub fn finish(self) {}

    fn record(&mut self, job: &Job, fault: Fault) {
        let record = FaultRecord {
            time: self.time,
            job: job.name,
            fault,
        };
        self.sched.faults.record(record, self.cfg);
    }

    /// The shared loop. `count` is `Some(n)` for a unit job (indices wrap from the saved cursor) and
    /// `None` for a step job (indices count up from 0, and a fault stops the job).
    fn slice<F>(
        &mut self,
        job: &Job,
        state: &mut JobState,
        count: Option<u32>,
        mut run: F,
    ) -> Report
    where
        F: FnMut(u32, &mut Commit) -> Result<Step, Fault>,
    {
        if self.highest.is_some_and(|h| job.tier < h) {
            self.stats.out_of_order = self.stats.out_of_order.saturating_add(1);
        }
        self.highest = self.highest.max(Some(job.tier));
        let Some(cap) = self.budget.cap(job.tier) else {
            self.stats.tier_off = self.stats.tier_off.saturating_add(1);
            return Report {
                ran: 0,
                faults: 0,
                stop: Stop::TierOff,
            };
        };
        if count == Some(0) {
            return Report {
                ran: 0,
                faults: 0,
                stop: Stop::Done,
            };
        }
        // A reading never lowers `spent()` (`used` only rises and commits only add), so a job that
        // starts at or past its cap would stop without running: skip the clock read.
        if self.spent() >= cap {
            self.stats.budget_stops = self.stats.budget_stops.saturating_add(1);
            return Report {
                ran: 0,
                faults: 0,
                stop: Stop::Budget,
            };
        }
        let start = count.map_or(0, |n| state.next % n);
        let max_batch = self.cfg.max_units_per_sample.max(1);
        let reserve = self.budget.reserve;
        let mut ran = 0u32;
        let mut faults = 0u32;
        let mut first = true;
        let bad_before = self.stats.bad_samples;
        let mut spent = self.sample();
        let opened = spent;
        let stop = loop {
            let left = cap - spent;
            if left.is_nan() || left <= 0.0 {
                break Stop::Budget;
            }
            // The first batch of a call re-measures the cost, which may have changed since the
            // estimate was made.
            let batch = if first {
                1
            } else {
                batch_size(left, reserve, state.unit_cost, max_batch)
            };
            first = false;
            let before = spent;
            let mut in_batch = 0u32;
            let mut stopped = None;
            let mut heavy = false;
            while in_batch < batch {
                let index = match count {
                    Some(n) if ran >= n => {
                        stopped = Some(Stop::Done);
                        break;
                    }
                    Some(n) => wrap(start, ran, n),
                    None => ran,
                };
                self.commit.heavy = false;
                let outcome = run(index, &mut self.commit);
                heavy = std::mem::take(&mut self.commit.heavy);
                ran = ran.saturating_add(1);
                in_batch += 1;
                match outcome {
                    Ok(Step::More) => {}
                    Ok(Step::Done) => {
                        stopped = Some(Stop::Done);
                        break;
                    }
                    Err(fault) => {
                        faults = faults.saturating_add(1);
                        self.record(job, fault);
                        if count.is_none() {
                            stopped = Some(Stop::Fault);
                            break;
                        }
                    }
                }
                if heavy {
                    break;
                }
            }
            if stopped.is_none() && count.is_some_and(|n| ran >= n) {
                stopped = Some(Stop::Done);
            }
            spent = self.sample();
            if in_batch > 0 && !heavy {
                state.observe((spent - before) / f64::from(in_batch));
            }
            // After a heavy unit the next batch is one unit again, so the cost of the ordinary units
            // is measured on this tick rather than trusted from the last.
            if heavy {
                first = true;
            }
            if let Some(stop) = stopped {
                break stop;
            }
        };
        if let Some(n) = count {
            state.next = wrap(start, ran.min(n), n);
        }
        // `spent` is the call's last reading here: the Optional work's CPU, with what it committed,
        // from the readings the slicer took anyway (no clock read of its own). A bad reading in the
        // call (it keeps the last good one, so `opened` may be from before the call) leaves the
        // call in the base load, the side that holds a claim back.
        let used = spent - opened;
        if job.tier == Tier::Optional
            && self.stats.bad_samples == bad_before
            && used.is_finite()
            && used > 0.0
        {
            self.optional += used;
        }
        self.stats.units = self.stats.units.saturating_add(ran);
        self.stats.faults = self.stats.faults.saturating_add(faults);
        if stop == Stop::Budget {
            self.stats.budget_stops = self.stats.budget_stops.saturating_add(1);
        }
        Report { ran, faults, stop }
    }
}

impl<C: Clock> Drop for TickCpu<'_, C> {
    fn drop(&mut self) {
        self.sched.last = Some(TickRecord {
            time: self.time,
            loop_index: self.loop_index,
            budget: self.budget,
            stats: self.stats,
            spent: self.spent(),
            committed: self.commit.cpu,
            optional: self.optional,
            clock_broken: self.clock_broken,
        });
    }
}

/// Units to run before the next clock sample: 1 while the cost is unknown, otherwise as many as fit,
/// at the measured cost, in half of what is left before the cap and in a quarter of the reserve,
/// clamped to `1..=max`. A unit too cheap to measure (cost 0) runs `max` at a time.
fn batch_size(left: f64, reserve: f64, unit_cost: Option<f64>, max: u32) -> u32 {
    match unit_cost {
        None => 1,
        Some(cost) if cost.is_nan() || cost <= 0.0 => max,
        Some(cost) => {
            let room = (left / 2.0).min(reserve / 4.0);
            let fit = (room / cost).floor();
            if fit >= f64::from(max) {
                max
            } else if fit >= 1.0 {
                // 1 <= fit < max <= u32::MAX, so the cast is exact.
                fit as u32
            } else {
                1
            }
        }
    }
}

/// `(start + offset) % n` without overflow, for `start < n` and `offset <= n`.
fn wrap(start: u32, offset: u32, n: u32) -> u32 {
    let sum = u64::from(start) + u64::from(offset);
    u32::try_from(sum % u64::from(n.max(1))).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEDULER_MMO;
    use crate::scheduler::Scheduler;
    use crate::snapshot::TickContext;
    use rustedscreep_consts::{CPU_BUCKET_MAX, CPU_TICK_LIMIT_MAX};
    use std::cell::Cell;
    use std::rc::Rc;

    /// A clock the test advances by hand; readings can be overridden to inject bad values.
    #[derive(Clone, Default)]
    struct FakeClock {
        now: Rc<Cell<f64>>,
        reads: Rc<Cell<u32>>,
        script: Rc<std::cell::RefCell<Vec<f64>>>,
    }

    impl FakeClock {
        fn spend(&self, cpu: f64) {
            self.now.set(self.now.get() + cpu);
        }
    }

    impl Clock for FakeClock {
        fn used(&mut self) -> f64 {
            self.reads.set(self.reads.get() + 1);
            let scripted = self.script.borrow_mut().pop();
            scripted.unwrap_or_else(|| self.now.get())
        }
    }

    const CRIT: Job = Job {
        name: "crit",
        tier: Tier::Critical,
    };
    const OPT: Job = Job {
        name: "opt",
        tier: Tier::Optional,
    };

    fn full_tick(time: u32) -> TickContext {
        TickContext::from_raw(
            time,
            20,
            f64::from(CPU_TICK_LIMIT_MAX),
            i32::try_from(CPU_BUCKET_MAX).expect("fits"),
        )
    }

    /// With room for all of them, every unit runs once, in order from the cursor, and the cursor
    /// stays put for the next tick.
    #[test]
    fn all_units_run_when_the_budget_allows() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut seen = Vec::new();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        let report = tick.run_units(&CRIT, &mut state, 40, |i, _| {
            clock.spend(0.01);
            seen.push(i);
            Ok(())
        });
        tick.finish();
        assert_eq!(
            report,
            Report {
                ran: 40,
                faults: 0,
                stop: Stop::Done
            }
        );
        assert_eq!(seen, (0..40).collect::<Vec<_>>());
        assert_eq!(state.next(), 0);
        // One sample to start plus one per batch: the first batch of a call is 1 unit, then batches
        // of max_units_per_sample.
        let max = SCHEDULER_MMO.max_units_per_sample;
        let batches = 1 + (40 - 1u32).div_ceil(max);
        assert_eq!(clock.reads.get(), 1 + batches);
    }

    /// Under a tight cap the job stops, and the next tick resumes at the first unit that did not
    /// run, so every unit gets its turn across ticks.
    #[test]
    fn a_tight_cap_resumes_round_robin() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let n = 50u32;
        let cost = 1.0;
        let mut visits = vec![0u32; 50];
        let mut ticks = 0;
        while visits.contains(&0) {
            ticks += 1;
            clock.now.set(0.0);
            let mut tick = sched.begin(&full_tick(ticks), &cfg, clock.clone());
            let cap = tick.budget().ceiling;
            // Spend most of the ceiling before the job, leaving room for about 10 units.
            clock.spend(cap - 10.0);
            let report = tick.run_units(&CRIT, &mut state, n, |i, _| {
                clock.spend(cost);
                if let Some(v) = visits.get_mut(usize::try_from(i).expect("small")) {
                    *v += 1;
                }
                Ok(())
            });
            assert_eq!(report.stop, Stop::Budget);
            assert!(report.ran >= 1 && report.ran < n);
            // The overshoot past the cap is at most one unit's cost.
            assert!(
                tick.spent() <= cap + cost,
                "{} > {}",
                tick.spent(),
                cap + cost
            );
            assert!(ticks < 20, "round robin must cover every unit");
        }
        assert!(visits.iter().all(|&v| v >= 1));
        let spread = visits.iter().max().expect("some") - visits.iter().min().expect("some");
        assert!(spread <= 1, "visits stay even: {visits:?}");
    }

    /// A failing unit is recorded with the job's name and skipped; the other units still run.
    #[test]
    fn a_failing_unit_is_isolated() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut ran = Vec::new();
        let mut tick = sched.begin(&full_tick(7), &SCHEDULER_MMO, clock.clone());
        let report = tick.run_units(&CRIT, &mut state, 5, |i, _| {
            ran.push(i);
            if i == 2 {
                Err(Fault::new("boom", 200 + i))
            } else {
                Ok(())
            }
        });
        tick.finish();
        assert_eq!((report.ran, report.faults, report.stop), (5, 1, Stop::Done));
        assert_eq!(ran, [0, 1, 2, 3, 4]);
        let last = sched.faults().last().expect("recorded");
        assert_eq!(
            last,
            FaultRecord {
                time: 7,
                job: "crit",
                fault: Fault::new("boom", 202)
            }
        );
        assert_eq!(sched.last_tick().expect("recorded").stats.faults, 1);
    }

    /// A tier that is off runs nothing and reads no clock.
    #[test]
    fn an_off_tier_runs_nothing() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        // Loop 0 of a new global: Optional is in warm-up.
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        let report = tick.run_units(&OPT, &mut state, 10, |_, _| {
            panic!("must not run");
        });
        assert_eq!(report.stop, Stop::TierOff);
        drop(tick);
        assert_eq!(clock.reads.get(), 0);
        assert_eq!(sched.last_tick().expect("recorded").stats.tier_off, 1);
    }

    /// A step job runs until it is done, resumes next tick after a budget stop, and stops on a fault.
    #[test]
    fn step_jobs_resume_and_stop_on_faults() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut progress = 0u32;
        let total = 30u32;
        let mut ticks = 0;
        loop {
            ticks += 1;
            clock.now.set(0.0);
            let mut tick = sched.begin(&full_tick(ticks), &cfg, clock.clone());
            clock.spend(tick.budget().ceiling - 5.0);
            let report = tick.run_steps(&CRIT, &mut state, |_, _| {
                clock.spend(1.0);
                progress += 1;
                Ok(if progress == total {
                    Step::Done
                } else {
                    Step::More
                })
            });
            if report.stop == Stop::Done {
                break;
            }
            assert_eq!(report.stop, Stop::Budget);
            assert!(ticks < 30);
        }
        assert_eq!(progress, total);
        assert!(ticks > 1, "the work spanned several ticks");

        clock.now.set(0.0);
        let mut tick = sched.begin(&full_tick(100), &cfg, clock.clone());
        let mut calls = 0;
        let report = tick.run_steps(&CRIT, &mut state, |i, _| {
            calls += 1;
            if i == 1 {
                Err(Fault::new("bad step", 0))
            } else {
                Ok(Step::More)
            }
        });
        assert_eq!(
            (report.ran, report.faults, report.stop),
            (2, 1, Stop::Fault)
        );
        assert_eq!(calls, 2);
    }

    /// Uses: INTENT_CPU_COST. Committed intent CPU counts as spent before the clock sees it.
    #[test]
    fn committed_intents_count_against_the_cap() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        let cap = tick.budget().ceiling;
        // Each unit costs nothing on the clock but queues 10 intents.
        let per_unit = 10.0 * INTENT_CPU_COST;
        let report = tick.run_units(&CRIT, &mut state, 10_000, |_, commit| {
            commit.intents(10);
            Ok(())
        });
        assert_eq!(report.stop, Stop::Budget);
        let committed = f64::from(report.ran) * per_unit;
        assert!((tick.spent() - committed).abs() < 1e-6);
        assert!(committed >= cap && committed <= cap + per_unit);
    }

    /// The clock is sanitized: a lower reading keeps the higher one, one bad reading is ignored,
    /// and a second one stops every tier for the rest of the tick.
    #[test]
    fn bad_clock_readings() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        clock.now.set(5.0);
        assert_eq!(tick.sample(), 5.0);
        clock.now.set(4.8); // a cancelOrder refunds 0.2 (V106)
        assert_eq!(tick.sample(), 5.0);
        clock.script.borrow_mut().push(f64::NAN);
        assert_eq!(
            tick.sample(),
            5.0,
            "one bad reading keeps the last good one"
        );
        assert!(tick.remaining(Tier::Critical) > 0.0);
        clock.script.borrow_mut().push(-1274.0); // the kind of value a user reported (V106, unverified)
        assert_eq!(
            tick.sample(),
            f64::INFINITY,
            "a second one breaks the clock"
        );
        let mut state = JobState::default();
        let report = tick.run_units(&CRIT, &mut state, 3, |_, _| Ok(()));
        assert_eq!((report.ran, report.stop), (0, Stop::Budget));
        drop(tick);
        let rec = sched.last_tick().expect("recorded");
        assert!(rec.clock_broken);
        assert_eq!(rec.stats.bad_samples, 2);
        // The next tick starts with a working clock.
        let mut tick = sched.begin(&full_tick(2), &SCHEDULER_MMO, clock.clone());
        assert!(tick.remaining(Tier::Critical) > 0.0);
    }

    /// The cursor survives the unit count shrinking, and an empty list reads no clock.
    #[test]
    fn the_cursor_wraps_when_units_disappear() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState {
            next: 7,
            unit_cost: Some(0.0),
        };
        let mut order = Vec::new();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        tick.run_units(&CRIT, &mut state, 5, |i, _| {
            order.push(i);
            Ok(())
        });
        // A full round from 7 % 5 visits every unit once and leaves the cursor where it started.
        let start = 7 % 5;
        let expected: Vec<u32> = (0..5).map(|k| (start + k) % 5).collect();
        assert_eq!(order, expected);
        assert_eq!(state.next(), start);
        let reads = clock.reads.get();
        let report = tick.run_units(&CRIT, &mut state, 0, |_, _| Ok(()));
        assert_eq!(report.stop, Stop::Done);
        assert_eq!(clock.reads.get(), reads);
    }

    /// The cost estimate rises at once and falls halfway, and batches follow it, within half of what
    /// is left and a quarter of the reserve.
    #[test]
    fn batches_follow_the_cost_estimate() {
        let max = SCHEDULER_MMO.max_units_per_sample;
        let wide = 1000.0; // a reserve that does not bind
        let mut s = JobState::default();
        assert_eq!(batch_size(100.0, wide, s.unit_cost, max), 1);
        s.observe(1.0);
        assert_eq!(s.unit_cost(), Some(1.0));
        s.observe(3.0);
        assert_eq!(s.unit_cost(), Some(3.0));
        s.observe(1.0);
        assert_eq!(s.unit_cost(), Some(2.0));
        s.observe(f64::NAN);
        assert_eq!(s.unit_cost(), Some(2.0));
        assert_eq!(batch_size(1000.0, wide, Some(2.0), max), max);
        assert_eq!(batch_size(20.0, wide, Some(2.0), max), 5);
        assert_eq!(batch_size(1.0, wide, Some(2.0), max), 1);
        assert_eq!(batch_size(1.0, wide, Some(0.0), max), max);
        // A reserve of 20 allows 5 CPU per batch: 2 units at 2.0, even with 1000 left.
        assert_eq!(batch_size(1000.0, 20.0, Some(2.0), max), 2);
        assert_eq!(batch_size(1000.0, 0.0, Some(2.0), max), 1);
        assert_eq!(wrap(u32::MAX - 1, u32::MAX, u32::MAX), u32::MAX - 1);
    }

    /// Uses: CPU_TICK_LIMIT_MAX. A unit cost that jumps up to 4 times, whether since the last tick
    /// or in the middle of a call, passes the cap by at most half the reserve plus one unit, so the
    /// tick stays below tickLimit at a full bucket and at an empty one (tickLimit = limit, V37).
    #[test]
    fn a_cost_jump_stays_inside_the_reserve() {
        let cfg = SCHEDULER_MMO;
        for (limit, bucket) in [(20u32, CPU_BUCKET_MAX), (20, 0)] {
            let tick_limit = CPU_TICK_LIMIT_MAX.min(bucket + limit);
            let ctx = TickContext::from_raw(
                1,
                limit,
                f64::from(tick_limit),
                i32::try_from(bucket).expect("fits"),
            );
            for base in [0.001, 0.01, 0.1] {
                for k in [2.0, 3.0, 4.0] {
                    for jump_at in [0u32, 1, 7, 40] {
                        let mut sched = Scheduler::default();
                        let clock = FakeClock::default();
                        // Last tick's estimate was the cheap cost.
                        let mut state = JobState {
                            next: 0,
                            unit_cost: Some(base),
                        };
                        let mut tick = sched.begin(&ctx, &cfg, clock.clone());
                        let cap = tick.budget().ceiling;
                        let reserve = tick.budget().reserve;
                        let unit_max = base * k;
                        let report = tick.run_units(&CRIT, &mut state, 1_000_000, |i, _| {
                            clock.spend(if i >= jump_at { unit_max } else { base });
                            Ok(())
                        });
                        assert_eq!(report.stop, Stop::Budget);
                        let spent = tick.spent();
                        assert!(
                            spent <= cap + reserve / 2.0 + unit_max + 1e-9,
                            "limit {limit} bucket {bucket} base {base} k {k} jump {jump_at}: {spent}"
                        );
                        assert!(spent < f64::from(tick_limit), "{spent} >= {tick_limit}");
                    }
                }
            }
        }
    }

    /// Uses: CPU_TICK_LIMIT_MAX. Units that got 10 times dearer since last tick's estimate (for example
    /// every creep repathing) are re-measured on the first unit of the call, so the tick passes its cap
    /// by at most one unit instead of a whole batch planned on the old cost.
    #[test]
    fn a_stale_estimate_is_re_measured_first() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let base = 0.5;
        let unit = base * 10.0;
        let mut state = JobState {
            next: 0,
            unit_cost: Some(base),
        };
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        let cap = tick.budget().ceiling;
        clock.now.set(cap - 30.0);
        let report = tick.run_units(&CRIT, &mut state, 1_000, |_, _| {
            clock.spend(unit);
            Ok(())
        });
        assert_eq!(report.stop, Stop::Budget);
        assert!(
            tick.spent() <= cap + unit,
            "{} > {}",
            tick.spent(),
            cap + unit
        );
        assert!(tick.spent() < f64::from(CPU_TICK_LIMIT_MAX));
    }

    /// A unit job whose last unit ends a batch exactly reports Done with no extra read, also when that
    /// last unit crosses the cap.
    #[test]
    fn a_round_that_ends_on_a_batch_boundary_is_done() {
        let cfg = SCHEDULER_MMO;
        let n = 1 + cfg.max_units_per_sample;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        let report = tick.run_units(&CRIT, &mut state, n, |_, _| {
            clock.spend(0.01);
            Ok(())
        });
        assert_eq!((report.ran, report.stop), (n, Stop::Done));
        // A start read, then one per batch: 1 unit, then max_units_per_sample.
        assert_eq!(clock.reads.get(), 3);
        drop(tick);

        let mut sched = Scheduler::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        clock.now.set(tick.budget().ceiling - 1.0);
        let report = tick.run_units(&CRIT, &mut state, 1, |_, _| {
            clock.spend(2.0);
            Ok(())
        });
        assert_eq!((report.ran, report.stop), (1, Stop::Done));
        drop(tick);
        assert_eq!(sched.last_tick().expect("recorded").stats.budget_stops, 0);
    }

    /// A job that starts with the tick at its cap stops without a clock read; exactly at the cap,
    /// nothing runs.
    #[test]
    fn a_job_at_its_cap_runs_nothing_and_reads_no_clock() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        clock.now.set(tick.budget().ceiling);
        let report = tick.run_units(&CRIT, &mut state, 5, |_, _| {
            panic!("must not run at the cap");
        });
        assert_eq!((report.ran, report.stop), (0, Stop::Budget));
        let reads = clock.reads.get();
        assert_eq!(reads, 1, "the first call reads once and finds the cap");
        for _ in 0..3 {
            let report = tick.run_units(&CRIT, &mut state, 5, |_, _| panic!("must not run"));
            assert_eq!(report.stop, Stop::Budget);
        }
        assert_eq!(
            clock.reads.get(),
            reads,
            "later calls know the cap is reached"
        );
        drop(tick);
        assert_eq!(sched.last_tick().expect("recorded").stats.budget_stops, 4);
    }

    /// `remaining` is 0 for a tier that is off and never negative.
    #[test]
    fn remaining_is_never_negative() {
        let cfg = SCHEDULER_MMO;
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        // Loop 0: Optional is in warm-up.
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        assert_eq!(tick.remaining(Tier::Optional), 0.0);
        clock.now.set(3.0);
        assert_eq!(tick.remaining(Tier::Critical), tick.budget().ceiling - 3.0);
        clock.now.set(tick.budget().ceiling + 5.0);
        assert_eq!(tick.remaining(Tier::Critical), 0.0);
    }

    /// An infinite reading counts as bad: it keeps the last good one and does not break the clock
    /// alone. Commit ignores negative and NaN amounts.
    #[test]
    fn infinite_readings_and_commit_guards() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        clock.now.set(5.0);
        tick.sample();
        clock.script.borrow_mut().push(f64::INFINITY);
        assert_eq!(tick.sample(), 5.0);
        tick.commit().add(-1.0);
        tick.commit().add(f64::NAN);
        assert_eq!(tick.commit().cpu(), 0.0);
        assert_eq!(tick.spent(), 5.0);
        drop(tick);
        let rec = sched.last_tick().expect("recorded");
        assert_eq!((rec.stats.bad_samples, rec.clock_broken), (1, false));
    }

    /// Uses: INTENT_CPU_COST. The tick record keeps the last reading plus committed intents, and the
    /// unit and stop counters.
    #[test]
    fn the_tick_record_keeps_what_happened() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        clock.now.set(3.0);
        let report = tick.run_units(&CRIT, &mut state, 4, |_, commit| {
            commit.intents(1);
            Ok(())
        });
        let samples = clock.reads.get();
        drop(tick);
        let rec = sched.last_tick().expect("recorded");
        let committed = 4.0 * INTENT_CPU_COST;
        assert_eq!(rec.stats.units, report.ran);
        assert_eq!(rec.stats.samples, samples);
        assert!((rec.committed - committed).abs() < 1e-9);
        assert!((rec.spent - (3.0 + committed)).abs() < 1e-9);
        assert_eq!((rec.stats.budget_stops, rec.stats.out_of_order), (0, 0));
    }

    /// Uses: INTENT_CPU_COST. The record splits the tick's CPU: the Optional job calls' CPU, with
    /// what their units committed, is `optional`, and `base()` is the rest; the split takes no clock
    /// reading of its own. A broken clock makes the base infinite.
    #[test]
    fn the_tick_record_splits_off_the_optional_work() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let (mut crit, mut opt) = (JobState::default(), JobState::default());
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, clock.clone());
        assert!(!tick.allows(Tier::Optional));
        clock.now.set(2.0);
        tick.run_units(&CRIT, &mut crit, 2, |_, commit| {
            clock.spend(0.5);
            commit.intents(1);
            Ok(())
        });
        drop(tick);
        // The first loop of a global runs no Optional work: all of it is base.
        let rec = *sched.last_tick().expect("recorded");
        assert_eq!(rec.optional, 0.0);
        assert!((rec.base() - rec.spent).abs() < 1e-9);

        let first_reads = clock.reads.get();
        let mut tick = sched.begin(&full_tick(2), &SCHEDULER_MMO, clock.clone());
        assert!(tick.allows(Tier::Optional));
        clock.now.set(2.0);
        tick.run_units(&CRIT, &mut crit, 2, |_, commit| {
            clock.spend(0.5);
            commit.intents(1);
            Ok(())
        });
        let reads = clock.reads.get();
        tick.run_steps(&OPT, &mut opt, |i, commit| {
            clock.spend(1.25);
            commit.intents(2);
            Ok(if i == 2 { Step::Done } else { Step::More })
        });
        let opt_reads = clock.reads.get() - reads;
        // A Critical job after the Optional one counts as base too.
        tick.run_units(&CRIT, &mut crit, 1, |_, _| {
            clock.spend(0.75);
            Ok(())
        });
        let samples = clock.reads.get() - first_reads;
        drop(tick);
        let rec = *sched.last_tick().expect("recorded");
        let optional = 3.0 * (1.25 + 2.0 * INTENT_CPU_COST);
        let base = 2.0 + 2.0 * (0.5 + INTENT_CPU_COST) + 0.75;
        assert!((rec.optional - optional).abs() < 1e-9, "{}", rec.optional);
        assert!((rec.base() - base).abs() < 1e-9, "{}", rec.base());
        assert!((rec.spent - (base + optional)).abs() < 1e-9);
        // The Optional call took only the slicer's own readings: one to start, one per batch.
        let batches = 1 + 2u32.div_ceil(SCHEDULER_MMO.max_units_per_sample);
        assert_eq!(opt_reads, 1 + batches);
        assert_eq!(rec.stats.samples, samples);

        let broken = TickRecord {
            clock_broken: true,
            ..rec
        };
        assert_eq!(broken.base(), f64::INFINITY);
    }

    /// An Optional call whose opening reading is bad (it keeps the last good one, from before the
    /// work in between) stays in the base load instead of taking that work into `optional`.
    #[test]
    fn a_bad_reading_leaves_the_optional_call_in_the_base_load() {
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let (mut crit, mut opt) = (JobState::default(), JobState::default());
        sched
            .begin(&full_tick(1), &SCHEDULER_MMO, clock.clone())
            .finish();
        let mut tick = sched.begin(&full_tick(2), &SCHEDULER_MMO, clock.clone());
        clock.now.set(1.0);
        tick.run_units(&CRIT, &mut crit, 1, |_, _| Ok(()));
        // Base work the clock has not seen yet when the Optional call opens on a NaN reading.
        clock.spend(4.0);
        clock.script.borrow_mut().push(f64::NAN);
        tick.run_steps(&OPT, &mut opt, |_, _| {
            clock.spend(0.5);
            Ok(Step::Done)
        });
        drop(tick);
        let rec = *sched.last_tick().expect("recorded");
        assert_eq!((rec.stats.bad_samples, rec.clock_broken), (1, false));
        assert_eq!(rec.optional, 0.0);
        assert!((rec.base() - 5.5).abs() < 1e-9, "{}", rec.base());
    }

    /// `isolate` records a failing unit like a sliced one and reports success or failure.
    #[test]
    fn isolate_records_faults() {
        let mut sched = Scheduler::default();
        let mut tick = sched.begin(&full_tick(9), &SCHEDULER_MMO, FakeClock::default());
        assert!(tick.isolate(&CRIT, Ok(())));
        assert!(!tick.isolate(&CRIT, Err(Fault::new("x", 3))));
        tick.finish();
        assert_eq!(sched.faults().total(), 1);
        assert_eq!(
            sched.faults().last(),
            Some(FaultRecord {
                time: 9,
                job: "crit",
                fault: Fault::new("x", 3)
            })
        );
        assert_eq!(sched.last_tick().expect("recorded").stats.faults, 1);
    }

    /// A job call whose tier comes after a later tier already ran is counted, and still runs.
    #[test]
    fn tier_order_breaches_are_counted() {
        const STD: Job = Job {
            name: "std",
            tier: Tier::Standard,
        };
        let mut sched = Scheduler::default();
        let mut state = JobState::default();
        let mut tick = sched.begin(&full_tick(1), &SCHEDULER_MMO, FakeClock::default());
        tick.run_units(&CRIT, &mut state, 1, |_, _| Ok(()));
        tick.run_units(&STD, &mut state, 1, |_, _| Ok(()));
        tick.run_units(&STD, &mut state, 1, |_, _| Ok(()));
        let report = tick.run_units(&CRIT, &mut state, 1, |_, _| Ok(()));
        assert_eq!(report.ran, 1, "a late Critical job still runs");
        drop(tick);
        assert_eq!(sched.last_tick().expect("recorded").stats.out_of_order, 1);
    }

    /// The break threshold comes from the config.
    #[test]
    fn the_break_threshold_is_configurable() {
        let cfg = SchedulerConfig {
            bad_samples_to_break: 3,
            ..SCHEDULER_MMO
        };
        let mut sched = Scheduler::default();
        let clock = FakeClock::default();
        let mut tick = sched.begin(&full_tick(1), &cfg, clock.clone());
        for n in 1..=3u32 {
            clock.script.borrow_mut().push(f64::NAN);
            let spent = tick.sample();
            assert_eq!(spent.is_infinite(), n == 3, "reading {n}");
        }
    }

    /// A unit that marks itself heavy ends its batch: the clock is read right after it, and its
    /// batch stays out of the cost estimate, so the cheap units after it are sized as before.
    #[test]
    fn a_heavy_unit_ends_its_batch_and_skips_the_estimate() {
        let cfg = SCHEDULER_MMO;
        let run_from = |mut state: JobState, heavy_unit: Option<u32>| {
            let clock = FakeClock::default();
            let mut sched = Scheduler::default();
            let mut tick = sched.begin(&full_tick(100), &cfg, clock.clone());
            let report = tick.run_units(&CRIT, &mut state, 6, |unit, commit| {
                if Some(unit) == heavy_unit {
                    clock.spend(5.0);
                    commit.heavy();
                } else {
                    clock.spend(0.1);
                }
                Ok(())
            });
            tick.finish();
            (report.ran, clock.reads.get(), state.unit_cost())
        };
        let run = |heavy_unit: Option<u32>| run_from(JobState::default(), heavy_unit);
        // Without a heavy unit: a reading at the start, after the first unit, and after the other five.
        let (ran, reads, cost) = run(None);
        assert_eq!((ran, reads), (6, 3));
        assert!(cost.is_some_and(|c| (c - 0.1).abs() < 1e-9));
        // Unit 2 is heavy: its batch (units 1 and 2) ends after it, the next batch is one unit to
        // re-measure, then the last two: two more readings, and the estimate stays at the cheap
        // units' cost.
        let (ran, reads, cost) = run(Some(2));
        assert_eq!((ran, reads), (6, 5));
        assert!(cost.is_some_and(|c| (c - 0.1).abs() < 1e-9), "{cost:?}");
        // A heavy first unit: the next batch is one unit (to measure), then the other four, even
        // when an estimate from an earlier tick exists (a stale one, too cheap), which would
        // otherwise size the batch at once.
        let (_, reads, cost) = run(Some(0));
        assert_eq!(reads, 4);
        assert!(cost.is_some_and(|c| (c - 0.1).abs() < 1e-9), "{cost:?}");
        let mut stale = JobState::default();
        stale.observe(0.01);
        let (_, reads, cost) = run_from(stale, Some(0));
        assert_eq!(reads, 4, "re-measured after the heavy unit");
        assert!(cost.is_some_and(|c| (c - 0.1).abs() < 1e-9), "{cost:?}");
    }
}
