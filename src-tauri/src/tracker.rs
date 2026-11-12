//! Running-game detection, play sessions and playtime.
//!
//! One background monitor ([`start`], spawned at app setup) polls the process list every
//! [`POLL_INTERVAL`] while `settings.tracking` is on. Each running executable is attributed to
//! the library game whose install folder contains it ([`Targets::attribute`]; the most
//! specific folder wins when install folders nest). With `detectExternalLaunches` on, any
//! installed game is detected however it was started; with it off, only games launched from
//! Game Lobby ([`Tracker::watch`]) are tracked, and the process list isn't read at all while
//! nothing is launching or running.
//!
//! False positives are avoided by:
//! - [`PathGuard`]: install folders that are drive roots, generic container folders
//!   (`Program Files`, `steamapps\common`, `Games`, ...), system/user folders (or their
//!   parents), or a store client's own folder are never used.
//! - [`is_helper_executable`]: crash handlers, uninstallers, installers, redistributables and
//!   anti-cheat setup programs never count as the game running.
//! - Game Lobby's own executable is ignored.
//! - A matched process only counts while it owns a play window
//!   (`platform::is_play_window`: visible, not DWM-cloaked, not a tool window, an owner-less
//!   or app window with a real size; minimized counts). Windows are enumerated once per poll,
//!   and only when some process matches a game. Services and wallpaper renderers never count.
//! - [`Policy`] (from the settings at every poll): excluded games, and detected games while
//!   `detectExternalLaunches` is off, are closed on the next poll and never picked up.
//!
//! Sessions are driven by the pure state machine [`Sessions`] (clock injected, unit-tested)
//! that turns each poll into [`Effect`]s written by [`apply`]:
//! - A session is stored once it reaches [`MIN_SESSION_SECONDS`] (shorter ones are dropped
//!   entirely); after that its length and the game's playtime are saved on every poll, so a
//!   crash loses at most one interval. `endedAt` stays empty while it runs.
//! - When the process has been gone for [`EXIT_GRACE_MS`] the session is closed at the last
//!   time it was seen. A gap longer than that (e.g. the PC slept) also splits the session.
//! - A Game Lobby launch waits in the `launching` state for up to [`START_TIMEOUT_MS`].
//!
//! Locking: the session state (`Tracker::sessions`) may be held while taking the database
//! lock, never the other way round. The install-folder snapshot (`Tracker::targets`) is a leaf
//! lock. No lock is held across an `.await`.
//!
//! Emulator games: a ROM route's install folder is its ROM folder and the emulator is shared
//! by many games, so ROM routes are never detection targets, and neither is anything inside
//! an emulator library root or an emulator's folder. A ROM game is tracked only when Game
//! Lobby launched it: [`Tracker::watch_process`] follows the emulator process it started
//! (by process id and executable path, no window needed) until it exits.
//!
//! Events: `tracking:changed` (payload `RunningGame[]`) whenever the running set or a state
//! changes, and at least every [`HEARTBEAT_MS`] while something runs; `library:changed` when
//! stored playtime changes (first save, each full minute, session end).

use crate::db::{self, Db};
use crate::platform::{processes_with_play_windows, running_processes, ProcessInfo};
use crate::state::AppState;
use crate::types::{now_ms, Game, Settings};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How long a game's process may be missing before its session is closed.
pub const EXIT_GRACE_MS: i64 = 30_000;
/// How long a Game Lobby launch waits for the game's process to appear.
pub const START_TIMEOUT_MS: i64 = 10 * 60_000;
/// Sessions shorter than this are not stored and add no playtime.
pub const MIN_SESSION_SECONDS: i64 = 10;
/// `tracking:changed` is emitted at least this often while a game runs.
pub const HEARTBEAT_MS: i64 = 60_000;

// ---------------------------------------------------------------------------
// Paths (pure)

/// Lower-case absolute path with `\` separators, no `\\?\` prefix, no `.`/`..`/empty
/// segments and no trailing separator (a drive root keeps it: `c:\`). Trailing dots and
/// spaces are dropped from segments, as Windows does. `None` for relative paths.
pub fn normalize_path(raw: &str) -> Option<String> {
    let mut path = raw.trim().replace('/', "\\");
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        path = format!(r"\\{rest}");
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        path = rest.to_string();
    }
    let path = path.to_lowercase();
    let (root, rest, min_segments) = if let Some(rest) = path.strip_prefix(r"\\") {
        // \\server\share is the root of a UNC path.
        (String::from(r"\"), rest, 2)
    } else {
        let bytes = path.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return None;
        }
        (path[..2].to_string(), &path[3..], 0)
    };
    let mut segments: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                if segments.len() > min_segments {
                    segments.pop();
                }
            }
            part => {
                let part = part.trim_end_matches(['.', ' ']);
                if !part.is_empty() {
                    segments.push(part);
                }
            }
        }
    }
    if segments.len() < min_segments {
        return None;
    }
    Some(if segments.is_empty() {
        format!("{root}\\")
    } else {
        format!("{root}\\{}", segments.join("\\"))
    })
}

/// True when normalized `path` is `folder` or inside it.
pub fn is_within(folder: &str, path: &str) -> bool {
    path == folder
        || (path.len() > folder.len()
            && path.starts_with(folder)
            && (folder.ends_with('\\') || path.as_bytes()[folder.len()] == b'\\'))
}

