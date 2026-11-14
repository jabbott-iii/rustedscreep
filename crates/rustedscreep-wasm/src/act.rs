//! The act step (Rust → intents; ROADMAP P1 module 7). After the scheduler's tick, the intent
//! buffer (`rustedscreep_core::intents`) walks each creep's plan blockers first, and each intent it
//! issues becomes one call of a `Creep` method on this tick's JS object, which the snapshot kept
//! row for row (`JsWorld`).
//!
//! The methods are bound here, not through screeps-game-api's typed methods: those convert the
//! return code with `result_from_i8(..).unwrap()`, so a code the crate does not list for the method
//! would panic and trap (VERIFICATION_LOG V111). These bindings read the return value as an `f64`
//! (an `undefined` arrives as NaN) and catch a JS exception (`catch`), so one bad call is counted
//! and logged as a fault, and the tick goes on.
//!
//! JS calls per tick: one per issued intent, plus nothing when there is none. The glue behind each
//! call is checked in V112.

use std::fmt::Write as _;

use rustedscreep_core::config::SchedulerConfig;
use rustedscreep_core::intents::{
    ActLog, CreepIntent, ERROR_CODES, IntentBuffer, Issue, IssueStats, Outcome, Rejected, SayText,
    TargetRow, error_name,
};
use rustedscreep_core::scheduler::{Commit, Fault, FaultRecord, Scheduler};
use rustedscreep_core::snapshot::Snapshot;
use screeps::ResourceType;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

use crate::snapshot::JsWorld;

#[wasm_bindgen]
extern "C" {
    /// A creep, for its action methods (engine `src/game/creeps.js` at a90d26f, V108).
    type Actor;

    #[wasm_bindgen(method, catch, js_name = move)]
    fn move_direction(this: &Actor, direction: u8) -> Result<f64, JsValue>;

    /// `move(creep)`, the same JS method with a creep argument (V108).
    #[wasm_bindgen(method, catch, js_name = move)]
    fn move_toward(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = say)]
    fn say(this: &Actor, message: &str, public: bool) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = drop)]
    fn drop_resource(
        this: &Actor,
        resource: ResourceType,
        amount: Option<u32>,
    ) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = transfer)]
    fn transfer(
        this: &Actor,
        target: &JsValue,
        resource: ResourceType,
        amount: Option<u32>,
    ) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = withdraw)]
    fn withdraw(
        this: &Actor,
        target: &JsValue,
        resource: ResourceType,
        amount: Option<u32>,
    ) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = heal)]
    fn heal(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = rangedHeal)]
    fn ranged_heal(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = repair)]
    fn repair(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = upgradeController)]
    fn upgrade_controller(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;

    #[wasm_bindgen(method, catch, js_name = pull)]
    fn pull(this: &Actor, target: &JsValue) -> Result<f64, JsValue>;
}

/// The job name act faults are logged under.
const JOB: &str = "act";

/// The act layer's state for one global: the intent buffer, the counters, and a `say` the console
/// asked for.
#[derive(Default)]
pub struct Act {
    pub buffer: IntentBuffer,
    log: ActLog,
    pending_say: Option<SayText>,
}

impl Act {
    /// Opens the buffer for this tick's snapshot and starts the counters.
    pub fn begin(&mut self, snapshot: &Snapshot) {
        self.buffer.begin(snapshot);
        self.log.begin(snapshot.tick.time);
    }

    /// Queues the `say` the console asked for, for every own creep (a spawning one is refused and
    /// counted). `say` is free (V34), so it costs JS calls but no intent CPU.
    pub fn queue_console_say(&mut self, snapshot: &Snapshot, commit: &mut Commit) {
        let Some(text) = self.pending_say.take() else {
            return;
        };
        let creeps = &snapshot.creeps;
        for (&handle, &my) in creeps.handles().iter().zip(creeps.my()) {
            if my {
                let intent = CreepIntent::Say {
                    text,
                    public: false,
                };
                // A refusal is counted by the buffer; nothing else to do.
                let _ = self.buffer.push(snapshot, handle, intent, commit);
            }
        }
    }

    /// This tick's call counters, for the profiler (`issued` JS calls, `billed` intents).
    pub fn issues(&self) -> &IssueStats {
        self.log.issues()
    }

    /// For the `rustedscreep.say(text)` console command: every own creep says `text` (cut to what
    /// the engine shows) on the next tick, visible only to the bot's owner.
    pub fn request_say(&mut self, text: &str) -> String {
        let text = SayText::new(text);
        self.pending_say = Some(text);
        // Plain quotes, not `{:?}`: Debug escaping links Unicode tables into the release wasm.
        format!(
            "rustedscreep: every own creep says \"{}\" on the next tick",
            text.as_str()
        )
    }
}

