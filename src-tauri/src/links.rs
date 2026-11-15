//! Linked games: two or more library entries combined into one game whose launch routes are
//! the entries' versions ("Xbox · Java Edition", "Xbox · Bedrock Edition"), named routes, route
//! launch options, and splitting a linked entry off again.
//!
//! - **Data model.** A link moves the other entries' routes into the kept game (`games.routes`,
//!   each route keeping its own store, target, install state, playtime and name) and records one
//!   [`LinkRecord`] per entry linked in (`game_links`: the entry's original id, the game it went
//!   into, when, and a [`LinkSnapshot`] of what it brought: its title, cover, favourite, hidden
//!   flag, playtime, route identities, session start times, screenshot and memory ids, and which
//!   of its details the combined game uses).
//! - **Nothing is deleted.** Sessions, screenshots and memories move to the kept game. For the
//!   per-game details with one value (page customization/hero, IGDB match, notes and score,
//!   controller details) the chosen entry's row is used by the combined game; the other entries'
//!   rows stay in the library under their original ids ("parked"), where unlinking finds them
//!   again. Removing the combined game removes its parked rows too (`db::delete_game_rows`).
//! - **Review first.** The renderer asks for a [`LinkPreview`] (what will be combined, which
//!   details conflict, the suggested route names) and then links with game ids and
//!   [`LinkChoices`] (game ids and a flag; the only free text is a custom title, cleaned with
//!   `validation::plain_text`). It never sends route contents: links only move stored routes.
//! - **Unlinking** ([`unlink`]) takes one route of a game (by index, checked against its stored
//!   source and target). When that route came in with a linked entry, every route that entry
//!   brought leaves together and the entry comes back under its own id with its own title,
//!   cover, favourite, hidden flag, details, sessions, screenshots and memories, and the playtime
//!   it brought (subtracted from the combined game). Sessions played after linking stay with the
//!   combined game: Game Lobby tracks games, not routes, so they can't be told apart. A route that
//!   didn't come from a link (or whose record is gone, for example after a backup restore without
//!   it) becomes a new game of its own, titled "<game> (<route name or store>)", with the store's
//!   art and none of the game's personal data. The pair is then remembered as "not duplicates"
//!   so the library check doesn't suggest linking it again.
//! - **Route names** are display text only: cleaned (`validation::route_name`), at most
//!   [`MAX_ROUTE_NAME`] characters, never part of `db::route_identity` or the renderer's launch
//!   identity check, never shown before the facts in a native confirmation.
//! - **Launch options** ([`prepare_options`] for an [`ArgsHost`]) are only accepted where Game
//!   Lobby itself starts a program without a shell: a game's own `.exe` (local PC routes), or a
//!   custom launcher's program (launcher routes of a launcher with a program), where they come
//!   after the launcher's template and the game's target. They get the checks every program Game
//!   Lobby starts with arguments gets (`launch::check_arguments`; paths only inside the game's
//!   folder, or the launcher's folder like its own template), are confirmed natively with the
//!   exact program, whole argv and working folder, and are checked again at every launch
//!   ([`checked_stored_args`], [`launcher_launch_argv`]): they only run for the command line they
//!   were confirmed for (`launch_args_confirmed`, written only after that dialog: the `.exe`, or
//!   the launcher's program and working folder, plus the whole argv), so options from a backup, a
//!   merge or an older version, or after the launcher's program, template or working folder or
//!   the route's target changed, stay inert until the user confirms them on the game page.
//!   Store links (`steam://`, `shell:AppsFolder\…`), label-only launchers and emulators start the
//!   game themselves (or have their own arguments), so their routes don't take options here.

use crate::db;
use crate::launchers::CustomLauncher;
use crate::local::{LOCAL_SOURCE, ROM_SOURCE};
use crate::types::*;
use crate::validation::{self, display_text, plain_text};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Longest route name, in characters.
pub const MAX_ROUTE_NAME: usize = 60;
/// Most entries linked in one go.
pub const MAX_LINK_GAMES: usize = 16;
/// Longest launch options text, in characters.
pub const MAX_ARGS_CHARS: usize = 1_000;
/// Most launch options (argv entries) on one route.
pub const MAX_ARGS: usize = 24;
/// Longest single launch option, in characters.
pub const MAX_ARG_CHARS: usize = 200;
/// Longest launcher command line with launch options that can be confirmed: the program, every
/// argument (template, target and options) and the working folder, as the native dialog shows
/// them (`validation::escape_for_prompt`), in characters. A message box doesn't scroll and the
/// options are its last numbered lines, so the whole command line must fit.
pub const MAX_CONFIRM_CHARS: usize = 2_000;

/// The links table and the launch options the user confirmed (idempotent added tables, no
/// `user_version` change; older builds ignore them). Called from `db::migrate`.
pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS game_links(memberId TEXT PRIMARY KEY,gameId TEXT NOT NULL,linkedAt INTEGER NOT NULL,snapshot TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS game_links_game ON game_links(gameId);
         CREATE TABLE IF NOT EXISTS launch_args_confirmed(exeKey TEXT NOT NULL,argv TEXT NOT NULL,confirmedAt INTEGER NOT NULL,PRIMARY KEY(exeKey,argv));",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Records

/// What a linked entry brought into the combined game (see the module docs).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkSnapshot {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    pub favorite: bool,
    pub hidden: bool,
    pub playtime_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    /// `db::route_identity` of the routes it brought that the game didn't have yet: unlinking
    /// moves these out of the game.
    pub route_keys: Vec<String>,
    /// `db::route_identity` of its routes the game already had (same store and target): they
    /// stay with the game, and unlinking gives the entry a copy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_keys: Vec<String>,
    /// Start times of the sessions it brought (unique per game, and kept by session backups).
    pub session_starts: Vec<i64>,
    pub screenshot_ids: Vec<String>,
    pub memory_ids: Vec<i64>,
    /// Detail parts ([`Part::key`]) for which the combined game uses this entry's row (the kept
    /// game's own row, if any, is parked under this entry's id meanwhile).
    pub took: Vec<String>,
}

/// Field names a backup's link record and snapshot may have (a backup is checked strictly,
/// [`validate_backup`]; rows already stored are read leniently, [`to_record`]). A test keeps
/// these in step with the structs.
const RECORD_FIELDS: [&str; 4] = ["memberId", "gameId", "linkedAt", "snapshot"];
const SNAPSHOT_FIELDS: [&str; 14] = [
    "title",
    "cover",
    "defaultCover",
    "genre",
    "favorite",
    "hidden",
    "playtimeSeconds",
    "lastPlayed",
    "routeKeys",
    "sharedKeys",
    "sessionStarts",
    "screenshotIds",
    "memoryIds",
    "took",
];

/// One `game_links` row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkRecord {
    pub member_id: String,
    pub game_id: String,
    pub linked_at: i64,
    pub snapshot: LinkSnapshot,
}

fn read_link(row: &rusqlite::Row) -> rusqlite::Result<(String, String, i64, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn to_record((member_id, game_id, linked_at, raw): (String, String, i64, String)) -> LinkRecord {
    LinkRecord {
        member_id,
        game_id,
        linked_at,
        // A row written by a newer version may have fields this one doesn't know; they are
        // ignored. A row that can't be read at all reads as empty (and is logged, without its
        // content).
        snapshot: serde_json::from_str(&raw).unwrap_or_else(|e| {
            log::warn!(
                "A game link record could not be read and is treated as empty ({:?} error).",
                e.classify()
            );
            LinkSnapshot::default()
        }),
    }
}

/// The entries linked into `game_id`, newest first (entries linked in one go keep the order they
/// were linked in: `rowid`).
pub fn records_of(conn: &Connection, game_id: &str) -> Result<Vec<LinkRecord>> {
    let mut stmt = conn.prepare(
        "SELECT memberId,gameId,linkedAt,snapshot FROM game_links WHERE gameId=? ORDER BY linkedAt DESC,rowid DESC",
    )?;
    let rows = stmt
        .query_map([game_id], read_link)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(to_record).collect())
}

/// The link records as a backup holds them: screenshot and memory ids are this library's own row
/// ids, which a restore doesn't keep, so they are left out (sessions are found by start time).
pub fn backup_records(conn: &Connection) -> Result<Vec<LinkRecord>> {
    Ok(all_records(conn)?
        .into_iter()
        .map(|mut r| {
            r.snapshot.screenshot_ids.clear();
            r.snapshot.memory_ids.clear();
            r
        })
        .collect())
}

/// Most link records in a backup.
pub const MAX_BACKUP_LINKS: usize = 100_000;

/// Latest time a backup's link record may hold (the largest JavaScript date, in milliseconds).
const MAX_TIME_MS: i64 = 8_640_000_000_000_000;

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= 512 && !id.chars().any(char::is_control)
}

/// Checks a backup's `gameLinks` (the file was chosen in a native dialog, but is still only
/// data): known fields only, sizes bounded, the title cleaned like every title, detail parts
/// from the known list, and no row ids (see [`backup_records`]).
pub fn validate_backup(value: &serde_json::Value) -> Result<Vec<LinkRecord>> {
    let list = value
        .as_array()
        .filter(|l| l.len() <= MAX_BACKUP_LINKS)
        .ok_or_else(|| anyhow!("Backup game links are invalid."))?;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let unknown_field = |value: &serde_json::Value, known: &[&str]| {
            value
                .as_object()
                .is_none_or(|o| o.keys().any(|k| !known.contains(&k.as_str())))
        };
        if unknown_field(entry, &RECORD_FIELDS)
            || entry
                .get("snapshot")
                .is_some_and(|s| unknown_field(s, &SNAPSHOT_FIELDS))
        {
            bail!("Backup game links are invalid.");
        }
        let mut record: LinkRecord = serde_json::from_value(entry.clone())
            .map_err(|e| anyhow!("Backup game links are invalid: {e}"))?;
        let s = &mut record.snapshot;
        s.title = plain_text(&s.title);
        let too_long =
            |v: &Option<String>, max: usize| v.as_ref().is_some_and(|v| v.chars().count() > max);
        if !id_ok(&record.member_id)
            || !id_ok(&record.game_id)
            || record.member_id == record.game_id
            || !seen.insert(record.member_id.clone())
            || s.title.chars().count() > 300
            || too_long(&s.cover, 8_192)
            || too_long(&s.default_cover, 8_192)
            || too_long(&s.genre, 500)
            || !(0..=60_000_000_000).contains(&s.playtime_seconds)
            || !(0..=MAX_TIME_MS).contains(&record.linked_at)
            || s.last_played
                .is_some_and(|t| !(0..=MAX_TIME_MS).contains(&t))
            || (s.route_keys.is_empty() && s.shared_keys.is_empty())
            || s.route_keys.len() > 128
            || s.shared_keys.len() > 128
            || s.route_keys
                .iter()
                .chain(&s.shared_keys)
                .any(|k| k.is_empty() || k.chars().count() > 8_400)
            || s.session_starts.len() > crate::sessions::MAX_BACKUP_SESSIONS
            || s.took.iter().any(|k| Part::from_key(k).is_none())
        {
            bail!("Backup game links are invalid.");
        }
        s.screenshot_ids.clear();
        s.memory_ids.clear();
        out.push(record);
    }
    Ok(out)
}

/// Every link record (backups).
pub fn all_records(conn: &Connection) -> Result<Vec<LinkRecord>> {
    let mut stmt = conn.prepare(
        "SELECT memberId,gameId,linkedAt,snapshot FROM game_links ORDER BY gameId,linkedAt,rowid",
    )?;
    let rows = stmt
        .query_map([], read_link)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(to_record).collect())
}

fn put_record(conn: &Connection, record: &LinkRecord) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO game_links(memberId,gameId,linkedAt,snapshot) VALUES(?,?,?,?)",
        params![
            record.member_id,
            record.game_id,
            record.linked_at,
            serde_json::to_string(&record.snapshot)?
        ],
    )?;
    Ok(())
}

/// Restores backup link records (validated by [`validate_backup`]) inside the caller's
/// transaction, after the backup's games. A record is only data from a file, so it is skipped
/// unless it fits the library as it is now: its game exists and some but not all of that game's
/// routes are the ones it says it brought; its entry id is not a game, not a game any record
/// points at (here or in the backup), and not already linked (a live record is never replaced).
/// Returns the entry ids restored, whose parked rows the backup may then restore too.
pub fn restore_records(conn: &Connection, records: &[LinkRecord]) -> Result<HashSet<String>> {
    let backup_games: HashSet<&str> = records.iter().map(|r| r.game_id.as_str()).collect();
    // Oldest first, so entries linked in one go keep their order (`rowid`).
    let mut order: Vec<&LinkRecord> = records.iter().collect();
    order.sort_by_key(|r| r.linked_at);
    let mut restored = HashSet::new();
    for record in order {
        let member = record.member_id.as_str();
        let Some(game) = db::get_game(conn, &record.game_id)? else {
            continue;
        };
        let keys: HashSet<String> = game.routes.iter().map(db::route_identity).collect();
        let own = keys
            .iter()
            .filter(|k| record.snapshot.route_keys.contains(k))
            .count();
        let matched = own
            + keys
                .iter()
                .filter(|k| record.snapshot.shared_keys.contains(k))
                .count();
        let linked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM game_links WHERE memberId=?1 OR gameId=?1)",
            [member],
            |r| r.get(0),
        )?;
        if matched == 0
            || own == keys.len()
            || linked
            || backup_games.contains(member)
            || db::get_game(conn, member)?.is_some()
        {
            continue;
        }
        let added = conn.execute(
            "INSERT OR IGNORE INTO game_links(memberId,gameId,linkedAt,snapshot) VALUES(?,?,?,?)",
            params![
                record.member_id,
                record.game_id,
                record.linked_at,
                serde_json::to_string(&record.snapshot)?
            ],
        )?;
        if added == 1 {
            restored.insert(record.member_id.clone());
        }
    }
    Ok(restored)
}

/// The entries linked into `game_id`, oldest first (`linkedAt`, then `rowid`).
fn ordered_records(conn: &Connection, game_id: &str) -> Result<Vec<LinkRecord>> {
    let mut list = records_of(conn, game_id)?;
    list.reverse();
    Ok(list)
}

/// `inner` came in with `outer` (a chained link: `outer`'s routes, its own and the shared ones,
/// include all the routes `inner` brought). An entry that brought no routes of its own (all
/// were shared) is not tied to another entry this way.
fn within(inner: &LinkRecord, outer: &LinkRecord) -> bool {
    let outer = &outer.snapshot;
    !inner.snapshot.route_keys.is_empty()
        && inner
            .snapshot
            .route_keys
            .iter()
            .all(|k| outer.route_keys.contains(k) || outer.shared_keys.contains(k))
}

fn took(record: &LinkRecord, part: Part) -> bool {
    record.snapshot.took.iter().any(|k| k == part.key())
}

/// The entry `list[i]` was linked into, when that was an entry that was itself linked in later
/// (the oldest newer record whose routes include all of `list[i]`'s); `None`: the game itself.
fn container(list: &[LinkRecord], i: usize) -> Option<usize> {
    (i + 1..list.len()).find(|&j| within(&list[i], &list[j]))
}

/// Where the row of `part` that the entry `list[i]` brought is now (`list` oldest first). When
/// it was linked, its row became the row of what it was linked into, and that row was parked
/// under its id. Later links that took `part` into the same place parked it in turn under their
/// own id (the next one has it); otherwise it is still that place's row: the game's, or, for an
/// entry linked into an entry that was linked on later, wherever that entry's row went.
fn holder(list: &[LinkRecord], i: usize, part: Part, game_id: &str) -> String {
    let scope = container(list, i);
    if let Some(j) =
        (i + 1..list.len()).find(|&j| container(list, j) == scope && took(&list[j], part))
    {
        return list[j].member_id.clone();
    }
    match scope {
        None => game_id.to_string(),
        Some(c) if took(&list[c], part) => holder(list, c, part, game_id),
        Some(c) => list[c].member_id.clone(),
    }
}

