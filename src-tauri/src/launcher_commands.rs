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
    let window = &window;
    let launcher =
        save_launcher(
            &state.db,
            id.as_deref(),
            stored,
            &usage,
            accepted.launcher,
            move |prompt| async move {
                confirm_natively(window, "Confirm launcher", &prompt, "Save").await
            },
        )
        .await?;
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

/// The confirmation and the save of [`launchers_save`], after the input was accepted
/// (`launcher`, checked against `stored` and the launcher's games `usage`, both read before):
/// the native dialog when [`launchers::needs_confirmation`] says so (`confirm` shows it with the
/// prompt it gets and says whether the user chose Save; a parameter so tests can answer it),
/// then the save, only over what was checked (compare-and-save). A confirmed dialog also
/// remembers the launcher's command as confirmed ([`launchers::record_confirmed`]); a save
/// without one (name, icon, colour) leaves that confirmation as it is. Nothing is written when
/// the dialog is cancelled.
async fn save_launcher<F, Fut>(
    db: &db::Db,
    id: Option<&str>,
    stored: Option<CustomLauncher>,
    usage: &(bool, Vec<String>),
    mut launcher: CustomLauncher,
    confirm: F,
) -> CmdResult<CustomLauncher>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let asked = launchers::needs_confirmation(stored.as_ref(), &launcher);
    if asked {
        let prompt = launchers::confirmation_prompt(&launcher).map_err(err)?;
        if !confirm(prompt).await {
            return Err("The launcher was not saved.".into());
        }
    }
    let conn = db.lock();
    // The dialog may have been open for a while: save only over what was confirmed.
    let now = match id {
        Some(id) => launchers::get(&conn, id).map_err(err)?,
        None => None,
    };
    if now != stored {
        return Err("The launcher changed in the meantime. Open it again and save.".into());
    }
    // Its games or folders may have changed too (a scan, Add game): the checks before relied on
    // them.
    if let Some(id) = id {
        if launcher_usage(&conn, id).map_err(err)? != *usage {
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
    if asked {
        // The dialog above showed exactly this program, template, working folder and kinds.
        launchers::record_confirmed(&conn, &launcher).map_err(err)?;
    }
    Ok(launcher)
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
/// fits otherwise). The renderer names no path, program or argument. `options` is optional
/// launch options text for the new route: checked like options set on the game page
/// (`links::prepare_options` for the launcher and that route's target), shown in the native
/// dialog (appended to the arguments), and saved with their confirmation only together with the
/// route. See [`assign_game`] for when a dialog is shown. A launcher without a program only
/// labels a route (nothing new runs), without a dialog, and refuses options. Returns the
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
    let kind = match kind.as_deref() {
        None => None,
        Some(raw) => Some(TargetKind::parse(raw).ok_or("That target kind is invalid.")?),
    };
    let window = &window;
    let assigned = assign_game(
        &state.db,
        &game_id,
        &launcher_id,
        route_index,
        kind,
        options.unwrap_or_default(),
        move |prompt| async move {
            confirm_natively(window, "Confirm launcher", &prompt, "Use launcher").await
        },
    )
    .await?;
    notify_library(&app, &state);
    Ok(assigned.games)
}

/// What [`launchers_link`] did: the library, and the target the new launch route passes to the
/// launcher (`None` for a launcher without a program, which only labels a route).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedGame {
    games: Vec<Game>,
    target: Option<AssignTarget>,
}

/// "Link to launcher": [`launchers_assign`] with nothing but the game and the launcher. Rust picks
/// the target from the game's own routes (route order, then file, folder, store id, limited to
/// the launcher's kinds; the first that exists). No launch options (they are added later on the
/// game page, with their own confirmation). A launcher whose command the user already confirmed
/// on this PC links in one click; otherwise the native dialog asks once (see [`assign_game`]).
#[tauri::command]
pub async fn launchers_link(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
    launcher_id: String,
) -> CmdResult<LinkedGame> {
    let game_id = game_arg(&game_id)?;
    let launcher_id = id_arg(&launcher_id)?;
    let window = &window;
    let assigned = assign_game(
        &state.db,
        &game_id,
        &launcher_id,
        None,
        None,
        String::new(),
        move |prompt| async move {
            confirm_natively(window, "Confirm launcher", &prompt, "Use launcher").await
        },
    )
    .await?;
    notify_library(&app, &state);
    Ok(LinkedGame {
        games: assigned.games,
        target: assigned.candidate.map(|c| AssignTarget {
            route_index: c.route_index,
            kind: c.kind.as_str(),
            target: c.target,
        }),
    })
}

/// What [`assign_game`] did.
struct Assigned {
    games: Vec<Game>,
    /// The target of the new launch route (none for a launcher without a program).
    candidate: Option<launchers::AssignCandidate>,
    /// Whether the native dialog was shown (and confirmed).
    #[cfg_attr(not(test), allow(dead_code))]
    asked: bool,
}

