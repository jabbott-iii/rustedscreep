//! The per-tick creep intent buffer (ROADMAP P1 module 7).
//!
//! Jobs [`IntentBuffer::push`] intents during the decide step. After the scheduler's tick ends,
//! [`IntentBuffer::resolve`] keeps each creep's last push per method, ordered blockers first, and
//! [`IntentBuffer::issue`] makes one JS call per intent it still issues. Every intent the game API
//! accepts is billed `INTENT_CPU_COST` whether or not the processor later runs it (VERIFICATION_LOG
//! V34), so the buffer avoids three kinds of call:
//! - Refused at push, so nothing is queued and nothing earlier is replaced, as when a JS call fails
//!   (it records nothing, V108). Calls the game API refuses: an actor that is gone, not the bot's or
//!   still spawning; a target that is gone; a creep target that is spawning for `transfer`, `pull`
//!   and a move toward it; a target out of range; a structure the method never takes. Billed no-ops,
//!   which the API accepts and the processor skips (V109): `heal` and `rangedHeal` of a spawning
//!   creep, a `repair` of a type without a `CONSTRUCTION_COST`, a move toward itself. And an amount
//!   of 0, which the API reads as "all". Dropping a billed no-op also removes it as a blocker.
//! - An earlier push of the same method on the same creep: only the last push is issued. The engine
//!   keeps the last recorded call (V34, V47); the buffer issues the last push as the creep's decision
//!   and does not fall back to an earlier one if that call fails.
//! - An intent that the creep's other intents block in the engine's priority table (V44, V45). Only
//!   a recorded intent blocks, so `issue` calls a creep's intents in [`PRIORITY_ORDER`] (blockers
//!   first) and skips one only once a blocker's call was recorded. When every call is accepted this
//!   issues [`ActionSet::kept`]; when a blocker is refused, the intents it would have blocked run.
//!
//! A `move(direction)` while the creep is fatigued is refused (`ERR_TIRED` in the game API, unbilled,
//! V108; the snapshot's fatigue column since P2.1, V160); a move toward a creep is not, since the API
//! skips that check for it (V108). Other state-dependent checks need snapshot columns that later
//! modules add with the jobs that read them: a transfer into a full store is a refusal (unbilled; it
//! must not replace an earlier push), and a repair at full hits is a billed no-op to drop (V109).
//!
//! A tick allocates nothing here once the vectors have grown to the tick's intent count.

use rustedscreep_consts::{CREEP_SAY_MAX_LENGTH, Direction, Position, ResourceType, StructureType};

use super::log::ActLog;
use super::outcome::Outcome;
use super::table::{ActionSet, CreepAction, PRIORITY_ORDER};
use crate::scheduler::Commit;
use crate::snapshot::{CreepHandle, Snapshot, StructureHandle};

/// The length the engine shows a `say` message at, in UTF-16 code units (the API's
/// `CREEP_SAY_MAX_LENGTH`; the processor's `substring(0, 10)`, V109).
const SAY_MAX: usize = CREEP_SAY_MAX_LENGTH as usize;

/// Most UTF-8 bytes a [`SayText`] can hold: a character of one UTF-16 unit takes at most 3 bytes, and
/// one of two units 4, so `SAY_MAX` units fit in 3 bytes each.
const SAY_TEXT_BYTES: usize = 3 * SAY_MAX;

/// The text of a `say` intent, cut to the length the engine shows (`CREEP_SAY_MAX_LENGTH` UTF-16
/// units) without splitting a character, so what is sent is what is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SayText {
    bytes: [u8; SAY_TEXT_BYTES],
    len: u8,
}

impl SayText {
    /// `text`, cut after the last whole character that fits.
    pub fn new(text: &str) -> Self {
        let mut bytes = [0u8; SAY_TEXT_BYTES];
        let mut len = 0usize;
        let mut units = 0usize;
        for c in text.chars() {
            units += c.len_utf16();
            if units > SAY_MAX {
                break;
            }
            let Some(dst) = bytes.get_mut(len..len + c.len_utf8()) else {
                break;
            };
            c.encode_utf8(dst);
            len += c.len_utf8();
        }
        Self {
            bytes,
            len: u8::try_from(len).unwrap_or(0),
        }
    }

    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }
}

impl std::fmt::Debug for SayText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// Who or what an intent acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Creep(CreepHandle),
    Structure(StructureHandle),
}

/// A target resolved to its row in this tick's snapshot, where rustedscreep-wasm finds its JS object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetRow {
    Creep(u32),
    Structure(u32),
}

/// One creep intent with its arguments. The variants are the creep methods whose targets this
/// build's snapshot can name; harvest, build and pickup arrive with their target tables (P2), and
/// attacks with the ally check every attack intent needs (P7, P10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CreepIntent {
    /// `drop(resource, amount)`; `None` drops all of it.
    Drop {
        resource: ResourceType,
        amount: Option<u32>,
    },
    /// `transfer(target, resource, amount)`; `None` transfers as much as fits.
    Transfer {
        target: Target,
        resource: ResourceType,
        amount: Option<u32>,
    },
    /// `withdraw(target, resource, amount)`; `None` takes as much as fits.
    Withdraw {
        target: StructureHandle,
        resource: ResourceType,
        amount: Option<u32>,
    },
    Heal {
        target: CreepHandle,
    },
    RangedHeal {
        target: CreepHandle,
    },
    /// `move(direction)`. The game API refuses it while the creep is fatigued or has no active MOVE
    /// part (V108).
    Move {
        direction: Direction,
    },
    /// `move(creep)`: one step onto an adjacent creep's tile. The game API skips the fatigue and MOVE
    /// checks for it (V108), so this is how a pulled creep follows its puller (V53). It shares the
    /// `move` slot with [`CreepIntent::Move`]: the last push of either is issued.
    MoveToward {
        target: CreepHandle,
    },
    Repair {
        target: StructureHandle,
    },
    /// `say(message, public)`.
    Say {
        text: SayText,
        public: bool,
    },
    UpgradeController {
        target: StructureHandle,
    },
    /// `pull(target)`; the pulled creep must also move onto this creep's tile, with
    /// [`CreepIntent::MoveToward`] when it is fatigued or has no MOVE part (V53, V108).
    Pull {
        target: CreepHandle,
    },
}

impl CreepIntent {
    pub const fn action(&self) -> CreepAction {
        match self {
            Self::Drop { .. } => CreepAction::Drop,
            Self::Transfer { .. } => CreepAction::Transfer,
            Self::Withdraw { .. } => CreepAction::Withdraw,
            Self::Heal { .. } => CreepAction::Heal,
            Self::RangedHeal { .. } => CreepAction::RangedHeal,
            Self::Move { .. } | Self::MoveToward { .. } => CreepAction::Move,
            Self::Repair { .. } => CreepAction::Repair,
            Self::Say { .. } => CreepAction::Say,
            Self::UpgradeController { .. } => CreepAction::UpgradeController,
            Self::Pull { .. } => CreepAction::Pull,
        }
    }

    /// The amount argument of `drop`, `transfer` and `withdraw` (`None` for every other method).
    pub const fn amount(&self) -> Option<u32> {
        match *self {
            Self::Drop { amount, .. }
            | Self::Transfer { amount, .. }
            | Self::Withdraw { amount, .. } => amount,
            _ => None,
        }
    }

