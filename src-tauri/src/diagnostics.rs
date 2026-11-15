//! Support diagnostics: a plain-text report the user can copy into a bug report, and the
//! "open the logs folder" action.
//!
//! The report never contains a credential: key and token presence is a yes/no, the database is
//! only counted, and the whole text (log lines included) goes through [`crate::redact`] before
//! it leaves Rust. The renderer supplies no paths; the logs folder is fixed.

use crate::state::AppState;
use crate::{applog, redact, secrets};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use tauri::{AppHandle, State};

/// How many trailing log lines the report carries.
pub const LOG_LINES: usize = 200;

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

/// Facts that do not come from the library database.
pub struct Environment {
    pub app_version: String,
    pub windows: String,
    pub arch: String,
    pub webview: Option<String>,
    pub data_dir: String,
    pub update_endpoints: Vec<String>,
}

/// "Windows 11 Pro 24H2 (build 26200.5000)" from the registry; `None` off Windows.
#[cfg(windows)]
pub fn windows_version() -> Option<String> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .ok()?;
    let text = |name: &str| key.get_value::<String, _>(name).ok();
    let build: u32 = text("CurrentBuildNumber")?.trim().parse().ok()?;
    let revision = key.get_value::<u32, _>("UBR").ok();
    // `ProductName` still says "Windows 10" on Windows 11.
    let mut name = text("ProductName").unwrap_or_else(|| "Windows".into());
    if build >= 22_000 {
        name = name.replacen("Windows 10", "Windows 11", 1);
    }
    let display = text("DisplayVersion")
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let build = match revision {
        Some(r) => format!("{build}.{r}"),
        None => build.to_string(),
    };
    Some(format!("{name}{display} (build {build})"))
}

#[cfg(not(windows))]
pub fn windows_version() -> Option<String> {
    None
}

