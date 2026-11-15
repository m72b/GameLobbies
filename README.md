# Game Lobby

**Game Lobby** is a free, open-source Windows app that brings the games you already have into one library: Steam, Epic, GOG, Xbox and other store games, plus your own PC games and emulator ROM folders. It shows them as a cover-art grid with search and filters, launches them from the right store or emulator, and tracks your playtime locally.

Game Lobby is local-first: your library, playtime, covers and settings stay on your PC. It is an independent community project with its own name, codebase and roadmap. It is not the official One Game Launcher app and is not affiliated with or endorsed by its developers.

> **Bring your own games.** Game Lobby does not include, download or link to games, ROMs, BIOS or firmware files, or emulators. It only finds and launches games and emulators you have installed or own yourself.

## What it offers

- A dark, responsive, virtualized poster library with search, store filters, favorites, collections and a Steam-like game page.
- Store library discovery for Steam, Epic, GOG, Xbox / Microsoft Store, EA app, Battle.net, Ubisoft Connect, Amazon Games and itch.io, with optional ownership imports for Steam, Epic and GOG.
- Emulator and ROM folders: about 35 known emulators detected automatically, 24 systems, safe rescans and a review list for anything unclear.
- Playtime tracking for games you launch, and for store games you start elsewhere.
- Cover art from SteamGridDB and Steam, cached locally, plus custom covers, heroes and logos.
- JSON library backup and restore, remote MyUI themes, and signed in-app updates.
- A small native Rust backend (Tauri 2) behind a narrow, validated command API; the React UI runs in the system WebView2.

## Install

