//! Play session history (table `play_sessions`, written by `tracker`), the session journal
//! (a note and linked screenshots per session), share summaries and session backups.
//!
//! - **Schema** ([`migrate`]): `play_sessions` gains `edited` and `interrupted` flags; notes live
//!   in `session_notes`, screenshot links in `session_screenshots`. Both cascade when the
//!   session (or the screenshot) is deleted.
//! - **Recovery**: a session still open when the library is opened belongs to a run that ended
//!   without closing it (crash, power loss). The tracker saves a session's length on every poll,
//!   so `startedAt + seconds` is the last time the game was seen running: that becomes the end,
//!   and the session is marked `interrupted`. Sessions are never counted up to "now".
//! - **Duplicates**: [`start_session`] returns the existing row for a game and start time
//!   instead of inserting a second one.
//! - **Playtime totals**: `games.playtimeSeconds` includes playtime imported from stores, so it
//!   is never recomputed from the sessions. Editing or deleting a session adjusts it by the
//!   change in that session's length (never below zero), as tracking adds to it.
//! - **Renderer trust**: the renderer sends ids, times and note text only. Exports are written
//!   to a path chosen in the native save dialog, always as `.txt` or `.md`.

use crate::state::AppState;
use crate::types::{now_ms, Game};
use crate::{catalog, db, screenshots};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// Longest session note, in characters.
pub const MAX_NOTE_CHARS: usize = 2000;
/// Most screenshots linked to one session.
pub const MAX_SESSION_SCREENSHOTS: usize = 24;
/// Most sessions per page.
pub const MAX_PAGE: i64 = 200;
/// Earliest start an edit accepts (2000-01-01).
pub const MIN_START_MS: i64 = 946_684_800_000;
/// Longest session an edit accepts.
pub const MAX_SESSION_MS: i64 = 30 * 24 * 3_600_000;
/// Most sessions in a backup.
pub const MAX_BACKUP_SESSIONS: usize = 500_000;
/// Clock difference tolerated for backups made on another PC.
const BACKUP_FUTURE_SLACK_MS: i64 = 24 * 3_600_000;

// ---------------------------------------------------------------------------
// Schema

/// Adds the session history columns and tables (idempotent), then closes sessions left open by
/// an earlier run. Called from `db::migrate` on every open.
pub fn migrate(db: &Connection) -> Result<()> {
    let columns: HashSet<String> = db
        .prepare("SELECT name FROM pragma_table_info('play_sessions')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for name in ["edited", "interrupted"] {
        if !columns.contains(name) {
            db.execute_batch(&format!(
                "ALTER TABLE play_sessions ADD COLUMN {name} INTEGER NOT NULL DEFAULT 0;"
            ))?;
        }
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS play_sessions_started ON play_sessions(startedAt,id);
         CREATE TABLE IF NOT EXISTS session_notes(sessionId INTEGER PRIMARY KEY REFERENCES play_sessions(id) ON DELETE CASCADE,note TEXT NOT NULL,updatedAt INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS session_screenshots(sessionId INTEGER NOT NULL REFERENCES play_sessions(id) ON DELETE CASCADE,screenshotId TEXT NOT NULL REFERENCES game_screenshots(id) ON DELETE CASCADE,PRIMARY KEY(sessionId,screenshotId));
         CREATE INDEX IF NOT EXISTS session_screenshots_shot ON session_screenshots(screenshotId);",
    )?;
    recover_interrupted(db)?;
    Ok(())
}

/// Closes every open session at its last saved length and marks it interrupted. Only called
/// when the library is opened, before tracking starts, so no open session is a live one.
pub fn recover_interrupted(db: &Connection) -> Result<usize> {
    Ok(db.execute(
        "UPDATE play_sessions SET endedAt=startedAt+MAX(seconds,0)*1000,interrupted=1 WHERE endedAt IS NULL",
        [],
    )?)
}

/// Stores the start of a tracked session, or returns the session already stored for this game
/// and start time (a repeated event), keeping the longer length. `None` if the game is gone.
pub fn start_session(
    db: &Connection,
    game_id: &str,
    started_at: i64,
    seconds: i64,
    source: &str,
) -> Result<Option<i64>> {
    let existing: Option<i64> = db
        .query_row(
            "SELECT id FROM play_sessions WHERE gameId=? AND startedAt=? ORDER BY id LIMIT 1",
            params![game_id, started_at],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        db::update_session(db, id, seconds)?;
        return Ok(Some(id));
    }
    db::insert_session(db, game_id, started_at, seconds, source)
}

// ---------------------------------------------------------------------------
// Reading

/// A screenshot linked to a session.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionShot {
    pub id: String,
    /// `lobby-shot://<dir>/<file>`.
    pub src: String,
}

/// One session in the history view.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub id: i64,
    pub game_id: String,
    pub game_title: String,
    pub started_at: i64,
    /// Absent while the session is in progress.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub seconds: i64,
    /// `"launch"` or `"detected"`.
    pub source: String,
    pub edited: bool,
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub screenshots: Vec<SessionShot>,
}

/// Keyset position: the last session of the previous page.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cursor {
    pub started_at: i64,
    pub id: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub sessions: Vec<SessionEntry>,
    /// Pass back to load the next page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
    /// Read-only "Added from Steam" entries (one per game with a positive difference), on the
    /// first page only. Not sessions: they cannot be edited, deleted, shared or exported, and
    /// are not counted as sessions.
    pub store_entries: Vec<crate::stats::StoreAddition>,
}

const ENTRY_SELECT: &str = "SELECT s.id,s.gameId,COALESCE(g.title,s.gameId),s.startedAt,s.endedAt,s.seconds,s.source,s.edited,s.interrupted,n.note FROM play_sessions s LEFT JOIN games g ON g.id=s.gameId LEFT JOIN session_notes n ON n.sessionId=s.id";

fn read_entry(row: &rusqlite::Row) -> rusqlite::Result<SessionEntry> {
    Ok(SessionEntry {
        id: row.get(0)?,
        game_id: row.get(1)?,
        game_title: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        seconds: row.get::<_, i64>(5)?.max(0),
        source: row.get(6)?,
        edited: row.get::<_, i64>(7)? != 0,
        interrupted: row.get::<_, i64>(8)? != 0,
        note: row.get(9)?,
        screenshots: Vec::new(),
    })
}

