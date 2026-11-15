//! Local cover cache: remote library covers are downloaded once into the covers folder and
//! shown from there (`lobby-cover://cache-<url hash>-<content hash>.<ext>`), instead of being
//! fetched from a CDN on every start.
//!
//! - **Index**: table `cover_cache(url, file, fetchedAt)` maps each original remote URL to its
//!   file. `games.cover` / `games.defaultCover` hold the local value once cached; the original
//!   URL stays identifiable through the index ([`source_of`]), e.g. for backups. One URL always
//!   maps to one file, so `cover == defaultCover` (not customized) is preserved: both columns are
//!   switched in one statement ([`commit`]), and a reset (`cover = defaultCover`) needs no
//!   download.
//! - **When**: applying a SteamGridDB cover downloads it right away ([`cache_now`]); everything
//!   else (store imports, the bulk SteamGridDB fetch, `library_save` with a remote cover, backups,
//!   covers saved by older versions) is picked up by a background pass ([`spawn_pass`]) that
//!   runs a few seconds after start-up and after those changes, [`CONCURRENCY`] downloads at a
//!   time, emitting `library:changed` in batches. A failed download leaves the remote URL as it
//!   is; failures are retried later with a growing delay (and at most [`MAX_ATTEMPTS`] times per
//!   run of the app).
//! - **Safety**: https only, from [`ALLOWED_HOSTS`] (redirects only between allowed hosts), a
//!   size cap enforced while streaming, a timeout, content checked by its magic bytes, file
//!   names derived only from SHA-256 hashes, written to a temporary file and renamed into place
//!   inside the covers folder.
//! - **Size**: large static PNG/JPEG/WebP covers are scaled down to fit 600x900 and re-encoded
//!   (JPEG q85, PNG when the image has transparency). Animated images (APNG, animated WebP,
//!   GIF) and AVIF are stored byte for byte.
//! - **Clean-up**: [`sweep`] deletes cache files no game refers to any more. Only files listed
//!   in the index (or, at start-up, files matching the exact cache-name pattern) are ever
//!   deleted; user uploads are never touched.

use crate::art::{sniff_image, MAX_IMAGE_BYTES};
use crate::db::Db;
use crate::state::AppState;
use crate::validation::LOBBY_COVER_PREFIX;
use anyhow::Result;
use futures_util::StreamExt;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

/// Hosts covers may be downloaded from (https, default port only). The Steam importer and the
/// game page use `cdn.akamai.steamstatic.com`; SteamGridDB serves images from `cdn2`; IGDB
/// screenshots and artworks come from `images.igdb.com`.
pub const ALLOWED_HOSTS: [&str; 7] = [
    "cdn2.steamgriddb.com",
    "steamgriddb.com",
    "cdn.akamai.steamstatic.com",
    "cdn.cloudflare.steamstatic.com",
    "shared.akamai.steamstatic.com",
    "steamcdn-a.akamaihd.net",
    crate::igdb::IMAGE_HOST,
];
/// Downloads running at the same time in a background pass.
pub const CONCURRENCY: usize = 4;
/// Downloads per URL in one run of the app before it is left alone until the next start.
pub const MAX_ATTEMPTS: u32 = 4;
const TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
const MAX_URL_LEN: usize = 2048;
/// Delay before the first pass after start-up, so it never competes with the first paint.
pub const STARTUP_DELAY: Duration = Duration::from_secs(4);
/// Delays before retrying failed downloads (by retry round).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(60 * 60),
];
/// `library:changed` is emitted after this many cached URLs, or this long after the last one.
const BATCH_SIZE: usize = 8;
const BATCH_INTERVAL: Duration = Duration::from_millis(1500);
/// Static images above this size, or larger than [`MAX_WIDTH`]x[`MAX_HEIGHT`], are shrunk.
pub const SHRINK_ABOVE_BYTES: usize = 1_500_000;
pub const MAX_WIDTH: u32 = 600;
pub const MAX_HEIGHT: u32 = 900;
const JPEG_QUALITY: u8 = 85;
/// Untracked files younger than this are left alone by the start-up sweep (a download may be
/// about to record them).
const FILE_SWEEP_MIN_AGE: Duration = Duration::from_secs(10 * 60);

const FILE_PREFIX: &str = "cache-";
static CACHE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^cache-[0-9a-f]{16}-[0-9a-f]{12}\.(png|jpg|webp|gif|avif)$").unwrap()
});
static CACHE_TEMP_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^cache-[0-9a-f]{16}-[0-9a-f]{12}\.(png|jpg|webp|gif|avif)\.[0-9a-f-]{36}\.tmp$")
        .unwrap()
});

/// Decides whether a URL (the first request and every redirect) may be fetched.
pub type Allow = fn(&Url) -> bool;

// ---------------------------------------------------------------------------
// Which URLs are cached

/// https on an [`ALLOWED_HOSTS`] host, default port, no credentials.
pub fn is_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|h| ALLOWED_HOSTS.contains(&h.to_ascii_lowercase().as_str()))
}

/// The parsed URL when a stored cover value is a remote image that `allow` lets us cache.
pub fn cacheable(raw: &str, allow: Allow) -> Option<Url> {
    if raw.len() > MAX_URL_LEN || raw.starts_with(LOBBY_COVER_PREFIX) {
        return None;
    }
    Url::parse(raw).ok().filter(allow)
}

/// `lobby-cover://<file>` for a cache file.
pub fn local_value(file: &str) -> String {
    format!("{LOBBY_COVER_PREFIX}{file}")
}

/// Whether `name` is exactly a cache file name (never a user upload).
pub fn is_cache_file_name(name: &str) -> bool {
    CACHE_NAME.is_match(name)
}

// ---------------------------------------------------------------------------
// Downloading

#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The URL (or a redirect) is not https on an allowed host.
    NotAllowed,
    Http(u16),
    TooLarge,
    NotImage,
    Network(String),
    Io(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowed => f.write_str("the image host is not allowed"),
            Self::Http(code) => write!(f, "HTTP {code}"),
            Self::TooLarge => f.write_str("the image is too large"),
            Self::NotImage => f.write_str("the download is not a supported image"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Io(msg) => write!(f, "could not save the image: {msg}"),
        }
    }
}

impl std::error::Error for FetchError {}

fn network(e: reqwest::Error) -> FetchError {
    if e.is_redirect() {
        FetchError::NotAllowed
    } else {
        // Without the URL: errors are logged, and URLs are not secrets here, but stay tidy.
        FetchError::Network(e.without_url().to_string())
    }
}

