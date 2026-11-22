//! Built-in game facts, logos and free covers from Wikidata, Wikipedia and Wikimedia Commons.
//! No key is needed; the setting `wikimediaEnabled` (default on) turns all of it off.
//!
//! What is fetched (see `docs/design/wikimedia-provider.md`):
//! - Wikidata (CC0): title, first release, platforms, developers, publishers, genres and
//!   external ids (Steam, GOG, Epic, IGDB, MobyGames).
//! - English Wikipedia (CC BY-SA 4.0): the first paragraph of the article's lead, shown with
//!   "From Wikipedia", a link to the article and the licence, and only when IGDB has no
//!   description.
//! - Wikimedia Commons: logos (P154, or P18 when its file is a logo) and covers (P18 when its
//!   file is a portrait cover or box art), only when Commons hosts the file under a free
//!   licence ([`licence`]). Never fair-use or non-free images: those live on Wikipedia, not
//!   Commons, and the image host is limited to the Commons upload folder.
//!
//! Matching ([`matching`]): external ids first (one batched SPARQL query), editions stand for
//! their game, then an exact title search; anything ambiguous is offered to the user, never
//! guessed. Results (no match and ambiguous ones too) are cached per game in
//! `wikimedia_match`; image licences and credits per image URL in `wikimedia_images`. Neither
//! table is in backups (like the IGDB cache); nothing scraped is ever bundled with the app.
//!
//! Only missing information is filled: Wikidata values are shown where IGDB and the user have
//! none, the game's title, genre and cover in the library are never changed, a Wikimedia logo
//! is only an automatic fallback behind Steam and SteamGridDB (a chosen logo always wins), and
//! a Wikimedia cover is never set automatically: covers are only offered in Customize. Choosing
//! a logo or cover is the user's explicit "replace". A Commons image is never shown without its
//! credit record; chosen ones are checked on Commons again with every refresh.

pub mod http;
pub mod licence;
pub mod matching;
pub mod parse;

use crate::db;
use crate::state::AppState;
use crate::types::{now_ms, Game, GamePatch};
use anyhow::{anyhow, bail, Result};
use http::{Api, Endpoints, Limiter, Service, WikiError};
use licence::CommonsImage;
use matching::Decision;
use parse::{Entity, IdKind};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const MATCH_TTL_MS: i64 = 30 * 24 * 3_600_000;
pub const MISS_TTL_MS: i64 = 7 * 24 * 3_600_000;
const FUTURE_SKEW_MS: i64 = 24 * 3_600_000;
/// Version of the stored JSON; older rows count as stale.
pub const SHAPE: u32 = 1;
/// After a failed background fetch, the same game is not retried for this long.
const FAILURE_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// Games per batch in the background (one SPARQL query, shared entity and image requests).
pub const BATCH: usize = 10;
const ENTITY_BATCH: usize = 5;
const LABEL_BATCH: usize = 50;
const FILE_BATCH: usize = 50;
const EXTRACT_BATCH: usize = 20;
const SEARCH_LIMIT: &str = "10";
const MAX_CANDIDATES: usize = 10;
const MAX_LOGOS: usize = 3;
const MAX_COVERS: usize = 2;
/// The rendering width asked from Commons (a standard thumbnail step).
const THUMB_WIDTH: &str = "500";
/// A cover's original must be at least this tall; below [`TRUSTED_COVER_PX`] it is offered
/// but never set automatically.
const MIN_COVER_PX: u32 = 350;
const TRUSTED_COVER_PX: u32 = 600;
/// The background run stops after this many failed batches in a row.
const MAX_CONSECUTIVE_FAILURES: usize = 3;
/// Wikipedia text licence.
pub const TEXT_LICENCE: &str = "CC BY-SA 4.0";
pub const TEXT_LICENCE_URL: &str = "https://creativecommons.org/licenses/by-sa/4.0/";

// ---------------------------------------------------------------------------
// Data

/// The Wikipedia description.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Article {
    pub title: String,
    pub url: String,
    pub text: String,
}

/// What is stored per matched game (and shown, after re-sanitizing).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WikiMetadata {
    pub qid: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub steam: Vec<String>,
    #[serde(default)]
    pub gog: Vec<String>,
    #[serde(default)]
    pub epic: Vec<String>,
    #[serde(default)]
    pub igdb: Vec<String>,
    #[serde(default)]
    pub mobygames: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article: Option<Article>,
    #[serde(default)]
    pub logos: Vec<CommonsImage>,
    #[serde(default)]
    pub covers: Vec<CommonsImage>,
    #[serde(default)]
    pub shape: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub qid: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
}

/// An image with its credit, as the page shows it. `src` is the Commons URL, `display` what to
/// load (the cached copy once downloaded).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageCredit {
    #[serde(flatten)]
    pub image: CommonsImage,
    pub display: String,
    /// `"logo"` or `"cover"`.
    pub kind: &'static str,
    /// A small original (a cover under 600 px tall): offered, never set automatically.
    pub low_trust: bool,
}

/// What `wikimedia_get` returns.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WikiView {
    pub enabled: bool,
    pub pending: bool,
    pub checked: bool,
    /// `"matched"`, `"none"` or `"ambiguous"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<WikiMetadata>,
    /// `"auto"` or `"user"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `"steam"`, `"igdb"`, `"title"` or `"user"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub logos: Vec<ImageCredit>,
    #[serde(default)]
    pub covers: Vec<ImageCredit>,
    /// The credit for the game's current cover when it came from Commons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_credit: Option<ImageCredit>,
    /// The Wikipedia text licence, set when there is a description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_licence: Option<TextLicence>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextLicence {
    pub name: &'static str,
    pub url: &'static str,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MetadataEvent<'a> {
    game_id: &'a str,
    changed: bool,
}

// ---------------------------------------------------------------------------
// Database

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS wikimedia_match(gameId TEXT PRIMARY KEY,qid TEXT,status TEXT NOT NULL,matchedBy TEXT NOT NULL DEFAULT '',source TEXT NOT NULL DEFAULT 'auto',json TEXT,candidates TEXT,fetchedAt INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS wikimedia_images(url TEXT PRIMARY KEY,json TEXT NOT NULL,fetchedAt INTEGER NOT NULL);",
    )?;
    Ok(())
}

/// One `wikimedia_match` row.
#[derive(Clone, Debug, PartialEq)]
pub struct CachedMatch {
    pub qid: Option<String>,
    pub status: String,
    pub matched_by: String,
    pub source: String,
    pub json: Option<String>,
    pub candidates: Option<String>,
    pub fetched_at: i64,
}