fn attach_shots(db: &Connection, entries: &mut [SessionEntry]) -> Result<()> {
    let mut stmt = db.prepare(
        "SELECT t.id,t.file FROM session_screenshots l JOIN game_screenshots t ON t.id=l.screenshotId WHERE l.sessionId=? ORDER BY t.addedAt DESC,t.rowid DESC",
    )?;
    for entry in entries {
        entry.screenshots = stmt
            .query_map([entry.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            // Rows whose file name was tampered with are not shown (as in `screenshots::list`).
            .filter(|(_, file)| screenshots::valid_relative(file).is_some())
            .map(|(id, file)| SessionShot {
                id,
                src: format!("{}{file}", screenshots::SHOT_PREFIX),
            })
            .collect();
    }
    Ok(())
}

/// One session, or `None` if it no longer exists.
pub fn entry(db: &Connection, id: i64) -> Result<Option<SessionEntry>> {
    let found = db
        .query_row(&format!("{ENTRY_SELECT} WHERE s.id=?"), [id], read_entry)
        .optional()?;
    let Some(entry) = found else {
        return Ok(None);
    };
    let mut entries = [entry];
    attach_shots(db, &mut entries)?;
    let [entry] = entries;
    Ok(Some(entry))
}

/// Sessions newest first, for one game or all games, `limit` at a time after `cursor`.
pub fn page(
    db: &Connection,
    game_id: Option<&str>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<SessionPage> {
    let limit = limit.clamp(1, MAX_PAGE);
    let mut stmt = db.prepare(&format!(
        "{ENTRY_SELECT} WHERE (?1 IS NULL OR s.gameId=?1) AND (?2 IS NULL OR s.startedAt<?2 OR (s.startedAt=?2 AND s.id<?3)) ORDER BY s.startedAt DESC,s.id DESC LIMIT ?4"
    ))?;
    let mut sessions = stmt
        .query_map(
            params![
                game_id,
                cursor.map(|c| c.started_at),
                cursor.map(|c| c.id),
                limit + 1
            ],
            read_entry,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = sessions.len() as i64 > limit;
    sessions.truncate(limit as usize);
    attach_shots(db, &mut sessions)?;
    let next = more.then(|| sessions.last()).flatten().map(|last| Cursor {
        started_at: last.started_at,
        id: last.id,
    });
    let store_entries = if cursor.is_none() {
        crate::stats::additions(db, game_id)?
    } else {
        Vec::new()
    };
    Ok(SessionPage {
        sessions,
        next,
        store_entries,
    })
}

// ---------------------------------------------------------------------------
// Editing

struct Stored {
    game_id: String,
    ended_at: Option<i64>,
    seconds: i64,
}

fn stored(db: &Connection, id: i64) -> Result<Stored> {
    db.query_row(
        "SELECT gameId,endedAt,seconds FROM play_sessions WHERE id=?",
        [id],
        |row| {
            Ok(Stored {
                game_id: row.get(0)?,
                ended_at: row.get(1)?,
                seconds: row.get::<_, i64>(2)?.max(0),
            })
        },
    )
    .optional()?
    .ok_or_else(|| anyhow!("This session no longer exists."))
}

/// Whether another session of the game overlaps `[start, end)`. A session in progress counts
/// as running until `now`.
fn overlaps(
    db: &Connection,
    game_id: &str,
    except: Option<i64>,
    start: i64,
    end: i64,
    now: i64,
) -> Result<bool> {
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM play_sessions WHERE gameId=?1 AND id<>COALESCE(?2,-1) AND startedAt<?4 AND COALESCE(endedAt,MAX(?5,startedAt+seconds*1000))>?3",
        params![game_id, except, start, end, now],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Checks a corrected start and end. Returns the new length in seconds.
pub fn validate_times(started_at: i64, ended_at: i64, now: i64) -> Result<i64> {
    if started_at < MIN_START_MS {
        bail!("The start must be after 1 January 2000.");
    }
    if ended_at <= started_at {
        bail!("The end must be after the start.");
    }
    if ended_at > now {
        bail!("The end can't be in the future.");
    }
    if ended_at - started_at < 1000 {
        bail!("A session must be at least one second long.");
    }
    if ended_at - started_at > MAX_SESSION_MS {
        bail!("A session can be at most 30 days long.");
    }
    Ok((ended_at - started_at) / 1000)
}

/// Adds `delta` seconds (may be negative) to a game's playtime, never going below zero.
fn adjust_playtime(db: &Connection, game_id: &str, delta: i64) -> Result<()> {
    if delta != 0 {
        db.execute(
            "UPDATE games SET playtimeSeconds=MAX(0,playtimeSeconds+?1),playtimeMinutes=CAST(MAX(0,playtimeSeconds+?1)/60 AS INTEGER) WHERE id=?2",
            params![delta, game_id],
        )?;
    }
    Ok(())
}

/// Corrects a finished session's start and end. Its length becomes the time between them, the
/// game's playtime changes by the difference, and the session is marked edited.
pub fn update(
    db: &Connection,
    id: i64,
    started_at: i64,
    ended_at: i64,
    now: i64,
) -> Result<SessionEntry> {
    let tx = db.unchecked_transaction()?;
    let old = stored(&tx, id)?;
    if old.ended_at.is_none() {
        bail!("This session is still running. Edit it after it ends.");
    }
    let seconds = validate_times(started_at, ended_at, now)?;
    if overlaps(&tx, &old.game_id, Some(id), started_at, ended_at, now)? {
        bail!("These times overlap another session of this game.");
    }
    tx.execute(
        "UPDATE play_sessions SET startedAt=?2,endedAt=?3,seconds=?4,edited=1 WHERE id=?1",
        params![id, started_at, ended_at, seconds],
    )?;
    adjust_playtime(&tx, &old.game_id, seconds - old.seconds)?;
    tx.execute(
        "UPDATE games SET lastPlayed=MAX(COALESCE(lastPlayed,0),?2) WHERE id=?1",
        params![old.game_id, started_at],
    )?;
    tx.commit()?;
    entry(db, id)?.ok_or_else(|| anyhow!("This session no longer exists."))
}

/// Deletes a finished session with its note and screenshot links, and takes its length off
/// the game's playtime. Returns the game id.
pub fn delete(db: &Connection, id: i64) -> Result<String> {
    let tx = db.unchecked_transaction()?;
    let old = stored(&tx, id)?;
    if old.ended_at.is_none() {
        bail!("This session is still running. Delete it after it ends.");
    }
    tx.execute("DELETE FROM session_screenshots WHERE sessionId=?", [id])?;
    tx.execute("DELETE FROM session_notes WHERE sessionId=?", [id])?;
    tx.execute("DELETE FROM play_sessions WHERE id=?", [id])?;
    adjust_playtime(&tx, &old.game_id, -old.seconds)?;
    tx.commit()?;
    Ok(old.game_id)
}

/// A note as stored: line endings normalized, control characters other than newline and tab
/// removed, trimmed. Empty means no note.
pub fn clean_note(raw: &str) -> Result<String> {
    let text: String = raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let text = text.trim().to_string();
    if text.chars().count() > MAX_NOTE_CHARS {
        bail!("A session note can be at most {MAX_NOTE_CHARS} characters.");
    }
    Ok(text)
}

/// Replaces a session's note and linked screenshots. The screenshots must be the game's own.
pub fn set_journal(
    db: &Connection,
    id: i64,
    note: &str,
    screenshot_ids: &[String],
    now: i64,
) -> Result<SessionEntry> {
    let note = clean_note(note)?;
    let mut ids: Vec<&str> = Vec::new();
    for shot in screenshot_ids {
        if !ids.contains(&shot.as_str()) {
            ids.push(shot);
        }
    }
    if ids.len() > MAX_SESSION_SCREENSHOTS {
        bail!("A session can have at most {MAX_SESSION_SCREENSHOTS} screenshots.");
    }
    let tx = db.unchecked_transaction()?;
    let session = stored(&tx, id)?;
    for shot in &ids {
        let owned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM game_screenshots WHERE id=? AND gameId=?)",
            params![shot, session.game_id],
            |row| row.get(0),
        )?;
        if !owned {
            bail!("A chosen screenshot is not one of this game's screenshots.");
        }
    }
    if note.is_empty() {
        tx.execute("DELETE FROM session_notes WHERE sessionId=?", [id])?;
    } else {
        tx.execute(
            "INSERT INTO session_notes(sessionId,note,updatedAt) VALUES(?1,?2,?3) ON CONFLICT(sessionId) DO UPDATE SET note=excluded.note,updatedAt=excluded.updatedAt",
            params![id, note, now],
        )?;
    }
    tx.execute("DELETE FROM session_screenshots WHERE sessionId=?", [id])?;
    for shot in &ids {
        tx.execute(
            "INSERT INTO session_screenshots(sessionId,screenshotId) VALUES(?,?)",
            params![id, shot],
        )?;
    }
    tx.commit()?;
    entry(db, id)?.ok_or_else(|| anyhow!("This session no longer exists."))
}

// ---------------------------------------------------------------------------
// Share summaries

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
}

impl Format {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "txt" => Ok(Self::Text),
            "md" => Ok(Self::Markdown),
            _ => bail!("Choose a text (txt) or Markdown (md) summary."),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Markdown => "md",
        }
    }
}

