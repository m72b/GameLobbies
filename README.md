<p align="center">
  <img src="src-tauri/icons/128x128.png" width="96" height="96" alt="Game Lobby logo">
</p>

<h1 align="center">Game Lobby</h1>

<p align="center">
  A fast, free and open-source game library and launcher for Windows.<br>
  All your games, from every store, emulator and folder, in one place.
</p>

<p align="center">
  <a href="https://github.com/m72b/GameLobby/actions/workflows/ci.yml"><img src="https://github.com/m72b/GameLobby/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/m72b/GameLobby/releases"><img src="https://img.shields.io/github/package-json/v/m72b/GameLobby?label=version" alt="Version"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078d4" alt="Platform: Windows 10 and 11">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri%202-dea584" alt="Built with Rust and Tauri 2">
</p>

---

## About

Game Lobby brings the games you already own into one library: Steam, Epic, GOG, Xbox and other store games, your own PC games, and emulator ROM folders. It shows them as a cover-art grid with search and filters, launches each one through the right store, emulator or launcher, and keeps track of your playtime, all on your own PC.

Game Lobby is **my first Rust project**. It started as an Electron app and was rebuilt with a Rust backend on [Tauri 2](https://tauri.app/), which made it much lighter: the installer is about 5 MB instead of 125 MB, and it uses less memory. I'm still learning Rust, so feedback, issues and pull requests are very welcome.

> **Bring your own games.** Game Lobby does not include, download or link to games, ROMs, BIOS or firmware files, or emulators. It only finds and launches what you have installed or own yourself.

## Highlights

**One library**
- Finds installed games from Steam, Epic Games, GOG, Xbox / Microsoft Store, EA app, Battle.net, Ubisoft Connect, Amazon Games and itch.io automatically.
- Scans your own game folders and emulator folders: 23 known emulators and 24 systems, with ROMs matched to the right emulator.
- Custom launchers with your own name, icon and colour, and games folders that are scanned for games and emulators.
- Search, store filters, favourites, a recently added shelf, a storage view with install sizes, and a library health check for missing files and duplicates.

**Rich game pages**
- Cover art, heroes and logos from SteamGridDB, including animated art.
- Descriptions, genres, developers, ratings, screenshots and time to beat from IGDB.
- A screenshot wall, your own score, notes and memories, and controller details.

**Playtime that stays yours**
- Tracks play sessions for games you launch and for store games you start elsewhere.
- Full play history with exact times, corrections, a journal per session, and sharing as text or Markdown.

**Built to be safe**
- Your library, playtime and settings stay on your PC. There is no account and no telemetry.
- API keys are encrypted with Windows DPAPI and never logged or shared.
- The interface can never choose which program Game Lobby runs: programs and folders come only from Windows file pickers, and new launch settings are confirmed in a Windows dialog.
- The library database is checked at every start and repaired automatically if it is damaged.
- Updates are signed and verified before they install.

## Installation

1. Download `Game Lobby_<version>_x64-setup.exe` from the [releases page](https://github.com/m72b/GameLobby/releases).
2. Run it. It installs for your Windows user only and needs no administrator rights.

Windows 10 and 11 already include the WebView2 runtime that Game Lobby uses.

The installer is not yet signed with a code-signing certificate, so Windows SmartScreen may show a warning the first time. Choose **More info → Run anyway** if you trust the download. App updates are signed separately and verified by the app before they install.

## Getting started

1. Open Game Lobby. Installed store games are found automatically on the first scan.
2. Add your own game and emulator folders in **Settings → Library folders**.
3. Optionally, add API keys for cover art and game details (see below).
4. Press **Play** on any game. Game Lobby minimizes while you play and records the session.

See the [user guide](docs/USER-GUIDE.md) for emulators, custom launchers, play history, themes and more.

## Optional API keys

Game Lobby works without any account. Three optional features use free keys that **you** create. They are stored on your PC, encrypted for your Windows user, and are never shared or included in backups. Game Lobby never ships a shared key.

| Feature | What you need | Where to enter it |
| --- | --- | --- |
| Cover art, heroes and logos | A free [SteamGridDB API key](https://www.steamgriddb.com/profile/preferences/api) | **Settings → Appearance → Cover art** |
| Game details, screenshots and time to beat | Your own [Twitch developer application](https://dev.twitch.tv/console) (Client ID and Client Secret) for IGDB | **Settings → Playtime and Stats → Metadata provider** |
| Steam games you own but haven't installed | A free [Steam Web API key](https://steamcommunity.com/dev/apikey) and your SteamID64, with your Steam profile's game details set to Public | **Settings → Libraries → Steam → Show games you own but haven't installed** |

Keep your keys private: don't paste them into issues, screenshots or logs. **Copy diagnostics** in **Settings → About and Updates** leaves them out.

## Supported stores

| Store | How games are found |
| --- | --- |
| Steam | Installed games from `appmanifest_*.acf` files; optionally also owned games that aren't installed, from the Steam Web API with your own key |
| Epic Games | Installed games from the Epic Games Launcher manifests |
| GOG | Owned games from the local GOG Galaxy database, with install state |
| EA app, Battle.net, Ubisoft Connect, Amazon Games | Installed games from Windows registry and uninstall records |
| itch.io | Installed games from the itch app folders |
| Xbox / Microsoft Store | Installed game packages (best effort) |

No store accounts or keys are needed. Game Lobby reads each launcher's local data and opens its own launch links; it never signs in to them. Each store can be turned off in **Settings → Libraries**.

## Privacy and data

- Everything is stored locally in `%APPDATA%\app.gamelobby.desktop`: the library database, cached artwork, screenshots and logs.
- Game Lobby only goes online for the features you use: SteamGridDB, IGDB, the Steam artwork CDN, themes you add, and update checks on GitHub.
- JSON backups include your games, settings, emulators, custom launchers and play history, but never API keys.

### Data sources and terms

Each service is used under its own terms:
- **SteamGridDB** artwork belongs to its creators and is for personal, non-commercial use under the [SteamGridDB Terms of Use](https://www.steamgriddb.com/terms).
- **IGDB** data is requested with your own Twitch application under the [Twitch Developer Services Agreement](https://www.twitch.tv/p/legal/developer-agreement/) and IGDB's terms, and is shown with an "IGDB.com" attribution.
- **Steam** store artwork belongs to Valve and the games' publishers.

Game Lobby does not contact HowLongToBeat or Backloggd; those buttons only open their search pages in your browser. Downloaded artwork is cached only on your PC and is never committed to this repository or bundled in the installer.

## Building from source

**Requirements:**
- Node.js 24;
- Rust via `rustup` (the toolchain version is pinned in `rust-toolchain.toml`);
- the Visual Studio C++ Build Tools.

```bash
npm install
npm run dev        # run the app with hot reload
npm test           # Rust unit tests
npm run test:web   # TypeScript tests
npm run build      # release build and NSIS installer
```

`npm run build` makes an unsigned installer in `src-tauri/target/release/bundle/nsis` and needs no signing key. Only the release workflow signs updates; see [docs/RELEASING.md](docs/RELEASING.md).

### Tech stack

| Part | Technology |
| --- | --- |
| Backend | Rust, Tauri 2, SQLite (rusqlite), reqwest, the Windows API |
| Interface | React 19, TypeScript, Vite, Tailwind CSS 4, running in WebView2 |
| Tests and CI | Rust unit tests, Node test runner, GitHub Actions |

### Project layout

| Path | Contents |
| --- | --- |
| `src-tauri/src/` | Rust backend: commands, database, scanners, launchers, play tracking |
| `src-tauri/src/importers/` | One importer per store |
| `src/renderer/` | React interface |
| `src/shared/` | Types and helpers shared by the interface, with their tests |
| `docs/` | User guide, release process and manual test checks |

## Contributing

Bug reports, fixes and new store integrations are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks and ground rules: no games, ROMs, artwork or secrets in the repository.

Please report security problems privately, as described in [SECURITY.md](SECURITY.md). Changes are listed in the [changelog](CHANGELOG.md).

## License

Game Lobby is licensed under the [GNU General Public License v3.0](LICENSE) (GPL-3.0-only). You may use, modify and redistribute it under that license; if you distribute a modified version, you must keep the same freedoms and share its source code. Third-party dependencies keep their own licenses.

## Trademarks

Game Lobby is an independent project. It is not affiliated with or endorsed by Valve, Epic Games, GOG, Microsoft, Ubisoft, Electronic Arts, Blizzard, Amazon, itch.io, Twitch, IGDB, SteamGridDB, Playnite, One Game Launcher, or any emulator project. Store names and logos are trademarks of their owners and are used only to identify the stores Game Lobby works with. Store icons come from [Simple Icons](https://simpleicons.org/) (CC0).

## Acknowledgements

- [Playnite](https://playnite.link/) inspired the idea of one library for every store. Game Lobby includes none of its code or assets.
- Cover art comes from the [SteamGridDB](https://www.steamgriddb.com/) community, and game details from [IGDB.com](https://www.igdb.com/).
- The [Quantico](https://fonts.google.com/specimen/Quantico) font by Matthew Desmond is bundled through [`@fontsource/quantico`](https://fontsource.org/fonts/quantico) under the [SIL Open Font License 1.1](https://openfontlicense.org/).
- The app icon uses letter shapes from [Roboto](https://fonts.google.com/specimen/Roboto) (Apache License 2.0).
