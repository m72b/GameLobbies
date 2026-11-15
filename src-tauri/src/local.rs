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
//! - Roots may nest across kinds (a games root holding the emulator root, or the reverse). A
//!   scan never enters another registered root nested in it (`roots::nested_roots`), so it
//!   never finds, claims or marks missing that root's games or emulators; each route keeps the
//!   `root:<id>` state of the root that found it, and reconcile and root removal only touch
//!   their own root's state.
//! - Games are only ever matched by route identity (or, in launcher games folders, the same
//!   file or folder, below); similar titles are reported as `duplicate` review items, never
//!   merged.
//!
//! Launcher games folders (a `games` root with `launcherId`, see `roots`):
//! - One scan classifies the mixed app folder: emulator executables (the emulator discovery,
//!   unless the folder is one emulator roots refuse) become profiles with the catalog's default
//!   arguments, de-duplicated by executable path as for emulator roots; PC game folders,
//!   shortcuts and ROMs are found by the mixed walk (`localscan::ScanInputs::mixed`).
//! - A launcher with a program: every game gets a route `launcher:<id>` whose target is the
//!   discovered file (or, when the launcher takes folders only, the game folder), run through the
//!   launcher's own checked argv like a game added by hand. A shortcut with arguments (or a target
//!   that isn't a plain `.exe`) still waits for the user's confirmation under Review, exactly as in
//!   label-only folders, and is checked again at every launch (`launch::check_launcher_shortcut`).
//! - A launcher without a program (label only): PC games and ROMs get the ordinary local and ROM
//!   routes above (same trust: discovered under a folder picked natively), with `launcherId` as
//!   their label. Shortcuts with arguments still wait for confirmation. A ROM whose extension
//!   several systems share is matched to the one system an enabled emulator runs; otherwise it is
//!   an `uncertain-rom` review item where the user picks the emulator.
//! - A route found there joins an existing game instead of duplicating it: the game holding the
//!   same route identity, else the one game with a route to the same file or folder. Only the
//!   route is added: title, cover, favourite, hidden, notes, sessions and playtime stay as they
//!   are. A title alone never joins games (it may be another edition): a find with the same
//!   clean title as a game is added as its own game and offered as a `duplicate` to link, with
//!   the review step (`links`).
//! - Rescans touch only routes holding this root's install state, like any root.
//! - Removing such a root keeps its games (their routes just stop belonging to the folder) or
//!   removes the routes it found ([`remove_launcher_root`]).

use crate::catalog;
use crate::db::{self, Db};
use crate::emulators::{self, EmulatorCounts};
use crate::launchers::{self, CustomLauncher, TargetKind};
use crate::localscan::{
    self, classify_shortcut, key_of, path_key, GameScan, ScanInputs, ShortcutKind,
};
use crate::review::{
    self, AMBIGUOUS_EXE, DUPLICATE, SHORTCUT, UNCERTAIN_ROM, UNREADABLE, UNSUPPORTED,
};
use crate::roots::{LibraryRoot, RootGuard};
use crate::tracker::{is_within, normalize_path};
use crate::types::{now_ms, EmulatorProfile, Game, GameRoute};
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
    /// The entry cap was reached: results were added but nothing was marked missing (see
    /// `notice`).
    pub truncated: bool,
    /// Subfolders that couldn't be read.
    pub unreadable: usize,
    /// Something the user should know although the scan worked (removals held back, the
    /// folder became unreadable before removals were applied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Not an error: the folder is so large that the scan stopped at its entry cap (see
    /// [`truncation_notice`]). Offer a full scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Whether this was a full scan (much higher limits).
    pub full: bool,
}

/// The user-facing text for a scan that stopped at its entry cap.
pub fn truncation_notice(path: &str, kind: &str, cap: usize, full: bool) -> String {
    let what = if kind == "emulators" {
        "Emulators"
    } else {
        "Games"
    };
    let cap = group_thousands(cap);
    if full {
        format!(
            "{path} has a very large number of files (more than {cap}), so even the full scan stopped early. {what} found so far were kept and nothing was marked missing. Consider adding smaller subfolders instead."
        )
    } else {
        format!(
            "{path} has a very large number of files (more than {cap}), so the quick scan stopped early. {what} found so far were kept and nothing was marked missing. A full scan can take several minutes."
        )
    }
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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
    /// Launcher games folders: routes added to games already in the library.
    #[serde(default)]
    pub routes_added: usize,
    /// ROMs found whose system an enabled emulator runs (or that were matched to one).
    #[serde(default)]
    pub roms_matched: usize,
    /// Review items this scan left open for its roots (choose an executable or emulator,
    /// confirm a shortcut, unsupported or unreadable folders).
    #[serde(default)]
    pub needs_review: usize,
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

/// A route of a launcher with a program: `launcher:<id>`, the discovered file or folder as the
/// target (exactly like a game added to the launcher by hand), this root's install state.
fn launcher_route(
    l: &CustomLauncher,
    root_id: Option<&str>,
    target: &Path,
    kind: TargetKind,
) -> GameRoute {
    let text = target.to_string_lossy().into_owned();
    GameRoute {
        source_id: launchers::source_id(&l.id),
        store: "Custom".into(),
        app_id: Some(key_of(target)),
        install_path: match kind {
            TargetKind::Folder => Some(text.clone()),
            _ => target.parent().map(|p| p.to_string_lossy().into_owned()),
        },
        launch_target: text,
        installed: Some(true),
        install_states: root_id.map(|id| [(db::root_state_key(id), true)].into()),
        ..Default::default()
    }
}

/// What a launcher with a program is given for a game: the file when it takes files, else the
/// game's folder when it takes folders. `None` when it takes neither for this game.
fn launcher_target(
    l: &CustomLauncher,
    file: &Path,
    folder: Option<&Path>,
) -> Option<(PathBuf, TargetKind)> {
    if l.allows(TargetKind::File) {
        return Some((file.to_path_buf(), TargetKind::File));
    }
    match folder {
        Some(folder) if l.allows(TargetKind::Folder) => {
            Some((folder.to_path_buf(), TargetKind::Folder))
        }
        _ => None,
    }
}

/// How one root's finds become routes (see the module docs).
#[derive(Clone, Copy)]
pub enum RootMode<'a> {
    /// An ordinary library folder.
    Library,
    /// A launcher's games folder: launcher routes when it has a program, labelled local and ROM
    /// routes when it hasn't.
    Launcher(&'a CustomLauncher),
}

impl RootMode<'_> {
    fn label(&self) -> Option<String> {
        match self {
            RootMode::Launcher(l) if launchers::is_label_only(l) => Some(l.id.clone()),
            _ => None,
        }
    }

    fn program(&self) -> Option<&CustomLauncher> {
        match self {
            RootMode::Launcher(l) if !launchers::is_label_only(l) => Some(l),
            _ => None,
        }
    }
}

fn labelled(mut route: GameRoute, label: Option<&String>) -> GameRoute {
    route.launcher_id = label.cloned();
    route
}

