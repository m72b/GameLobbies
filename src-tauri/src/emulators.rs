//! Emulator profiles: discovery in emulator library roots, status, and default selection.
//!
//! Discovery walks an emulator root (at most [`MAX_DEPTH`] folders deep, at most
//! [`MAX_ENTRIES`] entries, never following links or junctions, skipping hidden/system
//! folders) for executables named in `catalog::EMULATORS`. Recognition is by file name only,
//! so discovery is conservative. Results are merged into the `emulators` table by
//! [`apply_discovered`]:
//! - de-duplicated by normalized executable path (a manual profile for the same file counts);
//! - an existing profile keeps everything the user can edit (name, args, systems, enabled);
//! - a second executable recognised as an emulator that already has a profile is added
//!   disabled, so a stray copy never takes over launches;
//! - executables the user removed (`scan_ignore` kind `emulator`) are never added again;
//! - missing executables are reported unavailable, never deleted.
//!
//! `available` needs the filesystem, so it is filled in by [`refresh_available`] after the
//! database lock is released; `db::emulator_profiles` leaves it false.

use crate::catalog::{self, KnownEmulator};
use crate::db;
use crate::localscan::{walk, WalkLimits, WalkOutcome};
use crate::tracker::{is_within, normalize_path};
use crate::types::{now_ms, EmulatorProfile};
use anyhow::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub const MAX_DEPTH: usize = 4;
pub const MAX_ENTRIES: usize = 50_000;

/// Fills the output-only fields: `available` (absolute `.exe` that exists) and
/// `needsConfiguration` (a catalog template that still has its placeholder). Touches the
/// filesystem: never call it while holding the database lock.
pub fn with_status(mut p: EmulatorProfile) -> EmulatorProfile {
    p.available = executable_ok(&p.executable);
    p.needs_configuration = needs_configuration(&p);
    p
}

/// [`with_status`] for a list (call after releasing the database lock).
pub fn refresh_available(profiles: Vec<EmulatorProfile>) -> Vec<EmulatorProfile> {
    profiles.into_iter().map(with_status).collect()
}

/// An absolute drive-letter path ending in `.exe` (syntax only).
pub fn executable_path_ok(executable: &str) -> bool {
    let bytes = executable.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && executable.to_lowercase().ends_with(".exe")
        && normalize_path(executable).is_some()
}

/// An absolute, existing `.exe` file.
pub fn executable_ok(executable: &str) -> bool {
    executable_path_ok(executable) && Path::new(executable).is_file()
}

pub fn needs_configuration(p: &EmulatorProfile) -> bool {
    p.detected_as
        .as_deref()
        .and_then(catalog::known_emulator)
        .is_some_and(|known| known.needs_configuration)
        && p.args.contains(catalog::RETROARCH_CORE_PLACEHOLDER)
}

/// Whether a profile may be picked automatically for `system`.
fn usable_for(p: &EmulatorProfile, system: &str) -> bool {
    p.enabled && p.available && !p.needs_configuration && p.systems.iter().any(|s| s == system)
}

/// Ranking among usable profiles, fully deterministic: manual profiles first (the user set
/// them up on purpose), then the oldest profile (a newly found executable never displaces one
/// already in use), then catalog preference, name and executable path.
fn rank(p: &EmulatorProfile, system: &str) -> (usize, i64, usize, String, String) {
    let manual = usize::from(p.source != "manual");
    let preference = p
        .detected_as
        .as_deref()
        .and_then(|id| catalog::preference(id, system))
        .unwrap_or(50);
    (
        manual,
        p.added_at,
        preference,
        p.name.to_lowercase(),
        p.executable.to_lowercase(),
    )
}

/// The best enabled, available, configured profile that runs `system`. `profiles` must have
/// had [`refresh_available`] applied.
pub fn default_for_system<'a>(
    profiles: &'a [EmulatorProfile],
    system: &str,
) -> Option<&'a EmulatorProfile> {
    profiles
        .iter()
        .filter(|p| usable_for(p, system))
        .min_by(|a, b| rank(a, system).cmp(&rank(b, system)))
}

/// One executable found in an emulator root.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub known: &'static KnownEmulator,
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub found: Vec<Found>,
    pub unreadable: Vec<String>,
    pub truncated: bool,
}

