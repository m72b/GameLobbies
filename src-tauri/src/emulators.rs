//! Emulator profiles: discovery in emulator library roots, status, and default selection.
//!
//! Discovery ([`discover`]) walks an emulator root within [`ScanLimits`] (quick: 5 folders deep,
//! 200,000 entries; full: 8 deep, 3,000,000), never following links or junctions, looking for
//! executables known to `catalog::EMULATORS`. Hidden/System attributes are ignored. To stay
//! fast on real emulator folders (RPCS3 or RetroArch alone can hold 50,000 files):
//! - each folder's files are checked before its subfolders; once a folder holds a known
//!   emulator executable, its subfolders are never entered (they are that emulator's data);
//! - folders named in `catalog::HEAVY_EMULATOR_DIRS` (`dev_hdd0`, `cores`, `shaders`, ...)
//!   are never entered;
//! - when one folder holds several builds of the same emulator, the preferred one (the GUI
//!   build before `nogui`) is used.
//!
//! Recognition is by file name only, so discovery is conservative. Results are merged into the
//! `emulators` table by [`apply_discovered`]:
//! - de-duplicated by normalized executable path (a manual profile for the same file counts);
//! - an existing profile keeps everything the user can edit (name, args, systems, enabled);
//! - a new profile is named after its folder when that differs from the catalog name
//!   (`Ryubing`, `Citron Neo`), so copies can be told apart;
//! - a second executable recognised as an emulator that already has a profile is added
//!   disabled, so a stray copy never takes over launches;
//! - profiles added by one scan share one `addedAt`, so catalog preference (not folder order)
//!   decides between them;
//! - executables the user removed (`scan_ignore` kind `emulator`) are never added again;
//! - missing executables are reported unavailable, never deleted.
//!
//! `available` needs the filesystem, so it is filled in by [`refresh_available`] after the
//! database lock is released; `db::emulator_profiles` leaves it false.

use crate::catalog::{self, KnownEmulator};
use crate::db;
use crate::localscan::{list_dir, listed_key, path_key, ScanLimits, MAX_UNREADABLE};
use crate::tracker::{is_within, normalize_path};
use crate::types::{now_ms, EmulatorProfile};
use anyhow::{bail, Result};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

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

/// A profile of a catalog emulator marked `needs_configuration` whose arguments are still the
/// catalog default (or still contain RetroArch's `CORE_NAME`).
pub fn needs_configuration(p: &EmulatorProfile) -> bool {
    p.detected_as
        .as_deref()
        .and_then(catalog::known_emulator)
        .is_some_and(|known| {
            known.needs_configuration
                && (p.args.contains(catalog::RETROARCH_CORE_PLACEHOLDER)
                    || p.args.trim() == known.default_args.trim())
        })
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
    /// The entry cap was reached; what was found so far is still valid.
    pub truncated: bool,
    /// Directory entries listed (files and folders).
    pub visited: usize,
}

fn is_heavy_dir(name: &str) -> bool {
    catalog::HEAVY_EMULATOR_DIRS.contains(&name.to_lowercase().as_str())
}