/// Folder segments below the root (`c:\a\b` → 2; `\\server\share\a` → 1).
fn depth(path: &str) -> usize {
    let segments = path.split('\\').filter(|s| !s.is_empty()).count();
    if path.starts_with(r"\\") {
        segments.saturating_sub(2)
    } else {
        segments.saturating_sub(1)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

/// Folder names that hold many games (or programs) and are never one game's install folder.
const GENERIC_FOLDERS: &[&str] = &[
    "program files",
    "program files (x86)",
    "programdata",
    "windows",
    "users",
    "public",
    "appdata",
    "local",
    "locallow",
    "roaming",
    "programs",
    "temp",
    "desktop",
    "documents",
    "downloads",
    "games",
    "my games",
    "steam",
    "steamlibrary",
    "steam library",
    "steamapps",
    "common",
    "epic games",
    "epicgames",
    "gog games",
    "gog galaxy",
    "gog.com",
    "xboxgames",
    "windowsapps",
    "modifiablewindowsapps",
    "ea games",
    "origin games",
    "electronic arts",
    "ubisoft",
    "ubisoft game launcher",
    "battle.net",
    "amazon games",
    "itch",
    "apps",
];

/// A store client's folder. Only its game library subfolders (absolute, normalized) may hold
/// install folders; anything else inside it is the client itself.
#[derive(Clone, Debug, Default)]
pub struct Launcher {
    pub folder: String,
    pub libraries: Vec<String>,
}

/// Decides which install folders are specific enough to attribute processes to.
#[derive(Clone, Debug, Default)]
pub struct PathGuard {
    /// System and user folders: never an install folder, nor a parent of one of them.
    pub system: Vec<String>,
    /// Trees no game is installed in (the Windows folder).
    pub forbidden: Vec<String>,
    pub launchers: Vec<Launcher>,
    /// Game Lobby's own executable.
    pub self_exe: Option<String>,
}

impl PathGuard {
    /// Builds the guard from the environment and the Steam library list. Called once.
    pub fn from_environment() -> Self {
        let env = |name: &str| std::env::var(name).ok().and_then(|v| normalize_path(&v));
        let mut guard = PathGuard::default();
        let windows = env("SystemRoot")
            .or_else(|| env("windir"))
            .unwrap_or_else(|| r"c:\windows".into());
        guard.forbidden.push(windows.clone());
        guard.system.push(windows);
        for name in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
            "ProgramData",
            "LOCALAPPDATA",
            "APPDATA",
            "USERPROFILE",
            "PUBLIC",
            "TEMP",
            "TMP",
        ] {
            if let Some(path) = env(name) {
                guard.system.push(path);
            }
        }
        if let Some(local) = env("LOCALAPPDATA") {
            guard.system.push(format!(r"{local}\programs"));
        }
        let mut program_dirs: Vec<String> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(|n| env(n))
            .collect();
        program_dirs.extend([r"c:\program files".into(), r"c:\program files (x86)".into()]);
        program_dirs.sort();
        program_dirs.dedup();
        for base in &program_dirs {
            for (folder, libraries) in [
                ("steam", &["steamapps\\common"][..]),
                ("epic games\\launcher", &[][..]),
                ("gog galaxy", &["games"][..]),
                ("electronic arts\\ea desktop", &[][..]),
                ("origin", &[][..]),
                ("ubisoft\\ubisoft game launcher", &["games"][..]),
                ("battle.net", &[][..]),
            ] {
                guard
                    .launchers
                    .push(launcher(&format!(r"{base}\{folder}"), libraries));
            }
        }
        if let Some(local) = env("LOCALAPPDATA") {
            guard
                .launchers
                .push(launcher(&format!(r"{local}\amazon games\app"), &[]));
            guard
                .launchers
                .push(launcher(&format!(r"{local}\itch"), &[]));
        }
        // Steam client and library folders (D:\SteamLibrary etc.): only steamapps\common.
        for root in crate::importers::steam::steam_roots().unwrap_or_default() {
            if let Some(root) = normalize_path(&root.to_string_lossy()) {
                guard
                    .launchers
                    .push(launcher(&root, &["steamapps\\common"]));
            }
        }
        guard.self_exe = std::env::current_exe()
            .ok()
            .and_then(|p| normalize_path(&p.to_string_lossy()));
        guard
    }

    /// Whether a normalized install folder may be used to attribute processes.
    pub fn allows_install_path(&self, path: &str) -> bool {
        if depth(path) == 0 || GENERIC_FOLDERS.contains(&file_name(path)) {
            return false;
        }
        if self.forbidden.iter().any(|f| is_within(f, path))
            || self.system.iter().any(|s| is_within(path, s))
        {
            return false;
        }
        self.launchers.iter().all(|l| {
            if is_within(path, &l.folder) {
                return false; // The client folder itself, or a folder containing it.
            }
            !is_within(&l.folder, path)
                || l.libraries
                    .iter()
                    .any(|lib| path != lib && is_within(lib, path))
        })
    }

    pub fn is_self(&self, exe: &str) -> bool {
        self.self_exe.as_deref() == Some(exe)
    }
}

fn launcher(folder: &str, libraries: &[&str]) -> Launcher {
    let folder = normalize_path(folder).unwrap_or_default();
    Launcher {
        libraries: libraries.iter().map(|l| format!(r"{folder}\{l}")).collect(),
        folder,
    }
}

/// Executable name fragments of helpers that run beside (or instead of) a game.
const HELPER_NAME_PARTS: &[&str] = &[
    "crashhandler",
    "crash_handler",
    "crashreport",
    "crash_report",
    "crashpad",
    "crashsender",
    "crashuploader",
    "errorreporter",
    "bugsplat",
    "bugreporter",
    "unins",
    "uninst",
    "setup",
    "install",
    "redist",
    "prereq",
    "dxwebsetup",
    "dotnetfx",
    "windowsdesktop-runtime",
    "oalinst",
    "physx",
    "easyanticheat",
    "beservice",
    "battleye",
    "touchup",
    "cleanup",
    "werfault",
];

/// Folder names that only hold installers and redistributables.
const HELPER_FOLDERS: &[&str] = &[
    "_commonredist",
    "commonredist",
    "redist",
    "_redist",
    "redistributables",
    "__installer",
    "_installer",
    "installer",
    "installers",
    "prerequisites",
    "directx",
    "vcredist",
    "dotnet",
    "easyanticheat",
    "battleye",
];

/// True for a normalized executable path that is a crash handler, uninstaller, installer,
/// redistributable or anti-cheat helper rather than the game itself.
pub fn is_helper_executable(exe: &str) -> bool {
    let name = file_name(exe);
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    if HELPER_NAME_PARTS.iter().any(|part| stem.contains(part)) || stem.starts_with("vc_") {
        return true;
    }
    let mut folders = exe.split('\\').rev().skip(1);
    folders.any(|folder| HELPER_FOLDERS.contains(&folder))
}

/// Normalized install folders of the library, most specific first.
#[derive(Debug, Default)]
pub struct Targets {
    /// (folder + `\`, game id), longest folder first.
    entries: Vec<(String, String)>,
}

impl Targets {
    /// Uses every installed route's install folder that the guard allows. ROM routes (and
    /// any route with an emulator) are skipped, as are folders that are inside, equal to or
    /// contain one of `excluded` (emulator roots and emulator folders, normalized).
    pub fn build(games: &[Game], guard: &PathGuard, excluded: &[String]) -> Self {
        let mut entries: Vec<(String, String)> = games
            .iter()
            .flat_map(|game| {
                game.routes
                    .iter()
                    .filter(|r| r.installed != Some(false))
                    .filter(|r| r.source_id != "rom" && r.emulator_id.is_none())
                    .filter_map(|r| r.install_path.as_deref())
                    .filter_map(normalize_path)
                    .filter(|path| guard.allows_install_path(path))
                    .filter(|path| {
                        !excluded
                            .iter()
                            .any(|e| is_within(e, path) || is_within(path, e))
                    })
                    .map(|path| (format!("{path}\\"), game.id.clone()))
            })
            .collect();
        entries.sort_by(|a, b| {
            b.0.len()
                .cmp(&a.0.len())
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        entries.dedup();
        Self { entries }
    }

    pub fn has_game(&self, game_id: &str) -> bool {
        self.entries.iter().any(|(_, id)| id == game_id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Running processes that belong to a library game: (game id, process). Helpers and Game
    /// Lobby itself are skipped; nested install folders go to the most specific one.
    pub fn candidates<'a>(
        &self,
        processes: &'a [ProcessInfo],
        guard: &PathGuard,
    ) -> Vec<(String, &'a ProcessInfo)> {
        processes
            .iter()
            .filter_map(|process| {
                let exe = normalize_path(&process.path)?;
                if guard.is_self(&exe) || is_helper_executable(&exe) {
                    return None;
                }
                self.entries
                    .iter()
                    .find(|(p, _)| exe.starts_with(p))
                    .map(|(_, id)| (id.clone(), process))
            })
            .collect()
    }

    /// Maps each game being played to its executable: a candidate process counts only when it
    /// owns a play window (`with_window` holds those process ids; see
    /// `platform::is_play_window`). Background services and tray helpers never count.
    pub fn attribute(
        &self,
        processes: &[ProcessInfo],
        guard: &PathGuard,
        with_window: &HashSet<u32>,
    ) -> HashMap<String, String> {
        let mut found = HashMap::new();
        for (id, process) in self.candidates(processes, guard) {
            if with_window.contains(&process.pid) {
                found.entry(id).or_insert_with(|| process.path.clone());
            }
        }
        found
    }
}

// ---------------------------------------------------------------------------
// Sessions (pure state machine; `now` is injected)

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Started from Game Lobby.
    Launch,
    /// Found running (started from a store client, a shortcut, ...).
    Detected,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Detected => "detected",
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Launching,
    Running,
}

/// One tracked game, as sent to the renderer (`tracking:changed`, `tracking_status`).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunningGame {
    pub game_id: String,
    pub state: RunState,
    /// Launch request time while launching; session start while running.
    pub started_at: i64,
    pub session_seconds: i64,
    pub source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A Game Lobby launch was first seen running: set `lastPlayed`.
    MarkPlayed { game_id: String, at: i64 },
    /// Store the session so far (inserting it when `session_id` is `None`) and add `credit`
    /// seconds to the game's playtime.
    Save {
        game_id: String,
        session_id: Option<i64>,
        started_at: i64,
        seconds: i64,
        credit: i64,
        source: Source,
    },
    /// The session ended.
    Close {
        session_id: i64,
        ended_at: i64,
        seconds: i64,
    },
}

#[derive(Debug, Default)]
pub struct Tick {
    pub effects: Vec<Effect>,
    /// The running set, a state or a minute count changed: emit `tracking:changed`.
    pub running_changed: bool,
    /// Stored playtime changed: emit `library:changed`.
    pub library_changed: bool,
}

#[derive(Debug, Clone)]
struct Run {
    started_at: i64,
    last_seen_at: i64,
    exe: String,
    session_id: Option<i64>,
    /// Session seconds already added to the game's playtime.
    credited: i64,
    notified_minute: i64,
}

impl Run {
    fn new(now: i64, exe: &str) -> Self {
        Self {
            started_at: now,
            last_seen_at: now,
            exe: exe.to_string(),
            session_id: None,
            credited: 0,
            notified_minute: 0,
        }
    }

    fn seconds(&self) -> i64 {
        (self.last_seen_at - self.started_at).max(0) / 1000
    }
}

#[derive(Debug, Clone)]
struct Entry {
    source: Source,
    requested_at: i64,
    run: Option<Run>,
}

/// What may be tracked, from the settings at each poll.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// `detectExternalLaunches`.
    pub detect_external: bool,
    /// `trackingExcludedGameIds`.
    pub excluded: HashSet<String>,
}

impl Policy {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            detect_external: settings.detect_external_launches,
            excluded: settings
                .tracking_excluded_game_ids
                .iter()
                .cloned()
                .collect(),
        }
    }

    pub fn allows(&self, game_id: &str, source: Source) -> bool {
        !self.excluded.contains(game_id) && (self.detect_external || source == Source::Launch)
    }
}

