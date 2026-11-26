//! Tests of terminal balancing (P9.4, ADR-047, `economy::terminals`). Expected values derive
//! from the constants (`TERMINAL_SEND_COST_SCALE`, `TERMINAL_COOLDOWN`, `TERMINAL_CAPACITY`,
//! `CONTROLLER_STRUCTURES`, `REACTIONS` through `ResourceType::reaction_components`) and the
//! terminals config. The world below plays the engine's side as V435 to V441 read it: the game
//! side's checks, then the global pass that drops a send to a room without an own terminal and
//! clips one to the destination's free room. Any call the engine would refuse is a model error.

use rustedscreep_consts::{
    ErrorCode, Position, ResourceType, RoomCoordinate, RoomName, StructureType, TERMINAL_CAPACITY,
    TERMINAL_COOLDOWN, TERMINAL_SEND_COST_SCALE,
};

use super::Hauling;
use super::terminals::*;
use crate::config::{TERMINALS_MMO, TerminalsConfig};
use crate::intents::{CreepIntent, IntentBuffer, Outcome};
use crate::planner::rules::earliest_level;
use crate::scheduler::Commit;
use crate::snapshot::{
    BALANCED_RESOURCES, BalancedStock, Controller, CreepRow, LAB_RESOURCES, ObjectKey, Snapshot,
    StructureHandle, StructureStatic, TerminalRead, TickContext, balanced_resource,
};

use crate::spawning::{Identity, Role};

use ResourceType as R;

fn room(name: &str) -> RoomName {
    name.parse().expect("room")
}

fn pos(r: RoomName, x: u8, y: u8) -> Position {
    Position::new(
        RoomCoordinate::new(x).expect("x"),
        RoomCoordinate::new(y).expect("y"),
        r,
    )
}

/// The first level that allows a terminal (`CONTROLLER_STRUCTURES`).
fn terminal_level() -> u8 {
    earliest_level(StructureType::Terminal, 0).expect("a terminal is allowed")
}

fn index(r: ResourceType) -> usize {
    balanced_resource(r).expect("a balanced resource")
}

/// The engine's fee (V432), written out again here: `ceil(amount × (1 − e^(−range / 30)))`.
fn engine_fee(amount: u32, range: u32) -> u32 {
    let scale = f64::from(TERMINAL_SEND_COST_SCALE);
    (f64::from(amount) * (1.0 - (-f64::from(range) / scale).exp())).ceil() as u32
}

/// The engine's distance without the wrap (V433): the larger coordinate difference.
fn engine_range(a: RoomName, b: RoomName) -> u32 {
    let (ax, ay) = (a.x_coord(), a.y_coord());
    let (bx, by) = (b.x_coord(), b.y_coord());
    ((ax - bx).unsigned_abs()).max((ay - by).unsigned_abs())
}

// ---- The fee and the range ----

/// The fee is the engine's formula for every amount and range, never more than the amount, and
/// grows with both; range 0 is free. Pinned from V432's formula: 1000 units cost 33 energy to a
/// neighbor and 633 over `TERMINAL_SEND_COST_SCALE` rooms.
#[test]
fn the_fee_is_the_engines_formula() {
    for range in 0..=60 {
        let mut last = 0;
        for amount in (0..=300_000).step_by(997) {
            let f = fee(amount, range);
            assert_eq!(f, engine_fee(amount, range), "{amount} over {range}");
            assert!(f <= amount && f >= last, "{amount} over {range}: {f}");
            last = f;
        }
        if range > 0 {
            assert!(fee(1000, range) >= fee(1000, range - 1));
        }
    }
    assert_eq!(fee(5000, 0), 0);
    assert_eq!(fee(1000, 1), engine_fee(1000, 1));
    assert_eq!(engine_fee(1000, 1), 33);
    assert_eq!(engine_fee(1000, TERMINAL_SEND_COST_SCALE), 633);
}

/// `affordable` is the largest amount whose fee the energy pays: its fee fits, one more does
/// not; range 0 is unbounded.
#[test]
fn affordable_is_the_most_the_energy_pays_the_fee_of() {
    for range in 1..=60 {
        for energy in (0..=20_000).step_by(131) {
            let a = affordable(energy, range);
            assert!(fee(a, range) <= energy, "{energy} over {range}: {a}");
            assert!(fee(a + 1, range) > energy, "{energy} over {range}: {a} + 1");
        }
    }
    assert_eq!(affordable(0, 0), u32::MAX);
}

/// The range is the larger of the horizontal and vertical room distances, symmetric, 0 for one
/// room. Pinned from V446's coordinates: E3N5 is (3, -6), W2S1 is (-3, 1), so 7.
#[test]
fn the_range_is_the_larger_room_distance() {
    let (a, b) = (room("E3N5"), room("W2S1"));
    assert_eq!(range(a, b), 7);
    assert_eq!(range(b, a), 7);
    assert_eq!(range(a, a), 0);
    assert_eq!(range(room("E0N0"), room("W0N0")), 1, "across the meridian");
    assert_eq!(range(room("E0S0"), room("E0N0")), 1, "across the equator");
    for (x, y) in [("E7N57", "E8N57"), ("E7N57", "E9N60"), ("W10S3", "E4N2")] {
        assert_eq!(range(room(x), room(y)), engine_range(room(x), room(y)));
    }
}