/// Walks an emulator root (see the module docs). `excluded`: long-form keys of other
/// registered roots inside it (`roots::nested_roots`), never entered: a games folder nested in
/// an emulator folder is never searched for emulators. Errors only when the root itself
/// can't be read.
pub fn discover(
    root: &Path,
    ignored: &HashSet<String>,
    excluded: &HashSet<String>,
    limits: ScanLimits,
    progress: &mut dyn FnMut(usize, &Path),
) -> Result<Discovery> {
    let mut out = Discovery::default();
    // Walk from the long form, so found paths (and their keys) are long.
    let long_root = crate::platform::long_path(root);
    let root = long_root.as_path();
    let first = match list_dir(root) {
        Ok(listing) => listing,
        Err(e) => bail!("{} can't be read: {e}", root.display()),
    };
    let note_unreadable = |out: &mut Discovery, path: &Path| {
        if out.unreadable.len() < MAX_UNREADABLE {
            out.unreadable.push(path.to_string_lossy().into_owned());
        }
    };
    let mut stack = vec![(root.to_path_buf(), first, 0usize)];
    while let Some((dir, listing, depth)) = stack.pop() {
        if !listing.complete {
            note_unreadable(&mut out, &dir);
        }
        if out.visited + listing.items.len() > limits.max_entries {
            out.truncated = true;
            break;
        }
        out.visited += listing.items.len();
        progress(out.visited, &dir);
        // Files first: the preferred build of each known emulator in this folder.
        let mut here: BTreeMap<&'static str, (usize, &'static KnownEmulator, PathBuf)> =
            BTreeMap::new();
        let mut emulator_folder = false;
        for item in listing.items.iter().filter(|i| !i.is_dir) {
            let Some((known, rank)) = catalog::match_executable(&item.name) else {
                continue;
            };
            emulator_folder = true;
            if ignored.contains(&normalize_path(&item.path.to_string_lossy()).unwrap_or_default()) {
                continue;
            }
            let better = here.get(known.id).is_none_or(|(r, _, _)| rank < *r);
            if better {
                here.insert(known.id, (rank, known, item.path.clone()));
            }
        }
        out.found.extend(
            here.into_values()
                .map(|(_, known, path)| Found { path, known }),
        );
        // An emulator's own folder: its subfolders are its data, never other emulators.
        if emulator_folder || depth >= limits.max_depth {
            continue;
        }
        for item in listing.items.iter().filter(|i| i.is_dir).rev() {
            if is_heavy_dir(&item.name) || excluded.contains(&listed_key(&item.path)) {
                continue;
            }
            match list_dir(&item.path) {
                Ok(children) => stack.push((item.path.clone(), children, depth + 1)),
                Err(_) => note_unreadable(&mut out, &item.path),
            }
        }
    }
    // Folder by folder ("Citron" before "Citron Neo"), so the first copy is the plain one.
    out.found.sort_by_key(|f| {
        f.path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
    });
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
    let mut seen = HashSet::new();
    let scan_time = now_ms();
    for found in &discovery.found {
        let display = found.path.to_string_lossy().into_owned();
        let Some(key) = path_key(&display) else {
            continue;
        };
        if ignored.contains(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let existing = profiles
            .iter()
            .find(|p| path_key(&p.executable).as_deref() == Some(key.as_str()));
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
                profile.added_at = scan_time;
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
        .filter(|p| path_key(&p.executable).is_some_and(|exe| is_within(root, &exe)))
        .count()
}

/// Folder names that say nothing about which emulator build lives in them.
const GENERIC_EMULATOR_FOLDERS: &[&str] = &[
    "bin",
    "x64",
    "x86",
    "x86_64",
    "win64",
    "win32",
    "windows",
    "release",
    "app",
    "emulator",
    "emulators",
    "current",
    "latest",
    "portable",
    "build",
    "program",
];

/// A profile name: the executable's folder name when it names a different build than the
/// catalog (`Ryubing` for Ryujinx.exe, `Citron Neo`), else the catalog name.
pub fn profile_name(path: &Path, known: &KnownEmulator) -> String {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().trim().to_string())
        .unwrap_or_default();
    let key = catalog::alias_key(&folder);
    if key.is_empty()
        || folder.chars().count() > 64
        || GENERIC_EMULATOR_FOLDERS.contains(&folder.to_lowercase().as_str())
        || key == catalog::alias_key(known.name)
    {
        return known.name.to_string();
    }
    folder
}