/// An HTTP client that follows at most [`MAX_REDIRECTS`] redirects, and only to URLs `allow`
/// accepts. No credentials are ever attached.
pub fn http_client(allow: Allow, timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if allow(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("redirect to a host that is not allowed")
            }
        }))
        .user_agent(concat!("GameLobby/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Downloads `raw` with at most `max_bytes` of body, checked while streaming.
pub async fn download(
    client: &reqwest::Client,
    raw: &str,
    allow: Allow,
    max_bytes: u64,
) -> Result<Vec<u8>, FetchError> {
    let url = Url::parse(raw).map_err(|_| FetchError::NotAllowed)?;
    if !allow(&url) {
        return Err(FetchError::NotAllowed);
    }
    let mut response = client.get(url).send().await.map_err(network)?;
    if !allow(response.url()) {
        return Err(FetchError::NotAllowed);
    }
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if (body.len() + chunk.len()) as u64 > max_bytes {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Content checks and downscaling

/// The image format by magic bytes: [`sniff_image`]'s formats plus GIF.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    sniff_image(bytes)
        .or_else(|| (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")).then_some("gif"))
}

/// APNG: an `acTL` chunk before the first `IDAT`.
pub fn is_apng(bytes: &[u8]) -> bool {
    let Some(mut rest) = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n".as_slice()) else {
        return false;
    };
    while rest.len() >= 8 {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        match &rest[4..8] {
            b"acTL" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        let Some(next) = len.checked_add(12).filter(|n| *n <= rest.len()) else {
            return false;
        };
        rest = &rest[next..];
    }
    false
}

/// Animated WebP: the animation flag in `VP8X`, or an `ANIM`/`ANMF` chunk.
pub fn is_animated_webp(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut rest = &bytes[12..];
    while rest.len() >= 8 {
        let fourcc = &rest[..4];
        let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        match fourcc {
            b"ANIM" | b"ANMF" => return true,
            b"VP8X" => return rest.get(8).is_some_and(|flags| flags & 0x02 != 0),
            _ => {}
        }
        let Some(next) = len.checked_add(8 + (len & 1)).filter(|n| *n <= rest.len()) else {
            return false;
        };
        rest = &rest[next..];
    }
    false
}

/// Whether the bytes must be stored unchanged: animations (and GIFs, which may be), and AVIF,
/// which isn't decoded here.
pub fn keep_original(bytes: &[u8], ext: &str) -> bool {
    match ext {
        "png" => is_apng(bytes),
        "webp" => is_animated_webp(bytes),
        "jpg" => false,
        _ => true,
    }
}

/// The largest size a cached image is kept at: covers fit a poster card, game screenshots
/// (from `images.igdb.com`) fit a 1080p viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub width: u32,
    pub height: u32,
    pub shrink_above_bytes: usize,
}

pub const COVER_BOUNDS: Bounds = Bounds {
    width: MAX_WIDTH,
    height: MAX_HEIGHT,
    shrink_above_bytes: SHRINK_ABOVE_BYTES,
};
pub const SCREENSHOT_BOUNDS: Bounds = Bounds {
    width: 1920,
    height: 1080,
    shrink_above_bytes: 4_000_000,
};

/// Screenshot bounds for IGDB images, cover bounds for everything else.
pub fn bounds_for(url: &str) -> Bounds {
    let igdb = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.eq_ignore_ascii_case("images.igdb.com"))
        })
        .unwrap_or(false);
    if igdb {
        SCREENSHOT_BOUNDS
    } else {
        COVER_BOUNDS
    }
}

/// A smaller re-encoding of a static PNG/JPEG/WebP when it is larger than the bounds (size or
/// bytes); `None` keeps the original (small enough, undecodable, or re-encoding would not help).
pub fn shrink_to(bytes: &[u8], ext: &str, bounds: Bounds) -> Option<(Vec<u8>, &'static str)> {
    use image::{ImageFormat, ImageReader, Limits};
    use std::io::Cursor;
    let format = match ext {
        "png" => ImageFormat::Png,
        "jpg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::WebP,
        _ => return None,
    };
    let reader = || {
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(12_000);
        limits.max_image_height = Some(12_000);
        limits.max_alloc = Some(512 * 1024 * 1024);
        reader.limits(limits);
        reader
    };
    let (width, height) = reader().into_dimensions().ok()?;
    let oversized = width > bounds.width || height > bounds.height;
    if !oversized && bytes.len() <= bounds.shrink_above_bytes {
        return None;
    }
    let mut img = reader().decode().ok()?;
    if oversized {
        img = img.resize(
            bounds.width,
            bounds.height,
            image::imageops::FilterType::CatmullRom,
        );
    }
    let transparent = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255);
    let mut out = Vec::new();
    let ext = if transparent {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .ok()?;
        "png"
    } else {
        let rgb = img.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&rgb)
            .ok()?;
        "jpg"
    };
    if !oversized && out.len() >= bytes.len() {
        return None;
    }
    Some((out, ext))
}

/// Checks the content and shrinks it when worthwhile: the bytes to store and their extension.
#[cfg(test)]
pub fn prepare(bytes: Vec<u8>) -> Result<(Vec<u8>, &'static str), FetchError> {
    prepare_with(bytes, COVER_BOUNDS)
}

/// [`prepare`] with explicit bounds.
pub fn prepare_with(bytes: Vec<u8>, bounds: Bounds) -> Result<(Vec<u8>, &'static str), FetchError> {
    let ext = sniff(&bytes).ok_or(FetchError::NotImage)?;
    if keep_original(&bytes, ext) {
        return Ok((bytes, ext));
    }
    Ok(shrink_to(&bytes, ext, bounds).unwrap_or((bytes, ext)))
}

// ---------------------------------------------------------------------------
// Files

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `cache-<sha256(url)[..16]>-<sha256(content)[..12]>.<ext>`: nothing from the remote name, and
/// new content always gets a new name (so the web view never shows a stale image).
pub fn file_name(url: &str, content: &[u8], ext: &str) -> String {
    let url_hash = hex(&Sha256::digest(url.as_bytes()));
    let content_hash = hex(&Sha256::digest(content));
    format!(
        "{FILE_PREFIX}{}-{}.{ext}",
        &url_hash[..16],
        &content_hash[..12]
    )
}

/// Writes `bytes` as `covers_dir/name` through a temporary file in the same folder. `name` must
/// be a cache file name (so it can't point anywhere else). An existing regular file of that
/// name already has this content (the name includes its hash) and is kept.
pub fn write_file(covers_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), FetchError> {
    let io = |e: std::io::Error| FetchError::Io(e.to_string());
    if !is_cache_file_name(name) {
        return Err(FetchError::Io("invalid file name".into()));
    }
    std::fs::create_dir_all(covers_dir).map_err(io)?;
    let target = covers_dir.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_file() => return Ok(()),
        Ok(_) => return Err(FetchError::Io("the cache path is not a file".into())),
        Err(_) => {}
    }
    let temp = covers_dir.join(format!("{name}.{}.tmp", uuid::Uuid::new_v4()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&temp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        // Another download of the same URL may have won the race with the same content.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_file()) {
            return Ok(());
        }
        return Err(io(e));
    }
    Ok(())
}