/// Assigns game `game_id` to launcher `launcher_id` (both checked by the caller): the work of
/// [`launchers_assign`] and [`launchers_link`]. `confirm` shows the native dialog with the
/// prompt it gets and says whether the user confirmed it (a parameter so tests can answer it).
///
/// A launcher without a program only labels a route: no dialog. A launcher with a program gets
/// a new launch route whose target comes from the game's own stored routes, checked like every
/// launch. The dialog ([`launchers::assign_prompt_with`]) is skipped only when no launch options are
/// added and the user already confirmed exactly this launcher's command on this PC
/// ([`launchers::is_confirmed`]): the route then runs that command with the game's own target,
/// which is what that confirmation covered. When the dialog is confirmed, the launcher's
/// command is remembered as confirmed too ([`launchers::record_confirmed`]), so the next link
/// needs none, unless the dialog was too long to be sure it was read whole
/// (`links::MAX_CONFIRM_CHARS` plus 1,000 characters). An "open first" launcher starts the game
/// directly from the chosen file, so a file of another launcher's game
/// ([`launchers::takes_another_launchers_file`]) always gets the dialog and never confirms the
/// launcher for later links. Everything is saved only over what was checked (compare-and-save).
async fn assign_game<F, Fut>(
    db: &db::Db,
    game_id: &str,
    launcher_id: &str,
    route_index: Option<usize>,
    kind: Option<TargetKind>,
    options: String,
    confirm: F,
) -> CmdResult<Assigned>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    // Launch options text for the new route (checked below against the launcher, like options
    // set later on the game page); empty means none.
    if options.chars().count() > crate::links::MAX_ARGS_CHARS {
        return Err(format!(
            "Launch options can be at most {} characters.",
            crate::links::MAX_ARGS_CHARS
        ));
    }
    let (game, launcher, confirmed) = {
        let conn = db.lock();
        let game = db::get_game(&conn, game_id)
            .map_err(err)?
            .ok_or("Game not found.")?;
        let launcher = launchers::get(&conn, launcher_id)
            .map_err(err)?
            .ok_or("That launcher no longer exists.")?;
        let confirmed = launchers::is_confirmed(&conn, &launcher).map_err(err)?;
        (game, launcher, confirmed)
    };
    if let Some(reason) = launchers::assign_blocker(&launcher, &game) {
        return Err(reason);
    }
    let mut asked = false;
    // Whether the dialog was short enough to be read whole, so its confirmation can cover later
    // links too (see below).
    let mut record = false;
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
        let confirmations = db::shortcut_confirmations(&db.lock()).map_err(err)?;
        let target = chosen.target.clone();
        crate::platform::on_thread(move || {
            crate::launch::check_launcher_shortcut(&target, &confirmations)
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        // A file of another launcher's game would start directly, outside that launcher: such
        // a link always asks, and never confirms the launcher for later links.
        let other_launcher =
            launchers::takes_another_launchers_file(&launcher, &game.routes[chosen.route_index]);
        let mut facts = launchers::OpenFirstFacts {
            other_launcher,
            ..Default::default()
        };
        if launcher.start_mode == launchers::StartMode::OpenFirst {
            // Opened first: the launcher's program and folder, and the game's own file as it
            // would start without a launcher (a folder or a shortcut nobody confirmed is refused
            // here already; the emulator for a ROM is checked at Play, and named in the dialog
            // when one is set up).
            launchers::prepare_open_first(&launcher, &route).map_err(err)?;
            let (confirmations, profiles) = {
                let conn = db.lock();
                (
                    db::shortcut_confirmations(&conn).map_err(err)?,
                    db::emulator_profiles(&conn).map_err(err)?,
                )
            };
            let (owned_game, owned_route) = (game.clone(), route.clone());
            facts.start = crate::platform::on_thread(move || {
                crate::launch::direct_route(&owned_game, &owned_route, &confirmations).map(
                    |direct| {
                        let profiles = crate::emulators::refresh_available(profiles);
                        crate::launch::direct_start(&direct, &profiles)
                    },
                )
            })
            .await
            .map_err(err)?
            .map_err(err)?;
        } else {
            launchers::prepare_launch(&launcher, &route).map_err(err)?;
        }
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
        // Nothing new runs when the command was confirmed and no options are added (and the
        // file isn't another launcher's).
        if checked.is_some() || !confirmed || other_launcher {
            let shown = checked.as_ref().map(|(_, argv)| argv.as_slice());
            let prompt = launchers::assign_prompt_with(
                &launcher,
                &game.title,
                &chosen,
                shown.unwrap_or_default(),
                &facts,
            )
            .map_err(err)?;
            // A dialog this long may be clipped on screen, hiding what confirming it means: it
            // still links this game, but the launcher stays unconfirmed (the next link asks
            // again).
            let fits = prompt.chars().count() <= crate::links::MAX_CONFIRM_CHARS + 1_000;
            if !confirm(prompt).await {
                return Err(format!(
                    "{} was not added to {}.",
                    game.title, launcher.name
                ));
            }
            asked = true;
            record = fits && !other_launcher;
        }
        Some((chosen, checked))
    };
    let (candidate, checked) = match candidate {
        Some((chosen, checked)) => (Some(chosen), checked),
        None => (None, None),
    };
    let games = {
        let conn = db.lock();
        // The dialog may have been open for a while: save only over what was confirmed.
        let now_game = db::get_game(&conn, game_id).map_err(err)?;
        let now_launcher = launchers::get(&conn, launcher_id).map_err(err)?;
        if now_game.as_ref() != Some(&game) || now_launcher.as_ref() != Some(&launcher) {
            return Err("The game or the launcher changed in the meantime. Try again.".into());
        }
        // Without a dialog, the confirmation it relied on must still be there.
        if candidate.is_some()
            && !asked
            && !launchers::is_confirmed(&conn, &launcher).map_err(err)?
        {
            return Err("The game or the launcher changed in the meantime. Try again.".into());
        }
        let mut routes =
            launchers::assign(&launcher, &game, candidate.as_ref(), route_index).map_err(err)?;
        let tx = conn.unchecked_transaction().map_err(err)?;
        if let Some((host, argv)) = &checked {
            // `assign` puts the new launcher route first. The host was worked out from this
            // launcher (unchanged, checked above) and that route's target.
            let added = &mut routes[0];
            if crate::links::launcher_host(&launcher, added).ok().as_ref() != Some(host) {
                return Err("The game or the launcher changed in the meantime. Try again.".into());
            }
            added.launch_args = Some(serde_json::to_string(argv).map_err(err)?);
            db::set_routes(&tx, game_id, routes).map_err(err)?;
            // Confirmed in the dialog above, together with the route.
            crate::links::confirm_host_args(&tx, host, argv).map_err(err)?;
        } else {
            db::set_routes(&tx, game_id, routes).map_err(err)?;
        }
        if record {
            // The dialog showed this launcher's program, arguments and working folder, and was
            // short enough to be read whole.
            launchers::record_confirmed(&tx, &launcher).map_err(err)?;
        }
        tx.commit().map_err(err)?;
        db::all_games(&conn).map_err(err)?
    };
    Ok(Assigned {
        games,
        candidate,
        asked,
    })
}

