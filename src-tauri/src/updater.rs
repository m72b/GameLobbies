//! Signed self-updates (tauri-plugin-updater), driven only from these commands: the webviews have
//! no updater or process permissions.
//!
//! The manifest (`latest.json`) is fetched over HTTPS from the public releases repository. It is
//! not signed itself, so everything taken from it is treated as untrusted: the installer must
//! carry a minisign signature from the key in `tauri.conf.json`, the signature must name the
//! announced version (`requireSignedVersion`), the version must be newer than the running one,
//! and the release notes are reduced to short plain text.
//!
//! Flow: `updates_check` (or `updates_auto_check`) keeps the found [`Update`]; `updates_install`
//! downloads exactly that update (progress: `updates:progress`), ends play sessions, backs up the
//! library, emits `updates:ready` and starts the installer, which closes GameLobbies and starts the
//! new version when it is done.

use crate::db;
use crate::state::AppState;
use crate::tracker;
use crate::types::now_ms;
use rusqlite::{params, Connection, OptionalExtension};
use semver::Version;
use serde::Serialize;
use std::cmp::Ordering;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

type CmdResult<T> = Result<T, String>;

/// Whole-request limit for fetching the manifest.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Connection and per-read limits (the download itself has no total limit).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Release notes are cut to this many bytes (UTF-8), including the trailing ellipsis.
pub const NOTES_MAX_BYTES: usize = 4096;
/// Automatic checks run at most this often.
const AUTO_CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;
/// Row in the `settings` table holding the time of the last check that reached the server.
/// Kept out of [`crate::types::Settings`] so the renderer cannot set it.
const LAST_CHECK_KEY: &str = "updaterLastCheckAt";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// Time for the renderer to show "Restarting…" before the installer closes the app.
const READY_DELAY: Duration = Duration::from_millis(500);
const BUSY: &str =
    "GameLobbies is already checking for or installing an update. Wait for it to finish.";
/// Largest installer accepted: the download stops as soon as more than this has arrived.
pub const MAX_DOWNLOAD_BYTES: u64 = 300 * 1024 * 1024;
/// Library backups made before updates; only the newest [`BACKUPS_KEPT`] are kept.
const BACKUP_PREFIX: &str = "library.sqlite.before-update-";
const BACKUPS_KEPT: usize = 3;
/// Minimum gap between manual checks; a quicker repeat gets the previous result.
const MANUAL_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const TOO_SOON: &str = "GameLobbies checked for updates a moment ago. Try again in a few seconds.";

/// The last manual check: when it ran, and its result when it succeeded.
type LastCheck = (Instant, Option<UpdateInfo>);

/// Managed state: one check or install at a time, and the update the last check found.
#[derive(Default)]
pub struct Updates {
    busy: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
    last_manual: Mutex<Option<LastCheck>>,
}

impl Updates {
    fn pending(&self) -> MutexGuard<'_, Option<Update>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn last_manual(&self) -> MutexGuard<'_, Option<LastCheck>> {
        self.last_manual
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Release date, milliseconds since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<i64>,
    /// Plain text (see [`sanitize_notes`]); never HTML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Progress {
    downloaded: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
}

// ---------------------------------------------------------------------------
// Commands

#[tauri::command]
pub async fn updates_check(app: AppHandle, updates: State<'_, Updates>) -> CmdResult<UpdateInfo> {
    let _busy = updates.busy.try_lock().map_err(|_| BUSY.to_string())?;
    let now = Instant::now();
    if let Some(recent) = recent_check(updates.last_manual().as_ref(), now) {
        return recent;
    }
    let result = check(&app, &updates).await.map_err(|f| f.message());
    *updates.last_manual() = Some((now, result.as_ref().ok().cloned()));
    result
}

/// The startup check: runs only when `autoCheckUpdates` is on and the last check that reached
/// the server is a day old. Returns the update when one is available; failures are logged only.
#[tauri::command]
pub async fn updates_auto_check(
    app: AppHandle,
    state: State<'_, AppState>,
    updates: State<'_, Updates>,
) -> CmdResult<Option<UpdateInfo>> {
    let (enabled, last) = {
        let conn = state.db.lock();
        let enabled = db::get_settings(&conn)
            .map(|s| s.auto_check_updates)
            .unwrap_or(true);
        (enabled, last_check(&conn))
    };
    if !should_auto_check(enabled, last, now_ms()) {
        return Ok(None);
    }
    let Ok(_busy) = updates.busy.try_lock() else {
        return Ok(None);
    };
    match check(&app, &updates).await {
        Ok(info) => Ok(info.available.then_some(info)),
        Err(failure) => {
            log::warn!("Automatic update check failed: {}", failure.message());
            Ok(None)
        }
    }
}

/// The update found by the last check in this run, if any (no network request).
#[tauri::command]
pub async fn updates_last(
    app: AppHandle,
    updates: State<'_, Updates>,
) -> CmdResult<Option<UpdateInfo>> {
    Ok(updates
        .pending()
        .as_ref()
        .map(|update| info(&app, Some(update))))
}

/// Downloads and installs the update found by the last check. Never returns on success: the
/// installer closes GameLobbies (Windows) or the app restarts into the new version.
#[tauri::command]
pub async fn updates_install(app: AppHandle, updates: State<'_, Updates>) -> CmdResult<()> {
    let _busy = updates.busy.try_lock().map_err(|_| BUSY.to_string())?;
    let update = updates
        .pending()
        .clone()
        .ok_or("No update is waiting to be installed. Check for updates first.")?;
    let current = app.package_info().version.clone();
    let next = Version::parse(&update.version).map_err(|_| Failure::BadManifest.message())?;
    if !is_newer(&current, &next) {
        return Err(format!(
            "GameLobbies {next} is not newer than the installed version {current}, so it was not installed."
        ));
    }

    // The signature is checked as part of the download; nothing changes before it passes.
    let bytes = download(&app, &update).await.map_err(|f| f.message())?;

    tracker::end_all_sessions(&app);
    let backup_app = app.clone();
    let version = update.version.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = backup_app.state::<AppState>();
        let backup = backup_database(&state.db.lock(), &state.data_dir, &version)?;
        if let Err(e) = prune_backups(&state.data_dir, BACKUPS_KEPT) {
            log::warn!("Could not remove old update backups: {e}");
        }
        anyhow::Ok(backup)
    })
    .await
    .map_err(|e| format!("The library backup failed: {e}"))?
    .map_err(|e| {
        format!("The library could not be backed up, so the update was not installed: {e:#}")
    })?;

    let _ = app.emit("updates:ready", ());
    tokio::time::sleep(READY_DELAY).await;
    // Windows: starts the installer in passive mode and exits; the installer starts the new
    // version when it is done. Other platforms return here with the new version in place.
    // The installer ends this process; Close to tray must not get in its way.
    crate::tray::allow_exit();
    update.install(&bytes).map_err(|e| describe(&e).message())?;
    app.restart()
}

