# Manual checks

Checks that need a real desktop session and are not covered by `cargo test`.

## Single instance

Run these with a build you made yourself (never with the installed copy of your own library
while testing a debug build).

1. Start a release build (`npm run build` output, or `cargo tauri build` and run the unpacked
   exe). The main window opens.
2. Minimize the window, then start the same exe again.
   - The second process exits within a moment (Task Manager shows one `game-lobby.exe`).
   - The first window is restored, shown and focused.
3. Close the main window if the app keeps running in the background, start the exe again: a
   main window must open.
4. Open `<data folder>\logs\game-lobby.log`: the second start leaves one line, "A second copy
   of Game Lobby was started; showing the running one.", and no second "starting" block.
5. Start two copies at the same moment (for example `start game-lobby.exe` twice in one
   command): exactly one stays running.

Behaviour to know:

- The single-instance plugin keys on the app identifier (`app.gamelobby.desktop`), not on
  the data folder. All release builds therefore share one instance.
- A debug build started with `GAME_LOBBY_DATA_DIR` set skips the check on purpose, so a
  throwaway-library dev build can run next to an installed copy. Two dev builds on the same
  `GAME_LOBBY_DATA_DIR` are not guarded (SQLite's own locking still applies).
- The second copy exits while the app is being built, before `setup` opens the log and the
  library, so it never touches `library.sqlite`. `lib.rs` has a test that pins this order.

## Diagnostics

1. Settings > About and Updates > Support > Copy diagnostics shows "Copied"; paste into a text
   editor: versions, counts, yes/no key flags, last log lines, `%USERPROFILE%` instead of the
   user folder, no keys.
2. Open logs folder opens `<data folder>\logs` in Explorer.
