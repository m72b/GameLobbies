//! Library scan orchestration and the optional scheduled rescan.

use crate::db;
use crate::importers::{epic, gog, registry, steam, xbox};
use crate::platform::on_thread;
use crate::state::AppState;
use crate::types::{now_ms, Game, LibraryStats, StoreDetection, StoreStatus, STORE_SOURCES};
use crate::{local, roots};
use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

struct Collected {
    discovered: Vec<Game>,
    errors: Vec<String>,
    installed: Vec<(&'static str, Vec<Game>)>,
    /// Per store id (see [`STORE_SOURCES`]): what this scan found.
    detected: HashMap<String, StoreDetection>,
}

impl Collected {
    fn record(
        &mut self,
        store: &str,
        name: &str,
        result: Result<Vec<Game>>,
        installed: &'static str,
    ) {
        let now = now_ms();
        let entry = self
            .detected
            .entry(store.to_string())
            .or_insert(StoreDetection {
                found: 0,
                error: None,
                scanned_at: now,
            });
        match result {
            Ok(games) => {
                entry.found += games.len();
                self.discovered.extend(games.iter().cloned());
                self.installed.push((installed, games));
            }
            Err(e) => {
                log::warn!("Could not scan {name}: {e:#}");
                let message: String = format!("{e:#}").chars().take(300).collect();
                entry.error = Some(message);
                self.errors.push(format!("{name}: {e:#}"));
            }
        }
    }
}

/// Every store with its on/off state and what the last scan found.
pub fn store_statuses(state: &AppState) -> Result<Vec<StoreStatus>> {
    let conn = state.db.lock();
    let disabled = db::get_settings(&conn)?.disabled_stores;
    let detected = db::store_detection(&conn)?;
    Ok(STORE_SOURCES
        .iter()
        .map(|(id, name)| {
            let last = detected.get(*id);
            StoreStatus {
                id: id.to_string(),
                name: name.to_string(),
                enabled: !disabled.iter().any(|d| d == id),
                found: last.map(|d| d.found),
                error: last.and_then(|d| d.error.clone()),
                scanned_at: last.map(|d| d.scanned_at),
            }
        })
        .collect())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await?
}

/// Store scans and library folder scans run side by side, so a large ROM folder doesn't hold
/// up the store results (each applies its changes and emits `library:changed` as it ends).
async fn perform(app: &AppHandle) -> Result<LibraryStats> {
    let (stores, local) = tokio::join!(
        perform_stores(app),
        scan_local(app, LocalScope::All, true, false)
    );
    let mut stats = stores?;
    match local {
        Ok(summary) => {
            if !summary.roots.is_empty() {
                stats.total = db::game_count(&app.state::<AppState>().db.lock())?;
                stats.local = Some(summary);
            }
        }
        Err(e) => stats
            .errors
            .get_or_insert_with(Vec::new)
            .push(format!("Library folders: {e}")),
    }
    Ok(stats)
}

/// Which library roots a folder scan covers.
#[derive(Debug, Clone)]
pub enum LocalScope {
    /// Every enabled root.
    All,
    /// Every enabled root of one kind (`emulators` / `games`).
    Kind(&'static str),
    /// One root (must be enabled).
    Root(String),
}

/// Minimum time between two `library-scan:progress` events of the same phase.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Scans library roots, emitting `library-scan:progress` (at most ~10 per second, plus every
/// phase change) and `library-scan:finished` with the summary. With `wait` false a scan
/// that is already running is an error instead of being waited for. `full` uses the much
/// higher "scan everything" limits (`localscan::ScanLimits`).
pub async fn scan_local(
    app: &AppHandle,
    scope: LocalScope,
    wait: bool,
    full: bool,
) -> Result<local::ScanSummary, String> {
    let state = app.state::<AppState>();
    let _guard = if wait {
        state.local_scan_lock.lock().await
    } else {
        state
            .local_scan_lock
            .try_lock()
            .map_err(|_| "A library folder scan is already running.".to_string())?
    };
    let roots = roots::list(&state.db.lock()).map_err(|e| format!("{e:#}"))?;
    let selected: Vec<roots::LibraryRoot> = match &scope {
        LocalScope::All => roots,
        LocalScope::Kind(kind) => roots.into_iter().filter(|r| r.kind == *kind).collect(),
        LocalScope::Root(id) => {
            let root = roots
                .into_iter()
                .find(|r| r.id == *id)
                .ok_or("That library folder no longer exists.")?;
            if !root.enabled {
                return Err("That library folder is turned off. Turn it on to scan it.".into());
            }
            vec![root]
        }
    };
    if !selected.iter().any(|r| r.enabled) {
        return Ok(local::ScanSummary::default());
    }
    let worker = app.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let state = worker.state::<AppState>();
        let mut last: Option<(&'static str, std::time::Instant)> = None;
        // Where launcher games folders may discover emulators (as for emulator roots).
        let guard = roots::RootGuard::from_environment(&state.data_dir);
        local::scan_roots_with(&state.db, &selected, full, &guard, &mut |progress| {
            let now = std::time::Instant::now();
            let due = match last {
                Some((phase, at)) => phase != progress.phase || now - at >= PROGRESS_INTERVAL,
                None => true,
            };
            if due {
                last = Some((progress.phase, now));
                let _ = worker.emit("library-scan:progress", &progress);
            }
        })
    })
    .await
    .map_err(|e| format!("{e}"))?
    .map_err(|e| format!("{e:#}"))?;
    // Cheap health pass (missing files, duplicates; no search for moved files).
    let _ = crate::health_commands::run_check(app, false).await;
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    let _ = app.emit("review:changed", ());
    let _ = app.emit("library-scan:finished", &summary);
    Ok(summary)
}

async fn perform_stores(app: &AppHandle) -> Result<LibraryStats> {
    let state = app.state::<AppState>();
    let (connections, disabled) = {
        let conn = state.db.lock();
        (
            db::get_connection_values(&conn)?,
            db::get_settings(&conn)?.disabled_stores,
        )
    };
    let on = |store: &str| !disabled.iter().any(|d| d == store);
    let mut c = Collected {
        discovered: Vec::new(),
        errors: Vec::new(),
        installed: Vec::new(),
        detected: HashMap::new(),
    };

    if on("steam") {
        c.record(
            "steam",
            "Steam installed",
            blocking(steam::scan_installed).await,
            "steam-installed",
        );
    }
    if on("epic") {
        c.record(
            "epic",
            "Epic Games installed",
            blocking(epic::scan_installed).await,
            "epic-installed",
        );
    }
    // Registry sources (EA, Battle.net, Ubisoft, Amazon) share one read of the uninstall records.
    let registry_stores = ["ea", "battlenet", "ubisoft", "amazon"];
    if registry_stores.iter().any(|s| on(s)) {
        let results = blocking(|| {
            let mut cache = registry::UninstallCache::default();
            Ok(registry::SOURCES
                .iter()
                .map(|s| registry::scan_source(s, &mut cache))
                .collect::<Vec<_>>())
        })
        .await?;
        for ((source, result), store) in registry::SOURCES.iter().zip(results).zip(registry_stores)
        {
            if on(store) {
                let label = format!("{} installed", source.name);
                c.record(store, &label, result, source.id);
            }
        }
    }
    if on("itch") {
        c.record(
            "itch",
            "itch.io installed",
            blocking(registry::scan_itch).await,
            "itch-installed",
        );
    }
    if on("xbox") {
        c.record(
            "xbox",
            "Xbox / Microsoft Store installed",
            on_thread(xbox::scan).await.and_then(|r| r),
            "xbox-appx",
        );
    }
    if on("gog") {
        let gog_path = connections.gog_database_path.clone();
        c.record(
            "gog",
            "GOG Galaxy",
            blocking(move || gog::scan(&gog_path)).await,
            "gog-galaxy",
        );
    }

    let imported = c.discovered.len();
    let mut errors = c.errors.clone();
    let app_for_db = app.clone();
    blocking(move || {
        let state = app_for_db.state::<AppState>();
        let mut conn = state.db.lock();
        db::upsert_imported_games(&mut conn, &c.discovered)?;
        for (id, games) in &c.installed {
            db::reconcile_installed_source(&mut conn, id, games)?;
        }
        db::put_store_detection(&conn, &c.detected)?;
        Ok(())
    })
    .await?;
    // Optional owned-but-not-installed Steam games (only with a saved key; throttled). After
    // the manifest scan, so installed games are known and keep their manifest state.
    match crate::steam_owned::run_for_scan(app).await {
        Ok(Some(problem)) => errors.push(problem),
        Ok(None) => {}
        Err(e) => {
            log::warn!("Steam owned games could not be applied: {e:#}");
            errors.push(format!("Steam owned games: {e:#}"));
        }
    }
    // Signed-in GOG and Battle.net accounts (throttled; after the installed scans, so
    // installed games are known and keep their entries).
    errors.extend(crate::accounts::stores::run_for_scan(app).await);
    let total = db::game_count(&state.db.lock())?;
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    Ok(LibraryStats {
        total,
        imported: Some(imported),
        errors: Some(errors),
        scanned_at: Some(now_ms()),
        local: None,
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
                // New store covers (e.g. Steam's) go into the local cover cache.
                crate::covercache::spawn_pass(app, std::time::Duration::ZERO);
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
                    log::error!("Scheduled library scan failed: {e}");
                }
            }
        }));
    }
}
