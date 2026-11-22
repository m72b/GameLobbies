//! EA app, Battle.net, Ubisoft Connect, Amazon Games (registry + uninstall records) and
//! itch.io (local app folders). Reads the registry directly instead of parsing `reg.exe`.

use super::states;
use crate::types::{Game, GameRoute};
use anyhow::{bail, Result};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Clone, Debug, Default)]
pub struct RegistryEntry {
    pub key: String,
    pub values: HashMap<String, String>,
}

impl RegistryEntry {
    fn text(&self, keys: &[&str]) -> String {
        keys.iter()
            .filter_map(|k| self.values.get(&k.to_lowercase()))
            .find(|v| !v.is_empty())
            .cloned()
            .unwrap_or_default()
    }
    fn install_path(&self) -> String {
        expand_path(&self.text(&[
            "installdir",
            "install dir",
            "installlocation",
            "install location",
            "installpath",
            "install path",
            "path",
        ]))
    }
    fn id(&self) -> String {
        self.key
            .rsplit('\\')
            .next()
            .unwrap_or(&self.key)
            .to_string()
    }
}

#[cfg(windows)]
pub fn query_registry(key: &str) -> Result<Vec<RegistryEntry>> {
    use winreg::enums::*;
    use winreg::RegKey;
    let (hive, full_hive, path) = match key.split_once('\\') {
        Some(("HKLM", rest)) => (HKEY_LOCAL_MACHINE, "HKEY_LOCAL_MACHINE", rest),
        Some(("HKCU", rest)) => (HKEY_CURRENT_USER, "HKEY_CURRENT_USER", rest),
        _ => bail!("Unsupported registry key {key}"),
    };
    let root = match RegKey::predef(hive).open_subkey_with_flags(path, KEY_READ) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => bail!("Could not read registry key {key}: {e}"),
    };
    fn walk(key: &RegKey, name: String, out: &mut Vec<RegistryEntry>, depth: usize) {
        let mut values = HashMap::new();
        for (value_name, value) in key.enum_values().flatten() {
            let text = match value.vtype {
                REG_DWORD => u32::from_reg_value(&value).map(|v| format!("0x{v:x}")).ok(),
                REG_QWORD => u64::from_reg_value(&value).map(|v| format!("0x{v:x}")).ok(),
                _ => String::from_reg_value(&value).ok(),
            };
            if let Some(text) = text {
                values.insert(
                    value_name.to_lowercase(),
                    text.trim_end_matches('\0').trim().to_string(),
                );
            }
        }
        // Like `reg query /s`, keys without values (container keys) are not entries.
        if !values.is_empty() {
            out.push(RegistryEntry {
                key: name.clone(),
                values,
            });
        }
        if depth > 8 {
            return;
        }
        for child in key.enum_keys().flatten() {
            if let Ok(sub) = key.open_subkey_with_flags(&child, KEY_READ) {
                walk(&sub, format!("{name}\\{child}"), out, depth + 1);
            }
        }
    }
    use winreg::types::FromRegValue;
    let mut out = Vec::new();
    walk(&root, format!("{full_hive}\\{path}"), &mut out, 0);
    Ok(out)
}

#[cfg(not(windows))]
pub fn query_registry(_key: &str) -> Result<Vec<RegistryEntry>> {
    Ok(Vec::new())
}

static ENV_VAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([^%]+)%").unwrap());
static ICON_INDEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*-?\d+$").unwrap());
static IGNORED_EXE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(unins|uninstall|setup|installer|crash|unitycrashhandler|vc_redist|eac|battle\.net|updater|repair)").unwrap()
});