    pub const fn target(&self) -> Option<Target> {
        match *self {
            Self::Transfer { target, .. } => Some(target),
            Self::Withdraw { target, .. }
            | Self::Repair { target }
            | Self::UpgradeController { target } => Some(Target::Structure(target)),
            Self::Heal { target }
            | Self::RangedHeal { target }
            | Self::Pull { target }
            | Self::MoveToward { target } => Some(Target::Creep(target)),
            Self::Drop { .. } | Self::Move { .. } | Self::Say { .. } => None,
        }
    }

    /// How far the target may be (Chebyshev distance, same room), as the game API checks it:
    /// `isNearTo` for 1, `inRangeTo(target, 3)` for 3 (V108, V113). `None` without a target.
    pub const fn range(&self) -> Option<u8> {
        match self {
            Self::Transfer { .. }
            | Self::Withdraw { .. }
            | Self::Heal { .. }
            | Self::Pull { .. }
            | Self::MoveToward { .. } => Some(1),
            Self::RangedHeal { .. } | Self::Repair { .. } | Self::UpgradeController { .. } => {
                Some(3)
            }
            Self::Drop { .. } | Self::Move { .. } | Self::Say { .. } => None,
        }
    }
}

/// Why a push was refused. Nothing was queued and nothing was replaced, as when the game API
/// refuses a call: it records no intent (V108).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The buffer is not collecting: [`IntentBuffer::begin`] was not called yet, [`IntentBuffer::resolve`]
    /// already ran, or the snapshot's `Game.time` or creep count differs from the one `begin` saw.
    Closed,
    /// The actor is not in this tick's snapshot.
    GoneActor,
    /// The actor is another player's creep (every method returns `ERR_NOT_OWNER` first, V108).
    NotMine,
    /// The actor is still spawning (every method returns `ERR_BUSY` next, V108).
    Spawning,
    /// A `move(direction)` while the actor is fatigued (`ERR_TIRED`, unbilled, V108, V160).
    Tired,
    /// The target is not in this tick's snapshot.
    GoneTarget,
    /// The target creep is still spawning: `transfer` and `pull` return `ERR_INVALID_TARGET`; `heal`
    /// and `rangedHeal` return OK and the processor skips them, billed; a move toward it would step
    /// onto its spawn, an obstacle, billed (V108, V109, V113).
    SpawningTarget,
    /// The method never acts on this target: the creep itself for `pull` (`ERR_INVALID_TARGET`),
    /// `transfer` (billed, it moves nothing) and a move toward it (billed, no step); for `transfer`
    /// and `withdraw` a controller or a structure that is built without a store, for `withdraw` also
    /// a nuker or a power bank (`ERR_INVALID_TARGET`; `transfer` of energy to a controller would
    /// instead record an `upgradeController` intent); for `repair` a type without a
    /// `CONSTRUCTION_COST` (billed, skipped); for `upgradeController` anything but a controller
    /// (V108, V109, V113).
    WrongTarget,
    /// The target is farther than the method's range or in another room (`ERR_NOT_IN_RANGE`, V108,
    /// V113).
    NotInRange,
    /// An amount of 0: `drop`, `transfer` and `withdraw` read a falsy amount as "all" (V108), which
    /// is never what a computed 0 means. Pass `None` for "all".
    ZeroAmount,
}

impl Rejected {
    /// Every reason, in a fixed order (the order of the per-reason counters).
    pub const ALL: [Self; 10] = [
        Self::Closed,
        Self::GoneActor,
        Self::NotMine,
        Self::Spawning,
        Self::Tired,
        Self::GoneTarget,
        Self::SpawningTarget,
        Self::WrongTarget,
        Self::NotInRange,
        Self::ZeroAmount,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::GoneActor => "gone actor",
            Self::NotMine => "not mine",
            Self::Spawning => "spawning",
            Self::Tired => "tired",
            Self::GoneTarget => "gone target",
            Self::SpawningTarget => "spawning target",
            Self::WrongTarget => "wrong target",
            Self::NotInRange => "not in range",
            Self::ZeroAmount => "zero amount",
        }
    }
}

/// What a push did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queued {
    /// A new intent for this creep and method.
    New,
    /// It replaced the creep's earlier push of the same method: only the last push is issued.
    Replaced,
}

/// One intent to issue: the actor's and target's rows in this tick's snapshot, and the intent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Issue {
    pub actor: u32,
    pub target: Option<TargetRow>,
    pub intent: CreepIntent,
}

/// The buffer's counters for one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferStats {
    /// Pushes queued, replacements included.
    pub queued: u32,
    /// Pushes that replaced an earlier intent of the same creep and method.
    pub replaced: u32,
    /// Pushes refused, per [`Rejected::ALL`] reason.
    pub rejected: [u32; Rejected::ALL.len()],
    /// Intents not issued because a blocker's call on the same creep was recorded (V44, V45).
    pub blocked: u32,
    /// Intents planned by [`IntentBuffer::resolve`]: each creep's last push per method.
    pub planned: u32,
}

impl BufferStats {
    pub fn rejected(&self, reason: Rejected) -> u32 {
        self.rejected.get(reason as usize).copied().unwrap_or(0)
    }

    pub fn rejected_total(&self) -> u32 {
        self.rejected.iter().fold(0, |s, &n| s.saturating_add(n))
    }
}

/// Slots of a row's latest push per method, indexed by [`CreepAction`]; valid only where the row's
/// [`ActionSet`] in `present` has the action.
type Latest = [u32; CreepAction::ALL.len()];

/// The tick's creep intents. One per global; [`Self::begin`] clears it each tick.
#[derive(Default)]
pub struct IntentBuffer {
    /// Every accepted push, in push order.
    entries: Vec<Issue>,
    /// Per actor row: the methods queued so far this tick.
    present: Vec<ActionSet>,
    /// Per actor row: the entry index of the latest push of each method (see [`Latest`]). Never
    /// cleared, only grown: `present` says which slots belong to this tick.
    latest: Vec<Latest>,
    /// What `resolve` plans, by row and then in [`PRIORITY_ORDER`].
    planned: Vec<Issue>,
    /// `Game.time` and creep count of the snapshot `begin` was called for.
    tick: Option<(u32, usize)>,
    resolved: bool,
    stats: BufferStats,
}

impl IntentBuffer {
    /// Starts a tick for `snap`, which must already be taken: drops last tick's intents (keeping the
    /// capacity) and counters.
    pub fn begin(&mut self, snap: &Snapshot) {
        let creeps = snap.creeps.len();
        self.entries.clear();
        self.planned.clear();
        self.present.clear();
        self.present.resize(creeps, ActionSet::EMPTY);
        if self.latest.len() < creeps {
            self.latest.resize(creeps, [0; CreepAction::ALL.len()]);
        }
        self.tick = Some((snap.tick.time, creeps));
        self.resolved = false;
        self.stats = BufferStats::default();
    }

