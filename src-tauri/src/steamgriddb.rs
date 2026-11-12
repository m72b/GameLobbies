//! SteamGridDB cover art (https://www.steamgriddb.com/api/v2).
//!
//! The API key comes from [`crate::secrets`] and is only ever sent in the
//! `Authorization: Bearer` header. Requests don't follow redirects (so the header can't be
//! forwarded elsewhere) and errors are reported without URLs or response bodies that could
//! carry request details.
//!
//! Renderer-facing commands (see `commands.rs` and `LobbyAPI.steamGridDb`):
//! - `steamgriddb_status` / `steamgriddb_save_key` — `{ keySet }`, never the key itself.
//! - `steamgriddb_search(gameId)` — up to [`MAX_CANDIDATES`] poster candidates.
//! - `steamgriddb_apply(gameId, url)` — sets a SteamGridDB image as the custom `cover`.
//! - `steamgriddb_fetch_missing()` — bulk-fills games with no cover at all; also runs in the
//!   background after a library scan when `steamGridDbAutoFetch` is on. Emits
//!   `library:changed` and `steamgriddb:fetched` (payload: [`FetchSummary`]).
//! - `steamgriddb_test_key()` — one authenticated request; [`KeyTest`] `{ ok, message }`.
//!
//! Grid requests follow the user's settings (read at request time, see [`GridPrefs`]):
//! `steamGridDbStyles`, `steamGridDbAnimated`, `steamGridDbNsfw`, `steamGridDbHumor`.
//! Game page heroes and logos ([`fetch_art`], used by `crate::art`) follow the same settings
//! except the grid styles, which don't apply to them.

use crate::db;
use crate::secrets::{get_steamgriddb_api_key, has_steamgriddb_api_key};
use crate::state::AppState;
use crate::types::{non_empty, Game, GamePatch, Settings, STEAM_GRID_DB_STYLES};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const API: &str = "https://www.steamgriddb.com/api/v2";
/// Poster sizes close to the UI's 2:3 card.
const DIMENSIONS: &str = "600x900,342x482,660x930";
pub const MAX_CANDIDATES: usize = 30;
/// Hero or logo candidates kept per game for the game page.
pub const MAX_ART_CANDIDATES: usize = 10;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPORTED_ERRORS: usize = 50;
/// Pause between games in a bulk run, to stay well below the API's rate limits.
const BULK_DELAY: Duration = Duration::from_millis(350);
const NO_KEY: &str = "Add a SteamGridDB API key in Settings first.";
const NO_SAVED_KEY: &str = "No SteamGridDB key is saved.";
const KEY_WORKS: &str = "SteamGridDB key works.";

/// One poster candidate returned to the renderer.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cover {
    pub id: u64,
    /// Full-size image (https://cdn2.steamgriddb.com/...).
    pub url: String,
    /// Smaller preview of the same image.
    pub thumb: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upvotes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downvotes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FetchSummary {
    pub updated: usize,
    pub not_found: usize,
    pub errors: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub key_set: bool,
}

/// Result of `steamgriddb_test_key`. `message` never contains the key.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyTest {
    pub ok: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Grid preferences (from Settings)

/// Which grids to ask SteamGridDB for. Built from [`Settings`] at request time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridPrefs {
    /// Known, distinct styles in canonical order; empty = any style.
    pub styles: Vec<&'static str>,
    pub animated: bool,
    pub nsfw: bool,
    pub humor: bool,
}

impl GridPrefs {
    /// Unknown or repeated styles are ignored (validation already rejects them on save).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            styles: STEAM_GRID_DB_STYLES
                .iter()
                .copied()
                .filter(|known| settings.steam_grid_db_styles.iter().any(|s| s == known))
                .collect(),
            animated: settings.steam_grid_db_animated,
            nsfw: settings.steam_grid_db_nsfw,
            humor: settings.steam_grid_db_humor,
        }
    }

    /// Query parameters for `GET /grids/game/{id}`.
    pub fn query(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![("dimensions", DIMENSIONS.to_string())];
        if !self.styles.is_empty() {
            pairs.push(("styles", self.styles.join(",")));
        }
        let types = if self.animated {
            "static,animated"
        } else {
            "static"
        };
        pairs.push(("types", types.into()));
        pairs.push(("nsfw", if self.nsfw { "any" } else { "false" }.into()));
        pairs.push(("humor", if self.humor { "any" } else { "false" }.into()));
        pairs
    }

    /// Query parameters for `GET /heroes/game/{id}` and `GET /logos/game/{id}`. Grid styles
    /// and poster dimensions don't apply to those.
    pub fn art_query(&self) -> Vec<(&'static str, String)> {
        self.query()
            .into_iter()
            .filter(|(key, _)| !matches!(*key, "dimensions" | "styles"))
            .collect()
    }

    /// Identifies the preferences that affect hero/logo results (for the art cache).
    pub fn art_cache_key(&self) -> String {
        format!(
            "animated={};nsfw={};humor={}",
            self.animated, self.nsfw, self.humor
        )
    }

    /// Local post-filter matching [`Self::query`]; epilepsy-flagged grids are always dropped.
    fn allows(&self, grid: &RawGrid) -> bool {
        grid.epilepsy != Some(true)
            && (self.nsfw || grid.nsfw != Some(true))
            && (self.humor || grid.humor != Some(true))
            && (self.styles.is_empty()
                || grid
                    .style
                    .as_deref()
                    .is_none_or(|s| self.styles.contains(&s)))
    }
}

