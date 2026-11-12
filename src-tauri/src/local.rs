//! Library roots → library: runs emulator discovery and game discovery for roots, applies
//! the results with the shared upsert/reconcile rules, keeps review items, and resolves them.
//! Everything here works on a [`Db`] without Tauri, so it is tested end to end with temp
//! folders. The database is locked only while results are applied, never during a walk.
//!
//! Library integration:
//! - ROM games: store `Emulator`, route `sourceId: "rom"`, `appId` = normalized ROM path (the
//!   stable identity), `launchTarget` = the ROM path, `installPath` = its folder, `system`.
//!   Scans never set `emulatorId`: it is only ever the user's choice (so it counts as user
//!   data); without one the system's default profile is resolved at launch.
//! - PC games: store `Custom`, route `sourceId: "local"`, `appId` = normalized game folder
//!   (or shortcut file) path, `launchTarget` = the executable, a validated store link (from a
//!   `.url`) or a `.lnk` the user confirmed, `system: "pc"`.
//! - Every route carries the install state `root:<rootId>`. A rescan of a root that could be
//!   read completely marks its routes that weren't found as not installed (removed when the
//!   game holds no user data). A root that is offline or unreadable (also when re-checked just
//!   before applying removals), or whose scan hit the entry cap, changes nothing; routes under
//!   a folder that couldn't be listed completely are kept; and a scan that would mark more
//!   than half of a root's routes (and more than `db::MASS_REMOVAL_MIN`) missing is held back
//!   and reported (`removalsHeld`, `warning`).
//! - Games are only ever matched by route identity; similar titles are reported as
//!   `duplicate` review items, never merged.

use crate::catalog;
use crate::db::{self, Db};
use crate::emulators::{self, EmulatorCounts};
use crate::localscan::{self, classify_shortcut, key_of, GameScan, ScanInputs, ShortcutKind};
use crate::review::{self, AMBIGUOUS_EXE, DUPLICATE, SHORTCUT, UNREADABLE, UNSUPPORTED};
use crate::roots::LibraryRoot;
use crate::tracker::{is_within, normalize_path};
use crate::types::{now_ms, Game, GameRoute};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const ROM_SOURCE: &str = "rom";
pub const LOCAL_SOURCE: &str = "local";
/// Review items of one kind kept per root and scan.
const MAX_REVIEW_PER_KIND: usize = 200;
const MAX_SAMPLES: usize = 20;

/// `library-scan:progress` payload.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `"emulators"`, `"games"`, `"library"` (saving and checking duplicates) or `"done"`.
    pub phase: &'static str,
    /// The root being scanned (its path); empty for the library phase.
    pub root: String,
    /// Entries looked at so far in this root.
    pub processed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RootOutcome {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The entry cap was reached: results were added but nothing was marked missing.
    pub truncated: bool,
    /// Subfolders that couldn't be read.
    pub unreadable: usize,
    /// Something the user should know although the scan worked (removals held back, the
    /// folder became unreadable before removals were applied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GameCounts {
    pub added: usize,
    pub updated: usize,
    pub removed_or_uninstalled: usize,
    pub skipped: usize,
    /// Folders that couldn't be read.
    pub failed: usize,
    /// Routes not marked missing because too many at once looked gone (see the module docs).
    pub removals_held: usize,
}

/// Result of a library folder scan (returned and emitted as `library-scan:finished`).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub roots: Vec<RootOutcome>,
    pub emulators: EmulatorCounts,
    pub games: GameCounts,
    /// Open review items after the scan.
    pub review: usize,
    pub unsupported_samples: Vec<String>,
    pub duration_ms: i64,
}

fn rom_route(root_id: &str, rom: &localscan::RomGame) -> GameRoute {
    let path = rom.path.to_string_lossy().into_owned();
    GameRoute {
        source_id: ROM_SOURCE.into(),
        store: "Emulator".into(),
        app_id: Some(key_of(&rom.path)),
        launch_target: path,
        install_path: rom.path.parent().map(|p| p.to_string_lossy().into_owned()),
        installed: Some(true),
        install_states: Some([(db::root_state_key(root_id), true)].into()),
        system: Some(rom.system.into()),
        ..Default::default()
    }
}

/// A PC route. `source` identifies it (the game folder or the shortcut file); `target` is
/// what is launched.
fn pc_route(
    root_id: Option<&str>,
    folder: Option<&Path>,
    source: &Path,
    target: &str,
) -> GameRoute {
    GameRoute {
        source_id: LOCAL_SOURCE.into(),
        store: "Custom".into(),
        app_id: Some(key_of(source)),
        launch_target: target.to_string(),
        install_path: folder.map(|f| f.to_string_lossy().into_owned()),
        installed: Some(true),
        install_states: root_id.map(|id| [(db::root_state_key(id), true)].into()),
        system: Some(catalog::PC.into()),
        ..Default::default()
    }
}

fn new_game(prefix: &str, title: &str, store: &str, route: GameRoute) -> Game {
    Game {
        id: format!("{prefix}:{}", uuid::Uuid::new_v4()),
        title: title.chars().take(300).collect(),
        store: store.into(),
        routes: vec![route],
        ..Default::default()
    }
}

fn scan_inputs(conn: &Connection) -> Result<ScanInputs> {
    let mut stmt = conn.prepare("SELECT folderKey,executable FROM local_exe_choice")?;
    let exe_choices = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<HashMap<String, String>>>()?;
    Ok(ScanInputs {
        ignored: db::ignore_keys(conn, db::IGNORE_PATH)?,
        exe_choices,
        shortcut_ok: db::shortcut_confirmations(conn)?,
    })
}

/// Route identities in the library with their install state (identity → installed, target).
fn known_routes(conn: &Connection) -> Result<HashMap<String, (bool, String)>> {
    Ok(db::all_games(conn)?
        .into_iter()
        .flat_map(|g| g.routes)
        .map(|r| {
            (
                db::route_identity(&r),
                (r.installed == Some(true), r.launch_target.clone()),
            )
        })
        .collect())
}

