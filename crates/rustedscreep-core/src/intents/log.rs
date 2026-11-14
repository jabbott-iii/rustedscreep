//! The act layer's per-tick intent counter and its totals for the global (ROADMAP P1 module 7).
//! `rustedscreep.act()` prints them; the profiler (module 8) will read them.

use super::buffer::{BufferStats, Rejected};
use super::outcome::{ERROR_CODES, Outcome, error_index};
use super::table::CreepAction;
use rustedscreep_consts::{ErrorCode, INTENT_CPU_COST};

/// What this tick's intent calls returned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IssueStats {
    /// JS calls made.
    pub issued: u32,
    pub ok: u32,
    /// OK calls of methods that are not free: what the engine bills (V34).
    pub billed: u32,
    /// Refusals, per [`ERROR_CODES`] slot.
    pub failed: [u32; ERROR_CODES.len()],
    pub unknown: u32,
    pub threw: u32,
    /// Issues with no JS object for the actor or the target, so no call was made (a snapshot
    /// alignment bug; each is also logged as a fault).
    pub missing: u32,
}

impl IssueStats {
    pub fn failed(&self, code: ErrorCode) -> u32 {
        self.failed.get(error_index(code)).copied().unwrap_or(0)
    }

    pub fn failed_total(&self) -> u32 {
        self.failed.iter().fold(0, |s, &n| s.saturating_add(n))
    }

    /// The CPU the engine bills for this tick's intents. Unit: CPU.
    ///
    /// Uses: `INTENT_CPU_COST` (V34).
    pub fn billed_cpu(&self) -> f64 {
        f64::from(self.billed) * INTENT_CPU_COST
    }
}

/// Counts since the global started (a global reset clears them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActTotals {
    pub ticks: u64,
    pub queued: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub blocked: u64,
    pub issued: u64,
    pub ok: u64,
    pub billed: u64,
    pub failed: u64,
    pub unknown: u64,
    pub threw: u64,
    pub missing: u64,
}

/// The last call that did not return OK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    /// `Game.time` of the tick it happened in.
    pub time: u32,
    pub action: CreepAction,
    pub outcome: Outcome,
}

/// This tick's issue counters, the last tick's buffer counters, and totals since the global started.
#[derive(Default)]
pub struct ActLog {
    time: u32,
    issues: IssueStats,
    buffer: BufferStats,
    totals: ActTotals,
    last_failure: Option<Failure>,
}

impl ActLog {
    /// Starts a tick's counters.
    pub fn begin(&mut self, time: u32) {
        self.time = time;
        self.issues = IssueStats::default();
        self.buffer = BufferStats::default();
    }

    /// Records one call's outcome.
    pub fn record(&mut self, action: CreepAction, outcome: Outcome) {
        let s = &mut self.issues;
        s.issued = s.issued.saturating_add(1);
        match outcome {
            Outcome::Ok => {
                s.ok = s.ok.saturating_add(1);
                if !action.is_free() {
                    s.billed = s.billed.saturating_add(1);
                }
            }
            Outcome::Failed(code) => {
                if let Some(n) = s.failed.get_mut(error_index(code)) {
                    *n = n.saturating_add(1);
                }
            }
            Outcome::Unknown => s.unknown = s.unknown.saturating_add(1),
            Outcome::Threw => s.threw = s.threw.saturating_add(1),
        }
        if outcome != Outcome::Ok {
            self.last_failure = Some(Failure {
                time: self.time,
                action,
                outcome,
            });
        }
    }

    /// Records an issue that was not called because its JS object was missing.
    pub fn missing(&mut self) {
        self.issues.missing = self.issues.missing.saturating_add(1);
    }

    /// Ends the tick: keeps the buffer's counters and adds both to the totals.
    pub fn finish(&mut self, buffer: &BufferStats) {
        self.buffer = *buffer;
        let t = &mut self.totals;
        let i = &self.issues;
        let add = |total: &mut u64, n: u32| *total = total.saturating_add(u64::from(n));
        add(&mut t.ticks, 1);
        add(&mut t.queued, buffer.queued);
        add(&mut t.replaced, buffer.replaced);
        add(&mut t.rejected, buffer.rejected_total());
        add(&mut t.blocked, buffer.blocked);
        add(&mut t.issued, i.issued);
        add(&mut t.ok, i.ok);
        add(&mut t.billed, i.billed);
        add(&mut t.failed, i.failed_total());
        add(&mut t.unknown, i.unknown);
        add(&mut t.threw, i.threw);
        add(&mut t.missing, i.missing);
    }