/// What a share summary shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
    pub title: String,
    pub started_at: i64,
    /// Absent while running.
    pub ended_at: Option<i64>,
    pub seconds: i64,
    pub source: String,
    /// Stores the game is in, e.g. `["Steam"]`.
    pub platforms: Vec<String>,
    /// Emulator and system names, e.g. `("Dolphin", "GameCube")`.
    pub emulators: Vec<(String, String)>,
    pub edited: bool,
    pub interrupted: bool,
    pub note: Option<String>,
    /// The viewer's UTC offset at the session's start, in minutes (east positive).
    pub utc_offset_minutes: i32,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Local calendar fields: (year, month 1-12, day, hour, minute, weekday 0 = Sunday).
fn civil(ms: i64, offset_minutes: i32) -> (i64, u32, u32, u32, u32, usize) {
    let local = ms + offset_minutes as i64 * 60_000;
    let days = local.div_euclid(86_400_000);
    let of_day = local.rem_euclid(86_400_000) / 60_000;
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let weekday = (days + 4).rem_euclid(7) as usize;
    (
        year,
        month,
        day,
        (of_day / 60) as u32,
        (of_day % 60) as u32,
        weekday,
    )
}

/// "Sat 14 Nov 2026".
pub fn format_day(ms: i64, offset_minutes: i32) -> String {
    let (y, m, d, _, _, wd) = civil(ms, offset_minutes);
    format!("{} {d} {} {y}", WEEKDAYS[wd], MONTHS[(m - 1) as usize])
}

/// "18:05".
pub fn format_clock(ms: i64, offset_minutes: i32) -> String {
    let (_, _, _, h, min, _) = civil(ms, offset_minutes);
    format!("{h:02}:{min:02}")
}

/// "UTC+01:00", "UTC-05:30", "UTC".
pub fn format_offset(offset_minutes: i32) -> String {
    if offset_minutes == 0 {
        return "UTC".into();
    }
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", abs / 60, abs % 60)
}

/// "40 sec", "45 min", "2h 7m" (as the game page shows session lengths).
pub fn format_duration(seconds: i64) -> String {
    let total = seconds.max(0);
    if total < 60 {
        format!("{total} sec")
    } else if total < 3600 {
        format!("{} min", total / 60)
    } else {
        format!("{}h {}m", total / 3600, (total % 3600) / 60)
    }
}

fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\`*_{}[]()<>#+-.!|~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The share summary as plain text or Markdown. Pure: everything comes from `summary`.
pub fn format_summary(summary: &Summary, format: Format) -> String {
    let off = summary.utc_offset_minutes;
    let md = format == Format::Markdown;
    let esc = |s: &str| if md { md_escape(s) } else { s.to_string() };
    let start_day = format_day(summary.started_at, off);
    let time = match summary.ended_at {
        Some(end) if format_day(end, off) != start_day => format!(
            "{} – {} {} ({})",
            format_clock(summary.started_at, off),
            format_day(end, off),
            format_clock(end, off),
            format_offset(off)
        ),
        Some(end) => format!(
            "{} – {} ({})",
            format_clock(summary.started_at, off),
            format_clock(end, off),
            format_offset(off)
        ),
        None => format!(
            "{} – still playing ({})",
            format_clock(summary.started_at, off),
            format_offset(off)
        ),
    };
    let mut fields: Vec<(&str, String)> = vec![
        ("Date", start_day.clone()),
        ("Time", time),
        ("Duration", format_duration(summary.seconds)),
    ];
    if !summary.platforms.is_empty() {
        fields.push(("Platform", summary.platforms.join(", ")));
    }
    for (emulator, system) in &summary.emulators {
        fields.push(("Emulator", format!("{emulator} ({system})")));
    }
    fields.push((
        "Started",
        if summary.source == "launch" {
            "Launched from Game Lobby".into()
        } else {
            "Detected running".into()
        },
    ));
    if summary.interrupted {
        fields.push((
            "Note on timing",
            "Game Lobby closed unexpectedly; the end is when the game was last seen running."
                .into(),
        ));
    }
    if summary.edited {
        fields.push(("Times", "corrected by hand".into()));
    }
    let heading = format!("{} — play session", summary.title.trim());
    let mut out = String::new();
    if md {
        out.push_str(&format!("# {}\n\n", esc(&heading)));
        for (label, value) in &fields {
            out.push_str(&format!("- **{label}:** {}\n", esc(value)));
        }
    } else {
        out.push_str(&heading);
        out.push('\n');
        for (label, value) in &fields {
            out.push_str(&format!("{label}: {value}\n"));
        }
    }
    if let Some(note) = summary.note.as_deref().filter(|n| !n.trim().is_empty()) {
        if md {
            out.push_str("\n## Note\n\n");
            for line in note.lines() {
                out.push_str(&format!("> {}\n", esc(line)).replace("> \n", ">\n"));
            }
        } else {
            out.push_str("\nNote:\n");
            out.push_str(note);
            out.push('\n');
        }
    }
    out
}

fn summary_for(db: &Connection, id: i64, utc_offset_minutes: i32) -> Result<Summary> {
    if !(-14 * 60..=14 * 60).contains(&utc_offset_minutes) {
        bail!("The time zone offset is invalid.");
    }
    let entry = entry(db, id)?.ok_or_else(|| anyhow!("This session no longer exists."))?;
    let game: Option<Game> = db::get_game(db, &entry.game_id)?;
    let mut platforms: Vec<String> = Vec::new();
    let mut emulators = Vec::new();
    if let Some(game) = &game {
        let profiles = db::emulator_profiles(db)?;
        for route in &game.routes {
            if route.source_id == "rom" {
                let system = route
                    .system
                    .as_deref()
                    .map(|s| catalog::system(s).map_or(s.to_string(), |sys| sys.name.into()))
                    .unwrap_or_else(|| "unknown system".into());
                let emulator = route
                    .emulator_id
                    .as_deref()
                    .and_then(|id| profiles.iter().find(|p| p.id == id))
                    .map_or("default emulator".to_string(), |p| p.name.clone());
                let pair = (emulator, system);
                if !emulators.contains(&pair) {
                    emulators.push(pair);
                }
            } else if !route.store.trim().is_empty() && !platforms.contains(&route.store) {
                platforms.push(route.store.clone());
            }
        }
        if platforms.is_empty() && emulators.is_empty() && !game.store.trim().is_empty() {
            platforms.push(game.store.clone());
        }
    }
    Ok(Summary {
        title: entry.game_title,
        started_at: entry.started_at,
        ended_at: entry.ended_at,
        seconds: entry.seconds,
        source: entry.source,
        platforms,
        emulators,
        edited: entry.edited,
        interrupted: entry.interrupted,
        note: entry.note,
        utc_offset_minutes,
    })
}

/// The share summary for a session.
pub fn share_text(
    db: &Connection,
    id: i64,
    utc_offset_minutes: i32,
    format: Format,
) -> Result<String> {
    Ok(format_summary(
        &summary_for(db, id, utc_offset_minutes)?,
        format,
    ))
}

/// A safe default file name: "<title> <yyyy-mm-dd>.<ext>".
pub fn export_file_name(title: &str, started_at: i64, offset: i32, format: Format) -> String {
    let (y, m, d, _, _, _) = civil(started_at, offset);
    let clean: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_'&()".contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = clean.chars().take(80).collect();
    let base = if clean.is_empty() {
        "Play session".into()
    } else {
        clean
    };
    format!("{base} {y:04}-{m:02}-{d:02}.{}", format.extension())
}

/// The path to write: the chosen path, with the summary's extension if it has another.
pub fn with_extension(path: std::path::PathBuf, format: Format) -> std::path::PathBuf {
    let ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(format.extension()));
    if ok {
        path
    } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(format.extension());
        path.with_file_name(name)
    }
}

// ---------------------------------------------------------------------------
// Backups

fn is_false(value: &bool) -> bool {
    !*value
}

/// One finished session in a library backup (`playSessions`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionBackup {
    pub game_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub seconds: i64,
    pub source: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Linked screenshots by id; restored only where the screenshot exists for the game.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshot_ids: Vec<String>,
}

/// Every finished session, oldest first. Sessions in progress are left out (the next backup
/// has them).
pub fn export_all(db: &Connection) -> Result<Vec<SessionBackup>> {
    let mut stmt = db.prepare(
        "SELECT s.id,s.gameId,s.startedAt,s.endedAt,s.seconds,s.source,s.edited,s.interrupted,n.note FROM play_sessions s LEFT JOIN session_notes n ON n.sessionId=s.id WHERE s.endedAt IS NOT NULL ORDER BY s.startedAt,s.id",
    )?;
    let mut shots = db.prepare(
        "SELECT screenshotId FROM session_screenshots WHERE sessionId=? ORDER BY screenshotId",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                SessionBackup {
                    game_id: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    seconds: row.get::<_, i64>(4)?.max(0),
                    source: row.get(5)?,
                    edited: row.get::<_, i64>(6)? != 0,
                    interrupted: row.get::<_, i64>(7)? != 0,
                    note: row.get(8)?,
                    screenshot_ids: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, mut row) in rows {
        row.screenshot_ids = shots
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        out.push(row);
    }
    Ok(out)
}

/// Checks the `playSessions` list of a backup file.
pub fn validate_backup(value: &Value, now: i64) -> Result<Vec<SessionBackup>> {
    let Value::Array(list) = value else {
        bail!("Backup play sessions are invalid.");
    };
    if list.len() > MAX_BACKUP_SESSIONS {
        bail!("Backup has too many play sessions.");
    }
    list.iter()
        .enumerate()
        .map(|(i, raw)| {
            let what = format!("Backup play session {}", i + 1);
            let mut s: SessionBackup = serde_json::from_value(raw.clone())
                .map_err(|e| anyhow!("{what} is invalid: {e}"))?;
            if s.game_id.trim().is_empty() || s.game_id.chars().count() > 512 {
                bail!("{what} has an invalid game id.");
            }
            if !matches!(s.source.as_str(), "launch" | "detected") {
                bail!("{what} has an unknown source.");
            }
            if s.started_at < 0 || s.ended_at <= s.started_at {
                bail!("{what} must end after it starts.");
            }
            if s.ended_at > now + BACKUP_FUTURE_SLACK_MS {
                bail!("{what} ends in the future.");
            }
            if s.ended_at - s.started_at > MAX_SESSION_MS {
                bail!("{what} is longer than 30 days.");
            }
            // Never longer than its span (a second's rounding allowed).
            if s.seconds < 0 || s.seconds > (s.ended_at - s.started_at) / 1000 + 1 {
                bail!("{what} has an invalid length.");
            }
            s.note = match s.note.as_deref() {
                Some(raw) => Some(clean_note(raw).map_err(|e| anyhow!("{what}: {e}"))?)
                    .filter(|n| !n.is_empty()),
                None => None,
            };
            if s.screenshot_ids.len() > MAX_SESSION_SCREENSHOTS
                || s.screenshot_ids
                    .iter()
                    .any(|id| id.is_empty() || id.len() > 128)
            {
                bail!("{what} has invalid screenshots.");
            }
            Ok(s)
        })
        .collect()
}

/// Restores backup sessions for games in the library. A session already stored (same game
/// and start) or overlapping one of the game's sessions is skipped, so importing the same
/// backup twice changes nothing. Playtime totals are not changed: they come with the backup's
/// games. Returns how many sessions were added.
pub fn restore(db: &Connection, sessions: &[SessionBackup], now: i64) -> Result<usize> {
    let mut added = 0;
    for s in sessions {
        let game_exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM games WHERE id=?)",
            [&s.game_id],
            |row| row.get(0),
        )?;
        if !game_exists || overlaps(db, &s.game_id, None, s.started_at, s.ended_at, now)? {
            continue;
        }
        db.execute(
            "INSERT INTO play_sessions(gameId,startedAt,endedAt,seconds,source,edited,interrupted) VALUES(?,?,?,?,?,?,?)",
            params![
                s.game_id,
                s.started_at,
                s.ended_at,
                s.seconds,
                s.source,
                s.edited as i64,
                s.interrupted as i64
            ],
        )?;
        let id = db.last_insert_rowid();
        if let Some(note) = &s.note {
            db.execute(
                "INSERT INTO session_notes(sessionId,note,updatedAt) VALUES(?,?,?)",
                params![id, note, now],
            )?;
        }
        for shot in &s.screenshot_ids {
            db.execute(
                "INSERT OR IGNORE INTO session_screenshots(sessionId,screenshotId) SELECT ?1,id FROM game_screenshots WHERE id=?2 AND gameId=?3",
                params![id, shot, s.game_id],
            )?;
        }
        added += 1;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// Commands

type CmdResult<T> = Result<T, String>;

fn err(e: impl Into<anyhow::Error>) -> String {
    format!("{:#}", e.into())
}

/// Sessions newest first: one game's (`game_id`) or all games'. `limit` defaults to 50.
#[tauri::command]
pub async fn sessions_page(
    state: State<'_, AppState>,
    game_id: Option<String>,
    cursor: Option<Cursor>,
    limit: Option<i64>,
) -> CmdResult<SessionPage> {
    page(
        &state.db.lock(),
        game_id.as_deref(),
        cursor,
        limit.unwrap_or(50),
    )
    .map_err(err)
}

#[tauri::command]
pub async fn sessions_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    started_at: i64,
    ended_at: i64,
) -> CmdResult<SessionEntry> {
    let entry = update(&state.db.lock(), id, started_at, ended_at, now_ms()).map_err(err)?;
    let _ = app.emit("library:changed", ());
    Ok(entry)
}

#[tauri::command]
pub async fn sessions_delete(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    delete(&state.db.lock(), id).map_err(err)?;
    let _ = app.emit("library:changed", ());
    Ok(())
}

#[tauri::command]
pub async fn sessions_set_journal(
    state: State<'_, AppState>,
    id: i64,
    note: String,
    screenshot_ids: Vec<String>,
) -> CmdResult<SessionEntry> {
    if note.len() > MAX_NOTE_CHARS * 4 + 64 || screenshot_ids.len() > 1000 {
        return Err("The session note is too long.".into());
    }
    set_journal(&state.db.lock(), id, &note, &screenshot_ids, now_ms()).map_err(err)
}

/// The share summary (`format`: `"txt"` or `"md"`), for the renderer to copy.
#[tauri::command]
pub async fn sessions_share_text(
    state: State<'_, AppState>,
    id: i64,
    utc_offset_minutes: i32,
    format: String,
) -> CmdResult<String> {
    let format = Format::parse(&format).map_err(err)?;
    share_text(&state.db.lock(), id, utc_offset_minutes, format).map_err(err)
}

/// Saves the share summary to a file chosen in the native save dialog. The file always gets
/// the summary's extension. Returns the path, or `None` when cancelled.
#[tauri::command]
pub async fn sessions_export(
    window: WebviewWindow,
    state: State<'_, AppState>,
    id: i64,
    utc_offset_minutes: i32,
    format: String,
) -> CmdResult<Option<String>> {
    let format = Format::parse(&format).map_err(err)?;
    let (text, name) = {
        let conn = state.db.lock();
        let summary = summary_for(&conn, id, utc_offset_minutes).map_err(err)?;
        let name = export_file_name(
            &summary.title,
            summary.started_at,
            utc_offset_minutes,
            format,
        );
        (format_summary(&summary, format), name)
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (label, ext) = match format {
        Format::Text => ("Text", "txt"),
        Format::Markdown => ("Markdown", "md"),
    };
    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_file_name(name)
        .add_filter(label, &[ext])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let path = with_extension(path, format);
    std::fs::write(&path, text).map_err(err)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    const HOUR: i64 = 3_600_000;
    /// 2026-11-14 12:00 UTC (a Saturday).
    const T0: i64 = 1_794_657_600_000;

    fn db_with_game() -> Db {
        let db = Db::memory().unwrap();
        db.lock()
            .execute(
                "INSERT INTO games(id,title,store,playtimeSeconds,playtimeMinutes) VALUES('a','Hollow Knight','Steam',0,0)",
                [],
            )
            .unwrap();
        db
    }

    fn playtime(conn: &Connection) -> (i64, i64) {
        conn.query_row(
            "SELECT playtimeSeconds,playtimeMinutes FROM games WHERE id='a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    /// A finished tracked session, with its playtime credited as the tracker does.
    fn tracked(conn: &Connection, start: i64, seconds: i64) -> i64 {
        let id = start_session(conn, "a", start, seconds, "launch")
            .unwrap()
            .unwrap();
        db::add_tracked_playtime(conn, "a", seconds, start).unwrap();
        db::close_session(conn, id, start + seconds * 1000, seconds).unwrap();
        id
    }

    #[test]
    fn start_end_and_duration_are_stored() {
        let db = db_with_game();
        let conn = db.lock();
        let id = start_session(&conn, "a", T0, 15, "detected")
            .unwrap()
            .unwrap();
        let e = entry(&conn, id).unwrap().unwrap();
        assert_eq!((e.started_at, e.ended_at, e.seconds), (T0, None, 15));
        db::close_session(&conn, id, T0 + 3_600_000, 3600).unwrap();
        let e = entry(&conn, id).unwrap().unwrap();
        assert_eq!(
            (e.ended_at, e.seconds, e.source.as_str()),
            (Some(T0 + HOUR), 3600, "detected")
        );
        assert_eq!(e.game_title, "Hollow Knight");
        assert!(!e.edited && !e.interrupted);
    }

    #[test]
    fn repeated_start_events_reuse_the_session() {
        let db = db_with_game();
        let conn = db.lock();
        let a = start_session(&conn, "a", T0, 10, "launch").unwrap();
        let b = start_session(&conn, "a", T0, 20, "launch").unwrap();
        assert_eq!(a, b);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM play_sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(entry(&conn, a.unwrap()).unwrap().unwrap().seconds, 20);
        assert_eq!(
            start_session(&conn, "ghost", T0, 10, "launch").unwrap(),
            None
        );
    }

    #[test]
    fn open_sessions_are_recovered_as_interrupted_at_the_last_saved_tick() {
        let db = db_with_game();
        let conn = db.lock();
        let id = start_session(&conn, "a", T0, 125, "launch")
            .unwrap()
            .unwrap();
        let done = tracked(&conn, T0 - 5 * HOUR, 600);
        // The app is restarted after a crash: the library is opened (and migrated) again.
        migrate(&conn).unwrap();
        let e = entry(&conn, id).unwrap().unwrap();
        assert_eq!(
            (e.ended_at, e.seconds, e.interrupted),
            (Some(T0 + 125_000), 125, true)
        );
        assert!(!entry(&conn, done).unwrap().unwrap().interrupted);
        // Running migrate again changes nothing.
        assert_eq!(recover_interrupted(&conn).unwrap(), 0);
    }

    #[test]
    fn migration_adds_columns_to_older_libraries() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE games(id TEXT PRIMARY KEY,title TEXT);
             CREATE TABLE game_screenshots(id TEXT PRIMARY KEY,gameId TEXT,file TEXT,addedAt INTEGER);
             CREATE TABLE play_sessions(id INTEGER PRIMARY KEY,gameId TEXT NOT NULL,startedAt INTEGER NOT NULL,endedAt INTEGER,seconds INTEGER NOT NULL DEFAULT 0,source TEXT NOT NULL);
             INSERT INTO play_sessions(gameId,startedAt,endedAt,seconds,source) VALUES('a',1000,61000,60,'launch');",
        )
        .unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let (edited, interrupted): (i64, i64) = conn
            .query_row("SELECT edited,interrupted FROM play_sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((edited, interrupted), (0, 0));
    }

    #[test]
    fn edits_are_validated() {
        let db = db_with_game();
        let conn = db.lock();
        let now = T0 + 10 * HOUR;
        let first = tracked(&conn, T0, 3600);
        let second = tracked(&conn, T0 + 2 * HOUR, 1800);
        let open = start_session(&conn, "a", T0 + 9 * HOUR, 60, "launch")
            .unwrap()
            .unwrap();
        let msg = |r: Result<SessionEntry>| format!("{:#}", r.unwrap_err());
        assert!(msg(update(&conn, first, T0, T0, now)).contains("after the start"));
        assert!(msg(update(&conn, first, T0, T0 - 1000, now)).contains("after the start"));
        assert!(msg(update(&conn, first, T0, now + 1000, now)).contains("future"));
        assert!(msg(update(&conn, first, T0, T0 + 2 * HOUR + 1, now)).contains("overlap"));
        // Overlapping the running session (counts until now) is refused too.
        assert!(msg(update(&conn, second, T0 + 2 * HOUR, now, now)).contains("overlap"));
        assert!(msg(update(&conn, open, T0 + 9 * HOUR, now, now)).contains("still running"));
        assert!(msg(update(&conn, 999, T0, T0 + 1000, now)).contains("no longer exists"));
        assert!(msg(update(&conn, first, 1000, 5000, now)).contains("2000"));
        // Touching the next session's start is fine (half-open ranges).
        let e = update(&conn, first, T0 + HOUR, T0 + 2 * HOUR, now).unwrap();
        assert_eq!((e.seconds, e.edited), (3600, true));
    }

    #[test]
    fn edits_and_deletes_keep_playtime_consistent() {
        let db = db_with_game();
        let conn = db.lock();
        // Imported store playtime that no session accounts for.
        conn.execute(
            "UPDATE games SET playtimeSeconds=600,playtimeMinutes=10",
            [],
        )
        .unwrap();
        let first = tracked(&conn, T0, 3600);
        let second = tracked(&conn, T0 + 2 * HOUR, 1800);
        assert_eq!(playtime(&conn), (6000, 100));
        let now = T0 + 10 * HOUR;
        update(&conn, first, T0, T0 + HOUR / 2, now).unwrap();
        assert_eq!(playtime(&conn), (4200, 70));
        update(&conn, first, T0, T0 + HOUR + HOUR / 2, now).unwrap();
        assert_eq!(playtime(&conn), (7800, 130));
        assert_eq!(delete(&conn, second).unwrap(), "a");
        assert_eq!(playtime(&conn), (6000, 100));
        assert!(entry(&conn, second).unwrap().is_none());
        // Never below zero.
        conn.execute("UPDATE games SET playtimeSeconds=100,playtimeMinutes=1", [])
            .unwrap();
        delete(&conn, first).unwrap();
        assert_eq!(playtime(&conn), (0, 0));
        // A running session can't be deleted.
        let open = start_session(&conn, "a", now, 60, "launch")
            .unwrap()
            .unwrap();
        assert!(format!("{:#}", delete(&conn, open).unwrap_err()).contains("still running"));
    }

    fn add_shot(conn: &Connection, id: &str, game: &str) {
        conn.execute(
            "INSERT INTO game_screenshots(id,gameId,file,hash,width,height,addedAt) VALUES(?1,?2,?3,?1,1,1,1)",
            params![id, game, format!("{}/{}.png", "0123456789abcdef", "0".repeat(32))],
        )
        .unwrap();
    }

    #[test]
    fn journal_notes_and_screenshots() {
        let db = db_with_game();
        let conn = db.lock();
        conn.execute(
            "INSERT INTO games(id,title,store) VALUES('b','Other','Steam')",
            [],
        )
        .unwrap();
        add_shot(&conn, "s1", "a");
        add_shot(&conn, "s2", "b");
        let id = tracked(&conn, T0, 600);
        let e = set_journal(
            &conn,
            id,
            "  Beat the boss\r\nfinally\u{7}  ",
            &["s1".into(), "s1".into()],
            T0,
        )
        .unwrap();
        assert_eq!(e.note.as_deref(), Some("Beat the boss\nfinally"));
        assert_eq!(e.screenshots.len(), 1);
        assert!(set_journal(&conn, id, "x", &["s2".into()], T0).is_err());
        assert!(set_journal(&conn, id, &"x".repeat(MAX_NOTE_CHARS + 1), &[], T0).is_err());
        assert!(set_journal(&conn, id, &"é".repeat(MAX_NOTE_CHARS), &[], T0).is_ok());
        let e = set_journal(&conn, id, "   ", &[], T0).unwrap();
        assert_eq!((e.note, e.screenshots.len()), (None, 0));
        // Notes and links go with the session.
        set_journal(&conn, id, "keep", &["s1".into()], T0).unwrap();
        delete(&conn, id).unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM session_notes)+(SELECT COUNT(*) FROM session_screenshots)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn pages_follow_the_cursor() {
        let db = db_with_game();
        let conn = db.lock();
        conn.execute(
            "INSERT INTO games(id,title,store) VALUES('b','Other','Steam')",
            [],
        )
        .unwrap();
        for i in 0..5 {
            tracked(&conn, T0 + i * HOUR, 60);
        }
        conn.execute(
            "INSERT INTO play_sessions(gameId,startedAt,endedAt,seconds,source) VALUES('b',?1,?2,60,'launch')",
            params![T0 + 30 * 60_000, T0 + 31 * 60_000],
        )
        .unwrap();
        let p1 = page(&conn, Some("a"), None, 2).unwrap();
        assert_eq!(p1.sessions.len(), 2);
        assert_eq!(p1.sessions[0].started_at, T0 + 4 * HOUR);
        let p2 = page(&conn, Some("a"), p1.next, 2).unwrap();
        let p3 = page(&conn, Some("a"), p2.next, 2).unwrap();
        assert_eq!((p3.sessions.len(), p3.next), (1, None));
        assert_eq!(p3.sessions[0].started_at, T0);
        let all = page(&conn, None, None, 100).unwrap();
        assert_eq!(all.sessions.len(), 6);
        assert_eq!(all.sessions[4].game_title, "Other");
    }

    #[test]
    fn backups_round_trip_and_are_validated() {
        let db = db_with_game();
        let id;
        let exported = {
            let conn = db.lock();
            add_shot(&conn, "s1", "a");
            id = tracked(&conn, T0, 600);
            tracked(&conn, T0 + HOUR, 300);
            update(&conn, id, T0, T0 + 700_000, T0 + 2 * HOUR).unwrap();
            set_journal(&conn, id, "A note", &["s1".into()], T0).unwrap();
            start_session(&conn, "a", T0 + 3 * HOUR, 30, "launch").unwrap();
            export_all(&conn).unwrap()
        };
        assert_eq!(exported.len(), 2, "running sessions are left out");
        let json = serde_json::to_value(&exported).unwrap();
        let parsed = validate_backup(&json, T0 + 4 * HOUR).unwrap();
        assert_eq!(parsed, exported);

        let target = db_with_game();
        let conn = target.lock();
        add_shot(&conn, "s1", "a");
        assert_eq!(restore(&conn, &parsed, T0 + 4 * HOUR).unwrap(), 2);
        // Importing again adds nothing.
        assert_eq!(restore(&conn, &parsed, T0 + 4 * HOUR).unwrap(), 0);
        let all = page(&conn, Some("a"), None, 10).unwrap().sessions;
        let restored = all.iter().find(|s| s.started_at == T0).unwrap();
        assert_eq!(restored.seconds, 700);
        assert!(restored.edited);
        assert_eq!(restored.note.as_deref(), Some("A note"));
        assert_eq!(restored.screenshots.len(), 1);
        // Playtime comes with the backup's games, not from restored sessions.
        assert_eq!(playtime(&conn), (0, 0));
        // Sessions of games not in the library are skipped.
        let mut ghost = parsed[0].clone();
        ghost.game_id = "ghost".into();
        assert_eq!(restore(&conn, &[ghost], T0).unwrap(), 0);

        let bad = |patch: serde_json::Value| {
            let mut v = serde_json::to_value(&exported[1]).unwrap();
            for (k, val) in patch.as_object().unwrap() {
                v[k] = val.clone();
            }
            validate_backup(&serde_json::json!([v]), T0 + 4 * HOUR).is_err()
        };
        assert!(bad(serde_json::json!({ "endedAt": T0 })));
        assert!(bad(serde_json::json!({ "endedAt": T0 + 400 * HOUR })));
        assert!(bad(serde_json::json!({ "seconds": 99_999 })));
        assert!(bad(serde_json::json!({ "source": "manual" })));
        assert!(bad(serde_json::json!({ "extra": 1 })));
        assert!(bad(
            serde_json::json!({ "note": "x".repeat(MAX_NOTE_CHARS + 1) })
        ));
        assert!(bad(serde_json::json!({ "gameId": "" })));
        assert!(validate_backup(&serde_json::json!({}), T0).is_err());
    }

    #[test]
    fn calendar_and_durations() {
        assert_eq!(format_day(0, 0), "Thu 1 Jan 1970");
        assert_eq!(format_day(T0, 0), "Sat 14 Nov 2026");
        assert_eq!(format_clock(T0, 0), "12:00");
        assert_eq!(format_clock(T0, 90), "13:30");
        assert_eq!(format_day(T0 + 12 * HOUR, 60), "Sun 15 Nov 2026");
        // 2024-02-29 (leap day) 00:00 UTC.
        assert_eq!(format_day(1_709_164_800_000, 0), "Thu 29 Feb 2024");
        assert_eq!(format_offset(0), "UTC");
        assert_eq!(format_offset(60), "UTC+01:00");
        assert_eq!(format_offset(-330), "UTC-05:30");
        assert_eq!(format_duration(40), "40 sec");
        assert_eq!(format_duration(2700), "45 min");
        assert_eq!(format_duration(7620), "2h 7m");
    }

    #[test]
    fn share_summaries() {
        let summary = Summary {
            title: "Hollow Knight".into(),
            started_at: T0,
            ended_at: Some(T0 + 7_620_000),
            seconds: 7620,
            source: "launch".into(),
            platforms: vec!["Steam".into()],
            note: Some("Beat *Hornet*\nfinally".into()),
            utc_offset_minutes: 60,
            ..Default::default()
        };
        assert_eq!(
            format_summary(&summary, Format::Text),
            "Hollow Knight — play session\nDate: Sat 14 Nov 2026\nTime: 13:00 – 15:07 (UTC+01:00)\nDuration: 2h 7m\nPlatform: Steam\nStarted: Launched from Game Lobby\n\nNote:\nBeat *Hornet*\nfinally\n"
        );
        let md = format_summary(&summary, Format::Markdown);
        assert!(md.starts_with("# Hollow Knight — play session\n\n- **Date:** Sat 14 Nov 2026\n"));
        assert!(md.contains("\n## Note\n\n> Beat \\*Hornet\\*\n> finally\n"));
        let across = Summary {
            ended_at: Some(T0 + 12 * HOUR),
            emulators: vec![("Dolphin".into(), "GameCube".into())],
            platforms: vec![],
            interrupted: true,
            edited: true,
            source: "detected".into(),
            note: None,
            ..summary.clone()
        };
        let text = format_summary(&across, Format::Text);
        assert!(text.contains("Time: 13:00 – Sun 15 Nov 2026 01:00 (UTC+01:00)\n"));
        assert!(text.contains("Emulator: Dolphin (GameCube)\n"));
        assert!(text.contains("Started: Detected running\n"));
        assert!(text.contains("closed unexpectedly"));
        assert!(text.contains("Times: corrected by hand\n"));
        assert!(!text.contains("Platform"));
        assert!(!text.contains("Note:"));
    }

    #[test]
    fn share_summary_reads_the_library() {
        let db = db_with_game();
        let conn = db.lock();
        let id = tracked(&conn, T0, 600);
        let text = share_text(&conn, id, 0, Format::Text).unwrap();
        assert!(text.starts_with("Hollow Knight — play session\n"));
        assert!(text.contains("Platform: Steam\n"));
        assert!(share_text(&conn, id, 15 * 60, Format::Text).is_err());
        assert!(share_text(&conn, 999, 0, Format::Text).is_err());
    }

    #[test]
    fn export_names_and_extensions() {
        assert_eq!(
            export_file_name("Half-Life 2: Episode <One>", T0, 0, Format::Text),
            "Half-Life 2 Episode One 2026-11-14.txt"
        );
        assert_eq!(
            export_file_name("../..", T0, 0, Format::Markdown),
            "Play session 2026-11-14.md"
        );
        let p = |s: &str| std::path::PathBuf::from(s);
        assert_eq!(
            with_extension(p("C:/x/notes.txt"), Format::Text),
            p("C:/x/notes.txt")
        );
        assert_eq!(
            with_extension(p("C:/x/notes.TXT"), Format::Text),
            p("C:/x/notes.TXT")
        );
        assert_eq!(
            with_extension(p("C:/x/run.bat"), Format::Text),
            p("C:/x/run.bat.txt")
        );
        assert_eq!(
            with_extension(p("C:/x/notes"), Format::Markdown),
            p("C:/x/notes.md")
        );
        assert!(Format::parse("exe").is_err());
    }

    #[test]
    fn repeated_tracker_saves_store_one_session() {
        use crate::tracker::{apply, Effect, Sessions, Source};
        let db = db_with_game();
        let conn = db.lock();
        let save = |seconds| Effect::Save {
            game_id: "a".into(),
            session_id: None,
            started_at: T0,
            seconds,
            credit: 10,
            source: Source::Launch,
        };
        let mut sessions = Sessions::default();
        apply(&conn, &mut sessions, vec![save(10), save(20)]);
        let rows = db::play_sessions(&conn, "a", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seconds, 20);
        assert_eq!(playtime(&conn).0, 20);
    }

    #[test]
    fn library_backups_carry_sessions() {
        let db = db_with_game();
        let backup = {
            let conn = db.lock();
            let id = tracked(&conn, T0, 600);
            set_journal(&conn, id, "Note", &[], T0).unwrap();
            crate::types::Backup {
                games: db::all_games(&conn).unwrap(),
                settings: None,
                emulators: None,
                game_page_custom: None,
                game_notes: None,
                game_added: None,
                play_sessions: Some(export_all(&conn).unwrap()),
                game_links: None,
                launchers: None,
            }
        };
        let json = serde_json::to_value(&backup).unwrap();
        assert_eq!(json["playSessions"][0]["note"], "Note");
        let parsed = crate::validation::backup(&json).unwrap();
        let target = Db::memory().unwrap();
        let mut conn = target.lock();
        db::restore_backup(&mut conn, &parsed).unwrap();
        let rows = page(&conn, Some("a"), None, 10).unwrap().sessions;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].seconds, rows[0].note.as_deref()),
            (600, Some("Note"))
        );
        assert_eq!(playtime(&conn), (600, 10));
        // Older backups without sessions still import.
        let mut old = json.clone();
        old.as_object_mut().unwrap().remove("playSessions");
        assert!(crate::validation::backup(&old)
            .unwrap()
            .play_sessions
            .is_none());
    }
}
