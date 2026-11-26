//! Tests of the factory (P9.3, ADR-046, `economy::factory`). Expected values derive from the
//! constants (`COMMODITIES` through `ResourceType::commodity_recipe`, `CREEP_LIFE_TIME`,
//! `CARRY_CAPACITY`, `FACTORY_CAPACITY`, `TERMINAL_CAPACITY`, `CONTROLLER_STRUCTURES`) and the
//! factory config. The world below plays the engine's side as V420 to V427 read it, and any call
//! the engine would refuse is a model error.

use rustedscreep_consts::{
    CARRY_CAPACITY, CREEP_LIFE_TIME, ErrorCode, FACTORY_CAPACITY, Part, Position, ROOM_AREA,
    ResourceType, RoomCoordinate, RoomName, SPAWN_ENERGY_CAPACITY, StructureType,
    TERMINAL_CAPACITY, extension_energy_capacity,
};

use super::factory::Why as FactoryWhy;
use super::factory::*;
use crate::config::{FACTORY_MMO, FactoryConfig, MOVEMENT_MMO, SPAWN_MMO};
use crate::intents::{CreepIntent, IntentBuffer, Outcome, Target};
use crate::movement::{Footprint, Movement, ShoveCaps, Tile};
use crate::planner::rules::{allowed, earliest_level};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, CreepStore, FACTORY_MINERALS, FACTORY_RESOURCES, FactoryState,
    FactoryStock, FactoryTerminal, ObjectKey, RoomEnergy, Snapshot, StructureHandle,
    StructureStatic, TickContext, factory_resource,
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
const FACTORY: (u8, u8) = (28, 24);

/// The first level that allows a factory (`CONTROLLER_STRUCTURES`).
fn factory_level() -> u8 {
    earliest_level(StructureType::Factory, 0).expect("a factory is allowed")
}

fn capacity_at(level: u8) -> u32 {
    allowed(StructureType::Spawn, level) * SPAWN_ENERGY_CAPACITY
        + allowed(StructureType::Extension, level) * extension_energy_capacity(u32::from(level))
}

fn index(r: ResourceType) -> usize {
    factory_resource(r).expect("a factory resource")
}

/// The utrium bar's recipe, the one most tests compress.
fn utrium() -> Recipe {
    *recipes()
        .iter()
        .find(|r| r.mineral == R::Utrium)
        .expect("utrium's bar")
}

// ---- The tables ----

/// `FACTORY_RESOURCES` is 8 minerals then their 8 bars, each once; every bar's `COMMODITIES`
/// entry has no level and takes only its mineral and energy (V131, V421), so `recipes` keeps all
/// 8 with the crate's numbers; each mineral's own entry takes its bar back (the decompression,
/// which the module never orders).
#[test]
fn every_mineral_has_a_level_free_bar_recipe() {
    for (i, r) in FACTORY_RESOURCES.iter().enumerate() {
        assert_eq!(factory_resource(*r), Some(i), "{r:?} once");
    }
    assert_eq!(factory_resource(R::Energy), None);
    let all = recipes();
    assert_eq!(all.len(), FACTORY_MINERALS);
    for (i, r) in all.iter().enumerate() {
        assert_eq!(r.mineral, FACTORY_RESOURCES[i]);
        assert_eq!(r.bar, FACTORY_RESOURCES[i + FACTORY_MINERALS]);
        let c = r.bar.commodity_recipe().expect("a recipe");
        assert_eq!(c.level, None, "{:?} needs no factory level", r.bar);
        assert_eq!(c.components.len(), 2);
        assert_eq!(c.components.get(&r.mineral), Some(&r.take));
        assert_eq!(c.components.get(&R::Energy), Some(&r.energy));
        assert_eq!((c.amount, c.cooldown), (r.make, r.cooldown));
        let back = r.mineral.commodity_recipe().expect("decompression");
        assert!(back.components.contains_key(&r.bar), "{:?}", r.mineral);
        // A batch's components leave room for its bars in any factory (V425).
        assert!(r.make <= r.take + r.energy && r.take + r.energy <= FACTORY_CAPACITY);
    }
}

// ---- The pace ----

