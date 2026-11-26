//! Tests of lab reactions (P9.2, ADR-045, `economy::labs`). Expected values derive from the
//! constants (`LAB_REACTION_AMOUNT`, `LAB_MINERAL_CAPACITY`, `LAB_REACTION_RANGE`,
//! `REACTION_TIME` through `ResourceType::reaction_time`, `REACTIONS` through
//! `ResourceType::reaction_components`, `CARRY_CAPACITY`, `BODYPART_COST`, `MOVE_POWER`,
//! `MOVE_COST_ROAD`, `MAX_CREEP_SIZE`, `CONTROLLER_STRUCTURES`), the planner's lab stamp, and the
//! labs config. The world below plays the engine's side as V403 to V417 read it, and any call
//! the engine would refuse is a model error.

use rustedscreep_consts::{
    CARRY_CAPACITY, LAB_MINERAL_CAPACITY, LAB_REACTION_AMOUNT, LAB_REACTION_RANGE, MAX_CREEP_SIZE,
    MOVE_COST_ROAD, MOVE_POWER, Part, Position, ROOM_AREA, ResourceType, RoomCoordinate, RoomName,
    SPAWN_ENERGY_CAPACITY, StructureType, TERMINAL_CAPACITY, extension_energy_capacity,
};

use super::labs::*;
use crate::config::{LABS_MMO, LabsConfig, MOVEMENT_MMO, SPAWN_MMO};
use crate::intents::{CreepIntent, IntentBuffer, Outcome, Target};
use crate::movement::{Footprint, Movement, ShoveCaps, Tile};
use crate::planner::bunker::LABS;
use crate::planner::rules::{allowed, earliest_level};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, CreepStore, LAB_RESOURCES, LabState, LabTerminal, ObjectKey, RoomEnergy,
    Snapshot, StructureHandle, StructureStatic, TickContext, lab_resource,
};
use crate::spawning::{Body, BodySpec, Identity, Role, Spawner};

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

fn tile(x: u8, y: u8) -> Tile {
    Tile::new(x, y).expect("tile")
}

const HOME: &str = "E2N2";
const SPAWN: (u8, u8) = (30, 30);
const TERMINAL: (u8, u8) = (26, 24);
/// Where the planner's lab stamp ([`LABS`]) is laid in the test room.
const STAMP: (u8, u8) = (20, 20);

/// The base minerals every target's chain starts from.
const BASES: [ResourceType; 7] = [
    R::Hydrogen,
    R::Oxygen,
    R::Utrium,
    R::Lemergium,
    R::Keanium,
    R::Zynthium,
    R::Catalyst,
];

/// The stamp's labs as (tile, whether it is a source lab), source labs first.
fn stamp() -> Vec<(Tile, bool)> {
    let mut out = Vec::new();
    for (dy, row) in LABS.iter().enumerate() {
        for (dx, c) in row.bytes().enumerate() {
            let t = tile(STAMP.0 + dx as u8, STAMP.1 + dy as u8);
            match c {
                b's' => out.push((t, true)),
                b'l' => out.push((t, false)),
                _ => {}
            }
        }
    }
    out.sort_by_key(|&(t, s)| (!s, t.index()));
    out
}

/// The first level that allows `n` labs (`CONTROLLER_STRUCTURES`).
fn labs_level(n: u32) -> u8 {
    earliest_level(StructureType::Lab, n - 1).expect("labs allowed")
}

fn capacity_at(level: u8) -> u32 {
    allowed(StructureType::Spawn, level) * SPAWN_ENERGY_CAPACITY
        + allowed(StructureType::Extension, level) * extension_energy_capacity(u32::from(level))
}

fn stock_with(items: &[(ResourceType, u32)]) -> Stock {
    let mut s = [0; LAB_RESOURCES.len()];
    for &(r, n) in items {
        s[lab_resource(r).expect("a lab resource")] = n;
    }
    s
}

fn bases(n: u32) -> Stock {
    stock_with(&BASES.map(|b| (b, n)))
}

// ---- The tables ----

/// `LAB_RESOURCES` is the 7 base minerals (no reaction) and the 34 products of `REACTIONS`
/// (V131: REACTION_TIME lists 34), each with a reaction time and both reagents in the list, with
/// no repeat; every target is a product whose chain bottoms out in the list.
#[test]
fn the_lab_resources_are_the_bases_and_every_product() {
    let products: Vec<_> = LAB_RESOURCES
        .iter()
        .filter(|r| r.reaction_components().is_some())
        .collect();
    assert_eq!(products.len(), 34, "REACTION_TIME's 34 products");
    for (i, r) in LAB_RESOURCES.iter().enumerate() {
        assert_eq!(lab_resource(*r), Some(i), "{r:?} once");
        match r.reaction_components() {
            None => assert!(BASES.contains(r), "{r:?} is a base mineral"),
            Some(parts) => {
                assert!(r.reaction_time().is_some(), "{r:?} has a time");
                for p in parts {
                    assert!(lab_resource(p).is_some(), "{r:?}'s {p:?} is listed");
                }
            }
        }
    }
    for t in TARGETS {
        let parts = t.reaction_components().expect("a product");
        assert!(parts.contains(&R::Catalyst), "{t:?} is catalyzed");
    }
    assert_eq!(lab_resource(R::Energy), None);
}

/// `product_of` reads `REACTIONS` backward in both orders, and nothing else reacts.
#[test]
fn product_of_inverts_the_reactions() {
    for &p in &LAB_RESOURCES {
        if let Some([a, b]) = p.reaction_components() {
            assert_eq!(product_of(a, b), Some(p));
            assert_eq!(product_of(b, a), Some(p));
        }
    }
    assert_eq!(product_of(R::Hydrogen, R::Hydrogen), None);
    assert_eq!(product_of(R::Catalyst, R::Utrium), None);
}

// ---- Picking a job ----

