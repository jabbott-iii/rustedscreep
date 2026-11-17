//! Creep bodies (ROADMAP P2, the minimal spawn path): a fixed array of at most `MAX_CREEP_SIZE`
//! parts with its cost and spawn time, and the scaled bodies the spawn path can size from a room's
//! energy ([`Body::scaled`]). Since the P3 body solver (P3.6, `economy::sizing`) no job asks for a
//! scaled pattern: every body is solved from its targets and requested exactly.

use std::fmt;

use rustedscreep_consts::{CREEP_SPAWN_TIME, MAX_CREEP_SIZE, Part};

/// Most parts a body holds (`MAX_CREEP_SIZE`, V170).
const MAX: usize = MAX_CREEP_SIZE as usize;

/// The parts this build knows, or `None` for the crate's hidden `__Invalid` variant, whose cost the
/// crate gives as 0 and which the engine refuses (V170, V178). The wildcard is required: the
/// wasm-bindgen enum cannot be matched exhaustively outside its crate (V104).
pub fn known_part(part: Part) -> Option<Part> {
    use Part::*;
    match part {
        Move | Work | Carry | Attack | RangedAttack | Tough | Heal | Claim => Some(part),
        _ => None,
    }
}

/// A creep body: 1 to `MAX_CREEP_SIZE` known parts, in the order they are spawned (the engine
/// damages a body front first). Every constructor checks that, so a `Body` is always one the
/// engine's `spawnCreep` accepts (V170).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Body {
    parts: [Part; MAX],
    len: u8,
}

impl Body {
    /// The body of `parts`, or `None` when it is empty, longer than `MAX_CREEP_SIZE` or holds an
    /// unknown part.
    pub fn new(parts: &[Part]) -> Option<Self> {
        if parts.is_empty() || parts.len() > MAX {
            return None;
        }
        let mut body = Self {
            parts: [Part::Move; MAX],
            len: 0,
        };
        for &part in parts {
            body.push(known_part(part)?)?;
        }
        Some(body)
    }

    /// The largest body of whole repeats of `pattern` that costs at most `energy`, has at most
    /// `max_repeats` repeats and at most `MAX_CREEP_SIZE` parts; `None` when not even one repeat
    /// fits, or `pattern` is empty or holds an unknown part. Each part of the pattern is repeated in
    /// place, so `[WORK, CARRY, MOVE]` three times is `WORK×3, CARRY×3, MOVE×3`.
    ///
    /// Uses: `BODYPART_COST` (`Part::cost`, V131, V170) and `MAX_CREEP_SIZE`.
    pub fn scaled(pattern: &[Part], energy: u32, max_repeats: u8) -> Option<Self> {
        let unit = Self::new(pattern)?;
        let unit_cost = unit.cost();
        if unit_cost == 0 {
            return None;
        }
        let by_energy = energy / unit_cost;
        let by_size = u32::try_from(MAX / pattern.len()).unwrap_or(0);
        let repeats = by_energy.min(by_size).min(u32::from(max_repeats));
        if repeats == 0 {
            return None;
        }
        let mut body = Self {
            parts: [Part::Move; MAX],
            len: 0,
        };
        for &part in pattern {
            for _ in 0..repeats {
                body.push(part)?;
            }
        }
        Some(body)
    }

    fn push(&mut self, part: Part) -> Option<()> {
        let slot = self.parts.get_mut(usize::from(self.len))?;
        *slot = part;
        self.len += 1;
        Some(())
    }

