//! SQLite library storage. Ported from the Electron `src/main/db/index.ts`; the schema is
//! unchanged so existing `library.sqlite` files open as-is.

use crate::types::*;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const GAMES_TABLE: &str = "CREATE TABLE games(id TEXT PRIMARY KEY,title TEXT NOT NULL COLLATE NOCASE,cover TEXT,defaultCover TEXT,store TEXT NOT NULL,genre TEXT,playtimeMinutes INTEGER NOT NULL DEFAULT 0,playtimeSeconds INTEGER NOT NULL DEFAULT 0,lastPlayed INTEGER,hidden INTEGER NOT NULL DEFAULT 0,favorite INTEGER NOT NULL DEFAULT 0,routes TEXT NOT NULL DEFAULT '[]')";

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(file: &Path) -> Result<Self> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
        migrate(&conn, Some(file))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn, None)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn migrate(db: &Connection, file: Option<&Path>) -> Result<()> {
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.execute_batch(&format!(
        "{};
        CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS emulators(id TEXT PRIMARY KEY,name TEXT NOT NULL,executable TEXT NOT NULL,args TEXT NOT NULL,romFolders TEXT NOT NULL,extensions TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS plugins(id TEXT PRIMARY KEY,manifest TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS library_connections(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS secrets(key TEXT PRIMARY KEY,value BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS play_sessions(id INTEGER PRIMARY KEY,gameId TEXT NOT NULL,startedAt INTEGER NOT NULL,endedAt INTEGER,seconds INTEGER NOT NULL DEFAULT 0,source TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS play_sessions_game ON play_sessions(gameId,startedAt);
        CREATE TABLE IF NOT EXISTS game_art(gameId TEXT PRIMARY KEY,hero TEXT NOT NULL DEFAULT '[]',logo TEXT NOT NULL DEFAULT '[]',prefs TEXT NOT NULL DEFAULT '',fetchedAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS game_page_custom(gameId TEXT PRIMARY KEY,hero TEXT,logo TEXT,logoPosition TEXT NOT NULL DEFAULT 'bottom-left',logoScale REAL NOT NULL DEFAULT 1,updatedAt INTEGER NOT NULL);",
        GAMES_TABLE.replace("CREATE TABLE games", "CREATE TABLE IF NOT EXISTS games")
    ))?;
    let columns: HashSet<String> = db
        .prepare("SELECT name FROM pragma_table_info('games')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.contains("playtimeSeconds") {
        db.execute_batch(
            "ALTER TABLE games ADD COLUMN playtimeSeconds INTEGER NOT NULL DEFAULT 0;
             UPDATE games SET playtimeSeconds=playtimeMinutes*60;",
        )?;
    }
    let schema: String = db
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='games'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let unique_title =
        regex::Regex::new(r"(?i)title\s+TEXT\s+NOT\s+NULL\s+COLLATE\s+NOCASE\s+UNIQUE")?;
    if unique_title.is_match(&schema) {
        if let Some(file) = file {
            let busy: i64 = db.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| row.get(0))?;
            if busy != 0 {
                bail!("Could not safely checkpoint the library before its database migration.");
            }
            let backup = file.with_file_name(format!(
                "library.sqlite.pre-title-migration-{}.bak",
                now_ms()
            ));
            std::fs::copy(file, backup)?;
        }
        db.execute_batch(&format!(
            "BEGIN;
             ALTER TABLE games RENAME TO games_unique_title_old;
             {GAMES_TABLE};
             INSERT INTO games(id,title,cover,defaultCover,store,genre,playtimeMinutes,playtimeSeconds,lastPlayed,hidden,favorite,routes) SELECT id,title,cover,defaultCover,store,genre,playtimeMinutes,playtimeSeconds,lastPlayed,hidden,favorite,routes FROM games_unique_title_old;
             DROP TABLE games_unique_title_old;
             COMMIT;"
        ))?;
    }
    // Sessions still open belong to a previous run that ended without closing them (crash,
    // power loss): they keep their last saved length and are closed at that point.
    db.execute(
        "UPDATE play_sessions SET endedAt=startedAt+seconds*1000 WHERE endedAt IS NULL",
        [],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO settings(key,value) VALUES(?,?)",
        params!["app", serde_json::to_string(&Settings::default())?],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rows

#[derive(Clone, Debug, Default)]
struct GameRow {
    id: String,
    title: Option<String>,
    cover: Option<String>,
    default_cover: Option<String>,
    store: Option<String>,
    genre: Option<String>,
    playtime_minutes: i64,
    playtime_seconds: Option<i64>,
    last_played: Option<i64>,
    hidden: bool,
    favorite: bool,
    routes: Vec<GameRoute>,
}

impl GameRow {
    fn seconds(&self) -> i64 {
        self.playtime_seconds.unwrap_or(self.playtime_minutes * 60)
    }
}

fn col_int(row: &Row, name: &str) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(name)? {
        ValueRef::Integer(i) => Some(i),
        ValueRef::Real(f) if f.is_finite() => Some(f.floor() as i64),
        ValueRef::Text(t) => std::str::from_utf8(t)
            .ok()
            .and_then(|s| s.trim().parse().ok()),
        _ => None,
    })
}

fn col_text(row: &Row, name: &str) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(name)? {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        _ => None,
    })
}

fn parse_routes(raw: Option<String>) -> Vec<GameRoute> {
    raw.filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn read_row(row: &Row) -> rusqlite::Result<GameRow> {
    Ok(GameRow {
        id: col_text(row, "id")?.unwrap_or_default(),
        title: col_text(row, "title")?,
        cover: col_text(row, "cover")?,
        default_cover: col_text(row, "defaultCover")?,
        store: col_text(row, "store")?,
        genre: col_text(row, "genre")?,
        playtime_minutes: col_int(row, "playtimeMinutes")?.unwrap_or(0),
        playtime_seconds: col_int(row, "playtimeSeconds")?,
        last_played: col_int(row, "lastPlayed")?,
        hidden: col_int(row, "hidden")?.unwrap_or(0) != 0,
        favorite: col_int(row, "favorite")?.unwrap_or(0) != 0,
        routes: parse_routes(col_text(row, "routes")?),
    })
}

fn row_to_game(r: GameRow) -> Game {
    let seconds = r.seconds();
    let some = |v: Option<String>| v.filter(|s| !s.is_empty());
    Game {
        id: r.id,
        title: r.title.unwrap_or_default(),
        cover: some(r.cover),
        default_cover: some(r.default_cover),
        store: r.store.unwrap_or_default(),
        genre: some(r.genre),
        playtime_minutes: seconds.div_euclid(60),
        playtime_seconds: Some(seconds),
        last_played: r.last_played.filter(|v| *v != 0),
        hidden: r.hidden,
        favorite: r.favorite,
        routes: r.routes,
    }
}

fn all_rows(db: &Connection) -> Result<Vec<GameRow>> {
    let mut stmt = db.prepare("SELECT * FROM games")?;
    let rows = stmt
        .query_map([], read_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn get_row(db: &Connection, id: &str) -> Result<Option<GameRow>> {
    Ok(db
        .query_row("SELECT * FROM games WHERE id=?", [id], read_row)
        .optional()?)
}

pub fn all_games(db: &Connection) -> Result<Vec<Game>> {
    let mut stmt = db.prepare("SELECT * FROM games ORDER BY title COLLATE NOCASE")?;
    let rows = stmt
        .query_map([], read_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(row_to_game).collect())
}

pub fn get_game(db: &Connection, id: &str) -> Result<Option<Game>> {
    Ok(get_row(db, id)?.map(row_to_game))
}

pub fn game_count(db: &Connection) -> Result<usize> {
    Ok(db.query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))? as usize)
}

/// Inserts or updates a game. Fields missing from the patch keep their stored value.
fn persist_game(db: &Connection, game: &GamePatch) -> Result<GameRow> {
    let old = get_row(db, &game.id)?;
    let old_ref = old.as_ref();
    let previous_seconds = old_ref.map(GameRow::seconds).unwrap_or(0);
    let seconds = game
        .playtime_seconds
        .or(game.playtime_minutes.map(|m| m * 60))
        .unwrap_or(previous_seconds);
    let previous_title = old_ref
        .and_then(|o| o.title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled".into());
    let previous_store = old_ref
        .and_then(|o| o.store.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Custom".into());
    let row = GameRow {
        id: game.id.clone(),
        title: Some(
            game.title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or(previous_title),
        ),
        cover: game
            .cover
            .clone()
            .or_else(|| old_ref.and_then(|o| o.cover.clone())),
        default_cover: game
            .default_cover
            .clone()
            .or_else(|| old_ref.and_then(|o| o.default_cover.clone())),
        store: Some(
            game.store
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or(previous_store),
        ),
        genre: game
            .genre
            .clone()
            .or_else(|| old_ref.and_then(|o| o.genre.clone())),
        playtime_minutes: seconds.div_euclid(60),
        playtime_seconds: Some(seconds),
        last_played: game.last_played.or(old_ref.and_then(|o| o.last_played)),
        hidden: game.hidden.unwrap_or(old_ref.is_some_and(|o| o.hidden)),
        favorite: game.favorite.unwrap_or(old_ref.is_some_and(|o| o.favorite)),
        routes: game
            .routes
            .clone()
            .unwrap_or_else(|| old_ref.map(|o| o.routes.clone()).unwrap_or_default()),
    };
    db.execute(
        "INSERT INTO games(id,title,cover,defaultCover,store,genre,playtimeMinutes,playtimeSeconds,lastPlayed,hidden,favorite,routes)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(id) DO UPDATE SET title=excluded.title,cover=excluded.cover,defaultCover=excluded.defaultCover,store=excluded.store,genre=excluded.genre,playtimeMinutes=excluded.playtimeMinutes,playtimeSeconds=excluded.playtimeSeconds,lastPlayed=excluded.lastPlayed,hidden=excluded.hidden,favorite=excluded.favorite,routes=excluded.routes",
        params![
            row.id,
            row.title,
            row.cover,
            row.default_cover,
            row.store,
            row.genre,
            row.playtime_minutes,
            seconds,
            row.last_played,
            row.hidden as i64,
            row.favorite as i64,
            serde_json::to_string(&row.routes)?,
        ],
    )?;
    Ok(row)
}

pub fn save_game(db: &Connection, game: &GamePatch) -> Result<Vec<Game>> {
    persist_game(db, game)?;
    all_games(db)
}

pub fn route_identity(route: &GameRoute) -> String {
    let key = non_empty(&route.app_id).unwrap_or(&route.launch_target);
    format!("{}:{}", route.source_id.to_lowercase(), key.to_lowercase())
}

pub fn merge_route(existing: &GameRoute, incoming: &GameRoute) -> GameRoute {
    let mut combined = incoming.clone();
    combined.app_id = incoming.app_id.clone().or_else(|| existing.app_id.clone());
    combined.launch_args = incoming
        .launch_args
        .clone()
        .or_else(|| existing.launch_args.clone());
    combined.install_path = incoming
        .install_path
        .clone()
        .or_else(|| existing.install_path.clone());
    combined.owned = if existing.owned == Some(true) || incoming.owned == Some(true) {
        Some(true)
    } else if existing.owned.is_some() || incoming.owned.is_some() {
        Some(false)
    } else {
        None
    };
    let mut states = existing.install_states.clone().unwrap_or_default();
    states.extend(incoming.install_states.clone().unwrap_or_default());
    combined.installed = if !states.is_empty() {
        Some(states.values().any(|v| *v))
    } else {
        incoming.installed.or(existing.installed)
    };
    combined.install_states = Some(states);
    combined.playtime_minutes = match (existing.playtime_minutes, incoming.playtime_minutes) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let last = existing
        .last_played
        .unwrap_or(0)
        .max(incoming.last_played.unwrap_or(0));
    combined.last_played = (last != 0).then_some(last);
    combined
}

fn merge_routes_into(routes: &mut Vec<GameRoute>, incoming: &[GameRoute]) {
    for route in incoming {
        let identity = route_identity(route);
        match routes.iter().position(|r| route_identity(r) == identity) {
            Some(index) => routes[index] = merge_route(&routes[index], route),
            None => routes.push(route.clone()),
        }
    }
}

pub fn upsert_imported_games(db: &mut Connection, games: &[Game]) -> Result<()> {
    let rows = all_rows(db)?;
    let mut by_id: HashMap<String, GameRow> = HashMap::new();
    let mut by_route: HashMap<String, String> = HashMap::new();
    for row in rows {
        for route in &row.routes {
            by_route
                .entry(route_identity(route))
                .or_insert_with(|| row.id.clone());
        }
        by_id.insert(row.id.clone(), row);
    }
    let tx = db.transaction()?;
    for game in games {
        let existing_id = if by_id.contains_key(&game.id) {
            Some(game.id.clone())
        } else {
            game.routes
                .iter()
                .find_map(|route| by_route.get(&route_identity(route)).cloned())
        };
        let Some(existing) = existing_id.and_then(|id| by_id.get(&id).cloned()) else {
            let mut row = persist_game(&tx, &GamePatch::from(game))?;
            row.playtime_seconds =
                Some(game.playtime_seconds.unwrap_or(game.playtime_minutes * 60));
            row.playtime_minutes = game.playtime_minutes;
            for route in &game.routes {
                by_route.insert(route_identity(route), game.id.clone());
            }
            by_id.insert(game.id.clone(), row);
            continue;
        };
        let mut routes = existing.routes.clone();
        merge_routes_into(&mut routes, &game.routes);
        let imported = game.playtime_seconds.unwrap_or(game.playtime_minutes * 60);
        let seconds = existing.seconds().max(imported);
        let last = existing
            .last_played
            .unwrap_or(0)
            .max(game.last_played.unwrap_or(0));
        let pick = |a: &Option<String>, b: &Option<String>| {
            non_empty(a).or(non_empty(b)).map(str::to_owned)
        };
        let row = persist_game(
            &tx,
            &GamePatch {
                cover: pick(&existing.cover, &game.cover),
                default_cover: pick(&existing.default_cover, &game.default_cover),
                genre: pick(&existing.genre, &game.genre),
                playtime_seconds: Some(seconds),
                playtime_minutes: Some(seconds.div_euclid(60)),
                last_played: (last != 0).then_some(last),
                routes: Some(routes.clone()),
                ..GamePatch::new(existing.id.clone())
            },
        )?;
        for route in &routes {
            by_route.insert(route_identity(route), existing.id.clone());
        }
        by_id.insert(existing.id.clone(), row);
    }
    tx.commit()?;
    Ok(())
}

fn legacy_install_sources(source_id: &str) -> &'static [&'static str] {
    match source_id {
        "steam-installed" => &["steam"],
        "epic-installed" => &["epic"],
        "ea-registry" => &["ea-registry"],
        "battlenet-registry" => &["battlenet-registry"],
        "ubisoft-registry" => &["ubisoft-registry"],
        "amazon-registry" => &["amazon-registry"],
        "itch-installed" => &["itch-installed"],
        "xbox-appx" => &["xbox-appx"],
        "gog-galaxy" => &["gog-galaxy"],
        _ => &[],
    }
}

const LOCAL_ONLY_SOURCES: [&str; 6] = [
    "ea-registry",
    "battlenet-registry",
    "ubisoft-registry",
    "amazon-registry",
    "itch-installed",
    "xbox-appx",
];

/// Ids of games with a page customization (loaded once per reconcile).
fn customized_game_ids(db: &Connection) -> Result<HashSet<String>> {
    let mut stmt = db.prepare("SELECT gameId FROM game_page_custom")?;
    let ids = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Whether the user put anything into this game that a scan must not throw away. A game page
/// customization (`customized` holds those game ids) counts too.
fn has_user_data(row: &GameRow, customized: &HashSet<String>) -> bool {
    let route_stats = row
        .routes
        .iter()
        .any(|r| r.playtime_minutes.unwrap_or(0) > 0 || r.last_played.is_some());
    customized.contains(&row.id)
        || row.favorite
        || row.hidden
        || row.seconds() > 0
        || row.last_played.is_some()
        || route_stats
        || non_empty(&row.genre).is_some()
        || (non_empty(&row.cover).is_some() && row.cover != row.default_cover)
}

/// Applies an installed-games scan. An empty scan is ambiguous (client missing, drive
/// offline, unreadable database), so it never marks anything uninstalled.
pub fn reconcile_installed_source(
    db: &mut Connection,
    source_id: &str,
    scanned: &[Game],
) -> Result<()> {
    if scanned.is_empty() {
        return Ok(());
    }
    let active: HashSet<String> = scanned
        .iter()
        .flat_map(|g| g.routes.iter())
        .filter(|r| r.installed == Some(true))
        .map(route_identity)
        .collect();
    let rows = all_rows(db)?;
    let customized = customized_game_ids(db)?;
    let tx = db.transaction()?;
    for row in rows {
        let mut changed = false;
        let mut updated = Vec::with_capacity(row.routes.len());
        for route in &row.routes {
            let has_state = route
                .install_states
                .as_ref()
                .is_some_and(|s| s.contains_key(source_id));
            let inferred =
                !has_state && legacy_install_sources(source_id).contains(&route.source_id.as_str());
            let epic_installed_legacy = source_id == "epic-installed"
                && route.source_id == "epic"
                && non_empty(&route.launch_args).is_none()
                && route.launch_target.contains("action=launch");
            let epic_legendary_legacy = !has_state
                && source_id == "epic-legendary"
                && route.source_id == "epic"
                && non_empty(&route.launch_args).is_some();
            if !has_state && !inferred && !epic_installed_legacy && !epic_legendary_legacy {
                updated.push(route.clone());
                continue;
            }
            let mut next = route.clone();
            let mut states = route.install_states.clone().unwrap_or_default();
            states.insert(
                source_id.to_string(),
                active.contains(&route_identity(route)),
            );
            let installed = states.values().any(|v| *v);
            next.install_states = Some(states);
            next.installed = Some(installed);
            if !installed {
                next.install_path = None;
            }
            if (epic_installed_legacy || LOCAL_ONLY_SOURCES.contains(&source_id))
                && non_empty(&next.launch_args).is_none()
            {
                next.owned = None;
            }
            changed = true;
            if !installed && next.owned != Some(true) && !has_user_data(&row, &customized) {
                continue;
            }
            updated.push(next);
        }
        if !changed {
            continue;
        }
        if updated.is_empty() {
            delete_game_rows(&tx, &row.id)?;
        } else {
            persist_game(
                &tx,
                &GamePatch {
                    routes: Some(updated),
                    ..GamePatch::new(row.id.clone())
                },
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Applies an owned-games scan for one route source. Empty scans are ignored.
pub fn reconcile_owned_source(
    db: &mut Connection,
    route_source_id: &str,
    scanned: &[Game],
) -> Result<()> {
    if scanned.is_empty() {
        return Ok(());
    }
    let owned: HashSet<String> = scanned
        .iter()
        .flat_map(|g| g.routes.iter())
        .filter(|r| r.owned == Some(true))
        .map(route_identity)
        .collect();
    let rows = all_rows(db)?;
    let customized = customized_game_ids(db)?;
    let tx = db.transaction()?;
    for row in rows {
        let mut changed = false;
        let mut updated = Vec::with_capacity(row.routes.len());
        for route in &row.routes {
            if route.source_id != route_source_id
                || route.owned != Some(true)
                || owned.contains(&route_identity(route))
            {
                updated.push(route.clone());
                continue;
            }
            changed = true;
            if route.installed == Some(true) || has_user_data(&row, &customized) {
                updated.push(GameRoute {
                    owned: Some(false),
                    installed: Some(route.installed == Some(true)),
                    ..route.clone()
                });
            }
        }
        if !changed {
            continue;
        }
        if updated.is_empty() {
            delete_game_rows(&tx, &row.id)?;
        } else {
            persist_game(
                &tx,
                &GamePatch {
                    routes: Some(updated),
                    ..GamePatch::new(row.id.clone())
                },
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn add_tracked_playtime(
    db: &Connection,
    id: &str,
    seconds: i64,
    started_at: i64,
) -> Result<()> {
    let amount = seconds.max(0);
    if amount == 0 {
        return Ok(());
    }
    db.execute(
        "UPDATE games SET playtimeSeconds=playtimeSeconds+?1,playtimeMinutes=CAST((playtimeSeconds+?1)/60 AS INTEGER),lastPlayed=MAX(COALESCE(lastPlayed,0),?2) WHERE id=?3",
        params![amount, started_at, id],
    )?;
    Ok(())
}

pub fn mark_game_played(db: &Connection, id: &str, played_at: i64) -> Result<()> {
    db.execute(
        "UPDATE games SET lastPlayed=? WHERE id=?",
        params![played_at, id],
    )?;
    Ok(())
}

pub fn reset_playtime(db: &mut Connection) -> Result<Vec<Game>> {
    let games = all_games(db)?;
    let tx = db.transaction()?;
    tx.execute("DELETE FROM play_sessions", [])?;
    for game in games {
        let routes: Vec<GameRoute> = game
            .routes
            .into_iter()
            .map(|r| GameRoute {
                playtime_minutes: Some(0),
                last_played: None,
                ..r
            })
            .collect();
        tx.execute(
            "UPDATE games SET playtimeSeconds=0,playtimeMinutes=0,lastPlayed=NULL,routes=? WHERE id=?",
            params![serde_json::to_string(&routes)?, game.id],
        )?;
    }
    tx.commit()?;
    all_games(db)
}

/// Deletes a game together with its play sessions and cached art.
fn delete_game_rows(db: &Connection, id: &str) -> Result<()> {
    for sql in [
        "DELETE FROM play_sessions WHERE gameId=?",
        "DELETE FROM game_art WHERE gameId=?",
        "DELETE FROM game_page_custom WHERE gameId=?",
        "DELETE FROM games WHERE id=?",
    ] {
        db.execute(sql, [id])?;
    }
    Ok(())
}

pub fn remove_game(db: &Connection, id: &str) -> Result<Vec<Game>> {
    let tx = db.unchecked_transaction()?;
    delete_game_rows(&tx, id)?;
    tx.commit()?;
    all_games(db)
}

pub fn merge_games(db: &mut Connection, target_id: &str, source_id: &str) -> Result<Vec<Game>> {
    if target_id == source_id {
        bail!("Choose two different games to merge.");
    }
    let (Some(target), Some(source)) = (get_game(db, target_id)?, get_game(db, source_id)?) else {
        bail!("One of the selected games no longer exists.");
    };
    let mut routes = target.routes.clone();
    merge_routes_into(&mut routes, &source.routes);
    let custom = |g: &Game| non_empty(&g.cover).is_some() && g.cover != g.default_cover;
    let cover = if custom(&target) {
        target.cover.clone()
    } else if custom(&source) {
        source.cover.clone()
    } else {
        target.cover.clone().or_else(|| source.cover.clone())
    };
    let seconds = target
        .playtime_seconds
        .unwrap_or(target.playtime_minutes * 60)
        + source
            .playtime_seconds
            .unwrap_or(source.playtime_minutes * 60);
    let last = target
        .last_played
        .unwrap_or(0)
        .max(source.last_played.unwrap_or(0));
    let tx = db.transaction()?;
    persist_game(
        &tx,
        &GamePatch {
            cover,
            default_cover: target
                .default_cover
                .clone()
                .or(source.default_cover.clone()),
            genre: target.genre.clone().or(source.genre.clone()),
            playtime_seconds: Some(seconds),
            playtime_minutes: Some(seconds.div_euclid(60)),
            last_played: (last != 0).then_some(last),
            favorite: Some(target.favorite || source.favorite),
            hidden: Some(target.hidden || source.hidden),
            routes: Some(routes),
            ..GamePatch::from(&target)
        },
    )?;
    tx.execute(
        "UPDATE play_sessions SET gameId=?1 WHERE gameId=?2",
        params![target_id, source_id],
    )?;
    // The merged game's routes changed, so its art lookup may resolve differently: refetch.
    tx.execute("DELETE FROM game_art WHERE gameId=?", [target_id])?;
    // Page customization: the target's wins; otherwise the source's moves over.
    tx.execute(
        "UPDATE OR IGNORE game_page_custom SET gameId=?1 WHERE gameId=?2",
        params![target_id, source_id],
    )?;
    delete_game_rows(&tx, source_id)?;
    tx.commit()?;
    all_games(db)
}

// ---------------------------------------------------------------------------
// Play sessions

pub const TWO_WEEKS_MS: i64 = 14 * 24 * 3_600_000;

/// Stores a new session; `None` if the game no longer exists.
pub fn insert_session(
    db: &Connection,
    game_id: &str,
    started_at: i64,
    seconds: i64,
    source: &str,
) -> Result<Option<i64>> {
    let inserted = db.execute(
        "INSERT INTO play_sessions(gameId,startedAt,seconds,source) SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM games WHERE id=?1)",
        params![game_id, started_at, seconds.max(0), source],
    )?;
    Ok((inserted == 1).then(|| db.last_insert_rowid()))
}

/// Updates an in-progress session's length (never shortens it).
pub fn update_session(db: &Connection, id: i64, seconds: i64) -> Result<()> {
    db.execute(
        "UPDATE play_sessions SET seconds=MAX(seconds,?2) WHERE id=?1",
        params![id, seconds],
    )?;
    Ok(())
}

pub fn close_session(db: &Connection, id: i64, ended_at: i64, seconds: i64) -> Result<()> {
    db.execute(
        "UPDATE play_sessions SET seconds=MAX(seconds,?2),endedAt=?3 WHERE id=?1",
        params![id, seconds, ended_at],
    )?;
    Ok(())
}

/// A game's sessions, newest first.
pub fn play_sessions(db: &Connection, game_id: &str, limit: i64) -> Result<Vec<PlaySession>> {
    let mut stmt = db.prepare(
        "SELECT id,startedAt,endedAt,seconds,source FROM play_sessions WHERE gameId=? ORDER BY startedAt DESC,id DESC LIMIT ?",
    )?;
    let rows = stmt
        .query_map(params![game_id, limit], |row| {
            Ok(PlaySession {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                seconds: row.get(3)?,
                source: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Seconds of one session that fall after `cutoff`. A session that straddles the cutoff counts
/// in proportion to the part of its wall-clock span after it.
pub fn seconds_after(started_at: i64, ended_at: Option<i64>, seconds: i64, cutoff: i64) -> i64 {
    let seconds = seconds.max(0);
    if started_at >= cutoff {
        return seconds;
    }
    let ended = ended_at
        .unwrap_or(started_at + seconds * 1000)
        .max(started_at);
    if ended <= cutoff {
        return 0;
    }
    let span = (ended - started_at) as i128;
    (seconds as i128 * (ended - cutoff) as i128 / span) as i64
}

/// Totals for the game page. `total_seconds` is the library playtime (it includes playtime
/// imported from stores, so it can be more than the sum of the sessions).
pub fn play_stats(db: &Connection, game_id: &str, now: i64) -> Result<PlayStats> {
    let mut stmt =
        db.prepare("SELECT startedAt,endedAt,seconds FROM play_sessions WHERE gameId=?")?;
    let sessions = stmt
        .query_map([game_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let cutoff = now - TWO_WEEKS_MS;
    let game = get_row(db, game_id)?;
    let session_total: i64 = sessions.iter().map(|s| s.2.max(0)).sum();
    let last_session = sessions.iter().map(|s| s.0).max();
    let last_played = match (game.as_ref().and_then(|g| g.last_played), last_session) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    Ok(PlayStats {
        total_seconds: game.as_ref().map_or(session_total, GameRow::seconds),
        last_two_weeks_seconds: sessions
            .iter()
            .map(|&(start, end, secs)| seconds_after(start, end, secs, cutoff))
            .sum(),
        session_count: sessions.len() as i64,
        first_played: sessions.iter().map(|s| s.0).min(),
        last_played: last_played.filter(|v| *v != 0),
    })
}

// ---------------------------------------------------------------------------
// Game page art cache

/// SteamGridDB hero/logo URLs cached per game. `prefs` records the art preferences they were
/// fetched with, so changing those preferences invalidates the entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CachedArt {
    pub hero: Vec<String>,
    pub logo: Vec<String>,
    pub prefs: String,
    pub fetched_at: i64,
}

pub fn cached_art(db: &Connection, game_id: &str) -> Result<Option<CachedArt>> {
    let row = db
        .query_row(
            "SELECT hero,logo,prefs,fetchedAt FROM game_art WHERE gameId=?",
            [game_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(hero, logo, prefs, fetched_at)| CachedArt {
        hero: serde_json::from_str(&hero).unwrap_or_default(),
        logo: serde_json::from_str(&logo).unwrap_or_default(),
        prefs,
        fetched_at,
    }))
}

/// Caches art for a game that still exists.
pub fn put_cached_art(db: &Connection, game_id: &str, art: &CachedArt) -> Result<()> {
    db.execute(
        "INSERT INTO game_art(gameId,hero,logo,prefs,fetchedAt) SELECT ?1,?2,?3,?4,?5 WHERE EXISTS(SELECT 1 FROM games WHERE id=?1)
         ON CONFLICT(gameId) DO UPDATE SET hero=excluded.hero,logo=excluded.logo,prefs=excluded.prefs,fetchedAt=excluded.fetchedAt",
        params![
            game_id,
            serde_json::to_string(&art.hero)?,
            serde_json::to_string(&art.logo)?,
            art.prefs,
            art.fetched_at
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Game page customization

fn read_page_custom(row: &Row) -> rusqlite::Result<GamePageCustom> {
    let art = |name: &str| -> rusqlite::Result<Option<String>> {
        Ok(col_text(row, name)?.filter(|s| !s.is_empty()))
    };
    // Values are validated on the way in; anything odd in the file falls back to defaults.
    let position = col_text(row, "logoPosition")?
        .and_then(|p| LogoPosition::parse(&p))
        .unwrap_or_default();
    let scale = match row.get_ref("logoScale")? {
        ValueRef::Real(f) => Some(f),
        ValueRef::Integer(i) => Some(i as f64),
        _ => None,
    }
    .filter(|f| valid_logo_scale(*f))
    .unwrap_or(DEFAULT_LOGO_SCALE);
    Ok(GamePageCustom {
        hero: art("hero")?,
        logo: art("logo")?,
        logo_position: position,
        logo_scale: scale,
    })
}

/// A game's page customization; defaults when it has none (or doesn't exist).
pub fn page_custom(db: &Connection, game_id: &str) -> Result<GamePageCustom> {
    Ok(db
        .query_row(
            "SELECT * FROM game_page_custom WHERE gameId=?",
            [game_id],
            read_page_custom,
        )
        .optional()?
        .unwrap_or_default())
}

/// Stores a game's customization (an all-default value removes the row). Values must already
/// be validated. Returns false, storing nothing, when the game doesn't exist.
pub fn put_page_custom(db: &Connection, game_id: &str, custom: &GamePageCustom) -> Result<bool> {
    if get_row(db, game_id)?.is_none() {
        return Ok(false);
    }
    if custom.is_default() {
        db.execute("DELETE FROM game_page_custom WHERE gameId=?", [game_id])?;
        return Ok(true);
    }
    db.execute(
        "INSERT INTO game_page_custom(gameId,hero,logo,logoPosition,logoScale,updatedAt) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(gameId) DO UPDATE SET hero=excluded.hero,logo=excluded.logo,logoPosition=excluded.logoPosition,logoScale=excluded.logoScale,updatedAt=excluded.updatedAt",
        params![
            game_id,
            custom.hero,
            custom.logo,
            custom.logo_position.as_str(),
            custom.logo_scale,
            now_ms()
        ],
    )?;
    Ok(true)
}

/// Every stored customization (backups), by game id.
pub fn all_page_custom(db: &Connection) -> Result<Vec<GamePageCustomEntry>> {
    let mut stmt = db.prepare("SELECT * FROM game_page_custom ORDER BY gameId")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(GamePageCustomEntry::new(
                &col_text(row, "gameId")?.unwrap_or_default(),
                &read_page_custom(row)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Settings, connections, secrets, emulators, plugins

fn settings_object(db: &Connection) -> Result<Map<String, Value>> {
    let mut merged = match serde_json::to_value(Settings::default())? {
        Value::Object(map) => map,
        _ => unreachable!(),
    };
    let stored: Option<String> = db
        .query_row("SELECT value FROM settings WHERE key='app'", [], |row| {
            row.get(0)
        })
        .optional()?;
    if let Some(Value::Object(stored)) = stored.and_then(|s| serde_json::from_str(&s).ok()) {
        merged.extend(stored);
    }
    Ok(merged)
}

pub fn get_settings(db: &Connection) -> Result<Settings> {
    serde_json::from_value(Value::Object(settings_object(db)?)).or_else(|_| Ok(Settings::default()))
}

/// Applies a JSON patch to the stored settings. A `null` value removes that key.
pub fn put_settings(db: &Connection, patch: Map<String, Value>) -> Result<Settings> {
    let mut next = settings_object(db)?;
    for (key, value) in patch {
        if value.is_null() {
            next.remove(&key);
        } else {
            next.insert(key, value);
        }
    }
    let settings: Settings = serde_json::from_value(Value::Object(next))
        .map_err(|e| anyhow!("Settings could not be saved: {e}"))?;
    db.execute(
        "INSERT INTO settings(key,value) VALUES('app',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(&settings)?],
    )?;
    Ok(settings)
}

pub fn get_connection_values(db: &Connection) -> Result<ConnectionValues> {
    let stored: Option<String> = db
        .query_row(
            "SELECT value FROM library_connections WHERE key='main'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

pub fn put_connection_values(
    db: &Connection,
    input: &ConnectionValues,
) -> Result<ConnectionValues> {
    let clean = ConnectionValues {
        steam_id: input.steam_id.trim().into(),
        legendary_path: input.legendary_path.trim().into(),
        gog_database_path: input.gog_database_path.trim().into(),
    };
    db.execute(
        "INSERT INTO library_connections(key,value) VALUES('main',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(&clean)?],
    )?;
    Ok(clean)
}

pub fn read_secret(db: &Connection, key: &str) -> Result<Option<Vec<u8>>> {
    Ok(db
        .query_row("SELECT value FROM secrets WHERE key=?", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

pub fn write_secret(db: &Connection, key: &str, value: &[u8]) -> Result<()> {
    db.execute(
        "INSERT INTO secrets(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn delete_secret(db: &Connection, key: &str) -> Result<()> {
    db.execute("DELETE FROM secrets WHERE key=?", [key])?;
    Ok(())
}

pub fn emulator_profiles(db: &Connection) -> Result<Vec<EmulatorProfile>> {
    let mut stmt = db.prepare(
        "SELECT id,name,executable,args,romFolders,extensions FROM emulators ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, executable, args, folders, extensions)| EmulatorProfile {
                id,
                name,
                executable,
                args,
                rom_folders: serde_json::from_str(&folders).unwrap_or_default(),
                extensions: serde_json::from_str(&extensions).unwrap_or_default(),
            },
        )
        .collect())
}

pub fn save_emulator(db: &Connection, p: &EmulatorProfile) -> Result<Vec<EmulatorProfile>> {
    db.execute(
        "INSERT INTO emulators(id,name,executable,args,romFolders,extensions) VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,executable=excluded.executable,args=excluded.args,romFolders=excluded.romFolders,extensions=excluded.extensions",
        params![
            p.id,
            p.name,
            p.executable,
            p.args,
            serde_json::to_string(&p.rom_folders)?,
            serde_json::to_string(&p.extensions)?
        ],
    )?;
    emulator_profiles(db)
}

pub fn plugins(db: &Connection) -> Result<Vec<Value>> {
    let mut stmt = db.prepare("SELECT manifest FROM plugins")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect())
}

pub fn save_plugin(db: &Connection, manifest: &PluginManifest) -> Result<Vec<Value>> {
    db.execute(
        "INSERT OR REPLACE INTO plugins(id,manifest) VALUES(?,?)",
        params![manifest.id, serde_json::to_string(manifest)?],
    )?;
    plugins(db)
}

pub fn restore_backup(db: &mut Connection, backup: &Backup) -> Result<usize> {
    let tx = db.transaction()?;
    for game in &backup.games {
        persist_game(&tx, &GamePatch::from(game))?;
    }
    if let Some(settings) = &backup.settings {
        let Value::Object(patch) = serde_json::to_value(settings)? else {
            unreachable!()
        };
        put_settings(&tx, patch)?;
    }
    for emulator in backup.emulators.iter().flatten() {
        save_emulator(&tx, emulator)?;
    }
    // Entries for games that aren't in the library are skipped.
    for entry in backup.game_page_custom.iter().flatten() {
        put_page_custom(&tx, &entry.game_id, &entry.custom())?;
    }
    tx.commit()?;
    Ok(backup.games.len())
}

/// The Electron build kept its data in `%APPDATA%\game-lobby`. On first launch, copy the
/// library and custom covers into the Tauri data directory so nothing is lost.
pub fn migrate_from_electron(data_dir: &Path) -> Result<bool> {
    let target = data_dir.join("library.sqlite");
    if target.exists() {
        return Ok(false);
    }
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Ok(false);
    };
    let old_dir = appdata.join("game-lobby");
    let old = old_dir.join("library.sqlite");
    if !old.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(data_dir)?;
    for suffix in ["", "-wal", "-shm"] {
        let from = old_dir.join(format!("library.sqlite{suffix}"));
        if from.exists() {
            std::fs::copy(&from, data_dir.join(format!("library.sqlite{suffix}")))?;
        }
    }
    let old_covers = old_dir.join("covers");
    if old_covers.is_dir() {
        let covers = data_dir.join("covers");
        std::fs::create_dir_all(&covers)?;
        for entry in std::fs::read_dir(old_covers)?.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                std::fs::copy(entry.path(), covers.join(entry.file_name()))?;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(source: &str, app: &str, installed: bool) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            store: "Steam".into(),
            app_id: Some(app.into()),
            launch_target: format!("steam://run/{app}"),
            installed: Some(installed),
            install_states: Some([("steam-installed".to_string(), installed)].into()),
            ..Default::default()
        }
    }

    fn game(id: &str, title: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: title.into(),
            store: "Steam".into(),
            routes,
            ..Default::default()
        }
    }

    #[test]
    fn imports_merge_by_route_identity_and_keep_user_data() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        upsert_imported_games(
            &mut conn,
            &[game(
                "steam:570",
                "Dota 2",
                vec![route("steam", "570", true)],
            )],
        )
        .unwrap();
        save_game(
            &conn,
            &GamePatch {
                favorite: Some(true),
                ..GamePatch::new("steam:570")
            },
        )
        .unwrap();
        // Same route identity under a different id merges into the existing game.
        let mut owned = route("steam", "570", false);
        owned.owned = Some(true);
        owned.install_states = None;
        owned.playtime_minutes = Some(90);
        upsert_imported_games(
            &mut conn,
            &[Game {
                playtime_minutes: 90,
                ..game("other", "Dota 2", vec![owned])
            }],
        )
        .unwrap();
        let games = all_games(&conn).unwrap();
        assert_eq!(games.len(), 1);
        let g = &games[0];
        assert!(g.favorite);
        assert_eq!(g.playtime_minutes, 90);
        assert_eq!(g.routes.len(), 1);
        assert_eq!(g.routes[0].owned, Some(true));
        assert_eq!(
            g.routes[0].installed,
            Some(true),
            "install state kept from the installed scan"
        );
    }

    #[test]
    fn uninstalled_games_without_user_data_are_removed_but_empty_scans_are_ignored() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        let a = game("steam:1", "A", vec![route("steam", "1", true)]);
        let b = game("steam:2", "B", vec![route("steam", "2", true)]);
        upsert_imported_games(&mut conn, &[a.clone(), b]).unwrap();
        reconcile_installed_source(&mut conn, "steam-installed", &[]).unwrap();
        assert_eq!(all_games(&conn).unwrap().len(), 2);
        reconcile_installed_source(&mut conn, "steam-installed", &[a]).unwrap();
        let games = all_games(&conn).unwrap();
        assert_eq!(
            games.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
            ["steam:1"]
        );
    }

    #[test]
    fn merge_sums_playtime_and_moves_routes() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        save_game(
            &conn,
            &GamePatch::from(&Game {
                playtime_minutes: 10,
                ..game("a", "A", vec![route("steam", "1", true)])
            }),
        )
        .unwrap();
        save_game(
            &conn,
            &GamePatch::from(&Game {
                playtime_minutes: 5,
                ..game("b", "A", vec![route("gog-galaxy", "9", false)])
            }),
        )
        .unwrap();
        let games = merge_games(&mut conn, "a", "b").unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].playtime_minutes, 15);
        assert_eq!(games[0].routes.len(), 2);
        assert!(merge_games(&mut conn, "a", "a").is_err());
    }

    #[test]
    fn settings_patch_and_reset() {
        let db = Db::memory().unwrap();
        let conn = db.lock();
        let mut patch = Map::new();
        patch.insert("posterWidth".into(), 200.into());
        patch.insert("themeUrl".into(), "https://x.example/t.json".into());
        let s = put_settings(&conn, patch).unwrap();
        assert_eq!(s.poster_width, 200);
        let mut reset = Map::new();
        reset.insert("themeUrl".into(), Value::Null);
        let s = put_settings(&conn, reset).unwrap();
        assert_eq!(s.theme_url, None);
        assert_eq!(get_settings(&conn).unwrap().poster_width, 200);
    }

    #[test]
    fn older_settings_default_cover_auto_fetch_on() {
        let stored: Settings = serde_json::from_value(serde_json::json!({
            "scanIntervalHours": 0, "posterWidth": 172, "showBadges": true,
            "animations": true, "tracking": true, "theme": serde_json::to_value(ThemeTokens::default()).unwrap()
        }))
        .unwrap();
        assert!(stored.steam_grid_db_auto_fetch);
        assert!(stored.show_only_installed);
        assert!(stored.steam_grid_db_styles.is_empty());
        assert!(!stored.steam_grid_db_animated);
        assert!(!stored.steam_grid_db_nsfw);
        assert!(!stored.steam_grid_db_humor);
        assert_eq!(stored, Settings::default());
        let db = Db::memory().unwrap();
        let conn = db.lock();
        let mut patch = Map::new();
        patch.insert("steamGridDbAutoFetch".into(), false.into());
        assert!(!put_settings(&conn, patch).unwrap().steam_grid_db_auto_fetch);
        assert!(!get_settings(&conn).unwrap().steam_grid_db_auto_fetch);

        let mut patch = Map::new();
        patch.insert("showOnlyInstalled".into(), false.into());
        patch.insert("steamGridDbStyles".into(), serde_json::json!(["material"]));
        patch.insert("steamGridDbAnimated".into(), true.into());
        let s = put_settings(&conn, patch).unwrap();
        assert!(!s.show_only_installed && s.steam_grid_db_animated);
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.steam_grid_db_styles, ["material"]);
        assert!(!s.steam_grid_db_nsfw && !s.steam_grid_db_humor);
    }

    #[test]
    fn secrets_can_be_written_and_deleted() {
        let db = Db::memory().unwrap();
        let conn = db.lock();
        write_secret(&conn, "k", b"sealed").unwrap();
        assert_eq!(
            read_secret(&conn, "k").unwrap().as_deref(),
            Some(&b"sealed"[..])
        );
        delete_secret(&conn, "k").unwrap();
        assert_eq!(read_secret(&conn, "k").unwrap(), None);
        delete_secret(&conn, "missing").unwrap();
    }

    #[test]
    fn existing_databases_gain_session_tables_and_open_sessions_are_closed() {
        let conn = Connection::open_in_memory().unwrap();
        // A library from before sessions existed.
        conn.execute_batch(&format!(
            "{GAMES_TABLE};
             CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO games(id,title,store,playtimeSeconds) VALUES('a','A','Steam',120);"
        ))
        .unwrap();
        migrate(&conn, None).unwrap();
        let tables: HashSet<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for name in ["play_sessions", "play_sessions_game", "game_art"] {
            assert!(tables.contains(name), "{name} missing");
        }
        assert_eq!(get_game(&conn, "a").unwrap().unwrap().playtime_minutes, 2);
        // A session left open by a crash keeps its saved length and is closed on next start.
        let id = insert_session(&conn, "a", 1_000, 42, "detected")
            .unwrap()
            .unwrap();
        assert_eq!(play_sessions(&conn, "a", 10).unwrap()[0].ended_at, None);
        migrate(&conn, None).unwrap();
        let s = &play_sessions(&conn, "a", 10).unwrap()[0];
        assert_eq!((s.id, s.seconds, s.ended_at), (id, 42, Some(43_000)));
        // Sessions are only stored for games in the library.
        assert_eq!(
            insert_session(&conn, "ghost", 1, 10, "launch").unwrap(),
            None
        );
    }

    fn art(url: &str) -> CachedArt {
        CachedArt {
            hero: vec![format!("https://cdn2.steamgriddb.com/hero/{url}.png")],
            logo: vec![],
            prefs: "p".into(),
            fetched_at: 5,
        }
    }

    #[test]
    fn sessions_and_art_follow_remove_merge_and_reset() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        for id in ["a", "b", "c"] {
            save_game(&conn, &GamePatch::from(&game(id, id, vec![]))).unwrap();
            insert_session(&conn, id, 1_000, 60, "launch").unwrap();
            put_cached_art(&conn, id, &art(id)).unwrap();
        }
        put_cached_art(&conn, "ghost", &art("ghost")).unwrap();
        assert_eq!(cached_art(&conn, "ghost").unwrap(), None);
        assert_eq!(cached_art(&conn, "a").unwrap(), Some(art("a")));

        remove_game(&conn, "c").unwrap();
        assert!(play_sessions(&conn, "c", 10).unwrap().is_empty());
        assert_eq!(cached_art(&conn, "c").unwrap(), None);

        merge_games(&mut conn, "a", "b").unwrap();
        assert_eq!(play_sessions(&conn, "a", 10).unwrap().len(), 2);
        assert!(play_sessions(&conn, "b", 10).unwrap().is_empty());
        assert_eq!(cached_art(&conn, "b").unwrap(), None);
        assert_eq!(
            cached_art(&conn, "a").unwrap(),
            None,
            "refetched after a merge"
        );

        reset_playtime(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM play_sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn sessions_list_newest_first_and_stats_cover_two_weeks() {
        let db = Db::memory().unwrap();
        let conn = db.lock();
        let now = 100 * TWO_WEEKS_MS;
        let cutoff = now - TWO_WEEKS_MS;
        save_game(
            &conn,
            &GamePatch {
                playtime_seconds: Some(10_000),
                ..GamePatch::from(&game("a", "A", vec![]))
            },
        )
        .unwrap();
        let old = insert_session(&conn, "a", cutoff - 50_000_000, 600, "launch")
            .unwrap()
            .unwrap();
        close_session(&conn, old, cutoff - 49_400_000, 600).unwrap();
        // Straddles the cutoff: 100 s span, 60 s of it inside the window.
        let straddling = insert_session(&conn, "a", cutoff - 40_000, 100, "detected")
            .unwrap()
            .unwrap();
        close_session(&conn, straddling, cutoff + 60_000, 100).unwrap();
        let recent = insert_session(&conn, "a", now - 3_600_000, 1_200, "detected")
            .unwrap()
            .unwrap();
        update_session(&conn, recent, 1_500).unwrap();
        update_session(&conn, recent, 900).unwrap(); // never shortens

        let list = play_sessions(&conn, "a", 10).unwrap();
        assert_eq!(
            list.iter().map(|s| s.id).collect::<Vec<_>>(),
            [recent, straddling, old]
        );
        assert_eq!((list[0].seconds, list[0].ended_at), (1_500, None));
        assert_eq!(play_sessions(&conn, "a", 1).unwrap().len(), 1);

        let stats = play_stats(&conn, "a", now).unwrap();
        assert_eq!(
            stats,
            PlayStats {
                total_seconds: 10_000,
                last_two_weeks_seconds: 1_500 + 60,
                session_count: 3,
                first_played: Some(cutoff - 50_000_000),
                last_played: Some(now - 3_600_000),
            }
        );
        let json = serde_json::to_value(&stats).unwrap();
        assert!(json.get("lastTwoWeeksSeconds").is_some());
        assert_eq!(
            play_stats(&conn, "none", now).unwrap(),
            PlayStats::default()
        );

        assert_eq!(seconds_after(10, None, 5, 0), 5);
        assert_eq!(seconds_after(0, Some(1_000), 1, 1_000), 0);
        assert_eq!(seconds_after(0, None, 10, 5_000), 5);
    }

    #[test]
    fn existing_databases_gain_the_page_custom_table() {
        let conn = Connection::open_in_memory().unwrap();
        // A library from before game page customization.
        conn.execute_batch(&format!(
            "{GAMES_TABLE};
             CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE game_art(gameId TEXT PRIMARY KEY,hero TEXT NOT NULL DEFAULT '[]',logo TEXT NOT NULL DEFAULT '[]',prefs TEXT NOT NULL DEFAULT '',fetchedAt INTEGER NOT NULL);
             INSERT INTO games(id,title,store) VALUES('a','A','Steam');
             INSERT INTO game_art(gameId,hero,fetchedAt) VALUES('a','[\"https://cdn2.steamgriddb.com/h.png\"]',5);"
        ))
        .unwrap();
        migrate(&conn, None).unwrap();
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('game_page_custom')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "gameId",
                "hero",
                "logo",
                "logoPosition",
                "logoScale",
                "updatedAt"
            ]
        );
        assert_eq!(cached_art(&conn, "a").unwrap().unwrap().hero.len(), 1);
        assert_eq!(page_custom(&conn, "a").unwrap(), GamePageCustom::default());
        let custom = GamePageCustom {
            hero: Some("https://cdn2.steamgriddb.com/hero/a.png".into()),
            logo_scale: 1.25,
            ..Default::default()
        };
        assert!(put_page_custom(&conn, "a", &custom).unwrap());
        // Migrating again keeps the data.
        migrate(&conn, None).unwrap();
        assert_eq!(page_custom(&conn, "a").unwrap(), custom);
        // Values damaged outside the app fall back to defaults instead of failing.
        conn.execute(
            "UPDATE game_page_custom SET logoPosition='sideways',logoScale=99,logo='' WHERE gameId='a'",
            [],
        )
        .unwrap();
        let read = page_custom(&conn, "a").unwrap();
        assert_eq!(
            (read.logo, read.logo_position, read.logo_scale),
            (None, LogoPosition::BottomLeft, 1.0)
        );
        assert!(!put_page_custom(&conn, "ghost", &custom).unwrap());
        assert_eq!(
            page_custom(&conn, "ghost").unwrap(),
            GamePageCustom::default()
        );
    }

    fn custom_hero(n: &str) -> GamePageCustom {
        GamePageCustom {
            hero: Some(format!("https://cdn2.steamgriddb.com/hero/{n}.png")),
            ..Default::default()
        }
    }

    #[test]
    fn page_custom_follows_remove_and_merge() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        for id in ["a", "b", "c", "d", "e"] {
            save_game(&conn, &GamePatch::from(&game(id, id, vec![]))).unwrap();
        }
        for id in ["a", "b", "d", "e"] {
            put_page_custom(&conn, id, &custom_hero(id)).unwrap();
        }
        remove_game(&conn, "e").unwrap();
        assert_eq!(page_custom(&conn, "e").unwrap(), GamePageCustom::default());

        // Both customized: the target's row is kept, the source's is dropped.
        merge_games(&mut conn, "a", "b").unwrap();
        assert_eq!(page_custom(&conn, "a").unwrap(), custom_hero("a"));
        // Only the source customized: it moves to the target.
        merge_games(&mut conn, "c", "d").unwrap();
        assert_eq!(page_custom(&conn, "c").unwrap(), custom_hero("d"));
        let ids: Vec<String> = all_page_custom(&conn)
            .unwrap()
            .into_iter()
            .map(|e| e.game_id)
            .collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn page_custom_round_trips_through_backups_and_old_backups_still_restore() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        save_game(&conn, &GamePatch::from(&game("a", "A", vec![]))).unwrap();
        save_game(&conn, &GamePatch::from(&game("b", "B", vec![]))).unwrap();
        let custom = GamePageCustom {
            hero: Some("lobby-cover://hero-a-1.png".into()),
            logo: Some("https://cdn2.steamgriddb.com/logo/a.png".into()),
            logo_position: LogoPosition::BottomCenter,
            logo_scale: 0.75,
        };
        put_page_custom(&conn, "a", &custom).unwrap();
        let backup = Backup {
            games: all_games(&conn).unwrap(),
            settings: None,
            emulators: None,
            game_page_custom: Some(all_page_custom(&conn).unwrap()),
        };
        let text = serde_json::to_string(&backup).unwrap();
        let raw: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            raw["gamePageCustom"],
            serde_json::json!([{ "gameId": "a", "hero": "lobby-cover://hero-a-1.png",
                "logo": "https://cdn2.steamgriddb.com/logo/a.png",
                "logoPosition": "bottom-center", "logoScale": 0.75 }])
        );
        let parsed = crate::validation::backup(&raw).unwrap();

        let fresh = Db::memory().unwrap();
        let mut fresh = fresh.lock();
        save_game(&fresh, &GamePatch::from(&game("b", "B", vec![]))).unwrap();
        put_page_custom(&fresh, "b", &custom_hero("b")).unwrap();
        assert_eq!(restore_backup(&mut fresh, &parsed).unwrap(), 2);
        assert_eq!(page_custom(&fresh, "a").unwrap(), custom);
        assert_eq!(
            page_custom(&fresh, "b").unwrap(),
            custom_hero("b"),
            "games without an entry keep theirs"
        );

        // A backup from before this feature: no field, nothing changes, nothing fails.
        let mut old = raw.clone();
        old.as_object_mut().unwrap().remove("gamePageCustom");
        let parsed = crate::validation::backup(&old).unwrap();
        assert!(parsed.game_page_custom.is_none());
        assert_eq!(restore_backup(&mut conn, &parsed).unwrap(), 2);
        assert_eq!(page_custom(&conn, "a").unwrap(), custom);
        let without = serde_json::to_value(Backup {
            game_page_custom: None,
            ..parsed
        })
        .unwrap();
        assert!(without.get("gamePageCustom").is_none());

        // Entries for games missing from the library are skipped.
        let mut ghost = raw;
        ghost["gamePageCustom"][0]["gameId"] = "ghost".into();
        restore_backup(&mut conn, &crate::validation::backup(&ghost).unwrap()).unwrap();
        assert_eq!(
            page_custom(&conn, "ghost").unwrap(),
            GamePageCustom::default()
        );
    }

    #[test]
    fn scans_keep_uninstalled_games_whose_page_was_customized() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        let kept = game("steam:1", "Kept", vec![route("steam", "1", true)]);
        let plain = game("steam:2", "Plain", vec![route("steam", "2", true)]);
        let other = game("steam:3", "Other", vec![route("steam", "3", true)]);
        upsert_imported_games(&mut conn, &[kept, plain, other.clone()]).unwrap();
        put_page_custom(&conn, "steam:1", &custom_hero("1")).unwrap();
        // Only steam:3 is still installed.
        reconcile_installed_source(&mut conn, "steam-installed", &[other]).unwrap();
        let ids: Vec<String> = all_games(&conn)
            .unwrap()
            .into_iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(ids, ["steam:1", "steam:3"]);
        let g = get_game(&conn, "steam:1").unwrap().unwrap();
        assert_eq!(
            g.routes[0].installed,
            Some(false),
            "kept, marked not installed"
        );
        assert_eq!(page_custom(&conn, "steam:1").unwrap(), custom_hero("1"));

        // Owned scans: a customized game that is no longer owned (and not installed) is kept.
        let mut owned = route("steam", "4", false);
        owned.owned = Some(true);
        owned.install_states = None;
        let mut owned2 = route("steam", "5", false);
        owned2.owned = Some(true);
        owned2.install_states = None;
        let mut still = route("steam", "6", false);
        still.owned = Some(true);
        still.install_states = None;
        let still_game = game("steam:6", "Still", vec![still]);
        upsert_imported_games(
            &mut conn,
            &[
                game("steam:4", "Custom", vec![owned]),
                game("steam:5", "Gone", vec![owned2]),
                still_game.clone(),
            ],
        )
        .unwrap();
        put_page_custom(
            &conn,
            "steam:4",
            &GamePageCustom {
                logo_position: LogoPosition::Center,
                ..Default::default()
            },
        )
        .unwrap();
        reconcile_owned_source(&mut conn, "steam", &[still_game]).unwrap();
        assert!(get_game(&conn, "steam:5").unwrap().is_none());
        let g = get_game(&conn, "steam:4").unwrap().unwrap();
        assert_eq!(
            (g.routes[0].owned, g.routes[0].installed),
            (Some(false), Some(false))
        );
    }
}