/// The pace spends at most `factory.energy_per_life` in any creep life, never runs faster than
/// the cooldown, and never slower than one batch a life the budget pays for; a budget below one
/// batch makes nothing. A larger budget never slows the pace.
#[test]
fn the_pace_keeps_the_energy_under_its_budget() {
    for r in recipes() {
        let mut last = u32::MAX;
        for budget in (1..=40 * r.energy).step_by(37) {
            let cfg = FactoryConfig {
                energy_per_life: budget,
                ..FACTORY_MMO
            };
            match pace(&r, &cfg) {
                None => {
                    assert!(budget < r.energy, "{budget}");
                    assert_eq!(life_energy(&r, &cfg), 0);
                }
                Some(p) => {
                    assert!(p >= r.cooldown && p <= CREEP_LIFE_TIME, "{budget}: {p}");
                    let spent = life_energy(&r, &cfg);
                    assert!(spent <= u64::from(budget), "{budget}: {spent}");
                    assert!(spent >= u64::from(r.energy), "{budget}: at least one batch");
                    assert!(p <= last, "{budget}: {p} after {last}");
                    last = p;
                }
            }
        }
    }
    // The MMO's budget: a batch every CREEP_LIFE_TIME / (budget / energy) ticks.
    let r = utrium();
    let batches = FACTORY_MMO.energy_per_life / r.energy;
    assert_eq!(
        pace(&r, &FACTORY_MMO),
        Some(CREEP_LIFE_TIME.div_ceil(batches).max(r.cooldown))
    );
}

// ---- Picking a job ----

fn held_of(items: &[(ResourceType, u32)]) -> impl Fn(ResourceType) -> u32 + '_ {
    move |k| items.iter().filter(|(r, _)| *r == k).map(|(_, n)| *n).sum()
}

/// A mineral is compressed only with a whole batch above the reserve, and only while its bars stay
/// within the cap once the batch is made; the current job is kept over an earlier mineral; else
/// the first in `FACTORY_RESOURCES` order.
#[test]
fn a_job_needs_a_batch_above_the_reserve_and_room_under_the_cap() {
    let cfg = FACTORY_MMO;
    let all = recipes();
    let u = all.iter().position(|r| r.mineral == R::Utrium).expect("U");
    let h = all
        .iter()
        .position(|r| r.mineral == R::Hydrogen)
        .expect("H");
    assert!(h < u, "hydrogen comes first");
    let r = all[u];
    let enough = cfg.mineral_keep + r.take;
    assert_eq!(
        pick(&all, held_of(&[(R::Utrium, enough - 1)]), None, &cfg),
        None
    );
    assert_eq!(
        pick(&all, held_of(&[(R::Utrium, enough)]), None, &cfg),
        Some(u)
    );
    let bars = |n| [(R::Utrium, enough), (R::UtriumBar, n)];
    assert_eq!(
        pick(&all, held_of(&bars(cfg.bar_max - r.make)), None, &cfg),
        Some(u)
    );
    assert_eq!(
        pick(&all, held_of(&bars(cfg.bar_max - r.make + 1)), None, &cfg),
        None
    );
    let both = [(R::Utrium, enough), (R::Hydrogen, enough)];
    assert_eq!(pick(&all, held_of(&both), None, &cfg), Some(h));
    assert_eq!(pick(&all, held_of(&both), Some(u), &cfg), Some(u));
    assert_eq!(pick(&all, held_of(&[]), Some(u), &cfg), None);
    // A cap of 0 compresses nothing.
    let none = FactoryConfig { bar_max: 0, ..cfg };
    assert_eq!(pick(&all, held_of(&both), None, &none), None);
}

// ---- The world ----

#[derive(Clone, Copy, Debug)]
struct Creep {
    key: u128,
    at: Position,
    carry: u32,
    energy: u32,
    other: u32,
    other_kind: Option<ResourceType>,
    spawning: bool,
}

