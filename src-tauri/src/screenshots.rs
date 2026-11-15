//! The user's own screenshots, bound to a game (table `game_screenshots`).
//!
//! - **Adding**: only files chosen in the native picker (`screenshots_add`) or found in the
//!   local Steam client's screenshot folders (`screenshots_import_steam`) are read; the renderer
//!   never supplies a path. Each source must be a regular file (no symlink or reparse point),
//!   at most [`MAX_SCREENSHOT_BYTES`], and a PNG, JPEG, WebP, GIF or BMP by its magic bytes.
//!   BMP is converted to PNG; everything else is copied byte for byte. The original is only
//!   read, never changed or moved.
//! - **Storage**: `<data dir>/screenshots/<sha256(game id)[..16]>/<sha256(original)[..32]>.<ext>`,
//!   written through a temporary file and renamed into place. The same picture is stored once
//!   per game (deduplicated by the content hash).
//! - **Serving**: `lobby-shot://<dir>/<file>` (http://lobby-shot.localhost/<dir>/<file> in
//!   WebView2). Only exactly that two-part shape, with both names matching their patterns, is
//!   served ([`resolve_request`]).
//! - **Clean-up**: removing a screenshot, a game or merging games updates the rows; files no row
//!   refers to any more are deleted by [`sweep_files`], which only ever looks inside the
//!   screenshots folder and only at names it creates. The cover cache never touches this folder.

use crate::db::{self, Db};
use crate::state::AppState;
use crate::types::{now_ms, GamePageCustom};
use anyhow::{anyhow, bail, Result};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, UriSchemeContext, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// Largest screenshot file accepted.
pub const MAX_SCREENSHOT_BYTES: u64 = 40 * 1024 * 1024;
/// Widest or tallest picture accepted (pixels).
const MAX_DIMENSION: u32 = 16_384;
/// Steam screenshots looked at per import (newest first).
pub const MAX_STEAM_IMPORT: usize = 200;
/// Extensions offered in the picker (the content decides what is accepted).
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];
pub const SHOT_PREFIX: &str = "lobby-shot://";
/// Leftover temporary files older than this are removed by [`sweep_files`].
const TEMP_MIN_AGE: Duration = Duration::from_secs(10 * 60);

static DIR_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{16}$").unwrap());
static FILE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{32}\.(png|jpg|webp|gif)$").unwrap());
static TEMP_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-f]{32}\.(png|jpg|webp|gif)\.[0-9a-f-]{36}\.tmp$").unwrap()
});

/// One stored screenshot as the renderer sees it (no file system paths).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    pub id: String,
    pub game_id: String,
    /// `lobby-shot://<dir>/<file>`.
    pub src: String,
    pub width: u32,
    pub height: u32,
    pub added_at: i64,
    /// `"user"` or `"steam"`.
    pub source: String,
    /// The stored file is gone (e.g. the data folder came from a database-only backup). Adding
    /// the same picture again restores it.
    pub missing: bool,
}

/// Result of adding or importing screenshots.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub screenshots: Vec<Screenshot>,
    pub added: usize,
    pub duplicates: usize,
    /// Pictures already listed whose missing file was written back.
    pub restored: usize,
    /// One line per refused file (file name only).
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Names and paths

/// The per-game folder name: the first 16 hex digits of SHA-256 over the game id.
pub fn dir_for(game_id: &str) -> String {
    hex(&Sha256::digest(game_id.as_bytes()))[..16].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `<dir>/<file>` when both parts have exactly the shapes this module creates.
pub fn valid_relative(relative: &str) -> Option<(&str, &str)> {
    let (dir, file) = relative.split_once('/')?;
    (DIR_NAME.is_match(dir) && FILE_NAME.is_match(file)).then_some((dir, file))
}

/// The file a `lobby-shot` request refers to, or `None`. The query and fragment are ignored;
/// anything but `/<dir>/<file>` with valid names is refused (no traversal, no other files).
pub fn resolve_request(root: &Path, uri_path: &str) -> Option<PathBuf> {
    let raw = uri_path.strip_prefix('/')?;
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    let (dir, file) = valid_relative(&decoded)?;
    Some(root.join(dir).join(file))
}

// ---------------------------------------------------------------------------
// Reading and checking files

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
}

fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Kind::Gif)
    } else if bytes.len() >= 26 && bytes.starts_with(b"BM") {
        Some(Kind::Bmp)
    } else {
        None
    }
}

/// A checked picture ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
    /// First 32 hex digits of SHA-256 over the original file (the dedupe key and file name).
    pub hash: String,
}

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits
}

