//! Library roots: folders scanned for emulators (`emulators`) or games (`games`), stored in
//! `library_roots`.
//!
//! A root path must be a local, absolute drive-letter folder path (`D:\Roms`): network (UNC),
//! `\\?\`/`\\.\` device paths, relative paths, `.`/`..` segments, control characters, stream
//! names (`a:b`), reserved device names (`CON`, `NUL`, ...), segments ending in a dot or space
//! and paths over [`MAX_ROOT_CHARS`] are rejected. It must exist, be a folder, not be a link
//! or junction itself, and not be on a network drive (`GetDriveTypeW` = `DRIVE_REMOTE`). The
//! path is canonicalized (links in parent folders resolved) and checked again, so a link
//! can't smuggle in a protected folder. Paths are stored with `\` separators, an upper-case
//! drive letter and no trailing separator, and compared case-insensitively. A root may not
//! repeat another root of either kind, nor contain or sit inside another root of the same
//! kind. A games root may contain or sit inside an emulator root (and the other way round);
//! each scan skips the other registered roots nested in it ([`nested_roots`]).
//!
//! Protected folders ([`RootGuard`]):
//! - refused when equal to, inside or containing the root: the Windows folder, Roaming
//!   AppData (`%APPDATA%`) and Game Lobby's own data folder;
//! - refused when equal to or containing the root (folders inside them are fine): Program
//!   Files (both), ProgramData, the user profile, Public, and Local AppData
//!   (`%LOCALAPPDATA%`, where per-user installs such as `Programs\...` live);
//! - for emulator roots only, also refused when equal, inside or containing: Downloads,
//!   Desktop and the Temp folders, which commonly hold files nobody installed on purpose.
//!
//! A launcher's games folder is a `games` root with `launcherId` set (the custom launcher it
//! belongs to): the same rules apply, and its scan also looks for emulators (unless the folder
//! is one emulator roots refuse, see [`RootGuard::allows_emulators`]) and classifies a mixed app
//! folder (`localscan::ScanInputs::mixed`).
//!
//! [`prepare`] does every filesystem check without the database; [`insert`] then only
//! compares with the stored roots. `online` is filled in by [`with_online`] after the
//! database lock is released.

use crate::tracker::{is_within, normalize_path};
use crate::types::now_ms;
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

pub const MAX_ROOT_CHARS: usize = 1_024;
pub const MAX_ROOTS_PER_KIND: usize = 64;
pub const KINDS: [&str; 2] = ["emulators", "games"];

/// A library root as sent to the renderer.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRoot {
    pub id: String,
    /// `"emulators"` or `"games"`.
    pub kind: String,
    pub path: String,
    pub enabled: bool,
    pub added_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<Value>,
    /// Output only: the folder exists right now.
    pub online: bool,
    /// The custom launcher whose games folder this is (`games` roots only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_id: Option<String>,
}

impl LibraryRoot {
    pub fn key(&self) -> String {
        self.path.to_lowercase()
    }
}

/// Folders a root may not be, contain, or (for `inside`) sit in. Normalized lower-case.
#[derive(Debug, Clone, Default)]
pub struct RootGuard {
    /// Rejected when equal to or inside a root (`C:\` contains them).
    pub containers: Vec<String>,
    /// Rejected when equal, containing or inside.
    pub trees: Vec<String>,
    pub emulator_trees: Vec<String>,
}

