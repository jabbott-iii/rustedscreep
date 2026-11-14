---
name: deploy
description: Deploy the bot to a named Screeps server profile. For `mmo`, stage a checked build in `deploy/` for the user to push through the GitHub link (ADR-012). For other servers, or the ADR-012 fallback, dry run and then upload through the API. Official servers only when the user names them in the current message.
argument-hint: "<server> [--dryrun]"
disable-model-invocation: true
---

# /deploy $ARGUMENTS

1. **Server.** `$ARGUMENTS` must name a profile. Official servers (`mmo`, `season`, `ptr`) require that the user's current message
   explicitly asks to deploy there. Otherwise stop and ask. Remind the user that deploying to an official server's active branch replaces
   the running code.
2. **Never read or print `.screeps.yaml`.** If the profile is missing, ask the user to add it. Do not create the file yourself.
3. Run `/check`. If any check fails, stop and report. Deploy a failing build only if the user explicitly says so in this conversation,
   and state the risk.

## `mmo` through the GitHub link (ADR-012)
4. If STATUS says the link does not point at `deploy/` yet, tell the user: pushing `deploy/` will not deploy until the link is
   re-pointed, and re-pointing (Sync) overwrites the account's code at once with what `deploy/` holds on GitHub. Follow ADR-012's order.
5. With `--dryrun`: run `npm run stage -- --out <scratch directory>` and report the module names, sizes, `.wasm` hash and build ID that
   would be staged. Leave `deploy/` untouched, and stop.
6. Otherwise run `npm run stage`. Report the module names, sizes, `.wasm` hash and build ID, plus `git status --short -- deploy/` and
   `git diff --stat -- deploy/`. Record "staged" in `docs/STATUS.md` with the build ID and sizes.
7. Tell the user that committing and pushing (any push to `main`) deploys `deploy/` to the live `default` branch. Also say that an open
   Steam client will offer to replace its local mirror: decline, and never edit files in the mirror. Never commit or push. Stop here.
8. Only after the user confirms the push: offer the capture in step 12, check that the running code logs the staged build ID before using
   any capture as evidence, and record "deployed" in STATUS with the observations.

## Other servers, or the `mmo` API fallback (ADR-012)
9. For the `mmo` fallback, first confirm that the user has unlinked the GitHub integration (or pointed it at a folder that is never
   pushed). Otherwise the next push overwrites the API upload. Do not run `npm run stage`.
10. Run `npm run deploy -- --server <server> --dryrun` and report what would be uploaded (module names and sizes).
11. Unless `--dryrun` was requested, run `npm run deploy -- --server <server>` (the permission prompt applies).
12. For any server (an official one only once step 1 is satisfied), offer to capture the first minutes with
    `npm run console -- --server <server>` and `npm run stats -- --server <server>` into `logs/`. Then report boot ticks, cold-start CPU,
    errors, and the bucket trend. On the MMO this capture is the evidence for core gates such as P1.
13. Record the deploy in `docs/STATUS.md`: server, wasm size, bundle size, and the observations made.