/// Tracked games keyed by game id.
#[derive(Debug, Default)]
pub struct Sessions {
    entries: BTreeMap<String, Entry>,
    pending_change: bool,
}

impl Sessions {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, game_id: &str) -> bool {
        self.entries.contains_key(game_id)
    }

    /// A Game Lobby launch: wait for the game's process. No-op if it is already tracked.
    pub fn launch(&mut self, game_id: &str, now: i64) {
        if !self.entries.contains_key(game_id) {
            self.entries.insert(
                game_id.to_string(),
                Entry {
                    source: Source::Launch,
                    requested_at: now,
                    run: None,
                },
            );
            self.pending_change = true;
        }
    }

    /// Advances every tracked game given the games being played now (game id → executable).
    /// Games the policy no longer allows (excluded, or detected while detection is off) are
    /// closed first, keeping the time already saved. With `policy.detect_external`, games
    /// seen running that aren't tracked yet are picked up.
    pub fn tick(&mut self, now: i64, seen: &HashMap<String, String>, policy: &Policy) -> Tick {
        let mut tick = Tick {
            running_changed: std::mem::take(&mut self.pending_change),
            ..Default::default()
        };
        self.entries.retain(|game_id, entry| {
            if policy.allows(game_id, entry.source) {
                return true;
            }
            if let Some(run) = entry.run.as_ref() {
                close(run, &mut tick);
            }
            tick.running_changed = true;
            false
        });
        if policy.detect_external {
            for game_id in seen.keys().filter(|id| !policy.excluded.contains(*id)) {
                self.entries.entry(game_id.clone()).or_insert(Entry {
                    source: Source::Detected,
                    requested_at: now,
                    run: None,
                });
            }
        }
        self.entries.retain(|game_id, entry| {
            let source = entry.source;
            match (seen.get(game_id), entry.run.as_mut()) {
                (Some(exe), None) => {
                    entry.run = Some(Run::new(now, exe));
                    tick.running_changed = true;
                    if source == Source::Launch {
                        tick.effects.push(Effect::MarkPlayed {
                            game_id: game_id.clone(),
                            at: now,
                        });
                    }
                    true
                }
                (Some(exe), Some(run)) if now - run.last_seen_at > EXIT_GRACE_MS => {
                    // Gone longer than the grace period (or the PC slept): a new session.
                    close(run, &mut tick);
                    *run = Run::new(now, exe);
                    tick.running_changed = true;
                    true
                }
                (Some(exe), Some(run)) => {
                    run.last_seen_at = run.last_seen_at.max(now);
                    if run.exe != *exe {
                        run.exe = exe.clone();
                        tick.running_changed = true;
                    }
                    save(game_id, source, run, &mut tick);
                    true
                }
                (None, Some(run)) => {
                    if now - run.last_seen_at > EXIT_GRACE_MS {
                        close(run, &mut tick);
                        tick.running_changed = true;
                        false
                    } else {
                        true
                    }
                }
                (None, None) => {
                    let waiting = now - entry.requested_at <= START_TIMEOUT_MS;
                    if !waiting {
                        tick.running_changed = true;
                    }
                    waiting
                }
            }
        });
        tick
    }

    /// Tracking was turned off: close every session and forget all games.
    pub fn stop_all(&mut self) -> Tick {
        let mut tick = Tick {
            running_changed: std::mem::take(&mut self.pending_change) || !self.is_empty(),
            ..Default::default()
        };
        for entry in self.entries.values_mut() {
            if let Some(run) = entry.run.as_mut() {
                close(run, &mut tick);
            }
        }
        self.entries.clear();
        tick
    }

    /// Records the id of a session stored by [`apply`].
    pub fn set_session_id(&mut self, game_id: &str, started_at: i64, id: i64) {
        if let Some(run) = self
            .entries
            .get_mut(game_id)
            .and_then(|e| e.run.as_mut())
            .filter(|r| r.started_at == started_at)
        {
            run.session_id = Some(id);
        }
    }

    /// The game was removed (its sessions are already deleted).
    pub fn forget(&mut self, game_id: &str) {
        if self.entries.remove(game_id).is_some() {
            self.pending_change = true;
        }
    }

    /// `source_id` was merged into `target_id` (sessions already moved). The source's run
    /// continues as the target's unless the target is tracked too; then it is closed.
    pub fn merge(&mut self, target_id: &str, source_id: &str) -> Tick {
        let mut tick = Tick::default();
        if let Some(mut entry) = self.entries.remove(source_id) {
            self.pending_change = true;
            if self.entries.contains_key(target_id) {
                if let Some(run) = entry.run.as_mut() {
                    close(run, &mut tick);
                }
            } else {
                self.entries.insert(target_id.to_string(), entry);
            }
        }
        tick
    }

    /// Playtime was reset (all sessions deleted): running games start a fresh session now.
    pub fn reset(&mut self, now: i64) {
        for entry in self.entries.values_mut() {
            if let Some(run) = entry.run.as_mut() {
                *run = Run::new(now, &run.exe.clone());
                self.pending_change = true;
            }
        }
    }

    /// What the renderer sees.
    pub fn snapshot(&self, now: i64) -> Vec<RunningGame> {
        self.entries
            .iter()
            .map(|(game_id, entry)| match &entry.run {
                Some(run) => RunningGame {
                    game_id: game_id.clone(),
                    state: RunState::Running,
                    started_at: run.started_at,
                    session_seconds: (now - run.started_at).max(0) / 1000,
                    source: entry.source,
                    exe: Some(run.exe.clone()),
                },
                None => RunningGame {
                    game_id: game_id.clone(),
                    state: RunState::Launching,
                    started_at: entry.requested_at,
                    session_seconds: 0,
                    source: entry.source,
                    exe: None,
                },
            })
            .collect()
    }
}