// ---------------------------------------------------------------------------
// Checking and downloading

async fn check(app: &AppHandle, updates: &Updates) -> Result<UpdateInfo, Failure> {
    let updater = app
        .updater_builder()
        .version_comparator(|current, release| is_newer(&current, &release.version))
        .timeout(CHECK_TIMEOUT)
        .configure_client(|client| {
            client
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
        })
        .build()
        .map_err(|e| describe(&e))?;
    let result = updater.check().await;
    let reached_server = match &result {
        Ok(_) => true,
        Err(e) => describe(e).reached_server(),
    };
    if reached_server {
        let state = app.state::<AppState>();
        let recorded = record_check(&state.db.lock(), now_ms());
        if let Err(e) = recorded {
            log::warn!("Could not store the update check time: {e:#}");
        }
    }
    let mut found = result.map_err(|e| describe(&e))?;
    if let Some(update) = found.as_mut() {
        // Only installers published in the configured releases repository are downloaded.
        let allowed = configured_download_prefixes(app)
            .iter()
            .any(|prefix| download_url_allowed(update.download_url.as_str(), prefix));
        if !allowed {
            *updates.pending() = None;
            return Err(Failure::UntrustedLocation);
        }
        // The installer download is limited per read, not in total (slow connections).
        update.timeout = None;
    }
    let info = info(app, found.as_ref());
    *updates.pending() = found;
    Ok(info)
}

async fn download(app: &AppHandle, update: &Update) -> Result<Vec<u8>, Failure> {
    let mut downloaded: u64 = 0;
    let mut total: Option<u64> = None;
    let mut last_emit: Option<Instant> = None;
    let too_large = tokio::sync::Notify::new();
    let transfer = update.download(
        |chunk, content_length| {
            downloaded = downloaded.saturating_add(chunk as u64);
            total = content_length;
            if exceeds_limit(downloaded, total) {
                too_large.notify_one();
                return;
            }
            let now = Instant::now();
            if progress_due(last_emit, now) {
                last_emit = Some(now);
                let _ = app.emit("updates:progress", Progress { downloaded, total });
            }
        },
        || {},
    );
    let bytes = with_size_limit(transfer, &too_large)
        .await?
        .map_err(|e| describe(&e))?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(Failure::TooLarge);
    }
    let _ = app.emit(
        "updates:progress",
        Progress {
            downloaded: bytes.len() as u64,
            total: total.or(Some(bytes.len() as u64)),
        },
    );
    Ok(bytes)
}

fn info(app: &AppHandle, update: Option<&Update>) -> UpdateInfo {
    UpdateInfo {
        available: update.is_some(),
        current_version: app.package_info().version.to_string(),
        version: update.map(|u| u.version.clone()),
        date: update
            .and_then(|u| u.date)
            .map(|d| d.unix_timestamp().saturating_mul(1000)),
        notes: update
            .and_then(|u| u.body.as_deref())
            .and_then(sanitize_notes),
    }
}

/// Runs `transfer` until it finishes or `too_large` is notified; then the transfer is dropped,
/// which aborts the download.
async fn with_size_limit<F: Future>(
    transfer: F,
    too_large: &tokio::sync::Notify,
) -> Result<F::Output, Failure> {
    tokio::select! {
        biased;
        _ = too_large.notified() => Err(Failure::TooLarge),
        output = transfer => Ok(output),
    }
}

/// `/<owner>/<repo>/releases/download/` for each configured update endpoint: the repository
/// and, until no installed copy reads it, its name before the rename ([`legacy_github_repo`]).
fn configured_download_prefixes(app: &AppHandle) -> Vec<String> {
    let config = app.config();
    let endpoints = config
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("endpoints"))
        .and_then(serde_json::Value::as_array);
    download_prefixes(
        endpoints
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str),
    )
}