/// Checks the content and returns what to store: BMP becomes PNG, other formats are kept.
pub fn prepare(original: Vec<u8>) -> Result<Prepared> {
    let kind =
        sniff(&original).ok_or_else(|| anyhow!("not a PNG, JPEG, WebP, GIF or BMP image"))?;
    let hash = hex(&Sha256::digest(&original))[..32].to_string();
    let reader = |format: image::ImageFormat| {
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&original), format);
        reader.limits(limits());
        reader
    };
    let (bytes, ext, (width, height)) = match kind {
        Kind::Bmp => {
            let image = reader(image::ImageFormat::Bmp)
                .decode()
                .map_err(|_| anyhow!("the BMP file could not be read"))?;
            let dims = (image.width(), image.height());
            let mut out = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .map_err(|_| anyhow!("the BMP file could not be converted"))?;
            (out, "png", dims)
        }
        Kind::Gif => {
            // Logical screen size from the header (little endian).
            let w = u16::from_le_bytes([original[6], original[7]]) as u32;
            let h = u16::from_le_bytes([
                original.get(8).copied().unwrap_or(0),
                original.get(9).copied().unwrap_or(0),
            ]) as u32;
            let dims = (w, h);
            (original, "gif", dims)
        }
        other => {
            let (format, ext) = match other {
                Kind::Png => (image::ImageFormat::Png, "png"),
                Kind::Jpeg => (image::ImageFormat::Jpeg, "jpg"),
                _ => (image::ImageFormat::WebP, "webp"),
            };
            let dims = reader(format)
                .into_dimensions()
                .map_err(|_| anyhow!("the image could not be read"))?;
            (original, ext, dims)
        }
    };
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!("the image size is not supported");
    }
    Ok(Prepared {
        bytes,
        ext,
        width,
        height,
        hash,
    })
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Reads a source file: a regular file (not a link or reparse point) of at most `max` bytes.
pub fn read_source(path: &Path, max: u64) -> Result<Vec<u8>> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|_| anyhow!("the file could not be read"))?;
    if meta.file_type().is_symlink() || is_reparse_point(&meta) || !meta.is_file() {
        bail!("only regular files can be added (no links or shortcuts)");
    }
    if meta.len() > max {
        bail!("the file is larger than {} MB", max / (1024 * 1024));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(max + 1).read_to_end(&mut bytes))
        .map_err(|_| anyhow!("the file could not be read"))?;
    if bytes.len() as u64 > max {
        bail!("the file is larger than {} MB", max / (1024 * 1024));
    }
    Ok(bytes)
}

/// Writes `bytes` as `root/dir/file` through a temporary file in the same folder. An existing
/// regular file of that name already has this content (its name is the content hash).
fn write_atomic(root: &Path, dir: &str, file: &str, bytes: &[u8]) -> Result<()> {
    if !DIR_NAME.is_match(dir) || !FILE_NAME.is_match(file) {
        bail!("invalid screenshot name");
    }
    let folder = root.join(dir);
    std::fs::create_dir_all(&folder)?;
    let target = folder.join(file);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_file() => return Ok(()),
        Ok(_) => bail!("the screenshot path is not a file"),
        Err(_) => {}
    }
    let temp = folder.join(format!("{file}.{}.tmp", uuid::Uuid::new_v4()));
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
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_file()) {
            return Ok(());
        }
        return Err(anyhow!("Could not save the screenshot: {e}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Database

fn read_row(row: &rusqlite::Row) -> rusqlite::Result<Screenshot> {
    let file: String = row.get("file")?;
    Ok(Screenshot {
        id: row.get("id")?,
        game_id: row.get("gameId")?,
        src: format!("{SHOT_PREFIX}{file}"),
        width: row.get::<_, i64>("width")?.clamp(0, MAX_DIMENSION as i64) as u32,
        height: row.get::<_, i64>("height")?.clamp(0, MAX_DIMENSION as i64) as u32,
        added_at: row.get("addedAt")?,
        source: row.get("source")?,
        missing: false,
    })
}

/// Whether the file behind `relative` (`<dir>/<file>`) is a regular file under `root`.
fn file_present(root: &Path, relative: &str) -> bool {
    valid_relative(relative).is_some_and(|(dir, file)| {
        std::fs::symlink_metadata(root.join(dir).join(file)).is_ok_and(|m| m.is_file())
    })
}

/// A game's screenshots, newest first. Rows whose file is gone are kept and flagged `missing`.
pub fn list(conn: &Connection, root: &Path, game_id: &str) -> Result<Vec<Screenshot>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM game_screenshots WHERE gameId=? ORDER BY addedAt DESC, rowid DESC",
    )?;
    let rows = stmt
        .query_map([game_id], read_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Rows whose file name was tampered with are not shown.
    Ok(rows
        .into_iter()
        .filter(|s| valid_relative(&s.src[SHOT_PREFIX.len()..]).is_some())
        .map(|s| Screenshot {
            missing: !file_present(root, &s.src[SHOT_PREFIX.len()..]),
            ..s
        })
        .collect())
}

/// What [`store`] did with a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Added,
    /// The game already has it and its file is there.
    Duplicate,
    /// The game already had it but the file was missing: written back.
    Restored,
}