/// Downloads, checks, shrinks and stores one URL; returns the cache file name.
pub async fn fetch_to_file(
    client: &reqwest::Client,
    covers_dir: &Path,
    url: &str,
    allow: Allow,
) -> Result<String, FetchError> {
    let bytes = download(client, url, allow, MAX_IMAGE_BYTES).await?;
    let covers_dir = covers_dir.to_path_buf();
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let (bytes, ext) = prepare_with(bytes, bounds_for(&url))?;
        let name = file_name(&url, &bytes, ext);
        write_file(&covers_dir, &name, &bytes)?;
        Ok(name)
    })
    .await
    .map_err(|e| FetchError::Io(e.to_string()))?
}

// ---------------------------------------------------------------------------
// Index (table `cover_cache`)

/// The cache file recorded for `url`, if any.
pub fn lookup(conn: &Connection, url: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT file FROM cover_cache WHERE url=?", [url], |r| {
            r.get(0)
        })
        .optional()?)
}

/// The cache file for `url` when it is recorded and still on disk.
pub fn existing_file(conn: &Connection, covers_dir: &Path, url: &str) -> Result<Option<String>> {
    Ok(lookup(conn, url)?.filter(|file| {
        is_cache_file_name(file)
            && std::fs::symlink_metadata(covers_dir.join(file)).is_ok_and(|m| m.is_file())
    }))
}

/// The original remote URL behind a stored cover value (the value itself when it isn't a
/// cache file).
pub fn source_of(conn: &Connection, value: &str) -> Result<String> {
    let Some(file) = value.strip_prefix(LOBBY_COVER_PREFIX) else {
        return Ok(value.to_string());
    };
    let url = conn
        .query_row("SELECT url FROM cover_cache WHERE file=?", [file], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(url.unwrap_or_else(|| value.to_string()))
}

/// Records `url -> file` and points every game whose cover or default cover is exactly `url`
/// at the file (both columns in one statement, so `cover == defaultCover` stays true). Returns
/// the number of games changed.
pub fn commit(conn: &Connection, url: &str, file: &str) -> Result<usize> {
    let local = local_value(file);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO cover_cache(url,file,fetchedAt) VALUES(?1,?2,?3)
         ON CONFLICT(url) DO UPDATE SET file=excluded.file,fetchedAt=excluded.fetchedAt",
        params![url, file, crate::types::now_ms()],
    )?;
    let changed = tx.execute(
        "UPDATE games SET cover=CASE WHEN cover=?1 THEN ?2 ELSE cover END,
                          defaultCover=CASE WHEN defaultCover=?1 THEN ?2 ELSE defaultCover END
         WHERE cover=?1 OR defaultCover=?1",
        params![url, local],
    )?;
    tx.commit()?;
    Ok(changed)
}

/// Games showing a cache file that is gone from disk (deleted by hand, say) go back to the
/// original URL, so the next pass downloads it again. Returns the number of games changed.
pub fn restore_missing(conn: &Connection, covers_dir: &Path) -> Result<usize> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT url,file FROM cover_cache")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = 0;
    for (url, file) in rows {
        if std::fs::symlink_metadata(covers_dir.join(&file)).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let local = local_value(&file);
        changed += conn.execute(
            "UPDATE games SET cover=CASE WHEN cover=?1 THEN ?2 ELSE cover END,
                              defaultCover=CASE WHEN defaultCover=?1 THEN ?2 ELSE defaultCover END
             WHERE cover=?1 OR defaultCover=?1",
            params![local, url],
        )?;
        conn.execute("DELETE FROM cover_cache WHERE url=?", [&url])?;
    }
    Ok(changed)
}

/// Distinct remote cover/default cover values that `allow` lets us cache.
pub fn remote_urls(conn: &Connection, allow: Allow) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT cover FROM games WHERE cover LIKE 'http%'
         UNION SELECT defaultCover FROM games WHERE defaultCover LIKE 'http%'",
    )?;
    let values: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(values
        .into_iter()
        .filter(|v| cacheable(v, allow).is_some())
        .collect())
}

/// File names referenced by any game (cover/default cover) or game page customization.
fn referenced_files(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT cover FROM games UNION SELECT defaultCover FROM games
         UNION SELECT hero FROM game_page_custom UNION SELECT logo FROM game_page_custom",
    )?;
    let values: Vec<Option<String>> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(values
        .into_iter()
        .flatten()
        .filter_map(|v| v.strip_prefix(LOBBY_COVER_PREFIX).map(str::to_owned))
        .collect())
}

