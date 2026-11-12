//! Game Lobby — a unified local game library and launcher (Tauri backend).

mod art;
mod commands;
mod db;
mod importers;
mod launch;
mod platform;
mod scan;
mod secrets;
mod state;
mod steamgriddb;
mod theme;
mod tracker;
mod types;
mod validation;
mod windows;

use state::AppState;
use std::borrow::Cow;
use std::sync::Mutex;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext};

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
    let raw = request.uri().path().trim_start_matches('/');
    let name = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    // Only a bare file name inside the covers folder is ever served.
    let Some(file_name) = std::path::Path::new(name.as_ref()).file_name() else {
        return not_found();
    };
    let file = state.covers_dir().join(file_name);
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("lobby-cover", cover_protocol)
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            match db::migrate_from_electron(&data_dir) {
                Ok(true) => eprintln!("Copied the library from the Electron build of Game Lobby."),
                Ok(false) => {}
                Err(e) => eprintln!("Could not copy the Electron library: {e:#}"),
            }
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
            });
            scan::configure_schedule(app.handle(), hours);
            tracker::start(app.handle());
            windows::create(app.handle(), false)?;
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
            commands::library_connections,
            commands::library_save_connections,
            commands::library_choose_legendary,
            commands::library_choose_gog_database,
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
            commands::emulators_list,
            commands::emulators_save,
            commands::emulators_scan,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Game Lobby");
}
