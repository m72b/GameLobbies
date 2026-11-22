//! Steam: installed games from `appmanifest_*.acf` files in the Steam library folders.

use super::{single_route_game, states};
use crate::types::{Game, GameRoute};
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// A quoted VDF string, honoring `\"` and `\\` escapes.
const QUOTED: &str = r#""((?:[^"\\]|\\.)*)""#;

static LIBRARY_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#""(?:path|\d+)"\s+{QUOTED}"#)).unwrap());
static STATE_FLAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)"StateFlags"\s+"(\d+)""#).unwrap());
static MANIFEST_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^appmanifest_\d+\.acf$").unwrap());

fn field(text: &str, key: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r#""{}"\s+{QUOTED}"#, regex::escape(key))).ok()?;
    pattern.captures(text).map(|c| {
        c[1].replace(r"\\", "\u{0}")
            .replace(r#"\""#, "\"")
            .replace('\u{0}', r"\")
    })
}

pub(crate) fn cover(app_id: &str) -> String {
    format!("https://cdn.akamai.steamstatic.com/steam/apps/{app_id}/library_600x900.jpg")
}

#[cfg(windows)]
fn registry_steam_path() -> Result<Option<String>> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    match hkcu
        .open_subkey(r"Software\Valve\Steam")
        .and_then(|k| k.get_value::<String, _>("SteamPath"))
    {
        Ok(path) => Ok(Some(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("Could not locate Steam libraries: {e}")),
    }
}

#[cfg(not(windows))]
fn registry_steam_path() -> Result<Option<String>> {
    Ok(None)
}

/// The Steam client's own folders (where `userdata` lives): the usual install paths that exist
/// plus the registry's `SteamPath`. Never fails; an unreadable registry just adds nothing.
pub fn steam_client_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let candidates = [
        r"C:\Program Files (x86)\Steam".to_string(),
        r"C:\Program Files\Steam".to_string(),
    ];
    let registry = registry_steam_path()
        .ok()
        .flatten()
        .map(|p| p.trim().replace('/', "\\"));
    for path in candidates.into_iter().chain(registry) {
        let key = path.to_lowercase();
        if Path::new(&path).is_dir()
            && !dirs
                .iter()
                .any(|d| d.to_string_lossy().to_lowercase() == key)
        {
            dirs.push(PathBuf::from(path));
        }
    }
    dirs
}

/// Steam client folders and every Steam library folder (`libraryfolders.vdf`).
pub fn steam_roots() -> Result<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let add = |roots: &mut Vec<PathBuf>, path: PathBuf| {
        let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
        if !roots
            .iter()
            .any(|r| r.to_string_lossy().replace('/', "\\").to_lowercase() == key)
        {
            roots.push(path);
        }
    };
    for path in [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
    ] {
        if Path::new(path).exists() {
            add(&mut roots, PathBuf::from(path));
        }
    }
    if let Some(path) = registry_steam_path()? {
        add(&mut roots, PathBuf::from(path.trim().replace('/', "\\")));
    }
    let mut found_library_list = false;
    for root in roots.clone() {
        let file = root.join("steamapps").join("libraryfolders.vdf");
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        found_library_list = true;
        for capture in LIBRARY_PATH.captures_iter(&text) {
            let path = capture[1].replace(r"\\", r"\");
            if path.len() > 2 && path.as_bytes()[1] == b':' {
                add(&mut roots, PathBuf::from(path.replace('/', "\\")));
            }
        }
    }
    if !roots.is_empty() && !found_library_list {
        bail!("Steam libraryfolders.vdf could not be found. Existing Steam entries were left unchanged.");
    }
    Ok(roots)
}

pub fn state_flags_indicate_installed(manifest: &str) -> Result<bool> {
    let raw = STATE_FLAGS
        .captures(manifest)
        .ok_or_else(|| anyhow!("manifest is missing its install state flags"))?;
    let flags: u64 = raw[1]
        .parse()
        .map_err(|_| anyhow!("manifest has invalid install state flags"))?;
    Ok(flags & 4 != 0)
}

fn parse_manifest(text: &str, folder: &Path) -> Result<Option<Game>> {
    let app_id =
        field(text, "appid").filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()));
    let title = field(text, "name").filter(|t| !t.is_empty());
    let (Some(app_id), Some(title)) = (app_id, title) else {
        bail!("manifest is missing its app ID or title");
    };
    if !state_flags_indicate_installed(text)? {
        return Ok(None);
    }
    let install_path = field(text, "installdir")
        .filter(|d| !d.is_empty())
        .map(|d| folder.join("common").join(d).to_string_lossy().into_owned());
    // Tools, redistributables and broken installs: an install folder with nothing to launch
    // (only DLLs and data) is not an installed game. A missing folder is left alone.
    if install_path
        .as_deref()
        .is_some_and(|path| !super::has_launchable(Path::new(path)))
    {
        return Ok(None);
    }
    Ok(Some(single_route_game(
        format!("steam:{app_id}"),
        title,
        "Steam",
        Some(cover(&app_id)),
        GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            launch_target: format!("steam://run/{app_id}"),
            app_id: Some(app_id),
            install_path,
            installed: Some(true),
            install_states: states("steam-installed", true),
            ..Default::default()
        },
    )))
}

