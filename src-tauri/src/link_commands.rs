//! Tauri commands for linked games, route names and route launch options (see `links` for the
//! data model and trust rules). The renderer sends game ids, a [`RouteRef`] that selects a
//! stored route, review choices (ids and a flag, plus an optional custom title), a route name
//! and launch options text. It never sends route contents: linking and unlinking only move
//! stored routes, a name is display text, and launch options are checked here and confirmed
//! in a native dialog before they are saved.

use crate::links::{self, LinkChoices, LinkPreview, RouteRef, UnlinkPreview};
use crate::local_commands::confirm_natively;
use crate::state::AppState;
use crate::types::Game;
use crate::{covercache, db, health, review, tracker, validation};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State, WebviewWindow};

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

fn game_arg(value: &str) -> CmdResult<String> {
    if value.is_empty() || value.chars().count() > 512 || value.chars().any(char::is_control) {
        return Err("The game id is invalid.".into());
    }
    Ok(value.to_string())
}

fn id_list(values: &[String]) -> CmdResult<Vec<String>> {
    if values.len() > links::MAX_LINK_GAMES {
        return Err(format!(
            "You can link at most {} games at once.",
            links::MAX_LINK_GAMES
        ));
    }
    values.iter().map(|v| game_arg(v)).collect()
}

fn route_arg(value: Value) -> CmdResult<RouteRef> {
    let route: RouteRef =
        serde_json::from_value(value).map_err(|_| "Launch route is invalid.".to_string())?;
    route.check().map_err(err)?;
    Ok(route)
}

fn changed(app: &AppHandle, state: &AppState) {
    state.tracker.library_changed();
    let _ = app.emit("library:changed", ());
}

/// What linking `gameIds` would combine, the conflicting details and the suggested route names
/// (`title`: the shared title the names are suggested against; default the first game's).
#[tauri::command]
pub async fn library_link_preview(
    state: State<'_, AppState>,
    game_ids: Vec<String>,
    title: Option<String>,
) -> CmdResult<LinkPreview> {
    let ids = id_list(&game_ids)?;
    if title.as_ref().is_some_and(|t| t.chars().count() > 300) {
        return Err("The title can be at most 300 characters.".into());
    }
    links::preview(&state.db.lock(), &ids, title.as_deref()).map_err(err)
}

/// Links `gameIds` with the review's `choices` (`links::LinkChoices`). `reviewId` names the
/// duplicate item the review was opened from; it is marked linked when it lists only these
/// games.
#[tauri::command]
pub async fn library_link(
    app: AppHandle,
    state: State<'_, AppState>,
    game_ids: Vec<String>,
    choices: Value,
    review_id: Option<String>,
) -> CmdResult<Vec<Game>> {
    let ids = id_list(&game_ids)?;
    let choices: LinkChoices = serde_json::from_value(choices)
        .map_err(|e| format!("The link choices are invalid: {e}"))?;
    let review_id = review_id.map(|id| game_arg(&id)).transpose()?;
    let games = {
        // Session state before the database (see `tracker`): no poll runs in between.
        let mut sessions = state.tracker.sessions();
        let mut conn = state.db.lock();
        let (keep, linked) = links::link(&mut conn, &ids, Some(&choices)).map_err(err)?;
        for source in &linked {
            let tick = sessions.merge(&keep, source);
            tracker::apply(&conn, &mut sessions, tick.effects);
        }
        if let Some(id) = &review_id {
            if let Some(item) = review::get(&conn, id).map_err(err)? {
                let listed: Vec<String> = item
                    .details
                    .get("games")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|g| g.get("gameId").and_then(Value::as_str).map(String::from))
                    .collect();
                let kind_ok =
                    item.kind == health::HEALTH_DUPLICATE || item.kind == review::DUPLICATE;
                if item.status == "open"
                    && kind_ok
                    && !listed.is_empty()
                    && listed.iter().all(|g| ids.contains(g))
                {
                    review::resolve(&conn, id, "linked").map_err(err)?;
                }
            }
        }
        db::all_games(&conn).map_err(err)?
    };
    covercache::clean_up(&state);
    changed(&app, &state);
    let _ = app.emit("review:changed", ());
    Ok(games)
}