struct World {
    time: u32,
    level: u8,
    terminal: bool,
    factory: bool,
    /// The terminal's and the factory's stocks, and their energy.
    stock: FactoryStock,
    energy: u32,
    f_stock: FactoryStock,
    f_energy: u32,
    f_cooldown_until: u32,
    creeps: Vec<Creep>,
    reads: Vec<RoomName>,
    bank_reads: Vec<RoomName>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    module: Factories,
    cfg: FactoryConfig,
    standard: bool,
    hauled: bool,
    spawn_creeps: bool,
    /// What the next produce returns: `None` plays the engine.
    refuse: Option<ErrorCode>,
    terminal_handle: Option<StructureHandle>,
    factory_handle: Option<StructureHandle>,
    /// Batches applied: (tick, product).
    batches: Vec<(u32, ResourceType)>,
    /// Ticks the room was read, and ticks it had a bank read.
    read_at: Vec<u32>,
    bank_at: Vec<u32>,
}

impl World {
    /// The room at the first level that allows a factory, with a terminal holding `stock` and
    /// `energy`, and a factory.
    fn new(stock: &[(ResourceType, u32)], energy: u32) -> Self {
        let mut s = [0; 2 * FACTORY_MINERALS];
        for &(r, n) in stock {
            s[index(r)] = n;
        }
        let mut w = Self {
            time: 300_000,
            level: factory_level(),
            terminal: true,
            factory: true,
            stock: s,
            energy,
            f_stock: [0; 2 * FACTORY_MINERALS],
            f_energy: 0,
            f_cooldown_until: 0,
            creeps: Vec::new(),
            reads: Vec::new(),
            bank_reads: Vec::new(),
            snap: Snapshot::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            module: Factories::default(),
            cfg: FACTORY_MMO,
            standard: true,
            hauled: true,
            spawn_creeps: true,
            refuse: None,
            terminal_handle: None,
            factory_handle: None,
            batches: Vec::new(),
            read_at: Vec::new(),
            bank_at: Vec::new(),
        };
        // The first tick reads nothing; it asks for the room's read on the next.
        w.tick();
        w
    }

    fn home(&self) -> RoomName {
        room(HOME)
    }

    fn t_free(&self) -> u32 {
        TERMINAL_CAPACITY - self.stock.iter().sum::<u32>() - self.energy
    }

    fn f_free(&self) -> u32 {
        FACTORY_CAPACITY - self.f_stock.iter().sum::<u32>() - self.f_energy
    }

    fn held(&self, r: ResourceType) -> u32 {
        self.stock[index(r)]
    }

