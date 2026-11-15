# Game Lobby user guide

Detailed reference for Game Lobby's features. For an overview, installation and setup, see the [README](../README.md).

## Contents

- [Store libraries](#store-libraries)
- [Emulators and ROM library](#emulators-and-rom-library)
- [Custom launchers](#custom-launchers)
- [Linked games, versions and launch options](#linked-games-versions-and-launch-options)
- [Launch options](#launch-options)
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

Scanning combines routes when their stable source and external app identity match. Games with the same title remain separate unless a source provides a shared identity; use **Link other games…** on a game's page to combine them yourself after a review (see [Linked games](#linked-games-versions-and-launch-options)). This avoids merging unrelated games such as different releases with identical names. On a game's page, **Play from** chooses the store or launcher to start it with. Installed discovery for the other listed stores is local-only; those integrations do not retrieve complete online account catalogs.

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

Rescans are safe. Titles, favorites, covers, playtime, and emulator choices are kept, and nothing is duplicated. Possible duplicates go to **Settings → Review**, where you can keep them separate, link them after reviewing what is combined, or dismiss them. Offline or unreadable folders never remove games. If a scan would remove more than half of a folder's games (and more than 20), it holds off and reports it. Unreadable subfolders and unsupported files are listed in the scan summary and in Review.

Folder rules: use full local paths only. System and program folders, network paths, and links (junctions) are refused. Folders of the same kind cannot overlap, and one folder cannot be both a game folder and an emulator folder. A game folder may contain the emulator folder (and the other way round): each scan skips the other. Emulator folders cannot be Downloads, Desktop, or Temp.

## Custom launchers

**Settings → Custom launchers** sets up your own launchers, such as Playnite, a mod manager or an older store client. Each has a name, an icon (24 built-in icons, or a PNG, JPEG, WebP or GIF image of your own), a colour, a program (an `.exe` on a local drive, chosen with **Browse**), launch arguments, and what each of its games uses: a **file**, a **folder** (both chosen with a file picker), or an **identifier** you type (up to 200 letters, digits, spaces and `. _ : @ # ( ) ' + , ! -`, not starting with `-` or `@` or like a drive such as `C:`). The arguments use `{target}` for the game, as an argument on its own; without it the game is added last. For example, Playnite (`Playnite.DesktopApp.exe`) with the arguments `--start {target}` and the game's Playnite id as its identifier. Like emulator arguments, they never use a shell, the game is always one argument, and files named in them must be inside the launcher's folder. Options that let a program run other programs or be remote-controlled (such as `--inspect`, `--remote-debugging-port`, `--gpu-launcher`, `--*-cmd-prefix`, `--no-sandbox`, `--js-flags`) are refused in launcher and emulator arguments. Script files (`.bat`, `.cmd`, `.ps1`) can't be used as launchers. Adding a launcher, or changing its program, arguments, target kind or working folder, shows exactly what will run in a confirmation dialog first; the same goes for a new emulator or a new emulator program or arguments.

Heroic starts games through `heroic://launch/<runner>/<game>` links. Those can't be a launcher identifier (identifiers can't contain `/`, and `{target}` can't be glued to other text). For Heroic games, create a desktop shortcut in Heroic and add that shortcut with **Add local game**; a shortcut with arguments is confirmed once under **Review**.

A launcher may allow several of these (for example files and identifiers); **Add game** then asks which one the game uses. The program is optional: leave it empty to run each game directly, with the launcher's name as the label (an "app folder" such as a portable collection of games and emulators).

**Games folders.** Each launcher can have games folders (**Add folder…**, chosen with a folder picker). A scan looks through the folder and its subfolders and sorts what it finds: emulator programs it knows (added to your emulator list with their standard settings, never twice), PC games (a folder with one main `.exe`, also in `bin` or `x64` subfolders, or a shortcut), and ROMs (in a folder named after their system, or a file type only one system uses). A ROM type several systems share (such as `.iso` or `.rvz`) is matched to the one system an emulator of yours runs; otherwise it waits under **Review** for you to pick the emulator. Loose programs in the folder (such as the app's own `.exe`) are never guessed to be games. With a program, each game is passed to it as its file (or folder); without one, games run directly. Their launch routes show the launcher's name under **Play from**. A game that is already in your library (the same file or folder) gets another route instead of a copy, and its title, cover, favourite, notes and playtime are never changed. A game that only has the same title as one in your library is added on its own and offered under **Review** to link: the same name may be another edition. Rescans only update what the folder added and mark vanished files as not installed. Removing a games folder asks whether to keep its games or remove them. Scanning never runs, opens or reads the files it finds beyond their names, sizes and the metadata the library scan already uses; shortcuts with arguments still need your confirmation, and emulators in Downloads, the Desktop or a Temp folder are not added.

Each launcher appears in the side bar with its icon and colour and filters the library. **Add game** (or **Add game to …** while a launcher is selected) adds a game to it; covers come from SteamGridDB by title. Playtime is tracked while the launcher's process runs. Deleting a launcher with games asks whether to keep them (as Custom games without the launcher) or remove them. Launchers are included in library backups and checked again when a backup is imported.

**Adding a game you already have: Link to launcher.** Any game in your library can be linked to any custom launcher. Open **Link to launcher…** from the game page's **⋯** menu, from **Link to launcher…** under **Launchers** on the right of the game page, or by right-clicking the game's card in the library (or its **⋯** button). The list shows every custom launcher with its icon and colour; click **Link** next to one and the game is linked straight away. A short message says so (**Linked to Heroic. Play now starts it through Heroic.**) with **Undo**. Launchers that can't take the game are greyed out with the reason, and launchers it is already linked to show **Linked** with **Unlink**.

For a launcher with a program, the game is passed to it as something the game already has: its file (a PC game or ROM), its install folder, or its store id (such as a Steam app id), whichever the launcher's target kinds allow. Game Lobby picks it: the game's first way to start comes first, and within it a file, then a folder, then a store id. Nothing is typed in. When the game offers the launcher more than one, **Choose which file…** under that launcher lets you pick another. **Play** then runs the game through the launcher (the button names it), and the game's original way to start stays under **Play from**. For a launcher without a program, the game is only listed under it and keeps starting as before.

Linking is usually one click, with no Windows dialog. When you saved the launcher, its Windows dialog showed the program, arguments and working folder and said that any game in your library can then be linked to it without asking again; linking runs exactly that command with each game's own file, folder or store id. A Windows dialog showing exactly what will run appears only in these cases. **The first link to a launcher saved before this version of Game Lobby, or restored from a backup**: it asks once, and after you confirm, later links are one click again. **After you change the launcher's program, arguments, working folder or target kinds**: Settings shows its save dialog once for the new command, and once you confirm it there, links stay one click. **When adding launch options**: they always get their own confirmation. (A launcher with a very long argument list is the exception: when its dialog is too long to fit on screen, every link asks.)

**Launch options** for the launcher route are added afterwards with **Launch options** under **Play from** on the game page; they always get their own Windows confirmation. If none of the game's files, folders or ids fit what the launcher takes (for example a launcher for files and a game that is only a store id), the list says so and nothing is added. **Unlink** (or the **×** next to the launcher on the game page) removes only what linking added; games added through the launcher itself or its games folders stay. Rescans keep these links, a game's own store route stays even when the store stops listing it, and library backups include them. Built-in stores can't take games from elsewhere, and ROMs choose their emulator in the emulator picker on the same page.

**Waiting for a launcher.** Some launchers need a while to start, sign in or accept being woken up. When you play a game through a custom launcher with a program and that program isn't running (checked by its exact path in the Windows process list), Game Lobby starts the launcher on its own, without the game, and shows **Waiting for …**. It shows when the launcher is running; press **Continue** once it is open and signed in, and the game starts through it. **Cancel** leaves the launcher open and starts nothing. The waiting launch lasts five minutes and can be continued once; after that, press **Play** again. When the launcher is already running, the game starts straight away. Stores and emulators start as before.


### Launcher arguments

The arguments are the command line after the launcher's program. Game Lobby splits them into pieces and starts the program with those pieces directly, one argument each. No shell is used.

- The only placeholder is `{target}`. It must be an argument on its own, such as `--start {target}`. It can't be glued to other text (`--id={target}`, `-x{target}`, `@{target}`), and no other `{…}` text is allowed.
- `{target}` becomes what the game uses, as one argument, whatever it contains:
  - **File**: the game's full path, such as `D:\Games\Retro\Chrono.iso`.
  - **Folder**: the game's full folder path, such as `D:\Games\Pac-Man`.
  - **Identifier**: the identifier exactly as typed, such as `game-42`.
- Without `{target}`, the target is added as the last argument.
- Quotes only group text: `--name="My Game"` becomes `--name=My Game`. Backslashes are always literal, so `D:\Games` needs no escaping. An unclosed quote is refused. Pipes and `&&` don't work and aren't needed, because each argument is passed as it is.

**Refused in arguments**, with the reason shown when you save:

- Tabs, line breaks and other control characters.
- `&`, `|`, `<`, `>`, `^`, `%` and the backtick, which only a shell would use.
- Non-ASCII punctuation, such as curly quotes pasted from a web page. Letters and digits of any language are fine.
- Options that let a program run other programs, load code or accept remote control. An option is refused when its name, without its leading `--`, `-` or `/` and in any letter case, starts with one of these: `inspect`, `remote-debugging`, `gpu-launcher`, `no-sandbox`, `load-extension`, `disable-web-security`, `renderer-cmd-prefix` (and any name containing `cmd-prefix`), `js-flags`, `browser-subprocess-path`, `proxy-server`, `proxy-pac-url`, `host-rules`, `host-resolver-rules`, `ignore-certificate-errors`, `disable-gpu-sandbox` and `require`. The parts after `=` are checked too.
- Paths must stay inside the program's folder. An absolute path (`C:\…`) must be inside that folder. A relative path (`cores\x.dll`) must start with a letter, digit, `_` or `.`, and can't use `..`. Network paths, web addresses (`https://…`) and root-relative paths (`\x.dll`) are refused. Paths are also checked after `=`, `;` and `,`.

**Limits**

- Arguments: at most 2,048 characters and 128 pieces.
- Program: a full path to an `.exe` on a local drive. `.bat`, `.cmd`, `.ps1`, network paths and relative paths are refused.
- File or folder targets: full local paths that must still exist when the game starts. A `.lnk` or `.url` file is passed only if Game Lobby could start it itself: a plain shortcut to an `.exe`, a store link, or a shortcut you confirmed under **Review**.
- Working folder: a full local path that must exist at launch. By default it is the program's folder.

**Saving**

- The **Arguments** box shows a **Preview** of the command line with a sample target. **Save** stays disabled while the preview is invalid.
- Saving a new launcher, or changing its program, arguments, target kinds or working folder, opens a Windows confirmation, **Save this launcher?**. It lists the program, each argument on its own line (with `{target}` for the game), the working folder and the name. Name, icon and colour changes save without it.
- With **No program**, there are no arguments and no working folder. Each game runs directly, as an ordinary game from the launcher's games folders, labelled with the launcher's name.
- Launcher arguments apply to every game of that launcher. A game can add its own [launch options](#launch-options) on top: they come after the launcher's arguments and the game's target, follow the same rules (files only inside the launcher's folder, no `{` or `}`), and are confirmed in their own Windows dialog with the whole command line. Changing the launcher's program, arguments or working folder later means each game's options show **Needs confirmation** until you confirm them again.

**Examples**

- A launcher that opens a game file: program `ModHub.exe`, arguments `--open {target}`, target **File**. It runs `ModHub.exe --open D:\Games\Retro\Chrono.iso`.
- A launcher that takes a game id: program `StoreHub.exe`, arguments `--launch {target}`, target **Identifier**, id `game-42`. It runs `StoreHub.exe --launch game-42`.
- A fixed flag with a folder: program `ArcadeBox.exe`, arguments `--portable --folder {target}`, target **Folder**. It runs `ArcadeBox.exe --portable --folder D:\Arcade\Pac-Man`.
- The first example with launch options `-windowed -nosplash` on one game: it runs `ModHub.exe --open D:\Games\Retro\Chrono.iso -windowed -nosplash`. The confirmation lists the four arguments and says that 3 and 4 are the game's launch options.

## Linked games, versions and launch options

**Linking.** Two or more library entries can be one game with several ways to play it: the same game from two stores, or two editions or versions, even from the same store (for example Minecraft Java Edition and Minecraft Bedrock Edition, which both come from Xbox). On a game's page, open **⋯ → Link other games…**, select one or more entries and choose **Review link…**. The review lists every entry with its store, playtime and sessions and shows how the details that differ are resolved, with a choice for each: which entry is kept, the title (one of theirs or your own), the cover, the game page art (hero and logo), your score and notes, the IGDB details and favourite. It also lists the launch routes the linked game will have under **Play from**, with suggested names. Nothing is linked until you press **Link games**. Library health (**Settings → Review → Check library**) and folder scans only *suggest* likely duplicates (same title on the same system, same store id, same file); they never link anything on their own, and **Link…** on a suggestion opens the same review.

What linking keeps: every launch route with its own store, target, install state, owned flag, playtime and name; all play sessions, screenshots and memories; playtime is added up. Details you didn't pick (for example the other entry's notes) are not deleted: they are kept in the library for that entry and come back if you unlink it. Removing the linked game removes them too.

**Route names.** Every route can have a name of up to 60 characters, shown after its store or launcher: **Play from**, the **Stores** list on the game page and the **Play** button read **Xbox · Java Edition** and **Xbox · Bedrock Edition**, so you always see which version Play starts. Linking suggests names: known Xbox packages name their edition (Minecraft's `Microsoft.4297127D64EC6` is Java Edition, `Microsoft.MinecraftUWP` is Bedrock Edition), an entry titled "Hades: Demo" next to the shared title "Hades" becomes "Demo", and two routes that would still read the same get their `.exe` name. Rename a route with the pencil next to it under **Stores** (empty removes the name). A name only changes how the route reads; it never changes what Play runs, and rescans keep it.

**Launch options.** Each route can have its own launch options, set on the game page under **Play from** or with the sliders button next to the route under **Stores**. See [Launch options](#launch-options).

**Unlinking.** The unlink button next to a route (shown when the game has more than one) splits it off into a game of its own, after a preview of what moves and what stays. If the route came from an entry you linked, every route that entry brought leaves together (an entry that was itself linked into it comes along, and can be unlinked from it afterwards) and the entry comes back as it was: its id, title, cover, favourite, hidden flag, notes, page art, IGDB match, controller details, screenshots, memories, the sessions it brought and the playtime it brought (taken off the linked game). Sessions played after linking stay with the linked game, because Game Lobby records sessions per game, not per route. A route that was never linked becomes a new game titled "<game> (<route name or store>)" with the store's cover; your notes, sessions and playtime stay with the original game. Unlinked games aren't suggested as duplicates of each other again. Entries can be unlinked in any order; each gets its own details back. A route the entry had that the game already had before linking (the same store and target) stays with the game when the entry is unlinked, and the entry comes back with a copy of it. Unlinking is refused while the game is running; close it first. If a scan or a backup adds an entry back as a game of its own while it is still linked, it gets its own details back the same way (its routes stay in the linked game) and is no longer linked.

**Backups and older versions.** Library backups include route names, what each linked entry brought and the notes, controller details and page art kept for linked entries, so unlinking still works after a restore, also into an empty library (it then restores the entry's title, cover, flags, notes, page art, controller details, sessions and playtime; screenshots, memories and IGDB matches stay with the linked game, because backups don't carry them for entries). A restore never replaces a link that is in the library now, and skips linked entries that don't fit the library as it is (their game is missing or no longer has their routes, or the entry is a game itself). Launch options in a backup are restored but need confirming before they run. Older versions of Game Lobby show linked games and their routes but drop route names when they save the game, can't unlink, run local games and launcher games without their launch options, and refuse to import a backup that contains route names or linked games.
## Launch options

Launch options are extra arguments for one way of starting a game, such as `-windowed -nosplash`. They work for:

- **Games started from their own `.exe`** (games added from a folder, a scan or **Add local game**): the options are passed to the game's `.exe`.
- **Games played through a custom launcher with a program** (added with the launcher's **Add game**, found in its games folders, or added with **Add to a launcher…**): the options are added after the launcher's own arguments and the game's target (see [Launcher arguments](#launcher-arguments)).

Store routes (`steam://`, Xbox apps, Epic, GOG and the others), launchers without a program and emulators start the game themselves, so the page says where to set options instead (for example in Steam, on the game's own `.exe` route, or under **Settings → Emulators**).

**Setting them.** On a game's page, under **Play from**, the chosen route shows its options (**Options: -windowed -nosplash**) or a **Launch options** button; **Edit** changes them. The sliders button next to each route under **Stores** does the same for any route. While you type, Game Lobby checks the options and either says why they can't be used or shows what will run (**Runs** with the program and every argument). **Save options…** opens a Windows dialog that shows the program, every argument on its own numbered line (for a launcher: its arguments, the game's target, then your options, with a line saying which numbers are yours) and the working folder before anything is saved; the game, route and launcher names come last. The options are saved only if the game, and for a launcher route the launcher's program, arguments and working folder, didn't change while the dialog was open. **Remove** (or an empty box) clears them without a dialog.

**When adding a game to a launcher.** **Add to a launcher…** has an optional **Launch options** box for launchers with a program. Whatever you type is checked the same way and shown in the same Windows dialog as the new route, so one confirmation covers both.

**Rules.** Options are passed as separate arguments, never through a command prompt, and get the same checks as emulator and launcher arguments: no shell characters (`&`, `|`, `<`, `>`, `^`, `%` or a backtick), no control or look-alike characters, no options that let a program run other code, redirect its network traffic or be remote-controlled, and files only inside the program's folder (also after a `;` or `,` in a value): the game's folder for a game's own `.exe`, and the launcher's folder for a launcher route, exactly like the launcher's own arguments (for example `-config plugins\settings.ini`). If a launcher has a working folder other than its program's folder, its options can't use relative paths (they would be looked up in the working folder): use a full path inside the launcher's folder instead. On a launcher route, `{` and `}` aren't allowed: the launcher's own arguments already pass the game with `{target}`. Shells, script hosts and interpreters (such as `cmd.exe`, `powershell.exe`, `python.exe`, `node.exe` or `java.exe`) and programs inside the Windows folder can't take launch options at all, as a game or as a launcher. At most 24 options of up to 200 characters each, 1,000 characters in all. On a launcher route the whole command line must also fit the Windows dialog: at most 24 arguments counting the launcher's own and the game's target, and 2,000 characters for the program, every argument and the working folder; a launcher with very many arguments or a game with a very long path can't take launch options.

**Needs confirmation.** Options are checked again at every launch, and they only run for exactly what you confirmed: the game's `.exe`, or for a launcher route the launcher's program, its working folder and the whole command line (the launcher's arguments, the game's target and your options). Options that came from a backup or an older version of Game Lobby, a route whose `.exe` or target moved or changed, and a launcher whose program, arguments or working folder changed show **Needs confirmation** under **Play from** (with **Confirm…**) and in the editor (**Confirm options…**), and Play refuses to start the game until you confirm them again or clear them. When a launcher isn't running yet, it is first started on its own without the game and without your options; **Continue** then starts the game with the options checked again, and refuses if they changed in the meantime.

**Other notes.** Moving a game's `.exe` with **Locate…** in library health drops its options, and merging two routes keeps them only when both start the same `.exe`. A game that needs administrator rights can't get launch options on its own `.exe` (Windows doesn't pass them), and Play says so.

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
