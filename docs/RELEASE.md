# Releases

A release is a version tag, `v<version>`, pushed to GitHub. The tag starts the release workflow
(`.github/workflows/release.yml`, ADR-034), which publishes one version in three places. It builds nothing:
it ships `deploy/` exactly as committed at the tag.

## What a release publishes

- **crates.io:** the workspace's three crates, `rustedscreep-consts`, `rustedscreep-core`, and `rustedscreep-wasm`, at
  the workspace's version. crates.io keeps every version it takes. A broken one can be yanked (hidden from new
  dependency resolution) but never deleted or replaced.
- **A GitHub release:** the files in `deploy/` and nothing else. These are `main.js` and the wasm's base64 text modules
  (`rustedscreep_wasm.js`, then `rustedscreep_wasm_1.js` and so on when needed; the wasm is compressed with DEFLATE
  before the base64, ADR-040, and ADR-028 splits the text across modules of at most 905709 bytes each). The release notes give the build ID from `main.js` and each file's SHA-256 sum.
- **The Steam Workshop of Screeps: World** (app 464350): the same files, as one Workshop item. That Workshop is "Server
  Mods & Bots": it holds bots and mods for private, self-hosted servers, not code for the MMO (V318).

### Why the release ships `deploy/` instead of a fresh build

Any push to `main` uploads `deploy/` to the MMO through the GitHub link (ADR-012). So `deploy/` at a commit on `main` is
the code the MMO received from that commit, and the release ships those same bytes.

A build in the workflow would need the pinned wasm-bindgen CLI and binaryen. Its build ID would also differ: `npm run
stage` names the commit it built from, plus `-dirty` for uncommitted changes. The tag itself deploys nothing, since
the GitHub link follows `main`, not tags.

### Before a private server runs it

