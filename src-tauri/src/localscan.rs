//! Filesystem side of library roots: folder listing and game discovery (the emulator scan
//! has its own walk in `emulators`). Nothing here touches the database.
//!
//! Listing rules (both emulator and game roots):
//! - symbolic links, junctions and other reparse points are never followed (nor reported as
//!   games), so a scan can't leave its root;
//! - Hidden and System attributes are ignored: inside a library root they are common (copied
//!   from discs, synced or archived folders) and say nothing about whether something is a
//!   game. Only names starting with `.`, `$RECYCLE.BIN`, `System Volume Information`,
//!   `desktop.ini` and `Thumbs.db` are skipped;
//! - a folder that can't be listed completely (the folder itself, any entry, or an entry's
//!   type or attributes failing to read) is recorded as unreadable, so nothing under it is
//!   ever treated as gone; the walk carries on; only an unreadable root fails the scan;
//! - depth and entry-count caps ([`ScanLimits`]: a quick scan by default, a much larger full
//!   scan on request) stop runaway scans; hitting the entry cap marks the result `truncated`
//!   (the caller then never infers removals from it).
//!
//! Game discovery (see [`scan_games`]):
//! - The system comes from the nearest folder whose name is a catalog alias (`Sony 3` →
//!   PS3). The root counts too: a root named `Sony Playstation` is scanned as that system
//!   folder (ROM files directly in it, per-game subfolders and folder-format games). Files
//!   outside any system folder are skipped; first-level folders without any recognised system
//!   are reported as unsupported.
//! - ROM systems: one game per supported file. Index formats win over the files they list:
//!   `.m3u` over its discs, `.cue`/`.gdi` over their tracks, `.ccd`/`.mds` over same-named
//!   `.img`/`.sub`/`.mdf`. Only references inside the index file's own folder count (no
//!   absolute paths, drive letters or `..`). `(Disc N)` files without an `.m3u` become one
//!   game that starts the lowest disc. Folder formats (PS3 `PS3_DISC.SFB`,
//!   `PS3_GAME/PARAM.SFO` or `USRDIR/EBOOT.BIN`, PS4 `eboot.bin` + `sce_sys`, Wii U
//!   `code/*.rpx`, Xbox 360 `default.xex`) are one game each. Switch updates and DLC are
//!   skipped.
//! - PC folders: each immediate subfolder is one game whose executable is picked with the
//!   store importers' ranking (`registry::ranked_executables`); a stored user choice wins.
//!   When the pick isn't clear the folder is reported as ambiguous instead of guessed. A
//!   folder name without any letter (`00356`) isn't a title: the executable's version
//!   resource (ProductName, else FileDescription) or its file name is used instead.
//! - Mixed app folders ([`ScanInputs::mixed`], a custom launcher's games folder): folders
//!   outside any system folder are classified one by one instead of skipped. A folder named
//!   after a system is scanned as above; a folder holding a known emulator executable (or one
//!   the emulator discovery found, [`ScanInputs::emulator_dirs`]) is an emulator, never a game;
//!   a folder whose executables sit in it or in plainly named subfolders (`bin`, `x64`, ...) is
//!   one PC game, picked like a PC folder's game; any other folder is a container and is
//!   classified the same way, level by level. Shortcuts lying in a mixed folder are games as in
//!   a PC folder. Loose `.exe` files are not (an app folder holds its own program and tools):
//!   they are skipped with a reason. Loose files with a ROM extension of exactly one system are
//!   that system's ROMs; an extension several systems share (`.iso`, `.chd`) is an
//!   [`UncertainRom`] for the caller to match against the emulators it knows. Generic
//!   extensions (archives, `.bin`, `.md`, playlists) are never guessed at outside a system
//!   folder.
//! - Shortcuts ([`classify_shortcut`]), directly in the PC folder or the only launcher in a
//!   game folder: a `.url` counts only when its link passes `launch::validate_launch_uri`,
//!   and the link itself is stored (never the file). A `.lnk` is read with `IShellLinkW`:
//!   when it points at an existing `.exe` without arguments that `.exe` is stored; anything
//!   else (arguments, documents, scripts, missing targets) is never added until the user
//!   confirmed exactly that target and those arguments (a `shortcut` review item).

use crate::catalog::{self, System, PC};
use crate::tracker::normalize_path;
use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Paths longer than this are skipped.
pub const MAX_PATH_CHARS: usize = 4_096;
/// Index files (`.m3u`, `.cue`, `.gdi`, `PARAM.SFO`) larger than this are not parsed.
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_SAMPLES: usize = 20;
pub const MAX_UNREADABLE: usize = 200;

/// How far a scan may go. Folders deeper than `max_depth` below the root are not listed; after
/// `max_entries` listed entries the scan stops and is `truncated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    pub max_depth: usize,
    pub max_entries: usize,
}

impl ScanLimits {
    /// Game roots, normal scans.
    pub const GAMES_QUICK: Self = Self {
        max_depth: 8,
        max_entries: 250_000,
    };
    /// Game roots, "scan everything".
    pub const GAMES_FULL: Self = Self {
        max_depth: 16,
        max_entries: 3_000_000,
    };
    /// Emulator roots, normal scans.
    pub const EMULATORS_QUICK: Self = Self {
        max_depth: 5,
        max_entries: 200_000,
    };
    /// Emulator roots, "scan everything".
    pub const EMULATORS_FULL: Self = Self {
        max_depth: 8,
        max_entries: 3_000_000,
    };

    pub fn games(full: bool) -> Self {
        if full {
            Self::GAMES_FULL
        } else {
            Self::GAMES_QUICK
        }
    }

    pub fn emulators(full: bool) -> Self {
        if full {
            Self::EMULATORS_FULL
        } else {
            Self::EMULATORS_QUICK
        }
    }
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self::GAMES_QUICK
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// A folder listing. `complete` is false when any entry (or its type or attributes) couldn't
/// be read: the items are what could be read, and the folder must be treated as unreadable.
#[derive(Debug, Default)]
pub struct Listing {
    pub items: Vec<Item>,
    pub complete: bool,
}

fn skipped_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.')
        || lower == "$recycle.bin"
        || lower == "system volume information"
        || catalog::SKIPPED_FILE_NAMES.contains(&lower.as_str())
}

/// Lists a folder: no links/junctions/reparse points and none of the skipped names (Hidden
/// and System entries ARE listed, see the module docs); sorted by name (case-insensitive)
/// for stable results. Errors only when the folder itself can't be opened; per-entry errors
/// make the listing incomplete instead.
pub fn list_dir(dir: &Path) -> std::io::Result<Listing> {
    let mut listing = Listing {
        items: Vec::new(),
        complete: true,
    };
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            listing.complete = false;
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            listing.complete = false;
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const REPARSE_POINT: u32 = 0x400;
            match entry.metadata() {
                Ok(meta) if meta.file_attributes() & REPARSE_POINT == 0 => {}
                Ok(_) => continue,
                Err(_) => {
                    listing.complete = false;
                    continue;
                }
            }
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if skipped_name(&name) {
            continue;
        }
        listing.items.push(Item {
            path: entry.path(),
            name,
            is_dir: file_type.is_dir(),
        });
    }
    listing.items.sort_by_key(|i| i.name.to_lowercase());
    Ok(listing)
}

/// A user-facing title for a PC game in a folder whose name has no letters (`00356`): the
/// executable's version resource when it names a product, else its file name (`Cairn`).
pub fn title_from_executable(exe: &Path) -> String {
    if let Some(name) = crate::platform::exe_product_name(exe) {
        return name;
    }
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    catalog::clean_title(&stem)
}

/// The title of a PC game folder: the folder name, or — when it has no letters — the title
/// of the executable it launches.
fn pc_title(folder_name: &str, exe: Option<&Path>) -> String {
    let from_folder = catalog::clean_title(folder_name);
    if from_folder.chars().any(char::is_alphabetic) {
        return from_folder;
    }
    match exe.filter(|e| e.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))) {
        Some(exe) => title_from_executable(exe),
        None => from_folder,
    }
}

// ---------------------------------------------------------------------------
// Shortcuts

/// What a `.lnk` / `.url` file may become (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub enum ShortcutKind {
    /// A `.url` whose link is an allowed store launch link (already validated).
    Uri(String),
    /// A `.lnk` pointing at an existing local `.exe` with no arguments.
    Exe(PathBuf),
    /// A `.lnk` the user confirmed with exactly this target and these arguments.
    Confirmed,
    /// A `.lnk` that needs the user's confirmation first.
    NeedsConfirmation { target: String, arguments: String },
    /// Not usable (with a user-facing reason).
    Invalid(String),
    /// The file couldn't be read.
    Unreadable,
}