/// With every base mineral at a full load, the first target's chain is followed to its deepest
/// step: XGHO2 needs GHO2 (GO and OH), GO needs G and O, G needs UL and ZK, and UL is made from
/// two held bases, the whole load.
#[test]
fn a_job_starts_at_the_deepest_held_step_of_the_first_target() {
    let cfg = LABS_MMO;
    let step = pick(&bases(LAB_MINERAL_CAPACITY), &cfg).expect("a step");
    let want = R::UtriumLemergite;
    assert_eq!(step.product, want);
    assert_eq!(step.reagents, want.reaction_components().expect("UL"));
    assert_eq!(step.amount, LAB_MINERAL_CAPACITY.min(cfg.goal_xgho2));
    // With UL held, the next is ZK, then G, then GO, and so on up to XGHO2 itself.
    let mut stock = bases(LAB_MINERAL_CAPACITY);
    let mut made = Vec::new();
    for _ in 0..12 {
        let Some(s) = pick(&stock, &cfg) else { break };
        made.push(s.product);
        if s.product == TARGETS[0] {
            break;
        }
        for r in s.reagents {
            stock[lab_resource(r).expect("listed")] -= s.amount;
        }
        stock[lab_resource(s.product).expect("listed")] += s.amount;
        // Keep the bases topped up, as imports would.
        for b in BASES {
            stock[lab_resource(b).expect("listed")] = LAB_MINERAL_CAPACITY;
        }
    }
    assert_eq!(made.last(), Some(&TARGETS[0]), "{made:?}");
    for p in &made {
        assert!(chain(TARGETS[0]).contains(p), "{p:?} is in XGHO2's chain");
    }
}

/// Every product in `target`'s chain, `target` included.
fn chain(target: ResourceType) -> Vec<ResourceType> {
    let mut out = vec![target];
    let mut i = 0;
    while let Some(&p) = out.get(i) {
        if let Some(parts) = p.reaction_components() {
            for r in parts {
                if r.reaction_components().is_some() && !out.contains(&r) {
                    out.push(r);
                }
            }
        }
        i += 1;
    }
    out
}

/// A target at its goal, or less than `labs.batch_min` short of it, or with a goal of 0, is
/// passed; so is one whose chain needs a base mineral the terminal lacks (every target needs the
/// catalyst, X), and no intermediate is made for it.
#[test]
fn targets_that_are_stocked_off_or_short_of_a_base_are_passed() {
    let cfg = LABS_MMO;
    let mut stock = bases(LAB_MINERAL_CAPACITY);
    stock[lab_resource(TARGETS[0]).expect("listed")] = cfg.goal_xgho2;
    // XGHO2 stocked: the second target, XZH2O, from its deepest step (ZH from Z and H).
    let s = pick(&stock, &cfg).expect("a step");
    assert_eq!(s.product, R::ZynthiumHydride);
    // Short by less than the batch: still passed.
    stock[lab_resource(TARGETS[0]).expect("listed")] = cfg.goal_xgho2 - cfg.batch_min + 1;
    assert_eq!(
        pick(&stock, &cfg).map(|s| s.product),
        Some(R::ZynthiumHydride)
    );
    // Short by exactly the batch: worked again.
    stock[lab_resource(TARGETS[0]).expect("listed")] = cfg.goal_xgho2 - cfg.batch_min;
    assert_eq!(
        pick(&stock, &cfg).map(|s| s.product),
        Some(R::UtriumLemergite)
    );
    // A goal of 0 takes a target off.
    let off = LabsConfig {
        goal_xgho2: 0,
        ..cfg
    };
    assert_eq!(
        pick(&bases(LAB_MINERAL_CAPACITY), &off).map(|s| s.product),
        Some(R::ZynthiumHydride)
    );
    // No catalyst: every target is passed, and no intermediate is started.
    let mut no_x = bases(LAB_MINERAL_CAPACITY);
    no_x[lab_resource(R::Catalyst).expect("listed")] = 0;
    assert_eq!(pick(&no_x, &cfg), None);
    // Every goal 0: nothing.
    let none = LabsConfig {
        goal_xgho2: 0,
        goal_xzh2o: 0,
        goal_xzho2: 0,
        goal_xlho2: 0,
        goal_xkho2: 0,
        goal_xuh2o: 0,
        goal_xgh2o: 0,
        ..cfg
    };
    assert_eq!(pick(&bases(LAB_MINERAL_CAPACITY), &none), None);
    assert_eq!(pick(&[0; LAB_RESOURCES.len()], &cfg), None);
}

/// A held intermediate is used before more of it is made: with GHO2 and X held, XGHO2 is made
/// from them at once, at the least of the shortfall and both stocks.
#[test]
fn a_held_intermediate_is_used_first() {
    let cfg = LABS_MMO;
    let held = cfg.batch_min + 2 * LAB_REACTION_AMOUNT;
    let stock = stock_with(&[
        (R::GhodiumAlkalide, held),
        (R::Catalyst, LAB_MINERAL_CAPACITY),
    ]);
    let s = pick(&stock, &cfg).expect("a step");
    assert_eq!(s.product, TARGETS[0]);
    assert_eq!(s.amount, held);
}

/// Over many stocks the step's amount is a whole number of `LAB_REACTION_AMOUNT`s, at most
/// `LAB_MINERAL_CAPACITY` and the target's shortfall, at least `LAB_REACTION_AMOUNT`, and both
/// reagents hold it; the same stock always gives the same step.
#[test]
fn every_step_fits_its_reagents_and_one_lab() {
    let cfg = LABS_MMO;
    let mut seed = 0x9e37_79b9_u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..2000 {
        let mut stock = [0; LAB_RESOURCES.len()];
        for s in &mut stock {
            *s = match next() % 4 {
                0 => 0,
                1 => next() % 600,
                _ => next() % 4000,
            };
        }
        let Some(step) = pick(&stock, &cfg) else {
            continue;
        };
        assert_eq!(pick(&stock, &cfg), Some(step), "deterministic");
        assert_eq!(step.amount % LAB_REACTION_AMOUNT, 0, "{step:?}");
        assert!(
            (LAB_REACTION_AMOUNT..=LAB_MINERAL_CAPACITY).contains(&step.amount),
            "{step:?}"
        );
        assert_eq!(step.product.reaction_components(), Some(step.reagents));
        for r in step.reagents {
            assert!(
                stock[lab_resource(r).expect("listed")] >= step.amount,
                "{step:?}"
            );
        }
    }
}

