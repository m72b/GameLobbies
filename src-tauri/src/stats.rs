//! Library statistics for the Statistics view, and the table that keeps playtime a store
//! reports (Steam's `playtime_forever`) apart from the playtime Game Lobby tracks itself.
//!
//! `stats_summary` is read-only and takes one boolean (include hidden games). Everything is
//! computed here, in one pass over the library; the renderer only formats the result.
//!
//! Definitions:
//! - Per game, time played is `max(tracked, store-reported)`. A game's tracked time (sessions
//!   Game Lobby recorded) and Steam's own figure usually count the same hours, so the two are
//!   never added. Store figures live in `store_playtime` and are never merged into the tracked
//!   playtime; they are refetched on every Steam owned-games import and are not part of backups.
//! - Last played: the newer of the tracked time and the store's own last-played time.
//! - The part of the store's figure Game Lobby did not track, `max(0, store - tracked)`, is shown
//!   as one read-only "Added from Steam" entry in a game's activity (never a session, never
//!   written to the tracked tables). A game with several Steam routes uses the largest figure.
//! - Played: that time is above zero, or the game has a last-played time or a recorded session.
//!   Not played: the rest.
//! - Average playtime: total time over the games with time above zero.
//! - Library breakdown: every game counts once, under the store shown on its card (a Custom
//!   game run by a launcher is listed under the launcher's name).
//! - Install size: the cached sizes of installed games (`storage::report`); installed games
//!   without a known size are counted, not guessed.
//! - Hidden games are left out of every figure unless asked for; `hidden` is always the number
//!   of hidden games in the library.

use crate::state::AppState;
use crate::types::{Game, GameRoute};
use crate::{db, launchers, storage};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use tauri::State;

type CmdResult<T> = Result<T, String>;

/// Sources of store-reported playtime (`store_playtime.source`).
pub const STEAM: &str = "steam";
pub const GOG: &str = "gog";

/// The playtime source a route's figure is stored under, by the route's source: Steam routes
/// (installed or owned) and GOG routes (Galaxy's installed games and GOG account games).
pub fn playtime_source(route_source: &str) -> Option<&'static str> {
    match route_source {
        "steam" => Some(STEAM),
        "gog-galaxy" | "gog-account" => Some(GOG),
        _ => None,
    }
}
pub const TOP_LIMIT: usize = 50;

/// Adds the table of store-reported playtime (idempotent, no `user_version` change; older builds
/// ignore it). Called from `db::migrate`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS store_playtime(source TEXT NOT NULL,appId TEXT NOT NULL,seconds INTEGER NOT NULL,updatedAt INTEGER NOT NULL,lastPlayed INTEGER,PRIMARY KEY(source,appId));",
    )?;
    // The table of 0.6.0 had no `lastPlayed` (ms the store says the game was last played).
    let has_last_played = conn
        .prepare("SELECT name FROM pragma_table_info('store_playtime')")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "lastPlayed");
    if !has_last_played {
        conn.execute_batch("ALTER TABLE store_playtime ADD COLUMN lastPlayed INTEGER;")?;
    }
    Ok(())
}