Download the latest installer (`Game Lobby_<version>_x64-setup.exe`) from the [releases page](https://github.com/m72b/GameLobby-releases/releases) and run it. It installs for your Windows user only and needs no administrator rights. Windows 10 and 11 already include the WebView2 runtime it uses.

The installer isn't signed with a Windows code-signing certificate, so Windows SmartScreen may warn about an unrecognized app the first time; choose **More info → Run anyway** if you trust the download. App updates are separately signed and verified (see [Updates](#updates)).

## Optional accounts and API keys

Game Lobby works without any account: it detects installed games from your launchers' local files. These optional features use credentials that **you** create and enter yourself. Each is stored on your PC, encrypted with Windows DPAPI for your user account, and is never shared, sent anywhere other than the service it belongs to, or included in backups. Game Lobby never ships a shared key.

| Feature | What you need | Where to enter it |
| --- | --- | --- |
| Cover art from SteamGridDB | A free SteamGridDB account and API key from [steamgriddb.com/profile/preferences/api](https://www.steamgriddb.com/profile/preferences/api) | **Settings → Appearance → Cover art** |
| Game details from IGDB (description, genres, developers, publishers, ratings) | Your own Twitch developer application from [dev.twitch.tv/console](https://dev.twitch.tv/console): its **Client ID** and a **Client Secret** | **Settings → Playtime and Stats → Metadata provider** |
| Steam | Nothing: installed games are detected automatically | **Settings → Libraries** (turn the store on or off) |
| GOG | Nothing: GOG Galaxy's local database is detected automatically | **Settings → Libraries** (turn the store on or off) |

Keep your keys private: don't paste them into issues, screenshots or logs.

## Data sources and terms

Game Lobby talks to these services directly from your PC, using your own credentials where needed. By using a feature, you use that service under its own terms.

- **SteamGridDB** (cover art, heroes, logos): community-made artwork; copyright belongs to its creators. Use is limited to personal, non-commercial use under the [SteamGridDB Terms of Use](https://www.steamgriddb.com/terms). Game Lobby fetches art on demand, spaces out bulk requests, and only caches images on your PC.
- **IGDB** (game descriptions, genres, companies, ratings): game data from [IGDB.com](https://www.igdb.com/), requested with your own Twitch application under the [Twitch Developer Services Agreement](https://www.twitch.tv/p/legal/developer-agreement/) and IGDB's terms. Game Lobby shows the "Game data from IGDB.com" attribution wherever it displays this data, spaces requests to stay within IGDB's rate limit, and caches results only on your PC (refreshed after 30 days). The access token is kept in memory only.
- **IGDB images and time to beat**: screenshots and artwork are loaded only from `images.igdb.com`, from image ids that Game Lobby checks before building the address, and are cached on your PC like covers. Time-to-beat figures come from IGDB's own data. Game Lobby does not contact HowLongToBeat; **Search on HowLongToBeat** only opens its search page in your browser.
- **Steam** (store artwork CDN): artwork belongs to Valve and the games' publishers.
- **Local launchers** (Steam, Epic, GOG Galaxy, Xbox, EA app, Battle.net, Ubisoft Connect, Amazon Games, itch.io): Game Lobby reads their local install data and opens their own launch links; it doesn't sign in to them.

Downloaded artwork is cached only in your local data folder. It is never committed to this repository or bundled in the installer.

## Updates

Game Lobby can check for new versions in **Settings → About and Updates**, and optionally once a day on startup. An update is installed only when you click install, and only after its signature matches the public key built into the app and is bound to the announced version; older versions are refused. Your library is backed up before an update installs. Maintainers: see [docs/RELEASING.md](docs/RELEASING.md).

## Building from source

Requirements: Node.js 24, Rust via `rustup` (the toolchain is pinned in `rust-toolchain.toml`), and the Visual Studio C++ Build Tools.

```bash
npm install
npm run dev      # app with hot reload
npm test         # Rust unit tests
npm run build    # release build + NSIS installer in src-tauri/target/release/bundle/nsis
```

`npm run build` makes an unsigned installer and needs no signing key. Only the release workflow signs updates.

## Contributing

Bug reports, fixes and new integrations are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks and ground rules (no games, ROMs or artwork in the repository, and no secrets). Report security problems privately as described in [SECURITY.md](SECURITY.md).

## Library and emulator support

### Store libraries

The Windows scanners support these sources:

| Store | Current import method |
| --- | --- |
| Steam | Auto-detected installed games from `appmanifest_*.acf` files |
| Epic Games | Auto-detected installed games from the Epic Games Launcher `.item` manifests |
| GOG | Owned titles from the local GOG Galaxy database, with installed state when available |
| EA app, Battle.net, Ubisoft Connect, Amazon Games | Best-effort installed-game discovery from Windows registry and uninstall records |
| itch.io | Installed games found in the local itch app folders |
| Xbox / Microsoft Store | Best-effort installed game-package scan; package-name matching can miss games |

All stores are detected automatically; no store accounts or keys are used. **Settings → Libraries** shows which stores and launchers were found on this PC, lets you turn each one off, and sets the scan interval. In the library, **Sort → Library** orders games by store, then title.

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

Folder rules: use full local paths only. System and program folders, network paths, and links (junctions) are refused. Folders of the same kind cannot overlap, and one folder cannot be both a game folder and an emulator folder. A game folder may contain the emulator folder (and the other way round): each scan skips the other. Emulator folders cannot be Downloads, Desktop, or Temp.

### Custom launchers

**Settings → Custom launchers** sets up your own launchers, such as Playnite, a mod manager or an older store client. Each has a name, an icon (24 built-in icons, or a PNG, JPEG, WebP or GIF image of your own), a colour, a program (an `.exe` on a local drive, chosen with **Browse**), launch arguments, and what each of its games uses: a **file**, a **folder** (both chosen with a file picker), or an **identifier** you type (up to 200 letters, digits, spaces and `. _ : @ # ( ) ' + , ! -`, not starting with `-` or `@` or like a drive such as `C:`). The arguments use `{target}` for the game, as an argument on its own; without it the game is added last. For example, Playnite (`Playnite.DesktopApp.exe`) with the arguments `--start {target}` and the game's Playnite id as its identifier. Like emulator arguments, they never use a shell, the game is always one argument, and files named in them must be inside the launcher's folder. Options that let a program run other programs or be remote-controlled (such as `--inspect`, `--remote-debugging-port`, `--gpu-launcher`, `--*-cmd-prefix`, `--no-sandbox`, `--js-flags`) are refused in launcher and emulator arguments. Script files (`.bat`, `.cmd`, `.ps1`) can't be used as launchers. Adding a launcher, or changing its program, arguments, target kind or working folder, shows exactly what will run in a confirmation dialog first; the same goes for a new emulator or a new emulator program or arguments.

Heroic starts games through `heroic://launch/<runner>/<game>` links. Those can't be a launcher identifier (identifiers can't contain `/`, and `{target}` can't be glued to other text). For Heroic games, create a desktop shortcut in Heroic and add that shortcut with **Add local game**; a shortcut with arguments is confirmed once under **Review**.

A launcher may allow several of these (for example files and identifiers); **Add game** then asks which one the game uses. The program is optional: leave it empty to run each game directly, with the launcher's name as the label (an "app folder" such as a portable collection of games and emulators).

**Games folders.** Each launcher can have games folders (**Add folder…**, chosen with a folder picker). A scan looks through the folder and its subfolders and sorts what it finds: emulator programs it knows (added to your emulator list with their standard settings, never twice), PC games (a folder with one main `.exe`, also in `bin` or `x64` subfolders, or a shortcut), and ROMs (in a folder named after their system, or a file type only one system uses). A ROM type several systems share (such as `.iso` or `.rvz`) is matched to the one system an emulator of yours runs; otherwise it waits under **Review** for you to pick the emulator. Loose programs in the folder (such as the app's own `.exe`) are never guessed to be games. With a program, each game is passed to it as its file (or folder); without one, games run directly. Their launch routes show the launcher's name under **Play from**. A game that is already in your library (the same file, or the same title) gets another route instead of a copy, and its title, cover, favourite, notes and playtime are never changed. Rescans only update what the folder added and mark vanished files as not installed. Removing a games folder asks whether to keep its games or remove them. Scanning never runs, opens or reads the files it finds beyond their names, sizes and the metadata the library scan already uses; shortcuts with arguments still need your confirmation, and emulators in Downloads, the Desktop or a Temp folder are not added.

Each launcher appears in the side bar with its icon and colour and filters the library. **Add game** (or **Add game to …** while a launcher is selected) adds a game to it; covers come from SteamGridDB by title. Playtime is tracked while the launcher's process runs. Deleting a launcher with games asks whether to keep them (as Custom games without the launcher) or remove them. Launchers are included in library backups and checked again when a backup is imported.

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

## Play session history

Every tracked session (game, start, end, duration and whether it was launched from Game Lobby or detected running) is kept in the library. The game page lists recent sessions with their date and start and end times; **All sessions** on the game page, and **Play history** in the library bar or the side rail, open the full history for one game or for all games, loaded 50 at a time.

- **Limits.** Sessions are only recorded while Game Lobby is running. A game that starts and ends while Game Lobby is closed isn't recorded, and detection relies on the game's process being seen with a game window.
- **Crashes and shutdowns.** Playtime is saved every few seconds while a game runs. If Game Lobby closes unexpectedly, the next start ends the open session at the last time the game was seen running and marks it **Interrupted**; it is never counted up to the current time.
- **Corrections.** You can correct a finished session's start and end (the end must be after the start, not in the future, and not overlap another session of the same game) or delete it after confirming. Corrected sessions are marked **Edited**, and the game's playtime changes by the same amount. Playtime imported from stores is kept.
- **Journal.** Each session can have a short note (up to 2,000 characters) and screenshots you added to the game.
- **Share.** **Share session** builds a plain-text or Markdown summary (game, date, start and end, duration, store or emulator and system, note) to copy or save through a Save dialog, and can open a Backloggd search for the game in your browser. Nothing leaves your PC unless you copy, save or open a link.
- **Storage and backups.** History, notes and screenshot links are stored in the local library database in the app data folder (see below). Library backups include finished sessions and their notes; importing a backup restores them for games in the library and skips sessions you already have.

## Local data and privacy

The SQLite database is managed by `src-tauri/src/db.rs` and is stored in `%APPDATA%\app.gamelobby.desktop`, together with cached cover art. On first launch, Game Lobby copies an existing library and custom covers from the earlier Electron build (`%APPDATA%\game-lobby`), leaving the original untouched; re-enter API keys once, because keys encrypted by the Electron build can't be read. It uses write-ahead logging and foreign-key checks. JSON backups include game metadata, launch routes, settings, emulator profiles, custom launchers and play session history with notes, but never API keys.

Game Lobby has no telemetry and no account of its own. It contacts the internet only for the features you use: the services listed under [Data sources and terms](#data-sources-and-terms), remote themes you add, and update checks against GitHub.

## License

Game Lobby is licensed under the GNU General Public License v3.0 only (GPL-3.0-only). You may use, modify, and redistribute the project under that license. When you distribute a modified version, GPL-3.0 requires you to preserve the same freedoms and provide the corresponding source code. See [`LICENSE`](LICENSE) for the complete terms. Third-party dependencies retain their own licenses.

## Trademarks

Game Lobby is not affiliated with or endorsed by Valve, Epic Games, GOG, Microsoft, Ubisoft, Electronic Arts, Blizzard, Amazon, itch.io, Twitch, IGDB, SteamGridDB, Playnite, or any emulator project. Steam, Epic Games, GOG, Xbox and the other names and logos mentioned are trademarks of their respective owners and are used only to identify the stores and programs Game Lobby works with. Store icons come from [Simple Icons](https://simpleicons.org/) (CC0); the icons themselves remain subject to their owners' trademark rights.

## Project status

Game Lobby is an early-stage independent project. Steam, Epic, GOG, and best-effort local discovery for the listed Windows storefronts are available, with ownership coverage varying by source as described above. Broader account-catalog imports, richer metadata services, and executable plugin support remain future work.

## Acknowledgements

The project takes inspiration from the unified-library experience of [Playnite](https://playnite.link/) and the library presentation patterns of game storefronts. Game Lobby is an independent project and does not include or redistribute proprietary code or assets from those products. Cover art comes from the [SteamGridDB](https://www.steamgriddb.com/) community. Game details, screenshots, artwork and time-to-beat figures come from [IGDB.com](https://www.igdb.com/).

The About section on the game page uses the [Quantico](https://fonts.google.com/specimen/Quantico) typeface by Matthew Desmond, bundled locally through the [`@fontsource/quantico`](https://fontsource.org/fonts/quantico) package under the [SIL Open Font License 1.1](https://openfontlicense.org/). The font is never downloaded at runtime.