    /// Queues `intent` for the creep `actor`, or refuses it (see [`Rejected`]; nothing changes then).
    /// A new intent that is not free commits `INTENT_CPU_COST` on `commit` (V34), so the scheduler
    /// counts it before the clock can; a replacement commits nothing, since the engine bills a
    /// creep's method once. An intent that `issue` later skips as blocked stays committed: the
    /// commit is an upper bound.
    ///
    /// Uses: `INTENT_CPU_COST` through [`CreepAction::cpu`].
    pub fn push(
        &mut self,
        snap: &Snapshot,
        actor: CreepHandle,
        intent: CreepIntent,
        commit: &mut Commit,
    ) -> Result<Queued, Rejected> {
        match self.check(snap, actor, &intent) {
            Ok((row, target)) => {
                let action = intent.action();
                let index = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
                let r = row as usize;
                // `check` found the row in a snapshot of `present.len()` creeps, and `begin` grew
                // `latest` to that length, so both lookups succeed.
                let (Some(set), Some(slots)) = (self.present.get_mut(r), self.latest.get_mut(r))
                else {
                    return Err(Rejected::Closed);
                };
                let replaced = set.contains(action);
                *set = set.with(action);
                if let Some(slot) = slots.get_mut(action as usize) {
                    *slot = index;
                }
                self.entries.push(Issue {
                    actor: row,
                    target,
                    intent,
                });
                self.stats.queued = self.stats.queued.saturating_add(1);
                if replaced {
                    self.stats.replaced = self.stats.replaced.saturating_add(1);
                    Ok(Queued::Replaced)
                } else {
                    commit.add(action.cpu());
                    Ok(Queued::New)
                }
            }
            Err(reason) => {
                if let Some(n) = self.stats.rejected.get_mut(reason as usize) {
                    *n = n.saturating_add(1);
                }
                Err(reason)
            }
        }
    }

    fn check(
        &self,
        snap: &Snapshot,
        actor: CreepHandle,
        intent: &CreepIntent,
    ) -> Result<(u32, Option<TargetRow>), Rejected> {
        if self.resolved || self.tick != Some((snap.tick.time, snap.creeps.len())) {
            return Err(Rejected::Closed);
        }
        let creeps = &snap.creeps;
        let row = creeps.row_of(actor).ok_or(Rejected::GoneActor)?;
        if !creeps.my().get(row).copied().unwrap_or(false) {
            return Err(Rejected::NotMine);
        }
        if creeps.spawning().get(row).copied().unwrap_or(true) {
            return Err(Rejected::Spawning);
        }
        if matches!(intent, CreepIntent::Move { .. })
            && creeps.fatigue().get(row).copied().unwrap_or(0) > 0
        {
            return Err(Rejected::Tired);
        }
        let (target, at) = match intent.target() {
            None => (None, None),
            Some(Target::Creep(h)) => {
                let t = creeps.row_of(h).ok_or(Rejected::GoneTarget)?;
                let to_self = matches!(
                    intent,
                    CreepIntent::Pull { .. }
                        | CreepIntent::MoveToward { .. }
                        | CreepIntent::Transfer { .. }
                );
                if to_self && t == row {
                    return Err(Rejected::WrongTarget);
                }
                if creeps.spawning().get(t).copied().unwrap_or(true) {
                    return Err(Rejected::SpawningTarget);
                }
                let at = creeps.positions().get(t).copied();
                (Some(TargetRow::Creep(row_u32(t)?)), at)
            }
            Some(Target::Structure(h)) => {
                let st = &snap.structures;
                let t = st.row_of(h).ok_or(Rejected::GoneTarget)?;
                let kind = st.kinds().get(t).copied();
                if !kind.is_some_and(|k| accepts(intent, k)) {
                    return Err(Rejected::WrongTarget);
                }
                let at = st.positions().get(t).copied();
                (Some(TargetRow::Structure(row_u32(t)?)), at)
            }
        };
        if let (Some(range), Some(at)) = (intent.range(), at) {
            let from = creeps.positions().get(row).copied();
            if !from.is_some_and(|from| within(from, at, range)) {
                return Err(Rejected::NotInRange);
            }
        }
        if intent.amount() == Some(0) {
            return Err(Rejected::ZeroAmount);
        }
        Ok((row_u32(row)?, target))
    }

    /// Ends collection and plans the tick: for each creep (by row), the last push of each method,
    /// ordered blockers first ([`PRIORITY_ORDER`]). One pass over the rows, no sort. The plan does not
    /// depend on push order apart from which push of a method is last. Calling it again does nothing.
    pub fn resolve(&mut self) {
        if self.resolved {
            return;
        }
        self.resolved = true;
        for (&queued, slots) in self.present.iter().zip(&self.latest) {
            if queued.is_empty() {
                continue;
            }
            for action in PRIORITY_ORDER {
                if !queued.contains(action) {
                    continue;
                }
                let entry = slots
                    .get(action as usize)
                    .and_then(|&i| self.entries.get(i as usize));
                if let Some(&issue) = entry {
                    self.planned.push(issue);
                }
            }
        }
        self.stats.planned = u32::try_from(self.planned.len()).unwrap_or(u32::MAX);
    }

    /// The planned intents, once [`Self::resolve`] ran (none before): by row, then blockers first.
    pub fn planned(&self) -> impl ExactSizeIterator<Item = &Issue> {
        self.planned.iter()
    }

    /// Issues the plan (resolving first if needed) through `call`, which makes the JS call and
    /// returns its [`Outcome`], or `None` when it found no JS object to call. Within a creep, an
    /// intent is skipped (counted as blocked) once a call that blocks it was recorded
    /// ([`Outcome::recorded`]); a blocker that the game refused does not block (V45, V108). Each
    /// outcome goes to `log`, and the tick's counters are folded into it at the end. When every
    /// call is accepted, this calls [`ActionSet::kept`] of each creep's queued set.
    pub fn issue(&mut self, log: &mut ActLog, mut call: impl FnMut(&Issue) -> Option<Outcome>) {
        self.resolve();
        let mut blocked = 0u32;
        let mut row = None;
        let mut recorded = ActionSet::EMPTY;
        for issue in &self.planned {
            if row != Some(issue.actor) {
                row = Some(issue.actor);
                recorded = ActionSet::EMPTY;
            }
            let action = issue.intent.action();
            if action.blocked_by().intersects(recorded) {
                blocked = blocked.saturating_add(1);
                continue;
            }
            match call(issue) {
                Some(outcome) => {
                    log.record(action, outcome);
                    if outcome.recorded() {
                        recorded = recorded.with(action);
                    }
                }
                None => log.missing(),
            }
        }
        self.stats.blocked = blocked;
        log.finish(&self.stats);
    }

    pub fn stats(&self) -> &BufferStats {
        &self.stats
    }
}

/// Whether `a` and `b` are within `range` of each other as the engine's `inRangeTo` and
/// `isNearTo` judge it: in the same room, and at most `range` apart on each axis (rooms.js
/// L1362-1381 at a90d26f, V113).
fn within(a: Position, b: Position, range: u8) -> bool {
    a.room_name() == b.room_name()
        && a.x().u8().abs_diff(b.x().u8()) <= range
        && a.y().u8().abs_diff(b.y().u8()) <= range
}

/// Whether a structure of type `kind` is built with a store (engine `build.js` at a90d26f gives one
/// to the spawn, extension, link, storage, tower, lab, power spawn, terminal, container, nuker and
/// factory, and none to the rampart, road, wall, observer and extractor; a controller has none,
/// V113). Types the engine never builds are not judged here.
const fn built_without_store(kind: StructureType) -> bool {
    matches!(
        kind,
        StructureType::Rampart
            | StructureType::Road
            | StructureType::Wall
            | StructureType::Observer
            | StructureType::Extractor
            | StructureType::Controller
    )
}

