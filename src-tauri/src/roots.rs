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
//! repeat, contain or sit inside any other root, of either kind.
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
    /// The last scan of this root (`RootScanResult` in TypeScript).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<Value>,
    /// Output only: the folder exists right now.
    pub online: bool,
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
    /// Like `trees`, for emulator roots only.
    pub emulator_trees: Vec<String>,
}

impl RootGuard {
    pub fn from_environment(data_dir: &Path) -> Self {
        let env = |name: &str| std::env::var(name).ok().and_then(|v| normalize_path(&v));
        let mut guard = RootGuard::default();
        if let Some(windows) = env("SystemRoot").or_else(|| env("windir")) {
            guard.trees.push(windows);
        } else {
            guard.trees.push(r"c:\windows".into());
        }
        if let Some(p) = env("APPDATA") {
            guard.trees.push(p);
        }
        if let Some(p) = normalize_path(&data_dir.to_string_lossy()) {
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

    /// The kind-specific part of the guard.
    fn check_kind(&self, key: &str, kind: &str) -> Result<()> {
        if kind == "emulators" {
            for tree in &self.emulator_trees {
                if is_within(tree, key) || is_within(key, tree) {
                    bail!("Emulators can't be scanned from Downloads, the Desktop or a Temp folder. Move them to a folder of their own first.");
                }
            }
        }
        Ok(())
    }

    fn check(&self, key: &str) -> Result<()> {
        for tree in &self.trees {
            if is_within(tree, key) || is_within(key, tree) {
                bail!("That folder is a system or app data folder (or contains one) and can't be a library folder.");
            }
        }
        for folder in &self.containers {
            if is_within(key, folder) {
                bail!("That folder is a system folder like Program Files or your user folder (or contains one). Choose a folder inside it instead.");
            }
        }
        Ok(())
    }
}

/// Syntax check and normalization (no filesystem access): `(display path, comparison key)`.
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

/// Rejects duplicates and nesting among roots of the same kind.
/// Rejects duplicates and nesting among all roots, of either kind (an emulator folder inside
/// a games folder would mix executables into a game scan, and the other way round).
pub fn check_overlap(key: &str, kind: &str, existing: &[LibraryRoot]) -> Result<()> {
    if existing.iter().filter(|r| r.kind == kind).count() >= MAX_ROOTS_PER_KIND {
        bail!("At most {MAX_ROOTS_PER_KIND} {kind} folders can be added.");
    }
    for root in existing {
        let other = root.key();
        if other == key {
            bail!("That folder is already in the list.");
        }
        if is_within(&other, key) {
            bail!(
                "That folder is inside {}, which is already in the list.",
                root.path
            );
        }
        if is_within(key, &other) {
            bail!("That folder contains {}, which is already in the list. Remove it first or add a different folder.", root.path);
        }
    }
    Ok(())
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

/// Stores a prepared root after checking it against the stored ones.
pub fn insert(conn: &Connection, kind: &str, display: &str, key: &str) -> Result<LibraryRoot> {
    valid_kind(kind)?;
    check_overlap(key, kind, &list(conn)?)?;
    let id = format!("root:{}", uuid::Uuid::new_v4());
    conn.execute(
        "INSERT INTO library_roots(id,kind,path,enabled,addedAt) VALUES(?,?,?,1,?)",
        params![id, kind, display, now_ms()],
    )?;
    get(conn, &id)?.ok_or_else(|| anyhow!("The folder could not be saved."))
}

/// [`prepare`] + [`insert`] (tests and callers that don't share the lock).
#[cfg(test)]
pub fn add(conn: &Connection, kind: &str, raw: &str, guard: &RootGuard) -> Result<LibraryRoot> {
    let (display, key) = prepare(kind, raw, guard)?;
    insert(conn, kind, &display, &key)
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
        // Roots of the other kind may not overlap either.
        assert!(
            add(&conn, "emulators", &roms.to_string_lossy(), &g).is_err(),
            "same folder"
        );
        assert!(
            add(&conn, "emulators", &ps2.to_string_lossy(), &g).is_err(),
            "inside"
        );
        assert!(
            add(&conn, "emulators", &dir.path().to_string_lossy(), &g).is_err(),
            "contains"
        );
        assert!(add(&conn, "emulators", &emus.to_string_lossy(), &g).is_ok());
        assert!(
            add(&conn, "games", &emus.to_string_lossy(), &g).is_err(),
            "games on emulators"
        );
        assert!(add(&conn, "games", &other.to_string_lossy(), &g).is_ok());
        assert!(
            add(&conn, "games", &file.to_string_lossy(), &g).is_err(),
            "a file"
        );
        let missing = dir.path().join("missing");
        assert!(add(&conn, "games", &missing.to_string_lossy(), &g).is_err());
        assert!(add(&conn, "other", &emus.to_string_lossy(), &g).is_err());
        assert_eq!(list(&conn).unwrap().len(), 3);
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

    #[cfg(windows)]
    #[test]
    fn links_are_refused_and_resolved_before_the_guard() {
        let dir = TempDir::new("roots-links");
        let real = dir.dir("Real");
        let protected = dir.dir("Protected");
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
        dir.dir("Protected/Sub");
        assert!(prepare("games", &link.join("Sub").to_string_lossy(), &g).is_err());
        let ok = prepare("games", &real.to_string_lossy(), &g).unwrap();
        assert_eq!(ok.1, normalize_path(&real.to_string_lossy()).unwrap());
        std::fs::remove_dir(&link).unwrap();
        assert!(!crate::platform::is_remote_drive(&real.to_string_lossy()));
    }
}