    /// What the room holds of `r` in all: terminal, factory and haulers.
    fn total(&self, r: ResourceType) -> u32 {
        self.held(r)
            + self.f_stock[index(r)]
            + self
                .creeps
                .iter()
                .filter(|c| c.other_kind == Some(r))
                .map(|c| c.other)
                .sum::<u32>()
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
                role: Role::FactoryHauler,
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
                        energy: c.energy,
                        capacity: c.carry * CARRY_CAPACITY,
                        other: c.other,
                        other_kind: c.other_kind.filter(|_| c.other > 0),
                    },
                );
            }
        }
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
        let bank = self.bank_reads.contains(&home);
        if read {
            self.snap.factory.push(home);
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
            let at = pos(home, TERMINAL.0, TERMINAL.1);
            let h = self
                .snap
                .structures
                .push(ObjectKey(12), || {
                    Ok(StructureStatic {
                        kind: StructureType::Terminal,
                        pos: at,
                    })
                })
                .expect("terminal");
            self.terminal_handle = Some(h);
            if read {
                assert!(self.snap.factory.set_terminal(FactoryTerminal {
                    handle: h,
                    at,
                    free: self.t_free(),
                    energy: self.energy,
                }));
                for (i, &n) in self.stock.iter().enumerate() {
                    if n > 0 {
                        self.snap.factory.set_stock(home, FACTORY_RESOURCES[i], n);
                    }
                }
            } else if bank {
                assert!(
                    !read,
                    "a room read in full has no bank read (Factories::bank_reads)"
                );
                self.snap.factory.set_bank(home, self.energy);
                self.bank_at.push(self.time);
            }
        }
        self.factory_handle = None;
        if self.factory {
            let at = pos(home, FACTORY.0, FACTORY.1);
            let h = self
                .snap
                .structures
                .push(ObjectKey(13), || {
                    Ok(StructureStatic {
                        kind: StructureType::Factory,
                        pos: at,
                    })
                })
                .expect("factory");
            let row = u32::try_from(self.snap.structures.len() - 1).expect("small");
            self.factory_handle = Some(h);
            if read {
                assert!(self.snap.factory.set_factory(FactoryState {
                    handle: h,
                    row,
                    at,
                    cooldown: self.f_cooldown_until.saturating_sub(self.time),
                    free: self.f_free(),
                    energy: self.f_energy,
                    stock: self.f_stock,
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
            let blocked: Vec<(Tile, Footprint)> = [SPAWN, TERMINAL, FACTORY]
                .into_iter()
                .map(|(x, y)| (tile(x, y), Footprint::Blocked))
                .collect();
            self.movement.set_objects(r, blocked);
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.module.begin(&self.snap);
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        for u in 0..self.module.units() {
            self.module
                .plan_room(
                    u,
                    &self.snap,
                    &mut self.spawner,
                    &self.cfg,
                    self.standard,
                    self.hauled,
                )
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
        let (reads, banks) = self.module.reads(&self.cfg, self.standard);
        self.reads = reads.to_vec();
        self.bank_reads = banks.to_vec();
    }

    /// The engine's side of the tick (V420 to V427), then the model's spawn.
    fn apply(&mut self) {
        let orders: Vec<_> = self.module.orders().to_vec();
        for (i, o) in orders.iter().enumerate() {
            assert_eq!(Some(o.factory), self.factory_handle);
            if let Some(code) = self.refuse {
                let _ = self
                    .module
                    .record_produce(i, Outcome::Failed(code), &self.cfg);
                continue;
            }
            assert!(self.f_cooldown_until <= self.time, "on cooldown (V420)");
            let r = recipes()
                .into_iter()
                .find(|r| r.bar == o.product)
                .expect("a bar");
            assert!(
                self.f_stock[index(r.mineral)] >= r.take,
                "no mineral (V420)"
            );
            assert!(self.f_energy >= r.energy, "no energy (V420)");
            let used = FACTORY_CAPACITY - self.f_free();
            assert!(
                used - r.take - r.energy + r.make <= FACTORY_CAPACITY,
                "full (V425)"
            );
            self.f_stock[index(r.mineral)] -= r.take;
            self.f_energy -= r.energy;
            self.f_stock[index(r.bar)] += r.make;
            self.f_cooldown_until = self.time + r.cooldown;
            self.batches.push((self.time, r.bar));
            let _ = self.module.record_produce(i, Outcome::Ok, &self.cfg);
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
                    let room_left = c.carry * CARRY_CAPACITY - c.other - c.energy;
                    assert!(
                        c.other == 0 || c.other_kind == Some(resource),
                        "a second type in the hauler"
                    );
                    let energy = resource == R::Energy;
                    let n = if Some(target) == self.terminal_handle {
                        let held = if energy {
                            self.energy
                        } else {
                            self.held(resource)
                        };
                        let n = amount.expect("an amount from the terminal");
                        assert!(n <= held, "more than the terminal holds (V400)");
                        assert!(n <= room_left, "more than the hauler holds (V400)");
                        if energy {
                            self.energy -= n;
                        } else {
                            self.stock[index(resource)] -= n;
                        }
                        n
                    } else {
                        assert_eq!(Some(target), self.factory_handle, "a factory");
                        assert_eq!(amount, None);
                        assert!(!energy, "the hauler never takes the factory's energy");
                        let have = self.f_stock[index(resource)];
                        assert!(have > 0, "a withdraw of nothing (V400)");
                        let n = room_left.min(have);
                        self.f_stock[index(resource)] -= n;
                        n
                    };
                    let c = &mut self.creeps[i];
                    if energy {
                        c.energy += n;
                    } else {
                        c.other += n;
                        c.other_kind = Some(resource);
                    }
                }
                CreepIntent::Transfer {
                    target: Target::Structure(target),
                    resource,
                    amount: None,
                } => {
                    let c = self.creeps[i];
                    let energy = resource == R::Energy;
                    let carried = if energy { c.energy } else { c.other };
                    assert!(carried > 0, "a transfer of nothing");
                    if !energy {
                        assert_eq!(c.other_kind, Some(resource));
                    }
                    let n = if Some(target) == self.terminal_handle {
                        let n = carried.min(self.t_free());
                        if energy {
                            self.energy += n;
                        } else {
                            self.stock[index(resource)] += n;
                        }
                        n
                    } else {
                        assert_eq!(Some(target), self.factory_handle, "a factory");
                        let n = carried.min(self.f_free());
                        if energy {
                            self.f_energy += n;
                        } else {
                            self.f_stock[index(resource)] += n;
                        }
                        n
                    };
                    let c = &mut self.creeps[i];
                    if energy {
                        c.energy -= n;
                    } else {
                        c.other -= n;
                        if c.other == 0 {
                            c.other_kind = None;
                        }
                    }
                }
                other => panic!("an intent the factory does not make: {other:?}"),
            }
        }
        if self.spawn_creeps {
            for (role, body, _) in self.requests() {
                assert_eq!(role, Role::FactoryHauler);
                let key = 1000 + self.creeps.len() as u128;
                self.creeps.push(Creep {
                    key,
                    at: pos(self.home(), SPAWN.0 + 1, SPAWN.1),
                    carry: body.count(Part::Carry) as u32,
                    energy: 0,
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
                    panic!("the factory hauler's body is exact");
                };
                (r.role, b, r.priority)
            })
            .collect()
    }

    fn room(&self) -> &FactoryRoom {
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

/// A room holding three batches of utrium above the reserve and energy for them compresses them:
/// one factory hauler at `PRIORITY_FACTORY_HAULER`, three batches at least a pace apart with no
/// refused call, the job ending at the reserve, the bars all back in the terminal, the mineral at
/// the reserve, and the factory left holding no mineral or bar.
#[test]
fn a_room_compresses_its_surplus_to_the_reserve() {
    let r = utrium();
    let cfg = FACTORY_MMO;
    let start = cfg.mineral_keep + 3 * r.take;
    let mut w = World::new(&[(R::Utrium, start)], 3 * r.energy);
    w.run_until(5, |w| w.room().job.is_some());
    assert_eq!(w.room().why, FactoryWhy::Compressing);
    let reqs = w.requests();
    assert!(
        reqs.iter()
            .all(|&(role, _, p)| role == Role::FactoryHauler && p == PRIORITY_FACTORY_HAULER),
        "{reqs:?}"
    );
    let pace = pace(&r, &cfg).expect("a pace");
    w.run_until(6 * pace, |w| {
        w.room().job.is_none() && w.held(R::UtriumBar) == 3 * r.make
    });
    assert_eq!(w.batches.len(), 3);
    for pair in w.batches.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= pace, "{pair:?}");
    }
    assert_eq!(w.room().stats.refused, 0);
    assert_eq!(w.room().stats.requests, 1, "one hauler");
    assert_eq!(w.total(R::Utrium), cfg.mineral_keep);
    assert_eq!(w.total(R::UtriumBar), 3 * r.make);
    w.run_until(50, |w| w.f_stock.iter().all(|&n| n == 0));
    assert_eq!(w.held(R::UtriumBar), 3 * r.make);
    assert_eq!(w.held(R::Utrium), cfg.mineral_keep);
    assert_eq!(w.room().why, FactoryWhy::Idle);
    assert_eq!(w.room().stats.jobs, 1);
}

/// The job stops at the bar cap: with the room's bars one batch short of `factory.bar_max`, one
/// batch is made and the job ends there, though the mineral still has a surplus.
#[test]
fn the_bar_cap_ends_a_job() {
    let r = utrium();
    let cfg = FACTORY_MMO;
    let mut w = World::new(
        &[
            (R::Utrium, cfg.mineral_keep + 5 * r.take),
            (R::UtriumBar, cfg.bar_max - r.make),
        ],
        5 * r.energy,
    );
    w.run_until(5 * pace(&r, &cfg).expect("a pace"), |w| {
        !w.batches.is_empty() && w.room().job.is_none()
    });
    assert_eq!(w.batches.len(), 1);
    assert_eq!(w.total(R::UtriumBar), cfg.bar_max);
    assert!(w.total(R::Utrium) >= cfg.mineral_keep + r.take);
    assert!(
        !w.room().wants || w.factory,
        "a standing factory is still wanted"
    );
}

/// The hauler is requested only with work, only while the Standard tier runs, and never twice.
#[test]
fn the_hauler_is_asked_for_only_with_work() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut idle = World::new(&[(R::Utrium, cfg.mineral_keep)], 10 * r.energy);
    for _ in 0..20 {
        idle.tick();
    }
    assert_eq!(idle.room().stats.requests, 0);
    assert_eq!(idle.room().why, FactoryWhy::Idle);
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + r.take)], r.energy);
    w.spawn_creeps = false;
    w.standard = false;
    for _ in 0..5 {
        w.tick();
    }
    assert_eq!(
        w.room().stats.requests,
        0,
        "no request off the Standard tier"
    );
    w.standard = true;
    w.tick();
    w.tick();
    assert!(w.room().stats.requests >= 1);
    w.spawn_creeps = true;
    w.tick();
    let before = w.room().stats.requests;
    for _ in 0..20 {
        w.tick();
    }
    assert_eq!(w.room().stats.requests, before, "never a second hauler");
}

