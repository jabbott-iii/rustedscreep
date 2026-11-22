# Roadmap and exit gates: private-server track (optional)

This track is optional (user decision, 2026-11-13; ADR-010, the architecture decision record, or ADR, that set up the roadmap tracks). It
builds on the MMO core track in `docs/ROADMAP.md` (the bot for the official MMO, the massively multiplayer online server) and never blocks
it: no core gate (a phase's exit criteria, which pass only with evidence recorded in STATUS.md) needs anything from here. Each phase names every phase it builds on, core (P#) or private (PS#), and can start once all of
them have passed their gates. Close a phase with `/phase-gate PS#`. Passing a PS gate never changes the core's current phase. The
evidence and checkbox conventions of the core track apply here as well.

What it adds: a reproducible local server, cheap baseline comparisons, and the scripted and destructive scenarios that would be unsafe
or impossible on the live MMO (killing the colony's creeps, scripted attackers, dummy bases). Creeps are the bot's units.

"Private-server run" means screeps-launcher (a tool that installs and runs a private Screeps server) or an equivalent, with the result captured into `logs/` by a deploy and capture path that
PS1 chooses (the API tools were dropped, ADR-021). Deploy to the private server only when the user asks (CLAUDE.md).

---

## PS1 Private server setup (builds on P1)
Modules:
- [ ] screeps-launcher pinned to `v1.17.0` (Docker image or release binary, VERIFICATION_LOG V59). `nodeVersion` pinned to an exact
  `vX.Y.Z` string so runs are reproducible (V61: the default `nodeVersion` floats to the newest patch). Docker commands always ask first.
- [ ] `config.yml`, mods, world reset, and the bot user, written up in `docs/TEST_PLAN.md` §3 so the setup can be rebuilt from scratch.
- [ ] A `private` server profile in `config.rs` and `docs/TUNING.md`, and the deploy and capture path into `logs/` for the private server, chosen here in an ADR (there are no API tools, ADR-021; a `private` entry in `.screeps.yaml`, in V93's screepers unified credentials format, only if that path reads one).
  The profile handles an unlimited CPU setting, where `Game.cpu.limit` and `bucket` (the reserve of unused CPU) read `Infinity` (V37).
- [ ] Feature probe (a check of the WebAssembly, or wasm, features and JavaScript syntax the server accepts) on the private server, with results in VERIFICATION_LOG section D. Re-check ADR-002 and ADR-008 (the wasm features the build keeps and the loader's JavaScript target) against them.
- [ ] Cold-start cost measured on the private server, and the `private` profile's boot gate (the bucket level the loader waits for before it loads the wasm) set from it.

**Gate:**
- [ ] Probe results are recorded for the private server.
- [ ] The core bot's wasm loads and runs on the private server with the `private` profile.
- [ ] Cold-start cost on the private server is recorded, and the `private` profile's boot gate is set from it.
- [ ] The setup in TEST_PLAN §3 reproduces the server from an empty directory.

## PS2 Baselines (builds on PS1 and P2)
Modules:
- [ ] The unmodified screeps-starter-rust bot as a second reference for P1–P2 (the core's build-pipeline and bootstrap phases) behavior (cold start, empty-bot CPU, bootstrap ticks; a tick is one game turn).
- [ ] Naive-baseline (the same bot with every cache and optimization off) comparisons over long windows with the same seed, map, and tick window, which the MMO's 20 CPU free tier cannot afford.

**Gate:**
- [ ] CPU per tick for the core bot, the naive baseline, and the starter bot is recorded with the same seed, map, and tick window, with the method.

## PS3 Scripted scenarios (builds on PS1, and each scenario also on the core phase in brackets)
Modules (each is independent; run the ones that are useful):
- [ ] [P1] `trap-test`: 10 forced traps (wasm aborts, such as the one a Rust panic causes) in a row recover within the ADR-003 bound (in halt mode, the loop runs again on the third tick after a trap), without risk to a live colony.
- [ ] [P2] Zero-creep recovery: kill every creep at RCL2 (room controller level 2) and at RCL3, and the bot recovers with no manual action.
- [ ] [P6] A scripted invader in a remote room (one the bot mines without owning, P6's remote mining): the bot shows both auto-abandon and threat-driven evacuation.
- [ ] [P7] Scripted attacks (melee, ranged, and a healer duo) against a defended room: the defense holds or responds as the combat sim predicts.
- [ ] [P10] A scripted dummy base (towers, ramparts, economy structures, scripted defenders): every offense phase is logged, including at
  least one tested abort.
- [ ] [P11] A siege-breaker (one heavily armored creep that dismantles enemy structures) sortie against a dummy base with towers completes, with predicted ETK (expected time-to-kill) and survival logged next to what happened.

**Gate (per scenario, recorded as its own STATUS item; the scenario's box above is ticked when it passes):**
- The outcome stated in that scenario's bullet is observed.
- The scenario script is in `docs/TEST_PLAN.md`, its run is captured in `logs/`, and the result is recorded in STATUS.

## PS4 Automated server tests (builds on PS1)
Modules:
- [ ] A spike with screeps-server-mockup (a library that runs a private Screeps server from test code) installed from git commit 703645f (its Node 24 fix is not on npm, VERIFICATION_LOG V63).
- [ ] If the spike works: the PS3 scenarios completed so far become automated tests behind one local command. Later PS3 scenarios are added
  as they complete.

**Gate:**
- [ ] The spike result is recorded. Either the completed PS3 scenarios run automatically from one command, or the reason to drop the
  mockup is recorded.