/// The balanced resources are the base minerals: the lab resources that no reaction makes
/// (`REACTIONS`), each once; energy is not one.
#[test]
fn the_balanced_resources_are_the_base_minerals() {
    let base: Vec<ResourceType> = LAB_RESOURCES
        .iter()
        .copied()
        .filter(|r| r.reaction_components().is_none())
        .collect();
    assert_eq!(BALANCED_RESOURCES.to_vec(), base);
    for (i, &r) in BALANCED_RESOURCES.iter().enumerate() {
        assert_eq!(balanced_resource(r), Some(i));
    }
    assert_eq!(balanced_resource(R::Energy), None);
    assert_eq!(balanced_resource(R::Ghodium), None, "made by a reaction");
}

/// `Hauling::set_banks` keeps one bank a room, the larger want, so the factory's bank and a
/// send's fee in one room do not hide each other (ADR-047).
#[test]
fn a_room_banked_twice_keeps_the_larger_want() {
    let mut snap = Snapshot::default();
    snap.begin(TickContext::from_raw(1, 20, 500.0, 10_000));
    let (a, b) = (room("E1N1"), room("E2N1"));
    let mut handle = |k: u32, r: RoomName| -> (StructureHandle, Position) {
        let at = pos(r, 20, 20);
        let h = snap
            .structures
            .push(ObjectKey(u128::from(k)), || {
                Ok(StructureStatic {
                    kind: StructureType::Terminal,
                    pos: at,
                })
            })
            .expect("terminal");
        (h, at)
    };
    let (ha, pa) = handle(1, a);
    let (hb, pb) = handle(2, b);
    let mut hauling = Hauling::default();
    hauling.set_banks(&[
        (a, ha, pa, 5),
        (b, hb, pb, 0),
        (a, ha, pa, 900),
        (a, ha, pa, 7),
    ]);
    assert_eq!(hauling.banks(), &[(a, ha, pa, 900)]);
}

// ---- The plan, on reads alone ----

/// A structure handle for the reads the plan tests make (the plan never looks at it).
fn any_handle() -> StructureHandle {
    let mut snap = Snapshot::default();
    snap.begin(TickContext::from_raw(1, 20, 500.0, 10_000));
    snap.structures
        .push(ObjectKey(1), || {
            Ok(StructureStatic {
                kind: StructureType::Terminal,
                pos: pos(room("E1N1"), 20, 20),
            })
        })
        .expect("terminal")
}

fn read(name: &str, stock: &[(ResourceType, u32)], energy: u32) -> TerminalRead {
    let r = room(name);
    let mut s: BalancedStock = [0; BALANCED_RESOURCES.len()];
    for &(k, n) in stock {
        s[index(k)] = n;
    }
    let used: u32 = s.iter().sum::<u32>() + energy;
    TerminalRead {
        room: r,
        handle: any_handle(),
        row: 0,
        at: pos(r, 20, 20),
        cooldown: 0,
        free: TERMINAL_CAPACITY - used,
        energy,
        stock: s,
    }
}

const FREE_GIVING: fn(usize) -> Giving = |_| Giving {
    may_give: true,
    bankable: true,
};

fn plan_of(reads: &[TerminalRead], cfg: &TerminalsConfig) -> Vec<Send> {
    plan_with(reads, FREE_GIVING, cfg)
}

fn plan_with(
    reads: &[TerminalRead],
    giving: impl Fn(usize) -> Giving,
    cfg: &TerminalsConfig,
) -> Vec<Send> {
    let mut out = Vec::new();
    plan(reads, giving, cfg, &mut PlanScratch::default(), &mut out);
    out
}