pub fn cached(conn: &Connection, game_id: &str) -> Result<Option<CachedMatch>> {
    Ok(conn
        .query_row(
            "SELECT qid,status,matchedBy,source,json,candidates,fetchedAt FROM wikimedia_match WHERE gameId=?",
            [game_id],
            |r| {
                Ok(CachedMatch {
                    qid: r.get(0)?,
                    status: r.get(1)?,
                    matched_by: r.get(2)?,
                    source: r.get(3)?,
                    json: r.get(4)?,
                    candidates: r.get(5)?,
                    fetched_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// Stores a result for a game that still exists. A user's choice is only replaced by another
/// user choice, or refreshed by an automatic fetch of the same item (it stays `user`).
pub fn put(conn: &Connection, game_id: &str, row: &CachedMatch) -> Result<bool> {
    let source = if row.source == "user" { "user" } else { "auto" };
    let changed = conn.execute(
        "INSERT INTO wikimedia_match(gameId,qid,status,matchedBy,source,json,candidates,fetchedAt) SELECT ?1,?2,?3,?4,?5,?6,?7,?8 WHERE EXISTS(SELECT 1 FROM games WHERE id=?1)
         ON CONFLICT(gameId) DO UPDATE SET qid=excluded.qid,status=excluded.status,matchedBy=excluded.matchedBy,json=excluded.json,candidates=excluded.candidates,fetchedAt=excluded.fetchedAt,
           source=CASE WHEN wikimedia_match.source='user' THEN 'user' ELSE excluded.source END
         WHERE wikimedia_match.source<>'user' OR excluded.source='user' OR wikimedia_match.qid IS excluded.qid",
        params![
            game_id,
            row.qid,
            row.status,
            row.matched_by,
            source,
            row.json,
            row.candidates,
            row.fetched_at
        ],
    )?;
    Ok(changed > 0)
}

pub fn delete(conn: &Connection, game_id: &str) -> Result<()> {
    conn.execute("DELETE FROM wikimedia_match WHERE gameId=?", [game_id])?;
    Ok(())
}

/// Records the licence and credit of each image by its URL (kept for chosen logos and covers
/// even when the match changes).
pub fn put_images(conn: &Connection, images: &[&CommonsImage], now: i64) -> Result<()> {
    for image in images {
        conn.execute(
            "INSERT INTO wikimedia_images(url,json,fetchedAt) VALUES(?1,?2,?3)
             ON CONFLICT(url) DO UPDATE SET json=excluded.json,fetchedAt=excluded.fetchedAt",
            params![image.src, serde_json::to_string(image)?, now],
        )?;
    }
    Ok(())
}

/// The recorded credit for a Commons image URL (re-checked).
pub fn image(conn: &Connection, url: &str) -> Option<CommonsImage> {
    if !licence::valid_image_url(url) {
        return None;
    }
    let json: String = conn
        .query_row(
            "SELECT json FROM wikimedia_images WHERE url=?",
            [url],
            |r| r.get(0),
        )
        .optional()
        .ok()??;
    serde_json::from_str::<CommonsImage>(&json)
        .ok()
        .and_then(clean_image)
        .filter(|i| i.src == url)
}

/// Re-applies the checks to a stored image.
fn clean_image(image: CommonsImage) -> Option<CommonsImage> {
    let src = licence::display_url(&image.src)?;
    (src == image.src
        && image
            .source_page
            .starts_with("https://commons.wikimedia.org/wiki/File:")
        && !image.licence.is_empty())
    .then(|| CommonsImage {
        artist: image.artist.and_then(|a| licence::plain_text(&a)),
        credit: image.credit.and_then(|c| licence::plain_text(&c)),
        restrictions: image.restrictions.and_then(|r| licence::plain_text(&r)),
        licence_url: image
            .licence_url
            .filter(|u| u.starts_with("https://creativecommons.org/")),
        licence: licence::plain_text(&image.licence).unwrap_or_default(),
        ..image
    })
}

/// Stored metadata, re-sanitized.
fn metadata_of(row: &CachedMatch) -> Option<WikiMetadata> {
    let m: WikiMetadata = serde_json::from_str(row.json.as_deref()?).ok()?;
    let list = |items: Vec<String>| -> Vec<String> {
        items
            .into_iter()
            .filter_map(|s| crate::igdb::clean_name(&s))
            .take(parse::MAX_PER_PROPERTY)
            .collect()
    };
    let ids = |items: Vec<String>, ok: fn(&str) -> bool| -> Vec<String> {
        items.into_iter().filter(|s| ok(s)).collect()
    };
    if !parse::valid_qid(&m.qid) {
        return None;
    }
    Some(WikiMetadata {
        label: crate::igdb::clean_name(&m.label)?,
        description: m.description.and_then(|d| crate::igdb::clean_name(&d)),
        release: m
            .release
            .filter(|r| r.len() <= 10 && r.chars().all(|c| c.is_ascii_digit() || c == '-')),
        platforms: list(m.platforms),
        developers: list(m.developers),
        publishers: list(m.publishers),
        genres: list(m.genres),
        steam: ids(m.steam, parse::valid_steam_id),
        gog: ids(m.gog, parse::valid_slug),
        epic: ids(m.epic, parse::valid_slug),
        igdb: ids(m.igdb, parse::valid_slug),
        mobygames: ids(m.mobygames, parse::valid_slug),
        article: m.article.and_then(|a| {
            let title = crate::igdb::clean_name(&a.title)?;
            Some(Article {
                url: parse::article_url(&title),
                text: crate::igdb::clean_text(&a.text, parse::MAX_EXTRACT_BYTES * 2)?,
                title,
            })
        }),
        logos: m.logos.into_iter().filter_map(clean_image).collect(),
        covers: m.covers.into_iter().filter_map(clean_image).collect(),
        ..m
    })
}

/// Every Commons image URL the cache or a chosen logo/hero refers to (the cover cache sweep
/// keeps their files).
pub fn referenced_image_urls(conn: &Connection) -> Result<HashSet<String>> {
    let rows: Vec<String> = conn
        .prepare("SELECT json FROM wikimedia_match WHERE json IS NOT NULL")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut urls: HashSet<String> = rows
        .iter()
        .filter_map(|j| serde_json::from_str::<WikiMetadata>(j).ok())
        .flat_map(|m| m.logos.into_iter().chain(m.covers).map(|i| i.src))
        .filter(|u| licence::valid_image_url(u))
        .collect();
    let custom: Vec<Option<String>> = conn
        .prepare("SELECT hero FROM game_page_custom UNION SELECT logo FROM game_page_custom")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    urls.extend(
        custom
            .into_iter()
            .flatten()
            .filter(|u| licence::valid_image_url(u)),
    );
    Ok(urls)
}

pub fn enabled(conn: &Connection) -> bool {
    db::get_settings(conn).is_ok_and(|s| s.wikimedia_enabled)
}

pub fn is_fresh(row: &CachedMatch, now: i64) -> bool {
    let matched = row.status == "matched";
    let ttl = if matched { MATCH_TTL_MS } else { MISS_TTL_MS };
    let shape_ok = !matched
        || row
            .json
            .as_deref()
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .and_then(|v| v.get("shape").and_then(serde_json::Value::as_u64))
            .is_some_and(|s| s >= SHAPE as u64);
    shape_ok && row.fetched_at <= now + FUTURE_SKEW_MS && now - row.fetched_at < ttl
}

// ---------------------------------------------------------------------------
// Resolving games (network, batched)

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameInput {
    pub game_id: String,
    pub title: String,
    pub steam: Option<String>,
    pub igdb: Option<String>,
    pub platform: Option<&'static str>,
    pub year: Option<i32>,
    pub known: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub game_id: String,
    pub decision: Decision,
    pub matched_by: String,
    pub metadata: Option<WikiMetadata>,
    pub candidates: Vec<Candidate>,
}

fn igdb_slug(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let mut segments = url.path_segments()?;
    (segments.next()? == "games")
        .then(|| segments.next().map(str::to_string))
        .flatten()
        .filter(|s| parse::valid_slug(s) && !s.contains('/'))
}

/// The UTC calendar year of a Unix time in milliseconds (civil-from-days).
pub fn year_of_ms(ms: i64) -> i32 {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2)) as i32
}

pub fn input_for(conn: &Connection, game: &Game) -> GameInput {
    let row = cached(conn, &game.id).ok().flatten();
    let known = row
        .as_ref()
        .filter(|r| r.status == "matched")
        .and_then(|r| r.qid.clone())
        .filter(|q| parse::valid_qid(q));
    let igdb = db::cached_metadata(conn, &game.id)
        .ok()
        .flatten()
        .and_then(|m| m.json)
        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
    let slug = igdb
        .as_ref()
        .and_then(|v| v.get("url").and_then(serde_json::Value::as_str))
        .and_then(igdb_slug);
    let year = igdb
        .as_ref()
        .and_then(|v| v.get("releaseDate").and_then(serde_json::Value::as_i64))
        .map(year_of_ms);
    let platform = game
        .routes
        .iter()
        .find_map(|r| r.system.as_deref())
        .map(matching::platform_item)
        .unwrap_or_else(|| {
            game.routes
                .iter()
                .any(|r| r.store != "Emulator" && r.source_id != "rom")
                .then_some("Q1406")
        });
    GameInput {
        game_id: game.id.clone(),
        title: game.title.clone(),
        steam: crate::steamgriddb::steam_app_id(game)
            .filter(|s| parse::valid_steam_id(s))
            .map(str::to_string),
        igdb: slug,
        platform,
        year,
        known,
    }
}

/// Items fetched so far, and every id ever asked for (so an id Wikidata doesn't return, such as
/// a deleted edition base, is never asked for again).
#[derive(Default)]
pub struct Entities {
    pub map: HashMap<String, Entity>,
    requested: HashSet<String>,
}

const MAX_EDITION_ROUNDS: usize = 2;

async fn fetch_entities(
    api: &Api<'_>,
    entities: &mut Entities,
    qids: impl IntoIterator<Item = String>,
) -> Result<(), WikiError> {
    let mut next: Vec<String> = qids.into_iter().collect();
    for _ in 0..=MAX_EDITION_ROUNDS {
        let wanted: Vec<String> = next
            .drain(..)
            .filter(|q| parse::valid_qid(q) && !entities.map.contains_key(q))
            .filter(|q| entities.requested.insert(q.clone()))
            .collect();
        if wanted.is_empty() {
            break;
        }
        fetch_entity_ids(api, &mut entities.map, &wanted).await?;
        // Editions: their game too (once).
        next = entities
            .map
            .values()
            .filter_map(matching::edition_base)
            .filter(|b| !entities.map.contains_key(*b))
            .map(str::to_string)
            .collect();
    }
    Ok(())
}

async fn fetch_entity_ids(
    api: &Api<'_>,
    entities: &mut HashMap<String, Entity>,
    wanted: &[String],
) -> Result<(), WikiError> {
    for chunk in wanted.chunks(ENTITY_BATCH) {
        let ids = chunk.join("|");
        let body = api
            .get(
                Service::Wikidata,
                &[
                    ("action", "wbgetentities"),
                    ("ids", &ids),
                    ("props", "claims|sitelinks|labels|descriptions|aliases"),
                    ("languages", "en"),
                    ("languagefallback", "1"),
                    ("sitefilter", "enwiki"),
                ],
            )
            .await?;
        for (key, entity) in parse::parse_entities(&body)? {
            if chunk.contains(&key) {
                entities.insert(key, entity);
            }
        }
    }
    Ok(())
}

async fn fetch_labels(
    api: &Api<'_>,
    labels: &mut HashMap<String, String>,
    qids: Vec<String>,
) -> Result<(), WikiError> {
    let mut wanted: Vec<String> = qids
        .into_iter()
        .filter(|q| parse::valid_qid(q) && !labels.contains_key(q))
        .collect();
    wanted.sort();
    wanted.dedup();
    for chunk in wanted.chunks(LABEL_BATCH) {
        let ids = chunk.join("|");
        let body = api
            .get(
                Service::Wikidata,
                &[
                    ("action", "wbgetentities"),
                    ("ids", &ids),
                    ("props", "labels"),
                    ("languages", "en"),
                    ("languagefallback", "1"),
                ],
            )
            .await?;
        labels.extend(parse::parse_labels(&body)?);
    }
    Ok(())
}

/// What Commons said about one file.
#[derive(Debug, Clone, PartialEq)]
pub enum FileVerdict {
    /// Hosted by Commons under a free licence.
    Free(Box<CommonsImage>),
    /// Explicitly missing, or its image info failed the licence filter.
    Refused,
}

/// The verdicts in one Commons `prop=imageinfo` answer (formatversion 2), by requested file key.
/// Requested names follow `normalized` and `redirects` (a renamed file is its new page). A file
/// the answer says nothing definite about (absent, or a page without image info) has no verdict.
/// An API error body or an answer without `query.pages` is an error: it decides nothing.
pub fn parse_imageinfo(
    body: &str,
    requested: &[String],
) -> Result<HashMap<String, FileVerdict>, WikiError> {
    let value = parse::json(body)?;
    let query = value
        .get("query")
        .ok_or(WikiError::Invalid("the Commons answer has no query."))?;
    let pages = query
        .get("pages")
        .and_then(serde_json::Value::as_array)
        .ok_or(WikiError::Invalid("the Commons answer has no pages."))?;
    let mut by_title: HashMap<String, FileVerdict> = HashMap::new();
    for page in pages {
        let Some(title) = page.get("title").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let key = licence::file_key(title);
        let missing = page
            .get("missing")
            .is_some_and(|m| m.as_bool() != Some(false));
        let has_info = page
            .get("imageinfo")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|a| !a.is_empty());
        let verdict = if missing {
            FileVerdict::Refused
        } else if has_info {
            match licence::check_page(page) {
                Ok(image) => FileVerdict::Free(Box::new(image)),
                Err(reason) => {
                    log::debug!("Wikimedia: refused {title}: {reason}");
                    FileVerdict::Refused
                }
            }
        } else {
            continue;
        };
        by_title.insert(key, verdict);
    }
    let pairs = |name: &str| -> Vec<(String, String)> {
        query
            .get(name)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    licence::file_key(p.get("from")?.as_str()?),
                    licence::file_key(p.get("to")?.as_str()?),
                ))
            })
            .collect()
    };
    let moves: Vec<(String, String)> = pairs("normalized")
        .into_iter()
        .chain(pairs("redirects"))
        .collect();
    let mut out = HashMap::new();
    for key in requested {
        let mut at = key.clone();
        for _ in 0..4 {
            match moves.iter().find(|(from, _)| *from == at) {
                Some((_, to)) if *to != at => at = to.clone(),
                _ => break,
            }
        }
        if let Some(v) = by_title.get(&at) {
            out.insert(key.clone(), v.clone());
        }
    }
    Ok(out)
}