impl RootGuard {
    pub fn from_environment(data_dir: &Path) -> Self {
        // Long form: env vars may hold 8.3 short names (`C:\Users\RUNNER~1`).
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| crate::localscan::path_key(&v))
        };
        let mut guard = RootGuard::default();
        if let Some(windows) = env("SystemRoot").or_else(|| env("windir")) {
            guard.trees.push(windows);
        } else {
            guard.trees.push(r"c:\windows".into());
        }
        if let Some(p) = env("APPDATA") {
            guard.trees.push(p);
        }
        if let Some(p) = crate::localscan::path_key(&data_dir.to_string_lossy()) {
            guard.trees.push(p);
        }
        if let Some(profile) = env("USERPROFILE") {
            guard.emulator_trees.push(format!(r"{profile}\downloads"));
            guard.emulator_trees.push(format!(r"{profile}\desktop"));
        }
        for name in ["TEMP", "TMP"] {
            if let Some(p) = env(name) {
                guard.emulator_trees.push(p);
            }
        }
        if let Some(windows) = env("SystemRoot") {
            guard.emulator_trees.push(format!(r"{windows}\temp"));
        }
        for name in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
            "USERPROFILE",
            "LOCALAPPDATA",
            "PUBLIC",
        ] {
            if let Some(p) = env(name) {
                guard.containers.push(p);
            }
        }
        guard
    }

    /// Whether emulators may be discovered under `path` (a launcher's games folder): not in
    /// Downloads, the Desktop or a Temp folder, like emulator roots.
    pub fn allows_emulators(&self, path: &str) -> bool {
        let key = crate::localscan::path_key(path).unwrap_or_default();
        !key.is_empty() && self.check_kind(&key, "emulators").is_ok()
    }

    /// The kind-specific part of the guard.
    fn check_kind(&self, key: &str, kind: &str) -> Result<()> {
        if kind == "emulators" {
            for tree in long_forms(&self.emulator_trees) {
                if is_within(&tree, key) || is_within(key, &tree) {
                    bail!("Emulators can't be scanned from Downloads, the Desktop or a Temp folder. Move them to a folder of their own first.");
                }
            }
        }
        Ok(())
    }

    fn check(&self, key: &str) -> Result<()> {
        for tree in long_forms(&self.trees) {
            if is_within(&tree, key) || is_within(key, &tree) {
                bail!("That folder is a system or app data folder (or contains one) and can't be a library folder.");
            }
        }
        for folder in long_forms(&self.containers) {
            if is_within(key, &folder) {
                bail!("That folder is a system folder like Program Files or your user folder (or contains one). Choose a folder inside it instead.");
            }
        }
        Ok(())
    }
}

/// Each protected folder as given and in its long form (`RUNNER~1` → `runneradmin`), so a
/// guard built from short-form paths still matches the canonical (long) root path.
fn long_forms(folders: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(folders.len() * 2);
    for folder in folders {
        out.push(folder.clone());
        if let Some(long) = crate::localscan::path_key(folder).filter(|l| l != folder) {
            out.push(long);
        }
    }
    out
}