/// A room short of a mineral is filled up to the keep by the nearest giver, which gives only
/// what it holds above the keep; the fee is the engine's for the amount and the range.
#[test]
fn a_need_is_served_by_the_nearest_giver_up_to_the_keep() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    // E2N1 is 1 room from E1N1, E5N1 is 4 rooms away and holds more.
    let reads = [
        read("E1N1", &[], 1_000),
        read("E2N1", &[(R::Hydrogen, keep + 5_000)], 1_000),
        read("E5N1", &[(R::Hydrogen, keep + 50_000)], 1_000),
    ];
    let sends = plan_of(&reads, &cfg);
    assert_eq!(sends.len(), 1, "{sends:?}");
    let s = sends[0];
    assert_eq!(
        (s.from, s.to, s.kind),
        (room("E2N1"), room("E1N1"), R::Hydrogen)
    );
    assert_eq!(s.amount, keep);
    assert_eq!(s.range, 1);
    assert_eq!(s.fee, engine_fee(keep, 1));
    assert_eq!(s.wait, Wait::Ready);
    // A giver gives only its surplus: 2000 above the keep fills 2000 of the need.
    let reads = [
        read("E1N1", &[], 1_000),
        read("E2N1", &[(R::Hydrogen, keep + 2_000)], 1_000),
    ];
    let sends = plan_of(&reads, &cfg);
    assert_eq!(sends.iter().map(|s| s.amount).collect::<Vec<_>>(), [2_000]);
    // Below `send_min` of surplus, or of need, nothing moves.
    let reads = [
        read("E1N1", &[(R::Hydrogen, keep - cfg.send_min + 1)], 1_000),
        read("E2N1", &[(R::Hydrogen, 3 * keep)], 1_000),
    ];
    assert!(
        plan_of(&reads, &cfg).is_empty(),
        "the need is under send_min"
    );
    let reads = [
        read("E1N1", &[], 1_000),
        read("E2N1", &[(R::Hydrogen, keep + cfg.send_min - 1)], 1_000),
    ];
    assert!(
        plan_of(&reads, &cfg).is_empty(),
        "the surplus is under send_min"
    );
}

/// The largest need goes first, and a terminal sends once a plan: two rooms short of what one
/// giver holds get one send now, the larger need's.
#[test]
fn the_largest_need_first_and_one_send_a_giver() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    let reads = [
        read("E1N1", &[(R::Oxygen, keep - 1_500)], 1_000),
        read("E1N2", &[], 1_000),
        read(
            "E2N1",
            &[(R::Oxygen, 10 * keep), (R::Hydrogen, 10 * keep)],
            1_000,
        ),
    ];
    let sends = plan_of(&reads, &cfg);
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0].to, room("E1N2"), "the larger need");
    assert_eq!(sends[0].amount, keep);
}

/// A send is cut to the receiver's free room and to what `terminals.energy_keep` pays the fee
/// of, and dropped under `terminals.send_min`.
#[test]
fn the_amount_fits_the_free_room_and_the_fee_budget() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    let mut full = read("E1N1", &[], 1_000);
    full.free = cfg.send_min + 200;
    let giver = read("E2N1", &[(R::Utrium, 10 * keep)], 1_000);
    let sends = plan_of(&[full, giver], &cfg);
    assert_eq!(
        sends.iter().map(|s| s.amount).collect::<Vec<_>>(),
        [cfg.send_min + 200]
    );
    full.free = cfg.send_min - 1;
    assert!(plan_of(&[full, giver], &cfg).is_empty());
    // 9 rooms away, an energy keep of 300 pays the fee of fewer units than the need.
    let far = read("E10N1", &[(R::Utrium, 10 * keep)], 300);
    let near = read("E1N1", &[], 1_000);
    let small = TerminalsConfig {
        energy_keep: 300,
        ..cfg
    };
    let sends = plan_of(&[near, far], &small);
    let paid = affordable(300 - FEE_MARGIN, 9);
    assert!(paid < keep && paid >= small.send_min, "{paid}");
    assert_eq!(sends.iter().map(|s| s.amount).collect::<Vec<_>>(), [paid]);
    assert!(sends[0].fee + FEE_MARGIN <= small.energy_keep);
    let none = TerminalsConfig {
        energy_keep: 0,
        ..cfg
    };
    assert!(
        plan_of(&[near, far], &none).is_empty(),
        "no energy keep, no send"
    );
}

/// A giver short of its fee sends what its energy pays for when that is at least
/// `terminals.send_min`; else the send waits for its fee where the terminal can be filled, and
/// otherwise the next giver serves the need.
#[test]
fn a_giver_short_of_its_fee_sends_less_waits_or_is_passed_over() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    let need = read("E1N1", &[], 1_000);
    // 10 rooms: 1500 units cost more than 300 energy, 1000 cost less.
    let mut giver = read("E11N1", &[(R::Keanium, 10 * keep)], 0);
    giver.energy = engine_fee(cfg.send_min, 10) + FEE_MARGIN;
    let sends = plan_of(&[need, giver], &cfg);
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].wait, Wait::Ready);
    assert_eq!(
        sends[0].amount,
        affordable(giver.energy - FEE_MARGIN, 10).min(keep)
    );
    assert!(sends[0].fee + FEE_MARGIN <= giver.energy);
    // One unit short of the margin, the send waits for its fee.
    giver.energy -= 1;
    assert_eq!(plan_of(&[need, giver], &cfg)[0].wait, Wait::Fee);
    giver.energy = 0;
    let sends = plan_of(&[need, giver], &cfg);
    assert_eq!(sends.len(), 1);
    assert_eq!((sends[0].wait, sends[0].amount), (Wait::Fee, keep));
    // Not bankable: passed over, for a farther giver that holds its fee.
    let other = read("E1N11", &[(R::Keanium, 10 * keep)], 1_000);
    let not_giver = |i: usize| Giving {
        may_give: true,
        bankable: i != 1,
    };
    let sends = plan_with(&[need, giver, other], not_giver, &cfg);
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!((sends[0].from, sends[0].wait), (room("E1N11"), Wait::Ready));
    assert!(plan_with(&[need, giver], not_giver, &cfg).is_empty());
    // A full terminal cannot take the haulers' energy: passed over too.
    let mut full = giver;
    full.free = 0;
    assert!(plan_of(&[need, full], &cfg).is_empty());
}

