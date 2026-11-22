//! Personal notes, score, memories and controller details per game.
//!
//! Tables: `game_notes` (score and quick notes), `game_memories` (short dated entries, each
//! optionally pointing at one of the game's screenshots) and `game_input` (controller support,
//! preferred controller and input notes).
//!
//! Everything here is plain text or numbers: nothing stored through these commands is ever
//! launched, opened or executed. Every field is validated for length and range on the way in,
//! and again when a backup is imported.
//!
//! Controller support is auto-filled for Steam games from Steam's public store API
//! (`appdetails`, no key). The lookup runs only from the game page, at most once per
//! [`STEAM_TTL_MS`] per game, spaced by [`MIN_GAP`] between requests, and never replaces a
//! value the user chose (`supportSource = 'user'`).

use crate::db;
use crate::state::AppState;
use crate::steamgriddb::steam_app_id;
use crate::types::now_ms;
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

type CmdResult<T> = Result<T, String>;

pub const MAX_NOTES_CHARS: usize = 5000;
pub const MAX_MEMORY_CHARS: usize = 1000;
pub const MAX_MEMORIES_PER_GAME: usize = 500;
pub const MAX_PREFERRED_CHARS: usize = 60;
pub const MAX_INPUT_NOTES_CHARS: usize = 1000;
const MAX_GAME_ID_CHARS: usize = 512;
const MAX_BACKUP_ROWS: usize = 100_000;
pub const SUPPORT_LEVELS: &[&str] = &["full", "partial", "none", "unknown"];
pub const STEAM_TTL_MS: i64 = 7 * 24 * 3_600_000;
const MIN_GAP: Duration = Duration::from_millis(1500);
/// After a failed Steam request (offline, rate limited) the same game is not asked again for
/// this long.
const STEAM_RETRY_AFTER_MS: i64 = 10 * 60_000;
const STEAM_TIMEOUT: Duration = Duration::from_secs(10);
const STEAM_MAX_BYTES: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Types

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: i64,
    pub game_id: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub text: String,
    pub screenshot_id: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameNotes {
    /// Stars from 0.5 to 5 in half steps; `None` when not rated.
    pub rating: Option<f64>,
    pub notes: String,
    pub updated_at: i64,
    pub memories: Vec<Memory>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameInput {
    /// `full`, `partial`, `none` (keyboard and mouse only) or `unknown`.
    pub support: String,
    /// `steam` when filled in from Steam, `user` when chosen by the user, else empty.
    pub support_source: String,
    pub preferred: String,
    pub notes: String,
    pub updated_at: i64,
    /// When Steam last answered (0 = never asked).
    pub steam_checked_at: i64,
}

impl Default for GameInput {
    fn default() -> Self {
        Self {
            support: "unknown".into(),
            support_source: String::new(),
            preferred: String::new(),
            notes: String::new(),
            updated_at: 0,
            steam_checked_at: 0,
        }
    }
}

// Backup shapes (no ids: memories are renumbered on import).

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoteBackup {
    pub game_id: String,
    #[serde(default)]
    pub rating: Option<f64>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryBackup {
    pub game_id: String,
    pub date: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputBackup {
    pub game_id: String,
    pub support: String,
    #[serde(default)]
    pub support_source: String,
    #[serde(default)]
    pub preferred: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotesBackup {
    #[serde(default)]
    pub notes: Vec<NoteBackup>,
    #[serde(default)]
    pub memories: Vec<MemoryBackup>,
    #[serde(default)]
    pub input: Vec<InputBackup>,
}

impl NotesBackup {
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.memories.is_empty() && self.input.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Validation

pub fn valid_rating(value: f64) -> Result<f64> {
    if !value.is_finite() || !(0.5..=5.0).contains(&value) || (value * 2.0).fract() != 0.0 {
        bail!("A score is 0.5 to 5 stars, in half steps.");
    }
    Ok(value)
}

fn limited(value: &str, max: usize, what: &str) -> Result<String> {
    if value.chars().count() > max {
        bail!("{what} can be at most {max} characters.");
    }
    if value.contains('\0') {
        bail!("{what} contains an invalid character.");
    }
    Ok(value.to_string())
}

pub fn valid_notes(value: &str) -> Result<String> {
    limited(value, MAX_NOTES_CHARS, "Notes")
}

pub fn valid_game_id(value: &str) -> Result<()> {
    if value.is_empty() || value.chars().count() > MAX_GAME_ID_CHARS {
        bail!("Invalid game id.");
    }
    Ok(())
}

/// `YYYY-MM-DD`, a real calendar day between 1970 and 2200.
pub fn valid_date(value: &str) -> Result<String> {
    let bad = || anyhow!("A date looks like 2025-03-31.");
    let b = value.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(bad());
    }
    let num = |s: &[u8]| -> Result<u32> {
        if s.iter().all(u8::is_ascii_digit) {
            Ok(s.iter().fold(0, |n, c| n * 10 + u32::from(c - b'0')))
        } else {
            Err(bad())
        }
    };
    let (y, m, d) = (num(&b[0..4])?, num(&b[5..7])?, num(&b[8..10])?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad()),
    };
    if !(1970..=2200).contains(&y) || d == 0 || d > days {
        return Err(bad());
    }
    Ok(value.to_string())
}

pub fn valid_memory_text(value: &str) -> Result<String> {
    let text = limited(value.trim(), MAX_MEMORY_CHARS, "A memory")?;
    if text.is_empty() {
        bail!("A memory needs some text.");
    }
    Ok(text)
}

fn valid_support(value: &str) -> Result<()> {
    if SUPPORT_LEVELS.contains(&value) {
        Ok(())
    } else {
        bail!("Unknown controller support level.")
    }
}

fn valid_preferred(value: &str) -> Result<String> {
    let text = limited(value.trim(), MAX_PREFERRED_CHARS, "Preferred controller")?;
    if text.chars().any(char::is_control) {
        bail!("Preferred controller contains an invalid character.");
    }
    Ok(text)
}

fn valid_input_notes(value: &str) -> Result<String> {
    limited(value, MAX_INPUT_NOTES_CHARS, "Input notes")
}

fn valid_source(value: &str) -> Result<()> {
    if ["", "steam", "user"].contains(&value) {
        Ok(())
    } else {
        bail!("Unknown controller support source.")
    }
}

/// Checks an imported backup section (shape, lengths, ranges). Whether each game exists is
/// decided at restore time against the library.
pub fn validate_backup(backup: &NotesBackup) -> Result<()> {
    if backup.notes.len() > MAX_BACKUP_ROWS
        || backup.memories.len() > MAX_BACKUP_ROWS
        || backup.input.len() > MAX_BACKUP_ROWS
    {
        bail!("Backup notes are too large.");
    }
    for n in &backup.notes {
        valid_game_id(&n.game_id)?;
        if let Some(r) = n.rating {
            valid_rating(r)?;
        }
        valid_notes(&n.notes)?;
    }
    let mut per_game = std::collections::HashMap::<&str, usize>::new();
    for m in &backup.memories {
        valid_game_id(&m.game_id)?;
        valid_date(&m.date)?;
        valid_memory_text(&m.text)?;
        let count = per_game.entry(&m.game_id).or_default();
        *count += 1;
        if *count > MAX_MEMORIES_PER_GAME {
            bail!("A game has too many memories in the backup.");
        }
    }
    for i in &backup.input {
        valid_game_id(&i.game_id)?;
        valid_support(&i.support)?;
        valid_source(&i.support_source)?;
        valid_preferred(&i.preferred)?;
        valid_input_notes(&i.notes)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Notes, score and memories

fn game_exists(conn: &Connection, game_id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM games WHERE id=?", [game_id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn require_game(conn: &Connection, game_id: &str) -> Result<()> {
    if game_exists(conn, game_id)? {
        Ok(())
    } else {
        bail!("Game not found.")
    }
}

pub fn load(conn: &Connection, game_id: &str) -> Result<GameNotes> {
    let row: Option<(Option<f64>, String, i64)> = conn
        .query_row(
            "SELECT rating,notes,updatedAt FROM game_notes WHERE gameId=?",
            [game_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (rating, notes, updated_at) = row.unwrap_or((None, String::new(), 0));
    // A link to a screenshot that has been removed reads as "no screenshot".
    let mut stmt = conn.prepare(
        "SELECT m.id,m.gameId,m.date,m.text,s.id,m.createdAt FROM game_memories m
         LEFT JOIN game_screenshots s ON s.id=m.screenshotId AND s.gameId=m.gameId
         WHERE m.gameId=? ORDER BY m.date DESC,m.id DESC",
    )?;
    let memories = stmt
        .query_map([game_id], |r| {
            Ok(Memory {
                id: r.get(0)?,
                game_id: r.get(1)?,
                date: r.get(2)?,
                text: r.get(3)?,
                screenshot_id: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(GameNotes {
        rating,
        notes,
        updated_at,
        memories,
    })
}

/// Removes the row once it holds nothing.
fn prune_notes(conn: &Connection, game_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM game_notes WHERE gameId=? AND rating IS NULL AND notes=''",
        [game_id],
    )?;
    Ok(())
}

pub fn set_rating(conn: &Connection, game_id: &str, rating: Option<f64>) -> Result<()> {
    let rating = rating.map(valid_rating).transpose()?;
    require_game(conn, game_id)?;
    conn.execute(
        "INSERT INTO game_notes(gameId,rating,notes,updatedAt) VALUES(?1,?2,'',?3)
         ON CONFLICT(gameId) DO UPDATE SET rating=?2,updatedAt=?3",
        params![game_id, rating, now_ms()],
    )?;
    prune_notes(conn, game_id)
}

pub fn set_notes(conn: &Connection, game_id: &str, notes: &str) -> Result<()> {
    let notes = valid_notes(notes)?;
    require_game(conn, game_id)?;
    conn.execute(
        "INSERT INTO game_notes(gameId,rating,notes,updatedAt) VALUES(?1,NULL,?2,?3)
         ON CONFLICT(gameId) DO UPDATE SET notes=?2,updatedAt=?3",
        params![game_id, notes, now_ms()],
    )?;
    prune_notes(conn, game_id)
}

/// Adds a memory (`id` None) or edits one (the screenshot must belong to the same game).
pub fn save_memory(
    conn: &Connection,
    game_id: &str,
    id: Option<i64>,
    date: &str,
    text: &str,
    screenshot_id: Option<&str>,
) -> Result<()> {
    let date = valid_date(date)?;
    let text = valid_memory_text(text)?;
    require_game(conn, game_id)?;
    let screenshot_id = match screenshot_id.filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => {
            let found: Option<String> = conn
                .query_row(
                    "SELECT id FROM game_screenshots WHERE id=? AND gameId=?",
                    params![s, game_id],
                    |r| r.get(0),
                )
                .optional()?;
            Some(found.ok_or_else(|| anyhow!("That screenshot is not one of this game's."))?)
        }
    };
    match id {
        Some(id) => {
            let changed = conn.execute(
                "UPDATE game_memories SET date=?1,text=?2,screenshotId=?3 WHERE id=?4 AND gameId=?5",
                params![date, text, screenshot_id, id, game_id],
            )?;
            if changed == 0 {
                bail!("That memory no longer exists.");
            }
        }
        None => {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM game_memories WHERE gameId=?",
                [game_id],
                |r| r.get(0),
            )?;
            if count as usize >= MAX_MEMORIES_PER_GAME {
                bail!("A game can have at most {MAX_MEMORIES_PER_GAME} memories.");
            }
            conn.execute(
                "INSERT INTO game_memories(gameId,date,text,screenshotId,createdAt) VALUES(?1,?2,?3,?4,?5)",
                params![game_id, date, text, screenshot_id, now_ms()],
            )?;
        }
    }
    Ok(())
}

pub fn delete_memory(conn: &Connection, game_id: &str, id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM game_memories WHERE id=? AND gameId=?",
        params![id, game_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Controller details

pub fn load_input(conn: &Connection, game_id: &str) -> Result<GameInput> {
    Ok(conn
        .query_row(
            "SELECT support,supportSource,preferred,notes,updatedAt,steamCheckedAt FROM game_input WHERE gameId=?",
            [game_id],
            |r| {
                Ok(GameInput {
                    support: r.get(0)?,
                    support_source: r.get(1)?,
                    preferred: r.get(2)?,
                    notes: r.get(3)?,
                    updated_at: r.get(4)?,
                    steam_checked_at: r.get(5)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

/// Saves the user's choices. A support level other than `unknown` is a manual choice that
/// Steam never overwrites (unless it is just the Steam level sent back unchanged); `unknown`
/// hands the level back to the automatic lookup.
pub fn save_input(
    conn: &Connection,
    game_id: &str,
    support: &str,
    preferred: &str,
    notes: &str,
) -> Result<()> {
    valid_support(support)?;
    let preferred = valid_preferred(preferred)?;
    let notes = valid_input_notes(notes)?;
    require_game(conn, game_id)?;
    let existing = load_input(conn, game_id)?;
    let source = if support == "unknown" {
        ""
    } else if existing.support_source == "steam" && existing.support == support {
        "steam"
    } else {
        "user"
    };
    conn.execute(
        "INSERT INTO game_input(gameId,support,supportSource,preferred,notes,updatedAt,steamCheckedAt) VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(gameId) DO UPDATE SET support=?2,supportSource=?3,preferred=?4,notes=?5,updatedAt=?6",
        params![
            game_id,
            support,
            source,
            preferred,
            notes,
            now_ms(),
            existing.steam_checked_at
        ],
    )?;
    prune_input(conn, game_id)
}

fn prune_input(conn: &Connection, game_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM game_input WHERE gameId=? AND support='unknown' AND preferred='' AND notes='' AND steamCheckedAt=0",
        [game_id],
    )?;
    Ok(())
}

/// Stores Steam's answer unless the user has chosen a level. Always records the check time.
pub fn apply_steam(conn: &Connection, game_id: &str, support: Option<&str>) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT OR IGNORE INTO game_input(gameId,support,supportSource,preferred,notes,updatedAt,steamCheckedAt)
         VALUES(?1,'unknown','','','',?2,0)",
        params![game_id, now],
    )?;
    conn.execute(
        "UPDATE game_input SET steamCheckedAt=?2 WHERE gameId=?1",
        params![game_id, now],
    )?;
    if let Some(level) = support {
        valid_support(level)?;
        conn.execute(
            "UPDATE game_input SET support=?2,supportSource='steam',updatedAt=?3 WHERE gameId=?1 AND supportSource<>'user'",
            params![game_id, level, now],
        )?;
    }
    Ok(())
}

/// Reads `controller_support` out of an `appdetails` answer. `Ok(None)`: Steam has no entry
/// or declares nothing. Only `full` and `partial` are accepted.
pub fn parse_controller_support(body: &str, app_id: &str) -> Result<Option<&'static str>> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| anyhow!("Steam sent an unreadable answer."))?;
    let Some(entry) = value.get(app_id) else {
        bail!("Steam's answer did not include this game.");
    };
    if entry.get("success").and_then(|s| s.as_bool()) != Some(true) {
        return Ok(None);
    }
    match entry
        .pointer("/data/controller_support")
        .and_then(|s| s.as_str())
    {
        Some(s) if s.eq_ignore_ascii_case("full") => return Ok(Some("full")),
        Some(s) if s.eq_ignore_ascii_case("partial") => return Ok(Some("partial")),
        _ => {}
    }
    // Many games (TEKKEN 8, for one) leave `controller_support` out and only list the store
    // categories 28 "Full Controller Support" or 18 "Partial Controller Support".
    let ids: Vec<i64> = entry
        .pointer("/data/categories")
        .and_then(|c| c.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|c| c.get("id").and_then(|id| id.as_i64()))
                .collect()
        })
        .unwrap_or_default();
    Ok(if ids.contains(&28) {
        Some("full")
    } else if ids.contains(&18) {
        Some("partial")
    } else {
        None
    })
}

pub fn steam_due(input: &GameInput, now: i64) -> bool {
    input.support_source != "user" && now - input.steam_checked_at >= STEAM_TTL_MS
}

/// Which games have a Steam request running and which failed recently, so that many calls
/// for one game (a page opened repeatedly, several windows) make one request and a failing
/// Steam is not hammered.
#[derive(Default)]
struct SteamGate {
    in_flight: HashSet<String>,
    failed_at: HashMap<String, i64>,
}

impl SteamGate {
    /// `true` when this caller should fetch (and must call `finish` afterwards).
    fn begin(&mut self, game_id: &str, now: i64) -> bool {
        if self.in_flight.contains(game_id) {
            return false;
        }
        if self
            .failed_at
            .get(game_id)
            .is_some_and(|at| now - at < STEAM_RETRY_AFTER_MS)
        {
            return false;
        }
        self.failed_at.remove(game_id);
        self.in_flight.insert(game_id.to_string());
        true
    }

    fn finish(&mut self, game_id: &str, ok: bool, now: i64) {
        self.in_flight.remove(game_id);
        if !ok {
            self.failed_at.insert(game_id.to_string(), now);
        }
    }
}

static STEAM_GATE: LazyLock<Mutex<SteamGate>> = LazyLock::new(Mutex::default);

/// Holds a game's slot in `STEAM_GATE`; dropped (also when the command is cancelled) it frees
/// the slot, and records a failure unless `succeeded` was called.
struct SteamSlot {
    game_id: String,
    ok: bool,
}

impl SteamSlot {
    fn take(game_id: &str) -> Option<Self> {
        let mut gate = STEAM_GATE.lock().unwrap_or_else(|p| p.into_inner());
        gate.begin(game_id, now_ms()).then(|| Self {
            game_id: game_id.to_string(),
            ok: false,
        })
    }

    fn succeeded(&mut self) {
        self.ok = true;
    }
}

impl Drop for SteamSlot {
    fn drop(&mut self) {
        let mut gate = STEAM_GATE.lock().unwrap_or_else(|p| p.into_inner());
        gate.finish(&self.game_id, self.ok, now_ms());
    }
}

static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// Waits so that Steam requests are at least [`MIN_GAP`] apart.
async fn polite_wait() {
    let wait = {
        let mut last = LAST_REQUEST.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let start = match *last {
            Some(prev) if prev + MIN_GAP > now => prev + MIN_GAP,
            _ => now,
        };
        *last = Some(start);
        start - now
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

async fn fetch_steam(app_id: &str) -> Result<Option<&'static str>> {
    polite_wait().await;
    let client = reqwest::Client::builder()
        .timeout(STEAM_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .user_agent(concat!("GameLobbies/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("Could not start the Steam client: {}", e.without_url()))?;
    let mut response = client
        .get("https://store.steampowered.com/api/appdetails")
        .query(&[("appids", app_id)])
        .send()
        .await
        .map_err(|e| anyhow!("Could not reach Steam: {}", e.without_url()))?;
    if !response.status().is_success() {
        bail!("Steam answered with status {}.", response.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow!("Could not read Steam's answer: {}", e.without_url()))?
    {
        if body.len() + chunk.len() > STEAM_MAX_BYTES {
            bail!("Steam's answer was too large.");
        }
        body.extend_from_slice(&chunk);
    }
    parse_controller_support(&String::from_utf8_lossy(&body), app_id)
}

// ---------------------------------------------------------------------------
// Backup

pub fn export_backup(conn: &Connection) -> Result<NotesBackup> {
    let notes = conn
        .prepare("SELECT gameId,rating,notes FROM game_notes ORDER BY gameId")?
        .query_map([], |r| {
            Ok(NoteBackup {
                game_id: r.get(0)?,
                rating: r.get(1)?,
                notes: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let memories = conn
        .prepare("SELECT gameId,date,text FROM game_memories ORDER BY gameId,date,id")?
        .query_map([], |r| {
            Ok(MemoryBackup {
                game_id: r.get(0)?,
                date: r.get(1)?,
                text: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let input = conn
        .prepare(
            "SELECT gameId,support,supportSource,preferred,notes FROM game_input ORDER BY gameId",
        )?
        .query_map([], |r| {
            Ok(InputBackup {
                game_id: r.get(0)?,
                support: r.get(1)?,
                support_source: r.get(2)?,
                preferred: r.get(3)?,
                notes: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(NotesBackup {
        notes,
        memories,
        input,
    })
}

/// Restores a section inside the caller's transaction. Rows for games that are not in the
/// library are skipped, apart from the notes and controller details parked for the linked
/// entries in `parked` (restored from the same backup, `links::restore_records`). A game's
/// memories are replaced by the backup's.
pub fn restore_backup(
    conn: &Connection,
    backup: &NotesBackup,
    parked: &std::collections::HashSet<String>,
) -> Result<()> {
    validate_backup(backup)?;
    let now = now_ms();
    for n in &backup.notes {
        if !parked.contains(&n.game_id) && !game_exists(conn, &n.game_id)? {
            continue;
        }
        conn.execute(
            "INSERT OR REPLACE INTO game_notes(gameId,rating,notes,updatedAt) VALUES(?1,?2,?3,?4)",
            params![n.game_id, n.rating, n.notes, now],
        )?;
        prune_notes(conn, &n.game_id)?;
    }
    let mut cleared = std::collections::HashSet::new();
    for m in &backup.memories {
        if !game_exists(conn, &m.game_id)? {
            continue;
        }
        if cleared.insert(m.game_id.as_str()) {
            conn.execute("DELETE FROM game_memories WHERE gameId=?", [&m.game_id])?;
        }
        conn.execute(
            "INSERT INTO game_memories(gameId,date,text,screenshotId,createdAt) VALUES(?1,?2,?3,NULL,?4)",
            params![m.game_id, m.date, m.text.trim(), now],
        )?;
    }
    for i in &backup.input {
        if !parked.contains(&i.game_id) && !game_exists(conn, &i.game_id)? {
            continue;
        }
        // A level without a source in the file counts as the user's own choice.
        let source = match (i.support.as_str(), i.support_source.as_str()) {
            ("unknown", _) => "",
            (_, "") => "user",
            (_, s) => s,
        };
        conn.execute(
            "INSERT OR REPLACE INTO game_input(gameId,support,supportSource,preferred,notes,updatedAt,steamCheckedAt) VALUES(?1,?2,?3,?4,?5,?6,0)",
            params![i.game_id, i.support, source, i.preferred.trim(), i.notes, now],
        )?;
        prune_input(conn, &i.game_id)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Changed<'a> {
    game_id: &'a str,
}

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

#[tauri::command]
pub async fn notes_get(state: State<'_, AppState>, game_id: String) -> CmdResult<GameNotes> {
    load(&state.db.lock(), &game_id).map_err(err)
}

#[tauri::command]
pub async fn notes_set_rating(
    state: State<'_, AppState>,
    game_id: String,
    rating: Option<f64>,
) -> CmdResult<GameNotes> {
    let conn = state.db.lock();
    set_rating(&conn, &game_id, rating).map_err(err)?;
    load(&conn, &game_id).map_err(err)
}

#[tauri::command]
pub async fn notes_set_text(
    state: State<'_, AppState>,
    game_id: String,
    notes: String,
) -> CmdResult<GameNotes> {
    let conn = state.db.lock();
    set_notes(&conn, &game_id, &notes).map_err(err)?;
    load(&conn, &game_id).map_err(err)
}

#[tauri::command]
pub async fn notes_save_memory(
    state: State<'_, AppState>,
    game_id: String,
    id: Option<i64>,
    date: String,
    text: String,
    screenshot_id: Option<String>,
) -> CmdResult<GameNotes> {
    let conn = state.db.lock();
    save_memory(&conn, &game_id, id, &date, &text, screenshot_id.as_deref()).map_err(err)?;
    load(&conn, &game_id).map_err(err)
}

#[tauri::command]
pub async fn notes_delete_memory(
    state: State<'_, AppState>,
    game_id: String,
    id: i64,
) -> CmdResult<GameNotes> {
    let conn = state.db.lock();
    delete_memory(&conn, &game_id, id).map_err(err)?;
    load(&conn, &game_id).map_err(err)
}

#[tauri::command]
pub async fn input_get(state: State<'_, AppState>, game_id: String) -> CmdResult<GameInput> {
    load_input(&state.db.lock(), &game_id).map_err(err)
}

#[tauri::command]
pub async fn input_save(
    state: State<'_, AppState>,
    game_id: String,
    support: String,
    preferred: String,
    notes: String,
) -> CmdResult<GameInput> {
    let conn = state.db.lock();
    save_input(&conn, &game_id, &support, &preferred, &notes).map_err(err)?;
    load_input(&conn, &game_id).map_err(err)
}

/// Asks Steam about this game's controller support when it is a Steam game, the user has not
/// chosen a level and the last answer is older than a week. Failures (offline, rate limited)
/// are remembered for ten minutes (that game is not asked again meanwhile), and a game already
/// being asked is not asked twice. Returns the stored details either way.
#[tauri::command]
pub async fn input_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
    game_id: String,
) -> CmdResult<GameInput> {
    let (app_id, input) = {
        let conn = state.db.lock();
        let app_id = db::get_game(&conn, &game_id)
            .map_err(err)?
            .and_then(|g| steam_app_id(&g).map(str::to_string));
        (app_id, load_input(&conn, &game_id).map_err(err)?)
    };
    let Some(app_id) = app_id else {
        return Ok(input);
    };
    if !steam_due(&input, now_ms()) {
        return Ok(input);
    }
    // One request per game at a time, and none right after a failure.
    let Some(mut slot) = SteamSlot::take(&game_id) else {
        return Ok(input);
    };
    let Ok(found) = fetch_steam(&app_id).await else {
        return Ok(input);
    };
    slot.succeeded();
    let conn = state.db.lock();
    // The game may have been removed while waiting.
    if !game_exists(&conn, &game_id).map_err(err)? {
        return Ok(input);
    }
    apply_steam(&conn, &game_id, found).map_err(err)?;
    let updated = load_input(&conn, &game_id).map_err(err)?;
    drop(conn);
    let _ = app.emit("input:changed", Changed { game_id: &game_id });
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::types::{Game, GamePatch};

    fn setup() -> Db {
        let db = Db::memory().unwrap();
        {
            let conn = db.lock();
            for id in ["steam:1", "b"] {
                db::save_game(
                    &conn,
                    &GamePatch::from(&Game {
                        id: id.into(),
                        title: id.into(),
                        store: "Steam".into(),
                        ..Default::default()
                    }),
                )
                .unwrap();
            }
        }
        db
    }

    #[test]
    fn steam_requests_are_single_flight_and_back_off_after_failures() {
        let mut gate = SteamGate::default();
        let t0 = 1_000_000;
        assert!(gate.begin("a", t0));
        // A concurrent call for the same game does not fetch; another game does.
        assert!(!gate.begin("a", t0));
        assert!(gate.begin("b", t0));
        gate.finish("b", true, t0);
        assert!(gate.begin("b", t0 + 1));
        gate.finish("b", true, t0 + 1);
        // A failure blocks that game (only) until the backoff has passed.
        gate.finish("a", false, t0 + 5);
        assert!(!gate.begin("a", t0 + 6));
        assert!(!gate.begin("a", t0 + 5 + STEAM_RETRY_AFTER_MS - 1));
        assert!(gate.begin("a", t0 + 5 + STEAM_RETRY_AFTER_MS));
        // Success clears the way at once.
        gate.finish("a", true, t0 + 7 + STEAM_RETRY_AFTER_MS);
        assert!(gate.begin("a", t0 + 8 + STEAM_RETRY_AFTER_MS));
    }

    #[test]
    fn a_dropped_slot_frees_the_game_and_counts_as_a_failure_unless_it_succeeded() {
        let mut ok = SteamSlot::take("slot-ok").unwrap();
        assert!(SteamSlot::take("slot-ok").is_none());
        ok.succeeded();
        drop(ok);
        let mut again = SteamSlot::take("slot-ok").unwrap();
        again.succeeded();
        drop(again);
        let failed = SteamSlot::take("slot-failed").unwrap();
        drop(failed);
        assert!(SteamSlot::take("slot-failed").is_none());
    }

    #[test]
    fn rating_notes_save_load_and_clear() {
        let db = setup();
        let c = db.lock();
        assert_eq!(load(&c, "b").unwrap(), GameNotes::default());
        set_rating(&c, "b", Some(3.5)).unwrap();
        set_notes(&c, "b", "line one\nline two").unwrap();
        let n = load(&c, "b").unwrap();
        assert_eq!(n.rating, Some(3.5));
        assert_eq!(n.notes, "line one\nline two");
        set_rating(&c, "b", None).unwrap();
        let n = load(&c, "b").unwrap();
        assert_eq!((n.rating, n.notes.as_str()), (None, "line one\nline two"));
        set_notes(&c, "b", "").unwrap();
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM game_notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert!(set_rating(&c, "missing", Some(1.0)).is_err());
    }

    #[test]
    fn validation_limits() {
        let db = setup();
        let c = db.lock();
        for bad in [0.0, 0.25, 5.5, -1.0, f64::NAN, f64::INFINITY, 4.9] {
            assert!(set_rating(&c, "b", Some(bad)).is_err(), "{bad}");
        }
        for good in [0.5, 1.0, 4.5, 5.0] {
            assert!(set_rating(&c, "b", Some(good)).is_ok());
        }
        assert!(set_notes(&c, "b", &"x".repeat(MAX_NOTES_CHARS)).is_ok());
        assert!(set_notes(&c, "b", &"x".repeat(MAX_NOTES_CHARS + 1)).is_err());
        assert!(set_notes(&c, "b", "a\0b").is_err());
        for bad in [
            "2025-02-30",
            "2025-13-01",
            "2025-1-01",
            "20250101",
            "1969-12-31",
            "2025-00-10",
            "abcd-ef-gh",
            "2025-02-29",
        ] {
            assert!(valid_date(bad).is_err(), "{bad}");
        }
        assert!(valid_date("2024-02-29").is_ok());
        assert!(save_memory(&c, "b", None, "2025-01-01", "   ", None).is_err());
        let long = "x".repeat(MAX_MEMORY_CHARS + 1);
        assert!(save_memory(&c, "b", None, "2025-01-01", &long, None).is_err());
        let max = "x".repeat(MAX_MEMORY_CHARS);
        assert!(save_memory(&c, "b", None, "2025-01-01", &max, None).is_ok());
        assert!(save_input(&c, "b", "sometimes", "", "").is_err());
        let long = "x".repeat(MAX_PREFERRED_CHARS + 1);
        assert!(save_input(&c, "b", "full", &long, "").is_err());
        let long = "x".repeat(MAX_INPUT_NOTES_CHARS + 1);
        assert!(save_input(&c, "b", "full", "Xbox", &long).is_err());
    }

    #[test]
    fn memories_add_edit_delete_and_screenshot_links() {
        let db = setup();
        let c = db.lock();
        c.execute(
            "INSERT INTO game_screenshots(id,gameId,file,hash,width,height,addedAt) VALUES('s1','b','d/f.png','h',1,1,0),('s2','steam:1','d/g.png','h2',1,1,0)",
            [],
        )
        .unwrap();
        save_memory(&c, "b", None, "2025-01-02", "first", Some("s1")).unwrap();
        save_memory(&c, "b", None, "2025-03-04", "second", None).unwrap();
        // Another game's screenshot is refused.
        assert!(save_memory(&c, "b", None, "2025-03-04", "x", Some("s2")).is_err());
        let list = load(&c, "b").unwrap().memories;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text, "second"); // newest date first
        assert_eq!(list[1].screenshot_id.as_deref(), Some("s1"));
        save_memory(&c, "b", Some(list[0].id), "2025-03-05", "edited", None).unwrap();
        assert_eq!(load(&c, "b").unwrap().memories[0].text, "edited");
        // Editing through another game's id does nothing.
        assert!(save_memory(&c, "steam:1", Some(list[0].id), "2025-03-05", "x", None).is_err());
        // Removing the screenshot unlinks the memory.
        c.execute("DELETE FROM game_screenshots WHERE id='s1'", [])
            .unwrap();
        assert_eq!(load(&c, "b").unwrap().memories[1].screenshot_id, None);
        delete_memory(&c, "b", list[0].id).unwrap();
        assert_eq!(load(&c, "b").unwrap().memories.len(), 1);
    }

    #[test]
    fn memory_count_is_capped() {
        let db = setup();
        let c = db.lock();
        for _ in 0..MAX_MEMORIES_PER_GAME {
            save_memory(&c, "b", None, "2025-01-01", "m", None).unwrap();
        }
        assert!(save_memory(&c, "b", None, "2025-01-01", "m", None).is_err());
    }

    #[test]
    fn parses_steam_controller_support_from_fixtures() {
        let full = r#"{"570":{"success":true,"data":{"type":"game","controller_support":"full"}}}"#;
        let partial = r#"{"570":{"success":true,"data":{"controller_support":"partial"}}}"#;
        let none = r#"{"570":{"success":true,"data":{"type":"game"}}}"#;
        let failed = r#"{"570":{"success":false}}"#;
        let odd = r#"{"570":{"success":true,"data":{"controller_support":"<script>"}}}"#;
        assert_eq!(parse_controller_support(full, "570").unwrap(), Some("full"));
        assert_eq!(
            parse_controller_support(partial, "570").unwrap(),
            Some("partial")
        );
        assert_eq!(parse_controller_support(none, "570").unwrap(), None);
        assert_eq!(parse_controller_support(failed, "570").unwrap(), None);
        assert_eq!(parse_controller_support(odd, "570").unwrap(), None);
        assert!(parse_controller_support(full, "571").is_err());
        assert!(parse_controller_support("not json", "570").is_err());
        // No controller_support field: fall back to the store categories.
        let cat_partial = r#"{"1778820":{"success":true,"data":{"categories":[{"id":2},{"id":18,"description":"Partial Controller Support"}]}}}"#;
        let cat_full = r#"{"7":{"success":true,"data":{"categories":[{"id":18},{"id":28}]}}}"#;
        let cat_none = r#"{"7":{"success":true,"data":{"categories":[{"id":2},{"id":"28"}]}}}"#;
        assert_eq!(
            parse_controller_support(cat_partial, "1778820").unwrap(),
            Some("partial")
        );
        assert_eq!(
            parse_controller_support(cat_full, "7").unwrap(),
            Some("full")
        );
        assert_eq!(parse_controller_support(cat_none, "7").unwrap(), None);
    }

    #[test]
    fn steam_fills_in_but_never_overwrites_a_manual_choice() {
        let db = setup();
        let c = db.lock();
        apply_steam(&c, "steam:1", Some("partial")).unwrap();
        let i = load_input(&c, "steam:1").unwrap();
        assert_eq!(
            (i.support.as_str(), i.support_source.as_str()),
            ("partial", "steam")
        );
        assert!(i.steam_checked_at > 0);
        // Saving other fields keeps the Steam level and source.
        save_input(&c, "steam:1", "partial", "Xbox", "layout 2").unwrap();
        assert_eq!(load_input(&c, "steam:1").unwrap().support_source, "steam");
        // A different level is a manual choice, and Steam no longer touches it.
        save_input(&c, "steam:1", "none", "Xbox", "layout 2").unwrap();
        apply_steam(&c, "steam:1", Some("full")).unwrap();
        let i = load_input(&c, "steam:1").unwrap();
        assert_eq!(
            (i.support.as_str(), i.support_source.as_str()),
            ("none", "user")
        );
        assert!(!steam_due(&i, now_ms() + STEAM_TTL_MS * 2));
        // Back to automatic: Steam may fill it again.
        save_input(&c, "steam:1", "unknown", "Xbox", "layout 2").unwrap();
        apply_steam(&c, "steam:1", Some("full")).unwrap();
        assert_eq!(load_input(&c, "steam:1").unwrap().support, "full");
        // A check that found nothing is remembered and changes no level.
        apply_steam(&c, "b", None).unwrap();
        let i = load_input(&c, "b").unwrap();
        assert_eq!(i.support, "unknown");
        assert!(!steam_due(&i, now_ms()));
        assert!(steam_due(&i, now_ms() + STEAM_TTL_MS + 1));
    }

    #[test]
    fn input_clears_when_empty() {
        let db = setup();
        let c = db.lock();
        save_input(&c, "b", "full", "Xbox", "").unwrap();
        save_input(&c, "b", "unknown", "", "").unwrap();
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM game_input", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn backup_round_trips_and_skips_unknown_games() {
        let db = setup();
        let mut backup = {
            let c = db.lock();
            set_rating(&c, "b", Some(4.5)).unwrap();
            set_notes(&c, "b", "notes").unwrap();
            save_memory(&c, "b", None, "2025-05-06", "memory", None).unwrap();
            save_input(&c, "b", "partial", "PlayStation", "use layout").unwrap();
            apply_steam(&c, "steam:1", Some("full")).unwrap();
            export_backup(&c).unwrap()
        };
        assert_eq!(backup.notes.len(), 1);
        assert_eq!(backup.input.len(), 2);
        validate_backup(&backup).unwrap();
        let json = serde_json::to_string(&backup).unwrap();
        let back: NotesBackup = serde_json::from_str(&json).unwrap();
        assert_eq!(back, backup);

        backup.notes.push(NoteBackup {
            game_id: "ghost".into(),
            rating: Some(1.0),
            notes: String::new(),
        });
        let other = setup();
        let mut c = other.lock();
        let tx = c.transaction().unwrap();
        restore_backup(&tx, &backup, &Default::default()).unwrap();
        tx.commit().unwrap();
        let n = load(&c, "b").unwrap();
        assert_eq!((n.rating, n.notes.as_str()), (Some(4.5), "notes"));
        assert_eq!(n.memories.len(), 1);
        let i = load_input(&c, "b").unwrap();
        assert_eq!(
            (
                i.support.as_str(),
                i.support_source.as_str(),
                i.preferred.as_str()
            ),
            ("partial", "user", "PlayStation")
        );
        assert_eq!(load_input(&c, "steam:1").unwrap().support_source, "steam");
        assert_eq!(load(&c, "ghost").unwrap(), GameNotes::default());
        // Restoring again replaces memories instead of duplicating them.
        let tx = c.transaction().unwrap();
        restore_backup(&tx, &backup, &Default::default()).unwrap();
        tx.commit().unwrap();
        assert_eq!(load(&c, "b").unwrap().memories.len(), 1);
    }

    #[test]
    fn import_validation_rejects_bad_values() {
        let ok = NotesBackup {
            notes: vec![NoteBackup {
                game_id: "a".into(),
                rating: Some(2.5),
                notes: "n".into(),
            }],
            memories: vec![MemoryBackup {
                game_id: "a".into(),
                date: "2025-01-01".into(),
                text: "t".into(),
            }],
            input: vec![InputBackup {
                game_id: "a".into(),
                support: "full".into(),
                support_source: "steam".into(),
                preferred: "Xbox".into(),
                notes: String::new(),
            }],
        };
        validate_backup(&ok).unwrap();
        let bad = |edit: &dyn Fn(&mut NotesBackup)| {
            let mut b = ok.clone();
            edit(&mut b);
            validate_backup(&b).is_err()
        };
        assert!(bad(&|b| b.notes[0].rating = Some(7.0)));
        assert!(bad(&|b| b.notes[0].notes = "x".repeat(MAX_NOTES_CHARS + 1)));
        assert!(bad(&|b| b.memories[0].date = "yesterday".into()));
        assert!(bad(
            &|b| b.memories[0].text = "x".repeat(MAX_MEMORY_CHARS + 1)
        ));
        assert!(bad(&|b| b.input[0].support = "total".into()));
        assert!(bad(&|b| b.input[0].support_source = "plugin".into()));
        assert!(bad(&|b| b.notes[0].game_id = String::new()));
        assert!(bad(&|b| {
            b.memories = vec![b.memories[0].clone(); MAX_MEMORIES_PER_GAME + 1]
        }));
        // Unknown fields are refused.
        assert!(
            serde_json::from_str::<NotesBackup>(r#"{"notes":[{"gameId":"a","exe":"x"}]}"#).is_err()
        );
    }

    #[test]
    fn removing_a_game_removes_its_notes() {
        let db = setup();
        let c = db.lock();
        set_notes(&c, "b", "n").unwrap();
        save_memory(&c, "b", None, "2025-01-01", "m", None).unwrap();
        save_input(&c, "b", "full", "", "").unwrap();
        db::remove_game(&c, "b").unwrap();
        for t in ["game_notes", "game_memories", "game_input"] {
            let n: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{t}");
        }
    }
}
