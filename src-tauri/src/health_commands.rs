//! Tauri commands for the library health review (see `health`). The renderer only names a
//! review item (and a suggestion by its position); every path that ends up in a launch route
//! comes from the Rust-side search or from a native picker opened here.

use crate::commands::pick_file;
use crate::health::{self, HealthSummary};
use crate::review::{self, ReviewItem};
use crate::state::AppState;
use crate::tracker;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

fn id_arg(value: &str) -> CmdResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 512 {
        return Err("Review item is invalid.".into());
    }
    Ok(value.to_string())
}

fn changed(app: &AppHandle, state: &AppState) {
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
    let _ = app.emit("review:changed", ());
}

fn open_items(state: &AppState) -> CmdResult<Vec<ReviewItem>> {
    review::list(&state.db.lock(), false).map_err(err)
}

/// Runs a check (also after scans, with `search` false): refreshes the health items.
pub async fn run_check(app: &AppHandle, search: bool) -> CmdResult<HealthSummary> {
    use std::sync::atomic::Ordering::SeqCst;
    if health::RUNNING.swap(true, SeqCst) {
        return Err("A library check is already running.".into());
    }
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = worker.state::<AppState>();
        health::refresh(&state.db, search)
    })
    .await;
    health::RUNNING.store(false, SeqCst);
    let summary = result.map_err(err)?.map_err(err)?;
    let _ = app.emit("review:changed", ());
    Ok(summary)
}

/// "Check library": looks for missing and moved files and for duplicates.
#[tauri::command]
pub async fn health_check(app: AppHandle) -> CmdResult<HealthSummary> {
    run_check(&app, true).await
}

/// "Use this location": the suggestion at `index` of a missing-file item. The renderer only
/// names the item and the position; the old and new path are shown in a native dialog first,
/// and the suggestion must still be one of the item's candidates when it is applied.
#[tauri::command]
pub async fn health_use_found(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    index: usize,
) -> CmdResult<Vec<ReviewItem>> {
    let id = id_arg(&id)?;
    let (old, path) = {
        let worker = app.clone();
        let id = id.clone();
        tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<_> {
            let ctx = health::repair_context(&worker.state::<AppState>().db, &id)?;
            let path = health::suggested_path(&ctx, index)?;
            Ok((ctx.subject().path.clone(), path))
        })
        .await
        .map_err(err)?
        .map_err(err)?
    };
    let prompt = format!(
        "Use this location for the missing file?

Old path:
{old}

New path:
{}",
        path.display()
    );
    if !crate::local_commands::confirm_natively(&window, "Use this location", &prompt, "Use it")
        .await
    {
        return open_items(&app.state::<AppState>());
    }
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        let state = worker.state::<AppState>();
        // Read again: the item may have changed while the dialog was open.
        let ctx = health::repair_context(&state.db, &id)?;
        health::ensure_still_suggested(&ctx, &path)?;
        let path = health::suggested_path(&ctx, index).and_then(|p| {
            anyhow::ensure!(p == path, "That suggestion is no longer available.");
            Ok(p)
        })?;
        let fresh = health::prepare_relocation(&ctx, &path)?;
        health::apply_relocation(&state.db, &id, &ctx, &path, fresh)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    let state = app.state::<AppState>();
    changed(&app, &state);
    open_items(&state)
}

/// "Locate…": a native picker for the new file (or folder); `null` result when cancelled.
#[tauri::command]
pub async fn health_locate(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
) -> CmdResult<Vec<ReviewItem>> {
    let id = id_arg(&id)?;
    let state = app.state::<AppState>();
    let ctx = {
        let worker = app.clone();
        let id = id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            health::repair_context(&worker.state::<AppState>().db, &id)
        })
        .await
        .map_err(err)?
        .map_err(err)?
    };
    let picked = if ctx.subject().folder {
        let (tx, rx) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .file()
            .set_parent(&window)
            .pick_folder(move |path| {
                let _ = tx.send(path);
            });
        rx.await.ok().flatten().and_then(|p| p.into_path().ok())
    } else {
        let ext = ctx.subject().extension();
        let filter: Vec<&str> = vec![ext.as_str()];
        pick_file(&window, "The moved file", &filter).await
    };
    let Some(path) = picked else {
        return open_items(&state);
    };
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<()> {
        let state = worker.state::<AppState>();
        let fresh = health::prepare_relocation(&ctx, &path)?;
        health::apply_relocation(&state.db, &id, &ctx, &path, fresh)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    changed(&app, &state);
    open_items(&state)
}

/// "Remove this route": the interface confirms first; `confirmed` must be true. That flag is
/// a guard against an accidental double click in the interface, not a security control: the
/// renderer is trusted to send it, and the command can only remove a launch route (never a
/// file, never a game's last route) of an item the user has open.
#[tauri::command]
pub async fn health_remove_route(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    confirmed: bool,
) -> CmdResult<Vec<ReviewItem>> {
    if !confirmed {
        return Err("Removing a launch route needs confirmation.".into());
    }
    health::remove_route(&state.db, &id_arg(&id)?).map_err(err)?;
    changed(&app, &state);
    open_items(&state)
}

/// "Ignore" a missing file, "Not duplicates" for a pair, "Keep both" for routes.
#[tauri::command]
pub async fn health_dismiss(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<ReviewItem>> {
    let id = id_arg(&id)?;
    let kind = review::get(&state.db.lock(), &id)
        .map_err(err)?
        .map(|i| i.kind);
    match kind.as_deref() {
        Some(health::MISSING_FILE) => health::ignore_missing(&state.db, &id),
        Some(_) => health::keep_separate(&state.db, &id),
        None => Err(anyhow::anyhow!("That review item no longer exists.")),
    }
    .map_err(err)?;
    let _ = app.emit("review:changed", ());
    open_items(&state)
}

/// "Merge" a pair of games into `target_game_id` (default: the first listed), like
/// `library_merge`: playtime summed, sessions moved.
#[tauri::command]
pub async fn health_merge(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    target_game_id: Option<String>,
) -> CmdResult<Vec<ReviewItem>> {
    let id = id_arg(&id)?;
    {
        // Session state before the database (see `tracker`): no poll runs in between.
        let mut sessions = state.tracker.sessions();
        let (target, source) =
            health::merge_pair(&state.db, &id, target_game_id.as_deref()).map_err(err)?;
        let tick = sessions.merge(&target, &source);
        tracker::apply(&state.db.lock(), &mut sessions, tick.effects);
    }
    crate::covercache::clean_up(&state);
    changed(&app, &state);
    open_items(&state)
}

/// "Merge" duplicate routes of one game into one.
#[tauri::command]
pub async fn health_merge_routes(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<ReviewItem>> {
    health::merge_routes(&state.db, &id_arg(&id)?).map_err(err)?;
    changed(&app, &state);
    open_items(&state)
}
