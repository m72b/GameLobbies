//! Tauri commands for custom launchers (see `launchers` for the trust model). The renderer
//! sends ids, names, colours, icon names and checked text; every path comes from a native
//! picker opened here.

use crate::launchers::{self, CustomLauncher, LauncherInput, RemoveMode, SaveContext, TargetKind};
use crate::local_commands::confirm_natively;
use crate::picks::PickKind;
use crate::state::AppState;
use crate::types::Game;
use crate::{db, emulators, local, roots};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

fn id_arg(value: &str) -> CmdResult<String> {
    launchers::check_id(value).map_err(err)
}

/// Launchers with `available` filled in; the filesystem is checked after the lock is gone.
fn list(state: &AppState) -> CmdResult<Vec<CustomLauncher>> {
    let stored = launchers::list(&state.db.lock()).map_err(err)?;
    Ok(launchers::with_status(stored))
}

fn notify_launchers(app: &AppHandle) {
    let _ = app.emit("launchers:changed", ());
}

fn notify_library(app: &AppHandle, state: &AppState) {
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
}

/// Whether a launcher has games or games folders, and its own routes' targets (sorted), for the
/// save checks that depend on them.
fn launcher_usage(conn: &rusqlite::Connection, id: &str) -> anyhow::Result<(bool, Vec<String>)> {
    let games = !launchers::games_of(conn, id)?.is_empty()
        || roots::list(conn)?
            .iter()
            .any(|r| r.launcher_id.as_deref() == Some(id));
    let mut targets = launchers::route_targets(conn, id)?;
    targets.sort();
    Ok((games, targets))
}

#[tauri::command]
pub async fn launchers_list(state: State<'_, AppState>) -> CmdResult<Vec<CustomLauncher>> {
    list(&state)
}

/// Creates or updates a launcher from a [`LauncherInput`] (no paths). The executable and
/// working folder come from the latest picks of `launchers_browse_executable` /
/// `launchers_browse_folder` when the input asks for them, else they stay as stored.
///
/// A new launcher, or a change to what its launches run (program, arguments, target kind,
/// working folder), is shown to the user in a native dialog first (the exact program and
/// argument list) and saved only when they choose Save: the renderer alone can't make Game
/// Lobby run something new. Name, icon and colour changes save directly.
#[tauri::command]
pub async fn launchers_save(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    launcher: Value,
) -> CmdResult<Vec<CustomLauncher>> {
    if serde_json::to_string(&launcher).map_or(usize::MAX, |s| s.len()) > 16_384 {
        return Err("The launcher is too large.".into());
    }
    let input: LauncherInput =
        serde_json::from_value(launcher).map_err(|e| format!("The launcher is invalid: {e}"))?;
    let id = input.id.as_deref().map(id_arg).transpose()?;
    // Checked before the database lock is taken.
    let requested_icon = input.icon_value.clone();
    let icon_found = launchers::icon_exists(&state.covers_dir(), &requested_icon);
    let icon_exists = |name: &str| icon_found && name == requested_icon;
    // Which target kinds the launcher's games use needs the filesystem (is a path a folder?):
    // read the targets, release the lock, then classify them.
    let usage = match &id {
        Some(id) => launcher_usage(&state.db.lock(), id).map_err(err)?,
        None => (false, Vec::new()),
    };
    let (has_games, targets) = usage.clone();
    let used_kinds = launchers::used_kinds(&targets);
    let (stored, accepted) = {
        let conn = state.db.lock();
        let stored = match &id {
            Some(id) => launchers::get(&conn, id).map_err(err)?,
            None => None,
        };
        let accepted = launchers::accept_renderer_launcher(
            input,
            SaveContext {
                stored: stored.as_ref(),
                executable_pick: state.picks.latest(PickKind::LauncherExecutable),
                folder_pick: state.picks.latest(PickKind::LauncherFolder),
                has_games,
                used_kinds,
                icon_exists: &icon_exists,
            },
        )
        .map_err(err)?;
        (stored, accepted)
    };
    if launchers::needs_confirmation(stored.as_ref(), &accepted.launcher) {
        let prompt = launchers::confirmation_prompt(&accepted.launcher).map_err(err)?;
        if !confirm_natively(&window, "Confirm launcher", &prompt, "Save").await {
            return Err("The launcher was not saved.".into());
        }
    }
    let mut launcher = accepted.launcher;
    {
        let conn = state.db.lock();
        // The dialog may have been open for a while: save only over what was confirmed.
        let now = match &id {
            Some(id) => launchers::get(&conn, id).map_err(err)?,
            None => None,
        };
        if now != stored {
            return Err("The launcher changed in the meantime. Open it again and save.".into());
        }
        // Its games or folders may have changed too (a scan, Add game): the checks above relied
        // on them.
        if let Some(id) = &id {
            if launcher_usage(&conn, id).map_err(err)? != usage {
                return Err(
                    "The launcher's games changed in the meantime. Open it again and save.".into(),
                );
            }
        }
        if stored.is_none() {
            if launchers::list(&conn).map_err(err)?.len() >= launchers::MAX_LAUNCHERS {
                return Err(format!(
                    "You can have at most {} custom launchers.",
                    launchers::MAX_LAUNCHERS
                ));
            }
            launcher.sort_order = launchers::next_sort_order(&conn).map_err(err)?;
        }
        launchers::save(&conn, &launcher).map_err(err)?;
    }
    if accepted.used_executable_pick {
        state
            .picks
            .forget(PickKind::LauncherExecutable, &launcher.executable);
    }
    if accepted.used_folder_pick {
        if let Some(dir) = &launcher.working_dir {
            state.picks.forget(PickKind::LauncherFolder, dir);
        }
    }
    notify_launchers(&app);
    // Launcher names and icons show on the library cards.
    let _ = app.emit("library:changed", ());
    list(&state)
}