/// Applies one games root's scan (see the module docs). `root_readable`: the root could still
/// be listed right before the database was locked for this (checked by the caller, so no
/// filesystem access happens under the lock). Returns the counts and a warning for the user,
/// if any.
pub fn apply_game_scan(
    conn: &mut Connection,
    root: &LibraryRoot,
    scan: &GameScan,
    root_readable: bool,
) -> Result<(GameCounts, Option<String>)> {
    let mut counts = GameCounts {
        skipped: scan.skipped,
        failed: scan.unreadable.len(),
        ..Default::default()
    };
    let mut games: Vec<Game> = scan
        .roms
        .iter()
        .map(|rom| new_game("rom", &rom.title, "Emulator", rom_route(&root.id, rom)))
        .collect();
    games.extend(scan.pcs.iter().map(|pc| {
        new_game(
            "local",
            &pc.title,
            "Custom",
            pc_route(Some(&root.id), pc.folder.as_deref(), &pc.source, &pc.target),
        )
    }));
    let known = known_routes(conn)?;
    let mut found: HashSet<String> = HashSet::new();
    for game in &games {
        let identity = db::route_identity(&game.routes[0]);
        match known.get(&identity) {
            None => counts.added += 1,
            Some((installed, target)) => {
                if !installed || *target != game.routes[0].launch_target {
                    counts.updated += 1;
                }
            }
        }
        found.insert(identity);
    }
    // Ambiguous folders and shortcuts awaiting confirmation keep whatever they had until
    // the user decides.
    for item in &scan.ambiguous {
        found.insert(format!("{LOCAL_SOURCE}:{}", key_of(&item.folder)));
    }
    for item in &scan.shortcuts {
        let source = item.folder.as_deref().unwrap_or(&item.shortcut);
        found.insert(format!("{LOCAL_SOURCE}:{}", key_of(source)));
    }
    db::upsert_imported_games(conn, &games)?;
    let mut warning = None;
    if !scan.truncated {
        // The walk may have taken a while: never apply removals from a root that is gone now.
        if !root_readable {
            warning = Some(
                "The folder couldn't be read when the scan finished, so nothing was marked missing."
                    .to_string(),
            );
        } else {
            let unreadable: Vec<String> = scan
                .unreadable
                .iter()
                .filter_map(|p| normalize_path(p))
                .collect();
            let applied = db::reconcile_local_root(conn, &root.id, &found, &|route| {
                let key = non_empty_key(route);
                unreadable.iter().any(|u| is_within(u, &key))
            })?;
            counts.removed_or_uninstalled = applied.marked;
            counts.removals_held = applied.held;
            if applied.held > 0 {
                warning = Some(format!(
                    "{} games looked missing at once, so none were marked missing. If they really are gone, check the folder and rescan, or remove and re-add it.",
                    applied.held
                ));
            }
        }
    }
    // Review items for this root.
    let mut keep = HashSet::new();
    for item in scan.ambiguous.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!("{AMBIGUOUS_EXE}:{}", key_of(&item.folder));
        review::upsert(
            conn,
            AMBIGUOUS_EXE,
            &key,
            Some(&root.id),
            &json!({
                "title": item.title,
                "folder": item.folder.to_string_lossy(),
                "candidates": item.candidates.iter().map(|c| c.to_string_lossy()).collect::<Vec<_>>(),
                "rootId": root.id,
            }),
        )?;
        keep.insert(key);
    }
    for item in scan.unsupported.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!(
            "{UNSUPPORTED}:{}",
            normalize_path(&item.path).unwrap_or_default()
        );
        review::upsert(
            conn,
            UNSUPPORTED,
            &key,
            Some(&root.id),
            &json!({ "path": item.path, "reason": item.reason, "rootId": root.id }),
        )?;
        keep.insert(key);
    }
    for item in scan.shortcuts.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!("{SHORTCUT}:{}", key_of(&item.shortcut));
        review::upsert(
            conn,
            SHORTCUT,
            &key,
            Some(&root.id),
            &shortcut_details(item, Some(&root.id)),
        )?;
        keep.insert(key);
    }
    for path in scan.unreadable.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!("{UNREADABLE}:{}", normalize_path(path).unwrap_or_default());
        review::upsert(
            conn,
            UNREADABLE,
            &key,
            Some(&root.id),
            &json!({ "path": path, "reason": "This folder couldn't be read.", "rootId": root.id }),
        )?;
        keep.insert(key);
    }
    if !scan.truncated {
        review::prune(
            conn,
            Some(&root.id),
            &[AMBIGUOUS_EXE, UNSUPPORTED, UNREADABLE, SHORTCUT],
            &keep,
        )?;
    }
    Ok((counts, warning))
}

/// The root folder can still be opened and listed.
pub fn root_still_readable(root: &Path) -> bool {
    root.is_dir() && std::fs::read_dir(root).is_ok()
}

fn shortcut_details(item: &localscan::PendingShortcut, root_id: Option<&str>) -> Value {
    json!({
        "title": item.title,
        "shortcut": item.shortcut.to_string_lossy(),
        "target": item.target,
        "arguments": item.arguments,
        "folder": item.folder.as_ref().map(|f| f.to_string_lossy().into_owned()),
        "rootId": root_id,
    })
}

/// Stores a shortcut awaiting confirmation (`library.addLocalGame` of a `.lnk`).
pub fn request_shortcut_confirmation(
    conn: &Connection,
    item: &localscan::PendingShortcut,
) -> Result<()> {
    let key = format!("{SHORTCUT}:{}", key_of(&item.shortcut));
    review::upsert(conn, SHORTCUT, &key, None, &shortcut_details(item, None))
}

fn non_empty_key(route: &GameRoute) -> String {
    route
        .app_id
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| route.launch_target.to_lowercase())
}

/// Games of one (system, title) group: game id → (title, launch targets).
type DuplicateGroup = BTreeMap<String, (String, Vec<String>)>;

/// Recomputes `duplicate` review items over the whole library: installed ROM/PC routes of
/// different games with the same system and clean title.
pub fn refresh_duplicates(conn: &Connection) -> Result<()> {
    let mut groups: BTreeMap<(String, String), DuplicateGroup> = BTreeMap::new();
    for (game, _) in db::games_with_user_data(conn)? {
        for route in &game.routes {
            if !(route.source_id == ROM_SOURCE || route.source_id == LOCAL_SOURCE)
                || route.installed != Some(true)
            {
                continue;
            }
            let Some(system) = route.system.clone() else {
                continue;
            };
            let title_key = catalog::title_key(&game.title);
            if title_key.is_empty() {
                continue;
            }
            groups
                .entry((system, title_key))
                .or_default()
                .entry(game.id.clone())
                .or_insert_with(|| (game.title.clone(), Vec::new()))
                .1
                .push(route.launch_target.clone());
        }
    }
    let mut keep = HashSet::new();
    for ((system, title_key), games) in groups {
        if games.len() < 2 {
            continue;
        }
        let ids: Vec<&String> = games.keys().collect();
        let key = format!(
            "{DUPLICATE}:{system}:{title_key}:{}",
            ids.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")
        );
        let first_title = games
            .values()
            .next()
            .map(|g| g.0.clone())
            .unwrap_or_default();
        let details = json!({
            "system": system,
            "title": first_title,
            "games": games.iter().map(|(id, (title, paths))| json!({
                "gameId": id, "title": title, "paths": paths
            })).collect::<Vec<_>>(),
        });
        review::upsert(conn, DUPLICATE, &key, None, &details)?;
        keep.insert(key);
    }
    review::prune(conn, None, &[DUPLICATE], &keep)
}