/// Syntax check and normalization: `(display path, comparison key)`. The disk is touched
/// only to expand 8.3 short names in protected folders (see [`long_forms`]).
pub fn normalize_root_path(raw: &str, guard: &RootGuard) -> Result<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Choose a folder.");
    }
    if raw.chars().count() > MAX_ROOT_CHARS {
        bail!("That folder path is too long (at most {MAX_ROOT_CHARS} characters).");
    }
    if raw.chars().any(char::is_control) {
        bail!("That folder path contains invalid characters.");
    }
    if raw.starts_with(r"\\")
        || raw.starts_with("//")
        || raw.starts_with(r"\/")
        || raw.starts_with(r"/\")
    {
        bail!("Network and device paths aren't supported. Choose a folder on a local drive.");
    }
    let bytes = raw.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !(bytes[2] == b'\\' || bytes[2] == b'/')
    {
        bail!("Use a full folder path like D:\\Games.");
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut segments = Vec::new();
    for segment in raw[3..].split(['\\', '/']).filter(|s| !s.is_empty()) {
        if segment == "." || segment == ".." {
            bail!("Folder paths can't contain . or .. segments.");
        }
        if segment
            .chars()
            .any(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        {
            bail!("That folder path contains invalid characters.");
        }
        if segment.ends_with(['.', ' ']) {
            bail!("Folder names can't end with a dot or a space.");
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
        if device {
            bail!("\"{segment}\" is a reserved Windows device name.");
        }
        segments.push(segment);
    }
    let display = if segments.is_empty() {
        format!("{drive}:\\")
    } else {
        format!("{drive}:\\{}", segments.join("\\"))
    };
    let key = normalize_path(&display).ok_or_else(|| anyhow!("That folder path is invalid."))?;
    guard.check(&key)?;
    Ok((display, key))
}

/// Overlap rules: a root may not equal another root of either kind, nor contain or sit
/// inside another root of the SAME kind. A games root may contain or sit inside an emulator
/// root (and the other way round): each scan skips the other kind's roots (see
/// [`nested_roots`]). Compared by long-form path key.
pub fn check_overlap(key: &str, kind: &str, existing: &[LibraryRoot]) -> Result<()> {
    if existing.iter().filter(|r| r.kind == kind).count() >= MAX_ROOTS_PER_KIND {
        bail!("At most {MAX_ROOTS_PER_KIND} {kind} folders can be added.");
    }
    let key = crate::localscan::path_key(key).unwrap_or_else(|| key.to_string());
    for root in existing {
        let other = root_key(root);
        if other == key {
            if root.kind == kind {
                bail!("That folder is already in the list.");
            }
            bail!(
                "That folder is already in the list as {} folder. A folder can't be both a game folder and an emulator folder.",
                if root.kind == "emulators" { "an emulator" } else { "a game" }
            );
        }
        if root.kind != kind {
            continue;
        }
        if is_within(&other, &key) {
            bail!(
                "That folder is inside {}, which is already in the list.",
                root.path
            );
        }
        if is_within(&key, &other) {
            bail!("That folder contains {}, which is already in the list. Remove it first or add a different folder.", root.path);
        }
    }
    Ok(())
}

/// A root's long-form comparison key.
pub fn root_key(root: &LibraryRoot) -> String {
    crate::localscan::path_key(&root.path).unwrap_or_else(|| root.key())
}

/// Keys of the other registered roots (any kind, enabled or not) strictly inside `root`. A
/// scan of `root` never enters them: their files belong to their own root.
pub fn nested_roots(root: &LibraryRoot, all: &[LibraryRoot]) -> std::collections::HashSet<String> {
    let key = root_key(root);
    all.iter()
        .filter(|other| other.id != root.id)
        .map(root_key)
        .filter(|other| *other != key && is_within(&key, other))
        .collect()
}

pub fn valid_kind(kind: &str) -> Result<()> {
    if !KINDS.contains(&kind) {
        bail!("Library folder kind must be \"emulators\" or \"games\".");
    }
    Ok(())
}

fn read(row: &rusqlite::Row) -> rusqlite::Result<LibraryRoot> {
    let path: String = row.get("path")?;
    Ok(LibraryRoot {
        id: row.get("id")?,
        kind: row.get("kind")?,
        // Filled in by `with_online` (no filesystem access under the database lock).
        online: false,
        path,
        enabled: row.get::<_, i64>("enabled")? != 0,
        added_at: row.get("addedAt")?,
        last_scan_at: row.get("lastScanAt")?,
        last_result: row
            .get::<_, Option<String>>("lastResult")?
            .and_then(|s| serde_json::from_str(&s).ok()),
        launcher_id: row
            .get::<_, Option<String>>("launcherId")?
            .filter(|id| !id.is_empty()),
    })
}

pub fn list(conn: &Connection) -> Result<Vec<LibraryRoot>> {
    let mut stmt = conn.prepare("SELECT * FROM library_roots ORDER BY kind,path COLLATE NOCASE")?;
    let rows = stmt.query_map([], read)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<LibraryRoot>> {
    Ok(conn
        .query_row("SELECT * FROM library_roots WHERE id=?", [id], read)
        .optional()?)
}

/// Sets `online` (the folder exists now). Call after releasing the database lock.
pub fn with_online(mut roots: Vec<LibraryRoot>) -> Vec<LibraryRoot> {
    for root in &mut roots {
        root.online = Path::new(&root.path).is_dir();
    }
    roots
}

/// A root path checked against everything but the stored roots (see the module docs):
/// `(display path, comparison key)`. Touches the filesystem; holds no database lock.
pub fn prepare(kind: &str, raw: &str, guard: &RootGuard) -> Result<(String, String)> {
    valid_kind(kind)?;
    let (display, key) = normalize_root_path(raw, guard)?;
    guard.check_kind(&key, kind)?;
    if crate::platform::is_remote_drive(&display) {
        bail!("Network drives aren't supported. Choose a folder on a local drive.");
    }
    if crate::platform::is_reparse_point(Path::new(&display)) {
        bail!("That folder is a link or junction. Choose the folder it points to instead.");
    }
    match std::fs::metadata(&display) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => bail!("That path is a file, not a folder."),
        Err(_) => bail!("That folder doesn't exist or can't be opened."),
    }
    // Links in parent folders: check where the folder really is.
    let canonical =
        std::fs::canonicalize(&display).map_err(|_| anyhow!("That folder can't be opened."))?;
    let canonical = canonical.to_string_lossy();
    if canonical.starts_with(r"\\?\UNC\") {
        bail!("Network folders aren't supported. Choose a folder on a local drive.");
    }
    let real = canonical.strip_prefix(r"\\?\").unwrap_or(&canonical);
    let (real_display, real_key) = normalize_root_path(real, guard)?;
    guard.check_kind(&real_key, kind)?;
    if crate::platform::is_remote_drive(&real_display) {
        bail!("Network drives aren't supported. Choose a folder on a local drive.");
    }
    if real_key != key {
        // Stored as typed would hide where it points; store the real location.
        return Ok((real_display, real_key));
    }
    Ok((display, key))
}

/// [`prepare`] for a launcher's games folder: a `games` root that also may not be, contain or
/// sit in Downloads, the Desktop or a Temp folder (like an emulator root), because its scan adds
/// the programs, shortcuts and emulators it finds there. Checked on the path as chosen and where
/// it really is.
pub fn prepare_launcher_folder(raw: &str, guard: &RootGuard) -> Result<(String, String)> {
    let (display, key) = prepare("games", raw, guard)?;
    let typed = normalize_root_path(raw, guard)?.1;
    for k in [typed.as_str(), key.as_str()] {
        if guard.check_kind(k, "emulators").is_err() {
            bail!("A launcher's games folder can't be in Downloads, on the Desktop or in a Temp folder (or contain one). Move the games to a folder of their own first.");
        }
    }
    Ok((display, key))
}

/// Stores a prepared root after checking it against the stored ones.
pub fn insert(conn: &Connection, kind: &str, display: &str, key: &str) -> Result<LibraryRoot> {
    insert_for(conn, kind, display, key, None)
}

/// [`insert`], optionally as the games folder of custom launcher `launcher_id` (which must exist;
/// only `games` roots).
pub fn insert_for(
    conn: &Connection,
    kind: &str,
    display: &str,
    key: &str,
    launcher_id: Option<&str>,
) -> Result<LibraryRoot> {
    valid_kind(kind)?;
    if let Some(id) = launcher_id {
        if kind != "games" {
            bail!("A launcher's folders are game folders.");
        }
        if crate::launchers::get(conn, id)?.is_none() {
            bail!("That launcher no longer exists.");
        }
    }
    check_overlap(key, kind, &list(conn)?)?;
    let id = format!("root:{}", uuid::Uuid::new_v4());
    conn.execute(
        "INSERT INTO library_roots(id,kind,path,enabled,addedAt,launcherId) VALUES(?,?,?,1,?,?)",
        params![id, kind, display, now_ms(), launcher_id],
    )?;
    get(conn, &id)?.ok_or_else(|| anyhow!("The folder could not be saved."))
}

/// [`prepare`] + [`insert`] (tests and callers that don't share the lock).
#[cfg(test)]
pub fn add(conn: &Connection, kind: &str, raw: &str, guard: &RootGuard) -> Result<LibraryRoot> {
    let (display, key) = prepare(kind, raw, guard)?;
    insert(conn, kind, &display, &key)
}

/// [`prepare`] + [`insert_for`] a launcher (tests).
#[cfg(test)]
pub fn add_for_launcher(
    conn: &Connection,
    launcher_id: &str,
    raw: &str,
    guard: &RootGuard,
) -> Result<LibraryRoot> {
    let (display, key) = prepare("games", raw, guard)?;
    insert_for(conn, "games", &display, &key, Some(launcher_id))
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<()> {
    if conn.execute(
        "UPDATE library_roots SET enabled=? WHERE id=?",
        params![enabled as i64, id],
    )? == 0
    {
        bail!("That library folder no longer exists.");
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM library_roots WHERE id=?", [id])?;
    Ok(())
}

pub fn record_result(conn: &Connection, id: &str, at: i64, result: &Value) -> Result<()> {
    conn.execute(
        "UPDATE library_roots SET lastScanAt=?,lastResult=? WHERE id=?",
        params![at, serde_json::to_string(result)?, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;

    fn guard() -> RootGuard {
        RootGuard {
            containers: vec![
                r"c:\program files".into(),
                r"c:\program files (x86)".into(),
                r"c:\users\me".into(),
            ],
            trees: vec![r"c:\windows".into(), r"c:\users\me\appdata\roaming".into()],
            emulator_trees: vec![],
        }
    }

    #[test]
    fn root_paths_are_normalized_and_bad_ones_rejected() {
        let g = guard();
        assert_eq!(
            normalize_root_path(" d:/Roms//PS2/ ", &g).unwrap(),
            (r"D:\Roms\PS2".into(), r"d:\roms\ps2".into())
        );
        assert_eq!(normalize_root_path("e:\\", &g).unwrap().0, r"E:\");
        assert!(normalize_root_path(r"C:\Program Files\RetroArch", &g).is_ok());
        assert!(normalize_root_path(r"C:\Users\me\Emulators", &g).is_ok());
        for bad in [
            "",
            "   ",
            "Roms",
            r"Roms\PS2",
            r"\Roms",
            "D:Roms",
            r"\\server\share\roms",
            "//server/share",
            r"\\?\D:\Roms",
            r"\\.\D:\Roms",
            r"D:\Roms\..\Other",
            r"D:\Roms\.\PS2",
            "D:\\Roms\0x",
            "D:\\Roms\nx",
            r"D:\Roms\a:stream",
            r"D:\Roms\what?",
            r"D:\Roms\CON",
            r"D:\Roms\nul.txt",
            r"D:\Roms\com1",
            r"D:\Roms\trailing.",
            r"D:\Roms\trailing \x",
            r"C:\",
            r"C:\Windows",
            r"c:\windows\system32",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\Users",
            r"C:\Users\me",
            r"C:\Users\me\AppData\Roaming",
            r"C:\Users\me\AppData\Roaming\Emu",
            r"C:\Users\me\AppData",
        ] {
            assert!(
                normalize_root_path(bad, &g).is_err(),
                "{bad:?} should be rejected"
            );
        }
        let long = format!(r"D:\{}", "a".repeat(MAX_ROOT_CHARS));
        assert!(normalize_root_path(&long, &g).is_err());
    }

    #[test]
    fn roots_are_unique_not_nested_and_must_exist() {
        let db = crate::db::Db::memory().unwrap();
        let conn = db.lock();
        let dir = TempDir::new("roots");
        let roms = dir.dir("Roms");
        let ps2 = dir.dir("Roms/PS2");
        let emus = dir.dir("Emulators");
        let other = dir.dir("Other");
        let file = dir.file("file.txt");
        let g = RootGuard::default();
        let added = add(&conn, "games", &roms.to_string_lossy(), &g).unwrap();
        assert!(added.enabled);
        assert!(!added.online, "online is only filled in outside the lock");
        assert!(with_online(vec![added.clone()])[0].online);
        assert_eq!(added.kind, "games");
        let upper = roms.to_string_lossy().to_uppercase();
        assert!(
            add(&conn, "games", &upper, &g).is_err(),
            "duplicate (case-insensitive)"
        );
        assert!(add(&conn, "games", &format!("{}\\", roms.display()), &g).is_err());
        assert!(
            add(&conn, "games", &ps2.to_string_lossy(), &g).is_err(),
            "nested inside"
        );
        assert!(
            add(&conn, "games", &dir.path().to_string_lossy(), &g).is_err(),
            "contains"
        );
        // The other kind: the same folder is refused, nesting is allowed both ways.
        assert!(
            add(&conn, "emulators", &roms.to_string_lossy(), &g).is_err(),
            "same folder"
        );
        let inside = add(&conn, "emulators", &ps2.to_string_lossy(), &g).unwrap();
        assert_eq!(inside.kind, "emulators", "emulators inside a games root");
        assert!(add(&conn, "emulators", &emus.to_string_lossy(), &g).is_ok());
        // Same kind still can't nest: Emulators/x inside the Emulators root.
        let nested_emus = dir.dir("Emulators/Dolphin");
        assert!(add(&conn, "emulators", &nested_emus.to_string_lossy(), &g).is_err());
        assert!(
            add(&conn, "games", &emus.to_string_lossy(), &g).is_err(),
            "games on an emulator root (same folder)"
        );
        // A games folder inside an emulator folder.
        let games_in_emus = dir.dir("Emulators/My Roms");
        assert!(add(&conn, "games", &games_in_emus.to_string_lossy(), &g).is_ok());
        // Containing a root of the other kind is fine; containing one of the same kind isn't
        // (the test folder contains both kinds).
        assert!(
            add(&conn, "emulators", &dir.path().to_string_lossy(), &g).is_err(),
            "contains an emulator root"
        );
        assert!(add(&conn, "games", &other.to_string_lossy(), &g).is_ok());
        assert!(
            add(&conn, "games", &file.to_string_lossy(), &g).is_err(),
            "a file"
        );
        let missing = dir.path().join("missing");
        assert!(add(&conn, "games", &missing.to_string_lossy(), &g).is_err());
        assert!(add(&conn, "other", &emus.to_string_lossy(), &g).is_err());
        assert_eq!(list(&conn).unwrap().len(), 5);
        let all = list(&conn).unwrap();
        let root = |kind: &str, path: &Path| {
            all.iter()
                .find(|r| r.kind == kind && root_key(r) == crate::localscan::key_of(path))
                .unwrap()
                .clone()
        };
        // Each root skips the other registered roots inside it.
        let roms_root = root("games", &roms);
        assert_eq!(
            nested_roots(&roms_root, &all),
            [crate::localscan::key_of(&ps2)].into()
        );
        let emus_root = root("emulators", &emus);
        assert_eq!(
            nested_roots(&emus_root, &all),
            [crate::localscan::key_of(&games_in_emus)].into()
        );
        assert!(nested_roots(&root("emulators", &ps2), &all).is_empty());
        set_enabled(&conn, &added.id, false).unwrap();
        assert!(!get(&conn, &added.id).unwrap().unwrap().enabled);
        assert!(set_enabled(&conn, "nope", true).is_err());
        // A data dir guard refuses the app's own folder.
        let own = RootGuard::from_environment(&emus);
        assert!(normalize_root_path(&emus.to_string_lossy(), &own).is_err());
    }

    #[test]
    fn emulator_roots_refuse_downloads_desktop_and_temp() {
        let dir = TempDir::new("roots-emu-guard");
        let downloads = dir.dir("me/Downloads");
        let inside = dir.dir("me/Downloads/Dolphin");
        let desktop = dir.dir("me/Desktop");
        let fine = dir.dir("me/Emulators");
        let key = |p: &Path| normalize_path(&p.to_string_lossy()).unwrap();
        let g = RootGuard {
            emulator_trees: vec![key(&downloads), key(&desktop)],
            ..Default::default()
        };
        for bad in [&downloads, &inside, &desktop, &dir.path().join("me")] {
            assert!(
                prepare("emulators", &bad.to_string_lossy(), &g).is_err(),
                "{bad:?}"
            );
        }
        assert!(prepare("emulators", &fine.to_string_lossy(), &g).is_ok());
        // Games roots aren't affected by the emulator-only rule.
        assert!(prepare("games", &inside.to_string_lossy(), &g).is_ok());
        // The real environment lists Temp: the test folder itself is refused for emulators.
        let real = RootGuard::from_environment(Path::new(r"C:\NoSuchGameLobbyData"));
        assert!(prepare("emulators", &fine.to_string_lossy(), &real).is_err());
    }

    #[test]
    fn launcher_folders_refuse_downloads_desktop_and_temp() {
        let dir = TempDir::new("roots-launcher-guard");
        let downloads = dir.dir("me/Downloads");
        let inside = dir.dir("me/Downloads/Arcade");
        let desktop = dir.dir("me/Desktop");
        let fine = dir.dir("me/Games/Arcade");
        let key = |p: &Path| normalize_path(&p.to_string_lossy()).unwrap();
        let g = RootGuard {
            emulator_trees: vec![key(&downloads), key(&desktop)],
            ..Default::default()
        };
        for bad in [&downloads, &inside, &desktop, &dir.path().join("me")] {
            let e = prepare_launcher_folder(&bad.to_string_lossy(), &g).unwrap_err();
            assert!(e.to_string().contains("Downloads"), "{bad:?}: {e}");
            // A plain games folder there is still fine.
            assert!(prepare("games", &bad.to_string_lossy(), &g).is_ok());
        }
        assert!(prepare_launcher_folder(&fine.to_string_lossy(), &g).is_ok());
        // The real environment lists Temp: the test folder itself is refused.
        let real = RootGuard::from_environment(Path::new(r"C:\NoSuchGameLobbyData"));
        assert!(prepare_launcher_folder(&fine.to_string_lossy(), &real).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn links_are_refused_and_resolved_before_the_guard() {
        let dir = TempDir::new("roots-links");
        let real = dir.dir("Real");
        let protected = dir.dir("Protected Folder");
        let link = dir.path().join("Link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&protected)
            .output()
            .unwrap();
        assert!(made.status.success());
        let g = RootGuard {
            trees: vec![normalize_path(&protected.to_string_lossy()).unwrap()],
            ..Default::default()
        };
        // The junction itself is refused.
        let err = prepare("games", &link.to_string_lossy(), &g).unwrap_err();
        assert!(err.to_string().contains("link"), "{err}");
        // A folder reached through a junction is checked where it really is.
        dir.dir("Protected Folder/Sub");
        assert!(prepare("games", &link.join("Sub").to_string_lossy(), &g).is_err());
        // The same protected folder given in 8.3 short form (as env vars can hold it).
        let short = RootGuard {
            trees: vec![
                normalize_path(&crate::platform::short_path(&protected).to_string_lossy()).unwrap(),
            ],
            ..Default::default()
        };
        assert!(prepare("games", &link.join("Sub").to_string_lossy(), &short).is_err());
        assert!(prepare("games", &protected.join("Sub").to_string_lossy(), &short).is_err());
        let short_sub = crate::platform::short_path(&protected.join("Sub"));
        assert!(prepare("games", &short_sub.to_string_lossy(), &g).is_err());
        assert!(prepare("games", &real.to_string_lossy(), &short).is_ok());
        let ok = prepare("games", &real.to_string_lossy(), &g).unwrap();
        assert_eq!(ok.1, normalize_path(&real.to_string_lossy()).unwrap());
        std::fs::remove_dir(&link).unwrap();
        assert!(!crate::platform::is_remote_drive(&real.to_string_lossy()));
    }
}