/// Commons verdicts for files, by [`licence::file_key`] of the requested names.
async fn fetch_images(
    api: &Api<'_>,
    files: Vec<String>,
) -> Result<HashMap<String, FileVerdict>, WikiError> {
    let mut keys: Vec<String> = files.iter().map(|f| licence::file_key(f)).collect();
    keys.sort();
    keys.dedup();
    keys.retain(|k| !k.is_empty());
    let mut out = HashMap::new();
    for chunk in keys.chunks(FILE_BATCH) {
        let titles = chunk
            .iter()
            .map(|k| format!("File:{k}"))
            .collect::<Vec<_>>()
            .join("|");
        let body = api
            .get(
                Service::Commons,
                &[
                    ("action", "query"),
                    ("titles", &titles),
                    ("redirects", "1"),
                    ("prop", "imageinfo"),
                    ("iiprop", "url|size|mime|extmetadata"),
                    ("iiextmetadatafilter", "LicenseShortName|License|UsageTerms|Artist|Credit|AttributionRequired|Restrictions|NonFree|Copyrighted|LicenseUrl"),
                    ("iiurlwidth", THUMB_WIDTH),
                ],
            )
            .await?;
        out.extend(parse_imageinfo(&body, chunk)?);
    }
    Ok(out)
}

fn free_only(verdicts: HashMap<String, FileVerdict>) -> HashMap<String, CommonsImage> {
    verdicts
        .into_iter()
        .filter_map(|(k, v)| match v {
            FileVerdict::Free(image) => Some((k, *image)),
            FileVerdict::Refused => None,
        })
        .collect()
}