fn save(game_id: &str, source: Source, run: &mut Run, tick: &mut Tick) {
    let seconds = run.seconds();
    if seconds < MIN_SESSION_SECONDS {
        return;
    }
    if run.session_id.is_none() && run.credited == 0 {
        tick.library_changed = true;
    }
    tick.effects.push(Effect::Save {
        game_id: game_id.to_string(),
        session_id: run.session_id,
        started_at: run.started_at,
        seconds,
        credit: seconds - run.credited,
        source,
    });
    run.credited = seconds;
    let minute = seconds / 60;
    if minute > run.notified_minute {
        run.notified_minute = minute;
        tick.running_changed = true;
        tick.library_changed = true;
    }
}

fn close(run: &Run, tick: &mut Tick) {
    if let Some(session_id) = run.session_id {
        tick.effects.push(Effect::Close {
            session_id,
            ended_at: run.last_seen_at,
            seconds: run.seconds(),
        });
        tick.library_changed = true;
    }
}

/// Writes a tick's effects. Failures are logged; tracking carries on.
pub fn apply(conn: &Connection, sessions: &mut Sessions, effects: Vec<Effect>) {
    for effect in effects {
        let result = match effect {
            Effect::MarkPlayed { game_id, at } => db::mark_game_played(conn, &game_id, at),
            Effect::Save {
                game_id,
                session_id,
                started_at,
                seconds,
                credit,
                source,
            } => (|| -> anyhow::Result<()> {
                let tx = conn.unchecked_transaction()?;
                match session_id {
                    Some(id) => db::update_session(&tx, id, seconds)?,
                    None => {
                        if let Some(id) =
                            db::insert_session(&tx, &game_id, started_at, seconds, source.as_str())?
                        {
                            sessions.set_session_id(&game_id, started_at, id);
                        }
                    }
                }
                db::add_tracked_playtime(&tx, &game_id, credit, started_at)?;
                tx.commit()?;
                Ok(())
            })(),
            Effect::Close {
                session_id,
                ended_at,
                seconds,
            } => db::close_session(conn, session_id, ended_at, seconds),
        };
        if let Err(e) = result {
            eprintln!("Could not save play session: {e:#}");
        }
    }
}

// ---------------------------------------------------------------------------
// The monitor

/// A process Game Lobby started for a game (emulator launches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedProcess {
    pub pid: u32,
    /// Normalized executable path; guards against process id reuse.
    pub exe: String,
}

/// Games whose launched process is running now: matched by process id and executable.
pub fn watched_running(
    watched: &HashMap<String, WatchedProcess>,
    processes: &[ProcessInfo],
) -> HashMap<String, String> {
    watched
        .iter()
        .filter_map(|(game_id, w)| {
            processes
                .iter()
                .find(|p| p.pid == w.pid && normalize_path(&p.path).as_deref() == Some(&w.exe))
                .map(|p| (game_id.clone(), p.path.clone()))
        })
        .collect()
}

#[derive(Default)]
pub struct Tracker {
    sessions: Mutex<Sessions>,
    /// Emulator processes started by Game Lobby, by game id (a leaf lock).
    watched: Mutex<HashMap<String, WatchedProcess>>,
    targets: Mutex<Arc<Targets>>,
    /// False when the library changed since `targets` was built.
    targets_current: AtomicBool,
    guard: OnceLock<PathGuard>,
    wake: Notify,
}

impl Tracker {
    /// The session state. May be held while locking the database, never the reverse.
    pub fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn guard(&self) -> &PathGuard {
        self.guard.get_or_init(PathGuard::from_environment)
    }

    /// Runs a poll now instead of waiting for the next interval.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Games were added, removed or edited: rebuild the install folders on the next poll.
    pub fn library_changed(&self) {
        self.targets_current.store(false, Ordering::SeqCst);
        self.wake();
    }

