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
//! library, emits `updates:ready` and starts the installer, which closes Game Lobby and starts the
//! new version when it is done.

use crate::db;
use crate::state::AppState;
use crate::tracker;
use crate::types::now_ms;
use rusqlite::{params, Connection, OptionalExtension};
use semver::Version;
use serde::Serialize;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
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
/// Minimum gap between `updates:progress` events.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// Time for the renderer to show "Restarting…" before the installer closes the app.
const READY_DELAY: Duration = Duration::from_millis(500);
const BUSY: &str =
    "Game Lobby is already checking for or installing an update. Wait for it to finish.";

/// Managed state: one check or install at a time, and the update the last check found.
#[derive(Default)]
pub struct Updates {
    busy: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
}

impl Updates {
    fn pending(&self) -> MutexGuard<'_, Option<Update>> {
        self.pending
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
    check(&app, &updates).await.map_err(|f| f.message())
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
            eprintln!("Automatic update check failed: {}", failure.message());
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
/// installer closes Game Lobby (Windows) or the app restarts into the new version.
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
            "Game Lobby {next} is not newer than the installed version {current}, so it was not installed."
        ));
    }

    // The signature is checked as part of the download; nothing changes before it passes.
    let bytes = download(&app, &update)
        .await
        .map_err(|e| describe(&e).message())?;

    tracker::end_all_sessions(&app);
    let backup_app = app.clone();
    let version = update.version.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = backup_app.state::<AppState>();
        let conn = state.db.lock();
        backup_database(&conn, &state.data_dir, &version)
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
            eprintln!("Could not store the update check time: {e:#}");
        }
    }
    let mut found = result.map_err(|e| describe(&e))?;
    if let Some(update) = found.as_mut() {
        // The installer download is limited per read, not in total (slow connections).
        update.timeout = None;
    }
    let info = info(app, found.as_ref());
    *updates.pending() = found;
    Ok(info)
}

async fn download(app: &AppHandle, update: &Update) -> Result<Vec<u8>, UpdaterError> {
    let mut downloaded: u64 = 0;
    let mut total: Option<u64> = None;
    let mut last_emit: Option<Instant> = None;
    let bytes = update
        .download(
            |chunk, content_length| {
                downloaded += chunk as u64;
                total = content_length;
                let now = Instant::now();
                if progress_due(last_emit, now) {
                    last_emit = Some(now);
                    let _ = app.emit("updates:progress", Progress { downloaded, total });
                }
            },
            || {},
        )
        .await?;
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

// ---------------------------------------------------------------------------
// Pure helpers

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
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
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
            Self::NoRelease => "No Game Lobby release has been published yet.".into(),
            Self::BadSignature => {
                "The update was rejected because its signature didn't match. Nothing was installed."
                    .into()
            }
            Self::BadManifest => "The update information from the server could not be read.".into(),
            Self::NoWindowsBuild => "The latest release has no Windows installer.".into(),
            Self::Download => "The update could not be downloaded. Try again later.".into(),
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
        ] {
            assert!(!failure.message().contains("http"), "{failure:?}");
        }
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
        assert!(!endpoints.is_empty());
        for endpoint in endpoints {
            let url = url::Url::parse(endpoint.as_str().unwrap()).unwrap();
            assert_eq!(url.scheme(), "https", "{url}");
            assert_eq!(url.host_str(), Some("github.com"), "{url}");
            assert!(url.path().starts_with("/m72b/GameLobby-releases/"), "{url}");
            assert!(url.query().is_none() && url.username().is_empty(), "{url}");
        }
        // The release workflow publishes to the same repository the app reads from.
        let workflow = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/release.yml"),
        )
        .unwrap();
        let release_repo = workflow
            .lines()
            .find_map(|line| line.trim().strip_prefix("RELEASE_REPO:"))
            .expect("RELEASE_REPO in release.yml")
            .trim();
        assert_eq!(release_repo, "m72b/GameLobby-releases");
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
}
