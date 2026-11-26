//! Tests of mineral mining (P9.1, ADR-044, `economy::minerals`). Expected values derive from the
//! constants (`EXTRACTOR_COOLDOWN`, `HARVEST_MINERAL_POWER`, `ENERGY_REGEN_TIME`,
//! `CREEP_LIFE_TIME`, `CARRY_CAPACITY`, `CONTAINER_CAPACITY`, `BODYPART_COST`, `MAX_CREEP_SIZE`,
//! `CONTROLLER_STRUCTURES`) and the minerals config.

use rustedscreep_consts::{
    CARRY_CAPACITY, CONTAINER_CAPACITY, CREEP_LIFE_TIME, ENERGY_REGEN_TIME, EXTRACTOR_COOLDOWN,
    HARVEST_MINERAL_POWER, MAX_CREEP_SIZE, Part, Position, ROOM_AREA, ResourceType, RoomCoordinate,
    RoomName, SPAWN_ENERGY_CAPACITY, StructureType, TERMINAL_CAPACITY, extension_energy_capacity,
};

use super::minerals::*;
use crate::config::{HAULING_MMO, MINERALS_MMO, MOVEMENT_MMO, MineralsConfig, SPAWN_MMO};
use crate::economy::sizing::{Trip, haul_round, walk_ticks};
use crate::intents::{CreepIntent, IntentBuffer, Target};
use crate::movement::{Footprint, Movement, ShoveCaps, Tile};
use crate::planner::rules::{allowed, earliest_level};
use crate::scheduler::Commit;
use crate::snapshot::{
    Controller, CreepRow, CreepStore, MineralContainer, MineralSite, MineralTerminal, ObjectKey,
    ObjectKind, ObjectRow, ObjectStatic, RoomEnergy, Snapshot, StructureStatic, TickContext,
};
use crate::spawning::{Body, BodySpec, Identity, Role, Spawner};

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

const HOME: &str = "E2N2";
const SPAWN: (u8, u8) = (25, 25);
const MINERAL: (u8, u8) = (10, 10);
const CONTAINER: (u8, u8) = (11, 11);
const TERMINAL: (u8, u8) = (25, 28);
const KIND: ResourceType = ResourceType::Hydrogen;

/// The first level that allows the extractor and the terminal (V393).
fn mineral_level() -> u8 {
    earliest_level(StructureType::Extractor, 0)
        .max(earliest_level(StructureType::Terminal, 0))
        .expect("allowed at some level")
}

/// A room's capacity at `level`: its spawns' and extensions' (`CONTROLLER_STRUCTURES`,
/// `SPAWN_ENERGY_CAPACITY`, `EXTENSION_ENERGY_CAPACITY`).
fn capacity_at(level: u8) -> u32 {
    allowed(StructureType::Spawn, level) * SPAWN_ENERGY_CAPACITY
        + allowed(StructureType::Extension, level) * extension_energy_capacity(u32::from(level))
}

#[derive(Clone, Copy, Debug)]
struct Creep {
    key: u128,
    role: Role,
    at: Position,
    work: u32,
    carry: u32,
    other: u32,
    spawning: bool,
}

struct World {
    time: u32,
    level: u8,
    kind: Option<ResourceType>,
    amount: u32,
    cooldown: u32,
    extractor: bool,
    container: bool,
    /// The container's mineral and its other contents (energy).
    stock: u32,
    container_energy: u32,
    terminal: bool,
    terminal_stock: u32,
    terminal_free: u32,
    creeps: Vec<Creep>,
    /// The rooms the snapshot reads this tick (the module's last `reads`).
    reads: Vec<RoomName>,
    snap: Snapshot,
    movement: Movement,
    spawner: Spawner,
    buffer: IntentBuffer,
    minerals: Minerals,
    cfg: MineralsConfig,
    standard: bool,
    /// Requests served by the model's spawn, which places their creeps next tick.
    spawn_creeps: bool,
    harvests: Vec<u32>,
}

