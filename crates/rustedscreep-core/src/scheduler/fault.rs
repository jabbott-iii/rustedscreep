//! Per-unit error isolation (ROADMAP P1 module 6). A unit (a creep, a room, a task) that fails
//! returns a [`Fault`]; the scheduler records it, skips that unit and goes on with the next. The log
//! keeps counts and a rate-limited queue of reports, which rustedscreep-wasm drains and prints, so a
//! unit that fails every tick costs a counter increment, not a console line per tick.
//!
//! The rate limit is one budget per window for all jobs. So that one stuck unit cannot hide a new
//! failure elsewhere, the note that closes a window names the jobs whose faults it held back (up to
//! [`SUPPRESSED_JOBS`] of them).
//!
//! Nothing here formats or allocates per fault: a fault is two static strings and a number, and the
//! queue's capacity is reused.

use crate::config::SchedulerConfig;

/// Why one unit failed. Built by the unit's own code, which knows what the unit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// A short fixed description, such as "target vanished".
    pub what: &'static str,
    /// The unit's number for the log, such as its handle slot; the job decides what it means.
    pub unit: u32,
}

impl Fault {
    pub const fn new(what: &'static str, unit: u32) -> Self {
        Self { what, unit }
    }
}

/// One recorded fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultRecord {
    /// `Game.time` of the tick it happened in.
    pub time: u32,
    /// The job's name ([`super::Job::name`]).
    pub job: &'static str,
    pub fault: Fault,
}

/// Job names a [`Suppressed`] note can carry; more jobs only set [`Suppressed::more_jobs`].
pub const SUPPRESSED_JOBS: usize = 4;

/// Faults that a window counted but did not report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suppressed {
    pub count: u32,
    /// `Game.time` at which that window started.
    pub window_start: u32,
    /// The jobs whose faults were held back, in the order each was first held back.
    pub jobs: [Option<&'static str>; SUPPRESSED_JOBS],
    /// More jobs than [`SUPPRESSED_JOBS`] had faults held back.
    pub more_jobs: bool,
}

/// Fault counts and the reports still to be logged, for one global.
#[derive(Default)]
pub struct FaultLog {
    window_start: Option<u32>,
    in_window: u32,
    reported_in_window: u32,
    queue: Vec<FaultRecord>,
    suppressed: Option<Suppressed>,
    held_jobs: [Option<&'static str>; SUPPRESSED_JOBS],
    held_more: bool,
    total: u64,
    last: Option<FaultRecord>,
}

impl FaultLog {
    /// Starts a tick: a window that has lasted `fault_window_ticks` closes, and the faults it counted
    /// but did not report become one [`Suppressed`] note. A `time` before the window's start (which
    /// the engine never gives) also closes it.
    pub fn begin_tick(&mut self, time: u32, cfg: &SchedulerConfig) {
        let expired = match self.window_start {
            None => true,
            Some(start) => time.wrapping_sub(start) >= cfg.fault_window_ticks.max(1),
        };
        if !expired {
            return;
        }
        if let Some(start) = self.window_start {
            let unreported = self.in_window.saturating_sub(self.reported_in_window);
            if unreported > 0 {
                // A note not yet drained is replaced; its count is still in `total`.
                self.suppressed = Some(Suppressed {
                    count: unreported,
                    window_start: start,
                    jobs: self.held_jobs,
                    more_jobs: self.held_more,
                });
            }
        }
        self.window_start = Some(time);
        self.in_window = 0;
        self.reported_in_window = 0;
        self.held_jobs = [None; SUPPRESSED_JOBS];
        self.held_more = false;
    }

    /// Records a fault: it always counts; it is queued for the log only while the window has
    /// reports left and the queue is not full (so an undrained queue stays bounded).
    pub fn record(&mut self, record: FaultRecord, cfg: &SchedulerConfig) {
        self.total = self.total.saturating_add(1);
        self.last = Some(record);
        self.in_window = self.in_window.saturating_add(1);
        let per_window = cfg.fault_reports_per_window;
        let queue_room = u32::try_from(self.queue.len()).is_ok_and(|len| len < per_window);
        if self.reported_in_window < per_window && queue_room {
            self.queue.push(record);
            self.reported_in_window += 1;
        } else {
            self.note_held(record.job);
        }
    }

    /// Remembers that `job` had a fault held back in this window.
    fn note_held(&mut self, job: &'static str) {
        for slot in &mut self.held_jobs {
            match slot {
                Some(name) if *name == job => return,
                Some(_) => {}
                None => {
                    *slot = Some(job);
                    return;
                }
            }
        }
        self.held_more = true;
    }

    /// Takes the queued reports (oldest first) and the suppression note, if any, for logging.
    /// The queue keeps its capacity.
    pub fn drain(&mut self) -> (std::vec::Drain<'_, FaultRecord>, Option<Suppressed>) {
        let suppressed = self.suppressed.take();
        (self.queue.drain(..), suppressed)
    }

    /// Faults recorded since this global started.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The most recent fault.
    pub fn last(&self) -> Option<FaultRecord> {
        self.last
    }

