//! Optional Steam Web API import of owned games, so Steam games that are owned but not installed
//! show in the library. Without a key nothing here runs and Steam stays installed-games only
//! (`importers::steam`, from `appmanifest_*.acf`).
//!
//! The user supplies their own free Steam Web API key and SteamID64 (stored DPAPI-encrypted by
//! [`crate::secrets`] under new names the store-account cleanup never deletes). The renderer
//! only learns whether each is set. The one request goes to the documented
//! `IPlayerService/GetOwnedGames/v1` endpoint over https only, without redirects, with a timeout
//! and a response size cap. Steam only accepts the key in the query string, so no request URL,
//! `reqwest` error text, log line or status message ever contains it (or the SteamID): errors
//! are reduced to fixed sentences, and `redact` strips `key=` and SteamID64s as a second line of
//! defence.
//!
//! Owned games that are not installed become Steam games with `installed: false`, an owned
//! mark and the usual `steam://run/<appid>` link (Steam offers to install a game that isn't).
//! Installed games keep what the manifest scan says. Titles, covers, favourites, notes and
//! playtime are never overwritten (the import adds no playtime). A failed request keeps the
//! previous list; the outcome is kept as a short status line for Settings. Library scans fetch
//! at most every [`REFRESH_AFTER_MS`] (after a failure, [`RETRY_AFTER_MS`]; after a rejected
//! key, not until the credentials are saved again).

use crate::db;
use crate::importers::{single_route_game, steam};
use crate::state::AppState;
use crate::types::{now_ms, Game, GameRoute};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const API_URL: &str = "https://api.steampowered.com/IPlayerService/GetOwnedGames/v1/";
/// The only host this module talks to in production.
pub const HOST: &str = "api.steampowered.com";
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A large library is about 150 bytes per game; this allows far more than any real account.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GAMES: usize = 50_000;
const MAX_NAME_CHARS: usize = 200;
/// After a successful fetch, scans don't ask Steam again for this long.
pub const REFRESH_AFTER_MS: i64 = 30 * 60_000;
/// After a failed fetch (offline, private profile, server error), scans wait this long.
pub const RETRY_AFTER_MS: i64 = 10 * 60_000;
/// `library_connections` row with the import state. Not `main`, which the cleanup rewrites.
const STATE_KEY: &str = "steam-owned";

pub const PRIVATE_MESSAGE: &str = "Steam profile game details are private. In Steam, set Privacy Settings → Game details to Public.";
pub const REJECTED_MESSAGE: &str =
    "Key rejected. Steam did not accept the API key; check it and save it again.";

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

// ---------------------------------------------------------------------------
// Fetching