// ---------------------------------------------------------------------------
// Pure helpers

/// The path prefix of release downloads in the GitHub repository an endpoint such as
/// `https://github.com/<owner>/<repo>/releases/latest/download/latest.json` belongs to.
pub fn download_prefix(endpoint: &str) -> Option<String> {
    let url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let mut segments = url.path_segments()?;
    let owner = segments.next().filter(|s| !s.is_empty())?;
    let repo = segments.next().filter(|s| !s.is_empty())?;
    Some(format!("/{owner}/{repo}/releases/download/"))
}

/// [`download_prefix`] of each endpoint that has one, without duplicates, in order.
pub fn download_prefixes<'a>(endpoints: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    for prefix in endpoints.into_iter().filter_map(download_prefix) {
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    prefixes
}

/// True when an installer URL from the (unsigned) manifest is a release asset of the
/// configured repository: https, exactly github.com, no credentials or port, under `prefix`.
pub fn download_url_allowed(download_url: &str, prefix: &str) -> bool {
    let Ok(url) = url::Url::parse(download_url) else {
        return false;
    };
    let path = url.path();
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && path.len() > prefix.len()
        && path.starts_with(prefix)
        && !path.to_ascii_lowercase().contains("%2e")
        && !path.contains("/../")
}

/// True once a download is (or announces itself as) larger than [`MAX_DOWNLOAD_BYTES`].
pub fn exceeds_limit(downloaded: u64, total: Option<u64>) -> bool {
    downloaded > MAX_DOWNLOAD_BYTES || total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES)
}

/// The previous manual check's outcome when it ran under [`MANUAL_CHECK_INTERVAL`] ago: its
/// result, or a "try again" message when it failed. `None` when a new check may run.
fn recent_check(last: Option<&LastCheck>, now: Instant) -> Option<CmdResult<UpdateInfo>> {
    let (at, info) = last?;
    if now.saturating_duration_since(*at) >= MANUAL_CHECK_INTERVAL {
        return None;
    }
    Some(info.clone().ok_or_else(|| TOO_SOON.to_string()))
}

/// True when `remote` is a newer release than `current`. Same versions and downgrades are
/// refused; build metadata (`+…`) does not make a version newer.
pub fn is_newer(current: &Version, remote: &Version) -> bool {
    remote.cmp_precedence(current) == Ordering::Greater
}

/// True when an automatic check is due.
pub fn should_auto_check(enabled: bool, last_check_at: Option<i64>, now: i64) -> bool {
    enabled
        && match last_check_at {
            None => true,
            // A last check "in the future" means the clock was turned back: check again.
            Some(at) => at > now || now - at >= AUTO_CHECK_INTERVAL_MS,
        }
}

fn progress_due(last: Option<Instant>, now: Instant) -> bool {
    match last {
        None => true,
        Some(at) => now.saturating_duration_since(at) >= PROGRESS_INTERVAL,
    }
}

/// Release notes as plain text: line breaks normalised, control and bidirectional-override
/// characters removed (tabs and newlines kept), trimmed and cut to [`NOTES_MAX_BYTES`].
/// `None` when nothing is left.
pub fn sanitize_notes(raw: &str) -> Option<String> {
    let text: String = raw
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|&c| c == '\n' || c == '\t' || !(c.is_control() || is_bidi_control(c)))
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= NOTES_MAX_BYTES {
        return Some(text.to_string());
    }
    const ELLIPSIS: char = '…';
    let mut end = NOTES_MAX_BYTES - ELLIPSIS.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = text[..end].trim_end().to_string();
    cut.push(ELLIPSIS);
    Some(cut)
}

fn is_bidi_control(c: char) -> bool {
    crate::validation::is_bidi_or_hidden(c)
}

/// `library.sqlite.before-update-<version>.bak`, with the version reduced to characters that
/// are safe in a file name (it comes from the unsigned manifest).
pub fn backup_file_name(version: &str) -> String {
    let safe: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .take(64)
        .collect();
    let safe = safe.trim_matches('.');
    let safe = if safe.is_empty() { "unknown" } else { safe };
    format!("library.sqlite.before-update-{safe}.bak")
}

/// Copies the open library into `data_dir` with SQLite's online backup API (consistent even
/// with WAL), writing to a temporary name first so a failed copy never leaves a partial `.bak`.
fn backup_database(conn: &Connection, data_dir: &Path, version: &str) -> anyhow::Result<PathBuf> {
    let target = data_dir.join(backup_file_name(version));
    let partial = data_dir.join(format!("{}.partial", backup_file_name(version)));
    let _ = std::fs::remove_file(&partial);
    let result = conn
        .backup(rusqlite::DatabaseName::Main, &partial, None)
        .map_err(anyhow::Error::from)
        .and_then(|()| std::fs::rename(&partial, &target).map_err(anyhow::Error::from));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result.map(|()| target)
}

/// Deletes all but the newest `keep` `library.sqlite.before-update-*.bak` files in `dir` (by
/// modification time). Other files are never touched. Returns the deleted paths.
fn prune_backups(dir: &Path, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut backups: Vec<(SystemTime, String, PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !(name.starts_with(BACKUP_PREFIX) && name.ends_with(".bak")) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, name, entry.path()))
        })
        .collect();
    backups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut removed = Vec::new();
    for (_, _, path) in backups.into_iter().skip(keep) {
        std::fs::remove_file(&path)?;
        removed.push(path);
    }
    Ok(removed)
}