/// While a room compresses with its terminal below `factory.energy_keep`, the terminal is a bank
/// for the haulers with the rest as its want, only while the Standard tier runs and only where a
/// source is hauled; a room read in full gets no bank read, one not read gets one; with the
/// terminal at the mark, no bank.
#[test]
fn the_terminal_takes_energy_while_the_factory_needs_it() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 10 * r.take)], 0);
    w.spawn_creeps = false;
    w.run_until(5, |w| w.room().job.is_some());
    w.tick();
    let banks = w.module.banks().to_vec();
    assert_eq!(banks.len(), 1);
    let (name, h, at, want) = banks[0];
    assert_eq!(name, w.home());
    assert_eq!(Some(h), w.terminal_handle);
    assert_eq!(at, pos(w.home(), TERMINAL.0, TERMINAL.1));
    assert_eq!(want, cfg.energy_keep);
    // No hauler and not due: the next tick reads only the terminal's energy.
    assert!(!w.reads.contains(&w.home()));
    assert_eq!(w.bank_reads, vec![w.home()]);
    // The haulers deliver half: the bank read shows it and the want shrinks.
    w.energy = cfg.energy_keep / 2;
    w.tick();
    assert_eq!(w.bank_at.last(), Some(&(w.time - 1)));
    assert_eq!(
        w.module.banks().first().map(|b| b.3),
        Some(cfg.energy_keep - cfg.energy_keep / 2)
    );
    // Full: closed, and no more bank reads.
    w.energy = cfg.energy_keep;
    w.tick();
    assert!(w.module.banks().is_empty());
    assert!(w.bank_reads.is_empty());
    // Off the Standard tier: no bank.
    w.energy = 0;
    w.standard = false;
    w.reads = vec![w.home()];
    w.tick();
    assert!(w.module.banks().is_empty());
    // With no hauled source: no bank, and no bank read.
    w.standard = true;
    w.hauled = false;
    w.tick();
    w.tick();
    assert!(w.module.banks().is_empty());
    assert!(w.bank_reads.is_empty());
    // With no job: no bank.
    let mut idle = World::new(&[(R::Utrium, cfg.mineral_keep)], 0);
    for _ in 0..3 {
        idle.tick();
    }
    assert!(idle.module.banks().is_empty());
}

