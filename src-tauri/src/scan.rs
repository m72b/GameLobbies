//! Library scan orchestration and the optional scheduled rescan.

use crate::db;
use crate::importers::{epic, gog, registry, steam, xbox};
use crate::platform::on_thread;
use crate::secrets::{get_steam_api_key, has_steam_api_key};
use crate::state::AppState;
use crate::types::{now_ms, Game, LibraryStats};
use anyhow::Result;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

struct Collected {
    discovered: Vec<Game>,
    errors: Vec<String>,
    installed: Vec<(&'static str, Vec<Game>)>,
    owned: Vec<(&'static str, Vec<Game>)>,
}

impl Collected {
    fn record(
        &mut self,
        name: &str,
        result: Result<Vec<Game>>,
        installed: Option<&'static str>,
        owned: Option<&'static str>,
    ) {
        match result {
            Ok(games) => {
                self.discovered.extend(games.iter().cloned());
                if let Some(id) = installed {
                    self.installed.push((id, games.clone()));
                }
                if let Some(id) = owned {
                    self.owned.push((id, games));
                }
            }
            Err(e) => {
                eprintln!("Could not scan {name}: {e:#}");
                self.errors.push(format!("{name}: {e:#}"));
            }
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await?
}

async fn perform(app: &AppHandle) -> Result<LibraryStats> {
    let state = app.state::<AppState>();
    let connections = db::get_connection_values(&state.db.lock())?;
    let mut c = Collected {
        discovered: Vec::new(),
        errors: Vec::new(),
        installed: Vec::new(),
        owned: Vec::new(),
    };

    c.record(
        "Steam installed",
        blocking(steam::scan_installed).await,
        Some("steam-installed"),
        None,
    );
    c.record(
        "Epic Games installed",
        blocking(epic::scan_installed).await,
        Some("epic-installed"),
        None,
    );
    // Registry sources share one read of the uninstall records.
    let mut registry_results = blocking(|| {
        let mut cache = registry::UninstallCache::default();
        Ok(registry::SOURCES
            .iter()
            .map(|s| registry::scan_source(s, &mut cache))
            .collect::<Vec<_>>())
    })
    .await?
    .into_iter();
    for source in &registry::SOURCES[..3] {
        let label = format!("{} installed", source.name);
        c.record(
            &label,
            registry_results.next().unwrap(),
            Some(source.id),
            None,
        );
    }
    c.record(
        "itch.io installed",
        blocking(registry::scan_itch).await,
        Some("itch-installed"),
        None,
    );
    let amazon = &registry::SOURCES[3];
    c.record(
        &format!("{} installed", amazon.name),
        registry_results.next().unwrap(),
        Some(amazon.id),
        None,
    );
    c.record(
        "Xbox / Microsoft Store installed",
        on_thread(xbox::scan).await.and_then(|r| r),
        Some("xbox-appx"),
        None,
    );

    if !connections.steam_id.is_empty() && has_steam_api_key(&state)? {
        let result = match get_steam_api_key(&state) {
            Ok(Some(key)) => steam::scan_owned(&connections.steam_id, &key).await,
            Ok(None) => Ok(Vec::new()),
            Err(e) => Err(e),
        };
        c.record("Steam owned games", result, None, Some("steam"));
    }
    if !connections.legendary_path.is_empty() {
        let result = epic::scan_legendary(&connections.legendary_path).await;
        c.record(
            "Epic Games via Legendary",
            result,
            Some("epic-legendary"),
            Some("epic"),
        );
    }
    let gog_path = connections.gog_database_path.clone();
    c.record(
        "GOG Galaxy",
        blocking(move || gog::scan(&gog_path)).await,
        Some("gog-galaxy"),
        None,
    );

    let imported = c.discovered.len();
    let errors = c.errors.clone();
    let app_for_db = app.clone();
    let total = blocking(move || {
        let state = app_for_db.state::<AppState>();
        let mut conn = state.db.lock();
        db::upsert_imported_games(&mut conn, &c.discovered)?;
        for (id, games) in &c.installed {
            db::reconcile_installed_source(&mut conn, id, games)?;
        }
        for (id, games) in &c.owned {
            db::reconcile_owned_source(&mut conn, id, games)?;
        }
        db::game_count(&conn)
    })
    .await?;
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    Ok(LibraryStats {
        total,
        imported: Some(imported),
        errors: Some(errors),
        scanned_at: Some(now_ms()),
    })
}

/// Runs a scan, or — if one is already running — waits for it and returns its result.
pub async fn scan_libraries(app: &AppHandle) -> Result<LibraryStats, String> {
    let state = app.state::<AppState>();
    let outcome = match state.scan_lock.try_lock() {
        Ok(_guard) => {
            let result = perform(app).await.map_err(|e| format!("{e:#}"));
            *state.last_scan.lock().unwrap() = Some(result.clone());
            if result.is_ok() {
                // Runs in the background; never delays or fails the scan.
                crate::steamgriddb::spawn_auto_fetch(app);
            }
            result
        }
        Err(_) => {
            let _guard = state.scan_lock.lock().await;
            let last = state.last_scan.lock().unwrap().clone();
            last.unwrap_or_else(|| Err("Library scan failed.".into()))
        }
    };
    outcome
}

pub fn configure_schedule(app: &AppHandle, hours: i64) {
    let state = app.state::<AppState>();
    let mut timer = state.scan_timer.lock().unwrap();
    if let Some(handle) = timer.take() {
        handle.abort();
    }
    if hours > 0 {
        let app = app.clone();
        *timer = Some(tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(hours as u64 * 3600)).await;
                if let Err(e) = scan_libraries(&app).await {
                    eprintln!("Scheduled library scan failed: {e}");
                }
            }
        }));
    }
}