/// Classifies a shortcut file. `confirmations`: normalized `.lnk` path → (target, arguments)
/// the user confirmed.
pub fn classify_shortcut(
    path: &Path,
    confirmations: &HashMap<String, (String, String)>,
) -> ShortcutKind {
    let ext = path
        .file_name()
        .and_then(|n| catalog::extension_of(&n.to_string_lossy()))
        .unwrap_or_default();
    match ext.as_str() {
        ".url" => match crate::platform::read_url_shortcut(path) {
            Ok(link) => match crate::launch::validate_launch_uri(&link) {
                Ok(uri) => ShortcutKind::Uri(uri),
                Err(_) => ShortcutKind::Invalid(
                    "This internet shortcut doesn't open a supported game store link.".into(),
                ),
            },
            Err(_) if !path.is_file() => ShortcutKind::Unreadable,
            Err(_) => ShortcutKind::Invalid("This internet shortcut has no usable link.".into()),
        },
        ".lnk" => match crate::platform::resolve_shortcut(path) {
            Ok((target, arguments)) => {
                if arguments.trim().is_empty() && crate::emulators::executable_ok(&target) {
                    return ShortcutKind::Exe(PathBuf::from(target));
                }
                if target.trim().is_empty() {
                    return ShortcutKind::Invalid(
                        "This shortcut doesn't point at a file on this PC.".into(),
                    );
                }
                let confirmed = confirmations.get(&key_of(path));
                if confirmed == Some(&(target.clone(), arguments.clone())) {
                    ShortcutKind::Confirmed
                } else {
                    ShortcutKind::NeedsConfirmation { target, arguments }
                }
            }
            Err(_) => ShortcutKind::Unreadable,
        },
        _ => ShortcutKind::Invalid("Not a shortcut.".into()),
    }
}

// ---------------------------------------------------------------------------
// Game discovery

#[derive(Debug, Clone, PartialEq)]
pub struct RomGame {
    pub system: &'static str,
    /// What the emulator opens (a file; for folder games the file inside it).
    pub path: PathBuf,
    pub title: String,
    /// Lower-case extension (`.iso`) or `folder`.
    pub format: String,
    /// Other discs of a multi-disc game without an `.m3u`.
    pub other_discs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PcGame {
    /// The game's folder; `None` for a shortcut lying directly in the PC folder.
    pub folder: Option<PathBuf>,
    /// What identifies the game: its folder, or the shortcut file.
    pub source: PathBuf,
    /// What is launched: an `.exe` path, a validated store link, or a confirmed `.lnk`.
    pub target: String,
    /// The file that was found: the `.exe`, or the shortcut file (what a custom launcher with a
    /// program is given as the game's file).
    pub file: PathBuf,
    pub title: String,
}

/// A loose file in a mixed folder whose extension several systems use (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct UncertainRom {
    pub path: PathBuf,
    pub title: String,
    /// The systems using its extension (catalog ids, catalog order).
    pub systems: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AmbiguousPc {
    pub folder: PathBuf,
    pub title: String,
    pub candidates: Vec<PathBuf>,
}

/// A `.lnk` waiting for the user's confirmation of its target and arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingShortcut {
    pub shortcut: PathBuf,
    pub folder: Option<PathBuf>,
    pub title: String,
    pub target: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Unsupported {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct GameScan {
    pub roms: Vec<RomGame>,
    pub pcs: Vec<PcGame>,
    pub ambiguous: Vec<AmbiguousPc>,
    pub shortcuts: Vec<PendingShortcut>,
    pub unsupported: Vec<Unsupported>,
    pub unreadable: Vec<String>,
    /// Files skipped as not a game (unknown format, no system, Switch update/DLC, ...).
    pub skipped: usize,
    /// A few skipped paths with the reason, for the summary.
    pub samples: Vec<String>,
    pub truncated: bool,
    pub visited: usize,
    /// Mixed folders only: loose files whose system isn't certain.
    pub uncertain: Vec<UncertainRom>,
    /// Mixed folders only: emulator folders seen (and left to the emulator discovery).
    pub emulator_folders: usize,
}

/// What a game scan must honour: normalized paths from the ignore list, the user's
/// executable choices for PC folders (normalized folder → executable or shortcut), and the
/// shortcuts the user confirmed (normalized `.lnk` → (target, arguments)).
#[derive(Debug, Default, Clone)]
pub struct ScanInputs {
    pub ignored: HashSet<String>,
    pub exe_choices: HashMap<String, String>,
    pub shortcut_ok: HashMap<String, (String, String)>,
    /// Quick (default) or full scan limits.
    pub limits: ScanLimits,
    /// Long-form keys of other registered roots inside this one (`roots::nested_roots`): never
    /// entered, never reported. A first-level folder that contains one of them (like
    /// `Launcher` holding the emulator root) is skipped silently too.
    pub excluded: HashSet<String>,
    /// Classify the root as a mixed app folder (see the module docs) instead of by system
    /// folder names only.
    pub mixed: bool,
    /// Mixed folders: long-form keys of folders holding an emulator the emulator discovery
    /// found (never games).
    pub emulator_dirs: HashSet<String>,
    /// Long-form keys of files that are never games (a custom launcher's own program).
    pub skip_files: HashSet<String>,
}

/// The comparison key of a path that came from outside a walk (a stored path, an env var, a
/// picker, typed text): its long form ([`crate::platform::long_path`], so `C:\Users\RUNNER~1`
/// and `C:\Users\runneradmin` give the same key), normalized and lower-cased. Every stored
/// identity (route `appId`, review keys, ignore list, shortcut confirmations, executable
/// choices) uses it. `None` for relative or otherwise unusable paths.
pub fn path_key(raw: &str) -> Option<String> {
    normalize_path(&crate::platform::long_path(Path::new(raw.trim())).to_string_lossy())
}

/// [`path_key`] for a `Path` (empty when unusable).
pub fn key_of(path: &Path) -> String {
    path_key(&path.to_string_lossy()).unwrap_or_default()
}

/// The key of a path produced by a walk. Walks start from the long form of their root and
/// append names exactly as listed, so the result is already long: plain string normalization
/// suffices (and costs no disk access per entry).
pub fn listed_key(path: &Path) -> String {
    normalize_path(&path.to_string_lossy()).unwrap_or_default()
}

fn is_shortcut(name: &str) -> bool {
    matches!(
        catalog::extension_of(name).as_deref(),
        Some(".lnk") | Some(".url")
    )
}

struct GameWalker<'a> {
    inputs: &'a ScanInputs,
    out: GameScan,
    consumed: HashSet<String>,
    progress: &'a mut dyn FnMut(usize, &Path),
    /// Per first-level folder: (games found under it, skipped files without a system).
    top: BTreeMap<String, (usize, usize)>,
}

impl GameWalker<'_> {
    fn skip(&mut self, path: &Path, reason: &str) {
        self.out.skipped += 1;
        if self.out.samples.len() < MAX_SAMPLES {
            self.out
                .samples
                .push(format!("{} ({reason})", path.to_string_lossy()));
        }
    }

    fn ignored(&self, path: &Path) -> bool {
        self.inputs.ignored.contains(&listed_key(path))
    }

    /// Another registered root (or, at the first level, a folder holding one): not ours.
    fn excluded(&self, path: &Path, depth: usize) -> bool {
        if self.inputs.excluded.is_empty() {
            return false;
        }
        let key = listed_key(path);
        self.inputs.excluded.contains(&key)
            || (depth == 0
                && self
                    .inputs
                    .excluded
                    .iter()
                    .any(|nested| crate::tracker::is_within(&key, nested)))
    }

    fn count_entries(&mut self, n: usize) -> bool {
        self.out.visited += n;
        if self.out.visited > self.inputs.limits.max_entries {
            self.out.truncated = true;
        }
        !self.out.truncated
    }

    fn unreadable(&mut self, path: &Path) {
        if self.out.unreadable.len() < MAX_UNREADABLE {
            self.out
                .unreadable
                .push(path.to_string_lossy().into_owned());
        } else {
            // Past the cap nothing more can be protected: never infer removals.
            self.out.truncated = true;
        }
    }

