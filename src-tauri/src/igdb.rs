//! IGDB game metadata for the game page (description, genres, companies, ratings).
//!
//! IGDB authenticates with a Twitch application's client credentials (Client ID and Client
//! Secret, stored DPAPI-encrypted by [`crate::secrets`]). The access token is requested with a
//! form-encoded POST body (never a query string), cached in memory only with its expiry, and
//! refreshed when it expires or a request comes back 401. Neither the secret nor the token is
//! ever returned to the renderer, logged, put in a URL or included in an error message.
//!
//! Requests go only to `https://id.twitch.tv` and `https://api.igdb.com` (https only, no
//! redirects, 15 s timeout, 2 MB response cap) and are spaced [`REQUEST_SPACING`] apart (IGDB
//! allows about 4 per second); a 429 backs off and retries a couple of times.
//!
//! Matching, in order: a user-chosen IGDB id (always wins); a Steam app id through
//! `external_games` (Steam is source 1; the field is `external_game_source`, formerly
//! `category`, so both are tried); otherwise a search by the cleaned title, preferring exact
//! normalized names, the game's platform and main games over DLC. A non-matching name is never
//! picked automatically.
//!
//! Results are cached per game in `game_metadata` (a "no match" result too) and refreshed in
//! the background after [`METADATA_TTL_MS`]. `igdb_get` answers from the cache at once and
//! starts the fetch if needed; `igdb:metadata` (`{ gameId, changed }`) tells the page when it
//! is done. All text is sanitized to plain text with length caps before it is stored.

use crate::db::{self, CachedMetadata};
use crate::secrets;
use crate::state::AppState;
use crate::steamgriddb::steam_app_id;
use crate::types::{now_ms, Game};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const API_BASE: &str = "https://api.igdb.com/v4";
/// The only hosts this module talks to in production.
pub const HOSTS: [&str; 2] = ["id.twitch.tv", "api.igdb.com"];
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
/// Minimum gap between two IGDB requests (IGDB allows 4 per second).
pub const REQUEST_SPACING: Duration = Duration::from_millis(260);
/// Waits before retrying a request that got HTTP 429.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
/// A cached token is renewed this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// How long matched metadata stays fresh.
pub const METADATA_TTL_MS: i64 = 30 * 24 * 3_600_000;
/// How long a "no match" result stays fresh.
pub const MISS_TTL_MS: i64 = 7 * 24 * 3_600_000;
const FUTURE_SKEW_MS: i64 = 24 * 3_600_000;
/// After a failed background fetch, the same game is not retried automatically for this long.
const FAILURE_COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// After Twitch or IGDB rejects the credentials, no request is made for this long (or until
/// the credentials are saved or cleared, see [`IgdbState::reset`]).
const REJECTED_COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// A bulk fetch stops after this many network or server (5xx) failures in a row.
pub const MAX_CONSECUTIVE_FAILURES: usize = 5;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_NAME_CHARS: usize = 200;
const MAX_LIST: usize = 12;
const MAX_SEARCH_RESULTS: usize = 20;
const MAX_QUERY_CHARS: usize = 200;
/// IGDB's external source id (and legacy category) for Steam.
const STEAM_SOURCE: u32 = 1;
const NO_CREDENTIALS: &str =
    "Add your IGDB (Twitch) Client ID and Client Secret in Settings first.";
const WORKS: &str = "IGDB connection works.";

/// Fields fetched for the game page.
pub const GAME_FIELDS: &str = "name,summary,storyline,genres.name,involved_companies.company.name,involved_companies.developer,involved_companies.publisher,aggregated_rating,aggregated_rating_count,rating,rating_count,first_release_date,url,screenshots.image_id,artworks.image_id";
/// Fields of `game_time_to_beats` (seconds; `count` = number of submissions).
const TIME_TO_BEAT_FIELDS: &str = "hastily,normally,completely,count";
/// Version of the cached metadata JSON. Rows written with an older shape (without images or
/// time to beat) still load, but count as stale so the next view refetches them.
pub const CACHE_SHAPE: u32 = 2;
/// Screenshots plus artworks kept per game.
pub const MAX_IMAGES: usize = 8;
/// Screenshots kept before artworks fill the rest of [`MAX_IMAGES`].
const MAX_SCREENSHOTS_FIRST: usize = 5;
const MAX_IMAGE_ID_LEN: usize = 64;
/// The only host IGDB images are loaded from.
pub const IMAGE_HOST: &str = "images.igdb.com";
/// After trying to cache a game's images, they aren't tried again for this long.
const IMAGE_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// Longest time to beat accepted (seconds): 10,000 hours.
const MAX_TIME_TO_BEAT_SECONDS: u64 = 10_000 * 3600;
/// Fields fetched for search candidates; `game_type`/`category` mark main games vs DLC.
const SEARCH_FIELDS: &str =
    "name,first_release_date,platforms.abbreviation,game_type,category,version_parent";
/// Fallback when IGDB rejects one of the optional fields above.
const SEARCH_FIELDS_MINIMAL: &str = "name,first_release_date,platforms.abbreviation";

// ---------------------------------------------------------------------------
// Public data

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Rating {
    /// 0–100, rounded.
    pub score: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// Sanitized metadata for one game: what the page shows, and what `game_metadata.json` holds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameMetadata {
    pub igdb_id: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyline: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    /// IGDB `aggregated_rating` / `aggregated_rating_count` (critics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critic_rating: Option<Rating>,
    /// IGDB `rating` / `rating_count` (IGDB users).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_rating: Option<Rating>,
    /// First release, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<i64>,
    /// The IGDB page (https://www.igdb.com/games/...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Validated IGDB image ids (see [`valid_image_id`]); URLs are built from them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artworks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_beat: Option<TimeToBeat>,
    /// [`CACHE_SHAPE`] when written; 0 for rows from older versions.
    #[serde(default)]
    pub shape: u32,
}

/// IGDB `game_time_to_beats`, in seconds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimeToBeat {
    /// Main story ("hastily").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hastily: Option<u64>,
    /// Main story plus extras ("normally").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normally: Option<u64>,
    /// Completionist ("completely").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completely: Option<u64>,
    /// Number of submissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

impl TimeToBeat {
    fn cleaned(self) -> Option<Self> {
        let ok = |s: Option<u64>| s.filter(|s| (1..=MAX_TIME_TO_BEAT_SECONDS).contains(s));
        let t = Self {
            hastily: ok(self.hastily),
            normally: ok(self.normally),
            completely: ok(self.completely),
            count: self.count.filter(|c| *c > 0),
        };
        (t.hastily.is_some() || t.normally.is_some() || t.completely.is_some()).then_some(t)
    }
}

/// One picture for the About section: `thumb` and `full` are a cached `lobby-cover://` value
/// once downloaded, otherwise the https URL on [`IMAGE_HOST`].
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageView {
    /// `"screenshot"` or `"artwork"`.
    pub kind: &'static str,
    pub thumb: String,
    pub full: String,
}

/// Time to beat as shown on the page ("42½ h"); absent values are left out.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimeToBeatView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_story: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_extras: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completionist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

impl GameMetadata {
    /// Re-applies the sanitizing rules (used on values read back from the database).
    fn cleaned(self) -> Option<Self> {
        Some(Self {
            igdb_id: self.igdb_id,
            name: clean_name(&self.name)?,
            summary: self.summary.and_then(|s| clean_text(&s, MAX_TEXT_BYTES)),
            storyline: self.storyline.and_then(|s| clean_text(&s, MAX_TEXT_BYTES)),
            genres: clean_list(self.genres),
            developers: clean_list(self.developers),
            publishers: clean_list(self.publishers),
            critic_rating: self.critic_rating.filter(|r| r.score <= 100),
            user_rating: self.user_rating.filter(|r| r.score <= 100),
            release_date: self.release_date,
            url: self.url.and_then(|u| igdb_page_url(&u)),
            screenshots: clean_image_ids(self.screenshots, MAX_IMAGES),
            artworks: clean_image_ids(self.artworks, MAX_IMAGES),
            time_to_beat: self.time_to_beat.and_then(TimeToBeat::cleaned),
            shape: self.shape,
        })
    }

    /// Up to [`MAX_IMAGES`]: screenshots first (at most [`MAX_SCREENSHOTS_FIRST`] while
    /// artworks are available), then artworks, then more screenshots.
    pub fn image_ids(&self) -> Vec<(&'static str, &str)> {
        let shots: Vec<&str> = self.screenshots.iter().map(String::as_str).collect();
        let arts: Vec<&str> = self.artworks.iter().map(String::as_str).collect();
        let first = shots.len().min(if arts.is_empty() {
            MAX_IMAGES
        } else {
            MAX_SCREENSHOTS_FIRST
        });
        let mut out: Vec<(&'static str, &str)> = shots[..first]
            .iter()
            .map(|id| ("screenshot", *id))
            .collect();
        out.extend(arts.iter().map(|id| ("artwork", *id)));
        out.extend(shots[first..].iter().map(|id| ("screenshot", *id)));
        out.truncate(MAX_IMAGES);
        out
    }
}

// ---------------------------------------------------------------------------
// Images and time to beat (pure)

/// IGDB image ids are short lowercase letters and digits; anything else is refused, so an id
/// can never change the path or host of the URL built from it.
pub fn valid_image_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_IMAGE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn clean_image_ids(ids: Vec<String>, max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| valid_image_id(id) && seen.insert(id.clone()))
        .take(max)
        .collect()
}

/// Image sizes used by the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    /// `t_screenshot_med` (569x320) for the strip.
    Thumb,
    /// `t_1080p` (1920x1080) for the lightbox.
    Full,
}

/// `https://images.igdb.com/igdb/image/upload/<size>/<id>.jpg`, built only from a valid id.
pub fn image_url(id: &str, size: ImageSize) -> Option<String> {
    if !valid_image_id(id) {
        return None;
    }
    let size = match size {
        ImageSize::Thumb => "t_screenshot_med",
        ImageSize::Full => "t_1080p",
    };
    Some(format!(
        "https://{IMAGE_HOST}/igdb/image/upload/{size}/{id}.jpg"
    ))
}

/// Every image URL (both sizes) the metadata refers to.
pub fn image_urls(metadata: &GameMetadata) -> Vec<String> {
    metadata
        .image_ids()
        .into_iter()
        .flat_map(|(_, id)| {
            [
                image_url(id, ImageSize::Thumb),
                image_url(id, ImageSize::Full),
            ]
        })
        .flatten()
        .collect()
}

/// Image URLs referenced by any cached metadata (kept by the cover cache sweep).
pub fn referenced_image_urls(conn: &rusqlite::Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT json FROM game_metadata WHERE json IS NOT NULL")?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .iter()
        .filter_map(|j| serde_json::from_str::<GameMetadata>(j).ok())
        .filter_map(GameMetadata::cleaned)
        .flat_map(|m| image_urls(&m))
        .collect())
}

/// `image_id`s from `screenshots` / `artworks` (objects with `image_id`), validated.
fn image_ids_field(value: Option<&Value>) -> Vec<String> {
    let ids = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| str_field(item, "image_id"));
    clean_image_ids(ids.collect(), MAX_IMAGES)
}

pub fn query_time_to_beat(igdb_id: u64) -> String {
    format!("fields {TIME_TO_BEAT_FIELDS}; where game_id = {igdb_id}; limit 1;")
}

/// `POST /game_time_to_beats` → the first entry with any time; missing values are left out.
pub fn parse_time_to_beat(body: &str) -> Result<Option<TimeToBeat>, IgdbError> {
    Ok(json_array(body)?.iter().find_map(|item| {
        let secs = |key: &str| item.get(key).and_then(Value::as_u64);
        TimeToBeat {
            hastily: secs("hastily"),
            normally: secs("normally"),
            completely: secs("completely"),
            count: item
                .get("count")
                .and_then(Value::as_u64)
                .map(|c| c.min(u32::MAX as u64) as u32),
        }
        .cleaned()
    }))
}

/// Hours rounded to the nearest half: "42½ h", "3 h", "½ h".
pub fn hours_label(seconds: u64) -> String {
    let halves = ((seconds as f64) / 1800.0).round().max(1.0) as u64;
    let (whole, half) = (halves / 2, halves % 2 == 1);
    match (whole, half) {
        (0, _) => "½ h".into(),
        (w, true) => format!("{w}½ h"),
        (w, false) => format!("{w} h"),
    }
}

fn time_to_beat_view(t: &TimeToBeat) -> TimeToBeatView {
    TimeToBeatView {
        main_story: t.hastily.map(hours_label),
        main_extras: t.normally.map(hours_label),
        completionist: t.completely.map(hours_label),
        count: t.count,
    }
}

/// The page's pictures, each URL replaced by `resolve(url)` (the cached local value when there
/// is one).
pub fn image_views(metadata: &GameMetadata, resolve: &dyn Fn(&str) -> String) -> Vec<ImageView> {
    metadata
        .image_ids()
        .into_iter()
        .filter_map(|(kind, id)| {
            Some(ImageView {
                kind,
                thumb: resolve(&image_url(id, ImageSize::Thumb)?),
                full: resolve(&image_url(id, ImageSize::Full)?),
            })
        })
        .collect()
}