/// A new discovered profile with the catalog defaults.
pub fn new_profile(path: &Path, known: &KnownEmulator) -> EmulatorProfile {
    EmulatorProfile {
        id: format!("emu:{}", uuid::Uuid::new_v4()),
        name: profile_name(path, known),
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
            "Dolphin/Dolphin.exe",
            "Dolphin/DolphinTool.exe",
            "pcsx2/pcsx2-qt.exe",
            "RetroArch/retroarch.exe",
            "a/b/c/mGBA/mGBA.exe",           // a folder 4 levels down: found
            "a/b/c/d/e/Ryujinx/Ryujinx.exe", // 6 levels down: too deep
            ".hidden/xemu.exe",
        ] {
            dir.file(f);
        }
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let root = normalize_path(&dir.path().to_string_lossy()).unwrap();
        let none = std::collections::HashSet::new();
        let found = discover(
            dir.path(),
            &none,
            &HashSet::new(),
            ScanLimits::EMULATORS_QUICK,
            &mut |_, _| {},
        )
        .unwrap();
        let names: Vec<_> = found.found.iter().map(|f| f.known.id).collect();
        // Sorted by path, case-insensitively ("a/b/c/mGBA/mGBA.exe" first).
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
        let again = discover(
            dir.path(),
            &ignored,
            &HashSet::new(),
            ScanLimits::EMULATORS_QUICK,
            &mut |_, _| {},
        )
        .unwrap();
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
        std::fs::remove_file(dir.path().join("a/b/c/mGBA/mGBA.exe")).unwrap();
        let after = discover(
            dir.path(),
            &ignored,
            &HashSet::new(),
            ScanLimits::EMULATORS_QUICK,
            &mut |_, _| {},
        )
        .unwrap();
        apply_discovered(&conn, &after).unwrap();
        let list = listed(&conn);
        assert_eq!(list.len(), 3);
        assert_eq!(unavailable_in_root(&list, &root), 1);
        assert!(!list.iter().find(|p| p.name == "mGBA").unwrap().available);

        // A manual profile pointing at the same executable is not duplicated.
        let db2 = db::Db::memory().unwrap();
        let conn2 = db2.lock();
        let exe = dir.path().join("Dolphin").join("dolphin.EXE");
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
        apply_discovered(
            &conn,
            &discover(
                dir.path(),
                &none,
                &HashSet::new(),
                ScanLimits::EMULATORS_QUICK,
                &mut |_, _| {},
            )
            .unwrap(),
        )
        .unwrap();
        let first = listed(&conn);
        assert!(first[0].enabled);
        // A look-alike appears later (say, in another folder of the root).
        dir.file("Other/dolphin.exe");
        let counts = apply_discovered(
            &conn,
            &discover(
                dir.path(),
                &none,
                &HashSet::new(),
                ScanLimits::EMULATORS_QUICK,
                &mut |_, _| {},
            )
            .unwrap(),
        )
        .unwrap();
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
    /// The user's emulator root (`...\Playnite\Emulators`): (folder, direct entries). An entry
    /// ending in `/` is a subfolder.
    const PLAYNITE: &[(&str, &[&str])] = &[
        ("Azahar", &["azahar.exe"]),
        ("BizHawk", &["EmuHawk.exe", "dll/"]),
        ("BlueMSX", &["blueMSX.exe"]),
        ("BSNES HD", &["bsnes_hd.exe"]),
        ("Cemu", &["Cemu.exe"]),
        ("Citra", &["citra-qt.exe", "citra.exe"]),
        ("Citron", &["citron.exe"]),
        ("Citron Neo", &["citron.exe"]),
        ("DeSmuME X432R", &["DeSmuME_X432R_x64.exe"]),
        ("Dolphin", &["Dolphin.exe", "User/"]),
        (
            "DuckStation",
            &[
                "duckstation-nogui-x64-ReleaseLTCG.exe",
                "duckstation-qt-x64-ReleaseLTCG.exe",
            ],
        ),
        ("Eden", &["eden.exe"]),
        ("Eglantine", &["eden-cli.exe", "eglantine.exe"]),
        ("FCEUX", &["fceux64.exe"]),
        ("Final Burn Neo", &["fbneo64.exe"]),
        ("Kega Fusion", &["Fusion.exe"]),
        ("KytyPS5", &["kyty_emulator.exe"]),
        ("Lichii", &["eden-cli.exe", "lichii.exe"]),
        ("Lichii Frizti", &["lichii.exe"]),
        ("Lichii Sharkiix", &["lichii.exe"]),
        ("Mednafen", &["mednafen.exe"]),
        ("melonDS", &["melonDS.exe"]),
        ("Mesen", &["Backups/", "Mesen.exe"]),
        ("Mesen 2", &["Mesen.exe"]),
        ("mGBA", &["mGBA.exe"]),
        ("Orchid", &["eden-cli.exe", "orchid.exe"]),
        ("PCSX2", &["pcsx2-qtx64-avx2.exe"]),
        ("PokeDinner", &["eden-cli.exe", "eglantine.exe"]),
        ("PPSSPP", &["PPSSPPWindows64.exe"]),
        ("Project64", &["Project64.exe"]),
        ("Redream", &["redream.exe"]),
        ("RetroArch", &["cores/", "retroarch.exe"]),
        ("RPCS3", &["dev_hdd0/", "rpcs3.exe"]),
        ("Ryubing", &["Ryujinx.exe"]),
        ("Ryujinx", &["Ryujinx.exe"]),
        ("Selevet", &["selevet.exe"]),
        ("ShadPS4", &["shadPS4.exe"]),
        ("ShadPS4 GR2fork", &["GR2Launcher.exe"]),
        ("SharpEmu", &["SharpEmu.exe"]),
        ("Snes9x", &["snes9x-x64.exe"]),
        ("Visualboy Advance", &["visualboyadvance-m.exe"]),
        ("Vita3K", &["Vita3K.exe"]),
        ("Xemu", &["xemu.exe"]),
        ("Xenia", &["Emulators/", "XeniaManager.exe"]),
        ("Xenia/Emulators", &["Xenia Canary/"]),
        (
            "Xenia/Emulators/Xenia Canary",
            &["cache/", "xenia_canary.exe"],
        ),
        ("Yuzu", &["yuzu.exe"]),
        ("Shared", &["shaders/", "Tools/"]),
        ("Shared/Tools", &["updater.exe"]),
    ];

    fn playnite(dir: &TempDir) -> PathBuf {
        let root = dir.dir("Playnite/Emulators");
        for (folder, entries) in PLAYNITE {
            for entry in *entries {
                match entry.strip_suffix('/') {
                    Some(sub) => {
                        dir.dir(&format!("Playnite/Emulators/{folder}/{sub}"));
                    }
                    None => {
                        dir.file(&format!("Playnite/Emulators/{folder}/{entry}"));
                    }
                }
            }
        }
        // Heavy data trees that must never be walked.
        for i in 0..400 {
            dir.file(&format!(
                "Playnite/Emulators/RPCS3/dev_hdd0/game/BLUS{i:05}/USRDIR/EBOOT.BIN"
            ));
        }
        for i in 0..400 {
            dir.file(&format!(
                "Playnite/Emulators/RetroArch/cores/core{i:03}_libretro.dll"
            ));
        }
        for i in 0..300 {
            dir.file(&format!("Playnite/Emulators/Shared/shaders/s{i:03}/x.glsl"));
        }
        dir.file("Playnite/Emulators/Mesen/Backups/Mesen.2.1.exe");
        dir.file("Playnite/Emulators/RetroArch/cores/retroarch.exe");
        root
    }

    #[test]
    fn a_playnite_style_emulator_root_is_scanned_quickly_and_completely() {
        let dir = TempDir::new("emu-playnite");
        let root = playnite(&dir);
        let none = HashSet::new();
        let found = discover(
            &root,
            &none,
            &HashSet::new(),
            ScanLimits::EMULATORS_QUICK,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(!found.truncated);
        assert!(found.unreadable.is_empty());
        // Exactly the listed entries: every top folder, the direct entries of each listed folder,
        // nothing inside emulator folders, heavy folders or below them.
        let top = PLAYNITE.iter().filter(|(f, _)| !f.contains('/')).count();
        let listed: usize = PLAYNITE.iter().map(|(_, entries)| entries.len()).sum();
        assert_eq!(found.visited, top + listed);
        assert!(found.visited < 150);
        let mut ids: Vec<&str> = found.found.iter().map(|f| f.known.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(found.found.len(), 41, "{:?}", found.found);
        for id in [
            "azahar",
            "bizhawk",
            "bsnes",
            "cemu",
            "citra",
            "citron",
            "desmume",
            "dolphin",
            "duckstation",
            "eden",
            "eglantine",
            "fceux",
            "fbneo",
            "kega-fusion",
            "lichii",
            "mednafen",
            "melonds",
            "mesen",
            "mgba",
            "orchid",
            "pcsx2",
            "ppsspp",
            "project64",
            "redream",
            "retroarch",
            "rpcs3",
            "ryujinx",
            "selevet",
            "shadps4",
            "snes9x",
            "vbam",
            "vita3k",
            "xemu",
            "xenia-canary",
            "yuzu",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
        let path_of = |id: &str| {
            found
                .found
                .iter()
                .find(|f| f.known.id == id)
                .unwrap()
                .path
                .clone()
        };
        assert!(
            path_of("duckstation").to_string_lossy().contains("-qt-"),
            "GUI build preferred"
        );
        assert!(path_of("citra").ends_with("citra-qt.exe"));
        assert!(path_of("xenia-canary").ends_with("Xenia Canary/xenia_canary.exe"));
        assert!(path_of("retroarch").ends_with("RetroArch/retroarch.exe"));

        // Applying it: copies are disabled, folder names tell them apart, defaults follow the
        // catalog preference (not folder order).
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let counts = apply_discovered(&conn, &found).unwrap();
        assert_eq!(counts.added, 41);
        let list = refresh_available(db::emulator_profiles(&conn).unwrap());
        let by_name = |name: &str| {
            list.iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        for (enabled, disabled) in [
            ("Ryubing", "Ryujinx"),
            ("Citron", "Citron Neo"),
            ("Lichii", "Lichii Frizti"),
            ("Lichii", "Lichii Sharkiix"),
            ("Mesen", "Mesen 2"),
            ("Eglantine", "PokeDinner"),
        ] {
            assert!(by_name(enabled).enabled, "{enabled}");
            assert!(!by_name(disabled).enabled, "{disabled}");
        }
        assert_eq!(by_name("BSNES HD").detected_as.as_deref(), Some("bsnes"));
        assert_eq!(
            by_name("FinalBurn Neo").detected_as.as_deref(),
            Some("fbneo")
        );
        assert!(by_name("FinalBurn Neo").needs_configuration);
        assert!(by_name("RetroArch").needs_configuration);
        let added: HashSet<i64> = list.iter().map(|p| p.added_at).collect();
        assert_eq!(added.len(), 1, "one scan, one addedAt");
        let default = |system: &str| default_for_system(&list, system).map(|p| p.name.clone());
        assert_eq!(default("gba").as_deref(), Some("mGBA"));
        assert_eq!(default("ps1").as_deref(), Some("DuckStation"));
        assert_eq!(default("ps2").as_deref(), Some("PCSX2"));
        assert_eq!(default("switch").as_deref(), Some("Ryubing"));
        assert_eq!(default("snes").as_deref(), Some("Snes9x"));
        assert_eq!(default("n3ds").as_deref(), Some("Azahar"));
        assert_eq!(default("xbox360").as_deref(), Some("Xenia Canary"));
        assert_eq!(
            default("arcade"),
            None,
            "FinalBurn Neo and RetroArch need setup"
        );

        // A small cap stops early but keeps what was found; a full scan finishes.
        let tiny = ScanLimits {
            max_depth: 5,
            max_entries: 60, // the root alone lists 49 entries
        };
        let cut = discover(&root, &none, &HashSet::new(), tiny, &mut |_, _| {}).unwrap();
        assert!(cut.truncated);
        assert!(!cut.found.is_empty() && cut.found.len() < 41);
        let all = discover(
            &root,
            &none,
            &HashSet::new(),
            ScanLimits::EMULATORS_FULL,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(!all.truncated);
        assert_eq!(all.found.len(), 41);
    }

    #[test]
    fn profiles_are_named_after_their_folder_when_it_differs() {
        let known = |id: &str| catalog::known_emulator(id).unwrap();
        let name = |path: &str, id: &str| profile_name(Path::new(path), known(id));
        assert_eq!(name(r"C:\E\Ryubing\Ryujinx.exe", "ryujinx"), "Ryubing");
        assert_eq!(name(r"C:\E\Ryujinx\Ryujinx.exe", "ryujinx"), "Ryujinx");
        assert_eq!(name(r"C:\E\Citron Neo\citron.exe", "citron"), "Citron Neo");
        assert_eq!(name(r"C:\E\dolphin\Dolphin.exe", "dolphin"), "Dolphin");
        assert_eq!(name(r"C:\E\RPCS3\bin\rpcs3.exe", "rpcs3"), "RPCS3");
        assert_eq!(
            name(r"C:\E\Final Burn Neo\fbneo64.exe", "fbneo"),
            "FinalBurn Neo"
        );
        assert_eq!(name(r"C:\rpcs3.exe", "rpcs3"), "RPCS3");
    }
}