/// Deletes a launcher and its games folders. `mode` `keep`: its games stay without the
/// launcher's routes (games found in its folders keep running directly, without the label);
/// `remove`: its routes and the routes found in its folders go, and games left without routes
/// are removed too.
#[tauri::command]
pub async fn launchers_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    mode: String,
) -> CmdResult<Vec<CustomLauncher>> {
    let id = id_arg(&id)?;
    let mode = RemoveMode::parse(&mode).ok_or("Choose what happens to the launcher's games.")?;
    {
        // No folder scan may run meanwhile (its games folders go too); session state before the
        // database, as in `roots_remove`.
        let _scan = state.local_scan_lock.lock().await;
        let mut sessions = state.tracker.sessions();
        let removed = launchers::remove(&state.db.lock(), &id, mode)
            .map_err(err)?
            .ok_or("That launcher no longer exists.")?;
        for game in removed {
            sessions.forget(&game);
        }
    }
    crate::covercache::clean_up(&state);
    notify_launchers(&app);
    notify_library(&app, &state);
    let _ = app.emit("review:changed", ());
    list(&state)
}

/// Native folder picker for one of a launcher's games folders; the chosen path (for display) or
/// `null`. `launchers_add_folder` accepts it for a while (see `picks`): the renderer can't name a
/// folder by itself, because a scan turns the programs in it into launchable games.
#[tauri::command]
pub async fn launchers_browse_games_folder(
    window: WebviewWindow,
    state: State<'_, AppState>,
    launcher_id: String,
) -> CmdResult<Option<String>> {
    let launcher_id = id_arg(&launcher_id)?;
    let launcher = launchers::get(&state.db.lock(), &launcher_id)
        .map_err(err)?
        .ok_or("That launcher no longer exists.")?;
    // The dialog names the launcher; the pick is only accepted for it (`launchers_add_folder`).
    let title = format!("Games folder for {}", launcher.name);
    let chosen = pick_folder_titled(&window, &title)
        .await
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = &chosen {
        state
            .picks
            .remember_for(PickKind::LauncherGamesFolder, path, &launcher_id);
    }
    Ok(chosen)
}