/// Stores a prepared picture for a game. A picture the game already has is not stored twice,
/// but its file is written back when it has gone missing.
pub fn store(
    conn: &Connection,
    root: &Path,
    game_id: &str,
    picture: &Prepared,
    source: &str,
    original: Option<&Path>,
    added_at: i64,
) -> Result<Stored> {
    if db::get_game(conn, game_id)?.is_none() {
        bail!("Game not found.");
    }
    let existing: Option<String> = conn
        .query_row(
            "SELECT file FROM game_screenshots WHERE gameId=? AND hash=?",
            params![game_id, picture.hash],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(relative) = existing {
        if file_present(root, &relative) {
            return Ok(Stored::Duplicate);
        }
        let (dir, file) =
            valid_relative(&relative).ok_or_else(|| anyhow!("the stored screenshot is invalid"))?;
        // Same hash, same original: the stored name's extension matches what was prepared.
        if !file.ends_with(&format!(".{}", picture.ext)) {
            bail!("the stored screenshot has a different format");
        }
        write_atomic(root, dir, file, &picture.bytes)?;
        return Ok(Stored::Restored);
    }
    let dir = dir_for(game_id);
    let file = format!("{}.{}", picture.hash, picture.ext);
    write_atomic(root, &dir, &file, &picture.bytes)?;
    let source = if source == "steam" { "steam" } else { "user" };
    conn.execute(
        "INSERT INTO game_screenshots(id,gameId,file,hash,width,height,addedAt,source,originalPath) VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            uuid::Uuid::new_v4().to_string(),
            game_id,
            format!("{dir}/{file}"),
            picture.hash,
            picture.width,
            picture.height,
            added_at,
            source,
            original.map(|p| p.to_string_lossy().chars().take(4096).collect::<String>()),
        ],
    )?;
    Ok(Stored::Added)
}

/// Reads, checks and stores `paths` for a game (the picker's choice, or Steam's files). Also
/// the test entry point: it takes paths directly instead of opening the dialog.
pub fn add_files(
    db: &Db,
    root: &Path,
    game_id: &str,
    paths: &[PathBuf],
    source: &str,
) -> Result<AddResult> {
    if db::get_game(&db.lock(), game_id)?.is_none() {
        bail!("Game not found.");
    }
    let mut result = AddResult::default();
    let base = now_ms();
    for (index, path) in paths.iter().enumerate() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().chars().take(120).collect::<String>())
            .unwrap_or_else(|| "file".into());
        let prepared = read_source(path, MAX_SCREENSHOT_BYTES).and_then(prepare);
        let picture = match prepared {
            Ok(picture) => picture,
            Err(e) => {
                result.errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        // Later files in one pick sort first, as if added one after another.
        match store(
            &db.lock(),
            root,
            game_id,
            &picture,
            source,
            Some(path),
            base + index as i64,
        ) {
            Ok(Stored::Added) => result.added += 1,
            Ok(Stored::Duplicate) => result.duplicates += 1,
            Ok(Stored::Restored) => result.restored += 1,
            Err(e) => result.errors.push(format!("{name}: {e}")),
        }
    }
    result.screenshots = list(&db.lock(), root, game_id)?;
    Ok(result)
}

/// Deletes one screenshot row; returns its game id. The file goes with [`sweep_files`].
pub fn remove(conn: &Connection, id: &str) -> Result<Option<String>> {
    let game_id: Option<String> = conn
        .query_row(
            "SELECT gameId FROM game_screenshots WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if game_id.is_some() {
        conn.execute("DELETE FROM game_screenshots WHERE id=?", [id])?;
    }
    Ok(game_id)
}

/// Deletes files in the screenshots folder that no row refers to (only names this module
/// creates), old temporary files, and empty game folders. Returns the number of files removed.
pub fn sweep_files(conn: &Connection, root: &Path) -> Result<usize> {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return Ok(0);
    };
    let referenced: std::collections::HashSet<String> = conn
        .prepare("SELECT file FROM game_screenshots")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let now = SystemTime::now();
    let mut removed = 0;
    for dir in dirs.flatten() {
        let dir_name = dir.file_name().to_string_lossy().into_owned();
        let is_dir = dir.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir || !DIR_NAME.is_match(&dir_name) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for entry in files.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let orphan =
                FILE_NAME.is_match(&name) && !referenced.contains(&format!("{dir_name}/{name}"));
            let stale_temp = TEMP_NAME.is_match(&name)
                && meta
                    .modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|age| age >= TEMP_MIN_AGE);
            if (orphan || stale_temp) && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        // Only succeeds when the folder is empty.
        let _ = std::fs::remove_dir(dir.path());
    }
    Ok(removed)
}