    fn visit(&mut self, dir: &Path, depth: usize, system: Option<&'static System>, top: &str) {
        if self.out.truncated {
            return;
        }
        (self.progress)(self.out.visited, dir);
        let listing = match list_dir(dir) {
            Ok(listing) => listing,
            Err(_) => return self.unreadable(dir),
        };
        if !listing.complete {
            self.unreadable(dir);
        }
        if !self.count_entries(listing.items.len()) {
            return;
        }
        let items: Vec<Item> = listing
            .items
            .into_iter()
            .filter(|i| i.path.as_os_str().len() <= MAX_PATH_CHARS && !self.ignored(&i.path))
            .filter(|i| !self.excluded(&i.path, depth))
            .collect();
        if system.is_some_and(|s| s.id == PC) {
            return self.pc_folder(&items, top);
        }
        if system.is_none() && self.inputs.mixed {
            return self.mixed_folder(&items, depth, top);
        }
        let before = self.out.roms.len();
        match system {
            Some(system) => self.rom_files(system, &items),
            None => {
                for item in items.iter().filter(|i| !i.is_dir) {
                    if !ignored_extension(&item.name) {
                        self.skip(&item.path, "not inside a system folder");
                        if !top.is_empty() {
                            self.top.entry(top.into()).or_default().1 += 1;
                        }
                    }
                }
            }
        }
        if !top.is_empty() {
            self.top.entry(top.into()).or_default().0 += self.out.roms.len() - before;
        }
        for item in items.iter().filter(|i| i.is_dir) {
            if self.out.truncated {
                return;
            }
            let child_system = catalog::system_for_folder(&item.name).or(system);
            let child_top = if depth == 0 { item.name.as_str() } else { top };
            if depth == 0 {
                self.top.entry(child_top.into()).or_default();
            }
            if let Some(sys) = child_system.filter(|s| s.id != PC) {
                if let Some(game) = folder_game(sys, &item.path, &item.name) {
                    self.out.roms.push(game);
                    self.top.entry(child_top.into()).or_default().0 += 1;
                    continue;
                }
            }
            if depth + 1 > self.inputs.limits.max_depth {
                continue;
            }
            self.visit(&item.path, depth + 1, child_system, child_top);
        }
    }

    fn rom_files(&mut self, system: &'static System, items: &[Item]) {
        let files: Vec<&Item> = items.iter().filter(|i| !i.is_dir).collect();
        // Index formats claim the files they list.
        for item in &files {
            let ext = catalog::extension_of(&item.name).unwrap_or_default();
            if catalog::INDEX_FORMATS.contains(&ext.as_str())
                && system.extensions.contains(&ext.as_str())
            {
                for referenced in index_references(&item.path, &ext) {
                    self.consumed.insert(listed_key(&referenced));
                }
            }
        }
        let mut singles: Vec<(&Item, String)> = Vec::new();
        for item in files {
            if self.consumed.contains(&listed_key(&item.path)) {
                continue;
            }
            let ext = catalog::extension_of(&item.name).unwrap_or_default();
            if !system.extensions.contains(&ext.as_str()) {
                if !ignored_extension(&item.name) {
                    self.skip(&item.path, &format!("not a {} format", system.name));
                }
                continue;
            }
            let stem = stem_of(&item.name);
            if system.id == "switch" && catalog::switch_extra_content(stem) {
                self.skip(&item.path, "Switch update or DLC");
                continue;
            }
            singles.push((item, ext));
        }
        // (Disc N) groups without an .m3u: one game starting the lowest disc.
        let mut discs: BTreeMap<(String, String), Vec<(u32, &Item)>> = BTreeMap::new();
        let mut rest = Vec::new();
        for (item, ext) in singles {
            match catalog::disc_info(stem_of(&item.name)) {
                Some((n, base)) => discs
                    .entry((base.to_lowercase(), ext.clone()))
                    .or_default()
                    .push((n, item)),
                None => rest.push((item, ext)),
            }
        }
        for ((_, ext), mut group) in discs {
            group.sort_by_key(|(n, item)| (*n, item.name.to_lowercase()));
            let (_, first) = group[0];
            self.out.roms.push(RomGame {
                system: system.id,
                path: first.path.clone(),
                title: catalog::clean_title(stem_of(&first.name)),
                format: ext,
                other_discs: group[1..].iter().map(|(_, i)| i.path.clone()).collect(),
            });
        }
        for (item, ext) in rest {
            self.out.roms.push(RomGame {
                system: system.id,
                path: item.path.clone(),
                title: catalog::clean_title(stem_of(&item.name)),
                format: ext,
                other_discs: Vec::new(),
            });
        }
    }

    /// Adds what a shortcut stands for (see [`classify_shortcut`]). Returns whether anything
    /// (a game or a pending confirmation) came of it.
    fn shortcut(&mut self, folder: Option<&Path>, shortcut: &Path, title: String) -> bool {
        let source = folder.unwrap_or(shortcut).to_path_buf();
        let target = match classify_shortcut(shortcut, &self.inputs.shortcut_ok) {
            ShortcutKind::Uri(uri) => uri,
            ShortcutKind::Exe(exe) => exe.to_string_lossy().into_owned(),
            ShortcutKind::Confirmed => shortcut.to_string_lossy().into_owned(),
            ShortcutKind::NeedsConfirmation { target, arguments } => {
                self.out.shortcuts.push(PendingShortcut {
                    shortcut: shortcut.to_path_buf(),
                    folder: folder.map(Path::to_path_buf),
                    title,
                    target,
                    arguments,
                });
                return true;
            }
            ShortcutKind::Invalid(reason) => {
                self.out.unsupported.push(Unsupported {
                    path: shortcut.to_string_lossy().into_owned(),
                    reason,
                });
                self.skip(shortcut, "unusable shortcut");
                return false;
            }
            ShortcutKind::Unreadable => {
                self.unreadable(&source);
                return false;
            }
        };
        self.out.pcs.push(PcGame {
            folder: folder.map(Path::to_path_buf),
            source,
            target,
            file: shortcut.to_path_buf(),
            title,
        });
        true
    }

    fn pc_folder(&mut self, items: &[Item], top: &str) {
        let mut found = 0;
        for item in items {
            if !item.is_dir {
                let ext = catalog::extension_of(&item.name).unwrap_or_default();
                if is_shortcut(&item.name) {
                    let title = catalog::clean_title(stem_of(&item.name));
                    found += usize::from(self.shortcut(None, &item.path, title));
                } else if !ignored_extension(&item.name) || ext == ".exe" {
                    self.skip(&item.path, "put each PC game in its own folder");
                }
                continue;
            }
            found += self.pc_dir(item, None);
        }
        if !top.is_empty() {
            self.top.entry(top.into()).or_default().0 += found;
        }
    }