/// Adds the folder just chosen with `launchers_browse_games_folder` as a games folder of
/// launcher `launcherId` (a `games` library root with the same checks as `roots_add`); returns
/// every library root. The renderer starts the scan with `roots_rescan`.
#[tauri::command]
pub async fn launchers_add_folder(
    state: State<'_, AppState>,
    launcher_id: String,
    path: String,
) -> CmdResult<Vec<roots::LibraryRoot>> {
    let launcher_id = id_arg(&launcher_id)?;
    let picked = state
        .picks
        .find_for(PickKind::LauncherGamesFolder, &path, &launcher_id)
        .ok_or("Choose the folder with this launcher's Add folder… (it can't be typed in).")?;
    {
        let launcher = launchers::get(&state.db.lock(), &launcher_id)
            .map_err(err)?
            .ok_or("That launcher no longer exists.")?;
        if !launchers::is_label_only(&launcher)
            && !launcher.allows(TargetKind::File)
            && !launcher.allows(TargetKind::Folder)
        {
            return Err(format!(
                "{}'s games use identifiers, so a folder can't be scanned for them. Let it use files or folders too.",
                launcher.name
            ));
        }
    }
    let guard = roots::RootGuard::from_environment(&state.data_dir);
    // Every filesystem check first, without the database lock.
    let (display, key) = roots::prepare_launcher_folder(&picked, &guard).map_err(err)?;
    roots::insert_for(
        &state.db.lock(),
        "games",
        &display,
        &key,
        Some(&launcher_id),
    )
    .map_err(err)?;
    state.picks.forget(PickKind::LauncherGamesFolder, &picked);
    state.tracker.library_changed();
    let listed = roots::list(&state.db.lock()).map_err(err)?;
    Ok(roots::with_online(listed))
}

/// Native file picker for a launcher's `.exe`; the chosen path (for display) or `null`.
/// `launchers_save` with `useExecutablePick` uses it for a while (see `picks`).
#[tauri::command]
pub async fn launchers_browse_executable(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    let chosen = crate::commands::pick_file(&window, "Launcher program", &["exe"])
        .await
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = &chosen {
        if !emulators::executable_path_ok(path) {
            return Err(r"Choose an .exe file on a local drive (not a network folder).".into());
        }
        state.picks.remember(PickKind::LauncherExecutable, path);
    }
    Ok(chosen)
}

async fn pick_folder(window: &WebviewWindow) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

