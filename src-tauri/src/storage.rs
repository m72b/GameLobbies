//! Install sizes, uninstall hand-offs and the "recently added" shelf.
//!
//! Security: the renderer only ever sends a game id and a route index. Sizes are measured under
//! install paths that are already stored for that game, and the uninstall hand-off builds its
//! link here from a fixed set of templates with strictly validated ids (see [`handoff`]). Game
//! Lobby never deletes game files itself.

use crate::state::AppState;
use crate::types::{now_ms, Game, GameRoute};
use crate::{db, platform};
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

const DAY_MS: i64 = 86_400_000;
/// Games added within this many days (and not dismissed) are on the "recently added" shelf.
pub const RECENT_DAYS: i64 = 14;
const RECENT_LIMIT: usize = 24;
/// One install folder may take this long to walk; the result is then marked partial.
const WALK_TIME: Duration = Duration::from_secs(30);
const WALK_ENTRIES: u64 = 3_000_000;
/// A whole rescan stops starting new folders after this long.
const RESCAN_TIME: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// Schema support. The tables themselves are in `db::migrate`.

/// Runs after the schema exists. The first time (no insert trigger yet) every game already in
/// the library is recorded as seen, so an upgrade never floods the shelf; after that, every
/// new row in `games` (scans, imports, manual adds, backups) is recorded as unseen by a trigger.
pub fn prepare(conn: &Connection) -> Result<()> {
    let has_trigger: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name='games_record_added')",
        [],
        |row| row.get(0),
    )?;
    if !has_trigger {
        conn.execute(
            "INSERT OR IGNORE INTO game_added(gameId,addedAt,seen) SELECT id,?,1 FROM games",
            [now_ms()],
        )?;
        conn.execute_batch(
            "CREATE TRIGGER games_record_added AFTER INSERT ON games BEGIN
               INSERT OR IGNORE INTO game_added(gameId,addedAt,seen)
               VALUES(new.id,CAST(strftime('%s','now') AS INTEGER)*1000,0);
             END;",
        )?;
    }
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS games_drop_storage AFTER DELETE ON games BEGIN
           DELETE FROM game_added WHERE gameId=old.id;
           DELETE FROM install_sizes WHERE gameId=old.id;
         END;",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Measuring

pub struct Walked {
    pub bytes: u64,
    /// false = a limit was reached (or a folder could not be read): `bytes` is a lower bound.
    pub complete: bool,
}

