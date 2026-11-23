//! Criterion benches for the towers' combat sim (P7.2), native only: one hostile's fight against
//! six towers (`kill_ticks`, with and without a boosted TOUGH front), its effective hit points
//! (`effective_hits`), and a whole room's plan with six towers and ten hostiles (assessment and
//! tower choice). Run with `cargo bench -p rustedscreep-core --bench combat`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rustedscreep_consts::{
    Part, Position, ResourceType, RoomCoordinate, RoomName, StructureType, TOWER_CAPACITY,
    TOWER_ENERGY_COST, TOWER_POWER_ATTACK,
};
use rustedscreep_core::config::DEFENSE_MMO;
use rustedscreep_core::defense::{
    Shot, Threats, TowerControl, effective_hits, kill_ticks, tower_amount,
};
use rustedscreep_core::snapshot::{
    BodyPartRead, BodyRead, Controller, CreepRow, ObjectKey, Snapshot, StructureStatic, TickContext,
};

fn part(part: Part, boost: Option<ResourceType>) -> BodyPartRead {
    BodyPartRead { part, boost }
}

/// A 50-part body: boosted TOUGH in front (or plain), then RANGED_ATTACK, HEAL and MOVE.
fn body(boosted: bool) -> Vec<BodyPartRead> {
    let tough = boosted.then_some(ResourceType::CatalyzedGhodiumAlkalide);
    let mut parts = vec![part(Part::Tough, tough); 10];
    parts.extend(vec![part(Part::RangedAttack, None); 15]);
    parts.extend(vec![part(Part::Heal, None); 10]);
    parts.extend(vec![part(Part::Move, None); 15]);
    parts
}

fn at(x: u8, y: u8) -> Position {
    let room: RoomName = "W1N1".parse().expect("room");
    Position::new(
        RoomCoordinate::new(x).expect("x"),
        RoomCoordinate::new(y).expect("y"),
        room,
    )
}

fn combat(c: &mut Criterion) {
    let mut g = c.benchmark_group("combat");
    let towers: Vec<Shot> = (0..6)
        .map(|i| Shot {
            damage: tower_amount(TOWER_POWER_ATTACK, 4 + 3 * i),
            shots: TOWER_CAPACITY / TOWER_ENERGY_COST,
        })
        .collect();
    for boosted in [false, true] {
        let parts = body(boosted);
        let hits = 50 * 100;
        let name = if boosted {
            "kill-ticks-boosted"
        } else {
            "kill-ticks-plain"
        };
        g.bench_function(name, |b| {
            b.iter(|| {
                kill_ticks(
                    black_box(&parts),
                    hits,
                    hits,
                    black_box(&towers),
                    black_box(200),
                    DEFENSE_MMO.max_kill_ticks,
                )
            })
        });
    }
    let parts = body(true);
    g.bench_function("effective-hits-boosted", |b| {
        b.iter(|| effective_hits(black_box(&parts), black_box(4321)))
    });

    // A room with six towers and ten 50-part hostiles, boosted and not, around its center.
    let mut snap = Snapshot::default();
    snap.begin(TickContext::from_raw(1000, 20, 500.0, 10_000));
    snap.rooms
        .push("W1N1".parse().expect("room"), Controller::Mine { level: 8 });
    for i in 0..10u8 {
        let row = CreepRow {
            pos: at(20 + i, 25 + i % 3),
            my: false,
            spawning: false,
            fatigue: 0,
        };
        snap.creeps
            .push(ObjectKey(u128::from(i) + 1), row)
            .expect("hostile");
        let index = snap.creeps.len() - 1;
        snap.creeps.set_assessed(index, 50 * 100);
        snap.creeps
            .set_body(index, BodyRead::of(&body(i % 2 == 0)).expect("fits"));
    }
    for i in 0..6u8 {
        let handle = snap
            .structures
            .push(ObjectKey(1000 + u128::from(i)), || {
                Ok(StructureStatic {
                    kind: StructureType::Tower,
                    pos: at(10 + 6 * i, 10),
                })
            })
            .expect("tower");
        let row = u32::try_from(snap.structures.len() - 1).expect("small");
        snap.towers.push(handle, row, TOWER_CAPACITY);
    }
    snap.finish();
    let mut threats = Threats::default();
    let mut control = TowerControl::default();
    g.bench_function("room-plan-6-towers-10-hostiles", |b| {
        b.iter(|| {
            threats.assess(&snap, &DEFENSE_MMO);
            control.begin(&snap, &threats, &DEFENSE_MMO);
            for unit in 0..control.unit_count() {
                let _ = control.plan_room(unit, &snap, &threats, &DEFENSE_MMO);
            }
            control.finish(1000, &threats);
            black_box(control.orders().len())
        })
    });
    g.finish();
}

criterion_group!(benches, combat);
criterion_main!(benches);
