//! The engine's creep intent-priority table, as data (ROADMAP P1 module 7).
//!
//! Source: screeps/engine at [`ENGINE_COMMIT`], `src/processor/intents/creeps/intents.js`
//! (<https://raw.githubusercontent.com/screeps/engine/a90d26fc9eb943147799e65d0bf9d4d0d79a2722/src/processor/intents/creeps/intents.js>):
//! the `priorities` object (VERIFICATION_LOG V44), the `creepActions` order and `checkPriorities`
//! (V45), and the actions outside the table (V46). Identical in npm `@screeps/engine` 4.3.0 and
//! 4.3.2 (V57). The API exports no constant for it.
//!
//! The rule the processor applies (V45): an action in a creep's intent set runs only if none of the
//! actions that block it is in the same set. The test is on the set as queued, not on what ran, so
//! an action that is itself blocked still blocks others, and an action that does nothing (healing a
//! full-HP creep) still blocks (V47). A blocked intent is skipped silently but still billed
//! `INTENT_CPU_COST` (V34), which is why the act layer does not issue one.
//!
//! Only a call that the game API accepts records an intent (V108), and only a recorded intent
//! blocks. The act layer therefore issues a creep's intents blockers first ([`PRIORITY_ORDER`]) and
//! skips one only when a blocker's call was recorded. When every call is accepted this issues
//! [`ActionSet::kept`]: it runs whole in the engine, and it keeps every action the engine would run
//! on the full queued set ([`ActionSet::executed`]) plus any that only a skipped action blocked.

use rustedscreep_consts::INTENT_CPU_COST;

/// The engine commit this table was read from.
pub const ENGINE_COMMIT: &str = "a90d26fc9eb943147799e65d0bf9d4d0d79a2722";

/// A creep action, in the order the engine's processor runs them (`creepActions`, V45). The
/// discriminant is the action's bit in an [`ActionSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum CreepAction {
    Drop,
    Transfer,
    Withdraw,
    Pickup,
    Heal,
    RangedHeal,
    Dismantle,
    Attack,
    Harvest,
    Move,
    Repair,
    Build,
    RangedMassAttack,
    RangedAttack,
    Say,
    Suicide,
    ClaimController,
    UpgradeController,
    ReserveController,
    AttackController,
    GenerateSafeMode,
    SignController,
    Pull,
}

impl CreepAction {
    /// Every action, in the engine's processing order.
    pub const ALL: [Self; 23] = [
        Self::Drop,
        Self::Transfer,
        Self::Withdraw,
        Self::Pickup,
        Self::Heal,
        Self::RangedHeal,
        Self::Dismantle,
        Self::Attack,
        Self::Harvest,
        Self::Move,
        Self::Repair,
        Self::Build,
        Self::RangedMassAttack,
        Self::RangedAttack,
        Self::Say,
        Self::Suicide,
        Self::ClaimController,
        Self::UpgradeController,
        Self::ReserveController,
        Self::AttackController,
        Self::GenerateSafeMode,
        Self::SignController,
        Self::Pull,
    ];

