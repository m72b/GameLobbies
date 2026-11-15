//! Paths the user chose in a native file or folder picker, remembered for a short time.
//!
//! Some pickers only fill in a form (an emulator's executable, a library folder) that the
//! renderer then saves. The renderer can't be trusted to name executables or scan folders by
//! itself, so the save accepts such a path only when it matches the latest pick of the same
//! kind ([`PickedPaths::find`]). Only the latest pick per kind is kept; it is forgotten once
//! used ([`PickedPaths::forget`]) or after [`PICK_LIFETIME`]. Library folder picks are bound
//! to the folder kind they were picked for, so a games-folder pick can't become an emulator
//! folder.

use crate::tracker::normalize_path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a pick stays usable (long enough to finish filling in the form).
pub const PICK_LIFETIME: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    /// `emulators_browse_executable`.
    EmulatorExecutable,
    /// `roots_browse` for a `games` root.
    GamesFolder,
    /// `roots_browse` for an `emulators` root.
    EmulatorsFolder,
    /// `launchers_browse_executable`: a custom launcher's program.
    LauncherExecutable,
    /// `launchers_browse_folder`: a custom launcher's working folder.
    LauncherFolder,
    /// `launchers_browse_games_folder`: a custom launcher's games folder (a scanned `games`
    /// root). Never a working folder or a plain library folder, nor the other way round.
    LauncherGamesFolder,
}

