---
name: deploy
description: Build and deploy the bot to a named Screeps server profile from .screeps.yaml, dry run first. Official servers only when the user names them in the current message.
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
4. Run `npm run deploy -- --server <server> --dryrun` and report what would be uploaded (module names and sizes).
5. Unless `--dryrun` was requested, run `npm run deploy -- --server <server>` (the permission prompt applies).
6. For a private server, offer to capture the first minutes with `npm run console -- --server <server>` and `npm run stats -- --server <server>`
   into `logs/`. Then report boot ticks, cold-start CPU, errors, and the bucket trend.
7. Record the deploy in `docs/STATUS.md`: server, wasm size, bundle size, and the observations made.