/// Deletes cache files (and their index rows) that nothing refers to any more. Only files
/// recorded in the index with a cache file name are deleted. Returns how many were removed.
pub fn sweep(conn: &Connection, covers_dir: &Path) -> Result<usize> {
    let referenced = referenced_files(conn)?;
    // IGDB screenshots and artworks of cached game metadata stay too.
    let referenced_urls = crate::igdb::referenced_image_urls(conn)?;
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT url,file FROM cover_cache")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut removed = 0;
    for (url, file) in rows {
        if referenced.contains(&file) || referenced_urls.contains(&url) {
            continue;
        }
        conn.execute("DELETE FROM cover_cache WHERE url=?", [&url])?;
        if is_cache_file_name(&file) && std::fs::remove_file(covers_dir.join(&file)).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Start-up clean-up of the files themselves: leftover temporary files, and cache-named files
/// that are neither indexed nor referenced, and custom launcher icons no launcher uses (all only
/// when older than a few minutes, so an icon just chosen but not saved yet is kept).
pub fn sweep_files(conn: &Connection, covers_dir: &Path) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(covers_dir) else {
        return Ok(0);
    };
    let referenced = referenced_files(conn)?;
    let indexed: HashSet<String> = conn
        .prepare("SELECT file FROM cover_cache")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let icons = crate::launchers::referenced_icons(conn)?;
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let orphan = CACHE_TEMP_NAME.is_match(&name)
            || (is_cache_file_name(&name)
                && !indexed.contains(&name)
                && !referenced.contains(&name))
            // Icons of deleted launchers, or replaced by another image.
            || (crate::launchers::icon_file_ok(&name) && !icons.contains(&name));
        if !orphan {
            continue;
        }
        let old_enough = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= FILE_SWEEP_MIN_AGE);
        if old_enough && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Background pass

#[derive(Debug, Default, PartialEq)]
pub struct PassOutcome {
    /// URLs now cached (downloaded or already on disk).
    pub cached: usize,
    /// Games switched to a local file.
    pub games_changed: usize,
    /// URLs whose download failed (their remote URL stays in place).
    pub failed: Vec<String>,
}

/// Caches every remote cover in the library. `skip` excludes URLs (e.g. after too many failed
/// attempts); `on_batch` runs after each batch of switched games and once at the end, when
/// anything changed. The database lock is never held across a download.
pub async fn run_pass(
    db: &Db,
    covers_dir: &Path,
    client: &reqwest::Client,
    allow: Allow,
    skip: &(dyn Fn(&str) -> bool + Sync),
    on_batch: &mut (dyn FnMut() + Send),
) -> Result<PassOutcome> {
    let mut outcome = PassOutcome::default();
    let mut pending = 0usize;
    let mut todo = Vec::new();
    {
        let conn = db.lock();
        pending += restore_missing(&conn, covers_dir)?;
        for url in remote_urls(&conn, allow)? {
            if let Some(file) = existing_file(&conn, covers_dir, &url)? {
                // Already downloaded (e.g. the remote URL was saved again): no request needed.
                let changed = commit(&conn, &url, &file)?;
                outcome.cached += 1;
                outcome.games_changed += changed;
                pending += changed;
            } else if !skip(&url) {
                todo.push(url);
            }
        }
    }
    let mut last_emit = Instant::now();
    let mut downloads = futures_util::stream::iter(todo)
        .map(|url| async move {
            let result = fetch_to_file(client, covers_dir, &url, allow).await;
            (url, result)
        })
        .buffer_unordered(CONCURRENCY);
    while let Some((url, result)) = downloads.next().await {
        match result {
            Ok(file) => {
                // Recorded only if the file is still there (a sweep may have run meanwhile).
                match commit_present(&db.lock(), covers_dir, &url, &file)? {
                    Some(changed) => {
                        outcome.cached += 1;
                        outcome.games_changed += changed;
                        pending += changed;
                    }
                    None => outcome.failed.push(url),
                }
            }
            Err(e) => {
                log::warn!("Could not cache a cover: {e}");
                outcome.failed.push(url);
            }
        }
        if pending >= BATCH_SIZE || (pending > 0 && last_emit.elapsed() >= BATCH_INTERVAL) {
            on_batch();
            pending = 0;
            last_emit = Instant::now();
        }
    }
    drop(downloads);
    if let Err(e) = sweep(&db.lock(), covers_dir) {
        log::warn!("Could not clean up the cover cache: {e:#}");
    }
    if pending > 0 {
        on_batch();
    }
    Ok(outcome)
}

/// Run-time state of the cache (in [`AppState`]).
#[derive(Default)]
pub struct CacheState {
    running: AtomicBool,
    rerun: AtomicBool,
    retry_scheduled: AtomicBool,
    attempts: Mutex<HashMap<String, u32>>,
    client: OnceLock<reqwest::Client>,
}

impl CacheState {
    fn client(&self) -> Result<&reqwest::Client, FetchError> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }
        let client = http_client(is_allowed, TIMEOUT).map_err(network)?;
        Ok(self.client.get_or_init(|| client))
    }

    fn attempts(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
        self.attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Downloads `url` into the cache now (for a cover the user just chose), or finds it there.
/// Nothing is recorded yet: the caller records it with [`adopt`] in the same database lock as
/// the save, so a concurrent [`sweep`] can't remove the file in between. `None` when the URL
/// isn't cacheable or the download failed (the background pass tries again later).
pub async fn download_now(state: &AppState, url: &str) -> Option<String> {
    cacheable(url, is_allowed)?;
    let covers_dir = state.covers_dir();
    if let Ok(Some(file)) = existing_file(&state.db.lock(), &covers_dir, url) {
        return Some(file);
    }
    let result = match state.covers.client() {
        Ok(client) => fetch_to_file(client, &covers_dir, url, is_allowed).await,
        Err(e) => Err(e),
    };
    result
        .map_err(|e| log::warn!("Could not cache the chosen cover: {e}"))
        .ok()
}

/// The value to store for `url` given [`download_now`]'s result: the local file (recorded in
/// the index) when it is still on disk, otherwise `url` itself. Call with the lock held for
/// the save.
pub fn adopt(
    conn: &Connection,
    covers_dir: &Path,
    url: &str,
    file: Option<&str>,
) -> Result<String> {
    if let Some(file) = file {
        if commit_present(conn, covers_dir, url, file)?.is_some() {
            return Ok(local_value(file));
        }
    }
    Ok(url.to_string())
}

/// [`commit`] when `file` is a cache file that is on disk; `None` (nothing changed) otherwise.
fn commit_present(
    conn: &Connection,
    covers_dir: &Path,
    url: &str,
    file: &str,
) -> Result<Option<usize>> {
    let present = is_cache_file_name(file)
        && std::fs::symlink_metadata(covers_dir.join(file)).is_ok_and(|m| m.is_file());
    Ok(if present {
        Some(commit(conn, url, file)?)
    } else {
        None
    })
}

/// The local value for a remote cover that is already cached, else the value unchanged.
pub fn cached_value(conn: &Connection, covers_dir: &Path, value: &str) -> String {
    if cacheable(value, is_allowed).is_none() {
        return value.to_string();
    }
    match existing_file(conn, covers_dir, value) {
        Ok(Some(file)) => local_value(&file),
        _ => value.to_string(),
    }
}

/// Whether a cover value is a remote image a pass would cache.
pub fn needs_caching(value: Option<&str>) -> bool {
    value.is_some_and(|v| cacheable(v, is_allowed).is_some())
}

/// Removes unreferenced cache files after a cover change or removal. Never fails the caller.
pub fn clean_up(state: &AppState) {
    if let Err(e) = sweep(&state.db.lock(), &state.covers_dir()) {
        log::warn!("Could not clean up the cover cache: {e:#}");
    }
    // Games removed or merged may also leave screenshots behind (their own folder).
    crate::screenshots::clean_up(state);
}

/// Starts a background pass after `delay`. If one is running, it runs once more when done.
pub fn spawn_pass(app: &AppHandle, delay: Duration) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        run_until_idle(&app).await;
    });
}

/// Start-up: cleans leftover files, then caches the library's remote covers. Never blocks.
pub fn spawn_startup(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        {
            let state = app.state::<AppState>();
            let covers_dir = state.covers_dir();
            let conn = state.db.lock();
            if let Err(e) = sweep_files(&conn, &covers_dir).and_then(|_| sweep(&conn, &covers_dir))
            {
                log::warn!("Could not clean up the cover cache: {e:#}");
            }
            let shots = state.screenshots_dir();
            if let Err(e) = crate::screenshots::sweep_files(&conn, &shots) {
                log::warn!("Could not clean up screenshots: {e:#}");
            }
        }
        run_until_idle(&app).await;
    });
}