/// Gives a linked entry its details back and forgets its record: each detail the combined game
/// took from it returns to the entry's id, and the row parked there goes back where it came
/// from ([`holder`]), whatever order entries are unlinked in. Entries that came in with it (a
/// chained link) now belong to it. Used when the entry is unlinked and when its id becomes a
/// game again (`db::persist_game`: a scan or backup re-adds it), so a new game never inherits
/// rows parked for another one.
pub(crate) fn release_member(conn: &Connection, member_id: &str) -> Result<()> {
    let Some(game_id) = conn
        .query_row(
            "SELECT gameId FROM game_links WHERE memberId=?",
            [member_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let list = ordered_records(conn, &game_id)?;
    if let Some(i) = list.iter().position(|r| r.member_id == member_id) {
        for part in Part::ALL.into_iter().filter(|p| took(&list[i], *p)) {
            let holder = holder(&list, i, part, &game_id);
            swap(conn, part, member_id, &holder)?;
        }
        move_within(conn, &list[i], member_id)?;
    }
    conn.execute("DELETE FROM game_links WHERE memberId=?", [member_id])?;
    Ok(())
}

/// The records of entries that came in with `leaving` (all their routes are among the routes it
/// brought) move to `to`, the game `leaving` becomes, so they can be unlinked from it later.
fn move_within(conn: &Connection, leaving: &LinkRecord, to: &str) -> Result<()> {
    for record in records_of(conn, &leaving.game_id)? {
        if record.member_id != leaving.member_id && within(&record, leaving) {
            conn.execute(
                "UPDATE game_links SET gameId=?1 WHERE memberId=?2",
                params![to, record.member_id],
            )?;
        }
    }
    Ok(())
}

/// Parked rows of the entries linked into `game_id` and their records (the game is deleted).
/// Called from `db::delete_game_rows`.
pub(crate) fn delete_members(conn: &Connection, game_id: &str) -> Result<()> {
    let members: Vec<String> = conn
        .prepare("SELECT memberId FROM game_links WHERE gameId=?")?
        .query_map([game_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    conn.execute("DELETE FROM game_links WHERE gameId=?", [game_id])?;
    for member in members {
        // A member id that is a game again (a scan re-added it) owns those rows now.
        if db::get_game(conn, &member)?.is_none() {
            db::delete_game_rows(conn, &member)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Detail parts (one row per game)

/// Per-game details with one value, chosen in the review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// `game_page_custom`: hero, logo and their layout.
    Hero,
    /// `game_metadata`: the IGDB match.
    Igdb,
    /// `game_notes`: personal score and notes.
    Notes,
    /// `game_input`: controller details (follows the default rule, no choice).
    Input,
}

impl Part {
    pub const ALL: [Part; 4] = [Part::Hero, Part::Igdb, Part::Notes, Part::Input];

    fn table(self) -> &'static str {
        match self {
            Part::Hero => "game_page_custom",
            Part::Igdb => "game_metadata",
            Part::Notes => "game_notes",
            Part::Input => "game_input",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Part::Hero => "hero",
            Part::Igdb => "igdb",
            Part::Notes => "notes",
            Part::Input => "input",
        }
    }

    fn from_key(key: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|p| p.key() == key)
    }
}

fn has_row(conn: &Connection, part: Part, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            &format!("SELECT 1 FROM {} WHERE gameId=?", part.table()),
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn has_user_igdb(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM game_metadata WHERE gameId=? AND source='user'",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Exchanges the rows of `a` and `b` in `part`'s table (either may have none).
fn swap(conn: &Connection, part: Part, a: &str, b: &str) -> Result<()> {
    let table = part.table();
    let temp = format!("\u{1}link-swap:{a}");
    conn.execute(
        &format!("UPDATE {table} SET gameId=?1 WHERE gameId=?2"),
        params![temp, a],
    )?;
    conn.execute(
        &format!("UPDATE {table} SET gameId=?1 WHERE gameId=?2"),
        params![a, b],
    )?;
    conn.execute(
        &format!("UPDATE {table} SET gameId=?1 WHERE gameId=?2"),
        params![b, temp],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Route names and edition detection

/// Characters trimmed around an edition name taken from a longer title.
fn edition_trim(c: char) -> bool {
    c.is_whitespace() || ":-–—|·,/()[]".contains(c)
}

/// The version name an entry's title gives next to the linked game's `shared` title:
/// "Minecraft: Java Edition" next to "Minecraft" → "Java Edition"; "Minecraft for Windows" →
/// "Windows"; a title that doesn't start with the shared one is used whole. `None` when the
/// titles are the same game name (`catalog::title_key`).
pub fn edition_name(original: &str, shared: &str) -> Option<String> {
    let original = plain_text(original);
    let shared = plain_text(shared);
    if original.is_empty() {
        return None;
    }
    let lower = |s: &str| s.chars().flat_map(char::to_lowercase).collect::<String>();
    let (o, s) = (lower(&original), lower(&shared));
    if !s.is_empty() && o.starts_with(&s) && o.chars().count() > s.chars().count() {
        // Same characters lower-cased: cut the original by character count.
        let cut: String = original.chars().skip(shared.chars().count()).collect();
        let trimmed = cut.trim_matches(edition_trim).to_string();
        let rest = trimmed
            .get(..4)
            .filter(|p| p.eq_ignore_ascii_case("for "))
            .map(|_| trimmed[4..].trim().to_string())
            .unwrap_or(trimmed);
        if let Some(name) = validation::stored_route_name(&rest) {
            return Some(name);
        }
    }
    if crate::catalog::title_key(&original) == crate::catalog::title_key(&shared) {
        return None;
    }
    validation::stored_route_name(&original)
}

/// Known Xbox / Microsoft Store packages whose package name says which edition they are.
const XBOX_EDITIONS: [(&str, &str); 4] = [
    ("microsoft.minecraftuwp", "Bedrock Edition"),
    ("microsoft.4297127d64ec6", "Java Edition"),
    ("microsoft.minecrafteducationedition", "Education Edition"),
    ("microsoft.minecraftwindowsbeta", "Bedrock Preview"),
];

/// The edition a known Xbox / Microsoft Store package is (`Microsoft.MinecraftUWP` → "Bedrock
/// Edition"), from the package name in its app id. Xbox titles come from package names
/// ("Minecraft UWP"), so this is a better name than the title.
pub fn known_edition(route: &GameRoute) -> Option<String> {
    if route.source_id != "xbox-appx" {
        return None;
    }
    let aumid = non_empty(&route.app_id).unwrap_or(&route.launch_target);
    let aumid = aumid.strip_prefix("shell:AppsFolder\\").unwrap_or(aumid);
    let package = aumid
        .split(['_', '!'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    XBOX_EDITIONS
        .iter()
        .find(|(name, _)| *name == package)
        .map(|(_, edition)| edition.to_string())
}

/// A version name a route itself suggests, for routes that would otherwise read the same: a
/// [`known_edition`], else the `.exe` name of a local PC route.
pub fn route_hint(route: &GameRoute) -> Option<String> {
    if let Some(edition) = known_edition(route) {
        return Some(edition);
    }
    if route.source_id == LOCAL_SOURCE {
        let target = route.launch_target.trim();
        if target.to_lowercase().ends_with(".exe") {
            let stem = std::path::Path::new(target).file_stem()?.to_string_lossy();
            return validation::stored_route_name(&stem);
        }
    }
    None
}

/// What tells a route apart in "Play from" before its name: its store, or its launcher.
fn label_key(route: &GameRoute) -> String {
    match crate::launchers::route_launcher(route) {
        Some(id) => format!("launcher:{id}"),
        None => route.store.to_lowercase(),
    }
}

/// Fills in names for routes that have none: a [`known_edition`], else the entry's own title
/// next to the shared title ([`edition_name`]); then, where two routes would still read the
/// same, from the route itself ([`route_hint`]). `games` is (entry title, its routes). Names
/// already set are never changed. Returns the (game, route) positions that got a name.
pub fn suggest_names(games: &mut [(String, Vec<GameRoute>)], shared: &str) -> Vec<(usize, usize)> {
    let mut suggested = Vec::new();
    for (g, (title, routes)) in games.iter_mut().enumerate() {
        let name = edition_name(title, shared);
        for (r, route) in routes.iter_mut().enumerate() {
            if route.name.is_none() {
                if let Some(name) = known_edition(route).or_else(|| name.clone()) {
                    route.name = Some(name);
                    suggested.push((g, r));
                }
            }
        }
    }
    let label = |route: &GameRoute| {
        (
            label_key(route),
            route.name.clone().unwrap_or_default().to_lowercase(),
        )
    };
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for (_, routes) in games.iter() {
        for route in routes {
            *counts.entry(label(route)).or_default() += 1;
        }
    }
    for (g, (_, routes)) in games.iter_mut().enumerate() {
        for (r, route) in routes.iter_mut().enumerate() {
            if route.name.is_none() && counts.get(&label(route)).copied().unwrap_or(0) > 1 {
                if let Some(hint) = route_hint(route) {
                    route.name = Some(hint);
                    suggested.push((g, r));
                }
            }
        }
    }
    suggested
}

/// How a route reads in confirmations: its store, then its name, cleaned.
pub fn route_display(route: &GameRoute) -> String {
    match route.name.as_deref() {
        Some(name) => display_text(&format!("{} · {name}", route.store), 100),
        None => display_text(&route.store, 100),
    }
}

// ---------------------------------------------------------------------------
// Selecting a route

/// A route of a game as the renderer names it: its position plus its stored source and target,
/// compared exactly with the stored route (so a selection can't land on a route that moved).
/// It only selects; nothing in it is ever stored.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteRef {
    pub index: usize,
    pub source_id: String,
    pub launch_target: String,
}

impl RouteRef {
    pub fn check(&self) -> Result<()> {
        if self.index >= 128
            || self.source_id.chars().count() > 128
            || self.launch_target.chars().count() > 8_192
        {
            bail!("Launch route is invalid.");
        }
        Ok(())
    }

    /// The stored route this names in `game`.
    pub fn find<'a>(&self, game: &'a Game) -> Result<&'a GameRoute> {
        self.check()?;
        game.routes
            .get(self.index)
            .filter(|r| r.source_id == self.source_id && r.launch_target == self.launch_target)
            .ok_or_else(|| anyhow!("This game's launch routes changed. Try again."))
    }
}

fn game(conn: &Connection, id: &str) -> Result<Game> {
    db::get_game(conn, id)?.ok_or_else(|| anyhow!("Game not found."))
}

/// Renames one route (display only). `name` was cleaned by `validation::route_name`.
pub fn rename_route(
    conn: &Connection,
    game_id: &str,
    route: &RouteRef,
    name: Option<String>,
) -> Result<()> {
    let game = game(conn, game_id)?;
    route.find(&game)?;
    let mut routes = game.routes;
    routes[route.index].name = name;
    db::set_routes(conn, game_id, routes)
}

// ---------------------------------------------------------------------------
// Launch options

/// The `.exe` a route starts directly, when the route can take launch options without knowing
/// its launcher; otherwise why not, in words for the user. Launcher routes take options too, but
/// need their launcher ([`launcher_host`], or [`args_host`] with the database).
pub fn args_exe(route: &GameRoute) -> Result<&str> {
    if crate::launchers::is_launcher_route(route) {
        bail!("This route runs through a custom launcher: its launch options are checked against that launcher.");
    }
    if route.source_id == ROM_SOURCE || non_empty(&route.emulator_id).is_some() {
        bail!(
            "ROMs start with their emulator's arguments. Change them under Settings → Emulators."
        );
    }
    if route.source_id != LOCAL_SOURCE {
        bail!(
            "{} starts this game itself, so Game Lobby can't pass launch options to it. Set them in {} instead.",
            display_text(&route.store, 40),
            display_text(&route.store, 40)
        );
    }
    let target = route.launch_target.trim();
    if !crate::emulators::drive_path_ok(target) || !target.to_lowercase().ends_with(".exe") {
        bail!("Only a game started from its own .exe can have launch options here (not a shortcut, folder or link).");
    }
    refuse_host_program(target)?;
    Ok(target)
}

/// What a route's launch options are passed to, worked out from the stored route (and, for a
/// launcher route, its stored launcher). Never from the renderer.
///
/// - A local route: the game's own `.exe`, nothing before the options, run in the `.exe`'s
///   folder. Confirmations are keyed by the `.exe` alone (as they always were).
/// - A launcher route (a launcher with a program): the launcher's `.exe`, the launcher's
///   template with this route's target ([`crate::launchers::build_argv`]) before the options,
///   run in the launcher's working folder. Confirmations are keyed by the program and the
///   working folder, and the argv they cover is the whole command line (template, target and
///   options), so a change to any of them needs a new confirmation.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgsHost {
    /// The program that gets the options.
    pub program: String,
    /// The arguments before the options.
    pub base: Vec<String>,
    /// The working folder.
    pub cwd: String,
    /// The launcher's name (cleaned), for a launcher route.
    pub launcher: Option<String>,
    /// What a confirmation is for besides the argv (see above).
    key: String,
}

impl ArgsHost {
    /// The whole argv a launch runs with `options`.
    pub fn argv(&self, options: &[String]) -> Vec<String> {
        self.base.iter().chain(options).cloned().collect()
    }

    fn noun(&self) -> &'static str {
        if self.launcher.is_some() {
            "launcher"
        } else {
            "game"
        }
    }
}

fn exe_host(route: &GameRoute) -> Result<ArgsHost> {
    let exe = args_exe(route)?;
    let key = crate::tracker::normalize_path(exe).ok_or_else(|| {
        anyhow!("Only a game started from its own .exe can have launch options here.")
    })?;
    let cwd = std::path::Path::new(exe)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ArgsHost {
        program: exe.to_string(),
        base: Vec::new(),
        cwd,
        launcher: None,
        key,
    })
}

/// The [`ArgsHost`] of launcher route `route` of launcher `l`. Shape checks only (no disk
/// access; `launchers::prepare_launch` checks the files at launch): a launcher with a program
/// that may take arguments ([`refuse_host_program`]), a valid template and a target of one of
/// its kinds. Label-only launchers never run their routes, so they take no options.
pub fn launcher_host(l: &CustomLauncher, route: &GameRoute) -> Result<ArgsHost> {
    use crate::launchers as lx;
    if lx::launcher_id_of(route) != Some(l.id.as_str()) {
        bail!("This game belongs to another launcher.");
    }
    let name = display_text(&l.name, 64);
    if lx::is_label_only(l) {
        bail!("{name} has no program, so this route can't have launch options. Its games run directly: set options on the game's own .exe route instead.");
    }
    if !crate::emulators::executable_path_ok(&l.executable) {
        bail!("{name}'s program must be a full path to an .exe file.");
    }
    refuse_host_program(&l.executable)?;
    lx::check_launcher_template(&l.args_template, &l.executable)?;
    let target = route.launch_target.as_str();
    if !l
        .kinds()?
        .into_iter()
        .any(|k| lx::target_shape_ok(k, target))
    {
        bail!("This game's target doesn't fit {name}. Add the game again.");
    }
    let base = lx::build_argv(&l.args_template, target)?;
    let cwd = match &l.working_dir {
        Some(dir) => dir.clone(),
        None => std::path::Path::new(&l.executable)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let normal = |p: &str| {
        crate::tracker::normalize_path(p)
            .ok_or_else(|| anyhow!("{name}'s program or working folder is invalid."))
    };
    let key = serde_json::to_string(&["launcher", &normal(&l.executable)?, &normal(&cwd)?])?;
    Ok(ArgsHost {
        program: l.executable.clone(),
        base,
        cwd,
        launcher: Some(name),
        key,
    })
}

/// The [`ArgsHost`] of a stored route: its launcher's (read from the database) for a launcher
/// route, its own `.exe` otherwise; or why it can't take launch options.
pub fn args_host(conn: &Connection, route: &GameRoute) -> Result<ArgsHost> {
    match crate::launchers::launcher_id_of(route) {
        Some(id) => {
            let l = crate::launchers::get(conn, id)?.ok_or_else(|| {
                anyhow!("This game's launcher was deleted. Add the game again to another launcher.")
            })?;
            launcher_host(&l, route)
        }
        None if crate::launchers::is_launcher_route(route) => {
            bail!("This game's launcher route is invalid.")
        }
        None => exe_host(route),
    }
}

/// Programs that run whatever their arguments say (shells, script hosts, interpreters) or are
/// part of Windows. With launch options they would be a way to start any other program or
/// script, so a route to one can't take launch options (the game itself is the program that
/// takes them).
fn refuse_host_program(exe: &str) -> Result<()> {
    let refuse = || {
        anyhow!("Launch options can't be used with this program: it runs other programs or scripts from its arguments, or is part of Windows.")
    };
    let normal = crate::tracker::normalize_path(exe).ok_or_else(refuse)?;
    let windows = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(|w| w.to_string_lossy().into_owned())
        .and_then(|w| crate::tracker::normalize_path(&w));
    let in_windows = |folder: &str| crate::tracker::is_within(folder, &normal);
    if windows.as_deref().is_some_and(in_windows) || in_windows(r"c:\windows") {
        return Err(refuse());
    }
    let name = normal.rsplit('\\').next().unwrap_or_default();
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    if HOST_PROGRAMS.contains(&stem)
        || stem.starts_with("python")
        || numbered("java")
        || numbered("node")
    {
        return Err(refuse());
    }
    Ok(())
}

/// Program names (lower case, without `.exe`) refused by [`refuse_host_program`], besides
/// `python*` and numbered `java*`/`node*` builds.
const HOST_PROGRAMS: [&str; 20] = [
    "cmd",
    "powershell",
    "pwsh",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "msiexec",
    "node",
    "java",
    "javaw",
    "javaws",
    "javac",
    "bash",
    "sh",
    "wsl",
    "conhost",
    "pythonw",
    "powershell_ise",
];

/// The argument text for one argv entry, quoted so [`crate::launch::tokenize_arguments`] reads it
/// back as exactly that entry. `None` when it can't be written (both quote characters).
fn quote(arg: &str) -> Option<String> {
    if !arg.is_empty() && !arg.contains(char::is_whitespace) && !arg.contains(['"', '\'']) {
        return Some(arg.to_string());
    }
    if !arg.contains('"') {
        Some(format!("\"{arg}\""))
    } else if !arg.contains('\'') {
        Some(format!("'{arg}'"))
    } else {
        None
    }
}

/// `argv` as argument text (for the editor and the checks).
pub fn args_text(argv: &[String]) -> Option<String> {
    argv.iter()
        .map(|a| quote(a))
        .collect::<Option<Vec<_>>>()
        .map(|v| v.join(" "))
}

/// Checks launch options the user typed for a game's own `.exe` (see [`prepare_options`]).
#[cfg(test)]
pub fn prepare_args(route: &GameRoute, text: &str) -> Result<Option<Vec<String>>> {
    prepare_options(&exe_host(route)?, text)
}

/// Checks launch options the user typed for a stored route (for its [`args_host`]). Returns the
/// host they were checked for (compared again after the native dialog) and the options.
pub fn prepare_route_args(
    conn: &Connection,
    route: &GameRoute,
    text: &str,
) -> Result<(ArgsHost, Option<Vec<String>>)> {
    let host = args_host(conn, route)?;
    let argv = prepare_options(&host, text)?;
    Ok((host, argv))
}

/// Checks launch options the user typed for `host`: the rules of every program Game Lobby
/// starts with arguments (`launch::check_arguments`: no shell characters, control or look-alike
/// characters, code-loading or debugging options, and paths only inside the program's folder:
/// the game's for a game's own `.exe`, the launcher's for a launcher route, exactly as for the
/// launcher's own template), no `{` or `}` on a launcher route (`{target}` belongs to the
/// launcher's template and is never substituted in options), and the count and length limits.
/// Returns the argv, or `None` for empty text (no options).
pub fn prepare_options(host: &ArgsHost, text: &str) -> Result<Option<Vec<String>>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > MAX_ARGS_CHARS {
        bail!("Launch options can be at most {MAX_ARGS_CHARS} characters.");
    }
    let noun = host.noun();
    let example = if host.launcher.is_some() {
        r"plugins\settings.ini"
    } else {
        r"config\settings.ini"
    };
    if host.launcher.is_some() && text.contains(['{', '}']) {
        bail!("Launch options can't contain {{ or }}: the launcher's own arguments already pass the game ({{target}}).");
    }
    crate::launch::check_arguments(text, &host.program, &[], noun, example)?;
    let argv = crate::launch::tokenize_arguments(text, noun)?;
    if argv.len() > MAX_ARGS {
        bail!("A route can have at most {MAX_ARGS} launch options.");
    }
    if argv.iter().any(|a| a.chars().count() > MAX_ARG_CHARS) {
        bail!("A launch option can be at most {MAX_ARG_CHARS} characters.");
    }
    if args_text(&argv).is_none() {
        bail!("A launch option can't contain both kinds of quotes.");
    }
    if host.launcher.is_some() {
        check_launcher_options(host, &argv)?;
    }
    Ok(Some(argv))
}

/// The extra rules for options on a launcher route ([`prepare_options`]):
/// - the whole command line (program, template, target, options and working folder) must fit
///   the native confirmation: at most [`MAX_ARGS`] arguments in all and [`MAX_CONFIRM_CHARS`]
///   characters as shown;
/// - when the launcher runs in a working folder other than its program's folder, no relative
///   paths: they are checked against the program's folder (`launch::check_arguments`) but the
///   launcher would look them up in the working folder.
fn check_launcher_options(host: &ArgsHost, options: &[String]) -> Result<()> {
    let name = host.launcher.as_deref().unwrap_or("This launcher");
    let argv = host.argv(options);
    let shown = |s: &str| validation::escape_for_prompt(s).chars().count();
    let chars =
        shown(&host.program) + shown(&host.cwd) + argv.iter().map(|a| shown(a)).sum::<usize>();
    if argv.len() > MAX_ARGS || chars > MAX_CONFIRM_CHARS {
        bail!("{name}'s command line is too long to confirm with launch options. Shorten its arguments or the game's path, or use fewer options: the whole command line can have at most {MAX_ARGS} arguments and {MAX_CONFIRM_CHARS} characters, counting {name}'s own arguments and the game's path.");
    }
    let program_folder = std::path::Path::new(&host.program)
        .parent()
        .and_then(|p| crate::tracker::normalize_path(&p.to_string_lossy()));
    let same_folder =
        program_folder.is_some() && program_folder == crate::tracker::normalize_path(&host.cwd);
    if !same_folder {
        if let Some(token) = crate::launch::first_relative_path(options) {
            bail!(
                "The launch option \"{}\" is a relative path, but {name} runs in a working folder other than its program's folder, so it wouldn't be looked up where it is checked. Use a full path inside {name}'s folder instead, like {}.",
                validation::escape_for_prompt(token),
                std::path::Path::new(&host.program)
                    .with_file_name(r"plugins\settings.ini")
                    .display()
            );
        }
    }
    Ok(())
}

/// The stored launch options of a game's own `.exe` route, checked again (see
/// [`stored_options`]).
#[cfg(test)]
pub fn valid_stored_args(route: &GameRoute) -> Result<Vec<String>> {
    if non_empty(&route.launch_args).is_none() {
        return Ok(Vec::new());
    }
    stored_options(&exe_host(route)?, route)
}

/// The stored launch options of `route`, checked again for `host` (a route may come from a
/// backup or an older version): the same rules as [`prepare_options`]. Empty when it has none.
/// Whether the user confirmed them is checked by [`checked_options`].
fn stored_options(host: &ArgsHost, route: &GameRoute) -> Result<Vec<String>> {
    if non_empty(&route.launch_args).is_none() {
        return Ok(Vec::new());
    }
    let argv = crate::launch::parse_launch_arguments(route.launch_args.as_deref());
    let text = args_text(&argv).ok_or_else(|| {
        anyhow!("This game's launch options can't be used. Edit them on its page.")
    })?;
    match prepare_options(host, &text)? {
        Some(checked) if checked == argv => Ok(argv),
        None if argv.is_empty() => Ok(argv),
        _ => bail!("This game's launch options can't be used. Edit them on its page."),
    }
}

/// What a confirmation is for: the host's key and the whole argv (JSON).
fn confirm_key(host: &ArgsHost, options: &[String]) -> Result<(String, String)> {
    Ok((
        host.key.clone(),
        serde_json::to_string(&host.argv(options))?,
    ))
}

/// Whether the user confirmed `options` for exactly `host`, in the native dialog.
pub fn host_confirmed(conn: &Connection, host: &ArgsHost, options: &[String]) -> Result<bool> {
    let (key, argv) = confirm_key(host, options)?;
    Ok(conn
        .query_row(
            "SELECT 1 FROM launch_args_confirmed WHERE exeKey=? AND argv=?",
            params![key, argv],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Remembers that the user confirmed `options` for `host`. Only called after the native dialog
/// and its compare-and-save (`link_commands::library_route_args`,
/// `launcher_commands::launchers_assign`); the table is never part of a backup, so restored
/// options stay inert until confirmed there.
pub fn confirm_host_args(conn: &Connection, host: &ArgsHost, options: &[String]) -> Result<()> {
    let (key, argv) = confirm_key(host, options)?;
    conn.execute(
        "INSERT OR REPLACE INTO launch_args_confirmed(exeKey,argv,confirmedAt) VALUES(?,?,?)",
        params![key, argv, now_ms()],
    )?;
    Ok(())
}

/// [`confirm_host_args`] for a stored route's current host (tests only: the commands confirm
/// the host the dialog showed).
#[cfg(test)]
pub fn confirm_args(conn: &Connection, route: &GameRoute, argv: &[String]) -> Result<()> {
    confirm_host_args(conn, &args_host(conn, route)?, argv)
}

/// The launch options `route` runs with for `host`: [`stored_options`], and only ones the user
/// confirmed for exactly this host. Options from a backup, a merge or an older version, or
/// confirmed for another program, command line or folder, are refused until they are confirmed
/// on the game page.
fn checked_options(conn: &Connection, host: &ArgsHost, route: &GameRoute) -> Result<Vec<String>> {
    let argv = stored_options(host, route)?;
    if !argv.is_empty() && !host_confirmed(conn, host, &argv)? {
        bail!("Confirm this route's launch options again on its game page.");
    }
    Ok(argv)
}

/// The launch options a stored route runs with ([`checked_options`] for its [`args_host`]).
/// Empty when it has none (then nothing else about the route is checked here).
pub fn checked_stored_args(conn: &Connection, route: &GameRoute) -> Result<Vec<String>> {
    if non_empty(&route.launch_args).is_none() {
        return Ok(Vec::new());
    }
    checked_options(conn, &args_host(conn, route)?, route)
}

/// The whole argv of a launch through launcher `l` (`launch` is its checked
/// `launchers::prepare_launch`): the launcher's argv, plus the route's launch options when it
/// has some, which must be confirmed for exactly this program, command line and working folder.
pub fn launcher_launch_argv(
    conn: &Connection,
    l: &CustomLauncher,
    route: &GameRoute,
    launch: &crate::launchers::LauncherLaunch,
) -> Result<Vec<String>> {
    if non_empty(&route.launch_args).is_none() {
        return Ok(launch.argv.clone());
    }
    let host = launcher_host(l, route)?;
    // Both come from the same launcher and route; should they ever disagree, refuse rather than
    // run options confirmed for another command line.
    if host.base != launch.argv
        || std::path::Path::new(&host.program) != launch.program
        || std::path::Path::new(&host.cwd) != launch.cwd
    {
        bail!("Confirm this route's launch options again on its game page.");
    }
    let options = checked_options(conn, &host, route)?;
    Ok(host.argv(&options))
}

/// The route has launch options that would be refused at launch ([`checked_stored_args`]): the
/// game page shows them as needing confirmation.
pub fn args_need_confirmation(conn: &Connection, route: &GameRoute) -> bool {
    non_empty(&route.launch_args).is_some() && checked_stored_args(conn, route).is_err()
}

/// The native confirmation for new launch options of a game's own `.exe` route (tests; the
/// command uses [`route_args_prompt`] with the host it checked).
#[cfg(test)]
pub fn args_prompt(game_title: &str, route: &GameRoute, argv: &[String]) -> Result<String> {
    Ok(route_args_prompt(
        &exe_host(route)?,
        game_title,
        route,
        argv,
    ))
}

/// The numbered argument lines of a confirmation (one per line), escaped
/// (`validation::escape_for_prompt`).
pub fn numbered(argv: &[String]) -> String {
    argv.iter()
        .enumerate()
        .map(|(i, arg)| format!("  {}. {}", i + 1, validation::escape_for_prompt(arg)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Which arguments of a launcher command line are this game's launch options, for a
/// confirmation (`base` arguments come first).
pub fn options_note(base: usize, options: usize, launcher: &str) -> String {
    let first = base + 1;
    let which = if options == 1 {
        format!("Argument {first} is")
    } else {
        format!("Arguments {first} to {} are", base + options)
    };
    format!("{which} this game's launch options; the others come from {launcher}'s settings and the game's target.")
}

/// The native confirmation for new launch options: the program, the whole argv (numbered) and
/// the working folder first; the game title, route name and launcher name (user-editable text)
/// last, cleaned.
pub fn route_args_prompt(
    host: &ArgsHost,
    game_title: &str,
    route: &GameRoute,
    options: &[String],
) -> String {
    let facts = format!(
        "Game Lobby will run:\n  {}\n\nArguments (one per line):\n{}\n\nWorking folder:\n  {}\n\nFor the game: {}\nLaunch route: {}",
        validation::escape_for_prompt(&host.program),
        numbered(&host.argv(options)),
        validation::escape_for_prompt(&host.cwd),
        display_text(game_title, 80),
        route_display(route),
    );
    let end = "Play runs exactly this from now on when this route is chosen. Save only options you trust.";
    match &host.launcher {
        None => format!("{facts}\n\n{end}"),
        Some(name) => format!(
            "{facts}\nLauncher: {name}\n\n{} {end}",
            options_note(host.base.len(), options.len(), name)
        ),
    }
}

/// Stores `argv` (or none) on the route, as a JSON list.
pub fn set_args(
    conn: &Connection,
    game_id: &str,
    route: &RouteRef,
    argv: Option<&[String]>,
) -> Result<()> {
    let game = game(conn, game_id)?;
    route.find(&game)?;
    let mut routes = game.routes;
    routes[route.index].launch_args = argv.map(serde_json::to_string).transpose()?;
    db::set_routes(conn, game_id, routes)
}

// ---------------------------------------------------------------------------
// Linking

/// The review's choices: game ids from the linked set, a favourite flag and an optional custom
/// title (the only free text, cleaned like every title).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkChoices {
    /// The game that stays (its id is the combined game's id).
    pub keep: String,
    pub title_from: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    pub cover_from: String,
    pub hero_from: String,
    pub notes_from: String,
    pub igdb_from: String,
    pub favorite: bool,
}

/// Choices after checking (`None` = the default rule, as `db::merge_games` uses).
#[derive(Debug, Default, Clone)]
struct Resolved {
    /// The game whose title is used.
    title_from: Option<String>,
    /// A title the user typed (wins over `title_from`).
    custom_title: Option<String>,
    cover: Option<String>,
    hero: Option<String>,
    notes: Option<String>,
    igdb: Option<String>,
    favorite: Option<bool>,
}

fn check_ids(ids: &[String]) -> Result<()> {
    if ids.len() < 2 {
        bail!("Choose at least two games to link.");
    }
    if ids.len() > MAX_LINK_GAMES {
        bail!("You can link at most {MAX_LINK_GAMES} games at once.");
    }
    let unique: HashSet<&String> = ids.iter().collect();
    if unique.len() != ids.len() {
        bail!("Choose two different games to link.");
    }
    Ok(())
}

fn resolve_choices(ids: &[String], choices: &LinkChoices) -> Result<(String, Resolved)> {
    let listed = |id: &String, what: &str| -> Result<String> {
        if ids.contains(id) {
            Ok(id.clone())
        } else {
            bail!("Choose the {what} from one of the games being linked.")
        }
    };
    let keep = listed(&choices.keep, "game to keep")?;
    let custom = match choices.custom_title.as_deref().map(plain_text) {
        Some(t) if t.is_empty() => None,
        Some(t) if t.chars().count() > 300 => bail!("The title can be at most 300 characters."),
        other => other,
    };
    Ok((
        keep,
        Resolved {
            title_from: Some(listed(&choices.title_from, "title")?),
            custom_title: custom,
            cover: Some(listed(&choices.cover_from, "cover")?),
            hero: Some(listed(&choices.hero_from, "page art")?),
            notes: Some(listed(&choices.notes_from, "notes")?),
            igdb: Some(listed(&choices.igdb_from, "IGDB details")?),
            favorite: Some(choices.favorite),
        },
    ))
}

fn seconds(game: &Game) -> i64 {
    game.playtime_seconds.unwrap_or(game.playtime_minutes * 60)
}

fn custom_cover(game: &Game) -> bool {
    non_empty(&game.cover).is_some() && game.cover != game.default_cover
}

fn load(conn: &Connection, ids: &[String]) -> Result<Vec<Game>> {
    ids.iter()
        .map(|id| {
            db::get_game(conn, id)?
                .ok_or_else(|| anyhow!("One of the selected games no longer exists."))
        })
        .collect()
}

fn final_title(games: &[Game], keep: &Game, resolved: &Resolved) -> String {
    if let Some(custom) = &resolved.custom_title {
        return custom.clone();
    }
    resolved
        .title_from
        .as_deref()
        .and_then(|id| games.iter().find(|g| g.id == id))
        .map_or_else(|| keep.title.clone(), |g| g.title.clone())
}

/// Which entry's row a part uses: the explicit choice, else the default rule (the kept game's
/// row when it has one, else the other's; IGDB: a match the user chose wins).
fn winner(
    conn: &Connection,
    part: Part,
    keep: &str,
    source: &str,
    resolved: &Resolved,
) -> Result<bool> {
    let choice = match part {
        Part::Hero => resolved.hero.as_deref(),
        Part::Igdb => resolved.igdb.as_deref(),
        Part::Notes => resolved.notes.as_deref(),
        Part::Input => None,
    };
    let source_wins = match choice {
        Some(id) => id == source,
        None => match part {
            Part::Igdb => !has_user_igdb(conn, keep)? && has_user_igdb(conn, source)?,
            _ => !has_row(conn, part, keep)?,
        },
    };
    Ok(source_wins && has_row(conn, part, source)?)
}

fn column<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str, id: &str) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<T>>>()?;
    Ok(rows)
}

/// Links `ids` into one game (see the module docs); `choices` `None` uses the default rules
/// (`db::merge_games`). Returns (kept id, linked ids) for the play tracker.
pub fn link(
    conn: &mut Connection,
    ids: &[String],
    choices: Option<&LinkChoices>,
) -> Result<(String, Vec<String>)> {
    check_ids(ids)?;
    let (keep_id, resolved) = match choices {
        Some(c) => resolve_choices(ids, c)?,
        None => (ids[0].clone(), Resolved::default()),
    };
    let games = load(conn, ids)?;
    let keep = games
        .iter()
        .find(|g| g.id == keep_id)
        .cloned()
        .ok_or_else(|| anyhow!("Game not found."))?;
    let sources: Vec<Game> = games.iter().filter(|g| g.id != keep_id).cloned().collect();
    let title = final_title(&games, &keep, &resolved);
    // Names for unnamed routes, from each entry's own title (only when choices came from the
    // review, which shows them; the default merge leaves routes as they are).
    let mut named: Vec<(String, Vec<GameRoute>)> = std::iter::once(&keep)
        .chain(sources.iter())
        .map(|g| (g.title.clone(), g.routes.clone()))
        .collect();
    if choices.is_some() {
        suggest_names(&mut named, &title);
    }
    let now = now_ms();
    let tx = conn.transaction()?;
    let mut routes = named[0].1.clone();
    let mut total = seconds(&keep);
    let mut last = keep.last_played.unwrap_or(0);
    let mut linked = Vec::new();
    for (source, (_, source_routes)) in sources.iter().zip(named.iter().skip(1)) {
        let sid = source.id.as_str();
        // Routes the game already has (the kept game's, or an earlier entry's) stay the game's
        // when this entry is unlinked; the entry gets a copy instead.
        let have: HashSet<String> = routes.iter().map(db::route_identity).collect();
        let mut route_keys: Vec<String> = Vec::new();
        let mut shared_keys: Vec<String> = Vec::new();
        for route in source_routes {
            let key = db::route_identity(route);
            let list = if have.contains(&key) {
                &mut shared_keys
            } else {
                &mut route_keys
            };
            if !list.contains(&key) {
                list.push(key);
            }
        }
        db::merge_routes_into(&mut routes, source_routes);
        let mut snapshot = LinkSnapshot {
            title: source.title.clone(),
            cover: source.cover.clone(),
            default_cover: source.default_cover.clone(),
            genre: source.genre.clone(),
            favorite: source.favorite,
            hidden: source.hidden,
            playtime_seconds: seconds(source),
            last_played: source.last_played,
            route_keys,
            shared_keys,
            session_starts: column(
                &tx,
                "SELECT startedAt FROM play_sessions WHERE gameId=? ORDER BY startedAt",
                sid,
            )?,
            screenshot_ids: Vec::new(),
            memory_ids: column(
                &tx,
                "SELECT id FROM game_memories WHERE gameId=? ORDER BY id",
                sid,
            )?,
            took: Vec::new(),
        };
        let shots: Vec<String> =
            column(&tx, "SELECT id FROM game_screenshots WHERE gameId=?", sid)?;
        tx.execute(
            "UPDATE play_sessions SET gameId=?1 WHERE gameId=?2",
            params![keep_id, sid],
        )?;
        // A picture both have stays once (the kept game's row); the other is parked.
        tx.execute(
            "UPDATE OR IGNORE game_screenshots SET gameId=?1 WHERE gameId=?2",
            params![keep_id, sid],
        )?;
        snapshot.screenshot_ids = shots
            .into_iter()
            .filter(|id| {
                tx.query_row(
                    "SELECT 1 FROM game_screenshots WHERE id=? AND gameId=?",
                    params![id, keep_id],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some()
            })
            .collect();
        tx.execute(
            "UPDATE game_memories SET gameId=?1 WHERE gameId=?2",
            params![keep_id, sid],
        )?;
        for part in Part::ALL {
            if winner(&tx, part, &keep_id, sid, &resolved)? {
                swap(&tx, part, &keep_id, sid)?;
                snapshot.took.push(part.key().to_string());
                if part == Part::Igdb && resolved.igdb.as_deref() == Some(sid) {
                    // The user picked this entry's match in the review: it is their choice now,
                    // so the automatic matches are dropped below without touching it.
                    tx.execute(
                        "UPDATE game_metadata SET source='user' WHERE gameId=?",
                        [&keep_id],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE OR IGNORE install_sizes SET gameId=?1 WHERE gameId=?2",
            params![keep_id, sid],
        )?;
        tx.execute("DELETE FROM install_sizes WHERE gameId=?", [sid])?;
        tx.execute("DELETE FROM game_art WHERE gameId=?", [sid])?;
        // Entries linked into this one now belong to the kept game.
        tx.execute(
            "UPDATE game_links SET gameId=?1 WHERE gameId=?2",
            params![keep_id, sid],
        )?;
        put_record(
            &tx,
            &LinkRecord {
                member_id: source.id.clone(),
                game_id: keep_id.clone(),
                linked_at: now,
                snapshot,
            },
        )?;
        tx.execute("DELETE FROM games WHERE id=?", [sid])?;
        total += seconds(source);
        last = last.max(source.last_played.unwrap_or(0));
        linked.push(source.id.clone());
    }
    if routes.len() > 128 {
        bail!("A game can have at most 128 launch routes.");
    }
    let cover_game = match resolved.cover.as_deref() {
        Some(id) => games.iter().find(|g| g.id == id).cloned(),
        None => std::iter::once(&keep)
            .chain(sources.iter())
            .find(|g| custom_cover(g))
            .cloned(),
    };
    let cover = match &cover_game {
        Some(g) => g.cover.clone().or_else(|| g.default_cover.clone()),
        None => std::iter::once(&keep)
            .chain(sources.iter())
            .find_map(|g| g.cover.clone()),
    };
    let default_cover = cover_game
        .as_ref()
        .and_then(|g| g.default_cover.clone())
        .or_else(|| {
            std::iter::once(&keep)
                .chain(sources.iter())
                .find_map(|g| g.default_cover.clone())
        });
    let favorite = resolved
        .favorite
        .unwrap_or(keep.favorite || sources.iter().any(|g| g.favorite));
    db::save_patch(
        &tx,
        &GamePatch {
            title: Some(title),
            cover,
            default_cover,
            genre: keep
                .genre
                .clone()
                .or_else(|| sources.iter().find_map(|g| g.genre.clone())),
            playtime_seconds: Some(total),
            playtime_minutes: Some(total.div_euclid(60)),
            last_played: (last != 0).then_some(last),
            favorite: Some(favorite),
            hidden: Some(keep.hidden || sources.iter().any(|g| g.hidden)),
            routes: Some(routes),
            ..GamePatch::from(&keep)
        },
    )?;
    // The routes changed, so art and an automatic IGDB match are looked up again; a match the
    // user chose stays.
    tx.execute("DELETE FROM game_art WHERE gameId=?", [&keep_id])?;
    tx.execute(
        "DELETE FROM game_metadata WHERE gameId=? AND source<>'user'",
        [&keep_id],
    )?;
    tx.commit()?;
    Ok((keep_id, linked))
}

// ---------------------------------------------------------------------------
// Review preview

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRoute {
    pub game_id: String,
    pub store: String,
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The name was suggested for this link (not set before).
    pub suggested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned: Option<bool>,
    pub target: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewGame {
    pub id: String,
    pub title: String,
    pub store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    pub custom_cover: bool,
    pub favorite: bool,
    pub hidden: bool,
    pub playtime_seconds: i64,
    pub sessions: i64,
    pub screenshots: i64,
    pub has_hero: bool,
    pub has_notes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igdb_name: Option<String>,
    pub igdb_chosen: bool,
    /// Entries linked into this game earlier.
    pub linked: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Conflicts {
    pub title: bool,
    pub cover: bool,
    pub hero: bool,
    pub notes: bool,
    pub igdb: bool,
    pub favorite: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkPreview {
    pub games: Vec<PreviewGame>,
    /// Every route of the combined game, in order, with its (suggested) name.
    pub routes: Vec<PreviewRoute>,
    pub conflicts: Conflicts,
    /// What linking does without other choices (the first id is kept).
    pub defaults: LinkChoices,
    pub playtime_seconds: i64,
    pub sessions: i64,
}

fn count(conn: &Connection, sql: &str, id: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [id], |r| r.get(0))?)
}

fn preview_game(conn: &Connection, game: &Game) -> Result<PreviewGame> {
    let id = game.id.as_str();
    let (igdb_name, igdb_chosen) = conn
        .query_row(
            "SELECT json,source FROM game_metadata WHERE gameId=? AND igdbId IS NOT NULL",
            [id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(json, source)| {
            let name = json
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                .and_then(|v| {
                    v.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| display_text(n, 120))
                });
            (name, source == "user")
        })
        .unwrap_or((None, false));
    Ok(PreviewGame {
        id: game.id.clone(),
        title: game.title.clone(),
        store: game.store.clone(),
        cover: game.cover.clone().or_else(|| game.default_cover.clone()),
        custom_cover: custom_cover(game),
        favorite: game.favorite,
        hidden: game.hidden,
        playtime_seconds: seconds(game),
        sessions: count(
            conn,
            "SELECT COUNT(*) FROM play_sessions WHERE gameId=?",
            id,
        )?,
        screenshots: count(
            conn,
            "SELECT COUNT(*) FROM game_screenshots WHERE gameId=?",
            id,
        )?,
        has_hero: has_row(conn, Part::Hero, id)?,
        has_notes: count(
            conn,
            "SELECT COUNT(*) FROM game_notes WHERE gameId=? AND (rating IS NOT NULL OR notes<>'')",
            id,
        )? > 0,
        igdb_name,
        igdb_chosen,
        linked: count(conn, "SELECT COUNT(*) FROM game_links WHERE gameId=?", id)? as usize,
    })
}

/// What linking `ids` would do (the first id is the default game to keep; `title` the title the
/// names are suggested against, default the kept game's).
pub fn preview(conn: &Connection, ids: &[String], title: Option<&str>) -> Result<LinkPreview> {
    check_ids(ids)?;
    let games = load(conn, ids)?;
    let summaries = games
        .iter()
        .map(|g| preview_game(conn, g))
        .collect::<Result<Vec<_>>>()?;
    let keep = &games[0];
    let shared = title
        .map(plain_text)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| keep.title.clone());
    let mut named: Vec<(String, Vec<GameRoute>)> = games
        .iter()
        .map(|g| (g.title.clone(), g.routes.clone()))
        .collect();
    let suggested: HashSet<(usize, usize)> =
        suggest_names(&mut named, &shared).into_iter().collect();
    // The combined route list, as `link` builds it (routes that launch the same thing merge).
    let mut combined: Vec<(GameRoute, String, bool)> = Vec::new();
    for (g, (_, routes)) in named.iter().enumerate() {
        for (r, route) in routes.iter().enumerate() {
            let key = db::route_identity(route);
            if combined
                .iter()
                .any(|(c, _, _)| db::route_identity(c) == key)
            {
                continue;
            }
            combined.push((
                route.clone(),
                games[g].id.clone(),
                suggested.contains(&(g, r)),
            ));
        }
    }
    let routes = combined
        .into_iter()
        .map(|(route, game_id, suggested)| PreviewRoute {
            game_id,
            store: route.store.clone(),
            source_id: route.source_id.clone(),
            name: route.name.clone(),
            suggested,
            installed: route.installed,
            owned: route.owned,
            target: route.launch_target.clone(),
        })
        .collect();
    let distinct = |values: Vec<String>| values.iter().collect::<HashSet<_>>().len() > 1;
    let conflicts = Conflicts {
        title: distinct(
            games
                .iter()
                .map(|g| crate::catalog::title_key(&g.title))
                .collect(),
        ),
        cover: summaries.iter().filter(|g| g.custom_cover).count() > 1
            || distinct(
                games
                    .iter()
                    .filter_map(|g| g.cover.clone().or(g.default_cover.clone()))
                    .collect(),
            ),
        hero: summaries.iter().filter(|g| g.has_hero).count() > 1,
        notes: summaries.iter().filter(|g| g.has_notes).count() > 1,
        igdb: distinct(
            summaries
                .iter()
                .filter_map(|g| g.igdb_name.clone())
                .collect(),
        ),
        favorite: summaries.iter().any(|g| g.favorite) && !summaries.iter().all(|g| g.favorite),
    };
    let first_with = |f: &dyn Fn(&PreviewGame) -> bool| {
        summaries
            .iter()
            .find(|g| f(g))
            .unwrap_or(&summaries[0])
            .id
            .clone()
    };
    let defaults = LinkChoices {
        keep: keep.id.clone(),
        title_from: keep.id.clone(),
        custom_title: None,
        cover_from: first_with(&|g| g.custom_cover),
        hero_from: first_with(&|g| g.has_hero),
        notes_from: first_with(&|g| g.has_notes),
        igdb_from: summaries
            .iter()
            .find(|g| g.igdb_chosen)
            .map_or_else(|| first_with(&|g| g.igdb_name.is_some()), |g| g.id.clone()),
        favorite: summaries.iter().any(|g| g.favorite),
    };
    Ok(LinkPreview {
        playtime_seconds: summaries.iter().map(|g| g.playtime_seconds).sum(),
        sessions: summaries.iter().map(|g| g.sessions).sum(),
        games: summaries,
        routes,
        conflicts,
        defaults,
    })
}

// ---------------------------------------------------------------------------
// Unlinking

/// What [`unlink`] would do.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnlinkPreview {
    /// The new game's title.
    pub title: String,
    /// The routes that leave, as "store · name".
    pub routes: Vec<String>,
    /// The game the routes leave keeps these.
    pub remaining: Vec<String>,
    /// The entry comes back as it was linked (its own id, details and sessions).
    pub restores: bool,
    pub sessions: i64,
    pub playtime_seconds: i64,
}

struct Plan {
    game: Game,
    /// The routes (indexes) that leave the game.
    moving: Vec<usize>,
    /// Routes the game keeps that the entry also had when it was linked: the entry gets a copy.
    copies: Vec<usize>,
    member: Option<LinkRecord>,
    new_id: String,
    restores: bool,
    title: String,
    sessions: Vec<i64>,
    seconds: i64,
}

fn plan(conn: &Connection, game_id: &str, route: &RouteRef) -> Result<Plan> {
    let game = game(conn, game_id)?;
    let chosen = route.find(&game)?;
    let key = db::route_identity(chosen);
    // The entry that brought the route; else one that had the same route already (it is still
    // the game's, and that entry comes back with a copy).
    let records = records_of(conn, game_id)?;
    let member = records
        .iter()
        .find(|r| r.snapshot.route_keys.contains(&key))
        .or_else(|| {
            records
                .iter()
                .find(|r| r.snapshot.shared_keys.contains(&key))
        })
        .cloned();
    let indexes = |keys: &[String]| -> Vec<usize> {
        game.routes
            .iter()
            .enumerate()
            .filter(|(_, r)| keys.contains(&db::route_identity(r)))
            .map(|(i, _)| i)
            .collect()
    };
    let (moving, copies): (Vec<usize>, Vec<usize>) = match &member {
        Some(m) => (
            indexes(&m.snapshot.route_keys),
            indexes(&m.snapshot.shared_keys),
        ),
        None => (vec![route.index], Vec::new()),
    };
    if moving.len() >= game.routes.len() {
        bail!(match &member {
            Some(_) => "Every launch route of this game came from that entry, so there is nothing to split off.",
            None => "This is the game's only launch route.",
        });
    }
    if moving.is_empty() && copies.is_empty() {
        bail!("None of that entry's launch routes are part of this game any more, so there is nothing to split off.");
    }
    let (new_id, restores, title, sessions, seconds) = match &member {
        Some(m) => {
            let free = db::get_game(conn, &m.member_id)?.is_none();
            let new_id = if free {
                m.member_id.clone()
            } else {
                format!("linked:{}", uuid::Uuid::new_v4().simple())
            };
            let mut sessions = Vec::new();
            for start in &m.snapshot.session_starts {
                if let Some(id) = conn
                    .query_row(
                        "SELECT id FROM play_sessions WHERE gameId=? AND startedAt=?",
                        params![game_id, start],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?
                {
                    sessions.push(id);
                }
            }
            let title = Some(plain_text(&m.snapshot.title))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| game.title.clone());
            (
                new_id,
                free,
                title,
                sessions,
                m.snapshot.playtime_seconds.clamp(0, seconds(&game)),
            )
        }
        None => {
            let label = chosen.name.clone().unwrap_or_else(|| chosen.store.clone());
            let title = display_text(&format!("{} ({label})", game.title), 300);
            (
                format!("linked:{}", uuid::Uuid::new_v4().simple()),
                false,
                title,
                Vec::new(),
                0,
            )
        }
    };
    Ok(Plan {
        game,
        moving,
        copies,
        member,
        new_id,
        restores,
        title,
        sessions,
        seconds,
    })
}

pub fn unlink_preview(conn: &Connection, game_id: &str, route: &RouteRef) -> Result<UnlinkPreview> {
    let p = plan(conn, game_id, route)?;
    let (moving, remaining): (Vec<_>, Vec<_>) = p
        .game
        .routes
        .iter()
        .enumerate()
        .partition(|(i, _)| p.moving.contains(i));
    // The entry's own routes first, then its copies of routes the game keeps.
    let leaving: Vec<String> = moving
        .iter()
        .map(|(_, r)| route_display(r))
        .chain(p.copies.iter().map(|i| route_display(&p.game.routes[*i])))
        .collect();
    Ok(UnlinkPreview {
        title: p.title,
        routes: leaving,
        remaining: remaining
            .into_iter()
            .map(|(_, r)| route_display(r))
            .collect(),
        restores: p.member.is_some(),
        sessions: p.sessions.len() as i64,
        playtime_seconds: p.seconds,
    })
}

/// Splits the route `route` of `game_id` (and the routes that came in with it) off into a game
/// of its own (see the module docs). Returns the new game's id.
pub fn unlink(conn: &mut Connection, game_id: &str, route: &RouteRef) -> Result<String> {
    let p = plan(conn, game_id, route)?;
    let (moving, remaining): (Vec<_>, Vec<_>) = p
        .game
        .routes
        .iter()
        .cloned()
        .enumerate()
        .partition(|(i, _)| p.moving.contains(i));
    let mut moving: Vec<GameRoute> = moving.into_iter().map(|(_, r)| r).collect();
    let remaining: Vec<GameRoute> = remaining.into_iter().map(|(_, r)| r).collect();
    // The entry also takes a copy of the routes it shared with the game, which keeps them.
    moving.extend(p.copies.iter().map(|i| p.game.routes[*i].clone()));
    let game = &p.game;
    let tx = conn.transaction()?;
    // The entry's details and the records of the entries that came in with it go first, so the
    // new game is inserted under the entry's id with nothing left to release (`db::persist_game`).
    if let Some(m) = &p.member {
        if p.restores {
            release_member(&tx, &m.member_id)?;
        } else {
            move_within(&tx, m, &p.new_id)?;
            tx.execute("DELETE FROM game_links WHERE memberId=?", [&m.member_id])?;
        }
    }
    let new_game = match &p.member {
        Some(m) => {
            let s = &m.snapshot;
            GamePatch {
                id: p.new_id.clone(),
                title: Some(p.title.clone()),
                cover: s.cover.clone(),
                default_cover: s.default_cover.clone(),
                store: Some(moving[0].store.clone()),
                genre: s.genre.clone(),
                playtime_seconds: Some(p.seconds),
                playtime_minutes: Some(p.seconds.div_euclid(60)),
                last_played: s.last_played,
                hidden: Some(s.hidden),
                favorite: Some(s.favorite),
                routes: Some(moving.clone()),
            }
        }
        None => {
            let store_seconds = moving
                .iter()
                .map(|r| r.playtime_minutes.unwrap_or(0) * 60)
                .sum::<i64>();
            GamePatch {
                id: p.new_id.clone(),
                title: Some(p.title.clone()),
                // The store's art, not the user's own cover.
                cover: game.default_cover.clone(),
                default_cover: game.default_cover.clone(),
                store: Some(moving[0].store.clone()),
                genre: game.genre.clone(),
                playtime_seconds: Some(store_seconds),
                playtime_minutes: Some(store_seconds.div_euclid(60)),
                last_played: moving.iter().filter_map(|r| r.last_played).max(),
                hidden: Some(game.hidden),
                favorite: Some(false),
                routes: Some(moving.clone()),
            }
        }
    };
    db::save_patch(&tx, &new_game)?;
    let left = (seconds(game) - p.seconds).max(0);
    db::save_patch(
        &tx,
        &GamePatch {
            playtime_seconds: Some(left),
            playtime_minutes: Some(left.div_euclid(60)),
            routes: Some(remaining),
            ..GamePatch::new(game_id)
        },
    )?;
    for id in &p.sessions {
        tx.execute(
            "UPDATE play_sessions SET gameId=?1 WHERE id=?2 AND gameId=?3",
            params![p.new_id, id, game_id],
        )?;
    }
    if let Some(m) = &p.member {
        for id in &m.snapshot.screenshot_ids {
            tx.execute(
                "UPDATE OR IGNORE game_screenshots SET gameId=?1 WHERE id=?2 AND gameId=?3",
                params![p.new_id, id, game_id],
            )?;
        }
        for id in &m.snapshot.memory_ids {
            tx.execute(
                "UPDATE game_memories SET gameId=?1 WHERE id=?2 AND gameId=?3",
                params![p.new_id, id, game_id],
            )?;
        }
    }
    for id in [game_id, p.new_id.as_str()] {
        tx.execute("DELETE FROM game_art WHERE gameId=?", [id])?;
    }
    tx.execute(
        "DELETE FROM game_metadata WHERE gameId=? AND source<>'user'",
        [game_id],
    )?;
    crate::health::remember_separate(&tx, game_id, &p.new_id)?;
    tx.commit()?;
    Ok(p.new_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(source: &str, store: &str, target: &str) -> GameRoute {
        GameRoute {
            source_id: source.into(),
            store: store.into(),
            launch_target: target.into(),
            installed: Some(true),
            ..Default::default()
        }
    }

    fn xbox(package: &str) -> GameRoute {
        let aumid = format!("{package}_8wekyb3d8bbwe!App");
        GameRoute {
            app_id: Some(aumid.clone()),
            ..route("xbox-appx", "Xbox", &format!("shell:AppsFolder\\{aumid}"))
        }
    }

    fn game(id: &str, title: &str, routes: Vec<GameRoute>) -> Game {
        Game {
            id: id.into(),
            title: title.into(),
            store: routes.first().map(|r| r.store.clone()).unwrap_or_default(),
            routes,
            ..Default::default()
        }
    }

    fn put(conn: &mut Connection, games: &[Game]) {
        for g in games {
            db::save_game(conn, &GamePatch::from(g)).unwrap();
        }
    }

    fn rref(game: &Game, index: usize) -> RouteRef {
        RouteRef {
            index,
            source_id: game.routes[index].source_id.clone(),
            launch_target: game.routes[index].launch_target.clone(),
        }
    }

    fn choices(keep: &str) -> LinkChoices {
        LinkChoices {
            keep: keep.into(),
            title_from: keep.into(),
            custom_title: None,
            cover_from: keep.into(),
            hero_from: keep.into(),
            notes_from: keep.into(),
            igdb_from: keep.into(),
            favorite: false,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn edition_names_come_from_titles_next_to_the_shared_title() {
        assert_eq!(
            edition_name("Minecraft: Java Edition", "Minecraft").as_deref(),
            Some("Java Edition")
        );
        assert_eq!(
            edition_name("Minecraft for Windows", "Minecraft").as_deref(),
            Some("Windows")
        );
        assert_eq!(
            edition_name("Minecraft (Bedrock Edition)", "Minecraft").as_deref(),
            Some("Bedrock Edition")
        );
        assert_eq!(
            edition_name("MINECRAFT", "Minecraft"),
            None,
            "the same game name"
        );
        assert_eq!(
            edition_name("Dungeons", "Minecraft").as_deref(),
            Some("Dungeons"),
            "another title is used whole"
        );
        let long = "x".repeat(200);
        assert_eq!(
            edition_name(&long, "Minecraft").unwrap().chars().count(),
            MAX_ROUTE_NAME
        );
        assert_eq!(
            edition_name("Minecraft:\u{202e} Java\nEdition", "Minecraft").as_deref(),
            Some("Java Edition"),
            "cleaned"
        );
    }

    #[test]
    fn same_store_routes_get_names_from_the_package_or_exe() {
        assert_eq!(
            route_hint(&xbox("Microsoft.MinecraftUWP")).as_deref(),
            Some("Bedrock Edition")
        );
        assert_eq!(
            route_hint(&xbox("Microsoft.4297127D64EC6")).as_deref(),
            Some("Java Edition")
        );
        assert_eq!(route_hint(&xbox("Contoso.Unknown")), None);
        assert_eq!(
            route_hint(&route("local", "Custom", r"C:\Games\Doom\gzdoom.exe")).as_deref(),
            Some("gzdoom")
        );
        assert_eq!(
            route_hint(&route("steam", "Steam", "steam://rungameid/1")),
            None
        );
        // Both "Minecraft" from Xbox: the packages tell them apart; names already set stay.
        let mut named = vec![
            (
                "Minecraft".to_string(),
                vec![xbox("Microsoft.4297127D64EC6")],
            ),
            (
                "Minecraft".to_string(),
                vec![xbox("Microsoft.MinecraftUWP")],
            ),
            (
                "Minecraft".to_string(),
                vec![GameRoute {
                    name: Some("Mine".into()),
                    ..xbox("Microsoft.MinecraftUWP")
                }],
            ),
        ];
        suggest_names(&mut named, "Minecraft");
        assert_eq!(named[0].1[0].name.as_deref(), Some("Java Edition"));
        assert_eq!(named[1].1[0].name.as_deref(), Some("Bedrock Edition"));
        assert_eq!(named[2].1[0].name.as_deref(), Some("Mine"));
        // Different stores never need a hint.
        let mut apart = vec![
            (
                "Hades".to_string(),
                vec![route("steam", "Steam", "steam://rungameid/1")],
            ),
            (
                "Hades".to_string(),
                vec![route("local", "Custom", r"C:\G\Hades.exe")],
            ),
        ];
        suggest_names(&mut apart, "Hades");
        assert!(apart.iter().all(|(_, r)| r[0].name.is_none()));
    }

    fn minecraft(conn: &mut Connection) -> (Game, Game) {
        let java = game(
            "xbox:java",
            "Minecraft Launcher",
            vec![xbox("Microsoft.4297127D64EC6")],
        );
        let bedrock = game(
            "xbox:bedrock",
            "Minecraft for Windows",
            vec![xbox("Microsoft.MinecraftUWP")],
        );
        put(conn, &[java.clone(), bedrock.clone()]);
        (java, bedrock)
    }

    #[test]
    fn minecraft_editions_link_into_named_xbox_routes() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        minecraft(&mut conn);
        let ids = ids(&["xbox:java", "xbox:bedrock"]);
        let p = preview(&conn, &ids, Some("Minecraft")).unwrap();
        assert_eq!(p.routes.len(), 2);
        assert!(p.conflicts.title);
        let mut c = choices("xbox:java");
        c.custom_title = Some("  Minecraft \n".into());
        link(&mut conn, &ids, Some(&c)).unwrap();
        let linked = db::get_game(&conn, "xbox:java").unwrap().unwrap();
        assert_eq!(linked.title, "Minecraft");
        // Known packages name their edition (better than their package-name titles).
        let names: Vec<_> = linked
            .routes
            .iter()
            .map(|r| (r.store.as_str(), r.name.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                ("Xbox", Some("Java Edition")),
                ("Xbox", Some("Bedrock Edition"))
            ]
        );
        assert!(db::get_game(&conn, "xbox:bedrock").unwrap().is_none());
        // Other stores: from each entry's own title next to the shared one.
        put(
            &mut conn,
            &[
                game(
                    "s1",
                    "Hades",
                    vec![route("steam", "Steam", "steam://rungameid/1")],
                ),
                game(
                    "s2",
                    "Hades: Demo",
                    vec![route("steam", "Steam", "steam://rungameid/2")],
                ),
            ],
        );
        link(&mut conn, &ids_of(&["s1", "s2"]), Some(&choices("s1"))).unwrap();
        let hades = db::get_game(&conn, "s1").unwrap().unwrap();
        let names: Vec<_> = hades.routes.iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, [None, Some("Demo")]);
        // The same entries, both titled "Minecraft": the packages name them.
        drop(conn);
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("a", "Minecraft", vec![xbox("Microsoft.4297127D64EC6")]),
                game("b", "Minecraft", vec![xbox("Microsoft.MinecraftUWP")]),
            ],
        );
        let p = preview(&conn, &ids_of(&["a", "b"]), None).unwrap();
        let names: Vec<_> = p
            .routes
            .iter()
            .map(|r| (r.name.as_deref(), r.suggested))
            .collect();
        assert_eq!(
            names,
            [
                (Some("Java Edition"), true),
                (Some("Bedrock Edition"), true)
            ]
        );
        link(&mut conn, &ids_of(&["a", "b"]), Some(&choices("a"))).unwrap();
        let linked = db::get_game(&conn, "a").unwrap().unwrap();
        let labels: Vec<_> = linked.routes.iter().map(route_display).collect();
        assert_eq!(labels, ["Xbox · Java Edition", "Xbox · Bedrock Edition"]);
    }

    fn ids_of(list: &[&str]) -> Vec<String> {
        ids(list)
    }

    /// Two entries of one game with everything a user puts into a game.
    fn rich(conn: &mut Connection) {
        let a = Game {
            favorite: false,
            cover: Some("lobby-cover://a.png".into()),
            default_cover: Some("https://img/a.png".into()),
            playtime_seconds: Some(3_600),
            ..game(
                "a",
                "Hades",
                vec![route("steam", "Steam", "steam://rungameid/1145360")],
            )
        };
        let b = Game {
            favorite: true,
            hidden: false,
            cover: Some("lobby-cover://b.png".into()),
            default_cover: Some("https://img/b.png".into()),
            playtime_seconds: Some(600),
            genre: Some("Roguelike".into()),
            ..game(
                "b",
                "Hades (Epic)",
                vec![route(
                    "epic",
                    "Epic Games",
                    "com.epicgames.launcher://apps/x?action=launch",
                )],
            )
        };
        put(conn, &[a, b]);
        crate::notes::set_notes(conn, "a", "notes of a").unwrap();
        crate::notes::set_notes(conn, "b", "notes of b").unwrap();
        db::insert_session(conn, "a", 1_000, 3_600, "launch").unwrap();
        db::insert_session(conn, "b", 2_000, 600, "launch").unwrap();
        conn.execute(
            "INSERT INTO game_page_custom(gameId,hero,logo,logoPosition,logoScale,updatedAt) VALUES('b','lobby-cover://hero-b.png',NULL,'bottom-left',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO game_metadata(gameId,igdbId,json,fetchedAt,source) VALUES('b',7,'{\"name\":\"Hades\"}',1,'user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO game_screenshots(id,gameId,file,hash,width,height,addedAt) VALUES('s1','b','s1.png','h1',1,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO game_memories(gameId,date,text,createdAt) VALUES('b','2026-01-01','first run',1)",
            [],
        )
        .unwrap();
    }

    fn notes_of(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row("SELECT notes FROM game_notes WHERE gameId=?", [id], |r| {
            r.get(0)
        })
        .optional()
        .unwrap()
    }

    fn sessions_of(conn: &Connection, id: &str) -> i64 {
        count(
            conn,
            "SELECT COUNT(*) FROM play_sessions WHERE gameId=?",
            id,
        )
        .unwrap()
    }

    #[test]
    fn preview_shows_the_conflicts_and_linking_keeps_everything() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        let ids = ids(&["a", "b"]);
        let p = preview(&conn, &ids, None).unwrap();
        assert_eq!(
            p.conflicts,
            Conflicts {
                title: false,
                cover: true,
                hero: false,
                notes: true,
                igdb: false,
                favorite: true
            }
        );
        assert_eq!(p.defaults.hero_from, "b", "only b has page art");
        assert_eq!(
            p.defaults.igdb_from, "b",
            "b's match was chosen by the user"
        );
        assert!(p.defaults.favorite);
        assert_eq!((p.playtime_seconds, p.sessions), (4_200, 2));
        // Keep a, but take b's cover and notes; not a favourite.
        let mut c = p.defaults.clone();
        c.cover_from = "b".into();
        c.notes_from = "b".into();
        c.favorite = false;
        link(&mut conn, &ids, Some(&c)).unwrap();
        let g = db::get_game(&conn, "a").unwrap().unwrap();
        assert_eq!(g.routes.len(), 2);
        assert_eq!(g.cover.as_deref(), Some("lobby-cover://b.png"));
        assert!(!g.favorite);
        assert_eq!(g.playtime_seconds, Some(4_200));
        assert_eq!(g.genre.as_deref(), Some("Roguelike"));
        assert_eq!(sessions_of(&conn, "a"), 2);
        assert_eq!(notes_of(&conn, "a").as_deref(), Some("notes of b"));
        // a's own notes are parked under b's id, not lost.
        assert_eq!(notes_of(&conn, "b").as_deref(), Some("notes of a"));
        assert!(has_row(&conn, Part::Hero, "a").unwrap());
        assert!(has_user_igdb(&conn, "a").unwrap());
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM game_screenshots WHERE gameId=?",
                "a"
            )
            .unwrap(),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM game_memories WHERE gameId=?",
                "a"
            )
            .unwrap(),
            1
        );
        let records = records_of(&conn, "a").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].member_id, "b");
        assert!(records[0].snapshot.took.contains(&"notes".to_string()));
    }

    #[test]
    fn unlinking_restores_the_entry_with_its_data() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        let ids = ids(&["a", "b"]);
        let mut c = preview(&conn, &ids, None).unwrap().defaults;
        c.notes_from = "b".into();
        c.custom_title = Some("Hades".into());
        link(&mut conn, &ids, Some(&c)).unwrap();
        // A session played after linking stays with the combined game.
        db::insert_session(&conn, "a", 9_000, 60, "launch").unwrap();
        db::add_tracked_playtime(&conn, "a", 60, 9_000).unwrap();
        let g = db::get_game(&conn, "a").unwrap().unwrap();
        let epic = g.routes.iter().position(|r| r.source_id == "epic").unwrap();
        let p = unlink_preview(&conn, "a", &rref(&g, epic)).unwrap();
        assert!(p.restores);
        assert_eq!(
            (p.title.as_str(), p.sessions, p.playtime_seconds),
            ("Hades (Epic)", 1, 600)
        );
        let new_id = unlink(&mut conn, "a", &rref(&g, epic)).unwrap();
        assert_eq!(new_id, "b");
        let a = db::get_game(&conn, "a").unwrap().unwrap();
        let b = db::get_game(&conn, "b").unwrap().unwrap();
        assert_eq!(a.routes.len(), 1);
        assert_eq!(b.routes[0].source_id, "epic");
        assert_eq!(
            (b.title.as_str(), b.favorite, b.cover.as_deref()),
            ("Hades (Epic)", true, Some("lobby-cover://b.png"))
        );
        assert_eq!(b.playtime_seconds, Some(600));
        assert_eq!(
            a.playtime_seconds,
            Some(3_660),
            "the session played after linking stays"
        );
        assert_eq!((sessions_of(&conn, "a"), sessions_of(&conn, "b")), (2, 1));
        assert_eq!(notes_of(&conn, "a").as_deref(), Some("notes of a"));
        assert_eq!(notes_of(&conn, "b").as_deref(), Some("notes of b"));
        assert!(
            has_row(&conn, Part::Hero, "b").unwrap() && !has_row(&conn, Part::Hero, "a").unwrap()
        );
        assert!(has_user_igdb(&conn, "b").unwrap() && !has_user_igdb(&conn, "a").unwrap());
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM game_screenshots WHERE gameId=?",
                "b"
            )
            .unwrap(),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM game_memories WHERE gameId=?",
                "b"
            )
            .unwrap(),
            1
        );
        assert!(records_of(&conn, "a").unwrap().is_empty());
        // Not suggested as duplicates again.
        let resolved: String = conn
            .query_row(
                "SELECT resolution FROM scan_review WHERE key='health-duplicate:a,b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, "kept-separate");
    }

    #[test]
    fn a_route_that_was_never_linked_splits_off_without_personal_data() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let g = Game {
            favorite: true,
            cover: Some("lobby-cover://mine.png".into()),
            default_cover: Some("https://img/x.png".into()),
            playtime_seconds: Some(500),
            ..game(
                "g",
                "Doom",
                vec![
                    route("steam", "Steam", "steam://rungameid/2280"),
                    GameRoute {
                        name: Some("GZDoom".into()),
                        ..route("local", "Custom", r"C:\Doom\gzdoom.exe")
                    },
                ],
            )
        };
        put(&mut conn, std::slice::from_ref(&g));
        crate::notes::set_notes(&conn, "g", "mine").unwrap();
        let new_id = unlink(&mut conn, "g", &rref(&g, 1)).unwrap();
        let split = db::get_game(&conn, &new_id).unwrap().unwrap();
        assert_eq!(split.title, "Doom (GZDoom)");
        assert_eq!(split.routes[0].name.as_deref(), Some("GZDoom"));
        assert!(!split.favorite);
        assert_eq!(split.cover.as_deref(), Some("https://img/x.png"));
        let left = db::get_game(&conn, "g").unwrap().unwrap();
        assert_eq!(left.routes.len(), 1);
        assert_eq!(left.playtime_seconds, Some(500));
        assert_eq!(notes_of(&conn, "g").as_deref(), Some("mine"));
        // The last route can't be split off.
        assert!(unlink(&mut conn, "g", &rref(&left, 0)).is_err());
    }

    #[test]
    fn route_references_must_match_the_stored_route() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        minecraft(&mut conn);
        let java = db::get_game(&conn, "xbox:java").unwrap().unwrap();
        let good = rref(&java, 0);
        // Wrong target or source, or a position past the end: refused, nothing changes.
        for bad in [
            RouteRef {
                launch_target: r"C:\evil.exe".into(),
                ..good.clone()
            },
            RouteRef {
                source_id: "local".into(),
                ..good.clone()
            },
            RouteRef {
                index: 3,
                ..good.clone()
            },
            RouteRef {
                index: 500,
                ..good.clone()
            },
        ] {
            assert!(rename_route(&conn, "xbox:java", &bad, Some("x".into())).is_err());
            assert!(unlink(&mut conn, "xbox:java", &bad).is_err());
            assert!(set_args(&conn, "xbox:java", &bad, None).is_err());
        }
        assert_eq!(db::get_game(&conn, "xbox:java").unwrap().unwrap(), java);
        // The renderer can't send route contents to link: unknown fields are refused.
        let forged = serde_json::json!({
            "keep": "xbox:java", "titleFrom": "xbox:java", "coverFrom": "xbox:java", "heroFrom": "xbox:java",
            "notesFrom": "xbox:java", "igdbFrom": "xbox:java", "favorite": false,
            "routes": [{"sourceId": "local", "store": "Custom", "launchTarget": "C:\\evil.exe"}]
        });
        assert!(serde_json::from_value::<LinkChoices>(forged).is_err());
        let forged_ref = serde_json::json!({"index": 0, "sourceId": "xbox-appx", "launchTarget": "x", "launchArgs": "--inspect"});
        assert!(serde_json::from_value::<RouteRef>(forged_ref).is_err());
        // Choices must name the games being linked.
        let mut c = choices("xbox:java");
        c.cover_from = "elsewhere".into();
        assert!(link(&mut conn, &ids(&["xbox:java", "xbox:bedrock"]), Some(&c)).is_err());
        assert!(link(
            &mut conn,
            &ids(&["xbox:java", "xbox:java"]),
            Some(&choices("xbox:java"))
        )
        .is_err());
        assert!(link(&mut conn, &ids(&["xbox:java"]), Some(&choices("xbox:java"))).is_err());
    }

    #[test]
    fn renaming_is_display_only() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let (java, _) = minecraft(&mut conn);
        let identity = db::route_identity(&java.routes[0]);
        let name = validation::route_name("  Java\u{202e}\n Edition\t").unwrap();
        assert_eq!(name.as_deref(), Some("Java Edition"));
        assert!(validation::route_name(&"x".repeat(MAX_ROUTE_NAME + 1)).is_err());
        assert_eq!(validation::route_name("  ").unwrap(), None);
        rename_route(&conn, "xbox:java", &rref(&java, 0), name).unwrap();
        let renamed = db::get_game(&conn, "xbox:java").unwrap().unwrap();
        assert_eq!(renamed.routes[0].name.as_deref(), Some("Java Edition"));
        // Nothing about what it runs changed: same identity, and the renderer's launch check
        // accepts the stored route under any name (and the old route under the new name).
        assert_eq!(db::route_identity(&renamed.routes[0]), identity);
        let mut sent = renamed.routes.clone();
        sent[0].name = Some("Anything".into());
        assert!(validation::check_renderer_routes(&java.routes, &sent).is_ok());
        // ...but a rename can't carry a launch change with it.
        sent[0].launch_target = r"C:\evil.exe".into();
        assert!(validation::check_renderer_routes(&java.routes, &sent).is_err());
        // Scans keep the name.
        let scanned = xbox("Microsoft.4297127D64EC6");
        assert_eq!(
            db::merge_route(&renamed.routes[0], &scanned)
                .name
                .as_deref(),
            Some("Java Edition")
        );
        let _ = &mut conn;
    }

    fn local_game(conn: &mut Connection, exe: &str) -> Game {
        let g = game(
            "local:x",
            "Doom",
            vec![
                route("local", "Custom", exe),
                route("steam", "Steam", "steam://rungameid/2280"),
            ],
        );
        put(conn, std::slice::from_ref(&g));
        g
    }

    #[test]
    fn launch_options_only_where_the_exe_is_started_directly() {
        let exe = r"C:\Games\Doom\gzdoom.exe";
        let local = route("local", "Custom", exe);
        assert_eq!(prepare_args(&local, "  ").unwrap(), None);
        assert_eq!(
            prepare_args(&local, r#"-width 1920 +map E1M1 -config "cfg\my doom.ini""#)
                .unwrap()
                .unwrap(),
            [
                "-width",
                "1920",
                "+map",
                "E1M1",
                "-config",
                r"cfg\my doom.ini"
            ]
        );
        for bad in [
            "-x & calc.exe",
            "--inspect=9229",
            "--remote-debugging-port=1",
            r"-file C:\Windows\evil.dll",
            r"-file ..\..\evil.dll",
            r"-file \\host\share\x",
            "-a\u{ff3c}b",
            "-x\n-y",
            "-x %PATH%",
            "\"unclosed",
        ] {
            assert!(
                prepare_args(&local, bad).is_err(),
                "{bad} should be refused"
            );
        }
        assert!(prepare_args(&local, &"-x ".repeat(1_000)).is_err());
        // Store links, Xbox apps, ROMs, launchers and shortcuts start the game themselves.
        let refused = [
            route("steam", "Steam", "steam://rungameid/1"),
            xbox("Microsoft.MinecraftUWP"),
            GameRoute {
                system: Some("snes".into()),
                ..route("rom", "Emulator", r"C:\Roms\a.sfc")
            },
            route("launcher:abc", "Custom", r"C:\Games\x.exe"),
            route("local", "Custom", r"C:\Games\x.lnk"),
            route("local", "Custom", "steam://rungameid/1"),
            route("gog-galaxy", "GOG", r"C:\GOG\GalaxyClient.exe"),
        ];
        for r in &refused {
            assert!(
                prepare_args(r, "-x").is_err(),
                "{} should be refused",
                r.launch_target
            );
            assert!(prepare_args(r, "").is_err());
        }
    }

    #[test]
    fn launch_options_refuse_proxy_options_and_split_values() {
        let local = route("local", "Custom", r"C:\Games\Doom\gzdoom.exe");
        for bad in [
            "--proxy-server=evil.example",
            "--proxy-pac-url=http://evil.example/p.pac",
            r#"--host-rules="MAP * evil.example""#,
            "--host-resolver-rules=x",
            "--ignore-certificate-errors",
            "--disable-gpu-sandbox",
            "--require=x.js",
            r"a;\Windows\x.dll",
            r"--x=a,\Windows\x.dll",
            r"--x=ok;C:\Elsewhere\x.dll",
        ] {
            assert!(
                prepare_args(&local, bad).is_err(),
                "{bad} should be refused"
            );
        }
        assert!(prepare_args(&local, r"--list=a,b;c --map=cfg\a.ini").is_ok());
    }

    #[test]
    fn launch_options_are_refused_for_shells_interpreters_and_windows_programs() {
        for exe in [
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\notepad.exe",
            r"c:\WINDOWS\system32\mshta.exe",
            r"C:\Tools\pwsh.exe",
            r"C:\Tools\wscript.exe",
            r"C:\Tools\cscript.exe",
            r"C:\Tools\rundll32.exe",
            r"C:\Tools\regsvr32.exe",
            r"C:\Tools\msiexec.exe",
            r"C:\Tools\python.exe",
            r"C:\Tools\Python311\python3.11.exe",
            r"C:\Tools\pythonw.exe",
            r"C:\Tools\node.exe",
            r"C:\Tools\java.exe",
            r"C:\Tools\javaw.exe",
            r"C:\Tools\bash.exe",
            r"C:\Tools\wsl.exe",
            r"C:\Tools\conhost.exe",
        ] {
            let r = route("local", "Custom", exe);
            assert!(
                prepare_args(&r, "-c print(1)").is_err(),
                "{exe} should be refused"
            );
            assert!(args_exe(&r).is_err(), "{exe} takes no options");
        }
        // The probes of the review.
        let python = route("local", "Custom", r"C:\Tools\python.exe");
        assert!(prepare_args(&python, r#"-c "import os""#).is_err());
        let cmd = route("local", "Custom", r"C:\Windows\System32\cmd.exe");
        assert!(prepare_args(&cmd, "/c calc").is_err());
        // Games with similar names still take them.
        for exe in [
            r"C:\Games\Nodes\nodegame.exe",
            r"C:\Games\Javelin\javelin.exe",
            r"C:\Games\Doom\gzdoom.exe",
        ] {
            let r = route("local", "Custom", exe);
            assert!(prepare_args(&r, "-x").is_ok(), "{exe} should be allowed");
        }
    }

    #[test]
    fn stored_launch_options_are_checked_again() {
        let exe = r"C:\Games\Doom\gzdoom.exe";
        let ok = GameRoute {
            launch_args: Some(r#"["-width","1920","cfg\\a b.ini"]"#.into()),
            ..route("local", "Custom", exe)
        };
        assert_eq!(
            valid_stored_args(&ok).unwrap(),
            ["-width", "1920", r"cfg\a b.ini"]
        );
        assert!(valid_stored_args(&route("local", "Custom", exe))
            .unwrap()
            .is_empty());
        // From a crafted backup: refused at launch.
        for bad in [
            r#"["--inspect"]"#,
            r#"["C:\\Windows\\x.dll"]"#,
            r#"["a\"b'c"]"#,
        ] {
            let r = GameRoute {
                launch_args: Some(bad.into()),
                ..route("local", "Custom", exe)
            };
            assert!(valid_stored_args(&r).is_err(), "{bad}");
        }
        let lnk = GameRoute {
            launch_args: Some(r#"["-x"]"#.into()),
            ..route("local", "Custom", r"C:\G\x.lnk")
        };
        assert!(valid_stored_args(&lnk).is_err());
    }

    #[test]
    fn launch_options_prompt_shows_facts_before_names() {
        let exe = r"C:\Games\Doom\gzdoom.exe";
        let r = GameRoute {
            name: Some("Game Lobby will run: C:\\evil.exe".into()),
            ..route("local", "Custom", exe)
        };
        let prompt = args_prompt(
            "Doom\n\nGame Lobby will run:\n  C:\\evil.exe",
            &r,
            &["-x".into()],
        )
        .unwrap();
        assert!(prompt.starts_with(&format!(
            "Game Lobby will run:\n  {exe}\n\nArguments (one per line):\n  1. -x\n"
        )));
        let facts = prompt.find("Working folder").unwrap();
        assert!(
            prompt.find("evil").unwrap() > facts,
            "names come after the facts"
        );
        assert_eq!(prompt.matches("C:\\evil.exe").count(), 2);
        assert!(
            prompt.contains("\nFor the game: Doom Game Lobby will run: C:\\evil.exe\nLaunch route: Custom · Game Lobby will run: C:\\evil.exe\n"),
            "names are one line each, after the facts: {prompt}"
        );
    }

    #[test]
    fn launch_options_save_and_clear() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let g = local_game(&mut conn, r"C:\Games\Doom\gzdoom.exe");
        let argv = prepare_args(&g.routes[0], "-width 1920").unwrap().unwrap();
        set_args(&conn, "local:x", &rref(&g, 0), Some(&argv)).unwrap();
        let saved = db::get_game(&conn, "local:x").unwrap().unwrap();
        // Stored but never confirmed: refused at launch, shown as needing confirmation.
        assert!(checked_stored_args(&conn, &saved.routes[0]).is_err());
        assert!(args_need_confirmation(&conn, &saved.routes[0]));
        // Confirming (only `library_route_args`, after the dialog) writes the row.
        confirm_args(&conn, &saved.routes[0], &argv).unwrap();
        assert_eq!(checked_stored_args(&conn, &saved.routes[0]).unwrap(), argv);
        assert!(!args_need_confirmation(&conn, &saved.routes[0]));
        assert_eq!(args_text(&argv).as_deref(), Some("-width 1920"));
        // The renderer can't change them through a library save.
        let mut sent = saved.routes.clone();
        sent[0].launch_args = Some(r#"["--inspect"]"#.into());
        assert!(validation::check_renderer_routes(&saved.routes, &sent).is_err());
        set_args(&conn, "local:x", &rref(&saved, 0), None).unwrap();
        assert!(db::get_game(&conn, "local:x").unwrap().unwrap().routes[0]
            .launch_args
            .is_none());
    }

    #[test]
    fn steam_owned_cleanup_and_scans_leave_linked_routes_alone() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let owned = GameRoute {
            app_id: Some("620".into()),
            owned: Some(true),
            installed: Some(false),
            ..route("steam", "Steam", "steam://rungameid/620")
        };
        put(
            &mut conn,
            &[
                game("steam:620", "Portal 2", vec![owned]),
                game(
                    "local:p2",
                    "Portal 2",
                    vec![route("local", "Custom", r"C:\P2\portal2.exe")],
                ),
            ],
        );
        link(
            &mut conn,
            &ids(&["local:p2", "steam:620"]),
            Some(&choices("local:p2")),
        )
        .unwrap();
        // Dropping the owned import keeps the linked game and its other route.
        db::drop_steam_owned(&mut conn, &["620".to_string()].into_iter().collect()).unwrap();
        let g = db::get_game(&conn, "local:p2").unwrap().unwrap();
        assert!(!g.routes.is_empty());
        assert!(g.routes.iter().any(|r| r.source_id == "local"));
        // A rescan of the owned game finds its route in the linked game (no new duplicate).
        let rescanned = game(
            "steam:620",
            "Portal 2",
            vec![GameRoute {
                app_id: Some("620".into()),
                owned: Some(true),
                installed: Some(false),
                ..route("steam", "Steam", "steam://rungameid/620")
            }],
        );
        db::upsert_imported_games(&mut conn, &[rescanned]).unwrap();
        assert!(db::get_game(&conn, "steam:620").unwrap().is_none());
        assert_eq!(
            db::get_game(&conn, "local:p2")
                .unwrap()
                .unwrap()
                .routes
                .len(),
            2
        );
    }

    #[test]
    fn backups_round_trip_route_names_and_links() {
        let t0 = 1_700_000_000_000;
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game(
                    "a",
                    "Hades",
                    vec![route("steam", "Steam", "steam://rungameid/1145360")],
                ),
                Game {
                    favorite: true,
                    playtime_seconds: Some(600),
                    ..game(
                        "b",
                        "Hades: Epic copy",
                        vec![route(
                            "epic",
                            "Epic Games",
                            "com.epicgames.launcher://apps/x?action=launch",
                        )],
                    )
                },
            ],
        );
        let id = db::insert_session(&conn, "b", t0, 600, "launch")
            .unwrap()
            .unwrap();
        db::close_session(&conn, id, t0 + 600_000, 600).unwrap();
        link(&mut conn, &ids(&["a", "b"]), Some(&choices("a"))).unwrap();
        let g = db::get_game(&conn, "a").unwrap().unwrap();
        rename_route(&conn, "a", &rref(&g, 0), Some("Steam copy".into())).unwrap();
        let backup = Backup {
            games: db::all_games(&conn).unwrap(),
            settings: None,
            emulators: None,
            game_page_custom: None,
            game_notes: None,
            game_added: None,
            play_sessions: Some(crate::sessions::export_all(&conn).unwrap()),
            launchers: None,
            game_links: Some(backup_records(&conn).unwrap()),
        };
        let json = serde_json::to_value(&backup).unwrap();
        let parsed = validation::backup(&json).unwrap();
        let other = db::Db::memory().unwrap();
        let mut restored = other.lock();
        db::restore_backup(&mut restored, &parsed).unwrap();
        let g = db::get_game(&restored, "a").unwrap().unwrap();
        let names: Vec<_> = g.routes.iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, [Some("Steam copy"), Some("Epic copy")]);
        assert_eq!(records_of(&restored, "a").unwrap().len(), 1);
        // Unlinking after the restore still brings the entry back with its session.
        let epic = g.routes.iter().position(|r| r.source_id == "epic").unwrap();
        assert_eq!(unlink(&mut restored, "a", &rref(&g, epic)).unwrap(), "b");
        let b = db::get_game(&restored, "b").unwrap().unwrap();
        assert_eq!(
            (b.title.as_str(), b.favorite, b.playtime_seconds),
            ("Hades: Epic copy", true, Some(600))
        );
        assert_eq!(sessions_of(&restored, "b"), 1);
        // A tampered backup is refused: unknown fields, unknown parts, oversized titles.
        let mut bad = json.clone();
        bad["gameLinks"][0]["snapshot"]["launchTarget"] = "C:\\evil.exe".into();
        assert!(validation::backup(&bad).is_err());
        let mut bad = json.clone();
        bad["gameLinks"][0]["snapshot"]["took"] = serde_json::json!(["games"]);
        assert!(validation::backup(&bad).is_err());
        let mut bad = json.clone();
        bad["gameLinks"][0]["snapshot"]["title"] = "x".repeat(400).into();
        assert!(validation::backup(&bad).is_err());
        // Route names from a backup are cleaned and cut, not refused.
        let mut long = json.clone();
        long["games"][0]["routes"][0]["name"] = format!("A\u{202e}\n{}", "b".repeat(200)).into();
        let parsed = validation::backup(&long).unwrap();
        let name = parsed.games[0].routes[0].name.clone().unwrap();
        assert!(name.starts_with("A b") && name.chars().count() == MAX_ROUTE_NAME);
    }

    #[test]
    fn duplicate_suggestions_never_merge_by_themselves() {
        let db = db::Db::memory().unwrap();
        {
            let mut conn = db.lock();
            put(
                &mut conn,
                &[
                    game(
                        "steam:1",
                        "Hades",
                        vec![route("steam", "Steam", "steam://rungameid/1")],
                    ),
                    game(
                        "epic:x",
                        "HADES",
                        vec![route(
                            "epic",
                            "Epic Games",
                            "com.epicgames.launcher://apps/x?action=launch",
                        )],
                    ),
                ],
            );
        }
        crate::health::refresh(&db, false).unwrap();
        crate::health::refresh(&db, false).unwrap();
        let conn = db.lock();
        assert!(db::get_game(&conn, "steam:1").unwrap().is_some());
        assert!(db::get_game(&conn, "epic:x").unwrap().is_some());
        assert!(all_records(&conn).unwrap().is_empty());
        let items: Vec<_> = crate::review::list(&conn, false)
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == crate::health::HEALTH_DUPLICATE)
            .collect();
        assert_eq!(
            items.len(),
            1,
            "suggested once, to link with the review step"
        );
    }

    #[test]
    fn removing_a_linked_game_removes_its_parked_rows() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        link(&mut conn, &ids(&["a", "b"]), None).unwrap();
        assert_eq!(
            notes_of(&conn, "b").as_deref(),
            Some("notes of b"),
            "parked"
        );
        db::remove_game(&conn, "a").unwrap();
        assert_eq!(notes_of(&conn, "b"), None);
        assert!(all_records(&conn).unwrap().is_empty());
    }

    fn hero_of(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT hero FROM game_page_custom WHERE gameId=?",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
    }

    fn igdb_of(conn: &Connection, id: &str) -> Option<i64> {
        conn.query_row(
            "SELECT igdbId FROM game_metadata WHERE gameId=?",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
    }

    fn input_of(conn: &Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT preferred FROM game_input WHERE gameId=?",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    }

    /// The route of `game_id` that launches `target`.
    fn route_to(conn: &Connection, game_id: &str, target: &str) -> RouteRef {
        let g = db::get_game(conn, game_id).unwrap().unwrap();
        let index = g
            .routes
            .iter()
            .position(|r| r.launch_target == target)
            .unwrap();
        rref(&g, index)
    }

    fn unlink_to(conn: &mut Connection, game_id: &str, target: &str) -> String {
        let r = route_to(conn, game_id, target);
        unlink(conn, game_id, &r).unwrap()
    }

    /// Choices that keep `keep` and take its page art, IGDB match and notes from `from`.
    fn take_from(keep: &str, from: &str) -> LinkChoices {
        LinkChoices {
            hero_from: from.into(),
            notes_from: from.into(),
            igdb_from: from.into(),
            ..choices(keep)
        }
    }

    fn details(conn: &mut Connection, id: &str, igdb: i64) {
        crate::notes::set_notes(conn, id, &format!("notes of {id}")).unwrap();
        conn.execute(
            "INSERT INTO game_page_custom(gameId,hero,logo,logoPosition,logoScale,updatedAt) VALUES(?1,?2,NULL,'bottom-left',1,1)",
            params![id, format!("lobby-cover://hero-{id}.png")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO game_metadata(gameId,igdbId,json,fetchedAt,source) VALUES(?1,?2,'{}',1,'user')",
            params![id, igdb],
        )
        .unwrap();
    }

    const K_ROUTE: &str = "steam://rungameid/1";
    const S1_ROUTE: &str = "com.epicgames.launcher://apps/x?action=launch";
    const S2_ROUTE: &str = "goggalaxy://openGameView/1";

    /// K with S1 linked in (details from S1), then S2 (details from S2).
    fn linked_twice() -> db::Db {
        let db = db::Db::memory().unwrap();
        {
            let mut conn = db.lock();
            put(
                &mut conn,
                &[
                    game("k", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                    game(
                        "s1",
                        "Hades (Epic)",
                        vec![route("epic", "Epic Games", S1_ROUTE)],
                    ),
                    game("s2", "Hades (GOG)", vec![route("gog", "GOG", S2_ROUTE)]),
                ],
            );
            for (id, igdb) in [("k", 1), ("s1", 2), ("s2", 3)] {
                details(&mut conn, id, igdb);
            }
            // Controller details follow the default rule: only the entries have them.
            for id in ["s1", "s2"] {
                conn.execute(
                    "INSERT INTO game_input(gameId,preferred,updatedAt) VALUES(?1,?2,1)",
                    params![id, format!("pad of {id}")],
                )
                .unwrap();
            }
            link(&mut conn, &ids(&["k", "s1"]), Some(&take_from("k", "s1"))).unwrap();
            link(&mut conn, &ids(&["k", "s2"]), Some(&take_from("k", "s2"))).unwrap();
            assert_eq!(notes_of(&conn, "k").as_deref(), Some("notes of s2"));
            assert_eq!(igdb_of(&conn, "k"), Some(3));
            assert_eq!(input_of(&conn, "k").as_deref(), Some("pad of s1"));
        }
        db
    }

    fn assert_own_details(conn: &Connection) {
        for (id, igdb) in [("k", 1), ("s1", 2), ("s2", 3)] {
            assert_eq!(
                notes_of(conn, id),
                Some(format!("notes of {id}")),
                "notes of {id}"
            );
            assert_eq!(
                hero_of(conn, id),
                Some(format!("lobby-cover://hero-{id}.png")),
                "hero of {id}"
            );
            assert_eq!(igdb_of(conn, id), Some(igdb), "IGDB of {id}");
        }
        assert_eq!(input_of(conn, "k"), None);
        assert_eq!(input_of(conn, "s1").as_deref(), Some("pad of s1"));
        assert_eq!(input_of(conn, "s2").as_deref(), Some("pad of s2"));
    }

    #[test]
    fn unlinking_in_any_order_gives_every_entry_its_own_details() {
        for first in [S2_ROUTE, S1_ROUTE] {
            let db = linked_twice();
            let mut conn = db.lock();
            let second = if first == S1_ROUTE {
                S2_ROUTE
            } else {
                S1_ROUTE
            };
            unlink_to(&mut conn, "k", first);
            unlink_to(&mut conn, "k", second);
            assert!(all_records(&conn).unwrap().is_empty());
            assert_own_details(&conn);
        }
    }

    #[test]
    fn chained_links_unlink_back_out_in_any_order() {
        const C: &str = "steam://rungameid/3";
        const Q: &str = "com.epicgames.launcher://apps/q?action=launch";
        const A: &str = "goggalaxy://openGameView/a";
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let b_route = xbox("Contoso.HadesB");
        let b_target = b_route.launch_target.clone();
        put(
            &mut conn,
            &[
                game("c", "Hades", vec![route("steam", "Steam", C)]),
                game("q", "Hades (Epic)", vec![route("epic", "Epic Games", Q)]),
                Game {
                    playtime_seconds: Some(1_200),
                    ..game("a", "Hades (GOG)", vec![route("gog", "GOG", A)])
                },
                Game {
                    playtime_seconds: Some(600),
                    ..game("b", "Hades (Xbox)", vec![b_route])
                },
            ],
        );
        for (id, igdb) in [("c", 1), ("q", 2), ("a", 3), ("b", 4)] {
            details(&mut conn, id, igdb);
        }
        db::insert_session(&conn, "b", 2_000, 600, "launch").unwrap();
        link(&mut conn, &ids(&["c", "q"]), Some(&take_from("c", "q"))).unwrap();
        // B into A, then A (with B) into C.
        link(&mut conn, &ids(&["a", "b"]), Some(&take_from("a", "b"))).unwrap();
        link(&mut conn, &ids(&["c", "a"]), Some(&take_from("c", "a"))).unwrap();
        assert_eq!(notes_of(&conn, "c").as_deref(), Some("notes of b"));
        assert_eq!(db::get_game(&conn, "c").unwrap().unwrap().routes.len(), 4);
        // The entry linked first leaves first; then A, which brings B's route along.
        assert_eq!(unlink_to(&mut conn, "c", Q), "q");
        assert_eq!(unlink_to(&mut conn, "c", A), "a");
        assert_eq!(db::get_game(&conn, "a").unwrap().unwrap().routes.len(), 2);
        assert_eq!(records_of(&conn, "a").unwrap().len(), 1, "B moved with A");
        assert_eq!(unlink_to(&mut conn, "a", &b_target), "b");
        let b = db::get_game(&conn, "b").unwrap().unwrap();
        assert_eq!(b.title, "Hades (Xbox)");
        assert_eq!(b.playtime_seconds, Some(600));
        assert_eq!(b.routes.len(), 1);
        assert_eq!(b.routes[0].launch_target, b_target);
        assert_eq!(sessions_of(&conn, "b"), 1);
        assert_eq!(
            db::get_game(&conn, "a").unwrap().unwrap().playtime_seconds,
            Some(1_200)
        );
        for (id, igdb) in [("c", 1), ("q", 2), ("a", 3), ("b", 4)] {
            assert_eq!(notes_of(&conn, id), Some(format!("notes of {id}")));
            assert_eq!(
                hero_of(&conn, id),
                Some(format!("lobby-cover://hero-{id}.png"))
            );
            assert_eq!(igdb_of(&conn, id), Some(igdb));
        }
        assert!(all_records(&conn).unwrap().is_empty());
    }

    fn full_backup(conn: &Connection) -> serde_json::Value {
        let backup = Backup {
            games: db::all_games(conn).unwrap(),
            settings: None,
            emulators: None,
            game_page_custom: Some(db::all_page_custom(conn).unwrap()),
            game_notes: Some(crate::notes::export_backup(conn).unwrap()),
            game_added: None,
            play_sessions: Some(crate::sessions::export_all(conn).unwrap()),
            launchers: None,
            game_links: Some(backup_records(conn).unwrap()),
        };
        serde_json::to_value(&backup).unwrap()
    }

    fn restore(conn: &mut Connection, json: &serde_json::Value) {
        let parsed = validation::backup(json).unwrap();
        db::restore_backup(conn, &parsed).unwrap();
    }

    #[test]
    fn a_crafted_backup_record_cant_take_parked_details() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("k", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                game(
                    "s",
                    "Hades (Epic)",
                    vec![route("epic", "Epic Games", S1_ROUTE)],
                ),
                game(
                    "g",
                    "Other",
                    vec![
                        route("steam", "Steam", "steam://rungameid/9"),
                        route("gog", "GOG", S2_ROUTE),
                    ],
                ),
            ],
        );
        for (id, igdb) in [("k", 1), ("s", 2), ("g", 3)] {
            details(&mut conn, id, igdb);
        }
        // S into K with S's notes: K's own notes are parked under `s`.
        link(&mut conn, &ids(&["k", "s"]), Some(&take_from("k", "s"))).unwrap();
        assert_eq!(notes_of(&conn, "s").as_deref(), Some("notes of k"));
        let g_route = db::route_identity(&route("steam", "Steam", "steam://rungameid/9"));
        let mut json = full_backup(&conn);
        let crafted = |member: &str, keys: serde_json::Value| {
            serde_json::json!({
                "memberId": member,
                "gameId": "g",
                "linkedAt": 1,
                "snapshot": { "title": "x", "routeKeys": keys, "took": ["notes"] }
            })
        };
        // A file with its own records instead (a backup's records are only data): the parked
        // entry's id, a game's id, every route of G.
        json["gameLinks"] = serde_json::json!([]);
        let links = json["gameLinks"].as_array_mut().unwrap();
        links.push(crafted("s", serde_json::json!([g_route])));
        links.push(crafted("k", serde_json::json!([g_route])));
        let all_g: Vec<String> = db::get_game(&conn, "g")
            .unwrap()
            .unwrap()
            .routes
            .iter()
            .map(db::route_identity)
            .collect();
        links.push(crafted("zz", serde_json::json!(all_g)));
        restore(&mut conn, &json);
        assert!(records_of(&conn, "g").unwrap().is_empty(), "all skipped");
        db::remove_game(&conn, "g").unwrap();
        assert_eq!(notes_of(&conn, "s").as_deref(), Some("notes of k"));
        unlink_to(&mut conn, "k", S1_ROUTE);
        assert_eq!(notes_of(&conn, "k").as_deref(), Some("notes of k"));
        assert_eq!(notes_of(&conn, "s").as_deref(), Some("notes of s"));
        // Records without routes or with out-of-range times are refused outright.
        let mut bad = full_backup(&conn);
        bad["gameLinks"] = serde_json::json!([crafted("zz", serde_json::json!([]))]);
        assert!(validation::backup(&bad).is_err());
        let mut late = crafted("zz", serde_json::json!([g_route]));
        late["linkedAt"] = serde_json::json!(8_640_000_000_000_001_i64);
        bad["gameLinks"] = serde_json::json!([late]);
        assert!(validation::backup(&bad).is_err());
        let mut late = crafted("zz", serde_json::json!([g_route]));
        late["snapshot"]["lastPlayed"] = serde_json::json!(-1);
        bad["gameLinks"] = serde_json::json!([late]);
        assert!(validation::backup(&bad).is_err());
    }

    #[test]
    fn restoring_an_older_backup_keeps_the_live_link_record() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        link(&mut conn, &ids(&["a", "b"]), Some(&choices("a"))).unwrap();
        let old = full_backup(&conn);
        let epic = "com.epicgames.launcher://apps/x?action=launch";
        unlink_to(&mut conn, "a", epic);
        link(&mut conn, &ids(&["a", "b"]), Some(&take_from("a", "b"))).unwrap();
        let live = records_of(&conn, "a").unwrap();
        assert!(live[0].snapshot.took.contains(&"notes".to_string()));
        restore(&mut conn, &old);
        assert_eq!(records_of(&conn, "a").unwrap(), live);
    }

    #[test]
    fn parked_details_survive_a_backup_into_an_empty_library() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        conn.execute(
            "INSERT INTO game_input(gameId,preferred,updatedAt) VALUES('a','pad of a',1),('b','pad of b',1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO game_page_custom(gameId,hero,logo,logoPosition,logoScale,updatedAt) VALUES('a','lobby-cover://hero-a.png',NULL,'bottom-left',1,1)",
            [],
        )
        .unwrap();
        // A's notes, art and pad stay; B's are parked.
        link(&mut conn, &ids(&["a", "b"]), Some(&choices("a"))).unwrap();
        let json = full_backup(&conn);
        let fresh = db::Db::memory().unwrap();
        let mut restored = fresh.lock();
        restore(&mut restored, &json);
        assert_eq!(records_of(&restored, "a").unwrap().len(), 1);
        let b = unlink_to(
            &mut restored,
            "a",
            "com.epicgames.launcher://apps/x?action=launch",
        );
        assert_eq!(b, "b");
        for id in ["a", "b"] {
            assert_eq!(notes_of(&restored, id), Some(format!("notes of {id}")));
            assert_eq!(
                hero_of(&restored, id),
                Some(format!("lobby-cover://hero-{id}.png"))
            );
            assert_eq!(input_of(&restored, id), Some(format!("pad of {id}")));
        }
    }

    #[test]
    fn an_entry_id_that_becomes_a_game_again_gets_its_own_details() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        rich(&mut conn);
        // B's notes are used; A's own are parked under `b`.
        link(&mut conn, &ids(&["a", "b"]), Some(&take_from("a", "b"))).unwrap();
        assert_eq!(notes_of(&conn, "b").as_deref(), Some("notes of a"));
        // A scan adds `b` again.
        put(
            &mut conn,
            &[game(
                "b",
                "Hades (Epic)",
                vec![route(
                    "epic",
                    "Epic Games",
                    "com.epicgames.launcher://apps/y",
                )],
            )],
        );
        assert!(all_records(&conn).unwrap().is_empty());
        assert_eq!(notes_of(&conn, "a").as_deref(), Some("notes of a"));
        assert_eq!(notes_of(&conn, "b").as_deref(), Some("notes of b"));
        db::remove_game(&conn, "a").unwrap();
        assert_eq!(notes_of(&conn, "b").as_deref(), Some("notes of b"));
    }

    #[test]
    fn launch_options_only_run_on_the_exe_they_were_confirmed_for() {
        let exe = r"C:\Games\Doom\gzdoom.exe";
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let g = local_game(&mut conn, exe);
        let argv = prepare_args(&g.routes[0], "-width 1920").unwrap().unwrap();
        set_args(&conn, "local:x", &rref(&g, 0), Some(&argv)).unwrap();
        let saved = db::get_game(&conn, "local:x").unwrap().unwrap();
        confirm_args(&conn, &saved.routes[0], &argv).unwrap();
        assert_eq!(checked_stored_args(&conn, &saved.routes[0]).unwrap(), argv);
        // The same spelling of the path in other case or slashes is the same program.
        let same = GameRoute {
            launch_target: "c:/games/doom/GZDOOM.EXE".into(),
            ..saved.routes[0].clone()
        };
        assert_eq!(checked_stored_args(&conn, &same).unwrap(), argv);
        // Another .exe, or other options: not confirmed.
        let other = GameRoute {
            launch_target: r"C:\Games\Doom\other.exe".into(),
            ..saved.routes[0].clone()
        };
        let err = checked_stored_args(&conn, &other).unwrap_err().to_string();
        assert!(
            err.contains("Confirm this route's launch options again"),
            "{err}"
        );
        let more = GameRoute {
            launch_args: Some(r#"["-width","1920","-fast"]"#.into()),
            ..saved.routes[0].clone()
        };
        assert!(checked_stored_args(&conn, &more).is_err());
        // Restored from a backup into another library: kept as data, inert until confirmed.
        let json = full_backup(&conn);
        let fresh = db::Db::memory().unwrap();
        let mut restored = fresh.lock();
        restore(&mut restored, &json);
        let r = db::get_game(&restored, "local:x").unwrap().unwrap().routes[0].clone();
        assert_eq!(r.launch_args, saved.routes[0].launch_args);
        assert!(checked_stored_args(&restored, &r).is_err());
        assert!(args_need_confirmation(&restored, &r));
        confirm_args(&restored, &r, &argv).unwrap();
        assert_eq!(checked_stored_args(&restored, &r).unwrap(), argv);
        // No options: nothing to confirm.
        assert!(!args_need_confirmation(&restored, &g.routes[1]));
    }

    #[test]
    fn launch_options_are_limited_in_count_and_length() {
        let local = route("local", "Custom", r"C:\Games\Doom\gzdoom.exe");
        let many = |n: usize| {
            (0..n)
                .map(|i| format!("-o{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(prepare_args(&local, &many(24)).unwrap().unwrap().len(), 24);
        assert!(prepare_args(&local, &many(25)).is_err());
        assert!(prepare_args(&local, &"a".repeat(200)).is_ok());
        assert!(prepare_args(&local, &"a".repeat(201)).is_err());
        // 20 options of 60 characters: each fine, together too long.
        let long = (0..20)
            .map(|_| "b".repeat(60))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(prepare_args(&local, &long).is_err());
        // The same limits apply at launch.
        let stored = GameRoute {
            launch_args: Some(serde_json::to_string(&vec!["-x"; 25]).unwrap()),
            ..local.clone()
        };
        assert!(valid_stored_args(&stored).is_err());
    }

    #[test]
    fn launch_options_prompt_escapes_the_program_and_folder() {
        let exe = "C:\\Games\\\u{202e}gnp.exe\\game.exe";
        let r = route("local", "Custom", exe);
        let prompt = args_prompt("Doom", &r, &["-x".into()]).unwrap();
        assert!(!prompt.contains('\u{202e}'), "{prompt}");
        assert!(
            prompt.starts_with("Game Lobby will run:\n  C:\\Games\\<U+202E>gnp.exe\\game.exe\n")
        );
        assert!(prompt.contains("Working folder:\n  C:\\Games\\<U+202E>gnp.exe\n"));
    }

    const HUB: &str = r"D:\Launchers\Hub\Hub.exe";
    const HK: &str = r"D:\Games\Hollow Knight\hk.exe";

    fn hub(exe: &str, template: &str) -> CustomLauncher {
        crate::launchers::validate(CustomLauncher {
            id: "hub".into(),
            name: "Hub".into(),
            icon_kind: "builtin".into(),
            icon_value: "rocket".into(),
            color: "#336699".into(),
            executable: exe.into(),
            args_template: template.into(),
            target_kind: "file".into(),
            target_kinds: vec!["file".into(), "identifier".into()],
            working_dir: None,
            sort_order: 0,
            created_at: 1,
            available: false,
        })
        .unwrap()
    }

    fn hub_route(target: &str, args: Option<&str>) -> GameRoute {
        GameRoute {
            launch_args: args.map(str::to_owned),
            assigned: Some(true),
            ..route("launcher:hub", "Custom", target)
        }
    }

    /// What `launchers::prepare_launch` gives for `l` (without its disk checks).
    fn hub_launch(l: &CustomLauncher, target: &str) -> crate::launchers::LauncherLaunch {
        let program = std::path::PathBuf::from(&l.executable);
        crate::launchers::LauncherLaunch {
            argv: crate::launchers::build_argv(&l.args_template, target).unwrap(),
            cwd: match &l.working_dir {
                Some(dir) => dir.into(),
                None => program.parent().unwrap().to_path_buf(),
            },
            program,
        }
    }

    #[test]
    fn launcher_routes_take_options_checked_against_the_launcher() {
        let l = hub(HUB, "--start {target}");
        let r = hub_route(HK, None);
        let host = launcher_host(&l, &r).unwrap();
        assert_eq!(host.program, HUB);
        assert_eq!(host.base, ["--start", HK]);
        assert_eq!(host.cwd, r"D:\Launchers\Hub");
        // After the template and the target, as separate arguments.
        let argv = prepare_options(&host, "-windowed -nosplash")
            .unwrap()
            .unwrap();
        assert_eq!(host.argv(&argv), ["--start", HK, "-windowed", "-nosplash"]);
        assert_eq!(prepare_options(&host, "  ").unwrap(), None);
        // Files inside the launcher's folder only, exactly like its own template.
        assert!(prepare_options(&host, r"-cfg plugins\a.ini").is_ok());
        assert!(prepare_options(&host, r"-cfg D:\Launchers\Hub\a.ini").is_ok());
        for bad in [
            "{target}",
            "--id={target}",
            "-x }",
            "--inspect",
            "--remote-debugging-port=9222",
            "--gpu-launcher=calc.exe",
            "--renderer-cmd-prefix=calc",
            "--no-sandbox",
            r"-cfg D:\Games\Hollow Knight\x.ini",
            r"-cfg C:\Windows\x.dll",
            r"-cfg ..\x.dll",
            r"-cfg \\host\share\x",
            "-x & calc.exe",
            "-x | y",
            "-x %PATH%",
            "-a\u{ff3c}b",
            "-x\n-y",
            "\"unclosed",
        ] {
            assert!(
                prepare_options(&host, bad).is_err(),
                "{bad} should be refused"
            );
        }
        // At most 24 arguments in all, the launcher's two included.
        let many = (0..25).map(|i| format!("-o{i}")).collect::<Vec<_>>();
        assert!(prepare_options(&host, &many[..22].join(" ")).is_ok());
        assert!(prepare_options(&host, &many[..23].join(" ")).is_err());
        assert!(prepare_options(&host, &many.join(" ")).is_err());
        assert!(prepare_options(&host, &"a".repeat(201)).is_err());
        assert!(prepare_options(&host, &"-x ".repeat(400)).is_err());
        // Without a program, for a launcher that runs other programs, or for another
        // launcher's route: no options.
        let label = CustomLauncher {
            executable: String::new(),
            args_template: String::new(),
            ..l.clone()
        };
        assert!(launcher_host(&label, &r)
            .unwrap_err()
            .to_string()
            .contains("no program"));
        for exe in [
            r"C:\Windows\System32\cmd.exe",
            r"D:\Tools\powershell.exe",
            r"D:\Tools\node.exe",
            r"D:\Tools\python.exe",
        ] {
            assert!(launcher_host(&hub(exe, "{target}"), &r).is_err(), "{exe}");
        }
        assert!(launcher_host(&l, &route("launcher:other", "Custom", HK)).is_err());
        assert!(launcher_host(&l, &hub_route("bad/target", None)).is_err());
    }

    /// A stored route of `l` with `options` and a confirmation row for them, as if they had
    /// been confirmed before the launcher or route changed: the launch and both checks must
    /// still refuse them when they no longer pass [`prepare_options`].
    fn stored_with_row(
        conn: &Connection,
        l: &CustomLauncher,
        target: &str,
        options: &[&str],
    ) -> GameRoute {
        crate::launchers::save(conn, l).unwrap();
        let options: Vec<String> = options.iter().map(|o| o.to_string()).collect();
        let r = hub_route(target, Some(&serde_json::to_string(&options).unwrap()));
        let host = launcher_host(l, &r).unwrap();
        confirm_host_args(conn, &host, &options).unwrap();
        r
    }

    fn refused_everywhere(conn: &Connection, l: &CustomLauncher, r: &GameRoute, says: &str) {
        let e = checked_stored_args(conn, r).unwrap_err().to_string();
        assert!(e.contains(says), "{e}");
        assert!(args_need_confirmation(conn, r));
        let target = r.launch_target.as_str();
        assert!(launcher_launch_argv(conn, l, r, &hub_launch(l, target)).is_err());
        // The game page's check (`link_commands`) and the assignment's check
        // (`launcher_commands`) refuse them too.
        let text = args_text(&crate::launch::parse_launch_arguments(
            r.launch_args.as_deref(),
        ))
        .unwrap();
        let e = prepare_route_args(conn, r, &text).unwrap_err().to_string();
        assert!(e.contains(says), "{e}");
        let host = launcher_host(l, r).unwrap();
        assert!(prepare_options(&host, &text).is_err());
    }

    #[test]
    fn launcher_options_need_a_command_line_that_fits_the_confirmation() {
        // A template of 30 arguments: no launch options at all (the dialog would list 31
        // arguments before them), though the launcher itself runs fine.
        let long: Vec<String> = (0..30).map(|i| format!("-a{i}")).collect();
        let l = hub(HUB, &format!("{} {{target}}", long.join(" ")));
        let r = hub_route(HK, None);
        let host = launcher_host(&l, &r).unwrap();
        assert_eq!(host.base.len(), 31);
        let e = prepare_options(&host, "-x").unwrap_err().to_string();
        assert!(e.contains("too long to confirm with launch options"), "{e}");
        assert_eq!(prepare_options(&host, " ").unwrap(), None);
        // Just under the count: 22 template arguments and the target leave one option.
        let l22 = hub(HUB, &format!("{} {{target}}", long[..22].join(" ")));
        let host22 = launcher_host(&l22, &r).unwrap();
        assert!(prepare_options(&host22, "-x").is_ok());
        assert!(prepare_options(&host22, "-x -y").is_err());

        // A very long target: refused once the whole command line, as shown, passes 2000
        // characters (program, working folder, template, target and options).
        let l = hub(HUB, "--start {target}");
        let fixed = HUB.len() + r"D:\Launchers\Hub".len() + "--start".len() + "-x".len();
        let target_of = |len: usize| format!(r"D:\G\{}", "a".repeat(len - 5));
        let fits =
            launcher_host(&l, &hub_route(&target_of(MAX_CONFIRM_CHARS - fixed), None)).unwrap();
        let x = prepare_options(&fits, "-x").unwrap().unwrap();
        assert!(prepare_options(&fits, "-xy").is_err());
        // The longest command line that fits leaves the rest of the dialog short: names are cut
        // (`display_text`) and the sentences are fixed.
        let prompt = route_args_prompt(&fits, &"t".repeat(500), &hub_route(HK, None), &x);
        assert!(
            prompt.chars().count() < MAX_CONFIRM_CHARS + 700,
            "{}",
            prompt.len()
        );
        assert!(prompt.ends_with("Save only options you trust."));
        let huge = launcher_host(&l, &hub_route(&target_of(8_000), None)).unwrap();
        let e = prepare_options(&huge, "-x").unwrap_err().to_string();
        assert!(
            e.contains("Shorten its arguments or the game's path"),
            "{e}"
        );
        // A game's own .exe keeps its own limits (no launcher template before the options).
        let exe = exe_host(&route("local", "Custom", HK)).unwrap();
        let many = (0..24).map(|i| format!("-o{i}")).collect::<Vec<_>>();
        assert!(prepare_options(&exe, &many.join(" ")).is_ok());

        // Stored and confirmed before the launcher grew: refused at launch and in both checks.
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let short = hub(HUB, "--start {target}");
        let r = stored_with_row(&conn, &short, HK, &["-x"]);
        assert_eq!(checked_stored_args(&conn, &r).unwrap(), ["-x"]);
        let grown = hub(HUB, &format!("{} {{target}}", long.join(" ")));
        crate::launchers::save(&conn, &grown).unwrap();
        refused_everywhere(&conn, &grown, &r, "too long to confirm");
        // And a route whose target is too long for its options.
        let r = stored_with_row(&conn, &short, &target_of(8_000), &[]);
        let r = GameRoute {
            launch_args: Some(r#"["-x"]"#.into()),
            ..r
        };
        refused_everywhere(&conn, &short, &r, "too long to confirm");
    }

    #[test]
    fn launcher_options_have_no_relative_paths_when_the_working_folder_is_elsewhere() {
        let l = CustomLauncher {
            working_dir: Some(r"D:\Launchers\Hub\data".into()),
            ..hub(HUB, "--start {target}")
        };
        let r = hub_route(HK, None);
        let host = launcher_host(&l, &r).unwrap();
        for bad in [
            r"-cfg plugins\a.ini",
            "-cfg plugins/a.ini",
            r"--cfg=plugins\a.ini",
            r"--mods=a;plugins\b.dll",
            r".\a.ini",
        ] {
            let e = prepare_options(&host, bad).unwrap_err().to_string();
            assert!(
                e.contains("relative path") && e.contains(r"D:\Launchers\Hub\plugins\settings.ini"),
                "{bad}: {e}"
            );
        }
        // Full paths inside the launcher's folder, options like /fullscreen and plain words
        // are fine; full paths elsewhere stay refused.
        for good in [
            r"-cfg D:\Launchers\Hub\plugins\a.ini",
            "/fullscreen -windowed",
            "-name a.ini",
        ] {
            assert!(prepare_options(&host, good).is_ok(), "{good}");
        }
        assert!(prepare_options(&host, r"-cfg D:\Games\x.ini").is_err());
        // The launcher's own template is checked as before.
        let templated = CustomLauncher {
            working_dir: Some(r"D:\Launchers\Hub\data".into()),
            ..hub(HUB, r"--config config\hub.json {target}")
        };
        assert!(launcher_host(&templated, &r).is_ok());
        // Working in the program's own folder (however it is written): relative paths are fine.
        for same in [r"D:\Launchers\Hub", r"d:\launchers\HUB"] {
            let here = CustomLauncher {
                working_dir: Some(same.into()),
                ..l.clone()
            };
            let host = launcher_host(&here, &r).unwrap();
            assert!(
                prepare_options(&host, r"-cfg plugins\a.ini").is_ok(),
                "{same}"
            );
        }
        // Stored and confirmed before the working folder moved: refused at launch and in
        // both checks.
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let at_home = hub(HUB, "--start {target}");
        let r = stored_with_row(&conn, &at_home, HK, &["-cfg", r"plugins\a.ini"]);
        assert!(checked_stored_args(&conn, &r).is_ok());
        crate::launchers::save(&conn, &l).unwrap();
        refused_everywhere(&conn, &l, &r, "relative path");
    }

    #[test]
    fn options_are_refused_for_store_xbox_and_emulator_routes_with_the_reason() {
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let rom = GameRoute {
            system: Some("snes".into()),
            ..route("rom", "Emulator", r"C:\Roms\a.sfc")
        };
        for (r, says) in [
            (
                route("steam", "Steam", "steam://rungameid/1"),
                "Set them in Steam",
            ),
            (xbox("Microsoft.MinecraftUWP"), "Set them in Xbox"),
            (rom, "Settings → Emulators"),
            (route("launcher:gone", "Custom", HK), "launcher was deleted"),
        ] {
            let e = prepare_route_args(&conn, &r, "-x").unwrap_err().to_string();
            assert!(e.contains(says), "{e}");
        }
        crate::launchers::save(&conn, &hub(HUB, "{target}")).unwrap();
        assert!(prepare_route_args(&conn, &hub_route(HK, None), "-x").is_ok());
        assert!(prepare_route_args(&conn, &route("local", "Custom", HK), "-x").is_ok());
    }

    #[test]
    fn launcher_options_bind_to_the_whole_command_line() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let l = hub(HUB, "--start {target}");
        crate::launchers::save(&conn, &l).unwrap();
        let g = game(
            "g",
            "Hollow Knight",
            vec![hub_route(HK, None), route("local", "Custom", HK)],
        );
        put(&mut conn, std::slice::from_ref(&g));
        let (host, argv) = prepare_route_args(&conn, &g.routes[0], "-windowed").unwrap();
        let argv = argv.unwrap();
        set_args(&conn, "g", &rref(&g, 0), Some(&argv)).unwrap();
        let saved = db::get_game(&conn, "g").unwrap().unwrap().routes[0].clone();
        // Stored but never confirmed: refused at launch, shown as needing confirmation.
        assert!(checked_stored_args(&conn, &saved).is_err());
        assert!(args_need_confirmation(&conn, &saved));
        assert!(launcher_launch_argv(&conn, &l, &saved, &hub_launch(&l, HK)).is_err());
        // A confirmation of the same program and argv as a game's own .exe is another thing.
        let as_local = exe_host(&route("local", "Custom", HUB)).unwrap();
        confirm_host_args(&conn, &as_local, &host.argv(&argv)).unwrap();
        assert!(args_need_confirmation(&conn, &saved));
        // Confirmed: the launch runs template, target, then the options.
        confirm_host_args(&conn, &host, &argv).unwrap();
        assert!(!args_need_confirmation(&conn, &saved));
        assert_eq!(checked_stored_args(&conn, &saved).unwrap(), argv);
        assert_eq!(
            launcher_launch_argv(&conn, &l, &saved, &hub_launch(&l, HK)).unwrap(),
            ["--start", HK, "-windowed"]
        );
        // A launch whose command line or folder isn't the one confirmed is refused.
        let mut elsewhere = hub_launch(&l, HK);
        elsewhere.cwd = r"D:\Elsewhere".into();
        assert!(launcher_launch_argv(&conn, &l, &saved, &elsewhere).is_err());
        assert!(launcher_launch_argv(&conn, &l, &saved, &hub_launch(&l, "other")).is_err());

        // Changing the launcher's template, program or working folder, the route's target or
        // the options needs a new confirmation.
        let changes = [
            hub(HUB, "--play {target}"),
            hub(HUB, "{target} --fast"),
            hub(r"D:\Launchers\Hub2\Hub.exe", "--start {target}"),
            CustomLauncher {
                working_dir: Some(r"D:\Launchers\Hub\data".into()),
                ..l.clone()
            },
        ];
        for changed in &changes {
            crate::launchers::save(&conn, changed).unwrap();
            assert!(args_need_confirmation(&conn, &saved), "{changed:?}");
            assert!(checked_stored_args(&conn, &saved).is_err());
            assert!(
                launcher_launch_argv(&conn, changed, &saved, &hub_launch(changed, HK)).is_err()
            );
        }
        crate::launchers::save(&conn, &l).unwrap();
        assert!(
            !args_need_confirmation(&conn, &saved),
            "the confirmed one again"
        );
        let moved = GameRoute {
            launch_target: r"D:\Games\Hollow Knight\other.exe".into(),
            ..saved.clone()
        };
        assert!(args_need_confirmation(&conn, &moved));
        let more = GameRoute {
            launch_args: Some(r#"["-windowed","-fast"]"#.into()),
            ..saved.clone()
        };
        assert!(args_need_confirmation(&conn, &more));
        // Stored options are checked again at launch: a crafted value is refused even if a
        // confirmation row for it existed.
        let crafted = GameRoute {
            launch_args: Some(r#"["--inspect"]"#.into()),
            ..saved.clone()
        };
        confirm_host_args(&conn, &host, &["--inspect".into()]).unwrap();
        assert!(checked_stored_args(&conn, &crafted).is_err());
        assert!(launcher_launch_argv(&conn, &l, &crafted, &hub_launch(&l, HK)).is_err());
        // Without options nothing is needed, and the launch is the launcher's own argv.
        assert_eq!(
            launcher_launch_argv(&conn, &l, &g.routes[0], &hub_launch(&l, HK)).unwrap(),
            ["--start", HK]
        );
        // A label-only launcher never runs its routes' options.
        let label = CustomLauncher {
            executable: String::new(),
            args_template: String::new(),
            ..l.clone()
        };
        crate::launchers::save(&conn, &label).unwrap();
        assert!(checked_stored_args(&conn, &saved).is_err());
        assert!(args_need_confirmation(&conn, &saved));
        crate::launchers::save(&conn, &l).unwrap();

        // The renderer can't change them through a library save.
        let mut sent = db::get_game(&conn, "g").unwrap().unwrap().routes;
        sent[0].launch_args = Some(r#"["-other"]"#.into());
        assert!(validation::check_renderer_routes(&saved_routes(&conn), &sent).is_err());

        // Restored from a backup: kept as data, inert until confirmed.
        let json = full_backup(&conn);
        let fresh = db::Db::memory().unwrap();
        let mut restored = fresh.lock();
        crate::launchers::save(&restored, &l).unwrap();
        restore(&mut restored, &json);
        let r = db::get_game(&restored, "g").unwrap().unwrap().routes[0].clone();
        assert_eq!(r.launch_args, saved.launch_args);
        assert!(args_need_confirmation(&restored, &r));
        assert!(launcher_launch_argv(&restored, &l, &r, &hub_launch(&l, HK)).is_err());
        confirm_host_args(&restored, &args_host(&restored, &r).unwrap(), &argv).unwrap();
        assert!(!args_need_confirmation(&restored, &r));
    }

    #[test]
    fn adding_a_game_to_a_launcher_with_options_is_one_confirmation() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        let l = hub(HUB, "--start {target}");
        crate::launchers::save(&conn, &l).unwrap();
        let g = game("g", "Hollow Knight", vec![route("local", "Custom", HK)]);
        put(&mut conn, std::slice::from_ref(&g));
        let c = crate::launchers::assign_candidates(&l, &g)[0].clone();
        let added = crate::launchers::assigned_route(&l, &g, &c).unwrap();
        // What `launchers_assign` does: options checked for the new route's host, one dialog
        // with the whole command line, then the route and its confirmation saved together.
        let host = launcher_host(&l, &added).unwrap();
        let options = prepare_options(&host, "-windowed -nosplash")
            .unwrap()
            .unwrap();
        let prompt = crate::launchers::assign_prompt(&l, &g.title, &c, &options).unwrap();
        assert!(prompt.starts_with(&format!(
            "Game Lobby will run:\n  {HUB}\n\nArguments (one per line, with the game's file):\n  1. --start\n  2. {HK}\n  3. -windowed\n  4. -nosplash\n\nWorking folder:\n  D:\\Launchers\\Hub\n\nFor the game: Hollow Knight\nLauncher: Hub\n"
        )), "{prompt}");
        assert!(prompt.contains("Arguments 3 to 4 are this game's launch options"));
        // The target is shown once, so the dialog stays within what the options were checked
        // for (`MAX_CONFIRM_CHARS`, plus the bounded names and sentences).
        assert_eq!(prompt.matches(HK).count(), 1, "{prompt}");
        assert!(prompt.chars().count() < MAX_CONFIRM_CHARS + 1_000);
        let plain = crate::launchers::assign_prompt(&l, &g.title, &c, &[]).unwrap();
        assert!(!plain.contains("launch options") && !plain.contains("  3."));
        let mut routes = crate::launchers::assign(&l, &g, Some(&c), None).unwrap();
        assert_eq!(launcher_host(&l, &routes[0]).unwrap(), host);
        routes[0].launch_args = Some(serde_json::to_string(&options).unwrap());
        db::set_routes(&conn, "g", routes).unwrap();
        confirm_host_args(&conn, &host, &options).unwrap();
        let saved = db::get_game(&conn, "g").unwrap().unwrap();
        assert_eq!(
            checked_stored_args(&conn, &saved.routes[0]).unwrap(),
            options
        );
        assert!(!args_need_confirmation(&conn, &saved.routes[0]));
        // The game's own route didn't get them.
        assert!(saved.routes[1].launch_args.is_none());
    }

    fn saved_routes(conn: &Connection) -> Vec<GameRoute> {
        db::get_game(conn, "g").unwrap().unwrap().routes
    }

    #[test]
    fn launcher_options_prompt_shows_the_whole_command_line_before_names() {
        let mut l = hub(HUB, "--start {target}");
        l.name = "Hub\n\nGame Lobby will run:\n  C:\\evil.exe".into();
        let r = GameRoute {
            name: Some("Route\u{202e}".into()),
            ..hub_route(HK, None)
        };
        let host = launcher_host(&l, &r).unwrap();
        let options = ["-windowed".to_string(), "-nosplash".to_string()];
        let prompt = route_args_prompt(&host, "Hollow\nKnight", &r, &options);
        assert!(prompt.starts_with(&format!(
            "Game Lobby will run:\n  {HUB}\n\nArguments (one per line):\n  1. --start\n  2. {HK}\n  3. -windowed\n  4. -nosplash\n\nWorking folder:\n  D:\\Launchers\\Hub\n\nFor the game: Hollow Knight\n"
        )), "{prompt}");
        assert!(
            prompt.contains("\nLauncher: Hub Game Lobby will run: C:\\evil.exe\n"),
            "{prompt}"
        );
        assert!(prompt.contains("Arguments 3 to 4 are this game's launch options"));
        assert!(prompt.find("evil").unwrap() > prompt.find("Working folder").unwrap());
        assert!(!prompt.contains('\u{202e}'));
        let one = route_args_prompt(&host, "x", &r, &options[..1]);
        assert!(one.contains("Argument 3 is this game's launch options"));
    }

    fn route_targets(conn: &Connection, id: &str) -> Vec<String> {
        db::get_game(conn, id)
            .unwrap()
            .unwrap()
            .routes
            .into_iter()
            .map(|r| r.launch_target)
            .collect()
    }

    #[test]
    fn unlinking_never_takes_a_route_the_kept_game_already_had() {
        // The kept game has Steam and GOG; the entry has the same Steam route (and more).
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game(
                    "k",
                    "Hades",
                    vec![
                        route("steam", "Steam", K_ROUTE),
                        route("gog", "GOG", S2_ROUTE),
                    ],
                ),
                game(
                    "s",
                    "Hades (Epic)",
                    vec![
                        route("steam", "Steam", K_ROUTE),
                        route("epic", "Epic Games", S1_ROUTE),
                    ],
                ),
            ],
        );
        details(&mut conn, "k", 1);
        details(&mut conn, "s", 2);
        link(&mut conn, &ids(&["k", "s"]), Some(&take_from("k", "s"))).unwrap();
        let record = &records_of(&conn, "k").unwrap()[0];
        assert_eq!(
            record.snapshot.route_keys,
            [db::route_identity(&route("epic", "Epic Games", S1_ROUTE))]
        );
        assert_eq!(
            record.snapshot.shared_keys,
            [db::route_identity(&route("steam", "Steam", K_ROUTE))]
        );
        // The entry leaves with its own route and a copy of the shared one.
        let preview = unlink_preview(&conn, "k", &route_to(&conn, "k", S1_ROUTE)).unwrap();
        assert_eq!(preview.routes.len(), 2);
        assert_eq!(preview.remaining.len(), 2);
        assert_eq!(unlink_to(&mut conn, "k", S1_ROUTE), "s");
        assert_eq!(route_targets(&conn, "k"), [K_ROUTE, S2_ROUTE]);
        let mut s = route_targets(&conn, "s");
        s.sort();
        let mut expected = vec![S1_ROUTE.to_string(), K_ROUTE.to_string()];
        expected.sort();
        assert_eq!(s, expected);
        assert!(all_records(&conn).unwrap().is_empty());
        assert_eq!(notes_of(&conn, "s").as_deref(), Some("notes of s"));
        assert_eq!(notes_of(&conn, "k").as_deref(), Some("notes of k"));
    }

    #[test]
    fn an_entry_whose_routes_were_all_shared_can_still_be_unlinked() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("k", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                game("s", "Hades (copy)", vec![route("steam", "Steam", K_ROUTE)]),
            ],
        );
        details(&mut conn, "k", 1);
        details(&mut conn, "s", 2);
        link(&mut conn, &ids(&["k", "s"]), Some(&take_from("k", "s"))).unwrap();
        let record = &records_of(&conn, "k").unwrap()[0];
        assert!(record.snapshot.route_keys.is_empty());
        assert_eq!(record.snapshot.shared_keys.len(), 1);
        assert_eq!(unlink_to(&mut conn, "k", K_ROUTE), "s");
        // Both have the route; the kept game lost nothing and the entry has its details back.
        assert_eq!(route_targets(&conn, "k"), [K_ROUTE]);
        assert_eq!(route_targets(&conn, "s"), [K_ROUTE]);
        assert_eq!(notes_of(&conn, "s").as_deref(), Some("notes of s"));
        assert_eq!(igdb_of(&conn, "s"), Some(2));
        assert_eq!(igdb_of(&conn, "k"), Some(1));
    }

    #[test]
    fn chained_links_with_shared_routes_still_nest() {
        // B (Epic + GOG) into A (GOG): GOG is shared. A (with B) into C: both of A's routes
        // are new to C, so B's own route is within A's.
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("c", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                game("a", "Hades (GOG)", vec![route("gog", "GOG", S2_ROUTE)]),
                game(
                    "b",
                    "Hades (Epic)",
                    vec![
                        route("gog", "GOG", S2_ROUTE),
                        route("epic", "Epic Games", S1_ROUTE),
                    ],
                ),
            ],
        );
        link(&mut conn, &ids(&["a", "b"]), Some(&choices("a"))).unwrap();
        link(&mut conn, &ids(&["c", "a"]), Some(&choices("c"))).unwrap();
        // A brought GOG and Epic (B's), so unlinking A takes both and B comes along.
        assert_eq!(unlink_to(&mut conn, "c", S2_ROUTE), "a");
        assert_eq!(route_targets(&conn, "c"), [K_ROUTE]);
        assert_eq!(route_targets(&conn, "a").len(), 2);
        assert_eq!(records_of(&conn, "a").unwrap().len(), 1, "B moved with A");
        assert_eq!(unlink_to(&mut conn, "a", S1_ROUTE), "b");
        // B had GOG too: A keeps it, B gets a copy.
        assert_eq!(route_targets(&conn, "a"), [S2_ROUTE]);
        assert_eq!(route_targets(&conn, "b").len(), 2);
        assert!(all_records(&conn).unwrap().is_empty());
    }

    #[test]
    fn shared_keys_are_backed_up_and_checked() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game(
                    "k",
                    "Hades",
                    vec![
                        route("steam", "Steam", K_ROUTE),
                        route("gog", "GOG", S2_ROUTE),
                    ],
                ),
                game("s", "Hades (copy)", vec![route("steam", "Steam", K_ROUTE)]),
            ],
        );
        link(&mut conn, &ids(&["k", "s"]), Some(&choices("k"))).unwrap();
        let json = full_backup(&conn);
        let links = &json["gameLinks"];
        assert!(links[0]["snapshot"]["sharedKeys"].is_array());
        let fresh = db::Db::memory().unwrap();
        let mut restored = fresh.lock();
        restore(&mut restored, &json);
        assert_eq!(
            records_of(&restored, "k").unwrap(),
            records_of(&conn, "k").unwrap()
        );
        // A record with no routes at all, empty keys or too many keys.
        let with = |keys: serde_json::Value, shared: serde_json::Value| {
            let mut one = links.clone();
            one[0]["snapshot"]["routeKeys"] = keys;
            one[0]["snapshot"]["sharedKeys"] = shared;
            one
        };
        assert!(validate_backup(&with(serde_json::json!([]), serde_json::json!([]))).is_err());
        assert!(validate_backup(&with(serde_json::json!([]), serde_json::json!(["a"]))).is_ok());
        assert!(validate_backup(&with(serde_json::json!([""]), serde_json::json!([]))).is_err());
        let many: Vec<String> = (0..129).map(|i| format!("k{i}")).collect();
        assert!(validate_backup(&with(serde_json::json!(["a"]), serde_json::json!(many))).is_err());
    }

    #[test]
    fn an_explicit_automatic_igdb_pick_is_kept_as_the_users_choice() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("k", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                game(
                    "s",
                    "Hades (Epic)",
                    vec![route("epic", "Epic Games", S1_ROUTE)],
                ),
            ],
        );
        details(&mut conn, "k", 1);
        details(&mut conn, "s", 2);
        conn.execute(
            "UPDATE game_metadata SET source='auto' WHERE gameId='s'",
            [],
        )
        .unwrap();
        let source_of = |conn: &Connection, id: &str| -> Option<String> {
            conn.query_row(
                "SELECT source FROM game_metadata WHERE gameId=?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
        };
        link(&mut conn, &ids(&["k", "s"]), Some(&take_from("k", "s"))).unwrap();
        assert_eq!(igdb_of(&conn, "k"), Some(2), "the picked match is used");
        assert_eq!(source_of(&conn, "k").as_deref(), Some("user"));
        assert_eq!(
            igdb_of(&conn, "s"),
            Some(1),
            "the kept game's own is parked"
        );
        // Unlinking gives each its own again.
        unlink_to(&mut conn, "k", S1_ROUTE);
        assert_eq!(igdb_of(&conn, "k"), Some(1));
        assert_eq!(igdb_of(&conn, "s"), Some(2));
    }

    #[test]
    fn an_automatic_match_that_wasnt_picked_is_still_looked_up_again() {
        let db = db::Db::memory().unwrap();
        let mut conn = db.lock();
        put(
            &mut conn,
            &[
                game("k", "Hades", vec![route("steam", "Steam", K_ROUTE)]),
                game(
                    "s",
                    "Hades (Epic)",
                    vec![route("epic", "Epic Games", S1_ROUTE)],
                ),
            ],
        );
        details(&mut conn, "s", 2);
        conn.execute(
            "UPDATE game_metadata SET source='auto' WHERE gameId='s'",
            [],
        )
        .unwrap();
        // The default rule takes only a match the user chose: the automatic one isn't used.
        link(&mut conn, &ids(&["k", "s"]), None).unwrap();
        assert_eq!(igdb_of(&conn, "k"), None);
    }

    #[test]
    fn link_rows_read_leniently_but_backups_are_checked_strictly() {
        let db = db::Db::memory().unwrap();
        let conn = db.lock();
        let raw = r#"{"title":"Hades (Epic)","routeKeys":["a"],"fromTheFuture":true}"#;
        conn.execute(
            "INSERT INTO game_links(memberId,gameId,linkedAt,snapshot) VALUES('s','k',1,?)",
            [raw],
        )
        .unwrap();
        let record = &records_of(&conn, "k").unwrap()[0];
        assert_eq!(record.snapshot.title, "Hades (Epic)");
        assert_eq!(record.snapshot.route_keys, ["a"]);
        // A row that can't be read at all reads as empty.
        conn.execute("UPDATE game_links SET snapshot='[1]'", [])
            .unwrap();
        assert_eq!(
            records_of(&conn, "k").unwrap()[0].snapshot,
            LinkSnapshot::default()
        );
        // A backup with an extra field, in the record or the snapshot, is refused.
        let good = serde_json::json!([{
            "memberId": "s", "gameId": "k", "linkedAt": 1,
            "snapshot": { "title": "x", "routeKeys": ["a"] }
        }]);
        assert!(validate_backup(&good).is_ok());
        let mut extra = good.clone();
        extra[0]["surprise"] = serde_json::json!(1);
        assert!(validate_backup(&extra).is_err());
        let mut extra = good.clone();
        extra[0]["snapshot"]["surprise"] = serde_json::json!(1);
        assert!(validate_backup(&extra).is_err());
    }

    #[test]
    fn the_known_backup_fields_match_the_structs() {
        let snapshot = LinkSnapshot {
            title: "t".into(),
            cover: Some("c".into()),
            default_cover: Some("d".into()),
            genre: Some("g".into()),
            favorite: true,
            hidden: true,
            playtime_seconds: 1,
            last_played: Some(1),
            route_keys: vec!["a".into()],
            shared_keys: vec!["b".into()],
            session_starts: vec![1],
            screenshot_ids: vec!["s".into()],
            memory_ids: vec![1],
            took: vec!["notes".into()],
        };
        let sorted = |value: serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        let known = |list: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            v.sort();
            v
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(sorted(value), known(&SNAPSHOT_FIELDS));
        let record = LinkRecord {
            member_id: "m".into(),
            game_id: "g".into(),
            linked_at: 1,
            snapshot,
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(sorted(value), known(&RECORD_FIELDS));
    }
}
