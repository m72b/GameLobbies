//! Tauri commands for custom launchers (see `launchers` for the trust model). The renderer
//! sends ids, names, colours, icon names and checked text; every path comes from a native
//! picker opened here.

use crate::launchers::{self, CustomLauncher, LauncherInput, RemoveMode, SaveContext, TargetKind};
use crate::local_commands::confirm_natively;
use crate::picks::PickKind;
use crate::state::AppState;
use crate::types::Game;
use crate::{db, emulators};
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
    let (stored, accepted) = {
        let conn = state.db.lock();
        let stored = match &id {
            Some(id) => launchers::get(&conn, id).map_err(err)?,
            None => None,
        };
        let has_games = match &stored {
            Some(s) => !launchers::games_of(&conn, &s.id).map_err(err)?.is_empty(),
            None => false,
        };
        let accepted = launchers::accept_renderer_launcher(
            input,
            SaveContext {
                stored: stored.as_ref(),
                executable_pick: state.picks.latest(PickKind::LauncherExecutable),
                folder_pick: state.picks.latest(PickKind::LauncherFolder),
                has_games,
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

/// Deletes a launcher. `mode` `keep`: its games stay (Custom) without the launcher's route;
/// `remove`: games that only had that route are removed too.
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
        // Session state before the database, as in `roots_remove`.
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
    list(&state)
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

/// Adds a game to a launcher. File and folder launchers: a native picker opens here (cancel =
/// `null`); identifier launchers: `identifier` is checked (`launchers::check_identifier`).
/// An empty title uses the file or folder name. A game with the same target is reused.
#[tauri::command]
pub async fn launchers_add_game(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    launcher_id: String,
    title: String,
    identifier: Option<String>,
) -> CmdResult<Option<AddedLauncherGame>> {
    let launcher_id = id_arg(&launcher_id)?;
    if title.chars().count() > 300 {
        return Err("The title must be at most 300 characters.".into());
    }
    let launcher = launchers::get(&state.db.lock(), &launcher_id)
        .map_err(err)?
        .ok_or("That launcher no longer exists.")?;
    let kind = launcher.kind().map_err(err)?;
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
            text
        }
    };
    let game = launchers::new_game(&launcher, &title, &target).map_err(err)?;
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
