# Releasing GameLobbies

This page is for maintainers. It explains how signed releases and in-app updates work.

GameLobbies updates itself from the releases of this repository, [m72b/GameLobbies](https://github.com/m72b/GameLobbies/releases) (called m72b/GameLobby up to 0.9.0; see [The repository rename](#the-repository-rename)). Each release holds the signed installer, its signature and the update manifest (`latest.json`). The app reads `releases/latest/download/latest.json`, shows when a new version is available, and installs only after a click. It accepts an installer only when its minisign signature matches the public key in `src-tauri/tauri.conf.json` and is bound to the announced version.

To cut a release:

1. Bump `version` in `package.json` (the app and the installer take their version from it) and in `src-tauri/Cargo.toml` to the same value, then commit. The release workflow refuses a tag that doesn't match both.
2. Tag the commit with the same version and push the tag. An annotated tag's message becomes the release notes.
   ```bash
   git tag -a v0.4.0 -m "Release notes shown in the app"
   git push origin v0.4.0
   ```
3. `.github/workflows/release.yml` then runs three jobs:
   - **build**: checks that the tag matches `package.json` and builds the unsigned installer. It has no secrets, and `npm ci --ignore-scripts` keeps dependency install scripts from running.
   - **sign**: runs in the `release` environment, the only place the signing key is available. It installs the Tauri CLI version locked in `package-lock.json` into an empty folder, checks it against the lockfile, and signs the installer with the version bound to the signature. It writes `latest.json`, then checks the signature against the public key in `tauri.conf.json` with `.github/scripts/verify-minisign.mjs`, an independent check that uses Node's own crypto.
   - **publish-here**: creates release `v0.4.0` in this repository with the installer, its `.sig` and `latest.json`, using the built-in `GITHUB_TOKEN` (write access in this job only). Pre-release versions such as `1.0.0-rc.1` are published as pre-releases and are never offered as updates.

   All actions are pinned to commit SHAs. The Rust version comes from `rust-toolchain.toml`. `src-tauri/tauri.release.conf.json` turns on `createUpdaterArtifacts` for a signed build in one step, for example a local test with a throwaway key; CI signs separately instead.

Secrets:

| Secret | Where | Contents |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Environment `release` | The updater private key (from `tauri signer generate`) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Environment `release` | Its password |
| `RELEASES_TOKEN` | Repository | Not needed: releases are published to this repository. Only if `RELEASE_REPO` names another repository: a fine-grained personal access token with **Contents: read and write** on that repository only. |

Set up the `release` environment once, in **Settings → Environments**:

1. Create the environment `release` and add the two signing secrets to it. If they were added as repository secrets earlier, delete those afterwards, so only the sign job can read them.
2. Add yourself as a **required reviewer**: each release waits for your approval before anything is signed.
3. Under **Deployment branches and tags**, choose **Selected branches and tags** and add the tag rule `v*`.
4. Under **Settings → Rules → Rulesets**, add a tag ruleset for `v*` that restricts who can create, update and delete those tags, for example to repository admins only.

## The repository rename

The app was called Game Lobby and its repository `m72b/GameLobby` up to 0.9.0. The repository is now `m72b/GameLobbies`. GitHub redirects the old name (web pages, git, the API and release downloads) to the new one, as long as no repository named `GameLobby` exists on this account again.

**Never create a repository named `GameLobby` on this account** (not even by renaming another one to it). It would end the redirect, and every installed copy of 0.4.0 to 0.9.0 would stop finding updates, with no way to reach them.

What installed copies of 0.4.0 to 0.9.0 do, and why releases stay readable for them:

- They read `https://github.com/m72b/GameLobby/releases/latest/download/latest.json`. GitHub redirects it to the new repository, and the updater follows redirects (it already follows the one from `releases/latest/download` to GitHub's file storage).
- They accept the installer only when the URL **written in `latest.json`** is `https://github.com/` followed by exactly `/m72b/GameLobby/releases/download/` (case-sensitive). That check runs on the URL in the manifest, not on where a redirect leads. So `latest.json` keeps writing installer URLs under the old name: `DOWNLOAD_REPO: m72b/GameLobby` in `release.yml`, used by `.github/scripts/updater-manifest.mjs`. A URL under `m72b/GameLobbies` would be refused by those copies.
- The installer's file name changed with the product name (`GameLobbies_<version>_x64-setup.exe` instead of `GameLobby_<version>_x64-setup.exe`). Installed copies don't check it: they check the URL prefix above, the minisign signature over the file and the version bound in the signature.
- The GameLobbies installer finds the old Game Lobby install, removes it in update mode (the library, settings and app data stay; they are in `%APPDATA%\app.gamelobby.desktop`, which did not change) and installs to `%LOCALAPPDATA%\GameLobbies` (`src-tauri/installer/hooks.nsh`).

What new builds do:

- `plugins.updater.endpoints` in `src-tauri/tauri.conf.json` lists the new repository first and the old name second. The updater tries them in order, so updates keep working before the rename is done and if one of them fails.
- Installers are accepted under the release path of either endpoint's repository, so the old-name URLs in `latest.json` are accepted too (`download_prefixes` in `src-tauri/src/updater.rs`).
- Keep `DOWNLOAD_REPO` on the old name as long as copies of 0.9.0 or older may still be installed. Switching it to the new name would cut them off.

Tests that tie this together: `updater_config_points_at_the_public_releases_repo` and `manifest_urls_under_the_old_name_pass_old_and_new_clients` in `src-tauri/src/updater.rs`, and `.github/scripts/updater-manifest.test.mjs` (run with `node --test ".github/scripts/*.test.mjs"`).

## Changing the release repository

The repository name is set in these places; the tests above check that they agree:

1. `github_repo!` in `src-tauri/src/lib.rs` (links and the User-Agent contact URL).
2. `RELEASE_REPO` at the top of `.github/workflows/release.yml` (`m72b/GameLobbies`, the repository the workflow runs in, so it publishes with the built-in `GITHUB_TOKEN`; the workflow compares it with `GITHUB_REPOSITORY`). For another repository, the `publish` job uses `RELEASES_TOKEN` instead.
3. The first entry of `plugins.updater.endpoints` in `src-tauri/tauri.conf.json`.
4. Links outside the app: `LAUNCH_OPTIONS_HELP` in `src/shared/routes.ts`, `package.json`, the README and the docs.

Installed copies keep checking the endpoints they were built with until they install a build with new ones. When you switch to a repository that the old one does not redirect to, publish the first release that uses the new endpoint to both repositories: to the old one, so existing installs find it, and to the new one. For example, run the workflow once with each `RELEASE_REPO`.

Never commit the private key or its password, and don't store them in this repository. If the key is lost, installed copies can no longer be updated automatically. If it leaks, anyone holding it can sign an update those copies will accept.
