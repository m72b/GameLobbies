//! Tauri commands for library roots, emulator profiles, scan review items and local games.
//! Every renderer input is validated here before it reaches the database.

use crate::catalog;
use crate::commands::pick_file;
use crate::emulators;
use crate::local::{self, Resolution};
use crate::picks::PickKind;
use crate::review::{self, ReviewItem};
use crate::roots::{self, LibraryRoot, RootGuard};
use crate::scan::{self, LocalScope};
use crate::state::AppState;
use crate::tracker;
use crate::types::*;
use crate::{db, launch, validation};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

fn notify_library(app: &AppHandle, state: &AppState) {
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
}

fn notify_review(app: &AppHandle) {
    let _ = app.emit("review:changed", ());
}

/// Roots with `online` filled in; the filesystem is checked after the database lock is gone.
fn list_roots(state: &AppState) -> CmdResult<Vec<LibraryRoot>> {
    let roots = roots::list(&state.db.lock()).map_err(err)?;
    Ok(roots::with_online(roots))
}

/// Profiles with `available` filled in, the same way.
fn list_emulators(state: &AppState) -> CmdResult<Vec<EmulatorProfile>> {
    let profiles = db::emulator_profiles(&state.db.lock()).map_err(err)?;
    Ok(emulators::refresh_available(profiles))
}

fn id_arg(value: &str, what: &str) -> CmdResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 512 {
        return Err(format!("{what} is invalid."));
    }
    Ok(value.to_string())
}

// ---------------------------------------------------------------------------
// Library roots

#[tauri::command]
pub async fn roots_list(state: State<'_, AppState>) -> CmdResult<Vec<LibraryRoot>> {
    list_roots(&state)
}

fn root_pick_kind(kind: &str) -> CmdResult<PickKind> {
    PickKind::for_root(kind).ok_or_else(|| "Library folder kind is invalid.".into())
}

/// The folder a `roots_add` may add: the one the user just chose in `roots_browse` for the
/// same kind. Scans turn the executables and ROMs in a root into launchable games, so the
/// renderer can't name a folder by itself.
fn picked_root(picks: &crate::picks::PickedPaths, kind: &str, path: &str) -> CmdResult<String> {
    picks
        .find(root_pick_kind(kind)?, path)
        .ok_or_else(|| "Choose the folder with Browse… (it can't be typed in).".into())
}

/// Adds a root (`kind` = `emulators` | `games`) chosen with `roots_browse`, after validating
/// the path; returns the list.
#[tauri::command]
pub async fn roots_add(
    state: State<'_, AppState>,
    kind: String,
    path: String,
) -> CmdResult<Vec<LibraryRoot>> {
    let picked = picked_root(&state.picks, &kind, &path)?;
    let guard = RootGuard::from_environment(&state.data_dir);
    // Every filesystem check first, without the database lock.
    let (display, key) = roots::prepare(&kind, &picked, &guard).map_err(err)?;
    roots::insert(&state.db.lock(), &kind, &display, &key).map_err(err)?;
    state.picks.forget(root_pick_kind(&kind)?, &picked);
    state.tracker.library_changed();
    list_roots(&state)
}

/// Native folder picker for a root of `kind`; the chosen path (not yet added; `roots_add`
/// with the same kind accepts it for a while) or `null` when cancelled.
#[tauri::command]
pub async fn roots_browse(
    window: WebviewWindow,
    state: State<'_, AppState>,
    kind: String,
) -> CmdResult<Option<String>> {
    let pick_kind = root_pick_kind(&kind)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    let chosen = rx
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = &chosen {
        state.picks.remember(pick_kind, path);
    }
    Ok(chosen)
}

#[tauri::command]
pub async fn roots_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> CmdResult<Vec<LibraryRoot>> {
    let id = id_arg(&id, "Library folder")?;
    roots::set_enabled(&state.db.lock(), &id, enabled).map_err(err)?;
    list_roots(&state)
}