/// One candidate in the "Search IGDB" picker.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub igdb_id: u64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<i64>,
    pub platforms: Vec<String>,
    /// False for DLC, expansions, bundles and the like (when IGDB says so).
    pub main_game: bool,
}

/// What `igdb_get` returns. `configured` is false when no credentials are saved; `pending`
/// is true while a fetch for this game runs (an `igdb:metadata` event follows).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataView {
    pub configured: bool,
    pub pending: bool,
    /// A lookup has completed for this game (with or without a match).
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<GameMetadata>,
    /// `"auto"` or `"user"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    /// Screenshots and artworks, cached copies once downloaded.
    #[serde(default)]
    pub images: Vec<ImageView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_beat: Option<TimeToBeatView>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub client_id_set: bool,
    pub client_secret_set: bool,
}

/// Result of `igdb_test`; `message` never contains a credential or token.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

/// `igdb:progress` payload and `igdb_fetch_all` result.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BulkProgress {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub updated: usize,
    pub not_found: usize,
    pub failed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MetadataEvent<'a> {
    game_id: &'a str,
    changed: bool,
}

// ---------------------------------------------------------------------------
// Errors

#[derive(Debug, Clone, PartialEq)]
pub enum IgdbError {
    /// The token endpoint rejected the Client ID / Client Secret.
    BadCredentials,
    /// IGDB rejected a fresh token.
    Unauthorized,
    Forbidden,
    RateLimited,
    /// IGDB rejected the query (HTTP 400).
    BadRequest,
    Http(u16),
    Network(String),
    Invalid(&'static str),
}

impl fmt::Display for IgdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadCredentials => f.write_str(
                "Twitch rejected the IGDB Client ID or Client Secret. Check them in Settings.",
            ),
            Self::Unauthorized => {
                f.write_str("IGDB rejected the access token. Check the Client ID in Settings.")
            }
            Self::Forbidden => f.write_str("IGDB denied access to this request."),
            Self::RateLimited => {
                f.write_str("IGDB is limiting requests right now; try again in a minute.")
            }
            Self::BadRequest => f.write_str("IGDB did not accept the request."),
            Self::Http(code) => write!(f, "IGDB returned HTTP {code}."),
            Self::Network(msg) => write!(f, "Could not reach IGDB: {msg}"),
            Self::Invalid(msg) => write!(f, "IGDB returned an unexpected response: {msg}"),
        }
    }
}

impl std::error::Error for IgdbError {}

// ---------------------------------------------------------------------------
// Text sanitizing (pure)

/// Bidirectional controls and invisible characters that could disguise text.
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

/// Plain multi-line text: line breaks kept (at most one blank line in a row), other control
/// and bidi characters dropped, at most `max_bytes` bytes. `None` when nothing is left.
pub fn clean_text(raw: &str, max_bytes: usize) -> Option<String> {
    let normalized = raw
        .replace("\r\n", "\n")
        .replace(['\r', '\u{2028}', '\u{2029}', '\u{85}'], "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut blank = false;
    for line in normalized.split('\n') {
        let line: String = line
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control() && !is_hidden(*c))
            .collect();
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank = !lines.is_empty();
            continue;
        }
        if blank {
            lines.push(String::new());
            blank = false;
        }
        lines.push(line);
        if lines.iter().map(|l| l.len() + 1).sum::<usize>() > max_bytes + 4 {
            break;
        }
    }
    let text = lines.join("\n");
    (!text.is_empty()).then(|| truncate_bytes(&text, max_bytes))
}

/// A single-line name: no control or bidi characters, whitespace collapsed, at most
/// [`MAX_NAME_CHARS`] characters.
pub fn clean_name(raw: &str) -> Option<String> {
    let line: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !is_hidden(*c))
        .collect();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let line: String = line.chars().take(MAX_NAME_CHARS).collect();
    let line = line.trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Cleaned names, empty and duplicate ones (ignoring case) dropped, at most [`MAX_LIST`].
fn clean_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter_map(|s| clean_name(&s))
        .filter(|s| seen.insert(s.to_lowercase()))
        .take(MAX_LIST)
        .collect()
}

/// Only `https://igdb.com/...` or `https://www.igdb.com/...` without credentials or a port.
pub fn igdb_page_url(raw: &str) -> Option<String> {
    if raw.len() > 512 {
        return None;
    }
    let url = url::Url::parse(raw.trim()).ok()?;
    let host_ok = matches!(url.host_str(), Some("igdb.com" | "www.igdb.com"));
    (url.scheme() == "https"
        && host_ok
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none())
    .then(|| url.to_string())
}

// ---------------------------------------------------------------------------
// Titles and queries (pure)

static BRACKETS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\([^()]*\)|\[[^\[\]]*\]|\{[^{}]*\}").unwrap());
static DEMO: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:[\s:\-–—]+|^)(?:free\s+)?demo\b").unwrap());
static EDITION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)[\s:\-–—,]+(?:the\s+)?(?:(?:game\s+of\s+the\s+year|goty|definitive|complete|deluxe|digital\s+deluxe|ultimate|gold|premium|standard|enhanced|special|anniversary|legendary|collector'?s|launch|day\s+one)\s+edition|goty|edition)\s*$",
    )
    .unwrap()
});

/// The title without trademark signs, bracketed tags (`(USA)`, `[v1.02]`), "Demo" and
/// edition suffixes. `None` when nothing searchable is left.
pub fn clean_title(title: &str) -> Option<String> {
    let base: String = title
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©') && !is_hidden(*c))
        .map(|c| if c.is_control() || c == '_' { ' ' } else { c })
        .collect();
    let mut text = BRACKETS.replace_all(&base, " ").into_owned();
    text = DEMO.replace_all(&text, " ").into_owned();
    // Editions can stack ("Gold Edition - GOTY"); strip a couple of times.
    for _ in 0..3 {
        let next = EDITION.replace(&text, "").into_owned();
        if next == text {
            break;
        }
        text = next;
    }
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cleaned = collapse(&text)
        .trim_matches(|c: char| c == '-' || c == ':' || c == ',' || c.is_whitespace())
        .to_string();
    if cleaned.is_empty() {
        // Everything was a tag: fall back to the title with only the symbols removed.
        cleaned = collapse(&base);
    }
    let cleaned: String = cleaned.chars().take(MAX_QUERY_CHARS).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Lower-case letters and digits only, for comparing names.
pub fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Text that is safe inside an Apicalypse string literal: double quotes and backslashes (the
/// only characters that could end or escape the literal) and control characters are replaced
/// by spaces, so the input can never add clauses; capped at 200 characters.
pub fn apicalypse_text(raw: &str) -> Option<String> {
    let text: String = raw
        .chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() || is_hidden(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let text: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

pub fn query_by_id(id: u64) -> String {
    format!("fields {GAME_FIELDS}; where id = {id}; limit 1;")
}

/// The `external_games` field that holds the source: `external_game_source` today,
/// `category` before IGDB renamed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceField {
    ExternalGameSource,
    Category,
}

impl SourceField {
    const ALL: [Self; 2] = [Self::ExternalGameSource, Self::Category];

    fn name(self) -> &'static str {
        match self {
            Self::ExternalGameSource => "external_game_source",
            Self::Category => "category",
        }
    }
}

/// `external_games` lookup of a Steam app id (digits only, else `None`).
pub fn query_steam_external(app_id: &str, field: SourceField) -> Option<String> {
    if app_id.is_empty() || app_id.len() > 12 || !app_id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "fields game; where uid = \"{app_id}\" & {} = {STEAM_SOURCE}; limit 5;",
        field.name()
    ))
}

pub fn query_search(text: &str, minimal: bool) -> Option<String> {
    let text = apicalypse_text(text)?;
    let fields = if minimal {
        SEARCH_FIELDS_MINIMAL
    } else {
        SEARCH_FIELDS
    };
    Some(format!(
        "search \"{text}\"; fields {fields}; limit {MAX_SEARCH_RESULTS};"
    ))
}

/// IGDB platform id for a catalog system id (`crate::catalog`), for the common systems.
pub fn igdb_platform(system: &str) -> Option<u64> {
    Some(match system {
        "pc" => 6,
        "ps1" => 7,
        "ps2" => 8,
        "ps3" => 9,
        "ps4" => 48,
        "psp" => 38,
        "psvita" => 46,
        "gc" => 21,
        "wii" => 5,
        "wiiu" => 41,
        "switch" => 130,
        "n3ds" => 37,
        "nds" => 20,
        "gba" => 24,
        "gb" => 33,
        "n64" => 4,
        "snes" => 19,
        "nes" => 18,
        "genesis" => 29,
        "dreamcast" => 23,
        "saturn" => 32,
        "xbox" => 11,
        "xbox360" => 12,
        "arcade" => 52,
        _ => return None,
    })
}

/// The game's platform: a ROM route's system, or PC for store and PC routes.
pub fn platform_for(game: &Game) -> Option<u64> {
    if let Some(system) = game.routes.iter().find_map(|r| r.system.as_deref()) {
        return igdb_platform(system);
    }
    game.routes
        .iter()
        .any(|r| r.store != "Emulator" && r.source_id != "rom")
        .then_some(6)
}

// ---------------------------------------------------------------------------
// Response parsing (pure)

fn as_id(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().filter(|id| *id > 0),
        Value::Object(map) => map.get("id").and_then(as_id),
        _ => None,
    }
}

fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Rounds a 0–100 score; `None` when missing or not a number.
pub fn rating(score: Option<&Value>, count: Option<&Value>) -> Option<Rating> {
    let score = score.and_then(Value::as_f64).filter(|s| s.is_finite())?;
    let count = count
        .and_then(Value::as_u64)
        .filter(|c| *c > 0)
        .map(|c| c.min(u32::MAX as u64) as u32);
    Some(Rating {
        score: score.round().clamp(0.0, 100.0) as u8,
        count,
    })
}

/// Developer and publisher names from `involved_companies`.
pub fn companies(value: Option<&Value>) -> (Vec<String>, Vec<String>) {
    let mut developers = Vec::new();
    let mut publishers = Vec::new();
    for item in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = item
            .get("company")
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if item.get("developer").and_then(Value::as_bool) == Some(true) {
            developers.push(name.to_string());
        }
        if item.get("publisher").and_then(Value::as_bool) == Some(true) {
            publishers.push(name.to_string());
        }
    }
    (clean_list(developers), clean_list(publishers))
}

fn release_ms(value: Option<&Value>) -> Option<i64> {
    // Seconds; anything outside roughly 1900..5000 is ignored.
    value
        .and_then(Value::as_i64)
        .filter(|s| (-2_300_000_000..100_000_000_000).contains(s))
        .map(|s| s * 1000)
}

/// One game object from IGDB; `None` without a valid id and name.
pub fn parse_game(value: &Value) -> Option<GameMetadata> {
    let igdb_id = value.get("id").and_then(as_id)?;
    let name = clean_name(value.get("name")?.as_str()?)?;
    let genres = value
        .get("genres")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| str_field(g, "name"));
    let (developers, publishers) = companies(value.get("involved_companies"));
    Some(GameMetadata {
        igdb_id,
        name,
        summary: str_field(value, "summary").and_then(|s| clean_text(&s, MAX_TEXT_BYTES)),
        storyline: str_field(value, "storyline").and_then(|s| clean_text(&s, MAX_TEXT_BYTES)),
        genres: clean_list(genres),
        developers,
        publishers,
        critic_rating: rating(
            value.get("aggregated_rating"),
            value.get("aggregated_rating_count"),
        ),
        user_rating: rating(value.get("rating"), value.get("rating_count")),
        release_date: release_ms(value.get("first_release_date")),
        url: str_field(value, "url").and_then(|u| igdb_page_url(&u)),
        screenshots: image_ids_field(value.get("screenshots")),
        artworks: image_ids_field(value.get("artworks")),
        time_to_beat: None,
        shape: CACHE_SHAPE,
    })
}

fn json_array(body: &str) -> Result<Vec<Value>, IgdbError> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(IgdbError::Invalid("expected a list.")),
        Err(_) => Err(IgdbError::Invalid("the response is not valid JSON.")),
    }
}

/// `POST /games` with [`query_by_id`] → the first valid game.
pub fn parse_games(body: &str) -> Result<Option<GameMetadata>, IgdbError> {
    Ok(json_array(body)?.iter().find_map(parse_game))
}

/// `POST /external_games` → the first linked game id.
pub fn parse_external(body: &str) -> Result<Option<u64>, IgdbError> {
    Ok(json_array(body)?
        .iter()
        .find_map(|item| item.get("game").and_then(as_id)))
}

/// A search candidate with what ranking needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: u64,
    pub name: String,
    pub platforms: Vec<u64>,
    pub platform_names: Vec<String>,
    /// `game_type` (or legacy `category`); 0 is a main game.
    pub kind: Option<u64>,
    pub version_parent: bool,
    pub release_date: Option<i64>,
}

