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

/// A game that is being played can't be split: its open session belongs to the combined game,
/// and which route the running process belongs to can't be told reliably, so the session could
/// not be handed to the right half. (Linking is the other direction: the session simply
/// continues as the kept game's.)
fn not_running(sessions: &tracker::Sessions, game_id: &str) -> CmdResult<()> {
    if sessions.contains(game_id) {
        return Err("This game is running. Close it before unlinking one of its routes.".into());
    }
    Ok(())
}

/// Splits `route` (and the routes that came in with it) off `gameId` into a game of its own.
/// Refused while the game is running (see [`not_running`]).
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
        // Session state before the database (see `tracker`): no poll runs in between.
        let sessions = state.tracker.sessions();
        not_running(&sessions, &game_id)?;
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
/// Routes whose `.exe` Game Lobby starts directly and routes of a custom launcher with a program
/// take options (`links::args_host`); the text gets the checks of every program started with
/// arguments, the exact program, whole argv and working folder are shown in a native dialog,
/// and the options are saved (and their confirmation recorded) only when the user confirms and
/// the game and its launcher are still exactly as they were when the dialog opened.
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
    let (game, stored, checked) = {
        let conn = state.db.lock();
        let game = db::get_game(&conn, &game_id)
            .map_err(err)?
            .ok_or("Game not found.")?;
        let stored = route.find(&game).map_err(err)?.clone();
        // Clearing a launcher route's options runs nothing new, so it works even when its
        // launcher no longer takes options (deleted, or a backup's options on a launcher
        // without a program).
        let clearing = args.trim().is_empty() && crate::launchers::is_launcher_route(&stored);
        let checked = if clearing {
            None
        } else {
            Some(links::prepare_route_args(&conn, &stored, &args).map_err(err)?)
        };
        (game, stored, checked)
    };
    let argv = checked.as_ref().and_then(|(_, argv)| argv.clone());
    let current = crate::launch::parse_launch_arguments(stored.launch_args.as_deref());
    if argv.as_deref().unwrap_or_default() == current.as_slice() {
        let conn = state.db.lock();
        // The same options again: nothing to do once they were confirmed for this program;
        // options that never were (a backup, an older version) are confirmed now.
        let confirmed = match (&checked, &argv) {
            (Some((host, _)), Some(argv)) => {
                links::host_confirmed(&conn, host, argv).map_err(err)?
            }
            _ => true,
        };
        if confirmed {
            return db::all_games(&conn).map_err(err);
        }
    }
    // Removing options runs nothing new; new options are confirmed first.
    if let (Some((host, _)), Some(argv)) = (&checked, &argv) {
        let prompt = links::route_args_prompt(host, &game.title, &stored, argv);
        if !confirm_natively(&window, "Confirm launch options", &prompt, "Save").await {
            return Err("The launch options were not saved.".into());
        }
    }
    let games = {
        let conn = state.db.lock();
        // The dialog may have been open for a while: save only over what was confirmed (the
        // game, and the launcher's program, arguments and working folder).
        if db::get_game(&conn, &game_id).map_err(err)?.as_ref() != Some(&game) {
            return Err("The game changed in the meantime. Try again.".into());
        }
        if let (Some((host, _)), Some(_)) = (&checked, &argv) {
            if links::args_host(&conn, &stored).ok().as_ref() != Some(host) {
                return Err("The game's launcher changed in the meantime. Try again.".into());
            }
        }
        links::set_args(&conn, &game_id, &route, argv.as_deref()).map_err(err)?;
        if let (Some((host, _)), Some(argv)) = (&checked, &argv) {
            // The only place (with `launchers_assign`) a confirmation is written: these
            // options, this host, after the dialog and the check that nothing changed meanwhile.
            links::confirm_host_args(&conn, host, argv).map_err(err)?;
        }
        db::all_games(&conn).map_err(err)?
    };
    changed(&app, &state);
    Ok(games)
}

/// What launch options would run, for the editor to show while the user types: the program,
/// the whole argv and the working folder. Nothing is saved or confirmed here.
#[derive(Serialize, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArgsPreview {
    pub program: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

impl ArgsPreview {
    pub fn of(host: &links::ArgsHost, options: Option<&[String]>) -> Self {
        ArgsPreview {
            program: host.program.clone(),
            argv: host.argv(options.unwrap_or_default()),
            cwd: host.cwd.clone(),
        }
    }
}

/// Checks launch options text for one route without saving anything (the same checks as
/// [`library_route_args`]): the reason it would be refused, or what it would run. Empty text
/// checks nothing.
#[tauri::command]
pub async fn library_route_args_check(
    state: State<'_, AppState>,
    game_id: String,
    route: Value,
    args: String,
) -> CmdResult<ArgsPreview> {
    let game_id = game_arg(&game_id)?;
    let route = route_arg(route)?;
    if args.chars().count() > links::MAX_ARGS_CHARS {
        return Err(format!(
            "Launch options can be at most {} characters.",
            links::MAX_ARGS_CHARS
        ));
    }
    if args.trim().is_empty() {
        return Ok(ArgsPreview::default());
    }
    let conn = state.db.lock();
    let game = db::get_game(&conn, &game_id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let stored = route.find(&game).map_err(err)?;
    let (host, argv) = links::prepare_route_args(&conn, stored, &args).map_err(err)?;
    Ok(ArgsPreview::of(&host, argv.as_deref()))
}

/// Whether the launch options stored on `route` need confirming on the game page before they
/// run (they came from a backup or an older version, or the route's program, its launcher's
/// command line or working folder changed).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlinking_is_refused_while_the_game_is_running() {
        let mut sessions = tracker::Sessions::default();
        assert!(not_running(&sessions, "g").is_ok());
        sessions.launch("g", 1_000);
        let message = not_running(&sessions, "g").unwrap_err();
        assert!(message.contains("running"));
        assert!(not_running(&sessions, "other").is_ok());
    }
}
