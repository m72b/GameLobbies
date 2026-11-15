# Changelog

All notable changes to Game Lobby are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Library
- Optional: Steam games you own but haven't installed. Add your own free Steam Web API key and SteamID64 under **Settings → Libraries → Steam** and they show under All, marked not installed. Without a key Steam stays installed-games only. Installed state from the local scan wins, nothing you changed is overwritten, Steam is asked at most every 30 minutes, and a failed check (offline, private profile, rejected key) keeps the previous list and explains why. The key and SteamID are encrypted by Windows and left out of backups, logs and diagnostics; **Remove** clears them and the games they added.

### Linked games and launch routes
- Launch routes can have their own name (up to 60 characters), shown after the store or launcher under **Play from**, in the game page's **Stores** list and on the **Play** button, so two routes of one store read apart: **Xbox · Java Edition** and **Xbox · Bedrock Edition**. Names are display only (they never change what runs) and rescans keep them.
- **Link other games…** links two or more entries, including entries from the same store, after a review step: it lists what is combined and lets you choose the kept entry, title (or your own), cover, page art, notes and score, IGDB details and favourite, and shows the resulting routes with suggested names (from known Xbox packages such as Minecraft's, from the entries' own titles, or from `.exe` names). Every route keeps its own store, target, install state and playtime; sessions, screenshots and memories move over, playtime is added up, and details you didn't pick are kept for unlinking instead of deleted.
- Unlink a route from the game page: a route that came from a linked entry brings that entry back with its own title, cover, favourite, notes, page art, IGDB match, screenshots, memories, the sessions it brought and its playtime. Sessions played after linking stay with the linked game (sessions are recorded per game, not per route). A route that was never linked becomes a new game of its own. Unlinked games aren't suggested as duplicates again. Entries can be unlinked in any order and each gets its own details back, an entry linked into a game that was then linked into another comes back too, and an entry a scan or backup adds back as a game gets its own details back.
- Unlinking never takes a route the kept game already had: when a linked entry had a route the game already had, the route stays and the entry comes back with a copy. Unlinking is refused while the game is running. Launch options are also refused for shells, script hosts, interpreters and programs in the Windows folder, and for proxy, host-rule, certificate and `--require` options, and paths hidden behind `;` or `,` in a value are checked.
- Launch options per route for games Game Lobby starts from their own `.exe`: checked like emulator and launcher arguments (no shell, no code-loading or debugging options, files only inside the game's folder), at most 24 options of 200 characters (1,000 in all), shown in a Windows confirmation with the exact program and arguments before they are saved, and checked again at every launch. They only run on the `.exe` they were confirmed for: options from a backup or an older version, or on a route whose `.exe` changed, show **Needs confirmation** and don't run until confirmed again. Moving the `.exe` in library health drops them, and merged routes keep them only for the same `.exe`. Store, Xbox and emulator routes, and launchers without a program, say where to set options instead.
- Library health and scan duplicates only suggest linking; **Link…** opens the review. Launcher games folders no longer add a found game to an existing game just because the titles match: it is added on its own and offered under Review to link.
  - Note: linked entries are recorded in a new `game_links` table and confirmed launch options in `launch_args_confirmed` (no schema version change; confirmations are never part of a backup). Backups include route names, linked entries and the notes, controller details and page art kept for them; a restore never replaces a live link and skips linked entries that don't fit the library. Older versions of Game Lobby show linked games and their routes, but drop route names when they next save a game, can't unlink, start local games without their launch options, and refuse to import a backup that contains route names or linked games.

### Fixes
- Choosing a match with **Find details on IGDB** also renames the game to the IGDB name. Automatic matching never renames, and later scans keep the new name.
- Game page heroes follow the art's own shape, so much less is cut off, and Steam games use Steam's double-size hero when it has one, so the hero is sharper.
- Store games whose install folder has nothing to start (for example only DLL files) no longer show as installed. Folders on a drive that isn't connected are left as they are.
- Clearer installer wording when an older version is installed: the screen says your library is kept, the choices read "Uninstall the old version first (your library is kept)" and "Install over the old version", and the uninstaller's box now reads "Also delete my library and settings (permanent)". In-app updates skip these screens and never delete data.

### Custom launchers
- **Link to launcher**: link any game to a custom launcher in one click, from the game page's **⋯** menu, the game page's **Launchers** block, or a library card's right-click menu. The list shows each launcher with its icon and colour, greys out the ones that can't take the game (with the reason), and shows **Linked** with **Unlink** for the ones it is linked to. Game Lobby picks what the launcher gets from the game itself (its first way to start first; a file, then a folder, then a store id) and says **Linked to … Play now starts it through …** with **Undo**; **Choose which file…** appears only when the game offers several. No Windows dialog is shown when you already confirmed that launcher's program, arguments, working folder and target kinds on this PC when saving it; a launcher saved by an older version, restored from a backup or changed without its dialog asks once, and then links in one click. Both the launcher's save dialog and that link dialog say right after the program, where a long argument list can't push it off screen, that confirming lets any game in your library be linked later without asking, and name every kind of target the launcher takes (file, folder, store id). A link dialog too long to read whole still links the game but leaves the launcher unconfirmed, so the next link asks again. Launch options are added afterwards under **Play from** and are always confirmed. A store link can't be given a store id the game doesn't already have, so linking can only pass a launcher an id the game really has.
  - Note: confirmed launcher commands are recorded in a new `launcher_confirmed` table (no schema version change; never part of a backup, and a restored launcher is confirmed again on its first link). The game page's **Add to a launcher…** picker and its launch options box are replaced by **Link to launcher…**.
- Add any game already in your library to any custom launcher from its game page (**Launchers → Add to a launcher…**). The game shows under that launcher, and Play runs it through the launcher's program with the game's own file, install folder or store id (whichever the launcher takes), after a Windows confirmation showing exactly what will run. The game's original way to start stays under "Play from", the Play button names the launcher, and removing it from the launcher takes away only what was added. Assignments survive rescans and are included in backups. When none of the game's targets fit the launcher, the page says why instead of adding a route that can't work.
- Playing through a custom launcher whose program isn't running starts the launcher on its own first and shows "Waiting for …" until you press Continue (it shows when the launcher is running). The waiting launch can be continued once, within five minutes. Stores and emulators start as before.
  - Note: older versions of Game Lobby show a game added this way as a normal game of that launcher (without the remove option), and refuse to import a backup that contains one, so avoid going back to an older version after using it.
- Launch options for games played through a custom launcher with a program: extra arguments for one game, added after the launcher's own arguments and the game's target. They follow the launcher-argument rules (no shell, no code-loading or debugging options, files only inside the launcher's folder, no `{` or `}`), at most 24 options of 200 characters (1,000 in all), and a Windows dialog shows the program, the whole numbered command line (saying which arguments are the game's options) and the working folder before they are saved. They are bound to exactly that command line: changing the launcher's program, arguments or working folder, or the game's target, shows **Needs confirmation** and Play refuses until you confirm again. A launcher that isn't running yet is still started on its own without arguments, and **Continue** checks the options again. Launchers without a program don't take options.
- Easier launch options: the game page shows the chosen route's options under **Play from** (**Options: -windowed -nosplash**) with **Edit**, a **Launch options** button when it has none, and **Needs confirmation** with a one-click **Confirm…**. The editor checks the text as you type, shows why it would be refused or what will run, has an example and links to the user guide. **Add to a launcher…** has an optional **Launch options** box, confirmed in the same Windows dialog as the new route.
  - Note: launch options on launcher routes are stored in the route's existing launch options field, which backups already include; confirmations are kept in `launch_args_confirmed` as before (never in a backup), so restored options need confirming. Older versions of Game Lobby keep these options but start launcher games without them.

## [0.4.0] — 2026-11-15

The first public release. Version 1.0.0 follows once the release pipeline and hands-on test pass are done.

### Library
- Detects installed games from Steam, Epic Games, GOG, Xbox / Microsoft Store, Ubisoft Connect, EA app, Battle.net, itch.io and Amazon Games, grouped by library.
- Scans your own game folders and emulator / ROM folders, including hidden and system folders and a games folder that contains the emulators folder.
- Emulator profiles with ROM routes; duplicate emulator entries show the ROM file name.
- Custom games from folders, shortcuts and programs.
- Custom launchers: your own launcher with its name, icon, colour, program and arguments, its own side-rail entry and library filter. Changes to its program or arguments are confirmed in a Windows dialog.
- Custom launcher games folders: scan an app folder for PC games, shortcuts, emulators and ROMs in one go. Emulators are registered without duplicates, ROMs are matched to them (uncertain matches go to Review), games already in the library get an extra "Play from" route named after the launcher, and rescans never touch your titles, covers, favourites, notes or playtime.
- Custom launchers can allow several target kinds (file, folder, identifier), and the program is optional: without one, games run directly with the launcher as their label.
  - Note: older versions of Game Lobby treat a launcher's games folders as plain game folders (without the launcher's label or routes), so avoid going back to one after adding them. Launcher games folders, like other library folders, aren't included in backups: add them again after restoring one.