async fn fetch_extracts(
    api: &Api<'_>,
    titles: Vec<String>,
) -> Result<HashMap<String, parse::Extract>, WikiError> {
    let mut titles = titles;
    titles.sort();
    titles.dedup();
    let mut out = HashMap::new();
    for chunk in titles.chunks(EXTRACT_BATCH) {
        let joined = chunk.join("|");
        let body = api
            .get(
                Service::Wikipedia,
                &[
                    ("action", "query"),
                    ("prop", "extracts|pageprops"),
                    ("exintro", "1"),
                    ("explaintext", "1"),
                    ("exlimit", "20"),
                    ("ppprop", "disambiguation"),
                    ("redirects", "1"),
                    ("titles", &joined),
                ],
            )
            .await?;
        out.extend(parse::parse_extracts(&body)?);
    }
    Ok(out)
}

async fn search_hits(api: &Api<'_>, text: &str) -> Result<Vec<parse::SearchHit>, WikiError> {
    let text: String = text.chars().take(200).collect();
    let body = api
        .get(
            Service::Wikidata,
            &[
                ("action", "wbsearchentities"),
                ("search", &text),
                ("language", "en"),
                ("uselang", "en"),
                ("type", "item"),
                ("limit", SEARCH_LIMIT),
            ],
        )
        .await?;
    parse::parse_search(&body)
}

fn candidate(entity: &Entity) -> Option<Candidate> {
    Some(Candidate {
        qid: entity.qid.clone(),
        label: entity.label.clone()?,
        description: entity.description.clone(),
        year: entity.release.as_ref().map(|r| r.year),
    })
}

fn is_cover_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("cover") || lower.contains("box")
}

fn is_logo_file(name: &str) -> bool {
    name.to_lowercase().contains("logo")
}

/// Portrait, cover-shaped, and not tiny.
fn cover_shaped(image: &CommonsImage) -> bool {
    let (w, h) = (image.original_width as f64, image.original_height as f64);
    w > 0.0 && h / w >= 1.2 && h / w <= 1.8 && image.original_height >= MIN_COVER_PX
}

pub fn low_trust_cover(image: &CommonsImage) -> bool {
    image.original_height < TRUSTED_COVER_PX
}

fn build(
    entity: &Entity,
    labels: &HashMap<String, String>,
    images: &HashMap<String, CommonsImage>,
    extracts: &HashMap<String, parse::Extract>,
) -> WikiMetadata {
    let names = |ids: &[String]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if let Some(l) = labels.get(id) {
                if !out.iter().any(|o| o.eq_ignore_ascii_case(l)) {
                    out.push(l.clone());
                }
            }
        }
        out
    };
    let image = |f: &String| images.get(&licence::file_key(f)).cloned();
    let mut logos: Vec<CommonsImage> = entity.logos.iter().filter_map(image).collect();
    logos.extend(
        entity
            .images
            .iter()
            .filter(|f| is_logo_file(f))
            .filter_map(image),
    );
    logos.dedup_by(|a, b| a.src == b.src);
    logos.truncate(MAX_LOGOS);
    let mut covers: Vec<CommonsImage> = entity
        .images
        .iter()
        .filter(|f| is_cover_file(f) && !is_logo_file(f))
        .filter_map(image)
        .filter(cover_shaped)
        .collect();
    covers.truncate(MAX_COVERS);
    let article = entity
        .enwiki
        .as_ref()
        .and_then(|t| extracts.get(t))
        .map(|e| Article {
            url: parse::article_url(&e.title),
            title: e.title.clone(),
            text: e.text.clone(),
        });
    WikiMetadata {
        qid: entity.qid.clone(),
        label: entity.label.clone().unwrap_or_else(|| entity.qid.clone()),
        description: entity.description.clone(),
        release: entity.release.as_ref().map(|r| r.text()),
        platforms: names(&entity.platforms),
        developers: names(&entity.developers),
        publishers: names(&entity.publishers),
        genres: names(&entity.genres),
        steam: entity.steam.clone(),
        gog: entity.gog.clone(),
        epic: entity.epic.clone(),
        igdb: entity.igdb.clone(),
        mobygames: entity.mobygames.clone(),
        article,
        logos,
        covers,
        shape: SHAPE,
    }
}

/// Resolves a batch of games: one SPARQL query for every external id, shared entity, label,
/// image and article requests. A failure fails the whole batch (nothing is stored).
pub async fn resolve_batch(api: &Api<'_>, inputs: &[GameInput]) -> Result<Vec<Outcome>, WikiError> {
    let mut cache = Entities::default();
    let open: Vec<&GameInput> = inputs.iter().filter(|i| i.known.is_none()).collect();
    let steam: Vec<String> = open.iter().filter_map(|i| i.steam.clone()).collect();
    let igdb: Vec<String> = open.iter().filter_map(|i| i.igdb.clone()).collect();
    let mut rows: Vec<(IdKind, String, String)> = Vec::new();
    if let Some(query) = parse::sparql_query(&steam, &igdb) {
        let body = api.get(Service::Sparql, &[("query", &query)]).await?;
        rows = parse::parse_sparql(&body)?;
    }
    let hits = |kind: IdKind, id: &Option<String>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(id) = id {
            for (k, v, q) in &rows {
                if *k == kind && v == id && !out.contains(q) {
                    out.push(q.clone());
                }
            }
        }
        out
    };
    let mut wanted: Vec<String> = inputs.iter().filter_map(|i| i.known.clone()).collect();
    wanted.extend(rows.iter().map(|(_, _, q)| q.clone()));
    fetch_entities(api, &mut cache, wanted).await?;

    let mut decided: Vec<(Decision, String)> = Vec::new();
    for input in inputs {
        let entities = &cache.map;
        if let Some(known) = &input.known {
            let ok = entities.get(known).is_some_and(matching::is_game);
            decided.push(if ok {
                (
                    Decision::Match(matching::resolve(known, entities).to_string()),
                    "known".into(),
                )
            } else {
                (Decision::None, "known".into())
            });
            continue;
        }
        let steam_hits = hits(IdKind::Steam, &input.steam);
        let (by, list) = if steam_hits.is_empty() {
            ("igdb", hits(IdKind::Igdb, &input.igdb))
        } else {
            ("steam", steam_hits)
        };
        let decision = matching::pick_by_ids(&list, entities);
        if decision != Decision::None {
            decided.push((decision, by.into()));
            continue;
        }
        let Some(title) = crate::igdb::clean_title(&input.title) else {
            decided.push((Decision::None, "title".into()));
            continue;
        };
        let found: Vec<String> = search_hits(api, &title)
            .await?
            .into_iter()
            .map(|h| h.qid)
            .collect();
        fetch_entities(api, &mut cache, found.clone()).await?;
        let decision =
            matching::pick_by_title(&input.title, &found, &cache.map, input.platform, input.year);
        decided.push((decision, "title".into()));
    }

    // Everything the matched items need, in shared requests.
    let entities = &cache.map;
    let matched: Vec<&Entity> = decided
        .iter()
        .filter_map(|(d, _)| match d {
            Decision::Match(q) => entities.get(q),
            _ => None,
        })
        .collect();
    let mut labels = HashMap::new();
    fetch_labels(
        api,
        &mut labels,
        matched
            .iter()
            .flat_map(|e| {
                e.platforms
                    .iter()
                    .chain(&e.developers)
                    .chain(&e.publishers)
                    .chain(&e.genres)
                    .cloned()
            })
            .collect(),
    )
    .await?;
    let files: Vec<String> = matched
        .iter()
        .flat_map(|e| {
            e.logos.iter().cloned().chain(
                e.images
                    .iter()
                    .filter(|f| is_logo_file(f) || is_cover_file(f))
                    .cloned(),
            )
        })
        .collect();
    let images = if files.is_empty() {
        HashMap::new()
    } else {
        free_only(fetch_images(api, files).await?)
    };
    let titles: Vec<String> = matched.iter().filter_map(|e| e.enwiki.clone()).collect();
    let extracts = if titles.is_empty() {
        HashMap::new()
    } else {
        fetch_extracts(api, titles).await?
    };

    Ok(inputs
        .iter()
        .zip(decided)
        .map(|(input, (decision, by))| {
            let metadata = match &decision {
                Decision::Match(q) => entities
                    .get(q)
                    .map(|e| build(e, &labels, &images, &extracts)),
                _ => None,
            };
            let candidates = match &decision {
                Decision::Ambiguous(list) => list
                    .iter()
                    .filter_map(|q| entities.get(q))
                    .filter_map(candidate)
                    .take(MAX_CANDIDATES)
                    .collect(),
                _ => Vec::new(),
            };
            Outcome {
                game_id: input.game_id.clone(),
                decision,
                matched_by: by,
                metadata,
                candidates,
            }
        })
        .collect())
}

