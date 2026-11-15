//! Store library importers. Each returns normalized games with one or more launch routes.

pub mod epic;
pub mod gog;
pub mod registry;
pub mod steam;
pub mod xbox;

use crate::types::{Game, GameRoute};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

/// Builds a game with a single route — the common shape every importer produces.
pub fn single_route_game(
    id: String,
    title: String,
    store: &str,
    cover: Option<String>,
    route: GameRoute,
) -> Game {
    Game {
        id,
        title,
        cover: cover.clone(),
        default_cover: cover,
        store: store.into(),
        routes: vec![route],
        ..Default::default()
    }
}

pub fn states(source: &str, installed: bool) -> Option<BTreeMap<String, bool>> {
    Some([(source.to_string(), installed)].into())
}

/// Folders that only hold redistributable installers; their executables don't make a game.
const REDIST_DIRS: [&str; 8] = [
    "_commonredist",
    "commonredist",
    "redist",
    "redistributables",
    "directx",
    "dotnet",
    "vcredist",
    "__installer",
];
/// How deep (below the install folder) and how many entries a search looks at.
const MAX_SEARCH_DEPTH: usize = 6;
const MAX_SEARCH_ENTRIES: usize = 4000;

fn is_launchable_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if !matches!(ext, "exe" | "bat" | "cmd" | "lnk" | "com") {
        return false;
    }
    !["unins", "vc_redist", "vcredist", "dxsetup", "dotnet", "ndp"]
        .iter()
        .any(|prefix| stem.starts_with(prefix))
}

/// Whether an install folder holds anything launchable (`.exe`, `.bat`, `.cmd`, `.lnk`, `.com`)
/// at any depth, ignoring redistributable folders and uninstallers. Folders with only DLLs and
/// data are not a playable install. The search is shallowest-first and bounded; when the folder
/// can't be read (missing, drive not connected) or a bound (entries or depth) cut the search short, the answer is `true`, so
/// a game is never marked uninstalled on incomplete evidence.
pub fn has_launchable(dir: &Path) -> bool {
    let mut queue = VecDeque::from([(dir.to_path_buf(), 0usize)]);
    let mut seen = 0usize;
    let mut truncated = false;
    while let Some((current, depth)) = queue.pop_front() {
        let Ok(read) = std::fs::read_dir(&current) else {
            return true;
        };
        for entry in read {
            let Ok(entry) = entry else {
                return true;
            };
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            seen += 1;
            if seen > MAX_SEARCH_ENTRIES {
                return true;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_file() {
                if is_launchable_name(&name) {
                    return true;
                }
            } else if kind.is_dir() && !REDIST_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                if depth < MAX_SEARCH_DEPTH {
                    queue.push_back((entry.path(), depth + 1));
                } else {
                    // Not looked into: an exe may be down there.
                    truncated = true;
                }
            }
        }
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localscan::tests::TempDir;

    #[test]
    fn dll_only_folders_have_nothing_launchable() {
        let dir = TempDir::new("launchable");
        dir.file("game/engine.dll");
        dir.file("game/data/pak0.pak");
        dir.file("game/bin/win64/plugin.dll");
        assert!(!has_launchable(&dir.path().join("game")));
        // Redistributables and uninstallers don't count.
        dir.file("game/_CommonRedist/vcredist/vcredist_x64.exe");
        dir.file("game/DirectX/setup.exe");
        dir.file("game/unins000.exe");
        assert!(!has_launchable(&dir.path().join("game")));
        // An empty folder is not a game either.
        assert!(!has_launchable(&dir.dir("empty")));
        // An exe in a subfolder is enough.
        dir.file("game/bin/win64/Game.exe");
        assert!(has_launchable(&dir.path().join("game")));
        // A missing folder is unknown, not empty.
        assert!(has_launchable(&dir.path().join("gone")));
        // Deeper than the search looks: unknown, so not "nothing launchable".
        dir.file("deep/a/b/c/d/e/f/g/h/Game.exe");
        assert!(has_launchable(&dir.path().join("deep")));
        // A deep folder holding nothing launchable within reach but fully searched is still false.
        dir.file("shallow/a/b/c/data.pak");
        assert!(!has_launchable(&dir.path().join("shallow")));
    }
}