    /// The intent's name in the engine, which is also the name of the `Creep` method that queues it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Transfer => "transfer",
            Self::Withdraw => "withdraw",
            Self::Pickup => "pickup",
            Self::Heal => "heal",
            Self::RangedHeal => "rangedHeal",
            Self::Dismantle => "dismantle",
            Self::Attack => "attack",
            Self::Harvest => "harvest",
            Self::Move => "move",
            Self::Repair => "repair",
            Self::Build => "build",
            Self::RangedMassAttack => "rangedMassAttack",
            Self::RangedAttack => "rangedAttack",
            Self::Say => "say",
            Self::Suicide => "suicide",
            Self::ClaimController => "claimController",
            Self::UpgradeController => "upgradeController",
            Self::ReserveController => "reserveController",
            Self::AttackController => "attackController",
            Self::GenerateSafeMode => "generateSafeMode",
            Self::SignController => "signController",
            Self::Pull => "pull",
        }
    }

    /// The actions whose presence in the same creep's intent set stops this one (V44). Empty for
    /// every action outside the table (V46), `Heal` included: heal is never blocked.
    pub const fn blocked_by(self) -> ActionSet {
        use CreepAction::*;
        // Each list is copied from `priorities` at ENGINE_COMMIT, in the engine's own order.
        match self {
            RangedHeal => ActionSet::of(&[Heal]),
            AttackController => ActionSet::of(&[RangedHeal, Heal]),
            Dismantle => ActionSet::of(&[AttackController, RangedHeal, Heal]),
            Repair => ActionSet::of(&[Dismantle, AttackController, RangedHeal, Heal]),
            Build => ActionSet::of(&[Repair, Dismantle, AttackController, RangedHeal, Heal]),
            Attack => {
                ActionSet::of(&[Build, Repair, Dismantle, AttackController, RangedHeal, Heal])
            }
            Harvest => ActionSet::of(&[
                Attack,
                Build,
                Repair,
                Dismantle,
                AttackController,
                RangedHeal,
                Heal,
            ]),
            RangedMassAttack => ActionSet::of(&[Build, Repair, RangedHeal]),
            RangedAttack => ActionSet::of(&[RangedMassAttack, Build, Repair, RangedHeal]),
            _ => ActionSet::EMPTY,
        }
    }

    /// Whether the engine charges nothing for this intent: the driver's `freeMethods` are `say` and
    /// `pull` (V34).
    pub const fn is_free(self) -> bool {
        matches!(self, Self::Say | Self::Pull)
    }

    /// The CPU the engine bills for this intent once it is recorded. Unit: CPU.
    ///
    /// Uses: `INTENT_CPU_COST` (0.2 per state-changing intent, V34); 0 for the free methods.
    pub fn cpu(self) -> f64 {
        if self.is_free() { 0.0 } else { INTENT_CPU_COST }
    }

    const fn bit(self) -> u32 {
        1 << (self as u8)
    }
}

/// The order the act layer issues a creep's intents in: the ten actions in the table so that every
/// action comes after all of its blockers, then the outside actions in the engine's processing order.
/// JS call order does not change what the engine does (V47); this order lets a blocker's result
/// decide whether the actions it blocks are issued.
pub const PRIORITY_ORDER: [CreepAction; 23] = [
    CreepAction::Heal,
    CreepAction::RangedHeal,
    CreepAction::AttackController,
    CreepAction::Dismantle,
    CreepAction::Repair,
    CreepAction::Build,
    CreepAction::Attack,
    CreepAction::Harvest,
    CreepAction::RangedMassAttack,
    CreepAction::RangedAttack,
    CreepAction::Drop,
    CreepAction::Transfer,
    CreepAction::Withdraw,
    CreepAction::Pickup,
    CreepAction::Move,
    CreepAction::Say,
    CreepAction::Suicide,
    CreepAction::ClaimController,
    CreepAction::UpgradeController,
    CreepAction::ReserveController,
    CreepAction::GenerateSafeMode,
    CreepAction::SignController,
    CreepAction::Pull,
];

/// A set of creep actions, one bit per [`CreepAction`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActionSet(u32);

impl ActionSet {
    pub const EMPTY: Self = Self(0);

    /// The set of `actions`.
    pub const fn of(actions: &[CreepAction]) -> Self {
        let mut bits = 0u32;
        let mut rest = actions;
        while let [first, tail @ ..] = rest {
            bits |= first.bit();
            rest = tail;
        }
        Self(bits)
    }

    pub const fn contains(self, action: CreepAction) -> bool {
        self.0 & action.bit() != 0
    }

