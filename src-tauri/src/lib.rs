//! Game Lobby — a unified local game library and launcher (Tauri backend).

mod applog;
mod art;
mod catalog;
mod commands;
mod covercache;
mod db;
mod dbrepair;
mod diagnostics;
mod emulators;
mod health;
mod health_commands;
mod igdb;
mod importers;
mod launch;
mod launcher_commands;
mod launchers;
mod link_commands;
mod links;
mod local;
mod local_commands;
mod localscan;
mod notes;
mod pending_launch;
mod picks;
mod platform;
mod redact;
mod review;
mod roots;
mod scan;
mod screenshots;
mod secrets;
mod sessions;
mod state;
mod steam_owned;
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
        log::warn!("GAME_LOBBY_DATA_DIR must be an absolute folder path; ignoring it.");
        return None;
    }
    Some(dir)
}

#[cfg(not(debug_assertions))]
fn data_dir_override() -> Option<std::path::PathBuf> {
    None
}

/// Registers the single-instance plugin, which must come before every other plugin: a second
/// copy is detected (and exits) while the app is still being built, before `setup` below opens
/// the library or the log. The running copy brings its main window to the front.
///
/// The plugin keys the instance on the app identifier, not on the data folder. A debug build
/// started with `GAME_LOBBY_DATA_DIR` is a throwaway copy with its own library, so it skips the
/// check and may run next to an installed copy.
fn with_single_instance(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    if data_dir_override().is_some() {
        return builder;
    }
    builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        log::info!("A second copy of Game Lobby was started; showing the running one.");
        if let Err(e) = windows::focus_main(app) {
            log::warn!("Could not show the main window: {e}");
        }
    }))
}

pub fn run() {
    with_single_instance(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
        // Signed updates; used only by the `updates_*` commands (no webview permissions).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::Updates::default())
        .register_uri_scheme_protocol("lobby-cover", cover_protocol)
        .register_uri_scheme_protocol("lobby-shot", screenshots::protocol)
        .setup(|app| {
            // Only the first copy gets here (see `with_single_instance`), so only it touches
            // the log and the library.
            let (data_dir, development) = match data_dir_override() {
                Some(dir) => (dir, true),
                None => (app.path().app_data_dir()?, false),
            };
            if let Err(e) = applog::init(&data_dir) {
                // No log file; the app still runs.
                eprintln!("Could not start the log file: {e}");
            }
            diagnostics::log_startup(app, &data_dir);
            if development {
                // A test library: never copy the user's Electron library into it.
                log::info!("Using the development data folder {}", data_dir.display());
            } else {
                match db::migrate_from_electron(&data_dir) {
                    Ok(true) => {
                        log::info!("Copied the library from the Electron build of Game Lobby.")
                    }
                    Ok(false) => {}
                    Err(e) => log::error!("Could not copy the Electron library: {e:#}"),
                }
            }
            let (database, repair) = db::Db::open_checked(
                &data_dir.join("library.sqlite"),
                Some(&data_dir.join("screenshots")),
            )?;
            match &repair {
                None => log::info!("Library database quick_check: ok."),
                Some(notice) => log::error!(
                    "Library database quick_check failed; repaired and replaced: {}; error: {}.",
                    notice.replaced,
                    notice.error.as_deref().unwrap_or("none")
                ),
            }
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
                pending_launches: Default::default(),
            });
            scan::configure_schedule(app.handle(), hours);
            tracker::start(app.handle());
            windows::create(app.handle(), false)?;
            if let Some(notice) = repair {
                use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                app.dialog()
                    .message(notice.message())
                    .title(if notice.replaced {
                        "Library repaired"
                    } else {
                        "Library damaged"
                    })
                    .kind(if notice.replaced {
                        MessageDialogKind::Info
                    } else {
                        MessageDialogKind::Warning
                    })
                    .show(|_| {});
            }
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
            commands::library_launch_continue,
            commands::library_launch_cancel,
            commands::library_launch_pending,
            commands::library_save,
            link_commands::library_link_preview,
            link_commands::library_link,
            link_commands::library_unlink_preview,
            link_commands::library_unlink,
            link_commands::library_route_rename,
            link_commands::library_route_args,
            link_commands::library_route_args_pending,
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
            diagnostics::diagnostics_report,
            diagnostics::diagnostics_open_logs,
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
            health_commands::health_check,
            health_commands::health_use_found,
            health_commands::health_locate,
            health_commands::health_remove_route,
            health_commands::health_dismiss,
            health_commands::health_merge_routes,
            local_commands::library_set_emulator,
            local_commands::library_add_local_game,
            launcher_commands::launchers_list,
            launcher_commands::launchers_save,
            launcher_commands::launchers_remove,
            launcher_commands::launchers_browse_executable,
            launcher_commands::launchers_browse_folder,
            launcher_commands::launchers_browse_games_folder,
            launcher_commands::launchers_add_folder,
            launcher_commands::launchers_choose_icon,
            launcher_commands::launchers_add_game,
            launcher_commands::launchers_assignments,
            launcher_commands::launchers_assign,
            launcher_commands::launchers_unassign,
            commands::plugins_list,
            commands::plugins_install,
            commands::backup_export,
            commands::backup_import,
            notes::notes_get,
            notes::notes_set_rating,
            notes::notes_set_text,
            notes::notes_save_memory,
            notes::notes_delete_memory,
            notes::input_get,
            notes::input_save,
            notes::input_refresh,
            steam_owned::steam_owned_status,
            steam_owned::steam_owned_save,
            steam_owned::steam_owned_remove,
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
            sessions::sessions_page,
            sessions::sessions_update,
            sessions::sessions_delete,
            sessions::sessions_set_journal,
            sessions::sessions_share_text,
            sessions::sessions_export,
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

    /// The single-instance plugin must be the first plugin, and the plugin's check runs while
    /// the app is built, before `setup` (which starts the log and opens the library).
    #[test]
    fn single_instance_is_registered_first_and_before_the_library_is_opened() {
        let source = include_str!("lib.rs");
        let run = &source[source.find("pub fn run()").unwrap()..];
        let at = |needle: &str| run.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("with_single_instance(tauri::Builder::default())") < at(".plugin("));
        assert!(at(".plugin(") < at(".setup("));
        assert!(at(".setup(") < at("applog::init("));
        assert!(at("applog::init(") < at("Db::open_checked("));
        // Nothing before the setup hook opens the library.
        assert!(!run[..at(".setup(")].contains("open_checked"));
        let helper = &source
            [source.find("fn with_single_instance").unwrap()..source.find("pub fn run()").unwrap()];
        assert!(helper.contains("tauri_plugin_single_instance::init"));
        assert!(helper.contains("windows::focus_main"));
    }
}