/// A giver on its cooldown is planned to wait for it; a held giver and one beyond
/// `terminals.max_range` give nothing.
#[test]
fn cooldowns_holds_and_the_range_cap() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    let need = read("E1N1", &[], 1_000);
    let mut giver = read("E2N1", &[(R::Zynthium, 10 * keep)], 1_000);
    giver.cooldown = 4;
    let sends = plan_of(&[need, giver], &cfg);
    assert_eq!(
        sends.iter().map(|s| s.wait).collect::<Vec<_>>(),
        [Wait::Cooldown]
    );
    giver.cooldown = 0;
    let held = |i: usize| Giving {
        may_give: i != 1,
        bankable: true,
    };
    assert!(plan_with(&[need, giver], held, &cfg).is_empty());
    let beyond = read(
        &format!("E{}N1", 1 + cfg.max_range + 1),
        &[(R::Zynthium, 10 * keep)],
        100_000,
    );
    assert!(plan_of(&[need, beyond], &cfg).is_empty());
    let within = read(
        &format!("E{}N1", 1 + cfg.max_range),
        &[(R::Zynthium, 10 * keep)],
        100_000,
    );
    assert_eq!(plan_of(&[need, within], &cfg).len(), 1);
}

/// A small deterministic generator for the property tests (no ambient randomness).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

/// On random reads: one send a giver, never from a room to itself, a receiver never past the
/// keep, a giver never below it, the fee the engine's and within `terminals.energy_keep`, no
/// receiver past its free room, a ready send's fee held by its giver, nothing under
/// `terminals.send_min`; and the same plan for the reads in another order.
#[test]
fn plans_keep_their_invariants() {
    let cfg = TERMINALS_MMO;
    let keep = cfg.mineral_keep;
    let names = ["E1N1", "E2N1", "E4N3", "E1N6", "E9N1", "E13N2"];
    let mut rng = Lcg(7);
    for _ in 0..400 {
        let n = 2 + rng.below(names.len() as u32 - 1) as usize;
        let mut reads: Vec<TerminalRead> = names[..n]
            .iter()
            .map(|name| {
                let stock: Vec<(ResourceType, u32)> = BALANCED_RESOURCES
                    .iter()
                    .map(|&k| (k, rng.below(4 * keep)))
                    .collect();
                let mut r = read(name, &stock, rng.below(2_000));
                r.cooldown = if rng.below(4) == 0 { rng.below(10) } else { 0 };
                r.free = r.free.min(rng.below(3 * keep) + 500);
                r
            })
            .collect();
        let sends = plan_of(&reads, &cfg);
        let mut froms: Vec<RoomName> = sends.iter().map(|s| s.from).collect();
        froms.sort_unstable_by_key(|r| r.packed_repr());
        froms.dedup();
        assert_eq!(froms.len(), sends.len(), "one send a giver");
        for s in &sends {
            let g = reads.iter().find(|r| r.room == s.from).expect("giver");
            let to = reads.iter().find(|r| r.room == s.to).expect("receiver");
            let i = index(s.kind);
            assert_ne!(s.from, s.to);
            assert!(s.amount >= cfg.send_min);
            assert!(to.stock[i] + s.amount <= keep, "{s:?}");
            assert!(g.stock[i] >= keep + s.amount, "{s:?}");
            assert_eq!(s.range, engine_range(s.from, s.to));
            assert!(s.range <= cfg.max_range);
            assert_eq!(s.fee, engine_fee(s.amount, s.range));
            assert!(s.fee + FEE_MARGIN <= cfg.energy_keep);
            match s.wait {
                Wait::Ready => assert!(g.cooldown == 0 && s.fee + FEE_MARGIN <= g.energy, "{s:?}"),
                Wait::Fee => assert!(g.cooldown == 0 && g.free > 0, "{s:?}"),
                Wait::Cooldown => assert!(g.cooldown > 0, "{s:?}"),
                Wait::Busy => panic!("the plan never sets Busy: {s:?}"),
            }
        }
        for to in &reads {
            let arriving: u32 = sends
                .iter()
                .filter(|s| s.to == to.room)
                .map(|s| s.amount)
                .sum();
            assert!(
                arriving <= to.free,
                "{}: {arriving} into {}",
                to.room,
                to.free
            );
        }
        reads.reverse();
        assert_eq!(
            plan_of(&reads, &cfg),
            sends,
            "the same plan in another order"
        );
    }
}

// ---- A world of terminals ----