- Storage view: install sizes per game, a total, and "Uninstall…" that hands off to the game's own launcher.
- Recently added shelf for newly discovered games.
- Library health: finds missing or moved files and duplicate games or launch routes, and helps repair them.

### Game page
- Artwork from SteamGridDB (covers, heroes and logos, 50 per page with "Load more"), including animated art.
- Game details from IGDB: description, genres, developer, publisher, ratings, screenshots and time to beat, with a guided Twitch setup.
- Screenshot wall with your own screenshots and Steam screenshot import.
- Your score, notes, memories, and controller details (support level from Steam, preferred controller, input notes).

### Play tracking
- Playtime for games started from Game Lobby or detected while it runs.
- Full play history with exact start and end times, editing and deleting sessions, crash and shutdown recovery, a journal per session, and share or export as text or Markdown.

### App
- Game Lobby minimizes when a game starts (can be turned off).
- Only one copy runs at a time; opening it again brings up the running window.
- Log file and "Copy diagnostics" for bug reports, with keys and personal paths left out.
- Signed automatic updates.
- New GL app icon.

### Safety
- Game Lobby's interface can never choose which program, link or arguments the app runs: programs and folders come only from Windows file pickers, store links must match one known shape per store, and emulator arguments are checked.
- API keys are stored encrypted with Windows DPAPI and never logged.
- The library database is checked at every start and repaired automatically if it is damaged; backups use SQLite's own backup method.