/// Issues the tick's intents: the core walks each creep's plan blockers first and calls back once
/// per intent it still issues (`IntentBuffer::issue`), and each call here is one JS call. Call
/// after the scheduler's tick has ended; `TickCpu` holds `&mut Scheduler` until then, so this cannot
/// run inside the decide step. A call that throws is a fault whose unit is the actor's handle slot
/// (stable for the creep's life); an issue whose JS object is missing (the snapshot keeps them
/// aligned, so this would be a bug) is a fault whose unit is the row, and no call is made. Faults
/// are rate-limited like a job's.
pub fn issue(
    act: &mut Act,
    snapshot: &Snapshot,
    js: &JsWorld,
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
) {
    let Act { buffer, log, .. } = act;
    buffer.issue(log, |issue| {
        let Some(actor) = js.creeps.get(issue.actor as usize) else {
            fault(
                sched,
                cfg,
                snapshot,
                "no JS object for the actor row",
                issue.actor,
            );
            return None;
        };
        match call(actor.unchecked_ref(), issue, js) {
            Some(Ok(value)) => Some(Outcome::from_return(value)),
            Some(Err(_)) => {
                let slot = snapshot
                    .creeps
                    .handles()
                    .get(issue.actor as usize)
                    .map_or(issue.actor, |h| h.slot());
                fault(sched, cfg, snapshot, threw(&issue.intent), slot);
                Some(Outcome::Threw)
            }
            None => {
                fault(
                    sched,
                    cfg,
                    snapshot,
                    "no JS object for the target row",
                    issue.actor,
                );
                None
            }
        }
    });
}

/// One JS call for `issue`, or `None` when the target's JS object is missing.
fn call(actor: &Actor, issue: &Issue, js: &JsWorld) -> Option<Result<f64, JsValue>> {
    let target = || -> Option<&JsValue> {
        match issue.target? {
            TargetRow::Creep(row) => js.creeps.get(row as usize).map(AsRef::as_ref),
            TargetRow::Structure(row) => js.structures.get(row as usize).map(AsRef::as_ref),
        }
    };
    Some(match issue.intent {
        CreepIntent::Move { direction } => actor.move_direction(direction as u8),
        CreepIntent::MoveToward { .. } => actor.move_toward(target()?),
        CreepIntent::Say { text, public } => actor.say(text.as_str(), public),
        CreepIntent::Drop { resource, amount } => actor.drop_resource(resource, amount),
        CreepIntent::Transfer {
            resource, amount, ..
        } => actor.transfer(target()?, resource, amount),
        CreepIntent::Withdraw {
            resource, amount, ..
        } => actor.withdraw(target()?, resource, amount),
        CreepIntent::Heal { .. } => actor.heal(target()?),
        CreepIntent::RangedHeal { .. } => actor.ranged_heal(target()?),
        CreepIntent::Repair { .. } => actor.repair(target()?),
        CreepIntent::UpgradeController { .. } => actor.upgrade_controller(target()?),
        CreepIntent::Pull { .. } => actor.pull(target()?),
    })
}

/// The fault text for a call that threw.
const fn threw(intent: &CreepIntent) -> &'static str {
    match intent {
        CreepIntent::Move { .. } => "the move call threw",
        CreepIntent::MoveToward { .. } => "the move (toward a creep) call threw",
        CreepIntent::Say { .. } => "the say call threw",
        CreepIntent::Drop { .. } => "the drop call threw",
        CreepIntent::Transfer { .. } => "the transfer call threw",
        CreepIntent::Withdraw { .. } => "the withdraw call threw",
        CreepIntent::Heal { .. } => "the heal call threw",
        CreepIntent::RangedHeal { .. } => "the rangedHeal call threw",
        CreepIntent::Repair { .. } => "the repair call threw",
        CreepIntent::UpgradeController { .. } => "the upgradeController call threw",
        CreepIntent::Pull { .. } => "the pull call threw",
    }
}

fn fault(
    sched: &mut Scheduler,
    cfg: &SchedulerConfig,
    snapshot: &Snapshot,
    what: &'static str,
    row: u32,
) {
    let record = FaultRecord {
        time: snapshot.tick.time,
        job: JOB,
        fault: Fault::new(what, row),
    };
    sched.faults_mut().record(record, cfg);
}

/// One line for `rustedscreep.act()`: the last tick's buffer and call counters, totals since the
/// global started, and the last call that did not return OK.
pub fn summary(act: &Act) -> String {
    let log = &act.log;
    let t = log.totals();
    if t.ticks == 0 {
        return String::from("rustedscreep act: no tick finished yet in this global");
    }
    let b = log.buffer();
    let i = log.issues();
    let mut text = format!(
        "rustedscreep act at tick {}: queued {} (replaced {}), refused {}",
        log.time(),
        b.queued,
        b.replaced,
        b.rejected_total(),
    );
    for reason in Rejected::ALL {
        let n = b.rejected(reason);
        if n > 0 {
            let _ = write!(text, ", {} {n}", reason.as_str());
        }
    }
    let _ = write!(
        text,
        " | blocked {} | issued {}: ok {} (billed {}), failed {}",
        b.blocked,
        i.issued,
        i.ok,
        i.billed,
        i.failed_total(),
    );
    for code in ERROR_CODES {
        let n = i.failed(code);
        if n > 0 {
            let _ = write!(text, ", {} {n}", error_name(code));
        }
    }
    let _ = write!(
        text,
        ", unknown {}, threw {}, missing {} | since boot: ticks {}, queued {}, refused {}, blocked {}, \
         issued {}, ok {}, billed {}, failed {}, unknown {}, threw {}, missing {}",
        i.unknown,
        i.threw,
        i.missing,
        t.ticks,
        t.queued,
        t.rejected,
        t.blocked,
        t.issued,
        t.ok,
        t.billed,
        t.failed,
        t.unknown,
        t.threw,
        t.missing,
    );
    if let Some(f) = log.last_failure() {
        let what = match f.outcome {
            Outcome::Failed(code) => error_name(code),
            Outcome::Unknown => "unknown return",
            Outcome::Threw => "threw",
            Outcome::Ok => "ok",
        };
        let _ = write!(
            text,
            " | last failure at tick {}: {} {what}",
            f.time,
            f.action.as_str()
        );
    }
    text
}