async fn pick_folder_titled(window: &WebviewWindow, title: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .set_title(title)
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

async fn pick_any_file(window: &WebviewWindow) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Native folder picker for a launcher's working folder; the chosen path or `null`.
#[tauri::command]
pub async fn launchers_browse_folder(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    let chosen = pick_folder(&window)
        .await
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = &chosen {
        if !emulators::drive_path_ok(path) {
            return Err("Choose a folder on a local drive (not a network folder).".into());
        }
        state.picks.remember(PickKind::LauncherFolder, path);
    }
    Ok(chosen)
}

/// Native picker for an icon image; copies it into the app's covers folder and returns its new
/// file name (the renderer sends that name back with `launchers_save`), or `null`.
#[tauri::command]
pub async fn launchers_choose_icon(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    let Some(source) =
        crate::commands::pick_file(&window, "Images", &["png", "jpg", "jpeg", "webp", "gif"]).await
    else {
        return Ok(None);
    };
    let covers = state.covers_dir();
    let name = crate::platform::on_thread(move || launchers::import_icon(&covers, &source))
        .await
        .map_err(err)?
        .map_err(err)?;
    Ok(Some(name))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedLauncherGame {
    game_id: String,
    games: Vec<Game>,
}

/// Adds a game to a launcher. `kind` (`file` | `folder` | `identifier`) says what this game uses;
/// it may be left out when the launcher allows only one. File and folder targets: a native picker
/// opens here (cancel = `null`); identifiers: `identifier` is checked
/// (`launchers::check_identifier`). An empty title uses the file or folder name. A game with the
/// same target is reused. A launcher without a program adds a game file (.exe, .lnk, .url or a
/// ROM) chosen here, exactly like "Add local game", labelled with the launcher.
#[tauri::command]
pub async fn launchers_add_game(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    launcher_id: String,
    title: String,
    identifier: Option<String>,
    kind: Option<String>,
) -> CmdResult<Option<AddedLauncherGame>> {
    let launcher_id = id_arg(&launcher_id)?;
    if title.chars().count() > 300 {
        return Err("The title must be at most 300 characters.".into());
    }
    let launcher = launchers::get(&state.db.lock(), &launcher_id)
        .map_err(err)?
        .ok_or("That launcher no longer exists.")?;
    if launchers::is_label_only(&launcher) {
        if identifier.is_some() {
            return Err("This launcher's games are files you choose.".into());
        }
        return add_label_only_game(&app, &window, &state, &launcher, &title).await;
    }
    let allowed = launcher.kinds().map_err(err)?;
    let kind = match kind.as_deref() {
        Some(raw) => TargetKind::parse(raw)
            .filter(|k| allowed.contains(k))
            .ok_or_else(|| format!("{}'s games don't use that.", launcher.name))?,
        None if allowed.len() == 1 => allowed[0],
        None => {
            return Err("Choose what this game uses: a file, a folder or an identifier.".into())
        }
    };
    let target = match kind {
        TargetKind::Identifier => identifier.ok_or("Enter the game's identifier.")?,
        TargetKind::File | TargetKind::Folder => {
            if identifier.is_some() {
                return Err("This launcher's games use a file or folder you choose.".into());
            }
            let picked = match kind {
                TargetKind::File => pick_any_file(&window).await,
                _ => pick_folder(&window).await,
            };
            let Some(path) = picked else {
                return Ok(None);
            };
            let text = path.to_string_lossy().into_owned();
            let exists = if kind == TargetKind::File {
                path.is_file()
            } else {
                path.is_dir()
            };
            if !launchers::target_shape_ok(kind, &text) || !exists {
                return Err(
                    "Choose a file or folder on a local drive (not a network folder).".into(),
                );
            }
            if kind == TargetKind::File {
                // A shortcut with arguments waits for confirmation, as everywhere else.
                let confirmed = db::shortcut_confirmations(&state.db.lock()).map_err(err)?;
                let picked = path.clone();
                let pending = crate::platform::on_thread(move || {
                    local::launcher_shortcut_pending(&picked, &confirmed)
                })
                .await
                .map_err(err)?
                .map_err(err)?;
                if let Some(mut pending) = pending {
                    if !title.trim().is_empty() {
                        pending.title = title.trim().to_string();
                    }
                    local::request_shortcut_confirmation_for(
                        &state.db.lock(),
                        &pending,
                        Some(&launcher.id),
                    )
                    .map_err(err)?;
                    let _ = app.emit("review:changed", ());
                    return Err(format!(
                        "This shortcut runs {} with the arguments \"{}\". Check it under Review and confirm it to add the game to {}.",
                        pending.target, pending.arguments, launcher.name
                    ));
                }
            }
            text
        }
    };
    let game = launchers::new_game_of_kind(&launcher, &title, &target, kind).map_err(err)?;
    let identity = db::route_identity(&game.routes[0]);
    let (game_id, games) = {
        let mut conn = state.db.lock();
        db::upsert_imported_games(&mut conn, std::slice::from_ref(&game)).map_err(err)?;
        let games = db::all_games(&conn).map_err(err)?;
        let id = games
            .iter()
            .find(|g| g.routes.iter().any(|r| db::route_identity(r) == identity))
            .map(|g| g.id.clone())
            .unwrap_or(game.id);
        (id, games)
    };
    notify_library(&app, &state);
    // Covers come from SteamGridDB by title, like other custom games (when a key is set).
    crate::steamgriddb::spawn_auto_fetch(&app);
    Ok(Some(AddedLauncherGame { game_id, games }))
}

/// `launchers_add_game` for a launcher without a program: a game file chosen in a native picker,
/// added like `library_add_local_game` (same checks; shortcuts with arguments wait for
/// confirmation under Review), with the launcher's label on its route.
async fn add_label_only_game(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &AppState,
    launcher: &CustomLauncher,
    title: &str,
) -> CmdResult<Option<AddedLauncherGame>> {
    let extensions = local::pickable_extensions();
    let refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let Some(path) = crate::commands::pick_file(window, "Games and ROMs", &refs).await else {
        return Ok(None);
    };
    let confirmed = db::shortcut_confirmations(&state.db.lock()).map_err(err)?;
    let picked = path.clone();
    let file = crate::platform::on_thread(move || local::game_for_file(&picked, &confirmed))
        .await
        .map_err(err)?
        .map_err(err)?;
    let mut game = match file {
        local::LocalFile::Game(game) => game,
        local::LocalFile::NeedsConfirmation(pending) => {
            local::request_shortcut_confirmation_for(
                &state.db.lock(),
                &pending,
                Some(&launcher.id),
            )
            .map_err(err)?;
            let _ = app.emit("review:changed", ());
            return Err(format!(
                "This shortcut runs {} with the arguments \"{}\". Check it under Review and confirm it to add the game.",
                pending.target, pending.arguments
            ));
        }
    };
    let title = title.trim();
    if title.chars().any(char::is_control) {
        return Err("The title can't contain control characters.".into());
    }
    if !title.is_empty() {
        game.title = title.to_string();
    }
    game.routes[0].launcher_id = Some(launcher.id.clone());
    let identity = db::route_identity(&game.routes[0]);
    let (game_id, games) = {
        let mut conn = state.db.lock();
        db::upsert_imported_games(&mut conn, std::slice::from_ref(&game)).map_err(err)?;
        let games = db::all_games(&conn).map_err(err)?;
        let id = games
            .iter()
            .find(|g| g.routes.iter().any(|r| db::route_identity(r) == identity))
            .map(|g| g.id.clone())
            .unwrap_or(game.id);
        (id, games)
    };
    notify_library(app, state);
    crate::steamgriddb::spawn_auto_fetch(app);
    Ok(Some(AddedLauncherGame { game_id, games }))
}

// ---------------------------------------------------------------------------
// Assigning games already in the library (the game page's Launcher block)

/// A way to pass a game to a launcher, from one of its routes (`launchers::AssignCandidate`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignTarget {
    route_index: usize,
    kind: &'static str,
    target: String,
}

/// One launcher on a game's page: whether the game shows under it, whether that came from the
/// game page (it can be removed there), and how the game would be passed to it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentOption {
    launcher_id: String,
    /// The game shows under this launcher (any way: Add game, a games folder, the game page).
    listed: bool,
    /// The game page added it (`launchers_unassign` removes it).
    assigned: bool,
    /// For a launcher with a program: the targets it can use (existing files and folders only),
    /// the first being the default.
    targets: Vec<AssignTarget>,
    /// Why it can't be assigned, when it can't.
    problem: Option<String>,
}