pub fn scan_installed() -> Result<Vec<Game>> {
    let mut games = Vec::new();
    for root in steam_roots()? {
        let folder = root.join("steamapps");
        if !folder.exists() {
            bail!(
                "Steam library folder is unavailable: {}. Existing entries were left unchanged.",
                folder.display()
            );
        }
        let mut files: Vec<_> = std::fs::read_dir(&folder)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| MANIFEST_FILE.is_match(name))
            .collect();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(folder.join(&file))
                .with_context(|| format!("Could not verify Steam manifest {file}"))?;
            match parse_manifest(&text, &folder) {
                Ok(Some(game)) => games.push(game),
                Ok(None) => {}
                Err(e) => bail!("Could not verify Steam manifest {file}: {e}"),
            }
        }
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_flags_include_the_fully_installed_bit() {
        assert!(state_flags_indicate_installed(r#""StateFlags" "4""#).unwrap());
        assert!(state_flags_indicate_installed(r#""StateFlags" "6""#).unwrap());
        assert!(!state_flags_indicate_installed(r#""StateFlags" "2""#).unwrap());
        let err = state_flags_indicate_installed(r#""appid" "570""#).unwrap_err();
        assert!(err.to_string().contains("state flags"));
    }

    #[test]
    fn parses_manifests_with_escaped_quotes() {
        let text = "\"AppState\"\n{\n\t\"appid\"\t\t\"570\"\n\t\"name\"\t\t\"Dota \\\"2\\\"\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"dota 2 beta\"\n}";
        let game = parse_manifest(text, Path::new(r"C:\Steam\steamapps"))
            .unwrap()
            .unwrap();
        assert_eq!(game.title, "Dota \"2\"");
        assert_eq!(game.id, "steam:570");
        assert_eq!(
            game.routes[0].install_path.as_deref(),
            Some(r"C:\Steam\steamapps\common\dota 2 beta")
        );
    }

    #[test]
    fn manifests_of_folders_without_an_executable_are_not_installed() {
        use crate::localscan::tests::TempDir;
        let dir = TempDir::new("steam-dll");
        let manifest = |name: &str| {
            format!(
                "\"appid\" \"7\"\n\"name\" \"T\"\n\"StateFlags\" \"4\"\n\"installdir\" \"{name}\""
            )
        };
        let steamapps = dir.dir("steamapps");
        dir.file("steamapps/common/dlls/engine.dll");
        assert!(parse_manifest(&manifest("dlls"), &steamapps)
            .unwrap()
            .is_none());
        // The executable may live in a subfolder.
        dir.file("steamapps/common/deep/bin/win64/game.exe");
        assert!(parse_manifest(&manifest("deep"), &steamapps)
            .unwrap()
            .is_some());
    }

    #[test]
    fn reads_library_folders() {
        let vdf =
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}";
        let paths: Vec<String> = LIBRARY_PATH
            .captures_iter(vdf)
            .map(|c| c[1].replace(r"\\", r"\"))
            .collect();
        assert_eq!(paths, [r"D:\SteamLibrary"]);
    }
}
