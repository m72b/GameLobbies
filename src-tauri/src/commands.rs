//! Tauri commands — one per channel of the Electron preload API (`window.lobby`).
//! Every input from the renderer is validated before it reaches the database.

use crate::launch::launch_game;
use crate::platform::shell_open;
use crate::secrets::{has_steam_api_key, set_steam_api_key};
use crate::state::AppState;
use crate::steamgriddb::{self, Cover, FetchSummary, KeyStatus, KeyTest};
use crate::tracker::RunningGame;
use crate::types::*;
use crate::{art, covercache, db, igdb, scan, secrets, theme, tracker, validation, windows};
use base64::Engine;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

fn notify_library(app: &AppHandle) {
    let _ = app.emit("library:changed", ());
}

fn notify_settings(app: &AppHandle, settings: Settings) -> Settings {
    let _ = app.emit("settings:changed", &settings);
    settings
}

pub(crate) async fn pick_file(
    window: &WebviewWindow,
    name: &str,
    extensions: &[&str],
) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(window)
        .add_filter(name, extensions)
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

// ---------------------------------------------------------------------------
// Window

#[tauri::command]
pub fn window_minimize(window: WebviewWindow) -> CmdResult<()> {
    window.minimize().map_err(err)
}

#[tauri::command]
pub fn window_toggle_maximize(window: WebviewWindow) -> CmdResult<()> {
    if window.is_maximized().map_err(err)? {
        window.unmaximize()
    } else {
        window.maximize()
    }
    .map_err(err)
}

#[tauri::command]
pub fn window_close(window: WebviewWindow) -> CmdResult<()> {
    window.close().map_err(err)
}

