//! Library health: launch routes whose files are gone, and duplicate games or routes.
//!
//! Findings are stored as `scan_review` items (see `review`) of three kinds:
//! `missing-file`, `health-duplicate` (a pair of games) and `duplicate-route` (routes of one
//! game that launch the same thing). Keys are built from what the item is about, so a check
//! refreshes open items instead of adding copies, and an item the user resolved stays
//! resolved ("Ignore", "Not duplicates").
//!
//! Security rule: nothing here lets renderer text become a route's executable, target,
//! arguments or install path. A repair uses a path that either (a) the Rust search found
//! itself under a library folder and stored in the item (the renderer only sends the item id
//! and a candidate index, and the candidate is checked again when it is applied) or (b) the
//! user chose in a native picker opened by Rust, after [`check_replacement`]. Nothing here
//! deletes a file on disk; the worst a repair does is remove a launch route, which the
//! interface confirms first.

use crate::db::{self, Db};
use crate::local::{self, LOCAL_SOURCE, ROM_SOURCE};
use crate::localscan::{key_of, path_key};
use crate::review::{self, ReviewItem};
use crate::roots::{self, LibraryRoot};
use crate::tracker::is_within;
use crate::types::{Game, GameRoute};
use anyhow::{anyhow, bail, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MISSING_FILE: &str = "missing-file";
pub const HEALTH_DUPLICATE: &str = "health-duplicate";
pub const DUPLICATE_ROUTE: &str = "duplicate-route";
pub const KINDS: [&str; 3] = [MISSING_FILE, HEALTH_DUPLICATE, DUPLICATE_ROUTE];

/// Most "use this location" suggestions kept per route.
const MAX_CANDIDATES: usize = 5;
/// Limits of the search under the library folders (a check must stay cheap).
const SEARCH_MAX_ENTRIES: usize = 400_000;
const SEARCH_MAX_DEPTH: usize = 12;
const SEARCH_BUDGET: Duration = Duration::from_secs(20);

/// Set while a check runs; a second request is refused instead of repeating the work.
pub static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Serialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthSummary {
    pub missing: usize,
    pub offline: usize,
    pub with_suggestions: usize,
    pub duplicate_games: usize,
    pub duplicate_routes: usize,
}

// ---------------------------------------------------------------------------
// Presence

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    /// The volume is there but the path isn't.
    Missing,
    /// The path's drive (or network share) isn't connected: not a missing file.
    DriveNotConnected,
}

/// `C:\` or `\\server\share\` of an absolute Windows path; `/` elsewhere.
pub fn volume_root(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut root = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            Component::RootDir => {
                root.push(component.as_os_str());
                return Some(root);
            }
            _ => return None,
        }
    }
    None
}

/// Whether `path` exists, taking 8.3 short names into account and telling a missing file
/// from an offline drive.
pub fn presence(path: &Path) -> Presence {
    let long = crate::platform::long_path(path);
    if long.exists() {
        return Presence::Present;
    }
    match volume_root(&long) {
        Some(root) if !root.exists() => Presence::DriveNotConnected,
        _ => Presence::Missing,
    }
}

// ---------------------------------------------------------------------------
// Missing files

/// The file or folder a route needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    pub path: String,
    /// A folder (the install path of a route that launches through a link), not a file.
    pub folder: bool,
}

/// What a ROM or local route launches: the target when it is a path, otherwise the install
/// path. Store routes and routes not marked installed are not checked here (scans and the
/// stores report those themselves).
pub fn subject_of(route: &GameRoute) -> Option<Subject> {
    if !(route.source_id == ROM_SOURCE || route.source_id == LOCAL_SOURCE)
        || route.installed != Some(true)
    {
        return None;
    }
    let target = route.launch_target.trim();
    if !target.contains("://") && Path::new(target).is_absolute() {
        return Some(Subject {
            path: target.to_string(),
            folder: Path::new(target).extension().is_none(),
        });
    }
    let install = route.install_path.as_deref().map(str::trim)?;
    (Path::new(install).is_absolute()).then(|| Subject {
        path: install.to_string(),
        folder: true,
    })
}

#[derive(Debug, Clone)]
pub struct MissingRoute {
    pub game_id: String,
    pub title: String,
    pub route_id: String,
    pub source_id: String,
    pub system: Option<String>,
    pub subject: Subject,
    pub offline: bool,
}

impl MissingRoute {
    fn key(&self) -> String {
        let path = path_key(&self.subject.path).unwrap_or_else(|| self.subject.path.to_lowercase());
        format!("{MISSING_FILE}:{}:{path}", self.game_id)
    }