    /// Faults counted in the current window.
    pub fn in_window(&self) -> u32 {
        self.in_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SCHEDULER_MMO;

    fn rec(time: u32, unit: u32) -> FaultRecord {
        FaultRecord {
            time,
            job: "test",
            fault: Fault::new("boom", unit),
        }
    }

    /// A unit failing every tick is reported `fault_reports_per_window` times per window; the rest
    /// are counted and summed in one note when the window closes.
    #[test]
    fn reports_are_rate_limited_per_window() {
        let cfg = SCHEDULER_MMO;
        let per = cfg.fault_reports_per_window;
        let window = cfg.fault_window_ticks;
        let mut log = FaultLog::default();
        let mut reported = 0u32;
        let mut notes = Vec::new();
        let ticks = window * 3;
        for t in 0..ticks {
            log.begin_tick(1000 + t, &cfg);
            for unit in 0..2 {
                log.record(rec(1000 + t, unit), &cfg);
            }
            let (reports, note) = log.drain();
            reported += u32::try_from(reports.count()).expect("small");
            notes.extend(note);
        }
        assert_eq!(log.total(), u64::from(ticks * 2));
        assert_eq!(reported, per * 3, "three windows");
        // The first two windows closed inside the loop; the third is still open.
        assert_eq!(
            notes,
            [
                Suppressed {
                    count: window * 2 - per,
                    window_start: 1000,
                    jobs: [Some("test"), None, None, None],
                    more_jobs: false,
                },
                Suppressed {
                    count: window * 2 - per,
                    window_start: 1000 + window,
                    jobs: [Some("test"), None, None, None],
                    more_jobs: false,
                },
            ]
        );
        assert_eq!(log.in_window(), window * 2);
        assert_eq!(log.last(), Some(rec(1000 + ticks - 1, 1)));
    }

    /// Without draining, the queue never grows past one window's reports.
    #[test]
    fn an_undrained_queue_stays_bounded() {
        let cfg = SCHEDULER_MMO;
        let mut log = FaultLog::default();
        for t in 0..cfg.fault_window_ticks * 5 {
            log.begin_tick(t, &cfg);
            log.record(rec(t, 0), &cfg);
        }
        let (reports, _) = log.drain();
        assert_eq!(
            reports.count(),
            usize::try_from(cfg.fault_reports_per_window).expect("small")
        );
    }

    /// Time jumping forward by many windows, or backwards, closes the window; a quiet window leaves
    /// no note.
    #[test]
    fn time_jumps_close_the_window() {
        let cfg = SCHEDULER_MMO;
        let mut log = FaultLog::default();
        log.begin_tick(500, &cfg);
        for _ in 0..cfg.fault_reports_per_window + 3 {
            log.record(rec(500, 0), &cfg);
        }
        log.begin_tick(500 + cfg.fault_window_ticks * 50, &cfg);
        let (_, note) = log.drain();
        assert_eq!(note.map(|n| (n.count, n.window_start)), Some((3, 500)));
        log.begin_tick(10, &cfg);
        assert_eq!(log.in_window(), 0);
        log.begin_tick(10 + cfg.fault_window_ticks, &cfg);
        assert_eq!(log.drain().1, None, "a quiet window leaves no note");
    }

    /// A job that fails every tick uses up the window's reports, and a new failure in another job
    /// after that is still named in the window's note; past SUPPRESSED_JOBS names, the note says so.
    #[test]
    fn the_note_names_the_jobs_it_held_back() {
        let cfg = SCHEDULER_MMO;
        let mut log = FaultLog::default();
        let job = |name: &'static str, t| FaultRecord {
            time: t,
            job: name,
            fault: Fault::new("boom", 0),
        };
        log.begin_tick(0, &cfg);
        for t in 0..cfg.fault_reports_per_window + 2 {
            log.record(job("stuck", t), &cfg);
        }
        let names = ["new", "a", "b", "c", "d"];
        for name in names {
            log.record(job(name, 50), &cfg);
            log.record(job(name, 51), &cfg);
        }
        log.begin_tick(cfg.fault_window_ticks, &cfg);
        let (jobs, note) = {
            let (reports, note) = log.drain();
            (reports.map(|r| r.job).collect::<Vec<_>>(), note)
        };
        assert!(jobs.iter().all(|&j| j == "stuck"));
        let note = note.expect("a note");
        assert_eq!(
            note.count,
            2 + 2 * u32::try_from(names.len()).expect("small")
        );
        assert_eq!(
            note.jobs,
            [Some("stuck"), Some("new"), Some("a"), Some("b")]
        );
        assert!(note.more_jobs);
        // The next window starts with no names.
        log.record(job("x", 200), &cfg);
        log.begin_tick(cfg.fault_window_ticks * 2, &cfg);
        assert_eq!(log.drain().1, None, "a reported fault leaves no note");
    }

    /// Time falling before the window's start closes it, and its held-back faults become a note.
    #[test]
    fn a_rewind_closes_a_window_with_held_faults() {
        let cfg = SCHEDULER_MMO;
        let mut log = FaultLog::default();
        log.begin_tick(500, &cfg);
        for _ in 0..cfg.fault_reports_per_window + 2 {
            log.record(rec(500, 0), &cfg);
        }
        log.begin_tick(499, &cfg);
        let (_, note) = log.drain();
        assert_eq!(note.map(|n| (n.count, n.window_start)), Some((2, 500)));
        assert_eq!(log.in_window(), 0);
    }
}
