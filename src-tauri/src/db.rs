//! SQLite library storage. Ported from the Electron `src/main/db/index.ts`; the schema is
//! unchanged so existing `library.sqlite` files open as-is.

use crate::types::*;
#[cfg(test)]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};
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
    #[cfg(test)]
    pub fn open(file: &Path) -> Result<Self> {
        Ok(Self::open_checked(file, None)?.0)
    }

    /// Opens the library after a `PRAGMA quick_check`. A damaged library is repaired into a
    /// new file and swapped in before the connection is kept ([`crate::dbrepair::open_or_repair`]);
    /// the notice says what happened, for the user.
    pub fn open_checked(
        file: &Path,
        screenshots_dir: Option<&Path>,
    ) -> Result<(Self, Option<crate::dbrepair::RepairNotice>)> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let (conn, notice) = crate::dbrepair::open_or_repair(file, screenshots_dir)?;
        migrate(&conn, Some(file))?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            notice,
        ))
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

/// Creates the current schema in an empty database (the repair tool's target).
pub(crate) fn create_schema(db: &Connection) -> Result<()> {
    migrate(db, None)
}

/// A copy of the library made before a migration changes it, with the online backup API.
fn migration_backup(db: &Connection, file: &Path, kind: &str) -> Result<()> {
    let backup = file.with_file_name(format!(
        "library.sqlite.pre-{kind}-migration-{}.bak",
        now_ms()
    ));
    crate::dbrepair::backup_to(db, &backup)
        .context("Could not back up the library before its database migration.")
}

