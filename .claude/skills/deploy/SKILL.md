---
name: deploy
description: Deploy the bot to a named Screeps server. For `mmo`, stage a checked build in `deploy/` for the user to push through the GitHub link (ADR-012). Other servers, and ADR-012's fallback, have no deploy path (ADR-021). Official servers only when the user names them in the current message.
argument-hint: "<server> [--dryrun] [--features <list>]"
disable-model-invocation: true
---

# /deploy $ARGUMENTS

1. **Server.** `$ARGUMENTS` must name a server. Official servers (`mmo`, `season`, `ptr`) require that the user's current message
   explicitly asks to deploy there. Otherwise stop and ask. Remind the user that deploying to an official server's active branch replaces
   the running code. For any server other than `mmo`, or for `mmo` when the GitHub link has failed (ADR-012's fallback), say there is
   no deploy path (ADR-021) and stop.
2. **Never read or print `.screeps.yaml`.** No tool reads it (ADR-021); never create it.
3. Run `/check`. If any check fails, stop and report. Deploy a failing build only if the user explicitly says so in this conversation,
   and state the risk.
   **Features.** `--features <list>` (comma-separated rustedscreep-wasm features, for example `profiler` or `trap-test`) stages that
   variant: pass `--features <list>` to `npm run stage` in steps 5 and 6. `npm run stage` refuses `act-test` for `deploy/` (sandbox only),
   and an unknown feature fails the build. `/check` builds and sandbox-tests each of `trap-test` (row 10) and `profiler` (row 12) alone;
   a combination is not, though the stage's build still runs the smoke test on it. Before staging, tell the user what the variant does
   live: `trap-test` arms `rustedscreep.trapTest(n)`, which forces traps, and makes the loader read Memory every tick; `profiler` adds six `getUsed` calls per loop and writes
   segment 99 once per 1000 loops. A feature build's ID ends in `+<feature>` for each feature.

## `mmo` through the GitHub link (ADR-012)
4. If STATUS says the link does not point at `deploy/` yet, tell the user: pushing `deploy/` will not deploy until the link is
   re-pointed, and re-pointing (Sync) overwrites the account's code at once with what `deploy/` holds on GitHub. Follow ADR-012's order.
5. With `--dryrun`: run `npm run stage -- --out <scratch directory> [--features <list>]` and report the module names, sizes, `.wasm`
   hash, features and build ID that would be staged. Leave `deploy/` untouched, and stop.
6. Otherwise run `npm run stage [-- --features <list>]`. Report the module names, sizes, `.wasm` hash, features and build ID, plus
   `git status --short -- deploy/` and `git diff --stat -- deploy/`. Record "staged" in `docs/STATUS.md` with the build ID, features and
   sizes. A feature build stays live until a plain build replaces it, so remind the user to stage a plain build when the measurement or
   test is done.
7. Tell the user that committing and pushing (any push to `main`) deploys `deploy/` to the live `default` branch. Also say that an open
   Steam client will offer to replace its local mirror: decline, and never edit files in the mirror. If the live build logged incidents
   (STATUS), say that its backoff streak in `Memory.rustedscreepLoader` carries over: once `rustedscreep.status()` shows the staged build ID,
   the user runs `delete Memory.rustedscreepLoader` in the console (earlier, the old build rebuilds it; ADR-015). Never commit or push. Stop here.
8. Only after the user confirms the push: ask for the console output of step 9, check that the running code logs the staged build ID
   (with its `+<feature>` suffixes) before using any output as evidence, and record "deployed" in `docs/STATUS.md` with the server,
   features, wasm and bundle sizes, and the observations.

## Evidence after a deploy
9. On the MMO the evidence for core gates is the user's console output (ADR-017, ADR-021). Ask for the boot line and the summaries the
   checks need (`rustedscreep.status()`, `rustedscreep.scheduler()` and the module's own), then report boot ticks, cold-start CPU, errors
   and the bucket. Record each observation with its `Game.time`. A deploy path for another server, or for the fallback, needs a new
   decision (an ADR).
