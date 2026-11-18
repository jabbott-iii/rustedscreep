//! Criterion benches for the base planner (P4.2, ADR-007), native only: each candidate's whole plan
//! over the 72-room corpus (fresh, with a standing spawn on open ground, and with one by the
//! controller where the room has ground for it, the harness's modes), the same on three rooms
//! (the most open, the median and the most cramped by wall share), and the shared pieces (the
//! distance transform, a minimum rampart cut). Run with
//! `cargo bench -p rustedscreep-core --bench planner`.

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rustedscreep_consts::Terrain;
use rustedscreep_core::planner::{PlanInput, Scratch, bunker, defence, distance_transform, flood};

use common::corpus::{self, CorpusRoom};
use common::harness::{self, PlanFn, protected_of};

const CANDIDATES: [(&str, PlanFn); 2] = [("bunker", bunker::plan), ("flood", flood::plan)];

fn wall_share(r: &CorpusRoom) -> usize {
    r.raw
        .iter()
        .filter(|&&b| b & Terrain::Wall as u8 != 0)
        .count()
}

/// Every room's terrain, then its inputs in both modes, built once outside the timed loops.
struct Rooms {
    rooms: Vec<CorpusRoom>,
    terrains: Vec<rustedscreep_core::movement::TerrainGrid>,
}

impl Rooms {
    fn load() -> Self {
        let rooms = corpus::load();
        let terrains = rooms.iter().map(CorpusRoom::terrain).collect();
        Self { rooms, terrains }
    }

    fn input(&self, i: usize, spawned: bool, s: &mut Scratch) -> PlanInput<'_> {
        let r = &self.rooms[i];
        let fresh = PlanInput {
            terrain: &self.terrains[i],
            controller: r.controller,
            sources: &r.sources,
            mineral: r.minerals.first().map(|&(_, t)| t),
            spawn: None,
        };
        if spawned {
            PlanInput {
                spawn: harness::synthetic_spawn(&fresh, s),
                ..fresh
            }
        } else {
            fresh
        }
    }
}

fn plans(c: &mut Criterion) {
    let rooms = Rooms::load();
    let mut s = Scratch::default();
    let fresh: Vec<PlanInput<'_>> = (0..rooms.rooms.len())
        .map(|i| rooms.input(i, false, &mut s))
        .collect();
    let spawned: Vec<PlanInput<'_>> = (0..rooms.rooms.len())
        .map(|i| rooms.input(i, true, &mut s))
        .collect();
    let clear: Vec<PlanInput<'_>> = fresh
        .iter()
        .filter_map(|f| {
            harness::clear_spawn(f, &mut s).map(|t| PlanInput {
                spawn: Some(t),
                ..*f
            })
        })
        .collect();
    let mut by_walls: Vec<usize> = (0..rooms.rooms.len()).collect();
    by_walls.sort_by_key(|&i| (wall_share(&rooms.rooms[i]), i));
    let picks = [
        ("open", by_walls[0]),
        ("median", by_walls[by_walls.len() / 2]),
        ("cramped", by_walls[by_walls.len() - 1]),
    ];

    let mut group = c.benchmark_group("plan");
    group.sample_size(20);
    for (name, plan) in CANDIDATES {
        group.bench_function(format!("{name}/corpus-fresh"), |b| {
            b.iter(|| {
                for input in &fresh {
                    let _ = black_box(plan(black_box(input), &mut s));
                }
            });
        });
        group.bench_function(format!("{name}/corpus-spawned"), |b| {
            b.iter(|| {
                for input in &spawned {
                    let _ = black_box(plan(black_box(input), &mut s));
                }
            });
        });
        group.bench_function(format!("{name}/corpus-clear-{}", clear.len()), |b| {
            b.iter(|| {
                for input in &clear {
                    let _ = black_box(plan(black_box(input), &mut s));
                }
            });
        });
        for (label, i) in picks {
            let room = &rooms.rooms[i];
            group.bench_function(format!("{name}/{label}-{}", room.name), |b| {
                b.iter(|| black_box(plan(black_box(&fresh[i]), &mut s)));
            });
        }
    }
    group.finish();

    let mut shared = c.benchmark_group("shared");
    let median = picks[1].1;
    shared.bench_function("distance-transform", |b| {
        let terrain = &rooms.terrains[median];
        let mut out = [0u8; rustedscreep_consts::ROOM_AREA];
        b.iter(|| {
            distance_transform(
                |t| terrain.kind(t) != rustedscreep_core::movement::TerrainKind::Wall,
                &mut out,
            )
        });
    });
    if let Ok(layout) = flood::plan(&fresh[median], &mut s) {
        let protect = protected_of(&layout);
        shared.bench_function("min-cut-flood-layout", |b| {
            b.iter(|| black_box(defence::plan(black_box(&fresh[median]), &protect, &mut s)));
        });
    }
    shared.finish();
}

criterion_group!(benches, plans);
criterion_main!(benches);