// ---------------------------------------------------------------------------
// Errors

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    Unauthorized,
    Forbidden,
    NotFound,
    RateLimited,
    Http(u16),
    Network(String),
    Invalid(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => {
                f.write_str("SteamGridDB rejected the API key. Check it in Settings.")
            }
            Self::Forbidden => f.write_str("SteamGridDB denied access to this request."),
            Self::NotFound => f.write_str("SteamGridDB has no entry for this game."),
            Self::RateLimited => f.write_str(
                "SteamGridDB is limiting requests right now; try again in a few minutes.",
            ),
            Self::Http(code) => write!(f, "SteamGridDB returned HTTP {code}."),
            Self::Network(msg) => write!(f, "Could not reach SteamGridDB: {msg}"),
            Self::Invalid(msg) => write!(f, "SteamGridDB returned an unexpected response: {msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// Maps a non-success HTTP status to a friendly error.
pub fn status_error(status: u16) -> Option<ApiError> {
    match status {
        200..=299 => None,
        401 => Some(ApiError::Unauthorized),
        403 => Some(ApiError::Forbidden),
        404 => Some(ApiError::NotFound),
        429 => Some(ApiError::RateLimited),
        code => Some(ApiError::Http(code)),
    }
}

// ---------------------------------------------------------------------------
// Response parsing (pure, unit-tested)

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct SgdbGame {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub verified: Option<bool>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    errors: Option<Vec<Value>>,
}

fn envelope_data(body: &str) -> Result<Value, ApiError> {
    let env: Envelope = serde_json::from_str(body)
        .map_err(|_| ApiError::Invalid("the response is not valid JSON.".into()))?;
    if env.success == Some(false) {
        let detail = env
            .errors
            .unwrap_or_default()
            .into_iter()
            .find_map(|e| e.as_str().map(|s| s.chars().take(200).collect::<String>()))
            .unwrap_or_else(|| "the request failed.".into());
        return Err(ApiError::Invalid(detail));
    }
    env.data
        .ok_or_else(|| ApiError::Invalid("the response has no data.".into()))
}

/// `GET /games/steam/{appId}` → one game.
pub fn parse_game(body: &str) -> Result<SgdbGame, ApiError> {
    serde_json::from_value(envelope_data(body)?)
        .map_err(|_| ApiError::Invalid("the game entry is malformed.".into()))
}

/// `GET /search/autocomplete/{title}` → matching games (malformed entries are skipped).
pub fn parse_search(body: &str) -> Result<Vec<SgdbGame>, ApiError> {
    match envelope_data(body)? {
        Value::Array(items) => Ok(items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()),
        _ => Err(ApiError::Invalid("search results are malformed.".into())),
    }
}

#[derive(Deserialize)]
struct RawGrid {
    id: u64,
    url: String,
    thumb: String,
    width: u32,
    height: u32,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    score: Option<i64>,
    #[serde(default)]
    upvotes: Option<i64>,
    #[serde(default)]
    downvotes: Option<i64>,
    #[serde(default)]
    nsfw: Option<bool>,
    #[serde(default)]
    humor: Option<bool>,
    #[serde(default)]
    epilepsy: Option<bool>,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    mime: Option<String>,
}

/// Covers are drawn as CSS background images, which can show images (including animated
/// WebP/PNG/GIF) but not videos, so WebM grids are never offered or applied.
fn is_image(grid: &RawGrid) -> bool {
    let mime_ok = grid.mime.as_deref().is_none_or(|m| m.starts_with("image/"));
    mime_ok && !grid.url.to_ascii_lowercase().ends_with(".webm")
}

#[derive(Deserialize)]
struct RawAuthor {
    #[serde(default)]
    name: Option<String>,
}

/// `GET /grids/game/{id}` → candidates in API order, keeping only SteamGridDB-hosted,
/// non-epilepsy-flagged images that `prefs` allows (NSFW/humor/style), capped at
/// [`MAX_CANDIDATES`].
pub fn parse_grids(body: &str, prefs: &GridPrefs) -> Result<Vec<Cover>, ApiError> {
    let Value::Array(items) = envelope_data(body)? else {
        return Err(ApiError::Invalid("grid results are malformed.".into()));
    };
    Ok(items
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawGrid>(v).ok())
        .filter(|g| prefs.allows(g) && is_image(g))
        .filter_map(|g| {
            Some(Cover {
                id: g.id,
                url: validate_cover_url(&g.url).ok()?,
                thumb: validate_cover_url(&g.thumb).ok()?,
                width: g.width,
                height: g.height,
                style: g.style,
                score: g.score,
                upvotes: g.upvotes,
                downvotes: g.downvotes,
                author: g
                    .author
                    .and_then(|a| a.name)
                    .map(|n| n.chars().take(80).collect()),
            })
        })
        .take(MAX_CANDIDATES)
        .collect())
}

/// `GET /heroes/game/{id}` or `GET /logos/game/{id}` → image URLs in API order, with the same
/// safety filtering as [`parse_grids`] (grid styles ignored), capped at [`MAX_ART_CANDIDATES`].
pub fn parse_art(body: &str, prefs: &GridPrefs) -> Result<Vec<String>, ApiError> {
    let prefs = GridPrefs {
        styles: Vec::new(),
        ..prefs.clone()
    };
    Ok(parse_grids(body, &prefs)?
        .into_iter()
        .map(|c| c.url)
        .take(MAX_ART_CANDIDATES)
        .collect())
}

// ---------------------------------------------------------------------------
// Matching

/// How a game is looked up on SteamGridDB.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Steam(String),
    Title(String),
}