/// Removes unreferenced screenshot files. Never fails the caller.
pub fn clean_up(state: &AppState) {
    if let Err(e) = sweep_files(&state.db.lock(), &state.screenshots_dir()) {
        log::warn!("Could not clean up screenshots: {e:#}");
    }
}

// ---------------------------------------------------------------------------
// Steam

/// Steam's own screenshots of `app_id` for every local account, newest first, at most
/// [`MAX_STEAM_IMPORT`]: `<steam>\userdata\<account>\760\remote\<app>\screenshots\*.jpg`.
/// Only reads directory listings; links and non-files are skipped.
pub fn steam_screenshot_files(steam_dirs: &[PathBuf], app_id: &str) -> Vec<PathBuf> {
    if app_id.is_empty() || app_id.len() > 12 || !app_id.chars().all(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for steam in steam_dirs {
        let Ok(accounts) = std::fs::read_dir(steam.join("userdata")) else {
            continue;
        };
        for account in accounts.flatten() {
            let name = account.file_name().to_string_lossy().into_owned();
            if name.is_empty() || name.len() > 20 || !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let folder = account
                .path()
                .join("760")
                .join("remote")
                .join(app_id)
                .join("screenshots");
            let Ok(files) = std::fs::read_dir(&folder) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                let jpg = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("jpg"));
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if jpg && meta.is_file() && !is_reparse_point(&meta) {
                    found.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
                }
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.truncate(MAX_STEAM_IMPORT);
    found.into_iter().map(|(_, p)| p).collect()
}

// ---------------------------------------------------------------------------
// Protocol and commands

/// Serves `lobby-shot://<dir>/<file>` (http://lobby-shot.localhost/<dir>/<file>).
pub fn protocol<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Cow::Borrowed(&b"Not found"[..]))
            .unwrap()
    };
    let Some(state) = ctx.app_handle().try_state::<AppState>() else {
        return not_found();
    };
    let Some(file) = resolve_request(&state.screenshots_dir(), request.uri().path()) else {
        return not_found();
    };
    let regular =
        std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() && !is_reparse_point(&m));
    let Some(bytes) = regular.then(|| std::fs::read(&file).ok()).flatten() else {
        return not_found();
    };
    let mime = match file.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => return not_found(),
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(bytes))
        .unwrap()
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Changed<'a> {
    game_id: &'a str,
}

fn notify(app: &AppHandle, game_id: &str) {
    let _ = app.emit("screenshots:changed", Changed { game_id });
}