impl Candidate {
    /// Main game, remake, remaster, expanded game, port (IGDB game types 0, 8, 9, 10, 11).
    fn is_main(&self) -> bool {
        self.kind.is_none_or(|k| matches!(k, 0 | 8 | 9 | 10 | 11)) && !self.version_parent
    }
}

fn kind_of(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::Object(map) => map.get("id").and_then(Value::as_u64),
        _ => None,
    }
}

pub fn parse_candidates(body: &str) -> Result<Vec<Candidate>, IgdbError> {
    Ok(json_array(body)?
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(as_id)?;
            let name = clean_name(item.get("name")?.as_str()?)?;
            let platforms = item.get("platforms").and_then(Value::as_array);
            Some(Candidate {
                id,
                name,
                platforms: platforms.into_iter().flatten().filter_map(as_id).collect(),
                platform_names: clean_list(
                    platforms
                        .into_iter()
                        .flatten()
                        .filter_map(|p| str_field(p, "abbreviation")),
                ),
                kind: item
                    .get("game_type")
                    .and_then(kind_of)
                    .or_else(|| item.get("category").and_then(kind_of)),
                version_parent: item.get("version_parent").and_then(as_id).is_some(),
                release_date: release_ms(item.get("first_release_date")),
            })
        })
        .collect())
}

/// 4: equals the original title; 3: equals the cleaned title; 1: one contains the other;
/// 0: unrelated. `wanted` is `[original, cleaned]` (normalized, possibly equal).
fn name_score(candidate: &str, wanted: &[String]) -> u32 {
    let name = normalize(candidate);
    if name.is_empty() {
        return 0;
    }
    if wanted.first() == Some(&name) {
        return 4;
    }
    if wanted.contains(&name) {
        return 3;
    }
    let contains = |a: &str, b: &str| b.chars().count() >= 4 && a.contains(b);
    if wanted
        .iter()
        .any(|w| !w.is_empty() && (contains(w, &name) || contains(&name, w)))
    {
        1
    } else {
        0
    }
}

/// The best candidate for a title: exact normalized names first, then the platform, main
/// games over DLC and editions, then IGDB's order. Candidates whose name neither equals nor
/// contains the title are never picked.
pub fn best_match(
    original: &str,
    cleaned: &str,
    platform: Option<u64>,
    candidates: &[Candidate],
) -> Option<u64> {
    let symbols: String = original
        .chars()
        .filter(|c| !matches!(c, '™' | '®' | '©'))
        .collect();
    let wanted = vec![
        normalize(&BRACKETS.replace_all(&symbols, " ")),
        normalize(cleaned),
    ];
    if wanted.iter().all(|w| w.is_empty()) {
        return None;
    }
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, c)| {
            let name = name_score(&c.name, &wanted);
            if name == 0 {
                return None;
            }
            let mut score = name as i64 * 100;
            if platform.is_some_and(|p| c.platforms.contains(&p)) {
                score += 30;
            }
            score += if c.is_main() { 20 } else { -20 };
            Some((score, std::cmp::Reverse(index), c.id))
        })
        .max()
        .map(|(_, _, id)| id)
}

impl From<&Candidate> for SearchResult {
    fn from(c: &Candidate) -> Self {
        Self {
            igdb_id: c.id,
            name: c.name.clone(),
            release_date: c.release_date,
            platforms: c.platform_names.clone(),
            main_game: c.is_main(),
        }
    }
}

// ---------------------------------------------------------------------------
// Token request and cache

/// The token request body: `client_id`, `client_secret` and `grant_type`, form-encoded. Sent
/// as the POST body, never in the URL.
pub fn token_form(client_id: &str, client_secret: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id)
        .append_pair("client_secret", client_secret)
        .append_pair("grant_type", "client_credentials")
        .finish()
}

/// `{ access_token, expires_in, token_type }` → token and lifetime (1 minute to 90 days).
pub fn parse_token(body: &str) -> Result<(String, Duration), IgdbError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| IgdbError::Invalid("the token response is not valid JSON."))?;
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && t.len() <= 512 && t.chars().all(|c| c.is_ascii_graphic()))
        .ok_or(IgdbError::Invalid(
            "the token response has no usable token.",
        ))?;
    let seconds = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600)
        .clamp(60, 90 * 24 * 3600);
    Ok((token.to_string(), Duration::from_secs(seconds)))
}

/// Intentionally not `Debug`: it holds the access token.
struct CachedToken {
    client_id: String,
    token: String,
    expires_at: Instant,
}

/// The access token, in memory only. Never persisted.
#[derive(Default)]
pub struct TokenCache {
    inner: StdMutex<Option<CachedToken>>,
}

impl TokenCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<CachedToken>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The cached token for `client_id` unless it expires within [`TOKEN_MARGIN`] of `now`.
    pub fn get(&self, client_id: &str, now: Instant) -> Option<String> {
        self.lock()
            .as_ref()
            .filter(|t| t.client_id == client_id && now + TOKEN_MARGIN < t.expires_at)
            .map(|t| t.token.clone())
    }

    pub fn set(&self, client_id: &str, token: String, lifetime: Duration, now: Instant) {
        *self.lock() = Some(CachedToken {
            client_id: client_id.to_string(),
            token,
            expires_at: now + lifetime,
        });
    }

    pub fn clear(&self) {
        *self.lock() = None;
    }

    /// The cached token whatever its client or expiry (for redacting messages only).
    fn raw(&self) -> Option<String> {
        self.lock().as_ref().map(|t| t.token.clone())
    }
}

/// Spaces requests at least `spacing` apart; a 429 pushes the next slot further out.
#[derive(Default)]
pub struct RateLimiter {
    next: tokio::sync::Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub async fn wait(&self, spacing: Duration) {
        let mut next = self.next.lock().await;
        if let Some(at) = *next {
            let now = Instant::now();
            if at > now {
                tokio::time::sleep(at - now).await;
            }
        }
        *next = Some(Instant::now() + spacing);
    }

    async fn delay(&self, by: Duration) {
        let mut next = self.next.lock().await;
        let at = Instant::now() + by;
        if next.is_none_or(|n| n < at) {
            *next = Some(at);
        }
    }
}

/// IGDB state shared by all requests (part of [`AppState`]).
#[derive(Default)]
pub struct IgdbState {
    pub token: TokenCache,
    limiter: RateLimiter,
    /// Serializes token requests so concurrent lookups share one.
    token_fetch: tokio::sync::Mutex<()>,
    /// Games with a fetch in progress.
    in_flight: StdMutex<HashSet<String>>,
    /// Games whose last background fetch failed, and when.
    failed: StdMutex<HashMap<String, Instant>>,
    bulk_running: AtomicBool,
    bulk_cancel: AtomicBool,
    progress: StdMutex<BulkProgress>,
    /// When the credentials were last rejected, and how (see [`REJECTED_COOLDOWN`]).
    rejected: StdMutex<Option<(Instant, IgdbError)>>,
    /// Games whose images were last tried, and when (see [`IMAGE_RETRY_AFTER`]).
    images_tried: StdMutex<HashMap<String, Instant>>,
}

impl IgdbState {
    /// Marks a game as being fetched; false when it already is.
    fn begin(&self, game_id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(game_id.to_string())
    }

    fn end(&self, game_id: &str) {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(game_id);
    }

    fn is_in_flight(&self, game_id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(game_id)
    }

    fn recently_failed(&self, game_id: &str) -> bool {
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(game_id)
            .is_some_and(|at| at.elapsed() < FAILURE_COOLDOWN)
    }

    fn set_failed(&self, game_id: &str, failed: bool) {
        let mut map = self.failed.lock().unwrap_or_else(|p| p.into_inner());
        if failed {
            map.insert(game_id.to_string(), Instant::now());
        } else {
            map.remove(game_id);
        }
    }

    /// The rejection that still blocks requests, if any.
    fn rejection(&self, now: Instant) -> Option<IgdbError> {
        self.rejected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .filter(|(at, _)| now.duration_since(*at) < REJECTED_COOLDOWN)
            .map(|(_, e)| e.clone())
    }

    fn set_rejected(&self, error: IgdbError, now: Instant) {
        self.token.clear();
        *self.rejected.lock().unwrap_or_else(|p| p.into_inner()) = Some((now, error));
    }

    /// Forgets the token, the credential rejection and failure cooldowns (credentials changed).
    pub fn reset(&self) {
        self.token.clear();
        *self.rejected.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

// ---------------------------------------------------------------------------
// HTTP

/// Where requests go. Production is fixed to [`HOSTS`] over https; tests use a local server.
#[derive(Clone, Debug)]
pub struct Endpoints {
    token: url::Url,
    api: url::Url,
    https_only: bool,
    retry_delays: Vec<Duration>,
    spacing: Duration,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            token: url::Url::parse(TOKEN_URL).expect("valid token URL"),
            api: url::Url::parse(API_BASE).expect("valid API base"),
            https_only: true,
            retry_delays: RETRY_DELAYS.to_vec(),
            spacing: REQUEST_SPACING,
        }
    }

    #[cfg(test)]
    fn local(base: &str) -> Self {
        Self {
            token: url::Url::parse(&format!("{base}/oauth2/token")).unwrap(),
            api: url::Url::parse(&format!("{base}/v4")).unwrap(),
            https_only: false,
            retry_delays: vec![Duration::from_millis(10), Duration::from_millis(10)],
            spacing: Duration::from_millis(1),
        }
    }

    fn api_url(&self, endpoint: &str) -> url::Url {
        let mut url = self.api.clone();
        url.path_segments_mut()
            .expect("API base has a path")
            .push(endpoint);
        url
    }
}

/// Holds the credentials for one operation; intentionally not `Debug`.
pub struct Api<'a> {
    http: reqwest::Client,
    endpoints: Endpoints,
    client_id: String,
    secret: String,
    state: &'a IgdbState,
}