fn expand_path(value: &str) -> String {
    let unquoted = value.trim().trim_matches('"');
    ENV_VAR
        .replace_all(unquoted, |c: &regex::Captures| {
            std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
        })
        .trim()
        .to_string()
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Picks the game's executable: the uninstall entry's DisplayIcon when it is an .exe, else
/// the best-named .exe in the install folder (and one level below). Ambiguous → none.
fn executable_path(entry: &RegistryEntry, install_path: &str) -> Option<PathBuf> {
    let icon = expand_path(&entry.text(&["displayicon", "display icon"]));
    let icon = ICON_INDEX.replace(&icon, "").trim_matches('"').to_string();
    if icon.to_lowercase().ends_with(".exe") && Path::new(&icon).exists() {
        return Some(crate::platform::resolve(&icon));
    }
    if install_path.is_empty() {
        return None;
    }
    let folder = Path::new(install_path);
    let meta = std::fs::metadata(folder).ok()?;
    if meta.is_file() {
        return install_path
            .to_lowercase()
            .ends_with(".exe")
            .then(|| crate::platform::resolve(install_path));
    }
    let display = entry.values.get("displayname").cloned().unwrap_or_default();
    // A folder that can't be listed completely can't be verified.
    let ranked = ranked_executables(folder, &display).ok()?;
    match ranked.as_slice() {
        [] => None,
        [first, _, ..] if first.1 == 0 => None,
        [first, ..] => Some(crate::platform::resolve(&first.0.to_string_lossy())),
    }
}

/// The `.exe` files in `folder` and one level below, best first: each word (longer than
/// three characters) of `name` found in the file name scores 5, and an exe with a matching
/// `<name>_Data` folder (Unity) scores 10. Installers, uninstallers,
/// crash handlers and updaters are left out. Ties are sorted by path. Any error listing
/// `folder` or a subfolder (or reading an entry's type) is returned, so a partial listing is
/// never mistaken for the whole folder.
pub fn ranked_executables(folder: &Path, name: &str) -> std::io::Result<Vec<(PathBuf, usize)>> {
    ranked_executables_counted(folder, name, &mut 0)
}

/// [`ranked_executables`], adding the number of entries it listed to `listed` (so a bounded
/// scan can charge them to its entry budget).
pub fn ranked_executables_counted(
    folder: &Path,
    name: &str,
    listed: &mut usize,
) -> std::io::Result<Vec<(PathBuf, usize)>> {
    let mut entries = |dir: &Path| -> std::io::Result<Vec<(PathBuf, std::fs::FileType)>> {
        let mut out = Vec::new();
        for item in std::fs::read_dir(dir)? {
            let item = item?;
            out.push((item.path(), item.file_type()?));
        }
        *listed += out.len();
        Ok(out)
    };
    let mut candidates = Vec::new();
    let top = entries(folder)?;
    let mut dirs = Vec::new();
    for (path, kind) in top {
        if kind.is_file() && file_name_lower(&path).ends_with(".exe") {
            candidates.push(path);
        } else if kind.is_dir() {
            dirs.push(path);
        }
    }
    // Unity games ship `<Game>.exe` next to `<Game>_Data`: a strong sign of the main exe.
    let data_dirs: std::collections::HashSet<String> = dirs
        .iter()
        .map(|d| file_name_lower(d))
        .filter(|n| n.ends_with("_data"))
        .collect();
    for dir in dirs {
        for (path, kind) in entries(&dir)? {
            if kind.is_file() && file_name_lower(&path).ends_with(".exe") {
                candidates.push(path);
            }
        }
    }
    let display = name.to_lowercase();
    let terms: Vec<&str> = display
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 3)
        .collect();
    let mut ranked: Vec<(PathBuf, usize)> = candidates
        .into_iter()
        .filter(|p| !IGNORED_EXE.is_match(&file_name_lower(p)))
        .map(|p| {
            let name = file_name_lower(&p);
            let stem = name.strip_suffix(".exe").unwrap_or(&name);
            let data_bonus = if data_dirs.contains(&format!("{stem}_data")) {
                10
            } else {
                0
            };
            let score = terms.iter().filter(|t| name.contains(*t)).count() * 5 + data_bonus;
            (p, score)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| {
            a.0.to_string_lossy()
                .to_lowercase()
                .cmp(&b.0.to_string_lossy().to_lowercase())
        })
    });
    Ok(ranked)
}

pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub store: &'static str,
    keys: &'static [&'static str],
    matcher: &'static str,
    match_key: bool,
}

pub const SOURCES: [Source; 4] = [
    Source {
        id: "ea-registry",
        name: "EA app",
        store: "EA app",
        keys: &[
            r"HKLM\SOFTWARE\Electronic Arts\EA Games",
            r"HKLM\SOFTWARE\WOW6432Node\Electronic Arts\EA Games",
            r"HKLM\SOFTWARE\WOW6432Node\Origin Games",
        ],
        matcher: r"(?i)electronic arts|ea app|origin games",
        match_key: false,
    },
    Source {
        id: "battlenet-registry",
        name: "Battle.net",
        store: "Battle.net",
        keys: &[
            r"HKLM\SOFTWARE\Blizzard Entertainment",
            r"HKLM\SOFTWARE\WOW6432Node\Blizzard Entertainment",
        ],
        matcher: r"(?i)blizzard entertainment|battle\.net",
        match_key: false,
    },
    Source {
        id: "ubisoft-registry",
        name: "Ubisoft Connect",
        store: "Ubisoft",
        keys: &[
            r"HKLM\SOFTWARE\Ubisoft\Launcher\Installs",
            r"HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs",
        ],
        matcher: r"(?i)ubisoft",
        match_key: false,
    },
    Source {
        id: "amazon-registry",
        name: "Amazon Games",
        store: "Amazon Games",
        keys: &[
            r"HKLM\SOFTWARE\Amazon Games",
            r"HKLM\SOFTWARE\WOW6432Node\Amazon Games",
        ],
        matcher: r"(?i)amazon games",
        match_key: true,
    },
];

const UNINSTALL_ROOTS: [&str; 3] = [
    r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
];

/// Uninstall records, read once per library scan and shared by all registry sources.
#[derive(Default)]
pub struct UninstallCache(Option<Vec<RegistryEntry>>);

impl UninstallCache {
    fn entries(&mut self) -> Result<&[RegistryEntry]> {
        if self.0.is_none() {
            let mut all = Vec::new();
            for root in UNINSTALL_ROOTS {
                all.extend(query_registry(root)?);
            }
            self.0 = Some(all);
        }
        Ok(self.0.as_deref().unwrap_or_default())
    }
}