/// The first Steam route's app ID, when it is a plain number.
pub fn steam_app_id(game: &Game) -> Option<&str> {
    game.routes.iter().find_map(|r| {
        (r.source_id == "steam")
            .then(|| non_empty(&r.app_id))
            .flatten()
            .filter(|id| id.len() <= 12 && id.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Prefers a Steam route's app ID (exact), falling back to a title search.
pub fn lookup_for(game: &Game) -> Option<Lookup> {
    match steam_app_id(game) {
        Some(id) => Some(Lookup::Steam(id.to_string())),
        None => search_title(&game.title).map(Lookup::Title),
    }
}

/// Drops trademark symbols and extra whitespace; caps the length of the search term.
pub fn search_title(title: &str) -> Option<String> {
    let cleaned: String = title
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect::<String>();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// An exact (case/punctuation-insensitive) name match wins, verified entries first;
/// otherwise the API's own top-ranked result.
pub fn best_match<'a>(title: &str, results: &'a [SgdbGame]) -> Option<&'a SgdbGame> {
    let wanted = normalize(title);
    let exact = |g: &&SgdbGame| !wanted.is_empty() && normalize(&g.name) == wanted;
    results
        .iter()
        .filter(exact)
        .find(|g| g.verified == Some(true))
        .or_else(|| results.iter().find(exact))
        .or_else(|| results.first())
}

/// Only HTTPS images on steamgriddb.com hosts, without credentials or a port.
pub fn validate_cover_url(raw: &str) -> Result<String> {
    let invalid = || anyhow!("Only SteamGridDB image links can be used as covers.");
    if raw.len() > 2048 {
        return Err(invalid());
    }
    let url = url::Url::parse(raw.trim()).map_err(|_| invalid())?;
    let host_ok = url.host_str().is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        h == "steamgriddb.com" || h.ends_with(".steamgriddb.com")
    });
    if url.scheme() != "https"
        || !host_ok
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(invalid());
    }
    Ok(url.to_string())
}

// ---------------------------------------------------------------------------
// HTTP

/// Holds the key for one operation; intentionally not `Debug` so it can't be printed.
struct Client {
    http: reqwest::Client,
    key: String,
}

