//! Game Lobby — a unified local game library and launcher (Tauri backend).

mod art;
mod catalog;
mod commands;
mod covercache;
mod db;
mod emulators;
mod igdb;
mod importers;
mod launch;
mod local;
mod local_commands;
mod localscan;
mod picks;
mod platform;
mod review;
mod roots;
mod scan;
mod screenshots;
mod secrets;
mod state;
mod steamgriddb;
mod storage;
mod theme;
mod tracker;
mod types;
mod updater;
mod validation;
mod windows;

use state::AppState;
use std::borrow::Cow;
use std::sync::Mutex;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext};

/// The file a `lobby-cover` request refers to: only a bare file name inside the covers folder.
/// The query (e.g. a `?v=` cache-buster) and fragment are ignored.
fn cover_file(covers_dir: &std::path::Path, uri: &tauri::http::Uri) -> Option<std::path::PathBuf> {
    let raw = uri.path().trim_start_matches('/');
    let name = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let file_name = std::path::Path::new(name.as_ref()).file_name()?;
    Some(covers_dir.join(file_name))
}

/// Serves custom covers: `lobby-cover://<name>` (http://lobby-cover.localhost/<name> in WebView2).
fn cover_protocol<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"Not found"[..]))
            .unwrap()
    };
    let Some(state) = ctx.app_handle().try_state::<AppState>() else {
        return not_found();
    };
    let Some(file) = cover_file(&state.covers_dir(), request.uri()) else {
        return not_found();
    };
    let Ok(bytes) = std::fs::read(&file) else {
        return not_found();
    };
    let mime = match file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(bytes))
        .unwrap()
}

/// Development only: `GAME_LOBBY_DATA_DIR` (an absolute folder) replaces the data folder, so
/// a debug build can run against a throwaway library. Release builds ignore it.
#[cfg(debug_assertions)]
fn data_dir_override() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("GAME_LOBBY_DATA_DIR")?);
    if dir.as_os_str().is_empty() || !dir.is_absolute() {
        eprintln!("GAME_LOBBY_DATA_DIR must be an absolute folder path; ignoring it.");
        return None;
    }
    Some(dir)
}