/// Removes a root. Its games become not installed (removed only without user data).
#[tauri::command]
pub async fn roots_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<LibraryRoot>> {
    let id = id_arg(&id, "Library folder")?;
    {
        let _scan = state.local_scan_lock.lock().await;
        let mut sessions = state.tracker.sessions();
        let mut conn = state.db.lock();
        if roots::get(&conn, &id).map_err(err)?.is_none() {
            return Err("That library folder no longer exists.".into());
        }
        let before: Vec<String> = db::all_games(&conn)
            .map_err(err)?
            .into_iter()
            .map(|g| g.id)
            .collect();
        local::remove_root(&mut conn, &id).map_err(err)?;
        for gone in before {
            if db::get_game(&conn, &gone).map_err(err)?.is_none() {
                sessions.forget(&gone);
            }
        }
    }
    notify_library(&app, &state);
    notify_review(&app);
    list_roots(&state)
}

/// Scans one root (`id`) or every enabled root (`null`); `full` raises the limits for very
/// large folders. Rejects while a folder scan runs.
#[tauri::command]
pub async fn roots_rescan(
    app: AppHandle,
    id: Option<String>,
    full: Option<bool>,
) -> CmdResult<local::ScanSummary> {
    let scope = match id {
        Some(id) => LocalScope::Root(id_arg(&id, "Library folder")?),
        None => LocalScope::All,
    };
    scan::scan_local(&app, scope, false, full.unwrap_or(false)).await
}

// ---------------------------------------------------------------------------
// Emulator profiles

#[tauri::command]
pub async fn emulators_list(state: State<'_, AppState>) -> CmdResult<Vec<EmulatorProfile>> {
    list_emulators(&state)
}

/// Creates or updates a profile. Fields left out keep their stored value (`systems`,
/// `enabled`); `source`, `detectedAs`, `available` and `needsConfiguration` are managed by
/// Game Lobby and ignored here. The executable must be the stored one or one just chosen
/// with `emulators_browse_executable`, and new or changed arguments must pass
/// `launch::check_template` (see `emulators::accept_renderer_profile`).
#[tauri::command]
pub async fn emulators_save(
    state: State<'_, AppState>,
    profile: Value,
) -> CmdResult<Vec<EmulatorProfile>> {
    let p = validation::emulator(&profile).map_err(err)?;
    launch::tokenize_template(&p.args).map_err(err)?;
    let has = |key: &str| profile.get(key).is_some_and(|v| !v.is_null());
    let conn = state.db.lock();
    let stored = db::emulator_profile(&conn, &p.id).map_err(err)?;
    let (mut p, used_pick) = emulators::accept_renderer_profile(stored.as_ref(), p, |path| {
        state.picks.find(PickKind::EmulatorExecutable, path)
    })
    .map_err(err)?;
    match stored {
        Some(stored) => {
            if !has("systems") {
                p.systems = stored.systems;
            }
            if !has("enabled") {
                p.enabled = stored.enabled;
            }
            p.source = stored.source;
            p.detected_as = stored.detected_as;
            p.added_at = stored.added_at;
        }
        None => {
            p.source = "manual".into();
            p.detected_as = std::path::Path::new(&p.executable)
                .file_name()
                .and_then(|n| catalog::emulator_for_executable(&n.to_string_lossy()))
                .map(|k| k.id.to_string());
        }
    }
    db::save_emulator(&conn, &p).map_err(err)?;
    drop(conn);
    if used_pick {
        state
            .picks
            .forget(PickKind::EmulatorExecutable, &p.executable);
    }
    state.tracker.library_changed();
    list_emulators(&state)
}

/// Deletes a profile; a rescan never re-adds its executable (see `emulators.ignored`).
/// Games that used it go back to the automatic choice.
#[tauri::command]
pub async fn emulators_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<EmulatorProfile>> {
    let id = id_arg(&id, "Emulator")?;
    {
        let conn = state.db.lock();
        if !db::remove_emulator(&conn, &id).map_err(err)? {
            return Err("That emulator profile no longer exists.".into());
        }
    }
    notify_library(&app, &state);
    list_emulators(&state)
}