/// The configured update endpoints (`plugins.updater.endpoints` in `tauri.conf.json`).
fn update_endpoints(app: &AppHandle) -> Vec<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("endpoints"))
        .and_then(|e| e.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn environment(app: &AppHandle, state: &AppState) -> Environment {
    Environment {
        app_version: app.package_info().version.to_string(),
        windows: windows_version().unwrap_or_else(|| std::env::consts::OS.to_string()),
        arch: std::env::consts::ARCH.to_string(),
        webview: tauri::webview_version().ok(),
        data_dir: state.data_dir.display().to_string(),
        update_endpoints: update_endpoints(app),
    }
}

/// Games per store, sorted by store id.
fn store_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT store, COUNT(*) FROM games GROUP BY store ORDER BY store")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// Builds the report. Everything is redacted with `profile` (the user's profile folder) as
/// the folder to hide.
pub fn render(
    env: &Environment,
    conn: &Connection,
    log_lines: &[String],
    profile: Option<&str>,
) -> Result<String> {
    let games: i64 = conn.query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))?;
    let stores = store_counts(conn)?;
    let flags = secrets::stored_flags(conn)?;
    let library = Path::new(&env.data_dir).join("library.sqlite");
    let wal = Path::new(&env.data_dir).join("library.sqlite-wal");
    let size = match file_size(&library) {
        Some(bytes) => {
            let wal = file_size(&wal).filter(|b| *b > 0);
            match wal {
                Some(w) => format!(
                    "{} (+ {} write-ahead log)",
                    format_bytes(bytes),
                    format_bytes(w)
                ),
                None => format_bytes(bytes),
            }
        }
        None => "unknown".into(),
    };
    let endpoint = if env.update_endpoints.is_empty() {
        "none configured".to_string()
    } else {
        env.update_endpoints.join(", ")
    };
    let channel = if env
        .update_endpoints
        .iter()
        .any(|e| e.contains("/releases/latest/"))
    {
        "stable (latest GitHub release)"
    } else {
        "custom"
    };
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line("Game Lobby diagnostics".into());
    line(String::new());
    line(format!("App version: {}", env.app_version));
    line(format!("Windows: {}", env.windows));
    line(format!("Architecture: {}", env.arch));
    line(format!(
        "WebView2: {}",
        env.webview.as_deref().unwrap_or("unknown")
    ));
    line(format!("Data folder: {}", env.data_dir));
    line(format!("Library size: {size}"));
    line(format!("Games: {games}"));
    line(format!(
        "Stores: {}",
        if stores.is_empty() {
            "none".to_string()
        } else {
            stores
                .iter()
                .map(|(store, n)| format!("{store} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    line(format!("Update channel: {channel}"));
    line(format!("Update endpoint: {endpoint}"));
    line(format!(
        "SteamGridDB API key set: {}",
        yes_no(flags.steamgriddb_key)
    ));
    line(format!(
        "IGDB credentials set: {} (Client ID set: {}; Client Secret set: {})",
        yes_no(flags.igdb_client_id && flags.igdb_client_secret),
        yes_no(flags.igdb_client_id),
        yes_no(flags.igdb_client_secret)
    ));
    line(format!(
        "Steam owned games import: {} (API key set: {}; SteamID set: {})",
        yes_no(flags.steam_owned_key && flags.steam_owned_steam_id),
        yes_no(flags.steam_owned_key),
        yes_no(flags.steam_owned_steam_id)
    ));
    line(String::new());
    line(format!("--- Last {} log lines ---", log_lines.len()));
    for l in log_lines {
        line(l.clone());
    }
    Ok(redact::redact_with(&out, profile))
}

/// The report for the running app.
fn build(app: &AppHandle, state: &AppState) -> Result<String> {
    let env = environment(app, state);
    let lines = applog::tail_lines(&state.data_dir, LOG_LINES);
    let conn = state.db.lock();
    render(
        &env,
        &conn,
        &lines,
        std::env::var("USERPROFILE").ok().as_deref(),
    )
}

#[tauri::command]
pub async fn diagnostics_report(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    build(&app, &state).map_err(err)
}

/// Opens the log folder (always `<data folder>/logs`; no path comes from the renderer).
#[tauri::command]
pub async fn diagnostics_open_logs(state: State<'_, AppState>) -> CmdResult<()> {
    let dir = applog::log_dir(&state.data_dir);
    std::fs::create_dir_all(&dir).map_err(err)?;
    crate::platform::shell_open(dir.to_string_lossy().into_owned())
        .await
        .map_err(err)
}

/// Writes the startup facts to the log.
pub fn log_startup(app: &tauri::App, data_dir: &Path) {
    log::info!(
        "Game Lobby {} starting (Windows: {}, {}, WebView2: {})",
        app.package_info().version,
        windows_version().unwrap_or_else(|| std::env::consts::OS.into()),
        std::env::consts::ARCH,
        tauri::webview_version().unwrap_or_else(|_| "unknown".into())
    );
    log::info!("Data folder: {}", data_dir.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const FAKE_KEY: &str = "0123456789abcdef0123456789abcdef";
    const FAKE_ID: &str = "tw1tchc1ientid0123456789abcdef";
    const FAKE_SECRET: &str = "tw1tchc1ientsecret9876543210zyx";
    const FAKE_BLOB: &[u8] = b"DPAPI-BLOB-0123456789-abcdefghijklmnopqrstuvwxyz";
    const FAKE_STEAM_KEY: &str = "fedcba9876543210fedcba9876543210";
    const FAKE_STEAM_ID: &str = "76561197960287930";

    fn environment() -> Environment {
        Environment {
            app_version: "0.3.0".into(),
            windows: "Windows 11 Pro 24H2 (build 26200.5000)".into(),
            arch: "x86_64".into(),
            webview: Some("130.0.2849.80".into()),
            data_dir: r"C:\Users\Alice\AppData\Roaming\app.gamelobby.desktop".into(),
            update_endpoints: vec![
                "https://github.com/m72b/GameLobby/releases/latest/download/latest.json".into(),
            ],
        }
    }

    fn library_with_secrets() -> db::Db {
        let database = db::Db::memory().unwrap();
        {
            let conn = database.lock();
            for (id, store) in [("a", "steam"), ("b", "steam"), ("c", "epic")] {
                conn.execute(
                    "INSERT INTO games(id,title,store) VALUES(?,?,?)",
                    [id, id, store],
                )
                .unwrap();
            }
            for name in [
                "steamgriddb-api-key-dpapi",
                "igdb-client-id-dpapi",
                "igdb-client-secret-dpapi",
                crate::secrets::STEAM_OWNED_API_KEY,
                crate::secrets::STEAM_OWNED_STEAM_ID,
            ] {
                db::write_secret(&conn, name, FAKE_BLOB).unwrap();
            }
            // Fake plaintext values in other places a bug might read them from.
            conn.execute(
                "INSERT OR REPLACE INTO settings(key,value) VALUES('leak',?)",
                [format!(
                    "{FAKE_KEY} {FAKE_ID} {FAKE_SECRET} {FAKE_STEAM_KEY} {FAKE_STEAM_ID}"
                )],
            )
            .unwrap();
        }
        database
    }

    #[test]
    fn the_report_has_the_facts_and_no_secret_even_when_secrets_are_stored() {
        let database = library_with_secrets();
        let lines = vec![
            "2026-11-15T10:00:00.000Z INFO  game_lobby_lib Library opened".to_string(),
            format!(
                "2026-11-15T10:00:01.000Z WARN  game_lobby_lib failed: client_secret={FAKE_SECRET}"
            ),
            format!(
                "2026-11-15T10:00:02.000Z WARN  game_lobby_lib stray key {FAKE_KEY} and {FAKE_ID}"
            ),
            r"2026-11-15T10:00:03.000Z INFO  game_lobby_lib read C:\Users\Alice\Documents\x"
                .to_string(),
            format!(
                "2026-11-15T10:00:04.000Z WARN  reqwest GET https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/?key={FAKE_STEAM_KEY}&steamid={FAKE_STEAM_ID}&include_appinfo=1"
            ),
        ];
        let report = render(
            &environment(),
            &database.lock(),
            &lines,
            Some(r"C:\Users\Alice"),
        )
        .unwrap();
        for fact in [
            "App version: 0.3.0",
            "Windows: Windows 11 Pro 24H2 (build 26200.5000)",
            "Architecture: x86_64",
            "WebView2: 130.0.2849.80",
            "Games: 3",
            "Stores: epic 1, steam 2",
            "Update channel: stable (latest GitHub release)",
            "releases/latest/download/latest.json",
            "SteamGridDB API key set: yes",
            "IGDB credentials set: yes (Client ID set: yes; Client Secret set: yes)",
            "Steam owned games import: yes (API key set: yes; SteamID set: yes)",
            "--- Last 5 log lines ---",
            "Library opened",
        ] {
            assert!(report.contains(fact), "missing {fact:?} in\n{report}");
        }
        for secret in [
            FAKE_KEY,
            FAKE_ID,
            FAKE_SECRET,
            FAKE_STEAM_KEY,
            FAKE_STEAM_ID,
            "DPAPI-BLOB",
            "Alice",
        ] {
            assert!(!report.contains(secret), "{secret} leaked in\n{report}");
        }
        assert!(report.contains(r"Data folder: %USERPROFILE%\AppData\Roaming"));
        assert!(report.contains(r"read %USERPROFILE%\Documents\x"));
        assert!(report.contains("client_secret=[redacted]"));
    }

    #[test]
    fn presence_flags_say_no_when_nothing_is_stored() {
        let database = db::Db::memory().unwrap();
        let report = render(&environment(), &database.lock(), &[], None).unwrap();
        assert!(report.contains("SteamGridDB API key set: no"));
        assert!(
            report.contains("IGDB credentials set: no (Client ID set: no; Client Secret set: no)")
        );
        assert!(report.contains("Steam owned games import: no (API key set: no; SteamID set: no)"));
        assert!(report.contains("Games: 0"));
        assert!(report.contains("Stores: none"));
    }
}