#[cfg(not(debug_assertions))]
fn data_dir_override() -> Option<std::path::PathBuf> {
    None
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Signed updates; used only by the `updates_*` commands (no webview permissions).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::Updates::default())
        .register_uri_scheme_protocol("lobby-cover", cover_protocol)
        .register_uri_scheme_protocol("lobby-shot", screenshots::protocol)
        .setup(|app| {
            let data_dir = match data_dir_override() {
                Some(dir) => {
                    // A test library: never copy the user's Electron library into it.
                    eprintln!("Using the development data folder {}", dir.display());
                    dir
                }
                None => {
                    let data_dir = app.path().app_data_dir()?;
                    match db::migrate_from_electron(&data_dir) {
                        Ok(true) => {
                            eprintln!("Copied the library from the Electron build of Game Lobby.")
                        }
                        Ok(false) => {}
                        Err(e) => eprintln!("Could not copy the Electron library: {e:#}"),
                    }
                    data_dir
                }
            };
            let database = db::Db::open(&data_dir.join("library.sqlite"))?;
            let hours = db::get_settings(&database.lock())?.scan_interval_hours;
            app.manage(AppState {
                db: database,
                data_dir,
                tracker: Default::default(),
                scan_lock: tokio::sync::Mutex::new(()),
                last_scan: Mutex::new(None),
                scan_timer: Mutex::new(None),
                cover_fetch_lock: tokio::sync::Mutex::new(()),
                local_scan_lock: tokio::sync::Mutex::new(()),
                covers: Default::default(),
                igdb: Default::default(),
                picks: Default::default(),
            });
            scan::configure_schedule(app.handle(), hours);
            tracker::start(app.handle());
            windows::create(app.handle(), false)?;
            // Off the UI path: a few seconds after launch, cache remote covers locally.
            covercache::spawn_startup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
            commands::window_is_maximized,
            commands::app_quit,
            commands::library_list,
            commands::library_scan,
            commands::library_stores,
            commands::library_launch,
            commands::library_save,
            commands::library_merge,
            commands::library_remove,
            commands::library_reset_playtime,
            commands::library_add,
            commands::library_set_cover,
            commands::library_choose_cover,
            commands::library_open_install_folder,
            commands::settings_get,
            commands::settings_save,
            commands::settings_open,
            commands::theme_load,
            commands::theme_reset,
            local_commands::emulators_list,
            local_commands::emulators_save,
            local_commands::emulators_scan,
            local_commands::emulators_remove,
            local_commands::emulators_catalog,
            local_commands::emulators_ignored,
            local_commands::emulators_unignore,
            local_commands::emulators_browse_executable,
            local_commands::roots_list,
            local_commands::roots_add,
            local_commands::roots_browse,
            local_commands::roots_set_enabled,
            local_commands::roots_remove,
            local_commands::roots_rescan,
            local_commands::review_list,
            local_commands::review_resolve,
            local_commands::review_ignored,
            local_commands::review_unignore,
            local_commands::library_set_emulator,
            local_commands::library_add_local_game,
            commands::plugins_list,
            commands::plugins_install,
            commands::backup_export,
            commands::backup_import,
            commands::steamgriddb_status,
            commands::steamgriddb_save_key,
            commands::steamgriddb_test_key,
            commands::steamgriddb_search,
            commands::steamgriddb_apply,
            commands::steamgriddb_fetch_missing,
            commands::tracking_status,
            commands::tracking_sessions,
            commands::tracking_stats,
            commands::tracking_set_excluded,
            commands::art_get,
            commands::art_set_hero,
            commands::art_set_logo,
            commands::art_choose_hero_file,
            commands::art_choose_logo_file,
            commands::art_set_logo_layout,
            commands::steamgriddb_search_heroes,
            commands::steamgriddb_search_logos,
            screenshots::screenshots_list,
            screenshots::screenshots_add,
            screenshots::screenshots_import_steam,
            screenshots::screenshots_remove,
            screenshots::screenshots_set_as_hero,
            commands::igdb_status,
            commands::igdb_save_credentials,
            commands::igdb_clear_credentials,
            commands::igdb_test,
            commands::igdb_get,
            commands::igdb_search,
            commands::igdb_choose,
            commands::igdb_reset_match,
            commands::igdb_fetch_all,
            commands::igdb_cancel_fetch_all,
            commands::igdb_progress,
            storage::storage_list,
            storage::storage_rescan,
            storage::storage_uninstall,
            storage::recent_list,
            storage::recent_dismiss,
            updater::updates_check,
            updater::updates_auto_check,
            updater::updates_last,
            updater::updates_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Game Lobby");
}

#[cfg(test)]
mod tests {
    use super::cover_file;
    use std::path::Path;

    #[test]
    fn cover_requests_resolve_to_a_bare_name_in_the_covers_folder() {
        let dir = Path::new("covers");
        let file = |uri: &str| cover_file(dir, &uri.parse().unwrap());
        let name = "cache-0123456789abcdef-0123456789ab.png";
        let expected = Some(dir.join(name));
        assert_eq!(
            file(&format!("http://lobby-cover.localhost/{name}")),
            expected
        );
        // A cache-busting query or a fragment is ignored.
        assert_eq!(
            file(&format!("http://lobby-cover.localhost/{name}?v=abc123")),
            expected
        );
        assert_eq!(
            file(&format!(
                "http://lobby-cover.localhost/{name}?v=1&x=../../a"
            )),
            expected
        );
        assert_eq!(
            file(&format!("lobby-cover://localhost/{name}?v=2")),
            Some(dir.join(name))
        );
        // Paths never leave the folder.
        assert_eq!(
            file("http://lobby-cover.localhost/..%2F..%2Flibrary.sqlite"),
            Some(dir.join("library.sqlite"))
        );
        assert_eq!(
            file("http://lobby-cover.localhost/a/b/c.png"),
            Some(dir.join("c.png"))
        );
        assert_eq!(file("http://lobby-cover.localhost/.."), None);
        assert_eq!(file("http://lobby-cover.localhost/"), None);
    }
}