    fn file_name(&self) -> String {
        Path::new(&self.subject.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }
}

/// Routes whose file or folder isn't there. Touches the disk: call without the database lock.
pub fn find_missing(games: &[Game]) -> Vec<MissingRoute> {
    let mut out = Vec::new();
    for game in games {
        for route in &game.routes {
            let Some(subject) = subject_of(route) else {
                continue;
            };
            let offline = match presence(Path::new(&subject.path)) {
                Presence::Present => continue,
                Presence::Missing => false,
                Presence::DriveNotConnected => true,
            };
            out.push(MissingRoute {
                game_id: game.id.clone(),
                title: game.title.clone(),
                route_id: db::route_identity(route),
                source_id: route.source_id.clone(),
                system: route.system.clone(),
                subject,
                offline,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Moved files

/// Looks under the online, enabled game folders for files or folders with the names of the
/// missing ones (case-insensitive). Returns, per lower-cased name, the matches (best
/// first). Bounded in entries, depth and time; links are not followed.
fn search_roots(
    roots: &[LibraryRoot],
    wanted: &HashMap<String, bool>,
    budget: Duration,
) -> HashMap<String, Vec<PathBuf>> {
    let mut found: HashMap<String, Vec<PathBuf>> = HashMap::new();
    if wanted.is_empty() {
        return found;
    }
    let started = Instant::now();
    let mut visited = 0usize;
    for root in roots.iter().filter(|r| r.enabled && r.kind == "games") {
        let base = crate::platform::long_path(Path::new(&root.path));
        let mut stack = vec![(base, 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > SEARCH_MAX_ENTRIES || started.elapsed() > budget {
                    return found;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                // Links, junctions and other reparse points are never followed or reported.
                if crate::storage::is_reparse(&meta) {
                    continue;
                }
                let kind = meta.file_type();
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if let Some(&want_folder) = wanted.get(&name) {
                    if want_folder == kind.is_dir() {
                        found.entry(name).or_default().push(entry.path());
                    }
                }
                if kind.is_dir() && depth < SEARCH_MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            }
        }
    }
    found
}

/// Suggested new locations per missing route key. Offline routes get none.
pub fn find_moved(missing: &[MissingRoute], roots: &[LibraryRoot]) -> HashMap<String, Vec<String>> {
    find_moved_within(missing, roots, SEARCH_BUDGET)
}

fn find_moved_within(
    missing: &[MissingRoute],
    roots: &[LibraryRoot],
    budget: Duration,
) -> HashMap<String, Vec<String>> {
    let wanted: HashMap<String, bool> = missing
        .iter()
        .filter(|m| !m.offline)
        .map(|m| (m.file_name(), m.subject.folder))
        .filter(|(name, _)| !name.is_empty())
        .collect();
    let hits = search_roots(roots, &wanted, budget);
    let mut out = HashMap::new();
    for m in missing.iter().filter(|m| !m.offline) {
        let Some(paths) = hits.get(&m.file_name()) else {
            continue;
        };
        let old_key = path_key(&m.subject.path);
        let old_parent = Path::new(&m.subject.path)
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().to_lowercase());
        let mut paths: Vec<&PathBuf> = paths
            .iter()
            .filter(|p| path_key(&p.to_string_lossy()) != old_key)
            .collect();
        // Same containing folder name first, then shorter paths.
        paths.sort_by_key(|p| {
            let parent = p
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().to_lowercase());
            (parent != old_parent, p.to_string_lossy().len())
        });
        let list: Vec<String> = paths
            .into_iter()
            .take(MAX_CANDIDATES)
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if !list.is_empty() {
            out.insert(m.key(), list);
        }
    }
    out
}

/// A file the user picked may replace the original only with the same extension (an `.exe`
/// stays an `.exe`, a `.sfc` ROM a `.sfc`); a folder stays a folder. It must exist.
pub fn check_replacement(original: &Subject, picked: &Path) -> Result<()> {
    if !picked.is_absolute() || crate::tracker::normalize_path(&picked.to_string_lossy()).is_none()
    {
        bail!("Choose a file on a local drive.");
    }
    if original.folder {
        if !picked.is_dir() {
            bail!("Choose a folder.");
        }
        return Ok(());
    }
    if !picked.is_file() {
        bail!("That file doesn't exist.");
    }
    let ext = |p: &Path| {
        p.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let (want, got) = (ext(Path::new(&original.path)), ext(picked));
    if want != got {
        bail!(
            "That is a different kind of file (.{got}); this route needs a .{want} file.",
            want = if want.is_empty() { "?" } else { &want }
        );
    }
    Ok(())
}

/// The route as it is after its file moved to `new`. `fresh` is the route Game Lobby builds for
/// the new file when it is a shortcut (`local::game_for_file`).
fn relocated(
    old: &GameRoute,
    subject: &Subject,
    new: &Path,
    fresh: Option<&GameRoute>,
    roots: &[LibraryRoot],
) -> GameRoute {
    let mut route = old.clone();
    let new_text = new.to_string_lossy().into_owned();
    if let Some(fresh) = fresh {
        route.launch_target = fresh.launch_target.clone();
        route.launch_args = fresh.launch_args.clone();
        route.app_id = fresh.app_id.clone();
        route.install_path = fresh.install_path.clone();
    } else if subject.folder {
        if old.app_id.as_deref() == Some(key_of(Path::new(&subject.path)).as_str()) {
            route.app_id = Some(key_of(new));
        }
        // An absolute launch target without an extension counts as a folder (`subject_of`);
        // it moves with the folder, or the route would still launch the missing path.
        if Path::new(old.launch_target.trim()) == Path::new(&subject.path) {
            route.launch_target = new_text.clone();
        }
        route.install_path = Some(new_text);
    } else {
        let old_path = Path::new(&subject.path);
        // The install path keeps its place relative to the file when the new location has
        // the same shape (`Game\bin\game.exe` → `Moved\Game\bin\game.exe`).
        let install = old
            .install_path
            .as_deref()
            .map(Path::new)
            .and_then(|install| old_path.strip_prefix(install).ok())
            .and_then(|rel| {
                let depth = rel.components().count();
                let mut up = new;
                for _ in 0..depth {
                    up = up.parent()?;
                }
                (new.strip_prefix(up).ok()? == rel).then(|| up.to_path_buf())
            })
            .or_else(|| new.parent().map(Path::to_path_buf));
        let old_install_key = old
            .install_path
            .as_deref()
            .map(|p| key_of(Path::new(p)))
            .unwrap_or_default();
        if old.app_id.as_deref() == Some(key_of(old_path).as_str()) {
            route.app_id = Some(key_of(new));
        } else if old.app_id.as_deref() == Some(old_install_key.as_str()) {
            route.app_id = install.as_deref().map(key_of);
        }
        route.launch_target = new_text;
        route.install_path = install.map(|p| p.to_string_lossy().into_owned());
    }
    route.installed = Some(true);
    // The root the file belonged to no longer vouches for it; the root it lies in now does.
    let new_key = key_of(new);
    let states: BTreeMap<String, bool> = roots
        .iter()
        .filter(|r| r.enabled && is_within(&roots::root_key(r), &new_key))
        .map(|r| (db::root_state_key(&r.id), true))
        .collect();
    route.install_states = (!states.is_empty()).then_some(states);
    route
}

// ---------------------------------------------------------------------------
// Duplicates

fn game_systems(game: &Game) -> BTreeSet<String> {
    game.routes
        .iter()
        .map(|r| r.system.clone().unwrap_or_else(|| "pc".into()))
        .collect()
}

/// Systems for which the scan's own `duplicate` review already compares these two games.
fn scan_covers(a: &Game, b: &Game) -> bool {
    let local_systems = |g: &Game| -> BTreeSet<String> {
        g.routes
            .iter()
            .filter(|r| {
                (r.source_id == ROM_SOURCE || r.source_id == LOCAL_SOURCE)
                    && r.installed == Some(true)
            })
            .filter_map(|r| r.system.clone())
            .collect()
    };
    !local_systems(a).is_disjoint(&local_systems(b))
}

fn is_path_route(route: &GameRoute) -> bool {
    (route.source_id == ROM_SOURCE || route.source_id == LOCAL_SOURCE)
        && Path::new(route.launch_target.trim()).is_absolute()
        && !route.launch_target.contains("://")
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicatePair {
    pub a: String,
    pub b: String,
    /// `title`, `store-id` or `same-file`.
    pub reasons: BTreeSet<&'static str>,
}

impl DuplicatePair {
    fn key(&self) -> String {
        format!("{HEALTH_DUPLICATE}:{},{}", self.a, self.b)
    }
}

/// Pairs of games that look like one game: the same cleaned title on a shared system, the
/// same app id in the same store, or routes that launch the same file. Two ROM routes with
/// different files are different versions, never a match by themselves.
pub fn find_duplicate_games(games: &[Game]) -> Vec<DuplicatePair> {
    let by_id: HashMap<&str, &Game> = games.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut titles: HashMap<String, BTreeSet<&str>> = HashMap::new();
    let mut app_ids: HashMap<String, BTreeSet<&str>> = HashMap::new();
    let mut files: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for game in games {
        let title = crate::catalog::title_key(&game.title);
        if !title.is_empty() {
            titles.entry(title).or_default().insert(&game.id);
        }
        for route in &game.routes {
            if is_path_route(route) {
                if let Some(key) = path_key(&route.launch_target) {
                    files.entry(key).or_default().insert(&game.id);
                }
            } else if let Some(app) = route.app_id.as_deref().filter(|a| !a.is_empty()) {
                if route.source_id != ROM_SOURCE && route.source_id != LOCAL_SOURCE {
                    app_ids
                        .entry(format!(
                            "{}:{}",
                            route.store.to_lowercase(),
                            app.to_lowercase()
                        ))
                        .or_default()
                        .insert(&game.id);
                }
            }
        }
    }
    let mut pairs: BTreeMap<(String, String), BTreeSet<&'static str>> = BTreeMap::new();
    let mut add = |ids: &BTreeSet<&str>, reason: &'static str| {
        let ids: Vec<&&str> = ids.iter().collect();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                pairs
                    .entry((a.to_string(), b.to_string()))
                    .or_default()
                    .insert(reason);
            }
        }
    };
    for ids in titles.values().filter(|ids| ids.len() > 1) {
        // Title matches count only between games on a shared system (a Steam game and a
        // ROM of the same name are different), and only where the scan's own duplicate
        // review isn't already asking.
        let ids: BTreeSet<&str> = ids.iter().copied().collect();
        let list: Vec<&str> = ids.iter().copied().collect();
        for (i, a) in list.iter().enumerate() {
            for b in &list[i + 1..] {
                let (ga, gb) = (by_id[a], by_id[b]);
                if !game_systems(ga).is_disjoint(&game_systems(gb)) && !scan_covers(ga, gb) {
                    let mut pair = BTreeSet::new();
                    pair.insert(*a);
                    pair.insert(*b);
                    add(&pair, "title");
                }
            }
        }
    }
    for ids in app_ids.values().filter(|ids| ids.len() > 1) {
        add(ids, "store-id");
    }
    for ids in files.values().filter(|ids| ids.len() > 1) {
        add(ids, "same-file");
    }
    pairs
        .into_iter()
        .map(|((a, b), reasons)| DuplicatePair { a, b, reasons })
        .collect()
}

/// What makes two routes of one game "the same": the same file for ROM and local routes (and
/// the same arguments), the same store id otherwise. Different ROM files never match.
fn route_target_key(route: &GameRoute) -> String {
    let args = route
        .launch_args
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let target = if is_path_route(route) {
        path_key(&route.launch_target).unwrap_or_else(|| route.launch_target.to_lowercase())
    } else if let Some(app) = route.app_id.as_deref().filter(|a| !a.is_empty()) {
        format!("{}:{}", route.store.to_lowercase(), app.to_lowercase())
    } else {
        route.launch_target.trim().to_lowercase()
    };
    format!("{}|{target}|{args}", route.source_id.to_lowercase())
}

/// Groups of routes (by index) within one game that launch the same thing: (key, indexes).
pub fn find_duplicate_routes(game: &Game) -> Vec<(String, Vec<usize>)> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, route) in game.routes.iter().enumerate() {
        groups.entry(route_target_key(route)).or_default().push(i);
    }
    groups.into_iter().filter(|(_, v)| v.len() > 1).collect()
}

// ---------------------------------------------------------------------------
// Refresh

fn game_summary(game: &Game) -> Value {
    json!({
        "gameId": game.id,
        "title": game.title,
        "store": game.store,
        "playtimeMinutes": game.playtime_seconds.unwrap_or(game.playtime_minutes * 60) / 60,
        "paths": game.routes.iter().filter(|r| is_path_route(r)).map(|r| r.launch_target.clone()).collect::<Vec<_>>(),
    })
}

fn previous_candidates(conn: &Connection) -> Result<HashMap<String, Vec<String>>> {
    Ok(review::list(conn, false)?
        .into_iter()
        .filter(|i| i.kind == MISSING_FILE)
        .map(|i| {
            let list = i
                .details
                .get("candidates")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            (item_key(conn, &i.id).unwrap_or_default(), list)
        })
        .collect())
}

fn item_key(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT key FROM scan_review WHERE id=?", [id], |r| r.get(0))
        .ok()
}

/// Checks the library and brings the health items up to date: open items that no longer
/// apply are removed, new ones added, resolved ones left alone. `search` also looks for moved
/// files under the library folders (on demand; the quick check after a scan keeps earlier
/// suggestions). Reads the disk, so no database lock is held while it does.
pub fn refresh(db: &Db, search: bool) -> Result<HealthSummary> {
    let (games, roots, previous) = {
        let conn = db.lock();
        (
            db::all_games(&conn)?,
            roots::list(&conn)?,
            previous_candidates(&conn)?,
        )
    };
    let roots = roots::with_online(roots);
    let missing = find_missing(&games);
    let suggestions = if search {
        find_moved(&missing, &roots)
    } else {
        HashMap::new()
    };
    let pairs = find_duplicate_games(&games);
    let by_id: HashMap<&str, &Game> = games.iter().map(|g| (g.id.as_str(), g)).collect();

    let conn = db.lock();
    let mut keep = HashSet::new();
    let mut summary = HealthSummary::default();
    for m in &missing {
        let key = m.key();
        let candidates = if search {
            suggestions.get(&key).cloned().unwrap_or_default()
        } else {
            previous.get(&key).cloned().unwrap_or_default()
        };
        if m.offline {
            summary.offline += 1;
        } else {
            summary.missing += 1;
        }
        if !candidates.is_empty() {
            summary.with_suggestions += 1;
        }
        let details = json!({
            "gameId": m.game_id,
            "title": m.title,
            "routeId": m.route_id,
            "sourceId": m.source_id,
            "system": m.system,
            "path": m.subject.path,
            "folder": m.subject.folder,
            "status": if m.offline { "offline" } else { "missing" },
            "drive": volume_root(Path::new(&m.subject.path)).map(|p| p.to_string_lossy().into_owned()),
            "candidates": candidates,
        });
        review::upsert(&conn, MISSING_FILE, &key, None, &details)?;
        keep.insert(key);
    }
    for pair in &pairs {
        let (Some(a), Some(b)) = (by_id.get(pair.a.as_str()), by_id.get(pair.b.as_str())) else {
            continue;
        };
        let key = pair.key();
        let details = json!({
            "reasons": pair.reasons,
            "games": [game_summary(a), game_summary(b)],
        });
        review::upsert(&conn, HEALTH_DUPLICATE, &key, None, &details)?;
        keep.insert(key);
        summary.duplicate_games += 1;
    }
    for game in &games {
        for (target, indexes) in find_duplicate_routes(game) {
            let key = format!("{DUPLICATE_ROUTE}:{}:{target}", game.id);
            let details = json!({
                "gameId": game.id,
                "title": game.title,
                "routes": indexes.iter().map(|&i| {
                    let r = &game.routes[i];
                    json!({"store": r.store, "target": r.launch_target, "installed": r.installed})
                }).collect::<Vec<_>>(),
            });
            review::upsert(&conn, DUPLICATE_ROUTE, &key, None, &details)?;
            keep.insert(key);
            summary.duplicate_routes += 1;
        }
    }
    review::prune(&conn, None, &KINDS, &keep)?;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Repairs

fn open_item(conn: &Connection, id: &str, kind: &str) -> Result<ReviewItem> {
    let item =
        review::get(conn, id)?.ok_or_else(|| anyhow!("That review item no longer exists."))?;
    if item.status != "open" {
        bail!("That review item was already resolved.");
    }
    if item.kind != kind {
        bail!("That action doesn't apply to this item.");
    }
    Ok(item)
}

fn text(item: &ReviewItem, key: &str) -> Result<String> {
    item.details
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow!("This item is incomplete. Check the library again."))
}

/// What an item needs fixing, read back from the item (written by [`refresh`], not by the
/// renderer).
struct Target {
    game_id: String,
    route_id: String,
    subject: Subject,
    candidates: Vec<String>,
}

fn target_of(item: &ReviewItem) -> Result<Target> {
    Ok(Target {
        game_id: text(item, "gameId")?,
        route_id: text(item, "routeId")?,
        subject: Subject {
            path: text(item, "path")?,
            folder: item
                .details
                .get("folder")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        candidates: item
            .details
            .get("candidates")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Reads the item and what is needed to repair it, without the database lock afterwards.
pub fn repair_context(db: &Db, id: &str) -> Result<RepairContext> {
    let conn = db.lock();
    let item = open_item(&conn, id, MISSING_FILE)?;
    if item.details.get("status").and_then(Value::as_str) == Some("offline") {
        bail!("That drive isn't connected. Connect it, or choose Ignore or Remove this route.");
    }
    Ok(RepairContext {
        target: target_of(&item)?,
        roots: roots::with_online(roots::list(&conn)?),
        confirmed: db::shortcut_confirmations(&conn)?,
    })
}

pub struct RepairContext {
    target: Target,
    roots: Vec<LibraryRoot>,
    confirmed: HashMap<String, (String, String)>,
}

impl RepairContext {
    pub fn subject(&self) -> &Subject {
        &self.target.subject
    }
}

impl Subject {
    /// The file extension the replacement needs (no dot, lower case).
    pub fn extension(&self) -> String {
        Path::new(&self.path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }
}

/// Points the route at `new` (a path that was found by the search or picked by the user),
/// shortcuts included. Reads the disk (and COM for shortcuts): no lock is held.
pub fn prepare_relocation(ctx: &RepairContext, new: &Path) -> Result<Option<GameRoute>> {
    check_replacement(&ctx.target.subject, new)?;
    let ext = new
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ctx.target.subject.folder || (ext != "lnk" && ext != "url") {
        return Ok(None);
    }
    // A shortcut follows the shortcut rules (store links and plain programs only; arguments
    // need their own confirmation under Review).
    match local::game_for_file(new, &ctx.confirmed)? {
        local::LocalFile::Game(game) => Ok(game.routes.into_iter().next()),
        local::LocalFile::NeedsConfirmation(pending) => bail!(
            "That shortcut runs {} with the arguments \"{}\". Add it with \"Add local game\" to confirm it first.",
            pending.target,
            pending.arguments
        ),
    }
}

/// Applies a relocation prepared by [`prepare_relocation`] and resolves the item.
pub fn apply_relocation(
    db: &Db,
    id: &str,
    ctx: &RepairContext,
    new: &Path,
    fresh: Option<GameRoute>,
) -> Result<()> {
    let conn = db.lock();
    let item = open_item(&conn, id, MISSING_FILE)?;
    let target = target_of(&item)?;
    if target.route_id != ctx.target.route_id || target.game_id != ctx.target.game_id {
        bail!("That item changed. Check the library again.");
    }
    let game = db::get_game(&conn, &target.game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let mut routes = game.routes.clone();
    let index = routes
        .iter()
        .position(|r| db::route_identity(r) == target.route_id)
        .ok_or_else(|| anyhow!("That launch route no longer exists."))?;
    routes[index] = relocated(
        &routes[index],
        &target.subject,
        new,
        fresh.as_ref(),
        &ctx.roots,
    );
    db::set_routes(&conn, &target.game_id, routes)?;
    review::resolve(&conn, id, "repaired")
}

/// The candidate `index` of the item, if it is still acceptable: it lies under an enabled
/// game folder, exists and keeps the original's name.
pub fn suggested_path(ctx: &RepairContext, index: usize) -> Result<PathBuf> {
    let candidate = ctx.target.candidates.get(index).ok_or_else(|| {
        anyhow!("That suggestion is no longer available. Check the library again.")
    })?;
    let key = path_key(candidate).ok_or_else(|| anyhow!("That suggestion isn't usable."))?;
    if !ctx
        .roots
        .iter()
        .any(|r| r.enabled && r.kind == "games" && is_within(&roots::root_key(r), &key))
    {
        bail!("That location is no longer inside a library folder.");
    }
    let same_name = Path::new(candidate)
        .file_name()
        .map(|n| n.to_ascii_lowercase())
        == Path::new(&ctx.target.subject.path)
            .file_name()
            .map(|n| n.to_ascii_lowercase());
    if !same_name {
        bail!("That suggestion doesn't match the missing file.");
    }
    Ok(PathBuf::from(candidate))
}

/// Fails unless `path` is still one of the item's candidates, checked against a context read
/// after the user confirmed (the item may have been refreshed while the dialog was open).
pub fn ensure_still_suggested(ctx: &RepairContext, path: &Path) -> Result<()> {
    let key = path_key(&path.to_string_lossy());
    if key.is_some() && ctx.target.candidates.iter().any(|c| path_key(c) == key) {
        return Ok(());
    }
    bail!("That suggestion is no longer available. Check the library again.")
}

/// Resolves a missing-file item without touching the route ("Ignore"). It is remembered:
/// the same missing path isn't reported again.
pub fn ignore_missing(db: &Db, id: &str) -> Result<()> {
    let conn = db.lock();
    open_item(&conn, id, MISSING_FILE)?;
    review::resolve(&conn, id, "ignored")
}

/// Removes the launch route (never a file, and never a game's last route).
pub fn remove_route(db: &Db, id: &str) -> Result<()> {
    let conn = db.lock();
    let item = open_item(&conn, id, MISSING_FILE)?;
    let target = target_of(&item)?;
    let game = db::get_game(&conn, &target.game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let index = game
        .routes
        .iter()
        .position(|r| db::route_identity(r) == target.route_id)
        .ok_or_else(|| anyhow!("That launch route no longer exists."))?;
    if game.routes.len() < 2 {
        bail!("This is the game's only launch route. Remove the game from its page instead, or choose Ignore.");
    }
    let mut routes = game.routes;
    routes.remove(index);
    db::set_routes(&conn, &target.game_id, routes)?;
    review::resolve(&conn, id, "route-removed")
}

/// "Not duplicates" (games) or "Keep both" (routes): remembered, never asked again.
pub fn keep_separate(db: &Db, id: &str) -> Result<()> {
    let conn = db.lock();
    let item =
        review::get(&conn, id)?.ok_or_else(|| anyhow!("That review item no longer exists."))?;
    if item.status != "open" || (item.kind != HEALTH_DUPLICATE && item.kind != DUPLICATE_ROUTE) {
        bail!("That action doesn't apply to this item.");
    }
    review::resolve(&conn, id, "kept-separate")
}

/// Merges the second game of a duplicate pair into the first, or the first into the second
/// when `target` names it, with `db::merge_games` (playtime summed, sessions moved). Returns
/// the (target, source) pair for the play tracker.
pub fn merge_pair(db: &Db, id: &str, target: Option<&str>) -> Result<(String, String)> {
    let mut conn = db.lock();
    let item = open_item(&conn, id, HEALTH_DUPLICATE)?;
    let ids: Vec<String> = item
        .details
        .get("games")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| g.get("gameId").and_then(Value::as_str).map(String::from))
        .collect();
    let [first, second] = ids.as_slice() else {
        bail!("This item doesn't list two games.");
    };
    let target = target.unwrap_or(first);
    let source = if target == first {
        second
    } else if target == second {
        first
    } else {
        bail!("Choose one of the listed games to keep.");
    };
    db::merge_games(&mut conn, target, source)?;
    review::resolve(&conn, id, "linked")?;
    Ok((target.to_string(), source.clone()))
}

/// Merges the routes of one game that launch the same thing into one.
pub fn merge_routes(db: &Db, id: &str) -> Result<()> {
    let conn = db.lock();
    let item = open_item(&conn, id, DUPLICATE_ROUTE)?;
    let game_id = text(&item, "gameId")?;
    let game = db::get_game(&conn, &game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let groups = find_duplicate_routes(&game);
    if groups.is_empty() {
        bail!("These routes are no longer duplicates.");
    }
    let mut drop = HashSet::new();
    let mut routes = game.routes.clone();
    for (_, indexes) in groups {
        // The installed one (else the first) stays; the others add what they know to it.
        let keep = indexes
            .iter()
            .copied()
            .find(|&i| game.routes[i].installed == Some(true))
            .unwrap_or(indexes[0]);
        for &i in indexes.iter().filter(|&&i| i != keep) {
            routes[keep] = db::merge_route(&game.routes[i], &routes[keep]);
            drop.insert(i);
        }
    }
    let routes: Vec<GameRoute> = routes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop.contains(i))
        .map(|(_, r)| r)
        .collect();
    db::set_routes(&conn, &game_id, routes)?;
    review::resolve(&conn, id, "merged-routes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;
    use crate::roots::RootGuard;

    fn rom(path: &Path, system: &str) -> GameRoute {
        GameRoute {
            source_id: ROM_SOURCE.into(),
            store: "Emulator".into(),
            app_id: Some(key_of(path)),
            launch_target: path.to_string_lossy().into_owned(),
            install_path: path.parent().map(|p| p.to_string_lossy().into_owned()),
            installed: Some(true),
            system: Some(system.into()),
            ..Default::default()
        }
    }

    fn pc(path: &Path) -> GameRoute {
        GameRoute {
            source_id: LOCAL_SOURCE.into(),
            store: "Custom".into(),
            app_id: Some(key_of(path.parent().unwrap())),
            launch_target: path.to_string_lossy().into_owned(),
            install_path: path.parent().map(|p| p.to_string_lossy().into_owned()),
            installed: Some(true),
            system: Some("pc".into()),
            ..Default::default()
        }
    }

    fn game(id: &str, title: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: title.into(),
            store: "Custom".into(),
            routes,
            ..Default::default()
        }
    }

    fn store_route(store: &str, app: &str) -> GameRoute {
        GameRoute {
            source_id: store.to_lowercase(),
            store: store.into(),
            app_id: Some(app.into()),
            launch_target: format!("{}://run/{app}", store.to_lowercase()),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn save(db: &Db, games: &[Game]) {
        db::upsert_imported_games(&mut db.lock(), games).unwrap();
    }

    fn open_items(db: &Db, kind: &str) -> Vec<ReviewItem> {
        review::list(&db.lock(), false)
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == kind)
            .collect()
    }

    fn add_root(db: &Db, dir: &Path) -> LibraryRoot {
        roots::add(
            &db.lock(),
            "games",
            &dir.to_string_lossy(),
            &RootGuard::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_deleted_executable_is_reported_and_store_and_present_files_are_not() {
        let dir = TempDir::new("health-missing");
        let here = dir.file("Here\\here.exe");
        let gone = dir.path().join("Gone").join("gone.exe");
        let db = Db::memory().unwrap();
        let mut store = store_route("Steam", "10");
        store.installed = Some(false);
        save(
            &db,
            &[
                game("local:1", "Here", vec![pc(&here)]),
                game("local:2", "Gone", vec![pc(&gone)]),
                game("steam:10", "Stored", vec![store]),
            ],
        );
        let summary = refresh(&db, false).unwrap();
        assert_eq!((summary.missing, summary.offline), (1, 0));
        let items = open_items(&db, MISSING_FILE);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].details["gameId"], "local:2");
        assert_eq!(items[0].details["status"], "missing");
        // The file comes back: the open item goes away by itself.
        std::fs::create_dir_all(gone.parent().unwrap()).unwrap();
        std::fs::write(&gone, b"x").unwrap();
        refresh(&db, false).unwrap();
        assert!(open_items(&db, MISSING_FILE).is_empty());
    }

    #[test]
    fn a_missing_drive_is_not_a_missing_file() {
        let free = ('D'..='Z')
            .map(|c| format!("{c}:\\"))
            .find(|root| !Path::new(root).exists());
        let Some(root) = free else { return };
        let path = PathBuf::from(format!("{root}Games\\Foo\\foo.exe"));
        if cfg!(windows) {
            assert_eq!(presence(&path), Presence::DriveNotConnected);
            let db = Db::memory().unwrap();
            save(&db, &[game("local:1", "Foo", vec![pc(&path)])]);
            let summary = refresh(&db, true).unwrap();
            assert_eq!((summary.missing, summary.offline), (0, 1));
            assert_eq!(
                open_items(&db, MISSING_FILE)[0].details["status"],
                "offline"
            );
            // An offline route can't be repaired.
            let id = open_items(&db, MISSING_FILE)[0].id.clone();
            assert!(repair_context(&db, &id).is_err());
        }
        let dir = TempDir::new("health-present");
        assert_eq!(
            presence(&dir.path().join("nope.exe")),
            Presence::Missing,
            "an online volume with a missing file"
        );
    }

    #[test]
    fn a_moved_file_is_found_by_name_under_a_library_folder() {
        let dir = TempDir::new("health-moved");
        let library = dir.dir("Library");
        let moved = dir.file("Library\\Sorted\\Zelda\\zelda.exe");
        dir.file("Library\\Other\\readme.txt");
        let old = dir.path().join("Old").join("Zelda").join("zelda.exe");
        let db = Db::memory().unwrap();
        add_root(&db, &library);
        save(&db, &[game("local:1", "Zelda", vec![pc(&old)])]);
        let summary = refresh(&db, true).unwrap();
        assert_eq!(summary.with_suggestions, 1);
        let item = open_items(&db, MISSING_FILE).remove(0);
        let found: Vec<String> =
            serde_json::from_value(item.details["candidates"].clone()).unwrap();
        assert_eq!(found, vec![moved.to_string_lossy().into_owned()]);

        // A quick check keeps the suggestion; using it repairs the route.
        refresh(&db, false).unwrap();
        let item = open_items(&db, MISSING_FILE).remove(0);
        assert_eq!(item.details["candidates"].as_array().unwrap().len(), 1);
        let ctx = repair_context(&db, &item.id).unwrap();
        let path = suggested_path(&ctx, 0).unwrap();
        let fresh = prepare_relocation(&ctx, &path).unwrap();
        apply_relocation(&db, &item.id, &ctx, &path, fresh).unwrap();
        let route = db::get_game(&db.lock(), "local:1")
            .unwrap()
            .unwrap()
            .routes
            .remove(0);
        assert_eq!(route.launch_target, moved.to_string_lossy());
        assert_eq!(
            route.install_path.as_deref(),
            Some(moved.parent().unwrap().to_string_lossy().as_ref())
        );
        assert!(route.install_states.is_some());
        refresh(&db, false).unwrap();
        assert!(open_items(&db, MISSING_FILE).is_empty());
        // The old file is not touched (there was none) and nothing was deleted.
        assert!(moved.exists());
    }

    #[test]
    fn a_suggestion_outside_the_library_folders_is_refused() {
        let dir = TempDir::new("health-suggest");
        let library = dir.dir("Library");
        dir.file("Library\\a\\foo.exe");
        let outside = dir.file("Elsewhere\\foo.exe");
        let old = dir.path().join("Old").join("foo.exe");
        let db = Db::memory().unwrap();
        let root = add_root(&db, &library);
        save(&db, &[game("local:1", "Foo", vec![pc(&old)])]);
        refresh(&db, true).unwrap();
        let id = open_items(&db, MISSING_FILE)[0].id.clone();
        let mut ctx = repair_context(&db, &id).unwrap();
        assert!(suggested_path(&ctx, 0).is_ok());
        ctx.target.candidates = vec![outside.to_string_lossy().into_owned()];
        assert!(suggested_path(&ctx, 0).is_err());
        ctx.target.candidates = vec![dir
            .file("Library\\a\\other.exe")
            .to_string_lossy()
            .into_owned()];
        assert!(suggested_path(&ctx, 0).is_err(), "name must match");
        assert!(suggested_path(&ctx, 1).is_err());
        let listed = dir.path().join("Library").join("a").join("foo.exe");
        assert!(ensure_still_suggested(&ctx, &listed).is_err());
        ctx.target.candidates = vec![listed.to_string_lossy().into_owned()];
        assert!(ensure_still_suggested(&ctx, &listed).is_ok());
        assert!(ensure_still_suggested(&ctx, &outside).is_err());
        // A disabled root no longer vouches for its files.
        roots::set_enabled(&db.lock(), &root.id, false).unwrap();
        let ctx = repair_context(&db, &id).unwrap();
        assert!(suggested_path(&ctx, 0).is_err());
    }

    #[test]
    fn a_picked_file_must_have_the_same_extension() {
        let dir = TempDir::new("health-pick");
        let original = Subject {
            path: dir
                .path()
                .join("Old")
                .join("game.exe")
                .to_string_lossy()
                .into_owned(),
            folder: false,
        };
        let exe = dir.file("New\\Game.EXE");
        let txt = dir.file("New\\game.txt");
        let lnk = dir.file("New\\game.lnk");
        assert!(check_replacement(&original, &exe).is_ok());
        assert!(check_replacement(&original, &txt).is_err());
        assert!(check_replacement(&original, &lnk).is_err());
        assert!(check_replacement(&original, &dir.path().join("New\\none.exe")).is_err());
        assert!(check_replacement(&original, Path::new("relative.exe")).is_err());
        assert!(check_replacement(&original, &dir.dir("New\\Dir.exe")).is_err());
        // A folder subject takes a folder only.
        let folder = Subject {
            path: original.path.clone(),
            folder: true,
        };
        assert!(check_replacement(&folder, &dir.dir("New")).is_ok());
        assert!(check_replacement(&folder, &exe).is_err());

        // End to end: the wrong kind of file never reaches the route.
        let old = dir.path().join("Old").join("game.exe");
        let db = Db::memory().unwrap();
        save(&db, &[game("local:1", "Game", vec![pc(&old)])]);
        refresh(&db, false).unwrap();
        let id = open_items(&db, MISSING_FILE)[0].id.clone();
        let ctx = repair_context(&db, &id).unwrap();
        assert!(prepare_relocation(&ctx, &txt).is_err());
        let before = db::get_game(&db.lock(), "local:1").unwrap().unwrap().routes;
        assert_eq!(before[0].launch_target, old.to_string_lossy());
        assert_eq!(open_items(&db, MISSING_FILE).len(), 1);
    }

    #[test]
    fn duplicates_by_title_store_id_and_file_but_not_different_roms() {
        let dir = TempDir::new("health-dupes");
        let exe = dir.file("PC\\Hades\\hades.exe");
        let usa = dir.file("Roms\\Zelda (USA).sfc");
        let eur = dir.file("Roms\\Zelda (Europe).sfc");
        let games = vec![
            game("steam:1", "Hades", vec![store_route("Steam", "1145360")]),
            game("epic:1", "HADES™", vec![store_route("Epic", "abc")]),
            game("gog:1", "Other name", vec![store_route("Steam", "1145360")]),
            game("local:a", "Mine", vec![pc(&exe)]),
            game("local:b", "Also mine", vec![pc(&exe)]),
            game("rom:1", "Zelda (USA)", vec![rom(&usa, "snes")]),
            game("rom:2", "Zelda (Europe)", vec![rom(&eur, "snes")]),
        ];
        let pairs = find_duplicate_games(&games);
        let find = |a: &str, b: &str| pairs.iter().find(|p| p.a == a && p.b == b);
        let title = find("epic:1", "steam:1").expect("title match");
        assert!(title.reasons.contains("title"));
        let store = find("gog:1", "steam:1").expect("same store id");
        assert!(store.reasons.contains("store-id"));
        assert!(find("local:a", "local:b")
            .unwrap()
            .reasons
            .contains("same-file"));
        // Different ROM files of one title are versions (and the scan reviews those itself).
        assert!(find("rom:1", "rom:2").is_none());
        assert_eq!(pairs.len(), 3);

        // A Steam game and a ROM with the same name are not the same game.
        let cross = vec![
            game("steam:2", "Tetris", vec![store_route("Steam", "2")]),
            game("rom:3", "Tetris", vec![rom(&usa, "nes")]),
        ];
        assert!(find_duplicate_games(&cross).is_empty());
    }

    #[test]
    fn duplicate_routes_need_the_same_target_and_different_roms_are_kept() {
        let dir = TempDir::new("health-routes");
        let exe = dir.file("PC\\Hades\\hades.exe");
        let mut second = pc(&exe);
        second.app_id = Some("other-source".into());
        let usa = dir.file("Roms\\Zelda (USA).sfc");
        let eur = dir.file("Roms\\Zelda (Europe).sfc");
        let hades = game("local:1", "Hades", vec![pc(&exe), second]);
        let zelda = game("rom:1", "Zelda", vec![rom(&usa, "snes"), rom(&eur, "snes")]);
        assert_eq!(find_duplicate_routes(&hades).len(), 1);
        assert!(find_duplicate_routes(&zelda).is_empty());

        let db = Db::memory().unwrap();
        // Stored as is: the identities differ (other app id), so both routes survive.
        save(&db, &[hades.clone(), zelda]);
        refresh(&db, false).unwrap();
        let items = open_items(&db, DUPLICATE_ROUTE);
        assert_eq!(items.len(), 1);
        merge_routes(&db, &items[0].id).unwrap();
        let merged = db::get_game(&db.lock(), "local:1").unwrap().unwrap();
        assert_eq!(merged.routes.len(), 1);
        assert_eq!(merged.routes[0].launch_target, exe.to_string_lossy());
        refresh(&db, false).unwrap();
        assert!(open_items(&db, DUPLICATE_ROUTE).is_empty());
    }

    #[test]
    fn not_duplicates_is_remembered_and_merge_keeps_playtime() {
        let db = Db::memory().unwrap();
        let mut a = game("steam:1", "Hades", vec![store_route("Steam", "1")]);
        a.playtime_seconds = Some(3600);
        a.playtime_minutes = 60;
        let mut b = game("epic:1", "Hades", vec![store_route("Epic", "x")]);
        b.playtime_seconds = Some(1800);
        b.playtime_minutes = 30;
        let mut c = game("gog:1", "Hades", vec![store_route("GOG", "y")]);
        c.playtime_seconds = Some(600);
        c.playtime_minutes = 10;
        save(&db, &[a, b, c]);
        {
            let conn = db.lock();
            for (game_id, seconds) in [("steam:1", 3600), ("epic:1", 1800), ("gog:1", 600)] {
                conn.execute(
                    "INSERT INTO play_sessions(gameId,startedAt,endedAt,seconds,source) VALUES(?,?,?,?,'test')",
                    rusqlite::params![game_id, 1, 1 + seconds * 1000, seconds],
                )
                .unwrap();
            }
        }
        refresh(&db, false).unwrap();
        let items = open_items(&db, HEALTH_DUPLICATE);
        assert_eq!(items.len(), 3, "one item per pair");

        // Epic and GOG are not the same: remembered after a recheck.
        let pair = items
            .iter()
            .find(|i| {
                i.details["games"][0]["gameId"] == "epic:1"
                    && i.details["games"][1]["gameId"] == "gog:1"
            })
            .unwrap();
        keep_separate(&db, &pair.id).unwrap();
        refresh(&db, false).unwrap();
        refresh(&db, true).unwrap();
        assert_eq!(open_items(&db, HEALTH_DUPLICATE).len(), 2);

        // Merge Epic into Steam: playtime and sessions come along.
        let steam_epic = open_items(&db, HEALTH_DUPLICATE)
            .into_iter()
            .find(|i| {
                i.details["games"][0]["gameId"] == "epic:1"
                    && i.details["games"][1]["gameId"] == "steam:1"
            })
            .unwrap();
        let (target, source) = merge_pair(&db, &steam_epic.id, Some("steam:1")).unwrap();
        assert_eq!((target.as_str(), source.as_str()), ("steam:1", "epic:1"));
        let conn = db.lock();
        let merged = db::get_game(&conn, "steam:1").unwrap().unwrap();
        assert_eq!(merged.playtime_seconds, Some(5400));
        assert!(db::get_game(&conn, "epic:1").unwrap().is_none());
        let sessions: i64 = conn
            .query_row(
                "SELECT SUM(seconds) FROM play_sessions WHERE gameId='steam:1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sessions, 5400);
        assert_eq!(merged.routes.len(), 2);
    }

    #[test]
    fn removing_a_route_keeps_the_last_route_and_ignoring_is_remembered() {
        let dir = TempDir::new("health-remove");
        let here = dir.file("Here\\game.exe");
        let gone = dir.path().join("Gone").join("game.exe");
        let db = Db::memory().unwrap();
        save(
            &db,
            &[
                game("local:1", "Two routes", vec![pc(&here), pc(&gone)]),
                game(
                    "local:2",
                    "One route",
                    vec![pc(&dir.path().join("X").join("x.exe"))],
                ),
            ],
        );
        refresh(&db, false).unwrap();
        let items = open_items(&db, MISSING_FILE);
        assert_eq!(items.len(), 2);
        let two = items
            .iter()
            .find(|i| i.details["gameId"] == "local:1")
            .unwrap();
        let one = items
            .iter()
            .find(|i| i.details["gameId"] == "local:2")
            .unwrap();
        assert!(remove_route(&db, &one.id).is_err());
        remove_route(&db, &two.id).unwrap();
        let game_one = db::get_game(&db.lock(), "local:1").unwrap().unwrap();
        assert_eq!(game_one.routes.len(), 1);
        assert!(here.exists(), "no file is ever deleted");
        ignore_missing(&db, &one.id).unwrap();
        refresh(&db, true).unwrap();
        assert!(open_items(&db, MISSING_FILE).is_empty());
    }

    #[test]
    fn relocating_keeps_the_install_path_shape() {
        let dir = TempDir::new("health-shape");
        let old = dir
            .path()
            .join("Old")
            .join("Game")
            .join("bin")
            .join("game.exe");
        let new = dir.file("New\\Game\\bin\\game.exe");
        let mut route = pc(&old);
        route.install_path = Some(
            dir.path()
                .join("Old")
                .join("Game")
                .to_string_lossy()
                .into_owned(),
        );
        route.app_id = Some(key_of(&dir.path().join("Old").join("Game")));
        let subject = subject_of(&route).unwrap();
        let moved = relocated(&route, &subject, &new, None, &[]);
        let game_dir = dir.path().join("New").join("Game");
        assert_eq!(
            moved.install_path.as_deref(),
            Some(game_dir.to_string_lossy().as_ref())
        );
        assert_eq!(moved.app_id, Some(key_of(&game_dir)));
        assert_eq!(moved.launch_target, new.to_string_lossy());
    }

    #[test]
    fn relocating_a_folder_target_moves_the_launch_target_too() {
        let dir = TempDir::new("health-folder-target");
        let old = dir.path().join("Old").join("GameDir");
        let new = dir.dir("New/GameDir");
        // Launches an extensionless absolute path, so the subject is a folder.
        let mut route = pc(&old);
        route.install_path = None;
        let subject = subject_of(&route).unwrap();
        assert!(subject.folder);
        let moved = relocated(&route, &subject, &new, None, &[]);
        assert_eq!(moved.launch_target, new.to_string_lossy());
        assert_eq!(
            moved.install_path.as_deref(),
            Some(new.to_string_lossy().as_ref())
        );
        // A link-launched route (target isn't the subject) keeps its target.
        let mut link = pc(&old.join("g.exe"));
        link.launch_target = "steam://run/1".into();
        link.install_path = Some(old.to_string_lossy().into_owned());
        let subject = subject_of(&link).unwrap();
        let moved = relocated(&link, &subject, &new, None, &[]);
        assert_eq!(moved.launch_target, "steam://run/1");
    }
}