pub fn scan_source(source: &Source, cache: &mut UninstallCache) -> Result<Vec<Game>> {
    let matcher = Regex::new(source.matcher)?;
    let mut entries: Vec<RegistryEntry> = Vec::new();
    for key in source.keys {
        entries.extend(query_registry(key)?);
    }
    for entry in cache.entries()? {
        let haystack = format!(
            "{} {} {}",
            if source.match_key {
                String::new()
            } else {
                entry.text(&["publisher"])
            },
            entry.text(&["uninstallstring"]),
            entry.install_path()
        ) + if source.match_key { &entry.key } else { "" };
        if matcher.is_match(&haystack) {
            entries.push(entry.clone());
        }
    }
    let mut unique: BTreeMap<String, RegistryEntry> = BTreeMap::new();
    for entry in entries {
        unique.insert(entry.key.to_lowercase(), entry);
    }
    let mut found: BTreeMap<String, Game> = BTreeMap::new();
    for entry in unique.into_values() {
        let title = Some(entry.text(&["displayname", "display name"]))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| entry.id());
        let folder = entry.install_path();
        let Some(executable) = executable_path(&entry, &folder) else {
            bail!("{}: could not verify the installed executable for \"{title}\". Existing entries were left unchanged.", source.name);
        };
        let id = format!("{}:{}", source.id, entry.id());
        let install_path = if folder.is_empty() {
            executable
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            Some(folder)
        };
        found.insert(
            id.clone(),
            Game {
                id,
                title,
                store: source.store.into(),
                routes: vec![GameRoute {
                    source_id: source.id.into(),
                    store: source.store.into(),
                    app_id: Some(entry.id()),
                    launch_target: executable.to_string_lossy().into_owned(),
                    install_path,
                    installed: Some(true),
                    install_states: states(source.id, true),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    }
    Ok(found.into_values().collect())
}

fn title_case(name: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let collapsed = Regex::new(" +")
        .unwrap()
        .replace_all(&spaced, " ")
        .into_owned();
    let mut out = String::with_capacity(collapsed.len());
    let mut prev_word = false;
    for c in collapsed.chars() {
        let word = c.is_ascii_alphanumeric() || c == '_';
        out.extend(if word && !prev_word {
            c.to_uppercase().collect::<Vec<_>>()
        } else {
            vec![c]
        });
        prev_word = word;
    }
    out
}

pub fn scan_itch() -> Result<Vec<Game>> {
    let roots: Vec<PathBuf> = ["APPDATA", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join("itch").join("apps"))
        .filter(|p| p.exists())
        .collect();
    let mut games = Vec::new();
    for root in roots {
        let mut items: Vec<_> = std::fs::read_dir(&root)?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .collect();
        items.sort_by_key(|e| e.file_name());
        for item in items {
            let name = item.file_name().to_string_lossy().into_owned();
            let folder = item.path();
            let entry = RegistryEntry {
                key: folder.to_string_lossy().into_owned(),
                values: [("displayname".to_string(), name.clone())].into(),
            };
            let Some(executable) = executable_path(&entry, &folder.to_string_lossy()) else {
                bail!("Could not verify the installed itch.io game in {}. Existing entries were left unchanged.", folder.display());
            };
            games.push(Game {
                id: format!("itch:{}", name.to_lowercase()),
                title: title_case(&name),
                store: "itch.io".into(),
                routes: vec![GameRoute {
                    source_id: "itch-installed".into(),
                    store: "itch.io".into(),
                    app_id: Some(name),
                    launch_target: executable.to_string_lossy().into_owned(),
                    install_path: Some(folder.to_string_lossy().into_owned()),
                    installed: Some(true),
                    install_states: states("itch-installed", true),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_cases_itch_folder_names() {
        assert_eq!(title_case("celeste_classic"), "Celeste Classic");
        assert_eq!(title_case("a--short-hike"), "A Short Hike");
    }

    #[test]
    fn expands_environment_variables() {
        std::env::set_var("LOBBY_TEST_DIR", r"C:\Games");
        assert_eq!(expand_path(r#""%LOBBY_TEST_DIR%\Foo""#), r"C:\Games\Foo");
        assert_eq!(expand_path("%LOBBY_MISSING_VAR%"), "%LOBBY_MISSING_VAR%");
    }

    #[test]
    fn picks_the_best_named_executable() {
        let dir = std::env::temp_dir().join(format!("lobby-exe-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        for f in ["unins000.exe", "launcher.exe", "bin/Hollow Knight.exe"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let entry = RegistryEntry {
            key: "k".into(),
            values: [("displayname".into(), "Hollow Knight".into())].into(),
        };
        let exe = executable_path(&entry, &dir.to_string_lossy()).unwrap();
        assert!(exe.to_string_lossy().ends_with("Hollow Knight.exe"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_registry_and_treats_missing_keys_as_empty() {
        let entries = query_registry(r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion").unwrap();
        assert!(entries[0].values.contains_key("productname"));
        assert!(
            query_registry(r"HKCU\SOFTWARE\GameLobbiesDefinitelyMissing")
                .unwrap()
                .is_empty()
        );
    }
}