/// Scans `roots` (emulator roots first, so new games get a default emulator), applies the
/// results and returns the summary. Disabled roots are skipped. Each root's result is also
/// stored on the root (`lastScanAt`, `lastResult`).
pub fn scan_roots(
    db: &Db,
    roots: &[LibraryRoot],
    progress: &mut dyn FnMut(Progress),
) -> Result<ScanSummary> {
    let started = Instant::now();
    let mut summary = ScanSummary::default();
    let mut ordered: Vec<&LibraryRoot> = roots.iter().filter(|r| r.enabled).collect();
    ordered.sort_by_key(|r| r.kind != "emulators");
    for root in ordered {
        let root_started = Instant::now();
        let root_path = PathBuf::from(&root.path);
        let mut outcome = RootOutcome {
            id: root.id.clone(),
            path: root.path.clone(),
            kind: root.kind.clone(),
            ok: true,
            ..Default::default()
        };
        let phase = if root.kind == "emulators" {
            "emulators"
        } else {
            "games"
        };
        progress(Progress {
            phase,
            root: root.path.clone(),
            processed: 0,
            total: None,
            current: None,
        });
        let mut report = |processed: usize, current: &Path| {
            progress(Progress {
                phase,
                root: root.path.clone(),
                processed,
                total: None,
                current: Some(current.to_string_lossy().into_owned()),
            })
        };
        let mut root_counts = json!({});
        if root.kind == "emulators" {
            let ignored = db::ignore_keys(&db.lock(), db::IGNORE_EMULATOR)?;
            match emulators::discover(&root_path, &ignored, &mut report) {
                Ok(found) => {
                    let mut counts = emulators::apply_discovered(&db.lock(), &found)?;
                    // Availability needs the filesystem: check it without the database lock.
                    let raw = db::emulator_profiles(&db.lock())?;
                    let profiles = emulators::refresh_available(raw);
                    counts.unavailable = emulators::unavailable_in_root(&profiles, &root.key());
                    summary.emulators.added += counts.added;
                    summary.emulators.updated += counts.updated;
                    summary.emulators.unavailable += counts.unavailable;
                    outcome.truncated = found.truncated;
                    outcome.unreadable = found.unreadable.len();
                    root_counts = json!({ "emulators": counts });
                }
                Err(e) => {
                    outcome.ok = false;
                    outcome.error = Some(format!("{e:#}"));
                }
            }
        } else {
            let inputs = scan_inputs(&db.lock())?;
            match localscan::scan_games(&root_path, &inputs, &mut report) {
                Ok(scan) => {
                    // The walk may have taken a while: re-check the root, then lock.
                    let readable = root_still_readable(&root_path);
                    let (counts, warning) = apply_game_scan(&mut db.lock(), root, &scan, readable)?;
                    outcome.warning = warning;
                    let g = &mut summary.games;
                    g.added += counts.added;
                    g.updated += counts.updated;
                    g.removed_or_uninstalled += counts.removed_or_uninstalled;
                    g.skipped += counts.skipped;
                    g.failed += counts.failed;
                    g.removals_held += counts.removals_held;
                    outcome.truncated = scan.truncated;
                    outcome.unreadable = scan.unreadable.len();
                    for sample in scan
                        .unsupported
                        .iter()
                        .map(|u| format!("{} ({})", u.path, u.reason))
                        .chain(scan.samples.iter().cloned())
                    {
                        if summary.unsupported_samples.len() < MAX_SAMPLES {
                            summary.unsupported_samples.push(sample);
                        }
                    }
                    root_counts = json!({ "games": counts });
                }
                Err(e) => {
                    outcome.ok = false;
                    outcome.error = Some(format!("{e:#}"));
                }
            }
        }
        let mut result = serde_json::to_value(&outcome)?;
        if let (Value::Object(map), Value::Object(extra)) = (&mut result, root_counts) {
            map.extend(extra);
            map.insert(
                "durationMs".into(),
                json!(root_started.elapsed().as_millis() as i64),
            );
        }
        crate::roots::record_result(&db.lock(), &root.id, now_ms(), &result)?;
        summary.roots.push(outcome);
    }
    progress(Progress {
        phase: "library",
        root: String::new(),
        processed: 0,
        total: None,
        current: None,
    });
    {
        let conn = db.lock();
        refresh_duplicates(&conn)?;
        summary.review = review::open_count(&conn)?;
    }
    summary.duration_ms = started.elapsed().as_millis() as i64;
    progress(Progress {
        phase: "done",
        root: String::new(),
        processed: 0,
        total: None,
        current: None,
    });
    Ok(summary)
}

/// Removes a root. Its games are never simply deleted: routes found only under it become
/// not installed, and are dropped only when the game holds no user data (like a store
/// uninstall). Its open review items go too. Profiles found in an emulator root stay.
pub fn remove_root(conn: &mut Connection, root_id: &str) -> Result<(usize, usize)> {
    let key = db::root_state_key(root_id);
    let changed = db::mark_local_routes_missing(conn, Some(&key), &|_| true)?;
    crate::roots::delete(conn, root_id)?;
    review::forget_root(conn, root_id)?;
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Review resolution

/// How to resolve a review item (`review.resolve`).
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Resolution {
    /// `duplicate`: the games are different; never ask again for this set.
    KeepSeparate,
    /// `duplicate`: merge the games into `targetGameId` (default: the first listed).
    #[serde(rename_all = "camelCase")]
    Link { target_game_id: Option<String> },
    /// Any kind: never scan `path` again (one of the item's paths or its folder).
    Ignore { path: String },
    /// `ambiguous-exe`: use `executable` (one of the item's candidates) for the folder.
    ChooseExe { executable: String },
    /// `shortcut`: the user checked the shortcut's target and arguments; add the game. Fails
    /// when the shortcut changed since the item was made.
    ConfirmShortcut,
    /// Any kind: close without changes.
    Dismiss,
}

#[derive(Debug, Default)]
pub struct Resolved {
    /// (target, source) pairs merged, for the play tracker.
    pub merged: Vec<(String, String)>,
}

fn detail_paths(item: &review::ReviewItem) -> Vec<String> {
    let d = &item.details;
    let mut paths = Vec::new();
    for key in ["path", "folder", "shortcut"] {
        if let Some(p) = d.get(key).and_then(Value::as_str) {
            paths.push(p.to_string());
        }
    }
    for game in d
        .get("games")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for p in game
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(p) = p.as_str() {
                paths.push(p.to_string());
            }
        }
    }
    paths
}

fn duplicate_ids(item: &review::ReviewItem) -> Vec<String> {
    item.details
        .get("games")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| g.get("gameId").and_then(Value::as_str).map(String::from))
        .collect()
}

/// Filesystem facts a resolution needs, gathered without holding the database lock.
#[derive(Debug, Default)]
struct Checked {
    /// `choose-exe`: (the chosen candidate as listed, what the route launches).
    choice: Option<(String, String)>,
}

fn item_text(item: &review::ReviewItem, key: &str) -> Option<String> {
    item.details
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
}