// ---- Bodies and roles ----

/// The hauler's body: CARRY and MOVE only, a MOVE part for every `MOVE_POWER / MOVE_COST_ROAD`
/// CARRY parts (a loaded walk on roads at a tile a tick), within the energy, the part cap and
/// `labs.hauler_carry`, and never fewer CARRY parts for more energy.
#[test]
fn the_haulers_body_walks_roads_loaded_and_fits() {
    let per_move = MOVE_POWER / MOVE_COST_ROAD;
    let least = Part::Carry.cost() + Part::Move.cost();
    for max in [1, 2, 3, 10, 33, 49] {
        let mut last = 0;
        for energy in (0..=13_000).step_by(50) {
            let Some(b) = hauler_body(energy, max) else {
                assert!(energy < least, "no body at {energy} for {max}");
                continue;
            };
            let carry = b.count(Part::Carry) as u32;
            let moves = b.count(Part::Move) as u32;
            assert_eq!(carry + moves, b.len() as u32, "CARRY and MOVE only");
            assert!(carry >= 1 && carry <= max);
            assert!(
                moves * MOVE_POWER >= carry * MOVE_COST_ROAD,
                "{carry} {moves}"
            );
            assert_eq!(moves, carry.div_ceil(per_move));
            assert!(b.cost() <= energy);
            assert!(b.len() as u32 <= MAX_CREEP_SIZE);
            assert!(carry >= last, "more energy, never less CARRY");
            last = carry;
        }
    }
    let b = hauler_body(capacity_at(labs_level(3)), LABS_MMO.hauler_carry).expect("a body");
    assert_eq!(b.count(Part::Carry) as u32, LABS_MMO.hauler_carry);
}

/// With the whole stamp, the source pair is the stamp's two source labs and every other lab is
/// an output in range of both (V240); with only the first three labs the planner places (the
/// sources first, then the nearest), one output; with two, none.
#[test]
fn the_source_labs_are_the_stamps_middle_pair() {
    let s = stamp();
    let tiles: Vec<Tile> = s.iter().map(|&(t, _)| t).collect();
    let pair = source_pair(&tiles, None).expect("a pair");
    let mut want: Vec<usize> = s
        .iter()
        .enumerate()
        .filter(|(_, x)| x.1)
        .map(|(i, _)| i)
        .collect();
    want.sort_unstable();
    assert_eq!(pair.to_vec(), want);
    for (k, &t) in tiles.iter().enumerate() {
        if !pair.contains(&k) {
            for &i in &pair {
                assert!(u32::from(t.range(tiles[i])) <= LAB_REACTION_RANGE);
            }
        }
    }
    let three = &tiles[..3];
    let p = source_pair(three, None).expect("three labs react");
    let other = (0..3).find(|k| !p.contains(k)).expect("an output");
    for &i in &p {
        assert!(u32::from(three[other].range(three[i])) <= LAB_REACTION_RANGE);
    }
    assert_eq!(source_pair(&tiles[..2], None), None);
    // A tie keeps the current pair.
    let current = [three[p[1]], three[other]];
    let kept = source_pair(three, Some(current)).expect("a pair");
    let mut got = [three[kept[0]], three[kept[1]]];
    got.sort_by_key(|t| t.index());
    let mut cur = current;
    cur.sort_by_key(|t| t.index());
    assert_eq!(got, cur, "a tie keeps the current sources");
}

// ---- The world ----

#[derive(Clone, Copy, Debug)]
struct Lab {
    at: Tile,
    kind: Option<ResourceType>,
    amount: u32,
    cooldown_until: u32,
}

#[derive(Clone, Copy, Debug)]
struct Creep {
    key: u128,
    at: Position,
    carry: u32,
    other: u32,
    other_kind: Option<ResourceType>,
    spawning: bool,
}

struct World {
    time: u32,
    level: u8,
    labs: Vec<Lab>,
    terminal: bool,
    stock: Stock,
    creeps: Vec<Creep>,
    reads: Vec<RoomName>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    module: Labs,
    cfg: LabsConfig,
    standard: bool,
    spawn_creeps: bool,
    /// This tick's handles.
    terminal_handle: Option<StructureHandle>,
    lab_handles: Vec<StructureHandle>,
    lab_rows: Vec<u32>,
    /// Reactions applied: (tick, lab index).
    reactions: Vec<(u32, usize)>,
    /// Ticks the room was read.
    read_at: Vec<u32>,
    withdraws: u32,
}

impl World {
    /// The room at the first level that allows `n` labs, with the stamp's first `n` labs
    /// (sources first) and a terminal holding `stock`.
    fn new(n: usize, stock: Stock) -> Self {
        let labs = stamp()
            .into_iter()
            .take(n)
            .map(|(at, _)| Lab {
                at,
                kind: None,
                amount: 0,
                cooldown_until: 0,
            })
            .collect();
        let mut w = Self {
            time: 300_000,
            level: labs_level(u32::try_from(n.max(3)).expect("small")),
            labs,
            terminal: true,
            stock,
            creeps: Vec::new(),
            reads: Vec::new(),
            snap: Snapshot::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            module: Labs::default(),
            cfg: LABS_MMO,
            standard: true,
            spawn_creeps: true,
            terminal_handle: None,
            lab_handles: Vec::new(),
            lab_rows: Vec::new(),
            reactions: Vec::new(),
            read_at: Vec::new(),
            withdraws: 0,
        };
        // The first tick reads nothing; it asks for the room's read on the next.
        w.tick();
        w
    }

