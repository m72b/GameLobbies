//! Paths the user chose in a native file or folder picker, remembered for a short time.
//!
//! Some pickers only fill in a form (an emulator's executable, a library folder) that the
//! renderer then saves. The renderer can't be trusted to name executables or scan folders by
//! itself, so the save accepts such a path only when it matches a recent pick of the same
//! kind ([`PickedPaths::find`]). A pick is forgotten once used ([`PickedPaths::forget`]),
//! after [`PICK_LIFETIME`], or when more than [`MAX_PICKS`] newer picks of its kind exist.

use crate::tracker::normalize_path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a pick stays usable (long enough to finish filling in the form).
pub const PICK_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// Picks kept per kind.
pub const MAX_PICKS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    /// `emulators_browse_executable`.
    EmulatorExecutable,
    /// `roots_browse`.
    LibraryFolder,
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

    /// Remembers a path the user picked (paths that can't be normalized are ignored).
    pub fn remember(&self, kind: PickKind, path: &str) {
        self.remember_at(kind, path, Instant::now());
    }

    fn remember_at(&self, kind: PickKind, path: &str, at: Instant) {
        let Some(key) = normalize_path(path) else {
            return;
        };
        let mut picks = self.lock();
        picks.retain(|p| !(p.kind == kind && p.key == key));
        picks.push(Pick {
            kind,
            key,
            path: path.to_string(),
            at,
        });
        let count = picks.iter().filter(|p| p.kind == kind).count();
        if count > MAX_PICKS {
            let mut extra = count - MAX_PICKS;
            picks.retain(|p| {
                let drop = extra > 0 && p.kind == kind;
                if drop {
                    extra -= 1;
                }
                !drop
            });
        }
    }

    /// The picked path (as the picker returned it) that `raw` names, when it was picked
    /// recently. Comparison is case-insensitive and ignores `/` vs `\`.
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
    fn only_recent_picks_of_the_same_kind_match() {
        let picks = PickedPaths::default();
        let exe = r"D:\Emulators\mGBA\mGBA.exe";
        assert_eq!(picks.find(PickKind::EmulatorExecutable, exe), None);
        picks.remember(PickKind::EmulatorExecutable, exe);
        assert_eq!(
            picks.find(PickKind::EmulatorExecutable, "d:/emulators/mgba/MGBA.EXE"),
            Some(exe.to_string())
        );
        assert_eq!(picks.find(PickKind::LibraryFolder, exe), None);
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
        picks.forget(PickKind::EmulatorExecutable, exe);
        assert_eq!(picks.find(PickKind::EmulatorExecutable, exe), None);
    }

    #[test]
    fn picks_expire_and_are_capped() {
        let picks = PickedPaths::default();
        let start = Instant::now();
        picks.remember_at(PickKind::LibraryFolder, r"D:\Roms", start);
        assert!(picks
            .find_at(
                PickKind::LibraryFolder,
                r"D:\Roms",
                start + PICK_LIFETIME / 2
            )
            .is_some());
        assert!(picks
            .find_at(PickKind::LibraryFolder, r"D:\Roms", start + PICK_LIFETIME)
            .is_none());

        for i in 0..=MAX_PICKS {
            picks.remember(PickKind::EmulatorExecutable, &format!(r"D:\E{i}\e.exe"));
        }
        picks.remember(PickKind::LibraryFolder, r"D:\Games");
        assert!(picks
            .find(PickKind::EmulatorExecutable, r"D:\E0\e.exe")
            .is_none());
        assert!(picks
            .find(
                PickKind::EmulatorExecutable,
                &format!(r"D:\E{MAX_PICKS}\e.exe")
            )
            .is_some());
        assert!(picks.find(PickKind::LibraryFolder, r"D:\Games").is_some());
        // Unusable paths are never remembered.
        picks.remember(PickKind::LibraryFolder, "relative");
        assert!(picks.find(PickKind::LibraryFolder, "relative").is_none());
    }
}