/// The row to store for an automatic outcome. Its source is always `auto`: a user's choice is
/// protected by [`store`] and [`put`], never by copying the stored source (`matched_by` of an
/// earlier match carries on for a refresh of a known item).
pub fn row_for(outcome: &Outcome, previous: Option<&CachedMatch>, now: i64) -> Result<CachedMatch> {
    let (status, qid) = match &outcome.decision {
        Decision::Match(q) => ("matched", Some(q.clone())),
        Decision::Ambiguous(_) => ("ambiguous", None),
        Decision::None => ("none", None),
    };
    let matched_by = if outcome.matched_by == "known" {
        previous
            .map(|p| p.matched_by.clone())
            .unwrap_or_else(|| "user".into())
    } else {
        outcome.matched_by.clone()
    };
    Ok(CachedMatch {
        qid,
        status: status.into(),
        matched_by,
        source: "auto".into(),
        json: outcome
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        candidates: (!outcome.candidates.is_empty())
            .then(|| serde_json::to_string(&outcome.candidates))
            .transpose()?,
        fetched_at: now,
    })
}

/// Stores an automatic outcome. The user's choice is never replaced: only a refresh of the same
/// item updates it (and it stays the user's). When the chosen item is gone (deleted from
/// Wikidata, or no longer a game) the choice is kept as it was and only its time is updated, so
/// it isn't fetched again at once. Returns whether the data changed.
pub fn store(conn: &Connection, outcome: &Outcome, now: i64) -> Result<bool> {
    let previous = cached(conn, &outcome.game_id)?;
    if let Some(p) = previous.as_ref().filter(|p| p.source == "user") {
        let same =
            matches!(&outcome.decision, Decision::Match(q) if p.qid.as_deref() == Some(q.as_str()));
        if !same {
            conn.execute(
                "UPDATE wikimedia_match SET fetchedAt=?1 WHERE gameId=?2 AND source='user'",
                params![now, outcome.game_id],
            )?;
            return Ok(false);
        }
    }
    let row = row_for(outcome, previous.as_ref(), now)?;
    let changed = put(conn, &outcome.game_id, &row)?;
    if let Some(m) = &outcome.metadata {
        let all: Vec<&CommonsImage> = m.logos.iter().chain(&m.covers).collect();
        put_images(conn, &all, now)?;
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Views

fn credit(
    image: CommonsImage,
    kind: &'static str,
    resolve: &dyn Fn(&str) -> String,
) -> ImageCredit {
    ImageCredit {
        display: resolve(&image.src),
        low_trust: kind == "cover" && low_trust_cover(&image),
        kind,
        image,
    }
}

/// The page's view of a cache row. `cover` is the game's current cover source URL.
pub fn view(
    row: Option<CachedMatch>,
    enabled: bool,
    pending: bool,
    cover_source: Option<&str>,
    conn: &Connection,
    resolve: &dyn Fn(&str) -> String,
) -> WikiView {
    let cover_credit = cover_source
        .and_then(|url| image(conn, url))
        .map(|i| credit(i, "cover", resolve));
    if !enabled {
        return WikiView {
            cover_credit,
            ..Default::default()
        };
    }
    let Some(row) = row else {
        return WikiView {
            enabled,
            pending,
            cover_credit,
            ..Default::default()
        };
    };
    let metadata = metadata_of(&row);
    let candidates: Vec<Candidate> = row
        .candidates
        .as_deref()
        .and_then(|c| serde_json::from_str::<Vec<Candidate>>(c).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| parse::valid_qid(&c.qid))
        .filter_map(|c| {
            Some(Candidate {
                label: crate::igdb::clean_name(&c.label)?,
                description: c.description.and_then(|d| crate::igdb::clean_name(&d)),
                ..c
            })
        })
        .take(MAX_CANDIDATES)
        .collect();
    WikiView {
        enabled,
        pending,
        checked: true,
        status: Some(row.status.clone()),
        source: metadata.as_ref().map(|_| row.source.clone()),
        matched_by: metadata.as_ref().map(|_| row.matched_by.clone()),
        item_url: metadata.as_ref().and_then(|m| parse::item_url(&m.qid)),
        fetched_at: Some(row.fetched_at),
        candidates,
        logos: metadata
            .iter()
            .flat_map(|m| m.logos.clone())
            .map(|i| credit(i, "logo", resolve))
            .collect(),
        covers: metadata
            .iter()
            .flat_map(|m| m.covers.clone())
            .map(|i| credit(i, "cover", resolve))
            .collect(),
        text_licence: metadata
            .as_ref()
            .and_then(|m| m.article.as_ref())
            .map(|_| TextLicence {
                name: TEXT_LICENCE,
                url: TEXT_LICENCE_URL,
            }),
        metadata,
        cover_credit,
    }
}

/// Automatic logo candidates for the game page (Commons URLs), when the provider is on.
pub fn logo_urls(conn: &Connection, game_id: &str) -> Vec<String> {
    if !enabled(conn) {
        return Vec::new();
    }
    cached(conn, game_id)
        .ok()
        .flatten()
        .and_then(|r| metadata_of(&r))
        .map(|m| m.logos.into_iter().map(|i| i.src).collect())
        .unwrap_or_default()
}

/// Credits for Commons images: `(Commons URL, value the page loads, slot)` with slot `"hero"`,
/// `"logo"` or `"cover"`. Images without a valid credit record are left out.
pub fn credits_for(
    conn: &Connection,
    items: &[(String, String, &'static str)],
) -> Vec<ImageCredit> {
    items
        .iter()
        .filter_map(|(source, display, kind)| {
            let image = image(conn, source)?;
            Some(ImageCredit {
                display: display.clone(),
                low_trust: *kind == "cover" && low_trust_cover(&image),
                kind,
                image,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// App state and operations

/// After Wikimedia limits requests, nothing is asked for this long.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10 * 60);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(45);
/// An uncredited Commons image is looked up again at most this often.
pub const CREDIT_RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub struct WikimediaState {
    pub limiter: Limiter,
    in_flight: StdMutex<HashSet<String>>,
    failed: StdMutex<HashMap<String, Instant>>,
    auto_running: AtomicBool,
    auto_again: AtomicBool,
    /// Set when Wikimedia answered 429/503/maxlag beyond our patience.
    limited_until: StdMutex<Option<Instant>>,
    client: OnceLock<reqwest::Client>,
    image_client: OnceLock<reqwest::Client>,
    /// When each uncredited Commons URL was last looked up (see [`CREDIT_RETRY_AFTER`]).
    credit_tried: StdMutex<HashMap<String, Instant>>,
}

impl WikimediaState {
    fn begin(&self, id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_string())
    }
    fn end(&self, id: &str) {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }
    fn is_in_flight(&self, id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(id)
    }
    fn recently_failed(&self, id: &str) -> bool {
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .is_some_and(|at| at.elapsed() < FAILURE_COOLDOWN)
    }
    fn set_failed(&self, id: &str, failed: bool) {
        let mut map = self.failed.lock().unwrap_or_else(|p| p.into_inner());
        if failed {
            map.insert(id.to_string(), Instant::now());
        } else {
            map.remove(id);
        }
    }
    /// The URLs not looked up within [`CREDIT_RETRY_AFTER`] of `now`, marked as looked up now.
    pub fn claim_credit_lookups(&self, urls: Vec<String>, now: Instant) -> Vec<String> {
        let mut tried = self.credit_tried.lock().unwrap_or_else(|p| p.into_inner());
        tried.retain(|_, at| now.saturating_duration_since(*at) < CREDIT_RETRY_AFTER);
        let mut out = Vec::new();
        for url in urls {
            if !tried.contains_key(&url) && !out.contains(&url) {
                tried.insert(url.clone(), now);
                out.push(url);
            }
        }
        out
    }

    /// Whether requests are paused after Wikimedia limited them.
    pub fn rate_limited(&self) -> bool {
        self.limited_until
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|until| Instant::now() < until)
    }
    pub fn set_rate_limited(&self) {
        *self.limited_until.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(Instant::now() + RATE_LIMIT_PAUSE);
    }
    /// Notes a rate limit in `result`, passing it on.
    fn note<T>(&self, result: Result<T, WikiError>) -> Result<T, WikiError> {
        if matches!(result, Err(WikiError::RateLimited)) {
            self.set_rate_limited();
        }
        result
    }
    /// The API client (one HTTP client for the app, built once).
    pub fn api(&self) -> Result<Api<'_>, WikiError> {
        if self.rate_limited() {
            return Err(WikiError::RateLimited);
        }
        let endpoints = Endpoints::production();
        let client = match self.client.get() {
            Some(c) => c.clone(),
            None => {
                let built = http::build_client(&endpoints)?;
                self.client.get_or_init(|| built).clone()
            }
        };
        Api::with_client(endpoints, &self.limiter, client)
    }
    fn image_client(&self) -> Option<reqwest::Client> {
        if let Some(c) = self.image_client.get() {
            return Some(c.clone());
        }
        let built =
            crate::covercache::http_client(crate::covercache::is_allowed, IMAGE_TIMEOUT).ok()?;
        Some(self.image_client.get_or_init(|| built).clone())
    }
}

fn api(state: &AppState) -> Result<Api<'_>> {
    state.wikimedia.api().map_err(|e| anyhow!(e))
}

fn emit(app: &AppHandle, game_id: &str, changed: bool) {
    let _ = app.emit("wikimedia:metadata", MetadataEvent { game_id, changed });
    if changed {
        // The game page reloads its art (a Wikimedia logo may be new).
        let _ = app.emit("art:changed", serde_json::json!({ "gameId": game_id }));
    }
}

/// Downloads Commons images into the cover cache one at a time, spaced like API requests, and
/// stops (pausing all Wikimedia requests) when upload.wikimedia.org answers 429. Already cached
/// files need no request. Returns the cached file per URL.
pub async fn download_images(state: &AppState, urls: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let covers_dir = state.covers_dir();
    for url in urls {
        if !licence::valid_image_url(url) {
            continue;
        }
        if let Ok(Some(file)) = crate::covercache::existing_file(&state.db.lock(), &covers_dir, url)
        {
            out.insert(url.clone(), file);
            continue;
        }
        if state.wikimedia.rate_limited() {
            break;
        }
        let Some(client) = state.wikimedia.image_client() else {
            break;
        };
        // One request at a time, shared with the API requests.
        let _turn = state.wikimedia.limiter.turn().await;
        state.wikimedia.limiter.wait(http::SPACING).await;
        match crate::covercache::fetch_to_file(
            &client,
            &covers_dir,
            url,
            crate::covercache::is_allowed,
        )
        .await
        {
            Ok(file) => {
                out.insert(url.clone(), file);
            }
            Err(crate::covercache::FetchError::Http(429 | 503)) => {
                log::warn!("Wikimedia is limiting image downloads; pausing.");
                state.wikimedia.set_rate_limited();
                break;
            }
            Err(e) => log::warn!("Could not cache a Wikimedia image: {e}"),
        }
    }
    out
}

/// Downloads the logo a game is likely to show: its first Commons logo, only when the game has
/// no Steam logo to show first. Other logos and covers are loaded when they are shown or chosen.
async fn cache_logo(state: &AppState, outcome: &Outcome) {
    let Some(first) = outcome.metadata.as_ref().and_then(|m| m.logos.first()) else {
        return;
    };
    let steam = db::get_game(&state.db.lock(), &outcome.game_id)
        .ok()
        .flatten()
        .is_some_and(|g| crate::steamgriddb::steam_app_id(&g).is_some());
    if steam {
        return;
    }
    let files = download_images(state, std::slice::from_ref(&first.src)).await;
    if let Some(file) = files.get(&first.src) {
        let conn = state.db.lock();
        if let Err(e) = crate::covercache::adopt(&conn, &state.covers_dir(), &first.src, Some(file))
        {
            log::warn!("Could not record a Wikimedia image: {e:#}");
        }
    }
}

/// The Commons file name of a display URL (`.../commons/a/ab/Name.png` or
/// `.../commons/thumb/a/ab/Name.svg/500px-Name.svg.png`).
pub fn file_of_url(url: &str) -> Option<String> {
    if !licence::valid_image_url(url) {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let parts: Vec<&str> = parsed.path_segments()?.collect();
    // ["wikipedia", "commons", ...]
    let rest = parts.get(2..)?;
    let name = if rest.first() == Some(&"thumb") {
        rest.get(3)?
    } else {
        rest.get(2)?
    };
    let decoded = percent_encoding::percent_decode_str(name)
        .decode_utf8()
        .ok()?
        .to_string();
    let key = licence::file_key(&decoded);
    (!key.is_empty()).then_some(key)
}

/// Checks Commons images again by their file names: each URL whose file Commons still offers
/// under a free licence gets a fresh credit record (`true`); one whose page explicitly came back
/// missing, or whose image info fails the licence filter, gets `false` and loses its credit
/// record. Files the answer says nothing definite about get no verdict, and errors (network, API
/// error bodies, odd answers) decide nothing (`Err`).
pub async fn recheck(
    api: &Api<'_>,
    db: &db::Db,
    urls: &[String],
) -> Result<HashMap<String, bool>, WikiError> {
    let files: Vec<(String, String)> = urls
        .iter()
        .filter_map(|u| file_of_url(u).map(|f| (u.clone(), f)))
        .collect();
    if files.is_empty() {
        return Ok(HashMap::new());
    }
    let found = fetch_images(api, files.iter().map(|(_, f)| f.clone()).collect()).await?;
    let conn = db.lock();
    let mut out = HashMap::new();
    for (url, file) in files {
        match found.get(&file) {
            Some(FileVerdict::Free(image)) => {
                // The credit is the file's; the chosen display URL stays as it was.
                let image = CommonsImage {
                    src: url.clone(),
                    ..(**image).clone()
                };
                if let Err(e) = put_images(&conn, &[&image], now_ms()) {
                    log::warn!("Could not store a Wikimedia credit: {e:#}");
                }
                out.insert(url, true);
            }
            Some(FileVerdict::Refused) => {
                let _ = conn.execute("DELETE FROM wikimedia_images WHERE url=?", [&url]);
                out.insert(url, false);
            }
            None => {}
        }
    }
    Ok(out)
}

/// Commons images the user chose for these games (hero, logo, cover).
fn chosen_images(conn: &Connection, game_ids: &[String]) -> Vec<(String, &'static str, String)> {
    let mut out = Vec::new();
    for id in game_ids {
        let custom = db::page_custom(conn, id).unwrap_or_default();
        for (slot, value) in [("hero", custom.hero), ("logo", custom.logo)] {
            if let Some(v) = value.filter(|v| licence::valid_image_url(v)) {
                out.push((id.clone(), slot, v));
            }
        }
        if let Some(cover) = db::get_game(conn, id)
            .ok()
            .flatten()
            .and_then(|g| g.cover)
            .and_then(|c| crate::covercache::source_of(conn, &c).ok())
            .filter(|c| licence::valid_image_url(c))
        {
            out.push((id.clone(), "cover", cover));
        }
    }
    out
}

/// Re-checks the Commons images chosen for these games; one Commons no longer offers as a free
/// file is dropped: the hero/logo goes back to the automatic art, the cover to the game's default
/// cover. Returns the games whose library entry changed.
async fn recheck_chosen(state: &AppState, api: &Api<'_>, game_ids: &[String]) -> Vec<String> {
    let chosen = chosen_images(&state.db.lock(), game_ids);
    if chosen.is_empty() {
        return Vec::new();
    }
    let urls: Vec<String> = chosen.iter().map(|(_, _, u)| u.clone()).collect();
    let verdicts = match state.wikimedia.note(recheck(api, &state.db, &urls).await) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Could not re-check chosen Wikimedia images: {e}");
            return Vec::new();
        }
    };
    let mut changed = Vec::new();
    let conn = state.db.lock();
    for (game_id, slot, url) in chosen {
        if verdicts.get(&url) != Some(&false) {
            continue;
        }
        log::info!("Wikimedia: a chosen {slot} is no longer a free Commons file; it was removed.");
        let result = match slot {
            "hero" | "logo" => {
                let slot = if slot == "hero" {
                    crate::art::Slot::Hero
                } else {
                    crate::art::Slot::Logo
                };
                crate::art::set_image(&conn, &state.covers_dir(), &game_id, slot, None).map(|_| ())
            }
            _ => db::get_game(&conn, &game_id).and_then(|g| {
                let default = g.and_then(|g| g.default_cover).unwrap_or_default();
                db::save_game(
                    &conn,
                    &GamePatch {
                        cover: Some(default),
                        ..GamePatch::new(game_id.clone())
                    },
                )
                .map(|_| ())
            }),
        };
        match result {
            Ok(()) => changed.push(game_id),
            Err(e) => log::warn!("Could not remove a Wikimedia image: {e:#}"),
        }
    }
    changed
}

/// Commons images about to be shown on a game page whose credit record is missing (a backup
/// restored on another PC has the URLs but not their credits).
pub fn uncredited(conn: &Connection, values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter(|u| licence::valid_image_url(u) && image(conn, u).is_none())
        .cloned()
        .collect()
}

/// Looks up the credits of `urls` on Commons in the background (only with the provider on,
/// not paused, and at most once per URL per [`CREDIT_RETRY_AFTER`]), then tells the game page to
/// reload its art when any came back. The page never waits for it.
pub fn spawn_restore_credits(app: &AppHandle, game_id: &str, urls: Vec<String>) {
    let state = app.state::<AppState>();
    if urls.is_empty() || !enabled(&state.db.lock()) || state.wikimedia.rate_limited() {
        return;
    }
    let urls = state.wikimedia.claim_credit_lookups(urls, Instant::now());
    if urls.is_empty() {
        return;
    }
    let app = app.clone();
    let game_id = game_id.to_string();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Ok(api) = state.wikimedia.api() else {
            return;
        };
        match state.wikimedia.note(recheck(&api, &state.db, &urls).await) {
            Ok(verdicts) if verdicts.values().any(|ok| *ok) => {
                let _ = app.emit("art:changed", serde_json::json!({ "gameId": game_id }));
            }
            Ok(_) => {}
            Err(e) => log::warn!("Could not look up Wikimedia credits: {e}"),
        }
    });
}

async fn run_batch(app: &AppHandle, inputs: &[GameInput]) -> Result<(), WikiError> {
    let state = app.state::<AppState>();
    let api = state.wikimedia.api()?;
    let outcomes = state.wikimedia.note(resolve_batch(&api, inputs).await)?;
    for outcome in &outcomes {
        let changed = match store(&state.db.lock(), outcome, now_ms()) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Could not store Wikimedia data: {e:#}");
                false
            }
        };
        if changed {
            cache_logo(&state, outcome).await;
        }
        emit(app, &outcome.game_id, changed);
    }
    let ids: Vec<String> = inputs.iter().map(|i| i.game_id.clone()).collect();
    let removed = recheck_chosen(&state, &api, &ids).await;
    for id in &removed {
        emit(app, id, true);
    }
    if !removed.is_empty() {
        let _ = app.emit("library:changed", ());
    }
    Ok(())
}

/// Fetches one game in the background (skipped when already running for it). Waits for an
/// IGDB lookup of the same game first, so its IGDB id can be used for matching.
fn spawn_refresh(app: &AppHandle, game: Game) -> bool {
    let state = app.state::<AppState>();
    if state.wikimedia.rate_limited() || !state.wikimedia.begin(&game.id) {
        return false;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        for _ in 0..30 {
            if !state.igdb.is_in_flight(&game.id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let input = input_for(&state.db.lock(), &game);
        let result = run_batch(&app, &[input]).await;
        state.wikimedia.end(&game.id);
        match result {
            Ok(()) => state.wikimedia.set_failed(&game.id, false),
            Err(e) => {
                state.wikimedia.set_failed(&game.id, true);
                log::warn!("Wikimedia lookup failed: {e}");
                emit(&app, &game.id, false);
            }
        }
    });
    true
}

fn resolved_view(state: &AppState, game: &Game, pending: bool) -> Result<WikiView> {
    let conn = state.db.lock();
    let covers_dir = state.covers_dir();
    let row = cached(&conn, &game.id)?;
    let source = game
        .cover
        .as_deref()
        .and_then(|c| crate::covercache::source_of(&conn, c).ok());
    Ok(view(
        row,
        enabled(&conn),
        pending,
        source.as_deref(),
        &conn,
        &|url| crate::covercache::cached_value(&conn, &covers_dir, url),
    ))
}

/// The cached view at once; starts a background fetch when missing or stale (and on).
pub fn get(app: &AppHandle, game_id: &str) -> Result<WikiView> {
    let state = app.state::<AppState>();
    let (game, row, on) = {
        let conn = state.db.lock();
        (
            db::get_game(&conn, game_id)?,
            cached(&conn, game_id)?,
            enabled(&conn),
        )
    };
    let Some(game) = game else {
        return Ok(WikiView::default());
    };
    let stale = row.as_ref().is_none_or(|r| !is_fresh(r, now_ms()));
    let mut pending = state.wikimedia.is_in_flight(game_id);
    if on && stale && !pending && !state.wikimedia.recently_failed(game_id) {
        pending = spawn_refresh(app, game.clone());
    }
    resolved_view(&state, &game, pending)
}

/// "Find on Wikidata": game items for the game's title or for `text`.
pub async fn search(state: &AppState, game_id: &str, text: Option<&str>) -> Result<Vec<Candidate>> {
    let game =
        db::get_game(&state.db.lock(), game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    if !enabled(&state.db.lock()) {
        bail!("Wikipedia and Wikidata are turned off in Settings.");
    }
    let wanted = match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => crate::igdb::clean_name(t),
        None => crate::igdb::clean_title(&game.title),
    };
    let Some(wanted) = wanted else {
        return Ok(Vec::new());
    };
    let api = api(state)?;
    let hits: Vec<String> = search_hits(&api, &wanted)
        .await?
        .into_iter()
        .map(|h| h.qid)
        .collect();
    let mut entities = Entities::default();
    fetch_entities(&api, &mut entities, hits.clone()).await?;
    Ok(hits
        .iter()
        .filter_map(|q| entities.map.get(q))
        .filter(|e| matching::is_game(e))
        .filter_map(candidate)
        .collect())
}

/// Stores the user's choice of item (it always wins over automatic matches).
pub async fn choose(app: &AppHandle, game_id: &str, qid: &str) -> Result<WikiView> {
    if !parse::valid_qid(qid) {
        bail!("That Wikidata item id is not valid.");
    }
    let state = app.state::<AppState>();
    let game =
        db::get_game(&state.db.lock(), game_id)?.ok_or_else(|| anyhow!("Game not found."))?;
    if !enabled(&state.db.lock()) {
        bail!("Wikipedia and Wikidata are turned off in Settings.");
    }
    let api = api(&state)?;
    let input = GameInput {
        known: Some(qid.to_string()),
        ..input_for(&state.db.lock(), &game)
    };
    let outcome = state
        .wikimedia
        .note(resolve_batch(&api, &[input]).await)?
        .pop()
        .ok_or_else(|| anyhow!("Wikidata gave no answer."))?;
    if outcome.metadata.is_none() {
        bail!("That Wikidata item is not a video game.");
    }
    {
        let conn = state.db.lock();
        let mut row = row_for(&outcome, None, now_ms())?;
        row.source = "user".into();
        row.matched_by = "user".into();
        if !put(&conn, game_id, &row)? {
            bail!("Game not found.");
        }
        let m = outcome.metadata.as_ref().unwrap();
        let all: Vec<&CommonsImage> = m.logos.iter().chain(&m.covers).collect();
        put_images(&conn, &all, now_ms())?;
    }
    state.wikimedia.set_failed(game_id, false);
    cache_logo(&state, &outcome).await;
    emit(app, game_id, true);
    resolved_view(&state, &game, false)
}

/// Forgets the stored match (including a user choice) and matches again.
pub fn reset_match(app: &AppHandle, game_id: &str) -> Result<WikiView> {
    let state = app.state::<AppState>();
    delete(&state.db.lock(), game_id)?;
    state.wikimedia.set_failed(game_id, false);
    emit(app, game_id, true);
    get(app, game_id)
}

/// One of the game's Wikimedia images by its Commons URL (only those the lookup offered).
fn offered(conn: &Connection, game_id: &str, src: &str, kind: &str) -> Result<CommonsImage> {
    let m = cached(conn, game_id)?
        .and_then(|r| metadata_of(&r))
        .ok_or_else(|| anyhow!("No Wikimedia images are known for this game."))?;
    let list = if kind == "logo" { m.logos } else { m.covers };
    list.into_iter()
        .find(|i| i.src == src)
        .ok_or_else(|| anyhow!("That image is not one of this game's Wikimedia images."))
}

/// Uses an offered Commons logo as the game's logo (the user's explicit choice).
pub fn use_logo(
    state: &AppState,
    game_id: &str,
    src: &str,
) -> Result<crate::types::GamePageCustom> {
    let conn = state.db.lock();
    let image = offered(&conn, game_id, src, "logo")?;
    put_images(&conn, &[&image], now_ms())?;
    crate::art::set_image(
        &conn,
        &state.covers_dir(),
        game_id,
        crate::art::Slot::Logo,
        Some(&image.src),
    )
}

/// Uses an offered Commons cover as the game's cover (the user's explicit choice).
pub async fn use_cover(state: &AppState, game_id: &str, src: &str) -> Result<Vec<Game>> {
    let image = offered(&state.db.lock(), game_id, src, "cover")?;
    let file = download_images(state, std::slice::from_ref(&image.src))
        .await
        .remove(&image.src);
    let conn = state.db.lock();
    if db::get_game(&conn, game_id)?.is_none() {
        bail!("Game not found.");
    }
    put_images(&conn, &[&image], now_ms())?;
    let value = crate::covercache::adopt(&conn, &state.covers_dir(), &image.src, file.as_deref())?;
    let games = db::save_game(
        &conn,
        &GamePatch {
            cover: Some(value),
            ..GamePatch::new(game_id)
        },
    )?;
    if let Err(e) = crate::covercache::sweep(&conn, &state.covers_dir()) {
        log::warn!("Could not clean up the cover cache: {e:#}");
    }
    Ok(games)
}

/// Fills in every game whose cache is missing or stale, in the background, in batches (one at a
/// time; a request made while one runs makes it go around once more). Stops when the provider
/// is turned off, when Wikimedia limits requests, or after a few failed batches in a row.
pub fn spawn_auto(app: &AppHandle, delay: Duration) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let state = app.state::<AppState>();
        if state.wikimedia.rate_limited() {
            return;
        }
        state.wikimedia.auto_again.store(true, Ordering::SeqCst);
        if state.wikimedia.auto_running.swap(true, Ordering::SeqCst) {
            return;
        }
        while state.wikimedia.auto_again.swap(false, Ordering::SeqCst) {
            auto_pass(&app).await;
        }
        state.wikimedia.auto_running.store(false, Ordering::SeqCst);
    });
}