/// One owned room with a terminal, as the engine holds it.
#[derive(Clone)]
struct Term {
    name: RoomName,
    level: u8,
    stock: BalancedStock,
    /// Everything else it holds (compounds, bars): it takes free room.
    other: u32,
    energy: u32,
    /// The engine's `cooldownTime` (V441).
    cooldown_time: u32,
    hauled: bool,
}

impl Term {
    fn new(name: &str, stock: &[(ResourceType, u32)], energy: u32) -> Self {
        let mut s: BalancedStock = [0; BALANCED_RESOURCES.len()];
        for &(k, n) in stock {
            s[index(k)] = n;
        }
        Self {
            name: room(name),
            level: terminal_level(),
            stock: s,
            other: 0,
            energy,
            cooldown_time: 0,
            hauled: true,
        }
    }

    fn free(&self) -> u32 {
        TERMINAL_CAPACITY - self.stock.iter().sum::<u32>() - self.other - self.energy
    }

    fn allowed(&self) -> bool {
        StructureType::Terminal.controller_structures(u32::from(self.level)) > 0
    }
}

struct Net {
    time: u32,
    terms: Vec<Term>,
    cfg: TerminalsConfig,
    snap: Snapshot,
    t: Terminals,
    full: Vec<RoomName>,
    energy: Vec<RoomName>,
    standard: bool,
    /// The energy the source haulers bring a bank a tick (a load).
    fill: u32,
    /// The game refuses every send from this room with this code.
    refuse: Option<(RoomName, ErrorCode)>,
    sent: Vec<(u32, Send)>,
    full_reads: Vec<u32>,
    energy_reads: Vec<u32>,
    outcomes: Vec<Outcome>,
    /// A creep of this room stands next to its terminal and withdraws from it every tick.
    withdrawer: Option<RoomName>,
    buffer: IntentBuffer,
    /// This tick's terminal handles, by room.
    handles: Vec<(RoomName, StructureHandle)>,
}

impl Net {
    fn new(terms: Vec<Term>) -> Self {
        Self {
            time: 100,
            terms,
            cfg: TERMINALS_MMO,
            snap: Snapshot::default(),
            t: Terminals::default(),
            full: Vec::new(),
            energy: Vec::new(),
            standard: true,
            fill: 200,
            refuse: None,
            sent: Vec::new(),
            full_reads: Vec::new(),
            energy_reads: Vec::new(),
            outcomes: Vec::new(),
            withdrawer: None,
            buffer: IntentBuffer::default(),
            handles: Vec::new(),
        }
    }

    fn term(&self, name: &str) -> &Term {
        self.terms
            .iter()
            .find(|t| t.name == room(name))
            .expect("term")
    }

    fn total(&self, k: ResourceType) -> u32 {
        self.terms.iter().map(|t| t.stock[index(k)]).sum()
    }

    fn take_snapshot(&mut self) {
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        if !self.full.is_empty() {
            self.full_reads.push(self.time);
        }
        if !self.energy.is_empty() {
            self.energy_reads.push(self.time);
        }
        self.handles.clear();
        for (k, t) in self.terms.iter().enumerate() {
            self.snap
                .rooms
                .push(t.name, Controller::Mine { level: t.level });
            let at = pos(t.name, 25, 25);
            let h = self
                .snap
                .structures
                .push(ObjectKey(k as u128 + 1), || {
                    Ok(StructureStatic {
                        kind: StructureType::Terminal,
                        pos: at,
                    })
                })
                .expect("terminal");
            let row = u32::try_from(self.snap.structures.len() - 1).expect("small");
            self.handles.push((t.name, h));
            if self.withdrawer == Some(t.name) {
                let r = CreepRow {
                    pos: pos(t.name, 26, 25),
                    my: true,
                    spawning: false,
                    fatigue: 0,
                };
                let id = Identity {
                    role: Role::LabHauler,
                    home: t.name,
                    remote: None,
                    serial: 1,
                };
                self.snap
                    .creeps
                    .push_named(ObjectKey(500), r, || Some(id))
                    .expect("creep");
            }
            if self.full.contains(&t.name) {
                self.snap.terminals.push(TerminalRead {
                    room: t.name,
                    handle: h,
                    row,
                    at,
                    cooldown: t.cooldown_time.saturating_sub(self.time),
                    free: t.free(),
                    energy: t.energy,
                    stock: t.stock,
                });
            } else if self.energy.contains(&t.name) {
                self.snap.terminals.set_energy(t.name, t.energy);
            }
        }
        self.snap.finish();
    }