impl World {
    fn new() -> Self {
        let mut w = Self {
            time: 200_000,
            level: mineral_level(),
            kind: Some(KIND),
            amount: 35_000,
            cooldown: 0,
            extractor: true,
            container: true,
            stock: 0,
            container_energy: 0,
            terminal: true,
            terminal_stock: 0,
            terminal_free: TERMINAL_CAPACITY,
            creeps: Vec::new(),
            reads: Vec::new(),
            snap: Snapshot::default(),
            movement: Movement::default(),
            spawner: Spawner::default(),
            buffer: IntentBuffer::default(),
            minerals: Minerals::default(),
            cfg: MINERALS_MMO,
            standard: true,
            spawn_creeps: false,
            harvests: Vec::new(),
        };
        // The first tick reads nothing; it asks for the room's read on the next.
        w.tick();
        w
    }

    fn home(&self) -> RoomName {
        room(HOME)
    }

    fn add(&mut self, role: Role, x: u8, y: u8, work: u32, carry: u32) -> usize {
        let key = 1000 + self.creeps.len() as u128;
        self.creeps.push(Creep {
            key,
            role,
            at: pos(self.home(), x, y),
            work,
            carry,
            other: 0,
            spawning: false,
        });
        self.creeps.len() - 1
    }

    fn take_snapshot(&mut self) {
        let home = self.home();
        self.snap
            .begin(TickContext::from_raw(self.time, 20, 500.0, 10_000));
        for (row, c) in self.creeps.iter().enumerate() {
            let r = CreepRow {
                pos: c.at,
                my: true,
                spawning: c.spawning,
                fatigue: 0,
            };
            let id = Identity {
                role: c.role,
                home,
                remote: None,
                serial: c.key as u64,
            };
            self.snap
                .creeps
                .push_named(ObjectKey(c.key), r, || Some(id))
                .expect("creep");
            self.snap.creeps.set_store(
                row,
                CreepStore {
                    energy: 0,
                    capacity: c.carry * CARRY_CAPACITY,
                    other: if c.role == Role::MineralHauler {
                        c.other
                    } else {
                        0
                    },
                    other_kind: None,
                },
            );
            self.snap.creeps.set_death(row, self.time + CREEP_LIFE_TIME);
        }
        // A worker that stays out of the way, as a live home always holds own creeps: movement
        // reads only a room an own creep stands in. Its row comes after the model's creeps.
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
        let mineral = read.then(|| {
            self.snap
                .objects
                .push(
                    ObjectKey(1),
                    || {
                        Ok(ObjectStatic {
                            kind: ObjectKind::Mineral,
                            pos: pos(home, MINERAL.0, MINERAL.1),
                        })
                    },
                    ObjectRow {
                        amount: self.amount,
                        total: 0,
                        hits: 0,
                    },
                )
                .expect("mineral")
        });
        let container = self.container.then(|| {
            self.snap
                .objects
                .push(
                    ObjectKey(2),
                    || {
                        Ok(ObjectStatic {
                            kind: ObjectKind::Container,
                            pos: pos(home, CONTAINER.0, CONTAINER.1),
                        })
                    },
                    ObjectRow {
                        amount: self.container_energy,
                        total: 0,
                        hits: 1,
                    },
                )
                .expect("container")
        });
        if let Some(handle) = mineral {
            self.snap.minerals.push(MineralSite {
                room: home,
                handle,
                at: pos(home, MINERAL.0, MINERAL.1),
                kind: self.kind,
                amount: self.amount,
                regen: None,
                extractor: None,
                container: container.map(|handle| MineralContainer {
                    handle,
                    at: pos(home, CONTAINER.0, CONTAINER.1),
                    stock: self.stock,
                    used: self.stock + self.container_energy,
                }),
                terminal: None,
            });
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
        if self.extractor {
            let h = self
                .snap
                .structures
                .push(ObjectKey(11), || {
                    Ok(StructureStatic {
                        kind: StructureType::Extractor,
                        pos: pos(home, MINERAL.0, MINERAL.1),
                    })
                })
                .expect("extractor");
            if read {
                self.snap
                    .minerals
                    .set_extractor(pos(home, MINERAL.0, MINERAL.1), h, self.cooldown);
            }
        }
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
            if read {
                self.snap.minerals.set_terminal(MineralTerminal {
                    handle: h,
                    at: pos(home, TERMINAL.0, TERMINAL.1),
                    stock: self.terminal_stock,
                    free: self.terminal_free,
                });
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
        let home = self.home();
        for &r in &rooms {
            assert!(self.movement.set_terrain(r, &[0u8; ROOM_AREA]));
            self.movement
                .set_statics(r, [Tile::new(MINERAL.0, MINERAL.1).expect("tile")]);
            let blocked = [SPAWN, TERMINAL]
                .into_iter()
                .map(|(x, y)| (Tile::new(x, y).expect("tile"), Footprint::Blocked));
            self.movement.set_objects(r, blocked);
            assert_eq!(r, home);
        }
        self.buffer.begin(&self.snap);
        self.spawner.begin(&self.snap, &SPAWN_MMO);
        self.minerals.begin(&self.snap, &self.cfg);
    }

    fn work(&mut self) {
        let mut commit = Commit::default();
        for u in 0..self.minerals.units() {
            self.minerals
                .plan_room(
                    u,
                    &self.snap,
                    &mut self.movement,
                    &mut self.spawner,
                    &self.cfg,
                    &HAULING_MMO,
                    &MOVEMENT_MMO,
                    self.standard,
                    &mut commit,
                )
                .expect("plan");
        }
        for u in 0..self.minerals.units() {
            self.minerals
                .work_room(
                    u,
                    &self.snap,
                    &mut self.movement,
                    &mut self.buffer,
                    &MOVEMENT_MMO,
                    &mut commit,
                )
                .expect("work");
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
        self.reads = self.minerals.reads(&self.cfg, self.standard).to_vec();
    }

    /// The engine's side of the tick: the intents, the extractor's cooldown (V388, V389) and the
    /// model's spawn.
    fn apply(&mut self) {
        let mut harvested = false;
        let planned: Vec<_> = self.buffer.planned().copied().collect();
        for issue in planned {
            let i = issue.actor as usize;
            if i >= self.creeps.len() {
                // The resident worker, shoved aside.
                continue;
            }
            match issue.intent {
                CreepIntent::Move { direction } => {
                    let c = &mut self.creeps[i];
                    let t = Tile::of(c.at).step(direction).expect("in the room");
                    c.at = pos(c.at.room_name(), t.x(), t.y());
                }
                CreepIntent::Harvest { .. } => {
                    // V385's checks, then V387's amount and overflow into the container.
                    assert!(self.cooldown == 0, "a harvest on cooldown");
                    let n = (self.creeps[i].work * HARVEST_MINERAL_POWER).min(self.amount);
                    self.amount -= n;
                    assert!(
                        self.stock + self.container_energy + n <= CONTAINER_CAPACITY,
                        "a harvest the container cannot take"
                    );
                    self.stock += n;
                    harvested = true;
                    self.harvests.push(self.time);
                }
                CreepIntent::WithdrawObject { resource, .. } => {
                    assert_eq!(resource, KIND);
                    let c = &mut self.creeps[i];
                    let free = c.carry * CARRY_CAPACITY - c.other;
                    let n = free.min(self.stock);
                    self.stock -= n;
                    c.other += n;
                }
                CreepIntent::Transfer {
                    target: Target::Structure(_),
                    resource,
                    amount: None,
                } => {
                    assert_eq!(resource, KIND);
                    let c = &mut self.creeps[i];
                    let n = c.other.min(self.terminal_free);
                    c.other -= n;
                    self.terminal_stock += n;
                    self.terminal_free -= n;
                }
                other => panic!("an intent the minerals do not make: {other:?}"),
            }
        }
        // V389: a harvest sets the cooldown, which this tick's pass does not count down.
        if harvested {
            self.cooldown = EXTRACTOR_COOLDOWN;
        } else {
            self.cooldown = self.cooldown.saturating_sub(1);
        }
        if self.spawn_creeps {
            for r in self.requests() {
                let work = r.1.count(Part::Work) as u32;
                let carry = r.1.count(Part::Carry) as u32;
                self.add(r.0, SPAWN.0 + 1, SPAWN.1, work, carry);
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
                    panic!("the mineral bodies are exact");
                };
                (r.role, b, r.priority)
            })
            .collect()
    }

    fn intents(&self) -> Vec<CreepIntent> {
        self.buffer
            .planned()
            .filter(|i| !matches!(i.intent, CreepIntent::Move { .. }))
            .map(|i| i.intent)
            .collect()
    }

    fn room(&self) -> &MineralRoom {
        self.minerals.rooms().first().expect("the room")
    }
}

// ---- Sizing ----

#[test]
fn a_mineral_gives_a_harvest_every_cooldown_plus_one_ticks() {
    assert_eq!(HARVEST_PERIOD, EXTRACTOR_COOLDOWN + 1);
    for work in [1, 6, 17, 24, 49] {
        assert_eq!(
            rate_capacity(work),
            (work * HARVEST_MINERAL_POWER * ENERGY_REGEN_TIME).div_ceil(HARVEST_PERIOD)
        );
        assert_eq!(
            life_yield(work, 0),
            u64::from(CREEP_LIFE_TIME / HARVEST_PERIOD) * u64::from(work * HARVEST_MINERAL_POWER)
        );
    }
}

/// The miner's body over every capacity and a range of walks: within the energy, the part cap
/// and `minerals.max_work`; WORK then MOVE parts only; and a capacity never lowers its life's
/// yield (a larger room can always afford the smaller body).
#[test]
fn the_miners_body_fits_and_grows_with_the_capacity() {
    for cost in [2, 10, 30, 60] {
        let trip = Trip {
            tiles: cost / 2,
            cost,
        };
        let mut last = 0u64;
        for energy in (150..=12_900).step_by(50) {
            let Some(body) = miner_body(energy, MINERALS_MMO.max_work, trip) else {
                assert!(
                    energy < Part::Work.cost() + Part::Move.cost(),
                    "no body at {energy}"
                );
                continue;
            };
            let work = body.count(Part::Work) as u32;
            let moves = body.count(Part::Move) as u32;
            assert!(body.cost() <= energy);
            assert!(body.len() as u32 <= MAX_CREEP_SIZE);
            assert!((1..=MINERALS_MMO.max_work).contains(&work));
            assert!(moves >= 1);
            assert_eq!(work + moves, body.len() as u32, "no CARRY part");
            let walk = walk_ticks(trip, work, moves).expect("walks");
            let got = life_yield(work, walk);
            assert!(got >= last, "{energy}: {got} < {last}");
            last = got;
        }
    }
}

/// At the first mineral level's capacity the WORK parts are what it affords, up to the cap.
#[test]
fn the_miner_takes_what_the_mineral_level_affords() {
    let energy = capacity_at(mineral_level());
    let trip = Trip {
        tiles: 10,
        cost: 20,
    };
    let body = miner_body(energy, MINERALS_MMO.max_work, trip).expect("a body");
    let work = body.count(Part::Work) as u32;
    let most = (energy - Part::Move.cost()) / Part::Work.cost();
    assert!(work <= most.min(MINERALS_MMO.max_work));
    // Every WORK part it leaves out would cost the miner more walk than it harvests.
    assert!(work >= most.min(MINERALS_MMO.max_work) / 2, "{body:?}");
}

/// The haulers carry the miner's yield: with their round from `sizing::haul_round`, n haulers of
/// c CARRY parts move at least what the miner harvests in that time.
#[test]
fn the_haulers_carry_the_miners_rate() {
    let energy = capacity_at(mineral_level());
    for work in [1, 8, 18, 24] {
        for cost in [4, 20, 60] {
            let trip = Trip {
                tiles: cost / 2,
                cost,
            };
            let (body, n) = hauler_body(work, trip, energy, 0).expect("haulers");
            assert!(body.cost() <= energy);
            let carry = body.count(Part::Carry) as u32;
            assert!(carry * CARRY_CAPACITY <= CONTAINER_CAPACITY);
            let round = haul_round(&body, trip, CONTAINER_CAPACITY).expect("a round");
            let moved = u64::from(n * carry * CARRY_CAPACITY) * u64::from(ENERGY_REGEN_TIME);
            let mined = u64::from(rate_capacity(work)) * u64::from(round);
            assert!(moved >= mined, "work {work}, cost {cost}: {n} x {carry}");
        }
    }
}

// ---- Why a room mines ----

fn seen() -> Seen {
    let home = room(HOME);
    Seen {
        time: 0,
        level: mineral_level(),
        capacity: capacity_at(mineral_level()),
        kind: Some(KIND),
        mineral: pos(home, MINERAL.0, MINERAL.1),
        amount: 1000,
        extractor: true,
        container: Some(pos(home, CONTAINER.0, CONTAINER.1)),
        container_stock: 0,
        container_used: 0,
        terminal: Some(pos(home, TERMINAL.0, TERMINAL.1)),
        terminal_stock: 0,
        terminal_free: TERMINAL_CAPACITY,
    }
}

fn plan() -> MineralPlan {
    let home = room(HOME);
    let trip = Trip {
        tiles: 10,
        cost: 20,
    };
    let energy = capacity_at(mineral_level());
    let miner = miner_body(energy, MINERALS_MMO.max_work, trip);
    let (hauler, haulers) = hauler_body(18, trip, energy, 0).expect("haulers");
    MineralPlan {
        version: 1,
        capacity: energy,
        spawn: pos(home, SPAWN.0, SPAWN.1),
        container: pos(home, CONTAINER.0, CONTAINER.1),
        terminal: pos(home, TERMINAL.0, TERMINAL.1),
        max_work: MINERALS_MMO.max_work,
        margin_pct: 0,
        miner_trip: trip,
        haul_trip: trip,
        miner,
        hauler: Some(hauler),
        haulers,
    }
}

/// Each check of `why` in turn: with everything else in place, each missing fact is the reason.
#[test]
fn a_room_mines_only_with_everything_in_place() {
    let cfg = MINERALS_MMO;
    let p = plan();
    assert_eq!(why(&cfg, Some(&seen()), Some(&p)), Why::Mining);
    let off = MineralsConfig {
        enabled: false,
        ..cfg
    };
    assert_eq!(why(&off, Some(&seen()), Some(&p)), Why::Off);
    assert_eq!(why(&cfg, None, Some(&p)), Why::Unread);
    type Change = fn(&mut Seen);
    let cases: [(Change, Why); 9] = [
        (|s| s.level = mineral_level() - 1, Why::Level),
        (|s| s.extractor = false, Why::NoExtractor),
        (|s| s.container = None, Why::NoContainer),
        (|s| s.terminal = None, Why::NoTerminal),
        (|s| s.kind = None, Why::UnknownType),
        (|s| s.amount = 0, Why::Regenerating),
        (|s| s.terminal_stock = MINERALS_MMO.stock_max, Why::Stocked),
        (
            |s| s.terminal_free = MINERALS_MMO.free_min - 1,
            Why::TerminalFull,
        ),
        (|_| {}, Why::Mining),
    ];
    for (change, want) in cases {
        let mut s = seen();
        change(&mut s);
        assert_eq!(why(&cfg, Some(&s), Some(&p)), want);
    }
    // Just under the cap, it mines.
    let mut s = seen();
    s.terminal_stock = cfg.stock_max - 1;
    assert_eq!(why(&cfg, Some(&s), Some(&p)), Why::Mining);
    assert_eq!(why(&cfg, Some(&seen()), None), Why::NoPlan);
}

/// While it mines and a miner lives, the plan's haulers; with no miner yet, none for an empty
/// container; one for a container holding a full load the terminal has room for, mining or not;
/// none otherwise.
#[test]
fn haulers_follow_the_miner_and_the_leftover() {
    let p = plan();
    let load = p.load();
    assert!(load > 0);
    assert_eq!(haulers_wanted(Why::Mining, Some(&seen()), &p, 1), p.haulers);
    assert_eq!(haulers_wanted(Why::Mining, Some(&seen()), &p, 0), 0);
    let mut s = seen();
    s.container_stock = load - 1;
    assert_eq!(haulers_wanted(Why::Stocked, Some(&s), &p, 0), 0);
    s.container_stock = load;
    assert_eq!(haulers_wanted(Why::Regenerating, Some(&s), &p, 0), 1);
    assert_eq!(haulers_wanted(Why::Mining, Some(&s), &p, 0), 1);
    s.terminal_free = load - 1;
    assert_eq!(haulers_wanted(Why::Regenerating, Some(&s), &p, 0), 0);
}

// ---- The room's tick ----

/// A room that can mine is read on the tick after its first, plans, and asks for one miner at its
/// priority with the plan's exact body, and no hauler before a miner lives; with the miner
/// spawning, the plan's haulers; only on ticks the Standard tier runs.
#[test]
fn a_room_that_can_mine_asks_for_its_miner_and_haulers() {
    let mut w = World::new();
    assert!(w.reads.contains(&w.home()), "read on the next tick");
    w.standard = false;
    w.tick();
    assert!(
        w.requests().is_empty(),
        "no request without the Standard tier"
    );
    assert_eq!(w.room().why, Why::Mining);
    w.standard = true;
    w.tick();
    let plan = w.room().plan.expect("a plan");
    let req = w.requests();
    assert_eq!(req.len(), 1, "the miner only: {req:?}");
    assert_eq!(req[0].0, Role::MineralMiner);
    assert_eq!(req[0].1, plan.miner.expect("a miner"));
    assert_eq!(req[0].2, PRIORITY_MINERAL_MINER);
    // The miner is spawning: the haulers follow, one request a tick.
    let m = w.add(Role::MineralMiner, SPAWN.0, SPAWN.1, plan.work(), 0);
    w.creeps[m].spawning = true;
    w.tick();
    let req = w.requests();
    assert_eq!(req.len(), 1, "a hauler only: {req:?}");
    assert_eq!(req[0].0, Role::MineralHauler);
    assert_eq!(req[0].1, plan.hauler.expect("a hauler"));
    assert_eq!(req[0].2, PRIORITY_MINERAL_HAULER);
    // Below every other role's (the lowest so far is the signer's, 70).
    const {
        assert!(PRIORITY_MINERAL_MINER < PRIORITY_MINERAL_HAULER);
        assert!(PRIORITY_MINERAL_HAULER < crate::signing::PRIORITY_SIGNER);
    }
    // The upgraders' budget leaves the bodies out while it mines.
    assert_eq!(
        w.minerals.upkeeps(),
        &[(w.home(), plan.upkeep())][..],
        "the upkeep"
    );
}

#[test]
fn a_room_short_of_anything_asks_for_nothing() {
    let changes: [fn(&mut World); 6] = [
        |w| w.level = mineral_level() - 1,
        |w| w.container = false,
        |w| w.terminal = false,
        |w| w.amount = 0,
        |w| w.terminal_stock = MINERALS_MMO.stock_max,
        |w| w.cfg.enabled = false,
    ];
    for change in changes {
        let mut w = World::new();
        change(&mut w);
        w.tick();
        w.tick();
        assert!(w.requests().is_empty(), "{:?}", w.room().why);
        assert_ne!(w.room().why, Why::Mining);
        assert_eq!(w.minerals.upkeeps(), &[(w.home(), 0)][..]);
    }
}

/// The miner walks onto the container and harvests every `HARVEST_PERIOD` ticks, each harvest
/// `HARVEST_MINERAL_POWER` a WORK part into the container; never on cooldown (the model asserts
/// it) and never into a container that cannot take it.
#[test]
fn the_miner_harvests_once_a_period_into_the_container() {
    let mut w = World::new();
    w.tick();
    let work = w.room().plan.expect("plan").work();
    w.add(Role::MineralMiner, SPAWN.0 + 1, SPAWN.1, work, 0);
    for _ in 0..200 {
        w.tick();
    }
    let on = pos(w.home(), CONTAINER.0, CONTAINER.1);
    assert_eq!(w.creeps[0].at, on, "on the container");
    assert!(w.harvests.len() >= 2);
    for pair in w.harvests.windows(2) {
        assert_eq!(pair[1] - pair[0], HARVEST_PERIOD, "{:?}", w.harvests);
    }
    assert_eq!(
        w.stock,
        w.harvests.len() as u32 * work * HARVEST_MINERAL_POWER
    );
    // A container with no room for a harvest stops it.
    w.stock = CONTAINER_CAPACITY - work * HARVEST_MINERAL_POWER + 1;
    let before = w.harvests.len();
    for _ in 0..(2 * HARVEST_PERIOD) {
        w.tick();
    }
    assert_eq!(w.harvests.len(), before, "no harvest past the container");
}

/// An empty hauler waits next to the container for a full load, withdraws the mineral, carries
/// it to the terminal and transfers it all.
#[test]
fn the_hauler_takes_full_loads_to_the_terminal() {
    let mut w = World::new();
    w.tick();
    let h = w.add(Role::MineralHauler, CONTAINER.0 + 1, CONTAINER.1, 0, 4);
    // One WORK part, so its harvest leaves the container short of the hauler's load.
    w.add(Role::MineralMiner, CONTAINER.0, CONTAINER.1, 1, 0);
    // A tick for the room's read to be asked for (it has creeps now).
    w.tick();
    let load = 4 * CARRY_CAPACITY;
    w.stock = load - 2;
    w.cooldown = 0;
    w.tick();
    assert!(
        w.intents()
            .iter()
            .any(|i| matches!(i, CreepIntent::Harvest { .. })),
        "the miner harvests: {:?}",
        w.intents()
    );
    assert!(
        !w.intents()
            .iter()
            .any(|i| matches!(i, CreepIntent::WithdrawObject { .. })),
        "no withdraw short of a load while the miner harvests: {:?}",
        w.intents()
    );
    // No miner on the container: anything left is taken.
    w.creeps.pop();
    w.stock = load - 1;
    w.tick();
    assert_eq!(w.creeps[h].other, load - 1, "takes the leftover");
    w.stock = 0;
    for _ in 0..60 {
        w.tick();
    }
    assert_eq!(w.creeps[h].other, 0);
    assert_eq!(w.terminal_stock, load - 1);
    assert_eq!(w.terminal_free, TERMINAL_CAPACITY - (load - 1));
}

/// Over a long run with the model's spawn, the room fills its terminal to the stock cap and
/// stops: the harvests come every `HARVEST_PERIOD` ticks, the terminal ends at or above the cap,
/// and nothing is lost on the way.
#[test]
fn a_room_mines_up_to_the_stock_cap_and_stops() {
    let mut w = World::new();
    w.cfg.stock_max = 3000;
    w.spawn_creeps = true;
    let start = w.amount;
    for _ in 0..4000 {
        w.tick();
        if w.room().why == Why::Stocked && w.stock == 0 && w.creeps.iter().all(|c| c.other == 0) {
            break;
        }
    }
    assert_eq!(w.room().why, Why::Stocked);
    assert!(w.terminal_stock >= w.cfg.stock_max);
    let carried: u32 = w.creeps.iter().map(|c| c.other).sum();
    assert_eq!(
        start - w.amount,
        w.terminal_stock + w.stock + carried,
        "nothing lost"
    );
    let miners = w
        .creeps
        .iter()
        .filter(|c| c.role == Role::MineralMiner)
        .count();
    assert_eq!(miners, 1, "one miner");
    for pair in w.harvests.windows(2) {
        assert!(pair[1] - pair[0] >= HARVEST_PERIOD);
    }
}

/// The reads: every tick with a mineral creep; with none, once `minerals.read_ticks` has passed;
/// with the switch off, only rooms with creeps.
#[test]
fn a_room_is_read_while_it_has_creeps_or_when_due() {
    let mut w = World::new();
    w.tick();
    assert!(!w.reads.contains(&w.home()), "read last tick; not due");
    let mut gap = 1;
    while !w.reads.contains(&w.home()) {
        w.tick();
        gap += 1;
        assert!(gap <= w.cfg.read_ticks + 1);
    }
    assert_eq!(gap, w.cfg.read_ticks);
    w.add(Role::MineralHauler, CONTAINER.0 + 1, CONTAINER.1, 0, 4);
    for _ in 0..5 {
        w.tick();
        assert!(w.reads.contains(&w.home()), "with a creep, every tick");
    }
    w.creeps.clear();
    w.cfg.enabled = false;
    for _ in 0..(2 * w.cfg.read_ticks) {
        w.tick();
        assert!(!w.reads.contains(&w.home()), "off and no creep: no read");
    }
}

/// The read ticks among the next `n`.
fn reads_over(w: &mut World, n: u32) -> u32 {
    let mut got = 0;
    for _ in 0..n {
        w.tick();
        got += u32::from(w.reads.contains(&w.home()));
    }
    got
}

/// A room's creeps that cannot act do not hold it to a read every tick (the hot-path review,
/// 2026-11-26): a miner still spawning, an idle miner after a stop with nothing left to carry, and
/// any creep on ticks the Standard tier does not run; a hauler carrying the mineral does.
#[test]
fn reads_follow_creeps_that_can_act() {
    let n = 3 * MINERALS_MMO.read_ticks;
    let cadence = n.div_ceil(MINERALS_MMO.read_ticks) + 1;
    // A spawning miner.
    let mut w = World::new();
    w.tick();
    let m = w.add(Role::MineralMiner, SPAWN.0, SPAWN.1, 1, 0);
    w.creeps[m].spawning = true;
    assert!(reads_over(&mut w, n) <= cadence, "a spawning miner");
    // An idle miner on the container of a stocked room, nothing in the container.
    let mut w = World::new();
    w.terminal_stock = MINERALS_MMO.stock_max;
    w.tick();
    w.add(Role::MineralMiner, CONTAINER.0, CONTAINER.1, 1, 0);
    assert!(reads_over(&mut w, n) <= cadence, "an idle miner");
    assert_eq!(w.room().why, Why::Stocked);
    // A hauler carrying the mineral far from the terminal: every tick.
    let h = w.add(Role::MineralHauler, 40, 40, 0, 4);
    w.creeps[h].other = CARRY_CAPACITY;
    w.tick();
    assert_eq!(reads_over(&mut w, 3), 3, "a carrying hauler");
    // A mining room's miner, with the Standard tier off: the cadence only.
    let mut w = World::new();
    w.tick();
    w.add(Role::MineralMiner, SPAWN.0 + 1, SPAWN.1, 1, 0);
    w.tick();
    w.standard = false;
    assert!(reads_over(&mut w, n) <= cadence, "the Standard tier off");
    w.standard = true;
    w.tick();
    assert_eq!(reads_over(&mut w, 3), 3, "mining again with the tier on");
}

/// A room dropped from the module (no extractor, no mineral creep) sets its upkeep to 0 on the
/// tick it is dropped, so the upgraders' budget does not keep the mineral creeps' bodies.
#[test]
fn a_dropped_room_costs_the_upgraders_nothing() {
    let mut w = World::new();
    w.tick();
    let plan = w.room().plan.expect("a plan");
    assert_eq!(w.minerals.upkeeps(), &[(w.home(), plan.upkeep())][..]);
    w.extractor = false;
    w.tick();
    assert!(w.minerals.rooms().is_empty());
    assert_eq!(w.minerals.upkeeps(), &[(w.home(), 0)][..]);
}

/// No plan (and so no flow field) is built for a room that cannot mine, unless its container
/// holds a hauler's load: a stocked room plans only once a leftover shows.
#[test]
fn no_plan_is_built_for_a_room_that_cannot_mine() {
    let mut w = World::new();
    w.terminal_stock = MINERALS_MMO.stock_max;
    w.tick();
    assert_eq!(w.room().why, Why::Stocked);
    assert!(w.room().plan.is_none(), "no plan for a stocked room");
    w.stock = CONTAINER_CAPACITY;
    for _ in 0..=MINERALS_MMO.read_ticks {
        w.tick();
    }
    assert!(w.room().plan.is_some(), "a plan for the leftover's hauler");
}

/// Starting and stopping are events once each.
#[test]
fn starting_and_stopping_are_logged_once() {
    let mut w = World::new();
    w.tick();
    assert_eq!(
        w.minerals.events(),
        &[MineralEvent::Started {
            room: w.home(),
            kind: KIND,
            amount: w.amount,
        }][..]
    );
    w.tick();
    assert!(w.minerals.events().is_empty());
    w.add(Role::MineralHauler, CONTAINER.0 + 1, CONTAINER.1, 0, 4);
    // A tick for the room's read to be asked for (it has a creep now).
    w.tick();
    assert!(w.minerals.events().is_empty());
    w.terminal_stock = w.cfg.stock_max;
    w.tick();
    assert_eq!(
        w.minerals.events(),
        &[MineralEvent::Stopped {
            room: w.home(),
            why: Why::Stocked,
        }][..]
    );
}