    /// The executable or shortcut the user chose for a PC game folder, if it still exists.
    fn exe_choice(&self, item: &Item) -> Option<PathBuf> {
        self.inputs
            .exe_choices
            .get(&listed_key(&item.path))
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    /// One PC game folder: the user's choice, else `pick` (or the ranking when `None`). Returns
    /// how many games (or pending decisions) came of it.
    fn pc_dir(&mut self, item: &Item, pick: Option<Pick>) -> usize {
        let title = catalog::clean_title(&item.name);
        if let Some(choice) = self.exe_choice(item) {
            if is_shortcut(&choice.to_string_lossy()) {
                return usize::from(self.shortcut(Some(&item.path), &choice, title));
            }
            self.out.pcs.push(PcGame {
                folder: Some(item.path.clone()),
                source: item.path.clone(),
                target: choice.to_string_lossy().into_owned(),
                title: pc_title(&item.name, Some(&choice)),
                file: choice,
            });
            return 1;
        }
        match pick.unwrap_or_else(|| pick_executable(&item.path, &item.name)) {
            Pick::One(target) => {
                self.out.pcs.push(PcGame {
                    folder: Some(item.path.clone()),
                    source: item.path.clone(),
                    title: pc_title(&item.name, Some(&target)),
                    target: target.to_string_lossy().into_owned(),
                    file: target,
                });
                1
            }
            Pick::Shortcut(shortcut) => {
                usize::from(self.shortcut(Some(&item.path), &shortcut, title))
            }
            Pick::Ambiguous(candidates) => {
                self.out.ambiguous.push(AmbiguousPc {
                    folder: item.path.clone(),
                    title,
                    candidates,
                });
                1
            }
            Pick::None => {
                self.out.unsupported.push(Unsupported {
                    path: item.path.to_string_lossy().into_owned(),
                    reason: "No game executable or shortcut found in this PC game folder.".into(),
                });
                self.skip(&item.path, "no executable");
                0
            }
            Pick::Unreadable => {
                self.unreadable(&item.path);
                0
            }
        }
    }

    /// A folder of a mixed app folder outside any system folder (see the module docs).
    fn mixed_folder(&mut self, items: &[Item], depth: usize, top: &str) {
        let (mut found, mut skipped) = (0, 0);
        let mut loose: Vec<&Item> = Vec::new();
        for item in items.iter().filter(|i| !i.is_dir) {
            let ext = catalog::extension_of(&item.name).unwrap_or_default();
            if self.inputs.skip_files.contains(&listed_key(&item.path)) {
                continue;
            }
            if is_shortcut(&item.name) {
                let title = catalog::clean_title(stem_of(&item.name));
                found += usize::from(self.shortcut(None, &item.path, title));
            } else if ext == ".exe" {
                // Emulators are registered by the emulator discovery; other loose programs are
                // the app's own files or tools, never guessed to be games.
                if catalog::match_executable(&item.name).is_none() {
                    self.skip(&item.path, "put each PC game in its own folder");
                    skipped += 1;
                }
            } else if ignored_extension(&item.name) {
            } else if LOOSE_GENERIC_EXTENSIONS.contains(&ext.as_str())
                || catalog::systems_for_extension(&ext).is_empty()
            {
                self.skip(&item.path, "not a game, shortcut or ROM format");
                skipped += 1;
            } else {
                loose.push(item);
            }
        }
        if !top.is_empty() {
            self.top.entry(top.into()).or_default().1 += skipped;
        }
        found += self.loose_roms(&loose);
        if !top.is_empty() {
            self.top.entry(top.into()).or_default().0 += found;
        }
        for item in items.iter().filter(|i| i.is_dir) {
            if self.out.truncated {
                return;
            }
            let child_top = if depth == 0 { item.name.as_str() } else { top };
            if depth == 0 {
                self.top.entry(child_top.into()).or_default();
            }
            let key = listed_key(&item.path);
            if let Some(sys) = catalog::system_for_folder(&item.name) {
                if sys.id != PC {
                    if let Some(game) = folder_game(sys, &item.path, &item.name) {
                        self.out.roms.push(game);
                        self.top.entry(child_top.into()).or_default().0 += 1;
                        continue;
                    }
                }
                if depth < self.inputs.limits.max_depth {
                    self.visit(&item.path, depth + 1, Some(sys), child_top);
                }
                continue;
            }
            if self.inputs.emulator_dirs.contains(&key) || holds_emulator(&item.path) {
                self.out.emulator_folders += 1;
                self.top.entry(child_top.into()).or_default().0 += 1;
                continue;
            }
            let holds_nested_emulator = self
                .inputs
                .emulator_dirs
                .iter()
                .any(|dir| crate::tracker::is_within(&key, dir));
            let game = if self.exe_choice(item).is_some() {
                Some(None)
            } else if holds_nested_emulator {
                None
            } else {
                mixed_pick(&item.path, &item.name).map(Some)
            };
            match game {
                Some(pick) => {
                    let n = self.pc_dir(item, pick);
                    self.top.entry(child_top.into()).or_default().0 += n;
                }
                None if depth < self.inputs.limits.max_depth => {
                    self.visit(&item.path, depth + 1, None, child_top)
                }
                None => {}
            }
        }
    }

    /// Loose ROM-format files of a mixed folder: by system when their extension has exactly
    /// one, else [`UncertainRom`]s. Returns how many were found.
    fn loose_roms(&mut self, items: &[&Item]) -> usize {
        let mut by_system: BTreeMap<&'static str, Vec<Item>> = BTreeMap::new();
        let mut several_systems = Vec::new();
        for item in items {
            let ext = catalog::extension_of(&item.name).unwrap_or_default();
            match catalog::systems_for_extension(&ext).as_slice() {
                [only] => by_system.entry(only.id).or_default().push((*item).clone()),
                [] => {}
                several => several_systems.push((*item, several.iter().map(|s| s.id).collect())),
            }
        }
        let before = self.out.roms.len();
        for (id, files) in by_system {
            if let Some(system) = catalog::system(id) {
                self.rom_files(system, &files);
            }
        }
        let mut uncertain = 0;
        // After the index files above: a track a `.cue` lists is part of that game.
        for (item, systems) in several_systems {
            if self.consumed.contains(&listed_key(&item.path)) {
                continue;
            }
            self.out.uncertain.push(UncertainRom {
                path: item.path.clone(),
                title: catalog::clean_title(stem_of(&item.name)),
                systems,
            });
            uncertain += 1;
        }
        self.out.roms.len() - before + uncertain
    }
}

/// Extensions never guessed to be a ROM outside a system folder: archives and other formats
/// many unrelated files use (`.bin`, `.md` for Markdown, `.m3u` playlists, ...).
const LOOSE_GENERIC_EXTENSIONS: &[&str] = &[
    ".zip", ".7z", ".gz", ".bin", ".img", ".md", ".elf", ".pkg", ".dol", ".m3u", ".sub", ".mdf",
];

/// Folder names that hold a game's executables without naming it (`Game\bin\x64\game.exe`).
const GENERIC_EXE_FOLDERS: &[&str] = &[
    "bin", "bin32", "bin64", "x64", "x86", "x86_64", "win32", "win64", "windows", "binaries",
    "release", "game", "app", "program",
];

fn generic_exe_folder(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| GENERIC_EXE_FOLDERS.contains(&n.to_string_lossy().to_lowercase().as_str()))
}

/// A known emulator executable directly in `dir`.
fn holds_emulator(dir: &Path) -> bool {
    list_dir(dir).is_ok_and(|listing| {
        listing
            .items
            .iter()
            .any(|i| !i.is_dir && catalog::match_executable(&i.name).is_some())
    })
}

/// A folder of a mixed app folder as one PC game: its pick when its executables sit in it or in
/// plainly named subfolders (`bin`, `x64`, two levels down at most), `None` when it is a container
/// of other games (or of nothing usable).
pub fn mixed_pick(folder: &Path, name: &str) -> Option<Pick> {
    let own =
        |p: &PathBuf| p.parent() == Some(folder) || p.parent().is_some_and(generic_exe_folder);
    match pick_executable(folder, name) {
        Pick::One(p) => own(&p).then_some(Pick::One(p)),
        Pick::Ambiguous(c) => {
            let game = c.iter().any(|p| p.parent() == Some(folder)) || c.iter().all(own);
            game.then_some(Pick::Ambiguous(c))
        }
        Pick::None => {
            // `Game\bin\x64\game.exe`: the ranking looks one level deep; look inside the plainly
            // named subfolders once more.
            let listing = list_dir(folder).ok()?;
            for sub in listing
                .items
                .iter()
                .filter(|i| i.is_dir && generic_exe_folder(&i.path))
            {
                if let found @ (Pick::One(_) | Pick::Ambiguous(_)) =
                    pick_executable(&sub.path, name)
                {
                    return Some(found);
                }
            }
            None
        }
        other => Some(other),
    }
}

fn stem_of(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn ignored_extension(name: &str) -> bool {
    catalog::extension_of(name).is_some_and(|e| catalog::IGNORED_EXTENSIONS.contains(&e.as_str()))
}

/// A reference from an index file, if it stays inside the index file's folder: relative,
/// plain names only (no drive letters, roots, `.`/`..` or stream names).
fn local_reference(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') || name.contains(':') {
        return None;
    }
    let candidate = Path::new(name);
    if !candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(dir.join(candidate))
}

/// Files an index file lists (resolved next to it). Unreadable or huge files list nothing.
fn index_references(path: &Path, ext: &str) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new(""));
    if ext == ".ccd" || ext == ".mds" {
        let stem = stem_of(&path.file_name().unwrap_or_default().to_string_lossy()).to_string();
        let others: &[&str] = if ext == ".ccd" {
            &[".img", ".sub", ".bin"]
        } else {
            &[".mdf"]
        };
        return others
            .iter()
            .map(|e| dir.join(format!("{stem}{e}")))
            .collect();
    }
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_INDEX_BYTES) {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in text.lines().take(10_000) {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = match ext {
            ".m3u" => Some(line.to_string()),
            ".cue" => cue_file(line),
            ".gdi" => gdi_file(line),
            _ => None,
        };
        if let Some(reference) = name.and_then(|n| local_reference(dir, &n)) {
            out.push(reference);
        }
    }
    out
}

/// `FILE "Track 01.bin" BINARY` → `Track 01.bin`.
fn cue_file(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("FILE")
        .or_else(|| line.strip_prefix("file"))?;
    let rest = rest.trim();
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted.split_once('"').map(|(name, _)| name.to_string());
    }
    rest.rsplit_once(' ')
        .map(|(name, _)| name.trim().to_string())
}

