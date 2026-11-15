# Releasing Game Lobby

This page is for maintainers. It explains how signed releases and in-app updates work.

Game Lobby updates itself from the releases of this repository, [m72b/GameLobby](https://github.com/m72b/GameLobby/releases). Each release holds the signed installer, its signature and the update manifest (`latest.json`). The app reads `releases/latest/download/latest.json`, shows when a new version is available, and installs only after a click. It accepts an installer only when its minisign signature matches the public key in `src-tauri/tauri.conf.json` and is bound to the announced version.

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

## Publishing to a different repository

The release repository is set in two places, plus a test that checks them:

1. `RELEASE_REPO` at the top of `.github/workflows/release.yml` (now `m72b/GameLobby`, the repository the workflow runs in, so it publishes with the built-in `GITHUB_TOKEN`). For another repository, the `publish` job uses `RELEASES_TOKEN` instead.
2. `plugins.updater.endpoints` in `src-tauri/tauri.conf.json`.
3. The config test `updater_config_points_at_the_public_releases_repo` in `src-tauri/src/updater.rs`.

Installed copies keep checking the old endpoint until they install a build that points to the new one. When you switch, publish the first release that uses the new endpoint to both repositories: to the old one, so existing installs find it, and to the new one. For example, run the workflow once with each `RELEASE_REPO`.

Never commit the private key or its password, and don't store them in this repository. If the key is lost, installed copies can no longer be updated automatically. If it leaks, anyone holding it can sign an update those copies will accept.