/// The upkeep the upgraders leave out: the hauler's body while the factory has work, and the
/// pace's energy while it compresses with energy to be had (the source haulers carry energy home,
/// or a batch's energy is at hand); 0 with neither.
#[test]
fn the_upkeep_counts_the_hauler_and_the_paced_energy() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let body = crate::economy::lab_hauler_body(capacity_at(factory_level()), cfg.hauler_carry)
        .expect("a body");
    let upkeep = |w: &World| {
        w.module
            .upkeeps()
            .iter()
            .find(|u| u.0 == w.home())
            .map(|u| u.1)
    };
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 5 * r.take)], 0);
    w.spawn_creeps = false;
    w.run_until(5, |w| w.room().job.is_some());
    w.tick();
    assert_eq!(
        upkeep(&w),
        Some(u64::from(body.cost()) + life_energy(&r, &cfg))
    );
    w.hauled = false;
    w.tick();
    assert_eq!(
        upkeep(&w),
        Some(u64::from(body.cost())),
        "no energy to be had"
    );
    w.energy = r.energy;
    w.reads = vec![w.home()];
    w.tick();
    assert_eq!(
        upkeep(&w),
        Some(u64::from(body.cost()) + life_energy(&r, &cfg)),
        "energy at hand"
    );
    let mut idle = World::new(&[], 0);
    idle.tick();
    assert_eq!(upkeep(&idle), Some(0));
}

