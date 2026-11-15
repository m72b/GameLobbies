//! Scan review items (`scan_review`): things a scan found but would not decide on its own.
//!
//! Kinds: `duplicate` (same title and system in different files), `ambiguous-exe` (a PC game
//! folder without one clear executable), `unsupported` (a folder or game folder that couldn't
//! be used), `unreadable` (a folder that couldn't be read) and `shortcut` (a shortcut that
//! must be confirmed before it is added). Each item has a stable `key`
//! built from what it is about, so rescans update the same item instead of adding another,
//! and a resolved item stays resolved. Open items a successful rescan no longer produces are
//! removed.

use crate::types::now_ms;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

pub const DUPLICATE: &str = "duplicate";
pub const AMBIGUOUS_EXE: &str = "ambiguous-exe";
pub const UNSUPPORTED: &str = "unsupported";
pub const UNREADABLE: &str = "unreadable";
/// A `.lnk` with arguments (or a target that isn't a plain `.exe`): shown with its target
/// and arguments, added only once the user confirms exactly those.
pub const SHORTCUT: &str = "shortcut";
/// A ROM in a launcher's games folder whose extension several systems share, and no single
/// emulator runs: the user picks the emulator from the profiles listed.
pub const UNCERTAIN_ROM: &str = "uncertain-rom";

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    pub kind: String,
    /// `"open"` or `"resolved"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    pub details: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

fn read(row: &rusqlite::Row) -> rusqlite::Result<ReviewItem> {
    Ok(ReviewItem {
        id: row.get("id")?,
        kind: row.get("kind")?,
        status: row.get("status")?,
        resolution: row.get("resolution")?,
        root_id: row.get("rootId")?,
        details: row
            .get::<_, String>("details")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
        created_at: row.get("createdAt")?,
        updated_at: row.get("updatedAt")?,
    })
}

/// Adds an open item, or refreshes an open one's details. Resolved items are left alone.
pub fn upsert(
    conn: &Connection,
    kind: &str,
    key: &str,
    root_id: Option<&str>,
    details: &Value,
) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO scan_review(id,kind,key,rootId,details,status,createdAt,updatedAt) VALUES(?,?,?,?,?,'open',?,?)
         ON CONFLICT(key) DO UPDATE SET details=excluded.details,rootId=excluded.rootId,updatedAt=excluded.updatedAt WHERE scan_review.status='open'",
        params![
            format!("review:{}", uuid::Uuid::new_v4()),
            kind,
            key,
            root_id,
            serde_json::to_string(details)?,
            now,
            now
        ],
    )?;
    Ok(())
}

/// Removes open items of `kinds` (for `root_id`, or root-less items with `None`) whose key
/// isn't in `keep`.
pub fn prune(
    conn: &Connection,
    root_id: Option<&str>,
    kinds: &[&str],
    keep: &HashSet<String>,
) -> Result<()> {
    let mut stmt =
        conn.prepare("SELECT id,key,kind FROM scan_review WHERE status='open' AND rootId IS ?")?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([root_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, key, kind) in rows {
        if kinds.contains(&kind.as_str()) && !keep.contains(&key) {
            conn.execute("DELETE FROM scan_review WHERE id=?", [id])?;
        }
    }
    Ok(())
}

/// Removes every open item of a root (the root was removed).
pub fn forget_root(conn: &Connection, root_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM scan_review WHERE rootId=? AND status='open'",
        [root_id],
    )?;
    Ok(())
}

/// Items, open first then newest; resolved ones only with `include_resolved`.
pub fn list(conn: &Connection, include_resolved: bool) -> Result<Vec<ReviewItem>> {
    let sql = if include_resolved {
        "SELECT * FROM scan_review ORDER BY status='open' DESC,updatedAt DESC,id LIMIT 5000"
    } else {
        "SELECT * FROM scan_review WHERE status='open' ORDER BY updatedAt DESC,id LIMIT 5000"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], read)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<ReviewItem>> {
    Ok(conn
        .query_row("SELECT * FROM scan_review WHERE id=?", [id], read)
        .optional()?)
}

pub fn resolve(conn: &Connection, id: &str, resolution: &str) -> Result<()> {
    conn.execute(
        "UPDATE scan_review SET status='resolved',resolution=?,updatedAt=? WHERE id=?",
        params![resolution, now_ms(), id],
    )?;
    Ok(())
}

/// Open items of one root.
pub fn open_count_for_root(conn: &Connection, root_id: &str) -> Result<usize> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM scan_review WHERE status='open' AND rootId=?",
        [root_id],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

pub fn open_count(conn: &Connection) -> Result<usize> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM scan_review WHERE status='open'",
        [],
        |r| r.get::<_, i64>(0),
    )? as usize)
}