async fn read_capped(mut response: reqwest::Response) -> Result<String, IgdbError> {
    if response
        .content_length()
        .is_some_and(|len| len as usize > MAX_RESPONSE_BYTES)
    {
        return Err(IgdbError::Invalid("the response is too large."));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| IgdbError::Network(e.without_url().to_string()))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(IgdbError::Invalid("the response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| IgdbError::Invalid("the response is not text."))
}

impl<'a> Api<'a> {
    pub fn new(
        endpoints: Endpoints,
        client_id: String,
        secret: String,
        state: &'a IgdbState,
    ) -> Result<Self> {
        if endpoints.https_only {
            let allowed = |u: &url::Url| {
                u.scheme() == "https" && u.host_str().is_some_and(|h| HOSTS.contains(&h))
            };
            if !allowed(&endpoints.token) || !allowed(&endpoints.api) {
                bail!("IGDB requests may only go to id.twitch.tv and api.igdb.com.");
            }
        }
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(endpoints.https_only)
            .user_agent(concat!("GameLobby/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| anyhow!("Could not start the IGDB client: {}", e.without_url()))?;
        Ok(Self {
            http,
            endpoints,
            client_id,
            secret,
            state,
        })
    }

    /// Removes the secret, Client ID and any token from a message, should one ever echo them.
    fn redact(&self, message: String) -> String {
        let mut message = message;
        let token = self.state.token.raw();
        for value in [Some(self.secret.as_str()), token.as_deref()]
            .into_iter()
            .flatten()
        {
            if value.len() >= 4 {
                message = message.replace(value, "[redacted]");
            }
        }
        if self.client_id.len() >= 4 {
            message = message.replace(&self.client_id, "[client id]");
        }
        message
    }

    fn network(&self, error: reqwest::Error) -> IgdbError {
        IgdbError::Network(self.redact(error.without_url().to_string()))
    }

    async fn fetch_token(&self) -> Result<(String, Duration), IgdbError> {
        self.state.limiter.wait(self.endpoints.spacing).await;
        let response = self
            .http
            .post(self.endpoints.token.clone())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(token_form(&self.client_id, &self.secret))
            .send()
            .await
            .map_err(|e| self.network(e))?;
        match response.status().as_u16() {
            200..=299 => parse_token(&read_capped(response).await?),
            400 | 401 | 403 => Err(IgdbError::BadCredentials),
            429 => Err(IgdbError::RateLimited),
            code => Err(IgdbError::Http(code)),
        }
    }

    /// A valid access token: the cached one, or a new one.
    async fn token(&self) -> Result<String, IgdbError> {
        if let Some(rejected) = self.state.rejection(Instant::now()) {
            return Err(rejected);
        }
        if let Some(token) = self.state.token.get(&self.client_id, Instant::now()) {
            return Ok(token);
        }
        let _guard = self.state.token_fetch.lock().await;
        if let Some(rejected) = self.state.rejection(Instant::now()) {
            return Err(rejected);
        }
        if let Some(token) = self.state.token.get(&self.client_id, Instant::now()) {
            return Ok(token);
        }
        let (token, lifetime) = match self.fetch_token().await {
            Ok(fetched) => fetched,
            Err(IgdbError::BadCredentials) => {
                self.state
                    .set_rejected(IgdbError::BadCredentials, Instant::now());
                return Err(IgdbError::BadCredentials);
            }
            Err(e) => return Err(e),
        };
        self.state
            .token
            .set(&self.client_id, token.clone(), lifetime, Instant::now());
        Ok(token)
    }

    /// `POST /v4/<endpoint>` with an Apicalypse body. A 401 renews the token once; a 429
    /// waits and retries.
    pub async fn post(&self, endpoint: &str, body: &str) -> Result<String, IgdbError> {
        let url = self.endpoints.api_url(endpoint);
        let mut renewed = false;
        let mut delays = self.endpoints.retry_delays.iter();
        loop {
            let token = self.token().await?;
            self.state.limiter.wait(self.endpoints.spacing).await;
            let response = self
                .http
                .post(url.clone())
                .header("Client-ID", &self.client_id)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::CONTENT_TYPE, "text/plain")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| self.network(e))?;
            match response.status().as_u16() {
                200..=299 => return read_capped(response).await,
                401 if !renewed => {
                    renewed = true;
                    self.state.token.clear();
                }
                401 => {
                    self.state
                        .set_rejected(IgdbError::Unauthorized, Instant::now());
                    return Err(IgdbError::Unauthorized);
                }
                429 => match delays.next() {
                    Some(delay) => self.state.limiter.delay(*delay).await,
                    None => return Err(IgdbError::RateLimited),
                },
                400 => return Err(IgdbError::BadRequest),
                403 => return Err(IgdbError::Forbidden),
                code => return Err(IgdbError::Http(code)),
            }
        }
    }

    /// Full metadata for one IGDB id, with its time to beat.
    pub async fn game(&self, igdb_id: u64) -> Result<Option<GameMetadata>, IgdbError> {
        let Some(mut metadata) = parse_games(&self.post("games", &query_by_id(igdb_id)).await?)?
        else {
            return Ok(None);
        };
        metadata.time_to_beat = self.time_to_beat(metadata.igdb_id).await?;
        Ok(Some(metadata))
    }

    /// Time to beat for one IGDB id. A query IGDB doesn't accept (an API change) or a bad entry
    /// just means none; network, limit and credential errors are passed on.
    pub async fn time_to_beat(&self, igdb_id: u64) -> Result<Option<TimeToBeat>, IgdbError> {
        match self
            .post("game_time_to_beats", &query_time_to_beat(igdb_id))
            .await
        {
            Ok(body) => Ok(parse_time_to_beat(&body).unwrap_or(None)),
            Err(IgdbError::BadRequest | IgdbError::Http(404)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The IGDB id linked to a Steam app id, trying both names of the source field.
    pub async fn steam_game(&self, app_id: &str) -> Result<Option<u64>, IgdbError> {
        for field in SourceField::ALL {
            let Some(query) = query_steam_external(app_id, field) else {
                return Ok(None);
            };
            match self.post("external_games", &query).await {
                Ok(body) => {
                    if let Some(id) = parse_external(&body)? {
                        return Ok(Some(id));
                    }
                }
                // Unknown field on this API version: try the other name.
                Err(IgdbError::BadRequest) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Search candidates for free text.
    pub async fn search(&self, text: &str) -> Result<Vec<Candidate>, IgdbError> {
        let Some(query) = query_search(text, false) else {
            return Ok(Vec::new());
        };
        match self.post("games", &query).await {
            Ok(body) => parse_candidates(&body),
            Err(IgdbError::BadRequest) => {
                let query = query_search(text, true).expect("same text");
                parse_candidates(&self.post("games", &query).await?)
            }
            Err(e) => Err(e),
        }
    }

    /// Resolves a library game to an IGDB id: Steam app id first, then the cleaned title.
    pub async fn match_game(&self, game: &Game) -> Result<Option<u64>, IgdbError> {
        if let Some(app_id) = steam_app_id(game) {
            if let Some(id) = self.steam_game(app_id).await? {
                return Ok(Some(id));
            }
        }
        let Some(title) = clean_title(&game.title) else {
            return Ok(None);
        };
        let candidates = self.search(&title).await?;
        Ok(best_match(
            &game.title,
            &title,
            platform_for(game),
            &candidates,
        ))
    }
}

// ---------------------------------------------------------------------------
// Cache decisions (pure)

/// Whether a cache row can be used without refetching.
pub fn is_fresh(cached: &CachedMetadata, now: i64) -> bool {
    let ttl = if cached.igdb_id.is_some() {
        METADATA_TTL_MS
    } else {
        MISS_TTL_MS
    };
    let current_shape = cached.igdb_id.is_none() || cached_shape(cached) >= CACHE_SHAPE;
    current_shape && cached.fetched_at <= now + FUTURE_SKEW_MS && now - cached.fetched_at < ttl
}

/// The [`CACHE_SHAPE`] a cached row was written with (0 when unknown).
fn cached_shape(cached: &CachedMetadata) -> u32 {
    cached
        .json
        .as_deref()
        .and_then(|j| serde_json::from_str::<Value>(j).ok())
        .and_then(|v| v.get("shape").and_then(Value::as_u64))
        .map_or(0, |s| s.min(u32::MAX as u64) as u32)
}

/// How to refresh a game: a known id (user choice, or an earlier automatic match) is fetched
/// directly and never re-matched; otherwise the game is matched again.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    ById { igdb_id: u64, source: String },
    Match,
}

pub fn plan(cached: Option<&CachedMetadata>) -> Plan {
    match cached {
        Some(CachedMetadata {
            igdb_id: Some(id),
            source,
            ..
        }) if *id > 0 => Plan::ById {
            igdb_id: *id as u64,
            source: if source == "user" { "user" } else { "auto" }.into(),
        },
        _ => Plan::Match,
    }
}

fn entry(metadata: Option<&GameMetadata>, source: &str, now: i64) -> Result<CachedMetadata> {
    Ok(CachedMetadata {
        igdb_id: metadata.map(|m| m.igdb_id as i64),
        json: metadata.map(serde_json::to_string).transpose()?,
        fetched_at: now,
        source: source.to_string(),
    })
}

fn view(
    cached: Option<CachedMetadata>,
    configured: bool,
    pending: bool,
    resolve: &dyn Fn(&str) -> String,
) -> MetadataView {
    let Some(cached) = cached else {
        return MetadataView {
            configured,
            pending,
            ..Default::default()
        };
    };
    let metadata = cached
        .json
        .as_deref()
        .and_then(|j| serde_json::from_str::<GameMetadata>(j).ok())
        .and_then(GameMetadata::cleaned);
    MetadataView {
        configured,
        pending,
        checked: true,
        source: metadata.as_ref().map(|_| cached.source.clone()),
        images: metadata
            .as_ref()
            .map(|m| image_views(m, resolve))
            .unwrap_or_default(),
        time_to_beat: metadata
            .as_ref()
            .and_then(|m| m.time_to_beat.as_ref())
            .map(time_to_beat_view),
        metadata,
        fetched_at: Some(cached.fetched_at),
    }
}

/// Fetches and stores metadata for one game according to its cache row. Returns whether a
/// row was written (`Ok(false)` when a user choice made meanwhile took precedence).
pub async fn refresh_game(api: &Api<'_>, conn: &db::Db, game: &Game) -> Result<RefreshOutcome> {
    let cached = db::cached_metadata(&conn.lock(), &game.id)?;
    let (metadata, source) = match plan(cached.as_ref()) {
        Plan::ById { igdb_id, source } => match api.game(igdb_id).await? {
            Some(metadata) => (Some(metadata), source),
            // The user's game is gone from IGDB: keep their choice and what was cached.
            None if source == "user" => {
                return Ok(RefreshOutcome {
                    found: false,
                    changed: false,
                })
            }
            None => (None, source),
        },
        Plan::Match => match api.match_game(game).await? {
            Some(id) => (api.game(id).await?, "auto".to_string()),
            None => (None, "auto".to_string()),
        },
    };
    let found = metadata.is_some();
    let row = entry(metadata.as_ref(), &source, now_ms())?;
    let changed = db::put_metadata(&conn.lock(), &game.id, &row)?;
    Ok(RefreshOutcome { found, changed })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefreshOutcome {
    pub found: bool,
    pub changed: bool,
}

// ---------------------------------------------------------------------------
// Operations used by the commands

fn credentials(state: &AppState) -> Result<Option<(String, String)>> {
    secrets::get_igdb_credentials(state)
}

fn api<'a>(state: &'a AppState) -> Result<Api<'a>> {
    let (id, secret) = credentials(state)?.ok_or_else(|| anyhow!(NO_CREDENTIALS))?;
    Api::new(Endpoints::production(), id, secret, &state.igdb)
}

pub fn status(state: &AppState) -> Result<CredentialStatus> {
    Ok(CredentialStatus {
        client_id_set: secrets::has_igdb_client_id(state)?,
        client_secret_set: secrets::has_igdb_client_secret(state)?,
    })
}

fn configured(state: &AppState) -> bool {
    status(state).is_ok_and(|s| s.client_id_set && s.client_secret_set)
}

pub fn save_credentials(
    state: &AppState,
    client_id: &str,
    secret: &str,
) -> Result<CredentialStatus> {
    secrets::set_igdb_credentials(state, client_id, secret)?;
    state.igdb.reset();
    status(state)
}

pub fn clear_credentials(state: &AppState) -> Result<CredentialStatus> {
    secrets::clear_igdb_credentials(state)?;
    state.igdb.reset();
    status(state)
}

/// Requests a new token and makes one small query. Never fails; never echoes credentials.
pub async fn test(state: &AppState) -> TestResult {
    let fail = |message: String| TestResult { ok: false, message };
    let api = match api(state) {
        Ok(api) => api,
        Err(e) => return fail(e.to_string()),
    };
    state.igdb.token.clear();
    match api.post("games", "fields name; limit 1;").await {
        Ok(body) => match json_array(&body) {
            Ok(_) => TestResult {
                ok: true,
                message: WORKS.into(),
            },
            Err(e) => fail(api.redact(e.to_string())),
        },
        Err(e) => fail(api.redact(e.to_string())),
    }
}

fn emit_metadata(app: &AppHandle, game_id: &str, changed: bool) {
    let _ = app.emit("igdb:metadata", MetadataEvent { game_id, changed });
}

/// Fetches one game in the background (skipped when already running for it).
fn spawn_refresh(app: &AppHandle, game: Game) -> bool {
    let state = app.state::<AppState>();
    if !state.igdb.begin(&game.id) {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let result = async {
            let api = api(&state)?;
            refresh_game(&api, &state.db, &game).await
        }
        .await;
        state.igdb.end(&game.id);
        match result {
            Ok(outcome) => {
                state.igdb.set_failed(&game.id, false);
                emit_metadata(&app, &game.id, outcome.changed);
            }
            Err(e) => {
                state.igdb.set_failed(&game.id, true);
                log::warn!("IGDB metadata lookup failed: {e:#}");
                emit_metadata(&app, &game.id, false);
            }
        }
    });
    true
}

/// The cached metadata for a game, at once. Starts a background fetch when the cache is
/// missing or stale and credentials are saved.
pub fn get(app: &AppHandle, game_id: &str) -> Result<MetadataView> {
    let state = app.state::<AppState>();
    let (game, cached) = {
        let conn = state.db.lock();
        (
            db::get_game(&conn, game_id)?,
            db::cached_metadata(&conn, game_id)?,
        )
    };
    let Some(game) = game else {
        return Ok(MetadataView::default());
    };
    let configured = configured(&state);
    let stale = cached.as_ref().is_none_or(|c| !is_fresh(c, now_ms()));
    let mut pending = state.igdb.is_in_flight(game_id);
    if configured && stale && !pending && !state.igdb.recently_failed(game_id) {
        pending = spawn_refresh(app, game);
    }
    let view = resolved_view(&state, cached, configured, pending);
    spawn_image_cache(app, game_id, &view);
    Ok(view)
}

/// [`view`] with image URLs replaced by cached copies where they exist.
fn resolved_view(
    state: &AppState,
    cached: Option<CachedMetadata>,
    configured: bool,
    pending: bool,
) -> MetadataView {
    let conn = state.db.lock();
    let covers_dir = state.covers_dir();
    view(cached, configured, pending, &|url| {
        crate::covercache::cached_value(&conn, &covers_dir, url)
    })
}

/// Downloads the view's remote images into the cover cache in the background (at most once per
/// [`IMAGE_RETRY_AFTER`] per game), then tells the page to reload them.
fn spawn_image_cache(app: &AppHandle, game_id: &str, view: &MetadataView) {
    let remote: Vec<String> = view
        .images
        .iter()
        .flat_map(|i| [&i.thumb, &i.full])
        .filter(|v| crate::covercache::needs_caching(Some(v)))
        .cloned()
        .collect();
    if remote.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    {
        let mut tried = state
            .igdb
            .images_tried
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if tried
            .get(game_id)
            .is_some_and(|at| at.elapsed() < IMAGE_RETRY_AFTER)
        {
            return;
        }
        tried.insert(game_id.to_string(), Instant::now());
    }
    let app = app.clone();
    let game_id = game_id.to_string();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let mut cached_any = false;
        for url in remote {
            let Some(file) = crate::covercache::download_now(&state, &url).await else {
                continue;
            };
            let conn = state.db.lock();
            match crate::covercache::adopt(&conn, &state.covers_dir(), &url, Some(&file)) {
                Ok(value) if value != url => cached_any = true,
                Ok(_) => {}
                Err(e) => log::warn!("Could not record an IGDB image: {e:#}"),
            }
        }
        if cached_any {
            emit_metadata(&app, &game_id, true);
        }
    });
}

/// "Wrong game? Search IGDB": candidates for the game's cleaned title, or for `text`.
pub async fn search(
    state: &AppState,
    game_id: &str,
    text: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let game =
        db::get_game(&state.db.lock(), game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    let wanted = match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => apicalypse_text(text),
        None => clean_title(&game.title),
    };
    let Some(wanted) = wanted else {
        return Ok(Vec::new());
    };
    let api = api(state)?;
    let candidates = api
        .search(&wanted)
        .await
        .map_err(|e| anyhow!(api.redact(e.to_string())))?;
    Ok(candidates.iter().map(SearchResult::from).collect())
}

/// Renames the game to the IGDB game's name after a manual match. Returns whether the title
/// changed. Library scans never overwrite the title of a game that already exists, so the new
/// name survives later scans just like a rename made in the editor.
fn rename_to_match(conn: &rusqlite::Connection, game_id: &str, name: &str) -> Result<bool> {
    let Some(name) = clean_name(name) else {
        return Ok(false);
    };
    let Some(game) = db::get_game(conn, game_id)? else {
        return Ok(false);
    };
    if game.title == name {
        return Ok(false);
    }
    db::save_patch(
        conn,
        &crate::types::GamePatch {
            title: Some(name),
            ..crate::types::GamePatch::new(game_id.to_string())
        },
    )?;
    Ok(true)
}

/// Stores the user's choice of IGDB game (it always wins over automatic matches) and renames
/// the game to the IGDB name.
pub async fn choose(app: &AppHandle, game_id: &str, igdb_id: u64) -> Result<MetadataView> {
    if igdb_id == 0 || igdb_id > i64::MAX as u64 {
        bail!("That IGDB game id is not valid.");
    }
    let state = app.state::<AppState>();
    if db::get_game(&state.db.lock(), game_id)?.is_none() {
        bail!("Game not found.");
    }
    let api = api(&state)?;
    let metadata = api
        .game(igdb_id)
        .await
        .map_err(|e| anyhow!(api.redact(e.to_string())))?
        .ok_or_else(|| anyhow!("IGDB has no game with that id."))?;
    let row = entry(Some(&metadata), "user", now_ms())?;
    let renamed;
    let cached = {
        let conn = state.db.lock();
        if !db::put_metadata(&conn, game_id, &row)? {
            bail!("Game not found.");
        }
        renamed = rename_to_match(&conn, game_id, &metadata.name)?;
        db::cached_metadata(&conn, game_id)?
    };
    state.igdb.set_failed(game_id, false);
    state
        .igdb
        .images_tried
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(game_id);
    emit_metadata(app, game_id, true);
    if renamed {
        let _ = app.emit("library:changed", ());
    }
    let view = resolved_view(&state, cached, true, false);
    spawn_image_cache(app, game_id, &view);
    Ok(view)
}

/// Forgets the stored match (including a user choice) and matches the game again.
pub fn reset_match(app: &AppHandle, game_id: &str) -> Result<MetadataView> {
    let state = app.state::<AppState>();
    db::delete_metadata(&state.db.lock(), game_id)?;
    state.igdb.set_failed(game_id, false);
    emit_metadata(app, game_id, true);
    get(app, game_id)
}

fn set_progress(app: &AppHandle, state: &AppState, progress: &BulkProgress) {
    *state
        .igdb
        .progress
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = progress.clone();
    let _ = app.emit("igdb:progress", progress);
}

pub fn progress(state: &AppState) -> BulkProgress {
    state
        .igdb
        .progress
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

pub fn cancel_all(state: &AppState) {
    if state.igdb.bulk_running.load(Ordering::SeqCst) {
        state.igdb.bulk_cancel.store(true, Ordering::SeqCst);
    }
}

/// Starts fetching metadata for every game whose cache is missing or stale, in the
/// background (rate-limited like every request). Progress arrives as `igdb:progress`.
/// Whether a failure counts toward [`MAX_CONSECUTIVE_FAILURES`]: the network or IGDB itself
/// is failing (as opposed to one bad entry).
fn is_outage(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<IgdbError>(),
        Some(IgdbError::Network(_)) | Some(IgdbError::Http(500..=599))
    )
}

/// Fetches `games` one after another. `report(progress, changed_game)` is called after each
/// game (with the progress) and when a game's stored metadata changed. Stops when cancelled,
/// when the credentials are rejected or IGDB keeps limiting requests, or after
/// [`MAX_CONSECUTIVE_FAILURES`] network or server failures in a row; `message` says why.
pub async fn bulk_fetch(
    api: &Api<'_>,
    db: &db::Db,
    games: &[Game],
    start: BulkProgress,
    mut report: impl FnMut(Option<&BulkProgress>, Option<&str>),
) -> BulkProgress {
    let state = api.state;
    let mut progress = start;
    let mut outages = 0;
    for game in games {
        if state.bulk_cancel.load(Ordering::SeqCst) {
            progress.message = Some("Cancelled.".into());
            break;
        }
        if state.begin(&game.id) {
            let result = refresh_game(api, db, game).await;
            state.end(&game.id);
            match result {
                Ok(outcome) => {
                    outages = 0;
                    if outcome.found {
                        progress.updated += 1;
                    } else {
                        progress.not_found += 1;
                    }
                    if outcome.changed {
                        report(None, Some(&game.id));
                    }
                }
                Err(e) => {
                    progress.failed += 1;
                    let fatal = matches!(
                        e.downcast_ref::<IgdbError>(),
                        Some(
                            IgdbError::BadCredentials
                                | IgdbError::Unauthorized
                                | IgdbError::RateLimited
                        )
                    );
                    outages = if is_outage(&e) { outages + 1 } else { 0 };
                    if fatal || outages >= MAX_CONSECUTIVE_FAILURES {
                        let reason = api.redact(e.to_string());
                        progress.message = Some(if fatal {
                            reason
                        } else {
                            format!(
                                "Stopped after {MAX_CONSECUTIVE_FAILURES} failed requests in a row. {reason}"
                            )
                        });
                        progress.done += 1;
                        break;
                    }
                }
            }
        }
        progress.done += 1;
        report(Some(&progress), None);
    }
    progress
}

pub fn fetch_all(app: &AppHandle) -> Result<BulkProgress> {
    let state = app.state::<AppState>();
    if !configured(&state) {
        bail!(NO_CREDENTIALS);
    }
    if state.igdb.bulk_running.swap(true, Ordering::SeqCst) {
        bail!("An IGDB metadata fetch is already running.");
    }
    state.igdb.bulk_cancel.store(false, Ordering::SeqCst);
    let now = now_ms();
    let games: Vec<Game> = {
        let conn = state.db.lock();
        let games = match db::all_games(&conn) {
            Ok(games) => games,
            Err(e) => {
                state.igdb.bulk_running.store(false, Ordering::SeqCst);
                return Err(e);
            }
        };
        games
            .into_iter()
            .filter(|g| {
                db::cached_metadata(&conn, &g.id)
                    .ok()
                    .flatten()
                    .is_none_or(|c| !is_fresh(&c, now))
            })
            .collect()
    };
    let start = BulkProgress {
        running: true,
        total: games.len(),
        ..Default::default()
    };
    set_progress(app, &state, &start);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let mut progress = match api(&state) {
            Ok(api) => {
                bulk_fetch(&api, &state.db, &games, start, |progress, changed| {
                    if let Some(game_id) = changed {
                        emit_metadata(&app, game_id, true);
                    }
                    if let Some(progress) = progress {
                        set_progress(&app, &state, progress);
                    }
                })
                .await
            }
            Err(e) => BulkProgress {
                message: Some(e.to_string()),
                ..start
            },
        };
        progress.running = false;
        state.igdb.bulk_running.store(false, Ordering::SeqCst);
        state.igdb.bulk_cancel.store(false, Ordering::SeqCst);
        set_progress(&app, &state, &progress);
    });
    Ok(progress(&state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GameRoute;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn manual_match_renames_the_game_once_and_scans_keep_the_name() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let game = |title: &str| Game {
            id: "steam:1".into(),
            title: title.into(),
            store: "Steam".into(),
            routes: vec![GameRoute {
                source_id: "steam".into(),
                app_id: Some("1".into()),
                launch_target: "steam://rungameid/1".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        db::upsert_imported_games(&mut conn, &[game("ugly_name_v2")]).unwrap();
        assert!(rename_to_match(&conn, "steam:1", "Pretty Name").unwrap());
        // Same name again: nothing to do.
        assert!(!rename_to_match(&conn, "steam:1", "Pretty Name").unwrap());
        assert!(!rename_to_match(&conn, "missing", "Pretty Name").unwrap());
        db::upsert_imported_games(&mut conn, &[game("ugly_name_v2")]).unwrap();
        let games = db::all_games(&conn).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Pretty Name");
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // -- local HTTP server -------------------------------------------------

    #[derive(Debug, Clone)]
    struct Req {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Req {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    type Handler = Arc<dyn Fn(&Req) -> (u16, String) + Send + Sync>;
    type Log = Arc<StdMutex<Vec<Req>>>;

    async fn serve(handler: Handler) -> (String, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Log = Arc::default();
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
                    let mut lines = head.split("\r\n");
                    let first = lines.next().unwrap_or_default().to_string();
                    let mut parts = first.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let target = parts.next().unwrap_or_default().to_string();
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
                        .collect();
                    let len: usize = headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    while raw.len() < head_end + len {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => raw.extend_from_slice(&buf[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
                    let req = Req {
                        method,
                        target,
                        headers,
                        body,
                    };
                    seen.lock().unwrap().push(req.clone());
                    let (status, body) = handler(&req);
                    let out = format!(
                        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), log)
    }

    const ID: &str = "clientid0123456789abcdefghijkl";
    const SECRET: &str = "secret0123456789abcdefghijklmn";
    const TOKEN: &str = "tok0123456789abcdefghijklmnopq";

    fn token_ok() -> (u16, String) {
        (
            200,
            json!({"access_token": TOKEN, "expires_in": 5000, "token_type": "bearer"}).to_string(),
        )
    }

    fn game(title: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: "g1".into(),
            title: title.into(),
            store: "Steam".into(),
            routes,
            ..Default::default()
        }
    }

    fn steam(app_id: &str) -> GameRoute {
        GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            app_id: Some(app_id.into()),
            launch_target: "x".into(),
            ..Default::default()
        }
    }

    fn rom(system: &str) -> GameRoute {
        GameRoute {
            source_id: "rom".into(),
            store: "Emulator".into(),
            system: Some(system.into()),
            launch_target: "x".into(),
            ..Default::default()
        }
    }

    fn full_game() -> Value {
        json!({
            "id": 72,
            "name": "Portal 2",
            "summary": "Line one.\r\nLine two.",
            "storyline": "Story",
            "genres": [{"id": 5, "name": "Shooter"}, {"id": 9, "name": "Puzzle"}],
            "involved_companies": [
                {"id": 1, "company": {"id": 56, "name": "Valve"}, "developer": true, "publisher": true},
                {"id": 2, "company": {"id": 57, "name": "Electronic Arts"}, "developer": false, "publisher": true},
                {"id": 3, "company": 99, "developer": true, "publisher": false}
            ],
            "aggregated_rating": 94.56,
            "aggregated_rating_count": 31,
            "rating": 91.4,
            "rating_count": 2512,
            "first_release_date": 1303171200,
            "url": "https://www.igdb.com/games/portal-2"
        })
    }

    #[tokio::test]
    async fn rejected_credentials_pause_all_requests_until_reset() {
        let (base, log) = serve(Arc::new(|_: &Req| (400, "{}".into()))).await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        assert_eq!(api.game(1).await.unwrap_err(), IgdbError::BadCredentials);
        assert_eq!(api.game(2).await.unwrap_err(), IgdbError::BadCredentials);
        assert_eq!(
            api.search("x").await.unwrap_err(),
            IgdbError::BadCredentials
        );
        assert_eq!(log.lock().unwrap().len(), 1, "Twitch is asked only once");
        // Saving or clearing credentials lifts the pause.
        state.reset();
        assert_eq!(api.game(3).await.unwrap_err(), IgdbError::BadCredentials);
        assert_eq!(log.lock().unwrap().len(), 2);
        // The pause ends after the cooldown.
        let past = Instant::now() - REJECTED_COOLDOWN - Duration::from_secs(1);
        state.set_rejected(IgdbError::BadCredentials, past);
        assert_eq!(state.rejection(Instant::now()), None);

        // IGDB rejecting a fresh token twice pauses requests too.
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (401, "{}".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        assert_eq!(api.game(1).await.unwrap_err(), IgdbError::Unauthorized);
        let before = log.lock().unwrap().len();
        assert_eq!(api.game(1).await.unwrap_err(), IgdbError::Unauthorized);
        assert_eq!(log.lock().unwrap().len(), before, "no further requests");
    }

    #[tokio::test]
    async fn bulk_fetch_stops_after_repeated_server_failures() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (503, "{}".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let db = db::Db::memory().unwrap();
        let games: Vec<Game> = (0..12)
            .map(|i| {
                let mut g = game(&format!("Game {i}"), vec![]);
                g.id = format!("g{i}");
                g
            })
            .collect();
        for g in &games {
            db::save_game(&db.lock(), &crate::types::GamePatch::from(g)).unwrap();
        }
        let mut reports = 0;
        let start = BulkProgress {
            running: true,
            total: games.len(),
            ..Default::default()
        };
        let done = bulk_fetch(&api, &db, &games, start, |p, _| {
            if p.is_some() {
                reports += 1
            }
        })
        .await;
        assert_eq!(done.failed, MAX_CONSECUTIVE_FAILURES);
        assert_eq!(done.done, MAX_CONSECUTIVE_FAILURES);
        assert_eq!(reports, MAX_CONSECUTIVE_FAILURES - 1);
        let message = done.message.unwrap();
        assert!(
            message.starts_with("Stopped after 5 failed requests in a row."),
            "{message}"
        );
        assert!(message.contains("HTTP 503"));
        let api_calls = log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.target.starts_with("/v4/"))
            .count();
        assert_eq!(
            api_calls, MAX_CONSECUTIVE_FAILURES,
            "no requests after stopping"
        );

        // A success in between resets the count.
        let calls = Arc::new(StdMutex::new(0));
        let c = calls.clone();
        let (base, _) = serve(Arc::new(move |req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n % 4 == 0 {
                (200, "[]".into())
            } else {
                (500, "{}".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let done = bulk_fetch(&api, &db, &games[..8], BulkProgress::default(), |_, _| {}).await;
        assert_eq!(done.done, 8);
        assert_eq!(done.message, None);
    }

    #[test]
    fn redaction_covers_an_expired_token() {
        let state = IgdbState::default();
        state.token.set(
            ID,
            TOKEN.into(),
            Duration::from_secs(60),
            Instant::now() - Duration::from_secs(3600),
        );
        let api = api_for("http://127.0.0.1:9", &state);
        assert_eq!(api.redact(format!("bad {TOKEN}")), "bad [redacted]");
    }

    #[test]
    fn image_ids_are_validated_and_urls_built_only_from_them() {
        assert!(valid_image_id("mnljdjtrh44x4snmierh"));
        assert!(valid_image_id("co1wyy"));
        for bad in [
            "",
            "ABC",
            "a b",
            "a/b",
            "../x",
            "a.jpg",
            "a?x=1",
            "a#b",
            "%2e%2e",
            "a\u{0}b",
            "é",
            "https://evil.example/x",
        ] {
            assert!(!valid_image_id(bad), "{bad:?}");
            assert_eq!(image_url(bad, ImageSize::Full), None);
        }
        assert!(!valid_image_id(&"a".repeat(MAX_IMAGE_ID_LEN + 1)));
        assert_eq!(
            image_url("sc6lrc", ImageSize::Full).unwrap(),
            "https://images.igdb.com/igdb/image/upload/t_1080p/sc6lrc.jpg"
        );
        assert_eq!(
            image_url("sc6lrc", ImageSize::Thumb).unwrap(),
            "https://images.igdb.com/igdb/image/upload/t_screenshot_med/sc6lrc.jpg"
        );
        // The API's own `url` field is never used; bad ids are dropped, duplicates removed.
        let g = parse_game(&json!({
            "id": 1, "name": "A",
            "screenshots": [{"image_id": "s1", "url": "//evil.example/x.jpg"}, {"image_id": "../x"}, {"image_id": "s1"}, {"url": "x"}, 5],
            "artworks": [{"image_id": "a1"}]
        }))
        .unwrap();
        assert_eq!(g.screenshots, vec!["s1"]);
        assert_eq!(g.artworks, vec!["a1"]);
        let urls = image_urls(&g);
        assert_eq!(urls.len(), 4);
        assert!(urls.iter().all(|u| {
            let url = url::Url::parse(u).unwrap();
            url.scheme() == "https" && url.host_str() == Some(IMAGE_HOST) && url.query().is_none()
        }));
    }

    #[test]
    fn images_mix_screenshots_and_artworks_up_to_the_cap() {
        let ids = |p: &str, n: usize| (0..n).map(|i| format!("{p}{i}")).collect::<Vec<_>>();
        let m = GameMetadata {
            screenshots: ids("s", 8),
            artworks: ids("a", 8),
            ..Default::default()
        };
        let picked = m.image_ids();
        assert_eq!(picked.len(), MAX_IMAGES);
        assert_eq!(picked.iter().filter(|(k, _)| *k == "screenshot").count(), 5);
        let m = GameMetadata {
            screenshots: ids("s", 8),
            ..Default::default()
        };
        assert_eq!(m.image_ids().len(), 8);
        let views = image_views(&m, &|u| {
            if u.contains("s0.jpg") {
                "lobby-cover://cache-x.jpg".into()
            } else {
                u.to_string()
            }
        });
        assert!(
            views[0].thumb.contains("t_screenshot_med")
                || views[0].thumb.starts_with("lobby-cover://")
        );
        assert!(views.iter().any(|v| v.full == "lobby-cover://cache-x.jpg"));
        assert_eq!(views[0].kind, "screenshot");
    }

    #[test]
    fn time_to_beat_parses_with_missing_values() {
        assert_eq!(
            query_time_to_beat(72),
            "fields hastily,normally,completely,count; where game_id = 72; limit 1;"
        );
        let t = parse_time_to_beat(r#"[{"id":1,"game_id":72,"hastily":153000,"normally":228600,"completely":432000,"count":12}]"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            t,
            TimeToBeat {
                hastily: Some(153_000),
                normally: Some(228_600),
                completely: Some(432_000),
                count: Some(12)
            }
        );
        let t = parse_time_to_beat(r#"[{"normally": 3600}]"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            (t.hastily, t.normally, t.completely, t.count),
            (None, Some(3600), None, None)
        );
        // Nothing usable: none; odd values are dropped.
        assert_eq!(parse_time_to_beat("[]").unwrap(), None);
        assert_eq!(parse_time_to_beat(r#"[{"count": 5}]"#).unwrap(), None);
        assert_eq!(
            parse_time_to_beat(r#"[{"hastily": 0, "normally": -5, "completely": "x"}]"#).unwrap(),
            None
        );
        let huge = MAX_TIME_TO_BEAT_SECONDS + 1;
        assert_eq!(
            parse_time_to_beat(&format!(r#"[{{"hastily": {huge}}}]"#)).unwrap(),
            None
        );
        assert!(parse_time_to_beat("{}").is_err());
    }

    #[test]
    fn hours_are_formatted_to_the_nearest_half() {
        assert_eq!(hours_label(153_000), "42½ h"); // 42.5 h
        assert_eq!(hours_label(3 * 3600), "3 h");
        assert_eq!(hours_label(3 * 3600 + 500), "3 h");
        assert_eq!(hours_label(3 * 3600 + 1000 + 800), "3½ h");
        assert_eq!(hours_label(600), "½ h");
        assert_eq!(hours_label(1), "½ h");
        let v = time_to_beat_view(&TimeToBeat {
            hastily: Some(36_000),
            normally: None,
            completely: Some(90_000),
            count: Some(3),
        });
        assert_eq!(v.main_story.as_deref(), Some("10 h"));
        assert_eq!(v.main_extras, None);
        assert_eq!(v.completionist.as_deref(), Some("25 h"));
    }

    #[test]
    fn old_cache_rows_still_load_and_are_refetched() {
        let now = 1_000_000_000_000;
        let old = CachedMetadata {
            igdb_id: Some(9),
            json: Some(json!({"igdbId": 9, "name": "Old", "summary": "S"}).to_string()),
            fetched_at: now - 1000,
            source: "user".into(),
        };
        let v = view(Some(old.clone()), true, false, &|u| u.to_string());
        let m = v.metadata.unwrap();
        assert_eq!((m.name.as_str(), m.shape), ("Old", 0));
        assert!(m.screenshots.is_empty() && m.time_to_beat.is_none());
        assert!(v.images.is_empty() && v.time_to_beat.is_none());
        // Recent but in the old shape: stale, so the next view refetches (by id: user choice kept).
        assert!(!is_fresh(&old, now));
        assert_eq!(
            plan(Some(&old)),
            Plan::ById {
                igdb_id: 9,
                source: "user".into()
            }
        );
        let mut new = old.clone();
        new.json = Some(json!({"igdbId": 9, "name": "New", "shape": CACHE_SHAPE}).to_string());
        assert!(is_fresh(&new, now));
        // A cached miss has no JSON and keeps its own lifetime.
        let miss = CachedMetadata {
            igdb_id: None,
            json: None,
            fetched_at: now - 1000,
            source: "auto".into(),
        };
        assert!(is_fresh(&miss, now));
    }

    #[tokio::test]
    async fn games_are_fetched_with_images_and_time_to_beat() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            match req.target.as_str() {
                "/v4/games" => (
                    200,
                    r#"[{"id":7,"name":"G","screenshots":[{"id":1,"image_id":"abc123"}]}]"#.into(),
                ),
                "/v4/game_time_to_beats" => {
                    (200, r#"[{"game_id":7,"hastily":7200,"count":4}]"#.into())
                }
                _ => (404, "{}".into()),
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let g = api.game(7).await.unwrap().unwrap();
        assert_eq!(g.screenshots, vec!["abc123"]);
        assert_eq!(g.shape, CACHE_SHAPE);
        assert_eq!(g.time_to_beat.unwrap().hastily, Some(7200));
        let ttb = log
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.target == "/v4/game_time_to_beats")
            .unwrap()
            .body
            .clone();
        assert_eq!(ttb, query_time_to_beat(7));
        assert!(query_by_id(7).contains("screenshots.image_id,artworks.image_id"));

        // IGDB refusing the time-to-beat query doesn't lose the rest.
        let (base, _) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else if req.target == "/v4/games" {
                (200, r#"[{"id":7,"name":"G"}]"#.into())
            } else {
                (400, "[]".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let g = api_for(&base, &state).game(7).await.unwrap().unwrap();
        assert_eq!((g.name.as_str(), g.time_to_beat), ("G", None));
    }

    #[test]
    fn cached_metadata_keeps_its_images_referenced() {
        let db = library_with("g1");
        let m = GameMetadata {
            igdb_id: 3,
            name: "G".into(),
            screenshots: vec!["s1".into(), "BAD/../x".into()],
            artworks: vec!["a1".into()],
            shape: CACHE_SHAPE,
            ..Default::default()
        };
        db::put_metadata(&db.lock(), "g1", &entry(Some(&m), "auto", 1).unwrap()).unwrap();
        let urls = referenced_image_urls(&db.lock()).unwrap();
        assert_eq!(urls.len(), 4);
        assert!(urls.contains("https://images.igdb.com/igdb/image/upload/t_1080p/s1.jpg"));
        assert!(urls.iter().all(|u| !u.contains("BAD")));
    }

    // -- query building ------------------------------------------------------

    #[test]
    fn search_queries_cannot_be_broken_out_of() {
        let q = query_search(r#"Foo"; where id = 1; fields *; ""#, false).unwrap();
        // Exactly one string literal: the quote characters in the title are gone.
        assert_eq!(q.matches('"').count(), 2);
        let literal = q.split('"').nth(1).unwrap();
        assert_eq!(literal, "Foo ; where id = 1; fields *;");
        assert!(q.ends_with(&format!("limit {MAX_SEARCH_RESULTS};")));
        assert!(q.starts_with("search \"Foo ; where"));
        // Backslashes can't escape the closing quote; control characters become spaces.
        let q = query_search("Half\\\nLife \\", false).unwrap();
        assert!(q.starts_with("search \"Half Life\";"), "{q}");
        assert!(query_search("\"\\\"", false).is_none());
        assert!(query_search("   ", true).is_none());
        let long = "a".repeat(500);
        let q = query_search(&long, true).unwrap();
        assert!(q.contains(&format!("\"{}\"", "a".repeat(MAX_QUERY_CHARS))));
        assert!(q.contains(SEARCH_FIELDS_MINIMAL));
    }

    #[test]
    fn steam_external_queries_take_only_digits() {
        assert_eq!(
            query_steam_external("400", SourceField::ExternalGameSource).unwrap(),
            "fields game; where uid = \"400\" & external_game_source = 1; limit 5;"
        );
        assert_eq!(
            query_steam_external("400", SourceField::Category).unwrap(),
            "fields game; where uid = \"400\" & category = 1; limit 5;"
        );
        for bad in ["", "40a", "1\" | id > 0", &"1".repeat(13)] {
            assert!(query_steam_external(bad, SourceField::Category).is_none());
        }
        assert_eq!(
            query_by_id(72),
            format!("fields {GAME_FIELDS}; where id = 72; limit 1;")
        );
    }

    // -- parsing --------------------------------------------------------------

    #[test]
    fn full_responses_parse_and_sanitize() {
        let body = Value::Array(vec![full_game()]).to_string();
        let m = parse_games(&body).unwrap().unwrap();
        assert_eq!(m.igdb_id, 72);
        assert_eq!(m.name, "Portal 2");
        assert_eq!(m.summary.as_deref(), Some("Line one.\nLine two."));
        assert_eq!(m.genres, vec!["Shooter", "Puzzle"]);
        assert_eq!(m.developers, vec!["Valve"]);
        assert_eq!(m.publishers, vec!["Valve", "Electronic Arts"]);
        assert_eq!(
            m.critic_rating,
            Some(Rating {
                score: 95,
                count: Some(31)
            })
        );
        assert_eq!(
            m.user_rating,
            Some(Rating {
                score: 91,
                count: Some(2512)
            })
        );
        assert_eq!(m.release_date, Some(1_303_171_200_000));
        assert_eq!(
            m.url.as_deref(),
            Some("https://www.igdb.com/games/portal-2")
        );
        // Serialized for the renderer in camelCase.
        let out = serde_json::to_value(&m).unwrap();
        assert_eq!(out["criticRating"]["score"], 95);
        assert_eq!(out["igdbId"], 72);
    }

    #[test]
    fn missing_and_odd_fields_are_tolerated() {
        let m = parse_games(r#"[{"id": 5, "name": "Bare"}]"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            m,
            GameMetadata {
                igdb_id: 5,
                name: "Bare".into(),
                shape: CACHE_SHAPE,
                ..Default::default()
            }
        );
        let out = serde_json::to_value(&m).unwrap();
        assert!(out.get("summary").is_none() && out.get("criticRating").is_none());
        let odd = json!([{
            "id": 6, "name": "Odd", "genres": "x", "involved_companies": [{"company": "x"}, 5],
            "rating": "high", "aggregated_rating": null, "url": "javascript:alert(1)",
            "first_release_date": "soon", "summary": 7
        }]);
        let m = parse_games(&odd.to_string()).unwrap().unwrap();
        assert!(m.genres.is_empty() && m.developers.is_empty() && m.publishers.is_empty());
        assert!(m.user_rating.is_none() && m.critic_rating.is_none());
        assert!(m.url.is_none() && m.release_date.is_none() && m.summary.is_none());
        // No id or no name: skipped; not a list or not JSON: an error.
        assert_eq!(parse_games(r#"[{"name": "x"}, {"id": 3}]"#).unwrap(), None);
        assert_eq!(parse_games("[]").unwrap(), None);
        assert!(parse_games("{}").is_err());
        assert!(parse_games("<html>").is_err());
        assert_eq!(
            parse_external(r#"[{"id": 1, "game": 72}]"#).unwrap(),
            Some(72)
        );
        assert_eq!(
            parse_external(r#"[{"id": 1}, {"id": 2, "game": {"id": 9}}]"#).unwrap(),
            Some(9)
        );
        assert_eq!(parse_external("[]").unwrap(), None);
    }

    #[test]
    fn developers_and_publishers_are_split_and_deduplicated() {
        let (dev, publ) = companies(Some(&json!([
            {"company": {"name": "Studio"}, "developer": true, "publisher": false},
            {"company": {"name": "studio"}, "developer": true},
            {"company": {"name": "Pub"}, "publisher": true},
            {"company": {"name": "Porting House"}, "developer": false, "publisher": false},
            {"company": {"name": "  \u{202E}Evil  "}, "developer": true}
        ])));
        assert_eq!(dev, vec!["Studio", "Evil"]);
        assert_eq!(publ, vec!["Pub"]);
        assert_eq!(companies(None), (vec![], vec![]));
    }

    #[test]
    fn ratings_round_and_clamp() {
        let r = |s: Value, c: Value| rating(Some(&s), Some(&c));
        assert_eq!(r(json!(79.5), json!(10)).unwrap().score, 80);
        assert_eq!(r(json!(79.49), json!(10)).unwrap().score, 79);
        assert_eq!(r(json!(100.0), json!(1)).unwrap().score, 100);
        assert_eq!(r(json!(140), json!(1)).unwrap().score, 100);
        assert_eq!(r(json!(-3), json!(1)).unwrap().score, 0);
        assert_eq!(r(json!(50), json!(0)).unwrap().count, None);
        assert_eq!(r(json!(50), json!("x")).unwrap().count, None);
        assert!(r(json!("50"), json!(1)).is_none());
        assert!(rating(None, Some(&json!(3))).is_none());
    }

    #[test]
    fn text_is_sanitized_and_capped() {
        assert_eq!(
            clean_text("a\u{202E}b\u{0007}c\r\n\r\n\r\n\td\u{2066}e  f", 100).unwrap(),
            "abc\n\nde f"
        );
        assert_eq!(clean_text(" \n\u{200F} \n", 100), None);
        let long = "é".repeat(5000);
        let capped = clean_text(&long, MAX_TEXT_BYTES).unwrap();
        assert!(capped.len() <= MAX_TEXT_BYTES && capped.ends_with('…'));
        assert_eq!(clean_name("  A\n\tB\u{200B} "), Some("A B".into()));
        assert_eq!(
            clean_name(&"n".repeat(500)).unwrap().chars().count(),
            MAX_NAME_CHARS
        );
        assert_eq!(clean_name("\u{202A}\u{202C}"), None);
        // HTML is just text: kept literally, rendered by React as text.
        assert_eq!(clean_name("<b>x</b>"), Some("<b>x</b>".into()));
        assert!(igdb_page_url("https://www.igdb.com/games/x").is_some());
        assert!(igdb_page_url("https://igdb.com/games/x").is_some());
        for bad in [
            "http://www.igdb.com/games/x",
            "https://www.igdb.com:444/x",
            "https://u:p@www.igdb.com/x",
            "https://igdb.com.evil.example/x",
            "https://evil.example/?igdb.com",
            "lobby-cover://x",
        ] {
            assert!(igdb_page_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn titles_are_cleaned_for_search() {
        for (raw, want) in [
            ("Portal™ 2", "Portal 2"),
            ("DOOM® (2016)", "DOOM"),
            ("Super Mario World (USA) [!]", "Super Mario World"),
            ("Zelda (Europe) (En,Fr,De) (Rev 1)", "Zelda"),
            ("Chrono Trigger [v1.02]", "Chrono Trigger"),
            ("Hades Demo", "Hades"),
            ("Hades - Demo", "Hades"),
            ("Demon's Souls", "Demon's Souls"),
            (
                "The Witcher 3: Wild Hunt - Game of the Year Edition",
                "The Witcher 3: Wild Hunt",
            ),
            ("Skyrim Special Edition", "Skyrim"),
            ("Mass Effect Legendary Edition", "Mass Effect"),
            ("Borderlands GOTY", "Borderlands"),
            ("Some_Rom_Name", "Some Rom Name"),
            ("(Untitled)", "(Untitled)"),
        ] {
            assert_eq!(clean_title(raw).as_deref(), Some(want), "{raw}");
        }
        assert_eq!(clean_title("™ ®"), None);
    }

    fn cand(id: u64, name: &str, platforms: &[u64], kind: Option<u64>) -> Candidate {
        Candidate {
            id,
            name: name.into(),
            platforms: platforms.to_vec(),
            platform_names: vec![],
            kind,
            version_parent: false,
            release_date: None,
        }
    }

    #[test]
    fn matching_prefers_exact_names_platform_and_main_games() {
        let list = [
            cand(1, "Portal 2: Peer Review", &[6], Some(1)),
            cand(2, "Portal 2", &[6], Some(0)),
            cand(3, "Portal", &[6], Some(0)),
        ];
        assert_eq!(best_match("Portal 2", "Portal 2", Some(6), &list), Some(2));
        // Same name: the platform decides, then main game over DLC.
        let list = [
            cand(10, "Ys", &[19], Some(0)),
            cand(11, "Ys", &[7], Some(0)),
        ];
        assert_eq!(best_match("Ys", "Ys", Some(7), &list), Some(11));
        assert_eq!(best_match("Ys", "Ys", None, &list), Some(10));
        let list = [
            cand(20, "Hades", &[6], Some(1)),
            cand(21, "Hades", &[6], Some(0)),
        ];
        assert_eq!(best_match("Hades", "Hades", Some(6), &list), Some(21));
        // Editions match the base game; the original title's exact entry wins if listed.
        let list = [
            cand(30, "Mass Effect", &[6], Some(0)),
            cand(31, "Mass Effect Legendary Edition", &[6], Some(0)),
        ];
        assert_eq!(
            best_match(
                "Mass Effect Legendary Edition",
                "Mass Effect",
                Some(6),
                &list
            ),
            Some(31)
        );
        // Unrelated names are never picked automatically.
        let list = [cand(40, "Something Else", &[6], Some(0))];
        assert_eq!(best_match("Portal", "Portal", Some(6), &list), None);
        assert_eq!(best_match("Portal", "Portal", Some(6), &[]), None);
    }

    #[test]
    fn platforms_come_from_routes() {
        assert_eq!(platform_for(&game("x", vec![steam("1")])), Some(6));
        assert_eq!(platform_for(&game("x", vec![rom("ps2")])), Some(8));
        assert_eq!(platform_for(&game("x", vec![rom("unknown")])), None);
        assert_eq!(platform_for(&game("x", vec![])), None);
        assert_eq!(igdb_platform("switch"), Some(130));
    }

    #[test]
    fn search_candidates_parse_both_type_fields() {
        let body = json!([
            {"id": 1, "name": "A", "game_type": 0, "platforms": [{"id": 6, "abbreviation": "PC"}], "first_release_date": 10},
            {"id": 2, "name": "B", "category": 1, "platforms": [48]},
            {"id": 3, "name": "C", "version_parent": 1},
            {"id": 4}
        ])
        .to_string();
        let list = parse_candidates(&body).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].platforms, vec![6]);
        assert_eq!(list[0].platform_names, vec!["PC"]);
        assert!(list[0].is_main());
        assert_eq!(list[1].platforms, vec![48]);
        assert!(!list[1].is_main());
        assert!(!list[2].is_main());
        let r = SearchResult::from(&list[0]);
        assert_eq!(r.release_date, Some(10_000));
    }

    // -- tokens -----------------------------------------------------------------

    #[test]
    fn token_body_is_form_encoded() {
        let body = token_form("id 1", "s&e=c+r/et");
        assert_eq!(
            body,
            "client_id=id+1&client_secret=s%26e%3Dc%2Br%2Fet&grant_type=client_credentials"
        );
        let (token, life) = parse_token(r#"{"access_token":"abc","expires_in":100}"#).unwrap();
        assert_eq!((token.as_str(), life), ("abc", Duration::from_secs(100)));
        assert_eq!(
            parse_token(r#"{"access_token":"abc"}"#).unwrap().1,
            Duration::from_secs(3600)
        );
        assert_eq!(
            parse_token(r#"{"access_token":"abc","expires_in":1}"#)
                .unwrap()
                .1,
            Duration::from_secs(60)
        );
        for bad in [
            r#"{"access_token":"a b"}"#,
            r#"{"access_token":""}"#,
            "{}",
            "x",
        ] {
            assert!(parse_token(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn token_cache_expires_and_is_per_client() {
        let cache = TokenCache::default();
        let now = Instant::now();
        assert_eq!(cache.get("a", now), None);
        cache.set("a", "t1".into(), Duration::from_secs(3600), now);
        assert_eq!(cache.get("a", now).as_deref(), Some("t1"));
        assert_eq!(cache.get("b", now), None);
        // Renewed shortly before it expires.
        assert_eq!(
            cache
                .get("a", now + Duration::from_secs(3600 - 61))
                .as_deref(),
            Some("t1")
        );
        assert_eq!(cache.get("a", now + Duration::from_secs(3600 - 59)), None);
        cache.clear();
        assert_eq!(cache.get("a", now), None);
    }

    // -- HTTP against a local server -------------------------------------------------

    fn api_for<'a>(base: &str, state: &'a IgdbState) -> Api<'a> {
        Api::new(Endpoints::local(base), ID.into(), SECRET.into(), state).unwrap()
    }

    #[tokio::test]
    async fn token_is_requested_once_in_the_body_and_reused() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (200, Value::Array(vec![full_game()]).to_string())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let first = api.game(72).await.unwrap().unwrap();
        assert_eq!(first.name, "Portal 2");
        api.game(72).await.unwrap();
        let log = log.lock().unwrap().clone();
        let tokens: Vec<_> = log.iter().filter(|r| r.target.contains("oauth2")).collect();
        assert_eq!(tokens.len(), 1, "token cached between requests");
        let t = tokens[0];
        assert_eq!(t.method, "POST");
        assert_eq!(t.target, "/oauth2/token", "nothing in the query string");
        assert!(!t.target.contains(SECRET));
        assert_eq!(
            t.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(t.body, token_form(ID, SECRET));
        let games: Vec<_> = log.iter().filter(|r| r.target == "/v4/games").collect();
        assert_eq!(games.len(), 2);
        for g in games {
            assert_eq!(g.method, "POST");
            assert_eq!(g.header("client-id"), Some(ID));
            assert_eq!(g.header("authorization"), Some(&*format!("Bearer {TOKEN}")));
            assert_eq!(g.body, query_by_id(72));
            assert!(!g.body.contains(SECRET) && !g.target.contains(SECRET));
        }
    }

    #[tokio::test]
    async fn a_401_renews_the_token_once() {
        let calls = Arc::new(StdMutex::new(0));
        let c = calls.clone();
        let (base, log) = serve(Arc::new(move |req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n == 1 {
                (401, "{}".into())
            } else {
                (200, r#"[{"id":1,"name":"A"}]"#.into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        assert_eq!(api.game(1).await.unwrap().unwrap().name, "A");
        let token_requests = log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.target.contains("oauth2"))
            .count();
        assert_eq!(token_requests, 2);

        // A persistent 401 ends in an error instead of looping.
        let (base, _) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (401, "{}".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        assert_eq!(api.game(1).await.unwrap_err(), IgdbError::Unauthorized);
    }

    #[tokio::test]
    async fn rate_limits_back_off_then_give_up() {
        let calls = Arc::new(StdMutex::new(0));
        let c = calls.clone();
        let (base, _) = serve(Arc::new(move |req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n <= 2 {
                (429, "{}".into())
            } else {
                (200, r#"[{"id":1,"name":"A"}]"#.into())
            }
        }))
        .await;
        let state = IgdbState::default();
        assert!(api_for(&base, &state).game(1).await.unwrap().is_some());
        let (base, _) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (429, "{}".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        assert_eq!(
            api_for(&base, &state).game(1).await.unwrap_err(),
            IgdbError::RateLimited
        );
    }

    #[tokio::test]
    async fn rejected_credentials_never_appear_in_errors() {
        let (base, _) = serve(Arc::new(|_: &Req| {
            (
                400,
                json!({"status": 400, "message": format!("invalid client secret {SECRET}")})
                    .to_string(),
            )
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let e = api.game(1).await.unwrap_err();
        assert_eq!(e, IgdbError::BadCredentials);
        let text = e.to_string();
        assert!(!text.contains(SECRET) && !text.contains(ID));
        // Network errors are redacted too, should a message ever carry a credential.
        let msg = api.redact(format!("failed for {SECRET} / {ID}"));
        assert!(!msg.contains(SECRET) && !msg.contains(ID), "{msg}");
        // Unreachable server: the message has neither the URL's secret nor the body.
        let state = IgdbState::default();
        let api = api_for("http://127.0.0.1:9", &state);
        let text = api.game(1).await.unwrap_err().to_string();
        assert!(!text.contains(SECRET) && !text.contains(TOKEN), "{text}");
    }

    #[tokio::test]
    async fn steam_lookup_falls_back_to_the_legacy_field_then_the_title() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            match req.target.as_str() {
                "/v4/external_games" if req.body.contains("external_game_source") => {
                    (400, "[]".into())
                }
                "/v4/external_games" => (200, r#"[{"id":9,"game":72}]"#.into()),
                _ => (200, "[]".into()),
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let g = game("Portal 2", vec![steam("620")]);
        assert_eq!(api.match_game(&g).await.unwrap(), Some(72));
        let bodies: Vec<String> = log.lock().unwrap().iter().map(|r| r.body.clone()).collect();
        assert!(bodies
            .iter()
            .any(|b| b.contains("external_game_source = 1")));
        assert!(bodies.iter().any(|b| b.contains("category = 1")));

        // Not on IGDB by app id: searched by title instead.
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            if req.body.starts_with("search") {
                (
                    200,
                    r#"[{"id":5,"name":"Other"},{"id":6,"name":"Portal 2","game_type":0}]"#.into(),
                )
            } else {
                (200, "[]".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let g = game("Portal 2™ Demo", vec![steam("620")]);
        assert_eq!(api.match_game(&g).await.unwrap(), Some(6));
        let log = log.lock().unwrap();
        let search = log.iter().find(|r| r.body.starts_with("search")).unwrap();
        assert!(
            search.body.starts_with("search \"Portal 2\";"),
            "{}",
            search.body
        );
    }

    #[tokio::test]
    async fn search_retries_with_fewer_fields_on_400() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            if req.body.contains("game_type") {
                (400, "[]".into())
            } else {
                (200, r#"[{"id":1,"name":"A"}]"#.into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        assert_eq!(api.search("A").await.unwrap().len(), 1);
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn oversized_responses_are_refused() {
        let (base, _) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                token_ok()
            } else {
                (200, format!("[\"{}\"]", "x".repeat(MAX_RESPONSE_BYTES)))
            }
        }))
        .await;
        let state = IgdbState::default();
        assert_eq!(
            api_for(&base, &state).game(1).await.unwrap_err(),
            IgdbError::Invalid("the response is too large.")
        );
    }

    #[test]
    fn production_endpoints_are_https_to_the_two_hosts() {
        let e = Endpoints::production();
        assert!(e.https_only);
        for url in [&e.token, &e.api_url("games")] {
            assert_eq!(url.scheme(), "https");
            assert!(HOSTS.contains(&url.host_str().unwrap()));
        }
        assert_eq!(e.api_url("games").as_str(), "https://api.igdb.com/v4/games");
    }

    // -- cache ---------------------------------------------------------------------------

    fn row(id: Option<i64>, fetched_at: i64, source: &str) -> CachedMetadata {
        CachedMetadata {
            igdb_id: id,
            json: id.map(|i| {
                serde_json::to_string(&GameMetadata {
                    igdb_id: i as u64,
                    name: format!("Game {i}"),
                    shape: CACHE_SHAPE,
                    ..Default::default()
                })
                .unwrap()
            }),
            fetched_at,
            source: source.into(),
        }
    }

    #[test]
    fn cache_freshness() {
        let now = 1_000_000_000_000;
        assert!(is_fresh(
            &row(Some(1), now - METADATA_TTL_MS + 1, "auto"),
            now
        ));
        assert!(!is_fresh(&row(Some(1), now - METADATA_TTL_MS, "auto"), now));
        // Misses expire sooner.
        assert!(is_fresh(&row(None, now - MISS_TTL_MS + 1, "auto"), now));
        assert!(!is_fresh(&row(None, now - MISS_TTL_MS, "auto"), now));
        // Far-future timestamps (clock changes) are stale.
        assert!(!is_fresh(
            &row(Some(1), now + FUTURE_SKEW_MS + 1, "auto"),
            now
        ));
    }

    fn library_with(game_id: &str) -> db::Db {
        let db = db::Db::memory().unwrap();
        db::save_game(
            &db.lock(),
            &crate::types::GamePatch {
                title: Some("Portal".into()),
                store: Some("Steam".into()),
                ..crate::types::GamePatch::new(game_id)
            },
        )
        .unwrap();
        db
    }

    #[test]
    fn user_choice_wins_over_automatic_matches() {
        let db = library_with("g1");
        let conn = db.lock();
        assert!(db::put_metadata(&conn, "g1", &row(Some(1), 10, "auto")).unwrap());
        assert!(db::put_metadata(&conn, "g1", &row(Some(2), 20, "user")).unwrap());
        // A late automatic result for another id doesn't replace it...
        assert!(!db::put_metadata(&conn, "g1", &row(Some(3), 30, "auto")).unwrap());
        assert!(!db::put_metadata(&conn, "g1", &row(None, 30, "auto")).unwrap());
        let c = db::cached_metadata(&conn, "g1").unwrap().unwrap();
        assert_eq!(
            (c.igdb_id, c.source.as_str(), c.fetched_at),
            (Some(2), "user", 20)
        );
        // ...but a refresh of the same id updates it and keeps it the user's.
        assert!(db::put_metadata(&conn, "g1", &row(Some(2), 40, "auto")).unwrap());
        let c = db::cached_metadata(&conn, "g1").unwrap().unwrap();
        assert_eq!(
            (c.igdb_id, c.source.as_str(), c.fetched_at),
            (Some(2), "user", 40)
        );
        assert_eq!(
            plan(Some(&c)),
            Plan::ById {
                igdb_id: 2,
                source: "user".into()
            }
        );
        assert_eq!(plan(Some(&row(None, 1, "auto"))), Plan::Match);
        assert_eq!(plan(None), Plan::Match);
        // Unknown games store nothing; removing the game removes the row.
        assert!(!db::put_metadata(&conn, "ghost", &row(Some(1), 1, "auto")).unwrap());
        drop(conn);
        db::remove_game(&db.lock(), "g1").unwrap();
        assert_eq!(db::cached_metadata(&db.lock(), "g1").unwrap(), None);
    }

    #[tokio::test]
    async fn refresh_keeps_the_user_choice_and_caches_misses() {
        let (base, log) = serve(Arc::new(|req: &Req| {
            if req.target.starts_with("/oauth2/token") {
                return token_ok();
            }
            if req.body.contains("where id = 2;") {
                (200, r#"[{"id":2,"name":"Chosen","summary":"S"}]"#.into())
            } else {
                (200, "[]".into())
            }
        }))
        .await;
        let state = IgdbState::default();
        let api = api_for(&base, &state);
        let db = library_with("g1");
        let g = db::get_game(&db.lock(), "g1").unwrap().unwrap();
        // No match anywhere: a miss is cached.
        let out = refresh_game(&api, &db, &g).await.unwrap();
        assert_eq!(
            out,
            RefreshOutcome {
                found: false,
                changed: true
            }
        );
        let c = db::cached_metadata(&db.lock(), "g1").unwrap().unwrap();
        assert_eq!((c.igdb_id, c.json.as_deref()), (None, None));
        // The user picks id 2: refreshes fetch that id directly, never re-match.
        db::put_metadata(&db.lock(), "g1", &row(Some(2), 1, "user")).unwrap();
        log.lock().unwrap().clear();
        let out = refresh_game(&api, &db, &g).await.unwrap();
        assert_eq!(
            out,
            RefreshOutcome {
                found: true,
                changed: true
            }
        );
        let bodies: Vec<String> = log.lock().unwrap().iter().map(|r| r.body.clone()).collect();
        assert!(bodies
            .iter()
            .all(|b| !b.starts_with("search") && !b.contains("uid")));
        let v = view(
            db::cached_metadata(&db.lock(), "g1").unwrap(),
            true,
            false,
            &|u| u.to_string(),
        );
        assert_eq!(v.source.as_deref(), Some("user"));
        let m = v.metadata.unwrap();
        assert_eq!(
            (m.igdb_id, m.name.as_str(), m.summary.as_deref()),
            (2, "Chosen", Some("S"))
        );
    }

    #[test]
    fn merges_keep_user_choices_and_redo_automatic_matches() {
        let db = library_with("keep");
        db::save_game(
            &db.lock(),
            &crate::types::GamePatch {
                title: Some("Portal (GOG)".into()),
                store: Some("GOG".into()),
                ..crate::types::GamePatch::new("dupe")
            },
        )
        .unwrap();
        db::put_metadata(&db.lock(), "keep", &row(Some(1), 1, "auto")).unwrap();
        db::put_metadata(&db.lock(), "dupe", &row(Some(2), 1, "user")).unwrap();
        db::merge_games(&mut db.lock(), "keep", "dupe").unwrap();
        let c = db::cached_metadata(&db.lock(), "keep").unwrap().unwrap();
        assert_eq!((c.igdb_id, c.source.as_str()), (Some(2), "user"));
        // The kept game's own (automatic) row is parked under the linked entry's id until it is
        // unlinked (`links`); no game shows it meanwhile.
        let parked = db::cached_metadata(&db.lock(), "dupe").unwrap().unwrap();
        assert_eq!((parked.igdb_id, parked.source.as_str()), (Some(1), "auto"));
        assert!(db::get_game(&db.lock(), "dupe").unwrap().is_none());
    }

    #[test]
    fn views_re_sanitize_stored_json() {
        let tampered = CachedMetadata {
            igdb_id: Some(1),
            json: Some(
                json!({"igdbId": 1, "name": "A\u{202E}B", "url": "https://evil.example/", "genres": ["", "X", "x"]})
                    .to_string(),
            ),
            fetched_at: 5,
            source: "auto".into(),
        };
        let v = view(Some(tampered), true, false, &|u| u.to_string());
        let m = v.metadata.unwrap();
        assert_eq!(m.name, "AB");
        assert_eq!(m.url, None);
        assert_eq!(m.genres, vec!["X"]);
        assert!(v.checked);
        let none = view(None, false, false, &|u| u.to_string());
        assert!(!none.checked && none.metadata.is_none());
    }
}