    fn home(&self) -> RoomName {
        room(HOME)
    }

    fn free(&self) -> u32 {
        TERMINAL_CAPACITY - self.stock.iter().sum::<u32>()
    }

    fn held(&self, r: ResourceType) -> u32 {
        self.stock[lab_resource(r).expect("listed")]
    }

    /// What the haulers carry of `r`.
    fn carried(&self, r: ResourceType) -> u32 {
        self.creeps
            .iter()
            .filter(|c| c.other_kind == Some(r))
            .map(|c| c.other)
            .sum()
    }

    fn take_snapshot(&mut self) {
        let home = self.home();
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for c in &self.creeps {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: c.spawning,
                fatigue: 0,
            };
            let id = Identity {
                role: Role::LabHauler,
                home,
                remote: None,
                serial: c.key as u64,
            };
            let row = self.snap.creeps.len();
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), r, || Some(id))
                .expect("creep");
            if !c.spawning {
                self.snap.creeps.set_store(
                    row,
                    CreepStore {
                        energy: 0,
                        capacity: c.carry * CARRY_CAPACITY,
                        other: c.other,
                        other_kind: c.other_kind.filter(|_| c.other > 0),
                    },
                );
            }
        }
        let worker = Identity {
            role: Role::Worker,
            home,
            remote: None,
            serial: 1,
        };
        let resident = CreepRow {
            pos: pos(home, 45, 45),
            my: true,
            spawning: false,
            fatigue: 0,
        };
        self.snap
            .creeps
            .push_named(ObjectKey(999), resident, || Some(worker))
            .expect("worker");
        let row = self
            .snap
            .rooms
            .push(home, Controller::Mine { level: self.level });
        let capacity = capacity_at(self.level);
        self.snap.rooms.set_energy(
            row,
            RoomEnergy {
                available: capacity,
                capacity,
            },
        );
        let read = self.reads.contains(&home);
        if read {
            self.snap.labs.push(home);
            self.read_at.push(self.time);
        }
        let spawn = self
            .snap
            .structures
            .push(ObjectKey(10), || {
                Ok(StructureStatic {
                    kind: StructureType::Spawn,
                    pos: pos(home, SPAWN.0, SPAWN.1),
                })
            })
            .expect("spawn");
        self.snap.spawns.push(spawn, 0, false);
        self.terminal_handle = None;
        if self.terminal {
            let h = self
                .snap
                .structures
                .push(ObjectKey(12), || {
                    Ok(StructureStatic {
                        kind: StructureType::Terminal,
                        pos: pos(home, TERMINAL.0, TERMINAL.1),
                    })
                })
                .expect("terminal");
            self.terminal_handle = Some(h);
            if read {
                self.snap.labs.set_terminal(LabTerminal {
                    handle: h,
                    at: pos(home, TERMINAL.0, TERMINAL.1),
                    free: self.free(),
                });
                for (i, &n) in self.stock.iter().enumerate() {
                    if n > 0 {
                        self.snap.labs.set_stock(home, LAB_RESOURCES[i], n);
                    }
                }
            }
        }
        self.lab_handles.clear();
        self.lab_rows.clear();
        for (i, l) in self.labs.iter().enumerate() {
            let at = pos(home, l.at.x(), l.at.y());
            let h = self
                .snap
                .structures
                .push(ObjectKey(100 + i as u128), || {
                    Ok(StructureStatic {
                        kind: StructureType::Lab,
                        pos: at,
                    })
                })
                .expect("lab");
            let row = u32::try_from(self.snap.structures.len() - 1).expect("small");
            self.lab_handles.push(h);
            self.lab_rows.push(row);
            if read {
                assert!(self.snap.labs.push_lab(LabState {
                    handle: h,
                    row,
                    at,
                    kind: l.kind.filter(|_| l.amount > 0),
                    amount: l.amount,
                    cooldown: l.cooldown_until.saturating_sub(self.time),
                }));
            }
        }
        self.snap.finish();
    }

    fn begin(&mut self) {
        self.take_snapshot();
        let mcfg = MOVEMENT_MMO;
        self.movement.begin(&self.snap, &mcfg);
        let mut rooms = Vec::new();
        self.movement.first_reads(&mcfg, &mut rooms);
        let mut again = Vec::new();
        self.movement.rescans(&mcfg, &mut again);
        rooms.extend(again);
        for &r in &rooms {
            assert!(self.movement.set_terrain(r, &[0u8; ROOM_AREA]));
            self.movement.set_statics(r, []);
            let mut blocked: Vec<(Tile, Footprint)> = [SPAWN, TERMINAL]
                .into_iter()
                .map(|(x, y)| (tile(x, y), Footprint::Blocked))
                .collect();
            blocked.extend(self.labs.iter().map(|l| (l.at, Footprint::Blocked)));
            self.movement.set_objects(r, blocked);
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.module.begin(&self.snap, &self.cfg);
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        for u in 0..self.module.units() {
            self.module
                .plan_room(u, &self.snap, &mut self.spawner, &self.cfg, self.standard)
                .expect("plan");
        }
        if self.standard {
            for u in 0..self.module.units() {
                self.module
                    .work_room(
                        u,
                        &self.snap,
                        &mut self.movement,
                        &mut self.buffer,
                        &self.cfg,
                        &MOVEMENT_MMO,
                        &mut commit,
                    )
                    .expect("work");
            }
        }
        let caps = ShoveCaps::from_cpu_left([100.0; 3]);
        self.movement.resolve(
            &self.snap,
            &mut self.buffer,
            &MOVEMENT_MMO,
            caps,
            &mut commit,
        );
        self.buffer.resolve();
        self.reads = self.module.reads(&self.cfg, self.standard).to_vec();
    }

    fn lab_of(&self, h: StructureHandle) -> Option<usize> {
        self.lab_handles.iter().position(|&x| x == h)
    }

    /// The engine's side of the tick (V403 to V417), then the model's spawn.
    fn apply(&mut self) {
        // The reactions are structure intents; the processor runs them in order (V405).
        let orders: Vec<_> = self.module.orders().to_vec();
        for o in orders {
            let li = self.lab_of(o.lab).expect("an output lab");
            let src: Vec<usize> = o
                .source_rows
                .iter()
                .map(|r| {
                    self.lab_rows
                        .iter()
                        .position(|x| x == r)
                        .expect("a source row")
                })
                .collect();
            let out = self.labs[li];
            assert!(
                out.cooldown_until <= self.time,
                "a reaction on cooldown (V403)"
            );
            for &s in &src {
                assert_ne!(s, li);
                assert!(
                    u32::from(self.labs[s].at.range(out.at)) <= LAB_REACTION_RANGE,
                    "V403 range"
                );
                assert!(
                    self.labs[s].amount >= LAB_REACTION_AMOUNT,
                    "too few reagents (V403)"
                );
            }
            let product = product_of(
                self.labs[src[0]].kind.expect("a reagent"),
                self.labs[src[1]].kind.expect("a reagent"),
            )
            .expect("the sources react");
            assert_eq!(product, o.product);
            assert!(
                out.kind.is_none_or(|k| k == product),
                "another mineral (V403)"
            );
            assert!(
                out.amount + LAB_REACTION_AMOUNT <= LAB_MINERAL_CAPACITY,
                "full (V403)"
            );
            let out = &mut self.labs[li];
            out.kind = Some(product);
            out.amount += LAB_REACTION_AMOUNT;
            out.cooldown_until = self.time + product.reaction_time().expect("a time");
            for &s in &src {
                let l = &mut self.labs[s];
                l.amount -= LAB_REACTION_AMOUNT;
                if l.amount == 0 {
                    l.kind = None;
                }
            }
            self.reactions.push((self.time, li));
        }
        let planned: Vec<_> = self.buffer.planned().copied().collect();
        for issue in planned {
            let i = issue.actor as usize;
            if i >= self.creeps.len() {
                continue;
            }
            match issue.intent {
                CreepIntent::Move { direction } => {
                    let c = &mut self.creeps[i];
                    let t = Tile::of(c.at).step(direction).expect("in the room");
                    c.at = pos(c.at.room_name(), t.x(), t.y());
                }
                CreepIntent::Withdraw {
                    target,
                    resource,
                    amount,
                } => {
                    let c = self.creeps[i];
                    let room_left = c.carry * CARRY_CAPACITY - c.other;
                    assert!(
                        c.other == 0 || c.other_kind == Some(resource),
                        "a second type in the hauler"
                    );
                    let n = if Some(target) == self.terminal_handle {
                        let held = self.held(resource);
                        let n = amount.unwrap_or(room_left);
                        assert!(n <= held, "more than the terminal holds (V400)");
                        assert!(n <= room_left, "more than the hauler holds (V400)");
                        self.stock[lab_resource(resource).expect("listed")] -= n;
                        n
                    } else {
                        let li = self.lab_of(target).expect("a lab");
                        let l = &mut self.labs[li];
                        assert_eq!(l.kind, Some(resource), "a withdraw of another type");
                        assert_eq!(amount, None);
                        let n = room_left.min(l.amount);
                        l.amount -= n;
                        if l.amount == 0 {
                            l.kind = None;
                        }
                        n
                    };
                    self.withdraws += 1;
                    let c = &mut self.creeps[i];
                    c.other += n;
                    c.other_kind = Some(resource);
                }
                CreepIntent::Transfer {
                    target: Target::Structure(target),
                    resource,
                    amount: None,
                } => {
                    let c = self.creeps[i];
                    assert_eq!(c.other_kind, Some(resource));
                    let n = if Some(target) == self.terminal_handle {
                        let n = c.other.min(self.free());
                        self.stock[lab_resource(resource).expect("listed")] += n;
                        n
                    } else {
                        let li = self.lab_of(target).expect("a lab");
                        let l = &mut self.labs[li];
                        assert!(
                            l.kind.is_none_or(|k| k == resource),
                            "a lab refuses a second mineral (V411)"
                        );
                        let n = c.other.min(LAB_MINERAL_CAPACITY - l.amount);
                        l.kind = Some(resource);
                        l.amount += n;
                        n
                    };
                    let c = &mut self.creeps[i];
                    c.other -= n;
                    if c.other == 0 {
                        c.other_kind = None;
                    }
                }
                other => panic!("an intent the labs do not make: {other:?}"),
            }
        }
        if self.spawn_creeps {
            for (role, body, _) in self.requests() {
                assert_eq!(role, Role::LabHauler);
                let key = 1000 + self.creeps.len() as u128;
                self.creeps.push(Creep {
                    key,
                    at: pos(self.home(), SPAWN.0 + 1, SPAWN.1),
                    carry: body.count(Part::Carry) as u32,
                    other: 0,
                    other_kind: None,
                    spawning: false,
                });
            }
        }
    }

    fn tick(&mut self) {
        self.begin();
        self.work();
        self.apply();
        self.time += 1;
    }

    fn requests(&self) -> Vec<(Role, Body, u8)> {
        (0..self.spawner.stats().requests)
            .filter_map(|i| self.spawner.request_at(i))
            .map(|r| {
                let BodySpec::Exact(b) = r.body else {
                    panic!("the lab hauler's body is exact");
                };
                (r.role, b, r.priority)
            })
            .collect()
    }

    fn room(&self) -> &LabRoom {
        self.module.rooms().first().expect("the room")
    }

    /// Ticks until `done` holds, at most `limit`; panics past it.
    fn run_until(&mut self, limit: u32, done: impl Fn(&Self) -> bool) {
        for _ in 0..limit {
            if done(self) {
                return;
            }
            self.tick();
        }
        panic!("not done in {limit} ticks: {:?}", self.room().why);
    }
}