/// Symlinks, junctions and every other reparse point (shared with the health search).
pub(crate) fn is_reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Total size of the files under `root`. `root` itself may be a link (a library folder moved
/// to another drive); links found INSIDE it (symlinks, junctions, any reparse point) are
/// skipped, so loops and double counting cannot happen. Errors on single entries only make the
/// result partial.
pub fn walk_size(root: &Path, time: Duration, max_entries: u64) -> Result<Walked> {
    let meta = std::fs::metadata(root)?;
    if !meta.is_dir() {
        bail!("{} is not a folder.", root.display());
    }
    let deadline = Instant::now() + time;
    let mut stack = vec![root.to_path_buf()];
    let (mut bytes, mut seen, mut complete) = (0u64, 0u64, true);
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            complete = false;
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > max_entries || (seen % 256 == 0 && Instant::now() > deadline) {
                return Ok(Walked {
                    bytes,
                    complete: false,
                });
            }
            let Ok(entry) = entry else {
                complete = false;
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                complete = false;
                continue;
            };
            if is_reparse(&meta) {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    Ok(Walked { bytes, complete })
}

static SIZE_ON_DISK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""SizeOnDisk"\s+"(\d{1,18})""#).unwrap());
static STEAM_APP_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{1,10}$").unwrap());
static GOG_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]{1,20}$").unwrap());

/// Steam's own figure: `SizeOnDisk` in `steamapps/appmanifest_<id>.acf`, found from the stored
/// install folder `steamapps/common/<dir>`. `None` when it is not there, not readable or 0.
pub fn steam_manifest_size(install_path: &str, app_id: &str) -> Option<u64> {
    if !STEAM_APP_ID.is_match(app_id) {
        return None;
    }
    let common = Path::new(install_path).parent()?;
    if !common.file_name()?.eq_ignore_ascii_case("common") {
        return None;
    }
    let file = common.parent()?.join(format!("appmanifest_{app_id}.acf"));
    let meta = std::fs::metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let size: u64 = SIZE_ON_DISK.captures(&text)?[1].parse().ok()?;
    (size > 0).then_some(size)
}

struct Measured {
    bytes: u64,
    partial: bool,
    source: &'static str,
}

fn measure_route(route: &GameRoute, path: &str) -> Result<Measured> {
    if route.source_id == "steam" {
        if let Some(app_id) = route.app_id.as_deref() {
            if let Some(bytes) = steam_manifest_size(path, app_id) {
                return Ok(Measured {
                    bytes,
                    partial: false,
                    source: "manifest",
                });
            }
        }
    }
    let walked = walk_size(Path::new(path), WALK_TIME, WALK_ENTRIES)?;
    Ok(Measured {
        bytes: walked.bytes,
        partial: !walked.complete,
        source: "scan",
    })
}

/// The distinct stored install folders of a game's installed routes, with the route.
fn install_paths(game: &Game) -> Vec<(&GameRoute, String)> {
    let mut seen = HashSet::new();
    game.routes
        .iter()
        .filter(|r| r.installed == Some(true))
        .filter_map(|r| {
            let path = r.install_path.as_deref().filter(|p| !p.is_empty())?;
            seen.insert(path.to_lowercase())
                .then(|| (r, path.to_owned()))
        })
        .collect()
}

fn is_installed(game: &Game) -> bool {
    game.routes.iter().any(|r| r.installed == Some(true))
}

// ---------------------------------------------------------------------------
// Report

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UninstallInfo {
    pub route_index: usize,
    /// Display name of what will open.
    pub launcher: String,
    /// `uninstall` (the launcher's own uninstall dialog), `library` (its library: uninstall
    /// there) or `apps` (Windows' Installed apps settings).
    pub kind: &'static str,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    pub game_id: String,
    pub title: String,
    pub store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    /// `None` = unknown (no install path, or not measured yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<i64>,
    /// The size is a lower bound (a limit was reached or a folder could not be read).
    pub partial: bool,
    /// `manifest` (Steam's own figure) or `scan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uninstall: Option<UninstallInfo>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageReport {
    pub entries: Vec<StorageEntry>,
    pub total_bytes: u64,
}

fn put_size(conn: &Connection, game_id: &str, path: &str, size: &Measured, at: i64) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO install_sizes(gameId,path,bytes,measuredAt,partial,source) VALUES(?,?,?,?,?,?)",
        params![game_id, path, size.bytes as i64, at, size.partial, size.source],
    )?;
    Ok(())
}

type CachedRow = (String, i64, i64, bool, String);