async fn auto_pass(app: &AppHandle) {
    let state = app.state::<AppState>();
    let games: Vec<Game> = {
        let conn = state.db.lock();
        if !enabled(&conn) {
            return;
        }
        let now = now_ms();
        db::all_games(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|g| {
                cached(&conn, &g.id)
                    .ok()
                    .flatten()
                    .is_none_or(|r| !is_fresh(&r, now))
            })
            .collect()
    };
    let mut failures = 0;
    let mut done = 0;
    for chunk in games.chunks(BATCH) {
        if !enabled(&state.db.lock()) || state.wikimedia.rate_limited() {
            break;
        }
        let todo: Vec<&Game> = chunk
            .iter()
            .filter(|g| !state.wikimedia.recently_failed(&g.id) && state.wikimedia.begin(&g.id))
            .collect();
        if todo.is_empty() {
            continue;
        }
        let inputs: Vec<GameInput> = {
            let conn = state.db.lock();
            todo.iter().map(|g| input_for(&conn, g)).collect()
        };
        let result = run_batch(app, &inputs).await;
        for g in &todo {
            state.wikimedia.end(&g.id);
            state.wikimedia.set_failed(&g.id, result.is_err());
        }
        match result {
            Ok(()) => {
                failures = 0;
                done += todo.len();
            }
            Err(WikiError::RateLimited) => {
                log::warn!("Wikimedia is limiting requests; the background lookup stops for now.");
                break;
            }
            Err(e) => {
                // One odd answer is not an outage; repeated network or server errors are.
                if e.is_outage() {
                    failures += 1;
                }
                log::warn!("Wikimedia lookup failed for a batch: {e}");
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    log::warn!("Wikimedia: stopped after {failures} failed batches in a row.");
                    break;
                }
            }
        }
    }
    if done > 0 {
        log::info!("Wikimedia: looked up {done} games.");
    }
}

#[cfg(test)]
mod tests;