    pub fn parts(&self) -> &[Part] {
        self.parts.get(..usize::from(self.len)).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Always false: a body has at least one part.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Parts of type `part`.
    pub fn count(&self, part: Part) -> usize {
        self.parts().iter().filter(|&&p| p == part).count()
    }

    /// What the spawn charges for it: the sum of the parts' `BODYPART_COST` (`utils.calcCreepCost`,
    /// V170). Unit: energy.
    ///
    /// Uses: `BODYPART_COST` (`Part::cost`).
    pub fn cost(&self) -> u32 {
        self.parts()
            .iter()
            .fold(0u32, |sum, p| sum.saturating_add(p.cost()))
    }

    /// Ticks the spawn takes for it: `CREEP_SPAWN_TIME` per part (create-creep.js L49, V170). A
    /// spawn under `PWR_OPERATE_SPAWN` is faster; no power creep runs before P9. Unit: ticks.
    ///
    /// Uses: `CREEP_SPAWN_TIME`.
    pub fn spawn_time(&self) -> u32 {
        u32::from(self.len).saturating_mul(CREEP_SPAWN_TIME)
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.parts()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Part::*;
    use rustedscreep_consts::{SPAWN_ENERGY_CAPACITY, StructureType, extension_energy_capacity};

    const WORKER: [Part; 3] = [Work, Carry, Move];

    #[test]
    fn new_accepts_only_what_spawn_creep_accepts() {
        assert_eq!(Body::new(&[]), None, "empty");
        assert_eq!(
            Body::new(&[Move; MAX + 1]),
            None,
            "longer than MAX_CREEP_SIZE"
        );
        assert_eq!(Body::new(&[Move, Part::__Invalid]), None, "unknown part");
        let full = Body::new(&[Tough; MAX]).expect("MAX_CREEP_SIZE parts");
        assert_eq!(full.len(), MAX);
        let one = Body::new(&[Claim]).expect("one part");
        assert_eq!((one.len(), one.parts()), (1, &[Claim][..]));
        assert!(!one.is_empty());
    }

    #[test]
    fn known_parts_pass_and_the_hidden_variant_does_not() {
        for part in [Move, Work, Carry, Attack, RangedAttack, Tough, Heal, Claim] {
            assert_eq!(known_part(part), Some(part));
        }
        assert_eq!(known_part(Part::__Invalid), None);
    }

    #[test]
    fn cost_and_spawn_time_come_from_the_constants() {
        let body = Body::new(&[Work, Work, Carry, Move, Heal]).expect("body");
        assert_eq!(
            body.cost(),
            2 * Work.cost() + Carry.cost() + Move.cost() + Heal.cost()
        );
        assert_eq!(body.spawn_time(), 5 * CREEP_SPAWN_TIME);
        assert_eq!(body.count(Work), 2);
        assert_eq!(body.count(Attack), 0);
    }

    #[test]
    fn scaled_repeats_each_part_in_place() {
        let unit: u32 = WORKER.iter().map(|p| p.cost()).sum();
        let body = Body::scaled(&WORKER, 3 * unit, u8::MAX).expect("three repeats");
        assert_eq!(
            body.parts(),
            [Work, Work, Work, Carry, Carry, Carry, Move, Move, Move]
        );
        assert_eq!(body.cost(), 3 * unit);
    }

    /// The room energy levels a spawn path sees: a lone spawn, and a spawn with every RCL's full set
    /// of extensions (CONTROLLER_STRUCTURES, EXTENSION_ENERGY_CAPACITY), and every amount from 0 up
    /// to the largest in steps of 7, in ascending order.
    fn energies() -> Vec<u32> {
        let mut caps = vec![SPAWN_ENERGY_CAPACITY];
        for rcl in 1..=8 {
            let count = StructureType::Extension.controller_structures(rcl);
            let each = extension_energy_capacity(rcl);
            caps.push(SPAWN_ENERGY_CAPACITY + count * each);
        }
        let top = caps.iter().copied().max().expect("caps");
        let mut out: Vec<u32> = (0..=top).step_by(7).collect();
        out.extend(caps);
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Property test over patterns, energies and repeat caps: a scaled body costs at most the
    /// energy, has at most `MAX_CREEP_SIZE` parts and `max_repeats` repeats, holds only the pattern's
    /// parts in its proportions, cannot take one more repeat, and grows with the energy.
    #[test]
    fn scaled_bodies_are_the_largest_that_fit() {
        let patterns: [&[Part]; 5] = [
            &WORKER,
            &[Work, Work, Move],
            &[Carry, Carry, Move],
            &[Tough, Attack, Move, Move],
            &[Claim, Move],
        ];
        for pattern in patterns {
            let unit: u32 = pattern.iter().map(|p| p.cost()).sum();
            for cap in [1u8, 2, 5, 16, u8::MAX] {
                let mut last = 0;
                for energy in energies() {
                    let Some(body) = Body::scaled(pattern, energy, cap) else {
                        assert!(energy < unit, "{pattern:?} at {energy}: one repeat fits");
                        continue;
                    };
                    let repeats = body.len() / pattern.len();
                    assert_eq!(body.len() % pattern.len(), 0);
                    assert!(body.cost() <= energy);
                    assert!(body.len() <= MAX_CREEP_SIZE as usize);
                    assert!(repeats <= usize::from(cap));
                    assert_eq!(body.cost(), unit * repeats as u32);
                    for &p in pattern {
                        let per = pattern.iter().filter(|&&q| q == p).count();
                        assert_eq!(body.count(p), per * repeats);
                    }
                    let one_more = body.len() + pattern.len() <= MAX
                        && repeats < usize::from(cap)
                        && body.cost() + unit <= energy;
                    assert!(!one_more, "{pattern:?} at {energy}: not maximal");
                    assert!(body.len() >= last, "monotone in energy");
                    last = body.len();
                }
            }
        }
    }

    #[test]
    fn scaled_refuses_what_cannot_spawn() {
        assert_eq!(Body::scaled(&[], 10_000, 1), None);
        assert_eq!(Body::scaled(&[Part::__Invalid], 10_000, 1), None);
        assert_eq!(Body::scaled(&[Move; MAX + 1], 1_000_000, 1), None);
        assert_eq!(Body::scaled(&WORKER, 10_000, 0), None);
        let unit: u32 = WORKER.iter().map(|p| p.cost()).sum();
        assert_eq!(Body::scaled(&WORKER, unit - 1, 1), None);
    }
}