/// Scans every enabled emulator root for known emulators.
#[tauri::command]
pub async fn emulators_scan(app: AppHandle) -> CmdResult<local::ScanSummary> {
    scan::scan_local(&app, LocalScope::Kind("emulators"), false, false).await
}

#[derive(Serialize)]
pub struct Catalog {
    systems: &'static [catalog::System],
    emulators: &'static [catalog::KnownEmulator],
}

/// The built-in systems and known emulators.
#[tauri::command]
pub fn emulators_catalog() -> Catalog {
    Catalog {
        systems: catalog::SYSTEMS,
        emulators: catalog::EMULATORS,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredPath {
    path: String,
    added_at: i64,
}

fn ignored(state: &AppState, kind: &str) -> CmdResult<Vec<IgnoredPath>> {
    Ok(db::ignores(&state.db.lock(), kind)
        .map_err(err)?
        .into_iter()
        .map(|(_, path, added_at)| IgnoredPath { path, added_at })
        .collect())
}

fn unignore(state: &AppState, kind: &str, path: &str) -> CmdResult<()> {
    let key = crate::localscan::path_key(path).ok_or("That path is invalid.")?;
    if !db::remove_ignore(&state.db.lock(), kind, &key).map_err(err)? {
        return Err("That path isn't on the list.".into());
    }
    Ok(())
}

/// Executables of removed emulator profiles (never re-added by scans).
#[tauri::command]
pub async fn emulators_ignored(state: State<'_, AppState>) -> CmdResult<Vec<IgnoredPath>> {
    ignored(&state, db::IGNORE_EMULATOR)
}

/// Lets scans find a removed emulator's executable again.
#[tauri::command]
pub async fn emulators_unignore(
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<Vec<IgnoredPath>> {
    unignore(&state, db::IGNORE_EMULATOR, &path)?;
    ignored(&state, db::IGNORE_EMULATOR)
}

/// Native file picker for an emulator `.exe`; the chosen path or `null`. `emulators_save`
/// accepts the chosen path for a while (see `picks`).
#[tauri::command]
pub async fn emulators_browse_executable(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    let chosen = pick_file(&window, "Emulator executable", &["exe"])
        .await
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = &chosen {
        state.picks.remember(PickKind::EmulatorExecutable, path);
    }
    Ok(chosen)
}

// ---------------------------------------------------------------------------
// Review

/// Open items, or all items with `includeResolved`.
#[tauri::command]
pub async fn review_list(
    state: State<'_, AppState>,
    include_resolved: Option<bool>,
) -> CmdResult<Vec<ReviewItem>> {
    review::list(&state.db.lock(), include_resolved.unwrap_or(false)).map_err(err)
}

/// Resolves an item (see `local::Resolution`); returns the open items.
#[tauri::command]
pub async fn review_resolve(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    resolution: Value,
) -> CmdResult<Vec<ReviewItem>> {
    let id = id_arg(&id, "Review item")?;
    if serde_json::to_string(&resolution).map_or(0, |s| s.len()) > 16_384 {
        return Err("Review resolution is too large.".into());
    }
    let resolution: Resolution = serde_json::from_value(resolution)
        .map_err(|e| format!("Review resolution is invalid: {e}"))?;
    // Off the async threads: resolving may read shortcuts through COM. The database is only
    // locked while the change is applied (`local::resolve`).
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> CmdResult<()> {
        let state = worker.state::<AppState>();
        // Session state before the database, as in `library_merge`.
        let mut sessions = state.tracker.sessions();
        let resolved = local::resolve(&state.db, &id, &resolution).map_err(err)?;
        let conn = state.db.lock();
        for (target, source) in resolved.merged {
            let tick = sessions.merge(&target, &source);
            tracker::apply(&conn, &mut sessions, tick.effects);
        }
        local::refresh_duplicates(&conn).map_err(err)
    })
    .await
    .map_err(err)??;
    notify_library(&app, &state);
    notify_review(&app);
    review::list(&state.db.lock(), false).map_err(err)
}

/// Game files and folders ignored by scans.
#[tauri::command]
pub async fn review_ignored(state: State<'_, AppState>) -> CmdResult<Vec<IgnoredPath>> {
    ignored(&state, db::IGNORE_PATH)
}

/// Lets scans pick an ignored file or folder up again (on the next scan).
#[tauri::command]
pub async fn review_unignore(
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<Vec<IgnoredPath>> {
    unignore(&state, db::IGNORE_PATH, &path)?;
    ignored(&state, db::IGNORE_PATH)
}

// ---------------------------------------------------------------------------
// Local games

/// Sets a ROM route's emulator; `null` = automatic (the system's default).
#[tauri::command]
pub async fn library_set_emulator(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    route_index: i64,
    emulator_id: Option<String>,
) -> CmdResult<Vec<Game>> {
    let game_id = id_arg(&game_id, "Game")?;
    if !(0..128).contains(&route_index) {
        return Err("Launch route is invalid.".into());
    }
    let emulator_id = emulator_id.map(|e| id_arg(&e, "Emulator")).transpose()?;
    let games = {
        let conn = state.db.lock();
        local::set_emulator(
            &conn,
            &game_id,
            route_index as usize,
            emulator_id.as_deref(),
        )
        .map_err(err)?;
        db::all_games(&conn).map_err(err)?
    };
    notify_library(&app, &state);
    Ok(games)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedLocalGame {
    game_id: String,
    games: Vec<Game>,
}

/// Native file picker for a game (.exe/.lnk/.url) or ROM file; adds it and returns the new
/// game's id with the library, or `null` when cancelled.
#[tauri::command]
pub async fn library_add_local_game(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<AddedLocalGame>> {
    let extensions = local::pickable_extensions();
    let refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let Some(path) = pick_file(&window, "Games and ROMs", &refs).await else {
        return Ok(None);
    };
    let confirmed = db::shortcut_confirmations(&state.db.lock()).map_err(err)?;
    let picked = path.clone();
    // Shortcuts are read through COM: on a thread of their own.
    let file = crate::platform::on_thread(move || local::game_for_file(&picked, &confirmed))
        .await
        .map_err(err)?
        .map_err(err)?;
    let game = match file {
        local::LocalFile::Game(game) => game,
        local::LocalFile::NeedsConfirmation(pending) => {
            local::request_shortcut_confirmation(&state.db.lock(), &pending).map_err(err)?;
            notify_review(&app);
            return Err(format!(
                "This shortcut runs {} with the arguments \"{}\". Check it under Review and confirm it to add the game.",
                pending.target, pending.arguments
            ));
        }
    };
    let identity = db::route_identity(&game.routes[0]);
    let (game_id, games) = {
        let mut conn = state.db.lock();
        db::upsert_imported_games(&mut conn, std::slice::from_ref(&game)).map_err(err)?;
        let games = db::all_games(&conn).map_err(err)?;
        // An existing game with the same file is reused instead of duplicated.
        let id = games
            .iter()
            .find(|g| g.routes.iter().any(|r| db::route_identity(r) == identity))
            .map(|g| g.id.clone())
            .unwrap_or(game.id);
        (id, games)
    };
    notify_library(&app, &state);
    Ok(Some(AddedLocalGame { game_id, games }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picks::PickedPaths;

    #[test]
    fn roots_are_added_only_from_a_recent_pick_of_the_same_kind() {
        let picks = PickedPaths::default();
        assert!(picked_root(&picks, "games", r"C:\Program Files\Vendor").is_err());
        picks.remember(PickKind::GamesFolder, r"D:\Roms");
        assert_eq!(picked_root(&picks, "games", "d:/roms").unwrap(), r"D:\Roms");
        assert!(picked_root(&picks, "games", r"D:\Roms\..\Other").is_err());
        // A games-folder pick can't become an emulator folder; unknown kinds are refused.
        assert!(picked_root(&picks, "emulators", r"D:\Roms").is_err());
        assert!(picked_root(&picks, "other", r"D:\Roms").is_err());
        // An emulator pick is not a folder pick.
        picks.remember(PickKind::EmulatorExecutable, r"D:\Tools");
        assert!(picked_root(&picks, "emulators", r"D:\Tools").is_err());
    }
}
