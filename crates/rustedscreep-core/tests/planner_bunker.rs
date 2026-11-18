//! The bunker candidate (P4.2, ADR-007) on the whole corpus in the harness's three modes (fresh, a
//! standing spawn on open ground, one by the controller): every plan valid, the same plan again
//! with a fresh scratch, the same plan stepped one phase at a time with a new scratch each step,
//! and the measures (`cargo test --test planner_bunker -- --nocapture`); the cost of each step is
//! an ignored test.

mod common;

use common::harness;
use std::time::Instant;

use rustedscreep_core::planner::{Scratch, bunker};
use rustedscreep_core::scheduler::Step;

#[test]
fn every_room_gets_a_valid_plan() {
    harness::assert_valid(&harness::run_all(bunker::plan));
}

#[test]
fn plans_are_deterministic() {
    harness::assert_deterministic(bunker::plan);
}

#[test]
fn stepping_with_a_new_scratch_each_step_gives_the_same_plan() {
    harness::each(|room, mode, input| {
        let whole = bunker::plan(input, &mut Scratch::default());
        let mut run = bunker::Run::new(input);
        let stepped = loop {
            match run.step(&mut Scratch::default()) {
                Ok(Step::More) => continue,
                Ok(Step::Done) => break Ok(run.finish()),
                Err(e) => break Err(e),
            }
        };
        assert_eq!(whole, stepped, "{} {mode:?}", room.name);
    });
}

#[test]
fn measures() {
    println!(
        "{}",
        harness::summary("bunker", &harness::run_all(bunker::plan))
    );
}

/// The cost of each step (the Optional tier's time slices, P4.3), native and timed, so not part of
/// the gate: `cargo test --release -p rustedscreep-core --test planner_bunker -- --ignored
/// --nocapture`.
#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn step_costs() {
    let table = harness::step_costs("bunker", 3, bunker::plan, |input, s| {
        let mut run = bunker::Run::new(input);
        let mut out = Vec::new();
        loop {
            let phase = run.phase();
            let t0 = Instant::now();
            let r = run.step(s);
            out.push((phase, t0.elapsed().as_secs_f64() * 1e3));
            if r != Ok(Step::More) {
                break (out, r == Ok(Step::Done));
            }
        }
    });
    println!("{table}");
}