    /// The engine's side of one order (V435 to V441).
    fn engine(&mut self, order: SendOrder) -> Outcome {
        let s = order.send;
        if let Some((r, code)) = self.refuse
            && r == s.from
        {
            return Outcome::Failed(code);
        }
        let now = self.time;
        let rg = engine_range(s.from, s.to);
        let Some(gi) = self.terms.iter().position(|t| t.name == s.from) else {
            return Outcome::Failed(ErrorCode::NotOwner);
        };
        let g = &self.terms[gi];
        let i = index(s.kind);
        if !g.allowed() {
            return Outcome::Failed(ErrorCode::RclNotEnough);
        }
        if g.stock[i] < s.amount || s.amount == 0 {
            return Outcome::Failed(ErrorCode::NotEnough);
        }
        if g.cooldown_time > now {
            return Outcome::Failed(ErrorCode::Tired);
        }
        if g.energy < engine_fee(s.amount, rg) {
            return Outcome::Failed(ErrorCode::NotEnough);
        }
        // The global pass: a destination with an own terminal at its level, the amount clipped.
        let Some(di) = self
            .terms
            .iter()
            .position(|t| t.name == s.to && t.allowed())
        else {
            return Outcome::Ok;
        };
        let amount = s.amount.min(self.terms[di].free());
        if amount == 0 {
            return Outcome::Ok;
        }
        let paid = engine_fee(amount, rg);
        let g = &mut self.terms[gi];
        g.stock[i] -= amount;
        g.energy -= paid;
        g.cooldown_time = now + TERMINAL_COOLDOWN;
        self.terms[di].stock[i] += amount;
        self.sent.push((
            now,
            Send {
                amount,
                fee: paid,
                ..s
            },
        ));
        Outcome::Ok
    }

    fn tick(&mut self) {
        self.take_snapshot();
        let cfg = self.cfg;
        self.t.begin(&self.snap, &cfg);
        let hauled: Vec<RoomName> = self
            .terms
            .iter()
            .filter(|t| t.hauled)
            .map(|t| t.name)
            .collect();
        self.t
            .set_hauled(|r| hauled.contains(&r), self.standard, &cfg);
        // The source haulers fill each bank by a load a tick (their transfers carry no amount).
        let banks = self.t.banks().to_vec();
        for (r, _, _, want) in banks {
            assert!(want > 0);
            let t = self.terms.iter_mut().find(|t| t.name == r).expect("banked");
            assert!(t.hauled, "only a hauled room is banked");
            t.energy += self.fill;
        }
        // The jobs before this one queue their creep intents: a withdraw from the withdrawer's
        // terminal (P9.4, the hot-path review).
        self.buffer.begin(&self.snap);
        let mut commit = Commit::default();
        if let Some(room) = self.withdrawer {
            let terminal = self
                .handles
                .iter()
                .find(|h| h.0 == room)
                .expect("terminal")
                .1;
            let actor = self.snap.creeps.handles()[0];
            let intent = CreepIntent::Withdraw {
                target: terminal,
                resource: R::Hydrogen,
                amount: None,
            };
            self.buffer
                .push(&self.snap, actor, intent, &mut commit)
                .expect("a withdraw next to the terminal");
        }
        let before = commit.cpu();
        if self.standard {
            self.t.run(&cfg, &self.buffer, &mut commit).expect("run");
            // Each order commits one intent's CPU (V34).
            let n = self.t.orders().len() as f64;
            let committed = commit.cpu() - before;
            assert!(
                (committed - n * rustedscreep_consts::INTENT_CPU_COST).abs() < 1e-9,
                "{committed}"
            );
        } else {
            assert!(self.t.orders().is_empty());
        }
        for i in 0..self.t.orders().len() {
            let order = self.t.orders()[i];
            let outcome = self.engine(order);
            self.outcomes.push(outcome);
            self.t.record_send(i, outcome, &cfg);
        }
        let (full, energy) = self.t.reads(&cfg, self.standard);
        self.full = full.to_vec();
        self.energy = energy.to_vec();
        self.time += 1;
    }

    fn run(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn all_ok(&self) -> bool {
        self.outcomes.iter().all(|o| *o == Outcome::Ok)
    }
}

/// Two rooms that each mine one mineral fill each other up to the keep, then stop sending; every
/// call is OK, nothing is lost, and the fees are the engine's.
#[test]
fn two_rooms_trade_their_minerals_up_to_the_keep() {
    let keep = TERMINALS_MMO.mineral_keep;
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Hydrogen, 20_000)], 1_000),
        Term::new("E3N1", &[(R::Oxygen, 20_000)], 1_000),
    ]);
    let (h, o) = (net.total(R::Hydrogen), net.total(R::Oxygen));
    net.run(400);
    assert!(net.all_ok(), "{:?}", net.outcomes);
    assert_eq!(net.term("E3N1").stock[index(R::Hydrogen)], keep);
    assert_eq!(net.term("E1N1").stock[index(R::Oxygen)], keep);
    assert_eq!(
        (net.total(R::Hydrogen), net.total(R::Oxygen)),
        (h, o),
        "nothing lost"
    );
    assert_eq!(net.sent.len(), 2, "{:?}", net.sent);
    for &(_, s) in &net.sent {
        assert_eq!(s.fee, engine_fee(s.amount, 2));
    }
    let spent: u32 = net.sent.iter().map(|(_, s)| s.fee).sum();
    let energy: u32 = net.terms.iter().map(|t| t.energy).sum();
    assert_eq!(energy, 2_000 - spent);
    // Once balanced, the plan runs only every `terminals.read_ticks`.
    let last = net.sent.last().expect("a send").0;
    let later: Vec<u32> = net
        .full_reads
        .iter()
        .copied()
        .filter(|&t| t > last + TERMINAL_COOLDOWN)
        .collect();
    assert!(
        later
            .windows(2)
            .all(|w| w[1] - w[0] == TERMINALS_MMO.read_ticks),
        "{later:?}"
    );
    let stats: u32 = net.t.rooms().iter().map(|r| r.stats.sends).sum();
    assert_eq!(stats, 2);
}