fn migrate(db: &Connection, file: Option<&Path>) -> Result<()> {
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    // Deleted rows (such as removed secrets) are overwritten, not left in free pages.
    db.pragma_update(None, "secure_delete", "ON")?;
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
        CREATE TABLE IF NOT EXISTS game_page_custom(gameId TEXT PRIMARY KEY,hero TEXT,logo TEXT,logoPosition TEXT NOT NULL DEFAULT 'bottom-left',logoScale REAL NOT NULL DEFAULT 1,updatedAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS library_roots(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,addedAt INTEGER NOT NULL,lastScanAt INTEGER,lastResult TEXT);
        CREATE TABLE IF NOT EXISTS scan_review(id TEXT PRIMARY KEY,kind TEXT NOT NULL,key TEXT NOT NULL UNIQUE,rootId TEXT,details TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'open',resolution TEXT,createdAt INTEGER NOT NULL,updatedAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS scan_ignore(kind TEXT NOT NULL,key TEXT NOT NULL,path TEXT NOT NULL,addedAt INTEGER NOT NULL,PRIMARY KEY(kind,key));
        CREATE TABLE IF NOT EXISTS local_exe_choice(folderKey TEXT PRIMARY KEY,executable TEXT NOT NULL,chosenAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS local_shortcut_ok(shortcutKey TEXT PRIMARY KEY,target TEXT NOT NULL,arguments TEXT NOT NULL,confirmedAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS cover_cache(url TEXT PRIMARY KEY,file TEXT NOT NULL,fetchedAt INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS cover_cache_file ON cover_cache(file);
        CREATE TABLE IF NOT EXISTS game_metadata(gameId TEXT PRIMARY KEY,igdbId INTEGER,json TEXT,fetchedAt INTEGER NOT NULL,source TEXT NOT NULL DEFAULT 'auto');
        CREATE TABLE IF NOT EXISTS game_screenshots(id TEXT PRIMARY KEY,gameId TEXT NOT NULL,file TEXT NOT NULL,hash TEXT NOT NULL,width INTEGER NOT NULL,height INTEGER NOT NULL,addedAt INTEGER NOT NULL,source TEXT NOT NULL DEFAULT 'user',originalPath TEXT,UNIQUE(gameId,hash));
        CREATE INDEX IF NOT EXISTS game_screenshots_game ON game_screenshots(gameId,addedAt);
        CREATE TABLE IF NOT EXISTS game_notes(gameId TEXT PRIMARY KEY,rating REAL,notes TEXT NOT NULL DEFAULT '',updatedAt INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS game_memories(id INTEGER PRIMARY KEY,gameId TEXT NOT NULL,date TEXT NOT NULL,text TEXT NOT NULL,screenshotId TEXT,createdAt INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS game_memories_game ON game_memories(gameId,date);
        CREATE TABLE IF NOT EXISTS game_input(gameId TEXT PRIMARY KEY,support TEXT NOT NULL DEFAULT 'unknown',supportSource TEXT NOT NULL DEFAULT '',preferred TEXT NOT NULL DEFAULT '',notes TEXT NOT NULL DEFAULT '',updatedAt INTEGER NOT NULL,steamCheckedAt INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS install_sizes(gameId TEXT NOT NULL,path TEXT NOT NULL,bytes INTEGER NOT NULL,measuredAt INTEGER NOT NULL,partial INTEGER NOT NULL DEFAULT 0,source TEXT NOT NULL DEFAULT 'scan',PRIMARY KEY(gameId,path));
        CREATE TABLE IF NOT EXISTS game_added(gameId TEXT PRIMARY KEY,addedAt INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS custom_launchers(id TEXT PRIMARY KEY,name TEXT NOT NULL,iconKind TEXT NOT NULL,iconValue TEXT NOT NULL,color TEXT NOT NULL,executable TEXT NOT NULL,argsTemplate TEXT NOT NULL DEFAULT '',targetKind TEXT NOT NULL,workingDir TEXT,sortOrder INTEGER NOT NULL DEFAULT 0,createdAt INTEGER NOT NULL);",
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
    let emulator_columns: HashSet<String> = db
        .prepare("SELECT name FROM pragma_table_info('emulators')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, definition) in [
        ("systems", "TEXT NOT NULL DEFAULT '[]'"),
        ("enabled", "INTEGER NOT NULL DEFAULT 1"),
        ("source", "TEXT NOT NULL DEFAULT 'manual'"),
        ("detectedAs", "TEXT"),
        ("addedAt", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !emulator_columns.contains(name) {
            db.execute_batch(&format!(
                "ALTER TABLE emulators ADD COLUMN {name} {definition};"
            ))?;
        }
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
            migration_backup(db, file, "title")?;
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
    // Launcher games folders and launcher target-kind sets (nullable columns, idempotent; no
    // `user_version` change: older builds ignore them).
    let root_columns: HashSet<String> = db
        .prepare("SELECT name FROM pragma_table_info('library_roots')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !root_columns.contains("launcherId") {
        db.execute_batch("ALTER TABLE library_roots ADD COLUMN launcherId TEXT;")?;
    }
    crate::launchers::migrate(db)?;
    // Linked games (`links`): an added table, no `user_version` change (older builds ignore it).
    crate::links::migrate(db)?;
    // Session history columns and tables. Sessions still open belong to a previous run that
    // ended without closing them (crash, power loss): they keep their last saved length, are
    // closed at that point and marked interrupted.
    crate::sessions::migrate(db)?;
    db.execute(
        "INSERT OR IGNORE INTO settings(key,value) VALUES(?,?)",
        params!["app", serde_json::to_string(&Settings::default())?],
    )?;
    crate::storage::prepare(db)?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        // Written by a newer Game Lobby: open it as usual, but don't run cleanups that may
        // not suit its format.
        log::warn!(
            "The library was last used by a newer Game Lobby (schema {version}, this version knows {SCHEMA_VERSION}); skipping upgrade steps."
        );
        return Ok(());
    }
    remove_store_accounts(db, file, version)?;
    Ok(())
}

/// The `PRAGMA user_version` this build writes.
pub const SCHEMA_VERSION: i64 = 1;

/// What [`remove_store_accounts`] would change.
struct StoreAccountCleanup {
    secrets: Vec<&'static str>,
    connections: Option<ConnectionValues>,
    routes: Vec<(String, Vec<GameRoute>)>,
}

impl StoreAccountCleanup {
    fn is_empty(&self) -> bool {
        self.secrets.is_empty() && self.connections.is_none() && self.routes.is_empty()
    }
}

fn plan_store_account_cleanup(db: &Connection) -> Result<StoreAccountCleanup> {
    let mut secrets = Vec::new();
    for name in crate::secrets::RETIRED_SECRETS {
        if read_secret(db, name)?.is_some() {
            secrets.push(*name);
        }
    }
    let stored: Option<String> = db
        .query_row(
            "SELECT value FROM library_connections WHERE key='main'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let retired_keys = stored
        .as_deref()
        .and_then(|s| serde_json::from_str::<Map<String, Value>>(s).ok())
        .is_some_and(|map| map.contains_key("steamId") || map.contains_key("legendaryPath"));
    let connections = if retired_keys {
        Some(get_connection_values(db)?)
    } else {
        None
    };
    let mut routes = Vec::new();
    for row in all_rows(db)? {
        let next: Vec<GameRoute> = row.routes.iter().map(without_legendary).collect();
        if next != row.routes {
            routes.push((row.id, next));
        }
    }
    Ok(StoreAccountCleanup {
        secrets,
        connections,
        routes,
    })
}

/// Cleanup after the Steam Web API and Legendary imports were removed: deletes the stored
/// Steam API key, the SteamID and the Legendary path, and turns Legendary launch routes into
/// Epic Games Launcher links. It runs on every start and only writes when something is left
/// (for example after an older version was run again), so it heals itself. Before the first
/// change to a library file, a copy is kept next to it. The schema version is set in the same
/// transaction as the changes.
fn remove_store_accounts(db: &Connection, file: Option<&Path>, version: i64) -> Result<()> {
    let plan = plan_store_account_cleanup(db)?;
    if plan.is_empty() {
        if version < SCHEMA_VERSION {
            db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        return Ok(());
    }
    if let Some(file) = file {
        migration_backup(db, file, "store-accounts")?;
    }
    let tx = db.unchecked_transaction()?;
    for name in &plan.secrets {
        tx.execute("DELETE FROM secrets WHERE key=?", [name])?;
    }
    if let Some(values) = &plan.connections {
        put_connection_values(&tx, values)?;
    }
    for (id, routes) in &plan.routes {
        tx.execute(
            "UPDATE games SET routes=? WHERE id=?",
            params![serde_json::to_string(routes)?, id],
        )?;
    }
    if version < SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(())
}

/// An Epic route as the removed Legendary import left it, made launchable through the Epic
/// Games Launcher: Legendary's executable and arguments are replaced by the launcher link and
/// its `epic-legendary` install state is dropped. Other routes are returned unchanged.
pub fn without_legendary(route: &GameRoute) -> GameRoute {
    let mut next = route.clone();
    if route.source_id != "epic" {
        return next;
    }
    let legendary_state = next
        .install_states
        .as_mut()
        .and_then(|states| states.remove("epic-legendary"))
        .is_some();
    if legendary_state {
        let states = next.install_states.clone().unwrap_or_default();
        next.installed = Some(states.values().any(|v| *v));
    }
    if non_empty(&route.launch_args).is_some() {
        next.launch_args = None;
        if !legendary_state && next.install_states.as_ref().is_none_or(|s| s.is_empty()) {
            next.installed = Some(false);
        }
        // Not installed: an install link, like owned-only Epic games, so later Epic scans keep
        // the route as owned instead of treating it as a stale launcher entry.
        let action = if next.installed == Some(true) {
            "launch"
        } else {
            "install"
        };
        next.launch_target = match non_empty(&route.app_id) {
            Some(app) => crate::importers::epic::launch_uri(app, action),
            None => "com.epicgames.launcher://store".into(),
        };
    }
    if next.installed == Some(false) {
        next.install_path = None;
    }
    next
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
    if old.is_none() {
        // A new game under the id of an entry linked into another game (a scan or backup
        // re-adds it): the entry's details go back to it and its record is forgotten, so the
        // new game neither inherits rows parked there nor loses them when that game is removed.
        crate::links::release_member(db, &game.id)?;
    }
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

/// Saves one partial game without listing the library afterwards (for loops in a transaction).
pub(crate) fn save_patch(db: &Connection, game: &GamePatch) -> Result<()> {
    persist_game(db, game).map(drop)
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
    // Launch options of a local route belong to the .exe they were confirmed for: they carry
    // over only when both routes start the same file.
    let same_program = !incoming
        .source_id
        .eq_ignore_ascii_case(crate::local::LOCAL_SOURCE)
        || matches!(
            (
                crate::tracker::normalize_path(&existing.launch_target),
                crate::tracker::normalize_path(&incoming.launch_target),
            ),
            (Some(a), Some(b)) if a == b
        );
    combined.launch_args = incoming
        .launch_args
        .clone()
        .or_else(|| existing.launch_args.clone().filter(|_| same_program));
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
    // A chosen emulator is the user's decision: scans never replace it (they never set one).
    combined.emulator_id = existing
        .emulator_id
        .clone()
        .or_else(|| incoming.emulator_id.clone());
    combined.system = incoming.system.clone().or_else(|| existing.system.clone());
    combined.launcher_id = incoming
        .launcher_id
        .clone()
        .or_else(|| existing.launcher_id.clone());
    // A launcher assignment made on the game page is the user's decision: scans never set one,
    // and it stays while the route keeps the label (or launcher) it was made with.
    combined.assigned = if combined.launcher_id == existing.launcher_id {
        existing.assigned.or(incoming.assigned)
    } else {
        incoming.assigned
    };
    // A route name is the user's (scans never set one).
    combined.name = existing.name.clone().or_else(|| incoming.name.clone());
    combined
}

pub(crate) fn merge_routes_into(routes: &mut Vec<GameRoute>, incoming: &[GameRoute]) {
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
    let mut stmt = db
        .prepare("SELECT gameId FROM game_page_custom UNION SELECT gameId FROM game_links UNION SELECT gameId FROM game_screenshots UNION SELECT gameId FROM game_notes UNION SELECT gameId FROM game_memories UNION SELECT gameId FROM game_input WHERE supportSource='user' OR preferred<>'' OR notes<>''")?;
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
        // An emulator chosen for a ROM (scans never set one; only the user does).
        || row.routes.iter().any(|r| r.emulator_id.is_some())
        // A launcher assigned on the game page (scans never assign one): the route it was made
        // from stays when its store or folder no longer lists it.
        || row.routes.iter().any(|r| r.assigned == Some(true))
        // A route the user named (or that got its name when games were linked).
        || row.routes.iter().any(|r| r.name.is_some())
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
            if !has_state && !inferred && !epic_installed_legacy {
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

/// Steam app ids with an installed `steam` route (the owned-games import never adds these).
pub fn installed_steam_app_ids(db: &Connection) -> Result<HashSet<String>> {
    Ok(all_rows(db)?
        .iter()
        .flat_map(|row| row.routes.iter())
        .filter(|r| r.source_id == "steam" && r.installed == Some(true))
        .filter_map(|r| non_empty(&r.app_id).map(str::to_owned))
        .collect())
}

/// App ids of every `steam` route in any state (installed or not, owned or not).
pub fn steam_app_ids_any(db: &Connection) -> Result<HashSet<String>> {
    Ok(all_rows(db)?
        .iter()
        .flat_map(|row| row.routes.iter())
        .filter(|r| r.source_id == "steam")
        .filter_map(|r| non_empty(&r.app_id).map(str::to_owned))
        .collect())
}

/// Undoes the optional Steam owned-games import (`steam_owned`) for exactly the app ids in
/// `app_ids` (ones an earlier import listed that are no longer owned, or all of them when the
/// import is removed). Only `steam` routes with one of those app ids that are marked owned and
/// not installed are touched: a game the user put something into (including a manual IGDB
/// match; an edited title is not detectable) keeps its route with the
/// owned mark dropped; otherwise the route goes, and a game left without routes is deleted.
/// Installed games and every other route are left alone. Returns the number of games deleted.
pub fn drop_steam_owned(db: &mut Connection, app_ids: &HashSet<String>) -> Result<usize> {
    if app_ids.is_empty() {
        return Ok(0);
    }
    let rows = all_rows(db)?;
    let mut customized = customized_game_ids(db)?;
    // A manual IGDB match is the user's work too (an automatic one is only a cache).
    customized.extend(
        db.prepare("SELECT gameId FROM game_metadata WHERE source<>'auto'")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    let tx = db.transaction()?;
    let mut deleted = 0;
    for row in rows {
        let matches = |r: &GameRoute| {
            r.source_id == "steam"
                && r.owned == Some(true)
                && r.installed != Some(true)
                && non_empty(&r.app_id).is_some_and(|id| app_ids.contains(id))
        };
        if !row.routes.iter().any(matches) {
            continue;
        }
        let keep = has_user_data(&row, &customized);
        let mut updated = Vec::with_capacity(row.routes.len());
        for route in &row.routes {
            if !matches(route) {
                updated.push(route.clone());
            } else if keep {
                updated.push(GameRoute {
                    owned: None,
                    ..route.clone()
                });
            }
        }
        if updated.is_empty() {
            delete_game_rows(&tx, &row.id)?;
            deleted += 1;
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
    Ok(deleted)
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

/// Deletes a game together with its play sessions and cached art, and the rows of the entries
/// linked into it (`links`).
pub(crate) fn delete_game_rows(db: &Connection, id: &str) -> Result<()> {
    crate::links::delete_members(db, id)?;
    for sql in [
        "DELETE FROM play_sessions WHERE gameId=?",
        "DELETE FROM game_art WHERE gameId=?",
        "DELETE FROM game_page_custom WHERE gameId=?",
        "DELETE FROM game_metadata WHERE gameId=?",
        "DELETE FROM game_screenshots WHERE gameId=?",
        "DELETE FROM game_notes WHERE gameId=?",
        "DELETE FROM game_memories WHERE gameId=?",
        "DELETE FROM game_input WHERE gameId=?",
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

/// Links `source_id` into `target_id` with the default choices (`links::link`): routes and
/// sessions move over, playtime is summed, the target's title stays, and the details only the
/// source has (cover, page art, an IGDB match the user chose, notes) are used; the rest of the
/// source's details stay parked for unlinking. Only the tests use it now (the interface links
/// through the review, `links::link`).
#[cfg(test)]
pub fn merge_games(db: &mut Connection, target_id: &str, source_id: &str) -> Result<Vec<Game>> {
    if target_id == source_id {
        bail!("Choose two different games to merge.");
    }
    if get_game(db, target_id)?.is_none() || get_game(db, source_id)?.is_none() {
        bail!("One of the selected games no longer exists.");
    }
    crate::links::link(db, &[target_id.to_string(), source_id.to_string()], None)?;
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
        "SELECT id,startedAt,endedAt,seconds,source,edited,interrupted FROM play_sessions WHERE gameId=? ORDER BY startedAt DESC,id DESC LIMIT ?",
    )?;
    let rows = stmt
        .query_map(params![game_id, limit], |row| {
            Ok(PlaySession {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                seconds: row.get(3)?,
                source: row.get(4)?,
                edited: row.get::<_, i64>(5)? != 0,
                interrupted: row.get::<_, i64>(6)? != 0,
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
// IGDB metadata cache

/// One `game_metadata` row. `igdb_id`/`json` are `None` for a cached "no match" result.
/// `source` is `"auto"` (matched by Game Lobby) or `"user"` (chosen by the user; never
/// replaced by an automatic match).
#[derive(Clone, Debug, PartialEq)]
pub struct CachedMetadata {
    pub igdb_id: Option<i64>,
    pub json: Option<String>,
    pub fetched_at: i64,
    pub source: String,
}

pub fn cached_metadata(db: &Connection, game_id: &str) -> Result<Option<CachedMetadata>> {
    Ok(db
        .query_row(
            "SELECT igdbId,json,fetchedAt,source FROM game_metadata WHERE gameId=?",
            [game_id],
            |row| {
                Ok(CachedMetadata {
                    igdb_id: row.get(0)?,
                    json: row.get(1)?,
                    fetched_at: row.get(2)?,
                    source: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Stores metadata for a game that still exists. A user-chosen row is only replaced by
/// another user choice, or refreshed by an automatic fetch of the same IGDB id (it stays
/// `user`). Returns whether a row was written.
pub fn put_metadata(db: &Connection, game_id: &str, entry: &CachedMetadata) -> Result<bool> {
    let source = if entry.source == "user" {
        "user"
    } else {
        "auto"
    };
    let changed = db.execute(
        "INSERT INTO game_metadata(gameId,igdbId,json,fetchedAt,source) SELECT ?1,?2,?3,?4,?5 WHERE EXISTS(SELECT 1 FROM games WHERE id=?1)
         ON CONFLICT(gameId) DO UPDATE SET igdbId=excluded.igdbId,json=excluded.json,fetchedAt=excluded.fetchedAt,
           source=CASE WHEN game_metadata.source='user' THEN 'user' ELSE excluded.source END
         WHERE game_metadata.source<>'user' OR excluded.source='user' OR game_metadata.igdbId IS excluded.igdbId",
        params![game_id, entry.igdb_id, entry.json, entry.fetched_at, source],
    )?;
    Ok(changed > 0)
}

pub fn delete_metadata(db: &Connection, game_id: &str) -> Result<()> {
    db.execute("DELETE FROM game_metadata WHERE gameId=?", [game_id])?;
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
    write_page_custom(db, game_id, custom)?;
    Ok(true)
}

/// [`put_page_custom`] without the game check (rows parked for a linked entry, `links`).
fn write_page_custom(db: &Connection, game_id: &str, custom: &GamePageCustom) -> Result<()> {
    if custom.is_default() {
        db.execute("DELETE FROM game_page_custom WHERE gameId=?", [game_id])?;
        return Ok(());
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
    Ok(())
}

/// Every customization of a game in the library (backups), by game id, and the rows parked for
/// entries linked into a game (`links`), which a restore brings back with their link records.
pub fn all_page_custom(db: &Connection) -> Result<Vec<GamePageCustomEntry>> {
    let mut stmt = db.prepare(
        "SELECT * FROM game_page_custom WHERE gameId IN (SELECT id FROM games UNION SELECT memberId FROM game_links) ORDER BY gameId",
    )?;
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

/// What the last scan found per store id.
pub fn store_detection(db: &Connection) -> Result<HashMap<String, StoreDetection>> {
    let stored: Option<String> = db
        .query_row(
            "SELECT value FROM library_connections WHERE key='stores'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

/// Merges new per-store results into the stored ones (stores not scanned keep theirs).
pub fn put_store_detection(db: &Connection, found: &HashMap<String, StoreDetection>) -> Result<()> {
    let mut all = store_detection(db)?;
    all.extend(found.clone());
    db.execute(
        "INSERT INTO library_connections(key,value) VALUES('stores',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(&all)?],
    )?;
    Ok(())
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
        "SELECT id,name,executable,args,romFolders,extensions,systems,enabled,source,detectedAs,addedAt FROM emulators ORDER BY name COLLATE NOCASE,executable COLLATE NOCASE,addedAt,id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let json = |i: usize| -> rusqlite::Result<Vec<String>> {
                Ok(row
                    .get::<_, Option<String>>(i)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default())
            };
            Ok(EmulatorProfile {
                id: row.get(0)?,
                name: row.get(1)?,
                executable: row.get(2)?,
                args: row.get(3)?,
                rom_folders: json(4)?,
                extensions: json(5)?,
                systems: json(6)?,
                enabled: row.get::<_, Option<i64>>(7)?.unwrap_or(1) != 0,
                source: row
                    .get::<_, Option<String>>(8)?
                    .unwrap_or_else(|| "manual".into()),
                detected_as: row.get::<_, Option<String>>(9)?.filter(|s| !s.is_empty()),
                available: false,
                needs_configuration: false,
                added_at: row.get::<_, Option<i64>>(10)?.unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // No filesystem access here (the database lock is held): `available` stays false until
    // `emulators::refresh_available` runs after the lock is released.
    Ok(rows
        .into_iter()
        .map(|mut p| {
            p.needs_configuration = crate::emulators::needs_configuration(&p);
            p
        })
        .collect())
}

pub fn emulator_profile(db: &Connection, id: &str) -> Result<Option<EmulatorProfile>> {
    Ok(emulator_profiles(db)?.into_iter().find(|p| p.id == id))
}

/// Inserts or replaces a whole profile (`available` / `needsConfiguration` are not stored).
/// `addedAt` is set once, on insert (the profile's own value when it has one, else now).
pub fn save_emulator(db: &Connection, p: &EmulatorProfile) -> Result<Vec<EmulatorProfile>> {
    db.execute(
        "INSERT INTO emulators(id,name,executable,args,romFolders,extensions,systems,enabled,source,detectedAs,addedAt) VALUES(?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name,executable=excluded.executable,args=excluded.args,romFolders=excluded.romFolders,extensions=excluded.extensions,systems=excluded.systems,enabled=excluded.enabled,source=excluded.source,detectedAs=excluded.detectedAs",
        params![
            p.id,
            p.name,
            p.executable,
            p.args,
            serde_json::to_string(&p.rom_folders)?,
            serde_json::to_string(&p.extensions)?,
            serde_json::to_string(&p.systems)?,
            p.enabled as i64,
            p.source,
            p.detected_as,
            if p.added_at > 0 { p.added_at } else { now_ms() },
        ],
    )?;
    emulator_profiles(db)
}

/// Deletes a profile. Its executable is remembered (`scan_ignore` kind `emulator`) so a
/// rescan never brings it back, and ROM routes assigned to it go back to automatic.
pub fn remove_emulator(db: &Connection, id: &str) -> Result<bool> {
    let Some(profile) = emulator_profile(db, id)? else {
        return Ok(false);
    };
    let tx = db.unchecked_transaction()?;
    if let Some(key) = crate::localscan::path_key(&profile.executable) {
        add_ignore(&tx, IGNORE_EMULATOR, &key, &profile.executable)?;
    }
    tx.execute("DELETE FROM emulators WHERE id=?", [id])?;
    for row in all_rows(&tx)? {
        if row
            .routes
            .iter()
            .any(|r| r.emulator_id.as_deref() == Some(id))
        {
            let routes = row
                .routes
                .iter()
                .cloned()
                .map(|mut r| {
                    if r.emulator_id.as_deref() == Some(id) {
                        r.emulator_id = None;
                    }
                    r
                })
                .collect();
            set_routes(&tx, &row.id, routes)?;
        }
    }
    tx.commit()?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Scan ignore list (`scan_ignore`): removed emulator executables and ignored game paths.
// `key` is the normalized (lower-case) path; `path` the path as shown to the user.

pub const IGNORE_EMULATOR: &str = "emulator";
pub const IGNORE_PATH: &str = "path";

pub fn add_ignore(db: &Connection, kind: &str, key: &str, path: &str) -> Result<()> {
    db.execute(
        "INSERT INTO scan_ignore(kind,key,path,addedAt) VALUES(?,?,?,?) ON CONFLICT(kind,key) DO NOTHING",
        params![kind, key, path, now_ms()],
    )?;
    Ok(())
}

pub fn remove_ignore(db: &Connection, kind: &str, key: &str) -> Result<bool> {
    Ok(db.execute(
        "DELETE FROM scan_ignore WHERE kind=? AND key=?",
        params![kind, key],
    )? > 0)
}

/// (key, path, addedAt) of one ignore kind, oldest first.
pub fn ignores(db: &Connection, kind: &str) -> Result<Vec<(String, String, i64)>> {
    let mut stmt =
        db.prepare("SELECT key,path,addedAt FROM scan_ignore WHERE kind=? ORDER BY addedAt,key")?;
    let rows = stmt
        .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn ignore_keys(db: &Connection, kind: &str) -> Result<HashSet<String>> {
    Ok(ignores(db, kind)?.into_iter().map(|(k, _, _)| k).collect())
}

// ---------------------------------------------------------------------------
// Local (library folder) games

/// Install-state key of a library root's routes (`GameRoute.installStates`).
pub fn root_state_key(root_id: &str) -> String {
    format!("root:{root_id}")
}

/// Marks local routes as missing: for every route `missing` selects, the install state
/// `state_key` (or, with `None`, every `root:` state) becomes false. A route no longer
/// installed anywhere loses its install path and is dropped when it isn't owned and the game
/// holds no user data (`has_user_data`); a game left without routes is deleted. Returns
/// (routes marked not installed, games removed).
pub fn mark_local_routes_missing(
    db: &mut Connection,
    state_key: Option<&str>,
    missing: &dyn Fn(&GameRoute) -> bool,
) -> Result<(usize, usize)> {
    let rows = all_rows(db)?;
    let customized = customized_game_ids(db)?;
    let tx = db.transaction()?;
    let (mut marked, mut removed) = (0, 0);
    for row in rows {
        let mut changed = false;
        let mut updated = Vec::with_capacity(row.routes.len());
        for route in &row.routes {
            let mut states = route.install_states.clone().unwrap_or_default();
            let keys: Vec<String> = states
                .iter()
                .filter(|(k, v)| {
                    **v && match state_key {
                        Some(key) => k.as_str() == key,
                        None => k.starts_with("root:"),
                    }
                })
                .map(|(k, _)| k.clone())
                .collect();
            if keys.is_empty() || !missing(route) {
                updated.push(route.clone());
                continue;
            }
            for key in keys {
                states.insert(key, false);
            }
            let installed = states.values().any(|v| *v);
            let mut next = route.clone();
            next.install_states = Some(states);
            next.installed = Some(installed);
            changed = true;
            if !installed {
                next.install_path = None;
                marked += 1;
                if next.owned != Some(true) && !has_user_data(&row, &customized) {
                    continue;
                }
            }
            updated.push(next);
        }
        if !changed {
            continue;
        }
        if updated.is_empty() {
            delete_game_rows(&tx, &row.id)?;
            removed += 1;
        } else {
            set_routes(&tx, &row.id, updated)?;
        }
    }
    tx.commit()?;
    Ok((marked, removed))
}

/// Missing-route counts above which a scan's removals are held back (see
/// [`reconcile_local_root`]).
pub const MASS_REMOVAL_MIN: usize = 20;

/// Result of [`reconcile_local_root`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LocalReconcile {
    /// Routes marked not installed (including dropped ones).
    pub marked: usize,
    pub removed_games: usize,
    /// Routes that would have been marked missing but were held back by the guard.
    pub held: usize,
}

/// Applies one library root's scan: routes of that root (`root_state_key`) whose identity
/// isn't in `found` are marked missing, except where `protected` says the scan couldn't see
/// them (an unreadable folder). Like store scans, an empty result changes nothing. A scan that
/// would mark more than half of the root's installed routes, and more than
/// [`MASS_REMOVAL_MIN`], as missing changes nothing either (a drive half-mounted, a renamed
/// parent folder): the count is returned as `held`.
pub fn reconcile_local_root(
    db: &mut Connection,
    root_id: &str,
    found: &HashSet<String>,
    protected: &dyn Fn(&GameRoute) -> bool,
) -> Result<LocalReconcile> {
    if found.is_empty() {
        return Ok(LocalReconcile::default());
    }
    let key = root_state_key(root_id);
    let missing = |route: &GameRoute| !found.contains(&route_identity(route)) && !protected(route);
    let (mut total, mut would_mark) = (0, 0);
    for row in all_rows(db)? {
        for route in &row.routes {
            let active = route
                .install_states
                .as_ref()
                .is_some_and(|s| s.get(&key) == Some(&true));
            if active {
                total += 1;
                if missing(route) {
                    would_mark += 1;
                }
            }
        }
    }
    if would_mark > MASS_REMOVAL_MIN && would_mark * 2 > total {
        return Ok(LocalReconcile {
            held: would_mark,
            ..Default::default()
        });
    }
    let (marked, removed_games) = mark_local_routes_missing(db, Some(&key), &missing)?;
    Ok(LocalReconcile {
        marked,
        removed_games,
        held: 0,
    })
}

/// Shortcuts the user confirmed: normalized `.lnk` path → (target, arguments) they saw.
pub fn shortcut_confirmations(db: &Connection) -> Result<HashMap<String, (String, String)>> {
    let mut stmt = db.prepare("SELECT shortcutKey,target,arguments FROM local_shortcut_ok")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn confirm_shortcut(db: &Connection, key: &str, target: &str, arguments: &str) -> Result<()> {
    db.execute(
        "INSERT INTO local_shortcut_ok(shortcutKey,target,arguments,confirmedAt) VALUES(?,?,?,?) ON CONFLICT(shortcutKey) DO UPDATE SET target=excluded.target,arguments=excluded.arguments,confirmedAt=excluded.confirmedAt",
        params![key, target, arguments, now_ms()],
    )?;
    Ok(())
}

/// Every game with whether it holds user data (read-only passes such as duplicate checks).
pub fn games_with_user_data(db: &Connection) -> Result<Vec<(Game, bool)>> {
    let customized = customized_game_ids(db)?;
    Ok(all_rows(db)?
        .into_iter()
        .map(|row| {
            let user = has_user_data(&row, &customized);
            (row_to_game(row), user)
        })
        .collect())
}

/// Replaces one game's routes.
pub fn set_routes(db: &Connection, id: &str, routes: Vec<GameRoute>) -> Result<()> {
    persist_game(
        db,
        &GamePatch {
            routes: Some(routes),
            ..GamePatch::new(id)
        },
    )?;
    Ok(())
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
    // Games from older backups may still carry Legendary launch routes.
    let games: Vec<Game> = backup
        .games
        .iter()
        .map(|g| Game {
            routes: g.routes.iter().map(without_legendary).collect(),
            ..g.clone()
        })
        .collect();
    let backup = &Backup {
        games,
        ..backup.clone()
    };
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
    // Validated by `validation::backup` (same checks as a save, apart from the picks).
    for launcher in backup.launchers.iter().flatten() {
        crate::launchers::save(&tx, launcher)?;
    }
    // Link records first: the rows parked for the entries they restore come back with them.
    let parked = match &backup.game_links {
        Some(links) => crate::links::restore_records(&tx, links)?,
        None => HashSet::new(),
    };
    // Entries for games that aren't in the library (or restored linked entries) are skipped.
    for entry in backup.game_page_custom.iter().flatten() {
        if parked.contains(&entry.game_id) {
            write_page_custom(&tx, &entry.game_id, &entry.custom())?;
        } else {
            put_page_custom(&tx, &entry.game_id, &entry.custom())?;
        }
    }
    if let Some(notes) = &backup.game_notes {
        crate::notes::restore_backup(&tx, notes, &parked)?;
    }
    let ids: Vec<&str> = backup.games.iter().map(|g| g.id.as_str()).collect();
    crate::storage::restore_added(&tx, &ids, backup.game_added.as_deref())?;
    if let Some(sessions) = &backup.play_sessions {
        crate::sessions::restore(&tx, sessions, now_ms())?;
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
    copy_library_from(&appdata.join("game-lobby"), data_dir)
}

/// Copies `old_dir/library.sqlite` (read through its WAL, with the online backup API) and the
/// custom covers into `data_dir`. The `-wal` and `-shm` files are never copied: a WAL copied
/// apart from its main file can be replayed onto the wrong database.
pub(crate) fn copy_library_from(old_dir: &Path, data_dir: &Path) -> Result<bool> {
    let target = data_dir.join("library.sqlite");
    let old = old_dir.join("library.sqlite");
    if target.exists() || !old.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(data_dir)?;
    let copied = crate::dbrepair::open_source(&old, false)
        .and_then(|source| crate::dbrepair::backup_to(&source, &target));
    if let Err(e) = copied {
        // Only the file this call created; the next start tries again.
        let _ = std::fs::remove_file(&target);
        return Err(e);
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

    fn legendary_route(app: &str, installed: bool, also_launcher: Option<bool>) -> GameRoute {
        let mut states = std::collections::BTreeMap::new();
        states.insert("epic-legendary".to_string(), installed);
        if let Some(launcher) = also_launcher {
            states.insert("epic-installed".to_string(), launcher);
        }
        GameRoute {
            source_id: "epic".into(),
            store: "Epic Games".into(),
            app_id: Some(app.into()),
            launch_target: if installed {
                r"C:\Tools\legendary.exe".into()
            } else {
                format!("com.epicgames.launcher://apps/{app}?action=install&silent=true")
            },
            launch_args: installed.then(|| format!("[\"launch\",\"{app}\"]")),
            install_path: installed.then(|| format!(r"D:\Games\{app}")),
            owned: Some(true),
            installed: Some(installed || also_launcher == Some(true)),
            install_states: Some(states),
            ..Default::default()
        }
    }

    #[test]
    fn startup_cleanup_keeps_a_newly_stored_steam_owned_games_key() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        let key = crate::secrets::STEAM_OWNED_API_KEY;
        let id = crate::secrets::STEAM_OWNED_STEAM_ID;
        write_secret(&conn, key, b"sealed key").unwrap();
        write_secret(&conn, id, b"sealed id").unwrap();
        crate::steam_owned::save_import_state(
            &conn,
            &crate::steam_owned::ImportState {
                app_ids: vec!["10".into()],
                ..Default::default()
            },
        )
        .unwrap();
        // Leftovers of the removed import, so the cleanup really runs (on an old schema too).
        write_secret(&conn, "steam-api-key-dpapi", b"old").unwrap();
        conn.execute(
            "INSERT INTO library_connections(key,value) VALUES('main',?)",
            [serde_json::json!({ "steamId": "76561190000000000" }).to_string()],
        )
        .unwrap();
        for version in [0, SCHEMA_VERSION] {
            conn.pragma_update(None, "user_version", version).unwrap();
            migrate(&conn, None).unwrap();
            assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
            assert_eq!(
                read_secret(&conn, key).unwrap().as_deref(),
                Some(&b"sealed key"[..])
            );
            assert_eq!(
                read_secret(&conn, id).unwrap().as_deref(),
                Some(&b"sealed id"[..])
            );
            assert_eq!(
                crate::steam_owned::load_import_state(&conn)
                    .unwrap()
                    .app_ids,
                ["10"]
            );
        }
    }

    #[test]
    fn dropping_steam_owned_games_only_touches_the_listed_owned_routes() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        let owned = |app: &str| GameRoute {
            owned: Some(true),
            install_states: None,
            ..route("steam", app, false)
        };
        for g in [
            game("steam:1", "Owned only", vec![owned("1")]),
            game("steam:2", "Favourite", vec![owned("2")]),
            game("steam:3", "Installed", vec![route("steam", "3", true)]),
            game("steam:4", "Not listed", vec![owned("4")]),
            game(
                "x",
                "Two routes",
                vec![owned("5"), route("epic", "five", true)],
            ),
        ] {
            save_game(&conn, &GamePatch::from(&g)).unwrap();
        }
        conn.execute("UPDATE games SET favorite=1 WHERE id='steam:2'", [])
            .unwrap();
        let ids: HashSet<String> = ["1", "2", "3", "5"].map(String::from).into();
        assert_eq!(drop_steam_owned(&mut conn, &ids).unwrap(), 1);
        assert!(get_game(&conn, "steam:1").unwrap().is_none());
        let fav = get_game(&conn, "steam:2").unwrap().unwrap();
        assert_eq!(fav.routes[0].owned, None, "kept for the user's data");
        assert!(fav.favorite);
        assert_eq!(
            get_game(&conn, "steam:3").unwrap().unwrap().routes[0],
            route("steam", "3", true)
        );
        assert_eq!(
            get_game(&conn, "steam:4").unwrap().unwrap().routes[0].owned,
            Some(true)
        );
        let x = get_game(&conn, "x").unwrap().unwrap();
        assert_eq!(x.routes.len(), 1);
        assert_eq!(x.routes[0].source_id, "epic");
        assert_eq!(
            installed_steam_app_ids(&conn).unwrap(),
            ["3".to_string()].into()
        );
    }

    #[test]
    fn upgrade_removes_store_accounts_and_legendary_routes_once() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        // Simulate a library from before the removal.
        conn.pragma_update(None, "user_version", 0).unwrap();
        write_secret(&conn, "steam-api-key-dpapi", b"sealed").unwrap();
        write_secret(&conn, "steamgriddb-api-key-dpapi", b"keep").unwrap();
        conn.execute(
            "INSERT INTO library_connections(key,value) VALUES('main',?)",
            [serde_json::json!({
                "steamId": "76561190000000000",
                "legendaryPath": r"C:\Tools\legendary.exe",
                "gogDatabasePath": r"D:\galaxy.db"
            })
            .to_string()],
        )
        .unwrap();
        let steam = route("steam", "10", true);
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:A",
                "Both",
                vec![legendary_route("A", true, Some(true)), steam.clone()],
            )),
        )
        .unwrap();
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:B",
                "Legendary only",
                vec![legendary_route("B", true, None)],
            )),
        )
        .unwrap();
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:C",
                "Owned",
                vec![legendary_route("C", false, None)],
            )),
        )
        .unwrap();

        migrate(&conn, None).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
        assert!(read_secret(&conn, "steamgriddb-api-key-dpapi")
            .unwrap()
            .is_some());
        let raw: String = conn
            .query_row(
                "SELECT value FROM library_connections WHERE key='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !raw.contains("steamId") && !raw.contains("legendary"),
            "{raw}"
        );
        assert_eq!(
            get_connection_values(&conn).unwrap().gog_database_path,
            r"D:\galaxy.db"
        );

        let a = get_game(&conn, "epic:A").unwrap().unwrap();
        let epic = &a.routes[0];
        assert_eq!(
            epic.launch_target,
            "com.epicgames.launcher://apps/A?action=launch&silent=true"
        );
        assert_eq!(epic.launch_args, None);
        assert_eq!(
            epic.installed,
            Some(true),
            "still installed through the launcher"
        );
        assert!(!epic
            .install_states
            .as_ref()
            .unwrap()
            .contains_key("epic-legendary"));
        assert_eq!(a.routes[1], steam, "other routes are untouched");
        let b = get_game(&conn, "epic:B").unwrap().unwrap();
        assert_eq!(
            (b.routes[0].installed, b.routes[0].install_path.as_deref()),
            (Some(false), None)
        );
        assert!(b.routes[0]
            .launch_target
            .ends_with("action=install&silent=true"));
        let c = get_game(&conn, "epic:C").unwrap().unwrap();
        assert!(c.routes[0]
            .launch_target
            .ends_with("action=install&silent=true"));
        assert_eq!(c.routes[0].owned, Some(true));

        // Self-healing: an older version run again re-adds the key and a Legendary route;
        // the next start removes them again without changing anything else.
        write_secret(&conn, "steam-api-key-dpapi", b"x").unwrap();
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:D",
                "Again",
                vec![legendary_route("D", true, None)],
            )),
        )
        .unwrap();
        migrate(&conn, None).unwrap();
        assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
        let d = get_game(&conn, "epic:D").unwrap().unwrap();
        assert_eq!(d.routes[0].launch_args, None);
        assert_eq!(get_game(&conn, "epic:A").unwrap().unwrap(), a);
    }

    #[test]
    fn migrated_legendary_only_games_survive_the_next_epic_scan() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        conn.pragma_update(None, "user_version", 0).unwrap();
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:B",
                "Legendary only",
                vec![legendary_route("B", true, None)],
            )),
        )
        .unwrap();
        migrate(&conn, None).unwrap();
        // An Epic scan that finds no installed games for it (one other game is installed, so
        // the scan isn't ignored as empty).
        let other = GameRoute {
            source_id: "epic".into(),
            store: "Epic Games".into(),
            app_id: Some("Other".into()),
            launch_target: crate::importers::epic::launch_uri("Other", "launch"),
            installed: Some(true),
            install_states: Some([("epic-installed".to_string(), true)].into()),
            ..Default::default()
        };
        let scanned = vec![game("epic:Other", "Other", vec![other])];
        upsert_imported_games(&mut conn, &scanned).unwrap();
        reconcile_installed_source(&mut conn, "epic-installed", &scanned).unwrap();
        let b = get_game(&conn, "epic:B").unwrap().expect("kept as owned");
        assert_eq!(
            (b.routes[0].owned, b.routes[0].installed),
            (Some(true), Some(false))
        );
        assert!(b.routes[0]
            .launch_target
            .ends_with("action=install&silent=true"));
    }

    #[test]
    fn the_schema_version_is_set_with_the_cleanup_and_a_backup_is_kept() {
        let dir = std::env::temp_dir().join(format!("gl-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("library.sqlite");
        {
            let db = Db::open(&file).unwrap();
            let conn = db.lock();
            conn.pragma_update(None, "user_version", 0).unwrap();
            write_secret(&conn, "steam-api-key-dpapi", b"sealed").unwrap();
        }
        {
            let db = Db::open(&file).unwrap();
            let conn = db.lock();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
            assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
        }
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| {
                n.starts_with("library.sqlite.pre-store-accounts-migration-") && n.ends_with(".bak")
            })
            .collect();
        assert_eq!(backups.len(), 1, "one backup before the change");
        // The backup still holds the old data.
        let old = Connection::open(dir.join(&backups[0])).unwrap();
        assert!(read_secret(&old, "steam-api-key-dpapi").unwrap().is_some());
        drop(old);
        // Nothing left to clean: no further backups.
        drop(Db::open(&file).unwrap());
        let count = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            .count();
        assert_eq!(count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_cleanup_leaves_the_version_unchanged() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        conn.pragma_update(None, "user_version", 0).unwrap();
        write_secret(&conn, "steam-api-key-dpapi", b"sealed").unwrap();
        // Make the route update fail inside the transaction.
        save_game(
            &conn,
            &GamePatch::from(&game("epic:X", "X", vec![legendary_route("X", true, None)])),
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER no_route_updates BEFORE UPDATE OF routes ON games BEGIN SELECT RAISE(ABORT,'blocked'); END;",
        )
        .unwrap();
        assert!(migrate(&conn, None).is_err());
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 0, "rolled back together with the changes");
        assert!(read_secret(&conn, "steam-api-key-dpapi").unwrap().is_some());
    }

    #[test]
    fn newer_schemas_open_without_upgrade_steps_and_downgrades_heal() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        // A newer Game Lobby wrote version 2: opens fine, nothing is cleaned or rewritten.
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        write_secret(&conn, "steam-api-key-dpapi", b"sealed").unwrap();
        migrate(&conn, None).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert!(read_secret(&conn, "steam-api-key-dpapi").unwrap().is_some());
        // Downgrade then upgrade: an older build (no user_version handling) leaves version 1
        // and re-adds a Legendary route; this build repairs it on the next start.
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        save_game(
            &conn,
            &GamePatch::from(&game(
                "epic:Y",
                "Y",
                vec![legendary_route("Y", true, Some(true))],
            )),
        )
        .unwrap();
        migrate(&conn, None).unwrap();
        assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
        let y = get_game(&conn, "epic:Y").unwrap().unwrap();
        assert_eq!(y.routes[0].launch_args, None);
        assert!(y.routes[0]
            .launch_target
            .ends_with("action=launch&silent=true"));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn old_backups_with_store_accounts_still_import() {
        let raw = serde_json::json!({
            "games": [{
                "id": "epic:A", "title": "Legendary game", "store": "Epic Games", "playtimeMinutes": 0,
                "hidden": false, "favorite": false,
                "routes": [{
                    "sourceId": "epic", "store": "Epic Games", "appId": "A",
                    "launchTarget": "C:/Tools/legendary.exe", "launchArgs": "[\"launch\",\"A\"]",
                    "installed": true, "installStates": {"epic-legendary": true}
                }]
            }],
            "settings": {
                "scanIntervalHours": 0, "posterWidth": 172, "showBadges": true, "animations": true,
                "tracking": true, "steamId": "76561190000000000", "steamApiKey": "k", "legendaryPath": "C:/x.exe",
                "theme": { "name": "Default", "bg": "#101113", "surface": "#181a1d", "surface2": "#22252a", "accent": "#75d18a", "text": "#f2f3f4", "muted": "#969ba3", "radius": "10px", "gap": "9px", "font": "Inter, Segoe UI, sans-serif", "hover": "lift" }
            },
            "connections": {"steamId": "76561190000000000", "legendaryPath": "C:/x.exe"}
        });
        let parsed = crate::validation::backup(&raw).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        assert_eq!(restore_backup(&mut conn, &parsed).unwrap(), 1);
        let g = get_game(&conn, "epic:A").unwrap().unwrap();
        assert_eq!(g.routes[0].launch_args, None);
        assert!(g.routes[0]
            .launch_target
            .starts_with("com.epicgames.launcher://apps/A"));
        assert_eq!(read_secret(&conn, "steam-api-key-dpapi").unwrap(), None);
        // Unknown keys that were never valid are still rejected.
        let mut bad = raw.clone();
        bad["surprise"] = serde_json::json!(1);
        assert!(crate::validation::backup(&bad).is_err());
    }

    #[test]
    fn store_detection_merges_per_store() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, None).unwrap();
        assert!(store_detection(&conn).unwrap().is_empty());
        let entry = |found| StoreDetection {
            found,
            error: None,
            scanned_at: 5,
        };
        put_store_detection(
            &conn,
            &[
                ("steam".to_string(), entry(3)),
                ("gog".to_string(), entry(0)),
            ]
            .into(),
        )
        .unwrap();
        put_store_detection(&conn, &[("steam".to_string(), entry(4))].into()).unwrap();
        let all = store_detection(&conn).unwrap();
        assert_eq!((all["steam"].found, all["gog"].found), (4, 0));
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
    fn existing_databases_gain_the_links_table_and_keep_routes_without_names() {
        let conn = Connection::open_in_memory().unwrap();
        // A library from before linked games and route names, at the current schema version.
        conn.execute_batch(&format!(
            "{GAMES_TABLE};
             CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO games(id,title,store,routes) VALUES('a','A','Steam','[{{\"sourceId\":\"steam\",\"store\":\"Steam\",\"appId\":\"1\",\"launchTarget\":\"steam://rungameid/1\"}}]');"
        ))
        .unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        migrate(&conn, None).unwrap();
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('game_links')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(columns, ["memberId", "gameId", "linkedAt", "snapshot"]);
        let confirmed: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('launch_args_confirmed')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(confirmed, ["exeKey", "argv", "confirmedAt"]);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "an added table needs no new version"
        );
        let game = get_game(&conn, "a").unwrap().unwrap();
        assert_eq!(game.routes[0].name, None);
        // Named routes are stored as an extra JSON field that older builds ignore.
        set_routes(
            &conn,
            "a",
            vec![GameRoute {
                name: Some("Demo".into()),
                ..game.routes[0].clone()
            }],
        )
        .unwrap();
        let raw: String = conn
            .query_row("SELECT routes FROM games WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert!(raw.contains("\"name\":\"Demo\""));
        // Migrating again changes nothing.
        migrate(&conn, None).unwrap();
        assert_eq!(
            get_game(&conn, "a").unwrap().unwrap().routes[0]
                .name
                .as_deref(),
            Some("Demo")
        );
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
    fn merged_local_routes_keep_launch_options_only_for_the_same_exe() {
        let with_args = GameRoute {
            source_id: "local".into(),
            store: "Custom".into(),
            app_id: Some(r"c:\games\g".into()),
            launch_target: r"C:\Games\G\g.exe".into(),
            launch_args: Some(r#"["-x"]"#.into()),
            ..Default::default()
        };
        // Same identity (the folder), another .exe: the options were confirmed for g.exe.
        let other = GameRoute {
            launch_target: r"C:\Games\G\other.exe".into(),
            launch_args: None,
            ..with_args.clone()
        };
        assert_eq!(route_identity(&other), route_identity(&with_args));
        assert_eq!(merge_route(&with_args, &other).launch_args, None);
        let mut routes = vec![with_args.clone()];
        merge_routes_into(&mut routes, std::slice::from_ref(&other));
        assert_eq!(routes[0].launch_target, other.launch_target);
        assert_eq!(routes[0].launch_args, None);
        // The same file spelled differently keeps them.
        let same = GameRoute {
            launch_target: "c:/games/g/G.EXE".into(),
            launch_args: None,
            ..with_args.clone()
        };
        assert_eq!(
            merge_route(&with_args, &same).launch_args,
            with_args.launch_args
        );
        // Other stores' arguments aren't launch options confirmed for a file.
        let gog = GameRoute {
            source_id: "gog".into(),
            launch_target: r"C:\GOG\Galaxy.exe".into(),
            ..with_args.clone()
        };
        let moved = GameRoute {
            launch_target: r"D:\GOG\Galaxy.exe".into(),
            launch_args: None,
            ..gog.clone()
        };
        assert_eq!(merge_route(&gog, &moved).launch_args, gog.launch_args);
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

        // Both customized: the target's row is kept, the source's is parked for unlinking (and
        // backed up with its link record).
        merge_games(&mut conn, "a", "b").unwrap();
        assert_eq!(page_custom(&conn, "a").unwrap(), custom_hero("a"));
        assert_eq!(page_custom(&conn, "b").unwrap(), custom_hero("b"));
        // Only the source customized: it moves to the target.
        merge_games(&mut conn, "c", "d").unwrap();
        assert_eq!(page_custom(&conn, "c").unwrap(), custom_hero("d"));
        let ids: Vec<String> = all_page_custom(&conn)
            .unwrap()
            .into_iter()
            .map(|e| e.game_id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
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
            game_notes: None,
            game_added: None,
            play_sessions: None,
            game_links: None,
            launchers: None,
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
            game_notes: None,
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
    }

    #[test]
    fn old_emulator_tables_and_backups_still_work() {
        let conn = Connection::open_in_memory().unwrap();
        // A library from before library roots: emulators without the new columns.
        conn.execute_batch(&format!(
            "{GAMES_TABLE};
             CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE emulators(id TEXT PRIMARY KEY,name TEXT NOT NULL,executable TEXT NOT NULL,args TEXT NOT NULL,romFolders TEXT NOT NULL,extensions TEXT NOT NULL);
             INSERT INTO emulators VALUES('old','Old','D:\\Emu\\old.exe','{{rom}}','[\"D:\\\\Roms\"]','[\".iso\"]');"
        ))
        .unwrap();
        migrate(&conn, None).unwrap();
        migrate(&conn, None).unwrap();
        let list = emulator_profiles(&conn).unwrap();
        assert_eq!(list.len(), 1);
        let old = &list[0];
        assert_eq!((old.enabled, old.source.as_str()), (true, "manual"));
        assert!(old.systems.is_empty() && old.detected_as.is_none());
        assert_eq!(old.rom_folders, [r"D:\Roms"]);
        for table in [
            "library_roots",
            "scan_review",
            "scan_ignore",
            "local_exe_choice",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{table}");
        }

        // An old backup's emulator (no new fields) restores with defaults.
        let raw = serde_json::json!({ "games": [], "emulators": [
            { "id": "dolphin", "name": "Dolphin", "executable": "C:\\Emu\\Dolphin.exe", "args": "-e {rom}", "romFolders": [], "extensions": [".iso"] }
        ]});
        let parsed = crate::validation::backup(&raw).unwrap();
        let mut conn = conn;
        restore_backup(&mut conn, &parsed).unwrap();
        let restored = emulator_profile(&conn, "dolphin").unwrap().unwrap();
        assert!(restored.enabled && restored.systems.is_empty());
        // A new backup round-trips the new fields.
        let mut p = restored.clone();
        p.systems = vec!["gc".into(), "wii".into()];
        p.enabled = false;
        p.source = "discovered".into();
        p.detected_as = Some("dolphin".into());
        let text = serde_json::to_value(Backup {
            games: vec![],
            settings: None,
            emulators: Some(vec![p.clone()]),
            game_page_custom: None,
            game_notes: None,
            game_added: None,
            play_sessions: None,
            game_links: None,
            launchers: None,
        })
        .unwrap();
        let parsed = crate::validation::backup(&text).unwrap();
        restore_backup(&mut conn, &parsed).unwrap();
        let back = emulator_profile(&conn, "dolphin").unwrap().unwrap();
        assert_eq!(
            (
                back.systems.clone(),
                back.enabled,
                back.source.as_str(),
                back.detected_as.as_deref()
            ),
            (p.systems, false, "discovered", Some("dolphin"))
        );
        // Unknown systems or sources are rejected.
        let bad = serde_json::json!({ "games": [], "emulators": [
            { "id": "x", "name": "X", "executable": "x.exe", "args": "", "romFolders": [], "extensions": [], "systems": ["ps9"] }
        ]});
        assert!(crate::validation::backup(&bad).is_err());
    }

    #[test]
    fn routes_keep_their_emulator_choice_through_imports() {
        let db = Db::memory().unwrap();
        let mut conn = db.lock();
        let rom = |emulator: Option<&str>| GameRoute {
            source_id: "rom".into(),
            store: "Emulator".into(),
            app_id: Some(r"d:\roms\gba\a.gba".into()),
            launch_target: r"D:\Roms\GBA\a.gba".into(),
            installed: Some(true),
            emulator_id: emulator.map(Into::into),
            system: Some("gba".into()),
            ..Default::default()
        };
        let g = |id: &str, r| Game {
            id: id.into(),
            title: "A".into(),
            store: "Emulator".into(),
            routes: vec![r],
            ..Default::default()
        };
        upsert_imported_games(&mut conn, &[g("rom:1", rom(Some("chosen")))]).unwrap();
        upsert_imported_games(&mut conn, &[g("rom:2", rom(Some("default")))]).unwrap();
        let games = all_games(&conn).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].routes[0].emulator_id.as_deref(), Some("chosen"));
        // The new fields survive validation from the renderer.
        let value = serde_json::to_value(&games[0]).unwrap();
        assert_eq!(value["routes"][0]["emulatorId"], "chosen");
        assert!(crate::validation::game_patch(&value).is_ok());
        let mut bad = value.clone();
        bad["routes"][0]["system"] = "ps9".into();
        assert!(crate::validation::game_patch(&bad).is_err());
    }
}