/// A room with three labs and every base mineral at a load runs the first job (UL from U and L)
/// to its end: one lab hauler (priority, body), the sources loaded with the job's amount, the
/// output reacting once every `REACTION_TIME` ticks of UL with no refused call, the labs emptied,
/// and the terminal gaining exactly what the reactions made, each reagent losing as much. The
/// next job (ZK) then starts.
#[test]
fn a_room_runs_a_job_to_its_end() {
    let load = 2 * LABS_MMO.batch_min;
    let start = bases(load);
    let mut w = World::new(3, start);
    let step = pick(&start, &w.cfg).expect("a step");
    assert_eq!(step.product, R::UtriumLemergite);
    w.run_until(5, |w| w.room().job.is_some());
    assert_eq!(w.room().why, Why::Reacting);
    w.run_until(3000, |w| {
        w.room().stats.jobs == 2 && w.carried(R::UtriumLemergite) == 0
    });
    let room = w.room();
    assert_eq!(room.stats.requests, 1, "one hauler");
    let made = w.held(R::UtriumLemergite);
    assert_eq!(made, step.amount, "the whole job reached the terminal");
    assert_eq!(made, (w.reactions.len() as u32) * LAB_REACTION_AMOUNT);
    for r in step.reagents {
        assert_eq!(w.held(r), load - step.amount);
    }
    // One output: its reactions are REACTION_TIME apart.
    let period = R::UtriumLemergite.reaction_time().expect("a time");
    for pair in w.reactions.windows(2) {
        if pair[0].1 == pair[1].1 {
            assert!(pair[1].0 - pair[0].0 >= period, "{pair:?}");
        }
    }
    let first_out = w.reactions.first().expect("reactions").0;
    let gaps: Vec<u32> = w.reactions.windows(2).map(|p| p[1].0 - p[0].0).collect();
    assert!(gaps.iter().all(|&g| g >= period));
    assert!(
        gaps.iter().filter(|&&g| g == period).count() * 10 >= gaps.len() * 9,
        "the output reacts on the tick its cooldown ends: {first_out} {gaps:?}"
    );
    assert_eq!(room.job.map(|j| j.product), Some(R::ZynthiumKeanite));
}