async fn run_until_idle(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cache = &state.covers;
    loop {
        cache.rerun.store(true, Ordering::SeqCst);
        // One pass at a time; a request made while one runs makes it go around once more.
        loop {
            if cache.running.swap(true, Ordering::SeqCst) {
                return;
            }
            while cache.rerun.swap(false, Ordering::SeqCst) {
                pass_once(app, &state).await;
            }
            cache.running.store(false, Ordering::SeqCst);
            if !cache.rerun.load(Ordering::SeqCst) {
                break;
            }
        }
        // Failed downloads: this task waits and tries again (one waiting task at most).
        let pending = failed_pending(&state);
        if pending.is_empty() || cache.retry_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let round = pending.iter().map(|(_, n)| *n).min().unwrap_or(1).max(1) - 1;
        tokio::time::sleep(RETRY_DELAYS[(round as usize).min(RETRY_DELAYS.len() - 1)]).await;
        cache.retry_scheduled.store(false, Ordering::SeqCst);
    }
}

/// URLs that failed and may still be retried, with their attempt counts.
fn failed_pending(state: &AppState) -> Vec<(String, u32)> {
    state
        .covers
        .attempts()
        .iter()
        .filter(|(_, n)| **n > 0 && **n < MAX_ATTEMPTS)
        .map(|(url, n)| (url.clone(), *n))
        .collect()
}