/// A giver with more to give than one send sends again once its cooldown ends, not before.
#[test]
fn a_giver_sends_again_after_its_cooldown() {
    let keep = TERMINALS_MMO.mineral_keep;
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Catalyst, 30_000)], 1_000),
        Term::new("E2N1", &[], 1_000),
        Term::new("E1N2", &[], 1_000),
    ]);
    net.run(100);
    assert!(net.all_ok(), "{:?}", net.outcomes);
    assert_eq!(net.sent.len(), 2);
    let (t0, t1) = (net.sent[0].0, net.sent[1].0);
    assert!(t1 >= t0 + TERMINAL_COOLDOWN, "{t0} then {t1}");
    assert!(t1 <= t0 + TERMINAL_COOLDOWN + 1, "{t0} then {t1}");
    for name in ["E2N1", "E1N2"] {
        assert_eq!(net.term(name).stock[index(R::Catalyst)], keep);
    }
}

/// With one terminal (or none at a terminal's level) nothing is read and nothing is sent, and a
/// terminal below its level neither gives nor receives.
#[test]
fn one_member_reads_nothing_and_a_low_level_terminal_is_no_member() {
    let mut net = Net::new(vec![Term::new("E1N1", &[(R::Hydrogen, 50_000)], 1_000)]);
    net.run(300);
    assert!(net.full_reads.is_empty() && net.energy_reads.is_empty());
    assert!(net.sent.is_empty());
    let mut low = Term::new("E2N1", &[], 1_000);
    low.level = terminal_level() - 1;
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Hydrogen, 50_000)], 1_000),
        low,
    ]);
    net.run(300);
    assert_eq!(net.t.rooms().len(), 1);
    assert!(net.full_reads.is_empty() && net.sent.is_empty());
}

/// A giver with no energy, in a room whose sources are hauled, waits for its fee: its terminal is
/// banked and read for its energy alone each tick, and the send goes out on the tick after it
/// holds the fee.
#[test]
fn a_send_waits_for_the_haulers_to_bring_its_fee() {
    let keep = TERMINALS_MMO.mineral_keep;
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Lemergium, 20_000)], 0),
        Term::new("E9N1", &[], 0),
    ]);
    net.fill = 50;
    net.run(200);
    assert!(net.all_ok(), "{:?}", net.outcomes);
    assert_eq!(net.sent.len(), 1, "{:?}", net.sent);
    let (at, s) = net.sent[0];
    assert_eq!((s.amount, s.range), (keep, 8));
    let first = *net.full_reads.first().expect("a read");
    // The fills that paid it: one load a tick from the plan's next tick.
    let loads = (s.fee + FEE_MARGIN).div_ceil(net.fill);
    assert!(at >= first + loads, "{first} + {loads} > {at}");
    assert!(at <= first + loads + 2, "{first} + {loads} + 2 < {at}");
    assert!(!net.energy_reads.is_empty());
    assert!(net.energy_reads.iter().all(|&t| t > first && t <= at));
    assert!(net.term("E1N1").energy <= TERMINALS_MMO.energy_keep + net.fill);
}

/// A giver with no fee in a room whose sources are not hauled is passed over for a farther one
/// with energy, and is never banked.
#[test]
fn an_unfilled_giver_without_its_fee_is_passed_over() {
    let mut near = Term::new("E2N1", &[(R::Keanium, 20_000)], 0);
    near.hauled = false;
    let mut net = Net::new(vec![
        Term::new("E1N1", &[], 1_000),
        near,
        Term::new("E1N5", &[(R::Keanium, 20_000)], 1_000),
    ]);
    net.run(100);
    assert!(net.all_ok());
    assert_eq!(
        net.sent.iter().map(|(_, s)| s.from).collect::<Vec<_>>(),
        [room("E1N5")]
    );
    assert!(net.energy_reads.is_empty());
}

