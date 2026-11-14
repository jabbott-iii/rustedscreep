//! A scripted intent producer for the act layer's sandbox test (`--features act-test`; run by
//! `js/tools/act-sim.mjs`, which `npm run check` runs). It is compiled only into that test build
//! (`npm run stage` refuses it for deploy/): a default build has no intent producer besides the
//! console `say` until P2 adds jobs.
//!
//! Every tick it queues, for the first two own creeps that are not spawning (by id, so the roles do
//! not depend on row order, V102), one intent of each variant the act layer issues, a replacement,
//! intents the priority table blocks, and one push for each refusal reason the sandbox world can
//! produce on that tick: all but `closed`, each as soon as the world has what it needs. The
//! harness's scene puts every target in range except one far extension, and checks the JS calls and
//! the `rustedscreep.act()` lines against this script.

use rustedscreep_core::intents::{CreepIntent, IntentBuffer, SayText, Target};
use rustedscreep_core::scheduler::Commit;
use rustedscreep_core::snapshot::{CreepHandle, ObjectKey, Snapshot, StructureHandle};
use screeps::{Direction, ResourceType, StructureType};

/// The script's memory: a creep it saw earlier, to target once it is gone.
#[derive(Default)]
pub struct Script {
    earlier: Option<CreepHandle>,
}

impl Script {
    pub fn queue(&mut self, buf: &mut IntentBuffer, snap: &Snapshot, commit: &mut Commit) {
        let creeps = &snap.creeps;
        let mut own: Vec<(ObjectKey, CreepHandle)> = Vec::new();
        let mut spawning = None;
        let mut hostile = None;
        for ((&h, &my), &sp) in creeps
            .handles()
            .iter()
            .zip(creeps.my())
            .zip(creeps.spawning())
        {
            match (my, sp) {
                (true, false) => {
                    if let Some(key) = creeps.key_of(h) {
                        own.push((key, h));
                    }
                }
                (true, true) => spawning = spawning.or(Some(h)),
                (false, _) => hostile = hostile.or(Some(h)),
            }
        }
        own.sort_unstable_by_key(|&(key, _)| key);
        let (Some(&(_, a)), Some(&(_, b))) = (own.first(), own.get(1)) else {
            return;
        };
        let (Some(spawn), Some(controller)) = (
            structure(snap, StructureType::Spawn),
            structure(snap, StructureType::Controller),
        ) else {
            return;
        };
        let far = structure(snap, StructureType::Extension);
        let energy = ResourceType::Energy;
        let earlier = self.earlier;
        let earlier_live = earlier.is_none_or(|h| creeps.row_of(h).is_some());
        let mut push = |actor, intent| {
            // Refusals are part of the script; the buffer counts them.
            let _ = buf.push(snap, actor, intent, commit);
        };
        // a: every variant but MoveToward, a replaced move, and heal blocking rangedHeal and repair
        // (V44). The say is public, so `true` crosses the boundary too.
        push(
            a,
            CreepIntent::Say {
                text: SayText::new("act test"),
                public: true,
            },
        );
        push(
            a,
            CreepIntent::Move {
                direction: Direction::Top,
            },
        );
        push(
            a,
            CreepIntent::Move {
                direction: Direction::Right,
            },
        );
        push(a, CreepIntent::Heal { target: b });
        push(a, CreepIntent::RangedHeal { target: b });
        push(a, CreepIntent::Repair { target: spawn });
        push(
            a,
            CreepIntent::Transfer {
                target: Target::Structure(spawn),
                resource: energy,
                amount: None,
            },
        );
        push(
            a,
            CreepIntent::Drop {
                resource: energy,
                amount: Some(5),
            },
        );
        push(
            a,
            CreepIntent::Withdraw {
                target: spawn,
                resource: energy,
                amount: Some(10),
            },
        );
        push(a, CreepIntent::UpgradeController { target: controller });
        push(a, CreepIntent::Pull { target: b });
        // b: rangedHeal blocking repair (the harness refuses the rangedHeal on even ticks, so the
        // repair is issued then), and a move toward a.
        push(b, CreepIntent::RangedHeal { target: a });
        push(b, CreepIntent::Repair { target: spawn });
        push(b, CreepIntent::MoveToward { target: a });
        // Refused, each replacing nothing: an amount of 0, a transfer to the controller (wrong
        // target), and one to the far extension (not in range).
        push(
            a,
            CreepIntent::Drop {
                resource: energy,
                amount: Some(0),
            },
        );
        push(
            a,
            CreepIntent::Transfer {
                target: Target::Structure(controller),
                resource: energy,
                amount: None,
            },
        );
        if let Some(far) = far {
            push(
                a,
                CreepIntent::Transfer {
                    target: Target::Structure(far),
                    resource: energy,
                    amount: None,
                },
            );
        }
        // Refused when the world has them: a spawning actor and a heal of it, a hostile actor, and a
        // creep seen earlier but gone now, as a target and as an actor.
        if let Some(s) = spawning {
            push(
                s,
                CreepIntent::Say {
                    text: SayText::new("never"),
                    public: false,
                },
            );
            push(a, CreepIntent::Heal { target: s });
        }
        if let Some(h) = hostile {
            push(
                h,
                CreepIntent::Move {
                    direction: Direction::Top,
                },
            );
        }
        if let Some(gone) = earlier.filter(|_| !earlier_live) {
            push(a, CreepIntent::RangedHeal { target: gone });
            push(
                gone,
                CreepIntent::Move {
                    direction: Direction::Top,
                },
            );
        }
        if earlier_live {
            self.earlier = Some(b);
        }
    }
}

fn structure(snap: &Snapshot, kind: StructureType) -> Option<StructureHandle> {
    let st = &snap.structures;
    st.kinds()
        .iter()
        .position(|&k| k == kind)
        .and_then(|row| st.handles().get(row).copied())
}
