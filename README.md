# Game Lobby

**Game Lobby** is a local-first desktop app for bringing installed PC games and emulator libraries together in one browsable library. It pairs a cover-art grid with store-aware launch routes, local metadata, custom covers, collections, and a separate settings window.

Game Lobby is an independent community project with its own name, codebase, and roadmap. It is not the official One Game Launcher app and is not affiliated with or endorsed by its developers.

## What it offers

- A dark, responsive, virtualized poster library with search, store filters, game details, and launch actions.
- Local SQLite storage for games, launch routes, settings, emulator profiles, and theme data.
- Store library discovery with optional ownership imports for Steam, Epic, and GOG.
- Unified duplicate entries with a per-store “Play from” choice.
- Windows playtime tracking for launched games and sorting by last played or playtime.
- Custom cover management and JSON library backup and restore.
- A separate settings window for library sources, appearance, emulator profiles, and remote MyUI themes.
- A small native Rust backend (Tauri 2) behind a narrow, validated command API; the React UI runs in the system WebView2.

## Building

Requirements: Node.js 24, Rust (stable, MSVC toolchain via `rustup`), and the Visual Studio C++ Build Tools. Windows 10/11 ship the WebView2 runtime.

```bash
npm install
npm run dev      # app with hot reload
npm test         # Rust unit tests
npm run build    # release build + NSIS installer in src-tauri/target/release/bundle/nsis
```

## Library and emulator support

### Store libraries

The Windows scanners support these sources:

| Store | Current import method |
| --- | --- |
| Steam | Installed `appmanifest_*.acf` files; optional owned-game import through Steam Web API |
| Epic Games | Installed `.item` manifests; optional owned-game import through the Legendary CLI |
| GOG | Owned titles from the local GOG Galaxy database, with installed state when available |
| EA app, Battle.net, Ubisoft Connect, Amazon Games | Best-effort installed-game discovery from Windows registry and uninstall records |
| itch.io | Installed games found in the local itch app folders |
| Xbox / Microsoft Store | Best-effort installed game-package scan; package-name matching can miss games |

To import Steam ownership, enter a SteamID64 and Web API key in **Settings → Libraries**. Steam must make the profile's game details public for the API to return the library. The key is encrypted with Windows DPAPI for your user account. For Epic ownership, install and sign in to [Legendary](https://github.com/legendary-gl/legendary), then select its executable in the same settings page. GOG Galaxy is auto-detected when possible; otherwise, select its database file.

Scanning combines routes when their stable source and external app identity match. Games with the same title remain separate unless a source provides a shared identity; use **Link another game** in a game's detail panel to manually combine cross-store duplicates. This avoids merging unrelated games such as different releases with identical names. Open a game's detail panel to choose **Play from** a particular store. Installed discovery for the other listed stores is local-only; those integrations do not retrieve complete online account catalogs.

Windows playtime tracking looks for a running process inside the selected game's install folder while Game Lobby is open. It records elapsed time and last-played time locally. Routes without a usable install folder can still update last-played when launched, but cannot be process-tracked.