/// `1 0 4 2352 "track01.bin" 0` (or unquoted) → `track01.bin`. The first line (track count)
/// has a single field and lists nothing.
fn gdi_file(line: &str) -> Option<String> {
    if let Some(start) = line.find('"') {
        let rest = &line[start + 1..];
        return rest.split_once('"').map(|(name, _)| name.to_string());
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    (fields.len() >= 6).then(|| fields[4].to_string())
}

/// Case-insensitive relative path lookup (`PS3_GAME/PARAM.SFO`) inside `dir`.
fn find_ci(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    for part in rel.split('/') {
        let next = list_dir(&current)
            .ok()?
            .items
            .into_iter()
            .find(|i| i.name.eq_ignore_ascii_case(part))?;
        current = next.path;
    }
    Some(current)
}

/// A folder-format game (see the module docs), or `None`.
fn folder_game(system: &'static System, dir: &Path, name: &str) -> Option<RomGame> {
    for format in system.folder_formats {
        if !format.markers.iter().any(|m| find_ci(dir, m).is_some()) {
            continue;
        }
        let target = if format.targets.is_empty() {
            // Wii U: the .rpx in code/.
            find_ci(dir, "code")
                .and_then(|code| {
                    list_dir(&code).ok()?.items.into_iter().find(|i| {
                        !i.is_dir && catalog::extension_of(&i.name).as_deref() == Some(".rpx")
                    })
                })?
                .path
        } else {
            format.targets.iter().find_map(|t| find_ci(dir, t))?
        };
        let title = find_ci(dir, "PS3_GAME/PARAM.SFO")
            .or_else(|| find_ci(dir, "PARAM.SFO"))
            .or_else(|| find_ci(dir, "sce_sys/param.sfo"))
            .and_then(|p| sfo_title(&p))
            .unwrap_or_else(|| catalog::clean_title(name));
        return Some(RomGame {
            system: system.id,
            path: target,
            title,
            format: "folder".into(),
            other_discs: Vec::new(),
        });
    }
    None
}

/// The `TITLE` entry of a PlayStation `PARAM.SFO`, if the file is well formed.
fn sfo_title(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > 64 * 1024 {
        return None;
    }
    parse_sfo_title(&std::fs::read(path).ok()?)
}

fn parse_sfo_title(data: &[u8]) -> Option<String> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize);
    if data.get(0..4)? != b"\0PSF" {
        return None;
    }
    let keys = u32_at(8)?;
    let values = u32_at(12)?;
    let count = u32_at(16)?.min(256);
    for i in 0..count {
        let entry = 20 + i * 16;
        let key_offset = keys + u16_at(entry)? as usize;
        let key_end = data.get(key_offset..)?.iter().position(|b| *b == 0)? + key_offset;
        if &data[key_offset..key_end] != b"TITLE" {
            continue;
        }
        let len = u32_at(entry + 4)?;
        let start = values + u32_at(entry + 12)?;
        let raw = data.get(start..start.checked_add(len)?)?;
        let text = String::from_utf8_lossy(raw)
            .trim_end_matches('\0')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        return (!text.is_empty()).then_some(text);
    }
    None
}

#[derive(Debug, PartialEq)]
pub enum Pick {
    One(PathBuf),
    /// No `.exe`, one shortcut: classify it with [`classify_shortcut`].
    Shortcut(PathBuf),
    /// Several equally good candidates (best first).
    Ambiguous(Vec<PathBuf>),
    None,
    /// The folder (or a subfolder) couldn't be listed completely.
    Unreadable,
}

/// A PC game folder's executable: a single candidate, or a best-named one that clearly
/// beats the rest. Without any `.exe`, a single shortcut in the folder.
pub fn pick_executable(folder: &Path, name: &str) -> Pick {
    let Ok(ranked) = crate::importers::registry::ranked_executables(folder, name) else {
        return Pick::Unreadable;
    };
    let ranked: Vec<(PathBuf, usize)> = ranked
        .into_iter()
        .filter(|(p, _)| !crate::tracker::is_helper_executable(&listed_key(p)))
        .collect();
    match ranked.as_slice() {
        [] => {}
        [(only, _)] => return Pick::One(only.clone()),
        [(first, a), (_, b), ..] if a > b => return Pick::One(first.clone()),
        _ => {
            let top = ranked[0].1;
            return Pick::Ambiguous(
                ranked
                    .iter()
                    .filter(|(_, s)| top > 0 && *s == top || top == 0)
                    .take(20)
                    .map(|(p, _)| p.clone())
                    .collect(),
            );
        }
    }
    let listing = match list_dir(folder) {
        Ok(listing) if listing.complete => listing,
        _ => return Pick::Unreadable,
    };
    let shortcuts: Vec<PathBuf> = listing
        .items
        .into_iter()
        .filter(|i| !i.is_dir && is_shortcut(&i.name))
        .map(|i| i.path)
        .collect();
    match shortcuts.len() {
        0 => Pick::None,
        1 => Pick::Shortcut(shortcuts[0].clone()),
        _ => Pick::Ambiguous(shortcuts.into_iter().take(20).collect()),
    }
}

