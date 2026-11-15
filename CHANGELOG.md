# Changelog

All notable changes to Game Lobby are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased] — 1.0.0

The first public release.

### Library
- Detects installed games from Steam, Epic Games, GOG, Xbox / Microsoft Store, Ubisoft Connect, EA app, Battle.net, itch.io and Amazon Games, grouped by library.
- Scans your own game folders and emulator / ROM folders, including hidden and system folders and a games folder that contains the emulators folder.
- Emulator profiles with ROM routes; duplicate emulator entries show the ROM file name.
- Custom games from folders, shortcuts and programs.
- Custom launchers: your own launcher with its name, icon, colour, program and arguments, its own side-rail entry and library filter. Changes to its program or arguments are confirmed in a Windows dialog.
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