Only the MMO's support for the wasm features this build uses has been probed (ADR-002, V72). A private server's Node
may reject them, so PS1 (the private track's first phase) probes a server before the bot is run there. Whether the
Steam client's private server loads these files as a Workshop bot is unverified (VERIFICATION_LOG §E).

## Making a release

1. **Raise the version.** In the root `Cargo.toml`, change `[workspace.package]`'s `version`, plus the `version` of
   `rustedscreep-consts` and of `rustedscreep-core` in `[workspace.dependencies]`. The published crates name each
   other by those versions. Run `npm run check`; its cargo commands also bring `Cargo.lock` up to date. The workflow
   builds with `--locked`, so a stale lock fails it instead of being rewritten.
2. **Stage a plain build** (one with no features). `npm run stage` writes it to `deploy/`. The workflow's gate refuses
   a feature build such as `+profiler`.
3. **Commit and push to `main`.** This is a live MMO deploy (ADR-012). Wait for the deploy checks (the `/deploy`
   skill, step 8) before tagging.
4. **Tag that commit and push the tag:**

   ```bash
   make release VERSION=v0.1.0
   ```

   The root `Makefile` first checks what the gate will, because the tag ruleset (below) keeps a pushed tag from being
   moved or deleted:
   - `VERSION` is `v` followed by HEAD's workspace version;
   - HEAD's `deploy/` holds a plain build;
   - HEAD is on the remote's `main`;
   - the remote does not already have the tag.

   `DRY_RUN=1` runs only the checks and prints the tag and the push. `REMOTE=` names a remote other than `origin`.
   Without `make`, the commands are:

   ```bash
   git tag -a v0.1.0 -m "rustedscreep 0.1.0"
   ```

   ```bash
   git push origin refs/tags/v0.1.0
   ```

A version with a hyphen, such as `v0.2.0-rc.1`, is a pre-release. GitHub marks its release that way and never makes it
"latest". crates.io publishes it as a pre-release version.

One release runs at a time. GitHub keeps only the newest run waiting behind it, so push the next tag once the
previous release has finished.

## The workflow

1. **`gate`** stops the run unless:
   - the tag is `v` plus `[workspace.package]`'s version;
   - its commit is on `main`;
   - and `deploy/` holds `main.js` and at least one `rustedscreep_wasm*.js`, with a build record (the `const BUILD`
     line that `npm run stage` writes) that names no features.

   Its outputs are the version, whether it is a pre-release, and the build ID.
2. **`check`** runs:
   - `cargo fmt --check`;
   - both clippy lints (native, and wasm with `-D warnings`);
   - the native tests;
   - and `node js/tools/smoke.mjs deploy base64`, which boots `deploy/`'s own files in the sandbox, as the GitHub link
     delivers them.

   Then it uploads `deploy/` for the later jobs. This is a subset of `npm run check`, which also builds the wasm and runs
   the tool tests and every sandbox sim. This repository has no CI workflow apart from CodeQL, so run the full gate
   before tagging.
3. **`crates`**, in the `crates-io` environment, runs `cargo publish --workspace --locked` with `CARGO_REGISTRY_TOKEN`.
   - cargo packages and builds every crate before it uploads any, then publishes them in dependency order: consts,
     then core, then wasm.
   - It goes first because crates.io refuses things no check here sees, such as a bad manifest field. A refusal then
     stops the release with nothing else out.
   - A crate whose version crates.io already has is left out, so a failed attempt can run again.
   - An attempt that crates.io stops with `status 429` (too many new crates; it takes 5 at once, then one every 10
     minutes) waits 10 minutes and tries again. One that times out waiting for the index waits 1 minute. Either way,
     at most 6 attempts.
4. Once every crate is on crates.io, two jobs run at the same time:
   - **`release`**, in the `release` environment with `contents: write`, makes the GitHub release as a draft with
     `deploy/`'s files, and then publishes it. That way the newest release's files are never half there. A draft
     left by a failed attempt is filled again; a published release is never changed.
   - **`workshop`**, in the `steam-workshop` environment, runs SteamCMD (Valve's command-line Steam client) in the
     `steamcmd/steamcmd:ubuntu-22` image, pinned by digest (V322).
     - It writes the account's saved login to `$HOME/Steam/config/config.vdf`, then runs
       `steamcmd +login <account> +workshop_build_item item.vdf +quit` with no terminal, so a login that wants a
       password or a code fails instead of waiting (V320).
     - `item.vdf` (V319) names app 464350, `deploy/`'s files as the item's content folder, and a change note with the
       version, the build ID, and the GitHub release's address.
     - With no `STEAM_WORKSHOP_ITEM_ID` variable, it makes a new item: private (visibility 2, V321), titled
       `rustedscreep`. It then prints the new id, which you store as that variable.
     - With the variable set, it updates that item's files and change note, leaving its title, description,
       visibility, and preview image as they are on Steam.
     - The login file is deleted when the step ends.

Re-run a failed job once its cause is fixed. A crate version crates.io has, a published GitHub release, and an
uploaded Workshop version cannot be taken back. The next version mends a broken release.

## Setting it up, once

The repository holds no secret. The workflow names only these:

| Name | Kind | Environment | What it is |
| :- | :- | :- | :- |
| `CARGO_REGISTRY_TOKEN` | secret | `crates-io` | A crates.io API token with the scopes `publish-new` and `publish-update`, limited to the crates `rustedscreep-*` |
| `STEAM_USERNAME` | secret | `steam-workshop` | The Steam account that uploads the Workshop item |
| `STEAM_CONFIG_VDF` | secret | `steam-workshop` | That account's SteamCMD `config.vdf`, gzip-compressed and base64-encoded, saved after a login with Steam Guard (Steam's two-factor check) |
| `STEAM_WORKSHOP_ITEM_ID` | variable | `steam-workshop` | The item's id. Leave it unset for the first release, which makes the item |

1. **Environments** (Settings → Environments): make `crates-io`, `release`, and `steam-workshop`. Under "Deployment
   branches and tags", allow only tags that match `v*`.
   - A required reviewer on `crates-io` makes the whole release wait for approval before anything is published,
     since the other two publishing jobs wait for the crates.
   - One on `release` or `steam-workshop` holds only that job.
2. **Who may tag** (Settings → Rules → Rulesets): add a tag ruleset for `v*` that lets only you create tags, and that
   blocks updating and deleting them. The gate is a step of the tagged commit's own workflow, so anyone who can push a
   `v*` tag can reach the environments' secrets.
3. **The crates.io token:** crates.io → Account Settings → API Tokens, with `publish-new` and `publish-update`, and
   the crates limited to `rustedscreep-*`. Store it as `CARGO_REGISTRY_TOKEN` in `crates-io`. Once the three crates
   exist, crates.io's trusted publishing (a short-lived token that crates.io issues to this workflow, with no stored
   secret) can replace it. Until then the token needs `publish-new`, since trusted publishing cannot create a crate.
4. **The Steam login,** on a trusted machine. Log in once with SteamCMD as the uploading account: `steamcmd +login
   <account> +quit`, with its password and Steam Guard code. Then store the file it saves, gzip-compressed and base64-encoded, as
   `STEAM_CONFIG_VDF`.
   - SteamCMD keeps the file in its Steam folder, as `config/config.vdf`. In the image the workflow uses, that is
     `$HOME/Steam/config/config.vdf` (V320). On Linux:

     ```bash
     gzip -c ~/Steam/config/config.vdf | base64 -w0
     ```

   - The file holds a login token for the whole Steam account, not only for uploads. Keep the tag ruleset of step 2.
   - The token can expire. A failed login in the `workshop` job means logging in again and replacing the secret.
   - It is not verified whether the account must own Screeps: World to upload to its Workshop (VERIFICATION_LOG §E).
5. **The first Workshop item:** the first release makes it private. Store the id it prints as the variable
   `STEAM_WORKSHOP_ITEM_ID` in `steam-workshop`, or the next release makes another item. On Steam, give the item a
   preview image and a description, choose its category tags, and set it public when ready.
6. **The SteamCMD image:** the digest in `release.yml` pins the `ubuntu-22` tag as of 2026-11-23 (V322). SteamCMD
   updates itself when it starts, so an older image still uploads with the current client. To move the pin, read the
   tag's current digest:

   ```bash
   curl -s https://hub.docker.com/v2/repositories/steamcmd/steamcmd/tags/ubuntu-22 | jq -r .digest
   ```