#[tauri::command]
pub async fn screenshots_list(
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<Vec<Screenshot>> {
    list(&state.db.lock(), &state.screenshots_dir(), &game_id).map_err(err)
}

/// Opens the native multi-file picker and adds the chosen pictures. Cancelling adds nothing.
#[tauri::command]
pub async fn screenshots_add(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<AddResult> {
    if db::get_game(&state.db.lock(), &game_id)
        .map_err(err)?
        .is_none()
    {
        return Err("Game not found.".into());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .add_filter("Images", EXTENSIONS)
        .pick_files(move |paths| {
            let _ = tx.send(paths);
        });
    let paths: Vec<PathBuf> = rx
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .take(500)
        .collect();
    if paths.is_empty() {
        return Ok(AddResult {
            screenshots: list(&state.db.lock(), &state.screenshots_dir(), &game_id).map_err(err)?,
            ..Default::default()
        });
    }
    let root = state.screenshots_dir();
    let db_state = app.clone();
    let id = game_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = db_state.state::<AppState>();
        add_files(&state.db, &root, &id, &paths, "user")
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    if result.added > 0 || result.restored > 0 {
        notify(&app, &game_id);
    }
    Ok(result)
}

/// Copies the game's screenshots from the local Steam client (read only, newest
/// [`MAX_STEAM_IMPORT`]). Needs a Steam route with a numeric app id.
#[tauri::command]
pub async fn screenshots_import_steam(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<AddResult> {
    let game = db::get_game(&state.db.lock(), &game_id)
        .map_err(err)?
        .ok_or("Game not found.")?;
    let app_id = crate::steamgriddb::steam_app_id(&game)
        .ok_or("This game has no Steam app ID.")?
        .to_string();
    let root = state.screenshots_dir();
    let handle = app.clone();
    let id = game_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let files = steam_screenshot_files(&crate::importers::steam::steam_client_dirs(), &app_id);
        if files.is_empty() {
            bail!("No Steam screenshots were found for this game on this PC.");
        }
        add_files(&state.db, &root, &id, &files, "steam")
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    if result.added > 0 || result.restored > 0 {
        notify(&app, &game_id);
    }
    Ok(result)
}

/// Removes Game Lobby's copy of a screenshot (the original file is never touched).
#[tauri::command]
pub async fn screenshots_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Vec<Screenshot>> {
    let game_id = remove(&state.db.lock(), &id)
        .map_err(err)?
        .ok_or("That screenshot no longer exists.")?;
    clean_up(&state);
    notify(&app, &game_id);
    list(&state.db.lock(), &state.screenshots_dir(), &game_id).map_err(err)
}

/// Uses a screenshot as the game page hero: a copy goes into the covers folder (like a chosen
/// hero file), so removing the screenshot later doesn't change the page.
#[tauri::command]
pub async fn screenshots_set_as_hero(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<GamePageCustom> {
    let (game_id, file): (String, String) = state
        .db
        .lock()
        .query_row(
            "SELECT gameId,file FROM game_screenshots WHERE id=?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or("That screenshot no longer exists.")?;
    let (dir, name) = valid_relative(&file).ok_or("That screenshot is unavailable.")?;
    let source = state.screenshots_dir().join(dir).join(name);
    let covers = state.covers_dir();
    let value = crate::art::import_image_file(&covers, &game_id, crate::art::Slot::Hero, &source)
        .map_err(err)?;
    let saved = crate::art::set_image(
        &state.db.lock(),
        &covers,
        &game_id,
        crate::art::Slot::Hero,
        Some(&value),
    );
    match saved {
        Ok(custom) => {
            let _ = app.emit("art:changed", Changed { game_id: &game_id });
            Ok(custom)
        }
        Err(e) => {
            if let Some(name) = crate::validation::lobby_cover_name(&value) {
                let _ = std::fs::remove_file(covers.join(name));
            }
            Err(err(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GamePatch;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("game-lobby-shots-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn library(ids: &[&str]) -> Db {
        let db = Db::memory().unwrap();
        for id in ids {
            db::save_game(
                &db.lock(),
                &GamePatch {
                    title: Some(format!("Game {id}")),
                    store: Some("Custom".into()),
                    ..GamePatch::new(*id)
                },
            )
            .unwrap();
        }
        db
    }

    fn encode(width: u32, height: u32, seed: u8, format: image::ImageFormat) -> Vec<u8> {
        let img =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, seed])
            }));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), format)
            .unwrap();
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn files_in(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
            for file in std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                out.push(format!(
                    "{}/{}",
                    dir.file_name().to_string_lossy(),
                    file.file_name().to_string_lossy()
                ));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn pictures_are_added_deduplicated_listed_and_removed() {
        let src = TempDir::new();
        let root = TempDir::new();
        let db = library(&["g1", "g2"]);
        let a = write(&src.0, "a.png", &encode(40, 20, 1, image::ImageFormat::Png));
        let b = write(
            &src.0,
            "b.jpg",
            &encode(30, 30, 2, image::ImageFormat::Jpeg),
        );
        let original_a = std::fs::read(&a).unwrap();

        let r = add_files(&db, &root.0, "g1", &[a.clone(), b.clone()], "user").unwrap();
        assert_eq!((r.added, r.duplicates, r.errors.len()), (2, 0, 0));
        // Newest first: the later file in one pick sorts first.
        assert_eq!(r.screenshots.len(), 2);
        assert!(r.screenshots[0].src.ends_with(".jpg"));
        assert_eq!((r.screenshots[1].width, r.screenshots[1].height), (40, 20));
        assert!(r
            .screenshots
            .iter()
            .all(|s| s.src.starts_with(SHOT_PREFIX) && s.source == "user"));
        let dir = dir_for("g1");
        assert!(r
            .screenshots
            .iter()
            .all(|s| s.src.starts_with(&format!("{SHOT_PREFIX}{dir}/"))));
        // The original is untouched.
        assert_eq!(std::fs::read(&a).unwrap(), original_a);

        // The same picture again (even under another name) is not stored twice.
        let copy = write(&src.0, "copy of a.png", &original_a);
        let r = add_files(&db, &root.0, "g1", &[copy], "user").unwrap();
        assert_eq!((r.added, r.duplicates), (0, 1));
        // Another game can have it too.
        let r = add_files(&db, &root.0, "g2", std::slice::from_ref(&a), "user").unwrap();
        assert_eq!(r.added, 1);
        assert_eq!(files_in(&root.0).len(), 3);

        // Removing deletes the row; the sweep then deletes only that file.
        let first = list(&db.lock(), &root.0, "g1").unwrap()[0].clone();
        assert_eq!(
            remove(&db.lock(), &first.id).unwrap().as_deref(),
            Some("g1")
        );
        assert_eq!(remove(&db.lock(), &first.id).unwrap(), None);
        assert_eq!(sweep_files(&db.lock(), &root.0).unwrap(), 1);
        assert_eq!(list(&db.lock(), &root.0, "g1").unwrap().len(), 1);
        assert_eq!(files_in(&root.0).len(), 2);
        assert!(a.exists() && b.exists(), "originals are never removed");
    }

    #[test]
    fn missing_files_are_flagged_and_restored_by_adding_again() {
        let src = TempDir::new();
        let root = TempDir::new();
        let db = library(&["g1"]);
        let a = write(&src.0, "a.png", &encode(8, 4, 1, image::ImageFormat::Png));
        let b = write(&src.0, "b.png", &encode(8, 4, 2, image::ImageFormat::Png));
        add_files(&db, &root.0, "g1", &[a.clone(), b.clone()], "steam").unwrap();
        let listed = list(&db.lock(), &root.0, "g1").unwrap();
        assert!(listed.iter().all(|s| !s.missing));
        // The data folder lost one file (e.g. a database-only restore).
        let gone = listed
            .iter()
            .find(|s| {
                s.src
                    .contains(&prepare(std::fs::read(&a).unwrap()).unwrap().hash)
            })
            .unwrap()
            .clone();
        let (dir, file) = valid_relative(&gone.src[SHOT_PREFIX.len()..]).unwrap();
        std::fs::remove_file(root.0.join(dir).join(file)).unwrap();
        let listed = list(&db.lock(), &root.0, "g1").unwrap();
        assert_eq!(listed.len(), 2, "the row is kept, not deleted");
        assert_eq!(
            listed
                .iter()
                .filter(|s| s.missing)
                .map(|s| s.id.clone())
                .collect::<Vec<_>>(),
            vec![gone.id.clone()]
        );
        // Adding (or importing) the same picture writes the file back.
        let r = add_files(&db, &root.0, "g1", &[a.clone(), b.clone()], "steam").unwrap();
        assert_eq!((r.added, r.duplicates, r.restored), (0, 1, 1));
        assert!(r.screenshots.iter().all(|s| !s.missing));
        assert_eq!(
            std::fs::read(root.0.join(dir).join(file)).unwrap(),
            std::fs::read(&a).unwrap()
        );
        // The id and order are unchanged.
        assert!(r.screenshots.iter().any(|s| s.id == gone.id));
    }

    #[test]
    fn bmp_becomes_png_and_gif_webp_are_kept() {
        let bmp = encode(12, 7, 3, image::ImageFormat::Bmp);
        assert!(bmp.starts_with(b"BM"));
        let p = prepare(bmp.clone()).unwrap();
        assert_eq!((p.ext, p.width, p.height), ("png", 12, 7));
        assert!(p.bytes.starts_with(b"\x89PNG"));
        assert_eq!(p.hash, hex(&Sha256::digest(&bmp))[..32]);
        let webp = encode(9, 5, 4, image::ImageFormat::WebP);
        let p = prepare(webp.clone()).unwrap();
        assert_eq!(
            (p.ext, p.width, p.height, p.bytes == webp),
            ("webp", 9, 5, true)
        );
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xF0, 0x00, 0, 0, 0]);
        let p = prepare(gif).unwrap();
        assert_eq!((p.ext, p.width, p.height), ("gif", 320, 240));
    }

    #[test]
    fn non_images_oversize_files_and_links_are_refused() {
        let src = TempDir::new();
        let root = TempDir::new();
        let db = library(&["g1"]);
        let text = write(&src.0, "notes.png", b"hello, not an image at all");
        let html = write(&src.0, "x.jpg", b"<html><script>alert(1)</script></html>");
        let truncated = write(&src.0, "t.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        let missing = src.0.join("missing.png");
        let r = add_files(
            &db,
            &root.0,
            "g1",
            &[text, html, truncated, missing],
            "user",
        )
        .unwrap();
        assert_eq!((r.added, r.errors.len()), (0, 4));
        assert!(files_in(&root.0).is_empty());
        // Size cap (checked before reading everything).
        let big = write(&src.0, "big.png", &encode(8, 8, 1, image::ImageFormat::Png));
        assert!(read_source(&big, 10)
            .unwrap_err()
            .to_string()
            .contains("larger"));
        // Unknown games are refused.
        assert!(add_files(&db, &root.0, "ghost", &[big], "user").is_err());
        // Folders are not files.
        assert!(read_source(&src.0, MAX_SCREENSHOT_BYTES).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn symlinked_sources_are_refused_when_links_can_be_made() {
        let src = TempDir::new();
        let real = write(
            &src.0,
            "real.png",
            &encode(4, 4, 1, image::ImageFormat::Png),
        );
        let link = src.0.join("link.png");
        // Creating symlinks needs developer mode or admin rights; skip quietly otherwise.
        if std::os::windows::fs::symlink_file(&real, &link).is_ok() {
            assert!(read_source(&link, MAX_SCREENSHOT_BYTES).is_err());
        }
    }

    #[test]
    fn only_exact_names_are_served() {
        let root = Path::new("shots");
        let dir = dir_for("g1");
        let file = format!("{}.png", "a".repeat(32));
        assert_eq!(
            resolve_request(root, &format!("/{dir}/{file}")),
            Some(root.join(&dir).join(&file))
        );
        for bad in [
            format!("/{dir}/../{file}"),
            format!("/../{dir}/{file}"),
            format!("/{dir}%2F..%2F{file}"),
            format!("/{dir}/{file}/x"),
            format!("/{dir}"),
            format!("/{file}"),
            format!("/{dir}/{}.exe", "a".repeat(32)),
            format!("/{dir}/{}.PNG", "a".repeat(32)),
            format!("/{dir}/{}.png", "a".repeat(31)),
            format!("/{}/{file}", "g".repeat(16)),
            "/..%5C..%5Clibrary.sqlite".to_string(),
            "/".to_string(),
            String::new(),
        ] {
            assert_eq!(resolve_request(root, &bad), None, "{bad}");
        }
    }

    #[test]
    fn steam_screenshots_are_found_read_only_and_imported_once() {
        let steam = TempDir::new();
        let root = TempDir::new();
        let shots = steam
            .0
            .join("userdata")
            .join("12345")
            .join("760")
            .join("remote")
            .join("620")
            .join("screenshots");
        std::fs::create_dir_all(shots.join("thumbnails")).unwrap();
        let one = write(
            &shots,
            "20240101_1.jpg",
            &encode(16, 9, 1, image::ImageFormat::Jpeg),
        );
        write(
            &shots,
            "20240102_1.jpg",
            &encode(16, 9, 2, image::ImageFormat::Jpeg),
        );
        write(&shots, "notes.txt", b"x");
        write(
            &shots.join("thumbnails"),
            "20240101_1.jpg",
            &encode(4, 2, 1, image::ImageFormat::Jpeg),
        );
        // Other apps and non-numeric account folders are ignored.
        let other = steam
            .0
            .join("userdata")
            .join("12345")
            .join("760")
            .join("remote")
            .join("440")
            .join("screenshots");
        std::fs::create_dir_all(&other).unwrap();
        write(&other, "x.jpg", &encode(4, 4, 9, image::ImageFormat::Jpeg));
        let anon = steam
            .0
            .join("userdata")
            .join("anonymous")
            .join("760")
            .join("remote")
            .join("620")
            .join("screenshots");
        std::fs::create_dir_all(&anon).unwrap();
        write(&anon, "y.jpg", &encode(4, 4, 8, image::ImageFormat::Jpeg));

        let found = steam_screenshot_files(std::slice::from_ref(&steam.0), "620");
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.parent() == Some(shots.as_path())));
        assert!(steam_screenshot_files(std::slice::from_ref(&steam.0), "62/../440").is_empty());
        assert!(steam_screenshot_files(std::slice::from_ref(&steam.0), "").is_empty());

        let db = library(&["g1"]);
        let before = std::fs::read(&one).unwrap();
        let r = add_files(&db, &root.0, "g1", &found, "steam").unwrap();
        assert_eq!(r.added, 2);
        assert!(r.screenshots.iter().all(|s| s.source == "steam"));
        let r = add_files(&db, &root.0, "g1", &found, "steam").unwrap();
        assert_eq!((r.added, r.duplicates), (0, 2));
        assert_eq!(
            std::fs::read(&one).unwrap(),
            before,
            "Steam's files are only read"
        );
        assert_eq!(
            std::fs::read_dir(&shots).unwrap().count(),
            4,
            "nothing written into Steam's folder"
        );
    }

    #[test]
    fn deleting_and_merging_games_keep_files_consistent() {
        let src = TempDir::new();
        let root = TempDir::new();
        let db = library(&["keep", "dupe", "gone"]);
        let shared = write(
            &src.0,
            "shared.png",
            &encode(5, 5, 1, image::ImageFormat::Png),
        );
        let only = write(
            &src.0,
            "only.png",
            &encode(6, 6, 2, image::ImageFormat::Png),
        );
        add_files(&db, &root.0, "keep", std::slice::from_ref(&shared), "user").unwrap();
        add_files(
            &db,
            &root.0,
            "dupe",
            &[shared.clone(), only.clone()],
            "user",
        )
        .unwrap();
        add_files(&db, &root.0, "gone", std::slice::from_ref(&only), "user").unwrap();
        assert_eq!(files_in(&root.0).len(), 4);

        db::merge_games(&mut db.lock(), "keep", "dupe").unwrap();
        let kept = list(&db.lock(), &root.0, "keep").unwrap();
        assert_eq!(kept.len(), 2, "the duplicate picture is kept once");
        // The linked entry's copy of the shared picture is parked for unlinking (`links`).
        assert_eq!(list(&db.lock(), &root.0, "dupe").unwrap().len(), 1);
        sweep_files(&db.lock(), &root.0).unwrap();
        // The moved picture still points at an existing file.
        for shot in &kept {
            let (dir, file) = valid_relative(&shot.src[SHOT_PREFIX.len()..]).unwrap();
            assert!(root.0.join(dir).join(file).is_file());
        }
        assert_eq!(files_in(&root.0).len(), 4);

        db::remove_game(&db.lock(), "gone").unwrap();
        assert!(list(&db.lock(), &root.0, "gone").unwrap().is_empty());
        assert_eq!(sweep_files(&db.lock(), &root.0).unwrap(), 1);
        assert_eq!(files_in(&root.0).len(), 3);
        // Removing the linked game removes the parked copy too.
        db::remove_game(&db.lock(), "keep").unwrap();
        assert!(list(&db.lock(), &root.0, "dupe").unwrap().is_empty());
        assert_eq!(sweep_files(&db.lock(), &root.0).unwrap(), 3);
        assert_eq!(files_in(&root.0).len(), 0);
    }

    #[test]
    fn games_with_screenshots_survive_scans() {
        let src = TempDir::new();
        let root = TempDir::new();
        let db = Db::memory().unwrap();
        let route = crate::types::GameRoute {
            source_id: "steam".into(),
            store: "Steam".into(),
            app_id: Some("1".into()),
            launch_target: "steam://run/1".into(),
            installed: Some(true),
            install_states: Some([("steam-installed".to_string(), true)].into()),
            ..Default::default()
        };
        let game = crate::types::Game {
            id: "steam:1".into(),
            title: "One".into(),
            store: "Steam".into(),
            routes: vec![route.clone()],
            ..Default::default()
        };
        db::upsert_imported_games(&mut db.lock(), &[game]).unwrap();
        let shot = write(&src.0, "s.png", &encode(5, 5, 1, image::ImageFormat::Png));
        add_files(&db, &root.0, "steam:1", &[shot], "user").unwrap();
        // A scan that no longer finds it installed: no other user data, but screenshots keep it.
        let other = crate::types::Game {
            id: "steam:2".into(),
            title: "Two".into(),
            store: "Steam".into(),
            routes: vec![crate::types::GameRoute {
                app_id: Some("2".into()),
                launch_target: "steam://run/2".into(),
                ..route
            }],
            ..Default::default()
        };
        db::reconcile_installed_source(&mut db.lock(), "steam-installed", &[other]).unwrap();
        assert!(db::get_game(&db.lock(), "steam:1").unwrap().is_some());
    }

    #[test]
    fn the_cover_cache_sweep_leaves_screenshots_alone() {
        let data = TempDir::new();
        let covers = data.0.join("covers");
        let shots = data.0.join("screenshots");
        std::fs::create_dir_all(&covers).unwrap();
        let src = TempDir::new();
        let db = library(&["g1"]);
        let shot = write(&src.0, "s.png", &encode(5, 5, 1, image::ImageFormat::Png));
        add_files(&db, &shots, "g1", &[shot], "user").unwrap();
        // A stray cache-named file in the covers folder is the only thing the cover sweep sees.
        crate::covercache::sweep(&db.lock(), &covers).unwrap();
        crate::covercache::sweep_files(&db.lock(), &covers).unwrap();
        assert_eq!(files_in(&shots).len(), 1);
        // And the screenshot sweep ignores files it didn't create.
        let dir = shots.join(dir_for("g1"));
        write(&dir, "keep-me.png", b"x");
        write(&shots, "README.txt", b"x");
        assert_eq!(sweep_files(&db.lock(), &shots).unwrap(), 0);
        assert!(dir.join("keep-me.png").exists() && shots.join("README.txt").exists());
    }
}