/// Why a fetch failed. Messages are fixed text: never the key, the SteamID or a URL.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// HTTP 401/403: a wrong or revoked key.
    Rejected,
    /// No `games` list, or an empty one: the profile's game details are private.
    Private,
    RateLimited,
    Network(&'static str),
    Status(u16),
    Invalid(&'static str),
}

impl FetchError {
    pub fn message(&self) -> String {
        match self {
            Self::Rejected => REJECTED_MESSAGE.into(),
            Self::Private => PRIVATE_MESSAGE.into(),
            Self::RateLimited => "Steam is limiting requests. Game Lobby tries again later.".into(),
            Self::Network(what) => format!("Could not reach Steam: {what}"),
            Self::Status(code) => {
                format!("Steam returned an error (HTTP {code}). Game Lobby tries again later.")
            }
            Self::Invalid(what) => format!("Steam returned an unreadable response: {what}"),
        }
    }

    /// Whether only new credentials can fix it (no automatic retry).
    pub fn needs_new_credentials(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

/// One owned app from the response.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedApp {
    pub app_id: String,
    pub name: String,
}

/// A parsed response: every valid owned app id (named or not) and the apps that have a usable
/// name. What is gone is decided against `ids`, so an app whose name is missing from a degraded
/// response is never treated as removed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OwnedList {
    pub ids: HashSet<String>,
    pub apps: Vec<OwnedApp>,
}

/// Where the request goes. Production is fixed to [`HOST`] over https; tests use a local server.
#[derive(Clone, Debug)]
pub struct Endpoint {
    url: url::Url,
    https_only: bool,
}

impl Endpoint {
    pub fn production() -> Self {
        Self {
            url: url::Url::parse(API_URL).expect("valid Steam API URL"),
            https_only: true,
        }
    }

    #[cfg(test)]
    fn local(base: &str) -> Self {
        Self {
            url: url::Url::parse(&format!("{base}/IPlayerService/GetOwnedGames/v1/")).unwrap(),
            https_only: false,
        }
    }
}

fn network_error(e: &reqwest::Error) -> FetchError {
    // The error text is never used: it can carry the request URL (with the key).
    FetchError::Network(if e.is_timeout() {
        "the request timed out."
    } else if e.is_connect() {
        "no connection. Check that this PC is online."
    } else {
        "the connection failed."
    })
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > MAX_RESPONSE_BYTES)
    {
        return Err(FetchError::Invalid("the response is too large."));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| network_error(&e))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(FetchError::Invalid("the response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct Body {
    response: Option<OwnedGames>,
}
#[derive(Deserialize)]
struct OwnedGames {
    games: Option<Vec<OwnedItem>>,
}
#[derive(Deserialize)]
struct OwnedItem {
    appid: u64,
    #[serde(default)]
    name: Option<String>,
}

/// Plain one-line text: control and bidi characters dropped, whitespace collapsed, capped.
fn clean_name(raw: &str) -> Option<String> {
    let text = crate::validation::plain_text(raw);
    let text: String = text.chars().take(MAX_NAME_CHARS).collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Parses a `GetOwnedGames` response. Apps without a name still count as owned (`ids`); app id 0
/// is skipped and duplicates are dropped; a missing or empty list is [`FetchError::Private`].
pub fn parse_owned(body: &[u8]) -> Result<OwnedList, FetchError> {
    let parsed: Body = serde_json::from_slice(body)
        .map_err(|_| FetchError::Invalid("it is not the expected JSON."))?;
    let items = parsed
        .response
        .and_then(|r| r.games)
        .filter(|games| !games.is_empty())
        .ok_or(FetchError::Private)?;
    if items.len() > MAX_GAMES {
        return Err(FetchError::Invalid("it lists too many games."));
    }
    let mut list = OwnedList::default();
    for item in items {
        if item.appid == 0 || !list.ids.insert(item.appid.to_string()) {
            continue;
        }
        if let Some(name) = item.name.as_deref().and_then(clean_name) {
            list.apps.push(OwnedApp {
                app_id: item.appid.to_string(),
                name,
            });
        }
    }
    // Entries without any usable app: a degraded response, not a library that became empty.
    if list.apps.is_empty() {
        return Err(FetchError::Invalid("none of its games has a usable name."));
    }
    Ok(list)
}

/// One `GetOwnedGames` request. `key` and `steam_id` must already be validated.
pub async fn fetch_owned(
    endpoint: &Endpoint,
    key: &str,
    steam_id: &str,
) -> Result<OwnedList, FetchError> {
    if endpoint.https_only
        && (endpoint.url.scheme() != "https" || endpoint.url.host_str() != Some(HOST))
    {
        return Err(FetchError::Invalid(
            "requests may only go to api.steampowered.com.",
        ));
    }
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(endpoint.https_only)
        .user_agent(concat!("GameLobby/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| FetchError::Network("the HTTP client could not start."))?;
    let response = http
        .get(endpoint.url.clone())
        .query(&[
            ("key", key),
            ("steamid", steam_id),
            ("include_appinfo", "1"),
            ("include_played_free_games", "1"),
            ("format", "json"),
        ])
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| network_error(&e))?;
    let status = response.status().as_u16();
    match status {
        200 => {}
        401 | 403 => return Err(FetchError::Rejected),
        429 => return Err(FetchError::RateLimited),
        _ => return Err(FetchError::Status(status)),
    }
    parse_owned(&read_capped(response).await?)
}

// ---------------------------------------------------------------------------
// Stored state

/// The last outcome, for the status line in Settings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportStatus {
    pub ok: bool,
    pub message: String,
    pub checked_at: i64,
    /// Waits for new credentials (a rejected key): scans don't retry on their own.
    #[serde(default)]
    pub needs_new_credentials: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportState {
    /// App ids this import inserted or merged into a not-installed owned route, carried forward
    /// while they are still owned: what it may take out again.
    #[serde(default)]
    pub app_ids: Vec<String>,
    /// Ids a fetch reported gone but that were held back by the mass-removal guard; they are
    /// only dropped when the next successful fetch reports them gone too.
    #[serde(default)]
    pub pending_gone: Vec<String>,
    /// When `pending_gone` was first held back: the removal is only applied once it has stood
    /// for [`PENDING_MIN_AGE_MS`].
    #[serde(default)]
    pub pending_since: i64,
    /// When the last request was made (0 = never, or credentials changed since).
    #[serde(default)]
    pub attempted_at: i64,
    #[serde(default)]
    pub status: Option<ImportStatus>,
}

pub fn load_import_state(conn: &Connection) -> Result<ImportState> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM library_connections WHERE key=?",
            [STATE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

pub fn save_import_state(conn: &Connection, state: &ImportState) -> Result<()> {
    conn.execute(
        "INSERT INTO library_connections(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [STATE_KEY, &serde_json::to_string(state)?],
    )?;
    Ok(())
}

fn delete_import_state(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM library_connections WHERE key=?", [STATE_KEY])?;
    Ok(())
}

/// Whether a library scan should ask Steam now.
pub fn is_due(state: &ImportState, now: i64) -> bool {
    if state.attempted_at <= 0 || state.attempted_at > now {
        return true;
    }
    match &state.status {
        Some(s) if s.needs_new_credentials => false,
        Some(s) if !s.ok => now - state.attempted_at >= RETRY_AFTER_MS,
        _ => now - state.attempted_at >= REFRESH_AFTER_MS,
    }
}

// ---------------------------------------------------------------------------
// Applying

/// A library entry for an owned app that is not installed.
pub fn owned_game(app: &OwnedApp) -> Game {
    single_route_game(
        format!("steam:{}", app.app_id),
        app.name.clone(),
        "Steam",
        Some(steam::cover(&app.app_id)),
        GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            launch_target: format!("steam://run/{}", app.app_id),
            app_id: Some(app.app_id.clone()),
            owned: Some(true),
            installed: Some(false),
            ..Default::default()
        },
    )
}

/// Smallest earlier list the mass-removal guard applies to.
const MASS_REMOVAL_MIN: usize = 20;
/// How long a held-back removal must stand before a later fetch may apply it.
const PENDING_MIN_AGE_MS: i64 = 6 * 60 * 60 * 1000;
const HELD_BACK_MESSAGE: &str =
    "Steam returned far fewer games than before; nothing was removed. It will be checked again later.";

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Applies one fetch outcome made at `now`. Success: owned apps that aren't installed are added
/// or merged (installed ones are skipped; user data is never overwritten), and apps an earlier
/// import added that are no longer listed are dropped ([`db::drop_steam_owned`]). Failure: the
/// library and the previous list stay as they are. Either way the status is stored.
pub fn apply(
    conn: &mut Connection,
    outcome: &Result<OwnedList, FetchError>,
    now: i64,
) -> Result<ImportStatus> {
    let mut state = load_import_state(conn)?;
    state.attempted_at = now;
    let status = match outcome {
        Ok(list) => {
            let installed = db::installed_steam_app_ids(conn)?;
            // Every steam route that exists now, in any state: the import must never claim
            // (and later drop) a route it didn't create.
            let existing = db::steam_app_ids_any(conn)?;
            let games: Vec<Game> = list
                .apps
                .iter()
                .filter(|a| !installed.contains(&a.app_id))
                .map(owned_game)
                .collect();
            db::upsert_imported_games(conn, &games)?;
            let previous: HashSet<String> = state.app_ids.iter().cloned().collect();
            let gone: HashSet<String> = previous
                .iter()
                .filter(|id| !list.ids.contains(*id))
                .cloned()
                .collect();
            // A response far smaller than the last one is more likely degraded than true:
            // a removal is only applied when a later fetch, at least PENDING_MIN_AGE_MS after
            // the first, reports (nearly) the same apps gone.
            let suspicious = previous.len() >= MASS_REMOVAL_MIN && gone.len() * 2 > previous.len();
            let was_pending: HashSet<String> = state.pending_gone.iter().cloned().collect();
            let (to_drop, held): (HashSet<String>, HashSet<String>) = if suspicious {
                let agreed: HashSet<String> = gone.intersection(&was_pending).cloned().collect();
                let old_enough = !was_pending.is_empty()
                    && state.pending_since > 0
                    && now >= state.pending_since
                    && now - state.pending_since >= PENDING_MIN_AGE_MS;
                if old_enough && agreed.len() * 10 >= was_pending.len() * 9 {
                    let held = gone.difference(&agreed).cloned().collect();
                    (agreed, held)
                } else {
                    (HashSet::new(), gone)
                }
            } else {
                (gone, HashSet::new())
            };
            db::drop_steam_owned(conn, &to_drop)?;
            let mut ids: HashSet<String> = previous
                .iter()
                .filter(|id| list.ids.contains(*id) || held.contains(*id))
                .cloned()
                .collect();
            ids.extend(
                games
                    .iter()
                    .filter_map(|g| g.routes.first()?.app_id.clone())
                    .filter(|id| !existing.contains(id) || previous.contains(id)),
            );
            let mut ids: Vec<String> = ids.into_iter().collect();
            ids.sort();
            state.app_ids = ids;
            // The clock starts when a removal is first held; a different held set restarts it.
            state.pending_since = if held.is_empty() {
                0
            } else if !was_pending.is_empty() && state.pending_since > 0 && held == was_pending {
                state.pending_since
            } else {
                now
            };
            let mut pending: Vec<String> = held.into_iter().collect();
            pending.sort();
            state.pending_gone = pending;
            let apps = &list.apps;
            ImportStatus {
                ok: true,
                message: if state.pending_gone.is_empty() {
                    format!(
                        "{} on Steam · {} not installed",
                        plural(apps.len(), "owned game"),
                        games.len()
                    )
                } else if to_drop.is_empty() {
                    HELD_BACK_MESSAGE.to_string()
                } else {
                    format!(
                        "Steam returned far fewer games than before; removed {} that stayed missing, {} still held back.",
                        plural(to_drop.len(), "game"),
                        state.pending_gone.len()
                    )
                },
                checked_at: now,
                needs_new_credentials: false,
            }
        }
        Err(e) => ImportStatus {
            ok: false,
            message: e.message(),
            checked_at: now,
            needs_new_credentials: e.needs_new_credentials(),
        },
    };
    state.status = Some(status.clone());
    save_import_state(conn, &state)?;
    Ok(status)
}

/// Removes what the import added (games the user put nothing into) and its state.
pub fn undo(conn: &mut Connection) -> Result<usize> {
    let state = load_import_state(conn)?;
    let ids: HashSet<String> = state.app_ids.into_iter().collect();
    let deleted = db::drop_steam_owned(conn, &ids)?;
    delete_import_state(conn)?;
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Scan integration

fn steam_enabled(conn: &Connection) -> Result<bool> {
    Ok(!db::get_settings(conn)?
        .disabled_stores
        .iter()
        .any(|d| d == "steam"))
}

/// Fetches when credentials are saved, Steam is on and (unless `force`) the throttle allows.
/// `None` when no request was made.
async fn fetch_if_due(
    state: &AppState,
    force: bool,
) -> Result<Option<Result<OwnedList, FetchError>>> {
    {
        let conn = state.db.lock();
        if !steam_enabled(&conn)? || (!force && !is_due(&load_import_state(&conn)?, now_ms())) {
            return Ok(None);
        }
    }
    let Some((key, steam_id)) = crate::secrets::get_steam_owned_credentials(state)? else {
        return Ok(None);
    };
    Ok(Some(
        fetch_owned(&Endpoint::production(), &key, &steam_id).await,
    ))
}

/// Called by the library scan after the store scans were applied (the scan lock is held).
/// Returns an error line for the scan summary when a request failed.
pub async fn run_for_scan(app: &AppHandle) -> Result<Option<String>> {
    let Some(outcome) = fetch_if_due(&app.state::<AppState>(), false).await? else {
        return Ok(None);
    };
    let status = apply_in_background(app, outcome).await?;
    if status.ok {
        log::info!("Steam owned games: {}", status.message);
        Ok(None)
    } else {
        log::warn!("Steam owned games: {}", status.message);
        Ok(Some(format!("Steam owned games: {}", status.message)))
    }
}

/// [`apply`] on a blocking thread (it may write thousands of games).
async fn apply_in_background(
    app: &AppHandle,
    outcome: Result<OwnedList, FetchError>,
) -> Result<ImportStatus> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        apply(&mut app.state::<AppState>().db.lock(), &outcome, now_ms())
    })
    .await?
}

fn library_changed(app: &AppHandle) {
    app.state::<AppState>().tracker.library_changed();
    let _ = app.emit("library:changed", ());
}

// ---------------------------------------------------------------------------
// Commands. The key and SteamID are write-only from the renderer's side.

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SteamOwnedStatus {
    pub key_set: bool,
    pub steam_id_set: bool,
    pub last: Option<ImportStatus>,
}

fn status_of(state: &AppState) -> Result<SteamOwnedStatus> {
    let last = load_import_state(&state.db.lock())?.status;
    Ok(SteamOwnedStatus {
        key_set: crate::secrets::has_steam_owned_key(state)?,
        steam_id_set: crate::secrets::has_steam_owned_steam_id(state)?,
        last,
    })
}

#[tauri::command]
pub async fn steam_owned_status(state: State<'_, AppState>) -> CmdResult<SteamOwnedStatus> {
    status_of(&state).map_err(err)
}

/// Saves the non-empty fields (empty keeps the stored value), then, when both are saved,
/// imports the owned games right away (ignoring the scan throttle).
#[tauri::command]
pub async fn steam_owned_save(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    steam_id: String,
) -> CmdResult<SteamOwnedStatus> {
    crate::secrets::set_steam_owned_credentials(&state, &key, &steam_id).map_err(err)?;
    {
        // New credentials: forget the throttle and any "key rejected" pause.
        let conn = state.db.lock();
        let mut import = load_import_state(&conn).map_err(err)?;
        import.attempted_at = 0;
        import.status = None;
        save_import_state(&conn, &import).map_err(err)?;
    }
    // Waits for a running library scan, so the two never write the library at once.
    let _guard = state.scan_lock.lock().await;
    if let Some(outcome) = fetch_if_due(&state, true).await.map_err(err)? {
        let status = apply_in_background(&app, outcome).await.map_err(err)?;
        if status.ok {
            log::info!("Steam owned games: {}", status.message);
            library_changed(&app);
            crate::covercache::spawn_pass(&app, Duration::ZERO);
        } else {
            log::warn!("Steam owned games: {}", status.message);
        }
    }
    status_of(&state).map_err(err)
}

/// Removes the key and the SteamID, and the not-installed games the import added (games you
/// favourited, played, rated or customized stay).
#[tauri::command]
pub async fn steam_owned_remove(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<SteamOwnedStatus> {
    crate::secrets::clear_steam_owned_credentials(&state).map_err(err)?;
    let _guard = state.scan_lock.lock().await;
    let deleted = undo(&mut state.db.lock()).map_err(err)?;
    log::info!("Steam owned games import removed ({deleted} games removed from the library)");
    library_changed(&app);
    status_of(&state).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const STEAM_ID: &str = "76561197960287930";

    type Seen = Arc<Mutex<Vec<String>>>;

    /// A one-route HTTP server: every request gets `status` and `body`; request lines are kept.
    async fn serve(status: u16, body: String) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&raw).to_string();
                    log.lock()
                        .unwrap()
                        .push(head.lines().next().unwrap_or_default().to_string());
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    fn assert_no_credentials(text: &str) {
        assert!(!text.contains(KEY), "key leaked: {text}");
        assert!(!text.contains(STEAM_ID), "SteamID leaked: {text}");
        assert!(!text.contains("key="), "query leaked: {text}");
    }

    fn apps(list: &[(&str, &str)]) -> OwnedList {
        OwnedList {
            ids: list.iter().map(|(id, _)| id.to_string()).collect(),
            apps: list
                .iter()
                .map(|(id, name)| OwnedApp {
                    app_id: id.to_string(),
                    name: name.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn responses_are_parsed_cleaned_and_deduplicated() {
        let body = br#"{"response":{"game_count":5,"games":[
            {"appid":10,"name":"  Counter-Strike \n","playtime_forever":5},
            {"appid":10,"name":"Duplicate"},
            {"appid":0,"name":"Zero"},
            {"appid":20},
            {"appid":30,"name":"Bidi\u202e text\u0007"},
            {"appid":40,"name":"   "}
        ]}}"#;
        // Nameless apps are still owned (`ids`), they just aren't imported.
        let got = parse_owned(body).unwrap();
        assert_eq!(
            got.apps,
            apps(&[("10", "Counter-Strike"), ("30", "Bidi text")]).apps
        );
        let mut ids: Vec<_> = got.ids.iter().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["10", "20", "30", "40"]);
        let long = format!(
            r#"{{"response":{{"games":[{{"appid":1,"name":"{}"}}]}}}}"#,
            "x".repeat(500)
        );
        assert_eq!(
            parse_owned(long.as_bytes()).unwrap().apps[0].name.len(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn private_profiles_and_bad_bodies_are_errors() {
        for body in [
            &br#"{"response":{}}"#[..],
            br#"{"response":{"game_count":0,"games":[]}}"#,
            br#"{}"#,
        ] {
            assert_eq!(parse_owned(body), Err(FetchError::Private));
        }
        assert!(matches!(
            parse_owned(b"<html>"),
            Err(FetchError::Invalid(_))
        ));
        assert!(FetchError::Private
            .message()
            .starts_with("Steam profile game details are private"));
        assert!(FetchError::Rejected.message().starts_with("Key rejected"));
    }

    #[tokio::test]
    async fn the_request_uses_the_documented_endpoint_and_parses_the_games() {
        let (base, seen) = serve(
            200,
            r#"{"response":{"game_count":1,"games":[{"appid":570,"name":"Dota 2"}]}}"#.into(),
        )
        .await;
        let got = fetch_owned(&Endpoint::local(&base), KEY, STEAM_ID)
            .await
            .unwrap();
        assert_eq!(got, apps(&[("570", "Dota 2")]));
        let line = seen.lock().unwrap()[0].clone();
        assert!(
            line.starts_with("GET /IPlayerService/GetOwnedGames/v1/?"),
            "{line}"
        );
        for part in [
            format!("key={KEY}"),
            format!("steamid={STEAM_ID}"),
            "include_appinfo=1".into(),
            "include_played_free_games=1".into(),
            "format=json".into(),
        ] {
            assert!(line.contains(&part), "{part} missing in {line}");
        }
    }

    #[tokio::test]
    async fn failures_map_to_fixed_messages_without_credentials() {
        for (status, body, expected) in [
            (401, "", FetchError::Rejected),
            (403, "<html>Forbidden</html>", FetchError::Rejected),
            (429, "", FetchError::RateLimited),
            (500, "", FetchError::Status(500)),
            (200, r#"{"response":{}}"#, FetchError::Private),
        ] {
            let (base, _) = serve(status, body.into()).await;
            let e = fetch_owned(&Endpoint::local(&base), KEY, STEAM_ID)
                .await
                .unwrap_err();
            assert_eq!(e, expected);
            assert_no_credentials(&e.message());
        }
        // Too large.
        let (base, _) = serve(200, "x".repeat(MAX_RESPONSE_BYTES + 1)).await;
        let e = fetch_owned(&Endpoint::local(&base), KEY, STEAM_ID)
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::Invalid(_)));
        // Nothing listening.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = fetch_owned(
            &Endpoint::local(&format!("http://127.0.0.1:{port}")),
            KEY,
            STEAM_ID,
        )
        .await
        .unwrap_err();
        assert!(matches!(e, FetchError::Network(_)), "{e:?}");
        assert_no_credentials(&e.message());
    }

    #[tokio::test]
    async fn production_requests_only_go_to_the_steam_api_over_https() {
        let endpoint = Endpoint::production();
        assert_eq!(endpoint.url.scheme(), "https");
        assert_eq!(endpoint.url.host_str(), Some(HOST));
        let bad = Endpoint {
            url: url::Url::parse("http://api.steampowered.com/x").unwrap(),
            https_only: true,
        };
        assert!(matches!(
            fetch_owned(&bad, KEY, STEAM_ID).await,
            Err(FetchError::Invalid(_))
        ));
    }

    #[test]
    fn scans_are_throttled() {
        let now = 10 * REFRESH_AFTER_MS;
        let at = |attempted_at: i64, ok: bool, rejected: bool| ImportState {
            attempted_at,
            status: Some(ImportStatus {
                ok,
                message: String::new(),
                checked_at: attempted_at,
                needs_new_credentials: rejected,
            }),
            ..Default::default()
        };
        assert!(is_due(&ImportState::default(), now));
        assert!(!is_due(&at(now - REFRESH_AFTER_MS + 1, true, false), now));
        assert!(is_due(&at(now - REFRESH_AFTER_MS, true, false), now));
        assert!(!is_due(&at(now - RETRY_AFTER_MS + 1, false, false), now));
        assert!(is_due(&at(now - RETRY_AFTER_MS, false, false), now));
        assert!(!is_due(&at(now - 5 * REFRESH_AFTER_MS, false, true), now));
        // A clock that went backwards doesn't block forever.
        assert!(is_due(&at(now + 1000, true, false), now));
    }

    fn installed(app: &str, title: &str) -> Game {
        single_route_game(
            format!("steam:{app}"),
            title.into(),
            "Steam",
            Some(steam::cover(app)),
            GameRoute {
                source_id: "steam".into(),
                store: "Steam".into(),
                launch_target: format!("steam://run/{app}"),
                app_id: Some(app.into()),
                install_path: Some(format!(r"C:\Steam\steamapps\common\{app}")),
                installed: Some(true),
                install_states: crate::importers::states("steam-installed", true),
                ..Default::default()
            },
        )
    }

    #[test]
    fn owned_games_are_merged_without_duplicates_or_overwriting_user_data() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        db::upsert_imported_games(&mut conn, &[installed("570", "Dota 2")]).unwrap();
        // An owned game from before, renamed, favourited and given a cover by the user.
        db::upsert_imported_games(
            &mut conn,
            &[owned_game(&apps(&[("10", "Counter-Strike")]).apps[0])],
        )
        .unwrap();
        conn.execute(
            "UPDATE games SET title='CS 1.6',favorite=1,cover='lobby-cover://mine.png',playtimeSeconds=600 WHERE id='steam:10'",
            [],
        )
        .unwrap();
        let before_570 = db::get_game(&conn, "steam:570").unwrap().unwrap();

        let status = apply(
            &mut conn,
            &Ok(apps(&[
                ("570", "Dota 2 (Steam)"),
                ("10", "Counter-Strike"),
                ("20", "Team Fortress"),
            ])),
            1000,
        )
        .unwrap();
        assert!(status.ok);
        assert_eq!(status.message, "3 owned games on Steam · 2 not installed");

        assert_eq!(
            db::get_game(&conn, "steam:570").unwrap().unwrap(),
            before_570
        );
        let cs = db::get_game(&conn, "steam:10").unwrap().unwrap();
        assert_eq!(cs.title, "CS 1.6");
        assert!(cs.favorite);
        assert_eq!(cs.cover.as_deref(), Some("lobby-cover://mine.png"));
        assert_eq!(cs.playtime_seconds, Some(600));
        assert_eq!(cs.routes.len(), 1);
        let tf = db::get_game(&conn, "steam:20").unwrap().unwrap();
        assert_eq!(tf.title, "Team Fortress");
        assert_eq!(
            tf.cover.as_deref(),
            Some("https://cdn.akamai.steamstatic.com/steam/apps/20/library_600x900.jpg")
        );
        let route = &tf.routes[0];
        assert_eq!(
            (route.owned, route.installed, route.launch_target.as_str()),
            (Some(true), Some(false), "steam://run/20")
        );
        // The existing per-store link shape, so no link check had to be loosened.
        assert!(crate::validation::renderer_link_ok(&route.launch_target));
        assert_eq!(
            crate::launch::validate_launch_uri(&route.launch_target).unwrap(),
            "steam://run/20"
        );
        assert_eq!(db::game_count(&conn).unwrap(), 3);

        // A manifest scan afterwards still decides what is installed.
        db::reconcile_installed_source(&mut conn, "steam-installed", &[installed("570", "Dota 2")])
            .unwrap();
        let tf = db::get_game(&conn, "steam:20").unwrap().unwrap();
        assert_eq!(
            (tf.routes[0].owned, tf.routes[0].installed),
            (Some(true), Some(false))
        );
    }

    #[test]
    fn failures_keep_the_previous_list_and_record_a_status() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(apps(&[("10", "A"), ("20", "B")])), 1000).unwrap();
        for e in [
            FetchError::Private,
            FetchError::Rejected,
            FetchError::Network("x"),
        ] {
            let status = apply(&mut conn, &Err(e.clone()), 2000).unwrap();
            assert!(!status.ok);
            assert_eq!(status.message, e.message());
            assert_eq!(status.needs_new_credentials, e == FetchError::Rejected);
            let state = load_import_state(&conn).unwrap();
            assert_eq!(state.app_ids, ["10", "20"]);
            assert_eq!(state.attempted_at, 2000);
            assert_eq!(db::game_count(&conn).unwrap(), 2);
        }
    }

    #[test]
    fn games_no_longer_owned_and_a_removed_import_are_cleaned_up() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        // A game the import never listed (for example added by an older version) stays.
        db::upsert_imported_games(&mut conn, &[owned_game(&apps(&[("99", "Legacy")]).apps[0])])
            .unwrap();
        apply(
            &mut conn,
            &Ok(apps(&[("10", "A"), ("20", "B"), ("30", "C")])),
            1000,
        )
        .unwrap();
        conn.execute("UPDATE games SET favorite=1 WHERE id='steam:30'", [])
            .unwrap();
        apply(&mut conn, &Ok(apps(&[("10", "A"), ("30", "C")])), 2000).unwrap();
        assert!(db::get_game(&conn, "steam:20").unwrap().is_none());
        assert_eq!(load_import_state(&conn).unwrap().app_ids, ["10", "30"]);

        assert_eq!(undo(&mut conn).unwrap(), 1);
        assert!(db::get_game(&conn, "steam:10").unwrap().is_none());
        let kept = db::get_game(&conn, "steam:30").unwrap().unwrap();
        assert!(kept.favorite);
        assert_eq!(kept.routes[0].owned, None);
        assert!(db::get_game(&conn, "steam:99").unwrap().is_some());
        assert_eq!(load_import_state(&conn).unwrap(), ImportState::default());
    }

    fn many(n: usize) -> OwnedList {
        let list: Vec<(String, String)> = (1..=n)
            .map(|i| (i.to_string(), format!("Game {i}")))
            .collect();
        let refs: Vec<(&str, &str)> = list.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        apps(&refs)
    }

    #[test]
    fn nameless_entries_change_nothing() {
        // Only nameless entries: invalid, so nothing is applied.
        let body = br#"{"response":{"games":[{"appid":10},{"appid":20,"name":""}]}}"#;
        assert!(matches!(parse_owned(body), Err(FetchError::Invalid(_))));
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(apps(&[("10", "A"), ("20", "B")])), 1000).unwrap();
        let status = apply(&mut conn, &parse_owned(body), 2000).unwrap();
        assert!(!status.ok);
        assert_eq!(db::game_count(&conn).unwrap(), 2);
        assert_eq!(load_import_state(&conn).unwrap().app_ids, ["10", "20"]);
        // A named entry beside a nameless one: the nameless one is still owned, never dropped.
        let mut partial = apps(&[("10", "A")]);
        partial.ids.insert("20".into());
        apply(&mut conn, &Ok(partial), 3000).unwrap();
        assert!(db::get_game(&conn, "steam:20").unwrap().is_some());
        assert_eq!(load_import_state(&conn).unwrap().app_ids, ["10", "20"]);
    }

    #[test]
    fn a_far_smaller_list_is_held_back_until_a_second_fetch_agrees() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(many(100)), 1000).unwrap();
        assert_eq!(db::game_count(&conn).unwrap(), 100);
        let status = apply(&mut conn, &Ok(apps(&[("1", "Game 1")])), 2000).unwrap();
        assert!(status.ok);
        assert_eq!(status.message, HELD_BACK_MESSAGE);
        assert_eq!(db::game_count(&conn).unwrap(), 100);
        let state = load_import_state(&conn).unwrap();
        assert_eq!(state.app_ids.len(), 100);
        assert_eq!(state.pending_gone.len(), 99);
        // A recovered fetch clears the marker and drops nothing.
        apply(&mut conn, &Ok(many(100)), 3000).unwrap();
        assert!(load_import_state(&conn).unwrap().pending_gone.is_empty());
        assert_eq!(db::game_count(&conn).unwrap(), 100);
        // Two agreeing fetches six hours apart apply it.
        apply(&mut conn, &Ok(apps(&[("1", "Game 1")])), 4000).unwrap();
        assert_eq!(db::game_count(&conn).unwrap(), 100);
        let status = apply(
            &mut conn,
            &Ok(apps(&[("1", "Game 1")])),
            4000 + 6 * 60 * 60 * 1000,
        )
        .unwrap();
        assert_eq!(status.message, "1 owned game on Steam · 1 not installed");
        assert_eq!(db::game_count(&conn).unwrap(), 1);
        let state = load_import_state(&conn).unwrap();
        assert_eq!(state.app_ids, ["1"]);
        assert!(state.pending_gone.is_empty());
    }

    const SIX_HOURS: i64 = 6 * 60 * 60 * 1000;

    #[test]
    fn a_second_fetch_within_six_hours_does_not_confirm_a_mass_removal() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(many(100)), 1000).unwrap();
        let tiny = apps(&[("1", "Game 1")]);
        apply(&mut conn, &Ok(tiny.clone()), 2000).unwrap();
        let state = load_import_state(&conn).unwrap();
        assert_eq!((state.pending_since, state.pending_gone.len()), (2000, 99));
        for at in [3000, 2000 + SIX_HOURS - 1] {
            let status = apply(&mut conn, &Ok(tiny.clone()), at).unwrap();
            assert_eq!(status.message, HELD_BACK_MESSAGE);
            assert_eq!(db::game_count(&conn).unwrap(), 100);
            assert_eq!(load_import_state(&conn).unwrap().pending_since, 2000);
        }
        // A fetch that sees nothing suspicious clears the marker and the clock.
        apply(&mut conn, &Ok(many(100)), 4000).unwrap();
        let state = load_import_state(&conn).unwrap();
        assert!(state.pending_gone.is_empty());
        assert_eq!(state.pending_since, 0);
    }

    #[test]
    fn two_different_partial_responses_do_not_confirm_each_other() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(many(100)), 1000).unwrap();
        // Response A lists 1..=10, response B lists 51..=60: the gone sets are disjoint.
        let part = |from: usize| {
            let list: Vec<(String, String)> = (from..from + 10)
                .map(|i| (i.to_string(), format!("Game {i}")))
                .collect();
            let refs: Vec<(&str, &str)> =
                list.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            apps(&refs)
        };
        apply(&mut conn, &Ok(part(1)), 2000).unwrap();
        let status = apply(&mut conn, &Ok(part(51)), 2000 + SIX_HOURS).unwrap();
        assert_eq!(status.message, HELD_BACK_MESSAGE);
        assert_eq!(db::game_count(&conn).unwrap(), 100);
        // The held set changed, so the clock restarted at the second response.
        assert_eq!(
            load_import_state(&conn).unwrap().pending_since,
            2000 + SIX_HOURS
        );
    }

    #[test]
    fn an_agreeing_fetch_after_six_hours_applies_the_removal() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(&mut conn, &Ok(many(100)), 1000).unwrap();
        let tiny = apps(&[("1", "Game 1")]);
        apply(&mut conn, &Ok(tiny.clone()), 2000).unwrap();
        let status = apply(&mut conn, &Ok(tiny), 2000 + SIX_HOURS).unwrap();
        assert_eq!(status.message, "1 owned game on Steam · 1 not installed");
        assert_eq!(db::game_count(&conn).unwrap(), 1);
        let state = load_import_state(&conn).unwrap();
        assert_eq!(state.app_ids, ["1"]);
        assert!(state.pending_gone.is_empty());
        assert_eq!(state.pending_since, 0);
    }

    #[test]
    fn a_route_the_import_did_not_create_is_never_tracked() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        // A steam route that exists but is not owned and not installed.
        let mut pre = owned_game(&apps(&[("77", "Mine")]).apps[0]);
        pre.routes[0].owned = None;
        db::upsert_imported_games(&mut conn, &[pre]).unwrap();
        apply(&mut conn, &Ok(apps(&[("10", "A"), ("77", "Mine")])), 1000).unwrap();
        assert_eq!(load_import_state(&conn).unwrap().app_ids, ["10"]);
        apply(&mut conn, &Ok(apps(&[("10", "A")])), 2000).unwrap();
        assert!(db::get_game(&conn, "steam:77").unwrap().is_some());
    }
    #[test]
    fn a_manual_igdb_match_is_user_data() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        apply(
            &mut conn,
            &Ok(apps(&[("10", "A"), ("20", "B"), ("30", "C")])),
            1000,
        )
        .unwrap();
        for (id, source) in [("steam:10", "user"), ("steam:20", "auto")] {
            conn.execute(
                "INSERT INTO game_metadata(gameId,igdbId,json,fetchedAt,source) VALUES(?,1,NULL,0,?)",
                [id, source],
            )
            .unwrap();
        }
        apply(&mut conn, &Ok(apps(&[("30", "C")])), 2000).unwrap();
        let kept = db::get_game(&conn, "steam:10").unwrap().unwrap();
        assert_eq!(kept.routes[0].owned, None);
        assert!(db::get_game(&conn, "steam:20").unwrap().is_none());
    }

    #[test]
    fn installed_games_are_never_tracked_or_dropped() {
        let database = db::Db::memory().unwrap();
        let mut conn = database.lock();
        db::upsert_imported_games(&mut conn, &[installed("570", "Dota 2")]).unwrap();
        apply(
            &mut conn,
            &Ok(apps(&[("570", "Dota 2"), ("10", "A")])),
            1000,
        )
        .unwrap();
        assert_eq!(load_import_state(&conn).unwrap().app_ids, ["10"]);
        apply(&mut conn, &Ok(apps(&[("10", "A")])), 2000).unwrap();
        assert!(
            db::get_game(&conn, "steam:570").unwrap().unwrap().routes[0].installed == Some(true)
        );
        assert_eq!(undo(&mut conn).unwrap(), 1);
        assert!(db::get_game(&conn, "steam:570").unwrap().is_some());
    }
}