/// Checks files and shortcuts for `choose-exe` and `confirm-shortcut` (reads the disk and,
/// for `.lnk` files, COM).
fn check(
    item: &review::ReviewItem,
    resolution: &Resolution,
    confirmations: &HashMap<String, (String, String)>,
) -> Result<Checked> {
    let mut checked = Checked::default();
    match resolution {
        Resolution::ChooseExe { executable } => {
            if item.kind != AMBIGUOUS_EXE {
                bail!("Only ambiguous PC games need an executable.");
            }
            let candidates: Vec<String> = item
                .details
                .get("candidates")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            let chosen = normalize_path(executable)
                .and_then(|k| {
                    candidates
                        .iter()
                        .find(|c| normalize_path(c).as_deref() == Some(k.as_str()))
                })
                .cloned()
                .ok_or_else(|| anyhow!("Choose one of the listed executables."))?;
            if !Path::new(&chosen).is_file() {
                bail!("That executable no longer exists. Rescan the folder.");
            }
            // A shortcut candidate follows the shortcut rules: links must be store links,
            // and a .lnk with arguments needs its own confirmation.
            let target = if chosen.to_lowercase().ends_with(".exe") {
                chosen.clone()
            } else {
                match classify_shortcut(Path::new(&chosen), confirmations) {
                    ShortcutKind::Uri(uri) => uri,
                    ShortcutKind::Exe(exe) => exe.to_string_lossy().into_owned(),
                    ShortcutKind::Confirmed => chosen.clone(),
                    ShortcutKind::NeedsConfirmation { target, arguments } => bail!(
                        "That shortcut runs {target} {arguments}. Add it with \"Add local game\" to confirm it first."
                    ),
                    ShortcutKind::Invalid(reason) => bail!("{reason}"),
                    ShortcutKind::Unreadable => bail!("That shortcut can't be read."),
                }
            };
            checked.choice = Some((chosen, target));
        }
        Resolution::ConfirmShortcut => {
            if item.kind != SHORTCUT {
                bail!("Only shortcuts can be confirmed.");
            }
            let shortcut =
                item_text(item, "shortcut").ok_or_else(|| anyhow!("This item has no shortcut."))?;
            let seen = (
                item_text(item, "target").unwrap_or_default(),
                item_text(item, "arguments").unwrap_or_default(),
            );
            // The user confirmed what the item showed: the file must still say exactly that.
            match classify_shortcut(Path::new(&shortcut), &HashMap::new()) {
                ShortcutKind::NeedsConfirmation { target, arguments }
                    if (target.as_str(), arguments.as_str())
                        == (seen.0.as_str(), seen.1.as_str()) => {}
                _ => bail!(
                    "That shortcut changed or can't be read any more. Rescan to review it again."
                ),
            }
        }
        _ => {}
    }
    Ok(checked)
}

/// Applies a resolution: the item is read, files are checked without the database lock,
/// then the change is applied under it. Errors leave the item open.
pub fn resolve(db: &Db, id: &str, resolution: &Resolution) -> Result<Resolved> {
    let (item, confirmations) = {
        let conn = db.lock();
        let item =
            review::get(&conn, id)?.ok_or_else(|| anyhow!("That review item no longer exists."))?;
        (item, db::shortcut_confirmations(&conn)?)
    };
    if item.status != "open" {
        bail!("That review item was already resolved.");
    }
    let checked = check(&item, resolution, &confirmations)?;
    let mut conn = db.lock();
    // Still open? (Another resolution may have run meanwhile.)
    if review::get(&conn, id)?.is_none_or(|current| current.status != "open") {
        bail!("That review item was already resolved.");
    }
    apply(&mut conn, &item, resolution, checked)
}

fn apply(
    conn: &mut Connection,
    item: &review::ReviewItem,
    resolution: &Resolution,
    checked: Checked,
) -> Result<Resolved> {
    let id = item.id.as_str();
    let mut out = Resolved::default();
    let label = match resolution {
        Resolution::KeepSeparate => {
            if item.kind != DUPLICATE {
                bail!("Only duplicates can be kept separate.");
            }
            "kept-separate"
        }
        Resolution::Link { target_game_id } => {
            if item.kind != DUPLICATE {
                bail!("Only duplicates can be linked.");
            }
            let ids = duplicate_ids(item);
            let target = target_game_id
                .clone()
                .or_else(|| ids.first().cloned())
                .ok_or_else(|| anyhow!("This duplicate lists no games."))?;
            if !ids.contains(&target) {
                bail!("Choose one of the listed games to keep.");
            }
            for source in ids.iter().filter(|i| **i != target) {
                if db::get_game(conn, source)?.is_some() {
                    db::merge_games(conn, &target, source)?;
                    out.merged.push((target.clone(), source.clone()));
                }
            }
            "linked"
        }
        Resolution::Ignore { path } => {
            let allowed: HashSet<String> = detail_paths(item)
                .iter()
                .filter_map(|p| normalize_path(p))
                .collect();
            let key = normalize_path(path)
                .filter(|k| allowed.contains(k))
                .ok_or_else(|| anyhow!("Choose one of the paths listed in this item."))?;
            db::add_ignore(conn, db::IGNORE_PATH, &key, path.trim())?;
            db::mark_local_routes_missing(conn, None, &|route| {
                (route.source_id == ROM_SOURCE || route.source_id == LOCAL_SOURCE)
                    && is_within(&key, &non_empty_key(route))
            })?;
            "ignored"
        }
        Resolution::ChooseExe { .. } => {
            let (chosen, target) = checked
                .choice
                .ok_or_else(|| anyhow!("Choose one of the listed executables."))?;
            let folder = item
                .details
                .get("folder")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("This item has no folder."))?
                .to_string();
            let title = item
                .details
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
                .to_string();
            conn.execute(
                "INSERT INTO local_exe_choice(folderKey,executable,chosenAt) VALUES(?,?,?) ON CONFLICT(folderKey) DO UPDATE SET executable=excluded.executable,chosenAt=excluded.chosenAt",
                params![key_of(Path::new(&folder)), chosen, now_ms()],
            )?;
            let root_id = item.root_id.clone();
            let game = new_game(
                "local",
                &title,
                "Custom",
                pc_route(
                    root_id.as_deref(),
                    Some(Path::new(&folder)),
                    Path::new(&folder),
                    &target,
                ),
            );
            db::upsert_imported_games(conn, &[game])?;
            "exe-chosen"
        }
        Resolution::ConfirmShortcut => {
            // `check` verified the file still matches what the item showed.
            let text = |key: &str| item_text(item, key);
            let shortcut = text("shortcut").ok_or_else(|| anyhow!("This item has no shortcut."))?;
            let (seen_target, seen_args) = (
                text("target").unwrap_or_default(),
                text("arguments").unwrap_or_default(),
            );
            db::confirm_shortcut(
                conn,
                &key_of(Path::new(&shortcut)),
                &seen_target,
                &seen_args,
            )?;
            let folder = text("folder").map(PathBuf::from);
            let source = folder.clone().unwrap_or_else(|| PathBuf::from(&shortcut));
            let title = text("title").unwrap_or_else(|| "Untitled".into());
            let game = new_game(
                "local",
                &title,
                "Custom",
                pc_route(
                    item.root_id.as_deref(),
                    folder.as_deref(),
                    &source,
                    &shortcut,
                ),
            );
            db::upsert_imported_games(conn, &[game])?;
            "shortcut-confirmed"
        }
        Resolution::Dismiss => "dismissed",
    };
    review::resolve(conn, id, label)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Single games

/// The system of a ROM file: the nearest folder named after a system, else the only system
/// using its extension.
pub fn system_for_file(path: &Path) -> Option<&'static catalog::System> {
    let ext = catalog::extension_of(&path.file_name()?.to_string_lossy())?;
    let candidates = catalog::systems_for_extension(&ext);
    for ancestor in path.ancestors().skip(1) {
        if let Some(system) = ancestor
            .file_name()
            .and_then(|n| catalog::system_for_folder(&n.to_string_lossy()))
        {
            if candidates.iter().any(|c| c.id == system.id) {
                return Some(system);
            }
        }
    }
    match candidates.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// What a chosen file (`library.addLocalGame`) becomes.