impl Client {
    fn new(key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("GameLobby/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                anyhow!(
                    "Could not start the SteamGridDB client: {}",
                    e.without_url()
                )
            })?;
        Ok(Self { http, key })
    }

    fn endpoint(segments: &[&str]) -> url::Url {
        let mut url = url::Url::parse(API).expect("valid API base");
        url.path_segments_mut()
            .expect("API base has a path")
            .extend(segments);
        url
    }

    async fn get(&self, url: url::Url) -> Result<String, ApiError> {
        let mut response = self
            .http
            .get(url)
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.without_url().to_string()))?;
        if let Some(e) = status_error(response.status().as_u16()) {
            return Err(e);
        }
        if response
            .content_length()
            .is_some_and(|len| len as usize > MAX_RESPONSE_BYTES)
        {
            return Err(ApiError::Invalid("the response is too large.".into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ApiError::Network(e.without_url().to_string()))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(ApiError::Invalid("the response is too large.".into()));
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|_| ApiError::Invalid("the response is not text.".into()))
    }

    /// Resolves the SteamGridDB game ID; `None` when nothing matches.
    async fn game_id(&self, game: &Game) -> Result<Option<u64>, ApiError> {
        let Some(lookup) = lookup_for(game) else {
            return Ok(None);
        };
        if let Lookup::Steam(app_id) = &lookup {
            match self.get(Self::endpoint(&["games", "steam", app_id])).await {
                Ok(body) => return parse_game(&body).map(|g| Some(g.id)),
                // Not every Steam app is on SteamGridDB; try the title instead.
                Err(ApiError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        let Some(title) = search_title(&game.title) else {
            return Ok(None);
        };
        let body = match self
            .get(Self::endpoint(&["search", "autocomplete", &title]))
            .await
        {
            Err(ApiError::NotFound) => return Ok(None),
            other => other?,
        };
        let results = parse_search(&body)?;
        Ok(best_match(&title, &results).map(|g| g.id))
    }

    fn grids_url(sgdb_id: u64, prefs: &GridPrefs) -> url::Url {
        let mut url = Self::endpoint(&["grids", "game", &sgdb_id.to_string()]);
        url.query_pairs_mut().extend_pairs(prefs.query());
        url
    }

    /// `kind` is `"heroes"` or `"logos"`.
    fn art_url(kind: &str, sgdb_id: u64, prefs: &GridPrefs) -> url::Url {
        let mut url = Self::endpoint(&[kind, "game", &sgdb_id.to_string()]);
        url.query_pairs_mut().extend_pairs(prefs.art_query());
        url
    }

    async fn art(
        &self,
        kind: &str,
        sgdb_id: u64,
        prefs: &GridPrefs,
    ) -> Result<Vec<String>, ApiError> {
        match self.get(Self::art_url(kind, sgdb_id, prefs)).await {
            Ok(body) => parse_art(&body, prefs),
            Err(ApiError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn grids(&self, sgdb_id: u64, prefs: &GridPrefs) -> Result<Vec<Cover>, ApiError> {
        match self.get(Self::grids_url(sgdb_id, prefs)).await {
            Ok(body) => parse_grids(&body, prefs),
            Err(ApiError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

fn client(state: &AppState) -> Result<Client> {
    Client::new(get_steamgriddb_api_key(state)?.ok_or_else(|| anyhow!(NO_KEY))?)
}

/// The grid preferences as currently saved.
pub fn grid_prefs(state: &AppState) -> Result<GridPrefs> {
    Ok(GridPrefs::from_settings(&db::get_settings(
        &state.db.lock(),
    )?))
}

// ---------------------------------------------------------------------------
// Operations used by the commands

pub fn status(state: &AppState) -> Result<KeyStatus> {
    Ok(KeyStatus {
        key_set: has_steamgriddb_api_key(state)?,
    })
}

pub async fn search(state: &AppState, game_id: &str) -> Result<Vec<Cover>> {
    let game =
        db::get_game(&state.db.lock(), game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let client = client(state)?;
    let Some(sgdb_id) = client.game_id(&game).await? else {
        return Ok(Vec::new());
    };
    let prefs = grid_prefs(state)?;
    Ok(client.grids(sgdb_id, &prefs).await?)
}

/// Hero and logo URLs for the game page. `Ok(None)` when no key is saved; `Ok(Some(empty))`
/// when SteamGridDB has no match. Errors never contain the key or request URLs.
pub async fn fetch_art(
    state: &AppState,
    game: &Game,
    prefs: &GridPrefs,
) -> Result<Option<(Vec<String>, Vec<String>)>> {
    let Some(key) = get_steamgriddb_api_key(state)? else {
        return Ok(None);
    };
    let client = Client::new(key)?;
    let Some(sgdb_id) = client.game_id(game).await? else {
        return Ok(Some((Vec::new(), Vec::new())));
    };
    let hero = client.art("heroes", sgdb_id, prefs).await?;
    let logo = client.art("logos", sgdb_id, prefs).await?;
    Ok(Some((hero, logo)))
}

/// Makes one cheap authenticated request to check the saved key. Never fails: problems are
/// reported as `ok: false` with a friendly message that never includes the key.
pub async fn test_key(state: &AppState) -> KeyTest {
    let fail = |message: String| KeyTest { ok: false, message };
    let key = match get_steamgriddb_api_key(state) {
        Ok(Some(key)) => key,
        Ok(None) => return fail(NO_SAVED_KEY.into()),
        Err(e) => return fail(e.to_string()),
    };
    let client = match Client::new(key) {
        Ok(client) => client,
        Err(e) => return fail(e.to_string()),
    };
    let result = client
        .get(Client::endpoint(&["search", "autocomplete", "portal"]))
        .await
        .and_then(|body| parse_search(&body).map(|_| ()));
    key_test_result(result, &client.key)
}

/// Maps the probe's outcome to a [`KeyTest`], redacting the key should a message echo it.
fn key_test_result(result: Result<(), ApiError>, key: &str) -> KeyTest {
    let error = match result {
        Ok(()) => {
            return KeyTest {
                ok: true,
                message: KEY_WORKS.into(),
            }
        }
        // On this endpoint a 404 means the API path itself is missing, not the game.
        Err(ApiError::NotFound) => ApiError::Http(404),
        Err(e) => e,
    };
    let mut message = error.to_string();
    if !key.is_empty() {
        message = message.replace(key, "[key]");
    }
    KeyTest { ok: false, message }
}

pub fn apply(state: &AppState, game_id: &str, url: &str) -> Result<Vec<Game>> {
    let url = validate_cover_url(url)?;
    let conn = state.db.lock();
    if db::get_game(&conn, game_id)?.is_none() {
        bail!("Game not found.");
    }
    db::save_game(
        &conn,
        &GamePatch {
            cover: Some(url),
            ..GamePatch::new(game_id)
        },
    )
}

fn missing_cover(game: &Game) -> bool {
    non_empty(&game.cover).is_none() && non_empty(&game.default_cover).is_none()
}

/// Fills `cover` and `defaultCover` for every game that has neither. Fails if another run is
/// in progress or the key is rejected (401); other per-game failures are collected.
pub async fn fetch_missing(app: &AppHandle) -> Result<FetchSummary> {
    let state = app.state::<AppState>();
    let Ok(_guard) = state.cover_fetch_lock.try_lock() else {
        bail!("A SteamGridDB cover fetch is already running.");
    };
    let client = client(&state)?;
    let prefs = grid_prefs(&state)?;
    let games: Vec<Game> = db::all_games(&state.db.lock())?
        .into_iter()
        .filter(missing_cover)
        .collect();
    let mut summary = FetchSummary::default();
    let mut changed = false;
    let result = async {
        for (index, game) in games.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(BULK_DELAY).await;
            }
            let found = match client.game_id(game).await {
                Ok(Some(id)) => client.grids(id, &prefs).await.map(|g| g.into_iter().next()),
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            };
            match found {
                Ok(Some(cover)) => {
                    let conn = state.db.lock();
                    // Skip games that got a cover (or were removed) while we were waiting.
                    if db::get_game(&conn, &game.id)?.is_some_and(|g| missing_cover(&g)) {
                        db::save_game(
                            &conn,
                            &GamePatch {
                                cover: Some(cover.url.clone()),
                                default_cover: Some(cover.url),
                                ..GamePatch::new(game.id.clone())
                            },
                        )?;
                        summary.updated += 1;
                        changed = true;
                    }
                }
                Ok(None) => summary.not_found += 1,
                Err(ApiError::Unauthorized) => {
                    bail!("SteamGridDB rejected the API key. Check it in Settings.")
                }
                Err(ApiError::RateLimited) => {
                    summary.errors.push(ApiError::RateLimited.to_string());
                    break;
                }
                Err(e) => {
                    if summary.errors.len() < MAX_REPORTED_ERRORS {
                        summary.errors.push(format!("{}: {e}", game.title));
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if changed {
        let _ = app.emit("library:changed", ());
    }
    result?;
    let _ = app.emit("steamgriddb:fetched", &summary);
    Ok(summary)
}

/// Called after a library scan: fetches missing covers in the background when a key is set
/// and `steamGridDbAutoFetch` is on. Never affects the scan result.
pub fn spawn_auto_fetch(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let enabled = db::get_settings(&state.db.lock())
            .map(|s| s.steam_grid_db_auto_fetch)
            .unwrap_or(false);
        if !enabled || !has_steamgriddb_api_key(&state).unwrap_or(false) {
            return;
        }
        let has_missing = db::all_games(&state.db.lock())
            .map(|games| games.iter().any(missing_cover))
            .unwrap_or(false);
        if !has_missing || state.cover_fetch_lock.try_lock().is_err() {
            return;
        }
        match fetch_missing(&app).await {
            Ok(s) if s.updated > 0 || !s.errors.is_empty() => eprintln!(
                "SteamGridDB: {} covers added, {} not found, {} errors.",
                s.updated,
                s.not_found,
                s.errors.len()
            ),
            Ok(_) => {}
            Err(e) => eprintln!("SteamGridDB cover fetch failed: {e:#}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameRoute;

    fn game(title: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: "g".into(),
            title: title.into(),
            store: "GOG".into(),
            routes,
            ..Default::default()
        }
    }

    fn route(source: &str, app_id: Option<&str>) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            store: "Steam".into(),
            app_id: app_id.map(str::to_owned),
            launch_target: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn cover_urls_are_limited_to_steamgriddb_https() {
        let ok = "https://cdn2.steamgriddb.com/grid/abc.png";
        assert_eq!(validate_cover_url(ok).unwrap(), ok);
        assert!(validate_cover_url("https://steamgriddb.com/x.png").is_ok());
        assert!(validate_cover_url("https://CDN2.SteamGridDB.com/x.png").is_ok());
        for bad in [
            "http://cdn2.steamgriddb.com/x.png",
            "https://cdn2.steamgriddb.com:8443/x.png",
            "https://user:pw@cdn2.steamgriddb.com/x.png",
            "https://user@cdn2.steamgriddb.com/x.png",
            "https://evilsteamgriddb.com/x.png",
            "https://steamgriddb.com.evil.example/x.png",
            "https://evil.example/#.steamgriddb.com",
            "https://evil.example/?u=cdn2.steamgriddb.com",
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "lobby-cover://x.png",
            "cdn2.steamgriddb.com/x.png",
            "",
        ] {
            assert!(validate_cover_url(bad).is_err(), "{bad} should be rejected");
        }
        let long = format!("https://cdn2.steamgriddb.com/{}", "a".repeat(2100));
        assert!(validate_cover_url(&long).is_err());
    }

    #[test]
    fn lookup_prefers_the_steam_route() {
        let g = game(
            "Portal",
            vec![route("gog-galaxy", None), route("steam", Some("400"))],
        );
        assert_eq!(lookup_for(&g), Some(Lookup::Steam("400".into())));
        // Non-Steam routes, missing or malformed app IDs fall back to the title.
        let g = game(
            "Portal™ 2",
            vec![
                route("steam", Some("")),
                route("steam", Some("12/../x")),
                route("epic", Some("123")),
            ],
        );
        assert_eq!(lookup_for(&g), Some(Lookup::Title("Portal 2".into())));
        assert_eq!(lookup_for(&game(" ® ", vec![])), None);
    }

    #[test]
    fn titles_are_url_encoded_as_one_path_segment() {
        let url = Client::endpoint(&["search", "autocomplete", "AC/DC: Rock & Roll? #1 100%"]);
        assert_eq!(
            url.as_str(),
            "https://www.steamgriddb.com/api/v2/search/autocomplete/AC%2FDC:%20Rock%20&%20Roll%3F%20%231%20100%25"
        );
        assert_eq!(url.host_str(), Some("www.steamgriddb.com"));
    }

    #[test]
    fn parses_search_results_and_picks_the_best_match() {
        let body = r#"{"success":true,"data":[
            {"id":1,"name":"Hades II","types":["steam"],"verified":true},
            {"id":2,"name":"HADES","types":[],"verified":false},
            {"id":3,"name":"Hades","types":["steam"],"verified":true},
            {"oops":true}
        ]}"#;
        let results = parse_search(body).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(best_match("Hades", &results).unwrap().id, 3);
        assert_eq!(best_match("hades!", &results).unwrap().id, 3);
        assert_eq!(best_match("Something else", &results).unwrap().id, 1);
        assert_eq!(best_match("Hades", &[]), None);
        assert!(parse_search(r#"{"success":true,"data":[]}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parses_a_steam_game_lookup() {
        let body = r#"{"success":true,"data":{"id":5249,"name":"Portal","release_date":1192060800,"types":["steam"],"verified":true}}"#;
        assert_eq!(parse_game(body).unwrap().id, 5249);
    }

    #[test]
    fn parses_grids_and_filters_unsafe_entries() {
        let body = r#"{"success":true,"data":[
            {"id":10,"score":5,"style":"alternate","width":600,"height":900,"nsfw":false,"humor":false,
             "mime":"image/png","language":"en","url":"https://cdn2.steamgriddb.com/grid/a.png",
             "thumb":"https://cdn2.steamgriddb.com/thumb/a.jpg","lock":false,"epilepsy":false,
             "upvotes":7,"downvotes":1,"author":{"name":"artist","steam64":"1","avatar":"x"}},
            {"id":11,"width":600,"height":900,"nsfw":true,
             "url":"https://cdn2.steamgriddb.com/grid/b.png","thumb":"https://cdn2.steamgriddb.com/thumb/b.png"},
            {"id":12,"width":600,"height":900,"epilepsy":true,
             "url":"https://cdn2.steamgriddb.com/grid/c.png","thumb":"https://cdn2.steamgriddb.com/thumb/c.png"},
            {"id":13,"width":600,"height":900,
             "url":"https://evil.example/grid/d.png","thumb":"https://cdn2.steamgriddb.com/thumb/d.png"},
            {"id":14,"width":342,"height":482,
             "url":"https://cdn2.steamgriddb.com/grid/e.png","thumb":"https://cdn2.steamgriddb.com/thumb/e.png"},
            {"id":15,"width":600,"height":900,"mime":"video/webm",
             "url":"https://cdn2.steamgriddb.com/grid/f.webm","thumb":"https://cdn2.steamgriddb.com/thumb/f.png"},
            {"id":16,"width":600,"height":900,
             "url":"https://cdn2.steamgriddb.com/grid/g.WEBM","thumb":"https://cdn2.steamgriddb.com/thumb/g.png"}
        ]}"#;
        let covers = parse_grids(body, &GridPrefs::default()).unwrap();
        assert_eq!(covers.iter().map(|c| c.id).collect::<Vec<_>>(), [10, 14]);
        let first = &covers[0];
        assert_eq!(first.url, "https://cdn2.steamgriddb.com/grid/a.png");
        assert_eq!(first.thumb, "https://cdn2.steamgriddb.com/thumb/a.jpg");
        assert_eq!((first.width, first.height), (600, 900));
        assert_eq!(first.style.as_deref(), Some("alternate"));
        assert_eq!((first.score, first.upvotes), (Some(5), Some(7)));
        assert_eq!(first.author.as_deref(), Some("artist"));
        let json = serde_json::to_value(first).unwrap();
        assert!(json.get("upvotes").is_some() && json.get("nsfw").is_none());

        let many: Vec<String> = (0..50)
            .map(|i| format!(r#"{{"id":{i},"width":600,"height":900,"url":"https://cdn2.steamgriddb.com/g/{i}.png","thumb":"https://cdn2.steamgriddb.com/t/{i}.png"}}"#))
            .collect();
        let body = format!(r#"{{"success":true,"data":[{}]}}"#, many.join(","));
        assert_eq!(
            parse_grids(&body, &GridPrefs::default()).unwrap().len(),
            MAX_CANDIDATES
        );
    }

    fn prefs(styles: &[&str], animated: bool, nsfw: bool, humor: bool) -> GridPrefs {
        GridPrefs::from_settings(&Settings {
            steam_grid_db_styles: styles.iter().map(|s| s.to_string()).collect(),
            steam_grid_db_animated: animated,
            steam_grid_db_nsfw: nsfw,
            steam_grid_db_humor: humor,
            ..Settings::default()
        })
    }

    #[test]
    fn grid_query_follows_the_settings() {
        let defaults = GridPrefs::from_settings(&Settings::default());
        assert_eq!(defaults, GridPrefs::default());
        assert_eq!(
            defaults.query(),
            [
                ("dimensions", DIMENSIONS.to_string()),
                ("types", "static".into()),
                ("nsfw", "false".into()),
                ("humor", "false".into()),
            ]
        );
        // Styles come out in canonical order, de-duplicated, unknown values ignored.
        let all = prefs(
            &["no_logo", "alternate", "bogus", "alternate"],
            true,
            true,
            true,
        );
        assert_eq!(all.styles, ["alternate", "no_logo"]);
        assert_eq!(
            all.query(),
            [
                ("dimensions", DIMENSIONS.to_string()),
                ("styles", "alternate,no_logo".into()),
                ("types", "static,animated".into()),
                ("nsfw", "any".into()),
                ("humor", "any".into()),
            ]
        );
        let url = Client::grids_url(42, &prefs(&["white_logo", "material"], false, false, true));
        assert_eq!(
            url.as_str(),
            "https://www.steamgriddb.com/api/v2/grids/game/42?dimensions=600x900%2C342x482%2C660x930&styles=white_logo%2Cmaterial&types=static&nsfw=false&humor=any"
        );
    }

    #[test]
    fn grid_post_filter_respects_the_settings() {
        let grid = |id: u64, extra: &str| {
            format!(
                r#"{{"id":{id},"width":600,"height":900,{extra}"url":"https://cdn2.steamgriddb.com/g/{id}.png","thumb":"https://cdn2.steamgriddb.com/t/{id}.png"}}"#
            )
        };
        let body = format!(
            r#"{{"success":true,"data":[{},{},{},{},{},{}]}}"#,
            grid(1, r#""style":"alternate","#),
            grid(2, r#""nsfw":true,"style":"alternate","#),
            grid(3, r#""humor":true,"style":"material","#),
            grid(4, r#""epilepsy":true,"nsfw":false,"#),
            grid(5, r#""style":"blurred","#),
            grid(6, ""),
        );
        let ids = |p: &GridPrefs| {
            parse_grids(&body, p)
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&GridPrefs::default()), [1, 5, 6]);
        assert_eq!(ids(&prefs(&[], true, true, false)), [1, 2, 5, 6]);
        assert_eq!(ids(&prefs(&[], false, false, true)), [1, 3, 5, 6]);
        // Epilepsy-flagged grids stay out whatever the settings.
        assert_eq!(ids(&prefs(&[], true, true, true)), [1, 2, 3, 5, 6]);
        // A style filter keeps the listed styles and entries without a style.
        assert_eq!(
            ids(&prefs(&["alternate", "material"], false, true, true)),
            [1, 2, 3, 6]
        );
    }

    #[test]
    fn art_requests_follow_the_settings_except_grid_styles() {
        let p = prefs(&["material"], true, false, true);
        assert_eq!(
            Client::art_url("heroes", 7, &p).as_str(),
            "https://www.steamgriddb.com/api/v2/heroes/game/7?types=static%2Canimated&nsfw=false&humor=any"
        );
        assert_eq!(
            Client::art_url("logos", 7, &GridPrefs::default()).as_str(),
            "https://www.steamgriddb.com/api/v2/logos/game/7?types=static&nsfw=false&humor=false"
        );
        assert_ne!(
            p.art_cache_key(),
            prefs(&["material"], true, true, true).art_cache_key()
        );
        // Styles don't change hero/logo results, so they don't invalidate the cache.
        assert_eq!(
            p.art_cache_key(),
            prefs(&[], true, false, true).art_cache_key()
        );

        let body = r#"{"success":true,"data":[
            {"id":1,"style":"alternate","width":1920,"height":620,"url":"https://cdn2.steamgriddb.com/hero/a.png","thumb":"https://cdn2.steamgriddb.com/hero_thumb/a.jpg"},
            {"id":2,"width":1920,"height":620,"nsfw":true,"url":"https://cdn2.steamgriddb.com/hero/b.png","thumb":"https://cdn2.steamgriddb.com/hero_thumb/b.jpg"},
            {"id":3,"width":1920,"height":620,"mime":"video/webm","url":"https://cdn2.steamgriddb.com/hero/c.webm","thumb":"https://cdn2.steamgriddb.com/hero_thumb/c.jpg"},
            {"id":4,"width":1920,"height":620,"url":"https://evil.example/hero/d.png","thumb":"https://cdn2.steamgriddb.com/hero_thumb/d.jpg"},
            {"id":5,"style":"official","width":800,"height":300,"mime":"image/png","url":"https://cdn2.steamgriddb.com/logo/e.png","thumb":"https://cdn2.steamgriddb.com/logo_thumb/e.png"}
        ]}"#;
        // A grid style filter must not drop heroes/logos with other styles.
        assert_eq!(
            parse_art(body, &prefs(&["material"], false, false, false)).unwrap(),
            [
                "https://cdn2.steamgriddb.com/hero/a.png",
                "https://cdn2.steamgriddb.com/logo/e.png"
            ]
        );
        assert_eq!(
            parse_art(body, &prefs(&[], false, true, false))
                .unwrap()
                .len(),
            3
        );
        let many: Vec<String> = (0..20)
            .map(|i| format!(r#"{{"id":{i},"width":1,"height":1,"url":"https://cdn2.steamgriddb.com/h/{i}.png","thumb":"https://cdn2.steamgriddb.com/t/{i}.png"}}"#))
            .collect();
        let body = format!(r#"{{"success":true,"data":[{}]}}"#, many.join(","));
        assert_eq!(
            parse_art(&body, &GridPrefs::default()).unwrap().len(),
            MAX_ART_CANDIDATES
        );
    }

    #[test]
    fn key_test_messages_are_friendly_and_never_contain_the_key() {
        let key = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            key_test_result(Ok(()), key),
            KeyTest {
                ok: true,
                message: KEY_WORKS.into()
            }
        );
        let rejected = key_test_result(Err(ApiError::Unauthorized), key);
        assert!(!rejected.ok);
        assert_eq!(rejected.message, ApiError::Unauthorized.to_string());
        let net = key_test_result(Err(ApiError::Network("timed out".into())), key);
        assert!(!net.ok && net.message.starts_with("Could not reach SteamGridDB"));
        assert_eq!(
            key_test_result(Err(ApiError::NotFound), key).message,
            "SteamGridDB returned HTTP 404."
        );
        let echoed = key_test_result(Err(ApiError::Invalid(format!("bad token {key}"))), key);
        assert!(!echoed.ok && !echoed.message.contains(key));
    }

    #[test]
    fn error_payloads_and_statuses_map_to_friendly_errors() {
        let e = parse_search(r#"{"success":false,"errors":["Game not found"]}"#).unwrap_err();
        assert_eq!(e, ApiError::Invalid("Game not found".into()));
        assert!(matches!(
            parse_grids("<html>", &GridPrefs::default()),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            parse_grids(r#"{"success":true}"#, &GridPrefs::default()),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            parse_game(r#"{"success":true,"data":[]}"#),
            Err(ApiError::Invalid(_))
        ));
        assert_eq!(status_error(200), None);
        assert_eq!(status_error(401), Some(ApiError::Unauthorized));
        assert_eq!(status_error(403), Some(ApiError::Forbidden));
        assert_eq!(status_error(404), Some(ApiError::NotFound));
        assert_eq!(status_error(429), Some(ApiError::RateLimited));
        assert_eq!(status_error(302), Some(ApiError::Http(302)));
        assert!(ApiError::Unauthorized
            .to_string()
            .contains("SteamGridDB rejected the API key"));
    }

    #[test]
    fn only_games_without_any_cover_are_bulk_filled() {
        let mut g = game("A", vec![]);
        assert!(missing_cover(&g));
        g.cover = Some(String::new());
        assert!(missing_cover(&g));
        g.default_cover = Some("https://x/y.jpg".into());
        assert!(!missing_cover(&g));
        g.default_cover = None;
        g.cover = Some("lobby-cover://a.png".into());
        assert!(!missing_cover(&g));
    }
}