/// The hauler is requested only with work, at `PRIORITY_LAB_HAULER` with the room's body, only
/// while the Standard tier runs, and never twice.
#[test]
fn the_hauler_is_asked_for_only_with_work() {
    let mut idle = World::new(3, [0; LAB_RESOURCES.len()]);
    for _ in 0..20 {
        idle.tick();
    }
    assert_eq!(idle.room().stats.requests, 0);
    assert_eq!(idle.room().why, Why::Idle);
    let mut w = World::new(3, bases(LAB_MINERAL_CAPACITY));
    w.spawn_creeps = false;
    w.standard = false;
    for _ in 0..5 {
        w.tick();
    }
    assert!(
        w.requests().is_empty(),
        "no request without the Standard tier"
    );
    w.standard = true;
    w.tick();
    let req = w.requests();
    assert_eq!(req.len(), 1);
    let (role, body, priority) = req[0];
    assert_eq!(role, Role::LabHauler);
    assert_eq!(priority, PRIORITY_LAB_HAULER);
    assert_eq!(
        Some(body),
        hauler_body(capacity_at(w.level), w.cfg.hauler_carry)
    );
    // The upgraders' budget counts its body while the labs have work.
    let upkeep = w
        .module
        .upkeeps()
        .iter()
        .find(|u| u.0 == w.home())
        .map(|u| u.1);
    assert_eq!(upkeep, Some(u64::from(body.cost())));
    let upkeep = idle
        .module
        .upkeeps()
        .iter()
        .find(|u| u.0 == idle.home())
        .map(|u| u.1);
    assert_eq!(upkeep, Some(0));
}

/// A lab holding a mineral no job uses is emptied before a job starts, and an output holding
/// another product is emptied while the job runs.
#[test]
fn a_lab_holding_another_mineral_is_emptied_first() {
    let mut w = World::new(3, bases(LAB_MINERAL_CAPACITY));
    // Before the first read: the output holds some hydrogen hydroxide (OH).
    w.labs[2].kind = Some(R::Hydroxide);
    w.labs[2].amount = 100;
    w.run_until(400, |w| {
        w.labs[2].kind.is_none() && w.carried(R::Hydroxide) == 0
    });
    assert_eq!(w.held(R::Hydroxide), 100, "back in the terminal");
    w.run_until(400, |w| w.room().job.is_some());
    assert_eq!(w.room().job.map(|j| j.product), Some(R::UtriumLemergite));
}

/// After a reset, a job the source labs already hold is adopted with nothing more loaded: the
/// output reacts them, no reagent is taken from the terminal, and then the labs are emptied.
#[test]
fn a_loaded_job_is_adopted_after_a_reset() {
    let mut w = World::new(3, [0; LAB_RESOURCES.len()]);
    let [a, b] = R::UtriumLemergite.reaction_components().expect("UL");
    let n = 50;
    w.labs[0].kind = Some(a);
    w.labs[0].amount = n;
    w.labs[1].kind = Some(b);
    w.labs[1].amount = n;
    w.module = Labs::default();
    w.reads = vec![w.home()];
    w.tick();
    let job = w.room().job.expect("adopted");
    assert_eq!(job.product, R::UtriumLemergite);
    assert_eq!(job.loaded, [n, n]);
    w.run_until(1000, |w| {
        w.room().job.is_none() && w.room().stats.reads > 3 && w.carried(R::UtriumLemergite) == 0
    });
    assert_eq!(
        w.held(R::UtriumLemergite),
        n,
        "every unit reacted and came back"
    );
    assert_eq!(
        w.held(a) + w.held(b),
        0,
        "nothing was loaded from the terminal"
    );
}

