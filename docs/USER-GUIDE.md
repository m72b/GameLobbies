# Game Lobby user guide

Detailed reference for Game Lobby's features. For an overview, installation and setup, see the [README](../README.md).

## Contents

- [Store libraries](#store-libraries)
- [Emulators and ROM library](#emulators-and-rom-library)
- [Custom launchers](#custom-launchers)
- [Play session history](#play-session-history)
- [Themes](#themes)
- [Extensions](#extensions)
- [Local data](#local-data)

## Store libraries

The Windows scanners support these sources:

| Store | Current import method |
| --- | --- |
| Steam | Auto-detected installed games from `appmanifest_*.acf` files; optionally owned games that aren't installed, from the Steam Web API (see below) |
| Epic Games | Auto-detected installed games from the Epic Games Launcher `.item` manifests |
| GOG | Owned titles from the local GOG Galaxy database, with installed state when available |
| EA app, Battle.net, Ubisoft Connect, Amazon Games | Best-effort installed-game discovery from Windows registry and uninstall records |
| itch.io | Installed games found in the local itch app folders |
| Xbox / Microsoft Store | Best-effort installed game-package scan; package-name matching can miss games |

All stores are detected automatically; no store accounts or keys are needed. **Settings → Libraries** shows which stores and launchers were found on this PC, lets you turn each one off, and sets the scan interval. In the library, **Sort → Library** orders games by store, then title.

### Steam games you own but haven't installed

Steam is installed-games only unless you set this up. Under the Steam row in **Settings → Libraries**, open **Show games you own but haven't installed**:

1. Get a free Steam Web API key at [steamcommunity.com/dev/apikey](https://steamcommunity.com/dev/apikey) (any domain name, such as `localhost`, works).
2. Find your SteamID64: it is the 17-digit number starting with `7656119`, shown as "Steam ID" on your [Steam account page](https://store.steampowered.com/account/).
3. Set **Game details** to **Public** in your Steam profile's privacy settings; otherwise Steam returns no games.
4. Paste both and choose **Save**. Game Lobby asks Steam at once and shows the result on the status line.

Owned games that aren't installed are added as Steam games marked not installed; they show under **All** when **Show only installed games** is off, and **Play** asks Steam to install them. What the installed-games scan finds always wins, there are no duplicates, and your titles, covers, favourites, notes and playtime are never changed (Steam's own playtime is not imported). Library scans ask Steam at most every 30 minutes (10 minutes after a failure). If Steam can't be reached, the profile is private or the key is rejected, the games from the last successful check stay and the status line says why; after "Key rejected", save a new key. Games that leave your Steam account later are removed again unless you favourited, played, rated or customized them.

The key and SteamID are encrypted for your Windows user, are never shown again, and are left out of backups, logs and **Copy diagnostics**. **Remove** clears both and removes the not-installed games the import added (again, games you put something into stay).

Scanning combines routes when their stable source and external app identity match. Games with the same title remain separate unless a source provides a shared identity; use **Link another game** on a game's page to combine cross-store duplicates yourself. This avoids merging unrelated games such as different releases with identical names. On a game's page, **Play from** chooses the store or launcher to start it with. Installed discovery for the other listed stores is local-only; those integrations do not retrieve complete online account catalogs.

Windows playtime tracking looks for a running process inside the selected game's install folder while Game Lobby is open. It records elapsed time and last-played time locally. Routes without a usable install folder can still update last-played when launched, but cannot be process-tracked.

Emulators are configured as profiles and can be found automatically in emulator folders. See [Emulators and ROM library](#emulators-and-rom-library).

## Emulators and ROM library

Add library folders in **Settings → Library folders**: one or more **Game folders** and **Emulator folders**, chosen with **Browse** (folders and programs are never typed in). Each folder can be enabled or disabled, rescanned, or removed. **Scan all** scans every folder, and the library **Scan** also runs a folder scan.

**Game folders** hold one subfolder per system, named for it, such as `PS3`, `Sony 3`, `PlayStation 3`, `Switch`, `PS1`, `Wii`, `GameCube`, `NES`, or `PC`. Names are matched loosely. Game Lobby supports 24 systems: PS1, PS2, PS3, PS4, PSP, PS Vita, GameCube, Wii, Wii U, Switch, 3DS, DS, GBA, GB/GBC, N64, SNES, NES, Genesis/Mega Drive, Dreamcast, Saturn, Xbox, Xbox 360, Arcade, and PC.

- Multi-file games count once. `.m3u`, `.cue`, `.gdi`, `.ccd`, and `.mds` files claim the files they list, but only files in the same folder or below it.
- Some games are folders rather than single files: PS3 (`PS3_GAME/USRDIR/EBOOT.BIN`, with the title read from `PARAM.SFO`), PS4 (`eboot.bin` with `sce_sys`), Wii U (`code/*.rpx`), and Xbox 360 (`default.xex`). Switch updates and DLC are skipped.
- Under **PC**, each subfolder is one game. The main `.exe` is chosen automatically, or the game goes to **Review** when the choice is unclear.
- Internet shortcuts (`.url`) are accepted only for supported store links, such as `steam:` and Epic links. A `.lnk` that points to a plain `.exe` is stored as that `.exe`. Any other `.lnk`, such as one with arguments or a non-`.exe` target, goes to **Review**, must be confirmed, and will not launch if it changes after confirmation.

**Add local game** in the library adds a single `.exe`, `.lnk`, or `.url` by browsing, using the same shortcut rules.

**Emulator folders** are searched up to four levels deep for known emulators: Dolphin, PCSX2, RPCS3, DuckStation, PPSSPP, Ryujinx, yuzu forks, Cemu, xemu, Xenia and Xenia Canary, melonDS, Azahar/Lime3DS/Citra, mGBA, Project64, Snes9x, Mesen, Flycast, Vita3K, shadPS4, and RetroArch. Emulators can also be added manually with **Browse**. Each profile has editable launch arguments (a new emulator, or a new program or arguments, is confirmed in a Windows dialog) with the placeholders `{rom}`, `{romdir}`, and `{romname}`. Quotes group text. If `{rom}` is missing, the ROM path is appended. RetroArch needs its core set in the arguments before it can be used. The default argument templates come from each emulator's documentation; edit them if your build differs. A removed emulator stays removed on rescan, and you can restore it from **Ignored emulators**. A second copy of an already-known emulator is added disabled.

Each ROM game uses the best matching enabled emulator, or the one you pick on its game page. Launching never uses a shell: each argument is passed separately, so file names with spaces, quotes, `&`, `%`, or `^` are safe. Playtime for emulator games is tracked for games started from Game Lobby, by the emulator process it launched.

Rescans are safe. Titles, favorites, covers, playtime, and emulator choices are kept, and nothing is duplicated. Possible duplicates go to **Settings → Review**, where you can keep them separate, link them, or dismiss them. Offline or unreadable folders never remove games. If a scan would remove more than half of a folder's games (and more than 20), it holds off and reports it. Unreadable subfolders and unsupported files are listed in the scan summary and in Review.

Folder rules: use full local paths only. System and program folders, network paths, and links (junctions) are refused. Folders of the same kind cannot overlap, and one folder cannot be both a game folder and an emulator folder. A game folder may contain the emulator folder (and the other way round): each scan skips the other. Emulator folders cannot be Downloads, Desktop, or Temp.

## Custom launchers

**Settings → Custom launchers** sets up your own launchers, such as Playnite, a mod manager or an older store client. Each has a name, an icon (24 built-in icons, or a PNG, JPEG, WebP or GIF image of your own), a colour, a program (an `.exe` on a local drive, chosen with **Browse**), launch arguments, and what each of its games uses: a **file**, a **folder** (both chosen with a file picker), or an **identifier** you type (up to 200 letters, digits, spaces and `. _ : @ # ( ) ' + , ! -`, not starting with `-` or `@` or like a drive such as `C:`). The arguments use `{target}` for the game, as an argument on its own; without it the game is added last. For example, Playnite (`Playnite.DesktopApp.exe`) with the arguments `--start {target}` and the game's Playnite id as its identifier. Like emulator arguments, they never use a shell, the game is always one argument, and files named in them must be inside the launcher's folder. Options that let a program run other programs or be remote-controlled (such as `--inspect`, `--remote-debugging-port`, `--gpu-launcher`, `--*-cmd-prefix`, `--no-sandbox`, `--js-flags`) are refused in launcher and emulator arguments. Script files (`.bat`, `.cmd`, `.ps1`) can't be used as launchers. Adding a launcher, or changing its program, arguments, target kind or working folder, shows exactly what will run in a confirmation dialog first; the same goes for a new emulator or a new emulator program or arguments.

Heroic starts games through `heroic://launch/<runner>/<game>` links. Those can't be a launcher identifier (identifiers can't contain `/`, and `{target}` can't be glued to other text). For Heroic games, create a desktop shortcut in Heroic and add that shortcut with **Add local game**; a shortcut with arguments is confirmed once under **Review**.

A launcher may allow several of these (for example files and identifiers); **Add game** then asks which one the game uses. The program is optional: leave it empty to run each game directly, with the launcher's name as the label (an "app folder" such as a portable collection of games and emulators).

**Games folders.** Each launcher can have games folders (**Add folder…**, chosen with a folder picker). A scan looks through the folder and its subfolders and sorts what it finds: emulator programs it knows (added to your emulator list with their standard settings, never twice), PC games (a folder with one main `.exe`, also in `bin` or `x64` subfolders, or a shortcut), and ROMs (in a folder named after their system, or a file type only one system uses). A ROM type several systems share (such as `.iso` or `.rvz`) is matched to the one system an emulator of yours runs; otherwise it waits under **Review** for you to pick the emulator. Loose programs in the folder (such as the app's own `.exe`) are never guessed to be games. With a program, each game is passed to it as its file (or folder); without one, games run directly. Their launch routes show the launcher's name under **Play from**. A game that is already in your library (the same file, or the same title) gets another route instead of a copy, and its title, cover, favourite, notes and playtime are never changed. Rescans only update what the folder added and mark vanished files as not installed. Removing a games folder asks whether to keep its games or remove them. Scanning never runs, opens or reads the files it finds beyond their names, sizes and the metadata the library scan already uses; shortcuts with arguments still need your confirmation, and emulators in Downloads, the Desktop or a Temp folder are not added.

Each launcher appears in the side bar with its icon and colour and filters the library. **Add game** (or **Add game to …** while a launcher is selected) adds a game to it; covers come from SteamGridDB by title. Playtime is tracked while the launcher's process runs. Deleting a launcher with games asks whether to keep them (as Custom games without the launcher) or remove them. Launchers are included in library backups and checked again when a backup is imported.

**Adding a game you already have.** Any game in your library can be added to any custom launcher from its own page: under **Launchers** on the right, choose **Add to a launcher…**, pick the launcher and press **Play with …**. The game then shows under that launcher in the side bar and its filter. For a launcher with a program, the game is passed to it as something the game already has: its file (a PC game or ROM), its install folder, or its store id (such as a Steam app id), whichever the launcher's target kinds allow; when several fit, you choose which. Nothing is typed in. A Windows dialog shows exactly what will run (the program, its arguments with this game's file, folder or id, and the working folder) before the route is added. **Play** then runs the game through the launcher (the button names it), and the game's original way to start stays under **Play from**. For a launcher without a program, the game is only listed under it and keeps starting as before. If none of the game's files, folders or ids fit what the launcher takes (for example a launcher for files and a game that is only a store id), the page says so and nothing is added. The **×** next to a launcher you added this way removes only what was added; games added through the launcher itself or its games folders stay. Rescans keep these choices, a game's own store route stays even when the store stops listing it, and library backups include them. Built-in stores can't take games from elsewhere, and ROMs choose their emulator in the emulator picker on the same page.

**Waiting for a launcher.** Some launchers need a while to start, sign in or accept being woken up. When you play a game through a custom launcher with a program and that program isn't running (checked by its exact path in the Windows process list), Game Lobby starts the launcher on its own, without the game, and shows **Waiting for …**. It shows when the launcher is running; press **Continue** once it is open and signed in, and the game starts through it. **Cancel** leaves the launcher open and starts nothing. The waiting launch lasts five minutes and can be continued once; after that, press **Play** again. When the launcher is already running, the game starts straight away. Stores and emulators start as before.


## Play session history

Every tracked session (game, start, end, duration and whether it was launched from Game Lobby or detected running) is kept in the library. The game page lists recent sessions with their date and start and end times; **All sessions** on the game page, and **Play history** in the library bar or the side rail, open the full history for one game or for all games, loaded 50 at a time.

- **Limits.** Sessions are only recorded while Game Lobby is running. A game that starts and ends while Game Lobby is closed isn't recorded, and detection relies on the game's process being seen with a game window.
- **Crashes and shutdowns.** Playtime is saved every few seconds while a game runs. If Game Lobby closes unexpectedly, the next start ends the open session at the last time the game was seen running and marks it **Interrupted**; it is never counted up to the current time.
- **Corrections.** You can correct a finished session's start and end (the end must be after the start, not in the future, and not overlap another session of the same game) or delete it after confirming. Corrected sessions are marked **Edited**, and the game's playtime changes by the same amount. Playtime imported from stores is kept.
- **Journal.** Each session can have a short note (up to 2,000 characters) and screenshots you added to the game.
- **Share.** **Share session** builds a plain-text or Markdown summary (game, date, start and end, duration, store or emulator and system, note) to copy or save through a Save dialog, and can open a Backloggd search for the game in your browser. Nothing leaves your PC unless you copy, save or open a link.
- **Storage and backups.** History, notes and screenshot links are stored in the local library database in the app data folder (see [Local data](#local-data)). Library backups include finished sessions and their notes; importing a backup restores them for games in the library and skips sessions you already have.

## Themes

Game Lobby ships with a built-in dark fallback theme. Additional themes can be loaded from HTTPS JSON or token-only CSS URLs and applied without restarting the app. Theme values are validated design tokens; remote CSS is not injected as executable stylesheet content. A theme may define its name, colors, font, corner radius, spacing, hover effect, and an HTTPS background image. Loaded theme data is cached locally for offline use.

Theme JSON example: [`themes/example.json`](../themes/example.json). CSS themes should define supported `--token-name` values inside a `:root` block.

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

## Local data


The SQLite database is managed by `src-tauri/src/db.rs` and is stored in `%APPDATA%\app.gamelobby.desktop`, together with cached cover art. On first launch, Game Lobby copies an existing library and custom covers from the earlier Electron build (`%APPDATA%\game-lobby`), leaving the original untouched; re-enter API keys once, because keys encrypted by the Electron build can't be read. It uses write-ahead logging and foreign-key checks. JSON backups include game metadata, launch routes, settings, emulator profiles, custom launchers and play session history with notes, but never API keys.

Game Lobby has no telemetry and no account of its own. It contacts the internet only for the features you use: the services listed under [Data sources and terms](../README.md#data-sources-and-terms), remote themes you add, and update checks against GitHub.

For development, debug builds honour `GAME_LOBBY_DATA_DIR` (an absolute folder) to use a separate data folder. Release builds ignore it.