/// Walks an emulator root. Errors only when the root itself can't be read.
pub fn discover(
    root: &Path,
    ignored: &std::collections::HashSet<String>,
    progress: &mut dyn FnMut(usize, &Path),
) -> Result<Discovery> {
    let mut out = Discovery::default();
    let limits = WalkLimits {
        max_depth: MAX_DEPTH,
        max_entries: MAX_ENTRIES,
    };
    let outcome: WalkOutcome = walk(root, &limits, progress, &mut |entry| {
        if entry.is_dir {
            return true;
        }
        if let Some(known) = catalog::emulator_for_executable(&entry.name) {
            let key = normalize_path(&entry.path.to_string_lossy()).unwrap_or_default();
            if !ignored.contains(&key) {
                out.found.push(Found {
                    path: entry.path.to_path_buf(),
                    known,
                });
            }
        }
        true
    })?;
    out.unreadable = outcome.unreadable;
    out.truncated = outcome.truncated;
    out.found
        .sort_by_key(|f| f.path.to_string_lossy().to_lowercase());
    Ok(out)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct EmulatorCounts {
    pub added: usize,
    pub updated: usize,
    pub unavailable: usize,
}

/// Merges one root's discovery into the profiles table (see the module docs). Leaves
/// `unavailable` at 0: count it with [`unavailable_in_root`] after releasing the lock.
pub fn apply_discovered(conn: &Connection, discovery: &Discovery) -> Result<EmulatorCounts> {
    let mut counts = EmulatorCounts::default();
    let ignored = db::ignore_keys(conn, db::IGNORE_EMULATOR)?;
    let mut profiles = db::emulator_profiles(conn)?;
    let mut seen = std::collections::HashSet::new();
    for found in &discovery.found {
        let display = found.path.to_string_lossy().into_owned();
        let Some(key) = normalize_path(&display) else {
            continue;
        };
        if ignored.contains(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let existing = profiles
            .iter()
            .find(|p| normalize_path(&p.executable).as_deref() == Some(key.as_str()));
        match existing {
            Some(p) => {
                // Only fill in what the user can't have chosen: the recognised emulator.
                if p.detected_as.is_none() {
                    let mut next = p.clone();
                    next.detected_as = Some(found.known.id.into());
                    if next.systems.is_empty() {
                        next.systems = found.known.systems.iter().map(|s| s.to_string()).collect();
                    }
                    db::save_emulator(conn, &next)?;
                    counts.updated += 1;
                }
            }
            None => {
                let mut profile = new_profile(&found.path, found.known);
                if profiles
                    .iter()
                    .any(|p| p.detected_as.as_deref() == Some(found.known.id))
                {
                    profile.enabled = false;
                }
                profiles = db::save_emulator(conn, &profile)?;
                counts.added += 1;
            }
        }
    }
    Ok(counts)
}

/// Profiles (after [`refresh_available`]) inside the normalized `root` whose executable is gone.
pub fn unavailable_in_root(profiles: &[EmulatorProfile], root: &str) -> usize {
    profiles
        .iter()
        .filter(|p| !p.available)
        .filter(|p| normalize_path(&p.executable).is_some_and(|exe| is_within(root, &exe)))
        .count()
}

/// A new discovered profile with the catalog defaults.
pub fn new_profile(path: &Path, known: &KnownEmulator) -> EmulatorProfile {
    EmulatorProfile {
        id: format!("emu:{}", uuid::Uuid::new_v4()),
        name: known.name.into(),
        executable: path.to_string_lossy().into_owned(),
        args: known.default_args.into(),
        rom_folders: Vec::new(),
        extensions: Vec::new(),
        systems: known.systems.iter().map(|s| s.to_string()).collect(),
        enabled: true,
        source: "discovered".into(),
        detected_as: Some(known.id.into()),
        available: true,
        needs_configuration: false,
        added_at: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;

    fn profile(id: &str, source: &str, detected: Option<&str>, exe: &str) -> EmulatorProfile {
        EmulatorProfile {
            id: id.into(),
            name: id.into(),
            executable: exe.into(),
            args: "{rom}".into(),
            rom_folders: vec![],
            extensions: vec![],
            systems: vec!["gba".into(), "gb".into()],
            enabled: true,
            source: source.into(),
            detected_as: detected.map(Into::into),
            available: true,
            needs_configuration: false,
            added_at: 0,
        }
    }

    fn listed(conn: &Connection) -> Vec<EmulatorProfile> {
        refresh_available(db::emulator_profiles(conn).unwrap())
    }

    #[test]
    fn discovery_dedupes_keeps_edits_and_honours_tombstones() {
        let dir = TempDir::new("emu-discovery");
        for f in [
            "Dolphin-x64/Dolphin.exe",
            "Dolphin-x64/DolphinTool.exe",
            "pcsx2/pcsx2-qt.exe",
            "RetroArch/retroarch.exe",
            "a/b/c/d/mGBA.exe",      // depth 4 folders: found
            "a/b/c/d/e/Ryujinx.exe", // too deep
            ".hidden/xemu.exe",
        ] {
            dir.file(f);
        }
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let root = normalize_path(&dir.path().to_string_lossy()).unwrap();
        let none = std::collections::HashSet::new();
        let found = discover(dir.path(), &none, &mut |_, _| {}).unwrap();
        let names: Vec<_> = found.found.iter().map(|f| f.known.id).collect();
        // Sorted by path, case-insensitively ("a/b/c/d/mGBA.exe" first).
        assert_eq!(
            names,
            ["mgba", "dolphin", "pcsx2", "retroarch"],
            "{names:?}"
        );

        let counts = apply_discovered(&conn, &found).unwrap();
        assert_eq!((counts.added, counts.updated), (4, 0));
        // The database listing never touches the filesystem.
        assert!(db::emulator_profiles(&conn)
            .unwrap()
            .iter()
            .all(|p| !p.available));
        let list = listed(&conn);
        assert_eq!(unavailable_in_root(&list, &root), 0);
        let retro = list.iter().find(|p| p.name == "RetroArch").unwrap();
        assert!(retro.needs_configuration && retro.available);
        assert_eq!(retro.source, "discovered");
        assert!(retro.added_at > 0);

        // The user edits Dolphin and removes PCSX2.
        let mut dolphin = list.iter().find(|p| p.name == "Dolphin").unwrap().clone();
        dolphin.name = "My Dolphin".into();
        dolphin.args = "-e {rom}".into();
        dolphin.systems = vec!["gc".into()];
        dolphin.enabled = false;
        db::save_emulator(&conn, &dolphin).unwrap();
        let pcsx2 = list.iter().find(|p| p.name == "PCSX2").unwrap();
        assert!(db::remove_emulator(&conn, &pcsx2.id).unwrap());

        // Rescan: nothing added or changed, nothing resurrected.
        let ignored = db::ignore_keys(&conn, db::IGNORE_EMULATOR).unwrap();
        let again = discover(dir.path(), &ignored, &mut |_, _| {}).unwrap();
        assert_eq!(again.found.len(), 3);
        let counts = apply_discovered(&conn, &again).unwrap();
        assert_eq!((counts.added, counts.updated), (0, 0));
        // Even if the walk returned it, the tombstone wins.
        apply_discovered(&conn, &found).unwrap();
        let list = listed(&conn);
        assert_eq!(list.len(), 3);
        let kept = list.iter().find(|p| p.id == dolphin.id).unwrap();
        assert_eq!(
            (kept.name.as_str(), kept.args.as_str(), kept.enabled),
            ("My Dolphin", "-e {rom}", false)
        );
        assert_eq!(kept.systems, ["gc"]);
        assert_eq!(kept.added_at, dolphin.added_at, "addedAt never changes");

        // Missing executable: unavailable, not deleted.
        std::fs::remove_file(dir.path().join("a/b/c/d/mGBA.exe")).unwrap();
        let after = discover(dir.path(), &ignored, &mut |_, _| {}).unwrap();
        apply_discovered(&conn, &after).unwrap();
        let list = listed(&conn);
        assert_eq!(list.len(), 3);
        assert_eq!(unavailable_in_root(&list, &root), 1);
        assert!(!list.iter().find(|p| p.name == "mGBA").unwrap().available);

        // A manual profile pointing at the same executable is not duplicated.
        let db2 = db::Db::memory().unwrap();
        let conn2 = db2.lock();
        let exe = dir.path().join("Dolphin-x64").join("dolphin.EXE");
        let manual = profile("manual", "manual", None, &exe.to_string_lossy());
        db::save_emulator(&conn2, &manual).unwrap();
        let counts = apply_discovered(&conn2, &again).unwrap();
        // mGBA and RetroArch are new; the manual Dolphin profile is only completed.
        assert_eq!((counts.added, counts.updated), (2, 1));
        let list = listed(&conn2);
        assert_eq!(list.len(), 3);
        let m = list.iter().find(|p| p.id == "manual").unwrap();
        assert_eq!(m.detected_as.as_deref(), Some("dolphin"));
        assert_eq!(m.systems, ["gba", "gb"], "user systems kept");
    }

    #[test]
    fn a_second_copy_of_a_known_emulator_is_added_disabled() {
        let dir = TempDir::new("emu-second-copy");
        dir.file("Dolphin/Dolphin.exe");
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let none = std::collections::HashSet::new();
        apply_discovered(&conn, &discover(dir.path(), &none, &mut |_, _| {}).unwrap()).unwrap();
        let first = listed(&conn);
        assert!(first[0].enabled);
        // A look-alike appears later (say, in another folder of the root).
        dir.file("Other/dolphin.exe");
        let counts =
            apply_discovered(&conn, &discover(dir.path(), &none, &mut |_, _| {}).unwrap()).unwrap();
        assert_eq!(counts.added, 1);
        let list = listed(&conn);
        let copy = list.iter().find(|p| p.id != first[0].id).unwrap();
        assert!(!copy.enabled, "the new copy is disabled");
        assert_eq!(default_for_system(&list, "gc").unwrap().id, first[0].id);
    }

    #[test]
    fn default_emulator_selection_is_deterministic() {
        let dir = TempDir::new("emu-default");
        let exe = |n: &str| dir.file(n).to_string_lossy().into_owned();
        let mut profiles = vec![
            with_status(profile(
                "retro",
                "discovered",
                Some("retroarch"),
                &exe("r/retroarch.exe"),
            )),
            with_status(profile(
                "mesen",
                "discovered",
                Some("mesen"),
                &exe("m/Mesen.exe"),
            )),
            with_status(profile(
                "mgba",
                "discovered",
                Some("mgba"),
                &exe("g/mGBA.exe"),
            )),
        ];
        profiles[0].args = "-L cores\\CORE_NAME_libretro.dll {rom}".into();
        profiles[0] = with_status(profiles[0].clone());
        assert!(profiles[0].needs_configuration);
        // Same age: catalog preference decides.
        assert_eq!(default_for_system(&profiles, "gb").unwrap().id, "mgba");
        // An older profile wins over a newer, more preferred one.
        profiles[1].added_at = 1;
        profiles[2].added_at = 2;
        assert_eq!(default_for_system(&profiles, "gb").unwrap().id, "mesen");
        profiles[1].added_at = 0;
        profiles[2].added_at = 0;
        // Same age and preference: name, then executable path; never the random id.
        let mut twins = vec![
            with_status(profile(
                "emu:zzz",
                "discovered",
                Some("mgba"),
                &exe("b/mGBA.exe"),
            )),
            with_status(profile(
                "emu:aaa",
                "discovered",
                Some("mgba"),
                &exe("a/mGBA.exe"),
            )),
        ];
        twins[0].name = "mGBA".into();
        twins[1].name = "mGBA".into();
        assert_eq!(default_for_system(&twins, "gba").unwrap().id, "emu:aaa");
        twins.reverse();
        assert_eq!(default_for_system(&twins, "gba").unwrap().id, "emu:aaa");

        profiles[2].enabled = false;
        assert_eq!(default_for_system(&profiles, "gb").unwrap().id, "mesen");
        profiles[1].executable = dir.path().join("gone.exe").to_string_lossy().into();
        profiles[1] = with_status(profiles[1].clone());
        assert!(!profiles[1].available);
        assert!(
            default_for_system(&profiles, "gb").is_none(),
            "RetroArch unconfigured"
        );
        profiles[0].args = "-L cores\\mgba_libretro.dll {rom}".into();
        profiles[0] = with_status(profiles[0].clone());
        assert_eq!(default_for_system(&profiles, "gb").unwrap().id, "retro");
        // A manual profile wins over discovered ones.
        profiles.push(with_status(profile(
            "mine",
            "manual",
            None,
            &exe("x/my.exe"),
        )));
        assert_eq!(default_for_system(&profiles, "gba").unwrap().id, "mine");
        assert!(default_for_system(&profiles, "ps3").is_none());
        // Relative or non-.exe executables are never available.
        assert!(!executable_ok("mGBA.exe"));
        assert!(!executable_ok(&exe("x/run.bat")));
        assert!(!executable_path_ok(r"\\server\share\emu.exe"));
        assert!(executable_path_ok(r"D:\Emu\Dolphin.EXE"));
    }
}