    pub const fn with(self, action: CreepAction) -> Self {
        Self(self.0 | action.bit())
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Number of actions in the set.
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// The actions in `self` that are not in `other`.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The actions in the set, in the engine's processing order.
    pub fn iter(self) -> impl Iterator<Item = CreepAction> {
        CreepAction::ALL
            .into_iter()
            .filter(move |&a| self.contains(a))
    }

    /// The actions the engine runs when a creep's recorded intents are exactly `self`
    /// (`checkPriorities`, V45): each action none of whose blockers is in `self`. This is the engine
    /// model; the act layer issues [`Self::kept`].
    pub fn executed(self) -> Self {
        let mut run = self;
        for action in self.iter() {
            if action.blocked_by().intersects(self) {
                run = run.without(Self::of(&[action]));
            }
        }
        run
    }

    /// The actions the act layer issues for a creep that queued `self`, when every call it makes is
    /// accepted: in [`PRIORITY_ORDER`], each queued action none of whose blockers was kept. The
    /// result runs whole in the engine (`kept().executed() == kept()`), contains `executed()`, and
    /// every action it leaves out has a kept blocker, so no issued intent is billed for nothing.
    pub fn kept(self) -> Self {
        let mut kept = Self::EMPTY;
        for action in PRIORITY_ORDER {
            if self.contains(action) && !action.blocked_by().intersects(kept) {
                kept = kept.with(action);
            }
        }
        kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CreepAction::*;

    /// The `priorities` object at ENGINE_COMMIT, transcribed name for name from the engine source
    /// (V44); the pinned example the table is checked against.
    const ENGINE_PRIORITIES: [(&str, &[&str]); 9] = [
        ("rangedHeal", &["heal"]),
        ("attackController", &["rangedHeal", "heal"]),
        ("dismantle", &["attackController", "rangedHeal", "heal"]),
        (
            "repair",
            &["dismantle", "attackController", "rangedHeal", "heal"],
        ),
        (
            "build",
            &[
                "repair",
                "dismantle",
                "attackController",
                "rangedHeal",
                "heal",
            ],
        ),
        (
            "attack",
            &[
                "build",
                "repair",
                "dismantle",
                "attackController",
                "rangedHeal",
                "heal",
            ],
        ),
        (
            "harvest",
            &[
                "attack",
                "build",
                "repair",
                "dismantle",
                "attackController",
                "rangedHeal",
                "heal",
            ],
        ),
        ("rangedMassAttack", &["build", "repair", "rangedHeal"]),
        (
            "rangedAttack",
            &["rangedMassAttack", "build", "repair", "rangedHeal"],
        ),
    ];

    /// `creepActions` at ENGINE_COMMIT, in order (V45).
    const ENGINE_CREEP_ACTIONS: [&str; 23] = [
        "drop",
        "transfer",
        "withdraw",
        "pickup",
        "heal",
        "rangedHeal",
        "dismantle",
        "attack",
        "harvest",
        "move",
        "repair",
        "build",
        "rangedMassAttack",
        "rangedAttack",
        "say",
        "suicide",
        "claimController",
        "upgradeController",
        "reserveController",
        "attackController",
        "generateSafeMode",
        "signController",
        "pull",
    ];

    fn by_name(name: &str) -> CreepAction {
        *CreepAction::ALL
            .iter()
            .find(|a| a.as_str() == name)
            .unwrap_or_else(|| panic!("no action named {name}"))
    }

    /// `checkPriorities` written the engine's way, over names: an intent runs if it is present and
    /// none of its listed blockers is present (`_.any(priorities[name], i => !!intents[i])`).
    fn engine_runs(present: &[&str]) -> Vec<&'static str> {
        ENGINE_CREEP_ACTIONS
            .into_iter()
            .filter(|name| present.contains(name))
            .filter(|name| {
                ENGINE_PRIORITIES
                    .iter()
                    .find(|(n, _)| n == name)
                    .is_none_or(|(_, blockers)| !blockers.iter().any(|b| present.contains(b)))
            })
            .collect()
    }

    #[test]
    fn actions_follow_the_engine_processing_order() {
        let names: Vec<&str> = CreepAction::ALL.iter().map(|a| a.as_str()).collect();
        assert_eq!(names, ENGINE_CREEP_ACTIONS);
        for (i, a) in CreepAction::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(a as u8), i, "{a:?} has its own bit");
        }
        assert!(CreepAction::ALL.len() <= 32, "the set is a u32");
    }

    #[test]
    fn the_table_matches_the_engine_priorities() {
        for action in CreepAction::ALL {
            let engine = ENGINE_PRIORITIES
                .iter()
                .find(|(n, _)| *n == action.as_str())
                .map_or(ActionSet::EMPTY, |(_, blockers)| {
                    blockers
                        .iter()
                        .fold(ActionSet::EMPTY, |s, b| s.with(by_name(b)))
                });
            assert_eq!(action.blocked_by(), engine, "{}", action.as_str());
        }
        assert!(Heal.blocked_by().is_empty(), "heal is never blocked (V44)");
        // V46: these are outside the table, so nothing blocks them and they block nothing.
        let outside = [
            Drop,
            Transfer,
            Withdraw,
            Pickup,
            Move,
            Say,
            Suicide,
            Pull,
            ClaimController,
            UpgradeController,
            ReserveController,
            GenerateSafeMode,
            SignController,
        ];
        for a in outside {
            assert!(a.blocked_by().is_empty(), "{a:?} is never blocked");
            for b in CreepAction::ALL {
                assert!(!b.blocked_by().contains(a), "{a:?} blocks nothing");
            }
        }
    }

    /// Every subset of the ten actions in the table, plus each subset with every outside action added,
    /// against the engine's rule written over names.
    #[test]
    fn executed_matches_the_engine_on_every_subset_of_the_table() {
        let table: Vec<CreepAction> = CreepAction::ALL
            .into_iter()
            .filter(|a| {
                !a.blocked_by().is_empty()
                    || CreepAction::ALL.iter().any(|b| b.blocked_by().contains(*a))
            })
            .collect();
        assert_eq!(table.len(), 10);
        let outside: Vec<CreepAction> = CreepAction::ALL
            .into_iter()
            .filter(|a| !table.contains(a))
            .collect();
        for mask in 0u32..(1 << table.len()) {
            let chosen: Vec<CreepAction> = table
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, &a)| a)
                .collect();
            for with_outside in [false, true] {
                let mut present = chosen.clone();
                if with_outside {
                    present.extend(&outside);
                }
                let names: Vec<&str> = present.iter().map(|a| a.as_str()).collect();
                let set = ActionSet::of(&present);
                let got: Vec<&str> = set.executed().iter().map(|a| a.as_str()).collect();
                assert_eq!(got, engine_runs(&names), "queued {names:?}");
            }
        }
    }

    /// Properties of `executed` over every subset of the table's ten actions, each combined with
    /// every subset of three outside actions (all 23 would be 8 million sets, and an outside action
    /// interacts with nothing): it keeps a subset, issuing the result runs all of it, outside actions
    /// and heal always run, and a non-empty set always runs something (the table has no cycle).
    #[test]
    fn executed_is_a_stable_non_empty_subset() {
        let table = [
            Heal,
            RangedHeal,
            Dismantle,
            Attack,
            Harvest,
            Repair,
            Build,
            RangedMassAttack,
            RangedAttack,
            AttackController,
        ];
        let outside = [Move, Say, Transfer];
        for mask in 0u32..(1 << (table.len() + outside.len())) {
            let set = table
                .iter()
                .chain(&outside)
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .fold(ActionSet::EMPTY, |s, (_, &a)| s.with(a));
            let run = set.executed();
            assert_eq!(run.without(set), ActionSet::EMPTY, "a subset");
            assert_eq!(run.executed(), run, "issuing only what runs runs all of it");
            assert_eq!(set.is_empty(), run.is_empty(), "something runs");
            for a in set.iter() {
                if a == Heal || a.blocked_by().is_empty() {
                    assert!(run.contains(a), "{a:?} always runs");
                }
            }
        }
    }

    /// The derived consequences recorded in V56, as pinned cases.
    #[test]
    fn the_v56_combinations_hold() {
        let runs = |queued: &[CreepAction]| ActionSet::of(queued).executed();
        assert_eq!(runs(&[Dismantle, Move]), ActionSet::of(&[Dismantle, Move]));
        assert_eq!(
            runs(&[Dismantle, RangedAttack]),
            ActionSet::of(&[Dismantle, RangedAttack])
        );
        for healer in [Heal, RangedHeal, AttackController] {
            assert!(
                !runs(&[Dismantle, healer]).contains(Dismantle),
                "{healer:?}"
            );
        }
        for worker in [Attack, Build, Repair, Harvest] {
            assert_eq!(
                runs(&[Dismantle, worker]),
                ActionSet::of(&[Dismantle]),
                "{worker:?}"
            );
        }
        assert_eq!(runs(&[Attack, Heal]), ActionSet::of(&[Heal]));
        assert_eq!(
            runs(&[RangedAttack, Heal]),
            ActionSet::of(&[RangedAttack, Heal])
        );
        assert_eq!(
            runs(&[RangedMassAttack, RangedAttack]),
            ActionSet::of(&[RangedMassAttack])
        );
        assert_eq!(runs(&[Heal, RangedHeal]), ActionSet::of(&[Heal]));
        assert_eq!(
            runs(&[Heal, Move, RangedAttack, RangedMassAttack]),
            ActionSet::of(&[Heal, Move, RangedMassAttack])
        );
        // A recorded action that is itself blocked still blocks (V45): rangedHeal loses to heal but
        // still stops rangedAttack, which heal alone would not. The act layer does not issue the
        // rangedHeal, so it issues heal and rangedAttack (`kept`), and both run.
        assert_eq!(
            runs(&[Heal, RangedHeal, RangedAttack]),
            ActionSet::of(&[Heal])
        );
        let kept = ActionSet::of(&[Heal, RangedHeal, RangedAttack]).kept();
        assert_eq!(kept, ActionSet::of(&[Heal, RangedAttack]));
        assert_eq!(kept.executed(), kept);
        assert_eq!(
            runs(&[Heal, RangedAttack]),
            ActionSet::of(&[Heal, RangedAttack])
        );
    }

    /// PRIORITY_ORDER lists every action once, puts each after all of its blockers, and keeps the
    /// outside actions in the engine's processing order.
    #[test]
    fn priority_order_puts_blockers_first() {
        let pos = |a: CreepAction| PRIORITY_ORDER.iter().position(|&b| b == a).expect("listed");
        assert_eq!(
            ActionSet::of(&PRIORITY_ORDER),
            ActionSet::of(&CreepAction::ALL)
        );
        for a in CreepAction::ALL {
            for blocker in a.blocked_by().iter() {
                assert!(pos(blocker) < pos(a), "{blocker:?} before {a:?}");
            }
        }
        let outside: Vec<CreepAction> = PRIORITY_ORDER
            .into_iter()
            .filter(|a| a.blocked_by().is_empty() && *a != Heal)
            .filter(|a| !CreepAction::ALL.iter().any(|b| b.blocked_by().contains(*a)))
            .collect();
        assert_eq!(outside.len(), 13);
        let mut sorted = outside.clone();
        sorted.sort();
        assert_eq!(outside, sorted, "outside actions in processing order");
    }

    /// Over every subset of the table's ten actions plus three outside ones: `kept` is a subset that
    /// runs whole, contains what the engine runs on the full set, and leaves out only actions with a
    /// kept blocker.
    #[test]
    fn kept_runs_whole_and_drops_only_blocked_actions() {
        let table = [
            Heal,
            RangedHeal,
            Dismantle,
            Attack,
            Harvest,
            Repair,
            Build,
            RangedMassAttack,
            RangedAttack,
            AttackController,
        ];
        let outside = [Move, Say, Pull];
        let mut differs = 0;
        for mask in 0u32..(1 << (table.len() + outside.len())) {
            let set = table
                .iter()
                .chain(&outside)
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .fold(ActionSet::EMPTY, |s, (_, &a)| s.with(a));
            let kept = set.kept();
            assert_eq!(kept.without(set), ActionSet::EMPTY, "a subset");
            assert_eq!(kept.executed(), kept, "runs whole");
            assert_eq!(
                set.executed().without(kept),
                ActionSet::EMPTY,
                "keeps what runs"
            );
            for dropped in set.without(kept).iter() {
                assert!(
                    dropped.blocked_by().intersects(kept),
                    "{dropped:?} has a kept blocker"
                );
            }
            differs += u32::from(kept != set.executed());
        }
        assert!(differs > 0, "kept issues more than executed on some sets");
    }

    /// Uses: rustedscreep_consts::INTENT_CPU_COST. Say and pull are the driver's free methods (V34).
    #[test]
    fn only_say_and_pull_are_free() {
        for a in CreepAction::ALL {
            let free = matches!(a, Say | Pull);
            assert_eq!(a.is_free(), free, "{a:?}");
            assert_eq!(a.cpu(), if free { 0.0 } else { INTENT_CPU_COST });
        }
    }

    #[test]
    fn set_operations() {
        let s = ActionSet::EMPTY.with(Heal).with(Move);
        assert!(s.contains(Heal) && s.contains(Move) && !s.contains(Say));
        assert_eq!(s.len(), 2);
        assert_eq!(s.iter().collect::<Vec<_>>(), [Heal, Move]);
        assert_eq!(s.without(ActionSet::of(&[Heal])), ActionSet::of(&[Move]));
        assert!(s.intersects(ActionSet::of(&[Move, Say])));
        assert!(!s.intersects(ActionSet::of(&[Say])));
        assert!(ActionSet::EMPTY.is_empty() && !s.is_empty());
        assert_eq!(ActionSet::of(&CreepAction::ALL).len(), 23);
    }
}
