# Test plan (SPEC §6.4)

## 1. Unit and property tests (native, `cargo test -p rustedscreep-core -p rustedscreep-consts`)
_Per module._ The list of suites, what each asserts, and how expected values derive from constants.

## 2. Benchmarks (native, `cargo bench -p rustedscreep-core`)
_P2 / P4 / P7._ Planner, flow field, and combat sim, with the latest numbers and the machine used.

## 3. Private server
_P1._ screeps-launcher setup (Docker), config.yml mods, world reset, adding the bot user, deploy profile, and
capturing the console and stats to `logs/`. Scenario scripts: fresh bootstrap, zero-creep recovery, invader attacks, expansion.

## 4. CPU and bucket comparison against the naive baseline
_P2 onward._ Method: identical seed and map, the same tick window, the naive mode that runtime config toggles, mean and p95 CPU per tick,
and the bucket trend. Results go in a table per phase.

## 5. Offense tests against a scripted dummy base
_P10–P11._ Dummy base layout (towers, ramparts, economy structures), scripted defender behavior, success and abort criteria,
and the siege-breaker scenario with towers: predicted ETK and survival compared with what actually happened.

## 6. Loader robustness
_P1._ `trap-test` loop, aborted-tick simulation, compile failure, corrupt segments, and missing Memory.
