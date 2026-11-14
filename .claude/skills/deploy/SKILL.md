---
name: deploy
description: Deploy the bot to a named Screeps server profile. For `mmo`, stage a checked build in `deploy/` for the user to push through the GitHub link (ADR-012). For other servers, or the ADR-012 fallback, dry run and then upload through the API. Official servers only when the user names them in the current message.
argument-hint: "<server> [--dryrun] [--features <list>]"
disable-model-invocation: true
---

# /deploy $ARGUMENTS

1. **Server.** `$ARGUMENTS` must name a profile. Official servers (`mmo`, `season`, `ptr`) require that the user's current message
   explicitly asks to deploy there. Otherwise stop and ask. Remind the user that deploying to an official server's active branch replaces
   the running code.
2. **Never read or print `.screeps.yaml`.** If the profile is missing, ask the user to add it. Do not create the file yourself.
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
   Steam client will offer to replace its local mirror: decline, and never edit files in the mirror. Never commit or push. Stop here.
8. Only after the user confirms the push: offer the capture in step 12, check that the running code logs the staged build ID (with its
   `+<feature>` suffixes) before using any capture as evidence, and record "deployed" in STATUS with the features and the observations.

## Other servers, or the `mmo` API fallback (ADR-012)
9. For the `mmo` fallback, first confirm that the user has unlinked the GitHub integration (or pointed it at a folder that is never
   pushed). Otherwise the next push overwrites the API upload. Do not run `npm run stage`.
10. Run `npm run deploy -- --server <server> --dryrun` and report what would be uploaded (module names and sizes). `--features` reaches this
    path only once the deploy tool (STATUS item 3b) takes it; until then, say so and stop if features were requested.
11. Unless `--dryrun` was requested, run `npm run deploy -- --server <server>` (the permission prompt applies).
12. For any server (an official one only once step 1 is satisfied), offer to capture the first minutes with
    `npm run console -- --server <server>` and `npm run stats -- --server <server>` into `logs/`. Then report boot ticks, cold-start CPU,
    errors, and the bucket trend. On the MMO this capture is the evidence for core gates such as P1.
13. Record the deploy in `docs/STATUS.md`: server, features, wasm size, bundle size, and the observations made.