/// Whether `intent`'s method can act on a structure of type `kind` (V108, V109, V113).
///
/// Uses: `CONSTRUCTION_COST` (`StructureType::construction_cost`): the processor skips a repair of a
/// type without one.
const fn accepts(intent: &CreepIntent, kind: StructureType) -> bool {
    match intent {
        CreepIntent::UpgradeController { .. } => matches!(kind, StructureType::Controller),
        CreepIntent::Transfer { .. } => !built_without_store(kind),
        CreepIntent::Withdraw { .. } => {
            !built_without_store(kind)
                && !matches!(kind, StructureType::Nuker | StructureType::PowerBank)
        }
        CreepIntent::Repair { .. } => kind.construction_cost().is_some(),
        _ => true,
    }
}

/// A row as stored in an [`Issue`]. Rows index vectors, so they fit long before this could fail.
fn row_u32(row: usize) -> Result<u32, Rejected> {
    u32::try_from(row).map_err(|_| Rejected::GoneTarget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{CreepRow, ObjectKey, SkipReason, StructureStatic, TickContext};
    use rustedscreep_consts::{ErrorCode, INTENT_CPU_COST, RoomCoordinate, RoomName};

    fn pos_in(room: &str, x: u8, y: u8) -> Position {
        let room: RoomName = room.parse().expect("room");
        Position::new(
            RoomCoordinate::new(x).expect("x"),
            RoomCoordinate::new(y).expect("y"),
            room,
        )
    }

    fn pos(x: u8, y: u8) -> Position {
        pos_in("W1N1", x, y)
    }

    /// Creeps and structures with their places, so that every scripted intent is in range unless a
    /// test says otherwise. Rows (this tick): a 0, b 1, s 2, h 3, c 4, d 5.
    /// - a (10,10) and b (11,10): own, adjacent; s (9,10): own, spawning, next to a; h (12,10):
    ///   hostile, next to b; c (0,10) in W1N1 and d (49,10) in W2N1: own, at world distance 1 across
    ///   the room border; gone: a creep seen on the previous tick only.
    /// - spawn (10,9) and nuker (11,9), each next to a and b; controller (10,13), 3 from a; observer
    ///   (9,9), next to a; power bank (11,11); extension (30,30), far from everyone; gone: a tower
    ///   seen on the previous tick only.
    struct World {
        snap: Snapshot,
        a: CreepHandle,
        b: CreepHandle,
        s: CreepHandle,
        h: CreepHandle,
        c: CreepHandle,
        d: CreepHandle,
        gone_creep: CreepHandle,
        spawn: StructureHandle,
        controller: StructureHandle,
        nuker: StructureHandle,
        observer: StructureHandle,
        power_bank: StructureHandle,
        far: StructureHandle,
        gone_structure: StructureHandle,
    }

    fn creep(at: Position, my: bool, spawning: bool) -> CreepRow {
        CreepRow {
            pos: at,
            my,
            spawning,
            fatigue: 0,
        }
    }

    fn building(
        kind: StructureType,
        at: Position,
    ) -> impl FnOnce() -> Result<StructureStatic, SkipReason> {
        move || Ok(StructureStatic { kind, pos: at })
    }

    fn world() -> World {
        let mut snap = Snapshot::default();
        // Tick 100: a creep and a tower that are gone on tick 101.
        snap.begin(TickContext::from_raw(100, 20, 500.0, 10_000));
        let gone_creep = snap
            .creeps
            .push(ObjectKey(5), creep(pos(10, 11), true, false))
            .expect("gone creep");
        let gone_structure = snap
            .structures
            .push(ObjectKey(13), building(StructureType::Tower, pos(9, 11)))
            .expect("tower");
        snap.finish();
        snap.begin(TickContext::from_raw(101, 20, 500.0, 10_000));
        let cr = &mut snap.creeps;
        let a = cr
            .push(ObjectKey(1), creep(pos(10, 10), true, false))
            .expect("a");
        let b = cr
            .push(ObjectKey(2), creep(pos(11, 10), true, false))
            .expect("b");
        let s = cr
            .push(ObjectKey(3), creep(pos(9, 10), true, true))
            .expect("s");
        let h = cr
            .push(ObjectKey(4), creep(pos(12, 10), false, false))
            .expect("h");
        let c = cr
            .push(ObjectKey(6), creep(pos(0, 10), true, false))
            .expect("c");
        let d = cr
            .push(ObjectKey(7), creep(pos_in("W2N1", 49, 10), true, false))
            .expect("d");
        let st = &mut snap.structures;
        let mut put = |key, kind, at| {
            st.push(ObjectKey(key), building(kind, at))
                .expect("structure")
        };
        let spawn = put(10, StructureType::Spawn, pos(10, 9));
        let controller = put(11, StructureType::Controller, pos(10, 13));
        let nuker = put(12, StructureType::Nuker, pos(11, 9));
        let observer = put(14, StructureType::Observer, pos(9, 9));
        let power_bank = put(15, StructureType::PowerBank, pos(11, 11));
        let far = put(16, StructureType::Extension, pos(30, 30));
        snap.finish();
        assert_eq!(snap.creeps.row_of(gone_creep), None);
        assert_eq!(snap.structures.row_of(gone_structure), None);
        World {
            snap,
            a,
            b,
            s,
            h,
            c,
            d,
            gone_creep,
            spawn,
            controller,
            nuker,
            observer,
            power_bank,
            far,
            gone_structure,
        }
    }

    fn energy(target: Target, amount: Option<u32>) -> CreepIntent {
        CreepIntent::Transfer {
            target,
            resource: ResourceType::Energy,
            amount,
        }
    }

    fn withdraw(target: StructureHandle, amount: Option<u32>) -> CreepIntent {
        CreepIntent::Withdraw {
            target,
            resource: ResourceType::Energy,
            amount,
        }
    }

    fn mv(direction: Direction) -> CreepIntent {
        CreepIntent::Move { direction }
    }

    type Call = (u32, Option<TargetRow>, CreepIntent);

    /// Issues the buffer with every call answered by `answer`, and returns the calls made.
    fn issue_with(
        buf: &mut IntentBuffer,
        log: &mut ActLog,
        mut answer: impl FnMut(&Issue) -> Option<Outcome>,
    ) -> Vec<Call> {
        let mut calls = Vec::new();
        buf.issue(log, |i| {
            calls.push((i.actor, i.target, i.intent));
            answer(i)
        });
        calls
    }

    fn issue_ok(buf: &mut IntentBuffer) -> Vec<Call> {
        issue_with(buf, &mut ActLog::default(), |_| Some(Outcome::Ok))
    }

    fn intents(calls: &[Call]) -> Vec<(u32, CreepIntent)> {
        calls.iter().map(|&(a, _, i)| (a, i)).collect()
    }

    /// Uses: rustedscreep_consts::INTENT_CPU_COST. A new intent commits what the engine bills for it
    /// (V34): 0.2 for a state-changing method, nothing for say and pull, nothing for a replacement.
    #[test]
    fn commits_follow_what_the_engine_bills() {
        let w = world();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&w.snap);
        let push = |buf: &mut IntentBuffer, c: &mut Commit, actor, intent| {
            buf.push(&w.snap, actor, intent, c)
        };
        assert_eq!(
            push(&mut buf, &mut commit, w.a, mv(Direction::Top)),
            Ok(Queued::New)
        );
        assert_eq!(commit.cpu(), INTENT_CPU_COST);
        let toward = CreepIntent::MoveToward { target: w.b };
        assert_eq!(
            push(&mut buf, &mut commit, w.a, toward),
            Ok(Queued::Replaced)
        );
        assert_eq!(
            commit.cpu(),
            INTENT_CPU_COST,
            "a replacement is not billed again"
        );
        let say = CreepIntent::Say {
            text: SayText::new("hi"),
            public: false,
        };
        assert_eq!(push(&mut buf, &mut commit, w.a, say), Ok(Queued::New));
        let pull = CreepIntent::Pull { target: w.b };
        assert_eq!(push(&mut buf, &mut commit, w.a, pull), Ok(Queued::New));
        assert_eq!(commit.cpu(), INTENT_CPU_COST, "say and pull are free");
        let fill = energy(Target::Structure(w.spawn), None);
        assert_eq!(push(&mut buf, &mut commit, w.b, fill), Ok(Queued::New));
        assert_eq!(commit.cpu(), 2.0 * INTENT_CPU_COST);
        let mut log = ActLog::default();
        let calls = issue_with(&mut buf, &mut log, |_| Some(Outcome::Ok));
        assert_eq!(calls.len(), 4);
        assert_eq!(
            log.issues().billed_cpu(),
            2.0 * INTENT_CPU_COST,
            "what the engine bills"
        );
        let s = buf.stats();
        assert_eq!((s.queued, s.replaced, s.blocked, s.planned), (5, 1, 0, 4));
    }

    #[test]
    fn the_last_push_of_a_method_wins() {
        let w = world();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&w.snap);
        for intent in [
            mv(Direction::Top),
            mv(Direction::Right),
            CreepIntent::MoveToward { target: w.b },
            mv(Direction::Bottom),
        ] {
            buf.push(&w.snap, w.a, intent, &mut commit).expect("queued");
        }
        buf.push(
            &w.snap,
            w.b,
            CreepIntent::MoveToward { target: w.a },
            &mut commit,
        )
        .expect("queued");
        assert_eq!(
            intents(&issue_ok(&mut buf)),
            [
                (0, mv(Direction::Bottom)),
                (1, CreepIntent::MoveToward { target: w.a })
            ],
            "a move by direction and a move toward a creep share the move slot"
        );
    }

    /// Each refusal, with the API behavior it stands for (V108, V109, V113); a refused push changes
    /// nothing, so it never replaces an earlier valid intent of the same method.
    #[test]
    fn refused_pushes_change_nothing() {
        let w = world();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&w.snap);
        let valid = [
            (w.a, energy(Target::Structure(w.spawn), Some(50))),
            (w.a, withdraw(w.spawn, Some(10))),
            (w.a, CreepIntent::Heal { target: w.b }),
            (w.a, CreepIntent::Pull { target: w.b }),
            (w.a, CreepIntent::MoveToward { target: w.b }),
            (w.a, CreepIntent::Repair { target: w.spawn }),
            (
                w.a,
                CreepIntent::UpgradeController {
                    target: w.controller,
                },
            ),
            (
                w.a,
                CreepIntent::Drop {
                    resource: ResourceType::Energy,
                    amount: Some(3),
                },
            ),
        ];
        for (actor, intent) in valid {
            buf.push(&w.snap, actor, intent, &mut commit)
                .expect("queued");
        }
        let before = commit.cpu();
        let toward = |target| CreepIntent::MoveToward { target };
        let cases = [
            (w.gone_creep, mv(Direction::Top), Rejected::GoneActor),
            (w.h, mv(Direction::Top), Rejected::NotMine),
            (w.s, mv(Direction::Top), Rejected::Spawning),
            (
                w.a,
                energy(Target::Creep(w.gone_creep), None),
                Rejected::GoneTarget,
            ),
            (
                w.a,
                energy(Target::Structure(w.gone_structure), None),
                Rejected::GoneTarget,
            ),
            (
                w.a,
                energy(Target::Creep(w.s), None),
                Rejected::SpawningTarget,
            ),
            (
                w.a,
                CreepIntent::Heal { target: w.s },
                Rejected::SpawningTarget,
            ),
            (
                w.a,
                CreepIntent::RangedHeal { target: w.s },
                Rejected::SpawningTarget,
            ),
            (
                w.a,
                CreepIntent::Pull { target: w.s },
                Rejected::SpawningTarget,
            ),
            (w.a, toward(w.s), Rejected::SpawningTarget),
            (
                w.a,
                CreepIntent::Pull { target: w.a },
                Rejected::WrongTarget,
            ),
            (w.a, toward(w.a), Rejected::WrongTarget),
            (w.a, energy(Target::Creep(w.a), None), Rejected::WrongTarget),
            (
                w.a,
                energy(Target::Structure(w.controller), None),
                Rejected::WrongTarget,
            ),
            (
                w.a,
                energy(Target::Structure(w.observer), None),
                Rejected::WrongTarget,
            ),
            (w.a, withdraw(w.controller, None), Rejected::WrongTarget),
            (w.a, withdraw(w.nuker, None), Rejected::WrongTarget),
            (w.a, withdraw(w.observer, None), Rejected::WrongTarget),
            (w.a, withdraw(w.power_bank, None), Rejected::WrongTarget),
            (
                w.a,
                CreepIntent::Repair {
                    target: w.controller,
                },
                Rejected::WrongTarget,
            ),
            (
                w.a,
                CreepIntent::Repair {
                    target: w.power_bank,
                },
                Rejected::WrongTarget,
            ),
            (
                w.a,
                CreepIntent::UpgradeController { target: w.spawn },
                Rejected::WrongTarget,
            ),
            (
                w.a,
                energy(Target::Structure(w.far), None),
                Rejected::NotInRange,
            ),
            (w.a, CreepIntent::Heal { target: w.h }, Rejected::NotInRange),
            (w.a, toward(w.h), Rejected::NotInRange),
            (w.c, energy(Target::Creep(w.d), None), Rejected::NotInRange),
            (
                w.c,
                CreepIntent::RangedHeal { target: w.d },
                Rejected::NotInRange,
            ),
            (
                w.a,
                energy(Target::Structure(w.spawn), Some(0)),
                Rejected::ZeroAmount,
            ),
            (w.a, withdraw(w.spawn, Some(0)), Rejected::ZeroAmount),
            (
                w.a,
                CreepIntent::Drop {
                    resource: ResourceType::Energy,
                    amount: Some(0),
                },
                Rejected::ZeroAmount,
            ),
        ];
        for (actor, intent, reason) in cases {
            assert_eq!(
                buf.push(&w.snap, actor, intent, &mut commit),
                Err(reason),
                "{intent:?}"
            );
        }
        assert_eq!(commit.cpu(), before, "a refusal commits nothing");
        let s = *buf.stats();
        assert_eq!(s.queued as usize, valid.len());
        assert_eq!(s.rejected_total() as usize, cases.len());
        for reason in Rejected::ALL {
            let want = cases.iter().filter(|c| c.2 == reason).count();
            // Closed has its own test, and so has Tired (`a_tired_creep_cannot_move_by_direction`),
            // which needs a fatigued creep this world does not have.
            if reason != Rejected::Closed && reason != Rejected::Tired {
                assert!(want > 0, "{} is exercised", reason.as_str());
            }
            assert_eq!(s.rejected(reason) as usize, want, "{}", reason.as_str());
        }
        // Every valid push survives (heal blocks repair, the rest are outside the table).
        let issued = intents(&issue_ok(&mut buf));
        for (_, intent) in valid {
            let blocked = matches!(intent, CreepIntent::Repair { .. });
            assert_eq!(issued.contains(&(0, intent)), !blocked, "{intent:?}");
        }
        // The accepted counterparts: a repair of the nuker, a heal of a hostile creep, self-heal, a
        // range-3 upgrade and a range-3 rangedHeal.
        let mut buf = IntentBuffer::default();
        buf.begin(&w.snap);
        for (actor, intent) in [
            (w.a, CreepIntent::Repair { target: w.nuker }),
            (w.b, CreepIntent::Heal { target: w.h }),
            (w.a, CreepIntent::RangedHeal { target: w.a }),
            (
                w.b,
                CreepIntent::UpgradeController {
                    target: w.controller,
                },
            ),
            (w.c, CreepIntent::RangedHeal { target: w.a }),
        ] {
            let got = buf.push(&w.snap, actor, intent, &mut commit);
            let far = matches!(intent, CreepIntent::RangedHeal { .. }) && actor == w.c;
            let want = if far {
                Err(Rejected::NotInRange)
            } else {
                Ok(Queued::New)
            };
            assert_eq!(got, want, "{intent:?}");
        }
    }

    #[test]
    fn the_buffer_is_closed_before_begin_after_resolve_and_for_another_snapshot() {
        let w = world();
        let mut commit = Commit::default();
        let mut buf = IntentBuffer::default();
        assert_eq!(
            buf.push(&w.snap, w.a, mv(Direction::Top), &mut commit),
            Err(Rejected::Closed),
            "before begin"
        );
        buf.begin(&w.snap);
        // A snapshot of another tick, and one of the same tick with another creep count: their rows
        // do not index this tick's tables.
        let mut next = Snapshot::default();
        next.begin(TickContext::from_raw(102, 20, 500.0, 10_000));
        let n = next
            .creeps
            .push(ObjectKey(1), creep(pos(10, 10), true, false))
            .expect("n");
        next.finish();
        assert_eq!(
            buf.push(&next, n, mv(Direction::Top), &mut commit),
            Err(Rejected::Closed)
        );
        let mut same_tick = Snapshot::default();
        same_tick.begin(TickContext::from_raw(101, 20, 500.0, 10_000));
        let m = same_tick
            .creeps
            .push(ObjectKey(1), creep(pos(10, 10), true, false))
            .expect("m");
        same_tick.finish();
        assert_eq!(
            buf.push(&same_tick, m, mv(Direction::Top), &mut commit),
            Err(Rejected::Closed)
        );
        // The snapshot it was opened for is accepted until resolve.
        buf.push(&w.snap, w.a, mv(Direction::Top), &mut commit)
            .expect("open");
        assert_eq!(buf.planned().len(), 0, "nothing planned before resolve");
        buf.resolve();
        assert_eq!(buf.planned().len(), 1);
        assert_eq!(
            buf.push(&w.snap, w.b, mv(Direction::Top), &mut commit),
            Err(Rejected::Closed),
            "after resolve"
        );
        assert_eq!(
            buf.stats().rejected(Rejected::Closed),
            3,
            "the refusal before begin was cleared by begin"
        );
        buf.resolve();
        assert_eq!(buf.planned().len(), 1, "resolve twice changes nothing");
    }

    /// When every call is accepted, a creep's heal skips its rangedHeal and repair, and rangedHeal
    /// alone skips repair (V44); the calls come by row, blockers first.
    #[test]
    fn accepted_blockers_skip_what_they_block() {
        let w = world();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&w.snap);
        let repair = CreepIntent::Repair { target: w.spawn };
        let heal = CreepIntent::Heal { target: w.b };
        let ranged = CreepIntent::RangedHeal { target: w.a };
        for (actor, intent) in [
            (w.a, repair),
            (w.a, ranged),
            (w.a, mv(Direction::Top)),
            (w.a, heal),
            (w.b, repair),
            (w.b, ranged),
        ] {
            buf.push(&w.snap, actor, intent, &mut commit)
                .expect("queued");
        }
        let mut log = ActLog::default();
        let calls = issue_with(&mut buf, &mut log, |_| Some(Outcome::Ok));
        assert_eq!(
            intents(&calls),
            [(0, heal), (0, mv(Direction::Top)), (1, ranged)]
        );
        assert_eq!((buf.stats().blocked, buf.stats().planned), (3, 6));
        assert_eq!(log.buffer().blocked, 3, "folded into the log");
    }

    /// A blocker that is not recorded (refused, thrown or missing) does not block, as in the engine
    /// (V45, V108); an unknown return counts as recorded.
    #[test]
    fn a_refused_blocker_does_not_block() {
        let w = world();
        let heal = CreepIntent::Heal { target: w.b };
        let ranged = CreepIntent::RangedHeal { target: w.b };
        let repair = CreepIntent::Repair { target: w.spawn };
        let run = |heal_says: Option<Outcome>, ranged_says: Outcome| {
            let mut buf = IntentBuffer::default();
            let mut commit = Commit::default();
            buf.begin(&w.snap);
            for intent in [repair, ranged, heal] {
                buf.push(&w.snap, w.a, intent, &mut commit).expect("queued");
            }
            let mut log = ActLog::default();
            let calls = issue_with(&mut buf, &mut log, |i| match i.intent {
                CreepIntent::Heal { .. } => heal_says,
                CreepIntent::RangedHeal { .. } => Some(ranged_says),
                _ => Some(Outcome::Ok),
            });
            let called: Vec<CreepIntent> = calls.iter().map(|c| c.2).collect();
            (called, buf.stats().blocked, *log.issues())
        };
        let refused = Some(Outcome::Failed(ErrorCode::NotInRange));
        assert_eq!(run(Some(Outcome::Ok), Outcome::Ok).0, [heal]);
        assert_eq!(run(Some(Outcome::Unknown), Outcome::Ok).0, [heal]);
        assert_eq!(
            run(refused, Outcome::Ok).0,
            [heal, ranged],
            "rangedHeal still blocks repair"
        );
        assert_eq!(run(refused, Outcome::Ok).1, 1);
        let both_refused = run(refused, Outcome::Failed(ErrorCode::NoBodypart));
        assert_eq!(both_refused.0, [heal, ranged, repair]);
        assert_eq!(both_refused.1, 0);
        assert_eq!(run(Some(Outcome::Threw), Outcome::Ok).0, [heal, ranged]);
        let missing = run(None, Outcome::Ok);
        assert_eq!(missing.0, [heal, ranged]);
        assert_eq!((missing.2.issued, missing.2.missing), (1, 1));
    }

    /// Dropping a billed no-op at push also removes it as a blocker: a heal of a spawning creep is
    /// refused, so the creep's rangedHeal is issued.
    #[test]
    fn a_dropped_billed_no_op_blocks_nothing() {
        let w = world();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&w.snap);
        let ranged = CreepIntent::RangedHeal { target: w.b };
        assert_eq!(
            buf.push(&w.snap, w.a, CreepIntent::Heal { target: w.s }, &mut commit),
            Err(Rejected::SpawningTarget)
        );
        buf.push(&w.snap, w.a, ranged, &mut commit).expect("queued");
        assert_eq!(intents(&issue_ok(&mut buf)), [(0, ranged)]);
    }

    /// Every order of the same pushes gives the same calls (only which push of a method is last
    /// matters, and here each method is pushed once per creep).
    #[test]
    fn calls_do_not_depend_on_push_order() {
        let w = world();
        let pushes = [
            (w.b, mv(Direction::Top)),
            (w.a, CreepIntent::Repair { target: w.spawn }),
            (w.a, CreepIntent::RangedHeal { target: w.b }),
            (w.b, energy(Target::Creep(w.a), Some(5))),
            (w.a, mv(Direction::Right)),
        ];
        let mut first = None;
        let mut order: Vec<usize> = (0..pushes.len()).collect();
        let mut run = |order: &[usize]| {
            let mut buf = IntentBuffer::default();
            let mut commit = Commit::default();
            buf.begin(&w.snap);
            for &i in order {
                let (actor, intent) = pushes[i];
                buf.push(&w.snap, actor, intent, &mut commit)
                    .expect("queued");
            }
            let got = issue_ok(&mut buf);
            match &first {
                None => first = Some(got),
                Some(f) => assert_eq!(&got, f, "order {order:?}"),
            }
        };
        // All 120 permutations (Heap's algorithm).
        run(&order);
        let mut c = vec![0usize; order.len()];
        let mut i = 0;
        let mut seen = 1;
        while i < order.len() {
            if c[i] < i {
                if i % 2 == 0 {
                    order.swap(0, i);
                } else {
                    order.swap(c[i], i);
                }
                run(&order);
                seen += 1;
                c[i] += 1;
                i = 0;
            } else {
                c[i] = 0;
                i += 1;
            }
        }
        assert_eq!(seen, 120);
        let f = first.expect("ran");
        assert_eq!(
            f.len(),
            pushes.len() - 1,
            "repair is blocked by rangedHeal: {f:?}"
        );
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(self.0 >> 33).expect("fits") % n
        }
    }

    /// What the world's facts say about a push, written apart from `check`: which creeps can act,
    /// which targets each method takes, the places, and the amount.
    fn model_accepts(w: &World, actor: CreepHandle, intent: &CreepIntent) -> Result<(), Rejected> {
        let creeps = [
            (w.a, pos(10, 10), true, false),
            (w.b, pos(11, 10), true, false),
            (w.s, pos(9, 10), true, true),
            (w.h, pos(12, 10), false, false),
            (w.c, pos(0, 10), true, false),
            (w.d, pos_in("W2N1", 49, 10), true, false),
        ];
        let structures = [
            (w.spawn, pos(10, 9), "spawn"),
            (w.controller, pos(10, 13), "controller"),
            (w.nuker, pos(11, 9), "nuker"),
            (w.observer, pos(9, 9), "observer"),
            (w.power_bank, pos(11, 11), "power bank"),
            (w.far, pos(30, 30), "extension"),
        ];
        let Some(&(_, from, my, spawning)) = creeps.iter().find(|c| c.0 == actor) else {
            return Err(Rejected::GoneActor);
        };
        if !my {
            return Err(Rejected::NotMine);
        }
        if spawning {
            return Err(Rejected::Spawning);
        }
        let near = |p: Position, r: u8| {
            p.room_name() == from.room_name()
                && p.x().u8().abs_diff(from.x().u8()) <= r
                && p.y().u8().abs_diff(from.y().u8()) <= r
        };
        let (at, range) = match *intent {
            CreepIntent::Heal { target }
            | CreepIntent::RangedHeal { target }
            | CreepIntent::Pull { target }
            | CreepIntent::MoveToward { target }
            | CreepIntent::Transfer {
                target: Target::Creep(target),
                ..
            } => {
                let Some(&(_, at, _, sp)) = creeps.iter().find(|c| c.0 == target) else {
                    return Err(Rejected::GoneTarget);
                };
                let self_target = !matches!(
                    intent,
                    CreepIntent::Heal { .. } | CreepIntent::RangedHeal { .. }
                );
                if self_target && target == actor {
                    return Err(Rejected::WrongTarget);
                }
                if sp {
                    return Err(Rejected::SpawningTarget);
                }
                (
                    Some(at),
                    if matches!(intent, CreepIntent::RangedHeal { .. }) {
                        3
                    } else {
                        1
                    },
                )
            }
            CreepIntent::Transfer {
                target: Target::Structure(target),
                ..
            }
            | CreepIntent::Withdraw { target, .. }
            | CreepIntent::Repair { target }
            | CreepIntent::UpgradeController { target } => {
                let Some(&(_, at, kind)) = structures.iter().find(|s| s.0 == target) else {
                    return Err(Rejected::GoneTarget);
                };
                let ok = match intent {
                    CreepIntent::Transfer { .. } => !matches!(kind, "controller" | "observer"),
                    CreepIntent::Withdraw { .. } => {
                        !matches!(kind, "controller" | "observer" | "nuker" | "power bank")
                    }
                    CreepIntent::Repair { .. } => !matches!(kind, "controller" | "power bank"),
                    _ => kind == "controller",
                };
                if !ok {
                    return Err(Rejected::WrongTarget);
                }
                let range = match intent {
                    CreepIntent::Repair { .. } | CreepIntent::UpgradeController { .. } => 3,
                    _ => 1,
                };
                (Some(at), range)
            }
            CreepIntent::Drop { .. } | CreepIntent::Move { .. } | CreepIntent::Say { .. } => {
                (None, 0)
            }
        };
        if at.is_some_and(|at| !near(at, range)) {
            return Err(Rejected::NotInRange);
        }
        if intent.amount() == Some(0) {
            return Err(Rejected::ZeroAmount);
        }
        Ok(())
    }

    /// Seeded random pushes over 2000 ticks against a naive model: acceptance judged from the
    /// world's facts, the last accepted push per creep and method, and (every call accepted) the
    /// kept set per creep in priority order, with the target rows. The buffer keeps its capacity
    /// across ticks.
    #[test]
    fn random_pushes_match_a_naive_model() {
        let w = world();
        let creeps = [w.a, w.b, w.s, w.h, w.c, w.d, w.gone_creep];
        let structures = [
            w.spawn,
            w.controller,
            w.nuker,
            w.observer,
            w.power_bank,
            w.far,
            w.gone_structure,
        ];
        let random_intent = |rng: &mut Lcg| -> CreepIntent {
            let c = creeps[rng.next(creeps.len())];
            let st = structures[rng.next(structures.len())];
            let amount = [None, Some(0), Some(7)][rng.next(3)];
            let d = [Direction::Top, Direction::Left, Direction::BottomRight][rng.next(3)];
            match rng.next(12) {
                0 => mv(d),
                1 => CreepIntent::MoveToward { target: c },
                2 => CreepIntent::Heal { target: c },
                3 => CreepIntent::RangedHeal { target: c },
                4 => CreepIntent::Repair { target: st },
                5 => energy(Target::Creep(c), amount),
                6 => energy(Target::Structure(st), amount),
                7 => withdraw(st, amount),
                8 => CreepIntent::Pull { target: c },
                9 => CreepIntent::UpgradeController { target: st },
                10 => CreepIntent::Say {
                    text: SayText::new(["a", "bb", "ccc"][rng.next(3)]),
                    public: rng.next(2) == 0,
                },
                _ => CreepIntent::Drop {
                    resource: ResourceType::Energy,
                    amount,
                },
            }
        };
        let row_of_creep = |h| u32::try_from(w.snap.creeps.row_of(h).expect("row")).expect("u32");
        let target_row = |t: Option<Target>| {
            t.map(|t| match t {
                Target::Creep(h) => TargetRow::Creep(row_of_creep(h)),
                Target::Structure(h) => TargetRow::Structure(
                    u32::try_from(w.snap.structures.row_of(h).expect("row")).expect("u32"),
                ),
            })
        };
        let mut rng = Lcg(0x5eed);
        let mut buf = IntentBuffer::default();
        let mut capacity = (0, 0, 0);
        let mut accepted_total = 0;
        for round in 0..2000 {
            let mut commit = Commit::default();
            buf.begin(&w.snap);
            // model: (row, action) -> last accepted intent
            let mut model: Vec<((u32, CreepAction), CreepIntent)> = Vec::new();
            for _ in 0..rng.next(24) {
                let actor = creeps[rng.next(creeps.len())];
                let intent = random_intent(&mut rng);
                let want = model_accepts(&w, actor, &intent);
                let got = buf.push(&w.snap, actor, intent, &mut commit).map(|_| ());
                assert_eq!(got, want, "round {round}: {intent:?}");
                if got.is_ok() {
                    accepted_total += 1;
                    let key = (row_of_creep(actor), intent.action());
                    model.retain(|(k, _)| *k != key);
                    model.push((key, intent));
                }
            }
            let mut want = Vec::new();
            for row in 0..u32::try_from(w.snap.creeps.len()).expect("small") {
                let queued = model
                    .iter()
                    .filter(|((r, _), _)| *r == row)
                    .fold(ActionSet::EMPTY, |s, ((_, a), _)| s.with(*a));
                let mut kept = ActionSet::EMPTY;
                for action in PRIORITY_ORDER {
                    if queued.contains(action) && !action.blocked_by().intersects(kept) {
                        kept = kept.with(action);
                        let intent = model
                            .iter()
                            .find(|(k, _)| *k == (row, action))
                            .expect("in model")
                            .1;
                        want.push((row, target_row(intent.target()), intent));
                    }
                }
                assert_eq!(kept, queued.kept(), "round {round}");
            }
            assert_eq!(issue_ok(&mut buf), want, "round {round}");
            if round == 1000 {
                capacity = (
                    buf.entries.capacity(),
                    buf.planned.capacity(),
                    buf.latest.len(),
                );
            }
        }
        assert!(
            accepted_total > 2000,
            "the model accepts enough pushes ({accepted_total})"
        );
        assert_eq!(
            (
                buf.entries.capacity(),
                buf.planned.capacity(),
                buf.latest.len()
            ),
            capacity,
            "steady state reuses the capacity"
        );
    }

    /// Uses: rustedscreep_consts::CREEP_SAY_MAX_LENGTH. The processor keeps `substring(0, 10)` (UTF-16
    /// units, V109); the text is cut to that length without splitting a character.
    #[test]
    fn say_text_is_cut_to_what_the_engine_keeps() {
        let max = SAY_MAX;
        assert_eq!(
            max, 10,
            "the API constant equals the processor's substring(0, 10) (V109)"
        );
        let units = |s: &str| s.encode_utf16().count();
        assert_eq!(SayText::new("").as_str(), "");
        assert_eq!(SayText::new("hello").as_str(), "hello");
        let exact = "x".repeat(max);
        assert_eq!(SayText::new(&exact).as_str(), exact);
        let long = "y".repeat(max + 5);
        assert_eq!(units(SayText::new(&long).as_str()), max);
        // Three-byte characters, one unit each: max of them fit.
        let wide = "€".repeat(max + 1);
        assert_eq!(SayText::new(&wide).as_str(), "€".repeat(max));
        // A two-unit character that would end past the limit is left out whole.
        let tail = format!("{}😀", "z".repeat(max - 1));
        assert_eq!(SayText::new(&tail).as_str(), "z".repeat(max - 1));
        let emoji = "😀".repeat(max);
        assert_eq!(units(SayText::new(&emoji).as_str()), max);
        assert_eq!(SayText::new(&emoji).as_str(), "😀".repeat(max / 2));
        assert_eq!(format!("{:?}", SayText::new("hi")), "\"hi\"");
    }

    /// `within` follows the engine's inRangeTo/isNearTo: Chebyshev distance on each axis and the same
    /// room; positions next to each other across a room border are not in range (V113).
    #[test]
    fn range_needs_the_same_room() {
        assert!(within(pos(10, 10), pos(13, 7), 3));
        assert!(!within(pos(10, 10), pos(14, 10), 3));
        assert!(within(pos(10, 10), pos(10, 10), 1));
        assert!(!within(pos(0, 10), pos_in("W2N1", 49, 10), 1));
        assert!(!within(pos(0, 10), pos_in("W2N1", 0, 10), 1));
    }

    /// A fatigued creep's `move(direction)` is refused as `ERR_TIRED` would be (unbilled, nothing
    /// replaced, V108, V160); its move toward a creep, which the API lets through (V108), and its
    /// other methods are not.
    #[test]
    fn a_tired_creep_cannot_move_by_direction() {
        let mut snap = Snapshot::default();
        snap.begin(TickContext::from_raw(200, 20, 500.0, 10_000));
        let tired = snap
            .creeps
            .push(
                ObjectKey(1),
                CreepRow {
                    fatigue: 2,
                    ..creep(pos(10, 10), true, false)
                },
            )
            .expect("tired");
        let other = snap
            .creeps
            .push(ObjectKey(2), creep(pos(11, 10), true, false))
            .expect("other");
        snap.finish();
        let mut buf = IntentBuffer::default();
        let mut commit = Commit::default();
        buf.begin(&snap);
        let toward = CreepIntent::MoveToward { target: other };
        assert_eq!(buf.push(&snap, tired, toward, &mut commit), Ok(Queued::New));
        let before = commit.cpu();
        assert_eq!(
            buf.push(&snap, tired, mv(Direction::Right), &mut commit),
            Err(Rejected::Tired)
        );
        assert_eq!(commit.cpu(), before, "refused, nothing committed");
        assert_eq!(buf.stats().rejected(Rejected::Tired), 1);
        assert_eq!(
            buf.push(&snap, other, mv(Direction::Left), &mut commit),
            Ok(Queued::New)
        );
        let say = CreepIntent::Say {
            text: SayText::new("z"),
            public: false,
        };
        assert_eq!(buf.push(&snap, tired, say, &mut commit), Ok(Queued::New));
        buf.resolve();
        let calls = intents(&issue_ok(&mut buf));
        assert!(
            calls.contains(&(0, toward)),
            "the earlier move toward is kept: {calls:?}"
        );
        assert!(calls.contains(&(1, mv(Direction::Left))));
    }
}
