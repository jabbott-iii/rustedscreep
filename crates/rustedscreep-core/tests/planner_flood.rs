//! The flood candidate (P4.2, ADR-007) on the whole corpus in the harness's three modes (fresh, a
//! standing spawn on open ground, one by the controller): every plan valid, the same plan again
//! with a fresh scratch, the same plan stepped one phase at a time with a new scratch each step,
//! and the measures (`cargo test --test planner_flood -- --nocapture`); the cost of each step is
//! an ignored test.

mod common;

use common::harness;
use std::time::Instant;

use rustedscreep_consts::{ROOM_AREA, StructureType};
use rustedscreep_core::movement::{TerrainKind, Tile};
use rustedscreep_core::planner::rules::earliest_level;
use rustedscreep_core::planner::{MAX_LEVEL, Scratch, flood, validate};
use rustedscreep_core::scheduler::Step;

#[test]
fn every_room_gets_a_valid_plan() {
    harness::assert_valid(&harness::run_all(flood::plan));
}

#[test]
fn plans_are_deterministic() {
    harness::assert_deterministic(flood::plan);
}

#[test]
fn stepping_with_a_new_scratch_each_step_gives_the_same_plan() {
    harness::each(|room, mode, input| {
        let whole = flood::plan(input, &mut Scratch::default());
        let mut run = flood::Run::new(input);
        let stepped = loop {
            match run.step(input, &mut Scratch::default()) {
                Ok(Step::More) => continue,
                Ok(Step::Done) => break Ok(run.finish()),
                Err(e) => break Err(e),
            }
        };
        assert_eq!(whole, stepped, "{} {mode:?}", room.name);
    });
}

/// ADR-033 on the whole corpus in every mode (a fresh start, a standing spawn on open ground, one
/// by the controller), so the wall policy holds for any room the bot plans, with no code for any
/// one room: every plan is valid (the validator's exits, reach and defense rules count walls as part
/// of the line, so every exit side the bare room reaches is still reached and every wall is built
/// and repaired from reached ground); no wall stands on a road, and each belongs from the first level
/// ramparts are allowed; every rampart stands over another planned structure, on a planned road, or
/// next to ground hostile creeps reach from the exits past the line (a gate: one per region of that
/// ground, or a tile the plan keeps clear, which the layout does not record). Prints each plan's
/// counts and the corpus' totals (2026-11-23: 214 plans, 1011 ramparts of which 338 gates off
/// roads, 4635 walls, where every one of the 5646 was a rampart before).
#[test]
fn every_line_is_walls_but_its_gates() {
    let first = earliest_level(StructureType::Rampart, 0).expect("ramparts");
    let (mut plans, mut ramparts, mut walls, mut gates) = (0u32, 0u32, 0u32, 0u32);
    harness::each(|room, mode, input| {
        let Ok(layout) = flood::plan(input, &mut Scratch::default()) else {
            return;
        };
        let what = format!("{} {mode:?}", room.name);
        plans += 1;
        assert!(validate(input, &layout).valid(), "{what}");
        let has =
            |t: Tile, k: StructureType| layout.items.iter().any(|i| i.tile == t && i.kind == k);
        let under = |t: Tile| {
            layout.items.iter().any(|i| {
                i.tile == t && !matches!(i.kind, StructureType::Road | StructureType::Rampart)
            })
        };
        for w in layout.of(StructureType::Wall) {
            assert!(
                !has(w.tile, StructureType::Road),
                "{what}: a wall on a road at {:?}",
                w.tile
            );
            assert_eq!(w.level, first, "{what}: {w:?}");
        }
        // The ground hostile creeps reach from the exits past the line, by region.
        let natural = |t: Tile| {
            t == input.controller || input.sources.contains(&t) || input.mineral == Some(t)
        };
        let open = |t: Tile| {
            input.terrain.kind(t) != TerrainKind::Wall
                && !natural(t)
                && !has(t, StructureType::Rampart)
                && !has(t, StructureType::Wall)
        };
        let mut region = vec![0u16; ROOM_AREA];
        let mut regions = 0u16;
        for i in 0..ROOM_AREA {
            let Some(start) = Tile::from_index(i) else {
                continue;
            };
            if !start.is_edge() || !open(start) || region[i] != 0 {
                continue;
            }
            regions += 1;
            region[i] = regions;
            let mut stack = vec![start];
            while let Some(t) = stack.pop() {
                for (_, n) in t.neighbors() {
                    if open(n) && region[n.index()] == 0 {
                        region[n.index()] = regions;
                        stack.push(n);
                    }
                }
            }
        }
        let mut off_road = 0u16;
        for r in layout.of(StructureType::Rampart) {
            if under(r.tile) || has(r.tile, StructureType::Road) {
                continue;
            }
            assert!(
                r.tile.neighbors().any(|(_, n)| region[n.index()] != 0),
                "{what}: the rampart at {:?} is no gate",
                r.tile
            );
            off_road += 1;
        }
        println!(
            "{what}: ramparts {}, walls {}, gates off roads {off_road}, regions {regions}",
            layout.count(StructureType::Rampart, MAX_LEVEL),
            layout.count(StructureType::Wall, MAX_LEVEL)
        );
        ramparts += layout.count(StructureType::Rampart, MAX_LEVEL);
        walls += layout.count(StructureType::Wall, MAX_LEVEL);
        gates += u32::from(off_road);
    });
    assert!(plans > 0);
    println!("{plans} plans: {ramparts} ramparts ({gates} gates off roads), {walls} walls");
}

#[test]
fn measures() {
    println!(
        "{}",
        harness::summary("flood", &harness::run_all(flood::plan))
    );
}

/// The cost of each step (the Optional tier's time slices, P4.3), native and timed, so not part of
/// the gate: `cargo test --release -p rustedscreep-core --test planner_flood -- --ignored
/// --nocapture`.
#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn step_costs() {
    let table = harness::step_costs("flood", 3, flood::plan, |input, s| {
        let mut run = flood::Run::new(input);
        let mut out = Vec::new();
        loop {
            let phase = run.phase();
            let t0 = Instant::now();
            let r = run.step(input, s);
            out.push((phase, t0.elapsed().as_secs_f64() * 1e3));
            if r != Ok(Step::More) {
                break (out, r == Ok(Step::Done));
            }
        }
    });
    println!("{table}");
}