/// Scans a games root. Errors only when the root itself can't be read.
pub fn scan_games(
    root: &Path,
    inputs: &ScanInputs,
    progress: &mut dyn FnMut(usize, &Path),
) -> Result<GameScan> {
    // Walk from the long form, so every listed path (and its key) is long too.
    let long_root = crate::platform::long_path(root);
    let root = long_root.as_path();
    if let Err(e) = std::fs::read_dir(root) {
        bail!("{} can't be read: {e}", root.display());
    }
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut walker = GameWalker {
        inputs,
        out: GameScan::default(),
        consumed: HashSet::new(),
        progress,
        top: BTreeMap::new(),
    };
    let system = catalog::system_for_folder(&root_name);
    walker.visit(root, 0, system, "");
    let mut out = walker.out;
    if system.is_none() && inputs.mixed {
        for (folder, (games, skipped)) in walker.top {
            if games == 0 && skipped > 0 {
                out.unsupported.push(Unsupported {
                    path: root.join(&folder).to_string_lossy().into_owned(),
                    reason: format!(
                        "Nothing in \"{folder}\" could be added: no game folder with an executable, shortcut, emulator or ROM was found. Put each PC game in its own folder, or ROMs in a folder named after their system."
                    ),
                });
            }
        }
    } else if system.is_none() {
        for (folder, (games, skipped)) in walker.top {
            if games == 0 && skipped > 0 {
                out.unsupported.push(Unsupported {
                    path: root.join(&folder).to_string_lossy().into_owned(),
                    reason: format!(
                        "\"{folder}\" isn't a known system folder name (e.g. PS2, Switch, PC)."
                    ),
                });
            }
        }
    }
    // A file reached through two index files (or an .m3u and a disc group) is one game.
    let consumed = walker.consumed;
    out.roms
        .retain(|g| g.format == "folder" || !consumed.contains(&listed_key(&g.path)));
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A temporary folder under the system temp directory, removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            // The long form, as real roots are stored (CI runners have an 8.3 TEMP).
            let dir = crate::platform::long_path(&std::env::temp_dir()).join(format!(
                "gamelobby-test-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        /// Creates a file (and its folders) with some content; returns its path.
        pub fn file(&self, rel: &str) -> PathBuf {
            self.write(rel, b"x")
        }

        pub fn write(&self, rel: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path
        }

        pub fn dir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scan(dir: &TempDir) -> GameScan {
        scan_games(dir.path(), &ScanInputs::default(), &mut |_, _| {}).unwrap()
    }

    fn rom_names(scan: &GameScan) -> Vec<String> {
        let mut names: Vec<String> = scan
            .roms
            .iter()
            .map(|g| {
                format!(
                    "{}:{}",
                    g.system,
                    g.path.file_name().unwrap().to_string_lossy()
                )
            })
            .collect();
        names.sort();
        names
    }

    fn sfo(title: &str) -> Vec<u8> {
        // One entry: key "TITLE", utf8 value.
        let key = b"TITLE\0\0\0";
        let value = format!("{title}\0");
        let mut data = Vec::new();
        data.extend(b"\0PSF");
        data.extend(1u32.to_le_bytes()); // version
        let keys = 20 + 16;
        let values = keys + key.len();
        data.extend((keys as u32).to_le_bytes());
        data.extend((values as u32).to_le_bytes());
        data.extend(1u32.to_le_bytes());
        data.extend(0u16.to_le_bytes()); // key offset
        data.extend(0x0204u16.to_le_bytes()); // fmt
        data.extend((value.len() as u32).to_le_bytes());
        data.extend((value.len() as u32).to_le_bytes());
        data.extend(0u32.to_le_bytes()); // value offset
        data.extend(key);
        data.extend(value.as_bytes());
        data
    }

    #[test]
    fn index_formats_win_over_the_files_they_list() {
        let dir = TempDir::new("scan-index");
        dir.write(
            "PS1/Crash (USA).cue",
            b"FILE \"Crash (USA) (Track 1).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\nFILE \"Crash (USA) (Track 2).bin\" BINARY\r\n",
        );
        dir.file("PS1/Crash (USA) (Track 1).bin");
        dir.file("PS1/Crash (USA) (Track 2).bin");
        dir.file("PS1/Lone.bin"); // a .bin without .cue is a game
        dir.write(
            "PS1/FF7.m3u",
            b"#EXTM3U\nFF7 (Disc 1).cue\nFF7 (Disc 2).cue\n",
        );
        dir.write(
            "PS1/FF7 (Disc 1).cue",
            b"FILE \"FF7 (Disc 1).bin\" BINARY\n",
        );
        dir.write(
            "PS1/FF7 (Disc 2).cue",
            b"FILE \"FF7 (Disc 2).bin\" BINARY\n",
        );
        dir.file("PS1/FF7 (Disc 1).bin");
        dir.file("PS1/FF7 (Disc 2).bin");
        // Discs without an .m3u: one game, disc 1.
        dir.file("PS2/Xenosaga (USA) (Disc 2).iso");
        dir.file("PS2/Xenosaga (USA) (Disc 1).iso");
        dir.file("PS2/readme.txt");
        dir.write(
            "Dreamcast/Shenmue/disc.gdi",
            b"3\n1 0 4 2352 track01.bin 0\n2 600 0 2352 \"track 02.raw\" 0\n3 45000 4 2352 track03.bin 0\n",
        );
        for t in ["track01.bin", "track 02.raw", "track03.bin"] {
            dir.file(&format!("Dreamcast/Shenmue/{t}"));
        }
        dir.write("Saturn/Panzer.ccd", b"[CloneCD]");
        dir.file("Saturn/Panzer.img");
        dir.file("Saturn/Panzer.sub");
        let result = scan(&dir);
        assert_eq!(
            rom_names(&result),
            [
                "dreamcast:disc.gdi",
                "ps1:Crash (USA).cue",
                "ps1:FF7.m3u",
                "ps1:Lone.bin",
                "ps2:Xenosaga (USA) (Disc 1).iso",
                "saturn:Panzer.ccd",
            ]
        );
        let xeno = result.roms.iter().find(|g| g.system == "ps2").unwrap();
        assert_eq!(xeno.title, "Xenosaga");
        assert_eq!(xeno.other_discs.len(), 1);
        assert_eq!(result.skipped, 0, "{:?}", result.samples);
    }

    #[test]
    fn folder_games_switch_extras_and_unknown_folders() {
        let dir = TempDir::new("scan-folders");
        dir.write("Sony 3/BLUS30443/PS3_GAME/PARAM.SFO", &sfo("Demon's Souls"));
        for i in 0..50 {
            dir.file(&format!("Sony 3/BLUS30443/PS3_GAME/USRDIR/data{i}.bin"));
        }
        dir.file("Sony 3/BLUS30443/PS3_GAME/USRDIR/EBOOT.BIN");
        dir.file("Sony 3/NPUB31154/USRDIR/EBOOT.BIN");
        dir.file("Nintendo Switch/Zelda [01007EF00011E000][v0].nsp");
        dir.file("Nintendo Switch/Zelda [01007EF00011E800][v65536].nsp");
        dir.file("Nintendo Switch/Zelda DLC [01007EF00011F001][v0].nsp");
        dir.file("Wii U/Mario Kart 8/code/Turbo.rpx");
        dir.file("Wii U/Mario Kart 8/meta/meta.xml");
        dir.file("Wii U/Mario Kart 8/code/app.xml");
        dir.file("Stuff/random.bin");
        dir.file("Stuff/Deep/x.iso");
        dir.file("loose.iso");
        let result = scan(&dir);
        assert_eq!(
            rom_names(&result),
            [
                "ps3:EBOOT.BIN",
                "ps3:EBOOT.BIN",
                "switch:Zelda [01007EF00011E000][v0].nsp",
                "wiiu:Turbo.rpx"
            ]
        );
        let titles: HashSet<&str> = result.roms.iter().map(|g| g.title.as_str()).collect();
        assert!(titles.contains("Demon's Souls"), "{titles:?}");
        assert!(titles.contains("NPUB31154"));
        assert!(titles.contains("Mario Kart 8"));
        assert!(result
            .roms
            .iter()
            .all(|g| g.system != "ps3" || g.format == "folder"));
        // 2 Switch extras + 2 files under Stuff + loose.iso.
        assert_eq!(result.skipped, 5, "{:?}", result.samples);
        let unsupported: Vec<&str> = result.unsupported.iter().map(|u| u.path.as_str()).collect();
        assert_eq!(unsupported.len(), 1);
        assert!(unsupported[0].ends_with("Stuff"));
    }

    #[test]
    fn pc_folders_pick_executables_or_report_ambiguity() {
        let dir = TempDir::new("scan-pc");
        dir.file("PC/Hollow Knight/Hollow Knight.exe");
        dir.file("PC/Hollow Knight/UnityCrashHandler64.exe");
        dir.file("PC/Hollow Knight/unins000.exe");
        dir.file("PC/Celeste/Celeste.exe");
        dir.file("PC/Mystery/alpha.exe");
        dir.file("PC/Mystery/beta.exe");
        dir.file("PC/Empty/readme.txt");
        dir.write(
            "PC/Doom.url",
            b"[InternetShortcut]\r\nURL=steam://rungameid/2280\r\n",
        );
        dir.write(
            "PC/Phish.url",
            b"[InternetShortcut]\nURL=https://evil.example/x.exe\n",
        );
        dir.write(
            "PC/Local.url",
            b"[InternetShortcut]\nURL=file:///C:/Windows/System32/calc.exe\n",
        );
        dir.file("PC/setup.exe");
        let inputs = ScanInputs::default();
        let result = scan_games(dir.path(), &inputs, &mut |_, _| {}).unwrap();
        let mut pcs: Vec<String> = result
            .pcs
            .iter()
            .map(|g| format!("{}={}", g.title, g.target))
            .collect();
        pcs.sort();
        let folder = |rel: &str| {
            dir.path()
                .join(rel.replace('/', "\\"))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(
            pcs,
            [
                format!("Celeste={}", folder("PC/Celeste/Celeste.exe")),
                "Doom=steam://rungameid/2280".to_string(),
                format!(
                    "Hollow Knight={}",
                    folder("PC/Hollow Knight/Hollow Knight.exe")
                ),
            ]
        );
        // The .url game is identified by its file, launched by its (validated) link.
        let doom = result.pcs.iter().find(|g| g.title == "Doom").unwrap();
        assert!(doom.source.ends_with("Doom.url") && doom.folder.is_none());
        assert_eq!(result.ambiguous.len(), 1);
        assert_eq!(result.ambiguous[0].title, "Mystery");
        assert_eq!(result.ambiguous[0].candidates.len(), 2);
        let mut unsupported: Vec<&str> =
            result.unsupported.iter().map(|u| u.path.as_str()).collect();
        unsupported.sort();
        assert_eq!(
            unsupported.len(),
            3,
            "Empty, Local.url and Phish.url: {unsupported:?}"
        );
        assert!(unsupported.iter().any(|p| p.ends_with("Phish.url")));
        assert!(unsupported.iter().any(|p| p.ends_with("Local.url")));
        // A stored choice settles the ambiguity; ignored paths disappear.
        let mut inputs = ScanInputs::default();
        inputs.exe_choices.insert(
            key_of(&dir.path().join("PC/Mystery")),
            folder("PC/Mystery/beta.exe"),
        );
        inputs
            .ignored
            .insert(key_of(&dir.path().join("PC/Celeste")));
        let result = scan_games(dir.path(), &inputs, &mut |_, _| {}).unwrap();
        assert!(result.ambiguous.is_empty());
        assert!(result.pcs.iter().any(|g| g.target.ends_with("beta.exe")));
        assert!(!result.pcs.iter().any(|g| g.title == "Celeste"));
        // A folder that can't be listed is unreadable, never "no executable".
        assert_eq!(
            pick_executable(&dir.path().join("PC/Gone"), "Gone"),
            Pick::Unreadable
        );
    }

    #[cfg(windows)]
    #[test]
    fn lnk_shortcuts_are_resolved_and_never_auto_added_with_arguments() {
        use crate::platform::create_shortcut;
        let dir = TempDir::new("scan-lnk");
        let tool = dir.file("Bin/tool.exe");
        let tool_text = tool.to_string_lossy().into_owned();
        dir.dir("PC/Plain");
        create_shortcut(&dir.path().join("PC/Plain/Play.lnk"), &tool_text, "").unwrap();
        dir.dir("PC/Args");
        let args_lnk = dir.path().join("PC/Args/Play.lnk");
        create_shortcut(&args_lnk, &tool_text, "-connect evil.example").unwrap();
        let script = dir.path().join("PC/Script.lnk");
        create_shortcut(&script, r"C:\Windows\System32\cmd.exe", "/c calc").unwrap();
        let missing = dir.path().join("PC/Missing.lnk");
        create_shortcut(&missing, &dir.path().join("nope.exe").to_string_lossy(), "").unwrap();

        let result = scan_games(dir.path(), &ScanInputs::default(), &mut |_, _| {}).unwrap();
        // Plain → the .exe itself, never the .lnk.
        assert_eq!(result.pcs.len(), 1, "{:?}", result.pcs);
        let plain = &result.pcs[0];
        assert_eq!(plain.title, "Plain");
        assert_eq!(key_of(Path::new(&plain.target)), key_of(&tool));
        assert!(plain.source.ends_with("Plain"));
        // Arguments, a script host and a missing target wait for confirmation.
        let mut pending: Vec<(String, String)> = result
            .shortcuts
            .iter()
            .map(|s| (s.title.clone(), s.arguments.clone()))
            .collect();
        pending.sort();
        assert_eq!(
            pending,
            [
                ("Args".to_string(), "-connect evil.example".to_string()),
                ("Missing".to_string(), String::new()),
                ("Script".to_string(), "/c calc".to_string()),
            ]
        );
        let args_item = result.shortcuts.iter().find(|s| s.title == "Args").unwrap();
        assert_eq!(key_of(Path::new(&args_item.target)), key_of(&tool));
        assert!(args_item.folder.is_some());

        // Once confirmed with the same target and arguments, the .lnk itself is launched.
        let mut inputs = ScanInputs::default();
        inputs.shortcut_ok.insert(
            key_of(&args_lnk),
            (args_item.target.clone(), args_item.arguments.clone()),
        );
        let result = scan_games(dir.path(), &inputs, &mut |_, _| {}).unwrap();
        let args_game = result.pcs.iter().find(|g| g.title == "Args").unwrap();
        assert_eq!(key_of(Path::new(&args_game.target)), key_of(&args_lnk));
        // A changed shortcut needs confirming again.
        create_shortcut(&args_lnk, &tool_text, "-connect other.example").unwrap();
        assert!(matches!(
            classify_shortcut(&args_lnk, &inputs.shortcut_ok),
            ShortcutKind::NeedsConfirmation { .. }
        ));
        assert_eq!(
            classify_shortcut(&dir.path().join("PC/gone.lnk"), &inputs.shortcut_ok),
            ShortcutKind::Unreadable
        );
    }

    #[test]
    fn index_references_stay_inside_their_folder() {
        let dir = TempDir::new("scan-index-escape");
        let abs = dir.file("PS1/Abs.cue");
        dir.write(
            "PS1/List.m3u",
            format!(
                "../Saturn/Escape.cue\n{}\nSub/Disc.cue\nC:Drive.cue\n",
                abs.display()
            )
            .as_bytes(),
        );
        dir.file("PS1/Sub/Disc.cue");
        dir.file("PS1/Drive.cue");
        dir.file("Saturn/Escape.cue");
        let result = scan(&dir);
        assert_eq!(
            rom_names(&result),
            [
                "ps1:Abs.cue",
                "ps1:Drive.cue",
                "ps1:List.m3u",
                "saturn:Escape.cue"
            ]
        );
        let d = Path::new(r"D:\Roms");
        assert_eq!(local_reference(d, "a.bin"), Some(d.join("a.bin")));
        assert_eq!(local_reference(d, "sub/a.bin"), Some(d.join("sub/a.bin")));
        for bad in [
            "../a.bin",
            r"..\a.bin",
            r"C:\a.bin",
            "C:a.bin",
            r"\a.bin",
            "/a.bin",
            "./a.bin",
            "a.bin:stream",
            "",
        ] {
            assert_eq!(local_reference(d, bad), None, "{bad}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn folders_that_cant_be_listed_are_unreadable() {
        let dir = TempDir::new("scan-denied");
        dir.file("GBA/Open.gba");
        let locked = dir.dir("GBA/Locked");
        dir.file("GBA/Locked/Hidden.gba");
        // Everyone (by SID, so it works whatever the account name is).
        let user = "*S-1-1-0".to_string();
        let deny = std::process::Command::new("icacls")
            .arg(&locked)
            .args(["/deny", &format!("{user}:(RD)")])
            .output()
            .unwrap();
        assert!(deny.status.success());
        let result = scan(&dir);
        let _ = std::process::Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", &user])
            .output();
        assert_eq!(rom_names(&result), ["gba:Open.gba"]);
        assert_eq!(result.unreadable.len(), 1);
        assert!(result.unreadable[0].ends_with("Locked"));
    }

    #[test]
    fn walks_are_bounded_and_skip_dot_and_metadata_entries() {
        let dir = TempDir::new("scan-bounds");
        dir.file("GBA/.hidden/secret.gba");
        dir.file("GBA/a/b/c/d/e/f/g/h/i/deep.gba");
        dir.file("GBA/Pokemon (USA).gba");
        dir.file("GBA/desktop.ini");
        dir.file("GBA/Thumbs.db");
        dir.file("$RECYCLE.BIN/GBA/x.gba");
        let result = scan(&dir);
        assert_eq!(rom_names(&result), ["gba:Pokemon (USA).gba"]);
        assert_eq!(result.skipped, 0, "{:?}", result.samples);
        // The root itself may be a system folder.
        let sub = scan_games(
            &dir.path().join("GBA"),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(rom_names(&sub), ["gba:Pokemon (USA).gba"]);
        assert!(scan_games(
            &dir.path().join("missing"),
            &ScanInputs::default(),
            &mut |_, _| {}
        )
        .is_err());
        // A full scan goes deeper.
        let full = ScanInputs {
            limits: ScanLimits::GAMES_FULL,
            ..Default::default()
        };
        let deep = scan_games(dir.path(), &full, &mut |_, _| {}).unwrap();
        assert_eq!(rom_names(&deep).len(), 2);
        // The entry cap stops the scan and marks it truncated.
        let tiny = ScanInputs {
            limits: ScanLimits {
                max_depth: 10,
                max_entries: 3,
            },
            ..Default::default()
        };
        let cut = scan_games(dir.path(), &tiny, &mut |_, _| {}).unwrap();
        assert!(cut.truncated);
        assert!(cut.visited > 3);
    }

    /// Sets Hidden + System, like a library with hidden/system folders (attrib +h +s).
    #[cfg(windows)]
    pub(crate) fn hide(path: &Path) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
        };
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let ok = unsafe {
            SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)
        };
        assert_ne!(ok, 0, "SetFileAttributesW failed for {}", path.display());
    }

    /// A typical games layout (`M:\Game Library`), Hidden+System almost everywhere.
    #[cfg(windows)]
    pub(crate) fn sample_library(dir: &TempDir) -> PathBuf {
        let base = "Game Library";
        let f = |rel: &str| dir.file(&format!("{base}/{rel}"));
        let d = |rel: &str| dir.dir(&format!("{base}/{rel}"));
        f("Sony Playstation/Castlevania Symphony of the Night/Castlevania Symphony of the Night.chd");
        f("Sony Playstation/Castlevania Symphony of the Night/Castlevania Symphony of the Night (Japanese VA).chd");
        f("Sony Playstation 2/Sly 3 Honor Among Thieves.iso");
        dir.write(
            &format!("{base}/Sony Playstation 3/Sly Cooper Thieves in Time/PS3_GAME/PARAM.SFO"),
            &sfo("Sly Cooper: Thieves in Time"),
        );
        f("Sony Playstation 3/Sly Cooper Thieves in Time/PS3_GAME/USRDIR/EBOOT.BIN");
        f("Sony Playstation 3/Sly Cooper Thieves in Time/PS3_DISC.SFB");
        d("Sony Playstation 4");
        f("Sony Playstation Portable/Patapon.cso");
        f("Nintendo Switch/Celeste [01002B30028F6000][v0].nsp");
        f("PC Game/00356/Cairn.exe");
        f("PC Game/00356/RegroupLogs.exe");
        d("PC Game/00356/Cairn_Data/Managed");
        f("PC Game/00356/GameAssembly.dll");
        f("PC Game/00356/baselib.dll");
        d("PC Game/00356/D3D12");
        f("PC Game/01003/ObraDinn.exe");
        d("PC Game/01003/ObraDinn_Data");
        f("PC Game/01003/UnityPlayer.dll");
        for i in 0..40 {
            f(&format!("PC Game/00766/assets/file{i:02}.png"));
        }
        f("PC Game/00766/boot.xml");
        f("PC Game/00766/lime.ndll");
        f("PC Game/00766/Rogue Legacy.exe");
        f("Comic Books/00390/Volume 01.cbz");
        let root = dir.path().join(base);
        for folder in [
            "Sony Playstation",
            "Sony Playstation 2",
            "Sony Playstation 3",
            "Sony Playstation 4",
            "Sony Playstation Portable",
            "Nintendo Switch",
            "PC Game",
            "Comic Books",
            "Sony Playstation/Castlevania Symphony of the Night",
            "Sony Playstation 3/Sly Cooper Thieves in Time",
            "Sony Playstation 3/Sly Cooper Thieves in Time/PS3_GAME",
            "PC Game/00356",
            "PC Game/01003",
            "PC Game/00766",
            "PC Game/00356/Cairn.exe",
            "PC Game/00356/RegroupLogs.exe",
            "PC Game/01003/ObraDinn.exe",
            "Sony Playstation 2/Sly 3 Honor Among Thieves.iso",
        ] {
            hide(&root.join(folder));
        }
        root
    }

    #[cfg(windows)]
    #[test]
    fn hidden_system_library_scans_from_the_parent_and_from_a_system_folder() {
        let dir = TempDir::new("scan-library");
        let root = sample_library(&dir);
        let result = scan_games(&root, &ScanInputs::default(), &mut |_, _| {}).unwrap();
        assert_eq!(
            rom_names(&result),
            [
                "ps1:Castlevania Symphony of the Night (Japanese VA).chd",
                "ps1:Castlevania Symphony of the Night.chd",
                "ps2:Sly 3 Honor Among Thieves.iso",
                "ps3:EBOOT.BIN",
                "psp:Patapon.cso",
                "switch:Celeste [01002B30028F6000][v0].nsp",
            ]
        );
        let ps3 = result.roms.iter().find(|g| g.system == "ps3").unwrap();
        assert_eq!(ps3.title, "Sly Cooper: Thieves in Time");
        assert_eq!(ps3.format, "folder");
        assert!(ps3.path.ends_with("PS3_GAME/USRDIR/EBOOT.BIN"));
        let mut pcs: Vec<(String, String)> = result
            .pcs
            .iter()
            .map(|g| {
                (
                    g.title.clone(),
                    Path::new(&g.target)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect();
        pcs.sort();
        assert_eq!(
            pcs,
            [
                ("Cairn".to_string(), "Cairn.exe".to_string()),
                ("ObraDinn".to_string(), "ObraDinn.exe".to_string()),
                ("Rogue Legacy".to_string(), "Rogue Legacy.exe".to_string()),
            ]
        );
        assert!(result.ambiguous.is_empty(), "{:?}", result.ambiguous);
        assert!(result.unreadable.is_empty());
        // Comic Books holds no games: reported, nothing added.
        assert!(result
            .unsupported
            .iter()
            .any(|u| u.path.ends_with("Comic Books")));

        // The same folders added directly as roots.
        let ps1 = scan_games(
            &root.join("Sony Playstation"),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(rom_names(&ps1).len(), 2);
        assert!(ps1.unsupported.is_empty());
        let ps2 = scan_games(
            &root.join("Sony Playstation 2"),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(rom_names(&ps2), ["ps2:Sly 3 Honor Among Thieves.iso"]);
        let ps3 = scan_games(
            &root.join("Sony Playstation 3"),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(ps3.roms[0].title, "Sly Cooper: Thieves in Time");
        let pc = scan_games(
            &root.join("PC Game"),
            &ScanInputs::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(pc.pcs.len(), 3);
        // Hidden exes are candidates, and the Unity _Data folder picks Cairn over RegroupLogs.
        assert_eq!(
            pick_executable(&root.join("PC Game/00356"), "00356"),
            Pick::One(root.join("PC Game/00356/Cairn.exe"))
        );
    }

    #[test]
    fn pc_titles_come_from_the_executable_when_the_folder_is_a_number() {
        let exe = Path::new(r"D:\Games\PC\00356\Cairn.exe");
        assert_eq!(pc_title("00356", Some(exe)), "Cairn");
        assert_eq!(pc_title("Hollow Knight", Some(exe)), "Hollow Knight");
        assert_eq!(
            pc_title("01003 (v1.2)", Some(Path::new(r"D:\x\ObraDinn.exe"))),
            "ObraDinn"
        );
        assert_eq!(
            pc_title("00356", Some(Path::new(r"D:\x\play.lnk"))),
            "00356"
        );
        assert_eq!(pc_title("00356", None), "00356");
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_not_followed() {
        let dir = TempDir::new("scan-junction");
        let outside = TempDir::new("scan-junction-outside");
        outside.file("GBA/outside.gba");
        dir.file("GBA/inside.gba");
        let link = dir.path().join("GBA").join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path().join("GBA"))
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink failed");
        let result = scan(&dir);
        assert_eq!(rom_names(&result), ["gba:inside.gba"]);
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn index_file_parsers() {
        assert_eq!(
            cue_file("FILE \"a b.bin\" BINARY").as_deref(),
            Some("a b.bin")
        );
        assert_eq!(cue_file("FILE a.bin BINARY").as_deref(), Some("a.bin"));
        assert_eq!(cue_file("TRACK 01 AUDIO"), None);
        assert_eq!(gdi_file("3"), None);
        assert_eq!(
            gdi_file("1 0 4 2352 track01.bin 0").as_deref(),
            Some("track01.bin")
        );
        assert_eq!(
            gdi_file("2 1 0 2352 \"t 2.raw\" 0").as_deref(),
            Some("t 2.raw")
        );
        assert_eq!(
            parse_sfo_title(&sfo("Title  X")).as_deref(),
            Some("Title X")
        );
        assert_eq!(parse_sfo_title(b"\0PSF"), None);
        assert_eq!(parse_sfo_title(b"junk"), None);
    }
    #[cfg(windows)]
    #[test]
    fn shortcut_confirmations_match_short_and_long_forms() {
        use crate::platform::{create_shortcut, short_path};
        let dir = TempDir::new("scan-short-lnk");
        let tool = dir.file("Long Tools Folder/tool.exe");
        dir.dir("Long Shortcut Folder");
        let lnk = dir.path().join("Long Shortcut Folder/Server Join.lnk");
        create_shortcut(&lnk, &tool.to_string_lossy(), "-connect x").unwrap();
        let ShortcutKind::NeedsConfirmation { target, arguments } =
            classify_shortcut(&lnk, &HashMap::new())
        else {
            panic!("needs confirmation")
        };
        let short = short_path(&lnk);
        assert_eq!(key_of(&short), key_of(&lnk));
        // Confirmed under the short form, looked up by the long one, and the other way round.
        let by_short: HashMap<String, (String, String)> =
            [(key_of(&short), (target.clone(), arguments.clone()))].into();
        assert_eq!(classify_shortcut(&lnk, &by_short), ShortcutKind::Confirmed);
        let by_long: HashMap<String, (String, String)> =
            [(key_of(&lnk), (target, arguments))].into();
        assert_eq!(classify_shortcut(&short, &by_long), ShortcutKind::Confirmed);
        // Paths that don't exist keep plain normalization.
        assert_eq!(
            path_key(r"D:\Nope~1\Game.iso").as_deref(),
            Some(r"d:\nope~1\game.iso")
        );
        // A file under a short-named folder that was deleted still keys like its long form.
        let gone = dir.path().join("Long Tools Folder/deleted.exe");
        let short_gone = short_path(&dir.path().join("Long Tools Folder")).join("deleted.exe");
        assert_eq!(key_of(&short_gone), key_of(&gone));
    }
}