/// A refused send holds its giver for `terminals.read_ticks` (one log a hold), and a giver that
/// is held is not used; the need waits.
#[test]
fn a_refused_send_holds_the_giver() {
    let mut net = Net::new(vec![
        Term::new("E1N1", &[], 1_000),
        Term::new("E2N1", &[(R::Utrium, 20_000)], 1_000),
    ]);
    net.refuse = Some((room("E2N1"), ErrorCode::Tired));
    let mut started = 0;
    for _ in 0..250 {
        net.take_snapshot();
        let cfg = net.cfg;
        net.t.begin(&net.snap, &cfg);
        net.t.set_hauled(|_| true, true, &cfg);
        net.buffer.begin(&net.snap);
        net.t
            .run(&cfg, &net.buffer, &mut Commit::default())
            .expect("run");
        for i in 0..net.t.orders().len() {
            let o = net.t.orders()[i];
            let out = net.engine(o);
            if net.t.record_send(i, out, &cfg) {
                started += 1;
            }
        }
        let (f, e) = net.t.reads(&cfg, true);
        net.full = f.to_vec();
        net.energy = e.to_vec();
        net.time += 1;
    }
    let refused = net.t.rooms().iter().map(|r| r.stats.refused).sum::<u32>();
    assert!((2..=3).contains(&refused), "{refused}");
    assert_eq!(
        started, refused,
        "each refusal after a hold ends starts a new one"
    );
    assert!(net.sent.is_empty());
}

/// Without the Standard tier the plan neither runs nor asks for reads; off, nothing is read,
/// banked or sent.
#[test]
fn no_reads_without_the_standard_tier_or_switched_off() {
    let terms = vec![
        Term::new("E1N1", &[(R::Hydrogen, 20_000)], 0),
        Term::new("E2N1", &[], 0),
    ];
    let mut net = Net::new(terms.clone());
    net.standard = false;
    net.run(300);
    assert!(net.full_reads.is_empty() && net.energy_reads.is_empty() && net.sent.is_empty());
    let mut net = Net::new(terms);
    net.cfg.enabled = false;
    net.run(300);
    assert!(net.full_reads.is_empty() && net.energy_reads.is_empty() && net.sent.is_empty());
    assert!(net.t.banks().is_empty());
}

/// A destination's free room clips a send in the engine (V438); the plan never asks for more
/// than the read's free room, so a full terminal receives nothing and costs no fee.
#[test]
fn a_full_terminal_is_not_sent_to() {
    let mut full = Term::new("E1N1", &[], 1_000);
    full.other = TERMINAL_CAPACITY - 1_000 - 500;
    let mut net = Net::new(vec![full, Term::new("E2N1", &[(R::Oxygen, 20_000)], 1_000)]);
    net.run(300);
    assert!(net.sent.is_empty());
    assert_eq!(net.terms.iter().map(|t| t.energy).sum::<u32>(), 2_000);
}

/// A send that waits for its fee closes its bank once its terminal is the only one left: no plan
/// can end the wait, so no hauler is sent to it (the hot-path review's high, 2026-11-26).
#[test]
fn a_lost_member_closes_the_bank() {
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Lemergium, 20_000)], 0),
        Term::new("E9N1", &[], 0),
    ]);
    net.fill = 0;
    net.run(5);
    assert!(!net.t.banks().is_empty(), "the fee wait opened the bank");
    net.terms.pop();
    net.run(3);
    assert!(net.t.banks().is_empty());
    assert!(net.t.rooms().iter().all(|r| r.waiting.is_none() && !r.bank));
    let reads = net.energy_reads.len();
    net.run(20);
    assert_eq!(
        net.energy_reads.len(),
        reads,
        "no energy read for a lone terminal"
    );
}

/// Without the Standard tier the waiting terminal's energy is not read, so its bank stays shut:
/// the haulers would fill it from a stale want (the hot-path review, 2026-11-26).
#[test]
fn no_bank_without_the_standard_tier() {
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Lemergium, 20_000)], 0),
        Term::new("E9N1", &[], 0),
    ]);
    net.fill = 0;
    net.run(5);
    assert!(!net.t.banks().is_empty());
    net.standard = false;
    net.run(5);
    assert!(net.t.banks().is_empty());
}

/// A ready send whose giver's terminal a creep withdraws from this tick waits for the next tick,
/// and goes out on the first plan after the withdrawals stop (the hot-path review, 2026-11-26).
#[test]
fn a_creeps_withdrawal_delays_the_send() {
    let mut net = Net::new(vec![
        Term::new("E1N1", &[(R::Hydrogen, 20_000)], 1_000),
        Term::new("E2N1", &[], 1_000),
    ]);
    net.withdrawer = Some(room("E1N1"));
    net.run(30);
    assert!(net.sent.is_empty(), "{:?}", net.sent);
    let giver = &net.t.rooms()[0];
    assert_eq!(giver.waiting.map(|w| w.wait), Some(Wait::Busy));
    let reads = net.full_reads.len();
    assert!(
        reads >= 25,
        "planned again every tick while it waits: {reads}"
    );
    net.withdrawer = None;
    net.run(5);
    assert!(net.all_ok());
    assert_eq!(net.sent.len(), 1, "{:?}", net.sent);
    assert_eq!(
        net.term("E2N1").stock[index(R::Hydrogen)],
        TERMINALS_MMO.mineral_keep
    );
}