impl PickKind {
    /// The folder pick for a root kind (`games` | `emulators`).
    pub fn for_root(kind: &str) -> Option<Self> {
        match kind {
            "games" => Some(Self::GamesFolder),
            "emulators" => Some(Self::EmulatorsFolder),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Pick {
    kind: PickKind,
    key: String,
    path: String,
    at: Instant,
}

#[derive(Debug, Default)]
pub struct PickedPaths {
    picks: Mutex<Vec<Pick>>,
}

impl PickedPaths {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Pick>> {
        self.picks.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Remembers a path the user picked, replacing the previous pick of that kind. A path
    /// that can't be normalized only clears the previous pick.
    pub fn remember(&self, kind: PickKind, path: &str) {
        self.remember_at(kind, path, Instant::now());
    }

    fn remember_at(&self, kind: PickKind, path: &str, at: Instant) {
        let mut picks = self.lock();
        picks.retain(|p| p.kind != kind);
        if let Some(key) = normalize_path(path) {
            picks.push(Pick {
                kind,
                key,
                path: path.to_string(),
                at,
            });
        }
    }

    /// The picked path (as the picker returned it) that `raw` names, when it is the latest
    /// pick of `kind` and recent. Comparison is case-insensitive and ignores `/` vs `\`.
    pub fn find(&self, kind: PickKind, raw: &str) -> Option<String> {
        self.find_at(kind, raw, Instant::now())
    }

    fn find_at(&self, kind: PickKind, raw: &str, now: Instant) -> Option<String> {
        let key = normalize_path(raw)?;
        let mut picks = self.lock();
        picks.retain(|p| now.saturating_duration_since(p.at) < PICK_LIFETIME);
        picks
            .iter()
            .find(|p| p.kind == kind && p.key == key)
            .map(|p| p.path.clone())
    }

    /// The latest recent pick of `kind` (as the picker returned it), for saves where the
    /// renderer only says "use what I picked" and never sends the path itself.
    pub fn latest(&self, kind: PickKind) -> Option<String> {
        self.latest_at(kind, Instant::now())
    }

    fn latest_at(&self, kind: PickKind, now: Instant) -> Option<String> {
        let mut picks = self.lock();
        picks.retain(|p| now.saturating_duration_since(p.at) < PICK_LIFETIME);
        picks
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.path.clone())
    }

    /// Forgets a pick once it was used.
    pub fn forget(&self, kind: PickKind, path: &str) {
        if let Some(key) = normalize_path(path) {
            self.lock().retain(|p| !(p.kind == kind && p.key == key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_pick_of_the_same_kind_matches() {
        let picks = PickedPaths::default();
        let exe = r"D:\Emulators\mGBA\mGBA.exe";
        assert_eq!(picks.find(PickKind::EmulatorExecutable, exe), None);
        picks.remember(PickKind::EmulatorExecutable, exe);
        assert_eq!(
            picks.find(PickKind::EmulatorExecutable, "d:/emulators/mgba/MGBA.EXE"),
            Some(exe.to_string())
        );
        assert_eq!(picks.find(PickKind::GamesFolder, exe), None);
        assert_eq!(
            picks.find(PickKind::EmulatorExecutable, r"C:\Windows\System32\cmd.exe"),
            None
        );
        // `..` can't climb from a picked path to another one.
        assert_eq!(
            picks.find(
                PickKind::EmulatorExecutable,
                r"D:\Emulators\mGBA\mGBA.exe\..\..\..\Windows\cmd.exe"
            ),
            None
        );
        // A newer pick replaces the older one.
        let other = r"D:\Emulators\Dolphin\Dolphin.exe";
        picks.remember(PickKind::EmulatorExecutable, other);
        assert_eq!(picks.find(PickKind::EmulatorExecutable, exe), None);
        assert!(picks.find(PickKind::EmulatorExecutable, other).is_some());
        picks.forget(PickKind::EmulatorExecutable, other);
        assert_eq!(picks.find(PickKind::EmulatorExecutable, other), None);
    }

    #[test]
    fn folder_picks_are_bound_to_their_root_kind() {
        let picks = PickedPaths::default();
        assert_eq!(PickKind::for_root("games"), Some(PickKind::GamesFolder));
        assert_eq!(
            PickKind::for_root("emulators"),
            Some(PickKind::EmulatorsFolder)
        );
        assert_eq!(PickKind::for_root("other"), None);
        picks.remember(PickKind::GamesFolder, r"D:\Roms");
        assert!(picks.find(PickKind::GamesFolder, r"D:\Roms").is_some());
        assert!(picks.find(PickKind::EmulatorsFolder, r"D:\Roms").is_none());
        // Picks of other kinds are kept.
        picks.remember(PickKind::EmulatorsFolder, r"D:\Emulators");
        assert!(picks.find(PickKind::GamesFolder, r"D:\Roms").is_some());
    }

    #[test]
    fn the_latest_pick_is_per_kind_and_expires() {
        let picks = PickedPaths::default();
        let start = Instant::now();
        assert_eq!(picks.latest(PickKind::LauncherExecutable), None);
        picks.remember_at(PickKind::LauncherExecutable, r"D:\Heroic\Heroic.exe", start);
        picks.remember_at(PickKind::EmulatorExecutable, r"D:\Emu\mGBA.exe", start);
        assert_eq!(
            picks
                .latest_at(PickKind::LauncherExecutable, start)
                .as_deref(),
            Some(r"D:\Heroic\Heroic.exe")
        );
        // An emulator pick is never a launcher pick, nor a launcher folder.
        assert_eq!(picks.latest_at(PickKind::LauncherFolder, start), None);
        assert!(picks
            .latest_at(PickKind::LauncherExecutable, start + PICK_LIFETIME)
            .is_none());
        picks.remember(PickKind::LauncherFolder, r"D:\Mods");
        picks.forget(PickKind::LauncherFolder, r"D:\Mods");
        assert_eq!(picks.latest(PickKind::LauncherFolder), None);
    }

    #[test]
    fn launcher_games_folder_picks_are_their_own_kind() {
        let picks = PickedPaths::default();
        picks.remember(PickKind::LauncherGamesFolder, r"D:\Arcade");
        assert!(picks
            .find(PickKind::LauncherGamesFolder, r"d:\arcade")
            .is_some());
        // Never a library folder, a working folder or an executable.
        for other in [
            PickKind::GamesFolder,
            PickKind::EmulatorsFolder,
            PickKind::LauncherFolder,
            PickKind::LauncherExecutable,
            PickKind::EmulatorExecutable,
        ] {
            assert!(picks.find(other, r"D:\Arcade").is_none(), "{other:?}");
        }
        // And the other way round.
        picks.remember(PickKind::LauncherFolder, r"D:\Mods");
        assert!(picks
            .find(PickKind::LauncherGamesFolder, r"D:\Mods")
            .is_none());
    }

    #[test]
    fn picks_expire() {
        let picks = PickedPaths::default();
        let start = Instant::now();
        picks.remember_at(PickKind::GamesFolder, r"D:\Roms", start);
        assert!(picks
            .find_at(PickKind::GamesFolder, r"D:\Roms", start + PICK_LIFETIME / 2)
            .is_some());
        assert!(picks
            .find_at(PickKind::GamesFolder, r"D:\Roms", start + PICK_LIFETIME)
            .is_none());
        // An unusable path is never remembered, and clears the previous pick.
        picks.remember(PickKind::GamesFolder, r"D:\Games");
        picks.remember(PickKind::GamesFolder, "relative");
        assert!(picks.find(PickKind::GamesFolder, "relative").is_none());
        assert!(picks.find(PickKind::GamesFolder, r"D:\Games").is_none());
    }
}