/// A room wants a factory once its terminal holds a job's surplus, or once one stands; not with
/// the module off, and not before.
#[test]
fn a_factory_is_wanted_once_it_has_work() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + r.take - 1)], 0);
    w.factory = false;
    for _ in 0..3 {
        w.tick();
    }
    assert_eq!(w.room().why, FactoryWhy::NoFactory);
    assert!(w.module.wants_factory().is_empty());
    w.stock[index(R::Utrium)] += 1;
    w.reads = vec![w.home()];
    w.tick();
    let home = w.home();
    assert_eq!(w.module.wants_factory(), &[home][..]);
    assert!(w.room().job.is_none(), "no job without a factory");
    let mut standing = World::new(&[], 0);
    standing.tick();
    let home = standing.home();
    assert_eq!(standing.module.wants_factory(), &[home][..]);
    standing.cfg.enabled = false;
    standing.tick();
    assert!(standing.module.wants_factory().is_empty());
    assert_eq!(standing.room().why, FactoryWhy::Off);
}

/// A terminal below the factory's level is not tracked (nothing is read), one at it is.
#[test]
fn only_rooms_at_the_factorys_level_are_tracked() {
    let mut w = World::new(&[], 0);
    w.factory = false;
    w.level = factory_level() - 1;
    w.tick();
    assert!(w.module.rooms().is_empty());
    w.level = factory_level();
    w.tick();
    assert_eq!(w.module.rooms().len(), 1);
}

/// A batch's deadline asks for a read only while the Standard tier runs: with the cadence out of
/// reach, a room waiting for its pace is not read off the Standard tier, and is read on the due
/// tick once the tier is back, where the batch goes out. The tick after a batch is read too, and
/// the hauler reloads the factory well before the pace ends.
#[test]
fn a_due_batch_asks_for_no_read_without_the_standard_tier() {
    let r = utrium();
    let mut w = World::new(
        &[(R::Utrium, FACTORY_MMO.mineral_keep + 10 * r.take)],
        10 * r.energy,
    );
    w.cfg.read_ticks = 10_000;
    let pace = pace(&r, &w.cfg).expect("a pace");
    w.run_until(200, |w| !w.batches.is_empty());
    let first = w.batches[0].0;
    w.tick();
    assert!(
        w.read_at.contains(&(first + 1)),
        "the tick after a batch is read"
    );
    w.run_until(pace, |w| {
        !w.room().busy && w.f_stock[index(R::Utrium)] >= r.take && w.f_energy >= r.energy
    });
    assert!(w.time < first + pace, "reloaded before the pace ends");
    let next = w.room().next_produce().expect("the next batch is due");
    assert_eq!(next, first + pace);
    w.standard = false;
    let from = w.time;
    while w.time <= next + 2 {
        w.tick();
    }
    assert!(
        w.read_at.iter().all(|&t| t <= from),
        "no read off the Standard tier: {:?}",
        w.read_at.iter().filter(|&&t| t > from).collect::<Vec<_>>()
    );
    assert_eq!(w.batches.len(), 1);
    w.standard = true;
    w.run_until(5, |w| w.batches.len() == 2);
}

/// A refused `produce` holds the room's batches for `factory.read_ticks`, counted once.
#[test]
fn a_refused_batch_holds_the_room() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 3 * r.take)], 3 * r.energy);
    w.refuse = Some(ErrorCode::RclNotEnough);
    w.run_until(200, |w| w.room().stats.refused > 0);
    let held = w.room().held_until;
    assert_eq!(held, w.time - 1 + cfg.read_ticks);
    for _ in 0..cfg.read_ticks - 2 {
        w.tick();
    }
    assert_eq!(w.room().stats.refused, 1, "no call while held");
    w.refuse = None;
    w.run_until(3 * cfg.read_ticks, |w| !w.batches.is_empty());
    assert!(w.batches[0].0 >= held);
}

