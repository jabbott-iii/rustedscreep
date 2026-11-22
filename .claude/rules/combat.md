---
paths:
  - "crates/rustedscreep-core/src/defense/**"
  - "crates/rustedscreep-core/src/offense/**"
  - "crates/rustedscreep-core/src/combat/**"
  - "crates/rustedscreep-core/src/intents/**"
---

# Combat, defense and offense logic

## Intent pipeline (design around the verified table)
- Encode the engine's intent-priority table as data in `rustedscreep_core::intents`, with the engine commit SHA in a comment, and test it.
  An intent is an action that a creep (a player's unit) or a structure orders during a tick (one game turn). The table lists which
  intents block which on the same creep.
- Verified table (V44 to V47, read from the engine; it supersedes the seeded finding this rule began with):
  `heal` is never blocked. `dismantle` is blocked by `attackController`, `rangedHeal`, and `heal`.
  `attack` is blocked by `build`, `repair`, `dismantle`, `attackController`, `rangedHeal`, and `heal`. `rangedAttack` is blocked by `rangedMassAttack`, `build`, `repair`, and
  `rangedHeal`. Move, transfer, withdraw, pickup, and drop are outside the table. A heal on a target at full hit points (HP) still blocks.
- Consequences to respect:
  - A breaker (a siege creep of TOUGH/WORK/MOVE body parts that dismantles enemy structures) never self-heals while it dismantles.
  - Melee roles pick either attack or heal each tick.
  - Pre-healing is a deliberate choice that costs that tick's attack or dismantle.

## Damage model (verify every rule against engine source before coding)
- Tower damage, heal, and repair by range, with falloff (less power beyond the optimal range). Use constants only.
- Damage application order across body parts, and TOUGH boost reduction applied in body order (a boost is a lab compound that
  strengthens one body part; a boosted TOUGH part takes less damage).
- How damage and heal from multiple sources combine within one tick. The simulator must cite the engine file and commit it mirrors.
- Boost multipliers per compound and tier come from the constants table (`BOOSTS`), not from prose.

## Safety and targeting
- Never target allied or whitelisted players (config). Check the whitelist at target selection *and* before every intent.
- Skip rooms where attacks are impossible: active safe mode (a room controller's temporary protection against attackers),
  novice/respawn protection (walled start areas for new and respawned players), and any other verified restriction.
  Handle these cases gracefully: drop the target and log it once.
- Exclude the controller (the structure a player claims to own the room) and defensive structures unless they block the route.

## State machines and budgets
- Offense phases: stage → boost → approach → breach → clear → (claim | leave). Each phase is an explicit state
  with entry and exit conditions and a tested abort path. Abort or retreat when expected loss exceeds the configured threshold.
- Offense has its own energy and CPU budget in the Standard/Optional tiers (CPU priority levels below Critical, which stop as the
  bucket of saved CPU runs low). It never starves Critical work, and its
  spawn priority is below economy and defense.

## Siege breaker specifics
- The solver outputs body, boost list, expected time-to-kill, and expected survival time. Body order is TOUGH first, then WORK, with MOVE last.
  The MOVE count comes from the fatigue rules (moving builds fatigue by terrain and body weight, and MOVE parts remove it) for the
  planned route's terrain mix. The TOUGH count comes from the measured tower count and damage at the route's ranges.
- "Nearest economy structure" means the lowest real path cost from the creep, with enemy ramparts (protective structures) and walls
  treated as dismantle obstacles, not straight-line range. Economy structures are spawn, extension, link, storage, terminal, container, lab,
  factory, and power spawn. Add hysteresis on retargeting (a margin that keeps the creep from flipping between targets) and re-plan
  when the situation changes.
- Retreat rules (survival time below time-to-objective, boosts gone, no payoff) and the "no regrets" retreat threshold come from config.
- Log a telemetry record for every sortie (one breaker's attack run): energy and boost cost against damage dealt.