/// Replaces everything stored for `source` with `seconds_by_app` (app id to seconds) and
/// `last_played_by_app` (app id to ms), in one transaction. Zero and negative figures are not
/// stored.
pub fn replace_store_playtime(
    conn: &mut Connection,
    source: &str,
    seconds_by_app: &HashMap<String, i64>,
    last_played_by_app: &HashMap<String, i64>,
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM store_playtime WHERE source=?", [source])?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO store_playtime(source,appId,seconds,updatedAt,lastPlayed) VALUES(?,?,?,?,?)",
        )?;
        for (app_id, seconds) in seconds_by_app.iter().filter(|(_, s)| **s > 0) {
            let last = last_played_by_app.get(app_id).filter(|v| **v > 0);
            insert.execute(params![source, app_id, seconds, now, last])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Writes the figures of the apps in `checked` (app id to seconds; zero is kept as "checked,
/// nothing played", which every reader ignores) and leaves every other row of `source` as it
/// is. For stores whose playtime is read a part at a time (GOG). One transaction.
pub fn merge_store_playtime(
    conn: &mut Connection,
    source: &str,
    checked: &HashMap<String, i64>,
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut upsert = tx.prepare(
            "INSERT INTO store_playtime(source,appId,seconds,updatedAt,lastPlayed) VALUES(?,?,?,?,NULL) ON CONFLICT(source,appId) DO UPDATE SET seconds=excluded.seconds,updatedAt=excluded.updatedAt",
        )?;
        for (app_id, seconds) in checked {
            upsert.execute(params![source, app_id, (*seconds).max(0), now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn prune_store_playtime(
    conn: &mut Connection,
    source: &str,
    keep: &HashSet<String>,
) -> Result<()> {
    let stored: Vec<String> = conn
        .prepare("SELECT appId FROM store_playtime WHERE source=?")?
        .query_map([source], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let tx = conn.transaction()?;
    for app_id in stored.iter().filter(|id| !keep.contains(*id)) {
        tx.execute(
            "DELETE FROM store_playtime WHERE source=? AND appId=?",
            params![source, app_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// When each app of `source` was last checked (app id to ms), to check the oldest first.
pub fn store_playtime_ages(conn: &Connection, source: &str) -> Result<HashMap<String, i64>> {
    let rows = conn
        .prepare("SELECT appId,updatedAt FROM store_playtime WHERE source=?")?
        .query_map([source], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Forgets everything stored for `source`.
pub fn clear_store_playtime(conn: &Connection, source: &str) -> Result<()> {
    conn.execute("DELETE FROM store_playtime WHERE source=?", [source])?;
    Ok(())
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub all: usize,
    pub installed: usize,
    pub not_installed: usize,
    pub hidden: usize,
    pub favorite: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Playtime {
    pub total_seconds: i64,
    pub average_seconds: i64,
    pub played_games: usize,
    pub store_seconds: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallSize {
    pub bytes: u64,
    pub known: usize,
    /// Installed games without a measured size (not part of `bytes`).
    pub unknown: usize,
    pub partial: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub played: usize,
    pub not_played: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TopGame {
    pub game_id: String,
    pub title: String,
    /// What counts: the larger of the two below.
    pub seconds: i64,
    pub tracked_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_source: Option<String>,
    /// Share of the total playtime, 0 to 100.
    pub percent: f64,
    pub store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    pub name: String,
    pub count: usize,
    /// Share of the counted games, 0 to 100.
    pub percent: f64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatsSummary {
    pub include_hidden: bool,
    pub counts: Counts,
    pub playtime: Playtime,
    pub install_size: InstallSize,
    pub completion: Completion,
    pub top: Vec<TopGame>,
    pub library: Vec<LibraryEntry>,
    pub store_sources: Vec<String>,
}

fn percent(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 1000.0).round() / 10.0
    }
}

fn store_label(source: &str) -> &'static str {
    match source {
        STEAM => "Steam",
        GOG => "GOG",
        _ => "store",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoreRow {
    pub seconds: i64,
    /// Ms the store says the game was last played.
    pub last_played: Option<i64>,
    /// Ms of the sync that wrote the row.
    pub updated_at: i64,
}

type StoreRows = HashMap<(String, String), StoreRow>;

fn app_playtime(conn: &Connection) -> Result<StoreRows> {
    let mut stmt =
        conn.prepare("SELECT source,appId,seconds,lastPlayed,updatedAt FROM store_playtime")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                StoreRow {
                    seconds: r.get::<_, i64>(2)?,
                    last_played: r.get::<_, Option<i64>>(3)?.filter(|v| *v > 0),
                    updated_at: r.get::<_, i64>(4)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// What the stores report for a game: the largest figure across its Steam and GOG routes (with
/// the source it came from), the latest last-played time and the latest sync among those
/// routes. `None` when no route has a figure.
fn game_store_row(game: &Game, store: &StoreRows) -> Option<(&'static str, StoreRow)> {
    let rows: Vec<(&'static str, &StoreRow)> = game
        .routes
        .iter()
        .filter_map(|r: &GameRoute| {
            let source = playtime_source(&r.source_id)?;
            let id = r.app_id.as_deref().filter(|id| !id.is_empty())?;
            store
                .get(&(source.to_string(), id.to_string()))
                .filter(|row| row.seconds > 0)
                .map(|row| (source, row))
        })
        .collect();
    let (source, best) = rows.iter().max_by_key(|(_, r)| r.seconds)?;
    Some((
        source,
        StoreRow {
            seconds: best.seconds,
            last_played: rows.iter().filter_map(|(_, r)| r.last_played).max(),
            updated_at: rows.iter().map(|(_, r)| r.updated_at).max().unwrap_or(0),
        },
    ))
}

fn store_time(game: &Game, store: &StoreRows) -> Option<(i64, &'static str)> {
    game_store_row(game, store).map(|(source, row)| (row.seconds, source))
}

/// How a game's playtime reads once the store's figure is counted. Delivered to the renderer
/// (`stats_store_times`, `tracking_stats`); it is never stored and never written to the tracked
/// tables.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreTime {
    /// `"steam"` or `"gog"`.
    pub source: String,
    pub seconds: i64,
    pub tracked_seconds: i64,
    /// What counts: `max(tracked, store)`.
    pub effective_seconds: i64,
    /// `max(0, store - tracked)`: the hours the store has that Game Lobby did not track.
    pub added_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_last_played: Option<i64>,
    pub synced_at: i64,
}

/// The pure rules: effective time is the larger figure, the added part never negative, last
/// played the newer of the two (for a Steam figure).
#[cfg(test)]
pub fn combine(
    tracked_seconds: i64,
    tracked_last_played: Option<i64>,
    row: &StoreRow,
) -> StoreTime {
    combine_for(STEAM, tracked_seconds, tracked_last_played, row)
}

pub fn combine_for(
    source: &str,
    tracked_seconds: i64,
    tracked_last_played: Option<i64>,
    row: &StoreRow,
) -> StoreTime {
    let tracked = tracked_seconds.max(0);
    let store = row.seconds.max(0);
    StoreTime {
        source: source.to_string(),
        seconds: store,
        tracked_seconds: tracked,
        effective_seconds: tracked.max(store),
        added_seconds: (store - tracked).max(0),
        last_played: row.last_played,
        effective_last_played: tracked_last_played
            .filter(|v| *v > 0)
            .into_iter()
            .chain(row.last_played)
            .max(),
        synced_at: row.updated_at,
    }
}

/// A game's tracked time in seconds (older rows only have minutes).
pub fn tracked_seconds(game: &Game) -> i64 {
    game.playtime_seconds
        .unwrap_or(game.playtime_minutes * 60)
        .max(0)
}

fn game_store_time(game: &Game, rows: &StoreRows) -> Option<StoreTime> {
    let (source, row) = game_store_row(game, rows)?;
    Some(combine_for(
        source,
        tracked_seconds(game),
        game.last_played,
        &row,
    ))
}

/// One game's playtime with the store's figure counted, or `None` when no store reports time
/// for it. `tracked_last_played` replaces the game's own last-played time (the game page also
/// knows its sessions).
pub fn store_time_of(
    conn: &Connection,
    game: &Game,
    tracked_last_played: Option<i64>,
) -> Result<Option<StoreTime>> {
    let rows = app_playtime(conn)?;
    Ok(game_store_row(game, &rows)
        .map(|(source, row)| combine_for(source, tracked_seconds(game), tracked_last_played, &row)))
}

pub fn store_times(conn: &Connection) -> Result<HashMap<String, StoreTime>> {
    let rows = app_playtime(conn)?;
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(db::all_games(conn)?
        .into_iter()
        .filter_map(|game| game_store_time(&game, &rows).map(|t| (game.id, t)))
        .collect())
}

/// The read-only "Added from Steam" entry of a game's activity (never a play session).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreAddition {
    pub game_id: String,
    pub game_title: String,
    pub source: String,
    /// The hours the store has that Game Lobby did not track; always above zero.
    pub seconds: i64,
    pub store_seconds: i64,
    pub tracked_seconds: i64,
    /// Ms of the last sync; the entry is dated here.
    pub synced_at: i64,
}

pub fn additions(conn: &Connection, game_id: Option<&str>) -> Result<Vec<StoreAddition>> {
    let rows = app_playtime(conn)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let games = match game_id {
        Some(id) => db::get_game(conn, id)?.into_iter().collect(),
        None => db::all_games(conn)?,
    };
    let mut out: Vec<StoreAddition> = games
        .iter()
        .filter_map(|game| {
            let t = game_store_time(game, &rows)?;
            (t.added_seconds > 0).then(|| StoreAddition {
                game_id: game.id.clone(),
                game_title: game.title.clone(),
                source: t.source,
                seconds: t.added_seconds,
                store_seconds: t.seconds,
                tracked_seconds: t.tracked_seconds,
                synced_at: t.synced_at,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.synced_at
            .cmp(&a.synced_at)
            .then_with(|| a.game_title.cmp(&b.game_title))
    });
    Ok(out)
}

fn library_name(game: &Game, launcher_names: &HashMap<String, String>) -> String {
    if game.store == "Custom" {
        if let Some(name) = game
            .routes
            .iter()
            .filter_map(launchers::route_launcher)
            .find_map(|id| launcher_names.get(id))
        {
            return name.clone();
        }
    }
    game.store.clone()
}

fn sessions_exist(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT gameId FROM play_sessions")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn summarize(conn: &Connection, include_hidden: bool) -> Result<StatsSummary> {
    let all_games = db::all_games(conn)?;
    let hidden = all_games.iter().filter(|g| g.hidden).count();
    let games: Vec<&Game> = all_games
        .iter()
        .filter(|g| include_hidden || !g.hidden)
        .collect();
    let store = app_playtime(conn)?;
    let with_sessions = sessions_exist(conn)?;
    let launcher_names: HashMap<String, String> = launchers::list(conn)?
        .into_iter()
        .map(|l| (l.id, l.name))
        .collect();

    struct Row<'a> {
        game: &'a Game,
        tracked: i64,
        store: Option<(i64, &'static str)>,
        seconds: i64,
    }
    let rows: Vec<Row> = games
        .iter()
        .map(|game| {
            let tracked = tracked_seconds(game);
            let store = store_time(game, &store);
            let seconds = tracked.max(store.map_or(0, |(s, _)| s));
            Row {
                game,
                tracked,
                store,
                seconds,
            }
        })
        .collect();

    let mut summary = StatsSummary {
        include_hidden,
        counts: Counts {
            all: games.len(),
            hidden,
            favorite: games.iter().filter(|g| g.favorite).count(),
            ..Default::default()
        },
        ..Default::default()
    };

    // Install state and sizes.
    let report = storage::report(conn)?;
    let sizes: HashMap<&str, &storage::StorageEntry> = report
        .entries
        .iter()
        .map(|e| (e.game_id.as_str(), e))
        .collect();
    for game in &games {
        if storage::is_installed(game) {
            summary.counts.installed += 1;
            match sizes.get(game.id.as_str()) {
                Some(entry) if entry.bytes.is_some() => {
                    summary.install_size.known += 1;
                    summary.install_size.bytes = summary
                        .install_size
                        .bytes
                        .saturating_add(entry.bytes.unwrap_or(0));
                    summary.install_size.partial += usize::from(entry.partial);
                }
                _ => summary.install_size.unknown += 1,
            }
        }
    }
    summary.counts.not_installed = summary.counts.all - summary.counts.installed;

    // Playtime and completion.
    let mut played_games = 0usize;
    let mut total = 0i64;
    let mut from_store = 0i64;
    let mut sources = Vec::new();
    for row in &rows {
        total = total.saturating_add(row.seconds);
        if row.seconds > 0 {
            played_games += 1;
        }
        if let Some((seconds, source)) = row.store {
            // The store's figure is what counts only where it is the larger.
            if seconds > row.tracked {
                from_store = from_store.saturating_add(seconds - row.tracked);
                if !sources.contains(&source) {
                    sources.push(source);
                }
            }
        }
        let played = row.seconds > 0
            || row.game.last_played.is_some()
            || with_sessions.contains(&row.game.id);
        if played {
            summary.completion.played += 1;
        } else {
            summary.completion.not_played += 1;
        }
    }
    summary.playtime = Playtime {
        total_seconds: total,
        average_seconds: if played_games == 0 {
            0
        } else {
            total / played_games as i64
        },
        played_games,
        store_seconds: from_store,
    };
    summary.store_sources = sources
        .into_iter()
        .map(|s| store_label(s).to_string())
        .collect();

    // Top playtime.
    let mut ranked: Vec<&Row> = rows.iter().filter(|r| r.seconds > 0).collect();
    ranked.sort_by(|a, b| {
        b.seconds
            .cmp(&a.seconds)
            .then_with(|| {
                a.game
                    .title
                    .to_lowercase()
                    .cmp(&b.game.title.to_lowercase())
            })
            .then_with(|| a.game.id.cmp(&b.game.id))
    });
    summary.top = ranked
        .into_iter()
        .take(TOP_LIMIT)
        .map(|r| TopGame {
            game_id: r.game.id.clone(),
            title: r.game.title.clone(),
            seconds: r.seconds,
            tracked_seconds: r.tracked,
            store_seconds: r.store.map(|(s, _)| s),
            store_source: r.store.map(|(_, src)| store_label(src).to_string()),
            percent: percent(r.seconds, total),
            store: library_name(r.game, &launcher_names),
            cover: r.game.cover.clone(),
        })
        .collect();

    // Library breakdown.
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for game in &games {
        *by_name
            .entry(library_name(game, &launcher_names))
            .or_default() += 1;
    }
    let mut library: Vec<LibraryEntry> = by_name
        .into_iter()
        .map(|(name, count)| LibraryEntry {
            name,
            count,
            percent: percent(count as i64, games.len() as i64),
        })
        .collect();
    library.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    summary.library = library;
    Ok(summary)
}

#[tauri::command]
pub async fn stats_store_times(
    state: State<'_, AppState>,
) -> CmdResult<HashMap<String, StoreTime>> {
    store_times(&state.db.lock()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn stats_summary(
    state: State<'_, AppState>,
    include_hidden: bool,
) -> CmdResult<StatsSummary> {
    summarize(&state.db.lock(), include_hidden).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod gog_tests {
    use super::*;
    use crate::types::GameRoute;

    #[test]
    fn gog_playtime_counts_like_steam_and_names_its_source() {
        let database = crate::db::Db::memory().unwrap();
        let mut conn = database.lock();
        let game = Game {
            id: "gog:1207658924".into(),
            title: "Witcher".into(),
            store: "GOG".into(),
            playtime_seconds: Some(3600),
            routes: vec![GameRoute {
                source_id: "gog-account".into(),
                store: "GOG".into(),
                app_id: Some("1207658924".into()),
                launch_target: "goggalaxy://openGameView/1207658924".into(),
                owned: Some(true),
                installed: Some(false),
                ..Default::default()
            }],
            ..Default::default()
        };
        crate::db::upsert_imported_games(&mut conn, &[game]).unwrap();
        let seconds = HashMap::from([("1207658924".to_string(), 7200)]);
        let last = HashMap::from([("1207658924".to_string(), 1_700_000_000_000)]);
        replace_store_playtime(&mut conn, GOG, &seconds, &last, 5).unwrap();
        let times = store_times(&conn).unwrap();
        let t = &times["gog:1207658924"];
        assert_eq!(
            (t.source.as_str(), t.effective_seconds, t.added_seconds),
            ("gog", 7200, 3600)
        );
        let added = additions(&conn, None).unwrap();
        assert_eq!(added[0].source, "gog");
        let summary = summarize(&conn, false).unwrap();
        assert_eq!(summary.store_sources, ["GOG"]);
        assert_eq!(summary.top[0].store_source.as_deref(), Some("GOG"));
        assert_eq!(playtime_source("gog-galaxy"), Some(GOG));
        assert_eq!(playtime_source("humble-account"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn route(source: &str, app_id: Option<&str>, installed: bool, path: Option<&str>) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            store: if source == STEAM { "Steam" } else { "Custom" }.into(),
            app_id: app_id.map(str::to_string),
            launch_target: "x".into(),
            installed: Some(installed),
            install_path: path.map(str::to_string),
            ..Default::default()
        }
    }

    fn game(id: &str, store: &str, seconds: i64, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: id.into(),
            store: store.into(),
            playtime_seconds: Some(seconds),
            playtime_minutes: seconds / 60,
            routes,
            ..Default::default()
        }
    }

    fn put(conn: &mut Connection, game: &Game) {
        // Distinct targets, so imports never merge two test games into one.
        let mut game = game.clone();
        for (i, r) in game.routes.iter_mut().enumerate() {
            r.launch_target = format!("{}-{i}", game.id);
        }
        let game = &game;
        db::upsert_imported_games(conn, std::slice::from_ref(game)).unwrap();
        // Imports keep stored playtime, so set the figures directly.
        conn.execute(
            "UPDATE games SET playtimeSeconds=?,playtimeMinutes=?,lastPlayed=?,hidden=?,favorite=? WHERE id=?",
            params![
                game.playtime_seconds.unwrap_or(0),
                game.playtime_minutes,
                game.last_played,
                game.hidden,
                game.favorite,
                game.id
            ],
        )
        .unwrap();
    }

    fn setup() -> Db {
        Db::memory().unwrap()
    }

    #[test]
    fn an_empty_library_gives_zeros_and_empty_lists() {
        let db = setup();
        let s = summarize(&db.lock(), false).unwrap();
        assert_eq!(s.counts, Counts::default());
        assert_eq!(s.playtime, Playtime::default());
        assert_eq!(s.completion, Completion::default());
        assert!(s.top.is_empty() && s.library.is_empty() && s.store_sources.is_empty());
    }

    #[test]
    fn counts_playtime_average_and_completion() {
        let db = setup();
        let mut conn = db.lock();
        let mut a = game(
            "a",
            "Steam",
            3600,
            vec![route(STEAM, Some("1"), true, None)],
        );
        a.favorite = true;
        let b = game(
            "b",
            "Epic Games",
            7200,
            vec![route("epic", Some("b"), false, None)],
        );
        let c = game("c", "GOG", 0, vec![route("gog", Some("c"), false, None)]);
        let mut d = game("d", "GOG", 0, vec![route("gog", Some("d"), false, None)]);
        d.last_played = Some(5);
        for g in [&a, &b, &c, &d] {
            put(&mut conn, g);
        }
        let s = summarize(&conn, false).unwrap();
        assert_eq!(
            s.counts,
            Counts {
                all: 4,
                installed: 1,
                not_installed: 3,
                hidden: 0,
                favorite: 1
            }
        );
        assert_eq!(s.playtime.total_seconds, 10_800);
        // Average over the two games with time, not over all four.
        assert_eq!(s.playtime.average_seconds, 5_400);
        assert_eq!(s.playtime.played_games, 2);
        // d has a last-played time without playtime: played. c: not played.
        assert_eq!(
            s.completion,
            Completion {
                played: 3,
                not_played: 1
            }
        );
        assert_eq!(
            s.top.iter().map(|t| t.game_id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(s.top[0].percent, 66.7);
        assert_eq!(s.top[1].percent, 33.3);
    }

    #[test]
    fn a_recorded_session_counts_as_played_even_without_time() {
        let db = setup();
        let mut conn = db.lock();
        put(
            &mut conn,
            &game("a", "GOG", 0, vec![route("gog", Some("a"), false, None)]),
        );
        conn.execute(
            "INSERT INTO play_sessions(gameId,startedAt,endedAt,seconds,source) VALUES('a',1,1,0,'launch')",
            [],
        )
        .unwrap();
        let s = summarize(&conn, false).unwrap();
        assert_eq!(
            s.completion,
            Completion {
                played: 1,
                not_played: 0
            }
        );
        assert_eq!(s.playtime.average_seconds, 0);
    }

    #[test]
    fn hidden_games_are_left_out_unless_asked_for() {
        let db = setup();
        let mut conn = db.lock();
        let mut h = game(
            "h",
            "Steam",
            1000,
            vec![route(STEAM, Some("9"), true, None)],
        );
        h.hidden = true;
        put(&mut conn, &h);
        put(
            &mut conn,
            &game("v", "Steam", 500, vec![route(STEAM, Some("8"), true, None)]),
        );
        let without = summarize(&conn, false).unwrap();
        assert_eq!(without.counts.all, 1);
        assert_eq!(
            without.counts.hidden, 1,
            "hidden is always the library's number"
        );
        assert_eq!(without.playtime.total_seconds, 500);
        assert_eq!(
            without.library,
            [LibraryEntry {
                name: "Steam".into(),
                count: 1,
                percent: 100.0
            }]
        );
        assert_eq!(without.install_size.unknown, 1);
        let with = summarize(&conn, true).unwrap();
        assert!(with.include_hidden);
        assert_eq!(with.counts.all, 2);
        assert_eq!(with.playtime.total_seconds, 1500);
        assert_eq!(with.install_size.unknown, 2);
    }

    #[test]
    fn the_larger_of_tracked_and_store_time_counts_and_is_never_added() {
        let db = setup();
        let mut conn = db.lock();
        // Tracked more than Steam says, Steam says more than tracked, and an unplayed game.
        put(
            &mut conn,
            &game(
                "t",
                "Steam",
                7200,
                vec![route(STEAM, Some("1"), true, None)],
            ),
        );
        put(
            &mut conn,
            &game(
                "s",
                "Steam",
                600,
                vec![route(STEAM, Some("2"), false, None)],
            ),
        );
        put(
            &mut conn,
            &game("n", "Steam", 0, vec![route(STEAM, Some("3"), false, None)]),
        );
        let store: HashMap<String, i64> = [
            ("1".into(), 3600),
            ("2".into(), 36_000),
            ("3".into(), 1800),
            ("99".into(), 5),
        ]
        .into();
        replace_store_playtime(&mut conn, STEAM, &store, &HashMap::new(), 1).unwrap();
        let s = summarize(&conn, false).unwrap();
        let by_id = |id: &str| s.top.iter().find(|t| t.game_id == id).unwrap().clone();
        assert_eq!(by_id("t").seconds, 7200);
        assert_eq!(by_id("t").store_seconds, Some(3600));
        assert_eq!(by_id("s").seconds, 36_000);
        assert_eq!(by_id("s").tracked_seconds, 600);
        assert_eq!(by_id("s").store_source.as_deref(), Some("Steam"));
        assert_eq!(
            by_id("n").seconds,
            1800,
            "store time alone makes a game played"
        );
        assert_eq!(s.playtime.total_seconds, 7200 + 36_000 + 1800);
        assert_eq!(
            s.completion,
            Completion {
                played: 3,
                not_played: 0
            }
        );
        // Only the part of the total that exceeds what Game Lobby tracked is from the store.
        assert_eq!(s.playtime.store_seconds, (36_000 - 600) + 1800);
        assert_eq!(s.store_sources, ["Steam"]);
        // Tracked playtime itself is untouched.
        let tracked: i64 = conn
            .query_row("SELECT playtimeSeconds FROM games WHERE id='s'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(tracked, 600);
    }

    #[test]
    fn replacing_store_playtime_drops_old_rows_and_zeros() {
        let db = setup();
        let mut conn = db.lock();
        replace_store_playtime(
            &mut conn,
            STEAM,
            &[("1".into(), 60), ("2".into(), 120)].into(),
            &HashMap::new(),
            1,
        )
        .unwrap();
        replace_store_playtime(
            &mut conn,
            STEAM,
            &[("2".into(), 180), ("3".into(), 0)].into(),
            &HashMap::new(),
            2,
        )
        .unwrap();
        let rows = app_playtime(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[&(STEAM.to_string(), "2".to_string())].seconds, 180);
        clear_store_playtime(&conn, STEAM).unwrap();
        assert!(app_playtime(&conn).unwrap().is_empty());
    }

    #[test]
    fn the_library_counts_each_game_once_under_its_card_store() {
        let db = setup();
        let mut conn = db.lock();
        conn.execute(
            "INSERT INTO custom_launchers(id,name,iconKind,iconValue,color,executable,argsTemplate,targetKind,sortOrder,createdAt) VALUES('l1','My Launcher','builtin','rocket','#4f8cff','','{target}','folder',0,1)",
            [],
        )
        .unwrap();
        // A linked game with two routes counts once, under its own store.
        put(
            &mut conn,
            &game(
                "two",
                "Steam",
                0,
                vec![
                    route(STEAM, Some("1"), false, None),
                    route("gog", Some("g"), false, None),
                ],
            ),
        );
        put(
            &mut conn,
            &game("s2", "Steam", 0, vec![route(STEAM, Some("2"), false, None)]),
        );
        let mut labelled = route("local", None, true, None);
        labelled.launcher_id = Some("l1".into());
        put(&mut conn, &game("lab", "Custom", 0, vec![labelled]));
        put(
            &mut conn,
            &game("plain", "Custom", 0, vec![route("local", None, true, None)]),
        );
        let s = summarize(&conn, false).unwrap();
        let got: Vec<(&str, usize, f64)> = s
            .library
            .iter()
            .map(|e| (e.name.as_str(), e.count, e.percent))
            .collect();
        assert_eq!(
            got,
            [
                ("Steam", 2, 50.0),
                ("Custom", 1, 25.0),
                ("My Launcher", 1, 25.0)
            ]
        );
        assert_eq!(
            s.library.iter().map(|e| e.count).sum::<usize>(),
            s.counts.all
        );
    }

    #[test]
    fn install_sizes_sum_the_known_ones_and_count_the_unknown() {
        let db = setup();
        let mut conn = db.lock();
        put(
            &mut conn,
            &game(
                "k",
                "Steam",
                0,
                vec![route(STEAM, Some("1"), true, Some("C:\\g\\k"))],
            ),
        );
        put(
            &mut conn,
            &game(
                "u",
                "Steam",
                0,
                vec![route(STEAM, Some("2"), true, Some("C:\\g\\u"))],
            ),
        );
        put(
            &mut conn,
            &game("np", "Steam", 0, vec![route(STEAM, Some("3"), true, None)]),
        );
        put(
            &mut conn,
            &game(
                "off",
                "Steam",
                0,
                vec![route(STEAM, Some("4"), false, Some("C:\\g\\off"))],
            ),
        );
        conn.execute(
            "INSERT INTO install_sizes(gameId,path,bytes,measuredAt,partial,source) VALUES('k','C:\\g\\k',1000,1,0,'scan')",
            [],
        )
        .unwrap();
        // A size cached for a game that is not installed never counts.
        conn.execute(
            "INSERT INTO install_sizes(gameId,path,bytes,measuredAt,partial,source) VALUES('off','C:\\g\\off',5000,1,0,'scan')",
            [],
        )
        .unwrap();
        let s = summarize(&conn, false).unwrap();
        assert_eq!(s.counts.installed, 3);
        assert_eq!(
            s.install_size,
            InstallSize {
                bytes: 1000,
                known: 1,
                unknown: 2,
                partial: 0
            }
        );
    }

    #[test]
    fn percentages_round_to_one_decimal_and_guard_zero() {
        assert_eq!(percent(1, 3), 33.3);
        assert_eq!(percent(2, 3), 66.7);
        assert_eq!(percent(5, 0), 0.0);
    }

    #[test]
    fn the_top_list_is_capped() {
        let db = setup();
        let mut conn = db.lock();
        for i in 0..(TOP_LIMIT + 5) {
            put(
                &mut conn,
                &game(
                    &format!("g{i:03}"),
                    "GOG",
                    100 + i as i64,
                    vec![route("gog", Some(&format!("x{i}")), false, None)],
                ),
            );
        }
        let s = summarize(&conn, false).unwrap();
        assert_eq!(s.top.len(), TOP_LIMIT);
        assert_eq!(s.top[0].game_id, format!("g{:03}", TOP_LIMIT + 4));
    }

    fn row(seconds: i64, last_played: Option<i64>, updated_at: i64) -> StoreRow {
        StoreRow {
            seconds,
            last_played,
            updated_at,
        }
    }

    const H: i64 = 3600;

    #[test]
    fn effective_playtime_is_the_larger_figure_and_the_added_part_never_negative() {
        // Steam has 165 h, Game Lobby tracked 40 h: 165 h count, 125 h are "added".
        let t = combine(40 * H, None, &row(165 * H, None, 9));
        assert_eq!(
            (
                t.effective_seconds,
                t.added_seconds,
                t.seconds,
                t.tracked_seconds
            ),
            (165 * H, 125 * H, 165 * H, 40 * H)
        );
        assert_eq!(t.synced_at, 9);
        // Tracked is larger: tracked counts, nothing is added (never negative).
        let t = combine(200 * H, None, &row(165 * H, None, 9));
        assert_eq!((t.effective_seconds, t.added_seconds), (200 * H, 0));
        // Equal: nothing added.
        let t = combine(165 * H, None, &row(165 * H, None, 9));
        assert_eq!((t.effective_seconds, t.added_seconds), (165 * H, 0));
        // A negative tracked figure is treated as zero.
        let t = combine(-5, None, &row(60, None, 9));
        assert_eq!((t.tracked_seconds, t.added_seconds), (0, 60));
    }

    #[test]
    fn last_played_is_the_newer_of_tracked_and_steam() {
        let last = |tracked, steam| combine(0, tracked, &row(60, steam, 1)).effective_last_played;
        assert_eq!(last(Some(2_000), Some(5_000)), Some(5_000));
        assert_eq!(last(Some(9_000), Some(5_000)), Some(9_000));
        assert_eq!(last(None, Some(5_000)), Some(5_000));
        assert_eq!(last(Some(7), None), Some(7));
        assert_eq!(last(None, None), None);
        // Zero means unknown.
        assert_eq!(last(Some(0), None), None);
    }

    fn steam_game(conn: &mut Connection, id: &str, tracked: i64, apps: &[&str]) {
        put(
            conn,
            &game(
                id,
                "Steam",
                tracked,
                apps.iter()
                    .map(|a| route(STEAM, Some(a), false, None))
                    .collect(),
            ),
        );
    }

    #[test]
    fn additions_cover_each_game_with_a_positive_difference_and_use_the_largest_steam_route() {
        let db = setup();
        let mut conn = db.lock();
        steam_game(&mut conn, "more", 40 * H, &["1"]);
        steam_game(&mut conn, "same", 10 * H, &["2"]);
        steam_game(&mut conn, "less", 50 * H, &["3"]);
        // A linked game with two Steam routes: the larger Steam figure counts, not the sum.
        steam_game(&mut conn, "linked", 5 * H, &["4", "5"]);
        steam_game(&mut conn, "none", 5 * H, &["6"]);
        let store: HashMap<String, i64> = [
            ("1".into(), 165 * H),
            ("2".into(), 10 * H),
            ("3".into(), 20 * H),
            ("4".into(), 30 * H),
            ("5".into(), 20 * H),
        ]
        .into();
        let last: HashMap<String, i64> = [("4".into(), 8_000), ("5".into(), 9_000)].into();
        replace_store_playtime(&mut conn, STEAM, &store, &last, 777).unwrap();

        let all = additions(&conn, None).unwrap();
        let by_id = |id: &str| all.iter().find(|a| a.game_id == id).cloned();
        assert_eq!(by_id("more").unwrap().seconds, 125 * H);
        assert_eq!(by_id("more").unwrap().synced_at, 777);
        assert!(by_id("same").is_none(), "a zero difference is hidden");
        assert!(by_id("less").is_none(), "never negative");
        assert!(by_id("none").is_none());
        let linked = by_id("linked").unwrap();
        assert_eq!(
            (linked.store_seconds, linked.seconds),
            (30 * H, 25 * H),
            "max across routes, not the sum"
        );
        assert_eq!(all.len(), 2);
        assert_eq!(additions(&conn, Some("more")).unwrap().len(), 1);
        assert!(additions(&conn, Some("same")).unwrap().is_empty());
        assert!(additions(&conn, Some("missing")).unwrap().is_empty());

        let times = store_times(&conn).unwrap();
        assert_eq!(times.len(), 4);
        assert_eq!(times["linked"].effective_seconds, 30 * H);
        assert_eq!(times["linked"].last_played, Some(9_000));
        assert_eq!(times["less"].effective_seconds, 50 * H);
        assert_eq!(times["less"].added_seconds, 0);
    }

    #[test]
    fn reading_store_time_never_writes_to_tracked_tables() {
        let db = setup();
        let mut conn = db.lock();
        steam_game(&mut conn, "g", 600, &["1"]);
        db::insert_session(&conn, "g", 1_000_000, 600, "launch").unwrap();
        replace_store_playtime(
            &mut conn,
            STEAM,
            &[("1".into(), 9 * H)].into(),
            &[("1".into(), 5_000_000)].into(),
            3,
        )
        .unwrap();
        let snapshot = |conn: &Connection| -> (String, String) {
            let games: String = conn
                .query_row(
                    "SELECT group_concat(id||':'||playtimeSeconds||':'||COALESCE(lastPlayed,''),'|') FROM games",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let sessions: String = conn
                .query_row(
                    "SELECT group_concat(gameId||':'||seconds||':'||startedAt,'|') FROM play_sessions",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (games, sessions)
        };
        let before = snapshot(&conn);
        store_times(&conn).unwrap();
        additions(&conn, None).unwrap();
        let stats = db::play_stats(&conn, "g", 10_000_000).unwrap();
        let page = crate::sessions::page(&conn, Some("g"), None, 10).unwrap();
        assert_eq!(snapshot(&conn), before);

        // The game page totals count Steam's figure and date; sessions stay real sessions.
        assert_eq!(stats.total_seconds, 9 * H);
        assert_eq!(stats.last_played, Some(5_000_000));
        assert_eq!(stats.session_count, 1);
        assert_eq!(stats.first_played, Some(1_000_000));
        assert_eq!(stats.store.as_ref().unwrap().added_seconds, 9 * H - 600);
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.store_entries.len(), 1);
        assert_eq!(page.store_entries[0].seconds, 9 * H - 600);
        // Only the first page carries the entries.
        let later = crate::sessions::page(
            &conn,
            Some("g"),
            Some(crate::sessions::Cursor {
                started_at: 2_000_000,
                id: 99,
            }),
            10,
        )
        .unwrap();
        assert!(later.store_entries.is_empty());
    }

    #[test]
    fn a_database_from_before_last_played_gains_the_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE store_playtime(source TEXT NOT NULL,appId TEXT NOT NULL,seconds INTEGER NOT NULL,updatedAt INTEGER NOT NULL,PRIMARY KEY(source,appId));INSERT INTO store_playtime VALUES('steam','1',60,5);",
        )
        .unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let rows = app_playtime(&conn).unwrap();
        assert_eq!(
            rows[&(STEAM.to_string(), "1".to_string())],
            row(60, None, 5)
        );
    }
}