#[tauri::command]
pub fn window_is_maximized(window: WebviewWindow) -> CmdResult<bool> {
    window.is_maximized().map_err(err)
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Library

#[tauri::command]
pub async fn library_list(state: State<'_, AppState>) -> CmdResult<Vec<Game>> {
    db::all_games(&state.db.lock()).map_err(err)
}

#[tauri::command]
pub async fn library_scan(app: AppHandle) -> CmdResult<LibraryStats> {
    scan::scan_libraries(&app).await
}

fn connections(state: &AppState) -> anyhow::Result<LibraryConnections> {
    let values = db::get_connection_values(&state.db.lock())?;
    Ok(LibraryConnections {
        steam_id: values.steam_id,
        steam_api_key_set: has_steam_api_key(state)?,
        legendary_path: values.legendary_path,
        gog_database_path: values.gog_database_path,
    })
}

#[tauri::command]
pub async fn library_connections(state: State<'_, AppState>) -> CmdResult<LibraryConnections> {
    connections(&state).map_err(err)
}

#[tauri::command]
pub async fn library_save_connections(
    state: State<'_, AppState>,
    input: Value,
) -> CmdResult<LibraryConnections> {
    let input = validation::connections(&input).map_err(err)?;
    db::put_connection_values(
        &state.db.lock(),
        &ConnectionValues {
            steam_id: input.steam_id,
            legendary_path: input.legendary_path,
            gog_database_path: input.gog_database_path,
        },
    )
    .map_err(err)?;
    if let Some(key) = input.steam_api_key.filter(|k| !k.trim().is_empty()) {
        set_steam_api_key(&state, &key).map_err(err)?;
    }
    connections(&state).map_err(err)
}

#[tauri::command]
pub async fn library_choose_legendary(window: WebviewWindow) -> CmdResult<Option<String>> {
    Ok(pick_file(&window, "Legendary executable", &["exe"])
        .await
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn library_choose_gog_database(window: WebviewWindow) -> CmdResult<Option<String>> {
    Ok(
        pick_file(&window, "SQLite database", &["db", "sqlite", "sqlite3"])
            .await
            .map(|p| p.to_string_lossy().into_owned()),
    )
}

#[tauri::command]
pub async fn library_launch(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    route: Option<i64>,
) -> CmdResult<bool> {
    let route = route.unwrap_or(0);
    if !(0..128).contains(&route) {
        return Err("Launch route is invalid.".into());
    }
    let game = db::get_game(&state.db.lock(), &id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    launch_game(&app, &game, route as usize)
        .await
        .map_err(err)?;
    Ok(true)
}

#[tauri::command]
pub async fn library_save(
    app: AppHandle,
    state: State<'_, AppState>,
    game: Value,
) -> CmdResult<Vec<Game>> {
    let mut patch = validation::game_patch(&game).map_err(err)?;
    let games = {
        let conn = state.db.lock();
        // A remote cover that is already downloaded is stored as its local file.
        let covers = state.covers_dir();
        for value in [&mut patch.cover, &mut patch.default_cover] {
            if let Some(v) = value.as_mut() {
                *v = covercache::cached_value(&conn, &covers, v);
            }
        }
        let games = db::save_game(&conn, &patch).map_err(err)?;
        if patch.cover.is_some() || patch.default_cover.is_some() {
            if let Err(e) = covercache::sweep(&conn, &covers) {
                eprintln!("Could not clean up the cover cache: {e:#}");
            }
        }
        games
    };
    if covercache::needs_caching(patch.cover.as_deref())
        || covercache::needs_caching(patch.default_cover.as_deref())
    {
        covercache::spawn_pass(&app, std::time::Duration::ZERO);
    }
    state.tracker.library_changed();
    Ok(games)
}

#[tauri::command]
pub async fn library_merge(
    state: State<'_, AppState>,
    target_id: String,
    source_id: String,
) -> CmdResult<Vec<Game>> {
    // Session state before the database (see `tracker`): no poll runs in between.
    let games = {
        let mut sessions = state.tracker.sessions();
        let games = db::merge_games(&mut state.db.lock(), &target_id, &source_id).map_err(err)?;
        let tick = sessions.merge(&target_id, &source_id);
        tracker::apply(&state.db.lock(), &mut sessions, tick.effects);
        games
    };
    covercache::clean_up(&state);
    state.tracker.library_changed();
    Ok(games)
}

#[tauri::command]
pub async fn library_remove(state: State<'_, AppState>, id: String) -> CmdResult<Vec<Game>> {
    let games = {
        let mut sessions = state.tracker.sessions();
        let games = db::remove_game(&state.db.lock(), &id).map_err(err)?;
        sessions.forget(&id);
        games
    };
    covercache::clean_up(&state);
    state.tracker.library_changed();
    Ok(games)
}

#[tauri::command]
pub async fn library_reset_playtime(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<Game>> {
    let games = {
        let mut sessions = state.tracker.sessions();
        let games = db::reset_playtime(&mut state.db.lock()).map_err(err)?;
        sessions.reset(now_ms());
        games
    };
    state.tracker.wake();
    notify_library(&app);
    Ok(games)
}

#[tauri::command]
pub async fn library_add(
    state: State<'_, AppState>,
    title: String,
    target: String,
) -> CmdResult<Vec<Game>> {
    let (title, target) = (title.trim().to_string(), target.trim().to_string());
    if title.is_empty()
        || title.chars().count() > 300
        || target.is_empty()
        || target.chars().count() > 8_192
    {
        return Err("Enter a title and a launch target.".into());
    }
    let game = Game {
        id: format!("custom:{}", uuid::Uuid::new_v4()),
        title,
        store: "Custom".into(),
        routes: vec![GameRoute {
            source_id: "custom".into(),
            store: "Custom".into(),
            launch_target: target,
            installed: Some(true),
            ..Default::default()
        }],
        ..Default::default()
    };
    let games = db::save_game(&state.db.lock(), &GamePatch::from(&game)).map_err(err)?;
    state.tracker.library_changed();
    Ok(games)
}

static DATA_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^data:image/(png|jpeg|webp|gif|avif);base64,([A-Za-z\d+/=]+)$").unwrap()
});

fn safe_cover_name(id: &str, ext: &str) -> String {
    let clean: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{clean}-{}{}", uuid::Uuid::new_v4(), ext.to_lowercase())
}

fn cover_url(name: &str) -> String {
    format!(
        "lobby-cover://{}",
        crate::platform::encode_uri_component(name)
    )
}

#[tauri::command]
pub async fn library_set_cover(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    data_url: Option<String>,
) -> CmdResult<Vec<Game>> {
    let game = db::get_game(&state.db.lock(), &id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let cover = match data_url {
        Some(data_url) => {
            let captures = DATA_URL
                .captures(&data_url)
                .filter(|c| c[2].len() <= 18_000_000)
                .ok_or("Choose an image smaller than 12 MB.")?;
            let ext = if &captures[1] == "jpeg" {
                ".jpg".to_string()
            } else {
                format!(".{}", &captures[1])
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&captures[2])
                .map_err(err)?;
            std::fs::create_dir_all(state.covers_dir()).map_err(err)?;
            let name = safe_cover_name(&id, &ext);
            std::fs::write(state.covers_dir().join(&name), bytes).map_err(err)?;
            cover_url(&name)
        }
        // Resetting with no store cover clears the custom one. The default cover may be a
        // cached file already; otherwise the background pass caches it.
        None => game.default_cover.clone().unwrap_or_default(),
    };
    let remote = covercache::needs_caching(Some(&cover));
    let games = {
        let conn = state.db.lock();
        let games = db::save_game(
            &conn,
            &GamePatch {
                cover: Some(cover),
                ..GamePatch::new(id)
            },
        )
        .map_err(err)?;
        if let Err(e) = covercache::sweep(&conn, &state.covers_dir()) {
            eprintln!("Could not clean up the cover cache: {e:#}");
        }
        games
    };
    if remote {
        covercache::spawn_pass(&app, std::time::Duration::ZERO);
    }
    notify_library(&app);
    Ok(games)
}

#[tauri::command]
pub async fn library_choose_cover(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<Game>> {
    if let Some(source) =
        pick_file(&window, "Images", &["png", "jpg", "jpeg", "webp", "avif"]).await
    {
        if db::get_game(&state.db.lock(), &id).map_err(err)?.is_some() {
            let ext = source
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_else(|| ".jpg".into());
            let name = safe_cover_name(&id, &ext);
            std::fs::create_dir_all(state.covers_dir()).map_err(err)?;
            std::fs::copy(&source, state.covers_dir().join(&name)).map_err(err)?;
            db::save_game(
                &state.db.lock(),
                &GamePatch {
                    cover: Some(cover_url(&name)),
                    ..GamePatch::new(id)
                },
            )
            .map_err(err)?;
            covercache::clean_up(&state);
            notify_library(&app);
        }
    }
    db::all_games(&state.db.lock()).map_err(err)
}

#[tauri::command]
pub async fn library_open_install_folder(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let game = db::get_game(&state.db.lock(), &id).map_err(err)?;
    let folder = game.and_then(|g| {
        g.routes
            .into_iter()
            .find_map(|r| r.install_path.filter(|p| !p.is_empty()))
    });
    if let Some(folder) = folder.filter(|f| Path::new(f).is_dir()) {
        shell_open(folder).await.map_err(err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings and themes

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> CmdResult<Settings> {
    db::get_settings(&state.db.lock()).map_err(err)
}

#[tauri::command]
pub async fn settings_save(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Value,
) -> CmdResult<Settings> {
    let patch = validation::settings_patch(&settings).map_err(err)?;
    let saved = db::put_settings(&state.db.lock(), patch).map_err(err)?;
    scan::configure_schedule(&app, saved.scan_interval_hours);
    state.tracker.wake();
    Ok(notify_settings(&app, saved))
}

// Must be async: creating a window from a synchronous (main-thread) command deadlocks on Windows.
/// `section`: optional page to show (only the ids in `windows::SETTINGS_SECTIONS`).
#[tauri::command]
pub async fn settings_open(app: AppHandle, section: Option<String>) -> CmdResult<()> {
    let section = match section.as_deref() {
        None => None,
        Some(id) => Some(
            windows::SETTINGS_SECTIONS
                .iter()
                .copied()
                .find(|known| *known == id)
                .ok_or_else(|| "Unknown settings section.".to_string())?,
        ),
    };
    windows::open_settings_at(&app, section).map_err(err)
}

#[tauri::command]
pub async fn theme_load(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> CmdResult<Settings> {
    let url = validation::https_url(&url).map_err(err)?;
    let theme = theme::load_remote(&url).await.map_err(err)?;
    let mut patch = Map::new();
    patch.insert("themeName".into(), json!(theme.name));
    patch.insert("theme".into(), serde_json::to_value(&theme).map_err(err)?);
    patch.insert("themeUrl".into(), json!(url));
    let saved = db::put_settings(&state.db.lock(), patch).map_err(err)?;
    Ok(notify_settings(&app, saved))
}

#[tauri::command]
pub async fn theme_reset(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Settings> {
    let mut patch = Map::new();
    patch.insert(
        "theme".into(),
        serde_json::to_value(ThemeTokens::default()).map_err(err)?,
    );
    patch.insert("themeUrl".into(), Value::Null);
    patch.insert("themeName".into(), Value::Null);
    let saved = db::put_settings(&state.db.lock(), patch).map_err(err)?;
    Ok(notify_settings(&app, saved))
}

// ---------------------------------------------------------------------------
// Emulators, plugins, backups

#[tauri::command]
pub async fn plugins_list(state: State<'_, AppState>) -> CmdResult<Vec<Value>> {
    db::plugins(&state.db.lock()).map_err(err)
}

pub(crate) fn read_limited_json(path: &Path, max: u64, what: &str) -> anyhow::Result<Value> {
    if std::fs::metadata(path)?.len() > max {
        anyhow::bail!("{what} is too large.");
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[tauri::command]
pub async fn plugins_install(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Vec<Value>> {
    if let Some(path) = pick_file(&window, "Plugin manifests", &["json"]).await {
        let raw = read_limited_json(&path, 64 * 1024, "Plugin manifest").map_err(err)?;
        let manifest = validation::plugin_manifest(&raw).map_err(err)?;
        db::save_plugin(&state.db.lock(), &manifest).map_err(err)?;
    }
    db::plugins(&state.db.lock()).map_err(err)
}

#[tauri::command]
pub async fn backup_export(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_file_name("game-lobby-backup.json")
        .add_filter("JSON", &["json"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let backup = {
        let conn = state.db.lock();
        // Cached covers are exported as their original URLs, which work on any machine.
        let mut games = db::all_games(&conn).map_err(err)?;
        for game in &mut games {
            for value in [&mut game.cover, &mut game.default_cover] {
                if let Some(v) = value.as_mut() {
                    *v = covercache::source_of(&conn, v).map_err(err)?;
                }
            }
        }
        Backup {
            games,
            settings: Some(db::get_settings(&conn).map_err(err)?),
            emulators: Some(db::emulator_profiles(&conn).map_err(err)?),
            game_page_custom: Some(db::all_page_custom(&conn).map_err(err)?)
                .filter(|entries| !entries.is_empty()),
        }
    };
    std::fs::write(&path, serde_json::to_string_pretty(&backup).map_err(err)?).map_err(err)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn backup_import(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> CmdResult<usize> {
    let Some(path) = pick_file(&window, "JSON", &["json"]).await else {
        return Ok(0);
    };
    let raw = read_limited_json(&path, 128 * 1024 * 1024, "Backup file").map_err(err)?;
    let backup = validation::backup(&raw).map_err(err)?;
    let count = db::restore_backup(&mut state.db.lock(), &backup).map_err(err)?;
    state.tracker.library_changed();
    notify_library(&app);
    covercache::spawn_pass(&app, std::time::Duration::ZERO);
    if backup.settings.is_some() {
        if let Ok(settings) = db::get_settings(&state.db.lock()) {
            notify_settings(&app, settings);
        }
    }
    for entry in backup.game_page_custom.iter().flatten() {
        let _ = app.emit(
            "art:changed",
            ArtChanged {
                game_id: &entry.game_id,
            },
        );
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// SteamGridDB cover art. The API key is write-only from the renderer's side.

#[tauri::command]
pub async fn steamgriddb_status(state: State<'_, AppState>) -> CmdResult<KeyStatus> {
    steamgriddb::status(&state).map_err(err)
}

/// Saves the key, or clears it when `key` is empty or whitespace.
#[tauri::command]
pub async fn steamgriddb_save_key(state: State<'_, AppState>, key: String) -> CmdResult<KeyStatus> {
    secrets::set_steamgriddb_api_key(&state, &key).map_err(err)?;
    steamgriddb::status(&state).map_err(err)
}

/// Checks the saved key with one authenticated request; never returns the key.
#[tauri::command]
pub async fn steamgriddb_test_key(state: State<'_, AppState>) -> CmdResult<KeyTest> {
    Ok(steamgriddb::test_key(&state).await)
}

#[tauri::command]
pub async fn steamgriddb_search(
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<Vec<Cover>> {
    steamgriddb::search(&state, &game_id).await.map_err(err)
}

#[tauri::command]
pub async fn steamgriddb_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    url: String,
) -> CmdResult<Vec<Game>> {
    let games = steamgriddb::apply(&state, &game_id, &url)
        .await
        .map_err(err)?;
    // Not downloaded (offline, CDN error): shown from the CDN for now; retried in the background.
    let remote = games
        .iter()
        .find(|g| g.id == game_id)
        .is_some_and(|g| covercache::needs_caching(g.cover.as_deref()));
    if remote {
        covercache::spawn_pass(&app, std::time::Duration::ZERO);
    }
    notify_library(&app);
    Ok(games)
}

#[tauri::command]
pub async fn steamgriddb_fetch_missing(app: AppHandle) -> CmdResult<FetchSummary> {
    steamgriddb::fetch_missing(&app).await.map_err(err)
}

// ---------------------------------------------------------------------------
// Play tracking and game page art

/// Games currently launching or running (`RunningGame[]`); also pushed as `tracking:changed`.
#[tauri::command]
pub async fn tracking_status(state: State<'_, AppState>) -> CmdResult<Vec<RunningGame>> {
    Ok(state.tracker.status())
}

/// A game's play sessions, newest first. `limit` defaults to 50 (1 to 1000).
#[tauri::command]
pub async fn tracking_sessions(
    state: State<'_, AppState>,
    game_id: String,
    limit: Option<i64>,
) -> CmdResult<Vec<PlaySession>> {
    let limit = limit.unwrap_or(50).clamp(1, 1000);
    db::play_sessions(&state.db.lock(), &game_id, limit).map_err(err)
}

#[tauri::command]
pub async fn tracking_stats(state: State<'_, AppState>, game_id: String) -> CmdResult<PlayStats> {
    db::play_stats(&state.db.lock(), &game_id, now_ms()).map_err(err)
}

/// Adds or removes a game from `trackingExcludedGameIds`. A running session of a newly
/// excluded game is closed on the next poll (triggered right away). Emits `settings:changed`.
#[tauri::command]
pub async fn tracking_set_excluded(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    excluded: bool,
) -> CmdResult<Settings> {
    let saved = {
        let conn = state.db.lock();
        let mut ids = db::get_settings(&conn)
            .map_err(err)?
            .tracking_excluded_game_ids;
        ids.retain(|id| *id != game_id);
        if excluded {
            ids.push(game_id);
        }
        let mut patch = Map::new();
        patch.insert("trackingExcludedGameIds".into(), json!(ids));
        let patch = validation::settings_patch(&Value::Object(patch)).map_err(err)?;
        db::put_settings(&conn, patch).map_err(err)?
    };
    state.tracker.wake();
    Ok(notify_settings(&app, saved))
}

/// Hero/logo candidate URLs for the game page; empty lists when nothing is available.
#[tauri::command]
pub async fn art_get(state: State<'_, AppState>, game_id: String) -> CmdResult<GameArt> {
    art::get(&state, &game_id).await.map_err(err)
}

// ---------------------------------------------------------------------------
// Game page customization. Every change returns the updated `GamePageCustom` and emits
// `art:changed` with `{ gameId }`.

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ArtChanged<'a> {
    game_id: &'a str,
}

fn notify_art(app: &AppHandle, game_id: &str, custom: GamePageCustom) -> GamePageCustom {
    let _ = app.emit("art:changed", ArtChanged { game_id });
    custom
}

fn set_page_image(
    app: &AppHandle,
    state: &AppState,
    game_id: &str,
    slot: art::Slot,
    url: Option<&str>,
) -> CmdResult<GamePageCustom> {
    let custom =
        art::set_image(&state.db.lock(), &state.covers_dir(), game_id, slot, url).map_err(err)?;
    Ok(notify_art(app, game_id, custom))
}

/// Sets the custom hero (SteamGridDB https URL or an existing `lobby-cover://` name);
/// `null` goes back to the automatic art.
#[tauri::command]
pub async fn art_set_hero(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    url: Option<String>,
) -> CmdResult<GamePageCustom> {
    set_page_image(&app, &state, &game_id, art::Slot::Hero, url.as_deref())
}

/// Like `art_set_hero`, for the logo.
#[tauri::command]
pub async fn art_set_logo(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    url: Option<String>,
) -> CmdResult<GamePageCustom> {
    set_page_image(&app, &state, &game_id, art::Slot::Logo, url.as_deref())
}

async fn choose_page_image(
    app: &AppHandle,
    window: &WebviewWindow,
    state: &AppState,
    game_id: &str,
    slot: art::Slot,
) -> CmdResult<GamePageCustom> {
    if db::get_game(&state.db.lock(), game_id)
        .map_err(err)?
        .is_none()
    {
        return Err("Game not found.".into());
    }
    let filter = match slot {
        art::Slot::Hero => "Images",
        art::Slot::Logo => "Logo images (transparent PNG or WebP work best)",
    };
    let Some(source) = pick_file(window, filter, validation::PAGE_IMAGE_EXTENSIONS).await else {
        // Cancelled: nothing changes.
        return db::page_custom(&state.db.lock(), game_id).map_err(err);
    };
    let covers = state.covers_dir();
    let value = art::import_image_file(&covers, game_id, slot, &source).map_err(err)?;
    let saved = art::set_image(&state.db.lock(), &covers, game_id, slot, Some(&value));
    match saved {
        Ok(custom) => Ok(notify_art(app, game_id, custom)),
        Err(e) => {
            // Nothing references the new copy yet (e.g. the game was removed meanwhile).
            if let Some(name) = validation::lobby_cover_name(&value) {
                let _ = std::fs::remove_file(covers.join(name));
            }
            Err(err(e))
        }
    }
}

/// Opens a file dialog and copies the chosen image (PNG/JPEG/WebP/AVIF, at most 20 MB) into
/// the covers folder as the custom hero. Cancelling returns the current value unchanged.
#[tauri::command]
pub async fn art_choose_hero_file(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<GamePageCustom> {
    choose_page_image(&app, &window, &state, &game_id, art::Slot::Hero).await
}

/// Like `art_choose_hero_file`, for the logo.
#[tauri::command]
pub async fn art_choose_logo_file(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<GamePageCustom> {
    choose_page_image(&app, &window, &state, &game_id, art::Slot::Logo).await
}

/// Logo placement: `position` is bottom-left | top-center | center | bottom-center and
/// `scale` a finite number from 0.5 to 2.0.
#[tauri::command]
pub async fn art_set_logo_layout(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    position: String,
    scale: f64,
) -> CmdResult<GamePageCustom> {
    let custom = art::set_logo_layout(&state.db.lock(), &game_id, &position, scale).map_err(err)?;
    Ok(notify_art(&app, &game_id, custom))
}

/// SteamGridDB hero candidates for the picker (not cached).
#[tauri::command]
pub async fn steamgriddb_search_heroes(
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<Vec<Cover>> {
    steamgriddb::search_art(&state, &game_id, steamgriddb::ArtKind::Hero)
        .await
        .map_err(err)
}

/// SteamGridDB logo candidates for the picker (not cached).
#[tauri::command]
pub async fn steamgriddb_search_logos(
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<Vec<Cover>> {
    steamgriddb::search_art(&state, &game_id, steamgriddb::ArtKind::Logo)
        .await
        .map_err(err)
}

// ---------------------------------------------------------------------------
// IGDB metadata. The Client Secret (and the access token) never reach the renderer; the
// renderer can only look up its own games by id, or search plain text.

#[tauri::command]
pub async fn igdb_status(state: State<'_, AppState>) -> CmdResult<igdb::CredentialStatus> {
    igdb::status(&state).map_err(err)
}

/// Saves the non-empty fields (empty keeps the stored value).
#[tauri::command]
pub async fn igdb_save_credentials(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
) -> CmdResult<igdb::CredentialStatus> {
    igdb::save_credentials(&state, &client_id, &client_secret).map_err(err)
}

#[tauri::command]
pub async fn igdb_clear_credentials(
    state: State<'_, AppState>,
) -> CmdResult<igdb::CredentialStatus> {
    igdb::clear_credentials(&state).map_err(err)
}

#[tauri::command]
pub async fn igdb_test(state: State<'_, AppState>) -> CmdResult<igdb::TestResult> {
    Ok(igdb::test(&state).await)
}

/// Cached metadata at once; a background fetch follows when needed (`igdb:metadata`).
#[tauri::command]
pub async fn igdb_get(app: AppHandle, game_id: String) -> CmdResult<igdb::MetadataView> {
    igdb::get(&app, &game_id).map_err(err)
}

#[tauri::command]
pub async fn igdb_search(
    state: State<'_, AppState>,
    game_id: String,
    query: Option<String>,
) -> CmdResult<Vec<igdb::SearchResult>> {
    if query.as_deref().is_some_and(|q| q.len() > 1024) {
        return Err("The search text is too long.".into());
    }
    igdb::search(&state, &game_id, query.as_deref())
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn igdb_choose(
    app: AppHandle,
    game_id: String,
    igdb_id: u64,
) -> CmdResult<igdb::MetadataView> {
    igdb::choose(&app, &game_id, igdb_id).await.map_err(err)
}

#[tauri::command]
pub async fn igdb_reset_match(app: AppHandle, game_id: String) -> CmdResult<igdb::MetadataView> {
    igdb::reset_match(&app, &game_id).map_err(err)
}

/// Starts the background fetch for every game; returns at once (`igdb:progress`).
#[tauri::command]
pub async fn igdb_fetch_all(app: AppHandle) -> CmdResult<igdb::BulkProgress> {
    igdb::fetch_all(&app).map_err(err)
}

#[tauri::command]
pub async fn igdb_cancel_fetch_all(state: State<'_, AppState>) -> CmdResult<()> {
    igdb::cancel_all(&state);
    Ok(())
}

#[tauri::command]
pub async fn igdb_progress(state: State<'_, AppState>) -> CmdResult<igdb::BulkProgress> {
    Ok(igdb::progress(&state))
}
