//! The act layer's pure half (ROADMAP P1 module 7): the engine's creep intent-priority table, the
//! per-tick intent buffer, return-code classification, and the per-tick intent counter.
//!
//! One tick, as rustedscreep-wasm drives it:
//! 1. After the snapshot, [`IntentBuffer::begin`] opens the buffer for it and [`ActLog::begin`]
//!    starts the tick's counters.
//! 2. During the decide step, jobs [`IntentBuffer::push`] intents by creep handle. A push that the
//!    game API would refuse, or a billed no-op the snapshot can already see, is refused here
//!    ([`Rejected`]), and a new intent that is not free commits `INTENT_CPU_COST` to the scheduler
//!    (VERIFICATION_LOG V34).
//! 3. After the scheduler's tick ends, [`IntentBuffer::issue`] plans each creep's last push per
//!    method, ordered blockers first ([`PRIORITY_ORDER`]), and calls rustedscreep-wasm's closure
//!    once per intent it issues: within a creep it skips an intent once a call that blocks it was
//!    recorded (V44, V45), so a blocker the game refused blocks nothing. The closure makes the JS
//!    call and classifies what it returned ([`Outcome::from_return`]); each outcome goes to the
//!    [`ActLog`], which then adds the tick to its totals. Nothing reads the CPU clock after the
//!    decide step: `TickCpu` holds the scheduler's `&mut` borrow until the tick is finished.
//!
//! The buffer lives in the heap for the global; a global reset starts it empty, which loses
//! nothing, since it holds one tick's intents.
//!
//! ```
//! use rustedscreep_consts::{Direction, INTENT_CPU_COST, Position, RoomCoordinate, RoomName};
//! use rustedscreep_core::intents::{ActLog, CreepIntent, IntentBuffer, Outcome, Queued};
//! use rustedscreep_core::scheduler::Commit;
//! use rustedscreep_core::snapshot::{CreepRow, ObjectKey, Snapshot, TickContext};
//!
//! let room: RoomName = "W1N1".parse().unwrap();
//! let at = |x| Position::new(RoomCoordinate::new(x).unwrap(), RoomCoordinate::new(10).unwrap(), room);
//! let mut snap = Snapshot::default();
//! snap.begin(TickContext::from_raw(100, 20, 500.0, 10_000));
//! let row = |x| CreepRow { pos: at(x), my: true, spawning: false, fatigue: 0 };
//! let a = snap.creeps.push(ObjectKey(1), row(10)).unwrap();
//! snap.finish();
//!
//! let mut buffer = IntentBuffer::default();
//! let mut commit = Commit::default();
//! buffer.begin(&snap);
//! let up = CreepIntent::Move { direction: Direction::Top };
//! let left = CreepIntent::Move { direction: Direction::Left };
//! assert_eq!(buffer.push(&snap, a, up, &mut commit), Ok(Queued::New));
//! assert_eq!(buffer.push(&snap, a, left, &mut commit), Ok(Queued::Replaced));
//! assert_eq!(commit.cpu(), INTENT_CPU_COST, "the engine bills a creep's method once");
//! let mut log = ActLog::default();
//! let mut issued = Vec::new();
//! buffer.issue(&mut log, |issue| {
//!     issued.push(issue.intent);
//!     Some(Outcome::Ok)
//! });
//! assert_eq!(issued, [left], "only the last push is issued");
//! assert_eq!(log.issues().billed, 1);
//! ```

mod buffer;
mod log;
mod outcome;
mod table;

pub use buffer::{
    BufferStats, CreepIntent, IntentBuffer, Issue, Queued, Rejected, SayText, Target, TargetRow,
};
pub use log::{ActLog, ActTotals, Failure, IssueStats};
pub use outcome::{ERROR_CODES, Outcome, error_index, error_name};
pub use table::{ActionSet, CreepAction, ENGINE_COMMIT, PRIORITY_ORDER};