/// Checks launch options text for an assignment without saving anything (the checks of
/// [`launchers_assign`], for the route it would add): the reason it would be refused, or what
/// the new route would run. Empty text checks nothing.
#[tauri::command]
pub async fn launchers_assign_check(
    state: State<'_, AppState>,
    game_id: String,
    launcher_id: String,
    route_index: Option<i64>,
    kind: Option<String>,
    options: String,
) -> CmdResult<crate::link_commands::ArgsPreview> {
    let game_id = game_arg(&game_id)?;
    let launcher_id = id_arg(&launcher_id)?;
    let route_index = route_arg(route_index)?;
    let kind = match kind.as_deref() {
        None => None,
        Some(raw) => Some(TargetKind::parse(raw).ok_or("That target kind is invalid.")?),
    };
    if options.chars().count() > crate::links::MAX_ARGS_CHARS {
        return Err(format!(
            "Launch options can be at most {} characters.",
            crate::links::MAX_ARGS_CHARS
        ));
    }
    if options.trim().is_empty() {
        return Ok(Default::default());
    }
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
    if launchers::is_label_only(&launcher) {
        return Err(format!(
            "{} has no program, so it doesn't take launch options: the game keeps starting as before. Set options on the game's own .exe route instead.",
            crate::validation::display_text(&launcher.name, 64)
        ));
    }
    // The route `launchers_assign` would add (its target must exist, like there).
    let candidates = launchers::assign_candidates(&launcher, &game);
    let chosen = crate::platform::on_thread(move || {
        candidates.into_iter().find(|c| {
            route_index.is_none_or(|i| c.route_index == i)
                && kind.is_none_or(|k| c.kind == k)
                && launchers::candidate_exists(c)
        })
    })
    .await
    .map_err(err)?
    .ok_or_else(|| launchers::no_candidate_reason(&launcher, &game))?;
    let route = launchers::assigned_route(&launcher, &game, &chosen).map_err(err)?;
    let host = crate::links::launcher_host(&launcher, &route).map_err(err)?;
    let argv = crate::links::prepare_options(&host, &options).map_err(err)?;
    Ok(crate::link_commands::ArgsPreview::of(
        &host,
        argv.as_deref(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::launchers::{AssignCandidate, CustomLauncher};
    use crate::localscan::tests::TempDir;
    use crate::types::{GamePatch, GameRoute};
    use std::cell::Cell;
    use std::path::Path;

    fn launcher(id: &str, exe: &str, kinds: &[&str]) -> CustomLauncher {
        launchers::validate(CustomLauncher {
            id: id.into(),
            name: id.to_uppercase(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#4F8CFF".into(),
            executable: exe.into(),
            args_template: if exe.is_empty() {
                String::new()
            } else {
                "--start {target}".into()
            },
            target_kind: kinds[0].into(),
            target_kinds: kinds.iter().map(|k| k.to_string()).collect(),
            working_dir: None,
            start_mode: Default::default(),
            sort_order: 0,
            created_at: 1,
            available: false,
        })
        .unwrap()
    }

    fn local_route(exe: &Path) -> GameRoute {
        GameRoute {
            source_id: "local".into(),
            store: "Custom".into(),
            app_id: Some(crate::localscan::key_of(exe)),
            launch_target: exe.to_string_lossy().into_owned(),
            install_path: exe.parent().map(|p| p.to_string_lossy().into_owned()),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn steam_route(install: Option<&Path>) -> GameRoute {
        GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            app_id: Some("400".into()),
            launch_target: "steam://rungameid/400".into(),
            install_path: install.map(|p| p.to_string_lossy().into_owned()),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn game(id: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: "Hollow Knight".into(),
            store: routes[0].store.clone(),
            routes,
            ..Default::default()
        }
    }

    fn library(all: &[&CustomLauncher], games: &[&Game]) -> Db {
        let db = Db::memory().unwrap();
        {
            let conn = db.lock();
            for l in all {
                launchers::save(&conn, l).unwrap();
            }
            for g in games {
                db::save_game(&conn, &GamePatch::from(*g)).unwrap();
            }
        }
        db
    }

    /// [`assign_game`] the way `launchers_link` calls it (only the game and the launcher), with
    /// the dialog answered `answer`; returns the result and how often the dialog was shown.
    async fn link(
        db: &Db,
        game_id: &str,
        launcher_id: &str,
        answer: bool,
    ) -> (CmdResult<Assigned>, usize) {
        let shown = Cell::new(0);
        let result = assign_game(
            db,
            game_id,
            launcher_id,
            None,
            None,
            String::new(),
            |prompt| {
                assert!(prompt.starts_with("Game Lobby will run:"), "{prompt}");
                shown.set(shown.get() + 1);
                async move { answer }
            },
        )
        .await;
        (result, shown.get())
    }

    fn stored(db: &Db, id: &str) -> Game {
        db::get_game(&db.lock(), id).unwrap().unwrap()
    }

    fn confirmations(db: &Db) -> i64 {
        db.lock()
            .query_row("SELECT COUNT(*) FROM launcher_confirmed", [], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn a_confirmed_launcher_links_in_one_click() {
        let dir = TempDir::new("link-confirmed");
        let exe = dir.file("Hub/Hub.exe");
        let game_exe = dir.file("Games/hk/hk.exe");
        let l = launcher("hub", &exe.to_string_lossy(), &["file"]);
        let g = game("g", vec![steam_route(None), local_route(&game_exe)]);
        let db = library(&[&l], &[&g]);
        // Confirmed in the launcher's save dialog (what `launchers_save` records).
        launchers::record_confirmed(&db.lock(), &l).unwrap();

        let (result, shown) = link(&db, "g", "hub", false).await;
        let assigned = result.unwrap();
        assert_eq!(shown, 0, "no dialog");
        assert!(!assigned.asked);
        assert_eq!(
            assigned.candidate,
            Some(AssignCandidate {
                route_index: 1,
                kind: TargetKind::File,
                target: game_exe.to_string_lossy().into_owned(),
            })
        );
        let now = stored(&db, "g");
        assert_eq!(now.routes.len(), 3);
        assert_eq!(now.routes[0].source_id, "launcher:hub");
        assert_eq!(now.routes[0].launch_target, game_exe.to_string_lossy());
        assert_eq!(now.routes[0].assigned, Some(true));
        assert_eq!(now.routes[0].launch_args, None);
        assert_eq!(now.routes[1..], g.routes[..]);
        // It runs exactly the confirmed command with the game's own file.
        let launch = launchers::prepare_launch(&l, &now.routes[0]).unwrap();
        assert_eq!(
            launch.argv,
            ["--start".to_string(), now.routes[0].launch_target.clone()]
        );
        // Without launch options, Play needs no other confirmation.
        assert!(
            crate::links::checked_stored_args(&db.lock(), &now.routes[0])
                .unwrap()
                .is_empty()
        );

        // Undo: unassign gives back exactly the game's own routes.
        let routes = launchers::unassign(&now, "hub").unwrap();
        assert_eq!(routes, g.routes);
        db::set_routes(&db.lock(), "g", routes).unwrap();
        assert_eq!(stored(&db, "g").routes, g.routes);
        // Linking again is still one click.
        let (result, shown) = link(&db, "g", "hub", false).await;
        assert!(result.is_ok());
        assert_eq!(shown, 0);
        // Once linked, it can't be linked twice.
        let (result, shown) = link(&db, "g", "hub", true).await;
        assert!(result.err().unwrap().contains("already listed"));
        assert_eq!(shown, 0);
    }

    #[tokio::test]
    async fn an_unconfirmed_launcher_asks_once_then_links_in_one_click() {
        let dir = TempDir::new("link-legacy");
        let exe = dir.file("Hub/Hub.exe");
        let a = game("a", vec![local_route(&dir.file("Games/a/a.exe"))]);
        let b = game("b", vec![local_route(&dir.file("Games/b/b.exe"))]);
        // Saved before confirmations were recorded (or in an older version): no row.
        let l = launcher("hub", &exe.to_string_lossy(), &["file"]);
        let db = library(&[&l], &[&a, &b]);
        assert!(!launchers::is_confirmed(&db.lock(), &l).unwrap());

        // Cancelled: nothing changes, nothing is remembered.
        let (result, shown) = link(&db, "a", "hub", false).await;
        assert!(result.err().unwrap().contains("was not added"));
        assert_eq!(shown, 1);
        assert_eq!(stored(&db, "a").routes, a.routes);
        assert_eq!(confirmations(&db), 0);

        // Confirmed: the route is added and the launcher's command remembered.
        let (result, shown) = link(&db, "a", "hub", true).await;
        assert!(result.unwrap().asked);
        assert_eq!(shown, 1);
        assert_eq!(stored(&db, "a").routes[0].source_id, "launcher:hub");
        assert!(launchers::is_confirmed(&db.lock(), &l).unwrap());

        // The next game links without a dialog.
        let (result, shown) = link(&db, "b", "hub", false).await;
        assert!(!result.unwrap().asked);
        assert_eq!(shown, 0);
        assert_eq!(stored(&db, "b").routes[0].source_id, "launcher:hub");
    }

    #[tokio::test]
    async fn a_changed_or_restored_launcher_asks_again() {
        let dir = TempDir::new("link-changed");
        let exe = dir.file("Hub/Hub.exe");
        let other_exe = dir.file("Other/Other.exe").to_string_lossy().into_owned();
        let games_dir = dir.path().join("Games").to_string_lossy().into_owned();
        let g = game("g", vec![local_route(&dir.file("Games/g/g.exe"))]);
        let l = launcher("hub", &exe.to_string_lossy(), &["file", "folder"]);
        let db = library(&[&l], &[&g]);
        launchers::record_confirmed(&db.lock(), &l).unwrap();

        // Any change to the command without its dialog (template, program, working folder,
        // target kinds) needs a new confirmation.
        let mut changes = Vec::new();
        for i in 0..4 {
            let mut changed = l.clone();
            match i {
                0 => changed.args_template = "--run {target}".into(),
                1 => changed.executable = other_exe.clone(),
                2 => changed.working_dir = Some(games_dir.clone()),
                _ => changed.target_kinds = vec!["file".into()],
            }
            changes.push(launchers::validate(changed).unwrap());
        }
        for changed in &changes {
            launchers::save(&db.lock(), changed).unwrap();
            assert!(
                !launchers::is_confirmed(&db.lock(), changed).unwrap(),
                "{changed:?}"
            );
            let (result, shown) = link(&db, "g", "hub", false).await;
            assert!(result.is_err());
            assert_eq!(shown, 1, "{changed:?}");
            assert_eq!(stored(&db, "g").routes, g.routes);
            launchers::save(&db.lock(), &l).unwrap();
        }
        // Back to exactly what was confirmed. Name, icon and colour don't count.
        let mut cosmetic = l.clone();
        cosmetic.name = "Renamed".into();
        cosmetic.color = "#123456".into();
        launchers::save(&db.lock(), &cosmetic).unwrap();
        assert!(launchers::is_confirmed(&db.lock(), &cosmetic).unwrap());

        // A launcher restored from a backup (even with the same command) asks again: its command
        // came from a file. The table itself is never in a backup.
        let all = launchers::list(&db.lock()).unwrap();
        let backup = crate::types::Backup {
            games: db::all_games(&db.lock()).unwrap(),
            settings: None,
            emulators: None,
            game_page_custom: None,
            game_notes: None,
            game_added: None,
            play_sessions: None,
            game_links: None,
            launchers: Some(all),
        };
        let json = serde_json::to_value(&backup).unwrap();
        let text = json.to_string();
        assert!(
            !text.contains("onfirmed") && !text.contains("exeKey") && !text.contains("cwdKey"),
            "{text}"
        );
        let parsed = crate::validation::backup(&json).unwrap();
        db::restore_backup(&mut db.lock(), &parsed).unwrap();
        assert!(!launchers::is_confirmed(&db.lock(), &cosmetic).unwrap());
        let (result, shown) = link(&db, "g", "hub", true).await;
        assert!(result.unwrap().asked);
        assert_eq!(shown, 1);
        // A new library restored from that backup has no confirmations at all.
        let fresh = Db::memory().unwrap();
        db::restore_backup(&mut fresh.lock(), &parsed).unwrap();
        assert_eq!(confirmations(&fresh), 0);

        // Deleting the launcher, or taking its program away, forgets the confirmation.
        assert_eq!(confirmations(&db), 1);
        launchers::remove(&db.lock(), "hub", launchers::RemoveMode::KeepGames).unwrap();
        assert_eq!(confirmations(&db), 0);
        let label = launcher("hub", "", &["file"]);
        launchers::record_confirmed(&db.lock(), &l).unwrap();
        launchers::record_confirmed(&db.lock(), &label).unwrap();
        assert_eq!(confirmations(&db), 0);
        assert!(!launchers::is_confirmed(&db.lock(), &label).unwrap());
    }

    #[tokio::test]
    async fn launch_options_are_always_confirmed() {
        let dir = TempDir::new("link-options");
        let exe = dir.file("Hub/Hub.exe");
        let g = game("g", vec![local_route(&dir.file("Games/g/g.exe"))]);
        let l = launcher("hub", &exe.to_string_lossy(), &["file"]);
        let db = library(&[&l], &[&g]);
        launchers::record_confirmed(&db.lock(), &l).unwrap();
        let shown = Cell::new(0);
        let result = assign_game(&db, "g", "hub", None, None, "-windowed".into(), |prompt| {
            assert!(prompt.contains("-windowed"), "{prompt}");
            shown.set(shown.get() + 1);
            async { false }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(
            shown.get(),
            1,
            "options need the dialog even for a confirmed launcher"
        );
        assert_eq!(stored(&db, "g").routes, g.routes);
    }

    #[tokio::test]
    async fn the_target_is_picked_by_route_then_kind() {
        let dir = TempDir::new("link-pick");
        let exe = dir.file("Hub/Hub.exe");
        let exe = exe.to_string_lossy();
        let install = dir.path().join("Steam/hk");
        std::fs::create_dir_all(&install).unwrap();
        let install_text = install.to_string_lossy().into_owned();
        let game_exe = dir.file("Games/hk/hk.exe");
        let game_text = game_exe.to_string_lossy().into_owned();
        let all = launcher("all", &exe, &["file", "folder", "identifier"]);
        let ids = launcher("ids", &exe, &["identifier"]);
        let files = launcher("files", &exe, &["file"]);
        // A Steam route (no file; an install folder that exists; a store id) and a local route,
        // in both orders.
        let steam_first = game(
            "s",
            vec![steam_route(Some(&install)), local_route(&game_exe)],
        );
        let local_first = game(
            "l",
            vec![local_route(&game_exe), steam_route(Some(&install))],
        );
        let db = library(&[&all, &ids, &files], &[&steam_first, &local_first]);
        for l in [&all, &ids, &files] {
            launchers::record_confirmed(&db.lock(), l).unwrap();
        }
        let pick = |a: CmdResult<Assigned>| {
            let c = a.unwrap().candidate.unwrap();
            (c.route_index, c.kind, c.target)
        };
        // The first route's own target wins; within it: file, then folder, then store id.
        assert_eq!(
            pick(link(&db, "s", "all", false).await.0),
            (0, TargetKind::Folder, install_text.clone())
        );
        assert_eq!(
            pick(link(&db, "l", "all", false).await.0),
            (0, TargetKind::File, game_text.clone())
        );
        // Only the launcher's kinds. (Routes added by a link are never a source.)
        assert_eq!(
            pick(link(&db, "l", "ids", false).await.0),
            (2, TargetKind::Identifier, "400".to_string())
        );
        assert_eq!(
            pick(link(&db, "s", "files", false).await.0),
            (2, TargetKind::File, game_text.clone())
        );
        // A file that no longer exists is skipped; with nothing left, the reason is given.
        let gone = game("x", vec![local_route(&dir.path().join("gone/x.exe"))]);
        db::save_game(&db.lock(), &GamePatch::from(&gone)).unwrap();
        let (result, shown) = link(&db, "x", "files", true).await;
        assert_eq!(
            result.err().unwrap(),
            launchers::no_candidate_reason(&files, &gone)
        );
        assert_eq!(shown, 0);
    }

    #[tokio::test]
    async fn a_launcher_without_a_program_only_labels_without_a_dialog() {
        let dir = TempDir::new("link-label");
        let g = game("g", vec![local_route(&dir.file("Games/g/g.exe"))]);
        let app = launcher("app", "", &["file"]);
        let db = library(&[&app], &[&g]);
        let (result, shown) = link(&db, "g", "app", false).await;
        let assigned = result.unwrap();
        assert_eq!(shown, 0);
        assert_eq!(assigned.candidate, None);
        let now = stored(&db, "g");
        assert_eq!(now.routes.len(), 1);
        assert_eq!(now.routes[0].launcher_id.as_deref(), Some("app"));
        assert_eq!(now.routes[0].assigned, Some(true));
        assert_eq!(confirmations(&db), 0);
        assert_eq!(launchers::unassign(&now, "app").unwrap(), g.routes);
    }

    #[tokio::test]
    async fn an_over_long_dialog_links_but_leaves_the_launcher_unconfirmed() {
        let dir = TempDir::new("link-long");
        let exe = dir.file("Hub/Hub.exe");
        let a = game("a", vec![local_route(&dir.file("Games/a/a.exe"))]);
        let b = game("b", vec![local_route(&dir.file("Games/b/b.exe"))]);
        // As many arguments as a launcher can have, each long: a dialog too long to be sure it
        // was read whole.
        let mut l = launcher("hub", &exe.to_string_lossy(), &["file"]);
        l.args_template = format!("{}{{target}}", "-abcdefghijklm ".repeat(127));
        let l = launchers::validate(l).unwrap();
        let db = library(&[&l], &[&a, &b]);
        let shown = std::cell::RefCell::new(String::new());
        let result = assign_game(&db, "a", "hub", None, None, String::new(), |prompt| {
            *shown.borrow_mut() = prompt;
            async move { true }
        })
        .await;
        assert!(
            shown.borrow().chars().count() > crate::links::MAX_CONFIRM_CHARS + 1_000,
            "the test needs an over-long dialog"
        );
        // The game is linked, as confirmed...
        assert!(result.unwrap().asked);
        assert_eq!(stored(&db, "a").routes[0].source_id, "launcher:hub");
        // ...but the launcher's command isn't remembered: the next link asks again.
        assert_eq!(confirmations(&db), 0);
        assert!(!launchers::is_confirmed(&db.lock(), &l).unwrap());
        let (result, asked) = link(&db, "b", "hub", true).await;
        assert!(result.unwrap().asked);
        assert_eq!(asked, 1);
        assert_eq!(confirmations(&db), 0);
    }

    /// [`save_launcher`] for `next` over the stored launcher `id` (`None`: a new launcher), with
    /// the dialog answered `answer`; returns the result and how often the dialog was shown.
    async fn save(
        db: &Db,
        id: Option<&str>,
        next: &CustomLauncher,
        answer: bool,
    ) -> (CmdResult<CustomLauncher>, usize) {
        let (stored, usage) = {
            let conn = db.lock();
            match id {
                Some(id) => (
                    launchers::get(&conn, id).unwrap(),
                    launcher_usage(&conn, id).unwrap(),
                ),
                None => (None, (false, Vec::new())),
            }
        };
        let shown = Cell::new(0);
        let result = save_launcher(db, id, stored, &usage, next.clone(), |prompt| {
            assert!(prompt.starts_with("Save this launcher?"), "{prompt}");
            shown.set(shown.get() + 1);
            async move { answer }
        })
        .await;
        (result, shown.get())
    }

    fn confirmed_at(db: &Db, id: &str) -> Option<i64> {
        db.lock()
            .query_row(
                "SELECT confirmedAt FROM launcher_confirmed WHERE launcherId=?",
                [id],
                |r| r.get(0),
            )
            .ok()
    }

    #[tokio::test]
    async fn saving_a_launcher_records_only_what_its_dialog_showed() {
        let dir = TempDir::new("save-confirmed");
        let exe = dir.file("Hub/Hub.exe");
        let l = launcher("hub", &exe.to_string_lossy(), &["file"]);
        let db = Db::memory().unwrap();

        // A new launcher: cancelled writes nothing; confirmed saves it and its confirmation.
        let (result, shown) = save(&db, None, &l, false).await;
        assert!(result.err().unwrap().contains("not saved"));
        assert_eq!(shown, 1);
        assert!(launchers::list(&db.lock()).unwrap().is_empty());
        assert_eq!(confirmations(&db), 0);
        let (result, shown) = save(&db, None, &l, true).await;
        let saved = result.unwrap();
        assert_eq!(shown, 1);
        assert!(launchers::is_confirmed(&db.lock(), &saved).unwrap());
        let first = confirmed_at(&db, "hub").unwrap();

        // A cosmetic change (name, icon, colour) saves without a dialog and keeps the row.
        let mut renamed = saved.clone();
        renamed.name = "Hub 2".into();
        renamed.color = "#FF0000".into();
        let (result, shown) = save(&db, Some("hub"), &renamed, false).await;
        let renamed = result.unwrap();
        assert_eq!(shown, 0);
        assert_eq!(
            launchers::get(&db.lock(), "hub").unwrap().unwrap().name,
            "Hub 2"
        );
        assert_eq!(confirmations(&db), 1);
        assert_eq!(confirmed_at(&db, "hub"), Some(first));
        assert!(launchers::is_confirmed(&db.lock(), &renamed).unwrap());

        // A command change asks; cancelled, nothing changes: the launcher and its row stay.
        let mut changed = renamed.clone();
        changed.args_template = "--run {target}".into();
        let (result, shown) = save(&db, Some("hub"), &changed, false).await;
        assert!(result.err().unwrap().contains("not saved"));
        assert_eq!(shown, 1);
        assert_eq!(
            launchers::get(&db.lock(), "hub").unwrap(),
            Some(renamed.clone())
        );
        assert_eq!(confirmed_at(&db, "hub"), Some(first));
        assert!(launchers::is_confirmed(&db.lock(), &renamed).unwrap());
        assert!(!launchers::is_confirmed(&db.lock(), &changed).unwrap());

        // Confirmed, the row is replaced by the new command: the old one is no longer covered.
        let (result, shown) = save(&db, Some("hub"), &changed, true).await;
        let changed = result.unwrap();
        assert_eq!(shown, 1);
        assert_eq!(confirmations(&db), 1);
        assert!(launchers::is_confirmed(&db.lock(), &changed).unwrap());
        assert!(!launchers::is_confirmed(&db.lock(), &renamed).unwrap());

        // A save over a launcher that changed meanwhile writes nothing either.
        let mut other = changed.clone();
        other.executable = dir.file("Other/Other.exe").to_string_lossy().into_owned();
        let stale = Some(renamed.clone());
        let usage = launcher_usage(&db.lock(), "hub").unwrap();
        let result = save_launcher(&db, Some("hub"), stale, &usage, other.clone(), |_| async {
            true
        })
        .await;
        assert!(result.err().unwrap().contains("changed in the meantime"));
        assert_eq!(
            launchers::get(&db.lock(), "hub").unwrap(),
            Some(changed.clone())
        );
        assert!(launchers::is_confirmed(&db.lock(), &changed).unwrap());
        assert!(!launchers::is_confirmed(&db.lock(), &other).unwrap());
    }

    #[test]
    fn the_one_click_command_takes_only_the_game_and_the_launcher() {
        // The renderer can't name a target, a kind, a route or launch options for it.
        let source = include_str!("launcher_commands.rs");
        let start = source.find("pub async fn launchers_link(").unwrap();
        let end = start + source[start..].find(") -> CmdResult<LinkedGame>").unwrap();
        let params: Vec<&str> = source[start..end]
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            params,
            [
                "app: AppHandle,",
                "window: WebviewWindow,",
                "state: State<'_, AppState>,",
                "game_id: String,",
                "launcher_id: String,"
            ]
        );
        // It passes no route, kind or launch options on.
        let body =
            source[end..end + source[end..].find("notify_library").unwrap()].replace("\r\n", "\n");
        assert!(
            body.contains("None,\n        None,\n        String::new(),"),
            "{body}"
        );
    }

    fn open_first(id: &str, exe: &str) -> CustomLauncher {
        launchers::validate(CustomLauncher {
            start_mode: launchers::StartMode::OpenFirst,
            args_template: "--open {target}".into(),
            ..launcher(id, exe, &["file", "folder"])
        })
        .unwrap()
    }

    #[tokio::test]
    async fn changing_the_start_mode_asks_with_the_new_modes_wording() {
        let dir = TempDir::new("save-mode");
        let exe = dir.file("Retro/Retro Project.exe");
        let through = CustomLauncher {
            args_template: "--open {target}".into(),
            ..launcher("retro", &exe.to_string_lossy(), &["file", "folder"])
        };
        let db = Db::memory().unwrap();
        let (result, _) = save(&db, None, &through, true).await;
        let through = result.unwrap();
        assert!(launchers::is_confirmed(&db.lock(), &through).unwrap());

        // Switching to "open first" is a command change: the dialog shows it, and cancelling it
        // changes nothing.
        let next = CustomLauncher {
            start_mode: launchers::StartMode::OpenFirst,
            ..through.clone()
        };
        let prompt = launchers::confirmation_prompt(&next).unwrap();
        assert!(prompt.contains("on its own (no arguments)"), "{prompt}");
        let (result, shown) = save(&db, Some("retro"), &next, false).await;
        assert!(result.is_err());
        assert_eq!(shown, 1);
        assert_eq!(
            launchers::get(&db.lock(), "retro").unwrap(),
            Some(through.clone())
        );
        let (result, shown) = save(&db, Some("retro"), &next, true).await;
        let saved = result.unwrap();
        assert_eq!(shown, 1);
        assert_eq!(saved.start_mode, launchers::StartMode::OpenFirst);
        // The confirmation is for the new mode only.
        assert!(launchers::is_confirmed(&db.lock(), &saved).unwrap());
        assert!(!launchers::is_confirmed(&db.lock(), &through).unwrap());
        // And back: asked again.
        let (result, shown) = save(&db, Some("retro"), &through, true).await;
        assert!(result.is_ok());
        assert_eq!(shown, 1);
    }

    #[tokio::test]
    async fn linking_to_an_open_first_launcher_shows_what_runs_and_then_takes_one_click() {
        let dir = TempDir::new("link-open-first");
        let exe = dir.file("Retro/Retro Project.exe");
        let a_exe = dir.file("PC Game/00356/Cairn.exe");
        let b_exe = dir.file("PC Game/00357/Other.exe");
        let l = open_first("retro", &exe.to_string_lossy());
        let a = game("a", vec![local_route(&a_exe)]);
        let b = game("b", vec![local_route(&b_exe)]);
        let folder_game = game(
            "f",
            vec![GameRoute {
                source_id: "steam".into(),
                store: "Steam".into(),
                launch_target: "steam://rungameid/1".into(),
                install_path: Some(dir.dir("Steam/F").to_string_lossy().into_owned()),
                installed: Some(true),
                ..Default::default()
            }],
        );
        let db = library(&[&l], &[&a, &b, &folder_game]);
        let shown = Cell::new(0);
        let ask = |prompt: String| {
            assert!(
                prompt.starts_with("When you play this game, Game Lobby opens:"),
                "{prompt}"
            );
            assert!(prompt.contains("on its own (no arguments)"), "{prompt}");
            assert!(!prompt.contains("--open"), "{prompt}");
            shown.set(shown.get() + 1);
            async { true }
        };
        let result = assign_game(&db, "a", "retro", None, None, String::new(), ask).await;
        assert!(result.unwrap().asked);
        assert_eq!(shown.get(), 1);
        assert!(launchers::is_confirmed(&db.lock(), &l).unwrap());
        let route = stored(&db, "a").routes[0].clone();
        assert_eq!(route.source_id, "launcher:retro");
        assert_eq!(route.launch_target, a_exe.to_string_lossy());
        // The next link of this launcher needs no dialog: it starts the game's own file too.
        let result = assign_game(&db, "b", "retro", None, None, String::new(), |_| async {
            panic!("no dialog")
        })
        .await;
        assert!(!result.unwrap().asked);
        // A folder can't start directly: refused before anything is saved.
        let e = assign_game(&db, "f", "retro", None, None, String::new(), |_| async {
            true
        })
        .await
        .err()
        .unwrap();
        assert!(e.contains("folder"), "{e}");
        assert_eq!(stored(&db, "f").routes, folder_game.routes);

        // Launch options for the route are the game's own .exe options, confirmed for that .exe.
        let host = crate::links::launcher_host(&l, &route).unwrap();
        assert_eq!(host.program, a_exe.to_string_lossy());
        assert!(host.base.is_empty());
        assert!(host.launcher.is_none());
        let own = crate::links::args_host(&db.lock(), &local_route(&a_exe)).unwrap();
        assert_eq!(host, own);
    }

    #[tokio::test]
    async fn an_open_first_link_to_another_launchers_file_always_asks() {
        let dir = TempDir::new("link-other-launcher");
        let exe = dir.file("Retro/Retro Project.exe");
        let hub_exe = dir.file("Hub/Hub.exe");
        let a_exe = dir.file("Hub Games/Cairn/Cairn.exe");
        let b_exe = dir.file("PC Game/00357/Other.exe");
        let l = open_first("retro", &exe.to_string_lossy());
        let hub = launcher("hub", &hub_exe.to_string_lossy(), &["file"]);
        // A game of the "hub" launcher (found in its games folder), and a plain local game.
        let hub_route = GameRoute {
            source_id: launchers::source_id("hub"),
            store: "Custom".into(),
            launch_target: a_exe.to_string_lossy().into_owned(),
            installed: Some(true),
            ..Default::default()
        };
        let a = game("a", vec![hub_route.clone()]);
        let b = game("b", vec![local_route(&b_exe)]);
        let db = library(&[&l, &hub], &[&a, &b]);
        launchers::record_confirmed(&db.lock(), &l).unwrap();
        assert!(launchers::takes_another_launchers_file(&l, &hub_route));

        // Confirmed, yet the hub's file asks: it would start outside the hub.
        let shown = Cell::new(0);
        let ask = |answer: bool| {
            let shown = &shown;
            move |prompt: String| {
                assert!(
                    prompt.contains("belongs to another launcher's game"),
                    "{prompt}"
                );
                assert!(prompt.contains("always asks first"), "{prompt}");
                assert!(!prompt.contains("without asking again"), "{prompt}");
                shown.set(shown.get() + 1);
                async move { answer }
            }
        };
        let e = assign_game(&db, "a", "retro", None, None, String::new(), ask(false))
            .await
            .err()
            .unwrap();
        assert!(e.contains("was not added"), "{e}");
        assert_eq!(shown.get(), 1);
        assert_eq!(stored(&db, "a").routes, a.routes);
        // Confirming links it, and doesn't confirm the launcher for later links.
        launchers::forget_confirmed(&db.lock(), "retro").unwrap();
        let assigned = assign_game(&db, "a", "retro", None, None, String::new(), ask(true))
            .await
            .unwrap();
        assert!(assigned.asked);
        assert_eq!(shown.get(), 2);
        assert!(!launchers::is_confirmed(&db.lock(), &l).unwrap());
        assert_eq!(confirmations(&db), 0);
        assert_eq!(stored(&db, "a").routes[0].source_id, "launcher:retro");
        // So the next ordinary link asks, and then confirms the launcher as before.
        let assigned = assign_game(&db, "b", "retro", None, None, String::new(), |prompt| {
            assert!(prompt.contains("without asking again"), "{prompt}");
            async { true }
        })
        .await
        .unwrap();
        assert!(assigned.asked);
        assert!(launchers::is_confirmed(&db.lock(), &l).unwrap());
    }

    #[tokio::test]
    async fn an_open_first_launchers_own_games_and_through_launchers_are_unaffected() {
        let dir = TempDir::new("link-own-launcher");
        let exe = dir.file("Retro/Retro Project.exe");
        let hub_exe = dir.file("Hub/Hub.exe");
        let own_exe = dir.file("Retro Games/Cairn/Cairn.exe");
        let other_exe = dir.file("Retro Games/Sly/Sly.exe");
        let l = open_first("retro", &exe.to_string_lossy());
        let own_route = GameRoute {
            source_id: launchers::source_id("retro"),
            store: "Custom".into(),
            launch_target: own_exe.to_string_lossy().into_owned(),
            installed: Some(true),
            ..Default::default()
        };
        // The launcher's own route is not "another launcher's"; a route naming no launcher is.
        assert!(!launchers::takes_another_launchers_file(&l, &own_route));
        let unnamed = GameRoute {
            source_id: "launcher:".into(),
            ..own_route.clone()
        };
        assert!(launchers::takes_another_launchers_file(&l, &unnamed));
        assert!(!launchers::takes_another_launchers_file(
            &l,
            &local_route(&own_exe)
        ));
        // A game from the open-first launcher's own games folder, linked to a "through"
        // launcher that was confirmed: one click as before (it gets the file as an argument,
        // which its confirmation covers).
        let hub = launcher("hub", &hub_exe.to_string_lossy(), &["file"]);
        assert!(!launchers::takes_another_launchers_file(&hub, &own_route));
        let own = game("own", vec![own_route.clone()]);
        let db = library(&[&l, &hub], &[&own]);
        launchers::record_confirmed(&db.lock(), &hub).unwrap();
        let assigned = assign_game(&db, "own", "hub", None, None, String::new(), |_| async {
            panic!("no dialog")
        })
        .await
        .unwrap();
        assert!(!assigned.asked);
        // The open-first launcher's own games are already its own: nothing to link.
        let other = game(
            "other",
            vec![GameRoute {
                launch_target: other_exe.to_string_lossy().into_owned(),
                ..own_route
            }],
        );
        db::save_game(&db.lock(), &GamePatch::from(&other)).unwrap();
        let e = assign_game(
            &db,
            "other",
            "retro",
            None,
            None,
            String::new(),
            |_| async { panic!("no dialog") },
        )
        .await
        .err()
        .unwrap();
        assert!(e.contains("already listed"), "{e}");
    }

    #[tokio::test]
    async fn an_open_first_link_says_a_rom_starts_with_its_emulator() {
        let dir = TempDir::new("link-open-first-rom");
        let exe = dir.file("Retro/Retro Project.exe");
        let rom = dir.file("Roms/GBA/Cairn Quest.gba");
        let l = open_first("retro", &exe.to_string_lossy());
        let rom_route = GameRoute {
            source_id: crate::local::ROM_SOURCE.into(),
            store: "Emulator".into(),
            launch_target: rom.to_string_lossy().into_owned(),
            system: Some("gba".into()),
            installed: Some(true),
            ..Default::default()
        };
        let g = game("g", vec![rom_route.clone()]);
        let h = game("h", vec![rom_route.clone()]);
        let db = library(&[&l], &[&g, &h]);
        // No emulator yet: said so.
        let result = assign_game(&db, "g", "retro", None, None, String::new(), |prompt| {
            assert!(
                prompt.contains("Then it starts the game directly: its ROM, with the emulator set up for its system (none is set up yet"),
                "{prompt}"
            );
            assert!(!prompt.contains("from its own file:"), "{prompt}");
            async { false }
        })
        .await;
        assert!(result.is_err());
        // With one: named.
        let emulator = dir.file("mGBA/mGBA.exe");
        let profile = crate::emulators::new_profile(
            &emulator,
            crate::catalog::known_emulator("mgba").unwrap(),
        );
        db::save_emulator(&db.lock(), &profile).unwrap();
        let name = profile.name.clone();
        let result = assign_game(&db, "h", "retro", None, None, String::new(), |prompt| {
            assert!(
                prompt.contains(&format!(
                    "Then it starts the game directly: its ROM, with the emulator {name}:"
                )),
                "{prompt}"
            );
            async { true }
        })
        .await;
        assert!(result.unwrap().asked);
    }
}