fn game_arg(value: &str) -> CmdResult<String> {
    if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
        return Err("The game id is invalid.".into());
    }
    Ok(value.to_string())
}

fn route_arg(value: Option<i64>) -> CmdResult<Option<usize>> {
    match value {
        None => Ok(None),
        Some(i) if (0..128).contains(&i) => Ok(Some(i as usize)),
        Some(_) => Err("Launch route is invalid.".into()),
    }
}

/// Every launcher, as seen from game `gameId`'s page (see [`AssignmentOption`]).
#[tauri::command]
pub async fn launchers_assignments(
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<Vec<AssignmentOption>> {
    let game_id = game_arg(&game_id)?;
    let (game, all) = {
        let conn = state.db.lock();
        let game = db::get_game(&conn, &game_id)
            .map_err(err)?
            .ok_or("Game not found.")?;
        (game, launchers::list(&conn).map_err(err)?)
    };
    // Which files and folders exist is checked without the database lock.
    let options = crate::platform::on_thread(move || {
        all.iter()
            .map(|l| assignment_option(l, &game))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(err)?;
    Ok(options)
}

/// [`AssignmentOption`] for one launcher. Touches the filesystem.
fn assignment_option(l: &CustomLauncher, game: &Game) -> AssignmentOption {
    let mine = |r: &crate::types::GameRoute| launchers::route_launcher(r) == Some(l.id.as_str());
    let listed = game.routes.iter().any(mine);
    let assigned = game
        .routes
        .iter()
        .any(|r| launchers::is_assigned(r) && mine(r));
    let mut targets = Vec::new();
    let mut problem = launchers::assign_blocker(l, game);
    if problem.is_none() && !launchers::is_label_only(l) {
        targets = launchers::assign_candidates(l, game)
            .into_iter()
            .filter(launchers::candidate_exists)
            .map(|c| AssignTarget {
                route_index: c.route_index,
                kind: c.kind.as_str(),
                target: c.target,
            })
            .collect();
        if targets.is_empty() {
            problem = Some(launchers::no_candidate_reason(l, game));
        }
    }
    AssignmentOption {
        launcher_id: l.id.clone(),
        listed,
        assigned,
        targets,
        problem,
    }
}

/// Assigns game `gameId` to launcher `launcherId` from the game page: the game shows under the
/// launcher, and (for a launcher with a program) Play runs it through the launcher with a target
/// taken from one of the game's own routes (`routeIndex` and `kind` choose which; the first that
/// fits otherwise). The renderer names no path, program or argument. A new launch route is shown
/// in a native dialog (program, exact arguments, working folder) and added only when the user
/// confirms it; the game's other routes stay. `options` is optional launch options text for the
/// new route: checked like options set on the game page (`links::prepare_options` for the
/// launcher and that route's target), shown in the same dialog (appended to the arguments), and
/// saved with their confirmation only together with the route. A launcher without a program
/// only labels a route (nothing new runs), without a dialog, and refuses options. Returns the
/// library.
#[tauri::command]
// Tauri passes each renderer argument (and the app, window and state) as its own parameter.
#[allow(clippy::too_many_arguments)]
pub async fn launchers_assign(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
    launcher_id: String,
    route_index: Option<i64>,
    kind: Option<String>,
    options: Option<String>,
) -> CmdResult<Vec<Game>> {
    let game_id = game_arg(&game_id)?;
    let launcher_id = id_arg(&launcher_id)?;
    let route_index = route_arg(route_index)?;
    // Launch options text for the new route (checked below against the launcher, like options
    // set later on the game page); empty means none.
    let options = options.unwrap_or_default();
    if options.chars().count() > crate::links::MAX_ARGS_CHARS {
        return Err(format!(
            "Launch options can be at most {} characters.",
            crate::links::MAX_ARGS_CHARS
        ));
    }
    let kind = match kind.as_deref() {
        None => None,
        Some(raw) => Some(TargetKind::parse(raw).ok_or("That target kind is invalid.")?),
    };
    let (game, launcher) = {
        let conn = state.db.lock();
        let game = db::get_game(&conn, &game_id)
            .map_err(err)?
            .ok_or("Game not found.")?;
        let launcher = launchers::get(&conn, &launcher_id)
            .map_err(err)?
            .ok_or("That launcher no longer exists.")?;
        (game, launcher)
    };
    if let Some(reason) = launchers::assign_blocker(&launcher, &game) {
        return Err(reason);
    }
    let candidate = if launchers::is_label_only(&launcher) {
        if !options.trim().is_empty() {
            return Err(format!(
                "{} has no program, so it doesn't take launch options: the game keeps starting as before. Set options on the game's own .exe route instead.",
                crate::validation::display_text(&launcher.name, 64)
            ));
        }
        None
    } else {
        if kind.is_some_and(|k| !launcher.allows(k)) {
            return Err(format!("{}'s games don't use that.", launcher.name));
        }
        // The target comes from the stored route; files and folders must exist.
        let candidates = launchers::assign_candidates(&launcher, &game);
        let chosen = crate::platform::on_thread(move || {
            candidates.into_iter().find(|c| {
                route_index.is_none_or(|i| c.route_index == i)
                    && kind.is_none_or(|k| c.kind == k)
                    && launchers::candidate_exists(c)
            })
        })
        .await
        .map_err(err)?;
        let Some(chosen) = chosen else {
            return Err(launchers::no_candidate_reason(&launcher, &game));
        };
        let route = launchers::assigned_route(&launcher, &game, &chosen).map_err(err)?;
        // The same checks as every launch, before anything is saved: a shortcut handed to the
        // launcher must be one the user may run, and the program, arguments, target and working
        // folder must be usable.
        let confirmations = db::shortcut_confirmations(&state.db.lock()).map_err(err)?;
        let target = chosen.target.clone();
        crate::platform::on_thread(move || {
            crate::launch::check_launcher_shortcut(&target, &confirmations)
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        launchers::prepare_launch(&launcher, &route).map_err(err)?;
        // Launch options get the checks of options set on the game page, for this launcher and
        // this route's target, and are shown in the same dialog as the route.
        let checked = if options.trim().is_empty() {
            None
        } else {
            let host = crate::links::launcher_host(&launcher, &route).map_err(err)?;
            crate::links::prepare_options(&host, &options)
                .map_err(err)?
                .map(|argv| (host, argv))
        };
        let shown = checked.as_ref().map(|(_, argv)| argv.as_slice());
        let prompt =
            launchers::assign_prompt(&launcher, &game.title, &chosen, shown.unwrap_or_default())
                .map_err(err)?;
        if !confirm_natively(&window, "Confirm launcher", &prompt, "Use launcher").await {
            return Err(format!(
                "{} was not added to {}.",
                game.title, launcher.name
            ));
        }
        Some((chosen, checked))
    };
    let (candidate, checked) = match candidate {
        Some((chosen, checked)) => (Some(chosen), checked),
        None => (None, None),
    };
    let games = {
        let conn = state.db.lock();
        // The dialog may have been open for a while: save only over what was confirmed.
        let now_game = db::get_game(&conn, &game_id).map_err(err)?;
        let now_launcher = launchers::get(&conn, &launcher_id).map_err(err)?;
        if now_game.as_ref() != Some(&game) || now_launcher.as_ref() != Some(&launcher) {
            return Err("The game or the launcher changed in the meantime. Try again.".into());
        }
        let mut routes =
            launchers::assign(&launcher, &game, candidate.as_ref(), route_index).map_err(err)?;
        if let Some((host, argv)) = &checked {
            // `assign` puts the new launcher route first. The host was worked out from this
            // launcher (unchanged, checked above) and that route's target.
            let added = &mut routes[0];
            if crate::links::launcher_host(&launcher, added).ok().as_ref() != Some(host) {
                return Err("The game or the launcher changed in the meantime. Try again.".into());
            }
            added.launch_args = Some(serde_json::to_string(argv).map_err(err)?);
            db::set_routes(&conn, &game_id, routes).map_err(err)?;
            // Confirmed in the dialog above, together with the route.
            crate::links::confirm_host_args(&conn, host, argv).map_err(err)?;
        } else {
            db::set_routes(&conn, &game_id, routes).map_err(err)?;
        }
        db::all_games(&conn).map_err(err)?
    };
    notify_library(&app, &state);
    Ok(games)
}

/// Removes what [`launchers_assign`] added for launcher `launcherId` to game `gameId` (its launch
/// route, or its label), and nothing else. Returns the library.
#[tauri::command]
pub async fn launchers_unassign(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    launcher_id: String,
) -> CmdResult<Vec<Game>> {
    let game_id = game_arg(&game_id)?;
    let launcher_id = id_arg(&launcher_id)?;
    let games = {
        let conn = state.db.lock();
        let game = db::get_game(&conn, &game_id)
            .map_err(err)?
            .ok_or("Game not found.")?;
        let routes = launchers::unassign(&game, &launcher_id)
            .ok_or("This game wasn't added to that launcher on its page.")?;
        db::set_routes(&conn, &game_id, routes).map_err(err)?;
        db::all_games(&conn).map_err(err)?
    };
    notify_library(&app, &state);
    Ok(games)
}