/// What splitting `route` off `gameId` would do.
#[tauri::command]
pub async fn library_unlink_preview(
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
) -> CmdResult<UnlinkPreview> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    links::unlink_preview(&state.db.lock(), &game_id, &route).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unlinked {
    game_id: String,
    games: Vec<Game>,
}

/// Splits `route` (and the routes that came in with it) off `gameId` into a game of its own.
#[tauri::command]
pub async fn library_unlink(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
) -> CmdResult<Unlinked> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    let (new_id, games) = {
        let mut conn = state.db.lock();
        let new_id = links::unlink(&mut conn, &game_id, &route).map_err(err)?;
        (new_id, db::all_games(&conn).map_err(err)?)
    };
    changed(&app, &state);
    let _ = app.emit("review:changed", ());
    Ok(Unlinked {
        game_id: new_id,
        games,
    })
}

/// Names one route (display only; empty clears the name).
#[tauri::command]
pub async fn library_route_rename(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
    name: String,
) -> CmdResult<Vec<Game>> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    if name.chars().count() > 1_000 {
        return Err(format!(
            "A route name can be at most {} characters.",
            links::MAX_ROUTE_NAME
        ));
    }
    let name = validation::route_name(&name).map_err(err)?;
    let games = {
        let conn = state.db.lock();
        links::rename_route(&conn, &game_id, &route, name).map_err(err)?;
        db::all_games(&conn).map_err(err)?
    };
    changed(&app, &state);
    Ok(games)
}

/// Sets the launch options of one route from the text the user typed (empty removes them).
/// Only routes whose `.exe` Game Lobby starts directly take options (`links::args_exe`); the
/// text gets the checks of every program started with arguments, the exact program and argv
/// are shown in a native dialog, and the options are saved only when the user confirms and the
/// game is still exactly as it was when the dialog opened.
#[tauri::command]
pub async fn library_route_args(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
    args: String,
) -> CmdResult<Vec<Game>> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    if args.chars().count() > links::MAX_ARGS_CHARS {
        return Err(format!(
            "Launch options can be at most {} characters.",
            links::MAX_ARGS_CHARS
        ));
    }
    let game = db::get_game(&state.db.lock(), &game_id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let stored = route.find(&game).map_err(err)?.clone();
    let argv = links::prepare_args(&stored, &args).map_err(err)?;
    let current = crate::launch::parse_launch_arguments(stored.launch_args.as_deref());
    if argv.as_deref().unwrap_or_default() == current.as_slice() {
        let conn = state.db.lock();
        // The same options again: nothing to do once they were confirmed for this program;
        // options that never were (a backup, an older version) are confirmed now.
        let confirmed = match &argv {
            Some(argv) => links::args_confirmed(&conn, &stored, argv).map_err(err)?,
            None => true,
        };
        if confirmed {
            return db::all_games(&conn).map_err(err);
        }
    }
    // Removing options runs nothing new; new options are confirmed first.
    if let Some(argv) = &argv {
        let prompt = links::args_prompt(&game.title, &stored, argv).map_err(err)?;
        if !confirm_natively(&window, "Confirm launch options", &prompt, "Save").await {
            return Err("The launch options were not saved.".into());
        }
    }
    let games = {
        let conn = state.db.lock();
        // The dialog may have been open for a while: save only over what was confirmed.
        if db::get_game(&conn, &game_id).map_err(err)?.as_ref() != Some(&game) {
            return Err("The game changed in the meantime. Try again.".into());
        }
        links::set_args(&conn, &game_id, &route, argv.as_deref()).map_err(err)?;
        if let Some(argv) = &argv {
            // The only place a confirmation is written: these options, this program, after
            // the dialog and the check that nothing changed meanwhile.
            links::confirm_args(&conn, &stored, argv).map_err(err)?;
        }
        db::all_games(&conn).map_err(err)?
    };
    changed(&app, &state);
    Ok(games)
}

/// Whether the launch options stored on `route` need confirming on the game page before they
/// run (they came from a backup or an older version, or the route's program changed).
#[tauri::command]
pub async fn library_route_args_pending(
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
) -> CmdResult<bool> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    let conn = state.db.lock();
    let game = db::get_game(&conn, &game_id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let stored = route.find(&game).map_err(err)?;
    Ok(links::args_need_confirmation(&conn, stored))
}