/// Cached sizes joined with the library. Never touches the disk.
pub fn report(conn: &Connection) -> Result<StorageReport> {
    let mut cached = conn
        .prepare("SELECT path,bytes,measuredAt,partial,source FROM install_sizes WHERE gameId=?")?;
    let mut entries = Vec::new();
    for game in db::all_games(conn)?.iter().filter(|g| is_installed(g)) {
        let paths = install_paths(game);
        let rows: Vec<CachedRow> = cached
            .query_map([&game.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut bytes = 0u64;
        let (mut found, mut partial, mut measured_at) = (0, false, 0i64);
        let mut source = None;
        for (_, path) in &paths {
            if let Some(row) = rows.iter().find(|r| r.0.eq_ignore_ascii_case(path)) {
                found += 1;
                bytes = bytes.saturating_add(row.1.max(0) as u64);
                partial |= row.3;
                measured_at = measured_at.max(row.2);
                source = Some(if row.4 == "manifest" {
                    "manifest"
                } else {
                    "scan"
                });
            }
        }
        // Some folders measured, others not: what is shown is a lower bound.
        let known = found > 0;
        partial |= known && found < paths.len();
        entries.push(StorageEntry {
            game_id: game.id.clone(),
            title: game.title.clone(),
            store: game.store.clone(),
            last_played: game.last_played,
            bytes: known.then_some(bytes),
            install_path: paths.first().map(|(_, p)| p.clone()),
            measured_at: known.then_some(measured_at),
            partial,
            source: if found > 1 { Some("scan") } else { source },
            uninstall: uninstall_info(game),
        });
    }
    let total_bytes = entries.iter().filter_map(|e| e.bytes).sum();
    Ok(StorageReport {
        entries,
        total_bytes,
    })
}

// ---------------------------------------------------------------------------
// Uninstall hand-off

/// The only links `storage_uninstall` ever opens. `<id>` is filled with a validated numeric id.
pub const URI_STEAM_UNINSTALL: &str = "steam://uninstall/<id>";
pub const URI_EPIC_LIBRARY: &str = "com.epicgames.launcher://store/";
pub const URI_GOG_GAME: &str = "goggalaxy://openGameView/<id>";
pub const URI_APPS: &str = "ms-settings:appsfeatures";

/// What opening a route's uninstall hand-off means: the link to open and how to describe it.
#[derive(Debug, PartialEq)]
pub struct Handoff {
    pub uri: String,
    pub launcher: &'static str,
    pub kind: &'static str,
}

/// Builds the hand-off from a STORED route. Nothing from the renderer is used except which
/// route; the ids come from the route and must be plain digits, so no path, query or extra
/// segment can be smuggled in. ROM routes have no hand-off.
pub fn handoff(route: &GameRoute) -> Result<Handoff> {
    let id = |pattern: &Regex| -> Result<&str> {
        route
            .app_id
            .as_deref()
            .filter(|id| pattern.is_match(id))
            .ok_or_else(|| anyhow!("This game's launcher id is not valid."))
    };
    match route.source_id.as_str() {
        "steam" => Ok(Handoff {
            uri: URI_STEAM_UNINSTALL.replace("<id>", id(&STEAM_APP_ID)?),
            launcher: "Steam",
            kind: "uninstall",
        }),
        "epic" => Ok(Handoff {
            uri: URI_EPIC_LIBRARY.to_owned(),
            launcher: "Epic Games Launcher",
            kind: "library",
        }),
        "gog-galaxy" => Ok(Handoff {
            uri: URI_GOG_GAME.replace("<id>", id(&GOG_ID)?),
            launcher: "GOG Galaxy",
            kind: "library",
        }),
        "rom" => bail!("Games played in an emulator are not installed by a launcher."),
        _ => Ok(Handoff {
            uri: URI_APPS.to_owned(),
            launcher: "Windows Installed apps",
            kind: "apps",
        }),
    }
}

fn uninstall_info(game: &Game) -> Option<UninstallInfo> {
    game.routes.iter().enumerate().find_map(|(index, route)| {
        if route.installed != Some(true) {
            return None;
        }
        let h = handoff(route).ok()?;
        Some(UninstallInfo {
            route_index: index,
            launcher: h.launcher.to_owned(),
            kind: h.kind,
        })
    })
}

/// The native confirmation text: which game, which launcher, and that nothing is deleted here.
pub fn handoff_prompt(title: &str, h: &Handoff) -> String {
    let what = match h.kind {
        "uninstall" => format!("Open {}'s uninstall dialog for {title}?", h.launcher),
        "library" => format!(
            "Open {} so you can uninstall {title} from its library?",
            h.launcher
        ),
        _ => format!("Open {} so you can uninstall {title}?", h.launcher),
    };
    format!("{what} Game Lobby does not delete anything itself.")
}

/// Opens the launcher's uninstall screen after a native confirmation naming the game and the
/// launcher; the renderer only names the game and route.
#[tauri::command]
pub async fn storage_uninstall(
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
    route_index: i64,
) -> CmdResult<()> {
    if !(0..128).contains(&route_index) || game_id.len() > 512 {
        return Err("Uninstall request is invalid.".into());
    }
    let game = db::get_game(&state.db.lock(), &game_id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let route = game
        .routes
        .get(route_index as usize)
        .ok_or("Game route not found.")?;
    let h = handoff(route).map_err(err)?;
    // The scheme check of launch links does not cover ms-settings; that link is fixed text.
    if h.uri != URI_APPS {
        crate::launch::validate_launch_uri(&h.uri).map_err(err)?;
    }
    let prompt = handoff_prompt(&game.title, &h);
    if !crate::local_commands::confirm_natively(&window, "Uninstall game", &prompt, "Open").await {
        return Ok(());
    }
    platform::shell_open(h.uri).await.map_err(err)
}

// ---------------------------------------------------------------------------
// Commands: sizes

#[tauri::command]
pub async fn storage_list(state: State<'_, AppState>) -> CmdResult<StorageReport> {
    report(&state.db.lock()).map_err(err)
}

static RESCANNING: AtomicBool = AtomicBool::new(false);

struct RescanGuard;
impl RescanGuard {
    fn take() -> Option<Self> {
        (!RESCANNING.swap(true, Ordering::SeqCst)).then_some(Self)
    }
}
impl Drop for RescanGuard {
    fn drop(&mut self) {
        RESCANNING.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    done: usize,
    total: usize,
    title: &'a str,
}

struct Job {
    game_id: String,
    title: String,
    routes: Vec<(GameRoute, String)>,
}

/// Measures every installed game's stored install folders (off the UI thread), caching each
/// result as it finishes. Emits `storage:progress` `{done,total,title}`.
#[tauri::command]
pub async fn storage_rescan(app: AppHandle) -> CmdResult<StorageReport> {
    let Some(guard) = RescanGuard::take() else {
        return Err("A size scan is already running.".into());
    };
    let jobs: Vec<Job> = {
        let state = app.state::<AppState>();
        let games = db::all_games(&state.db.lock()).map_err(err)?;
        games
            .iter()
            .filter(|g| is_installed(g))
            .map(|g| Job {
                game_id: g.id.clone(),
                title: g.title.clone(),
                routes: install_paths(g)
                    .into_iter()
                    .map(|(r, p)| (r.clone(), p))
                    .collect(),
            })
            .collect()
    };
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let state = handle.state::<AppState>();
        let started = Instant::now();
        let total = jobs.len();
        for (done, job) in jobs.iter().enumerate() {
            let _ = handle.emit(
                "storage:progress",
                Progress {
                    done,
                    total,
                    title: &job.title,
                },
            );
            if started.elapsed() > RESCAN_TIME {
                break;
            }
            for (route, path) in &job.routes {
                let result = measure_route(route, path);
                let conn = state.db.lock();
                match result {
                    Ok(size) => {
                        let _ = put_size(&conn, &job.game_id, path, &size, now_ms());
                    }
                    // A folder that is gone: forget the old figure.
                    Err(_) => {
                        let _ = conn.execute(
                            "DELETE FROM install_sizes WHERE gameId=? AND path=?",
                            params![job.game_id, path],
                        );
                    }
                }
            }
        }
        let _ = handle.emit(
            "storage:progress",
            Progress {
                done: total,
                total,
                title: "",
            },
        );
        let result = report(&state.db.lock());
        result
    })
    .await
    .map_err(err)?
    .map_err(err)
}

// ---------------------------------------------------------------------------
// Recently added

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameAddedEntry {
    pub game_id: String,
    pub added_at: i64,
    pub seen: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentGame {
    pub game_id: String,
    pub added_at: i64,
}

/// Unseen games first discovered within [`RECENT_DAYS`] of `now`, newest first.
pub fn recent(conn: &Connection, now: i64) -> Result<Vec<RecentGame>> {
    let mut stmt = conn.prepare(
        "SELECT a.gameId,a.addedAt FROM game_added a JOIN games g ON g.id=a.gameId
         WHERE a.seen=0 AND a.addedAt>=? ORDER BY a.addedAt DESC,g.title LIMIT ?",
    )?;
    let rows = stmt
        .query_map(
            params![now - RECENT_DAYS * DAY_MS, RECENT_LIMIT as i64],
            |r| {
                Ok(RecentGame {
                    game_id: r.get(0)?,
                    added_at: r.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn mark_seen(conn: &Connection, game_id: Option<&str>) -> Result<()> {
    match game_id {
        Some(id) => conn.execute("UPDATE game_added SET seen=1 WHERE gameId=?", [id])?,
        None => conn.execute("UPDATE game_added SET seen=1 WHERE seen=0", [])?,
    };
    Ok(())
}

pub fn all_added(conn: &Connection) -> Result<Vec<GameAddedEntry>> {
    let mut stmt = conn.prepare("SELECT gameId,addedAt,seen FROM game_added ORDER BY gameId")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(GameAddedEntry {
                game_id: r.get(0)?,
                added_at: r.get(1)?,
                seen: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Restores backed-up flags for games that exist. Games a backup brought in without flags
/// (older backups) are marked seen: a restore is not a discovery.
pub fn restore_added(
    conn: &Connection,
    game_ids: &[&str],
    entries: Option<&[GameAddedEntry]>,
) -> Result<()> {
    for id in game_ids {
        mark_seen(conn, Some(id))?;
    }
    for e in entries.unwrap_or_default() {
        conn.execute(
            "UPDATE game_added SET addedAt=?,seen=? WHERE gameId=?",
            params![e.added_at, e.seen, e.game_id],
        )?;
    }
    Ok(())
}

#[tauri::command]
pub async fn recent_list(state: State<'_, AppState>) -> CmdResult<Vec<RecentGame>> {
    recent(&state.db.lock(), now_ms()).map_err(err)
}

/// `game_id` absent = dismiss the whole shelf.
#[tauri::command]
pub async fn recent_dismiss(
    state: State<'_, AppState>,
    game_id: Option<String>,
) -> CmdResult<Vec<RecentGame>> {
    if game_id.as_ref().is_some_and(|id| id.len() > 512) {
        return Err("Game id is invalid.".into());
    }
    let conn = state.db.lock();
    mark_seen(&conn, game_id.as_deref()).map_err(err)?;
    recent(&conn, now_ms()).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn route(source: &str, app_id: Option<&str>) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            app_id: app_id.map(str::to_owned),
            installed: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn the_confirmation_names_the_game_and_the_launcher() {
        let steam = handoff_prompt("Hades", &handoff(&route("steam", Some("1"))).unwrap());
        assert!(
            steam.contains("Hades") && steam.contains("Steam"),
            "{steam}"
        );
        let epic = handoff_prompt("Hades", &handoff(&route("epic", Some("x"))).unwrap());
        assert!(epic.contains("Hades") && epic.contains("Epic Games Launcher"));
        let other = handoff_prompt("Hades", &handoff(&route("local", None)).unwrap());
        assert!(other.contains("Windows Installed apps"));
        assert!(other.contains("does not delete anything"));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gl-storage-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; len]).unwrap();
    }

    #[test]
    fn walking_sums_nested_files_and_reports_missing_paths() {
        let dir = temp("walk");
        write(&dir.join("a.bin"), 100);
        write(&dir.join("x/b.bin"), 250);
        write(&dir.join("x/y/z/c.bin"), 1000);
        fs::create_dir_all(dir.join("empty")).unwrap();
        let walked = walk_size(&dir, WALK_TIME, WALK_ENTRIES).unwrap();
        assert_eq!((walked.bytes, walked.complete), (1350, true));
        assert!(walk_size(&dir.join("missing"), WALK_TIME, WALK_ENTRIES).is_err());
        assert!(walk_size(&dir.join("a.bin"), WALK_TIME, WALK_ENTRIES).is_err());
        let capped = walk_size(&dir, WALK_TIME, 2).unwrap();
        assert!(!capped.complete);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn junctions_inside_an_install_folder_are_not_followed() {
        let dir = temp("junction");
        let outside = temp("junction-target");
        write(&outside.join("big.bin"), 5000);
        write(&dir.join("game.bin"), 10);
        let link = dir.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink failed");
        // A loop back to the root must not hang either.
        let _ = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(outside.join("loop"))
            .arg(&dir)
            .output();
        let walked = walk_size(&dir, WALK_TIME, WALK_ENTRIES).unwrap();
        assert_eq!(walked.bytes, 10);
        // A root that is itself a junction is followed.
        let through = walk_size(&link, WALK_TIME, WALK_ENTRIES).unwrap();
        assert_eq!(through.bytes, 5000);
        // Remove the links before the folders (rmdir does not follow a junction).
        let _ = fs::remove_dir(&link);
        let _ = fs::remove_dir(outside.join("loop"));
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn steam_size_comes_from_the_manifest() {
        let lib = temp("steam");
        let game = lib.join("steamapps/common/Game One");
        write(&game.join("g.bin"), 5);
        fs::write(
            lib.join("steamapps/appmanifest_570.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"570\"\n\t\"SizeOnDisk\"\t\t\"123456789\"\n}",
        )
        .unwrap();
        let path = game.to_string_lossy().into_owned();
        assert_eq!(steam_manifest_size(&path, "570"), Some(123_456_789));
        assert_eq!(steam_manifest_size(&path, "571"), None);
        assert_eq!(steam_manifest_size(&path, "570/../x"), None);
        // Not under a `common` folder: the manifest is not looked for.
        let other = lib.join("elsewhere/Game");
        write(&other.join("g.bin"), 5);
        assert_eq!(steam_manifest_size(&other.to_string_lossy(), "570"), None);
        let measured = measure_route(&route("steam", Some("570")), &path).unwrap();
        assert_eq!((measured.bytes, measured.source), (123_456_789, "manifest"));
        // A route without a manifest falls back to walking.
        let walked = measure_route(&route("steam", Some("1")), &path).unwrap();
        assert_eq!((walked.bytes, walked.source), (5, "scan"));
        fs::remove_dir_all(&lib).unwrap();
    }

    #[test]
    fn uninstall_links_use_fixed_templates() {
        let h = |s: &str, id: Option<&str>| handoff(&route(s, id));
        assert_eq!(
            h("steam", Some("570")).unwrap().uri,
            "steam://uninstall/570"
        );
        assert_eq!(
            h("gog-galaxy", Some("1207658924")).unwrap().uri,
            "goggalaxy://openGameView/1207658924"
        );
        assert_eq!(h("epic", Some("Fortnite")).unwrap().uri, URI_EPIC_LIBRARY);
        assert_eq!(h("epic", None).unwrap().uri, URI_EPIC_LIBRARY);
        assert_eq!(h("xbox", None).unwrap().uri, URI_APPS);
        assert_eq!(h("local", Some("C:\\Games\\x")).unwrap().uri, URI_APPS);
        assert!(h("rom", Some("c:\\roms\\a.iso")).is_err());
    }

    #[test]
    fn uninstall_ids_cannot_inject_anything() {
        for bad in [
            "570//-console",
            "570/../../x",
            "570?x=1",
            "570#a",
            "570 ",
            " 570",
            "570\n",
            "-1",
            "5.7",
            "",
            "０５７０",
            "12345678901",
            "570%2f..",
            "570@evil",
            "570:80",
        ] {
            assert!(handoff(&route("steam", Some(bad))).is_err(), "{bad:?}");
        }
        for bad in ["1/../2", "abc", "1?x", "", "123456789012345678901"] {
            assert!(handoff(&route("gog-galaxy", Some(bad))).is_err(), "{bad:?}");
        }
        assert!(handoff(&route("steam", None)).is_err());
        // Every link that is not the fixed settings page passes the launch-link check.
        for uri in [
            handoff(&route("steam", Some("1"))).unwrap().uri,
            URI_EPIC_LIBRARY.to_owned(),
            handoff(&route("gog-galaxy", Some("5"))).unwrap().uri,
        ] {
            assert!(crate::launch::validate_launch_uri(&uri).is_ok(), "{uri}");
        }
    }

    fn insert(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO games(id,title,store) VALUES(?,?, 'Steam')",
            params![id, id],
        )
        .unwrap();
    }

    #[test]
    fn existing_games_are_backfilled_as_seen_and_new_ones_are_recent() {
        let dir = temp("recent");
        let file = dir.join("library.sqlite");
        {
            // A library from before this feature: games, no game_added table or trigger.
            let conn = Connection::open(&file).unwrap();
            conn.execute_batch(
                "CREATE TABLE games(id TEXT PRIMARY KEY,title TEXT NOT NULL COLLATE NOCASE,cover TEXT,defaultCover TEXT,store TEXT NOT NULL,genre TEXT,playtimeMinutes INTEGER NOT NULL DEFAULT 0,playtimeSeconds INTEGER NOT NULL DEFAULT 0,lastPlayed INTEGER,hidden INTEGER NOT NULL DEFAULT 0,favorite INTEGER NOT NULL DEFAULT 0,routes TEXT NOT NULL DEFAULT '[]');",
            )
            .unwrap();
            insert(&conn, "old1");
            insert(&conn, "old2");
        }
        let now = {
            let db = db::Db::open(&file).unwrap();
            let conn = db.lock();
            assert!(recent(&conn, now_ms()).unwrap().is_empty());
            insert(&conn, "new1");
            let now = now_ms();
            let list = recent(&conn, now).unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].game_id, "new1");
            now
        };
        // Reopening does not make old games new, and keeps the new one.
        let db = db::Db::open(&file).unwrap();
        let conn = db.lock();
        assert_eq!(recent(&conn, now).unwrap().len(), 1);
        // Dismissed games leave the shelf.
        mark_seen(&conn, Some("new1")).unwrap();
        assert!(recent(&conn, now).unwrap().is_empty());
        drop(conn);
        drop(db);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_is_newest_first_limited_to_the_window_and_follows_deletes() {
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        for id in ["a", "b", "c", "d"] {
            insert(&conn, id);
        }
        let now = now_ms();
        for (id, age_days) in [("a", 1), ("b", 3), ("c", 20)] {
            conn.execute(
                "UPDATE game_added SET addedAt=? WHERE gameId=?",
                params![now - age_days * DAY_MS, id],
            )
            .unwrap();
        }
        let ids = |conn: &Connection| -> Vec<String> {
            recent(conn, now)
                .unwrap()
                .into_iter()
                .map(|r| r.game_id)
                .collect()
        };
        // "d" was just added; "c" is outside the window.
        assert_eq!(ids(&conn), ["d", "a", "b"]);
        mark_seen(&conn, None).unwrap();
        assert!(ids(&conn).is_empty());
        insert(&conn, "e");
        conn.execute("DELETE FROM games WHERE id='e'", []).unwrap();
        assert!(ids(&conn).is_empty());
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM game_added WHERE gameId='e'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn restore_keeps_backed_up_flags_and_does_not_flood_the_shelf() {
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        insert(&conn, "a");
        insert(&conn, "b");
        let entries = [GameAddedEntry {
            game_id: "b".into(),
            added_at: now_ms() - DAY_MS,
            seen: false,
        }];
        restore_added(&conn, &["a", "b"], Some(&entries)).unwrap();
        let ids: Vec<_> = recent(&conn, now_ms())
            .unwrap()
            .into_iter()
            .map(|r| r.game_id)
            .collect();
        assert_eq!(ids, ["b"]);
        // Old backups carry no flags: everything restored is seen.
        restore_added(&conn, &["a", "b"], None).unwrap();
        assert!(recent(&conn, now_ms()).unwrap().is_empty());
        assert_eq!(all_added(&conn).unwrap().len(), 2);
    }

    #[test]
    fn backups_carry_the_seen_flags_and_old_backups_still_import() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        insert(&conn, "a");
        insert(&conn, "b");
        mark_seen(&conn, Some("a")).unwrap();
        let backup = crate::types::Backup {
            games: db::all_games(&conn).unwrap(),
            settings: None,
            emulators: None,
            game_page_custom: None,
            game_notes: None,
            game_added: Some(all_added(&conn).unwrap()),
            play_sessions: None,
            launchers: None,
        };
        let text = serde_json::to_value(&backup).unwrap();
        assert!(text["gameAdded"].is_array());
        // A fresh library restoring it: "b" stays new, "a" stays seen.
        let fresh = db::Db::memory().unwrap();
        let mut target = fresh.lock();
        let parsed = crate::validation::backup(&text).unwrap();
        db::restore_backup(&mut target, &parsed).unwrap();
        let ids: Vec<_> = recent(&target, now_ms())
            .unwrap()
            .into_iter()
            .map(|r| r.game_id)
            .collect();
        assert_eq!(ids, ["b"]);
        // Bad entries are refused; a backup without the field restores everything as seen.
        let mut bad = text.clone();
        bad["gameAdded"] = serde_json::json!([{ "gameId": "a" }]);
        assert!(crate::validation::backup(&bad).is_err());
        let mut old = text;
        old.as_object_mut().unwrap().remove("gameAdded");
        let parsed = crate::validation::backup(&old).unwrap();
        db::restore_backup(&mut conn, &parsed).unwrap();
        assert!(recent(&conn, now_ms()).unwrap().is_empty());
    }

    #[test]
    fn report_lists_installed_games_with_cached_sizes_and_unknowns() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let game = |id: &str, installed: bool, path: Option<&str>| Game {
            id: id.into(),
            title: id.into(),
            store: "Steam".into(),
            routes: vec![GameRoute {
                source_id: "steam".into(),
                app_id: Some(id.len().to_string() + &"0".repeat(id.len())),
                install_path: path.map(str::to_owned),
                installed: Some(installed),
                ..Default::default()
            }],
            ..Default::default()
        };
        db::upsert_imported_games(
            &mut conn,
            &[
                game("sized", true, Some("C:\\g\\sized")),
                game("nopath", true, None),
                game("gone", false, Some("C:\\g\\gone")),
                game("unmeasured", true, Some("C:\\g\\unmeasured")),
            ],
        )
        .unwrap();
        put_size(
            &conn,
            "sized",
            "C:\\g\\sized",
            &Measured {
                bytes: 4096,
                partial: false,
                source: "scan",
            },
            7,
        )
        .unwrap();
        let rep = report(&conn).unwrap();
        assert_eq!(rep.entries.len(), 3);
        assert_eq!(rep.total_bytes, 4096);
        let by = |id: &str| rep.entries.iter().find(|e| e.game_id == id).unwrap();
        assert_eq!(by("sized").bytes, Some(4096));
        assert_eq!(by("sized").uninstall.as_ref().unwrap().kind, "uninstall");
        assert_eq!(by("nopath").bytes, None);
        assert_eq!(by("nopath").install_path, None);
        assert_eq!(by("unmeasured").bytes, None);
        // Removing a game drops its cached size.
        db::remove_game(&conn, "sized").unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_sizes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }
}
