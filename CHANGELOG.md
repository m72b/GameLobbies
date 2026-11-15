# Changelog

All notable changes to Game Lobby are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Custom launchers
- Add any game already in your library to any custom launcher from its game page (**Launchers → Add to a launcher…**). The game shows under that launcher, and Play runs it through the launcher's program with the game's own file, install folder or store id (whichever the launcher takes), after a Windows confirmation showing exactly what will run. The game's original way to start stays under "Play from", the Play button names the launcher, and removing it from the launcher takes away only what was added. Assignments survive rescans and are included in backups. When none of the game's targets fit the launcher, the page says why instead of adding a route that can't work.
- Playing through a custom launcher whose program isn't running starts the launcher on its own first and shows "Waiting for …" until you press Continue (it shows when the launcher is running). The waiting launch can be continued once, within five minutes. Stores and emulators start as before.
  - Note: older versions of Game Lobby show a game added this way as a normal game of that launcher (without the remove option), and refuse to import a backup that contains one, so avoid going back to an older version after using it.

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