async fn pass_once(app: &AppHandle, state: &AppState) {
    let client = match state.covers.client() {
        Ok(client) => client,
        Err(e) => {
            log::warn!("Could not start the cover download client: {e}");
            return;
        }
    };
    let skip = |url: &str| {
        state
            .covers
            .attempts()
            .get(url)
            .is_some_and(|n| *n >= MAX_ATTEMPTS)
    };
    let emitter = app.clone();
    let mut on_batch = move || {
        let _ = emitter.emit("library:changed", ());
    };
    let outcome = run_pass(
        &state.db,
        &state.covers_dir(),
        client,
        is_allowed,
        &skip,
        &mut on_batch,
    )
    .await;
    match outcome {
        Ok(outcome) => {
            let mut attempts = state.covers.attempts();
            for url in &outcome.failed {
                *attempts.entry(url.clone()).or_default() += 1;
            }
            // Cached or no longer used: forget; still failing or given up: keep counting.
            attempts.retain(|url, n| *n >= MAX_ATTEMPTS || outcome.failed.contains(url));
            if outcome.cached > 0 || !outcome.failed.is_empty() {
                log::info!(
                    "Cover cache: {} cached, {} failed.",
                    outcome.cached,
                    outcome.failed.len()
                );
            }
        }
        Err(e) => log::warn!("Cover cache pass failed: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::types::GamePatch;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("game-lobby-covers-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Test rule: plain http to 127.0.0.1 only (no real network in tests).
    fn local(url: &Url) -> bool {
        url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
    }

    fn client(timeout: Duration) -> reqwest::Client {
        http_client(local, timeout).unwrap()
    }

    /// A raw HTTP/1.1 response; `None` from the handler keeps the connection silent.
    fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        let mut out = out.into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            &[("Content-Length", body.len().to_string())],
            body,
        )
    }

    type Handler = Arc<dyn Fn(&str, u16) -> Option<Vec<u8>> + Send + Sync>;

    /// Serves `handler(path, port)` on 127.0.0.1; returns the base URL.
    async fn serve(handler: Handler) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    match handler(&path, port) {
                        Some(bytes) => {
                            let _ = socket.write_all(&bytes).await;
                            let _ = socket.shutdown().await;
                        }
                        None => tokio::time::sleep(Duration::from_secs(5)).await,
                    }
                });
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let img = if alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, _| {
                image::Rgba([(x % 256) as u8, 40, 90, if x % 2 == 0 { 0 } else { 255 }])
            }))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
            }))
        };
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn png_with(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in chunks {
            out.extend(chunk(kind, data));
        }
        out
    }

    fn webp_with(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (kind, data) in chunks {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn sweep_keeps_igdb_images_of_cached_metadata() {
        let dir = TempDir::new();
        let db = Db::memory().unwrap();
        let conn = db.lock();
        crate::db::save_game(
            &conn,
            &crate::types::GamePatch {
                title: Some("G".into()),
                store: Some("Steam".into()),
                ..crate::types::GamePatch::new("g")
            },
        )
        .unwrap();
        let meta = serde_json::json!({"igdbId": 1, "name": "G", "screenshots": ["s1"], "shape": crate::igdb::CACHE_SHAPE});
        crate::db::put_metadata(
            &conn,
            "g",
            &crate::db::CachedMetadata {
                igdb_id: Some(1),
                json: Some(meta.to_string()),
                fetched_at: 1,
                source: "auto".into(),
            },
        )
        .unwrap();
        let kept = "https://images.igdb.com/igdb/image/upload/t_1080p/s1.jpg";
        let dropped = "https://images.igdb.com/igdb/image/upload/t_1080p/old.jpg";
        for (url, body) in [(kept, b"one".as_slice()), (dropped, b"two".as_slice())] {
            let name = file_name(url, body, "jpg");
            std::fs::write(dir.0.join(&name), body).unwrap();
            commit(&conn, url, &name).unwrap();
        }
        assert_eq!(sweep(&conn, &dir.0).unwrap(), 1);
        assert!(lookup(&conn, kept).unwrap().is_some());
        assert!(lookup(&conn, dropped).unwrap().is_none());
        // Once the metadata is gone, its images go too.
        crate::db::delete_metadata(&conn, "g").unwrap();
        assert_eq!(sweep(&conn, &dir.0).unwrap(), 1);
    }

    #[test]
    fn only_https_covers_on_allowed_hosts_are_cached() {
        for ok in [
            "https://cdn2.steamgriddb.com/grid/abc.png",
            "https://steamgriddb.com/x.jpg",
            "https://cdn.akamai.steamstatic.com/steam/apps/400/library_600x900.jpg",
            "https://CDN.Cloudflare.SteamStatic.com/steam/apps/400/library_600x900.jpg",
            "https://shared.akamai.steamstatic.com/a.jpg",
            "https://steamcdn-a.akamaihd.net/steam/apps/400/header.jpg",
            "https://images.igdb.com/igdb/image/upload/t_screenshot_big/sc6lrc.jpg",
        ] {
            assert!(cacheable(ok, is_allowed).is_some(), "{ok} should be cached");
        }
        let long = format!("https://cdn2.steamgriddb.com/{}", "a".repeat(2100));
        for bad in [
            "http://cdn2.steamgriddb.com/grid/abc.png",
            "https://cdn2.steamgriddb.com:8443/grid/abc.png",
            "https://user:pw@cdn2.steamgriddb.com/grid/abc.png",
            "https://cdn3.steamgriddb.com/grid/abc.png",
            "https://evilsteamgriddb.com/x.png",
            "https://cdn2.steamgriddb.com.evil.example/x.png",
            "https://evil.example/?u=cdn2.steamgriddb.com",
            "https://cdn1.epicgames.com/x.jpg",
            "http://images.igdb.com/igdb/image/upload/t_thumb/x.jpg",
            "https://images.igdb.com.evil.example/x.jpg",
            "https://img.igdb.com/x.jpg",
            "file:///C:/Windows/win.ini",
            "lobby-cover://cache-0123456789abcdef-0123456789ab.png",
            "C:\\covers\\a.png",
            "",
            long.as_str(),
        ] {
            assert!(
                cacheable(bad, is_allowed).is_none(),
                "{bad} should not be cached"
            );
        }
    }

    #[tokio::test]
    async fn downloads_refuse_disallowed_urls_and_redirects_to_other_hosts() {
        let base = serve(Arc::new(|path, port| match path {
            "/image.png" => Some(ok(&png(4, 6, false))),
            // Same allowed host: followed.
            "/hop" => Some(response(
                "302 Found",
                &[
                    ("Location", "/image.png".into()),
                    ("Content-Length", "0".into()),
                ],
                b"",
            )),
            // Another host (localhost is not 127.0.0.1 for the test rule): refused.
            "/away" => Some(response(
                "302 Found",
                &[
                    ("Location", format!("http://localhost:{port}/image.png")),
                    ("Content-Length", "0".into()),
                ],
                b"",
            )),
            "/loop" => Some(response(
                "302 Found",
                &[("Location", "/loop".into()), ("Content-Length", "0".into())],
                b"",
            )),
            _ => Some(response(
                "404 Not Found",
                &[("Content-Length", "0".into())],
                b"",
            )),
        }))
        .await;
        let client = client(Duration::from_secs(5));
        let max = MAX_IMAGE_BYTES;
        assert!(download(&client, &format!("{base}/image.png"), local, max)
            .await
            .is_ok());
        assert!(download(&client, &format!("{base}/hop"), local, max)
            .await
            .is_ok());
        assert_eq!(
            download(&client, &format!("{base}/away"), local, max).await,
            Err(FetchError::NotAllowed)
        );
        assert_eq!(
            download(&client, &format!("{base}/loop"), local, max).await,
            Err(FetchError::NotAllowed)
        );
        assert_eq!(
            download(&client, &format!("{base}/missing.png"), local, max).await,
            Err(FetchError::Http(404))
        );
        // The production rule refuses plain http and unknown hosts before any request.
        assert_eq!(
            download(&client, &format!("{base}/image.png"), is_allowed, max).await,
            Err(FetchError::NotAllowed)
        );
        assert_eq!(
            download(&client, "not a url", local, max).await,
            Err(FetchError::NotAllowed)
        );
    }

    #[tokio::test]
    async fn the_size_cap_is_enforced_while_streaming() {
        let body = vec![7u8; 5000];
        let declared = body.clone();
        let base = serve(Arc::new(move |path, _| match path {
            // No Content-Length: only the streamed size can stop it.
            "/stream" => Some(response("200 OK", &[], &body)),
            "/declared" => Some(ok(&declared)),
            _ => Some(ok(&[1, 2, 3])),
        }))
        .await;
        let client = client(Duration::from_secs(5));
        for path in ["stream", "declared"] {
            assert_eq!(
                download(&client, &format!("{base}/{path}"), local, 1000).await,
                Err(FetchError::TooLarge),
                "{path}"
            );
            assert_eq!(
                download(&client, &format!("{base}/{path}"), local, 5000)
                    .await
                    .map(|b| b.len()),
                Ok(5000),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn a_silent_server_times_out() {
        let base = serve(Arc::new(|_, _| None)).await;
        let client = client(Duration::from_millis(300));
        let result = download(&client, &format!("{base}/slow.png"), local, 1000).await;
        assert!(matches!(result, Err(FetchError::Network(_))), "{result:?}");
    }

    #[tokio::test]
    async fn content_is_checked_by_magic_bytes() {
        let html = b"<!doctype html><html><body>Not an image</body></html>".to_vec();
        let page = html.clone();
        let base = serve(Arc::new(move |_, _| {
            Some(response(
                "200 OK",
                &[
                    ("Content-Type", "image/png".into()),
                    ("Content-Length", page.len().to_string()),
                ],
                &page,
            ))
        }))
        .await;
        let dir = TempDir::new();
        let client = client(Duration::from_secs(5));
        let result = fetch_to_file(&client, &dir.0, &format!("{base}/cover.png"), local).await;
        assert_eq!(result, Err(FetchError::NotImage));
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);

        assert_eq!(prepare(html), Err(FetchError::NotImage));
        assert_eq!(prepare(Vec::new()), Err(FetchError::NotImage));
        assert_eq!(sniff(b"GIF89a\x01\0\x01\0"), Some("gif"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
    }

    #[test]
    fn animations_are_detected_and_kept_byte_for_byte() {
        let ihdr: &[u8] = &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0];
        let apng = png_with(&[
            (b"IHDR", ihdr),
            (b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]),
            (b"IDAT", &[0]),
        ]);
        let late = png_with(&[(b"IHDR", ihdr), (b"IDAT", &[0]), (b"acTL", &[0; 8])]);
        assert!(is_apng(&apng));
        assert!(!is_apng(&late), "acTL after IDAT is not an animation");
        assert!(!is_apng(&png(3, 3, false)));
        assert!(!is_apng(&apng[..20]), "truncated");
        assert!(!is_apng(b"not a png"));

        let animated = webp_with(&[
            (b"VP8X", &[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (b"ANIM", &[0; 6]),
        ]);
        let anim_only = webp_with(&[(b"ICCP", &[1, 2, 3]), (b"ANIM", &[0; 6])]);
        let still = webp_with(&[
            (b"VP8X", &[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (b"VP8 ", &[0; 10]),
        ]);
        let simple = webp_with(&[(b"VP8L", &[0x2f, 0, 0, 0, 0])]);
        assert!(is_animated_webp(&animated));
        assert!(is_animated_webp(&anim_only));
        assert!(!is_animated_webp(&still));
        assert!(!is_animated_webp(&simple));
        assert!(!is_animated_webp(&animated[..14]));

        // A large animation is never re-encoded, even above the size limit.
        let mut big_apng = apng.clone();
        big_apng.extend(vec![0u8; SHRINK_ABOVE_BYTES + 10]);
        assert_eq!(prepare(big_apng.clone()), Ok((big_apng, "png")));
        assert_eq!(prepare(animated.clone()), Ok((animated, "webp")));
        let gif = b"GIF89a\x01\0\x01\0\0\0\0;".to_vec();
        assert_eq!(prepare(gif.clone()), Ok((gif, "gif")));
    }

    #[test]
    fn large_static_covers_are_scaled_down_to_fit() {
        let dims = |bytes: &[u8]| {
            image::load_from_memory(bytes)
                .map(|i| (i.width(), i.height()))
                .unwrap()
        };
        let (bytes, ext) = prepare(png(1200, 1200, false)).unwrap();
        assert_eq!((dims(&bytes), ext), ((600, 600), "jpg"));
        assert_eq!(sniff(&bytes), Some("jpg"));

        let (bytes, ext) = prepare(png(1000, 2000, true)).unwrap();
        assert_eq!(
            (dims(&bytes), ext),
            ((450, 900), "png"),
            "transparency keeps PNG"
        );

        let (bytes, ext) = prepare(png(1800, 900, false)).unwrap();
        assert_eq!((dims(&bytes), ext), ((600, 300), "jpg"));

        // IGDB screenshots keep up to 1080p; covers from other hosts still fit a poster card.
        let igdb = "https://images.igdb.com/igdb/image/upload/t_1080p/abc.jpg";
        assert_eq!(bounds_for(igdb), SCREENSHOT_BOUNDS);
        assert_eq!(
            bounds_for("https://cdn2.steamgriddb.com/grid/a.png"),
            COVER_BOUNDS
        );
        assert_eq!(
            bounds_for("https://images.igdb.com.evil.example/x.jpg"),
            COVER_BOUNDS
        );
        let wide = png(1280, 720, false);
        assert_eq!(
            dims(&prepare_with(wide.clone(), SCREENSHOT_BOUNDS).unwrap().0),
            (1280, 720)
        );
        let (bytes, _) = prepare_with(png(3840, 2160, false), SCREENSHOT_BOUNDS).unwrap();
        assert_eq!(dims(&bytes), (1920, 1080));

        // Within the box and small: stored as it is.
        let small = png(300, 450, false);
        assert_eq!(prepare(small.clone()), Ok((small, "png")));
        let exact = png(600, 900, false);
        assert!(exact.len() <= SHRINK_ABOVE_BYTES);
        assert_eq!(prepare(exact.clone()), Ok((exact, "png")));

        // Corrupt data that looks like a PNG is kept rather than lost.
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        broken.extend(vec![0xAB; SHRINK_ABOVE_BYTES + 1]);
        assert_eq!(prepare(broken.clone()), Ok((broken, "png")));
    }

    #[test]
    fn file_names_come_only_from_hashes() {
        let url = "https://cdn2.steamgriddb.com/grid/../../..%2F..%5Cevil/CON.png?x=/a\\b";
        let name = file_name(url, b"content", "png");
        assert!(is_cache_file_name(&name), "{name}");
        assert!(!name.contains(['/', '\\', ':']) && !name.contains(".."));
        assert!(crate::validation::lobby_cover_name(&local_value(&name)).is_some());
        assert_eq!(name, file_name(url, b"content", "png"), "stable");
        assert_ne!(
            name,
            file_name(url, b"other", "png"),
            "new content, new name"
        );
        assert_ne!(
            name,
            file_name("https://cdn2.steamgriddb.com/b.png", b"content", "png")
        );

        let dir = TempDir::new();
        let covers = dir.0.join("covers");
        write_file(&covers, &name, b"one").unwrap();
        assert_eq!(std::fs::read(covers.join(&name)).unwrap(), b"one");
        // Same name = same content: the existing file is kept.
        write_file(&covers, &name, b"one").unwrap();
        for bad in [
            "../escape.png",
            "..\\escape.png",
            "C:\\escape.png",
            "/abs/cache-0123456789abcdef-0123456789ab.png",
            "cache-0123456789abcdef-0123456789ab.png/../../x.png",
            "custom-1.png",
            "cache-0123456789abcdef-0123456789ab.exe",
        ] {
            assert!(write_file(&covers, bad, b"x").is_err(), "{bad}");
        }
        assert!(!dir.0.join("escape.png").exists());
        let entries: Vec<_> = std::fs::read_dir(&covers)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, [name], "no temporary files left behind");
    }

    fn game(db: &Db, id: &str, cover: Option<&str>, default_cover: Option<&str>) {
        db::save_game(
            &db.lock(),
            &GamePatch {
                title: Some(id.into()),
                cover: cover.map(str::to_owned),
                default_cover: default_cover.map(str::to_owned),
                ..GamePatch::new(id)
            },
        )
        .unwrap();
    }

    fn covers_of(db: &Db, id: &str) -> (Option<String>, Option<String>) {
        let g = db::get_game(&db.lock(), id).unwrap().unwrap();
        (g.cover, g.default_cover)
    }

    #[tokio::test]
    async fn the_pass_caches_remote_covers_and_keeps_failures_remote() {
        let image = png(4, 6, false);
        let served = image.clone();
        let base = serve(Arc::new(move |path, _| match path {
            "/a.png" | "/b.png" => Some(ok(&served)),
            _ => Some(response("500 Oops", &[("Content-Length", "0".into())], b"")),
        }))
        .await;
        let (a, b, broken) = (
            format!("{base}/a.png"),
            format!("{base}/b.png"),
            format!("{base}/broken.png"),
        );
        let custom = "lobby-cover://steam_400-1b2c.png";
        let foreign = "https://cdn1.epicgames.com/x.jpg";
        let db = Db::memory().unwrap();
        game(&db, "store", Some(&a), Some(&a));
        game(&db, "chosen", Some(&b), Some(&a));
        game(&db, "custom", Some(custom), Some(&a));
        game(&db, "broken", Some(&broken), Some(&broken));
        game(&db, "foreign", Some(foreign), None);
        game(&db, "skipped", Some(&format!("{base}/skip.png")), None);
        game(&db, "none", None, None);

        let dir = TempDir::new();
        let client = client(Duration::from_secs(5));
        let mut batches = 0;
        let skip = |url: &str| url.ends_with("/skip.png");
        let outcome = run_pass(&db, &dir.0, &client, local, &skip, &mut || batches += 1)
            .await
            .unwrap();
        assert_eq!(outcome.cached, 2);
        assert_eq!(outcome.failed, std::slice::from_ref(&broken));
        assert!(batches >= 1, "the library is told about the new covers");

        let (cover, default_cover) = covers_of(&db, "store");
        let cover = cover.unwrap();
        assert!(cover.starts_with("lobby-cover://cache-"), "{cover}");
        assert_eq!(
            Some(&cover),
            default_cover.as_ref(),
            "not customized: still equal"
        );
        let file = cover.strip_prefix(LOBBY_COVER_PREFIX).unwrap();
        assert_eq!(std::fs::read(dir.0.join(file)).unwrap(), image);

        let (chosen, chosen_default) = covers_of(&db, "chosen");
        assert_ne!(chosen, chosen_default, "still customized");
        assert_eq!(chosen_default.as_ref(), Some(&cover));
        assert!(chosen.unwrap().starts_with("lobby-cover://cache-"));
        assert_eq!(
            covers_of(&db, "custom"),
            (Some(custom.into()), Some(cover.clone()))
        );
        assert_eq!(
            covers_of(&db, "broken"),
            (Some(broken.clone()), Some(broken))
        );
        assert_eq!(covers_of(&db, "foreign"), (Some(foreign.into()), None));
        assert_eq!(
            covers_of(&db, "skipped").0,
            Some(format!("{base}/skip.png"))
        );
        assert_eq!(covers_of(&db, "none"), (None, None));
        {
            let conn = db.lock();
            assert_eq!(source_of(&conn, &cover).unwrap(), a);
            assert_eq!(source_of(&conn, custom).unwrap(), custom);
            assert_eq!(source_of(&conn, foreign).unwrap(), foreign);
        }

        // Reset to default (cover = defaultCover) uses the cached file; the replaced cache
        // file is cleaned up, and nothing is downloaded again.
        game(&db, "chosen", chosen_default.as_deref(), None);
        assert_eq!(sweep(&db.lock(), &dir.0).unwrap(), 1);
        // The same remote URL saved again maps straight to the file on disk.
        game(&db, "none", Some(&a), None);
        let mut again = 0;
        let outcome = run_pass(&db, &dir.0, &client, local, &|_| true, &mut || again += 1)
            .await
            .unwrap();
        assert_eq!((outcome.cached, again), (1, 1));
        assert_eq!(covers_of(&db, "none").0, Some(cover));
    }

    #[test]
    fn chosen_covers_are_adopted_only_while_their_file_exists() {
        let dir = TempDir::new();
        let db = Db::memory().unwrap();
        let url = "https://cdn2.steamgriddb.com/grid/a.png";
        let name = file_name(url, b"x", "png");
        let local = local_value(&name);
        game(&db, "g", Some(url), Some(url));
        {
            let conn = db.lock();
            assert_eq!(
                adopt(&conn, &dir.0, url, Some(&name)).unwrap(),
                url,
                "no file"
            );
            assert_eq!(adopt(&conn, &dir.0, url, Some("../a.png")).unwrap(), url);
            assert_eq!(adopt(&conn, &dir.0, url, None).unwrap(), url);
            assert_eq!(lookup(&conn, url).unwrap(), None);
            std::fs::write(dir.0.join(&name), b"x").unwrap();
            assert_eq!(adopt(&conn, &dir.0, url, Some(&name)).unwrap(), local);
        }
        assert_eq!(covers_of(&db, "g"), (Some(local.clone()), Some(local)));
        // The file vanished: the game goes back to its original URL, to be downloaded again.
        std::fs::remove_file(dir.0.join(&name)).unwrap();
        assert_eq!(restore_missing(&db.lock(), &dir.0).unwrap(), 1);
        assert_eq!(covers_of(&db, "g"), (Some(url.into()), Some(url.into())));
        assert_eq!(lookup(&db.lock(), url).unwrap(), None);
    }

    #[test]
    fn orphan_cleanup_only_removes_unreferenced_cache_files() {
        let dir = TempDir::new();
        let db = Db::memory().unwrap();
        let write = |name: &str| std::fs::write(dir.0.join(name), b"x").unwrap();
        let used = file_name("https://cdn2.steamgriddb.com/used.png", b"1", "png");
        let unused = file_name("https://cdn2.steamgriddb.com/unused.png", b"2", "png");
        let page = file_name("https://cdn2.steamgriddb.com/page.png", b"3", "png");
        // User uploads: one in use, one no longer referenced; neither is ever deleted.
        let (upload, old_upload) = ("custom_abc-1f2e.png", "custom_abc-0000.png");
        for name in [&used, &unused, &page, upload, old_upload] {
            write(name);
        }
        game(
            &db,
            "a",
            Some(&local_value(&used)),
            Some(&local_value(&used)),
        );
        game(&db, "b", Some(&format!("lobby-cover://{upload}")), None);
        {
            let conn = db.lock();
            commit(&conn, "https://cdn2.steamgriddb.com/used.png", &used).unwrap();
            commit(&conn, "https://cdn2.steamgriddb.com/unused.png", &unused).unwrap();
            commit(&conn, "https://cdn2.steamgriddb.com/page.png", &page).unwrap();
            // Referenced from a game page customization only.
            conn.execute(
                "INSERT INTO game_page_custom(gameId,hero,updatedAt) VALUES('a',?,0)",
                [local_value(&page)],
            )
            .unwrap();
            // A row whose name is not a cache file name is dropped, but the file stays.
            conn.execute(
                "INSERT INTO cover_cache(url,file,fetchedAt) VALUES('https://cdn2.steamgriddb.com/x.png',?,0)",
                [old_upload],
            )
            .unwrap();
            assert_eq!(sweep(&conn, &dir.0).unwrap(), 1);
        }
        for name in [&used, &page, upload, old_upload] {
            assert!(dir.0.join(name).exists(), "{name} must be kept");
        }
        assert!(!dir.0.join(&unused).exists());
        let conn = db.lock();
        assert_eq!(
            lookup(&conn, "https://cdn2.steamgriddb.com/unused.png").unwrap(),
            None
        );
        assert_eq!(
            lookup(&conn, "https://cdn2.steamgriddb.com/used.png").unwrap(),
            Some(used.clone())
        );

        // Removing the game frees its file.
        db::remove_game(&conn, "a").unwrap();
        conn.execute("DELETE FROM game_page_custom", []).unwrap();
        assert_eq!(sweep(&conn, &dir.0).unwrap(), 2);
        assert!(dir.0.join(upload).exists() && dir.0.join(old_upload).exists());

        // Start-up file sweep: old untracked cache files and temporary files go; recent ones,
        // and anything that isn't a cache name, stay.
        let stray = file_name("https://cdn2.steamgriddb.com/stray.png", b"4", "png");
        let fresh = file_name("https://cdn2.steamgriddb.com/fresh.png", b"5", "png");
        let temp = format!("{stray}.{}.tmp", uuid::Uuid::new_v4());
        for name in [&stray, &fresh, &temp] {
            write(name);
        }
        let old = SystemTime::now() - Duration::from_secs(3600);
        for name in [&stray, &temp, &old_upload.to_string()] {
            std::fs::File::options()
                .write(true)
                .open(dir.0.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        assert_eq!(sweep_files(&conn, &dir.0).unwrap(), 2);
        assert!(!dir.0.join(&stray).exists() && !dir.0.join(&temp).exists());
        assert!(dir.0.join(&fresh).exists());
        assert!(dir.0.join(upload).exists() && dir.0.join(old_upload).exists());
    }
}