fn last_check(conn: &Connection) -> Option<i64> {
    conn.query_row(
        "SELECT value FROM settings WHERE key=?",
        [LAST_CHECK_KEY],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|value| value.parse().ok())
}

fn record_check(conn: &Connection, at: i64) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![LAST_CHECK_KEY, at.to_string()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Errors

/// What went wrong, in terms the user can act on. Messages never include URLs.
#[derive(Debug, Clone, PartialEq)]
enum Failure {
    Offline,
    TimedOut,
    NoRelease,
    BadSignature,
    BadManifest,
    NoWindowsBuild,
    Download,
    UntrustedLocation,
    TooLarge,
    Other(String),
}

impl Failure {
    fn message(&self) -> String {
        match self {
            Self::Offline => {
                "Could not reach the update server. Check your internet connection and try again."
                    .into()
            }
            Self::TimedOut => "The update server took too long to respond. Try again later.".into(),
            Self::NoRelease => "No GameLobbies release has been published yet.".into(),
            Self::BadSignature => {
                "The update was rejected because its signature didn't match. Nothing was installed."
                    .into()
            }
            Self::BadManifest => "The update information from the server could not be read.".into(),
            Self::NoWindowsBuild => "The latest release has no Windows installer.".into(),
            Self::Download => "The update could not be downloaded. Try again later.".into(),
            Self::UntrustedLocation => "The update was rejected because it would be downloaded from an unexpected location. Nothing was installed.".into(),
            Self::TooLarge => format!(
                "The update was rejected because the download was larger than {} MB. Nothing was installed.",
                MAX_DOWNLOAD_BYTES / (1024 * 1024)
            ),
            Self::Other(message) => format!("The update failed: {message}"),
        }
    }

    /// False when the server was never reached (the next automatic check should retry).
    fn reached_server(&self) -> bool {
        !matches!(self, Self::Offline | Self::TimedOut)
    }
}

fn describe(error: &UpdaterError) -> Failure {
    match error {
        // reqwest errors print the request URL; they are never shown as they are.
        UpdaterError::Reqwest(e) if e.is_timeout() => Failure::TimedOut,
        UpdaterError::Reqwest(e) if e.is_decode() => Failure::BadManifest,
        UpdaterError::Reqwest(_) => Failure::Offline,
        UpdaterError::ReleaseNotFound => Failure::NoRelease,
        UpdaterError::Minisign(_)
        | UpdaterError::Base64(_)
        | UpdaterError::SignatureUtf8(_)
        | UpdaterError::SignedVersionMismatch { .. }
        | UpdaterError::MissingSignedVersion => Failure::BadSignature,
        UpdaterError::Serialization(_) | UpdaterError::Semver(_) | UpdaterError::UrlParse(_) => {
            Failure::BadManifest
        }
        UpdaterError::TargetNotFound(_) | UpdaterError::TargetsNotFound(_) => {
            Failure::NoWindowsBuild
        }
        UpdaterError::Network(_) => Failure::Download,
        UpdaterError::Io(e) => Failure::Other(format!("the installer could not be started ({e})")),
        other => Failure::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn only_newer_versions_are_updates() {
        assert!(is_newer(&v("0.3.0"), &v("0.3.1")));
        assert!(is_newer(&v("0.3.0"), &v("1.0.0")));
        assert!(is_newer(&v("0.3.0-beta.1"), &v("0.3.0")));
        assert!(!is_newer(&v("0.3.0"), &v("0.3.0")), "same version");
        assert!(!is_newer(&v("0.3.0"), &v("0.2.9")), "downgrade");
        assert!(
            !is_newer(&v("0.3.0"), &v("0.3.0-rc.1")),
            "pre-release of the same version"
        );
        assert!(
            !is_newer(&v("0.3.0"), &v("0.3.0+build.7")),
            "build metadata only"
        );
        assert!(!is_newer(&v("1.0.0"), &v("0.99.99")));
    }

    #[test]
    fn notes_are_plain_trimmed_text() {
        assert_eq!(sanitize_notes(""), None);
        assert_eq!(sanitize_notes(" \r\n\t "), None);
        assert_eq!(
            sanitize_notes("  Fixes\r\n- one\r- two\t(tab)\n\n").as_deref(),
            Some("Fixes\n- one\n- two\t(tab)")
        );
        assert_eq!(
            sanitize_notes("a\u{0}b\u{7}c\u{1b}[31md\u{7f}e\u{85}f").as_deref(),
            Some("abc[31mdef")
        );
        assert_eq!(
            sanitize_notes("safe\u{202E}txt.exe\u{2066}x\u{200F}").as_deref(),
            Some("safetxt.exex")
        );
        // Markup is kept as literal text; the renderer never interprets it.
        assert_eq!(
            sanitize_notes("<b>bold</b>").as_deref(),
            Some("<b>bold</b>")
        );
    }

    #[test]
    fn long_notes_are_cut_on_a_character_boundary() {
        let long = "é".repeat(5000);
        let notes = sanitize_notes(&long).unwrap();
        assert!(notes.len() <= NOTES_MAX_BYTES, "{}", notes.len());
        assert!(notes.ends_with('…'));
        assert!(notes.trim_end_matches('…').chars().all(|c| c == 'é'));

        let exact = "x".repeat(NOTES_MAX_BYTES);
        assert_eq!(sanitize_notes(&exact).unwrap(), exact);
        let over = "x".repeat(NOTES_MAX_BYTES + 1);
        assert_eq!(sanitize_notes(&over).unwrap().len(), NOTES_MAX_BYTES);
    }

    #[test]
    fn backup_names_are_safe_file_names() {
        assert_eq!(
            backup_file_name("0.4.0"),
            "library.sqlite.before-update-0.4.0.bak"
        );
        assert_eq!(
            backup_file_name("1.2.3-rc.1+build.5"),
            "library.sqlite.before-update-1.2.3-rc.1+build.5.bak"
        );
        for hostile in [
            "../../evil",
            r"..\..\evil",
            "C:\\x",
            "a/b",
            "",
            "..",
            "<>:\"|?*",
        ] {
            let name = backup_file_name(hostile);
            assert!(name.starts_with("library.sqlite.before-update-"), "{name}");
            assert!(name.ends_with(".bak"), "{name}");
            assert!(
                !name.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*']),
                "{name}"
            );
            assert!(!name.contains("-.") && !name.contains(".."), "{name}");
        }
        assert_eq!(
            backup_file_name(""),
            "library.sqlite.before-update-unknown.bak"
        );
        assert!(backup_file_name(&"9".repeat(500)).len() < 120);
    }

    #[test]
    fn backup_copies_the_open_library_with_the_online_api() {
        let dir = std::env::temp_dir().join(format!("game-lobby-updater-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("library.sqlite");
        let db = crate::db::Db::open(&source).unwrap();
        db::put_settings(
            &db.lock(),
            serde_json::from_value(serde_json::json!({ "posterWidth": 200 })).unwrap(),
        )
        .unwrap();

        let path = backup_database(&db.lock(), &dir, "0.4.0").unwrap();
        assert_eq!(path, dir.join("library.sqlite.before-update-0.4.0.bak"));
        assert!(!dir
            .join("library.sqlite.before-update-0.4.0.bak.partial")
            .exists());
        let copy = Connection::open(&path).unwrap();
        assert_eq!(db::get_settings(&copy).unwrap().poster_width, 200);
        drop(copy);
        // A second backup of the same version replaces the first.
        backup_database(&db.lock(), &dir, "0.4.0").unwrap();
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn automatic_checks_run_at_most_daily() {
        let day = AUTO_CHECK_INTERVAL_MS;
        let now = 100 * day;
        assert!(should_auto_check(true, None, now));
        assert!(!should_auto_check(false, None, now));
        assert!(!should_auto_check(true, Some(now - day + 1), now));
        assert!(should_auto_check(true, Some(now - day), now));
        assert!(
            should_auto_check(true, Some(now + 60_000), now),
            "clock turned back"
        );
        assert!(!should_auto_check(false, Some(now - 2 * day), now));
    }

    #[test]
    fn last_check_time_round_trips() {
        let db = crate::db::Db::memory().unwrap();
        let conn = db.lock();
        assert_eq!(last_check(&conn), None);
        record_check(&conn, 1_700_000_000_000).unwrap();
        record_check(&conn, 1_700_000_000_123).unwrap();
        assert_eq!(last_check(&conn), Some(1_700_000_000_123));
        // The settings themselves are untouched and still load.
        assert!(db::get_settings(&conn).unwrap().auto_check_updates);
    }

    #[test]
    fn download_prefix_comes_from_a_github_endpoint() {
        let endpoint = "https://github.com/m72b/GameLobby/releases/latest/download/latest.json";
        assert_eq!(
            download_prefix(endpoint).as_deref(),
            Some("/m72b/GameLobby/releases/download/")
        );
        assert_eq!(
            download_prefix(
                "https://github.com/m72b/GameLobby/releases/latest/download/latest.json"
            )
            .as_deref(),
            Some("/m72b/GameLobby/releases/download/")
        );
        for bad in [
            "http://github.com/m72b/GameLobby/releases/latest/download/latest.json",
            "https://example.com/m72b/GameLobby/latest.json",
            "https://github.com.evil.example/m72b/GameLobby/latest.json",
            "https://user@github.com/m72b/GameLobby/latest.json",
            "https://github.com:444/m72b/GameLobby/latest.json",
            "https://github.com/m72b",
            "not a url",
        ] {
            assert_eq!(download_prefix(bad), None, "{bad}");
        }
    }

    #[test]
    fn installers_are_downloaded_only_from_the_releases_repo() {
        let prefix = "/m72b/GameLobby/releases/download/";
        assert!(download_url_allowed(
            "https://github.com/m72b/GameLobby/releases/download/v0.4.0/GameLobbies_0.4.0_x64-setup.exe",
            prefix
        ));
        for bad in [
            "http://github.com/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "https://evil.example/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "https://github.com.evil.example/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "https://objects.githubusercontent.com/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "https://github.com/m72b/Other/releases/download/v0.4.0/a.exe",
            "https://github.com/someone/GameLobby-releases/releases/download/v0.4.0/a.exe",
            "https://github.com/m72b/GameLobby/archive/v0.4.0.zip",
            "https://github.com/m72b/GameLobby/releases/download/",
            "https://github.com/m72b/GameLobby/releases/download/%2e%2e/%2E%2E/x/a.exe",
            "https://user:pw@github.com/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "https://github.com:8443/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "file:///C:/m72b/GameLobby/releases/download/v0.4.0/a.exe",
            "",
        ] {
            assert!(!download_url_allowed(bad, prefix), "{bad}");
        }
        // Dot segments are resolved before the check: this one leaves the repository.
        assert!(!download_url_allowed(
            "https://github.com/m72b/GameLobby/releases/download/../../../Other/releases/download/v1/a.exe",
            prefix
        ));
    }

    #[test]
    fn downloads_over_the_size_limit_are_refused() {
        assert!(!exceeds_limit(0, None));
        assert!(!exceeds_limit(MAX_DOWNLOAD_BYTES, Some(MAX_DOWNLOAD_BYTES)));
        assert!(exceeds_limit(MAX_DOWNLOAD_BYTES + 1, None));
        assert!(
            exceeds_limit(10, Some(MAX_DOWNLOAD_BYTES + 1)),
            "announced size"
        );
        assert_eq!(MAX_DOWNLOAD_BYTES, 300 * 1024 * 1024);
    }

    #[tokio::test]
    async fn the_size_limit_aborts_a_running_download() {
        // A transfer that would never finish is dropped once the limit is signalled.
        let notify = tokio::sync::Notify::new();
        notify.notify_one();
        let result = with_size_limit(std::future::pending::<()>(), &notify).await;
        assert_eq!(result, Err(Failure::TooLarge));

        // Signalled from inside the transfer (as the progress callback does).
        let notify = tokio::sync::Notify::new();
        let transfer = async {
            notify.notify_one();
            std::future::pending::<u8>().await
        };
        assert_eq!(
            with_size_limit(transfer, &notify).await,
            Err(Failure::TooLarge)
        );

        let notify = tokio::sync::Notify::new();
        assert_eq!(with_size_limit(async { 7 }, &notify).await, Ok(7));
    }

    #[test]
    fn manual_checks_are_at_most_every_ten_seconds() {
        let start = Instant::now();
        assert!(recent_check(None, start).is_none());
        let found = UpdateInfo {
            available: true,
            current_version: "0.3.0".into(),
            version: Some("0.4.0".into()),
            date: None,
            notes: None,
        };
        let last = (start, Some(found.clone()));
        assert_eq!(
            recent_check(Some(&last), start + Duration::from_secs(3)),
            Some(Ok(found))
        );
        assert!(recent_check(Some(&last), start + MANUAL_CHECK_INTERVAL).is_none());
        let failed = (start, None);
        assert_eq!(
            recent_check(Some(&failed), start + Duration::from_secs(1)),
            Some(Err(TOO_SOON.to_string()))
        );
        assert!(recent_check(Some(&failed), start + Duration::from_secs(11)).is_none());
        assert_eq!(MANUAL_CHECK_INTERVAL, Duration::from_secs(10));
    }

    #[test]
    fn only_the_newest_three_update_backups_are_kept() {
        let dir = std::env::temp_dir().join(format!("game-lobby-prune-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let make = |name: &str, age_days: u64| {
            let path = dir.join(name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_modified(base - Duration::from_secs(age_days * 86_400))
                .unwrap();
            path
        };
        let newest = [
            make("library.sqlite.before-update-0.9.0.bak", 0),
            make("library.sqlite.before-update-0.8.0.bak", 1),
            make("library.sqlite.before-update-0.7.0.bak", 2),
        ];
        let older = [
            make("library.sqlite.before-update-0.6.0.bak", 3),
            make("library.sqlite.before-update-0.5.0.bak", 4),
        ];
        // Never touched, however old.
        let others = [
            make("library.sqlite", 30),
            make("library.sqlite.pre-title-migration-1.bak", 30),
            make("library.sqlite.before-update-0.1.0.bak.partial", 30),
            make("xlibrary.sqlite.before-update-0.1.0.bak", 30),
            make("library.sqlite.before-update-0.1.0.bak.txt", 30),
        ];
        std::fs::create_dir(dir.join("library.sqlite.before-update-dir.bak")).unwrap();

        let mut removed = prune_backups(&dir, BACKUPS_KEPT).unwrap();
        removed.sort();
        let mut expected = older.to_vec();
        expected.sort();
        assert_eq!(removed, expected);
        for path in newest.iter().chain(others.iter()) {
            assert!(path.exists(), "{}", path.display());
        }
        assert!(dir.join("library.sqlite.before-update-dir.bak").is_dir());
        // Nothing more to remove.
        assert!(prune_backups(&dir, BACKUPS_KEPT).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_events_are_throttled() {
        let start = Instant::now();
        assert!(progress_due(None, start));
        assert!(!progress_due(
            Some(start),
            start + Duration::from_millis(10)
        ));
        assert!(progress_due(Some(start), start + PROGRESS_INTERVAL));
    }

    #[test]
    fn errors_are_friendly_and_signature_failures_are_explicit() {
        for error in [
            UpdaterError::MissingSignedVersion,
            UpdaterError::SignedVersionMismatch {
                signed: "0.3.0".into(),
                announced: "9.9.9".into(),
            },
            UpdaterError::SignatureUtf8("x".into()),
        ] {
            let message = describe(&error).message();
            assert!(
                message.contains("rejected because its signature didn't match"),
                "{message}"
            );
        }
        assert_eq!(describe(&UpdaterError::ReleaseNotFound), Failure::NoRelease);
        assert_eq!(
            describe(&UpdaterError::Network(
                "Download request failed with status: 404".into()
            )),
            Failure::Download
        );
        assert_eq!(
            describe(&UpdaterError::TargetNotFound("windows-x86_64".into())),
            Failure::NoWindowsBuild
        );
        assert!(Failure::NoRelease.reached_server());
        assert!(!Failure::Offline.reached_server());
        assert!(!Failure::TimedOut.reached_server());
        for failure in [
            Failure::Offline,
            Failure::TimedOut,
            Failure::NoRelease,
            Failure::BadSignature,
            Failure::BadManifest,
            Failure::NoWindowsBuild,
            Failure::Download,
            Failure::UntrustedLocation,
            Failure::TooLarge,
        ] {
            assert!(!failure.message().contains("http"), "{failure:?}");
        }
        assert!(Failure::UntrustedLocation
            .message()
            .contains("rejected because it would be downloaded from an unexpected location"));
        assert!(Failure::TooLarge.message().contains("larger than 300 MB"));
    }

    #[test]
    fn update_info_serializes_for_the_renderer() {
        let info = UpdateInfo {
            available: false,
            current_version: "0.3.0".into(),
            version: None,
            date: None,
            notes: None,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({ "available": false, "currentVersion": "0.3.0" })
        );
        let info = UpdateInfo {
            available: true,
            version: Some("0.4.0".into()),
            date: Some(1),
            notes: Some("n".into()),
            ..info
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["version"], "0.4.0");
        assert_eq!(value["date"], 1);
        assert_eq!(value["notes"], "n");
    }

    // -----------------------------------------------------------------------
    // Configuration

    fn json_file(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn updater_config_points_at_the_public_releases_repo() {
        let config = json_file("tauri.conf.json");
        let updater = &config["plugins"]["updater"];
        let endpoints = updater["endpoints"].as_array().expect("endpoints");
        // The repository first, then its name before the rename (which GitHub redirects): the
        // updater tries them in order, and installers are accepted from either one's releases.
        let endpoints: Vec<&str> = endpoints.iter().map(|e| e.as_str().unwrap()).collect();
        assert_eq!(
            endpoints,
            [
                concat!(
                    "https://github.com/",
                    github_repo!(),
                    "/releases/latest/download/latest.json"
                ),
                concat!(
                    "https://github.com/",
                    legacy_github_repo!(),
                    "/releases/latest/download/latest.json"
                ),
            ]
        );
        for endpoint in &endpoints {
            let url = url::Url::parse(endpoint).unwrap();
            assert_eq!(url.scheme(), "https", "{url}");
            assert_eq!(url.host_str(), Some("github.com"), "{url}");
            assert!(url.query().is_none() && url.username().is_empty(), "{url}");
        }
        assert_eq!(
            download_prefixes(endpoints.iter().copied()),
            [
                concat!("/", github_repo!(), "/releases/download/"),
                concat!("/", legacy_github_repo!(), "/releases/download/"),
            ]
        );
        // The release workflow publishes to the repository it runs in, the one the app reads
        // from, and writes installer URLs under the old name for installed 0.5.0–0.9.0 copies.
        let workflow = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release.yml"),
        )
        .unwrap();
        let env_value = |name: &str| {
            workflow
                .lines()
                .find_map(|line| line.trim().strip_prefix(name))
                .unwrap_or_else(|| panic!("{name} in release.yml"))
                .trim()
                .to_string()
        };
        assert_eq!(env_value("RELEASE_REPO:"), github_repo!());
        assert_eq!(env_value("DOWNLOAD_REPO:"), legacy_github_repo!());
        // The public key decodes to a minisign public key.
        let pubkey = updater["pubkey"].as_str().expect("pubkey");
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(pubkey)
            .expect("pubkey is base64");
        let text = String::from_utf8(decoded).unwrap();
        assert!(
            text.starts_with("untrusted comment: minisign public key"),
            "{text}"
        );
        assert!(
            !text.to_lowercase().contains("secret"),
            "a private key was configured"
        );
        assert_eq!(updater["windows"]["installMode"], "passive");
        assert_eq!(updater["requireSignedVersion"], true);
        assert_ne!(updater["allowDowngrades"], true);
        assert_ne!(updater["dangerousInsecureTransportProtocol"], true);
        assert_ne!(updater["dangerousAcceptInvalidCerts"], true);
        assert_ne!(updater["dangerousAcceptInvalidHostnames"], true);
        // Local builds must not need the signing key.
        assert!(
            config["bundle"].get("createUpdaterArtifacts").is_none(),
            "createUpdaterArtifacts belongs in tauri.release.conf.json only"
        );
        let release = json_file("tauri.release.conf.json");
        assert_eq!(release["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn webviews_have_no_updater_or_process_permissions() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            files += 1;
            let config: Value = serde_json::from_str(&text).unwrap();
            for permission in config["permissions"].as_array().into_iter().flatten() {
                let id = match permission {
                    Value::String(id) => id.as_str(),
                    other => other["identifier"].as_str().unwrap_or_default(),
                };
                assert!(
                    !id.starts_with("updater:") && !id.starts_with("process:"),
                    "{} grants {id}",
                    path.display()
                );
            }
            assert!(!text.contains("updater:") && !text.contains("process:"));
        }
        assert!(files > 0);
    }

    /// The rename from "Game Lobby" keeps everything that app data, the WebView2 profile, the
    /// single-instance lock and jump lists hang on (the identifier), and the installer hook
    /// moves an old install over without touching app data.
    #[test]
    fn the_renamed_installer_keeps_the_identifier_and_moves_the_old_install() {
        let config = json_file("tauri.conf.json");
        assert_eq!(config["productName"], "GameLobbies");
        assert_eq!(config["mainBinaryName"], "GameLobbies");
        assert_eq!(config["identifier"], "app.gamelobby.desktop");
        let nsis = &config["bundle"]["windows"]["nsis"];
        assert_eq!(nsis["installMode"], "currentUser");
        assert_eq!(nsis["installerHooks"], "installer/hooks.nsh");

        let hooks = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("installer/hooks.nsh"),
        )
        .unwrap();
        // Only NSIS code counts, not the comments explaining it.
        let code: String = hooks
            .lines()
            .filter(|l| !l.trim_start().starts_with(';'))
            .collect::<Vec<_>>()
            .join("\n");
        let lower = code.to_lowercase();
        assert!(code.contains("!macro NSIS_HOOK_PREINSTALL"));
        assert!(code.contains("!macro NSIS_HOOK_POSTINSTALL"));
        // The old install as the 0.9.0 installer wrote it (productName "Game Lobby", publisher
        // from the identifier), per user.
        assert!(code.contains(r#"!define GL_OLD_PRODUCTNAME "Game Lobby""#));
        assert!(code.contains(r#"!define GL_OLD_PUBLISHER "gamelobby""#));
        assert!(code.contains(
            r#"!define GL_OLD_UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Game Lobby""#
        ));
        assert!(!lower.contains("hklm") && !lower.contains("shctx"));
        // The old uninstaller runs silently in update mode, in place, so it keeps app data.
        assert!(
            code.contains(r#"ExecWait '"$GlOldDir\uninstall.exe" /S /UPDATE _?=$GlOldDir' $R3"#)
        );
        // Never a recursive delete, and never the app data folders.
        assert!(!lower.contains("rmdir /r"), "recursive delete in hooks.nsh");
        assert!(!lower.contains("$appdata") && !lower.contains("bundleid"));
        assert!(!lower.contains("$localappdata"));
        // Old shortcuts are removed only when they start the old app.
        assert_eq!(code.matches("!insertmacro IsShortcutTarget").count(), 2);
        assert_eq!(code.matches("Delete \"$SMPROGRAMS\\").count(), 1);
        assert_eq!(code.matches("Delete \"$DESKTOP\\").count(), 1);
        // The Run value the hook moves is the one the app used to write, in the app's format.
        assert_eq!(crate::autostart::OLD_VALUE_NAME, "Game Lobby");
        assert!(code.contains(&format!(
            r#"'"$INSTDIR\${{MAINBINARYNAME}}.exe" {}'"#,
            crate::autostart::FLAG
        )));
    }

    /// The installer URL the release workflow writes (updater-manifest.mjs with DOWNLOAD_REPO)
    /// must pass both the check of installed 0.5.0–0.9.0 copies, whose only prefix comes from
    /// their single endpoint under the old name, and this build's.
    #[test]
    fn manifest_urls_under_the_old_name_pass_old_and_new_clients() {
        let url = concat!(
            "https://github.com/",
            legacy_github_repo!(),
            "/releases/download/v1.0.0/GameLobbies_1.0.0_x64-setup.exe"
        );
        let old_prefix = download_prefix(
            "https://github.com/m72b/GameLobby/releases/latest/download/latest.json",
        )
        .unwrap();
        assert!(download_url_allowed(url, &old_prefix));
        let config = json_file("tauri.conf.json");
        let endpoints = config["plugins"]["updater"]["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap());
        let prefixes = download_prefixes(endpoints);
        assert!(prefixes.iter().any(|p| download_url_allowed(url, p)));
        // A URL under the new name would be refused by the old copies (exact path prefix), so
        // the manifest must not use it while they are around.
        let renamed = concat!(
            "https://github.com/",
            github_repo!(),
            "/releases/download/v1.0.0/GameLobbies_1.0.0_x64-setup.exe"
        );
        assert!(!download_url_allowed(renamed, &old_prefix));
        assert!(prefixes.iter().any(|p| download_url_allowed(renamed, p)));
        // The old name is not a prefix of other repository names.
        let lookalike = "https://github.com/m72b/GameLobbyEvil/releases/download/v1/a.exe";
        assert!(!download_url_allowed(lookalike, &old_prefix));
        assert!(!prefixes.iter().any(|p| download_url_allowed(lookalike, p)));
        // Duplicates and endpoints that are not GitHub release paths add nothing.
        assert_eq!(
            download_prefixes([
                "https://github.com/a/b/releases/latest/download/latest.json",
                "https://example.com/a/b/latest.json",
                "https://github.com/a/b/releases/latest/download/other.json",
            ]),
            ["/a/b/releases/download/"]
        );
    }
}