Emulators are configured as profiles and can be found automatically in emulator folders. See [Emulators and ROM library](#emulators-and-rom-library).

### Emulators and ROM library

Add library folders in **Settings → Library folders**: one or more **Game folders** and **Emulator folders**, typed as a path or chosen with **Browse**. Each folder can be enabled or disabled, rescanned, or removed. **Scan all** scans every folder, and the library **Scan** also runs a folder scan.

**Game folders** hold one subfolder per system, named for it, such as `PS3`, `Sony 3`, `PlayStation 3`, `Switch`, `PS1`, `Wii`, `GameCube`, `NES`, or `PC`. Names are matched loosely. Game Lobby supports 24 systems: PS1, PS2, PS3, PS4, PSP, PS Vita, GameCube, Wii, Wii U, Switch, 3DS, DS, GBA, GB/GBC, N64, SNES, NES, Genesis/Mega Drive, Dreamcast, Saturn, Xbox, Xbox 360, Arcade, and PC.

- Multi-file games count once. `.m3u`, `.cue`, `.gdi`, `.ccd`, and `.mds` files claim the files they list, but only files in the same folder or below it.
- Some games are folders rather than single files: PS3 (`PS3_GAME/USRDIR/EBOOT.BIN`, with the title read from `PARAM.SFO`), PS4 (`eboot.bin` with `sce_sys`), Wii U (`code/*.rpx`), and Xbox 360 (`default.xex`). Switch updates and DLC are skipped.
- Under **PC**, each subfolder is one game. The main `.exe` is chosen automatically, or the game goes to **Review** when the choice is unclear.
- Internet shortcuts (`.url`) are accepted only for supported store links, such as `steam:` and Epic links. A `.lnk` that points to a plain `.exe` is stored as that `.exe`. Any other `.lnk`, such as one with arguments or a non-`.exe` target, goes to **Review**, must be confirmed, and will not launch if it changes after confirmation.

**Add local game** in the library adds a single `.exe`, `.lnk`, or `.url` by browsing, using the same shortcut rules.

**Emulator folders** are searched up to four levels deep for known emulators: Dolphin, PCSX2, RPCS3, DuckStation, PPSSPP, Ryujinx, yuzu forks, Cemu, xemu, Xenia and Xenia Canary, melonDS, Azahar/Lime3DS/Citra, mGBA, Project64, Snes9x, Mesen, Flycast, Vita3K, shadPS4, and RetroArch. Emulators can also be added manually. Each profile has editable launch arguments with the placeholders `{rom}`, `{romdir}`, and `{romname}`. Quotes group text. If `{rom}` is missing, the ROM path is appended. RetroArch needs its core set in the arguments before it can be used. The default argument templates come from each emulator's documentation; edit them if your build differs. A removed emulator stays removed on rescan, and you can restore it from **Ignored emulators**. A second copy of an already-known emulator is added disabled.

Each ROM game uses the best matching enabled emulator, or the one you pick on its game page. Launching never uses a shell: each argument is passed separately, so file names with spaces, quotes, `&`, `%`, or `^` are safe. Playtime for emulator games is tracked for games started from Game Lobby, by the emulator process it launched.

Rescans are safe. Titles, favorites, covers, playtime, and emulator choices are kept, and nothing is duplicated. Possible duplicates go to **Settings → Review**, where you can keep them separate, link them, or dismiss them. Offline or unreadable folders never remove games. If a scan would remove more than half of a folder's games (and more than 20), it holds off and reports it. Unreadable subfolders and unsupported files are listed in the scan summary and in Review.

Folder rules: use full local paths only. System and program folders, network paths, and links (junctions) are refused. Folders cannot overlap each other, and emulator folders cannot be Downloads, Desktop, or Temp.

For development, debug builds honour `GAME_LOBBY_DATA_DIR` (an absolute folder) to use a separate data folder. Release builds ignore it.

## Extensions

Library integrations live in `src-tauri/src/importers/`. Each importer discovers games in the Rust backend and returns normalized game records with one or more launch routes. Scans are orchestrated in `src-tauri/src/scan.rs`.

Community integrations are intended to use declarative manifests for identity and metadata. A manifest does not make downloaded code safe: third-party JavaScript should not be downloaded and executed inside the renderer or main process.

```json
{
  "id": "example-library",
  "name": "Example Library",
  "version": "1.0.0",
  "kind": "library",
  "store": "Custom"
}
```

## MyUI themes

Game Lobby ships with a built-in dark fallback theme. Additional themes can be loaded from HTTPS JSON or token-only CSS URLs and applied without restarting the app. Theme values are validated design tokens; remote CSS is not injected as executable stylesheet content. A theme may define its name, colors, font, corner radius, spacing, hover effect, and an HTTPS background image. Loaded theme data is cached locally for offline use.

Theme JSON example: [`themes/example.json`](themes/example.json). CSS themes should define supported `--token-name` values inside a `:root` block.

## Local data and privacy

The SQLite database is managed by `src-tauri/src/db.rs` and is stored in `%APPDATA%\app.gamelobby.desktop`. On first launch, Game Lobby copies an existing library and custom covers from the earlier Electron build (`%APPDATA%\game-lobby`), leaving the original untouched; re-enter a Steam API key once, because keys encrypted by the Electron build can't be read. It uses write-ahead logging and foreign-key checks. JSON backups include game metadata, launch routes, settings, and emulator profiles. Only the optional Steam ownership import uses a credential entered in Game Lobby; the Steam API key is encrypted with Windows DPAPI. Epic ownership uses the separately installed Legendary client and its existing sign-in.

## License

Game Lobby is licensed under the GNU General Public License v3.0 only (GPL-3.0-only). You may use, modify, and redistribute the project under that license. When you distribute a modified version, GPL-3.0 requires you to preserve the same freedoms and provide the corresponding source code. See [`LICENSE`](LICENSE) for the complete terms. Third-party dependencies retain their own licenses.

## Project status

Game Lobby is an early-stage independent project. Steam, Epic, GOG, and best-effort local discovery for the listed Windows storefronts are available, with ownership coverage varying by source as described above. Broader account-catalog imports, richer metadata services, and executable plugin support remain future work.

## Acknowledgements

The project takes inspiration from the unified-library experience of [Playnite](https://playnite.link/) and the library presentation patterns of game storefronts. Game Lobby is an independent project and does not include or redistribute proprietary code or assets from those products.
