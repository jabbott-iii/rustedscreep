# Test plan (SPEC §6.4)

## 1. Unit and property tests (native, `cargo test -p rustedscreep-core -p rustedscreep-consts`)
_Per module._ The list of suites, what each asserts, and how expected values derive from constants.

## 2. Benchmarks (native, `cargo bench -p rustedscreep-core`)
_P2 / P4 / P7._ Planner, flow field, and combat sim, with the latest numbers and the machine used.

## 3. Private server (optional track, `docs/ROADMAP_PRIVATE.md`)
_PS1._ screeps-launcher setup (Docker), config.yml mods, world reset, adding the bot user, deploy profile, and
capturing the console and stats to `logs/`. Scenario scripts (PS3): zero-creep recovery, scripted invaders, scripted attacks, dummy bases. Core gates never depend on this section.

## 4. CPU and bucket comparison against the naive baseline
_P2 onward._ Core method (live MMO): short, bucket-gated windows that alternate optimized, naive and optimized at a comparable colony
state. Each window records its tick range, RCL, room count, creep count and intents. The window length and early-stop bucket level are TUNING
values (ROADMAP evidence conventions). PS2 method (optional private track): identical seed and map, the same tick window, the naive mode that runtime config toggles, mean and p95 CPU per tick,
and the bucket trend. Results go in a table per phase.

## 5. Offense tests against a scripted dummy base (optional track, PS3)
_PS3, after P10–P11._ Core evidence for P10–P11 is native scenario tests plus live sorties against user-approved targets. Dummy base layout (towers, ramparts, economy structures), scripted defender behavior, success and abort criteria,
and the siege-breaker scenario with towers: predicted ETK and survival compared with what actually happened.

## 6. Loader robustness
_P1._ `trap-test` loop, aborted-tick simulation, compile failure, corrupt segments, and missing Memory.

## 7. Native scenario tests (core)
_P2 onward._ Pure-Rust scenario tests in `rustedscreep-core` that stand in for in-game scenarios on the core track: zero-creep recovery
(P2), remote abandon and evacuation (P6), combat scenarios against tower and defender configurations (P7), offense state machine with an
abort (P10), and the siege scenario against simulated towers (P11). Expected outcomes derive from constants.

## 8. Live MMO checks (core)
_P1 onward._ What each core gate observes on the official MMO, how it is captured (`npm run console` and `npm run stats` into `logs/`),
and which checks need the user's approval first (live offense sorties in P10 and P11).