#[derive(Debug)]
pub enum LocalFile {
    Game(Game),
    /// A `.lnk` with arguments or a non-`.exe` target: the user must confirm it in review.
    NeedsConfirmation(localscan::PendingShortcut),
}

/// A new game for one chosen file: an `.exe` → a Custom game started directly; a `.url` → a
/// Custom game opening its link (only store launch links); a `.lnk` → its `.exe` when it has
/// no arguments, a confirmed shortcut, or a pending confirmation; a ROM format → an Emulator
/// game (system from its folders or extension; `None` when that is ambiguous, so the user
/// picks an emulator). `confirmed`: shortcuts the user confirmed before.
pub fn game_for_file(
    path: &Path,
    confirmed: &HashMap<String, (String, String)>,
) -> Result<LocalFile> {
    if !path.is_absolute() || normalize_path(&path.to_string_lossy()).is_none() {
        bail!("Choose a file on a local drive.");
    }
    if !path.is_file() {
        bail!("That file doesn't exist.");
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = catalog::extension_of(&name).unwrap_or_default();
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
    let title = catalog::clean_title(stem);
    let folder_of = |p: &Path| p.parent().map(Path::to_path_buf);
    if ext == ".exe" {
        let mut route = pc_route(None, None, path, &path.to_string_lossy());
        route.install_path = folder_of(path).map(|p| p.to_string_lossy().into_owned());
        return Ok(LocalFile::Game(new_game("local", &title, "Custom", route)));
    }
    if ext == ".lnk" || ext == ".url" {
        let route = match classify_shortcut(path, confirmed) {
            ShortcutKind::Uri(uri) => pc_route(None, None, path, &uri),
            ShortcutKind::Exe(exe) => {
                let mut route = pc_route(None, None, path, &exe.to_string_lossy());
                route.install_path = folder_of(&exe).map(|p| p.to_string_lossy().into_owned());
                route
            }
            ShortcutKind::Confirmed => pc_route(None, None, path, &path.to_string_lossy()),
            ShortcutKind::NeedsConfirmation { target, arguments } => {
                return Ok(LocalFile::NeedsConfirmation(localscan::PendingShortcut {
                    shortcut: path.to_path_buf(),
                    folder: None,
                    title,
                    target,
                    arguments,
                }))
            }
            ShortcutKind::Invalid(reason) => bail!("{reason}"),
            ShortcutKind::Unreadable => bail!("That shortcut can't be read."),
        };
        return Ok(LocalFile::Game(new_game("local", &title, "Custom", route)));
    }
    if catalog::systems_for_extension(&ext).is_empty() {
        bail!("That file type isn't a supported game or ROM format.");
    }
    let system = system_for_file(path);
    let route = GameRoute {
        source_id: ROM_SOURCE.into(),
        store: "Emulator".into(),
        app_id: Some(key_of(path)),
        launch_target: path.to_string_lossy().into_owned(),
        install_path: path.parent().map(|p| p.to_string_lossy().into_owned()),
        installed: Some(true),
        system: system.map(|s| s.id.to_string()),
        ..Default::default()
    };
    Ok(LocalFile::Game(new_game("rom", &title, "Emulator", route)))
}

/// File dialog extensions for `addLocalGame` (without dots).
pub fn pickable_extensions() -> Vec<String> {
    let mut exts: Vec<String> = vec!["exe".into(), "lnk".into(), "url".into()];
    for system in catalog::SYSTEMS {
        for ext in system.extensions {
            let e = ext.trim_start_matches('.').to_string();
            if !exts.contains(&e) {
                exts.push(e);
            }
        }
    }
    exts
}

/// Sets (or with `None` clears back to automatic) a ROM route's emulator.
pub fn set_emulator(
    conn: &Connection,
    game_id: &str,
    route_index: usize,
    emulator_id: Option<&str>,
) -> Result<()> {
    let game = db::get_game(conn, game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let mut routes = game.routes.clone();
    let route = routes
        .get_mut(route_index)
        .ok_or_else(|| anyhow!("That launch route doesn't exist."))?;
    if route.source_id != ROM_SOURCE {
        bail!("Only ROM routes use an emulator.");
    }
    if let Some(id) = emulator_id {
        if db::emulator_profile(conn, id)?.is_none() {
            bail!("That emulator profile doesn't exist.");
        }
    }
    route.emulator_id = emulator_id.map(String::from);
    db::set_routes(conn, game_id, routes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;
    use crate::roots::RootGuard;
    use crate::types::{GamePageCustom, GamePatch};

    struct Fixture {
        db: Db,
        dir: TempDir,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            Self {
                db: Db::memory().unwrap(),
                dir: TempDir::new(name),
            }
        }

        fn add_root(&self, kind: &str, rel: &str) -> LibraryRoot {
            let path = self.dir.dir(rel);
            crate::roots::add(
                &self.db.lock(),
                kind,
                &path.to_string_lossy(),
                &RootGuard::default(),
            )
            .unwrap()
        }

        fn scan(&self) -> ScanSummary {
            let roots = crate::roots::list(&self.db.lock()).unwrap();
            scan_roots(&self.db, &roots, &mut |_| {}).unwrap()
        }

        fn games(&self) -> Vec<Game> {
            db::all_games(&self.db.lock()).unwrap()
        }

        fn game(&self, title: &str) -> Game {
            self.games().into_iter().find(|g| g.title == title).unwrap()
        }
    }

    #[test]
    fn scans_add_rom_and_pc_games_with_default_emulators_and_are_idempotent() {
        let f = Fixture::new("local-scan");
        f.dir.file("Emu/mGBA/mGBA.exe");
        f.dir.file("Emu/Dolphin/Dolphin.exe");
        f.dir.file("Roms/GBA/Pokemon Emerald (USA).gba");
        f.dir.file("Roms/GameCube/Zelda (USA).rvz");
        f.dir.file("Roms/PS2/Okami (USA).iso");
        f.dir.file("Roms/PC/Celeste/Celeste.exe");
        f.dir.file("Roms/Odd/thing.bin");
        f.add_root("emulators", "Emu");
        let games_root = f.add_root("games", "Roms");
        let first = f.scan();
        assert!(first.roots.iter().all(|r| r.ok));
        assert_eq!(first.emulators.added, 2);
        assert_eq!(first.games.added, 4);
        assert_eq!(first.games.skipped, 1);
        assert_eq!(first.review, 1, "Odd is unsupported");
        assert!(first.unsupported_samples.iter().any(|s| s.contains("Odd")));

        let raw = db::emulator_profiles(&f.db.lock()).unwrap();
        let profiles = emulators::refresh_available(raw);
        let mgba = profiles.iter().find(|p| p.name == "mGBA").unwrap();
        let pokemon = f.game("Pokemon Emerald");
        let route = &pokemon.routes[0];
        assert_eq!(pokemon.store, "Emulator");
        assert_eq!(route.source_id, "rom");
        assert_eq!(route.system.as_deref(), Some("gba"));
        // Scans never choose an emulator; the default is resolved at launch.
        assert_eq!(route.emulator_id, None);
        assert_eq!(
            crate::launch::emulator_for_route(&profiles, route)
                .unwrap()
                .id,
            mgba.id
        );
        assert_eq!(route.installed, Some(true));
        assert!(route.launch_target.ends_with("Pokemon Emerald (USA).gba"));
        assert_eq!(
            route.app_id.as_deref(),
            Some(key_of(Path::new(&route.launch_target)).as_str())
        );
        assert!(route.install_path.as_deref().unwrap().ends_with("GBA"));
        assert_eq!(
            f.game("Okami").routes[0].emulator_id,
            None,
            "no PS2 emulator"
        );
        let celeste = f.game("Celeste");
        assert_eq!(
            (celeste.store.as_str(), celeste.routes[0].source_id.as_str()),
            ("Custom", "local")
        );

        // User data: favorite, custom cover, playtime, renamed title.
        {
            let conn = f.db.lock();
            db::save_game(
                &conn,
                &GamePatch {
                    favorite: Some(true),
                    title: Some("Pokemon Emerald Version".into()),
                    cover: Some("lobby-cover://p.png".into()),
                    playtime_seconds: Some(3_600),
                    ..GamePatch::new(pokemon.id.clone())
                },
            )
            .unwrap();
        }
        let second = f.scan();
        assert_eq!(
            second.games,
            GameCounts {
                skipped: 1,
                ..Default::default()
            }
        );
        assert_eq!((second.emulators.added, second.emulators.updated), (0, 0));
        assert_eq!(f.games().len(), 4);
        let kept = f.game("Pokemon Emerald Version");
        assert!(kept.favorite);
        assert_eq!(kept.cover.as_deref(), Some("lobby-cover://p.png"));
        assert_eq!(kept.playtime_seconds, Some(3_600));
        assert_eq!(kept.id, pokemon.id);
        assert_eq!(second.review, 1);
        let root = crate::roots::get(&f.db.lock(), &games_root.id)
            .unwrap()
            .unwrap();
        assert!(root.last_scan_at.is_some());
        assert_eq!(root.last_result.unwrap()["ok"], true);
    }

    #[test]
    fn missing_files_offline_roots_and_root_removal() {
        let f = Fixture::new("local-missing");
        let kept = f.dir.file("Roms/SNES/Kept (USA).sfc");
        let plain = f.dir.file("Roms/SNES/Plain (USA).sfc");
        f.dir.file("Roms/SNES/Other (USA).sfc");
        let root = f.add_root("games", "Roms");
        f.scan();
        assert_eq!(f.games().len(), 3);
        let kept_game = f.game("Kept");
        db::save_game(
            &f.db.lock(),
            &GamePatch {
                favorite: Some(true),
                ..GamePatch::new(kept_game.id.clone())
            },
        )
        .unwrap();
        std::fs::remove_file(&kept).unwrap();
        std::fs::remove_file(&plain).unwrap();
        let summary = f.scan();
        assert_eq!(summary.games.removed_or_uninstalled, 2);
        let titles: Vec<String> = f.games().into_iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Kept", "Other"]);
        let k = f.game("Kept");
        assert_eq!(k.routes[0].installed, Some(false));
        assert_eq!(k.routes[0].install_path, None);
        // The file comes back: installed again, same game.
        std::fs::write(&kept, b"x").unwrap();
        f.scan();
        assert_eq!(f.game("Kept").routes[0].installed, Some(true));
        assert_eq!(f.game("Kept").id, kept_game.id);

        // Offline root: nothing changes.
        let roms = f.dir.path().join("Roms");
        let moved = f.dir.path().join("Roms-offline");
        std::fs::rename(&roms, &moved).unwrap();
        let summary = f.scan();
        assert!(!summary.roots[0].ok && summary.roots[0].error.is_some());
        assert!(f
            .games()
            .iter()
            .all(|g| g.routes[0].installed == Some(true)));
        let listed = crate::roots::list(&f.db.lock()).unwrap();
        assert!(!crate::roots::with_online(listed)[0].online);
        std::fs::rename(&moved, &roms).unwrap();

        // Removing the root: user data stays (not installed), the rest goes.
        remove_root(&mut f.db.lock(), &root.id).unwrap();
        let games = f.games();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Kept");
        assert_eq!(games[0].routes[0].installed, Some(false));
        assert!(crate::roots::list(&f.db.lock()).unwrap().is_empty());
    }

    #[test]
    fn customized_pages_count_as_user_data() {
        let f = Fixture::new("local-custom");
        let file = f.dir.file("Roms/N64/Mario 64 (USA).z64");
        f.dir.file("Roms/N64/Other.z64");
        f.add_root("games", "Roms");
        f.scan();
        let g = f.game("Mario 64");
        db::put_page_custom(
            &f.db.lock(),
            &g.id,
            &GamePageCustom {
                logo_scale: 1.5,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::remove_file(file).unwrap();
        f.scan();
        assert_eq!(f.game("Mario 64").routes[0].installed, Some(false));
    }

    #[test]
    fn duplicates_are_reported_never_merged_and_can_be_resolved() {
        let f = Fixture::new("local-dupes");
        f.dir.file("A/GBA/Metroid Fusion (USA).gba");
        f.dir.file("A/GBA/Metroid Fusion (Europe).gba");
        f.dir.file("B/GBA/Metroid Fusion (Japan).gba");
        f.dir.file("A/GBA/Zelda (USA).gba");
        f.dir.file("A/GBA/Zelda (Europe).zip");
        f.dir.file("A/NES/Zelda (USA).nes");
        f.add_root("games", "A");
        f.add_root("games", "B");
        let summary = f.scan();
        assert_eq!(f.games().len(), 6, "nothing merged");
        assert_eq!(
            summary.review, 2,
            "Metroid x3 and GBA Zelda x2; NES Zelda differs"
        );
        let items = review::list(&f.db.lock(), false).unwrap();
        let metroid = items
            .iter()
            .find(|i| i.details["title"].as_str().unwrap().starts_with("Metroid"))
            .unwrap()
            .clone();
        let zelda = items.iter().find(|i| i.id != metroid.id).unwrap().clone();
        assert_eq!(metroid.details["games"].as_array().unwrap().len(), 3);

        // Rescan: same items, no new ones.
        assert_eq!(f.scan().review, 2);
        assert_eq!(review::list(&f.db.lock(), false).unwrap().len(), 2);

        // Keep separate: stays resolved across rescans.
        resolve(&f.db, &zelda.id, &Resolution::KeepSeparate).unwrap();
        assert_eq!(f.scan().review, 1);
        assert!(resolve(&f.db, &zelda.id, &Resolution::Dismiss).is_err());

        // Ignore one Metroid file: it disappears now and on rescans.
        let japan = metroid.details["games"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["paths"].as_array().unwrap())
            .filter_map(Value::as_str)
            .find(|p| p.contains("Japan"))
            .unwrap()
            .to_string();
        assert!(resolve(
            &f.db,
            &metroid.id,
            &Resolution::Ignore {
                path: "C:\\elsewhere.gba".into()
            }
        )
        .is_err());
        resolve(
            &f.db,
            &metroid.id,
            &Resolution::Ignore {
                path: japan.clone(),
            },
        )
        .unwrap();
        assert_eq!(f.games().len(), 5);
        f.scan();
        assert_eq!(f.games().len(), 5, "ignore list honoured by rescans");
        let ignored = db::ignores(&f.db.lock(), db::IGNORE_PATH).unwrap();
        assert_eq!(ignored.len(), 1);

        // The two remaining Metroids form a new duplicate; link them.
        let items = review::list(&f.db.lock(), false).unwrap();
        assert_eq!(items.len(), 1);
        let ids = duplicate_ids(&items[0]);
        let out = resolve(
            &f.db,
            &items[0].id,
            &Resolution::Link {
                target_game_id: Some(ids[1].clone()),
            },
        )
        .unwrap();
        assert_eq!(out.merged, [(ids[1].clone(), ids[0].clone())]);
        assert_eq!(f.games().len(), 4);
        let linked = db::get_game(&f.db.lock(), &ids[1]).unwrap().unwrap();
        assert_eq!(linked.routes.len(), 2);
        assert_eq!(f.scan().review, 0, "a linked game is not a duplicate");
        assert_eq!(f.games().len(), 4, "rescans keep the link");
    }

    #[test]
    fn ambiguous_pc_games_wait_for_a_choice() {
        let f = Fixture::new("local-ambiguous");
        f.dir.file("Games/PC/Mystery/alpha.exe");
        let beta = f.dir.file("Games/PC/Mystery/beta.exe");
        f.add_root("games", "Games");
        let summary = f.scan();
        assert_eq!(summary.games.added, 0);
        assert!(f.games().is_empty(), "never guessed");
        let items = review::list(&f.db.lock(), false).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, AMBIGUOUS_EXE);
        assert!(resolve(
            &f.db,
            &items[0].id,
            &Resolution::ChooseExe {
                executable: "C:\\other.exe".into()
            }
        )
        .is_err());
        assert!(resolve(&f.db, &items[0].id, &Resolution::KeepSeparate).is_err());
        resolve(
            &f.db,
            &items[0].id,
            &Resolution::ChooseExe {
                executable: beta.to_string_lossy().to_uppercase(),
            },
        )
        .unwrap();
        let games = f.games();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Mystery");
        assert!(games[0].routes[0].launch_target.ends_with("beta.exe"));
        let again = f.scan();
        assert_eq!(
            (again.games.added, again.games.updated, again.review),
            (0, 0, 0)
        );
        assert_eq!(f.games().len(), 1);
    }

    #[test]
    fn unreadable_and_truncated_scans_never_remove() {
        let f = Fixture::new("local-partial");
        f.dir.file("Roms/GB/Tetris.gb");
        f.dir.file("Roms/GB/Sub/Kirby.gb");
        let root = f.add_root("games", "Roms");
        f.scan();
        assert_eq!(f.games().len(), 2);
        // Simulate a scan that couldn't read Sub.
        let sub = f.dir.path().join("Roms/GB/Sub");
        let scan = GameScan {
            roms: vec![localscan::RomGame {
                system: "gb",
                path: f.dir.path().join("Roms/GB/Tetris.gb"),
                title: "Tetris".into(),
                format: ".gb".into(),
                other_discs: vec![],
            }],
            unreadable: vec![sub.to_string_lossy().into()],
            ..Default::default()
        };
        let (counts, _) = apply_game_scan(&mut f.db.lock(), &root, &scan, true).unwrap();
        assert_eq!(counts.removed_or_uninstalled, 0);
        assert_eq!(counts.failed, 1);
        assert_eq!(f.games().len(), 2);
        assert_eq!(
            review::list(&f.db.lock(), false).unwrap()[0].kind,
            UNREADABLE
        );
        // A truncated scan that saw nothing of Kirby still removes nothing.
        let truncated = GameScan {
            unreadable: vec![],
            truncated: true,
            ..scan
        };
        apply_game_scan(&mut f.db.lock(), &root, &truncated, true).unwrap();
        assert_eq!(f.games().len(), 2);
    }

    #[test]
    fn single_files_and_emulator_assignment() {
        let none = HashMap::new();
        let game_for_file = |path: &Path| match game_for_file(path, &none) {
            Ok(LocalFile::Game(game)) => Ok(game),
            Ok(LocalFile::NeedsConfirmation(p)) => Err(anyhow!("needs confirmation: {p:?}")),
            Err(e) => Err(e),
        };
        let f = Fixture::new("local-single");
        let rom = f.dir.file("Stuff/Sony 2/Game.iso");
        let iso = f.dir.file("Stuff/Game.iso");
        let gba = f.dir.file("Stuff/Game.gba");
        let exe = f.dir.file("Stuff/Tool/Game.exe");
        let txt = f.dir.file("Stuff/notes.txt");
        assert_eq!(
            game_for_file(&rom).unwrap().routes[0].system.as_deref(),
            Some("ps2")
        );
        assert_eq!(
            game_for_file(&iso).unwrap().routes[0].system,
            None,
            "ambiguous .iso"
        );
        assert_eq!(
            game_for_file(&gba).unwrap().routes[0].system.as_deref(),
            Some("gba")
        );
        let pc = game_for_file(&exe).unwrap();
        assert_eq!(
            (pc.store.as_str(), pc.routes[0].source_id.as_str()),
            ("Custom", "local")
        );
        assert!(pc.routes[0]
            .install_path
            .as_deref()
            .unwrap()
            .ends_with("Tool"));
        assert!(game_for_file(&txt).is_err());
        assert!(game_for_file(Path::new("relative.gba")).is_err());
        assert!(pickable_extensions().contains(&"nsp".to_string()));

        let conn = f.db.lock();
        let game = game_for_file(&gba).unwrap();
        db::save_game(&conn, &GamePatch::from(&game)).unwrap();
        let profile = emulators::new_profile(
            &f.dir.file("e/mGBA.exe"),
            catalog::known_emulator("mgba").unwrap(),
        );
        db::save_emulator(&conn, &profile).unwrap();
        assert!(set_emulator(&conn, &game.id, 0, Some("missing")).is_err());
        assert!(set_emulator(&conn, &game.id, 3, None).is_err());
        assert!(set_emulator(&conn, "ghost", 0, None).is_err());
        set_emulator(&conn, &game.id, 0, Some(&profile.id)).unwrap();
        let stored = db::get_game(&conn, &game.id).unwrap().unwrap();
        assert_eq!(
            stored.routes[0].emulator_id.as_deref(),
            Some(profile.id.as_str())
        );
        // Removing the profile puts the route back to automatic.
        db::remove_emulator(&conn, &profile.id).unwrap();
        assert_eq!(
            db::get_game(&conn, &game.id).unwrap().unwrap().routes[0].emulator_id,
            None
        );
        db::save_game(&conn, &GamePatch::from(&pc)).unwrap();
        assert!(
            set_emulator(&conn, &pc.id, 0, None).is_err(),
            "not a ROM route"
        );
    }
    #[test]
    fn mass_removals_are_held_back_and_reported() {
        let f = Fixture::new("local-mass");
        let mut files = Vec::new();
        for i in 0..30 {
            files.push(f.dir.file(&format!("Roms/NES/Game {i:02}.nes")));
        }
        let root = f.add_root("games", "Roms");
        f.scan();
        assert_eq!(f.games().len(), 30);
        // 21 of 30 vanish at once: more than half and more than 20 → held back.
        for file in &files[..21] {
            std::fs::remove_file(file).unwrap();
        }
        let summary = f.scan();
        assert_eq!(summary.games.removals_held, 21);
        assert_eq!(summary.games.removed_or_uninstalled, 0);
        assert!(summary.roots[0].warning.as_deref().unwrap().contains("21"));
        assert_eq!(f.games().len(), 30);
        let stored = crate::roots::get(&f.db.lock(), &root.id).unwrap().unwrap();
        let last = stored.last_result.unwrap();
        assert_eq!(last["games"]["removalsHeld"], 21);
        assert!(last["warning"].is_string());
        // A smaller change goes through.
        for file in &files[..21] {
            std::fs::write(file, b"x").unwrap();
        }
        for file in &files[..10] {
            std::fs::remove_file(file).unwrap();
        }
        let summary = f.scan();
        assert_eq!(summary.games.removals_held, 0);
        assert_eq!(summary.games.removed_or_uninstalled, 10);
        assert_eq!(f.games().len(), 20);
    }

    #[test]
    fn a_root_that_vanishes_before_removals_are_applied_changes_nothing() {
        let f = Fixture::new("local-vanish");
        f.dir.file("Roms/GB/Tetris.gb");
        let kirby = f.dir.file("Roms/GB/Kirby.gb");
        let root = f.add_root("games", "Roms");
        f.scan();
        std::fs::remove_file(&kirby).unwrap();
        let scan = localscan::scan_games(
            Path::new(&root.path),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        // The drive goes away between the walk and the update.
        let roms = f.dir.path().join("Roms");
        let moved = f.dir.path().join("Roms-gone");
        std::fs::rename(&roms, &moved).unwrap();
        let readable = root_still_readable(&roms);
        let (counts, warning) = apply_game_scan(&mut f.db.lock(), &root, &scan, readable).unwrap();
        std::fs::rename(&moved, &roms).unwrap();
        assert_eq!(counts.removed_or_uninstalled, 0);
        assert!(warning.is_some());
        assert_eq!(f.games().len(), 2);
    }

    #[test]
    fn a_chosen_emulator_is_user_data() {
        let f = Fixture::new("local-chosen-emu");
        let file = f.dir.file("Roms/GBA/Chosen.gba");
        f.dir.file("Roms/GBA/Other.gba");
        f.add_root("games", "Roms");
        f.scan();
        let chosen = f.game("Chosen");
        {
            let conn = f.db.lock();
            let profile = emulators::new_profile(
                &f.dir.file("e/mGBA.exe"),
                catalog::known_emulator("mgba").unwrap(),
            );
            db::save_emulator(&conn, &profile).unwrap();
            set_emulator(&conn, &chosen.id, 0, Some(&profile.id)).unwrap();
        }
        std::fs::remove_file(file).unwrap();
        f.scan();
        let kept = f.game("Chosen");
        assert_eq!(kept.routes[0].installed, Some(false), "kept, not installed");
        assert!(kept.routes[0].emulator_id.is_some());
    }

    #[test]
    fn url_shortcuts_store_validated_links_only() {
        let f = Fixture::new("local-url");
        let good = f.dir.write(
            "Stuff/Portal.url",
            b"[InternetShortcut]\r\nURL=steam://rungameid/400\r\n",
        );
        let bad = f.dir.write(
            "Stuff/Phish.url",
            b"[InternetShortcut]\nURL=https://evil.example/\n",
        );
        let local = f.dir.write(
            "Stuff/Calc.url",
            b"[InternetShortcut]\nURL=file:///C:/Windows/System32/calc.exe\n",
        );
        let none = HashMap::new();
        let Ok(LocalFile::Game(game)) = game_for_file(&good, &none) else {
            panic!("a store link is accepted")
        };
        let route = &game.routes[0];
        assert_eq!(route.launch_target, "steam://rungameid/400");
        assert_eq!(route.app_id.as_deref(), Some(key_of(&good).as_str()));
        assert_eq!(route.install_path, None);
        assert!(game_for_file(&bad, &none).is_err());
        assert!(game_for_file(&local, &none).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn lnk_shortcuts_need_confirmation_of_target_and_arguments() {
        use crate::platform::create_shortcut;
        let f = Fixture::new("local-lnk");
        let tool = f.dir.file("Bin/tool.exe");
        let tool_text = tool.to_string_lossy().into_owned();
        f.dir.dir("Games/PC/Server");
        let lnk = f.dir.path().join("Games/PC/Server/Join.lnk");
        create_shortcut(&lnk, &tool_text, "+connect 10.0.0.1").unwrap();
        let plain = f.dir.path().join("Games/PC/Tool.lnk");
        create_shortcut(&plain, &tool_text, "").unwrap();
        f.add_root("games", "Games");
        let summary = f.scan();
        // The plain shortcut becomes a game launching the .exe; the other waits.
        let games = f.games();
        assert_eq!(games.len(), 1);
        assert_eq!(
            key_of(Path::new(&games[0].routes[0].launch_target)),
            key_of(&tool)
        );
        assert_eq!(summary.review, 1);
        let items = review::list(&f.db.lock(), false).unwrap();
        let item = &items[0];
        assert_eq!(item.kind, SHORTCUT);
        assert_eq!(item.details["arguments"], "+connect 10.0.0.1");
        assert_eq!(
            key_of(Path::new(item.details["target"].as_str().unwrap())),
            key_of(&tool)
        );
        // Rescans keep one item, add nothing.
        assert_eq!(f.scan().review, 1);
        assert_eq!(f.games().len(), 1);
        // Changed since the item was made: confirming fails, nothing is stored.
        create_shortcut(&lnk, &tool_text, "+connect 10.0.0.2").unwrap();
        assert!(resolve(&f.db, &item.id, &Resolution::ConfirmShortcut).is_err());
        assert_eq!(f.games().len(), 1);
        // Rescan shows the new arguments; confirming adds the game launching the .lnk.
        f.scan();
        let item = review::list(&f.db.lock(), false).unwrap()[0].clone();
        assert_eq!(item.details["arguments"], "+connect 10.0.0.2");
        resolve(&f.db, &item.id, &Resolution::ConfirmShortcut).unwrap();
        let server = f.game("Server");
        assert_eq!(
            key_of(Path::new(&server.routes[0].launch_target)),
            key_of(&lnk)
        );
        let again = f.scan();
        assert_eq!(
            (again.games.added, again.games.updated, again.review),
            (0, 0, 0)
        );
        assert_eq!(f.games().len(), 2);

        // addLocalGame applies the same rules.
        let confirmed = db::shortcut_confirmations(&f.db.lock()).unwrap();
        let other = f.dir.path().join("Other.lnk");
        create_shortcut(&other, &tool_text, "-x").unwrap();
        assert!(matches!(
            game_for_file(&other, &confirmed).unwrap(),
            LocalFile::NeedsConfirmation(_)
        ));
        let Ok(LocalFile::Game(g)) = game_for_file(&plain, &confirmed) else {
            panic!("plain shortcut")
        };
        assert_eq!(key_of(Path::new(&g.routes[0].launch_target)), key_of(&tool));
        let Ok(LocalFile::Game(g)) = game_for_file(&lnk, &confirmed) else {
            panic!("confirmed shortcut")
        };
        assert_eq!(key_of(Path::new(&g.routes[0].launch_target)), key_of(&lnk));
    }
}