/// What the factory holds that the job does not use goes back first: another mineral's bar while
/// a job runs, and everything but energy once no job is left.
#[test]
fn strays_go_back_to_the_terminal() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 2 * r.take)], 2 * r.energy);
    w.f_stock[index(R::Oxidant)] = 2 * CARRY_CAPACITY;
    w.f_stock[index(R::Keanium)] = CARRY_CAPACITY;
    w.run_until(100, |w| {
        w.held(R::Oxidant) == 2 * CARRY_CAPACITY && w.held(R::Keanium) == CARRY_CAPACITY
    });
    assert_eq!(
        w.f_stock[index(R::Oxidant)] + w.f_stock[index(R::Keanium)],
        0
    );
    // The job runs out: its last bars and any leftover go back too.
    w.run_until(10 * pace(&r, &cfg).expect("a pace"), |w| {
        w.room().job.is_none()
            && w.f_stock.iter().all(|&n| n == 0)
            && w.creeps.iter().all(|c| c.other == 0)
    });
    assert_eq!(w.batches.len(), 2);
    assert_eq!(w.held(R::UtriumBar), w.total(R::UtriumBar));
}

/// A hauler holding a load with nowhere to go (a full terminal) keeps no read going, though an
/// empty hauler would have a task (the factory short of the job's mineral); once the terminal
/// has room, the next read sends it there (the hot-path review, 2026-11-26).
#[test]
fn a_load_with_nowhere_to_go_keeps_no_read_going() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 5 * r.take)], 5 * r.energy);
    w.run_until(100, |w| {
        w.room().job.is_some() && w.creeps.iter().any(|c| !c.spawning)
    });
    // The hauler holds bars it must take back, the terminal is full, the factory lacks utrium.
    let c = &mut w.creeps[0];
    c.other = CARRY_CAPACITY;
    c.other_kind = Some(R::Oxidant);
    w.f_stock[index(R::Utrium)] = 0;
    let room_left = w.t_free();
    w.stock[index(R::Catalyst)] += room_left;
    assert_eq!(w.t_free(), 0);
    w.reads = vec![w.home()];
    w.tick();
    w.tick();
    let from = w.time;
    for _ in 0..30 {
        w.tick();
    }
    let reads = w.read_at.iter().filter(|&&t| t > from).count();
    assert_eq!(reads, 0, "no read while the load has nowhere to go");
    // Room again: the cadence read sends the bars home.
    w.stock[index(R::Catalyst)] -= room_left;
    w.run_until(2 * cfg.read_ticks, |w| w.held(R::Oxidant) == CARRY_CAPACITY);
}

/// A factory in a room whose level no longer allows one is inactive (V420's controller check):
/// it gets no job and its room wants none built; and a budget that pays for no batch starts no
/// job, so no hauler is sent to load a factory that would never produce.
#[test]
fn no_job_below_the_factorys_level_or_without_a_batch_paid_for() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 5 * r.take)], 5 * r.energy);
    w.level = factory_level() - 1;
    for _ in 0..3 {
        w.tick();
    }
    assert!(w.room().job.is_none());
    assert_eq!(w.room().stats.requests, 0);
    let mut poor = World::new(&[(R::Utrium, cfg.mineral_keep + 5 * r.take)], 5 * r.energy);
    poor.cfg.energy_per_life = r.energy - 1;
    for _ in 0..3 {
        poor.tick();
    }
    assert!(poor.room().job.is_none());
    assert_eq!(poor.room().stats.requests, 0);
    assert!(poor.batches.is_empty());
}

/// Off, the hauler only delivers what it holds, to the terminal; nothing is produced or loaded.
#[test]
fn off_only_delivers_what_the_hauler_holds() {
    let cfg = FACTORY_MMO;
    let r = utrium();
    let mut w = World::new(&[(R::Utrium, cfg.mineral_keep + 5 * r.take)], 5 * r.energy);
    w.run_until(50, |w| w.creeps.iter().any(|c| c.other > 0 || c.energy > 0));
    w.cfg.enabled = false;
    let made = w.batches.len();
    for _ in 0..100 {
        w.tick();
    }
    assert!(w.creeps.iter().all(|c| c.other == 0 && c.energy == 0));
    assert_eq!(w.batches.len(), made);
    assert!(w.room().job.is_none());
    assert!(w.module.banks().is_empty());
}