    /// The install folders, rebuilt from the database only when the library changed.
    /// Locks the database briefly; must not be called while holding it.
    pub fn targets(&self, db: &Db) -> Arc<Targets> {
        if !self.targets_current.swap(true, Ordering::SeqCst) {
            let loaded = (|| -> anyhow::Result<(Vec<Game>, Vec<String>)> {
                let conn = db.lock();
                let games = db::all_games(&conn)?;
                let mut excluded: Vec<String> = crate::roots::list(&conn)?
                    .into_iter()
                    .filter(|r| r.kind == "emulators")
                    .map(|r| r.key())
                    .collect();
                excluded.extend(
                    db::emulator_profiles(&conn)?
                        .iter()
                        .filter_map(|p| normalize_path(&p.executable))
                        .filter_map(|exe| exe.rsplit_once('\\').map(|(dir, _)| dir.to_string()))
                        .filter(|dir| depth(dir) > 0),
                );
                Ok((games, excluded))
            })();
            match loaded {
                Ok((games, excluded)) => {
                    let built = Arc::new(Targets::build(&games, self.guard(), &excluded));
                    *self.targets.lock().unwrap_or_else(|p| p.into_inner()) = built;
                }
                Err(e) => {
                    self.targets_current.store(false, Ordering::SeqCst);
                    eprintln!("Game process tracking could not read the library: {e:#}");
                }
            }
        }
        self.targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// A game was launched from Game Lobby. Returns false when it can't be tracked (tracking
    /// off, the game excluded, or no usable install folder); the caller then just marks it
    /// played.
    pub fn watch(&self, db: &Db, game_id: &str, settings: &Settings) -> bool {
        if !settings.tracking
            || !cfg!(windows)
            || settings
                .tracking_excluded_game_ids
                .iter()
                .any(|id| id == game_id)
            || !self.targets(db).has_game(game_id)
        {
            return false;
        }
        self.sessions().launch(game_id, now_ms());
        self.wake();
        true
    }

    fn watched(&self) -> MutexGuard<'_, HashMap<String, WatchedProcess>> {
        self.watched.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// An emulator Game Lobby started for `game_id` (process `pid`, executable `exe`).
    /// Returns false when it can't be tracked (tracking off, the game excluded); the caller
    /// then just marks it played. The session ends when that process exits.
    pub fn watch_process(&self, game_id: &str, pid: u32, exe: &str, settings: &Settings) -> bool {
        let Some(exe) = normalize_path(exe) else {
            return false;
        };
        if !settings.tracking
            || !cfg!(windows)
            || settings
                .tracking_excluded_game_ids
                .iter()
                .any(|id| id == game_id)
        {
            return false;
        }
        let mut sessions = self.sessions();
        self.watched()
            .insert(game_id.to_string(), WatchedProcess { pid, exe });
        sessions.launch(game_id, now_ms());
        drop(sessions);
        self.wake();
        true
    }

    pub fn status(&self) -> Vec<RunningGame> {
        self.sessions().snapshot(now_ms())
    }
}

/// Starts the monitor. Called once at app setup.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_emit = 0;
        loop {
            poll(&app, &mut last_emit).await;
            let state = app.state::<AppState>();
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = state.tracker.wake.notified() => {}
            }
        }
    });
}