    /// `Game.time` of the tick the counters are for.
    pub fn time(&self) -> u32 {
        self.time
    }

    pub fn issues(&self) -> &IssueStats {
        &self.issues
    }

    /// The buffer's counters of the last finished tick.
    pub fn buffer(&self) -> &BufferStats {
        &self.buffer
    }

    pub fn totals(&self) -> &ActTotals {
        &self.totals
    }

    pub fn last_failure(&self) -> Option<&Failure> {
        self.last_failure.as_ref()
    }

    /// A buffer counter by reason, for summaries.
    pub fn rejected(&self, reason: Rejected) -> u32 {
        self.buffer.rejected(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses: rustedscreep_consts::INTENT_CPU_COST. OK calls of non-free methods are what the engine
    /// bills (V34); refusals, unknown returns and throws are counted apart, and the last of them is
    /// kept.
    #[test]
    fn outcomes_are_counted_and_billed_like_the_engine() {
        let mut log = ActLog::default();
        log.begin(500);
        log.record(CreepAction::Move, Outcome::Ok);
        log.record(CreepAction::Say, Outcome::Ok);
        log.record(CreepAction::Pull, Outcome::Ok);
        log.record(CreepAction::Transfer, Outcome::Failed(ErrorCode::Full));
        log.record(CreepAction::Transfer, Outcome::Failed(ErrorCode::Full));
        log.record(CreepAction::Repair, Outcome::Failed(ErrorCode::NotInRange));
        log.record(CreepAction::Heal, Outcome::Unknown);
        log.record(CreepAction::Drop, Outcome::Threw);
        let i = *log.issues();
        assert_eq!(
            (i.issued, i.ok, i.billed, i.unknown, i.threw),
            (8, 3, 1, 1, 1)
        );
        assert_eq!(i.failed(ErrorCode::Full), 2);
        assert_eq!(i.failed(ErrorCode::NotInRange), 1);
        assert_eq!(i.failed_total(), 3);
        assert_eq!(i.billed_cpu(), INTENT_CPU_COST);
        assert_eq!(
            log.last_failure(),
            Some(&Failure {
                time: 500,
                action: CreepAction::Drop,
                outcome: Outcome::Threw
            })
        );
    }

    #[test]
    fn finish_adds_the_tick_to_the_totals_and_begin_resets_only_the_tick() {
        let mut log = ActLog::default();
        let mut buffer = BufferStats {
            queued: 4,
            replaced: 1,
            blocked: 1,
            planned: 2,
            ..BufferStats::default()
        };
        if let Some(n) = buffer.rejected.get_mut(Rejected::Spawning as usize) {
            *n = 3;
        }
        for tick in 0..3u32 {
            log.begin(600 + tick);
            log.record(CreepAction::Move, Outcome::Ok);
            log.record(CreepAction::Withdraw, Outcome::Failed(ErrorCode::NotEnough));
            log.missing();
            log.finish(&buffer);
        }
        let t = *log.totals();
        assert_eq!(
            (t.ticks, t.queued, t.replaced, t.rejected, t.blocked),
            (3, 12, 3, 9, 3)
        );
        assert_eq!(
            (
                t.issued, t.ok, t.billed, t.failed, t.unknown, t.threw, t.missing
            ),
            (6, 3, 3, 3, 0, 0, 3)
        );
        assert_eq!(log.rejected(Rejected::Spawning), 3);
        assert_eq!(log.buffer(), &buffer);
        log.begin(700);
        assert_eq!(log.issues(), &IssueStats::default());
        assert_eq!(log.totals(), &t, "totals survive begin");
        assert_eq!(log.time(), 700);
        assert_eq!(log.last_failure().map(|f| f.time), Some(602));
    }
}