/// Path keys a route points at (its file or folder), for matching a find to an existing game.
/// Stored text only (no disk access: called under the database lock).
fn route_path_keys(route: &GameRoute) -> Vec<String> {
    let mut keys = Vec::new();
    for raw in [route.app_id.as_deref(), Some(route.launch_target.as_str())]
        .into_iter()
        .flatten()
    {
        if crate::emulators::drive_path_ok(raw) {
            if let Some(key) = normalize_path(raw) {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
    }
    keys
}

/// Points each new game of a launcher folder scan at the existing game it belongs to (see the
/// module docs) by giving it that game's id, so the upsert only adds its route. Returns the
/// indexes of the games that were attached that way.
fn attach_to_existing(conn: &Connection, games: &mut [Game]) -> Result<HashSet<usize>> {
    let existing = db::all_games(conn)?;
    let mut identities: HashSet<String> = HashSet::new();
    let mut by_path: HashMap<String, HashSet<String>> = HashMap::new();
    for game in &existing {
        for route in &game.routes {
            identities.insert(db::route_identity(route));
            for key in route_path_keys(route) {
                by_path.entry(key).or_default().insert(game.id.clone());
            }
        }
    }
    let mut attached = HashSet::new();
    for (index, game) in games.iter_mut().enumerate() {
        let Some(route) = game.routes.first() else {
            continue;
        };
        if identities.contains(&db::route_identity(route)) {
            continue;
        }
        let mut owners: HashSet<String> = HashSet::new();
        for key in route_path_keys(route) {
            owners.extend(by_path.get(&key).into_iter().flatten().cloned());
        }
        // Only the same file or folder joins a game. A title alone never does (it may be
        // another edition): `refresh_duplicates` offers that pair to link instead.
        if owners.len() == 1 {
            game.id = owners.into_iter().next().unwrap_or_default();
            attached.insert(index);
        }
    }
    Ok(attached)
}

/// A route that plays a file on this PC: local, ROM or custom launcher.
fn is_file_route(route: &GameRoute) -> bool {
    route.source_id == LOCAL_SOURCE
        || route.source_id == ROM_SOURCE
        || launchers::is_launcher_route(route)
}

/// Whether `profiles` (after `refresh_available`) can run `system` automatically.
fn has_emulator(profiles: &[EmulatorProfile], system: &str) -> bool {
    crate::emulators::default_for_system(profiles, system).is_some()
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
        limits: localscan::ScanLimits::default(),
        ..Default::default()
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
/// if any. (Tests; scans use [`apply_root_scan`].)
#[cfg(test)]
pub fn apply_game_scan(
    conn: &mut Connection,
    root: &LibraryRoot,
    scan: &GameScan,
    root_readable: bool,
) -> Result<(GameCounts, Option<String>)> {
    apply_root_scan(conn, root, scan, root_readable, RootMode::Library, &[])
}

/// A launcher folder scan's ROM with an extension several systems share, matched to the one
/// candidate system `profiles` can run, if exactly one.
fn matched_system(
    rom: &localscan::UncertainRom,
    profiles: &[EmulatorProfile],
) -> Option<&'static str> {
    let covered: Vec<&'static str> = rom
        .systems
        .iter()
        .copied()
        .filter(|s| has_emulator(profiles, s))
        .collect();
    match covered.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// [`apply_game_scan`] for any root: `mode` decides the routes (see the module docs); `profiles`
/// (after `refresh_available`, read without the lock) match ROMs to emulators.
pub fn apply_root_scan(
    conn: &mut Connection,
    root: &LibraryRoot,
    scan: &GameScan,
    root_readable: bool,
    mode: RootMode<'_>,
    profiles: &[EmulatorProfile],
) -> Result<(GameCounts, Option<String>)> {
    let mut counts = GameCounts {
        skipped: scan.skipped,
        failed: scan.unreadable.len(),
        ..Default::default()
    };
    let label = mode.label();
    let mut skipped_by_launcher: Vec<String> = Vec::new();
    let mut games: Vec<Game> = Vec::new();
    // Uncertain ROMs that become review items (label-only launchers).
    let mut uncertain_open: Vec<&localscan::UncertainRom> = Vec::new();
    match mode.program() {
        Some(l) => {
            let mut add = |title: &str, file: &Path, folder: Option<&Path>| match launcher_target(
                l, file, folder,
            ) {
                Some((target, kind)) => games.push(new_game(
                    "custom",
                    title,
                    "Custom",
                    launcher_route(l, Some(&root.id), &target, kind),
                )),
                None => skipped_by_launcher.push(file.to_string_lossy().into_owned()),
            };
            for rom in &scan.roms {
                add(&rom.title, &rom.path, None);
            }
            for rom in &scan.uncertain {
                add(&rom.title, &rom.path, None);
            }
            for pc in &scan.pcs {
                add(&pc.title, &pc.file, pc.folder.as_deref());
            }
            // A launcher that takes folders doesn't need the executable picked.
            if !l.allows(TargetKind::File) {
                for item in &scan.ambiguous {
                    add(&item.title, &item.folder, Some(&item.folder));
                }
            }
            // Shortcuts awaiting confirmation become review items below, in both modes, unless
            // the launcher can't be given them at all.
            for item in &scan.shortcuts {
                if launcher_target(l, &item.shortcut, item.folder.as_deref()).is_none() {
                    skipped_by_launcher.push(item.shortcut.to_string_lossy().into_owned());
                }
            }
        }
        None => {
            for rom in &scan.roms {
                games.push(new_game(
                    "rom",
                    &rom.title,
                    "Emulator",
                    labelled(rom_route(&root.id, rom), label.as_ref()),
                ));
            }
            for rom in &scan.uncertain {
                match matched_system(rom, profiles) {
                    Some(system) => {
                        let certain = localscan::RomGame {
                            system,
                            path: rom.path.clone(),
                            title: rom.title.clone(),
                            format: String::new(),
                            other_discs: Vec::new(),
                        };
                        games.push(new_game(
                            "rom",
                            &rom.title,
                            "Emulator",
                            labelled(rom_route(&root.id, &certain), label.as_ref()),
                        ));
                    }
                    None => uncertain_open.push(rom),
                }
            }
            games.extend(scan.pcs.iter().map(|pc| {
                new_game(
                    "local",
                    &pc.title,
                    "Custom",
                    labelled(
                        pc_route(Some(&root.id), pc.folder.as_deref(), &pc.source, &pc.target),
                        label.as_ref(),
                    ),
                )
            }));
        }
    }
    counts.skipped += skipped_by_launcher.len();
    for game in &games {
        let route = &game.routes[0];
        if route.source_id == ROM_SOURCE
            && route
                .system
                .as_deref()
                .is_some_and(|s| has_emulator(profiles, s))
        {
            counts.roms_matched += 1;
        }
    }
    let attached = match mode {
        RootMode::Launcher(_) => attach_to_existing(conn, &mut games)?,
        RootMode::Library => HashSet::new(),
    };
    let known = known_routes(conn)?;
    let mut found: HashSet<String> = HashSet::new();
    for (index, game) in games.iter().enumerate() {
        let identity = db::route_identity(&game.routes[0]);
        match known.get(&identity) {
            None if attached.contains(&index) => counts.routes_added += 1,
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
    let pending_identity = |source: &Path| match mode.program() {
        Some(l) => format!("{}:{}", launchers::source_id(&l.id), key_of(source)),
        None => format!("{LOCAL_SOURCE}:{}", key_of(source)),
    };
    for item in &scan.ambiguous {
        found.insert(pending_identity(&item.folder));
    }
    for item in &scan.shortcuts {
        // The route a confirmation creates: the shortcut file (or its folder) for a launcher with
        // a program, the game folder or shortcut otherwise.
        let source = match mode.program() {
            Some(l) => launcher_target(l, &item.shortcut, item.folder.as_deref())
                .map(|(target, _)| target)
                .unwrap_or_else(|| item.shortcut.clone()),
            None => item.folder.clone().unwrap_or_else(|| item.shortcut.clone()),
        };
        found.insert(pending_identity(&source));
    }
    for rom in &uncertain_open {
        found.insert(format!("{ROM_SOURCE}:{}", key_of(&rom.path)));
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
            let unreadable: Vec<String> =
                scan.unreadable.iter().filter_map(|p| path_key(p)).collect();
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
    // Launcher folders that take folders only never ask for an executable (see above).
    let ask_exe = mode.program().is_none_or(|l| l.allows(TargetKind::File));
    for item in scan
        .ambiguous
        .iter()
        .filter(|_| ask_exe)
        .take(MAX_REVIEW_PER_KIND)
    {
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
        let key = format!("{UNSUPPORTED}:{}", path_key(&item.path).unwrap_or_default());
        review::upsert(
            conn,
            UNSUPPORTED,
            &key,
            Some(&root.id),
            &json!({ "path": item.path, "reason": item.reason, "rootId": root.id }),
        )?;
        keep.insert(key);
    }
    for path in skipped_by_launcher.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!("{UNSUPPORTED}:{}", path_key(path).unwrap_or_default());
        review::upsert(
            conn,
            UNSUPPORTED,
            &key,
            Some(&root.id),
            &json!({ "path": path, "reason": "The launcher doesn't take files, and this game has no folder of its own to pass instead. Let the launcher use files to add it.", "rootId": root.id }),
        )?;
        keep.insert(key);
    }
    for rom in uncertain_open.iter().take(MAX_REVIEW_PER_KIND) {
        let key = format!("{UNCERTAIN_ROM}:{}", key_of(&rom.path));
        let emulators: Vec<Value> = profiles
            .iter()
            .filter(|p| p.enabled && rom.systems.iter().any(|s| p.systems.iter().any(|ps| ps == s)))
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "systems": p.systems.iter().filter(|s| rom.systems.contains(&s.as_str())).collect::<Vec<_>>(),
                })
            })
            .collect();
        let systems: Vec<Value> = rom
            .systems
            .iter()
            .map(|s| json!({ "id": s, "name": catalog::system(s).map_or(*s, |x| x.name) }))
            .collect();
        review::upsert(
            conn,
            UNCERTAIN_ROM,
            &key,
            Some(&root.id),
            &json!({
                "title": rom.title,
                "path": rom.path.to_string_lossy(),
                "systems": systems,
                "emulators": emulators,
                "rootId": root.id,
            }),
        )?;
        keep.insert(key);
    }
    for item in scan
        .shortcuts
        .iter()
        .filter(|item| {
            mode.program().is_none_or(|l| {
                launcher_target(l, &item.shortcut, item.folder.as_deref()).is_some()
            })
        })
        .take(MAX_REVIEW_PER_KIND)
    {
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
        let key = format!("{UNREADABLE}:{}", path_key(path).unwrap_or_default());
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
            &[
                AMBIGUOUS_EXE,
                UNSUPPORTED,
                UNREADABLE,
                SHORTCUT,
                UNCERTAIN_ROM,
            ],
            &keep,
        )?;
    }
    counts.needs_review = review::open_count_for_root(conn, &root.id)?;
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

/// A file picked as a launcher game (`launchers_add_game`): a `.lnk` that needs the user's
/// confirmation of its target and arguments first (`Some`); `None` for any other file, a plain
/// `.exe` shortcut, a store-link `.url` or a confirmed shortcut. Unusable shortcuts are refused.
/// Reads shortcuts (COM): off the async threads.
pub fn launcher_shortcut_pending(
    path: &Path,
    confirmations: &HashMap<String, (String, String)>,
) -> Result<Option<localscan::PendingShortcut>> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !matches!(
        catalog::extension_of(&name).as_deref(),
        Some(".lnk") | Some(".url")
    ) {
        return Ok(None);
    }
    match classify_shortcut(path, confirmations) {
        ShortcutKind::Exe(_) | ShortcutKind::Uri(_) | ShortcutKind::Confirmed => Ok(None),
        ShortcutKind::NeedsConfirmation { target, arguments } => {
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
            Ok(Some(localscan::PendingShortcut {
                shortcut: path.to_path_buf(),
                folder: None,
                title: catalog::clean_title(stem),
                target,
                arguments,
            }))
        }
        ShortcutKind::Invalid(reason) => bail!("{reason}"),
        ShortcutKind::Unreadable => bail!("That shortcut can't be read."),
    }
}

/// Stores a shortcut awaiting confirmation (`library.addLocalGame` of a `.lnk`).
pub fn request_shortcut_confirmation(
    conn: &Connection,
    item: &localscan::PendingShortcut,
) -> Result<()> {
    request_shortcut_confirmation_for(conn, item, None)
}