/// A room is read every tick while its hauler works, on the tick an output's reaction is due,
/// and otherwise every `labs.read_ticks`; a room with nothing to do keeps the cadence.
#[test]
fn reads_follow_the_hauler_and_the_reactions() {
    let mut idle = World::new(3, [0; LAB_RESOURCES.len()]);
    idle.read_at.clear();
    let n = 3 * idle.cfg.read_ticks;
    for _ in 0..n {
        idle.tick();
    }
    assert_eq!(idle.read_at.len() as u32, 3, "{:?}", idle.read_at);
    // A job loaded (adopted) with no hauler: reads come with the reactions only.
    let mut w = World::new(3, [0; LAB_RESOURCES.len()]);
    let [a, b] = R::Ghodium.reaction_components().expect("G");
    w.labs[0].kind = Some(a);
    w.labs[0].amount = 10 * LAB_REACTION_AMOUNT;
    w.labs[1].kind = Some(b);
    w.labs[1].amount = 10 * LAB_REACTION_AMOUNT;
    w.module = Labs::default();
    w.spawn_creeps = false;
    w.reads = vec![w.home()];
    w.read_at.clear();
    w.tick();
    let period = R::Ghodium.reaction_time().expect("a time");
    for _ in 0..(10 * period) {
        w.tick();
    }
    assert_eq!(w.reactions.len(), 10, "every reaction ran");
    assert!(
        w.read_at.len() <= 10 + 2,
        "one read a reaction: {:?}",
        w.read_at
    );
    for (t, _) in &w.reactions {
        assert!(w.read_at.contains(t), "the reaction's tick was read");
    }
}

/// A refused reaction holds the room's reactions for `labs.read_ticks`.
#[test]
fn a_refused_reaction_holds_the_room() {
    let mut w = World::new(3, [0; LAB_RESOURCES.len()]);
    let [a, b] = R::Ghodium.reaction_components().expect("G");
    w.labs[0].kind = Some(a);
    w.labs[0].amount = 100;
    w.labs[1].kind = Some(b);
    w.labs[1].amount = 100;
    w.module = Labs::default();
    w.spawn_creeps = false;
    w.reads = vec![w.home()];
    w.begin();
    w.work();
    assert_eq!(w.module.orders().len(), 1);
    let held = w.module.record_reaction(0, Outcome::Unknown, &w.cfg);
    assert!(held);
    assert!(!w.module.record_reaction(0, Outcome::Ok, &w.cfg));
    w.time += 1;
    let until = w.room().held_until;
    assert_eq!(until, w.time - 1 + w.cfg.read_ticks);
    let mut ordered = 0;
    while w.time < until {
        w.begin();
        w.work();
        ordered += w.module.orders().len();
        w.time += 1;
    }
    assert_eq!(ordered, 0, "no reaction while held");
    w.begin();
    w.work();
    assert_eq!(w.module.orders().len(), 1, "it reacts once the hold ends");
}

/// Off, no job starts, no hauler is asked for and no reaction runs; a hauler still carrying a
/// load takes it to the terminal.
#[test]
fn off_only_delivers_what_the_hauler_holds() {
    let mut w = World::new(3, bases(LAB_MINERAL_CAPACITY));
    w.cfg.enabled = false;
    w.creeps.push(Creep {
        key: 2000,
        at: pos(w.home(), TERMINAL.0 - 1, TERMINAL.1),
        carry: 10,
        other: 200,
        other_kind: Some(R::Utrium),
        spawning: false,
    });
    let before = w.held(R::Utrium);
    for _ in 0..20 {
        w.tick();
    }
    assert_eq!(w.room().why, Why::Off);
    assert_eq!(w.room().stats.requests, 0);
    assert!(w.reactions.is_empty());
    assert_eq!(w.held(R::Utrium), before + 200);
    assert_eq!(w.withdraws, 0);
}

/// A room wants labs once an own lab stands, or once its terminal holds what a target's next step
/// needs; never with `labs.enabled` off, and not for a stock every target passes (no catalyst).
#[test]
fn labs_are_wanted_once_reagents_are_on_hand() {
    let mut none = World::new(0, [0; LAB_RESOURCES.len()]);
    none.tick();
    assert!(!none.room().wants);
    assert_eq!(none.room().why, Why::NoLabs);
    assert!(none.module.wants_labs().is_empty());
    let mut no_x = bases(LAB_MINERAL_CAPACITY);
    no_x[lab_resource(R::Catalyst).expect("listed")] = 0;
    let mut short = World::new(0, no_x);
    short.tick();
    assert!(!short.room().wants, "no target can be made");
    let mut ready = World::new(0, bases(LAB_MINERAL_CAPACITY));
    ready.tick();
    assert!(ready.room().wants);
    let home = ready.home();
    assert_eq!(ready.module.wants_labs(), &[home]);
    assert_eq!(ready.room().stats.requests, 0, "no hauler without labs");
    let mut built = World::new(3, [0; LAB_RESOURCES.len()]);
    built.tick();
    assert!(built.room().wants, "a lab stands");
    let mut off = World::new(0, bases(LAB_MINERAL_CAPACITY));
    off.cfg.enabled = false;
    off.reads = vec![off.home()];
    off.tick();
    assert!(!off.room().wants);
}

/// With all 10 labs, the job's outputs react together: each of the 8 outputs once per
/// `REACTION_TIME`, while the sources last, and the sources are never overdrawn (the model
/// asserts V405's checks on every order).
#[test]
fn every_output_reacts_with_the_whole_stamp() {
    let start = bases(LAB_MINERAL_CAPACITY);
    let mut w = World::new(10, start);
    assert_eq!(allowed(StructureType::Lab, w.level), 10);
    w.run_until(3000, |w| w.room().stats.jobs == 2);
    let outputs: std::collections::BTreeSet<usize> = w.reactions.iter().map(|r| r.1).collect();
    assert_eq!(outputs.len(), 8, "every output reacted");
    let step = pick(&start, &w.cfg).expect("a step");
    assert_eq!(w.held(step.product), step.amount);
}