async fn poll(app: &AppHandle, last_emit: &mut i64) {
    let state = app.state::<AppState>();
    let tracker = &state.tracker;
    let settings = db::get_settings(&state.db.lock()).unwrap_or_default();
    let tracking = settings.tracking && cfg!(windows);
    let policy = Policy::from_settings(&settings);
    let seen = if !tracking || (!policy.detect_external && tracker.sessions().is_empty()) {
        HashMap::new()
    } else {
        let targets = tracker.targets(&state.db);
        if targets.is_empty() && tracker.sessions().is_empty() {
            HashMap::new()
        } else {
            let processes = match tauri::async_runtime::spawn_blocking(running_processes).await {
                Ok(Ok(processes)) => processes,
                // Skip this poll rather than treat every game as stopped.
                Ok(Err(e)) => {
                    return eprintln!("Game process tracking could not read processes: {e}")
                }
                Err(e) => return eprintln!("Game process tracking failed: {e}"),
            };
            // Windows are only enumerated when some process could belong to a game.
            let with_window = if targets.candidates(&processes, tracker.guard()).is_empty() {
                HashSet::new()
            } else {
                match tauri::async_runtime::spawn_blocking(processes_with_play_windows).await {
                    Ok(pids) => pids,
                    Err(e) => return eprintln!("Game window check failed: {e}"),
                }
            };
            let mut seen = targets.attribute(&processes, tracker.guard(), &with_window);
            // Launched emulators count by process, without a window check.
            seen.extend(watched_running(&tracker.watched(), &processes));
            seen
        }
    };
    let now = now_ms();
    let (tick, snapshot) = {
        let mut sessions = tracker.sessions();
        let mut tick = if tracking {
            sessions.tick(now, &seen, &policy)
        } else {
            sessions.stop_all()
        };
        let effects = std::mem::take(&mut tick.effects);
        if !effects.is_empty() {
            apply(&state.db.lock(), &mut sessions, effects);
        }
        // A watched process whose game is no longer tracked is forgotten.
        tracker
            .watched()
            .retain(|game_id, _| sessions.contains(game_id));
        (tick, sessions.snapshot(now))
    };
    if tick.library_changed {
        let _ = app.emit("library:changed", ());
    }
    if tick.running_changed || (!snapshot.is_empty() && now - *last_emit >= HEARTBEAT_MS) {
        *last_emit = now;
        let _ = app.emit("tracking:changed", &snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameRoute;

    fn n(path: &str) -> String {
        normalize_path(path).unwrap()
    }

    fn guard() -> PathGuard {
        PathGuard {
            system: [
                r"C:\Windows",
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\ProgramData",
                r"C:\Users\me",
                r"C:\Users\me\AppData\Local",
                r"C:\Users\me\AppData\Roaming",
                r"C:\Users\me\AppData\Local\Programs",
            ]
            .map(n)
            .to_vec(),
            forbidden: vec![n(r"C:\Windows")],
            launchers: vec![
                launcher(r"C:\Program Files (x86)\Steam", &["steamapps\\common"]),
                launcher(r"D:\SteamLibrary", &["steamapps\\common"]),
                launcher(r"C:\Program Files (x86)\Epic Games\Launcher", &[]),
                launcher(r"C:\Program Files (x86)\GOG Galaxy", &["games"]),
                launcher(r"C:\Program Files\Electronic Arts\EA Desktop", &[]),
                launcher(
                    r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher",
                    &["games"],
                ),
                launcher(r"C:\Program Files (x86)\Battle.net", &[]),
            ],
            self_exe: Some(n(r"C:\Users\me\AppData\Local\Game Lobby\game-lobby.exe")),
        }
    }

    #[test]
    fn normalizes_windows_paths() {
        assert_eq!(n(r"C:\Games\Foo\"), r"c:\games\foo");
        assert_eq!(n("c:/Games//Foo/./bin/../x.EXE"), r"c:\games\foo\x.exe");
        assert_eq!(n(r"\\?\C:\Games\Foo"), r"c:\games\foo");
        assert_eq!(n(r"\\?\UNC\nas\share\Games\Foo"), r"\\nas\share\games\foo");
        assert_eq!(n(r"C:\"), r"c:\");
        assert_eq!(n(r"C:\.."), r"c:\");
        assert_eq!(n(r"C:\Games\Foo. \x"), r"c:\games\foo\x");
        assert_eq!(normalize_path(r"Games\Foo"), None);
        assert_eq!(normalize_path("C:"), None);
        assert_eq!(normalize_path(r"\\nas"), None);
        assert_eq!(normalize_path(""), None);
        assert!(is_within(r"c:\games\foo", r"c:\games\foo\x.exe"));
        assert!(is_within(r"c:\", r"c:\games"));
        assert!(!is_within(r"c:\games\foo", r"c:\games\foobar\x.exe"));
        assert!(!is_within(r"c:\games\foo\x", r"c:\games\foo"));
    }

    #[test]
    fn rejects_roots_generic_system_and_launcher_folders() {
        let g = guard();
        for bad in [
            r"C:\",
            r"D:\",
            r"\\nas\share",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\Users",
            r"C:\Users\me",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me\AppData\Local\Programs",
            r"C:\Windows",
            r"C:\Windows\System32\SomeTool",
            r"C:\Program Files (x86)\Steam",
            r"C:\Program Files (x86)\Steam\steamapps",
            r"C:\Program Files (x86)\Steam\steamapps\common",
            r"C:\Program Files (x86)\Steam\bin",
            r"D:\SteamLibrary",
            r"D:\SteamLibrary\steamapps\common",
            r"E:\SteamLibrary\steamapps\common",
            r"C:\Program Files (x86)\Epic Games",
            r"C:\Program Files (x86)\Epic Games\Launcher",
            r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries",
            r"C:\Program Files (x86)\GOG Galaxy",
            r"C:\Program Files (x86)\GOG Galaxy\Games",
            r"C:\Program Files\Electronic Arts\EA Desktop",
            r"C:\Program Files\Electronic Arts",
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher",
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games",
            r"C:\Program Files (x86)\Battle.net",
            r"D:\Games",
            r"D:\XboxGames",
            r"C:\GOG Games",
        ] {
            assert!(!g.allows_install_path(&n(bad)), "{bad} should be rejected");
        }
        for good in [
            r"C:\Program Files (x86)\Steam\steamapps\common\Portal 2",
            r"D:\SteamLibrary\steamapps\common\Hades",
            r"C:\Program Files\Epic Games\Fortnite",
            r"C:\Program Files (x86)\GOG Galaxy\Games\Witcher 3",
            r"C:\GOG Games\Cyberpunk 2077",
            r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Anno 1800",
            r"C:\Program Files (x86)\Overwatch",
            r"C:\Program Files\EA Games\Battlefield 2042",
            r"D:\Cyberpunk 2077",
            r"C:\XboxGames\Halo Infinite\Content",
            r"C:\Users\me\AppData\Roaming\itch\apps\celeste",
        ] {
            assert!(g.allows_install_path(&n(good)), "{good} should be allowed");
        }
    }

    #[test]
    fn helper_executables_are_ignored() {
        for helper in [
            r"c:\games\foo\unitycrashhandler64.exe",
            r"c:\games\foo\unins000.exe",
            r"c:\games\foo\uninstall.exe",
            r"c:\games\foo\setup.exe",
            r"c:\games\foo\installer.exe",
            r"c:\games\foo\vc_redist.x64.exe",
            r"c:\games\foo\_commonredist\vcredist\2019\vc_redist.x64.exe",
            r"c:\games\foo\_commonredist\directx\jun2010\dxsetup.exe",
            r"c:\games\foo\easyanticheat\easyanticheat_eos_setup.exe",
            r"c:\games\foo\battleye\beservice_x64.exe",
            r"c:\games\foo\engine\binaries\win64\crashreportclient.exe",
            r"c:\games\foo\crashpad_handler.exe",
            r"c:\games\foo\ue4prereqsetup_x64.exe",
            r"c:\games\foo\__installer\touchup.exe",
        ] {
            assert!(is_helper_executable(helper), "{helper} is a helper");
        }
        for game in [
            r"c:\games\foo\foo.exe",
            r"c:\games\foo\bin\x64\game.exe",
            r"c:\games\foo\start_protected_game.exe",
            r"c:\games\foo\launcher.exe",
        ] {
            assert!(!is_helper_executable(game), "{game} is a game");
        }
    }

    fn game(id: &str, paths: &[&str]) -> Game {
        Game {
            id: id.into(),
            title: id.into(),
            store: "Custom".into(),
            routes: paths
                .iter()
                .map(|p| GameRoute {
                    source_id: "custom".into(),
                    store: "Custom".into(),
                    launch_target: "x".into(),
                    install_path: Some(p.to_string()),
                    installed: Some(true),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn attributes_processes_to_the_most_specific_install_folder() {
        let g = guard();
        let mut uninstalled = game("old", &[r"E:\Old Game"]);
        uninstalled.routes[0].installed = Some(false);
        let targets = Targets::build(
            &[
                game("outer", &[r"D:\Games\Collection"]),
                game("inner", &[r"D:\Games\Collection\Episode 2\"]),
                game(
                    "generic",
                    &[r"C:\Program Files (x86)\Steam\steamapps\common"],
                ),
                game("root", &[r"D:\"]),
                game("lobby", &[r"C:\Users\me\AppData\Local\Game Lobby"]),
                uninstalled,
            ],
            &g,
            &[],
        );
        assert!(targets.has_game("outer") && targets.has_game("inner"));
        assert!(!targets.has_game("generic") && !targets.has_game("root"));
        assert!(!targets.has_game("old"));
        let processes = procs(&[
            r"D:\Games\Collection\Episode 2\ep2.exe",
            r"D:\Games\Collection\launcher.exe",
            r"C:\Program Files (x86)\Steam\steamapps\common\Other\other.exe",
            r"D:\Games\CollectionX\nope.exe",
            r"C:\Users\me\AppData\Local\Game Lobby\game-lobby.exe",
            r"D:\random.exe",
        ]);
        let found = targets.attribute(&processes, &g, &all_pids(&processes));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found["inner"], r"D:\Games\Collection\Episode 2\ep2.exe");
        assert_eq!(found["outer"], r"D:\Games\Collection\launcher.exe");
        // Only a helper running: not counted as the game.
        let helpers = procs(&[r"D:\Games\Collection\UnityCrashHandler64.exe"]);
        assert!(targets.candidates(&helpers, &g).is_empty());
        assert!(targets
            .attribute(&helpers, &g, &all_pids(&helpers))
            .is_empty());
        // Game Lobby's own process never counts, even inside a game's install folder.
        assert!(targets.has_game("lobby") && !found.contains_key("lobby"));
    }

    const T0: i64 = 1_700_000_000_000;

    fn on() -> Policy {
        Policy {
            detect_external: true,
            ..Default::default()
        }
    }

    fn off() -> Policy {
        Policy::default()
    }

    fn procs(paths: &[&str]) -> Vec<ProcessInfo> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| ProcessInfo {
                path: p.to_string(),
                pid: 100 + i as u32,
            })
            .collect()
    }

    fn all_pids(processes: &[ProcessInfo]) -> HashSet<u32> {
        processes.iter().map(|p| p.pid).collect()
    }

    fn seen(ids: &[&str]) -> HashMap<String, String> {
        ids.iter()
            .map(|id| (id.to_string(), format!(r"C:\Games\{id}\{id}.exe")))
            .collect()
    }

    fn saves(tick: &Tick) -> Vec<(i64, i64, Option<i64>)> {
        tick.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Save {
                    seconds,
                    credit,
                    session_id,
                    ..
                } => Some((*seconds, *credit, *session_id)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn detected_session_accumulates_and_closes_after_the_grace_period() {
        let mut s = Sessions::default();
        let t = s.tick(T0, &seen(&["a"]), &on());
        assert!(t.running_changed && t.effects.is_empty());
        let snap = s.snapshot(T0);
        assert_eq!(snap.len(), 1);
        assert_eq!(
            (snap[0].state, snap[0].source, snap[0].started_at),
            (RunState::Running, Source::Detected, T0)
        );
        assert_eq!(snap[0].exe.as_deref(), Some(r"C:\Games\a\a.exe"));
        // Under the minimum: nothing stored yet.
        assert!(s.tick(T0 + 5_000, &seen(&["a"]), &on()).effects.is_empty());
        let t = s.tick(T0 + 10_000, &seen(&["a"]), &on());
        assert_eq!(saves(&t), [(10, 10, None)]);
        assert!(t.library_changed);
        s.set_session_id("a", T0, 7);
        let t = s.tick(T0 + 15_000, &seen(&["a"]), &on());
        assert_eq!(saves(&t), [(15, 5, Some(7))]);
        assert!(!t.running_changed && !t.library_changed);
        let t = s.tick(T0 + 40_000, &seen(&["a"]), &on());
        assert_eq!(saves(&t), [(40, 25, Some(7))]);
        assert!(!t.running_changed && !t.library_changed);
        // Each full minute is announced.
        let t = s.tick(T0 + 61_000, &seen(&["a"]), &on());
        assert_eq!(saves(&t), [(61, 21, Some(7))]);
        assert!(t.running_changed && t.library_changed);
        // Gone, but within the grace period: still running, nothing saved.
        let t = s.tick(T0 + 80_000, &seen(&[]), &on());
        assert!(t.effects.is_empty() && !t.running_changed);
        assert_eq!(s.snapshot(T0 + 80_000).len(), 1);
        // Grace over: closed at the last sighting.
        let t = s.tick(T0 + 95_000, &seen(&[]), &on());
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 7,
                ended_at: T0 + 61_000,
                seconds: 61
            }]
        );
        assert!(t.running_changed && s.is_empty());
    }

    #[test]
    fn only_processes_with_a_play_window_count() {
        let g = guard();
        let targets = Targets::build(
            &[
                game(
                    "wallpaper",
                    &[r"M:\SteamLibrary\steamapps\common\wallpaper_engine"],
                ),
                game(
                    "service",
                    &[r"C:\Program Files\WindowsApps\OneGameLauncher_2.2.9.0_x64"],
                ),
                game("game", &[r"D:\Games\Hades"]),
            ],
            &g,
            &[],
        );
        let processes = procs(&[
            r"M:\SteamLibrary\steamapps\common\wallpaper_engine\bin\webwallpaper64.exe",
            r"C:\Program Files\WindowsApps\OneGameLauncher_2.2.9.0_x64\Service\OneGameLauncherService.exe",
            r"D:\Games\Hades\Hades.exe",
            r"D:\Games\Hades\x64\Hades.exe",
        ]);
        // All four match a game by path...
        assert_eq!(targets.candidates(&processes, &g).len(), 4);
        // ...but only the processes owning a play window count (here the second Hades exe).
        let found = targets.attribute(&processes, &g, &HashSet::from([103]));
        assert_eq!(found.len(), 1);
        assert_eq!(found["game"], r"D:\Games\Hades\x64\Hades.exe");
        assert!(targets
            .attribute(&processes, &g, &HashSet::new())
            .is_empty());
    }

    #[test]
    fn turning_detection_off_closes_detected_sessions_at_once() {
        let mut s = Sessions::default();
        s.launch("launched", T0);
        s.tick(T0, &seen(&["launched", "external"]), &on());
        s.tick(T0 + 12_000, &seen(&["launched", "external"]), &on());
        s.set_session_id("launched", T0, 1);
        s.set_session_id("external", T0, 2);
        // Next poll after the settings change: still running, but no longer allowed.
        let t = s.tick(T0 + 14_000, &seen(&["launched", "external"]), &off());
        assert!(t.running_changed && t.library_changed);
        assert!(t.effects.contains(&Effect::Close {
            session_id: 2,
            ended_at: T0 + 12_000,
            seconds: 12
        }));
        let snap = s.snapshot(T0 + 14_000);
        assert_eq!(snap.len(), 1);
        assert_eq!(
            (snap[0].game_id.as_str(), snap[0].source),
            ("launched", Source::Launch)
        );
        // Not picked up again while detection stays off.
        s.tick(T0 + 20_000, &seen(&["launched", "external"]), &off());
        assert_eq!(s.snapshot(T0 + 20_000).len(), 1);
        // Tracking off: everything closes.
        let t = s.stop_all();
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 1,
                ended_at: T0 + 20_000,
                seconds: 20
            }]
        );
        assert!(t.running_changed && s.is_empty());
    }

    #[test]
    fn excluded_games_are_never_tracked_and_running_ones_close() {
        let excluded = |ids: &[&str]| Policy {
            detect_external: true,
            excluded: ids.iter().map(|s| s.to_string()).collect(),
        };
        let mut s = Sessions::default();
        let t = s.tick(T0, &seen(&["a", "b"]), &excluded(&["b"]));
        assert!(t.running_changed);
        assert_eq!(s.snapshot(T0).len(), 1);
        s.tick(T0 + 10_000, &seen(&["a", "b"]), &excluded(&["b"]));
        s.set_session_id("a", T0, 5);
        // Excluding a running game closes its session on the next poll.
        let t = s.tick(T0 + 15_000, &seen(&["a", "b"]), &excluded(&["a", "b"]));
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 5,
                ended_at: T0 + 10_000,
                seconds: 10
            }]
        );
        assert!(s.is_empty());
        // A launch of an excluded game that slipped in is dropped too.
        s.launch("a", T0 + 16_000);
        s.tick(T0 + 20_000, &seen(&["a"]), &excluded(&["a"]));
        assert!(s.is_empty());
        let settings = Settings {
            tracking_excluded_game_ids: vec!["a".into()],
            ..Settings::default()
        };
        let policy = Policy::from_settings(&settings);
        assert!(policy.detect_external);
        assert!(!policy.allows("a", Source::Launch));
        assert!(policy.allows("b", Source::Detected));
        assert!(!Policy::default().allows("b", Source::Detected));
        assert!(Policy::default().allows("b", Source::Launch));
    }

    #[test]
    fn short_sessions_are_dropped() {
        let mut s = Sessions::default();
        s.tick(T0, &seen(&["a"]), &on());
        s.tick(T0 + 5_000, &seen(&["a"]), &on());
        assert!(s.tick(T0 + 20_000, &seen(&[]), &on()).effects.is_empty());
        let t = s.tick(T0 + 40_000, &seen(&[]), &on());
        assert!(t.effects.is_empty() && t.running_changed && !t.library_changed);
        assert!(s.is_empty());
    }

    #[test]
    fn a_returning_process_within_the_grace_period_continues_the_session() {
        let mut s = Sessions::default();
        s.tick(T0, &seen(&["a"]), &on());
        s.tick(T0 + 20_000, &seen(&["a"]), &on());
        s.set_session_id("a", T0, 1);
        s.tick(T0 + 40_000, &seen(&[]), &on());
        let t = s.tick(T0 + 45_000, &seen(&["a"]), &on());
        assert_eq!(saves(&t), [(45, 25, Some(1))]);
        // A gap longer than the grace period (e.g. sleep) splits the session.
        let t = s.tick(T0 + 200_000, &seen(&["a"]), &on());
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 1,
                ended_at: T0 + 45_000,
                seconds: 45
            }]
        );
        assert_eq!(s.snapshot(T0 + 200_000)[0].started_at, T0 + 200_000);
    }

    #[test]
    fn launches_wait_for_the_process_and_time_out() {
        let mut s = Sessions::default();
        s.launch("a", T0);
        let snap = s.snapshot(T0 + 1_000);
        assert_eq!(
            (snap[0].state, snap[0].source, snap[0].exe.clone()),
            (RunState::Launching, Source::Launch, None)
        );
        // External detection is off: other running games are ignored.
        let t = s.tick(T0 + 5_000, &seen(&["b"]), &off());
        assert!(t.running_changed, "the launch itself is announced");
        assert_eq!(s.snapshot(T0).len(), 1);
        let t = s.tick(T0 + 60_000, &seen(&["a"]), &off());
        assert_eq!(
            t.effects,
            [Effect::MarkPlayed {
                game_id: "a".into(),
                at: T0 + 60_000
            }]
        );
        assert_eq!(s.snapshot(T0 + 60_000)[0].state, RunState::Running);
        let t = s.tick(T0 + 70_000, &seen(&["a"]), &off());
        assert!(matches!(
            t.effects[0],
            Effect::Save {
                source: Source::Launch,
                ..
            }
        ));

        let mut s = Sessions::default();
        s.launch("slow", T0);
        assert!(s.tick(T0, &seen(&[]), &off()).running_changed);
        assert!(
            !s.tick(T0 + START_TIMEOUT_MS, &seen(&[]), &off())
                .running_changed
        );
        assert!(!s.is_empty());
        let t = s.tick(T0 + START_TIMEOUT_MS + 1, &seen(&[]), &off());
        assert!(t.running_changed && t.effects.is_empty() && s.is_empty());
    }

    #[test]
    fn stopping_merging_and_resetting() {
        let mut s = Sessions::default();
        s.tick(T0, &seen(&["a", "b"]), &on());
        s.tick(T0 + 10_000, &seen(&["a", "b"]), &on());
        s.set_session_id("a", T0, 1);
        s.set_session_id("b", T0, 2);
        // b merged into a while both run: b's session is closed.
        let t = s.merge("a", "b");
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 2,
                ended_at: T0 + 10_000,
                seconds: 10
            }]
        );
        // a merged into c (not tracked): the run continues as c's.
        assert!(s.merge("c", "a").effects.is_empty());
        let t = s.tick(T0 + 15_000, &seen(&["c"]), &on());
        assert_eq!(saves(&t), [(15, 5, Some(1))]);
        s.reset(T0 + 20_000);
        let t = s.tick(T0 + 30_000, &seen(&["c"]), &on());
        assert_eq!(saves(&t), [(10, 10, None)]);
        s.set_session_id("c", T0 + 20_000, 3);
        let t = s.stop_all();
        assert_eq!(
            t.effects,
            [Effect::Close {
                session_id: 3,
                ended_at: T0 + 30_000,
                seconds: 10
            }]
        );
        assert!(s.is_empty());
        s.launch("d", T0);
        s.forget("d");
        assert!(s.is_empty());
    }

    #[test]
    fn effects_are_written_to_the_database() {
        let db = Db::memory().unwrap();
        let conn = db.lock();
        db::save_game(
            &conn,
            &crate::types::GamePatch::from(&game("a", &[r"D:\A"])),
        )
        .unwrap();
        let mut s = Sessions::default();
        s.tick(T0, &seen(&["a", "ghost"]), &on());
        let t = s.tick(T0 + 12_000, &seen(&["a", "ghost"]), &on());
        apply(&conn, &mut s, t.effects);
        let t = s.tick(T0 + 20_000, &seen(&["a", "ghost"]), &on());
        apply(&conn, &mut s, t.effects);
        let rows = db::play_sessions(&conn, "a", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].seconds, rows[0].ended_at, rows[0].source.as_str()),
            (20, None, "detected")
        );
        let g = db::get_game(&conn, "a").unwrap().unwrap();
        assert_eq!((g.playtime_seconds, g.last_played), (Some(20), Some(T0)));
        // A game missing from the library gets no session row.
        assert!(db::play_sessions(&conn, "ghost", 10).unwrap().is_empty());
        s.tick(T0 + 40_000, &seen(&[]), &on());
        let t = s.tick(T0 + 60_000, &seen(&[]), &on());
        apply(&conn, &mut s, t.effects);
        let rows = db::play_sessions(&conn, "a", 10).unwrap();
        assert_eq!((rows[0].seconds, rows[0].ended_at), (20, Some(T0 + 20_000)));
    }

    #[test]
    fn running_games_serialize_for_the_renderer() {
        let mut s = Sessions::default();
        s.launch("a", T0);
        s.tick(T0, &seen(&["b"]), &on());
        let json = serde_json::to_value(s.snapshot(T0 + 3_000)).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                { "gameId": "a", "state": "launching", "startedAt": T0, "sessionSeconds": 0, "source": "launch" },
                { "gameId": "b", "state": "running", "startedAt": T0, "sessionSeconds": 3, "source": "detected", "exe": r"C:\Games\b\b.exe" }
            ])
        );
    }

    #[test]
    fn rom_routes_and_emulator_folders_are_never_detection_targets() {
        let g = guard();
        let mut rom = game("rom", &[r"D:\Roms\GBA"]);
        rom.routes[0].source_id = "rom".into();
        let mut assigned = game("assigned", &[r"D:\Roms\SNES"]);
        assigned.routes[0].emulator_id = Some("emu".into());
        let targets = Targets::build(
            &[
                rom,
                assigned,
                game("inside-emu-root", &[r"E:\Emulators\Dolphin"]),
                game("emu-folder", &[r"F:\Tools\mGBA"]),
                game("pc", &[r"D:\Games\PC\Celeste"]),
            ],
            &g,
            &[n(r"E:\Emulators"), n(r"F:\Tools\mGBA")],
        );
        for id in ["rom", "assigned", "inside-emu-root", "emu-folder"] {
            assert!(!targets.has_game(id), "{id}");
        }
        assert!(targets.has_game("pc"));
        // A shared emulator running never makes ROM games look running.
        let processes = procs(&[
            r"F:\Tools\mGBA\mGBA.exe",
            r"E:\Emulators\Dolphin\Dolphin.exe",
        ]);
        assert!(targets
            .attribute(&processes, &g, &all_pids(&processes))
            .is_empty());
        // Only the process Game Lobby started (pid and exe both match) counts.
        let watched: HashMap<String, WatchedProcess> = [(
            "rom".to_string(),
            WatchedProcess {
                pid: processes[0].pid,
                exe: n(&processes[0].path),
            },
        )]
        .into();
        let running = watched_running(&watched, &processes);
        assert_eq!(running.keys().collect::<Vec<_>>(), ["rom"]);
        let reused: HashMap<String, WatchedProcess> = [(
            "rom".to_string(),
            WatchedProcess {
                pid: processes[1].pid,
                exe: n(&processes[0].path),
            },
        )]
        .into();
        assert!(
            watched_running(&reused, &processes).is_empty(),
            "pid reused by another exe"
        );
    }
}
