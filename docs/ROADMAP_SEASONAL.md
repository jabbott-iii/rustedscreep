# Roadmap and exit gates: Seasonal track (optional)

This track is optional (user decision, 2026-11-13; ADR-010, the architecture decision record, or ADR, that set up the roadmap tracks). It
builds on the finished MMO core track in `docs/ROADMAP.md` (the bot for the official MMO, the massively multiplayer online server): it starts
only after P12's gate (the exit criteria of the core's last phase, which finishes the debug visuals and the documents) has passed, and it never blocks the
core. Each phase names every phase it builds on. Close a phase with
`/phase-gate SE#`; passing it never changes the core's current phase. The evidence and checkbox conventions of the core track apply here as well,
with "live run" meaning the Seasonal server (an official server that runs the game in seasons, each with its own rules). Deploying to
Seasonal needs the user to name it (CLAUDE.md).

Each season changes the rules. screeps-game-api 0.23.5 (the Rust bindings to the game's API) has cargo features for some seasons only
(`seasonal-season-1`, `-2`, `-5`, and `-10`, VERIFICATION_LOG V2), so this track verifies the season running at that time before changing any code.

---

## SE1 Season verification (builds on P12)
Modules:
- [ ] Feature probe (a check of the WebAssembly, or wasm, features and JavaScript syntax the server accepts) on the Seasonal server, with results in VERIFICATION_LOG section D. Re-check ADR-002 and ADR-008 (the wasm features the build keeps and the loader's JavaScript target) against them.
- [ ] Check whether the screeps-game-api release then current has a feature for that season. If it has none, the bot uses its own `season`
  cargo feature, and any season-only API it needs goes through `rustedscreep-wasm` like any other binding.
- [ ] Verify that season's rules: season-specific objects, resources and scoring, the CPU rules on Seasonal (not found in P0, the core's kickoff research, V35), and any
  changed constants. Record each in VERIFICATION_LOG before coding against it.

**Gate:**
- [ ] Probe results are recorded for the Seasonal server.
- [ ] Whether a crate feature exists for the season is recorded.
- [ ] Every season rule the SE2 modules depend on is `verified` in VERIFICATION_LOG, or `unverified` with an assumption and a safe default.

## SE2 Season profile and modules (builds on SE1)
Modules:
- [ ] A `season` server profile in `config.rs` and `docs/TUNING.md`, and the deploy and capture path for the Seasonal server, chosen here in an ADR (there are no API tools and `/deploy` stops for `season`, ADR-021; a `season` entry in `.screeps.yaml`, in V93's screepers unified credentials format, only if that path reads one).
- [ ] Season-specific modules behind the season profile and the season cargo feature (the crate's, if one exists, otherwise our own).
  MMO modules are reused unchanged where the rules match.
- [ ] Cold-start cost measured on the Seasonal server, and the `season` profile's boot gate (the level of banked unused CPU, the bucket, that the loader waits for before it loads the wasm) set from it.

**Gate:**
- [ ] `/check` is green both with and without the season feature.
- [ ] A live run on the Seasonal server uses the season profile. Its CPU per tick (a tick is one game turn) and cold-start cost are recorded, and the `season`
  profile's boot gate is set from the measured cold start.
- [ ] MMO behavior is unchanged: the `mmo` profile's tests still pass.