/// [`request_shortcut_confirmation`], the game to be labelled with launcher `launcher_id` once
/// confirmed (a label-only launcher's Add game).
pub fn request_shortcut_confirmation_for(
    conn: &Connection,
    item: &localscan::PendingShortcut,
    launcher_id: Option<&str>,
) -> Result<()> {
    let key = format!("{SHORTCUT}:{}", key_of(&item.shortcut));
    let mut details = shortcut_details(item, None);
    if let (Some(id), Value::Object(map)) = (launcher_id, &mut details) {
        map.insert("launcherId".into(), json!(id));
    }
    review::upsert(conn, SHORTCUT, &key, None, &details)
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
    // Groups holding a game found in a launcher's games folder, and store-only games by title:
    // a launcher folder's game never joins a store game by title alone (see
    // `attach_to_existing`), so such pairs are offered here to link.
    let mut from_launcher_folder: HashSet<(String, String)> = HashSet::new();
    let mut store_only: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (game, _) in db::games_with_user_data(conn)? {
        let title_key = catalog::title_key(&game.title);
        if !game.routes.is_empty() && !game.routes.iter().any(is_file_route) {
            if !title_key.is_empty() {
                store_only
                    .entry(title_key.clone())
                    .or_default()
                    .push((game.id.clone(), game.title.clone()));
            }
            continue;
        }
        for route in &game.routes {
            let launcher_route = launchers::is_launcher_route(route);
            if !(route.source_id == ROM_SOURCE || route.source_id == LOCAL_SOURCE || launcher_route)
                || route.installed != Some(true)
            {
                continue;
            }
            let system = match (&route.system, launcher_route) {
                (Some(system), _) => system.clone(),
                // A launcher's own routes carry no system: they are PC games.
                (None, true) => catalog::PC.to_string(),
                (None, false) => continue,
            };
            if title_key.is_empty() {
                continue;
            }
            // Only launcher routes a folder scan found (not ones added by hand) are compared
            // with store games.
            let folder_find = route
                .install_states
                .as_ref()
                .is_some_and(|s| s.keys().any(|k| k.starts_with("root:")))
                && (launcher_route || route.launcher_id.is_some());
            if folder_find && system == catalog::PC {
                from_launcher_folder.insert((system.clone(), title_key.clone()));
            }
            // A launcher route of an identifier is no file: never a duplicate of one.
            if launcher_route
                && (!folder_find || !crate::emulators::drive_path_ok(&route.launch_target))
            {
                continue;
            }
            let title_key = title_key.clone();
            groups
                .entry((system, title_key))
                .or_default()
                .entry(game.id.clone())
                .or_insert_with(|| (game.title.clone(), Vec::new()))
                .1
                .push(route.launch_target.clone());
        }
    }
    for (system, title_key) in &from_launcher_folder {
        for (id, title) in store_only.get(title_key).into_iter().flatten() {
            if let Some(group) = groups.get_mut(&(system.clone(), title_key.clone())) {
                group
                    .entry(id.clone())
                    .or_insert_with(|| (title.clone(), Vec::new()));
            }
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

/// [`scan_roots_with`] with no protected folders (tests).
#[cfg(test)]
pub fn scan_roots(
    db: &Db,
    roots: &[LibraryRoot],
    full: bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<ScanSummary> {
    scan_roots_with(db, roots, full, &RootGuard::default(), progress)
}

fn add_emulator_counts(total: &mut EmulatorCounts, counts: &EmulatorCounts) {
    total.added += counts.added;
    total.updated += counts.updated;
    total.unavailable += counts.unavailable;
}

/// A launcher games folder's emulator discovery (see the module docs): registers what it finds
/// and returns the counts and the emulator folders, which the game walk then skips. `None` when
/// emulators aren't looked for there (`guard`).
fn launcher_emulators(
    db: &Db,
    root: &LibraryRoot,
    all_roots: &[LibraryRoot],
    full: bool,
    guard: &RootGuard,
    progress: &mut dyn FnMut(Progress),
) -> Result<Option<(EmulatorCounts, HashSet<String>, usize)>> {
    if !guard.allows_emulators(&root.path) {
        return Ok(None);
    }
    let ignored = db::ignore_keys(&db.lock(), db::IGNORE_EMULATOR)?;
    let nested = crate::roots::nested_roots(root, all_roots);
    let mut report = |processed: usize, current: &Path| {
        progress(Progress {
            phase: "emulators",
            root: root.path.clone(),
            processed,
            total: None,
            current: Some(current.to_string_lossy().into_owned()),
        })
    };
    let found = emulators::discover(
        Path::new(&root.path),
        &ignored,
        &nested,
        localscan::ScanLimits::emulators(full),
        &mut report,
    )?;
    let mut counts = emulators::apply_discovered(&db.lock(), &found)?;
    let raw = db::emulator_profiles(&db.lock())?;
    let profiles = emulators::refresh_available(raw);
    counts.unavailable = emulators::unavailable_in_root(&profiles, &root.key());
    let folders = found
        .folders
        .iter()
        .map(|f| localscan::listed_key(f))
        .collect();
    Ok(Some((counts, folders, found.found.len())))
}

/// Scans `roots` (emulator roots first, then launcher games folders, which find emulators too,
/// so new games get a default emulator), applies the results and returns the summary. Disabled
/// roots are skipped. Each root's result is also stored on the root (`lastScanAt`,
/// `lastResult`). `guard` says where launcher games folders may discover emulators.
pub fn scan_roots_with(
    db: &Db,
    roots: &[LibraryRoot],
    full: bool,
    guard: &RootGuard,
    progress: &mut dyn FnMut(Progress),
) -> Result<ScanSummary> {
    let started = Instant::now();
    let mut summary = ScanSummary::default();
    // Every registered root (also ones not being scanned now), to keep nested roots apart.
    let all_roots = crate::roots::list(&db.lock())?;
    let mut ordered: Vec<&LibraryRoot> = roots.iter().filter(|r| r.enabled).collect();
    ordered.sort_by_key(|r| (r.kind != "emulators", r.launcher_id.is_none()));
    for root in ordered {
        let root_started = Instant::now();
        let root_path = PathBuf::from(&root.path);
        let mut outcome = RootOutcome {
            id: root.id.clone(),
            path: root.path.clone(),
            kind: root.kind.clone(),
            ok: true,
            full,
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
            let limits = localscan::ScanLimits::emulators(full);
            let nested = crate::roots::nested_roots(root, &all_roots);
            match emulators::discover(&root_path, &ignored, &nested, limits, &mut report) {
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
                    if found.truncated {
                        outcome.notice = Some(truncation_notice(
                            &root.path,
                            &root.kind,
                            limits.max_entries,
                            full,
                        ));
                    }
                    outcome.unreadable = found.unreadable.len();
                    root_counts = json!({ "emulators": counts, "visited": found.visited });
                }
                Err(e) => {
                    outcome.ok = false;
                    outcome.error = Some(format!("{e:#}"));
                }
            }
        } else {
            let launcher = match &root.launcher_id {
                Some(id) => match launchers::get(&db.lock(), id)? {
                    Some(l) => Some(l),
                    None => {
                        outcome.ok = false;
                        outcome.error = Some("This folder's launcher no longer exists.".into());
                        summary.roots.push(outcome);
                        continue;
                    }
                },
                None => None,
            };
            let mut inputs = scan_inputs(&db.lock())?;
            inputs.limits = localscan::ScanLimits::games(full);
            inputs.excluded = crate::roots::nested_roots(root, &all_roots);
            let mut emulator_counts = None;
            if let Some(l) = &launcher {
                inputs.mixed = true;
                if let Some(exe) = path_key(&l.executable) {
                    inputs.skip_files.insert(exe);
                }
                match launcher_emulators(db, root, &all_roots, full, guard, progress) {
                    Ok(Some((counts, folders, _))) => {
                        add_emulator_counts(&mut summary.emulators, &counts);
                        inputs.emulator_dirs = folders;
                        emulator_counts = Some(counts);
                    }
                    Ok(None) => {
                        outcome.warning = Some("Emulators aren't looked for in Downloads, the Desktop or a Temp folder. Move them to a folder of their own and add it under Library folders.".into());
                    }
                    // The game walk below reports an unreadable root.
                    Err(_) => {}
                }
            }
            let mut report = |processed: usize, current: &Path| {
                progress(Progress {
                    phase,
                    root: root.path.clone(),
                    processed,
                    total: None,
                    current: Some(current.to_string_lossy().into_owned()),
                })
            };
            match localscan::scan_games(&root_path, &inputs, &mut report) {
                Ok(scan) => {
                    // The walk may have taken a while: re-check the root, then lock.
                    let readable = root_still_readable(&root_path);
                    // Availability needs the filesystem: read it before locking.
                    let raw = db::emulator_profiles(&db.lock())?;
                    let profiles = emulators::refresh_available(raw);
                    let mode = match &launcher {
                        Some(l) => RootMode::Launcher(l),
                        None => RootMode::Library,
                    };
                    let (counts, warning) =
                        apply_root_scan(&mut db.lock(), root, &scan, readable, mode, &profiles)?;
                    if warning.is_some() {
                        outcome.warning = warning;
                    }
                    let g = &mut summary.games;
                    g.added += counts.added;
                    g.updated += counts.updated;
                    g.removed_or_uninstalled += counts.removed_or_uninstalled;
                    g.skipped += counts.skipped;
                    g.failed += counts.failed;
                    g.removals_held += counts.removals_held;
                    g.routes_added += counts.routes_added;
                    g.roms_matched += counts.roms_matched;
                    g.needs_review += counts.needs_review;
                    outcome.truncated = scan.truncated;
                    if scan.truncated && scan.visited > inputs.limits.max_entries {
                        outcome.notice = Some(truncation_notice(
                            &root.path,
                            &root.kind,
                            inputs.limits.max_entries,
                            full,
                        ));
                    }
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
                    root_counts = match emulator_counts {
                        Some(e) => json!({ "games": counts, "emulators": e }),
                        None => json!({ "games": counts }),
                    };
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

/// Removes a launcher's games folder, like deleting a launcher: `KeepGames` keeps every game and
/// route it found (they only stop belonging to the folder: its install state is dropped, the
/// launcher label stays); `RemoveGames` removes the routes it found, and games left without any
/// route. Profiles of emulators found there stay. Returns the ids of removed games.
pub fn remove_launcher_root(
    conn: &mut Connection,
    root_id: &str,
    mode: launchers::RemoveMode,
) -> Result<Vec<String>> {
    let key = db::root_state_key(root_id);
    let tx = conn.transaction()?;
    let mut removed = Vec::new();
    for game in db::all_games(&tx)? {
        let ours = |r: &GameRoute| {
            r.install_states
                .as_ref()
                .is_some_and(|s| s.contains_key(&key))
        };
        if !game.routes.iter().any(ours) {
            continue;
        }
        let routes: Vec<GameRoute> = match mode {
            launchers::RemoveMode::RemoveGames => {
                game.routes.iter().filter(|r| !ours(r)).cloned().collect()
            }
            launchers::RemoveMode::KeepGames => game
                .routes
                .iter()
                .cloned()
                .map(|mut r| {
                    if let Some(states) = r.install_states.as_mut() {
                        states.remove(&key);
                    }
                    r
                })
                .collect(),
        };
        if routes.is_empty() {
            db::delete_game_rows(&tx, &game.id)?;
            removed.push(game.id);
        } else {
            db::set_routes(&tx, &game.id, routes)?;
        }
    }
    crate::roots::delete(&tx, root_id)?;
    review::forget_root(&tx, root_id)?;
    tx.commit()?;
    Ok(removed)
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
    /// `uncertain-rom`: play the ROM with `emulatorId` (one of the item's listed emulators).
    #[serde(rename_all = "camelCase")]
    ChooseEmulator { emulator_id: String },
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

/// How a resolution's new game is routed: by the item's root (a launcher games folder routes
/// like its scan, see [`RootMode`]) or, for a shortcut added with a label-only launcher's Add
/// game, the launcher stored in the item. `None` = an ordinary library item.
fn item_launcher(conn: &Connection, item: &review::ReviewItem) -> Result<Option<CustomLauncher>> {
    let from_root = match item.root_id.as_deref() {
        Some(id) => crate::roots::get(conn, id)?.and_then(|r| r.launcher_id),
        None => None,
    };
    let id = from_root.or_else(|| item_text(item, "launcherId"));
    match id {
        Some(id) => {
            Ok(Some(launchers::get(conn, &id)?.ok_or_else(|| {
                anyhow!("This item's launcher no longer exists.")
            })?))
        }
        None => Ok(None),
    }
}

/// Stores a resolution's new game, joining an existing one for launcher items (see the module
/// docs).
fn add_resolved_game(conn: &mut Connection, mut game: Game, launcher: bool) -> Result<()> {
    if launcher {
        attach_to_existing(conn, std::slice::from_mut(&mut game))?;
    }
    db::upsert_imported_games(conn, &[game])
}

fn item_text(item: &review::ReviewItem, key: &str) -> Option<String> {
    item.details
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
}

/// The text of the native confirmation dialog shown before a `shortcut` item is confirmed
/// (`review_resolve`). It comes from the item as the scan stored it, which is exactly what
/// [`check`] then requires the file to still say, so the renderer's own display of the item
/// can't hide what will run.
pub fn shortcut_confirmation_prompt(item: &review::ReviewItem) -> Result<String> {
    if item.kind != SHORTCUT {
        bail!("Only shortcuts can be confirmed.");
    }
    let shortcut =
        item_text(item, "shortcut").ok_or_else(|| anyhow!("This item has no shortcut."))?;
    let target = item_text(item, "target").unwrap_or_default();
    let arguments = item_text(item, "arguments").unwrap_or_default();
    let arguments = if arguments.is_empty() {
        "(none)".to_string()
    } else {
        arguments
    };
    Ok(format!(
        "Add this shortcut as a game?\n\nRuns: {target}\nArguments: {arguments}\nShortcut: {}\n\nOnly continue if you recognise this program and these arguments. Game Lobby will run them every time you play this game.",
        crate::validation::display_text(&shortcut, 300)
    ))
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
            let chosen = path_key(executable)
                .and_then(|k| {
                    candidates
                        .iter()
                        .find(|c| path_key(c).as_deref() == Some(k.as_str()))
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
        Resolution::ChooseEmulator { .. } => {
            if item.kind != UNCERTAIN_ROM {
                bail!("Only uncertain ROMs need an emulator chosen.");
            }
            let path = item_text(item, "path").ok_or_else(|| anyhow!("This item has no file."))?;
            if !Path::new(&path).is_file() {
                bail!("That file no longer exists. Rescan the folder.");
            }
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

/// Ignore and Dismiss belong to the scan kinds. Library health items (`health::KINDS`) have
/// their own `health_*` commands; ignoring, say, a health duplicate here would put a present
/// game's executable on the scan ignore list.
fn only_scan_kinds(item: &review::ReviewItem, what: &str) -> Result<()> {
    if matches!(
        item.kind.as_str(),
        DUPLICATE | AMBIGUOUS_EXE | UNSUPPORTED | UNREADABLE | SHORTCUT | UNCERTAIN_ROM
    ) {
        Ok(())
    } else {
        bail!("This item can't be {what} here. Use the buttons shown on the item.")
    }
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
            only_scan_kinds(item, "ignored")?;
            let allowed: HashSet<String> = detail_paths(item)
                .iter()
                .filter_map(|p| path_key(p))
                .collect();
            let key = path_key(path)
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
            let launcher = item_launcher(conn, item)?;
            let game = match launcher.as_ref().filter(|l| !launchers::is_label_only(l)) {
                // A launcher with a program is given the chosen file, like its scan does.
                Some(l) => {
                    let (target, kind) =
                        launcher_target(l, Path::new(&chosen), Some(Path::new(&folder)))
                            .ok_or_else(|| anyhow!("{} doesn't take files or folders.", l.name))?;
                    new_game(
                        "custom",
                        &title,
                        "Custom",
                        launcher_route(l, root_id.as_deref(), &target, kind),
                    )
                }
                None => new_game(
                    "local",
                    &title,
                    "Custom",
                    labelled(
                        pc_route(
                            root_id.as_deref(),
                            Some(Path::new(&folder)),
                            Path::new(&folder),
                            &target,
                        ),
                        launcher.as_ref().map(|l| &l.id),
                    ),
                ),
            };
            add_resolved_game(conn, game, launcher.is_some())?;
            "exe-chosen"
        }
        Resolution::ChooseEmulator { emulator_id } => {
            let listed = item
                .details
                .get("emulators")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|e| e.get("id").and_then(Value::as_str) == Some(emulator_id.as_str()));
            if !listed {
                bail!("Choose one of the listed emulators.");
            }
            let profile = db::emulator_profile(conn, emulator_id)?
                .ok_or_else(|| anyhow!("That emulator profile no longer exists."))?;
            let systems: Vec<String> = item
                .details
                .get("systems")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("id").and_then(Value::as_str).map(String::from))
                .collect();
            let system = systems
                .iter()
                .find(|s| profile.systems.contains(s))
                .ok_or_else(|| {
                    anyhow!("{} doesn't run any of this file's systems.", profile.name)
                })?;
            let system =
                catalog::system(system).ok_or_else(|| anyhow!("This item's system is unknown."))?;
            let path = PathBuf::from(item_text(item, "path").unwrap_or_default());
            let title = item_text(item, "title").unwrap_or_else(|| "Untitled".into());
            let launcher = item_launcher(conn, item)?;
            let rom = localscan::RomGame {
                system: system.id,
                path,
                title: title.clone(),
                format: String::new(),
                other_discs: Vec::new(),
            };
            let mut route = match item.root_id.as_deref() {
                Some(root) => rom_route(root, &rom),
                None => {
                    let mut r = rom_route("", &rom);
                    r.install_states = None;
                    r
                }
            };
            route.launcher_id = launcher
                .as_ref()
                .filter(|l| launchers::is_label_only(l))
                .map(|l| l.id.clone());
            // The user's choice (scans never set one).
            route.emulator_id = Some(profile.id.clone());
            let game = new_game("rom", &title, "Emulator", route);
            add_resolved_game(conn, game, launcher.is_some())?;
            "emulator-chosen"
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
            let launcher = item_launcher(conn, item)?;
            let game = match launcher.as_ref().filter(|l| !launchers::is_label_only(l)) {
                // A launcher with a program is given the confirmed shortcut file (or its
                // folder), like its scan does; the launch checks the shortcut again.
                Some(l) => {
                    let (target, kind) =
                        launcher_target(l, Path::new(&shortcut), folder.as_deref())
                            .ok_or_else(|| anyhow!("{} doesn't take files or folders.", l.name))?;
                    new_game(
                        "custom",
                        &title,
                        "Custom",
                        launcher_route(l, item.root_id.as_deref(), &target, kind),
                    )
                }
                None => new_game(
                    "local",
                    &title,
                    "Custom",
                    labelled(
                        pc_route(
                            item.root_id.as_deref(),
                            folder.as_deref(),
                            &source,
                            &shortcut,
                        ),
                        launcher.as_ref().map(|l| &l.id),
                    ),
                ),
            };
            add_resolved_game(conn, game, launcher.is_some())?;
            "shortcut-confirmed"
        }
        Resolution::Dismiss => {
            only_scan_kinds(item, "dismissed")?;
            "dismissed"
        }
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
            scan_roots(&self.db, &roots, false, &mut |_| {}).unwrap()
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
                // Pokemon (mGBA) and Zelda (Dolphin) have an emulator; Okami (PS2) hasn't.
                roms_matched: 2,
                needs_review: 1,
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
        // The native confirmation shows the shortcut, its program and its arguments.
        let prompt = shortcut_confirmation_prompt(item).unwrap();
        assert!(
            prompt.contains(&format!(
                "Shortcut: {}\n",
                item.details["shortcut"].as_str().unwrap()
            )),
            "{prompt}"
        );
        assert!(prompt.contains(r"Server\Join.lnk"), "{prompt}");
        assert!(
            prompt.contains(item.details["target"].as_str().unwrap()),
            "{prompt}"
        );
        assert!(prompt.contains("Arguments: +connect 10.0.0.1"), "{prompt}");
        let not_a_shortcut = review::ReviewItem {
            kind: AMBIGUOUS_EXE.into(),
            ..item.clone()
        };
        assert!(shortcut_confirmation_prompt(&not_a_shortcut).is_err());
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
    #[cfg(windows)]
    #[test]
    fn a_hidden_system_library_scans_as_a_parent_and_as_a_system_folder_root() {
        let f = Fixture::new("local-library");
        crate::localscan::tests::sample_library(&f.dir);
        f.add_root("games", "Game Library");
        let summary = f.scan();
        assert!(summary.roots[0].ok && !summary.roots[0].truncated);
        assert_eq!(summary.roots[0].notice, None);
        assert_eq!(summary.games.added, 9, "6 ROM/folder games and 3 PC games");
        let titles: Vec<String> = f.games().into_iter().map(|g| g.title).collect();
        for title in [
            "Cairn",
            "ObraDinn",
            "Rogue Legacy",
            "Sly Cooper: Thieves in Time",
            "Sly 3 Honor Among Thieves",
        ] {
            assert!(titles.contains(&title.to_string()), "{title} in {titles:?}");
        }
        // The two Castlevania files are separate games and one duplicate item.
        let items = review::list(&f.db.lock(), false).unwrap();
        assert!(items.iter().any(|i| i.kind == DUPLICATE));
        // A title the user typed always wins over the executable's.
        let cairn = f.game("Cairn");
        db::save_game(
            &f.db.lock(),
            &GamePatch {
                title: Some("Cairn (my copy)".into()),
                ..GamePatch::new(cairn.id.clone())
            },
        )
        .unwrap();
        let again = f.scan();
        assert_eq!((again.games.added, again.games.updated), (0, 0));
        assert_eq!(f.game("Cairn (my copy)").id, cairn.id);

        // The system folder itself as the root.
        let g = Fixture::new("local-library-system");
        crate::localscan::tests::sample_library(&g.dir);
        g.add_root("games", "Game Library/Sony Playstation");
        g.add_root("games", "Game Library/PC Game");
        let summary = g.scan();
        assert_eq!(summary.games.added, 5, "2 PlayStation games and 3 PC games");
        assert!(g.games().iter().any(|game| game.title == "ObraDinn"));
    }

    #[test]
    fn truncation_is_a_notice_not_an_error() {
        let quick = truncation_notice(r"C:\Retro\Emulators", "emulators", 200_000, false);
        assert!(quick.contains("more than 200,000"), "{quick}");
        assert!(quick.contains("quick scan stopped early"));
        assert!(quick.contains("Emulators found so far were kept"));
        assert!(quick.contains("full scan can take several minutes"));
        assert!(!quick.contains("cut short"));
        let full = truncation_notice(r"D:\Roms", "games", 3_000_000, true);
        assert!(full.contains("more than 3,000,000") && full.contains("even the full scan"));
        assert!(full.contains("Games found so far were kept"));
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
    }
    /// 8.3 short forms (`LONGGA~1`) come in from env vars, pickers and typed paths; they must
    /// match the long form stored by scans.
    #[cfg(windows)]
    #[test]
    fn short_form_paths_match_their_long_form() {
        use crate::platform::short_path;
        let f = Fixture::new("local-short-form");
        f.dir.file("Long Games Folder/PC/Mystery Game/alpha.exe");
        let beta = f.dir.file("Long Games Folder/PC/Mystery Game/beta.exe");
        let tetris = f.dir.file("Long Games Folder/GB/Tetris.gb");
        f.dir
            .file("Long Games Folder/GB/Unreadable Subfolder/Kirby.gb");
        let root = f.add_root("games", "Long Games Folder");
        f.scan();
        assert_eq!(f.games().len(), 2);
        let short_beta = short_path(&beta);
        assert_eq!(key_of(&short_beta), key_of(&beta));
        // ChooseExe with the short form of a listed candidate.
        let item = review::list(&f.db.lock(), false)
            .unwrap()
            .into_iter()
            .find(|i| i.kind == AMBIGUOUS_EXE)
            .unwrap();
        resolve(
            &f.db,
            &item.id,
            &Resolution::ChooseExe {
                executable: short_beta.to_string_lossy().into(),
            },
        )
        .unwrap();
        assert!(f.game("Mystery Game").routes[0]
            .launch_target
            .ends_with("beta.exe"));
        // Unreadable protection given in short form still protects the long-form routes.
        let mystery = f.dir.path().join("Long Games Folder/PC/Mystery Game");
        let sub = f
            .dir
            .path()
            .join("Long Games Folder/GB/Unreadable Subfolder");
        let scan = GameScan {
            roms: vec![localscan::RomGame {
                system: "gb",
                path: tetris,
                title: "Tetris".into(),
                format: ".gb".into(),
                other_discs: vec![],
            }],
            pcs: vec![localscan::PcGame {
                folder: Some(mystery.clone()),
                source: mystery.clone(),
                target: beta.to_string_lossy().into(),
                file: beta.clone(),
                title: "Mystery Game".into(),
            }],
            unreadable: vec![short_path(&sub).to_string_lossy().into()],
            ..Default::default()
        };
        let (counts, _) = apply_game_scan(&mut f.db.lock(), &root, &scan, true).unwrap();
        assert_eq!(counts.removed_or_uninstalled, 0);
        assert_eq!(f.game("Kirby").routes[0].installed, Some(true));
        // The ignore list matches either form too.
        assert_eq!(
            path_key(&short_path(&sub).to_string_lossy()),
            path_key(&sub.to_string_lossy())
        );
    }
    fn scan_only(f: &Fixture, root: &LibraryRoot) -> ScanSummary {
        let fresh = crate::roots::get(&f.db.lock(), &root.id).unwrap().unwrap();
        scan_roots(&f.db, &[fresh], false, &mut |_| {}).unwrap()
    }

    fn installed_titles(f: &Fixture) -> Vec<String> {
        let mut titles: Vec<String> = f
            .games()
            .into_iter()
            .filter(|g| g.routes.iter().any(|r| r.installed == Some(true)))
            .map(|g| g.title)
            .collect();
        titles.sort();
        titles
    }

    /// A typical layout: the games root `Game Library` holds the emulator root
    /// `Game Library\Launcher\Emulators`.
    #[test]
    fn a_games_root_may_contain_the_emulator_root_and_each_scan_skips_the_other() {
        let f = Fixture::new("local-nested");
        f.dir.file("Game Library/Nintendo 64/Mario 64.z64");
        f.dir.file("Game Library/Sega Genesis/Sonic.md");
        f.dir
            .file("Game Library/Launcher/Emulators/Dolphin/Dolphin.exe");
        // Inside the emulator root: things a game scan would pick up if it went in there.
        f.dir
            .file("Game Library/Launcher/Emulators/Dolphin/Sys/Wii/banner.iso");
        f.dir
            .file("Game Library/Launcher/Emulators/Nintendo 64/Hidden.z64");
        f.dir.file("Game Library/Launcher/Launcher.DesktopApp.exe");
        f.dir.file("Game Library/Comic Books/00390/Volume 01.cbz");
        let emus = f.add_root("emulators", "Game Library/Launcher/Emulators");
        let games = f.add_root("games", "Game Library");

        let first = f.scan();
        assert!(first
            .roots
            .iter()
            .all(|r| r.ok && !r.truncated && r.unreadable == 0));
        assert_eq!(first.emulators.added, 1);
        assert_eq!(first.games.added, 2);
        assert_eq!(installed_titles(&f), ["Mario 64", "Sonic"]);
        let items = review::list(&f.db.lock(), false).unwrap();
        assert!(
            items
                .iter()
                .all(|i| !i.details.to_string().contains("Launcher")),
            "no review item for the folder holding the emulator root: {items:?}"
        );
        // Every route belongs to the games root only.
        for game in f.games() {
            let states = game.routes[0].install_states.clone().unwrap();
            assert_eq!(
                states.keys().collect::<Vec<_>>(),
                [&db::root_state_key(&games.id)]
            );
        }
        let profiles = db::emulator_profiles(&f.db.lock()).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Dolphin");

        // Rescanning either root alone leaves the other's results alone.
        let only_emus = scan_only(&f, &emus);
        assert_eq!(only_emus.emulators.added, 0);
        assert_eq!(installed_titles(&f), ["Mario 64", "Sonic"]);
        let only_games = scan_only(&f, &games);
        assert_eq!(
            (
                only_games.games.added,
                only_games.games.removed_or_uninstalled
            ),
            (0, 0)
        );
        assert_eq!(db::emulator_profiles(&f.db.lock()).unwrap().len(), 1);

        // A disabled emulator root is still skipped by the game scan.
        crate::roots::set_enabled(&f.db.lock(), &emus.id, false).unwrap();
        f.scan();
        assert_eq!(installed_titles(&f), ["Mario 64", "Sonic"]);

        // Removing the emulator root doesn't touch the games; its folder then becomes part
        // of the games root (the next scan finds what is in it).
        remove_root(&mut f.db.lock(), &emus.id).unwrap();
        assert_eq!(installed_titles(&f), ["Mario 64", "Sonic"]);
        assert_eq!(db::emulator_profiles(&f.db.lock()).unwrap().len(), 1);
    }

    /// The other way round: an emulator root holding a games root.
    #[test]
    fn an_emulator_root_may_contain_a_games_root_and_never_searches_it() {
        let f = Fixture::new("local-nested-reverse");
        f.dir.file("Box/Dolphin/Dolphin.exe");
        f.dir.file("Box/Games/Nintendo 64/Mario 64.z64");
        f.dir.file("Box/Games/Nintendo 64/rpcs3.exe");
        f.dir.file("Box/Games/PC/Some Game/Game.exe");
        let games = f.add_root("games", "Box/Games");
        let emus = f.add_root("emulators", "Box");
        let summary = f.scan();
        assert!(summary
            .roots
            .iter()
            .all(|r| r.ok && !r.truncated && r.unreadable == 0));
        let profiles = db::emulator_profiles(&f.db.lock()).unwrap();
        let ids: Vec<&str> = profiles
            .iter()
            .filter_map(|p| p.detected_as.as_deref())
            .collect();
        assert_eq!(
            ids,
            ["dolphin"],
            "rpcs3.exe inside the games root is not an emulator"
        );
        assert_eq!(installed_titles(&f), ["Mario 64", "Some Game"]);
        // Rescans and removal of the emulator root leave the games root's games alone.
        scan_only(&f, &emus);
        scan_only(&f, &games);
        assert_eq!(installed_titles(&f), ["Mario 64", "Some Game"]);
        remove_root(&mut f.db.lock(), &emus.id).unwrap();
        assert_eq!(installed_titles(&f), ["Mario 64", "Some Game"]);
        // Removing the games root affects only its own games.
        remove_root(&mut f.db.lock(), &games.id).unwrap();
        assert!(installed_titles(&f).is_empty());
        assert_eq!(db::emulator_profiles(&f.db.lock()).unwrap().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Launcher games folders

    /// A launcher with a program (`exe`) or, with `None`, a label-only one.
    fn save_launcher(f: &Fixture, exe: Option<&Path>, kinds: &[&str]) -> CustomLauncher {
        let l = launchers::validate(CustomLauncher {
            id: if exe.is_some() { "prog" } else { "app" }.into(),
            name: if exe.is_some() { "Playnite" } else { "Arcade" }.into(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#4f8cff".into(),
            executable: exe
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default(),
            args_template: if exe.is_some() {
                "--start {target}"
            } else {
                ""
            }
            .into(),
            target_kind: kinds[0].into(),
            target_kinds: kinds.iter().map(|k| k.to_string()).collect(),
            working_dir: None,
            sort_order: 0,
            created_at: 1,
            available: false,
        })
        .unwrap();
        launchers::save(&f.db.lock(), &l).unwrap();
        l
    }

    fn launcher_root(f: &Fixture, l: &CustomLauncher, rel: &str) -> LibraryRoot {
        let path = f.dir.dir(rel);
        crate::roots::add_for_launcher(
            &f.db.lock(),
            &l.id,
            &path.to_string_lossy(),
            &RootGuard::default(),
        )
        .unwrap()
    }

    /// The target scenario: one app folder with PC games, a shortcut, emulators and ROMs.
    fn app_folder(f: &Fixture) {
        for rel in [
            "App/App.exe",
            "App/Celeste/Celeste.exe",
            "App/Hades/x64/Hades.exe",
            "App/Witcher/bin/x64/witcher3.exe",
            "App/Games/Ori/ori.exe",
            "App/Games/Hollow Knight/hollow_knight.exe",
            "App/Tools/red.exe",
            "App/Tools/blue.exe",
            "App/Emulators/Dolphin/Dolphin.exe",
            "App/Emulators/DuckStation/duckstation-qt.exe",
            "App/Emulators/mGBA/mGBA.exe",
            "App/Roms/GBA/Pokemon Emerald (USA).gba",
            "App/Roms/Metroid Fusion.gba",
            "App/Roms/Crash Bandicoot.chd",
            "App/Roms/Wind Waker.rvz",
            "App/Roms/readme.md",
            "App/Empty/data.bin",
        ] {
            f.dir.file(rel);
        }
        f.dir.write(
            "App/Portal.url",
            b"[InternetShortcut]\r\nURL=steam://rungameid/400\r\n",
        );
        let red = f.dir.path().join("App/Tools/red.exe");
        crate::platform::create_shortcut(
            &f.dir.path().join("App/Server.lnk"),
            &red.to_string_lossy(),
            "+connect 10.0.0.1",
        )
        .unwrap();
    }

    fn open_items(f: &Fixture) -> Vec<review::ReviewItem> {
        review::list(&f.db.lock(), false).unwrap()
    }

    fn item(f: &Fixture, kind: &str) -> review::ReviewItem {
        open_items(f).into_iter().find(|i| i.kind == kind).unwrap()
    }

    fn titles(f: &Fixture) -> Vec<String> {
        let mut t: Vec<String> = f.games().into_iter().map(|g| g.title).collect();
        t.sort();
        t
    }

    #[test]
    fn a_label_only_app_folder_is_classified_and_labelled() {
        let f = Fixture::new("launcher-label");
        app_folder(&f);
        let l = save_launcher(&f, None, &["file"]);
        let root = launcher_root(&f, &l, "App");
        let summary = f.scan();
        assert!(summary.roots[0].ok, "{:?}", summary.roots[0]);
        assert_eq!(summary.emulators.added, 3, "Dolphin, DuckStation, mGBA");
        assert_eq!(
            titles(&f),
            [
                "Celeste",
                "Crash Bandicoot",
                "Hades",
                "Hollow Knight",
                "Metroid Fusion",
                "Ori",
                "Pokemon Emerald",
                "Portal",
                "Witcher"
            ],
            "emulator folders, the app's own .exe and containers are never games"
        );
        let g = summary.games;
        assert_eq!((g.added, g.routes_added, g.updated), (9, 0, 0));
        // Pokemon and Metroid (mGBA), Crash (.chd: only DuckStation's PS1 has an emulator).
        assert_eq!(g.roms_matched, 3);
        // Tools (two programs), Wind Waker (.rvz: GameCube or Wii, both Dolphin), Server.lnk
        // (arguments) and Empty (nothing usable).
        assert_eq!(g.needs_review, 4, "{:#?}", open_items(&f));
        let kinds: HashSet<String> = open_items(&f).into_iter().map(|i| i.kind).collect();
        for kind in [AMBIGUOUS_EXE, UNCERTAIN_ROM, SHORTCUT, UNSUPPORTED] {
            assert!(kinds.contains(kind), "{kind}");
        }
        assert!(summary
            .unsupported_samples
            .iter()
            .any(|s| s.contains("App.exe") && s.contains("own folder")));
        // Every route: the ordinary local or ROM route, labelled with the launcher and owned by
        // the root.
        for game in f.games() {
            let route = &game.routes[0];
            assert_eq!(route.launcher_id.as_deref(), Some("app"), "{}", game.title);
            assert!(route.source_id == LOCAL_SOURCE || route.source_id == ROM_SOURCE);
            assert_eq!(
                route
                    .install_states
                    .as_ref()
                    .unwrap()
                    .get(&db::root_state_key(&root.id)),
                Some(&true)
            );
            assert_eq!(route.emulator_id, None, "scans never choose an emulator");
        }
        assert_eq!(
            f.game("Crash Bandicoot").routes[0].system.as_deref(),
            Some("ps1")
        );
        assert!(f.game("Hades").routes[0]
            .launch_target
            .ends_with(r"x64\Hades.exe"));
        assert!(f.game("Witcher").routes[0]
            .launch_target
            .ends_with(r"bin\x64\witcher3.exe"));
        assert_eq!(
            f.game("Portal").routes[0].launch_target,
            "steam://rungameid/400"
        );
        let profiles = db::emulator_profiles(&f.db.lock()).unwrap();
        for p in &profiles {
            // Discovered profiles use the catalog's checked default arguments.
            let known = catalog::known_emulator(p.detected_as.as_deref().unwrap()).unwrap();
            assert_eq!(p.args, known.default_args);
            crate::launch::check_template(&p.args, &p.executable).unwrap();
        }

        // The uncertain ROM: the user picks Dolphin (one of the listed emulators only).
        let uncertain = item(&f, UNCERTAIN_ROM);
        assert_eq!(uncertain.details["emulators"].as_array().unwrap().len(), 1);
        let dolphin = profiles.iter().find(|p| p.name == "Dolphin").unwrap();
        let other = profiles.iter().find(|p| p.name == "mGBA").unwrap();
        assert!(resolve(
            &f.db,
            &uncertain.id,
            &Resolution::ChooseEmulator {
                emulator_id: other.id.clone()
            }
        )
        .is_err());
        resolve(
            &f.db,
            &uncertain.id,
            &Resolution::ChooseEmulator {
                emulator_id: dolphin.id.clone(),
            },
        )
        .unwrap();
        let waker = f.game("Wind Waker").routes[0].clone();
        assert_eq!(waker.emulator_id.as_deref(), Some(dolphin.id.as_str()));
        assert_eq!(waker.system.as_deref(), Some("gc"));
        assert_eq!(waker.launcher_id.as_deref(), Some("app"));
        // The ambiguous folder: choosing among the listed candidates only.
        let tools = item(&f, AMBIGUOUS_EXE);
        assert!(resolve(
            &f.db,
            &tools.id,
            &Resolution::ChooseExe {
                executable: r"C:\Windows\System32\cmd.exe".into()
            }
        )
        .is_err());
        let red = tools.details["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find(|c| c.ends_with("red.exe"))
            .unwrap()
            .to_string();
        resolve(&f.db, &tools.id, &Resolution::ChooseExe { executable: red }).unwrap();
        let tools_game = f.game("Tools").routes[0].clone();
        assert_eq!(
            (
                tools_game.source_id.as_str(),
                tools_game.launcher_id.as_deref()
            ),
            (LOCAL_SOURCE, Some("app"))
        );

        // Rescan: nothing new, nothing duplicated (games, routes, emulators, review items).
        let again = f.scan();
        let g = again.games;
        assert_eq!(
            (g.added, g.routes_added, g.updated, g.removed_or_uninstalled),
            (0, 0, 0, 0)
        );
        assert_eq!((again.emulators.added, again.emulators.updated), (0, 0));
        assert_eq!(db::emulator_profiles(&f.db.lock()).unwrap().len(), 3);
        assert_eq!(f.games().len(), 11);
        assert_eq!(
            f.game("Wind Waker").routes.len(),
            1,
            "the chosen emulator is kept"
        );
        assert_eq!(
            f.game("Wind Waker").routes[0].emulator_id.as_deref(),
            Some(dolphin.id.as_str())
        );
        assert_eq!(
            again.games.needs_review, 2,
            "Server.lnk and Empty are still open"
        );
    }

    #[test]
    fn a_launcher_with_a_program_gets_launcher_routes_for_every_find() {
        let f = Fixture::new("launcher-program");
        app_folder(&f);
        let exe = f.dir.path().join("App/App.exe");
        let l = save_launcher(&f, Some(&exe), &["file"]);
        let root = launcher_root(&f, &l, "App");
        let summary = f.scan();
        assert_eq!(summary.emulators.added, 3);
        let g = summary.games;
        // 6 PC games + 4 ROMs (Wind Waker too: the launcher decides how to run it). Server.lnk
        // has arguments: it waits for confirmation like everywhere else.
        assert_eq!(g.added, 10, "{:?}", titles(&f));
        assert_eq!(
            g.needs_review, 3,
            "Tools (pick the file), Server.lnk (confirm) and Empty"
        );
        assert!(!titles(&f).contains(&"Server".to_string()));
        for game in f.games() {
            let route = &game.routes[0];
            assert_eq!(route.source_id, "launcher:prog", "{}", game.title);
            assert_eq!(route.launcher_id, None);
            assert!(
                Path::new(&route.launch_target).is_file(),
                "{}",
                route.launch_target
            );
            assert!(route
                .install_states
                .as_ref()
                .unwrap()
                .contains_key(&db::root_state_key(&root.id)));
            // Exactly what a launch checks and runs.
            let launch = launchers::prepare_launch(&l, route).unwrap();
            assert_eq!(launch.argv, ["--start", route.launch_target.as_str()]);
            assert_eq!(launch.program, exe);
        }
        // A store-link .url is a file the launcher may be given as it is (checked at launch).
        let portal = f.game("Portal").routes[0].clone();
        assert!(portal.launch_target.ends_with("Portal.url"));
        crate::launch::check_launcher_shortcut(&portal.launch_target, &HashMap::new()).unwrap();
        // Confirming the shortcut gives a launcher route with the shortcut file.
        let pending = item(&f, SHORTCUT);
        let lnk = pending.details["shortcut"].as_str().unwrap().to_string();
        // Unconfirmed, a launch refuses it.
        assert!(crate::launch::check_launcher_shortcut(&lnk, &HashMap::new()).is_err());
        resolve(&f.db, &pending.id, &Resolution::ConfirmShortcut).unwrap();
        let server = f.game("Server").routes[0].clone();
        assert_eq!(
            (server.source_id.as_str(), server.launch_target.as_str()),
            ("launcher:prog", lnk.as_str())
        );
        assert!(server
            .install_states
            .as_ref()
            .unwrap()
            .contains_key(&db::root_state_key(&root.id)));
        let confirmed = db::shortcut_confirmations(&f.db.lock()).unwrap();
        crate::launch::check_launcher_shortcut(&lnk, &confirmed).unwrap();
        // Changed after the confirmation: refused at launch.
        let red = f.dir.path().join("App/Tools/red.exe");
        crate::platform::create_shortcut(Path::new(&lnk), &red.to_string_lossy(), "+exec evil.cfg")
            .unwrap();
        let err = crate::launch::check_launcher_shortcut(&lnk, &confirmed).unwrap_err();
        assert!(err.to_string().contains("confirm"), "{err}");
        crate::platform::create_shortcut(
            Path::new(&lnk),
            &red.to_string_lossy(),
            "+connect 10.0.0.1",
        )
        .unwrap();
        assert_eq!(
            f.scan().games.added,
            0,
            "the confirmed shortcut is kept on rescan"
        );
        assert_eq!(f.game("Server").routes.len(), 1);
        assert!(
            !titles(&f).contains(&"App".to_string()),
            "the launcher's own program"
        );
        let tools = item(&f, AMBIGUOUS_EXE);
        let blue = tools.details["candidates"][0].as_str().unwrap().to_string();
        resolve(
            &f.db,
            &tools.id,
            &Resolution::ChooseExe {
                executable: blue.clone(),
            },
        )
        .unwrap();
        let route = f.game("Tools").routes[0].clone();
        assert_eq!(
            (route.source_id.as_str(), route.launch_target.as_str()),
            ("launcher:prog", blue.as_str())
        );
        assert_eq!(f.scan().games.added, 0, "idempotent");

        // A launcher that takes folders gets each game's folder instead (and no executable
        // has to be picked).
        let f = Fixture::new("launcher-folders");
        app_folder(&f);
        let l = save_launcher(&f, Some(&exe), &["folder"]);
        launcher_root(&f, &l, "App");
        let summary = f.scan();
        assert!(f.game("Tools").routes[0].launch_target.ends_with("Tools"));
        assert!(f.game("Celeste").routes[0]
            .launch_target
            .ends_with("Celeste"));
        // Loose ROMs and shortcuts have no folder of their own: reported, not guessed.
        assert!(f
            .games()
            .iter()
            .all(|g| Path::new(&g.routes[0].launch_target).is_dir()));
        assert!(summary.games.skipped > 0);
        assert!(open_items(&f).iter().all(|i| i.kind != AMBIGUOUS_EXE));
    }

    #[test]
    fn a_hand_picked_shortcut_with_arguments_needs_confirmation() {
        use crate::platform::create_shortcut;
        let f = Fixture::new("launcher-hand-lnk");
        let exe = f.dir.file("Launcher/Launcher.exe");
        let tool = f.dir.file("Games/Tool/tool.exe");
        let with_args = f.dir.path().join("Games/Server.lnk");
        create_shortcut(&with_args, &tool.to_string_lossy(), "+connect 1.2.3.4").unwrap();
        let plain = f.dir.path().join("Games/Plain.lnk");
        create_shortcut(&plain, &tool.to_string_lossy(), "").unwrap();
        let rom = f.dir.file("Games/x.gba");
        let none = HashMap::new();
        // Other files, plain .exe shortcuts and store links need nothing.
        assert!(launcher_shortcut_pending(&rom, &none).unwrap().is_none());
        assert!(launcher_shortcut_pending(&plain, &none).unwrap().is_none());
        let bad_url = f.dir.write(
            "Games/Site.url",
            b"[InternetShortcut]\r\nURL=https://example.com/\r\n",
        );
        assert!(launcher_shortcut_pending(&bad_url, &none).is_err());
        let pending = launcher_shortcut_pending(&with_args, &none)
            .unwrap()
            .unwrap();
        assert_eq!(pending.arguments, "+connect 1.2.3.4");
        // As `launchers_add_game` does: a review item naming the launcher, then the user confirms.
        let l = save_launcher(&f, Some(&exe), &["file"]);
        request_shortcut_confirmation_for(&f.db.lock(), &pending, Some(&l.id)).unwrap();
        assert!(
            f.games().is_empty(),
            "nothing is added before the confirmation"
        );
        let item = item(&f, SHORTCUT);
        resolve(&f.db, &item.id, &Resolution::ConfirmShortcut).unwrap();
        let route = f.game("Server").routes[0].clone();
        assert_eq!(route.source_id, "launcher:prog");
        assert_eq!(route.launch_target, with_args.to_string_lossy());
        assert_eq!(
            route.install_states, None,
            "picked by hand: no folder owns it"
        );
        let confirmed = db::shortcut_confirmations(&f.db.lock()).unwrap();
        assert!(launcher_shortcut_pending(&with_args, &confirmed)
            .unwrap()
            .is_none());
    }

    #[test]
    fn existing_games_get_another_route_and_keep_their_data() {
        let f = Fixture::new("launcher-existing");
        app_folder(&f);
        // A Steam game with the same title, and a game added by hand from the same .exe.
        let steam = Game {
            id: "steam:504230".into(),
            title: "Celeste".into(),
            store: "Steam".into(),
            routes: vec![GameRoute {
                source_id: "steam".into(),
                store: "Steam".into(),
                app_id: Some("504230".into()),
                launch_target: "steam://rungameid/504230".into(),
                installed: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let hades_exe = f.dir.path().join("App/Hades/x64/Hades.exe");
        let LocalFile::Game(hades) = game_for_file(&hades_exe, &HashMap::new()).unwrap() else {
            panic!("an .exe is a game");
        };
        // A local game with the same title elsewhere (a title alone never joins: offered to link).
        let other_ori = f.dir.file("Elsewhere/Ori.exe");
        let LocalFile::Game(ori) = game_for_file(&other_ori, &HashMap::new()).unwrap() else {
            panic!("an .exe is a game");
        };
        {
            let mut conn = f.db.lock();
            db::upsert_imported_games(&mut conn, &[steam.clone(), hades.clone(), ori.clone()])
                .unwrap();
            db::save_game(
                &conn,
                &GamePatch {
                    favorite: Some(true),
                    ..GamePatch::new(ori.id.clone())
                },
            )
            .unwrap();
            db::save_game(
                &conn,
                &GamePatch {
                    title: Some("Celeste (my copy)".into()),
                    favorite: Some(true),
                    hidden: Some(true),
                    cover: Some("lobby-cover://c.png".into()),
                    playtime_seconds: Some(7_200),
                    ..GamePatch::new("steam:504230")
                },
            )
            .unwrap();
            crate::notes::set_notes(&conn, "steam:504230", "my notes").unwrap();
        }
        let before = db::get_game(&f.db.lock(), "steam:504230").unwrap().unwrap();
        let l = save_launcher(&f, None, &["file"]);
        let root = launcher_root(&f, &l, "App");
        let summary = f.scan();
        // Hades: the same file joins. Ori and Celeste only share a clean title with a game (it
        // may be another edition): each is added as its own game and offered as a duplicate.
        assert_eq!(summary.games.routes_added, 1);
        assert_eq!(summary.games.added, 8);
        let hades_now = f.game("Hades");
        assert_eq!(hades_now.id, hades.id);
        assert_eq!(hades_now.routes.len(), 2);
        assert_eq!(hades_now.routes[0], hades.routes[0]);
        assert_eq!(hades_now.routes[1].launcher_id.as_deref(), Some("app"));
        let oris: Vec<Game> = f
            .games()
            .into_iter()
            .filter(|g| g.title == ori.title)
            .collect();
        assert_eq!(oris.len(), 2, "never merged by title");
        let ori_mine = oris.iter().find(|g| g.id == ori.id).unwrap();
        assert_eq!(ori_mine.routes, ori.routes);
        assert!(ori_mine.favorite, "user data kept");
        let ori_found = oris.iter().find(|g| g.id != ori.id).unwrap().clone();
        assert!(ori_found.routes[0]
            .launch_target
            .ends_with(r"Games\Ori\ori.exe"));
        let ori_duplicate = open_items(&f)
            .into_iter()
            .find(|i| i.kind == DUPLICATE && duplicate_ids(i).contains(&ori.id))
            .expect("the same title as a game played from a file is offered as a duplicate");
        resolve(
            &f.db,
            &ori_duplicate.id,
            &Resolution::Link {
                target_game_id: Some(ori.id.clone()),
            },
        )
        .unwrap();
        let ori_now = db::get_game(&f.db.lock(), &ori.id).unwrap().unwrap();
        assert_eq!(ori_now.routes.len(), 2);
        assert_eq!(ori_now.routes[0], ori.routes[0]);
        assert_eq!(ori_now.routes[1], ori_found.routes[0]);
        assert!(ori_now.favorite, "user data kept");
        // The store-only game is not touched at all.
        let celeste = db::get_game(&f.db.lock(), "steam:504230").unwrap().unwrap();
        assert_eq!(celeste, before);
        assert_eq!(
            crate::notes::load(&f.db.lock(), "steam:504230")
                .unwrap()
                .notes,
            "my notes"
        );
        let found = f.game("Celeste");
        assert_eq!(found.routes[0].launcher_id.as_deref(), Some("app"));
        let duplicate = open_items(&f)
            .into_iter()
            .find(|i| i.kind == DUPLICATE && duplicate_ids(i).contains(&"steam:504230".to_string()))
            .expect("the same title as a store game is offered as a duplicate");
        let ids: HashSet<String> = duplicate_ids(&duplicate).into_iter().collect();
        assert_eq!(ids, ["steam:504230".to_string(), found.id.clone()].into());
        // Linking them keeps the store game's data and gives it the found route.
        resolve(
            &f.db,
            &duplicate.id,
            &Resolution::Link {
                target_game_id: Some("steam:504230".into()),
            },
        )
        .unwrap();
        let celeste = db::get_game(&f.db.lock(), "steam:504230").unwrap().unwrap();
        assert_eq!(celeste.routes.len(), 2);
        assert_eq!(celeste.routes[0], before.routes[0]);
        assert_eq!(
            (
                celeste.title.as_str(),
                celeste.cover.as_deref(),
                celeste.favorite
            ),
            ("Celeste (my copy)", Some("lobby-cover://c.png"), true)
        );
        refresh_duplicates(&f.db.lock()).unwrap();
        assert!(open_items(&f).iter().all(|i| i.kind != DUPLICATE));

        // Rescans are idempotent; a vanished file is marked not installed (the game has data).
        let again = f.scan().games;
        assert_eq!((again.added, again.routes_added, again.updated), (0, 0, 0));
        std::fs::remove_file(f.dir.path().join("App/Games/Ori/ori.exe")).unwrap();
        let summary = f.scan();
        assert_eq!(summary.games.removed_or_uninstalled, 1);
        let ori_now = f.game(&ori.title);
        assert_eq!(ori_now.routes[1].installed, Some(false));
        assert_eq!(
            ori_now.routes[0], ori.routes[0],
            "the hand-added route is untouched"
        );

        // Removing the folder, keeping its games: the routes stay, no longer owned by it.
        let keep = Fixture::new("launcher-remove-keep");
        app_folder(&keep);
        let l = save_launcher(&keep, None, &["file"]);
        let r = launcher_root(&keep, &l, "App");
        keep.scan();
        let count = keep.games().len();
        remove_launcher_root(&mut keep.db.lock(), &r.id, launchers::RemoveMode::KeepGames).unwrap();
        assert_eq!(keep.games().len(), count);
        assert!(keep.games().iter().all(|g| g.routes[0]
            .install_states
            .as_ref()
            .is_none_or(|s| !s.contains_key(&db::root_state_key(&r.id)))
            && g.routes[0].launcher_id.as_deref() == Some("app")));
        assert!(crate::roots::list(&keep.db.lock()).unwrap().is_empty());
        assert!(open_items(&keep).is_empty());

        // Removing it with its games: only routes it found go (and games left without any).
        remove_launcher_root(
            &mut f.db.lock(),
            &root.id,
            launchers::RemoveMode::RemoveGames,
        )
        .unwrap();
        let celeste = db::get_game(&f.db.lock(), "steam:504230").unwrap().unwrap();
        assert_eq!(celeste.routes, before.routes[..1]);
        let hades_now = f.game("Hades");
        assert_eq!(hades_now.routes, hades.routes, "the hand-added route stays");
        assert_eq!(f.game(&ori.title).routes, ori.routes);
        let mut expected = vec![
            "Celeste (my copy)".to_string(),
            "Hades".into(),
            ori.title.clone(),
        ];
        expected.sort();
        assert_eq!(titles(&f), expected);
    }

    #[test]
    fn deleting_a_label_only_launcher_handles_its_folders_and_labels() {
        for mode in [
            launchers::RemoveMode::KeepGames,
            launchers::RemoveMode::RemoveGames,
        ] {
            let f = Fixture::new("launcher-delete");
            app_folder(&f);
            let l = save_launcher(&f, None, &["file"]);
            let root = launcher_root(&f, &l, "App");
            f.scan();
            let count = f.games().len();
            assert_eq!(
                launchers::games_of(&f.db.lock(), "app").unwrap().len(),
                count
            );
            launchers::remove(&f.db.lock(), "app", mode)
                .unwrap()
                .unwrap();
            assert!(crate::roots::list(&f.db.lock()).unwrap().is_empty());
            assert!(open_items(&f)
                .iter()
                .all(|i| i.root_id.as_deref() != Some(root.id.as_str())));
            match mode {
                launchers::RemoveMode::KeepGames => {
                    assert_eq!(f.games().len(), count);
                    for game in f.games() {
                        let route = &game.routes[0];
                        assert_eq!(route.launcher_id, None);
                        assert!(route.install_states.as_ref().unwrap().is_empty());
                        assert_eq!(route.installed, Some(true));
                    }
                }
                launchers::RemoveMode::RemoveGames => assert!(f.games().is_empty()),
            }
        }
    }

    #[test]
    fn emulators_in_downloads_are_not_discovered_from_an_app_folder() {
        let f = Fixture::new("launcher-guard");
        app_folder(&f);
        let l = save_launcher(&f, None, &["file"]);
        launcher_root(&f, &l, "App");
        let guard = RootGuard {
            emulator_trees: vec![normalize_path(&f.dir.path().to_string_lossy()).unwrap()],
            ..Default::default()
        };
        let roots = crate::roots::list(&f.db.lock()).unwrap();
        let summary = scan_roots_with(&f.db, &roots, false, &guard, &mut |_| {}).unwrap();
        assert_eq!(summary.emulators.added, 0);
        assert!(summary.roots[0]
            .warning
            .as_deref()
            .unwrap()
            .contains("Downloads"));
        // Their folders are still never games.
        assert!(!titles(&f)
            .iter()
            .any(|t| t == "Dolphin" || t == "Emulators"));
    }

    #[test]
    fn the_scan_never_starts_a_process() {
        // The scan, its walks and their review resolutions never start, open or shell-execute
        // anything (structural, like `launch_dispatch_never_uses_a_shell`).
        for (name, source) in [
            ("local.rs", include_str!("local.rs")),
            ("localscan.rs", include_str!("localscan.rs")),
            ("emulators.rs", include_str!("emulators.rs")),
            ("roots.rs", include_str!("roots.rs")),
            ("review.rs", include_str!("review.rs")),
        ] {
            let code = &source[..source.find("mod tests {").unwrap_or(source.len())];
            for forbidden in [
                "Command::new",
                "spawn_detached",
                "shell_open",
                "ShellExecute",
                "CreateProcess",
                "launch_game",
                "open_path",
                "opener",
            ] {
                assert!(!code.contains(forbidden), "{name} uses {forbidden}");
            }
        }
    }

    #[test]
    fn ignore_and_dismiss_are_refused_on_health_items() {
        let f = Fixture::new("local-health-kinds");
        let exe = f.dir.file("Games/Hades/hades.exe");
        let details = serde_json::json!({ "path": exe.to_string_lossy() });
        for kind in crate::health::KINDS {
            review::upsert(&f.db.lock(), kind, &format!("test:{kind}"), None, &details).unwrap();
        }
        let items = review::list(&f.db.lock(), false).unwrap();
        assert_eq!(items.len(), crate::health::KINDS.len());
        let path = exe.to_string_lossy().into_owned();
        for item in &items {
            assert!(resolve(&f.db, &item.id, &Resolution::Dismiss).is_err());
            assert!(resolve(&f.db, &item.id, &Resolution::Ignore { path: path.clone() }).is_err());
        }
        // Nothing was ignored and every item is still open.
        assert_eq!(
            review::list(&f.db.lock(), false).unwrap().len(),
            items.len()
        );
        assert!(db::ignores(&f.db.lock(), db::IGNORE_PATH)
            .unwrap()
            .is_empty());
    }
}