/// New labs while a job runs (a level-up) leave its source labs as they are, the new labs in
/// range of both joining as outputs; once it ends the roles are chosen again, and with the whole
/// stamp the sources are its middle pair, every other lab an output.
#[test]
fn a_job_keeps_its_sources_through_a_level_up() {
    let mut w = World::new(3, bases(2 * LABS_MMO.batch_min));
    w.run_until(20, |w| w.room().job.is_some());
    let before = w
        .room()
        .source_lab(0)
        .zip(w.room().source_lab(1))
        .map(|(a, b)| [a.at, b.at]);
    assert!(before.is_some());
    w.level = labs_level(10);
    for (at, _) in stamp().into_iter().skip(3) {
        w.labs.push(Lab {
            at,
            kind: None,
            amount: 0,
            cooldown_until: 0,
        });
    }
    w.run_until(50, |w| w.room().labs.len() == 10);
    while w.room().job.is_some() {
        let now = w
            .room()
            .source_lab(0)
            .zip(w.room().source_lab(1))
            .map(|(a, b)| [a.at, b.at]);
        assert_eq!(now, before, "the job keeps its sources");
        w.tick();
        assert!(w.time < 400_000, "the job ends");
    }
    w.run_until(500, |w| w.room().stats.jobs == 2);
    let mut want: Vec<Position> = stamp()
        .into_iter()
        .filter(|s| s.1)
        .map(|(t, _)| pos(w.home(), t.x(), t.y()))
        .collect();
    want.sort_by_key(|p| Tile::of(*p).index());
    let room = w.room();
    let mut got: Vec<Position> = [0, 1]
        .iter()
        .filter_map(|&i| room.source_lab(i))
        .map(|l| l.at)
        .collect();
    got.sort_by_key(|p| Tile::of(*p).index());
    assert_eq!(got, want, "the stamp's middle pair once the job ended");
    assert_eq!(room.outputs.len(), 8);
}

/// A reagent whose whole stock one trip takes (the terminal then holds none of it while the
/// hauler carries it) still reaches its source lab: the job is not emptied before the reactions,
/// and every unit the terminal held is made into the product.
#[test]
fn a_job_whose_reagents_fit_one_trip_is_not_cut_short() {
    let load = LABS_MMO.batch_min;
    assert!(
        load <= LABS_MMO.hauler_carry * CARRY_CAPACITY,
        "one trip takes it all"
    );
    let mut w = World::new(3, bases(load));
    let step = pick(&bases(load), &w.cfg).expect("a step");
    assert_eq!(step.amount, load);
    w.run_until(2000, |w| {
        w.room().stats.jobs == 2 && w.carried(step.product) == 0
    });
    assert_eq!(w.held(step.product), load);
    assert_eq!(w.reactions.len() as u32 * LAB_REACTION_AMOUNT, load);
}

/// Without the Standard tier no reaction can be ordered, so a running job's due reaction asks
/// for no read: the room keeps the `labs.read_ticks` cadence (the hot-path review, 2026-11-26:
/// the stale deadline read it every tick). Once the tier is back, the reaction is ordered.
#[test]
fn a_due_reaction_asks_for_no_read_without_the_standard_tier() {
    let mut w = World::new(3, [0; LAB_RESOURCES.len()]);
    let [a, b] = R::Ghodium.reaction_components().expect("G");
    w.labs[0].kind = Some(a);
    w.labs[0].amount = 100;
    w.labs[1].kind = Some(b);
    w.labs[1].amount = 100;
    w.module = Labs::default();
    w.spawn_creeps = false;
    w.reads = vec![w.home()];
    w.tick();
    assert!(w.room().job.is_some(), "adopted");
    w.standard = false;
    w.read_at.clear();
    let n = 3 * w.cfg.read_ticks;
    for _ in 0..n {
        w.tick();
    }
    assert!(
        w.read_at.len() as u32 <= n / w.cfg.read_ticks + 1,
        "{} reads in {n} ticks",
        w.read_at.len()
    );
    let before = w.reactions.len();
    w.standard = true;
    for _ in 0..(w.cfg.read_ticks + 2) {
        w.tick();
    }
    assert!(
        w.reactions.len() > before,
        "it reacts once the tier is back"
    );
}

/// A hauler holding a load the full terminal cannot take is no work: no read every tick, no
/// upkeep for the upgraders; once the terminal has room, it delivers.
#[test]
fn a_load_with_nowhere_to_go_keeps_no_read_going() {
    let mut full = [0; LAB_RESOURCES.len()];
    full[lab_resource(R::Hydrogen).expect("listed")] = TERMINAL_CAPACITY;
    let mut w = World::new(3, full);
    w.spawn_creeps = false;
    w.creeps.push(Creep {
        key: 2000,
        at: pos(w.home(), TERMINAL.0 - 1, TERMINAL.1),
        carry: 10,
        other: 200,
        other_kind: Some(R::Oxygen),
        spawning: false,
    });
    w.read_at.clear();
    let n = 2 * w.cfg.read_ticks;
    for _ in 0..n {
        w.tick();
    }
    assert!(
        w.read_at.len() as u32 <= n / w.cfg.read_ticks + 2,
        "{:?}",
        w.read_at
    );
    let upkeep = w
        .module
        .upkeeps()
        .iter()
        .find(|u| u.0 == w.home())
        .map(|u| u.1);
    assert_eq!(upkeep, Some(0));
    assert_eq!(w.held(R::Oxygen), 0);
    w.stock[lab_resource(R::Hydrogen).expect("listed")] -= 1000;
    w.run_until(3 * w.cfg.read_ticks, |w| w.held(R::Oxygen) == 200);
}
