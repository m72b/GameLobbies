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

/// The only store whose playtime is imported so far.
pub const STEAM: &str = "steam";
/// Rows in the top playtime list.
pub const TOP_LIMIT: usize = 50;

/// Adds the table of store-reported playtime (idempotent, no `user_version` change; older builds
/// ignore it). Called from `db::migrate`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS store_playtime(source TEXT NOT NULL,appId TEXT NOT NULL,seconds INTEGER NOT NULL,updatedAt INTEGER NOT NULL,PRIMARY KEY(source,appId));",
    )?;
    Ok(())
}

/// Replaces everything stored for `source` with `seconds_by_app` (app id to seconds), in one
/// transaction. Zero and negative figures are not stored.
pub fn replace_store_playtime(
    conn: &mut Connection,
    source: &str,
    seconds_by_app: &HashMap<String, i64>,
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM store_playtime WHERE source=?", [source])?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO store_playtime(source,appId,seconds,updatedAt) VALUES(?,?,?,?)",
        )?;
        for (app_id, seconds) in seconds_by_app.iter().filter(|(_, s)| **s > 0) {
            insert.execute(params![source, app_id, seconds, now])?;
        }
    }
    tx.commit()?;
    Ok(())
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
    /// Hidden games in the whole library, whether or not they are counted in the other figures.
    pub hidden: usize,
    pub favorite: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Playtime {
    pub total_seconds: i64,
    /// Total over the games with time above zero; zero when there are none.
    pub average_seconds: i64,
    pub played_games: usize,
    /// How much of the total comes from a store's figure rather than Game Lobby's own tracking.
    pub store_seconds: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallSize {
    /// Sum of the known sizes.
    pub bytes: u64,
    pub known: usize,
    /// Installed games without a measured size (not part of `bytes`).
    pub unknown: usize,
    /// Known sizes that are lower bounds.
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
    /// Absent when no store reported time for the game.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_seconds: Option<i64>,
    /// Which store `store_seconds` came from.
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
    /// A store ("Steam") or a custom launcher's name.
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
    /// Stores whose reported time is part of the figures (for the "includes ... playtime" hint).
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
        _ => "store",
    }
}

fn app_playtime(conn: &Connection) -> Result<HashMap<(String, String), i64>> {
    let mut stmt = conn.prepare("SELECT source,appId,seconds FROM store_playtime")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// A game's largest store-reported time across its routes, with the store it came from.
fn store_time(game: &Game, store: &HashMap<(String, String), i64>) -> Option<(i64, &'static str)> {
    game.routes
        .iter()
        .filter(|r: &&GameRoute| r.source_id == STEAM)
        .filter_map(|r| {
            let id = r.app_id.as_deref().filter(|id| !id.is_empty())?;
            let seconds = *store.get(&(STEAM.to_string(), id.to_string()))?;
            (seconds > 0).then_some((seconds, STEAM))
        })
        .max_by_key(|(seconds, _)| *seconds)
}

/// The name a game is counted under (the store shown on its card).
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

/// Everything the Statistics view shows, from the library as it is now.
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
            let tracked = game
                .playtime_seconds
                .unwrap_or(game.playtime_minutes * 60)
                .max(0);
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
pub async fn stats_summary(
    state: State<'_, AppState>,
    include_hidden: bool,
) -> CmdResult<StatsSummary> {
    summarize(&state.db.lock(), include_hidden).map_err(|e| format!("{e:#}"))
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
        replace_store_playtime(&mut conn, STEAM, &store, 1).unwrap();
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
            1,
        )
        .unwrap();
        replace_store_playtime(
            &mut conn,
            STEAM,
            &[("2".into(), 180), ("3".into(), 0)].into(),
            2,
        )
        .unwrap();
        let rows = app_playtime(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[&(STEAM.to_string(), "2".to_string())], 180);
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
}
