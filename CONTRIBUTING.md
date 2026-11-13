# Contributing to Game Lobby

Thanks for helping. Game Lobby is a free, open-source (GPL-3.0-only) Windows game library and launcher. Bug reports, fixes, new store importers, emulator profiles and documentation improvements are all welcome.

## Ground rules

- **No copyrighted game content.** Never add games, ROMs, BIOS or firmware files, disc images, proprietary emulator files, or game artwork to the repository, test fixtures, issues or pull requests. Tests create empty placeholder files instead.
- **No secrets.** Never commit API keys, tokens, signing keys or personal data. Game Lobby users supply their own credentials at runtime (see the README); the app must never embed shared secrets.
- **Report security problems privately.** See [SECURITY.md](SECURITY.md); don't open a public issue for them.
- Contributions are accepted under the project's license, GPL-3.0-only.

## Development setup

Requirements: Windows 10/11, Node.js 24, Rust via `rustup` (the toolchain is pinned in `rust-toolchain.toml`), and the Visual Studio C++ Build Tools. WebView2 ships with Windows.

```bash
npm install
npm run dev      # app with hot reload
```

`npm run dev` uses your real Game Lobby data folder. To develop against a throwaway library, set `GAME_LOBBY_DATA_DIR` to an absolute folder first (debug builds only). Close an installed copy of Game Lobby before running a dev build: both share the WebView2 profile.

## Before you open a pull request

Run the same checks as CI:

```bash
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd .. && npm run typecheck && npm run build:web
node --test .github/scripts/*.test.mjs
```

- Keep pull requests focused, and describe what changed and how you tested it.
- Add or update tests for behaviour changes, especially in path handling, launching, scanning and anything that touches the network.
- Match the existing code style; `cargo fmt` is authoritative for Rust.

## Areas that need extra care

These areas protect users' files and machines. Changes to them get a closer review:

- **Launching** (`src-tauri/src/launch.rs`, `platform.rs`): games and emulators are started without a shell, and each argument is passed separately. Keep it that way.
- **Library folders and scanning** (`roots.rs`, `localscan.rs`, `local.rs`): scans only read; they never rename, move or delete user files. Offline or unreadable folders must never cause games to be removed.
- **Network and downloads** (`covercache.rs`, `steamgriddb.rs`, `updater.rs`): HTTPS only, host allow-lists, size limits and content checks.
- **Secrets** (`secrets.rs`): credentials are encrypted with Windows DPAPI and never logged, returned to the UI, or placed in URLs or error messages.
- **The renderer** is treated as semi-trusted: new IPC commands must validate their input and must not let the UI run arbitrary programs or read arbitrary files.

## Reporting bugs and requesting features

Use the issue templates. For bugs, include your Windows version, the Game Lobby version (Settings → About and Updates), the steps to reproduce, and what you expected. Remove personal paths, account names and API keys from logs and screenshots.

Please don't ask for help finding or downloading games, ROMs or BIOS files; such issues will be closed.
